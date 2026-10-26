# openLab control loop (`openlab-engine::control`)

The engine's control thread (ARCHITECTURE §3.1, §3.2): it owns the workspace, its undo history, the cue engine and the mixer controller, and wires them to the disk streams. Contracts it follows: [cue-engine.md](cue-engine.md) "Actions for the binary (M4.2 contract)", [audio.md](audio.md) "Threads and queues", [protocol.md](protocol.md). Status: M4.2a (this page); device lists, loss and reopen are M4.2b (see [Deferred](#deferred)).

## Shape

- `Control` holds the state and logic and has **no thread or clock of its own**. Time is the mixer's sample counter (`MixerController::samples_rendered`), whoever advances it. Two drivers call the same `Control::handle(inbox message)` and `Control::tick()`:
  - `runner::run(control, &inbox)` — the control thread against a running device (`host::run_show` opens it with `open_output`; `openlab-engine SHOW_FILE` plays a show on the default device until stdin closes);
  - `offline::OfflineShow` — the offline render driver (ARCHITECTURE §3.6): render one block into memory, then tick. Tests drive it step by step; no sleeps, no wall clock.
- `Backend` is what only a device has (`Device { monitor }` or `Offline`): today the allocation-violation counter; M4.2b adds loss and reopen there.
- The loop logic never reads the wall clock. The only wall-clock value is how long `runner::run` waits on the inbox (`wait_hint`).

## Threads

| Thread | Owns | Talks via |
|---|---|---|
| control (`openlab-control`) | `Control` | crossbeam-channel inbox in; bounded outbound channel (`OUTBOUND_CAPACITY` 4096) out; `MixerController` (SPSC) to the audio thread |
| probe workers (`openlab-probe-N`, `PROBE_WORKERS` = 2) | `probe`, `preload_within` | job channel from control; answers through the control inbox (`Inbox::Probe`) |
| disk workers (`DiskStreamer`, 2) | decoders, resamplers, rings | `prepare` from control (no I/O); `StreamHandle` atomics back |
| audio (device) / caller (offline) | `Mixer` | SPSC queues, atomics |

**The control thread never touches the disk.** Probing a file decodes it whole (seconds for a long compressed file) and preloading decodes it into memory, so both run on the probe workers; `DiskStreamer::prepare` does no I/O (the disk worker opens the file). Tested with a named pipe whose probe blocks until a writer appears: the loop keeps ticking and a GO meanwhile does not block (`control_media.rs`). Only the offline driver waits for the disk, outside `tick` (below).

## Inbox and replies

`Inbox::Client { message, reply }` carries a `ClientMessage` already decoded and authenticated by the network layer (M4.3). Every `command`, `transport`, `undo`, `redo` and `request` gets exactly one `result` on `reply`; broadcasts go to the outbound channel. `Inbox::Probe` is internal feedback; `Inbox::Shutdown` (or `goodbye { shutdown: true }`) ends the loop, which then hard-stops the show (two Panics).

- **Transport** → the cue-engine input of docs/protocol.md's table (`hard_stop` = `Stop { fade_out: None }`), handled at once; `result` ok means "handed to the cue engine".
- **Command / undo / redo** → `History` on the workspace, then `engine.model_changed()` before the engine's next call (M4.2 contract), a `model_changed { revision }` broadcast, a `document` broadcast, then the `result` with that revision, then one `Tick` so the edit takes effect now (a deleted running cue stops on the next block). A refused command changes nothing: `command_refused`. `unsignalled_model_changes()` is checked after every engine call; a rise is logged as an error.
- **Requests**: `snapshot` answers `snapshot` then `result`; every other request is `unsupported` until M4.4.
- **Ordering across channels**: a command's `model_changed` goes to the outbound channel before its `result` goes to `reply`. M4.3 must keep that order per connection (e.g. drain the broadcasts before writing a reply).

`model_changed` carries **no upserts or deletes yet** (M4.1a): it is a revision bump, so a client sees the change and can request a snapshot. Arm/Disarm cues' `SetArmed` changes go through the same path as a runtime change (no undo step; the document becomes dirty, since a save stores it — QLab confirmation in PROGRESS M2.17).

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
| `StartVoice { run, at, offset }` | the loaded stream, else the cue's primed one (offset 0), else a fresh `prepare`; `VoiceStart` with the model's levels; `StartVoice` at `at` |
| `StopVoice { at, fade }` | mixer `StopVoice` (fade with the curve); the record goes with the `VoiceFinished` |
| `PauseVoice { at }` | mixer `PauseVoice` (native pause, M3.18: position and ring kept) |
| `ResumeVoice { run, paused, at }` | mixer `ResumeVoice { voice: old, new_id }` re-keys the voice to a fresh id for the fresh run |
| `Ramp` | mixer `Ramp` (`Master` / `CueOutput` / `Crosspoint`), no gangs (the Fade's values are already per output) |
| `DevampVoice { stop }` | mixer `Devamp` |
| `PreviewStart` / `PreviewStop` | as `StartVoice` (offset 0, never a primed stream) / `StopVoice`, on the cue's own patch until PB-09's audition output (M2.21) |
| `SetArmed` | applied to the model before the engine's next call, then `model_changed()` |

- **Routing** (`VoiceStart`): master `master_db`; cue output `o` at its level (muted or beyond the patch's outputs: silent); listed crosspoints (muted: silent); the patch's mixer matrix; the integrated fade-in (none when starting part way in) and fade-out. dB → gain with core's `db_to_gain` (the mixer's conversion).
- **Slice table**: `plan::cue_slices` turns trims, markers and play counts into `plan_at_rate`'s table (markers outside the trims are dropped; the slice the out point falls in plays as the marker that ends it says), then skips `offset` frames of the action with loops unrolled. An offset at or past a finite action's end plays nothing: the action ends on its start sample. A table that cannot be built (`LoopTooShort`, M3.17, or bad markers) fails the run (`ActionFailed { stream_failed }`, a `bad_plan` notice) and is listed in the workspace warnings as soon as the file is probed (the pre-show view; a `CueProblem` for it would need core and protocol changes — open question in PROGRESS).
- **Waiting for a probe**: a start (or load) whose file has not been probed yet waits; commands for it (ramps, devamps) are queued and replayed after the start, a pause holds it, a stop drops it. When the probe answers it starts at `max(start, now)` from its head with a `cold_start` notice; a failed probe fails the run.
- **Patches** (`Patches`): workspace patch `i` uses mixer matrix `i` (more patches than matrices: `unmapped`). Each patch is resolved with `resolve_patch` against the machine settings and the device list and sent to the engine (`Availability` on open, `PatchStatus`-equivalent `Availability` on every change). A mapped patch's matrix routes cue output `o` to its device channel at patch master × output level (muted: 0; polarity: negative gain); everything else 0. Only changed matrix entries are sent. The workspace master goes to `SetMaster`.

## Priming (ARCHITECTURE §3.5)

After every playhead change, model change and probe answer the loop keeps primed (a stream prepared and filling, ready to start): from each list's playhead the next `prime_cues` (16) cues, the targets of those cues (a group target primes its audio cues), and every cue with a hotkey or OSC trigger — armed audio cues with media and no run, at most `max_primes` (64). A prime is used by a start from the top of that cue if the cue's file, probe and trims/slices still match; otherwise it is released. A start without one is cold: it is heard once its ring fills, with a `cold_start` notice, never a failure.

**Preload** (AUD-05): a file whose cue asks for it, or whose probe advises it (`preload_advised`: damaged files that seek slowly), is decoded into memory on a probe worker within the workspace-wide `preload_budget` (default 512 MiB); a file that does not fit streams and is listed as `preload_over_budget`. Memory goes back when no cue uses the file.

## Feedback

Mixer events, stream state and probe answers become cue-engine inputs:

| From | Input |
|---|---|
| `VoiceFinished` (current id) | `ActionEnded { cue, run, at }`; `ActionFailed { stream_failed }` instead if its stream failed |
| `VoiceFinished` dated at or after a pending pause (the voice ended as the pause applied) | forwarded (the engine ignores it while paused); the record is kept, and the resume that follows reports `ActionEnded` with the **fresh** run on the resume sample (M2-E (a): the mixer ignores the resume of a voice it no longer has) |
| `VoiceFinished` under an old id after a resume was sent | the voice ended before the resume applied: `ActionEnded` with the fresh run, dated no earlier than the resume |
| `VoiceRejected` | `ActionFailed { voice_rejected }`; a refused resume also stops the voice still paused under its old id |
| `StreamHandle::error` (polled every tick) | `ActionFailed { stream_failed }` once per run, and the file is probed again (it may have gone or changed) |
| `DevampScheduled` / `DevampIgnored` | `Input::DevampScheduled { at }` / `DevampIgnored` for the run the id stands for (an old id's run is stale and ignored by the engine) |
| probe answer | `MediaStatus` in one `Availability` per tick: `available` (damaged-but-playable included; its damage is listed in the warnings), `missing` (`NotFound`), else `unsupported { reason }` |

- **No feedback before its sample**: a report dated `at` (a re-reported end on a resume sample, the end of an empty action) is held until `now > at` (answers PROGRESS M2-E (c) for this binary: a voice event is never handed over at a `now` at or before the sample it reports; mixer events always report rendered samples, so they qualify at once).
- An old id stays an **alias** of the voice until the voice is gone, so meter entries under it (one more after a re-key, docs/audio.md) and late events still name the cue.

## Reconciliation of lost events

The mixer drops events when its queue (1024) is full and counts them (`dropped_events`). A lost `VoiceFinished` would leave a cue running for ever. The voice-meter read (every meters period, ~30 Hz) lists every voice the mixer holds, paused ones included. A started voice whose start sample was rendered before the previous read and that is missing from **two reads in a row**, while `dropped_events > 0`, is taken as ended: `ActionEnded` on the read's sample (or `ActionFailed { stream_failed }` if its stream failed). A lost `VoiceRejected` is therefore reported as an end. The first loss is logged (`warn`). Tested by overflowing the queue with loop events while the control thread is held off (`control_feedback.rs`).

## Outbound

- `runtime { seq, changes }`: one message per engine call with changes, from `RuntimeChange::from_action` read right after the call (a broken state carries its reason), plus binary notices as `warning { notice }`: `cold_start`, `bad_plan`, `stream_failed`.
- `warnings`: the workspace-level list, replaced when it changes: `damaged_media`, `preload_over_budget`, `bad_plan` (per cue), `realtime_allocation`.
- `progress`: show cues with a playing voice (elapsed and remaining in the action, remaining `null` while it loops for ever), at most 30 Hz of sample time, only while one plays.
- `meters`: per mapped patch the device outputs it is routed to, per cue its voices' peaks per cue output (`read_output_meters_into` / `read_voice_meters_into` into preallocated buffers), at most 30 Hz, while audio plays plus one message after.
- `log`: engine faults (`error`), audio-thread allocations (`error`, M3.21), lost mixer events (`warn`), unsignalled model changes (`error`).
- The outbound channel is bounded: a full channel drops the message (counted); clients recover from the `seq`/`revision` gap with a snapshot.

## Debug allocation guard (M3.21)

Debug builds of the binary install `assert_no_alloc::AllocDisabler` as the global allocator, so the device callback's guard (M3.12) sees allocations; `warn_debug` counts them. The loop reads `DeviceMonitor::alloc_violations()` every tick and reports a rise as an error log and a `realtime_allocation` warning.

## Offline driver

`OfflineShow` builds the mixer, the control state and an `OfflineRenderer`. `step()` hands the inbox to the control state, **settles** the streams, renders one block (captured) and ticks. Rendering faster than real time would let the disk workers fall behind, which a device never allows for long, so settling waits (blocking, outside `tick`, with a 30 s timeout counted in `stalls()`) until every playing stream has written what the block will read and every prepared stream is primed; renders are exact and repeatable. `settle_media()` waits for every probe. `render_only(frames)` renders with no tick (a late control thread). `send`/`transport` hand a message in and return its replies; `take_outbound` returns the broadcasts.

## Deferred

- **M4.2b**: device list and hot-plug (`output_devices`, `PatchStatus` on change), device loss and reopen (`needs_reopen`, `MixerPending` polled each tick, `reopen_delay_ms` backoff, M3.22's policy, `ActionFailed { device_lost }`), patch output delays (a device-output setting, `SetOutput`), `outputs_dropped` warning. The seam: `Backend::Device` and `ControlConfig::devices`; `host::run_show` keeps the stream on its own thread.
- M4.1a upserts; M4.3 WebSocket server (fan-out of the outbound channel, reply ordering above); M4.4 new/open/save/relink (a newly opened workspace gets a new `Control`'s engine, model sync and `Availability`); M4.5 OSC; M4.7 xruns/underruns in status.
- File-system watching (a file that disappears is found when its stream fails).
- Progress for cues without a voice (pre-waits, Wait cues, post-waits) needs a cue-engine query.
