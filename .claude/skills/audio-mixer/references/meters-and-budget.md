# Meters, xruns, offline driver and CPU budget

## Meters (AUD-19, NFR-27)
- The control thread reads meters at any rate (the engine samples them at 30 Hz or less). A read takes the peak since the previous read and resets it, so a peak between two reads shows in the next one whatever the block sizes. Reads never block the audio thread.
- Values are absolute linear peaks (`linear_to_db` for display). NaN counts as 0.0 and +-inf reads as `f32::MAX`, so a meter is always finite and non-negative.

| Meter | Point | Read with |
|---|---|---|
| Device output `d` | `out[d]`: after the patch matrices (level, mute, polarity) and master | `read_output_meters_into(&mut [f32])`, `output_meters()` |
| Voice cue output `o` | the voice's own contribution to `bus_P[o]`: after crosspoints, cue-output level, voice master, stop fade, integrated fades and pause declick; before the patch matrix | `read_voice_meters_into(&mut [VoiceId], &mut [f32])`, `voice_meters()` |

**Mechanism**
- The audio thread tracks peaks in plain `f32`s while rendering and publishes them at the end of each `process`: one `fetch_max` per non-silent meter on an `AtomicU32` (outputs) or `AtomicU64` (voices). Non-negative finite floats order like their bit patterns, so `fetch_max` on the bits is a float max.
- The reader `swap(0)`s only after a plain load shows a non-zero word, so silent meters do not pull the audio thread's cache lines. No atomics are per sample.
- If `process` panics, the peaks it held locally are dropped (its output was replaced with silence). A voice that finished earlier in that call already published its final peaks.

**Voice attribution (invariant: a peak is never reported under another voice's id)**
- Each voice-pool slot has its meters, one per cue output, and publishes the `VoiceId` it holds. A slot's epoch changes whenever it gets a new id (a start, or a resume with `new_id`), and every voice peak carries its epoch in its top 32 bits, so `fetch_max` never mixes two voices' peaks.
- On an epoch change, unread peaks of the old one move to a second word per meter. A voice that ended or was re-keyed since the last read gets one more entry with its last non-zero peaks under its old id, even if the slot was reused.
- The reader remembers each slot's id and epoch from its last read. A voice it never saw (started and ended between reads) is not reported; when a slot changes hands twice between reads the earlier voice's last peaks are dropped. At 30 Hz that is a voice shorter than about 33 ms.
- A slot's `(id, epoch, active)` is published with a sequence lock: the audio thread writes without waiting; the reader retries 64 times, then leaves the slot for its next read.
- The reader reads identity before and after taking the peak words. If it changed in between, the words go back into the retired word (where the next epoch's first peak cannot overwrite them) and the slot waits for the next read. So a voice that ends or is re-keyed during a read still gets its last peaks once, under its own id.
- Every active voice has an entry on every read: a paused voice is present with 0.0 once its declick tail has played; a resumed voice reports under its new id from the resume on.
- `voice_meter_capacity()` (two entries per slot) sizes buffers for a complete read. Slots that do not fit are left for the next read, which starts at the first of them, so a short buffer serves every slot in turn. The `_into` reads allocate nothing.
- An epoch is a per-slot `u32`; after 2^32 reuses of one slot it wraps and that slot can drop one read's peaks.

## Xruns
- `xruns()` is a monotonic count reported by the device layer (host xruns and timestamp gaps; skill `devices`). `Mixer::xrun_notifier()` returns an `XrunNotifier` (clone, `Send + Sync`) for the host's error callback on another thread. Both are one atomic add, so RT-safe.
- Voice underruns are not xruns; they are `underrun_frames`.
- The offline driver injects xruns with `OfflineRenderer::inject_xruns(count)`.

## Offline render driver (QA-03)
- `OfflineRenderer::new(mixer)` calls `process_guarded` in a loop into memory: no device, no sleeps, faster than real time. It runs the exact same mixer code as the device callback.
- `render(frames, blocks)` / `render_into(buf, blocks)` cycle through a list of block sizes, so tests cover fixed and variable sizes (CoreAudio may vary block size). `render_into` does not allocate, so tests wrap it in `assert_no_alloc`.
- Tests that schedule beyond the lookahead call `MixerController::pump()` between blocks, as the engine's control loop does.
- Uses: sample-level assertions (a fade at 50% is at the expected gain on the expected sample; two cues start on the same sample), accelerated soak tests checking memory, drift and invariants, and the CPU gate.

## CPU budget (NFR-23, QA-06)
- NFR-23 target: at least 32 simultaneous stereo streamed cues with fades on a 2014 Mac mini or low-end Windows laptop. The gate demands 64 streaming stereo voices with active fades at **10x real time or better on one core of the Intel CI runner** (2x headroom over the target).
- `engine/crates/audio/tests/cpu_budget.rs` is a wall-clock gate marked `#[ignore]` (and a no-op in debug builds). CI runs `cargo test --release -p openlab-audio --test cpu_budget -- --ignored --nocapture`, which prints ratio and per-block p50/p99/max.
- **Run it alone:** the single test function times its cases in sequence, and other load changes the figures.
- **Throughput gate:** block 512, best of three runs, passes as soon as one reaches 10x. It runs first so report-only cases cannot pre-empt it. Blocks of 256 and 64 and the resampled variant are reported only. Block 64 is near 10x and deliberately not gated.
- **Dense-matrix gate:** three voices of 64 channels with every crosspoint on 64 outputs at block 512, then a `RampSet` per voice moving every gain (a short linear edit, a half-second S-curve Fade, nine S-curve Fades at once of different lengths). Gated on the worst block, best of three, against the block's own 10.67 ms: one overrun is a dropout on every output. A machine too slow for this fails here, not in a show.
- The workload (voice lifetimes, fade lengths, curve rotation, patches) lives in the test file; change it there. Only `OfflineRenderer::render_into` is timed; ring top-ups, commands, meter reads, event polls, decode and resampling are not.
- The assertions also require no underrun, no early end of a current voice, no fault and a non-silent finite mix, so a starved voice cannot flatter the ratio.
- The resampled variant runs the disk workers during the timing. On a machine with fewer free cores than busy threads it reads low because preemption counts as render time; that overstates worker load and never understates render cost. Do not "fix" it by loosening the gate.
- Perf work that changes the evaluator must keep output bit-identical (compare a checksum of every output sample and meter reading at blocks 512, 256 and 64) or justify the change against the tolerance bound in `signal-and-curves.md`.
