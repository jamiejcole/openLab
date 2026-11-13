---
name: audio-mixer
description: "Load when changing or reviewing the real-time mixer in engine/crates/audio (mixer/, controller.rs, gain.rs, meter.rs, offline.rs): commands, horizon, voices, pause/resume, fault stages, fade curves, routing, ganging, queues, meters, the offline driver, or anything that sends commands to the mixer."
---

# Real-time mixer (`openlab-audio`)

The mixer renders voices into the device buffer on the audio thread. The control thread talks to it only through `MixerController`. Decoding and disk streaming are skill `audio-streaming`; the device callback and loss policy are skill `devices`; who sends the commands and why is skill `engine-control`. The generic RT rules (no alloc, locks, syscalls, logging) are in CLAUDE.md.

## Must not break
- Nothing `process` reaches allocates, locks, makes a syscall or logs. All buffers are sized in `Mixer::new` from `MixerConfig`. Each RT path is tested under `assert_no_alloc`; add a case when you add a path.
- A panic in `process` never crosses the FFI boundary: `process_guarded` (`catch_unwind`) outputs a silent block, counts the fault, and still advances the clock.
- A fault is confined to its unit (voice, command, patch, output, master, meters) and escalates only at `FAULT_LIMIT` within `FAULT_WINDOW`. Voices and pending commands survive every reset.
- A queued Stop or Panic is never overtaken by a start that was already queued: the engine schedules starts no later than one block ahead, and held or pending commands cannot be withdrawn.
- A stop of the whole show is not held up behind ramps: a batch of only stops overtakes batches of only ramps.
- Output is independent of block size and of how commands were batched. Equal-`at` commands apply in send order.
- A command is never lost: when a queue or the pending set is full it waits or applies late.
- Gains reach the output only through the de-zipper or an explicit ramp: no gain step may click, and a falling ramp is the time-reverse of the rising one.
- Meter peaks are never reported under another voice's id.
- Never free a box on the audio thread: leak it and count `leaked_returns` (expected 0).
- A resume dated before its pause is ignored; the control side guarantees it never sends one (skill `cue-engine`: commands for one voice never go backwards in time).

## Threads and queues
- `Mixer::new(config)` returns `MixerController` (control thread) and `Mixer` (audio thread). They share only preallocated `rtrb` SPSC queues and atomics. `openlab-audio` does not depend on `openlab-core`; the engine translates core `Action`s into mixer commands.
- The controller schedules time-stamped `Command`s, polls `Event`s, frees what the mixer returns, and reads the clock (`samples_rendered`), counters and meters.
- Counters: faults, faulted voices and stages, `underrun_frames`, `dropped_events`, `leaked_returns`, `pending_full_blocks`, xruns.
- `Mixer::process(out, frames)` fills an interleaved `f32` buffer. Device callback and offline renderer both call `process_guarded`.

| Queue | Direction | Capacity | Contents |
|---|---|---|---|
| commands | control to audio | `COMMAND_CAPACITY` 1024 | `Command { at, kind }` |
| events | audio to control | 1024 | small `Copy` `Event` with exact sample `at`; when full, dropped and counted. Not sorted by `at` within a block |
| returns | audio to control | commands + `PENDING_CAPACITY` + voices + 1 | `Box<VoiceStart>` and `RampSet` lists the mixer is finished with |

## `MixerConfig` limits (validated in `Mixer::new`)
| Field | Default | Limit |
|---|---|---|
| `sample_rate` | 48000 | 1 to 384000 |
| `output_channels` | 2 | 1 to 64 (device outputs: the columns of every patch matrix) |
| `cue_outputs` | 128 | 1 to 128 (`MAX_CUE_OUTPUTS`): the rows of every patch matrix, the most a voice can use |
| `max_block_frames` | 1024 | 1 to 8192; internal chunk size, any device block size is accepted and split |
| `max_voices` | 128 | 1 to 4096 |
| `max_voice_channels` | 8 | 1 to 64 |
| `patches` | 8 | 1 to 64 |
| `lookahead_ms` | 500 | 10 to 60000 |

## Faults
- The mixer notes the stage it is in (a plain field write) and `process_guarded` blames a panic on it.
- Each unit keeps the blocks of its last `FAULT_LIMIT` (3) faults in a fixed array. That many within `FAULT_WINDOW` (16) `process_guarded` calls (about 170 ms at 512 frames, 48 kHz) is the limit. A unit faulting every block or every other block is dealt with; a one-off fault, or faults further apart than the window, cost only their blocks.

| Stage | On a fault | At the limit |
|---|---|---|
| a voice's render (pool index) | nothing more; the window resets when the slot plays another voice | finished with `FinishReason::Faulted` (start goes back, meters flush, `faulted_voices` counts it) |
| applying a command to a voice | the command is gone; the voice, in whatever state, is finished `Faulted` at once | none |
| a patch matrix (mixing its bus, `SetPatchRouting`, `SnapPatch`) | every gain 0, bus cleared; `StageFaulted { Patch(k) }` | disabled: not mixed (its voices play on unheard), routing commands ignored |
| a device output (master applied, meter peak) | nothing to reset; `StageFaulted { Output(o) }` | disabled: silent |
| the master (or `SetMaster`) | gain to unity, so one fault costs one block; `StageFaulted { Master }` | disabled: bypassed at unity, `SetMaster` ignored |
| the meter flush | the block's peaks dropped; `StageFaulted { Meters }` | disabled: meters no longer published |
| none of these (taking commands off the queue, scheduling, the copy out) | nothing more | rebuild: every patch silent, master unity, every unit enabled, `dev` cleared; `StageFaulted { Mixer }`. Voices and pending commands kept |

- A start that never reached the pool, or a voice that left the pool inside `finish` before being reported, is still reported as `VoiceFinished { Faulted }`.
- `StageFaulted { stage, at, disabled }` is counted in `faulted_stages` just before it is queued.
- A disabled unit is also published as bits in preallocated atomics (`MixerController::disabled_units`: `u64` masks of patches, outputs, master, meters; cleared by a rebuild), so a report lost to a full event queue is still found.
- The control thread answers from its own copy of the settings (skill `engine-control`, mixer faults). Every reset reuses the mixer's buffers. The panic itself (payload, hook) allocates, which is acceptable only on a faulted path.

## Scheduling horizon
- Every command goes through a hold queue on the control thread ordered by `(at, send order)`. `MixerController::pump()` releases commands whose `at` is within `lookahead_ms` of `samples_rendered`, in that order, and stops at the first batch that does not fit the command queue.
- The engine control loop calls `pump()` every tick (at most 5 ms apart); `send` and `send_batch` pump too.
- `send` never drops a command; beyond the horizon or with a full queue it waits on the control thread (`held()` counts them).
- The mixer holds only near-term commands, so a far-future schedule cannot delay a stop.
- `send_batch` (and `ramp_gang`) publishes with one queue write (single commit): the mixer sees all or none. A batch larger than the queue is refused with `BatchTooLarge`.
- Every command is stamped with its send order; the mixer uses it to break `at` ties, whether sent alone, in a batch or held first.
- **Stop overtakes ramps.** A batch of only `StopVoice` goes ahead of batches of only ramps, never ahead of a start, a move or a mixed batch. A ramp neither starts, resumes nor ends a voice, so overtaken ramps apply later to a voice that is fading out or gone (ignored). Without it, a Panic sent after more ramps than the queue holds would wait for them.
- The control thread sends stops that accompany nothing but ramps (Panic, Stop All holding a running Fade cue's levels) as a batch of their own, first (`stops_apart` in `engine/crates/engine/src/control/mod.rs`). Stops sent with anything else go with it and do not overtake.
- **A run of ramps is one command.** `ramps_together` (same file) turns each run of `Ramp`s of one voice with the same `at` and fade into one `RampSet` (level edit on a playing cue, volume-limit narrowing, Fade cue start, grown patch). A longer run is split at `MAX_RAMP_SET`, with parts on the same date, so one command's cost on the audio thread is bounded.
- A `RampSet` takes one queue slot and every gain starts on one sample: no block renders part of it. If the queue has no slot it waits whole on the control thread; if the pending set is full it applies late, all of it. A lone ramp stays a `Ramp`.

## Real-time mechanisms
**Pending set.** Released commands wait in a binary heap preallocated for `PENDING_CAPACITY` (4096), ordered by `(at, arrival)`. Pull and apply cost O(log n).
- If the heap is full while commands wait in the queue, they apply late rather than being lost; each such block counts in `pending_full_blocks` and one `Event::PendingFull` is sent per episode.
- During such an episode a batch can be pulled partly in one block and partly in the next, so a gang may move on different samples. `PendingFull` marks when this can happen.
- With the horizon, the heap fills only when more than 4096 commands fall within the lookahead.

**Return queue.** A `VoiceStart` is boxed on the control thread and moved in via `StartVoice`; a `RampSet` carries a boxed gain list.
- When the voice ends or is rejected the box returns, so rings and routing vectors are freed on the control thread. A `RampSet`'s list returns when applied, or ignored for an unknown voice.
- The queue has room for every box the mixer can hold, and `pump` drains it first. If ever full, the box is leaked and counted.

## Timing (NFR-21, NFR-22)
- `SampleTime` is a `u64` frame count at the device rate. The mixer's clock is frames rendered, published after each `process` as `samples_rendered`.
- Every command carries an absolute `at`. The mixer splits each block at command boundaries and applies a command before rendering sample `at`. A command whose `at` has passed applies at the start of the next block.
- Commands need not arrive in time order. A scripted sequence renders identically for any block-size pattern (property test).

## Voices
`StartVoice(Box<VoiceStart>)` carries the complete initial routing, so a voice is heard only as intended from its first sample. Field meanings are in the `VoiceStart` struct docs; the non-obvious semantics:
- **Width.** A start routes 1 to `cue_outputs` cue outputs (the engine sends its patch's count); `crosspoints` is `channels x` that. Only those outputs are rendered and metered, and a patch clears and mixes only the bus rows its voices wrote that segment (`Patch::rows`), so a 128-row mixer costs a stereo show what 2 rows do.
- A `Ramp`/`RampSet` naming a later cue output widens the voice to it. Outputs beyond a start's begin silent (gain 0, no crosspoint): the engine sends a level only for an output that is heard, so a muted or unlisted one stays silent. A start resets only the outputs earlier voices of its pool slot used, in every input row. A patch-routing ramp in a row no voice writes to still advances (`Patch::dirty_rows`).
- `VoiceStart::new` starts with all crosspoints at 0 (silent), cue outputs and master 1.0, patch 0. Set routes with `set_crosspoint`.
- The first source frame plays on sample `at` exactly, so voices started with the same `at` begin on the same output frame.
- `source` is an `rtrb::Consumer<f32>` of interleaved device-rate frames; dropping the producer is the end-of-stream marker.
- A fade-out needs `play_frames` or `slices`. `slices` replaces `play_frames` and `skip_frames` (the table carries its own start).
- `skip_frames`/`play_frames` give mixer-side sample-exact in and out points (AUD-11); the disk thread normally seeks to the in point.

**Rejection** (`Event::VoiceRejected`, box returned): `PoolFull`, `DuplicateId` (id already active), `BadChannelCount` (0 or above `max_voice_channels`), `BadRouting` (patch out of range, no cue outputs or more than `cue_outputs`, or crosspoints not `channels x` that), `BadFade` (fade-out without `play_frames`/`slices`), `BadSlices` (table with `play_frames`/`skip_frames`, an empty slice, `Times(0)`, or more than `MAX_SLICES`).

**End** is `Event::VoiceFinished { at, reason }`, `at` = first sample after the voice's last:
- `EndOfStream`: producer dropped and every whole frame consumed.
- `OutPoint`: `play_frames` played, the slice table's last play ended, or a devamp stop.
- `Stopped`: `StopVoice` with `fade.frames == 0` cuts on sample `at`. With a fade the stop gain ramps to 0, finishing at `at + fade.frames`, the first sample where it is exactly 0. Nothing after that is read or counted as underrun.

**Underrun** (source short and not ended): missing frames are silence, counted in `underrun_frames`; one `Event::Underrun { at }` per episode, at the first missing frame. Missing frames are not skipped: the voice resumes where it left off, so fades stay aligned with the audio.

## Pause and resume
`PauseVoice { voice }` on sample P and `ResumeVoice { voice, new_id }` on sample R keep the voice, its ring and all state. The resumed voice plays what the unpaused one would have, shifted by R - P, and every later event (`VoiceFinished` included) is R - P later. This is tested sample for sample for ring-fed, streamed, resampled, preloaded and looping voices.
- **Position.** On P the voice stops consuming; on R it continues from that frame. The resampler runs on the disk thread ahead of the ring, so a pause cannot disturb it. While paused the disk thread keeps the ring full as usual, so a paused voice never blocks a worker and a pause of any length resumes without underrun.
- **Declick.** The pause fades out over the de-zipper length D (5 ms) on samples P..P+D-1. Those samples play the frames after the pause point, peeked from the ring without consuming (an `rtrb` read chunk dropped without commit leaves the ring unchanged), times a linear 1 to 0 ramp exactly 0 on P+D-1. If fewer than D frames can be peeked (voice end, a pending devamp's boundary, end of a loop exit's buffer, a ring holding less) the tail is that long and still reaches exactly 0 on its last frame.
- The resume fades in over D (linear 0 to 1, exactly 1 on R+D) from frame P. Outside [P, P+D) and [R, R+D) the output equals the unpaused render shifted by R - P.
- **Frozen while paused.** Silence after the tail, reads nothing, no underrun, no events. Gain ramps, the stop fade and integrated fades freeze; the tail uses the gains the voice had on P. After the resume each ramp completes its remaining frames and a running stop fade ends R - P later.
- **Edge cases.**
  - A voice paused on its start sample (nothing heard) gets no tail and no fade-in; it plays as if started on R.
  - A resume during the tail (R < P+D) crossfades: the rest of the tail keeps playing while frame P fades in. The waveform never jumps, but for a few ms two nearby stretches overlap, and the tail takes the resumed frames' integrated fade envelope.
  - Pause and resume on the same sample before any tail played change nothing audible. A pause during that crossfade drops what is left of the old tail.
  - `StartVoice` then `PauseVoice` on one sample pauses the new voice; the reverse order pauses nothing (equal-time commands apply in send order).
- **Re-keying.** The cue engine gives every resumed segment a fresh id, so resume moves the voice from `voice` to `new_id`. Commands for the old id become unknown-voice commands (ignored) and events carry `new_id`.
  - A `new_id` held by another active voice is reported `VoiceRejected { voice: new_id, reason: DuplicateId }`; the voice stays paused under its old id. Resuming as its own id is allowed.
  - The `VoiceStart` (ring and routing) goes back on the return queue exactly once, when the voice ends. The controller keeps no per-id state.
- **Other commands while paused.**
  - `StopVoice` ends it at once with or without a fade: `VoiceFinished { Stopped }` on the stop's sample.
  - `Ramp` applies and is heard from the resume (its de-zipper or fade starts there).
  - `Devamp` is accepted as for a playing voice (the disk stream may cut at once). Its answer waits for the resume, which sends exactly one `DevampScheduled { voice: new_id, at }` however many devamps came in while paused.
  - A devamp answered before the pause is not announced again by the resume (its `at` is stale); the cue engine re-sends the pending devamp for the fresh id after the resume and that repeat gets the corrected `at`.
- **Ignored, no event:** pausing an unknown or already-paused voice; resuming a voice that is unknown or not paused (no re-key either).
- **Dating.** A `ResumeVoice` dated earlier than its `PauseVoice` applies first, to a voice not paused yet, and is ignored; the voice then stays paused, silent, under its old id until another resume.

## Reference files
- `references/signal-and-curves.md`: read when touching `gain.rs`, the signal-path formula, routing commands (`Ramp`, `SetPatchRouting`, `SnapPatch`, `MoveVoice`, `SetMaster`), de-zipper, ganging, fade curves, integrated fades, or when a core/Swift copy of the curve maths must stay in sync.
- `references/meters-and-budget.md`: read when touching `meter.rs`, `read_*_meters_into`, xrun counting, `offline.rs`, or `tests/cpu_budget.rs` and the CPU gate.
