# Fade cues and live level edits

Audio levels only (video fades are later). Sources: `fade.rs`, `level.rs`, `live_levels.rs`.

## Fade cues (CT-10)
- A Fade of an audio cue is absolute unless its `mode` is `relative` (then it moves the cue's levels as a Fade of a group does, below): over its duration, with its curve, it moves the target's levels from their current values to the Fade's values.
- A Fade of a group is relative: each level of each audio cue in it moves by the Fade's value (current + Fade's dB, i.e. gain times the Fade's gain), never above `MAX_DB` (+12 dB, `cue.rs`). Relative to the level on the ramp's first sample `S`, including where an earlier Fade has put or is moving it; stacking of concurrent relative fades is not modelled. A level silent then (an unlisted crosspoint) stays silent and gets no ramp.
- What it fades: at its action start `A`, every playing audio cue it targets (the target, or every audio cue inside a group, any depth). Playing = in its action with a started voice (running, fading, paused, stopping), not broken. Cues that start later are not faded.
- Which levels: `master_db`, each set `output_db[o]`, each listed crosspoint, per target. Indices beyond the target's outputs (and crosspoint inputs of 64 or more) are ignored. A muted output or crosspoint is not faded (a fade never changes mutes). An unlisted crosspoint of an absolute fade is silent and fades up from silence.
- Ramps: per target and level `Ramp { cue, run, at: S, param, gain, frames: E - S, curve }`. `S = max(A, now, voice start, the voice's last committed command)`, `E = A + duration` (minus a load-to-time offset). A ramp never names a voice before the mixer has it; a late fade starts late but still ends on `E`. `gain` is linear (`db_to_gain`). Duration 0 sets the levels on `S`.
- Ownership: each level is moved by the Fade that last ramped it. A later Fade takes it over; the earlier one no longer touches it (pausing or stopping it sends nothing for that level).
- Completion: the action completes on `E` (auto-follow fires then). With stop-target-when-done, the target (a group: with everything in it) is hard-stopped on `E`, on the sample the ramp ends, before the auto-follow, whatever it is doing then. A duration edit moves `E` (`broken-and-edits.md`); a level edit completes it at once without stopping the target (below).
- Pause of the Fade holds its ramps on the pause sample `P` (`Ramp { at: max(P, ramp start), gain: current, frames: 0 }`); resume at `R` ramps each held level from there to the Fade's value, ending on the shifted end `E + R - P`, with the same curve over the remaining frames (a fresh curve from the held value). Stop, Reset or deleting it holds the ramps where they are and never stops the target. A held level whose ramp had already ended sends nothing.
- State: the Fade cue is `running`. Each audio cue it moves is `fading` while in its action and a running, unpaused Fade with a non-zero ramp on it exists; back to `running` when that Fade completes, pauses or stops. Paused, stopping, broken take precedence. A group target reports its own state; its faded children report `fading`.
- Not playing: if nothing targeted is playing at `A`, `TargetNotRunning` and nothing happens, but it still runs its duration (timing and continue apply; stop-target-when-done then stops the target on `E` if it is running by then). Other kind: `TargetWrongKind`; none or missing: `NoTarget`/`TargetMissing`.
- A Fade that sets no level has static problem `nothing_to_fade`: broken like any cue with problems (GO/Start skip it, stop-target-when-done does not fire; timing and continue run). The target need not be set for it to show (`no_target` is listed first). Setting any one level fixes it.
- Feedback never breaks a Fade cue (no voice). Control and Fade cues read target and settings when their action starts.

### Engine-side level tracking
- The mixer starts every ramp from the gain's value on its first sample, so interrupted fades continue without a jump. The engine tracks the same: each running audio cue keeps the gains Fade cues set or are moving as `from -> to over frames from start`, evaluated with the mixer's formula (`ramp_gain`, `curve_shape`, `db_to_gain`).
- `tests/level_maths.rs` pins that they match `openlab_audio::ramp_value`, `Curve::shape` and the dB conversion bit for bit. Change either side and that test must move with it.
- The mixer itself matches `ramp_value` within `openlab_audio::RAMP_TOLERANCE` (1e-6 relative), which is inaudible; a held or resumed fade continues from the engine's level to within it.
- Untouched levels are the model's. `fade_level(cue, param, at)` (`None` = untouched) and `fade_ramps(cue, at)` read them.
- A ramp of 0 frames is tracked as the mixer hears it: a linear ramp over the de-zipper time (5 ms, at least one sample), except on the voice's own first start sample where it is immediate (a resume keeps the same mixer voice, so a 0-frame ramp on a resume sample is de-zippered). The `Ramp` action still carries `frames: 0`. A hold is a ramp from the current value to itself.
- These levels belong to the run: a stopped and restarted cue starts from the model's levels (Reset restores its saved levels).

## Level edits on a playing cue (AUD-06, AUD-08)
An edit of an Audio cue's levels (master, output levels and mutes, crosspoints and mutes; `SetCueProperty KindFields`, an undo or redo of one) is heard on its playing voice at once. No restart, no new voice, no state change.
- Which cues: audio cue with a started voice (running, fading, paused, stopping), not broken. Idle, loaded and pre-waiting cues have no voice: the model has it and the voice starts with it. Previews are not followed (keep the levels they started with).
- What is sent: each run keeps what its voice has been given per parameter (`Run::given`). When the engine follows edits, after the deadlines due in that call (so a fade ending in the call has already let go of its levels), each such run compares model levels with that (`level::level_edits`) and emits one linear `Ramp` per parameter whose gain or mute differs.
- Unlisted/removed crosspoint and an output beyond the list are silent; a mute is gain 0 (level kept so unmuting restores it); ganging alone changes nothing. `at = max(now, voice start, last committed command)`.
- Several edits between two calls are one ramp to the last. Work per call is one comparison per started audio voice plus the changed gains (at most 1 + 64 outputs + their crosspoints), whatever the edit rate. A fader drag is one short ramp per edit, each retargeted by the next from wherever the gain is.
- Ramp length: `EDIT_RAMP_SECONDS` = 10 ms (`level.rs`, at least one frame), linear; 0 on the voice's own start sample. Twice the 5 ms de-zipper because an edit can be a full-scale step, yet short enough to feel immediate. The mixer starts every ramp from the current gain, so an edit during an edit ramp, fade or resume never jumps.
- Paused cues: the ramp goes to the paused voice; the mixer advances no gain while paused, so the edit is heard over its ramp from the resume (under the resume declick).
- Patch change is not a level edit: core sends nothing; the binary moves the voice to the new outputs (skill `engine-control`).

### The edit wins over a Fade
- An edit (or mute) of a level that a running, unpaused Fade is moving completes that Fade at once on the call's `now`: its auto-follow fires but its stop-target-when-done does not apply. The operator took the level over; nudging a fader during a fade-out-and-stop must never cut the sound.
- A shortened duration (`broken-and-edits.md`) is not a take-over and still stops the target on the new end.
- The edited level is taken from any Fade that set or moves it (`Level::owner`); the model's level rules again. The Fade's other levels, on this cue or others it fades, are held where they are (`Ramp { gain: current, frames: 0 }`, no jump to the Fade's values) and it lets go of them.
- An edit of a level the Fade does not move changes nothing for it. A paused Fade starts nothing: the edited level leaves it, it stays paused, and its resume moves only the levels it still has. A level a finished or stopped fade set follows the next edit the same way.
- Since an edited level is the model's, a later Fade starts from it (absolute: from the heard value; relative: moves the edited level by its dB).

### Volume limits (PB-11)
- The cue's own levels above a lowered maximum, or at or below a raised minimum, reach the voice as an edit (above).
- Levels a Fade holds or is moving (`Run::levels`) are not touched by any edit, so when the engine finds limits narrowed (`follow_limits`; one comparison otherwise) each started voice's such levels come within them: a held level via the 10 ms ramp; one moving towards a level beyond the limit keeps its time and curve and arrives at the limit
- One above the new maximum goes to the maximum over 10 ms and, if on its way lower, carries on from there to arrive where and when it was going.
- A paused Fade's resume and a re-timed Fade's end are limited too.
- None of this is the operator taking a level over: the Fade keeps its level, stop-target-when-done and auto-follow, and the sweep does not jump the voice to the cue's new level. Raising a limit (undo included) puts back the cue's own levels as an edit and leaves what a Fade holds.
