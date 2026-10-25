# openLab audio engine (`openlab-audio`)

How the real-time mixer works and the exact semantics its tests pin down. Architecture context: [ARCHITECTURE §3](ARCHITECTURE.md#3-engine-internals). Requirement IDs refer to [`REQUIREMENTS.md`](../REQUIREMENTS.md).

Status: mixer, fades, routing, per-voice patches and offline render driver (M3.1, M3.2, M3.4, M3.5, M3.11); decoding, resampling, disk streaming and preload (M3.3); slices, loops and devamp (M3.6). Meters (M3.7) and the device layer (M3.8) are not built yet.

## Threads and queues

`Mixer::new(config)` returns a pair:

- **`MixerController`** (control thread) schedules time-stamped `Command`s, polls `Event`s, frees returned voice starts, and reads the clock (`samples_rendered`) and counters (faults, underrun frames, dropped events, leaked starts, pending-full blocks).
- **`Mixer`** (audio thread) renders with `process(out, frames)` into an interleaved `f32` buffer. The device callback and the offline renderer call `process_guarded`, which wraps `process` in `catch_unwind`: on a panic the block is silent, the fault counter goes up, and the clock still advances (ARCHITECTURE §3.4).

They share only preallocated `rtrb` SPSC queues and atomics:

| Queue | Direction | Capacity | Contents |
|---|---|---|---|
| commands | control → audio | 1024 | `Command { at, kind }` |
| events | audio → control | 1024 | `Event` (small, `Copy`, with its exact sample time `at`). When full, events are dropped and counted (`dropped_events`). Events from the same block are not sorted by `at` |
| returns | audio → control | 1024 + 4096 + voices + 1 | `Box<VoiceStart>` (source and routing) the mixer is finished with |

The openlab-audio crate does not depend on `openlab-core`; the engine translates core actions into mixer commands.

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

**Tests** run `process` under `assert_no_alloc` (a dev-dependency set to count violations), so any allocation on these paths fails the test. Paths covered: voice start/stop, ramps, underrun, voice end, rejection, 64 simultaneous voices, full event queue, full return queue, full pending set, slice tables, loops, devamp (stop, continue, cut and resume), and every disk-streaming test (`tests/stream.rs`, `tests/slices.rs`).

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

## Decoding (AUD-04)

`decode.rs` wraps symphonia 0.6. It runs on disk and I/O threads only, never on the audio thread.

- **Formats**: WAV and AIFF PCM (integer and float; 16/24/32-bit int and 32-bit float tested), FLAC, ALAC (M4A), MP3, AAC (M4A), Ogg Vorbis (optional, §21). 1–64 channels, 1–384000 Hz; anything else is `Unsupported`.
- `probe(path)` / `probe_media(media)` return `Probed { info: MediaInfo { channels, sample_rate, frames, duration }, warnings }`. **`frames` is exact** on the playback timeline: a PCM header's length is checked by decoding the tail; every compressed file is decoded once in full and counted. The full decode also finds any damage, so it is known when the cue loads, not during the show.
- **Damaged files play, with warnings** (`MediaWarning { kind, at, frames }`, positions in frames at the file's rate on the playback timeline, each reported once, at most 256 per file):
  - `Missing`: frames the container skipped (a FLAC frame failing its CRC, a lost Ogg page, a spliced file) or a packet that did not decode (AAC, MP3). They **play as silence of their length**, so every later frame keeps its position and seeks, loops and slices land exactly. Damage in the first packets makes silence from position 0.
  - `Repeated`: frames repeating positions already played; dropped.
  - `Truncated`: the file has `frames` fewer frames than its header declares and plays to its real end, `at` (the probed length). For lossy formats the missing frames may lie anywhere (an MP3 frame the reader skipped).
  - `Unreadable`: the container keeps failing (16 errors in a row) or its timestamps jump by more than a minute: the file plays to `at`.
  - Only a file with nothing playable is an error (`Unsupported`, `Corrupt`, `Io`, `NotFound`).
  - Known limits: symphonia's FLAC reader stops at damage in a very short file (reported as `Truncated`); an Ogg file whose damage also defeats symphonia's length scan has no declared length, so audio lost at its end cannot be reported. In a damaged Vorbis file the page after a lost page plays in the lost page's place and the silence comes one page later (symphonia's Ogg reader); straight playback, preload and seeks all agree on that timeline, which the warning describes.
- Errors are typed (`MediaError`): `NotFound`, `Io`, `Unsupported`, `Corrupt`, `Truncated { expected, actual }` (a stream's file has become shorter than its probe), `Changed` (the file no longer matches its probe), `BadSlices`, `TooManyStreams`, `Worker`, `TooLarge`. A panic inside symphonia (a decoder bug hit by malformed data) is caught and becomes `Corrupt`; it never takes a thread down.
- **Positions.** Position 0 is the first playable frame. The decoder applies every packet's encoder delay and padding itself (the same way for every codec), so an MP3 with LAME gapless info has its original length and Vorbis follows its granule positions. A packet never counts for more than its container duration, whether or not it decodes (a failed packet is silence of that duration), so the timeline does not depend on decoder state; this also drops AAC padding past the MP4's length. AAC priming is not trimmed: symphonia does not read the edit list, so AAC files start with the encoder's priming frames (about 1000–2000).
- **Continuity.** When the track's time base is a whole fraction of a frame (every format above), each packet's position comes from its timestamp; a gap before it is filled with silence (`Missing`), an overlap is dropped (`Repeated`). Position 0 is the container's start. Otherwise positions are counted, and a seek decodes forward from the start (an MP4 whose timescale is not a multiple of the rate, e.g. 90000 at 44.1 kHz; also any seek the container refuses): every loop seam then costs time proportional to its position in the file, so preload such files if they loop. A seek into a gap gives the same silence as reading through.
- **Seeking is sample-accurate.** A seek starts a fresh container reader and a fresh decoder (symphonia 0.6.1's FLAC reader returns stale frames after some seeks; its AAC `reset` keeps state), seeks to a pre-roll before the target (8192 frames for lossy codecs, none for lossless), drops the first packet after a mid-file lossy seek (it lacks its predecessor's overlap or bit reservoir), and discards frames up to the target by position. A seek that lands after its target is retried with twice the pre-roll, then from the start; one the container refuses decodes forward from the start. Afterwards every format yields exactly the frames a decode from the start yields, except AAC files that use noise substitution (ffmpeg's encoder does): those bands are pseudo-random and differ by under 0.003 after a mid-file seek.
- **Headers are checked first**: a WAV `fmt ` or AIFF `COMM` chunk with 0 or more than 64 channels, or more than 64 bits per sample, is `Corrupt` before symphonia parses it (its frame-size arithmetic overflows on such headers).
- **Samples are made safe**: a non-finite sample (NaN, ±∞, possible in float files) becomes 0.0 and the rest are clamped to ±16.0 (+24 dBFS, the mixer's gain ceiling), so one bad sample can never poison a mix bus or the resampler.
- `preload(media)` decodes a whole file into memory (`Preloaded`, AUD-05), at the file's rate; `bytes()` reports its size. It uses at most `DEFAULT_PRELOAD_BUDGET` (512 MiB, about 46 minutes of 48 kHz stereo); `preload_within(media, max_bytes)` sets another budget. The declared length is checked before decoding and the decoded length while decoding (a small compressed file can expand far beyond what it declares); over budget, or an allocation the system refuses, is `TooLarge { bytes, limit }`. The engine decides whether the cue then streams or is reported. The decode buffer is kept as is, with no final copy.

## Disk streaming and preload (AUD-05, ARCHITECTURE §3.1, §3.5)

`DiskStreamer::new(StreamerConfig)` starts a pool of disk workers (default 2) that decode and resample each stream into its voice's ring, about `buffer_ms` (default 2000) ahead.

- `prepare(StreamRequest { media, info, slices })` checks the slice table against `info` and the open-stream limit (`max_streams`, default 256), creates the ring, and hands the stream to a worker. It does no I/O; the worker opens the file. It returns a `PreparedStream`; `into_voice(voice, cue_outputs)` makes the silent `VoiceStart` (with `slices` and `link`) and keeps the `StreamHandle`. Dropping the handle cancels the stream.
- **Primed** (`is_primed`): the first chunk (4096 frames, or the whole stream if shorter) is in the ring; the cue is loaded. `is_finished`: every frame of the plan is in the ring. `frames_written`, `cuts`, and blocking `wait_*` helpers for I/O threads and tests (never the control thread).
- **Resampling** (`resample.rs`): rubato's synchronous FFT resampler when the file rate differs from the device rate, a pass-through otherwise. Device frame `p` is the signal at `p / device rate`, whether reached by reading or by seeking (a seek restarts the resampler on a chunk boundary one chunk early and discards its delay). The length is exactly `round(frames · device rate / file rate)` (`MediaInfo::frames_at`).
- **Loops** are unrolled into the ring: at each loop point the worker seeks back ahead of time, so a loop's own seam never waits on the disk. Trims and start offsets are seeks too.
- **Loop exits.** Leaving a loop early (devamp) is the one place the ring cannot be ahead: it is FIFO and full of loop plays that will not be heard. So before its first ring write, the worker decodes each loop exit's continuation (the slice table unrolled from the slice after the loop) for `exit_ms` (`DEFAULT_EXIT_MS`, 250 ms) into an exit buffer published on the voice's `StreamLink`. At the exit the voice plays that buffer while it drops the stale plays from the ring, and the worker refills the ring from where the buffer ends. The worker therefore has the whole 250 ms to notice the freed ring (5 ms poll), seek (fresh demuxer and pre-roll, up to about 10 ms on long compressed files) and decode its first chunk, with up to one 8192-frame device block (170 ms at 48 kHz) still taken ahead. Memory: one buffer per distinct loop exit, 250 ms × channels × 4 bytes (96 KB for stereo at 48 kHz). A devamp while a previous exit's buffer is still playing is ignored (`DevampIgnored`). `StreamHandle::last_resume` tells where the continuation starts in the ring (diagnostics and tests).
- **Preload** streams `Media::Preloaded` through the same resampler and ring, so it plays exactly as streaming does (tested sample for sample).
- **Damage while streaming** is handled exactly as the probe handles it: missing frames play as silence and every warning met is listed by `StreamHandle::warnings()` (damage the probe reported shows up again; anything else is new, i.e. the file changed after its probe). The stream reads nothing past the probed length, so damage beyond a file's playable end is never reached.
- **Failures.** A stream that cannot continue (a read error, a file that became shorter than its probe or no longer matches it, a worker panic) sets `StreamHandle::error` and lets go of its ring. The error is published only after the ring's producer is dropped, so once a caller sees it the voice is certain to play out what was written and end with `EndOfStream`. A file that ends before its probed length is `Truncated`, never padded with silence: silence is only ever played for damage that is also reported.
- **Underrun**: a slow disk gives the mixer's underrun behaviour (silence, counted, resumes where it left off).
- Dropping the `DiskStreamer` tells the workers to stop and does not wait for them, so a worker blocked on a stalled disk cannot hang the caller.
- Cost per loop-point seek (release, 10-minute files): about 0.03 ms WAV, 0.1 ms FLAC, 0.6–0.8 ms MP3/AAC, 1.6 ms Vorbis.

## Slices, loops and devamp (AUD-11, AUD-12, CT-16)

`plan(range, marks, plays, start)` builds a slice table in device frames: the in/out range cut at `marks`, `plays[i]` (`Times(n ≥ 1)` or `Infinite`) for slice `i` (empty = each once), starting at `start` inside the range. A start inside a looping slice splits it into a one-play entry and an entry for the remaining plays, with the same `index`. Bad input is a `PlanError`; `total_frames` is `None` if anything loops forever.

The same table drives the disk stream (which unrolls it into the ring) and the voice (which counts frames to find boundaries), so both agree exactly. Events, with the exact sample `at`:

| Event | When |
|---|---|
| `LoopReached { slice, count, at }` | a play of `slice` ended and it plays again from `at`; `count` plays done so far |
| `SliceEnded { slice, at }` | the slice's last play ended; the next slice, if any, starts on `at` |
| `DevampScheduled { at }` | a devamp was accepted; `at` is where the voice leaves the loop unless an underrun intervenes |
| `DevampIgnored { at }` | a devamp that cannot be carried out (no such voice playing, no slice table, a devamp already under way, or no `link` when one is needed); playback is unchanged |

Voices without a slice table report only `VoiceFinished`.

**Devamp** (`CommandKind::Devamp { voice, stop }`, CT-16) leaves the current loop at the end of the play under way when the command applies (a devamp on a loop boundary sample exits at the end of the play that starts there):

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
| `SetOutput { output, settings }` | per device output: level, mute, polarity, delay (AUD-07, PB-11) |
| `SetMaster { gain }` | global master (PB-11) |

- Gains are linear `f32`. Use `db_to_linear` and `linear_to_db`, where −120 dB and below is exactly 0.0 (ARCHITECTURE §4).
- Every gain the mixer applies is clamped to ±`MAX_GAIN` (+24 dB, `MAX_GAIN_DB`). This is a safety net; the show model stops at +12 dB. NaN counts as 0.0.
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
- Each sample is evaluated from the curve id, endpoints and position. There are no tables and no allocation.

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

## Offline render driver (QA-03, ARCHITECTURE §3.6)

`OfflineRenderer::new(mixer)` calls `process_guarded` in a loop, writing into memory with no device and no sleeps. `render(frames, blocks)` / `render_into(buf, blocks)` cycle through a list of block sizes, so tests cover fixed sizes and variable sizes (CoreAudio may deliver varying sizes). `render_into` does not allocate, so tests wrap it in `assert_no_alloc`. Tests that schedule beyond the lookahead call `MixerController::pump()` between blocks, as the engine's control loop will.

## Not yet built

- Short loops seek once per play (see the costs above). A loop cache (keep a short loop's frames after its first play) would remove those seeks and make looped AAC plays bit-identical (M3.13).
- AAC priming frames are not trimmed (symphonia does not read MP4 edit lists) (M3.14).
- `assert_no_alloc` around the device callback in debug builds is wired with the device layer (M3.12, with M3.8). It is a dev-dependency today.
- Curve evaluation calls `sin`/`cos`/`exp_m1` per sample during ramps. If the M3.10 CPU budget needs it, switch to incremental recurrences.
