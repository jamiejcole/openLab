# openLab control loop (`openlab-engine::control`)

The engine's control thread (ARCHITECTURE §3.1, §3.2): it owns the workspace, its undo history, the cue engine and the mixer controller, and wires them to the disk streams. Contracts it follows: [cue-engine.md](cue-engine.md) "Actions for the binary (M4.2 contract)", [audio.md](audio.md) "Threads and queues", [protocol.md](protocol.md). Status: M4.2a and M4.2b (devices: [Devices](#devices-m42b-nfr-08)).

## Shape

- `Control` holds the state and logic and has **no thread or clock of its own**. Time is the mixer's sample counter (`MixerController::samples_rendered`), whoever advances it. Two drivers call the same `Control::handle(inbox message)` and `Control::tick()`:
  - `runner::run(control, &inbox)` — the control thread against a running device (`host::run_show` opens it with `open_output`; `openlab-engine SHOW_FILE` plays a show on the default device until stdin closes);
  - `offline::OfflineShow` — the offline render driver (ARCHITECTURE §3.6): render one block into memory, then tick. Tests drive it step by step; no sleeps, no wall clock.
- `Backend` is what only a device has (`Device(devices::Device)` or `Offline`): the device list, loss and reopen, and the allocation-violation counter.
- The loop logic never reads the wall clock, except the device policy (grace period, reopen backoff, list polling: [Devices](#devices-m42b-nfr-08)) through an injected `WallClock`, and the probe watchdog. Otherwise the only wall-clock value is how long `runner::run` waits on the inbox (`wait_hint`).

## Threads

| Thread | Owns | Talks via |
|---|---|---|
| control (`openlab-control`) | `Control` | crossbeam-channel inbox in; bounded outbound channel (`OUTBOUND_CAPACITY` 4096) out; `MixerController` (SPSC) to the audio thread |
| probe workers (`openlab-probe-N`, `PROBE_WORKERS` = 2) | `probe`, `preload_within` | two job channels from control (high priority first); answers through the control inbox (`Inbox::Probe`) |
| device (`openlab-device`) | the device layer (cpal) and the open stream | job channel from control (open, close); answers through the control inbox (`Inbox::Device`) |
| device list (`openlab-device-list`) | the device lister | list requests from control; answers through the control inbox (`Inbox::Device`) |
| disk workers (`DiskStreamer`, 2) | decoders, resamplers, rings | `prepare` from control (no I/O); `StreamHandle` atomics back |
| audio (device) / caller (offline) | `Mixer` | SPSC queues, atomics |

**The control thread never touches the disk.** Probing a file decodes it whole (seconds for a long compressed file) and preloading decodes it into memory, so both run on the probe workers; `DiskStreamer::prepare` does no I/O (the disk worker opens the file). Tested with a named pipe whose probe blocks until a writer appears: the loop keeps ticking and a GO meanwhile does not block (`control_media.rs`). Only the offline driver waits for the disk, outside `tick` (below).

## Inbox and replies

`Inbox::Client { message, reply }` carries a `ClientMessage` already decoded and authenticated by the network layer (M4.3). Every `command`, `transport`, `undo`, `redo` and `request` gets exactly one `result` on `reply`; broadcasts go to the outbound channel. `Inbox::Probe` is internal feedback; `Inbox::Shutdown` (or `goodbye { shutdown: true }`) ends the loop, which then hard-stops the show (two Panics).

- **Transport** → the cue-engine input of docs/protocol.md's table (`hard_stop` = `Stop { fade_out: None }`), handled at once; `result` ok means "handed to the cue engine".
- **Command / undo / redo** → `History` on the workspace, then `engine.model_changed()` before the engine's next call (M4.2 contract), one `model_changed { revision, upserts, deletes }` broadcast (below), a `document` broadcast, then the `result` with that revision, then one `Tick` so the edit takes effect now (a deleted running cue stops on the next block). A refused command changes nothing: `command_refused`. `unsignalled_model_changes()` is checked after every engine call; a rise is logged as an error.
- **Requests**: `snapshot` answers `snapshot` then `result`; `list_devices` answers at once from the last device list read (no device call on the control thread; offline: `ControlConfig::devices`); every other request is `unsupported` until M4.4.
- **Ordering across channels**: a command's `model_changed` goes to the outbound channel before its `result` goes to `reply`. M4.3 must keep that order per connection (e.g. drain the broadcasts before writing a reply).

### Model changes (M4.1a)

Every model edit (command, undo, redo, a relink command, an Arm/Disarm `SetArmed`) broadcasts exactly one `model_changed`; the revision rises by one per message, so a client that sees a gap requests a `snapshot`. A newly opened or reverted workspace is a `snapshot`, not a diff (M4.4). A command refused by `History`, or an undo/redo with nothing to do, sends nothing.

The contents are derived from the edit, not by diffing workspaces. Commands touch cues by id and nothing else (`core::changes`): `Command::footprint()` lists the cue ids a command added, removed, moved or edited (a transaction: the union). For an undo or redo the footprint is that of the step about to be applied (`History::peek_undo`/`peek_redo`; the inverse is itself a command). `SetArmed` names the cues whose flag changed. After the edit, `model_changes(&workspace, &footprint)` reads those cues from the new workspace: one pass over the tree with ordered-set (`BTreeSet`) lookups, no per-cue allocation unless the cue is sent. A command whose footprint is empty (an empty transaction) changed nothing: no revision, no `model_changed`, no `document`, not dirty; the `result` reports the current revision. What is sent is O(cues named), not O(show): renaming one cue in a 2,000-cue show is one upsert.

- `deletes`: every removed or moved cue. Deleting movers first means a client never reasons about where a cue used to be, so several moves in one step (a transaction) land correctly applying `deletes` then `upserts` in order.
- `upserts`: every named cue that exists after the edit, at its final `(list, parent, index)`, in tree order (ascending within a container, parents first). A named cue inside another named group is not sent separately: the group's upsert carries its subtree. So the one case that costs more than the footprint is editing a group itself (rename, mode): its subtree travels with it, as the protocol defines a group upsert.
- A move is therefore a delete plus an upsert in one message.

No command edits cue lists, patches, network patches or settings yet; their upserts/deletes are defined (and applied by `Workspace::apply_changes`) but not produced. When such a command exists, its footprint grows the same way.

`Workspace::apply_changes` is the reference client: deletes (an unknown id is ignored), then upserts (a cue already present is replaced wherever it is). Tests (`core/tests/commands_prop.rs`, `engine/tests/control_model.rs`) assert mirror == engine workspace after every step of random command/undo/redo sequences.

## Time and wake rules (ARCHITECTURE §3.2)

- `block` = frames per device callback (the device's buffer, else the mixer's `max_block_frames`); offline: the step size.
- `now = samples_rendered + block`: the earliest sample a command can still hit. `EngineConfig::lookahead = block`, so a call commits the deadlines in `now .. now + block` (end excluded) — exactly the next block — and no voice start is ever scheduled beyond it.
- `tick` runs `Input::Tick` only when `engine.is_due(now)`.
- `runner::run` waits on the inbox until a message comes, the next deadline enters the window, a deferred report becomes due, or `MAX_WAIT` (5 ms), then ticks. Wake jitter never shifts audio: every transition is computed from sample time.
- `EngineConfig::seed` comes from `entropy_seed()` (std's OS-seeded `RandomState` over the time and process id), so random groups differ between runs; tests pass a fixed seed.

Each `tick`: probe answers to the engine (one `Availability`), voices that waited for them; mixer events; stream failures; deferred feedback that is due; `Tick` if due; priming; progress and meters (≤ 30 Hz); the workspace warnings list if it changed; health counters; `MixerController::pump()` (every tick, docs/audio.md).

## Voices

Each run segment gets its own mixer `VoiceId`, allocated by the loop and **never reused** (a later workspace's engine starts its `RunId`s again; its voices can never be taken for an old one's). `RunId ↔ VoiceId` are kept in both directions.

| Action | Carried out |
|---|---|
| `LoadVoice { run, offset }` | slice table, `prepare` (or wait for the probe); kept as the run's stream |
| `UnloadVoice` | the stream is dropped (cancelled) |
| `StartVoice { run, at, offset }` | the loaded stream, else the cue's primed one (offset 0), else a fresh `prepare`; `VoiceStart` with the model's levels; `StartVoice` at `at`; for a finite table, `Input::ActionLength { frames: offset + table frames (+ delay if held) }` back to the engine, which then owns the end as a deadline (auto-follow and playlist moves on the exact end sample; docs/cue-engine.md) |
| `StopVoice { at, fade }` | mixer `StopVoice` (fade with the curve); the record goes with the `VoiceFinished` |
| `PauseVoice { at }` | mixer `PauseVoice` (native pause, M3.18: position and ring kept) |
| `ResumeVoice { run, paused, at }` | mixer `ResumeVoice { voice: old, new_id }` re-keys the voice to a fresh id for the fresh run |
| `Ramp` | mixer `Ramp` (`Master` / `CueOutput` / `Crosspoint`), no gangs (the Fade's values are already per output) |
| `DevampVoice { stop }` | mixer `Devamp` |
| `PreviewStart` / `PreviewStop` | as `StartVoice` (offset 0, never a primed stream) / `StopVoice`, on the cue's own patch until PB-09's audition output (M2.21) |
| `SetArmed` | applied to the model before the engine's next call, then `model_changed()` with an upsert of each cue whose flag changed |

- **Routing** (`VoiceStart`): master `master_db`; cue output `o` at its level (muted or beyond the patch's outputs: silent); listed crosspoints (muted: silent); the patch's mixer matrix; the integrated fade-in (none when starting part way in) and fade-out. dB → gain with core's `db_to_gain` (the mixer's conversion).
- **Slice table**: `plan::cue_slices` turns trims, markers and play counts into `plan_at_rate`'s table (markers outside the trims are dropped; the slice the out point falls in plays as the marker that ends it says), then skips `offset` frames of the action with loops unrolled. An offset at or past a finite action's end plays nothing: the action ends on its start sample. A table that cannot be built (`LoopTooShort`, M3.17, or bad markers) fails the run (`ActionFailed { stream_failed }`, a `bad_plan` notice) and is listed in the workspace warnings as soon as the file is probed (the pre-show view; a `CueProblem` for it would need core and protocol changes — open question in PROGRESS).
- **Waiting for a probe**: a start (or load) whose file has not been probed yet waits; commands for it (ramps, devamps) are queued and replayed after the start, a pause holds it, a stop drops it. A held start sends a `probe_wait` notice at once. When the probe answers it starts at `max(start, now)` from its head with a `cold_start` notice (and its delay counted into its `ActionLength`); a failed probe fails the run. A start still held after `PROBE_HOLD_LIMIT` (5 s of sample time) fails (`ActionFailed { stream_failed }`, a `probe_timeout` notice): the cue goes broken instead of showing as running with no sound. It stays broken (a run-time break) until it is reset or started again; a late probe answer clears the file's static problem but does not restart it.
- **Probe order and stuck workers**: probes go out in workspace order on a low-priority queue; every priming pass (playhead move, edit, probe answer) puts the files of the cues it wants primed on the high-priority queue, which the workers always empty first, so after a restart mid-show the cue at the playhead is not behind a hundred others. A worker busy on one file longer than `probe_timeout` (`PROBE_TIMEOUT`, 30 s, wall clock: the only wall-clock rule in the loop) is abandoned and replaced, at most `MAX_ABANDONED_WORKERS` (16) times (reaching the limit is logged once as an error; further stuck probes then just wait, and their starts fail after the hold limit); its file is reported `unsupported` ("could not be read in time") meanwhile, and a late answer still counts. The abandoned thread is leaked, not joined: it ends when its read returns, if it ever does. A re-probe (after a stream failure) keeps the last answer, and so the cues' primes, until the new answer comes.
- **Patches** (`Patches`): workspace patch `i` uses mixer matrix `i` (more patches than matrices: `unmapped`). Each patch is resolved with `resolve_patch` against the machine settings and the **playable** devices (the device the mixer plays on, with the mixer outputs that reach it; none after an outage's grace period; offline: `ControlConfig::devices`) and sent to the engine (`Availability` on open, `PatchStatus`-equivalent `Availability` on every change: edits, device list changes, loss, reopen). The engine plays on one device (REQUIREMENTS §21, AUD-03): a patch mapped to another device resolves `device_missing`. A mapped patch's matrix routes cue output `o` to its device channel at patch master × output level (muted: 0; polarity: negative gain); everything else 0. Only changed matrix entries are sent. The workspace master goes to `SetMaster`.
- **Output delays**: a patch output's `delay_ms` is a device-output setting (`SetOutput { delay_frames }`, rounded at the device rate; level, mute and polarity stay in the matrix). Each device output gets the delay of the mapped patch outputs routed to it; patches that disagree get the longest and an `output_delay_conflict` warning. Only changes are sent; an unmapped patch's outputs go back to 0.

## Priming (ARCHITECTURE §3.5)

After every playhead change, model change and probe answer the loop keeps primed (a stream prepared and filling, ready to start): from each list's playhead the next `prime_cues` (16) cues, the targets of those cues (a group target primes its audio cues), and every cue with a hotkey or OSC trigger — armed audio cues with media and no run, at most `max_primes` (64). A prime is used by a start from the top of that cue if the cue's file, probe and trims/slices still match; otherwise it is released. A start without one is cold: it is heard once its ring fills, with a `cold_start` notice, never a failure.

**Preload** (AUD-05): a file whose cue asks for it, or whose probe advises it (`preload_advised`: damaged files that seek slowly), is decoded into memory on a probe worker within the workspace-wide `preload_budget` (default 512 MiB); a file that does not fit streams and is listed as `preload_over_budget`, one whose preload fails (a decode error, memory) streams and is listed as `preload_failed`. Memory goes back when no cue uses the file.

## Feedback

Mixer events, stream state and probe answers become cue-engine inputs:

| From | Input |
|---|---|
| `VoiceFinished` (current id) | `ActionEnded { cue, run, at }`; `ActionFailed { stream_failed }` instead if its stream failed |
| `VoiceFinished` dated at or after a pending pause (the voice ended as the pause applied) | forwarded (the engine ignores it while paused); the record is kept, and the resume that follows reports `ActionEnded` with the **fresh** run on the resume sample (M2-E (a): the mixer ignores the resume of a voice it no longer has) |
| `VoiceFinished` under an old id after a resume was sent | the voice ended before the resume applied: `ActionEnded` with the fresh run, dated no earlier than the resume |
| `VoiceRejected` | `ActionFailed { voice_rejected }`; a refused resume also stops the voice still paused under its old id |
| `StreamHandle::error` (polled every tick) | `ActionFailed { stream_failed }` once per run, and the file is probed again (it may have gone or changed) |
| `DevampScheduled` / `DevampIgnored` | `Input::DevampScheduled { at }` / `DevampIgnored` for the run the id stands for (an old id's run is stale and ignored by the engine); after `DevampScheduled` for the current id, the action's new `ActionLength` (position on `at` plus the table after the loop; `at` alone for a devamp stop; none while the rest loops for ever) |
| `Underrun` | an `underrun` notice for the cue. A starved voice does not skip what it missed (docs/audio.md, Voices), so it ends later than its planned end; the engine keeps the planned end (the next cue starts on time, the late tail overlaps it) |
| probe answer | `MediaStatus` in one `Availability` per tick: `available` (damaged-but-playable included; its damage is listed in the warnings), `missing` (`NotFound`), else `unsupported { reason }` |

- **No feedback before its sample**: a report dated `at` (a re-reported end on a resume sample, the end of an empty action) is held until `now > at` (answers PROGRESS M2-E (c) for this binary: a voice event is never handed over at a `now` at or before the sample it reports; mixer events always report rendered samples, so they qualify at once).
- The id a voice had before its latest resume stays an **alias** until the voice is gone (an end before the resume applied comes under it); older ids are named for one more meter read only (one more entry after a re-key, docs/audio.md), so a cue paused and resumed many times keeps one alias.
- **Orphaned tails**: after an underrun a voice can sound past the end the engine completed its cue on (the cue no longer owns it). Panic, a hard stop (`hard_stop`, `stop` without fade) and Reset also stop every such voice in their scope (the whole show, or the cue and its descendants): Panic with a fade ending on the panic's end, the others at once. Tested with a voice starved by a stalled disk worker (`control_media.rs`).
- The resume of a voice that had ended (re-reported with the fresh run) is now only reachable for an action whose length the engine was not told (a loop): with a known length the engine completes the action on its end sample before a pause dated there.

## Reconciliation of lost events

The mixer drops events when its queue (1024) is full and counts them (`dropped_events`). A lost `VoiceFinished` would leave a cue running for ever. The voice-meter read (every meters period, ~30 Hz) lists every voice the mixer holds, paused ones included. A started voice whose start sample was rendered before the previous read and that is missing from **two reads in a row**, while `dropped_events > 0`, is taken as ended: `ActionEnded` on the read's sample (or `ActionFailed { stream_failed }` if its stream failed). A voice that no meter read ever listed was never taken by the mixer: its lost report was a rejection, so it fails (`ActionFailed { voice_rejected }`) instead. (A rejected voice of known length whose rejection is lost completes on its planned end if that comes before the two reads.) The first loss is logged (`warn`). Tested by overflowing the queue with loop events while the control thread is held off (`control_feedback.rs`).

## Outbound

- `runtime { seq, changes }`: one message per engine call with changes, from `RuntimeChange::from_action` read right after the call (a broken state carries its reason), plus binary notices as `warning { notice }`: `probe_wait`, `probe_timeout`, `cold_start`, `underrun`, `bad_plan`, `stream_failed`, `device_lost` (the moment a device is lost).
- `warnings`: the workspace-level list, replaced when it changes, rebuilt at most once per tick: `damaged_media`, `preload_over_budget`, `preload_failed`, `bad_plan` (per cue), `device_lost` (while an outage lasts; the text says whether the grace period is over), `device_unrecoverable`, `outputs_dropped` (the device has fewer channels than the mixer's outputs; and per patch that maps more channels than the device has), `output_delay_conflict`, `realtime_allocation`. The `bad_plan` check keeps each cue's result and rebuilds a cue's plan only when its trims/markers or its file's probe change (`plan_builds()`: an edit of one cue in a 2,000-cue show builds at most one plan; tested).
- `progress`: show cues with a playing voice (elapsed and remaining in the action, remaining `null` while it loops for ever), at most 30 Hz of sample time, only while one plays.
- `meters`: per mapped patch the device outputs it is routed to, per cue its voices' peaks per cue output (`read_output_meters_into` / `read_voice_meters_into` into preallocated buffers), at most 30 Hz, while audio plays plus one message after.
- `log`: engine faults (`error`), audio-thread allocations (`error`, M3.21), lost mixer events (`warn`), unsignalled model changes (`error`), device loss and the end of its grace period (`error`), failed reopen attempts and host errors that need no reopen (`warn`), a device back (`info`).
- The outbound channel is bounded (`OUTBOUND_CAPACITY`). Meters and progress are dropped first, as soon as it is half full (the next ones replace them). A sequenced message (`runtime`, `model_changed`) or log that does not fit is dropped; clients recover from the `seq`/`revision` gap with a snapshot. A `document` or `warnings` message that does not fit is sent again, newest, once there is room. Drops are logged (`warn`) once there is room.

## Devices (M4.2b, NFR-08)

One output device (REQUIREMENTS §21: single device, up to 64 outputs). `host::run_show` opens the default device, by its concrete id, at start; the stream never follows the system default (docs/audio.md, Device layer).

**Device threads.** Listing, opening and closing a device can block in the host (CoreAudio holds locks while a device goes), and a cpal stream may not leave the thread that opened it. So the `openlab-device` thread owns the device layer (`DeviceLayer`: `CpalDevices`, or a stand-in in tests) and the open stream, carries out `Open { mixer }` and `Close` in order, and the `openlab-device-list` thread reads the list (`DeviceLister`), so a hung open or close cannot stop hot-plug from being seen. Both answer through the control inbox (`Inbox::Device`). A panic inside the layer is caught on its thread and answered as a failure (a panic during an open or close loses the mixer: `device_unrecoverable`). The control thread never calls the device layer; a GO, a command or a reply never waits for a device (tested with an open that blocks until the test releases it). At most one list read and one open or close are in flight. When the control loop ends, the device thread drops the stream (and the mixer) on its own side. The first open (`devices::start`) is waited for at most `FIRST_OPEN_TIMEOUT` (10 s); then the engine stops with "no answer from the device".

**Supervision** (`devices::Device`, every tick, wall clock injected as `WallClock`):

- **List** every `DEVICE_POLL` (1 s): the last answer serves `list_devices`. A change re-resolves the patches (cheap; they change only through a loss or reopen, see Patches).
- **Loss**: `DeviceMonitor::needs_reopen()` (lost, invalidated, rerouted), or the stream's device gone from the list (hosts that stop calling back without an error): missing from two reads in a row, or from one read while the stream has not called back since the previous read (one read can miss a device that is busy reconfiguring; cpal leaves out a device whose configurations cannot be read at that moment). Other host errors (real-time priority denied, bad buffers) are logged (`warn`); `take_errors` is read here only (single consumer).
- **Close and reopen**: on a loss the stream is closed on the device thread; the mixer comes back at once or as `MixerPending`, polled every tick. Reopen attempts on the same device id follow `reopen_delay_ms` (250 ms doubling to 4 s) by the number of opens since the stream last stayed up `STABLE_UP` (5 s): a device that keeps dropping out right after each reopen backs off further instead of starting again at 250 ms. A failure is logged with the next delay; the device coming back into the list (absent from the previous read) makes the next attempt immediate. A reopened stream continues the mixer as it was (voices, queues, clock).
- **Another buffer size** after a reopen: the lookahead (`CueEngine::set_lookahead`), the `now` offset and the wake rule follow the new block, so every deadline is still committed before the callback that renders it (tested with a device back at 4× the block: the auto-follow lands on its sample). `now` never goes back (a smaller block keeps it where it was until sample time passes it). A callback already under way when the reopen is handled can still miss events of its first block (inherent: the size is known only once the stream runs).
- **Watchdog** (`DEVICE_JOB_TIMEOUT`, 10 s wall clock): an open or close the device thread has not finished, or a mixer the host has not handed back, after that long makes the device unrecoverable: an `error` log and the `device_unrecoverable` warning (naming what is stuck and that openLab must be restarted) replace "reconnecting". A late answer still counts (the warning goes, an `info` log says so). A list read that does not finish is logged once (`error`) and listed as `device_list_stuck`.
- Allocation violations are summed over every stream (M3.21).

**Outage policy** (PROGRESS M3.22; orchestrator decision, QLab check X1 pending):

- **During an outage** nothing renders, so `samples_rendered` (the master clock) stands still; every deadline waits with it. Running cues stay `running`. At once: an `error` log and a `device_lost` runtime notice; the `device_lost` workspace warning while the outage lasts. The loop keeps serving the inbox (GO, commands, requests, replies) and ticking every `MAX_WAIT`.
- **Back within `DEVICE_LOSS_GRACE`** (10 s of downtime, wall clock): voices continue on the sample they were cut on (with no fade-in: the cut was abrupt too; QLab check X1), and auto-follows, waits and fades keep their sample-time distances (tested sample for sample against the same show without the outage). Nothing fails.
- **The grace period counts downtime over `GRACE_WINDOW`** (60 s): every outage in the last minute adds up, so a device that keeps dropping out and coming back fails the cues once 10 s of downtime have built up, as one long outage would (tested with 3.5 s outages: the cues fail in the third).
- **Not back within the grace period**: the patches on the device are reported `device_missing` (`Availability`; loaded cues on them unload, a start is refused as broken, idle cues show broken), then every run with a voice on the device (started in the mixer, or started and waiting for its probe; not loaded runs, not cues still in their pre-wait) gets `ActionFailed { device_lost }`: the cue is `broken` with reason `device_lost` and its voice is released. Every voice the mixer holds is stopped so nothing sounds when the device returns. An `error` log counts the failed cues. Reopen attempts go on.
- **Back after the grace period**: the patches are reported `mapped` again: cues broken by `device_lost` become playable (idle) again (docs/cue-engine.md, Broken cues); cues still in their pre-wait went on waiting and start on their sample.
- **GO during an outage** (and any start): accepted. Within the grace period it is dated on the frozen clock (one block after the sample the outage cut), so it plays when the device returns, as if pressed at the moment of the loss; if the device is not back in time it fails with the others at the grace deadline. After the grace period a start of a cue on the device is refused as broken (`cue_broken`); its timing (auto-continue) runs as for any broken cue.
- **Panic during an outage** stops everything at once (a panic fade could only play after the outage; the loop makes it a hard stop with a second Panic): every cue is idle at once and nothing resumes when the device returns. **Stops** given during an outage are dated on the first sample the mixer renders when it is back (never before the voice's own start or resume), so a stopped voice does not play one more block (`Voices::stop_date`). **Pauses** (Pause, Pause All) given during an outage are dated the same way, so no voice plays one more block either; the mixer then holds the voice that many frames earlier than the engine dated the pause, which is added to the action's length (`ActionLength` sent again while paused), so the engine's end, auto-follow included, stays on the voice's real end (tested sample for sample against a show paused on the cut sample).

**Tests** (`tests/control_devices.rs`): a stand-in device layer whose stream holds the mixer in an `OutputHost` the test renders block by block (`tests/show/device.rs`), on the real device thread, with a test-driven wall clock; the driver waits for the device thread's answers on the inbox, never sleeps. Hot-plug and `list_devices`, a loss found only from the list, a loss within and past the grace period, a slow open, Panic, stop and GO during an outage, a mixer held by the host after close, dropped outputs; output delays on the offline driver.

## Debug allocation guard (M3.21)

Debug builds of the binary install `assert_no_alloc::AllocDisabler` as the global allocator, so the device callback's guard (M3.12) sees allocations; `warn_debug` counts them. The loop reads `DeviceMonitor::alloc_violations()` every tick and reports a rise as an error log and a `realtime_allocation` warning.

## Offline driver

`OfflineShow` builds the mixer, the control state and an `OfflineRenderer`. `step()` hands the inbox to the control state, **settles** the streams, renders one block (captured) and ticks. Rendering faster than real time would let the disk workers fall behind, which a device never allows for long, so settling waits (blocking, outside `tick`, with a 30 s timeout counted in `stalls()`) until every playing stream has written what the block will read and every prepared stream is primed; renders are exact and repeatable. `settle_media()` waits for every probe. `render_only(frames)` renders with no tick (a late control thread). `send`/`transport` hand a message in and return its replies; `take_outbound` returns the broadcasts.

## Deferred

- Choosing the device and its rate/buffer from the machine settings (M5.13; today the default device at start), a watchdog for a device that stops calling back while still listed (PROGRESS M4.2g).
- M4.3 WebSocket server (fan-out of the outbound channel, reply ordering above); M4.4 new/open/save/relink (a newly opened workspace gets a new `Control`'s engine, model sync and `Availability`); M4.5 OSC; M4.7 xruns/underruns in status.
- File-system watching (a file that disappears is found when its stream fails).
- Progress for cues without a voice (pre-waits, Wait cues, post-waits) needs a cue-engine query.
