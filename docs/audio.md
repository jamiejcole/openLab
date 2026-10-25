# openLab audio engine (`openlab-audio`)

How the real-time mixer works and the exact semantics its tests pin down. Architecture context: [ARCHITECTURE §3](ARCHITECTURE.md#3-engine-internals). Requirement IDs refer to [`REQUIREMENTS.md`](../REQUIREMENTS.md).

Status: mixer, fades, routing and offline render driver (M3.1, M3.2, M3.4, M3.5). Decoding/disk streaming (M3.3), loops (M3.6), meters (M3.7) and the device layer (M3.8) are not built yet.

## Threads and queues

`Mixer::new(config)` returns a pair:

- **`MixerController`** (control thread) sends time-stamped `Command`s, polls `Event`s, drains returned sources, and reads the clock (`samples_rendered`) and counters (faults, underrun frames, dropped events, leaked sources).
- **`Mixer`** (audio thread) renders with `process(out, frames)` into an interleaved `f32` buffer. The device callback and the offline renderer call `process_guarded`, which wraps `process` in `catch_unwind`: on a panic the block is silent, the fault counter goes up, and the clock still advances (ARCHITECTURE §3.4).

They share only preallocated `rtrb` SPSC queues and atomics:

| Queue | Direction | Capacity | Contents |
|---|---|---|---|
| commands | control → audio | 1024 | `Command { at, kind }` |
| events | audio → control | 1024 | `Event` (small, `Copy`, with its exact sample time `at`); when full, events are dropped and counted. Events from the same block are not sorted by `at` |
| returns | audio → control | 2·1024 + voices + 1 | sources (`rtrb::Consumer<f32>`) the mixer is finished with |

The openlab-audio crate does not depend on `openlab-core`; the engine translates core actions into mixer commands.

### Real-time rules

Everything `process` can reach is allocation-free, lock-free, and makes no syscalls or log calls. All buffers (voice pool, per-voice gain matrices, scratch, delay lines, the pending command list) are sized in `Mixer::new` from `MixerConfig`:

| Field | Default | Limit |
|---|---|---|
| `sample_rate` | 48000 | > 0 |
| `output_channels` | 2 | 1–64 (the mixer has the same number of cue outputs) |
| `max_block_frames` | 1024 | > 0; the internal chunk size; any device block size is accepted and split |
| `max_voices` | 128 | > 0 |
| `max_voice_channels` | 8 | 1–64 |

A source is moved into the mixer inside `StartVoice` (a move, no allocation). When the voice ends or is rejected, the source goes back on the return queue so it is freed on the control thread. `MixerController::send` drains the return queue before every command, which keeps that queue within the capacity above. If it were ever full anyway, the mixer leaks the source rather than freeing it on the audio thread, and counts the leak (`leaked_sources`, expected to stay 0).

Tests run `process` under `assert_no_alloc` (a dev-dependency set to count violations), so any allocation on these paths fails the test: voice start/stop, ramps, underrun, voice end, rejection, and 64 simultaneous voices.

## Timing (NFR-21, NFR-22)

- `SampleTime` is a `u64` frame count at the device rate. The mixer's clock is the number of frames it has rendered and is published after each `process` call as `samples_rendered` (ARCHITECTURE §3.2).
- Every command carries an absolute `at`. The mixer splits each block at command boundaries and applies a command **before rendering sample `at`**. A command whose `at` has already passed applies at the start of the next block.
- Commands need not arrive in time order. The mixer keeps up to 1024 pending commands sorted by `at`. Commands with equal `at` apply in the order they were sent, so `StartVoice` followed by gain commands at the same time behaves as expected. Limit: if 1024 far-future commands are pending, newer commands wait in the queue until one is applied.
- The result is independent of block size: a scripted command sequence renders identically for any pattern of block sizes, including size 1, primes, and blocks larger than `max_block_frames` (property test).

## Voices

`StartVoice(VoiceStart)` fields:

| Field | Meaning |
|---|---|
| `voice` | caller-chosen `VoiceId`, unique among active voices |
| `source` | `rtrb::Consumer<f32>` of interleaved frames at the device rate (filled by the disk thread) |
| `channels` | 1..=`max_voice_channels` |
| `gain` | voice master gain (linear) |
| `skip_frames` | frames discarded from the source first (in point) |
| `play_frames` | frames played after the skip (out point); `None` = until end of stream |
| `fade_in`, `fade_out` | integrated fades (see below); the fade-out needs `play_frames` |

A new voice starts with all cue-output gains at 1.0 and identity crosspoints (input *n* → cue output *n*). Its first source frame plays on sample `at` exactly, so voices started with the same `at` begin on the same output frame.

Start is **rejected** with `Event::VoiceRejected` (and the source is returned) for `PoolFull`, `DuplicateId`, or `BadChannelCount`. Nothing panics.

A voice **ends** with `Event::VoiceFinished { at, reason }`, where `at` is the first sample after its last:

- `EndOfStream`: the producer was dropped and every whole frame has been consumed. Dropping the producer is the end-of-stream marker.
- `OutPoint`: `play_frames` frames have been played.
- `Stopped`: `StopVoice` with `fade.frames == 0` cuts on sample `at` exactly. With a fade, the voice's stop gain ramps to 0 over `fade.frames` and the voice finishes at `at + fade.frames`, the first sample where that gain is exactly 0.

**Underrun**: if the source has fewer frames than needed and has not ended, the missing frames are silence, they are counted (`underrun_frames`), and one `Event::Underrun { at }` is sent per episode (at the first missing frame). Playback resumes when data arrives. Missing frames are not skipped: the voice resumes where it left off, so its fades stay aligned with the audio.

**Trims** (AUD-11): the disk thread normally seeks to the in point; `skip_frames`/`play_frames` give the same sample-exact in and out points on the mixer side.

## Signal path (ARCHITECTURE §3.5)

Per sample, for each voice and its input channel *i*, cue output *o*, and device output *d*:

```
voice_gain   = master(t) · stop(t) · fade_env(p)
bus[o]      += Σ_i input[i] · crosspoint[i][o](t) · (cue_out[o](t) · voice_gain)
dev[d]       = Σ_o bus[o] · patch[o][d](t)
out[d]       = delay_d( dev[d] · out_gain[d](t) ) · global_master(t)
out_gain[d]  = (mute ? 0 : level) · (invert ? −1 : 1)
```

`t` is sample time and `p` is the voice's frame position after the skip. `bus` is shared by all voices, so patch routing costs nothing per voice.

| Command | Effect |
|---|---|
| `Ramp { voice, target, gain, fade }` | ramps one voice gain (`Master`, `CueOutput(o)`, `Crosspoint { input, output }`) to `gain` with `fade.curve` over `fade.frames` |
| `SetPatchRouting { cue_output, device_output, gain }` | cue output → device output gain (default: identity) (AUD-07) |
| `SetOutput { output, settings }` | per device output: level, mute, polarity, delay (AUD-07, PB-11) |
| `SetMaster { gain }` | global master (PB-11) |

- Gains are linear `f32`. Use `db_to_linear` and `linear_to_db`, where −120 dB and below is exactly 0.0 (ARCHITECTURE §4). Non-finite gains count as 0.0.
- Commands that name an unknown voice, an index out of range, or an input beyond the voice's channel count are ignored.
- Silent crosspoints and cue outputs (constant 0) are skipped per block, so cost scales with the routes in use.
- **Delay**: a per-output delay line preallocated for 1 s, in frames, clamped to that maximum. A delay change crossfades from the old tap to the new one over the de-zipper time.

### De-zipper

Gain changes without an explicit ramp (`Ramp` with `fade.frames == 0`, `SetPatchRouting`, `SetOutput`, `SetMaster`) move linearly over 5 ms (240 frames at 48 kHz), so no change can click. A polarity flip passes through zero. The exception is `Ramp` gain changes scheduled on a voice's start sample: they take effect immediately, because nothing has been heard yet. This is how a voice starts with its intended matrix.

### Ganging (AUD-08)

Gangs exist only on the control side. `MixerController::ramp_gang(at, voice, members, delta_db, fade)` moves every member by the same dB offset, keeping their relative levels. A member at silence stays silent. All of the member ramps are queued, or none are, so the gang always moves on one sample.

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

Each shape `f: [0,1] → [0,1]` has `f(0) = 0` and `f(1) = 1` exactly, and is continuous and monotonic (unit-tested).

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
- **Fade-out** over `N` frames with `play_frames = L`: `env_out(p) = ramp(1 → 0)` at step `p + N + 1 − L`, for steps ≥ 0. Frame `L − 1 − N` is exactly 1.0 and the **last played frame, `L − 1`, is exactly 0.0**. A fade-out longer than the playback starts part way along its curve.
- When both fades overlap, their envelopes multiply. A `Ramp` on the voice master (e.g. a Fade cue) multiplies with them.

## Offline render driver (QA-03, ARCHITECTURE §3.6)

`OfflineRenderer::new(mixer)` calls `process_guarded` in a loop, writing into memory with no device and no sleeps. `render(frames, blocks)` / `render_into(buf, blocks)` cycle through a list of block sizes, so tests cover fixed sizes and variable sizes (CoreAudio may deliver varying sizes). `render_into` does not allocate, so tests wrap it in `assert_no_alloc`.

## Not yet built

- `assert_no_alloc` around the device callback in debug builds is wired with the device layer (M3.8). It is a dev-dependency today.
- One global patch (cue output → device output matrix). Per-cue patch selection with several patches would add a patch index to `VoiceStart`.
- Curve evaluation calls `sin`/`cos`/`exp_m1` per sample during ramps. If the M3.10 CPU budget needs it, switch to incremental recurrences.
