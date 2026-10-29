# openLab audio engine (`openlab-audio`)

How the real-time mixer works and the exact semantics its tests pin down. Architecture context: [ARCHITECTURE §3](ARCHITECTURE.md#3-engine-internals). Requirement IDs refer to [`REQUIREMENTS.md`](../REQUIREMENTS.md).

Status: mixer, fades, routing, per-voice patches and offline render driver (M3.1, M3.2, M3.4, M3.5, M3.11); decoding, resampling, disk streaming and preload (M3.3); slices, loops and devamp (M3.6) with the loop cache (M3.13) and a minimum loop length (M3.17); voice pause and resume (M3.18); meters and the xrun counter (M3.7); waveform overview data (M3.9); the device layer with `assert_no_alloc` around its callback (M3.8, M3.12). The engine's use of it (reopen policy, settings, warnings) is M4.

## Threads and queues

`Mixer::new(config)` returns a pair:

- **`MixerController`** (control thread) schedules time-stamped `Command`s, polls `Event`s, frees returned voice starts, and reads the clock (`samples_rendered`), counters (faults, faulted voices and stages, underrun frames, dropped events, leaked starts, pending-full blocks, xruns) and meters (see Meters and xruns).
- **`Mixer`** (audio thread) renders with `process(out, frames)` into an interleaved `f32` buffer. The device callback and the offline renderer call `process_guarded`, which wraps `process` in `catch_unwind`: on a panic the block is silent, the fault counter goes up, and the clock still advances (ARCHITECTURE §3.4). The faulted block's unit is isolated (see Faults below).

They share only preallocated `rtrb` SPSC queues and atomics:

| Queue | Direction | Capacity | Contents |
|---|---|---|---|
| commands | control → audio | 1024 | `Command { at, kind }` |
| events | audio → control | 1024 | `Event` (small, `Copy`, with its exact sample time `at`). When full, events are dropped and counted (`dropped_events`). Events from the same block are not sorted by `at` |
| returns | audio → control | 1024 + 4096 + voices + 1 | `Box<VoiceStart>` (source and routing) the mixer is finished with |

The openlab-audio crate does not depend on `openlab-core`; the engine translates core actions into mixer commands.

### Faults

The mixer notes the stage it is in (`Stage`: a plain field write, no allocation) and `process_guarded` blames a panic on it. Each unit keeps the blocks of its last `FAULT_LIMIT` (3) faults in a fixed array; that many within `FAULT_WINDOW` (16) blocks (`process_guarded` calls; ~170 ms at 512 frames, 48 kHz) is the limit, so a unit that faults every block or every other one is dealt with while a one-off fault, or faults further apart than the window, cost only their blocks.

| Stage | On a fault | At the limit |
|---|---|---|
| a voice's render (pool index) | nothing more (the window resets when the slot plays another voice) | finished with `FinishReason::Faulted` (its start goes back, its meters flush, `faulted_voices` counts it) |
| applying a command to a voice | the command is gone (it left the pending set before it was applied); the voice, in whatever state it was left, is finished with `Faulted` at once; a start that never reached the pool, or a voice that left the pool inside `finish` before it was reported (a slot keeps its last id), is reported as `VoiceFinished { Faulted }` all the same | — |
| a patch matrix (mixing its bus, or `SetPatchRouting` / `SnapPatch` for it) | every gain set to 0, bus cleared; `StageFaulted { Patch(k) }` | disabled: not mixed (its voices play on unheard), routing commands ignored |
| a device output (delay line and gain, or `SetOutput` for it) | gain to unity (the only level the engine sends), the delay line cleared (no delay); `StageFaulted { Output(o) }` | disabled: silent, `SetOutput` ignored |
| the master (or `SetMaster`) | gain to unity (the engine keeps it there), so one fault costs one block; `StageFaulted { Master }` | disabled: bypassed at unity, `SetMaster` ignored |
| the meter flush | the block's peaks dropped; `StageFaulted { Meters }` | disabled: meters no longer published |
| none of these (taking commands off the queue, scheduling, the copy out) | nothing more | rebuild: every patch reset to silence, every output and the master to unity with no delay, all enabled again, `dev` cleared; `StageFaulted { Mixer }`. Voices and pending commands are kept |

`StageFaulted { stage, at, disabled }` is counted in `faulted_stages` just before it is queued. A disabled unit is also published before its report, as bits in preallocated atomics (`MixerController::disabled_units`: a `u64` mask of patches, one of outputs, master, meters; cleared by a rebuild), so a report lost to a full event queue is still found. The control thread answers it from its own copy (docs/control.md "Mixer faults"). Clearing a delay line costs at most 1 s of samples, on a fault only. Every reset reuses the mixer's buffers; the panic itself (its payload, the hook) allocates, as before. Test hooks (`#[cfg(test)]`): `panic_next` (unblamed), `panic_voice`, `panic_apply` (a command matching a predicate), `stage.panic_at` (entering a patch, output, master or meter stage).

### Scheduling horizon

Every command goes through a hold queue on the control thread, ordered by `(at, send order)`. `MixerController::pump()` releases commands whose `at` is within `lookahead_ms` (default 500 ms) of `samples_rendered`, in that order. It stops at the first batch that does not fit in the command queue.

- **The engine's control loop calls `pump()` on every tick** (ARCHITECTURE §3.2, at most 5 ms apart). `send` and `send_batch` pump too.
- `send` never drops a command. If the command is beyond the horizon, or the queue is full, it waits on the control thread. `held()` reports how many are waiting.
- The mixer only ever holds near-term commands, so a far-future schedule cannot delay a stop.
- `send_batch` (and `ramp_gang`) publishes its commands with one queue write (`write_chunk_uninit` with a single commit): the mixer sees all of them or none. A batch larger than the queue (1024) is refused with `BatchTooLarge`.
- Every command is stamped with its send order. The mixer uses that stamp to break ties between equal `at`, so equal-time commands apply in send order, whether they were sent alone, in a batch, or held first.
- Held and pending commands cannot be withdrawn. **The engine must not schedule voice starts beyond the next block** (ARCHITECTURE §3.2: `start = now + one block`), so a Stop or Panic is never followed by a start that was already queued.

### Real-time rules

Everything `process` can reach is allocation-free, lock-free, and makes no syscalls or log calls. All buffers are sized in `Mixer::new` from `MixerConfig`: voice pool, per-voice gain matrices, patch matrices and buses, scratch, delay lines, and the pending heap.

| Field | Default | Limit |
|---|---|---|
| `sample_rate` | 48000 | 1–384000 |
| `output_channels` | 2 | 1–64; every patch has this many cue outputs |
| `max_block_frames` | 1024 | 1–8192; the internal chunk size; any device block size is accepted and split |
| `max_voices` | 128 | 1–4096 |
| `max_voice_channels` | 8 | 1–64 |
| `patches` | 8 | 1–64 |
| `lookahead_ms` | 500 | 10–60000 |

**Pending set.** Released commands wait in a binary heap preallocated for 4096 entries, ordered by `(at, arrival order)`.

- Pulling and applying a command costs O(log n), so a 1024-command burst costs about one normal block (tested).
- If the heap is full while commands wait in the queue, those commands apply late rather than being lost.
- Each such block is counted (`pending_full_blocks`), and one `Event::PendingFull` is sent per episode.
- With the horizon, the heap only fills when more than 4096 commands fall within the lookahead.
- During such an episode, a batch can be pulled partly in one block and partly in the next. A gang due in that block may then move on different samples; the `PendingFull` event marks when this can happen.

**Return queue.** A `VoiceStart` is boxed on the control thread and moved into the mixer inside `StartVoice`.

- When the voice ends or is rejected, the box goes back on the return queue, so the source ring and routing vectors are freed on the control thread.
- `pump` drains the return queue first, which keeps it within its capacity.
- If it were ever full anyway, the mixer leaks the box rather than freeing it on the audio thread, and counts the leak (`leaked_starts`, expected to stay 0).

**Tests** run `process` under `assert_no_alloc` (set to count violations in tests), so any allocation on these paths fails the test. Paths covered: voice start/stop, ramps, underrun, voice end, rejection, 64 simultaneous voices, full event queue, full return queue, full pending set, slice tables, loops, devamp (stop, continue, cut and resume), pause and resume (`no_allocation_across_pause_and_resume`, `tests/pause.rs`), meters on 64 voices with their reads (`no_allocation_with_meters_active_on_64_voices`), and every disk-streaming test (`tests/stream.rs`, `tests/slices.rs`, `tests/loops.rs`).

## Timing (NFR-21, NFR-22)

- `SampleTime` is a `u64` frame count at the device rate. The mixer's clock is the number of frames it has rendered. It is published after each `process` call as `samples_rendered` (ARCHITECTURE §3.2).
- Every command carries an absolute `at`. The mixer splits each block at command boundaries and applies a command **before rendering sample `at`**. A command whose `at` has already passed applies at the start of the next block.
- Commands need not arrive in time order. Commands with equal `at` apply in the order they were sent (the controller's send stamp, across batches and held commands).
- The result is independent of block size. A scripted command sequence renders identically for any pattern of block sizes, including size 1, primes, and blocks larger than `max_block_frames` (property test).

## Voices

`StartVoice(Box<VoiceStart>)` carries the voice's **complete initial routing**, so a voice is heard only as intended from its first sample:

| Field | Meaning |
|---|---|
| `voice` | caller-chosen `VoiceId`, unique among active voices |
| `source` | `rtrb::Consumer<f32>` of interleaved frames at the device rate (filled by the disk thread) |
| `channels` | 1..=`max_voice_channels` |
| `patch` | output patch index, `< patches` (AUD-07) |
| `gain` | voice master gain (linear) |
| `cue_outputs` | one gain per cue output (`output_channels` of them) |
| `crosspoints` | row-major `[input][cue output]`, `channels × output_channels` |
| `skip_frames` | frames discarded from the source first (in point) |
| `play_frames` | frames played after the skip (out point); `None` = until end of stream |
| `fade_in`, `fade_out` | integrated fades (see below); a fade-out requires `play_frames` or `slices` |
| `slices` | slice table (see Slices, loops and devamp); replaces `play_frames`, which must then be `None`. Empty = play `play_frames` once |
| `link` | devamp link to the disk stream filling `source` (set by `PreparedStream::into_voice`) |

`VoiceStart::new(voice, source, channels, cue_outputs)` starts with **all crosspoints at 0 (silent)**, cue outputs and master at 1.0, and patch 0. Set routes with `set_crosspoint`. The first source frame plays on sample `at` exactly, so voices started with the same `at` begin on the same output frame.

A start is **rejected** with `Event::VoiceRejected`, and the box is returned, for:

- `PoolFull`: no free voice.
- `DuplicateId`: the id is already active.
- `BadChannelCount`: `channels` is 0 or above `max_voice_channels`.
- `BadRouting`: the patch index is out of range, or a routing vector has the wrong length.
- `BadFade`: a fade-out without `play_frames` or `slices`.
- `BadSlices`: a slice table combined with `play_frames` or `skip_frames` (the table carries its own start), with an empty slice or `Times(0)`, or with more than `MAX_SLICES` (256) entries.

Nothing panics.

A voice **ends** with `Event::VoiceFinished { at, reason }`, where `at` is the first sample after its last:

- `EndOfStream`: the producer was dropped and every whole frame has been consumed. Dropping the producer is the end-of-stream marker.
- `OutPoint`: `play_frames` frames have been played, or the slice table's last play ended (or a devamp stop).
- `Stopped`: `StopVoice` with `fade.frames == 0` cuts on sample `at` exactly. With a fade, the voice's stop gain ramps to 0 over `fade.frames`. The voice finishes at `at + fade.frames`, the first sample where that gain is exactly 0. Nothing after that point is read or counted as underrun.

**Underrun**: if the source has fewer frames than needed and has not ended:

- The missing frames are silence, and they are counted (`underrun_frames`).
- One `Event::Underrun { at }` is sent per episode, at the first missing frame.
- Playback resumes when data arrives. Missing frames are not skipped: the voice resumes where it left off, so its fades stay aligned with the audio.

**Trims** (AUD-11): the disk thread normally seeks to the in point; `skip_frames`/`play_frames` give the same sample-exact in and out points on the mixer side.

### Pause and resume (CUE-05)

`PauseVoice { voice }` on sample P and `ResumeVoice { voice, new_id }` on sample R keep the voice, its ring and all its state: the resumed voice plays exactly what the un-paused voice would have, shifted by R − P (tested sample for sample for ring-fed, disk-streamed, resampled, preloaded and looping voices). Every later event, `VoiceFinished` included, comes R − P later.

- **Position.** On P the voice stops consuming: its next frame stays frame P's. On R it continues from that frame. The resampler runs on the disk thread, ahead of the ring, so a pause cannot disturb it. While paused the disk thread keeps the ring full as usual; a full ring just makes the stream wait its turn, so a paused voice never blocks a worker. A pause of any length resumes without underrun.
- **Declick.** A cut would click, so the pause fades out over the de-zipper length D (5 ms) on samples P..P+D−1: those samples play the frames that follow the pause point, *peeked* from the ring without consuming them (an `rtrb` read chunk dropped without a commit leaves the ring unchanged), times a linear 1 → 0 ramp that is exactly 0 on sample P+D−1. If fewer than D frames can be peeked on P (the voice's end, a pending devamp's boundary, the end of a loop exit's buffer, or a ring holding less), the tail is that many frames long and still reaches exactly 0 on its last frame. The resume fades in over D (linear 0 → 1, exactly 1 on R+D) from frame P. Outside [P, P+D) and [R, R+D) the output equals the un-paused render shifted by R − P (tested; the two windows are tested too, across odd block splits).
- **Frozen while paused.** The voice outputs silence (after the tail), reads nothing, reports no underrun and sends no events. Gain ramps, the stop fade and the integrated fades freeze; the tail uses the gains the voice had on P. After the resume each ramp completes its remaining frames, and a running stop fade ends R − P later.
- **Edge cases.** A voice paused on its start sample (nothing heard yet) gets no tail and no fade-in: it plays exactly as if started on R. A resume during the tail (R < P+D) crossfades: the rest of the tail keeps playing on from where it was (frame P + (R − P) onwards, still fading out) while frame P fades in. The waveform therefore never jumps, but for those few ms two nearby stretches of audio overlap, and the tail takes the resumed frames' integrated fade envelope. Pause and resume on the same sample, before any of the tail has played, change nothing audible. A pause during that crossfade drops what is left of the old tail (at most D frames old). Equal-time commands apply in send order, so `StartVoice` then `PauseVoice` on one sample pauses the new voice, while the reverse order pauses nothing.
- **Re-keying.** The cue engine gives every resumed segment a fresh id (its `RunId`), so the resume moves the voice from `voice` to `new_id`: afterwards commands for the old id are unknown-voice commands (ignored) and events carry `new_id`. A `new_id` held by another active voice is reported as `VoiceRejected { voice: new_id, reason: DuplicateId }` and the voice stays paused under its old id; resuming as its own id is allowed. That rejection is about the resume, not about the voice that already has `new_id`, which plays on untouched; the cue engine's ids are unique, so it is not expected to happen. The voice's `VoiceStart` (ring and routing) still goes back on the return queue exactly once, when the voice ends; the controller keeps no per-id state, and stream handles stay with the engine, keyed by whatever it keyed them by.
- **Other commands while paused.** `StopVoice` ends it at once, with or without a fade (it is silent anyway): `VoiceFinished { Stopped }` for the paused id, on the stop's sample. `Ramp` applies and is heard from the resume (its de-zipper or fade starts there). `Devamp` (new or a repeat of the pending one) is accepted as for a playing voice (the disk stream may cut at once). Its answer waits for the resume, which sends exactly one `DevampScheduled { voice: new_id, at }` with the sample where the voice will leave the loop, however many devamps came in while paused. A devamp answered *before* the pause is not announced again by the resume (its `at` is then out of date): the cue engine re-sends the pending devamp for the fresh id after the resume, and that repeat is answered with the corrected `at`. One answer per devamp sent.
- **Ignored**, with no event, as other commands for unknown voices: pausing an unknown or already paused voice, and resuming a voice that is unknown or not paused (no re-key either).
- **Dating.** A `ResumeVoice` must be dated at or after its `PauseVoice`. One dated earlier applies first, to a voice that is not paused yet (or not started yet), and is ignored; the pause then applies and the voice stays paused, silent and under its old id, until another resume (tested). The cue engine guarantees it never sends one: commands for one voice never go backwards in time (docs/cue-engine.md), so a resume is never dated before its pause, even when the pause was dated at a start still in the lookahead.

## Decoding (AUD-04)

`decode.rs` wraps symphonia 0.6. It runs on disk and I/O threads only, never on the audio thread.

- **Formats**: WAV and AIFF PCM (integer and float; 16/24/32-bit int and 32-bit float tested; more than 8 channels see **Many-channel PCM** below), FLAC, ALAC (M4A), MP3, AAC (M4A), Ogg Vorbis (optional, §21). 1–64 channels, 8000–384000 Hz; anything else is `Unsupported`. The floor exists because rubato sizes its FFT chunk from the rate ratio (a 1 Hz file at 48 kHz asked for a 49-million-frame chunk, an out-of-memory found by the decode fuzzer); `Pipeline::new` refuses resampling outside the same range too. Worst case inside it is ~0.5 GB (64 channels, coprime 384 kHz rates).
- **Many-channel PCM** (M3.23, `pcm.rs`). symphonia 0.6.1 reads WAV only for channel layouts it knows (about 18 to 26 channels, 16 for ambisonic layouts), so a 64-channel file would not play although voices carry 64. Uncompressed WAV and AIFF with **9 to 64 channels** (`NATIVE_MAX_CHANNELS` = 8 is the most symphonia is trusted with) are therefore read by an in-house `PcmSource` behind the same `FrameSource` interface as the symphonia decoder: `probe`, `preload`, disk streams and the waveform build all go through `open_media`, which picks the reader from the header (the `fmt `/`COMM` scan of the M3.16 pre-check; counts of 0 or above 64 are still `Corrupt("header: ...")`). Nothing on the audio thread changes. Supported: integer 8-bit (unsigned in WAV, signed in AIFF), 16/24/32-bit and float 32/64-bit; little-endian WAV (plain and `WAVE_FORMAT_EXTENSIBLE`) and big-endian AIFF, plus AIFF-C `NONE`, `twos`, `sowt`, `in24`, `in32`, `fl32`, `fl64`. Integers are scaled by `2^-(bits-1)` (exact in `f32` up to 24 bits), floats are sanitised like every other decode (non-finite is silence, clamped to ±16). The extensible channel mask is **ignored**: openLab routes by channel index, so a mask that names more, fewer or different speakers than the file has channels plays the same. Header checks: the `fmt `/`COMM` chunk must be complete, block align must equal channels x sample bytes, an extensible header needs the standard or ambisonic B-format PCM/float sub-format GUID (any other is `Unsupported`) and valid bits within its container, the rate must be 8000..=384000 Hz, and at most 64 chunks are walked, each size checked against the file length before it is skipped; failures are `Corrupt` (`Unsupported` for a sample size, format tag or AIFF-C compression it does not read, or a rate out of range). Length: the frames the file holds, never more than the header declares; a data chunk or AIFF frame count above the file gives a `Truncated` warning (`at` = frames present) and plays to the real end, a WAV data size of `0xFFFFFFFF` means "to the end of the file", no whole frame is `Corrupt("no audio frames")`. A seek is a byte offset (`data_start + frame x frame_bytes`), so it is exact and costs one `seek` call; a file that shrinks after its probe gives `Changed`. Reads are 4096 frames at most (2 MiB of scratch at 64 channels of 64-bit floats); no length from the file sizes an allocation, and a preload checks its budget from the frame count before reserving. Compressed formats (FLAC, ALAC, MP3, AAC, Vorbis) keep symphonia's channel limits, whatever each codec reads (not extended here), and an AIFF-C with other compression and more than 8 channels is `Unsupported`, while a WAV with a tag other than PCM, float or extensible (A-law, mu-law, ADPCM) stays on symphonia's path. The decode fuzz target covers the reader through `probe_media`, `preload` and `open_source` (seeds `wide-*` in `fuzz/decode-seeds`). Tested in `tests/wide_pcm.rs` (sample-exact round trips of 64-channel WAV and AIFF in every format, masks, seeks and loops through a 64-output mixer, waveform, damaged headers and the decode fuzz invariants) and `engine/tests/control_wide_media.rs` (a 64-channel cue through the control loop).
- `probe(path)` / `probe_media(media)` return `Probed { info: MediaInfo { channels, sample_rate, frames, duration }, warnings }`. **`frames` is exact** on the playback timeline: a PCM header's length is checked by decoding the tail; every compressed file is decoded once in full and counted. The full decode also finds any damage, so it is known when the cue loads, not during the show.
- **Damaged files play, with warnings** (`MediaWarning { kind, at, frames }`, positions in frames at the file's rate on the playback timeline, each reported once, at most 256 per file):
  - `Missing`: frames the container skipped (a FLAC frame failing its CRC, a lost Ogg page, a spliced file) or a packet that did not decode (AAC, MP3). They **play as silence of their length**, so every later frame keeps its position and seeks, loops and slices land exactly. Damage in the first packets makes silence from position 0.
  - `Repeated`: frames repeating positions already played; dropped.
  - `Truncated`: the file has `frames` fewer frames than its header declares and plays to its real end, `at` (the probed length). For lossy formats the missing frames may lie anywhere (an MP3 frame the reader skipped).
  - `Unreadable`: the container keeps failing (16 errors in a row) or its timestamps jump by more than a minute: the file plays to `at`.
  - `PrimingIgnored` (`at` 0, `frames` 0): an AAC-in-MP4 file has gapless information that cannot be used (see Positions); it plays as decoded, starting with the encoder's priming.
  - Only a file with nothing playable is an error (`Unsupported`, `Corrupt`, `Io`, `NotFound`).
  - Headers symphonia 0.6.1 would overflow on are rejected as `Corrupt` before it parses them: WAV/AIFF channel count and sample size, and MP4 `stts` tables whose sample counts or durations overflow.
  - **Seeking a damaged file** (`Probed::seeking`, pass it in `StreamRequest`). After damage, symphonia's readers can place frames differently after a seek than when reading through, so a damaged file's seeks are checked against the probe's own decode:
    - The probe's full decode records a **seek index**: a hash of the 256 timeline frames at every 8192nd frame (`INDEX_SPACING`), 8 bytes per checkpoint, and whether the window is flat (digital silence or nearly constant: under about -80 dBFS peak to peak). A flat window verifies at any alignment, so seeks and the probe skip flat checkpoints and use the nearest one that is not.
    - `Seeking::Indexed`: a seek goes to the nearest checkpoint before its target, checks that the 256 frames there decode exactly as the probe decoded them, and decodes forward at most 8192 frames. If that checkpoint does not match, it tries the two before it, then decodes from the start. The probe verifies the checkpoint after each piece of damage and the last one before choosing this.
    - `Seeking::FromStart`: those checks failed, so seeks decode forward from the start. The probe adds a `SlowSeeks` warning and `Probed::preload_advised()` is true: preload the file if it fits the budget.
    - `Seeking::Direct`: undamaged files (and PCM files that are only truncated) use the container's own seeks.
    - Seen per format (tested bit-exact against preload from offsets all over the file and around the damage): FLAC and Vorbis get `Indexed`; MP3 gets `Indexed`, or `FromStart` when the reader misplaces frames after the damage; AAC always gets `FromStart`, because noise substitution never decodes identically after a seek.
    - However a seek decodes, a disk worker does at most `step_frames` (`DEFAULT_STEP_FRAMES`, 65536 frames) of it per step and then serves its other streams, so one slow seek never starves them.
  - Known limits: symphonia's FLAC reader stops at damage in a very short file (reported as `Truncated`); an Ogg file whose damage also defeats symphonia's length scan has no declared length, so audio lost at its end cannot be reported. In a damaged Vorbis file the page after a lost page plays in the lost page's place and the silence comes one page later (symphonia's Ogg reader); straight playback, preload and seeks all agree on that timeline, which the warning describes.
- Errors are typed (`MediaError`): `NotFound`, `NotAFile` (a FIFO, socket, device or folder at the path, #323: every media open, probe, preload, waveform and disk stream, goes through `open_regular`, which opens non-blocking on Unix (opening never waits on Windows) and checks the type on the opened handle, so a FIFO is refused at once instead of hanging the worker; `regular_file.rs`, shared with the engine's show-file reads; tested in `tests/decode.rs::a_fifo_or_device_as_media_is_refused_without_blocking`), `Io`, `Unsupported`, `Corrupt`, `Truncated { expected, actual }` (a stream's file has become shorter than its probe), `Changed` (the file no longer matches its probe), `BadSlices`, `TooManyStreams`, `Worker`, `TooLarge`. A panic inside symphonia (a decoder bug hit by malformed data) is caught and becomes `Corrupt`; it never takes a thread down.
- **Positions.** Position 0 is the first playable frame. The decoder applies every packet's encoder delay and padding itself (the same way for every codec), so an MP3 with LAME gapless info has its original length and Vorbis follows its granule positions. The stream ends at the container's end trim (the packet it trims padding off), even if damage left more data after it. A packet never counts for more than its container duration, whether or not it decodes (a failed packet is silence of that duration), so the timeline does not depend on decoder state; this also drops AAC padding past the MP4's length. **AAC priming in MP4 is trimmed** (M3.14): symphonia 0.6.1 ignores the edit list and iTunes' gapless tag, so `mp4_priming.rs` reads them (a bounded box walk like `mp4_check.rs`: depth 6, 4096 boxes, 64 tracks, an `iTunSMPB` text of at most 256 bytes, no allocation from file-supplied lengths) before symphonia parses the file. Only AAC tracks use it. The priming is the audio track's single `elst` entry at normal speed (`media_time`, scaled from the track's `mdhd` timescale to its rate), else the second field of the `iTunSMPB` tag (`moov > udta > meta > ilst > ----`, hex frames); the edit list wins, and a tag is also the fallback for an edit list that is not understood (several entries, a negative media time, a rate other than 1, no timescale). Position 0 is then the frame after the priming: it is part of the decoder's `origin`, so the frames before it have negative positions and every packet drops them, whether it is met by the probe, a preload, streaming, or after a seek (a seek to frame F decodes the stream's frame F + priming and lands on real frame F). Probe length and duration exclude the priming, and the container's declared length is compared net of it. An edit list of zero means no priming. A value that is absurd (32768 frames or more, or not less than the container's length), negative or overflowing, a malformed tag, or an unreadable edit list with no tag is ignored with a `PrimingIgnored` warning and the file plays as decoded. End padding was already dropped at the container's length. Other codecs (ALAC, MP3, Vorbis) are unaffected: their delay is handled by the codec or the container reader. If the real stream is shorter than the priming (a truncated file), nothing is left and the file is `Corrupt("no audio frames")`.
- **Continuity.** When the track's time base is a whole fraction of a frame (every format above), each packet's position comes from its timestamp; a gap before it is filled with silence (`Missing`), an overlap is dropped (`Repeated`). Position 0 is the container's start. Otherwise positions are counted, and a seek decodes forward from the start (an MP4 whose timescale is not a multiple of the rate, e.g. 90000 at 44.1 kHz; also any seek the container refuses): every loop seam then costs time proportional to its position in the file, so preload such files if they loop. A seek into a gap gives the same silence as reading through.
- **Seeking is sample-accurate.** A seek starts a fresh container reader and a fresh decoder (symphonia 0.6.1's FLAC reader returns stale frames after some seeks; its AAC `reset` keeps state), seeks to a pre-roll before the target (8192 frames for lossy codecs, none for lossless), drops the first packet after a mid-file lossy seek (it lacks its predecessor's overlap or bit reservoir), and discards frames up to the target by position. A seek that lands after its target is retried with twice the pre-roll, then from the start; one the container refuses decodes forward from the start. Afterwards every format yields exactly the frames a decode from the start yields, except AAC files that use noise substitution (ffmpeg's encoder does): those bands are pseudo-random and differ by under 0.003 after a mid-file seek.
- **Headers are checked first**: a WAV `fmt ` or AIFF `COMM` chunk with 0 or more than 64 channels, or more than 64 bits per sample, or a `WAVE_FORMAT_EXTENSIBLE` channel mask symphonia would shift out of range, is `Corrupt("header: ...")` before symphonia parses it (its `u16` frame-size arithmetic overflows on such headers: a panic with overflow checks on, as in fuzzing, and a wrong frame size without). The check covers the container at byte 0 and every `RIFF....WAVE` / `FORM....AIFF|AIFC` marker in the first 1 MiB that the probe would scan on to (at most 64), and follows each container's chunks as symphonia 0.6.1's readers do (`riff_check.rs`), not by the chunk sizes alone: their parsers leave the reader off the declared sizes (a `fmt ` chunk is not skipped to its end, `MARK`/`COMT`/`ID3 ` entries can run past theirs, an odd `LIST` moves two padding bytes), so every `fmt ` symphonia parses before `data` and the first `COMM` are the ones checked. More than 1024 chunks in one container is `Corrupt` too. The other numeric header fields (rates, frame counts, chunk sizes and offsets) symphonia checks or computes in `u64` itself; the rate is then range-checked (8000..=384000 Hz). Re-check `riff_check.rs` against symphonia's RIFF readers on upgrade. A panic in symphonia is still contained (`decode::contain` around probe, preload and each disk-stream step; the waveform build has its own): the cue is broken with `Corrupt("decoder failed")`, the worker lives on.
- **Samples are made safe**: a non-finite sample (NaN, ±∞, possible in float files) becomes 0.0 and the rest are clamped to ±16.0 (+24 dBFS, the mixer's gain ceiling), so one bad sample can never poison a mix bus or the resampler.
- `preload(media)` decodes a whole file into memory (`Preloaded`, AUD-05), at the file's rate; `bytes()` reports its size. It uses at most `DEFAULT_PRELOAD_BUDGET` (512 MiB, about 46 minutes of 48 kHz stereo); `preload_within(media, max_bytes)` sets another budget. The declared length is checked before decoding and the decoded length while decoding (a small compressed file can expand far beyond what it declares); over budget, or an allocation the system refuses, is `TooLarge { bytes, limit }`. The engine decides whether the cue then streams or is reported. The decode buffer is kept as is, with no final copy.

## Disk streaming and preload (AUD-05, ARCHITECTURE §3.1, §3.5)

`DiskStreamer::new(StreamerConfig)` starts a pool of disk workers (default 2) that decode and resample each stream into its voice's ring, about `buffer_ms` (default 2000) ahead.

- `prepare(StreamRequest { media, info, seeking, slices })` (`info` and `seeking` from the probe) checks the slice table against `info` and the open-stream limit (`max_streams`, default 256), creates the ring, and hands the stream to a worker. It does no I/O; the worker opens the file. It returns a `PreparedStream`; `into_voice(voice, cue_outputs)` makes the silent `VoiceStart` (with `slices` and `link`) and keeps the `StreamHandle`. Dropping the handle cancels the stream.
- **Primed** (`is_primed`): the first chunk (4096 frames, or the whole stream if shorter) is in the ring; the cue is loaded. `is_finished`: every frame of the plan is in the ring. `frames_written`, `cuts`, and blocking `wait_*` helpers for I/O threads and tests (never the control thread).
- **Resampling** (`resample.rs`): rubato's synchronous FFT resampler when the file rate differs from the device rate, a pass-through otherwise. Device frame `p` is the signal at `p / device rate`, whether reached by reading or by seeking (a seek restarts the resampler on a chunk boundary one chunk early and discards its delay). The length is exactly `round(frames · device rate / file rate)` (`MediaInfo::frames_at`).
- **Loops** are unrolled into the ring ahead of time, so a loop's own seam never waits on the disk. Trims and start offsets are seeks.
- **Loop cache** (M3.13). The first play of a loop entry (`Infinite`, or `Times(n)` with plays still to come) that starts at its first frame is recorded as it goes into the ring: exactly the device-rate frames written, after decoding and resampling. Every later play of that entry is written from the recording instead of seeking back (a fresh demuxer and pre-roll each time), several plays per worker step for short loops (up to one 4096-frame chunk). So tight loops cost a copy, and every play of a lossy loop is the same frames (an AAC or MP3 decoder restarted by a seek gives slightly different output each time). For lossless files a seek gives the very same frames, so the output is bit-identical to seeking: tested with WAV and FLAC at the device rate and with FLAC at 44.1 kHz resampled to 48 kHz (a seek restarts the resampler on a chunk boundary one chunk early, which reproduces the frames reached by reading straight through in that test; this is not proven for every rate pair). Damage met in the first play is reported once; later plays repeat its silence. Bounds: a play of at most `loop_cache_bytes` (`DEFAULT_LOOP_CACHE_BYTES`, 16 MiB: 43 s of stereo or 1.3 s of 64 channels at 48 kHz) per stream, one loop at a time (dropped when the cursor leaves the entry or a devamp cuts it), taken from the streamer-wide `cache_budget_bytes` (`DEFAULT_CACHE_BUDGET_BYTES`, 256 MiB, for loop caches only) and allocated with `try_reserve_exact` on the disk worker, never the audio thread. A loop over a bound, or when the budget or the allocation fails, seeks once per play as before (the next play tries again). A loaded cue records its cache while its ring is first filled (before the voice starts): those first seconds hold the plays of a short loop, the ones the cache is for. `StreamHandle::cached_frames` and `cache_bytes`, and `DiskStreamer::cache_bytes`, report it.
- Each worker step is bounded: one 4096-frame chunk, or `step_frames` of seek decoding, or one chunk of a loop exit's buffer. Streams take turns round-robin.
- **Loop exits.** Leaving a loop early (devamp) is the one place the ring cannot be ahead: it is FIFO and full of loop plays that will not be heard. So the worker decodes each loop exit's continuation (the slice table unrolled from the slice after the loop) for `exit_ms` (`DEFAULT_EXIT_MS`, 250 ms) into an exit buffer published on the voice's `StreamLink` (later plays of a loop within it are copied from its first play). At the exit the voice plays that buffer while it drops the stale plays from the ring, and the worker refills the ring from where the buffer ends. The worker therefore has the whole 250 ms to notice the freed ring (5 ms poll), seek (fresh demuxer and pre-roll, up to about 10 ms on long compressed files) and decode its first chunk, with up to one 8192-frame device block (170 ms at 48 kHz) still taken ahead. `StreamHandle::last_resume` tells where the continuation starts in the ring (diagnostics and tests).
  - **When** (L-R2): the first chunk goes into the ring before any exit is built, so a cue primes as fast as one without loops. After it, no frame of a loop is written to the ring until that loop's own exit is decided (built or refused), so a devamp of a loop the voice reached through the ring always finds its exit ready. Loops inside the first chunk are caught up on first. The exits of loops that an exit buffer reaches (the voice can be in them, through the buffer, before the ring has any of their frames) are built nearest first whenever the ring is at least two chunks (8192 frames, or half the ring if smaller) ahead of the voice, so building them never starves the ring: a cue started the moment it is primed on a dense table (60 slices of 100 ms, every other one looping) plays with the worker stepping once per 64-frame block and never underruns (tested). The ring is normally seconds ahead of the voice, so all of them are ready within milliseconds of priming; a devamp before that (within the first chunk, 85 ms at 48 kHz, of a cue that starts in or just before a loop, or out of a loop reached through an exit buffer right after priming) takes the fallback below.
  - **Memory**: one buffer per distinct loop exit, 250 ms × channels × 4 bytes (96 KB for stereo, 3 MB for 64 channels at 48 kHz), at most `exit_bytes` (`DEFAULT_EXIT_BYTES`, 32 MiB: every exit of a 255-slice stereo table, 10 of 64 channels) per stream, taken from the streamer-wide `exit_budget_bytes` (`DEFAULT_EXIT_BUDGET_BYTES`, 256 MiB), which is separate from the loop caches' budget so optional cache memory never crowds out exits; allocated fallibly on the disk worker. An exit put off because the exit budget was full is built when the stream is idle and the budget has room again (nearest first among the loops the voice can still reach; tested with two streams); one over the per-stream cap or whose allocation failed stays refused. Exits are freed with the link when the voice and the stream are done. A voice start the mixer leaks (`leaked_starts`, expected to stay 0: the return queue was full) keeps its link, so its exits' bytes stay reserved for the rest of the run; that only reduces the room other streams' exits have.
  - **Fallback** (an exit refused by the cap or the allocation, put off for the budget, or not yet built): the devamp is carried out the same way, but at the boundary the voice has nothing to play until the worker has cut and refilled the ring behind the stale frames: an underrun (silence, counted) of about one worker poll plus a seek and a chunk, after which playback continues exactly where it was. Whether the voice uses an exit is decided when the devamp is accepted and sent with the cut, so the voice and the worker always agree, even if the exit appears in between.
- **Preload** streams `Media::Preloaded` through the same resampler and ring, so it plays exactly as streaming does (tested sample for sample).
- **Damage while streaming** is handled exactly as the probe handles it: missing frames play as silence and every warning met is listed by `StreamHandle::warnings()` (damage the probe reported shows up again; anything else is new, i.e. the file changed after its probe). The stream reads nothing past the probed length, so damage beyond a file's playable end is never reached.
- **Failures.** A stream that cannot continue (a read error, a file that became shorter than its probe or no longer matches it, a worker panic) sets `StreamHandle::error` and lets go of its ring. The error is published only after the ring's producer is dropped, so once a caller sees it the voice is certain to play out what was written and end with `EndOfStream`. A file that ends before its probed length is `Truncated`, never padded with silence: silence is only ever played for damage that is also reported.
- **Underrun**: a slow disk gives the mixer's underrun behaviour (silence, counted, resumes where it left off).
- Dropping the `DiskStreamer` tells the workers to stop and does not wait for them, so a worker blocked on a stalled disk cannot hang the caller.
- Cost per seek (release, 10-minute files): about 0.03 ms WAV, 0.1 ms FLAC, 0.6–0.8 ms MP3/AAC, 1.6 ms Vorbis. Loops within the loop cache's bounds seek once, for their first play.

## Slices, loops and devamp (AUD-11, AUD-12, CT-16)

`plan(range, marks, plays, start)` builds a slice table in device frames: the in/out range cut at `marks`, `plays[i]` (`Times(n ≥ 1)` or `Infinite`) for slice `i` (empty = each once), starting at `start` inside the range. A start inside a looping slice splits it into a one-play entry and an entry for the remaining plays, with the same `index`. Bad input is a `PlanError`; `total_frames` is `None` if anything loops forever.

**Minimum loop length** (M3.17). Every play of a loop is one `LoopReached` event and one boundary for the voice, so a loop of a few frames would flood the event queue (1024 entries; dropped events are counted) and keep the mixer at its boundaries. The engine builds its tables with `plan_at_rate(range, marks, plays, start, device_rate)`, which is `plan` plus `check_loop_lengths`: every entry that plays a slice again (more than one play, or the rest of a slice split by a start offset) must be at least `min_loop_frames(device_rate)` = `MIN_LOOP_MS` (10 ms) rounded up to whole frames (480 at 48 kHz, 441 at 44.1 kHz), or it is `PlanError::LoopTooShort { slice, frames, min }`. Exactly the minimum is accepted. Slices that play once may be any length. QLab 5 documents no minimum slice length that we know of; 10 ms keeps 128 looping voices at about 64 loop events per 5 ms control tick. `plan` itself, the disk stream and the mixer still accept shorter loops (existing callers and tests use them): with the loop cache even a one-frame loop costs the disk worker one copy per chunk, not a seek per play (tested), but its events are not bounded.

The same table drives the disk stream (which unrolls it into the ring) and the voice (which counts frames to find boundaries), so both agree exactly. Events, with the exact sample `at`:

| Event | When |
|---|---|
| `LoopReached { slice, count, at }` | a play of `slice` ended and it plays again from `at`; `count` plays done so far |
| `SliceEnded { slice, at }` | the slice's last play ended; the next slice, if any, starts on `at` |
| `DevampScheduled { at }` | a devamp was accepted, or the same devamp (same `stop`) was sent again while it is pending; `at` is where the voice leaves the loop unless an underrun intervenes |
| `DevampIgnored { at }` | a devamp that cannot be carried out (no such voice playing, no slice table, a different devamp (other `stop`) pending, a devamp that needs a cut while an earlier devamp's cut still waits to be sent (see While a loop exit plays), or no `link` when one is needed); playback is unchanged |

Voices without a slice table report only `VoiceFinished`.

**Devamp** (`CommandKind::Devamp { voice, stop }`, CT-16) leaves the current loop at the end of the play under way when the command applies (a devamp on a loop boundary sample exits at the end of the play that starts there):

- **Non-looping slice** (M3.24): a devamp that arrives while a non-looping slice (a bridge) plays leaves at that slice's end, i.e. has no effect, and the next loop keeps looping. QLab's docs are silent on this; unconfirmed, decided by the orchestrator ([handoff/decisions/qlab-confirm.md](handoff/decisions/qlab-confirm.md)).
- **Idempotent.** A devamp equal to the one pending (same `stop`) changes nothing (no second devamp, no new cut) and is answered with `DevampScheduled` at the pending boundary again. This is how the cue engine's re-sent devamp after a resume gets its answer for the fresh id (docs/cue-engine.md, M4.2 contract). A different `stop` while one is pending is `DevampIgnored`.
- **While a loop exit plays** (L-R5): a devamp that comes while the voice still plays an earlier devamp's exit buffer is carried out like any other, at the end of the play under way (e.g. vamp A, a bridge shorter than the exit buffer, vamp C: a devamp in C leaves C at the end of that play, even if the boundary falls inside A's exit buffer; tested). Cuts are dated in voice frames (frames of the plan played, exit buffers included), which only the disk stream maps to ring positions, so the voice needs no ring position for them. The link carries one cut at a time: a devamp that comes before the disk stream has answered the previous cut (normally within one 5 ms poll of the previous boundary) is accepted and answered at once, and its cut is sent when the answer is in, also if the voice has passed its boundary meanwhile (it then plays the new exit buffer and waits for that answer too; tested). Only one cut waits: a further devamp that needs a cut before then is `DevampIgnored` (a dropped cut would make the disk stream map the next one from the wrong place). As a second line of defence the disk stream fails the stream (`BadSlices`, the voice ends) for a cut that is not on the end of a play of a loop, instead of cutting anywhere else. A test drives random tables and devamp sequences with the worker held at random against a reference model. A devamp in a slice that does not loop (the bridge) leaves at that slice's end, which is where playback goes anyway, with or without an exit buffer playing.

- `stop: true` ends the voice there (`SliceEnded`, then `VoiceFinished { reason: OutPoint }`).
- `stop: false` continues with the next slice. If more plays of the loop may already be in the ring, the voice publishes the boundary on its `StreamLink` and at the boundary plays the loop exit's buffer (see Loop exits); the disk worker stops unrolling there, writes the continuation from the end of the exit buffer, and publishes where it starts; the voice skips the stale frames in between. This holds however full the ring is and however close to the boundary the devamp comes (tested at 1 frame, 85 ms and 4096 frames before it). A devamp before the ring reaches the boundary skips nothing.
- To start the next cue when the loop ends, the engine uses `DevampScheduled.at` (schedule the start in the block containing it) or reacts to `SliceEnded`.
- If the disk worker cannot place the boundary in the table, the stream fails (`BadSlices`) and the voice ends, rather than waiting forever.

**Integrated fade-out with slices** follows the planned end: it ends on the last frame of the table's last play. A voice heading into an infinite loop has no known end, so no fade-out applies until a devamp continues it into a finite part. If a devamp leaves fewer frames than the fade-out's length, the fade restarts from the gain the last frame before the jump had and reaches exactly 0.0 on the new last frame, with the same curve: the gain never jumps. A devamp stop ends at the loop boundary without the fade-out.

## Signal path (ARCHITECTURE §3.5)

Per sample, for each voice (routed through patch *P*) and its input channel *i*, cue output *o*, and device output *d*:

```
voice_gain   = master(t) · stop(t) · fade_env(p)
bus_P[o]    += Σ_i input[i] · crosspoint[i][o](t) · (cue_out[o](t) · voice_gain)
dev[d]       = Σ_P Σ_o bus_P[o] · patch_P[o][d](t)
out[d]       = delay_d(dev[d]) · out_gain[d](t) · global_master(t)
out_gain[d]  = (mute ? 0 : level) · (invert ? −1 : 1)
```

- `t` is sample time and `p` is the voice's frame position after the skip.
- Each patch's bus is shared by all voices routed through it, so a patch matrix costs nothing per voice. Patches no voice uses in a block are skipped.
- Output level, mute and polarity act **after** the alignment delay, so the operator hears a mute or level change immediately.

| Command | Effect |
|---|---|
| `Ramp { voice, target, gain, fade }` | ramps one voice gain (`Master`, `CueOutput(o)`, `Crosspoint { input, output }`) to `gain` with `fade.curve` over `fade.frames` |
| `SetPatchRouting { patch, cue_output, device_output, gain }` | one patch's cue output → device output gain (default: identity in every patch) (AUD-07) |
| `SnapPatch { patch }` | the patch matrix takes the gains its routing is ramping to at once (a matrix just given to a patch, which nothing plays through: a cue started on that sample is heard at its level, on that patch's outputs only); an unknown patch is ignored (`tests/mixer.rs::snap_patch_ends_the_routing_ramps_on_their_targets`, also under `assert_no_alloc`) |
| `MoveVoice { voice, patch }` | moves a playing voice to another patch matrix (its cue's patch changed, #293): for the de-zipper time it plays into both, crossfading linearly from the old patch to the new, so neither set of outputs clicks and the two always add up to the voice. A voice not heard yet (moved on its start sample, or paused past its declick tail) moves at once. A move asked for during a move's crossfade waits for it and starts with the next segment rendered (at most a block later; the latest wins). Ignored for an unknown voice, a patch out of range, or the patch the voice is on. Tests: `tests/mixer.rs` (`a_moved_voice_crossfades_…`, `a_move_…`, `moving_a_voice_does_not_allocate`) |
| `SetOutput { output, settings }` | per device output: level, mute, polarity, delay (AUD-07, PB-11) |
| `SetMaster { gain }` | global master (PB-11). The control loop does not send it: each workspace's master is folded into its patch matrices (docs/control.md, Patches), so the mixer master stays at unity |

- Gains are linear `f32`. Use `db_to_linear` and `linear_to_db`, where −120 dB and below is exactly 0.0 (ARCHITECTURE §4).
- Every gain the mixer applies is clamped to ±`MAX_GAIN` (+24 dB, `MAX_GAIN_DB`). This is a safety net; the show model stops at +12 dB. NaN counts as 0.0. A patch matrix gain carries the workspace master × patch master × output level (M4.15b, docs/control.md "Patches"), so the clamp applies to their sum in dB: settings adding up to more than +24 dB (each at most +12) are heard at +24 dB. Before M4.15b the workspace master was a separate mixer stage with its own clamp.
- Commands that name an unknown voice, an index out of range, or an input beyond the voice's channel count are ignored.
- Silent crosspoints, cue outputs and patch entries (constant 0) are skipped per block, so cost scales with the routes in use.
- **Delay**: a per-output delay line preallocated for 1 s, in frames, clamped to that maximum. A delay change crossfades from the old tap to the new one over the de-zipper time. A change requested during a running crossfade waits for it to finish, so the output never jumps.

### De-zipper

Gain changes without an explicit ramp (`Ramp` with `fade.frames == 0`, `SetPatchRouting`, `SetOutput`, `SetMaster`) move linearly over 5 ms (240 frames at 48 kHz), so no change can click. A polarity flip passes through zero. The exception is `Ramp` gain changes scheduled on a voice's start sample, which take effect immediately because nothing has been heard yet.

### Ganging (AUD-08)

Gangs exist only on the control side. `MixerController::ramp_gang(at, voice, members, delta_db, fade)` moves every member by the same dB offset, keeping their relative levels.

- A member at silence stays silent.
- Results are capped at +24 dB.
- The member ramps go out as one batch (see Scheduling horizon), so the gang moves on one sample.

## Fades and curves (AUD-09, AUD-10)

### Gain domain

Fades interpolate **linear amplitude**. The perceptual shape comes from the curve, not from interpolating in dB, so a fade can start or end at true silence (exactly 0.0) without a −∞ special case.

### Ramp definition

A ramp from gain `a` to `b` over `N` frames that starts on sample `S` gives:

```
gain(S + k) = a + (b − a) · s(k / N)    for 0 ≤ k ≤ N, then b
s(t) = f(t)            if b ≥ a   (rising)
s(t) = 1 − f(1 − t)    if b < a   (falling)
```

- Sample `S` is exactly `a`. Sample `S + N` is exactly `b`, so a fade-out to silence is exactly 0.0 on that sample. Values are computed in `f64` and cast to `f32`.
- A falling ramp traces the rising ramp's gain path backwards: a fade-out is the time-reverse of the matching fade-in.
- A new ramp starts from the gain the old one would have had on its first sample, so interrupting a fade is continuous.
- Each sample is a function of the curve id, endpoints and position only. There are no tables and no allocation.

`ramp_value` (and `Curve::shape`) evaluate this formula exactly, one `sin`/`cos`/`exp_m1` per call; `openlab-core`'s level maths is tested bit-for-bit against them. The mixer (gain ramps, integrated fade-in/out and refades, stop fades) evaluates it faster, by a recurrence between exact knots (`CurveRamp` in `gain.rs`):

- At ramp positions that are multiples of 64 the state is computed exactly: `(cos φ, sin φ)` for EqualPower (`φ = πt/2`) and SCurve (`φ = πt`), `exp(kt)` or `exp(k(1 − t))` for Exponential and Logarithmic. Each following frame rotates `(cos φ, sin φ)` by the per-frame angle, or multiplies by `exp(±k/N)`; the progress `s` comes from the state (`sin φ`, `1 − cos φ`, `(1 − cos φ)/2`, `(y − 1)/(e^k − 1)` or its complement). Linear stays exact.
- `Param::fill` and the integrated fades evaluate a whole stretch between knots at once (`CurveRamp::apply`), with the curve chosen once per stretch; each frame still gets the same operations as a single evaluation, so the bits are the same (unit- and property-tested).
- The knots depend only on the ramp position, and any position (after a skip, a seek or a pause) is reached by the same operations from its knot, so a value is a pure function of ramp and position: output never depends on how the timeline is cut into blocks.
- Position 0 is exactly `a` and position `N` (and beyond) exactly `b`, as above.
- **Bound**: `|mixer gain − ramp_value| ≤ RAMP_TOLERANCE · max(|a|, |b|)`, `RAMP_TOLERANCE = 1e-6` (−120 dB, the silence floor). At most 63 recurrence steps run from an exact knot, which adds about 1e-13 of full scale in `f64`; casting to `f32` can then land one `f32` step (≤ 1.2e-7 of full scale) away from `ramp_value`'s result. Unit tests check every curve, both directions, gains from 0 to +24 dB (and negative), lengths from 1 frame to four hours at 192 kHz, every frame of 2^20-frame ramps and the last frame before every knot of a four-hour ramp (the worst drift), and that any block split or skip gives the same bits. The worst deviation observed is 6e-11 of full scale, measured before the `f32` cast; after the cast the mixer can be one `f32` step (≤ ~1.2e-7 relative, 5.7e-8 measured at ±`MAX_GAIN`) from `ramp_value`, still within `RAMP_TOLERANCE`.

### Curves

Each shape `f: [0,1] → [0,1]` has `f(0) = 0` and `f(1) = 1` exactly, and is continuous and monotonic (unit-tested). `t` is clamped to `[0, 1]`, and NaN counts as 0.

| Curve | `f(t)` | `f(0.5)` | Notes |
|---|---|---|---|
| Linear | `t` | 0.5 (−6.0 dB) | |
| EqualPower | `sin(πt/2)` | 0.7071 (−3.0 dB) | from/to silence, fade-in² + fade-out² = 1 throughout a crossfade |
| SCurve | `(1 − cos πt) / 2` | 0.5 | symmetric; zero slope at both ends |
| Exponential | `e(t) = (exp(kt) − 1) / (exp(k) − 1)`, `k = ln 1000` | 0.03065 (−30.3 dB) | close to dB-linear over 60 dB: slow start for a fade-in, fast initial drop for a fade-out |
| Logarithmic | `1 − e(1 − t)` | 0.96935 | point reflection of Exponential; with an Exponential fade-out it makes an amplitude-complementary crossfade |

Custom curves are out of scope for v0.0.1 (REQUIREMENTS §21).

### Integrated fades (AUD-10)

`fade_in` and `fade_out` are attached to the voice and follow its frame position `p` (frames consumed after the skip), not wall time. An underrun therefore does not shift them against the audio.

- **Fade-in** over `N` frames: `env_in(p) = ramp(0 → 1)` at step `p`. Frame 0 is exactly 0.0 and frame `N` is exactly 1.0.
- **Fade-out** over `N` frames with `play_frames = L`: `env_out(p) = ramp(1 → 0)` at step `p + N + 1 − L`, for steps ≥ 0. Frame `L − 1 − N` is exactly 1.0 and the **last played frame, `L − 1`, is exactly 0.0**. A fade-out longer than the playback starts part way along its curve. A fade-out without `L` is rejected (`BadFade`).
- When both fades overlap, their envelopes multiply. A `Ramp` on the voice master (e.g. a Fade cue) multiplies with them.

## Meters and xruns (AUD-19, NFR-27, ARCHITECTURE §3.1, §3.5)

The control thread reads meters at any rate (the engine samples them at ≤30 Hz). A read takes the peak since the previous read and resets it, so a peak landing between two reads shows in the next one, whatever the block sizes. Reads never block the audio thread.

**Metering points.**

| Meter | Point | Read with |
|---|---|---|
| Device output `d` | `out[d]` in the signal path: what goes to the device, after the patch, delay, output level, mute, polarity and master | `read_output_meters_into(&mut [f32])`, `output_meters()` |
| Voice cue output `o` | the voice's own contribution to `bus_P[o]`: after crosspoints, cue-output level, voice master, stop fade, integrated fades and pause declick; before the patch matrix | `read_voice_meters_into(&mut [VoiceId], &mut [f32])`, `voice_meters()` |

Values are absolute linear peaks (`linear_to_db` for display). NaN samples count as 0.0 and ±∞ reads as `f32::MAX`, so a meter is always finite and non-negative.

**Mechanism.** The audio thread tracks peaks in plain `f32`s while rendering and publishes them at the end of each `process` call: one `fetch_max` per non-silent meter on an `AtomicU32` (device outputs) or `AtomicU64` (voices). Non-negative finite floats order like their bit patterns, so `fetch_max` on the bits is a float max. The reader takes them with `swap(0)`, only after a plain load shows a word is non-zero, so it does not take the audio thread's cache lines for silent meters. With 64 voices × 2 routed cue outputs and 64 device outputs, a block costs at most 128 + 64 = 192 atomic read-modify-writes; a voice start, end or re-keying resume adds a few stores. No atomics are per sample; the per-sample work is one add and one compare per routed voice cue output (outputs a voice does not reach, playing or in a pause tail, cost nothing).

If `process` panics (a fault, ARCHITECTURE §3.4), the peaks it still held locally are dropped, since its output was replaced with silence. A voice that finished earlier in that call has already published its final peaks.

**Voice attribution.** Each voice-pool slot has its meters, one per cue output, and publishes the `VoiceId` it holds. A slot's *epoch* changes whenever it gets a new id (a start, or a resume with `new_id`), and every voice peak carries its epoch in its top 32 bits:

- `fetch_max` therefore never mixes two voices' peaks; a peak is reported only under the id whose epoch it carries, never under another voice's id.
- When the epoch changes, unread peaks of the old one move to a second word per meter, so a voice that ended or was re-keyed since the last read gets one more entry with its last non-zero peaks under its old id, even if its slot was reused meanwhile.
- The reader remembers each slot's id and epoch from its previous read. A voice it never saw (it started and ended between two reads) is not reported, and when a slot changes hands twice between two reads, the earlier voice's last peaks are dropped. At 30 Hz that is a voice shorter than about 33 ms.
- The slot's `(id, epoch, active)` is published with a sequence lock: the audio thread writes without waiting, and the reader retries (64 times, then leaves the slot for its next read).
- The reader reads a slot's identity before and after taking its peak words. If it changed in between (the voice ended, or was retired and replaced), the words go back, all into the retired word, where the audio thread cannot overwrite them with the next epoch's first peak, and the slot waits for the next read with the reader's memory of it unchanged. So a voice that ends or is re-keyed while a read is under way still gets its last peaks reported once, under its own id.
- Every active voice has an entry on every read: a paused voice is present with 0.0 once its declick tail has played; a resumed voice reports under its new id from the resume on.
- `voice_meter_capacity()` (two per slot) sizes the buffers for a complete read. Slots that do not fit are left for the next read, which starts at the first of them, so a short buffer serves every slot in turn. The `_into` reads allocate nothing.
- An epoch is a per-slot `u32`; after 2³² reuses of one slot it wraps and that slot can drop one read's peaks.

Memory: per slot and cue output, two `AtomicU64` plus an `f32` (20 bytes); per slot, about 40 bytes (identity and the reader's copy). Default config (128 voices × 2 outputs): about 10 KB. The maximum (4096 voices × 64 outputs): about 5.4 MB, small next to those voices' input buffers.

**Xruns.** `xruns()` is a monotonic count of xruns reported by the device layer. The device layer reports host xruns and timestamp gaps through it (see Device layer); `Mixer::xrun_notifier()` returns an `XrunNotifier` (clone, `Send + Sync`) for the host's error callback on another thread. Both are one atomic add, so real-time safe. Voice underruns are not xruns; they are counted in `underrun_frames`. The offline driver injects xruns with `OfflineRenderer::inject_xruns(count)`. Logging the count and showing it to the operator are M4.7/M5 work.

## Device layer (AUD-01, AUD-03, AUD-21, NFR-08, ARCHITECTURE §3.1, §3.4, §3.5)

Three modules. Only `device.rs` uses cpal (0.18, Apache-2.0); it is behind the `device` cargo feature, on by default. The fuzz crate builds `openlab-audio` with `default-features = false`, so it needs no ALSA headers. The other two are pure and always built.

| Module | Does | Feature |
|---|---|---|
| `negotiate.rs` | `negotiate(&[ConfigRange], Request) -> Negotiated`, `Latency`, `reopen_delay_ms` | none |
| `output.rs` | `OutputHost` (the callback adapter), `HostLink`, `DeviceMonitor`, `ErrorReporter` | none |
| `device.rs` | `output_devices()`, `default_output_device_id()`, `open_output(mixer, &OpenRequest)`, `OutputStream` | `device` |

**Listing.** `output_devices()` lists the default host's output devices (CoreAudio on macOS, WASAPI on Windows, ALSA on Linux; ASIO later, REQUIREMENTS §21). Each `DeviceInfo` has a stable `id` (cpal's `DeviceId`, `"host:id"`, to store in settings), a `name`, `is_default` and its `configs`. Zero devices is a valid answer.

**Negotiation** (pure). The request is the mixer's rate and output count plus an optional buffer size.

- **Rate.** It must be supported exactly: the mixer's rate is fixed when it is built. Otherwise the result is `NegotiateError::SampleRate`, and the engine decides whether to rebuild the mixer at a rate the device runs at.
- **Channels.** The exact count if the device offers it; else the smallest larger count, with the extra device channels silent; else the largest smaller count. With fewer channels, mixer outputs `0..mapped` reach the device and the rest are not heard: `Negotiated::outputs_dropped` / `OutputStream::outputs_dropped`, for the engine to warn about. At most 64 mixer outputs are ever used. A device with more than 64 channels opens at its own count, and the channels above 64 stay silent.
- **Format** at that channel count: f32, then f64, i32, i16, u16. Other formats (i24, u8, DSD) are skipped. A device that offers none of these gives `NoConfigs`.
- **Buffer.** The requested size if the range includes it, else the nearest end of the range. If the host cannot report its range, the requested size is tried, and if the host refuses it, `open_output` falls back to the host default. With no request, the host default is used.

**Opening.** `open_output(mixer, &OpenRequest { device_id, buffer_frames })` negotiates, builds a cpal stream for the chosen format, and starts it. Any failure returns an `OpenError` with a `DeviceError { kind, message }`, where `kind` is one of NotFound, Unavailable, Busy, PermissionDenied, UnsupportedConfig, `Negotiate(..)` or Backend. **The mixer is never lost**: `OpenError::take_mixer()` hands it back. When the host cannot report the default device's id, the open fails with Backend instead of storing an empty id.

**The stream never follows the system default.** `device_id: None` means the device that is the default when `open_output` runs. It is resolved to that device's concrete id and opened by id (CoreAudio: an AUHAL unit with disconnect reporting, not the default-output unit), and `OutputStream::device_id()` reports the resolved id. cpal's default-device stream would follow the system default: CoreAudio reroutes it in place, and WASAPI invalidates it. Then a show could move to headphones that connect mid-show while `negotiated()` and `outputs_dropped()` still describe the old device. A reroute that a host reports anyway is `Rerouted`, which sets `needs_reopen()`.

**The callback.** The cpal data callback calls `OutputHost::render(buf, Some(CallbackTime))` for the buffer's format. Tests and any other driver call the same function with their own buffers.

- f32 buffers whose channel count equals the mixer's outputs are rendered in place.
- Every other buffer goes through a preallocated scratch buffer of `SCRATCH_FRAMES` (1024) frames × mixer outputs. A larger callback is rendered in pieces, so it never allocates. Conversion clamps to ±1 and NaN becomes 0: i16/u16 use ×32768 rounded and saturated, i32 uses ×2³¹.
- A buffer that is not whole frames gets its tail silenced and is counted as `bad_buffer`. So is a buffer in a format the stream was not opened with.
- The whole body runs inside `catch_unwind` on top of the mixer's own guard. A panic outside the mixer leaves the buffer silent and is counted in `callback_faults`.
- **Engine load (M4.7a, DX-03).** `render` reads `Instant::now()` before the body and `elapsed()` after it, both inside the `assert_no_alloc` guard and a `catch_unwind` (a panic in the timing silences the block and counts a callback fault), and keeps the worst `elapsed / block duration` (block duration = frames / rate) in one `AtomicU64` with `fetch_max` (parts per million plus one, so 0 means "nothing measured"; integer maths, no float). `DeviceMonitor::take_engine_load()` swaps it to 0 and returns the fraction (1.0: the callback took its whole block), or `None` when no callback ran since the previous call. Single consumer: the engine's status reporter. The clock read is the one sanctioned exception to "no syscalls" in the callback (ARCHITECTURE §3.4): it neither allocates nor locks and normally does not enter the kernel (macOS `clock_gettime(CLOCK_UPTIME_RAW)` reads the commpage like `mach_absolute_time`; Windows `QueryPerformanceCounter` normally reads the TSC, falling back to a kernel-mediated timer without an invariant TSC and on some VMs; Linux's vDSO). The CPU budget gate reports the pair's cost (tens of ns, against at least 1.3 ms per block).

**Debug allocation guard (M3.12, NFR-02).** In debug builds the callback body runs inside `assert_no_alloc::assert_no_alloc`. `assert_no_alloc` is a normal dependency with `warn_debug`: a violation is counted rather than aborting, so a debug build survives a mixer panic, whose unwinding allocates. That fault path is already loud, and it includes the M3-A L3 case where a `Box<VoiceStart>` is freed during unwinding. The callback adds the thread-local violation count to `DeviceMonitor::alloc_violations()`. On a violation the crate writes one line to stderr, on the audio thread, and only in a build that is already faulty. **Detection needs the binary to install `assert_no_alloc::AllocDisabler` as its global allocator in debug builds**; the engine binary does this in M4.2. Release builds skip the wrapper entirely.

**Errors and loss (NFR-08).** cpal's error callback, which runs on a host thread and sometimes on the audio thread, maps each error kind to a `HostError` and calls `ErrorReporter::report`. That is one or two atomic writes and never allocates; cpal's own error value may free its message string when it is dropped.

| cpal kind | `HostError` | Effect |
|---|---|---|
| DeviceNotAvailable | `Lost` | counted; sets the sticky `needs_reopen()` |
| StreamInvalidated | `Invalidated` | counted; sets `needs_reopen()` |
| DeviceChanged | `Rerouted` | counted; sets `needs_reopen()` (the output moved, so the negotiated configuration is stale) |
| RealtimeDenied | `RealtimeDenied` | counted |
| Xrun | `Xrun` | the mixer's `xruns` and `host_xruns()`, not an error count |
| anything else | `Other` | counted |

`DeviceMonitor::take_errors()` reads and resets the counts, so the control thread sees each report exactly once. **Single consumer:** every clone shares the counts, so one owner (the engine's control loop) takes them and passes them on; a second poller would steal reports. `needs_reopen()` and the other getters do not reset anything and may be read from anywhere. `HostError::needs_reopen()` is the rule (Lost, Invalidated, Rerouted). Some hosts just stop calling back on unplug. `callbacks()`, and `samples_rendered` standing still, show that, and the engine's stall watchdog (docs/control.md, Devices) decides what it means: a warning after max(500 ms, 20 buffer periods) without a callback, a loss after max(3 s, 50 periods). The callback counter is the one relaxed `fetch_add` the callback already makes.

**Mixer handoff.**

- The cpal closure owns the `OutputHost`, which owns the mixer and the scratch buffer.
- Dropping the host, wherever cpal drops the closure, pushes both into a one-slot `rtrb` queue. That is usually the thread that drops the stream on CoreAudio (but see `MixerPending` below), and the joined audio thread on WASAPI and ALSA. That push does not allocate, free or block (tested under `assert_no_alloc`).
- `OutputStream::close()` drops the cpal stream and then takes the mixer from the slot, with its voices, queues, clock and counters intact. The engine can reopen the same device or another one with it.
- If cpal still holds the callback, `close` returns `MixerPending`. **This is expected on macOS device loss, not an error.** cpal's CoreAudio disconnect/monitor thread holds the stream briefly while it delivers `DeviceNotAvailable`, so the AudioUnit, and with it the handoff, is dropped on that cpal thread a moment later. The engine polls `MixerPending::try_take` (e.g. each control tick) until the mixer arrives. The handoff is safe on any thread.
- `OutputStream` drops its stream before the slot, so dropping it without `close` frees the mixer on the dropping thread, never on the audio thread.
- The push cannot fail (one host per slot). If it ever did, the mixer would be leaked, not freed, and counted in `leaked_mixers`.

**Outage semantics (mechanism only).** While no stream runs, nothing calls the mixer, so `samples_rendered`, the master clock, is frozen, and every sample-time deadline waits with it. After a reopen, voices continue from the frame where they were cut off, and commands dated during the outage apply in the first block. What the show does is the engine's policy (ARCHITECTURE §3.5, M4.2g: the control loop stops every voice on the lost device at once, dated on the first sample after the outage, so none continues after a reopen; docs/control.md, Devices). The frozen clock is kept: the engine plays on one device, so no other time base exists.

**Reopen backoff.** `reopen_delay_ms(attempt)` gives 250, 500, 1000, 2000 and then 4000 ms for every later attempt. The engine owns the clock, the attempts and the user warning (docs/control.md, Devices).

**Latency (AUD-21).** `OutputStream::latency()` returns a `Latency`:

- `buffer_frames`: the host's current buffer size, else the last callback's size, else the fixed size requested. `buffer_seconds()` converts it at the stream rate.
- `device_seconds`: playback minus callback time from the last callback's timestamps, when the host gives usable ones. CoreAudio's figure includes the device latency and the safety offset. Other hosts differ.

**Xruns from timestamps (NFR-27).** A callback that starts more than 1.5 × the previous buffer's duration after the previous callback counts one xrun, both in the mixer's `xruns` and in `gap_xruns()`.

- If the host reported an xrun since the previous callback, that gap is not counted again. A host report that arrives only after the next callback can still count twice.
- A clock that steps back re-baselines without counting. Callbacks without timestamps are not checked.
- The 1.5× margin absorbs scheduling jitter. A missed buffer always exceeds it.

**Tests** (`tests/device.rs`, unit tests in the three modules) use no hardware:

- **Negotiation:** exact match, nearest buffer size, unknown range, unsupported rate, more and fewer channels, 64 and 128 channels, an empty list, format order. Also cpal-to-own config and error mapping, and the backoff schedule.
- **Rendering:** the host-driven output equals the offline driver's sample for sample in every format, with channel mapping and with chunked callbacks.
- **Handoff and loss:** a voice mid-fade continues sample-exactly after close and "reopen" with another buffer size, and after a reported loss with a reopen on a 4-channel device. Errors are counted exactly once.
- **Xruns:** gaps count and host reports are not counted twice.
- **No allocation** on every callback path and on the handoff drop.

A real unplug cannot be automated here; it is checked by hand. The `#[ignore]`d `hardware_smoke_default_output` test plays 0.5 s of silence on the default device and closes it: `cargo test -p openlab-audio --test device -- --ignored`. On a Linux VM with no sound card, setting `ALSA_CONFIG_PATH` to a file containing `pcm.!default { type null }` runs it against ALSA's null device.

## Waveform overview (AUD-14)

`waveform.rs` builds the data the waveform editor (M5.8) draws; the drawing and drag editing are not here. It runs on a worker thread, never the control or audio thread.

- **Format.** `Waveform { sample_rate, frames, channels, levels }`. Level 0 holds one `Peak { min, max }` per `BASE_FRAMES` (256) frames per channel; each further level merges neighbouring buckets (factor 2, so a zoom step never loses more than 2x resolution) until a level has one bucket covering the file. The last bucket of a level may cover fewer frames. Peaks are `i16` (full scale = 32767; `Peak::min_f32`/`max_f32` convert): half the size of `f32`, and a pixel column needs no more precision. Samples are clamped to +-1 first (a float file's overs show as full scale) and NaN/+-inf count as 0.
- **Size.** 4 bytes per bucket per channel, about twice level 0 over all levels: `frames / 256 x 2 x 4 x channels` bytes. A 30-minute stereo 48 kHz file (86.4 M frames) has 337,500 level-0 buckets and 20 levels: about 5.4 MB in all (2.7 MB for level 0).
- **Positions are file frames at the file's own rate**, the unit the trims and slices use in the show file (`start_time`, `end_time` and slice `time` are seconds from the start of the file; `plan` takes them as frames), so `seconds x sample_rate` is the frame to ask for.
- `WaveformBuilder::new(rate, channels, expected_frames)` / `push(interleaved)` / `finish()` is pure and takes chunks of any size (the result does not depend on the chunking). It reserves memory for `expected_frames` once and ignores frames beyond them, so a file longer than its probe cannot grow it.
- `build_waveform(media, &info, &cancel)` decodes with the regular decoder (the same one the probe and streams use, no second path) and feeds the builder in 4096-frame reads. `info` is the file's probe (`Probed::info`; the caller has it from loading the cue, so the file is not probed twice). `cancel` is checked before every read: setting it gives `WaveformError::Cancelled`. Damaged media still gets a waveform: `WaveformResult { waveform, warnings }` carries the decoder's warnings (missing frames are silence in it). A file with fewer frames than probed gives a waveform of the frames it has and a `Truncated` warning; one with more is read only up to the probed length, as a stream is. Channels or rate that differ from `info` are `MediaError::Changed`; an unplayable file is `WaveformError::Media(..)`. A decoder panic is `Corrupt`.
- `Waveform::range(channel, start, end, buckets)` returns `buckets` min/max pairs over frames `start..end` from the coarsest level that still has a source bucket per output bucket (level 0 if even that is too coarse: output buckets then repeat source buckets, so the editor reads samples directly at its finest zoom). `end` is clamped to the file; an output bucket takes in every source bucket it overlaps, so edges round outwards by up to one source bucket. Empty for an unknown channel, `buckets == 0`, or an empty or inverted range; never more buckets than frames or `MAX_RANGE_BUCKETS` (65536).

## Offline render driver (QA-03, ARCHITECTURE §3.6)

`OfflineRenderer::new(mixer)` calls `process_guarded` in a loop, writing into memory with no device and no sleeps. `render(frames, blocks)` / `render_into(buf, blocks)` cycle through a list of block sizes, so tests cover fixed sizes and variable sizes (CoreAudio may deliver varying sizes). `render_into` does not allocate, so tests wrap it in `assert_no_alloc`. Tests that schedule beyond the lookahead call `MixerController::pump()` between blocks, as the engine's control loop will.

## CPU budget (NFR-23, QA-06, M3.10)

`tests/cpu_budget.rs` is a wall-clock gate, `#[ignore]`d so `cargo test` never runs it (and a no-op in a debug build). CI runs it on the Intel runner: `cargo test --release -p openlab-audio --test cpu_budget -- --ignored --nocapture`. It prints the ratio and per-block p50/p99/max of each run.

- **Workload**: 64 slots of stereo voices at 48 kHz, each fed through its ring. Every voice has a 1.5 s fade-in and an integrated 2 s fade-out (curves rotate over all five by voice id), lives 8 s and is replaced 0.5 s before its end (a crossfade), so about 64 to 72 voices play and fades run throughout the measured window; the initial lives are staggered from 4 to 8 s. Every second, a 600 ms voice-output ramp and a crosspoint ramp run on every voice (curves cycled by slot). Two cue outputs per voice, routed through four patches onto 16 device outputs, master on, meters publishing every block. 2 s of warm-up, then 64 s measured; about 590 voice starts in all.
- **Gate**: block 512, best of three runs; passes as soon as one reaches 10x real time. It runs first, so the report-only cases cannot pre-empt it. Blocks of 256 and 64 and the resampled variant are reported only, and a failure in them is printed, not asserted. Block 64 is near 10x and **not** gated: NFR-23 is met at 512-frame blocks.
- **Timed**: only `OfflineRenderer::render_into`, one block at a time, on the rendering thread. **Not timed**: topping up the rings, sending commands, reading meters and polling events (all between groups of 12288 frames), disk decode and the resampler.
- **Resampled variant** (44.1 kHz in-memory files through `DiskStreamer` into the 48 kHz mixer): resampling runs on the disk workers, off the rendering thread, which sees the same ring as before.
- The assertions also require no underrun, no early end of a current voice, no fault and a non-silent finite mix, so a starved voice cannot flatter the ratio. Run it alone: the single test function times its cases in sequence.
- **Figures** (release, 4-core Linux VM, not the Intel runner): block 512 28.9x to 31.9x (p50 0.36 ms of a 10.7 ms budget); 256 26.0x to 30.2x; 64 17.3x to 21.1x; resampled 512 28.4x to 30.0x. History at block 512: 8.2x with per-sample `sin`/`cos`/`exp_m1` curve evaluation; 14x with the knot recurrence (see Ramp definition); about 30x since M3.19 (curves evaluated per stretch between knots, steady gains applied as scalars instead of filled buffers, fade envelopes applied per stretch, meter peaks taken over eight lanes, a delay line without a delay or crossfade only stores the block, the ring read de-interleaved per channel). The output is bit-identical to before M3.19 (same checksum of every output sample and meter reading of this workload at blocks 512, 256 and 64).
- **Resampled variant on small machines**: the two disk workers (`DEFAULT_WORKERS`, as the engine uses) run while blocks are timed, and since the test renders about 30x faster than real time they must decode and resample about 30x faster than in a show, which costs about as much CPU as the rendering. With fewer free cores than the three busy threads the rendering thread is preempted and the preemption is counted as render time: on this VM pinned to two cores the resampled case gives 23x (rings 30x), on one core 12.6x. The Intel runner's lower resampled figure (7.4x against 10.4x before M3.19) fits this. The benchmark is left as it is: it overstates the workers' load, never understates the render cost.
