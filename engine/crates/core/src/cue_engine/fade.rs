// SPDX-License-Identifier: GPL-3.0-or-later

//! Fade cues (CT-10, audio levels): an absolute fade of the target's levels over the cue's
//! duration, as `Ramp` actions the mixer carries out. Rules: `docs/cue-engine.md`, "Fade cues".

use std::collections::BTreeSet;

use crate::cue::{Cue, CueKind};
use crate::model::CueId;

use super::group;
use super::level::{self, Level, LevelParam};
use super::run::{FadeRamp, FadeRun, Phase, Slot, Voice};
use super::time::SampleTime;
use super::types::{Action, EngineWarning};
use super::{Ctx, CueEngine};

impl CueEngine {
    /// The action of Fade cue `cue` started on `at`: every playing audio cue it targets ramps
    /// from its current levels to the fade's, ending when the fade's action ends.
    pub(super) fn fade_begin(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        let ws = cx.ws;
        let Some(CueKind::Fade {
            target,
            curve,
            levels,
            stop_target_when_done,
            ..
        }) = ws.find_cue(cue).map(|c| &c.kind)
        else {
            return;
        };
        let (seq, end) = match self.slots.get(&cue) {
            Some(Slot::Active(run)) => match run.phase {
                Phase::Action { end: Some(end), .. } => (run.seq, end),
                _ => return,
            },
            _ => return,
        };
        let Some(target) = *target else {
            return cx.warn(EngineWarning::NoTarget { cue });
        };
        let Some(target_model) = ws.find_cue(target) else {
            return cx.warn(EngineWarning::TargetMissing { cue, target });
        };
        let candidates: Vec<&Cue> = match &target_model.kind {
            CueKind::Audio { .. } => vec![target_model],
            // A group fades the audio cues inside it (any depth) that are playing now.
            CueKind::Group { .. } => group::descendants(target_model)
                .into_iter()
                .filter_map(|id| ws.find_cue(id))
                .filter(|c| matches!(c.kind, CueKind::Audio { .. }))
                .collect(),
            _ => return cx.warn(EngineWarning::TargetWrongKind { cue, target }),
        };
        let mut fade = FadeRun {
            ramps: Vec::new(),
            targets: Vec::new(),
            stop_target: stop_target_when_done.then_some(target),
            curve: *curve,
        };
        let now = cx.now;
        let mut seen = BTreeSet::new();
        for c in candidates {
            if !seen.insert(c.id) {
                continue;
            }
            let CueKind::Audio { levels: base, .. } = &c.kind else {
                continue;
            };
            let wanted = level::fade_targets(levels, &c.kind);
            let before = self.state(c.id);
            let Some(Slot::Active(run)) = self.slots.get_mut(&c.id) else {
                continue;
            };
            let Voice::Started { at: started, .. } = run.voice else {
                continue;
            };
            if run.broken.is_some() || run.group.is_some() {
                continue;
            }
            fade.targets.push((c.id, run.seq));
            // Never before the voice exists in the mixer, and the end stays on the fade's end.
            let start = at.max(now).max(started);
            let frames = end.saturating_sub(start);
            for &(param, gain) in &wanted {
                let from = run
                    .levels
                    .get(&param)
                    .map(|l| l.value(start))
                    .or_else(|| level::model_gain(base, param))
                    .unwrap_or(0.0);
                cx.out.push(Action::Ramp {
                    cue: c.id,
                    run: run.run,
                    at: start,
                    param,
                    gain,
                    frames,
                    curve: *curve,
                });
                run.levels.insert(
                    param,
                    Level {
                        owner: Some((cue, seq)),
                        from,
                        to: gain,
                        start,
                        frames,
                        curve: *curve,
                    },
                );
                fade.ramps.push(FadeRamp {
                    cue: c.id,
                    seq: run.seq,
                    param,
                    gain,
                });
            }
            if frames > 0 && !wanted.is_empty() {
                run.faders.insert(cue);
            }
            self.report(c.id, before, at, cx);
        }
        if fade.targets.is_empty() {
            cx.warn(EngineWarning::TargetNotRunning { cue, target });
        }
        if let Some(Slot::Active(run)) = self.slots.get_mut(&cue) {
            run.fade = Some(fade);
        }
    }

    /// The fade's action completed on `at`: its targets have reached its levels. With "stop
    /// target when done" the target stops (hard) on that sample.
    pub(super) fn fade_finish(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return;
        };
        let Some(fade) = run.fade.take() else {
            return;
        };
        let seq = run.seq;
        if let Some(target) = fade.stop_target {
            self.as_of(at, cx, |e, cx| e.stop_tree(target, None, cx));
        }
        for r in &fade.ramps {
            if let Some(level) = self.level_mut(r.cue, r.seq, r.param)
                && level.owner == Some((cue, seq))
            {
                level.owner = None;
            }
        }
        self.unfade(cue, &fade, at, cx);
    }

    /// Stops the fade's ramps where they are on `at` (a pause or a stop of the Fade cue). A
    /// pause keeps them as this fade's, so a resume can carry on.
    pub(super) fn hold_fade(
        &mut self,
        cue: CueId,
        seq: u64,
        fade: &FadeRun,
        at: SampleTime,
        keep: bool,
        cx: &mut Ctx,
    ) {
        for r in &fade.ramps {
            let Some(Slot::Active(run)) = self.slots.get_mut(&r.cue) else {
                continue;
            };
            if run.seq != r.seq {
                continue;
            }
            let voice = run.run;
            let Some(level) = run.levels.get_mut(&r.param) else {
                continue;
            };
            if level.owner != Some((cue, seq)) {
                continue;
            }
            // Not before the ramp's own start: a ramp committed for a later sample would
            // otherwise overtake the hold in the mixer.
            let when = at.max(level.start);
            if when >= level.end() {
                level.owner = None;
                continue;
            }
            let gain = level.value(when);
            *level = Level::held(gain, when, keep.then_some((cue, seq)));
            cx.out.push(Action::Ramp {
                cue: r.cue,
                run: voice,
                at: when,
                param: r.param,
                gain,
                frames: 0,
                curve: level.curve,
            });
        }
        self.unfade(cue, fade, at, cx);
    }

    /// A paused Fade cue resumes on `at`: its held ramps carry on to its levels, ending on its
    /// (shifted) end sample.
    pub(super) fn resume_fade(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        let now = cx.now;
        let Some(Slot::Active(run)) = self.slots.get(&cue) else {
            return;
        };
        let (Some(fade), Phase::Action { end: Some(end), .. }) = (run.fade.clone(), run.phase)
        else {
            return;
        };
        let seq = run.seq;
        let mut moving = BTreeSet::new();
        for r in &fade.ramps {
            let Some(Slot::Active(target)) = self.slots.get_mut(&r.cue) else {
                continue;
            };
            if target.seq != r.seq {
                continue;
            }
            let voice = target.run;
            let Some(level) = target.levels.get_mut(&r.param) else {
                continue;
            };
            if level.owner != Some((cue, seq)) {
                continue;
            }
            let start = at.max(now).max(level.start);
            let frames = end.saturating_sub(start);
            *level = Level {
                owner: Some((cue, seq)),
                from: level.value(start),
                to: r.gain,
                start,
                frames,
                curve: fade.curve,
            };
            cx.out.push(Action::Ramp {
                cue: r.cue,
                run: voice,
                at: start,
                param: r.param,
                gain: r.gain,
                frames,
                curve: fade.curve,
            });
            if frames > 0 {
                moving.insert(r.cue);
            }
        }
        for id in moving {
            let before = self.state(id);
            if let Some(Slot::Active(target)) = self.slots.get_mut(&id) {
                target.faders.insert(cue);
            }
            self.report(id, before, at, cx);
        }
    }

    /// The fade no longer moves its targets' levels: they stop reporting `fading` for it.
    fn unfade(&mut self, cue: CueId, fade: &FadeRun, at: SampleTime, cx: &mut Ctx) {
        for &(id, seq) in &fade.targets {
            let before = self.state(id);
            if let Some(Slot::Active(run)) = self.slots.get_mut(&id)
                && run.seq == seq
            {
                run.faders.remove(&cue);
            }
            self.report(id, before, at, cx);
        }
    }

    fn level_mut(&mut self, cue: CueId, seq: u64, param: LevelParam) -> Option<&mut Level> {
        match self.slots.get_mut(&cue) {
            Some(Slot::Active(run)) if run.seq == seq => run.levels.get_mut(&param),
            _ => None,
        }
    }

    /// The gain (linear) a Fade cue has given parameter `param` of a playing audio cue, on
    /// sample `at`. `None` when no fade has touched it (the model's level applies) or the cue
    /// has no runtime. For a sample before a ramp's start it gives the ramp's start value.
    pub fn fade_level(&self, cue: CueId, param: LevelParam, at: SampleTime) -> Option<f32> {
        match self.slots.get(&cue)? {
            Slot::Active(run) => run.levels.get(&param).map(|l| l.value(at)),
            Slot::Loaded(_) | Slot::Broken(_) => None,
        }
    }
}
