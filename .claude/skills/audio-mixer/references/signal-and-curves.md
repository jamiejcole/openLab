# Signal path, commands, de-zipper, ganging, fades and curves

## Pipeline picture
```
file -> disk thread: decode -> resample to device rate -> ring buffer (about 2 s)
audio thread: voice reads ring -> trims/loops/slices -> integrated fades -> channel levels
  -> routing matrix (file ch -> cue outputs -> patch outputs) -> Fade-cue ramps
  -> patch (level, mute, polarity) -> master -> device -> meters (atomics)
```
- Fades are per-voice gain ramps evaluated per sample from a curve id plus start/end gain and sample; no per-sample allocation and no table rebuilds on the audio thread.
- Loops, slices and devamp are resolved on the audio thread from a preloaded slice table (skill `audio-streaming`).
- The mixer sees voices and patch matrices, not documents. Several workspaces share one mixer; the engine maps `(DocId, PatchId)` to patch slots and `VoiceId` to owner (skill `engine-control`).
- A patch matrix's gains are patch master x output level; a workspace has no global master, so the mixer master stays at unity.
- There is no per-output delay (QLab 5 has none; speaker alignment belongs in a processor or the interface).

## Signal path
Per sample, voice routed through patch P, input channel i, cue output o, device output d:
```
voice_gain   = master(t) * duck(t) * stop(t) * fade_env(p)
bus_P[o]    += sum_i input[i] * crosspoint[i][o](t) * (cue_out[o](t) * voice_gain)
dev[d]       = sum_P sum_o bus_P[o] * patch_P[o][d](t)
out[d]       = dev[d] * global_master(t)
```
- `t` is sample time; `p` is the voice's frame position after the skip.
- Each patch's bus is shared by all voices routed through it, so a patch costs nothing per voice. Patches no voice uses in a block are skipped.
- A patch output's level, mute and polarity (AUD-07) are gains in the patch matrix; a device output has no stage of its own.
- Silent (constant 0) crosspoints, cue outputs and patch entries are skipped per block, so cost scales with routes in use.

## Commands
| Command | Semantics |
|---|---|
| `Ramp { voice, target, gain, fade }` | ramps one voice gain (`Master`, `Duck`, `CueOutput(o)`, `Crosspoint { input, output }`) to `gain` with `fade.curve` over `fade.frames`. `Duck` is a second whole-voice gain for "duck or boost others", 1.0 at every start; at exactly 1.0 and not ramping it is skipped, so a voice no cue ducks pays nothing for it |
| `SetPatchRouting { patch, cue_output, device_output, gain }` | one patch's cue-output to device-output gain; default is identity in every patch (AUD-07) |
| `SnapPatch { patch }` | the matrix takes the gains its routing is ramping to at once. Used for a matrix just given to a patch that nothing plays through, so a cue started on that sample is heard at its level on that patch's outputs only. Unknown patch ignored |
| `MoveVoice { voice, patch }` | moves a playing voice to another patch matrix |
| `SetMaster { gain }` | the mixer's own master. The control loop never sends it (no global master, PB-11) |

**MoveVoice**
- For the de-zipper time the voice plays into both patches, crossfading linearly old to new, so neither set of outputs clicks and the two always add up to the voice.
- A voice not heard yet (moved on its start sample, or paused past its declick tail) moves at once.
- A move requested during a crossfade waits for it and starts with the next segment rendered (at most a block later; latest wins).
- Ignored for an unknown voice, a patch out of range, or the patch the voice is on.
- Each move taken is counted (out-of-range patch included). `MoveEnded { voice, at, moves }` reports the voice settled on one patch once no crossfade is left: at once for a move with no crossfade, else from the end of the segment its last crossfade ends in, and not while another move waits.

**Gains**
- Gains are linear `f32`. Use `db_to_linear` and `linear_to_db`; -120 dB and below is exactly 0.0.
- Every gain the mixer applies is clamped to +-`MAX_GAIN` (+24 dB, `MAX_GAIN_DB`); NaN counts as 0.0. The show model stops at +12 dB, so the clamp is a safety net.
- A patch-matrix gain carries patch master x output level, so the clamp applies to their sum in dB: two settings of +12 dB each are heard at +24 dB.
- Commands naming an unknown voice, an out-of-range index, or an input beyond the voice's channel count are ignored.

## De-zipper
- Gain changes without an explicit ramp (`Ramp`/`RampSet` with `fade.frames == 0`, `SetPatchRouting`, `SetMaster`) move linearly over 5 ms (240 frames at 48 kHz), so no change can click. A polarity flip passes through zero.
- Exception: `Ramp`/`RampSet` gain changes scheduled on a voice's start sample take effect immediately, because nothing has been heard yet.
- The core cue engine's level maths (`core/cue_engine/level.rs`, `run.rs`) depends on these two rules: a new ramp starts from the current gain, and a 0-frame ramp is the de-zipper.

## Ganging (AUD-08)
- Gangs exist only on the control side. `MixerController::ramp_gang(at, voice, members, delta_db, fade)` moves every member by the same dB offset, keeping relative levels.
- A member at silence stays silent. Results are capped at +24 dB.
- The member ramps go out as one batch, so the gang moves on one sample.

## Gain domain and ramp definition (AUD-09, AUD-10)
Fades interpolate linear amplitude. The perceptual shape comes from the curve, so a fade can start or end at true silence (exactly 0.0) with no minus-infinity special case.

A ramp from `a` to `b` over `N` frames starting on sample `S`:
```
gain(S + k) = a + (b - a) * s(k / N)    for 0 <= k <= N, then b
s(t) = f(t)            if b >= a   (rising)
s(t) = 1 - f(1 - t)    if b < a    (falling)
```
- Sample `S` is exactly `a`; sample `S + N` is exactly `b`, so a fade-out to silence is exactly 0.0 there. Values are computed in `f64` and cast to `f32`.
- A falling ramp is the time-reverse of the matching rising ramp's gain path.
- A new ramp starts from the gain the old one would have had on its first sample, so interrupting a fade is continuous.
- Each sample is a function of curve id, endpoints and position only. No tables, no allocation.

## Curves
Each shape `f: [0,1] -> [0,1]` has `f(0) = 0` and `f(1) = 1` exactly and is continuous and monotonic. `t` is clamped to [0,1]; NaN counts as 0. Custom curves are out of scope for v0.0.1.

| Curve | `f(t)` | `f(0.5)` | Notes |
|---|---|---|---|
| Linear | `t` | 0.5 (-6.0 dB) | |
| EqualPower | `sin(pi t / 2)` | 0.7071 (-3.0 dB) | fade-in squared + fade-out squared = 1 throughout a crossfade |
| SCurve | `(1 - cos(pi t)) / 2` | 0.5 | symmetric; zero slope at both ends |
| Exponential | `e(t) = (exp(kt) - 1) / (exp(k) - 1)`, `k = ln 1000` | 0.03065 (-30.3 dB) | near dB-linear over 60 dB: slow start for fade-in, fast initial drop for fade-out |
| Logarithmic | `1 - e(1 - t)` | 0.96935 | point reflection of Exponential; with an Exponential fade-out gives an amplitude-complementary crossfade |

This table is the single home of the curve formulas. Two copies are tested against it: `core/cue_engine/level.rs` (bit for bit) and `mac/openLab/WaveformView.swift` (the waveform's fade overlay). A formula change updates both.

## Evaluation in `gain.rs` (keep in sync with `ramp_value`)
`ramp_value` and `Curve::shape` evaluate the formula exactly, one `sin`/`cos`/`exp_m1` per call. The mixer evaluates it faster:
- `CurveRamp` computes exact state at ramp positions that are multiples of 64 (`(cos, sin)` for EqualPower and SCurve, `exp` terms for Exponential and Logarithmic), then steps a recurrence frame to frame. Linear multiplies the position by `1/N`.
- Whole stretches between knots are evaluated at once with the curve chosen once per stretch; each frame gets the same operations as a single evaluation, so the bits match.
- A gain (`Param`) keeps only its ends, position and curve, and is `a + (b - a) * s` in `f32`. The curve progress `s` depends only on length, curve, direction and position, so a mixer shares it per stretch in `Shapes` (preallocated block buffers keyed by a hash). A whole-matrix edit or Fade computes one curve per segment however many gains follow it. Many distinct shapes at once are recomputed in turn, never wrongly, because a hit is a pure function of its key.
- `Param::current` gives the bits `fill` writes for its position, so a new ramp starts from what was heard.
- Knots depend only on ramp position, so a value is a pure function of ramp and position: output never depends on how the timeline is cut into blocks, nor on a skip, seek or pause.
- Position 0 is exactly `a`; position `N` and beyond exactly `b`.
- **Bound:** `|mixer gain - ramp_value| <= RAMP_TOLERANCE * max(|a|, |b|)` with `RAMP_TOLERANCE = 1e-6` (-120 dB, the silence floor). The recurrence runs at most 63 steps from an exact knot; the `f32` cast and the `f32` product and sum add the rest, well inside the bound.
- A new evaluator or a changed recurrence must keep the unit and property tests in `gain.rs` passing (every curve, both directions, gains from 0 to +24 dB and negative, lengths from 1 frame to hours at 192 kHz, any block split gives the same bits). Do not loosen the tolerance to pass.

## Integrated fades (AUD-10)
`fade_in` and `fade_out` are attached to the voice and follow its frame position `p` (frames consumed after the skip), not wall time. An underrun therefore does not shift them against the audio.
- Fade-in over `N` frames: `env_in(p) = ramp(0 to 1)` at step `p`. Frame 0 is exactly 0.0, frame `N` exactly 1.0.
- Fade-out over `N` frames with `play_frames = L`: `env_out(p) = ramp(1 to 0)` at step `p + N + 1 - L`, for steps >= 0. Frame `L - 1 - N` is exactly 1.0 and the last played frame, `L - 1`, is exactly 0.0. A fade-out longer than the playback starts part way along its curve. Without `L` (or slices) it is rejected `BadFade`.
- Overlapping fades multiply, and a `Ramp` on the voice master (a Fade cue) multiplies with them.
- With slices the fade-out follows the planned end of the table (see skill `audio-streaming`).
