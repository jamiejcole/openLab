// SPDX-License-Identifier: GPL-3.0-or-later

//! Audio levels as the mixer applies them: linear gains, fade curves and ramps. The formulas are
//! the mixer's (`openlab-audio::gain`, docs/audio.md "Fades and curves"), duplicated here because
//! core does not depend on the audio crate; a test compares the two sample by sample.

use std::collections::{BTreeMap, BTreeSet};

use crate::cue::{CueKind, CueLevels, FadeCurve, FadeLevels, MAX_CHANNELS, SILENCE_DB};
use crate::model::CueId;

use super::time::SampleTime;

/// One gain of an audio cue's voice, as the mixer's `GainTarget` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LevelParam {
    /// The cue's master level.
    Master,
    /// Cue output `n` (0-based).
    Output(u32),
    /// The crosspoint from file channel `input` to cue output `output`.
    Crosspoint { input: u32, output: u32 },
}

/// How long the mixer takes over a gain change that has no fade of its own (5 ms, at least one
/// frame): `Ramp` with 0 frames moves linearly over this many samples, except on the voice's own
/// start sample, where nothing has been heard yet (docs/audio.md, De-zipper).
pub(super) fn dezip_frames(sample_rate: u32) -> SampleTime {
    let frames = (f64::from(sample_rate) * 0.005).round();
    if frames.is_nan() || frames < 1.0 {
        1
    } else {
        frames as SampleTime
    }
}

/// The ramp a gain change is heard as: what the mixer makes of `Ramp { at, frames, curve }` on a
/// voice that started on `voice_start`. A change with a fade of its own is that fade; one with
/// none is the de-zipper (or immediate, on the voice's start sample).
pub(super) fn heard(
    at: SampleTime,
    frames: SampleTime,
    curve: FadeCurve,
    voice_start: SampleTime,
    sample_rate: u32,
) -> (SampleTime, FadeCurve) {
    if frames == 0 && at > voice_start {
        (dezip_frames(sample_rate), FadeCurve::Linear)
    } else {
        (frames, curve)
    }
}

/// How long an edit of a playing cue's levels (the operator riding a fader, a mute) takes to
/// be heard: a linear ramp of 10 ms, at least one frame. Twice the mixer's 5 ms de-zipper,
/// because an edit can be a full-scale step (a mute, a typed level) that a 5 ms ramp still
/// makes tick; still short enough to feel immediate (docs/cue-engine.md, "Level edits on a
/// playing cue").
pub(super) fn edit_frames(sample_rate: u32) -> SampleTime {
    let frames = (f64::from(sample_rate) * EDIT_RAMP_SECONDS).round();
    if frames.is_nan() || frames < 1.0 {
        1
    } else {
        frames as SampleTime
    }
}

/// See [`edit_frames`].
pub const EDIT_RAMP_SECONDS: f64 = 0.010;

/// One gain an edit of an audio cue's levels changes on its voice.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct LevelEdit {
    pub param: LevelParam,
    /// The gain (linear) the voice plays it at under the new levels.
    pub gain: f32,
    /// The edit muted it (a mute always takes effect, also on a level a fade is moving).
    pub muted: bool,
}

/// What a voice plays one parameter at under `levels`: its gain and whether it is muted. As the
/// binary starts a voice: unlisted outputs and crosspoints are silent, muted ones are 0.
type Played = (f32, bool);

fn played_output(levels: &CueLevels, o: usize) -> Played {
    match levels.outputs.get(o) {
        Some(out) if out.mute => (0.0, true),
        Some(out) => (db_to_gain(out.db), false),
        None => (0.0, false),
    }
}

/// The crosspoints a voice can have (both indices below [`MAX_CHANNELS`]); the first listed
/// wins, as for [`model_gain`] (the model has no duplicates once validated).
fn played_crosspoints(levels: &CueLevels) -> BTreeMap<(u32, u32), Played> {
    let mut out = BTreeMap::new();
    for c in &levels.crosspoints {
        if c.input < MAX_CHANNELS && c.output < MAX_CHANNELS {
            let played = if c.mute {
                (0.0, true)
            } else {
                (db_to_gain(c.db), false)
            };
            out.entry((c.input, c.output)).or_insert(played);
        }
    }
    out
}

/// Every parameter whose gain or mute differs between a voice given `old` and one given `new`,
/// in parameter order. Ganging alone changes nothing. Bounded by the levels' size (at most 1 +
/// 64 outputs + their crosspoints), whatever led to the change.
pub(super) fn level_edits(old: &CueLevels, new: &CueLevels) -> Vec<LevelEdit> {
    let mut out = Vec::new();
    if old == new {
        return out;
    }
    let mut push = |param: LevelParam, was: Played, now: Played| {
        if was != now {
            out.push(LevelEdit {
                param,
                gain: now.0,
                muted: now.1 && !was.1,
            });
        }
    };
    push(
        LevelParam::Master,
        (db_to_gain(old.master_db), false),
        (db_to_gain(new.master_db), false),
    );
    let outputs = old
        .outputs
        .len()
        .max(new.outputs.len())
        .min(MAX_CHANNELS as usize);
    for o in 0..outputs {
        if let Ok(param) = u32::try_from(o) {
            push(
                LevelParam::Output(param),
                played_output(old, o),
                played_output(new, o),
            );
        }
    }
    if old.crosspoints != new.crosspoints {
        let (was, now) = (played_crosspoints(old), played_crosspoints(new));
        let keys: BTreeSet<(u32, u32)> = was.keys().chain(now.keys()).copied().collect();
        let silent = (0.0, false);
        for (input, output) in keys {
            let key = (input, output);
            push(
                LevelParam::Crosspoint { input, output },
                was.get(&key).copied().unwrap_or(silent),
                now.get(&key).copied().unwrap_or(silent),
            );
        }
    }
    out
}

/// dB to linear amplitude, exactly as the mixer converts it: at or below [`SILENCE_DB`] (and
/// NaN) is exactly 0.0.
pub fn db_to_gain(db: f64) -> f32 {
    if db.is_nan() || db <= SILENCE_DB {
        0.0
    } else {
        10f64.powf(db / 20.0) as f32
    }
}

/// `ln 1000`: the exponential curve spans 60 dB.
const EXP_K: f64 = 6.907_755_278_982_137;

fn exp_shape(t: f64) -> f64 {
    (EXP_K * t).exp_m1() / EXP_K.exp_m1()
}

/// The rising shape `f(t)` of a curve, `t` clamped to `[0, 1]` (NaN counts as 0).
pub fn curve_shape(curve: FadeCurve, t: f64) -> f64 {
    let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
    let v = match curve {
        FadeCurve::Linear => t,
        FadeCurve::EqualPower => (t * std::f64::consts::FRAC_PI_2).sin(),
        FadeCurve::SCurve => (1.0 - (t * std::f64::consts::PI).cos()) * 0.5,
        FadeCurve::Logarithmic => 1.0 - exp_shape(1.0 - t),
        FadeCurve::Exponential => exp_shape(t),
    };
    v.clamp(0.0, 1.0)
}

/// Gain at step `pos` of a ramp from `from` to `to` over `len` steps: step 0 is exactly `from`,
/// step `len` and beyond exactly `to`; a falling ramp traces the rising one backwards.
pub fn ramp_gain(from: f32, to: f32, pos: u64, len: u64, curve: FadeCurve) -> f32 {
    if pos == 0 {
        return from;
    }
    if pos >= len {
        return to;
    }
    let t = pos as f64 / len as f64;
    let s = if to >= from {
        curve_shape(curve, t)
    } else {
        1.0 - curve_shape(curve, 1.0 - t)
    };
    let (a, b) = (f64::from(from), f64::from(to));
    (a + (b - a) * s) as f32
}

/// A gain a Fade cue has set or is moving, as [`CueEngine::fade_ramps`](super::CueEngine::fade_ramps)
/// reports it on a sample: what a voice restarted there needs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LevelRamp {
    pub param: LevelParam,
    /// The gain (linear) on that sample.
    pub value: f32,
    /// Where the ramp is going; equal to `value` once it has ended.
    pub to: f32,
    /// The sample the ramp ends on; at or before the sample asked about when it is not moving.
    pub end: SampleTime,
    pub curve: FadeCurve,
}

/// A gain a Fade cue has set or is moving: a ramp from `from` on sample `start` to `to` on
/// `start + frames`, then `to`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Level {
    /// The Fade cue and run (`seq`) moving it, while that fade may still move it.
    pub owner: Option<(CueId, u64)>,
    pub from: f32,
    pub to: f32,
    pub start: SampleTime,
    pub frames: SampleTime,
    pub curve: FadeCurve,
}

impl Level {
    pub fn held(gain: f32, at: SampleTime, owner: Option<(CueId, u64)>) -> Self {
        Self {
            owner,
            from: gain,
            to: gain,
            start: at,
            frames: 0,
            curve: FadeCurve::Linear,
        }
    }

    /// The gain on sample `at` (the start value for a sample before the ramp starts).
    pub fn value(&self, at: SampleTime) -> f32 {
        // A change with no ramp (on the voice's start sample, or a hold) is in force from its
        // own sample, where a ramp would still be on its start value.
        if self.frames == 0 && at >= self.start {
            return self.to;
        }
        ramp_gain(
            self.from,
            self.to,
            at.saturating_sub(self.start),
            self.frames,
            self.curve,
        )
    }

    pub fn end(&self) -> SampleTime {
        self.start.saturating_add(self.frames)
    }
}

/// The gain the model gives an audio cue's parameter (what its voice starts with). A muted
/// output or crosspoint has none: a fade never changes mutes, so it leaves it alone.
pub(super) fn model_gain(levels: &CueLevels, param: LevelParam) -> Option<f32> {
    match param {
        LevelParam::Master => Some(db_to_gain(levels.master_db)),
        LevelParam::Output(o) => {
            let out = levels.outputs.get(usize::try_from(o).ok()?)?;
            (!out.mute).then(|| db_to_gain(out.db))
        }
        LevelParam::Crosspoint { input, output } => {
            match levels
                .crosspoints
                .iter()
                .find(|c| c.input == input && c.output == output)
            {
                Some(c) if c.mute => None,
                Some(c) => Some(db_to_gain(c.db)),
                // Unlisted crosspoints are silent; a fade can bring them up.
                None => Some(0.0),
            }
        }
    }
}

/// What a Fade cue's levels ask of one audio cue: `(parameter, target gain)` for every set value
/// that names a parameter the cue has and that is not muted. Outputs (and crosspoint outputs)
/// beyond the cue's outputs, and crosspoint inputs of [`MAX_CHANNELS`] or more, are ignored;
/// crosspoint inputs beyond the file's channels are ignored by the mixer.
pub(super) fn fade_targets(fade: &FadeLevels, target: &CueKind) -> Vec<(LevelParam, f32)> {
    let CueKind::Audio { levels, .. } = target else {
        return Vec::new();
    };
    let outputs = levels.outputs.len();
    let mut out = Vec::new();
    let mut add = |param: LevelParam, db: f64| {
        if model_gain(levels, param).is_some() {
            out.push((param, db_to_gain(db)));
        }
    };
    if let Some(db) = fade.master_db {
        add(LevelParam::Master, db);
    }
    for (o, db) in fade.output_db.iter().enumerate().take(outputs) {
        if let (Some(db), Ok(o)) = (db, u32::try_from(o)) {
            add(LevelParam::Output(o), *db);
        }
    }
    for c in &fade.crosspoints {
        if usize::try_from(c.output).is_ok_and(|o| o < outputs) && c.input < MAX_CHANNELS {
            add(
                LevelParam::Crosspoint {
                    input: c.input,
                    output: c.output,
                },
                c.db,
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramps_hit_their_endpoints_exactly() {
        for curve in [
            FadeCurve::Linear,
            FadeCurve::EqualPower,
            FadeCurve::SCurve,
            FadeCurve::Logarithmic,
            FadeCurve::Exponential,
        ] {
            assert_eq!(ramp_gain(1.0, 0.0, 0, 100, curve), 1.0);
            assert_eq!(ramp_gain(1.0, 0.0, 100, 100, curve), 0.0);
            assert_eq!(ramp_gain(0.25, 0.5, 7, 0, curve), 0.5);
            assert_eq!(curve_shape(curve, f64::NAN), 0.0);
        }
        assert_eq!(db_to_gain(SILENCE_DB), 0.0);
        assert_eq!(db_to_gain(f64::NAN), 0.0);
        assert_eq!(db_to_gain(0.0), 1.0);
    }

    #[test]
    fn level_edits_list_each_changed_gain_once() {
        use crate::cue::{Crosspoint, CueOutput};
        let out = |db, mute| CueOutput { db, mute };
        let old = CueLevels {
            master_db: 0.0,
            outputs: vec![out(0.0, false), out(-6.0, false)],
            crosspoints: vec![Crosspoint {
                input: 0,
                output: 0,
                db: 0.0,
                mute: false,
            }],
            gangs: vec![],
        };
        assert!(level_edits(&old, &old).is_empty());
        // An output removed is silent; one added comes up; a muted one at -inf is still a mute.
        let mut new = old.clone();
        new.outputs = vec![out(SILENCE_DB, true)];
        let mut grown = old.clone();
        grown.outputs.push(out(-3.0, false));
        assert_eq!(
            level_edits(&old, &new),
            vec![
                LevelEdit {
                    param: LevelParam::Output(0),
                    gain: 0.0,
                    muted: true
                },
                LevelEdit {
                    param: LevelParam::Output(1),
                    gain: 0.0,
                    muted: false
                },
            ]
        );
        assert_eq!(
            level_edits(&old, &grown),
            vec![LevelEdit {
                param: LevelParam::Output(2),
                gain: db_to_gain(-3.0),
                muted: false
            }]
        );
        // Crosspoints out of any voice's range are never sent.
        let mut wide = old.clone();
        wide.crosspoints.push(Crosspoint {
            input: MAX_CHANNELS,
            output: 0,
            db: 0.0,
            mute: false,
        });
        assert!(level_edits(&old, &wide).is_empty());
        assert_eq!(edit_frames(1000), 10);
        assert_eq!(edit_frames(48_000), 480);
        assert_eq!(edit_frames(1), 1);
    }

    #[test]
    fn a_held_level_keeps_its_value() {
        let level = Level::held(0.5, 10, None);
        assert_eq!(level.value(0), 0.5);
        assert_eq!(level.value(1_000), 0.5);
        assert_eq!(level.end(), 10);
    }
}
