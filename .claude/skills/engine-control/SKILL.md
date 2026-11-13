---
name: engine-control
description: "Load when changing or reviewing the engine control thread, engine/crates/engine/src/control/: inbox, replies, model_changed, wake/tick, DocId, voices, Action to mixer commands, patch slots, priming, feedback, mixer faults, lost-event reconciliation, performance budgets, pre-show check, hotkeys."
---

# Engine control loop

`Control` is the engine's single control thread: it owns every open workspace, its undo history, its cue engine and the mixer controller, and turns cue-engine `Action`s into mixer, disk-stream and network commands, and mixer/stream/probe events back into cue-engine `Input`s.
  Semantics of the actions live in skill `cue-engine`; audio-thread behaviour in `audio-mixer`; devices and loss policy in `devices`; media probes and the stalled-volume gate in `media-files`; open/save/close and writers in `persistence`; startup/shutdown in `engine-lifecycle`; OSC and Network cue sending in `osc`.

## Must not break
- **The control thread never touches the disk, sockets or the device layer.** Reads and saves go to the document worker and writer, probes and preloads to probe workers, sends to per-destination workers, device open/close/list to device threads. A GO never waits on any of them.
- **No thread or clock of its own.** Time is the mixer's sample counter. Wall clock is used only by the device policy and the probe/document watchdogs.
- **Every workspace edit reaches the cue engine** (only through `DocModel::edit()`; a forgotten signal must not compile). A refused or no-op command changes and broadcasts nothing.
- Exactly one `result` per command/transport/undo/redo/request, and `model_changed` goes out before it.
- `VoiceId`s are engine-wide and never reused; each has an owning doc. A voice event no open doc owns is dropped.
- A patch slot is never handed out while a voice may play through it, and no edit re-routes or silences a voice that is sounding (except a playing cue's own patch change, which follows by crossfade).
- A start that will never play fails (`StartFailed`: cue skipped, auto-follow fires) rather than showing as running with no sound.
- No feedback is handed to the cue engine before the sample it reports (`now > at`).
- A lost mixer event must never leave a cue running for ever (reconciliation).
- Every volume limit holds for every level the model keeps and every playing fade.
- Debug and release behave alike. Test-only fault switches never compile into a shipped build.

## Shape
- Two drivers call the same `Control::handle(message)` and `Control::tick()`: `runner::run` (control thread on a device; the engine starts with no doc open; `--open FILE` goes through the same `open` request a client sends) and `offline::OfflineShow` (renders one block, then ticks; no sleeps or wall clock).
- `Control` is one `Engine` (mixer controller, `Backend`, disk streamer, `VoiceIds`, `Patches`, probe pool, media watch, workers, outbound channel) plus its `Doc`s in open order, at most `MAX_DOCS`. A `Doc` is workspace, `History`, `CueEngine`, `Voices`, `MediaCatalog`, `PlanChecks`, `NetOut`, file state. `Backend` (`Device` or `Offline`) is what only a device has.
- **`DocId` is handed out in open order and never reused**, so a late answer or event of a closed doc cannot be taken for another's. Worker answers are tagged with the doc (and a document-worker answer names its request) and dropped for a doc closed since.
- **One wake (`Control::tick`)** runs the engine-wide steps once, with zero docs too, then each doc, then the closing steps:
  1. device (a loss or return acts on every doc); mixer events routed to the owning doc; catalog key changes and media watch batches; one meter read when due;
  2. each doc's tick (events, streams, deadlines, priming, progress, warnings, saves);
  3. probe and document-worker watchdogs; freed slots handed to docs waiting; autosave removals; settings, health, status; `MixerController::pump()`.
- Client messages arrive as `Inbox::Conn { doc, .. }` (a WebSocket or OSC connection bound to a doc, or the app connection `None`). `Control::app_scope` takes the app-scope messages first: `new open open_recovery scan_recovery list_devices log_tail set_output_overrides close panic hotkey`. Everything else goes to the sender's doc (`no_workspace` for an app connection or a closed doc). `Inbox::Client` (tests, offline driver) addresses the first doc.
- Every broadcast is an `Outgoing { doc, message }`: a doc's reports tagged with it; the engine's own (`log`, `log_entry`, `status`, `workspaces`) with `None`.

### Threads
| Thread | Owns | Notes |
|---|---|---|
| control `openlab-control` | `Control` | crossbeam inbox in; bounded tokio outbound channel out; SPSC to the audio thread |
| probe workers (2, all docs share) | `probe`, `preload_within` | two job queues, high priority first; answers via `Inbox::Probe(doc, _)` |
| waveform reader | `build_waveform` | one read at a time, bounded queue. **Never a probe worker**: a GO waits on probes. A read past its timeout is abandoned and fails `io` (not counted as a stuck probe worker); after `MAX_ABANDONED_READS` waveforms answer `io` until restart |
| document worker | show reads, recovery scans, `settings.json` writes | answers via `Inbox::Doc` |
| writer `SaveWriter` | serialising/writing saves and autosaves | polled each tick |
| device, device list | the cpal layer and its stream | via `Inbox::Device`; control never calls the device layer |
| disk workers (2), openers (4) | decoders, resamplers, rings; openers only open files | `prepare` does no I/O; a hung open stalls only its stream |

## Inbox and replies
- `Inbox::Client { message, reply }` is already decoded and authenticated. `reply` is an unbounded tokio channel, outbound is bounded with `try_send`: the control thread only ever sends without blocking.
- Transport actions map to cue-engine inputs and are handled at once; `result` ok means "handed to the cue engine". `hard_stop` = `Stop` with no fade; `stop_all`/`hard_stop_all` = `StopAll`.
- **Command / undo / redo** order: `History` through the write guard (tells the cue engine `model_changed_cues(footprint)`, or `model_changed()` when `Footprint::is_model_wide`: settings, patches, lists), `MediaCatalog::sync_cues(footprint)`, one `model_changed` broadcast, a `document` broadcast, the `result` with that revision, then one `Tick` so the edit takes effect now. The network layer keeps `model_changed` N before a `result` with `ok.revision = N`.
- An undo/redo step that removes an active cue is performed and the engine stops the cue as for a delete.
- `unsignalled_model_changes()` is checked after every engine call; a rise is logged at `error`.
- **`RollbackFailed`** (a wrong inverse left the workspace partly changed; core cleared history) is an edit nobody can undo: error log, `revision += 1`, full `model_changed()` signal (anything may have changed), catalog `sync` (probe results kept), `document` plus a whole `snapshot` to every client, then the `command_refused` reply (`DocCx::partly_changed`).
- Requests: `snapshot` answers `snapshot` then `result`; `list_devices` answers from the last list read (no device call); `relink` and document requests see `persistence`/`media-files`.
- `Inbox::Shutdown` or `goodbye { shutdown: true }` ends the loop, which hard-stops the show (two Panics).
- `Inbox::Trigger { source, detail }` precedes each client/OSC transport action; the loop logs at most 5 per source per 10 s and summarises the rest.

## Model changes
- A doc's workspace and cue engine live in `DocModel` (`control/model.rs`) with private fields. The workspace is written only through `DocModel::edit()`, a guard whose drop calls `engine.model_changed()`; `touched(footprint)` / `ended(footprint, result)` end it with `model_changed_cues(footprint)` instead.
  Every path (command, undo, redo, relink, save-as re-spelling, `SetArmed`, a copy's fresh id) goes through it. A footprint that misses a cue is not caught by types; debug builds' edit cross-check counts it (skill `cue-engine`).
- An edit that changes nothing (`Ok(false)`, empty footprint) costs no rebuild, revision, `model_changed`, `document`, or dirty flag.
- Exactly one `model_changed { revision, upserts, deletes }` per model edit; revision rises by one per message, so a gap means the client asks for a `snapshot`. A newly opened doc is a `snapshot`, not a diff.
- Contents come from `Command::footprint()` (a transaction is the union; undo/redo use the step about to be applied, `peek_undo`/`peek_redo`), read back with `model_changes`, never by diffing workspaces.
- **`deletes` are applied before `upserts`**: every removed or moved cue is a delete, every named cue that exists after is an upsert at its final `(list, parent, index)`, in tree order. A named cue inside another named group is not sent twice (the group upsert carries its subtree). So several moves in one transaction land correctly.
- Settings, cue list, patch and network patch edits add those upserts/deletes first (a reordered collection sends every item at its final index), tell the engine with `model_changed()` (patches decide which cues are broken; lists own playheads) and call `sync_patches`.
- Coalescing: `History::execute_coalescing`; a key over 128 bytes is `bad_request`.
- `Workspace::apply_changes` is the reference client; property tests assert mirror == engine workspace after random command/undo/redo sequences.

## Time and wake
- `block` = frames per device callback (offline: step size). **`now = samples_rendered + block`** is the earliest sample a command can still hit. `EngineConfig::lookahead = block`, so a call commits deadlines in `now .. now + block` and no voice start is scheduled beyond it.
- `tick` runs `Input::Tick` only when `engine.is_due(now)`. `runner::run` waits until a message, the next deadline enters the window, a deferred report is due, or `MAX_WAIT`, then ticks. A message ends any wait at once, so GO is never delayed. Wake jitter never shifts audio: every transition is computed from sample time.
- **No spin on a stalled clock.** A device can stall without an error; a deadline just outside the window would then give a tiny hint over and over (a full tick per wake). When the last tick saw `samples_rendered` unchanged, the hint is at least `MIN_STALL_WAIT` (or a quarter block if shorter). The first wake after progress is not floored; an outage keeps its `MAX_WAIT` hint.
- Tests pass a fixed `EngineConfig::seed`; the engine seeds from `entropy_seed()` so random groups differ per run. Each doc's seed is the configured seed plus its `DocId`.

## Voices and actions
Every run segment gets its own `VoiceId` from the engine-wide `VoiceIds`; `RunId <-> VoiceId` kept both ways; an id is forgotten once its doc's table no longer knows it (checked after each meter read) or the doc closes.

| Action | Carried out |
|---|---|
| `LoadVoice` | slice table, `prepare` (or wait for the probe); kept as the run's stream |
| `UnloadVoice` | stream dropped |
| `StartVoice { run, at, offset }` | loaded stream, else the cue's primed one (offset 0 only), else a fresh `prepare`; `VoiceStart` with the model's levels; then `ActionLength` (below) |
| `StopVoice { at, fade }` | mixer `StopVoice` with the curve; record goes with `VoiceFinished` |
| `PauseVoice` | native mixer pause (position and ring kept) |
| `ResumeVoice { run, paused, at }` | mixer resume re-keys the voice to a fresh id for the fresh run |
| `Ramp` | mixer `Ramp`, or one `RampSet` for a run of ramps of one voice with the same date and fade; no gangs (Fade and level-edit values are already per output). Fade cues and level edits on a playing cue are carried out alike |
| `DevampVoice { stop }` | mixer `Devamp`. A pending devamp is re-sent for the fresh run after a resume; a native pause keeps the first and answers `DevampScheduled` tagged with the fresh run, not `DevampIgnored` |
| `PreviewStart` / `PreviewStop` | as start (offset 0, never a primed stream) / stop, on the cue's own patch (no audition output exists) |
| `SetArmed` | applied to the model through its guard (engine told) before the engine's next call, then `model_changed` with each changed cue; not an undo step; honoured for the rest of that call |
| `SendOsc` | to the destination's worker (never control-thread I/O, never audio thread); a failure is a warning, not feedback; names are looked up off-thread; the destination is never probed. See skill `osc` |

- **Routing at `VoiceStart`**: master; cue output at its level (muted or beyond the patch's outputs: silent); listed crosspoints; the patch's matrix; integrated fade-in (none when starting part way in) and fade-out; dB to gain with core's `db_to_gain`. Levels are read once at start; an edit while playing reaches the voice only as engine `Ramp`s, never a restart.
- **`ActionLength`** after a start of a finite action: `offset + table frames (+ delay if held)`, so the engine owns the end as a deadline on the exact sample (auto-follow and playlists do not wait for the mixer report).
  After `DevampScheduled` send the new length (leave sample plus the rest of the table; the leave sample alone for a devamp stop); none while what follows loops for ever. Previews send none. An offset at or past the table's end is an empty plan: nothing plays and `ActionLength { frames: offset (+ delay) }` ends the action on its start sample.
- **Pause by stop and restart** (a binary without native pause) must restart with the Fade levels and ramps from `fade_ramps(cue, at)` on the resume sample (start with each `value`, then `Ramp` for each still moving), and must never report its stop as `ActionEnded`. This engine pauses natively.
- **Slice table** (`plan::cue_slices`): trims, markers and play counts, then skip `offset` frames with loops unrolled. A table that cannot be built fails the run if started anyway (`StartFailed { stream_failed }`, or `ActionFailed` if only loaded, plus `bad_plan`). As soon as the file is probed the plan checks make the cue broken (`slices_unplayable`) before any GO.
- **Waiting for a probe**: a start/load whose file is not probed waits (`probe_wait` notice); commands for it are queued and replayed after the start, a pause holds it, a stop drops it. On the answer it starts at `max(start, now)` with `cold_start` (delay counted into `ActionLength`); a failed probe fails the run.
  **A start held past `PROBE_HOLD_LIMIT` (sample time) fails with `StartFailed { stream_failed }`** and `probe_timeout`: cue skipped (auto-follow fires), broken until reset or started again; a late answer clears the file's static problem but does not restart it.
- **A stream that never opens** (volume hangs after the probe): the voice starts cold, plays silence, and holds one opener, never a disk worker.
  Still not primed `STREAM_START_LIMIT` after its start (paused time excluded): stopped at once with `stream_timeout`, and its run fails when the mixer reports the end (`StartFailed` if nothing was written), so it never plays late if the volume answers. A shorter cue has already completed on its planned end; its silent voice gets the same stop.
- Probe order, stuck-worker replacement and the stalled-volume gate: skill `media-files`. A re-probe keeps the last answer (primes, preloads) until the new one arrives.

## Patches and levels
- Each patch uses the mixer slot allocated to its id (reference `patch-slots`). Each is resolved with `resolve_patch` against machine settings and the **playable** devices and sent as `Availability` on open and on every change (edits, device list, loss, reopen). The engine plays on one device: a patch mapped to another resolves `device_missing`; during an outage no device is playable.
- A mapped patch routes cue output `o` to its device channel at patch master x output level (muted 0; polarity: negative gain); all else 0. Only changed entries are sent. A workspace has no global master: `SetMaster` is never sent (unity). Level changes ramp with the de-zipper.
- **Volume limits** (`settings.output_max_db`/`output_min_db`): every level the model holds is within them; a command clamps what it sets; a limit change puts every level above it at the limit **in the same undo step** (`Command::prepare`, one transaction). The cue engine pulls a Fade's or moving level within a narrowed limit in the same call, so nothing plays louder than the maximum 10 ms after it is lowered.
- Default mapping and the set-mapping request: reference `patch-slots`.
- Mixer outputs have no per-device-output setting and no per-output delay; a patch output's level, mute and polarity are its matrix gains.

## Priming and preload
- After every playhead change, model change, probe answer, landed preload and freed show voice, the loop keeps primed (stream prepared and filling): the next `PRIME_CUES` from each playhead, their targets (a group primes its audio cues), and every cue with a hotkey or OSC trigger; only armed audio cues with media and no run, at most `MAX_PRIMES`.
- A prime is used by a start from the top only if file, probe, trims/slices and source (file or preloaded samples) still match; otherwise it is released. A start without a prime is cold: audible once its ring fills, `cold_start` notice, **never a failure**.
- Primes are one pool across docs, at most half the streamer's `max_streams` (primes must never take a permit a start needs; a start finding none fails `stream_failed`). Each doc keeps what it wants while all wants fit; otherwise max-min fair shares: a doc over its share lets go of its least urgent primes next pass, a doc under it fills at most 4 rings per pass (no disk burst).
- **Preload** (a cue asks, or the probe advises `preload_advised`): decoded on a probe worker within `preload_budget`, one budget for all docs, given back when no cue uses the file or a doc closes. Over budget or failed: it streams, listed `preload_over_budget` / `preload_failed`.
  The catalog counts per file the cues using it and asking for preload (from each edit's footprint); only a change of mind acts, so an over-budget file is not retried when budget frees elsewhere. A voice playing from samples keeps them; a preload answering after release is ignored.

## Several docs
- Panic and hotkey are **app-scope** and fan out to every doc; Pause All, Stop All, GO and the rest act on the doc they are sent to.
- **Panic** (`panic_all`, Esc or `transport: panic` on an app connection) panics every doc as a `panic` transport (twice within the fade is a hard stop in each).
- **Hotkeys**: `Control::hotkey` (`phase: down`) sends `Start` for every cue of every doc that `Workspace::cues_with_hotkey` returns (same key and modifiers, workspace order; shared hotkeys all start, no warning). A doc whose workspace map (transport, selected-cue or cue-editing keys) uses that key starts nothing (the workspace action wins; that action is the front window's, not the engine's; the engine does nothing on a cue-editing key).
  `Control::hotkey_released` (`phase: up`) sends `TriggerReleased` to the same cues; a cue whose second trigger is not on release ignores it. A press on a paused cue resumes it. Semantics: skill `cue-engine`.
- **Open** is refused (`command_refused`) while another document request runs (one at a time, engine-wide) or with `MAX_DOCS` open, and with `io` while reads are stuck.
  The document worker builds the doc's parts (`DocParts`: workspace, followed cue engine, catalog index with paths resolved, slice-plan checks), so installing on the control thread is an insert plus probe jobs, slot commands (`SnapPatch`), first reports, `result { opened { doc } }` and the `workspaces` list. Install and close stay under half an audio block.
- **Close** order: `no_workspace` if not open; `unsaved_changes` unless `discard_changes` (then the autosave it wrote is removed after any write in flight).
  Cues stop as a double Panic stops them; orphans and every voice it has in the mixer are hard stopped; then its slots are zeroed and freed on the same sample (`Patches::release_doc`); voice-id owners, logged states, warnings, watched files and waiting probe jobs dropped; the doc is dropped on the document worker (preloads can be hundreds of MB). Other docs are untouched.
- **Device loss** stops every doc's cues (policy: skill `devices`); logged once with the total, each doc's clients get `device_lost` with their own count. A reopen with another block size sets the new lookahead in every doc's cue engine before the engine's block changes; if one cannot, all keep the old one and a warning says commands may land late.
- Shared and counted once: probe pool, preload budget, prime pool, media watch, look-up bound, network override, meter read, health, status.
- **Zero docs**: every engine-wide step still runs; workspace messages answer `no_workspace`; app-scope messages (including `new`/`open`) are still carried out; shutdown still shuts down. The engine starts with none (`Control::empty`); `Control::new(ws, ..)` is `empty` plus doc 0.

## Outbound
- Policy and message inventory: reference `outbound`. The channel is bounded (`OUTBOUND_CAPACITY`). Meters and progress drop first at half full; sequenced messages (`runtime`, `model_changed`) and logs that do not fit are dropped and clients recover by `seq`/`revision` gap with a snapshot; `document` and `warnings` that do not fit are re-sent, newest, once there is room.

## Offline driver
`OfflineShow::step()` hands the inbox to control, **settles** the streams, renders one block, then ticks.
  Settling waits (blocking, outside `tick`, with a 30 s timeout counted in `stalls()`) until every playing stream has written what the block will read and every prepared stream is primed; that makes renders exact and repeatable (a device never lets disk workers fall far behind; a faster-than-real-time render would). `settle_media()` waits for probes and started preloads; `render_only(frames)` renders with no tick (a late control thread).

## Performance budgets
Numbers are asserted in `engine/crates/engine/tests/perf.rs` (a 2,110-cue show built in the test); the CI performance gate runs the timed ones on an Intel runner. NFR-24/25 stay unticked (minimum-spec hardware is not measured).

| Budget | Limit |
|---|---|
| Idle loop (every `cargo test`, virtual time) | one wake per `MAX_WAIT`, never zero-wait; no work, no sends, no plan rebuilds on a wake; empty workspace wakes at the same rate |
| Open (read, parse, validate, control state, probes, snapshot encoded) | < 1.5 s |
| One edit (command, `model_changed`, engine follow-up, next tick) | < 12 ms |
| Idle tick | < 1% of a core at one wake per 5 ms |
| 4x the cues | < 12x the time (linear is 4x, quadratic 16x) |
| Media watch with 2,000 files | < 1% of a core |
| Second 2,000-cue doc open | idle tick <= 3x one doc, edits <= 1.5x + 0.5 ms; install, the wake after, and close each under half an audio block |
| 64 stereo fading cues, 512-frame blocks | >= 10x real time; two docs <= 1.25x slower than one |

- Bookkeeping follows the edit's footprint, not the show (cue index position maps, problem facts, catalog key set, `PlanChecks` examining only touched cues, `model_changes` reading through the index). Counters pin it (`whole_show_passes`, `examined`, `walks`, `plan_builds`); do not add a per-edit or per-probe-answer pass over every cue. Priming finds cues through the cue index; its one per-pass walk is a flag test for triggers.
- Install/close stay under half a block by doing work off the control thread (paths resolved and docs dropped on the document worker).

## Pre-show check (engine half)
`preshow_check` (`control/preshow.rs`) is built on the control thread from state it holds, no I/O: `CueEngine::broken_cues()` (folds in probes, watch, patch availability), patch resolutions, the workspace's cues, and the `warnings` list (refreshed if stale). Media is as fresh as the last probe and watch event; never re-read at request time.
  `summary.total` counts broken cues plus `system` entries that are problems (`preload_over_budget` is information); an unused patch is listed, not counted. Network destinations are `not_checked` when present, `missing` when a cue names one the workspace lacks.

## References (read only when the task needs it)
- `references/feedback.md`: the mixer/stream/probe event to cue-engine input table, orphaned tails and aliases, reconciliation of lost events, mixer stage faults. Read when changing feedback handling, `control/faults.rs`, or anything that ends or fails a run.
- `references/patch-slots.md`: slot allocation, retire/resize/follow rules, default mapping, `SetPatchMapping`. Read when touching `control/patches/`, `sync_patches`, or patch mapping.
- `references/outbound.md`: what the loop sends and when (runtime, warnings, progress, meters, log, status), the slice-plan and length plumbing. Read when adding or changing an outbound message.
