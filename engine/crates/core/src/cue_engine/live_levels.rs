// SPDX-License-Identifier: GPL-3.0-or-later

//! Level edits on a playing audio cue (AUD-06, AUD-08): an edit of the levels, mutes or
//! crosspoints of an audio cue whose voice has started reaches the voice as short `Ramp`s, one
//! per changed gain, the way Fade cues reach it.
//!
//! The diff is made here, from the levels the voice was last given (kept on its run) and the
//! model's, so the binary carries out `Ramp` actions without knowing why they came; which
//! levels a running fade owns is the engine's knowledge too.

use std::collections::BTreeSet;

use crate::cue::{CueKind, FadeCurve, LevelLimits};
use crate::model::CueId;
use crate::video_geometry::PictureParam;

use super::level::{self, Level, LevelEdit, LevelParam};
use super::run::{Phase, Slot, Voice};
use super::time::SampleTime;
use super::types::Action;
use super::{Ctx, CueEngine};

impl CueEngine {
    /// Sends every playing audio cue's voice the gains an edit changed since it was last given
    /// its levels; each follows the edit and is the model's again. An edit of a gain a running
    /// Fade cue is moving ends that Fade (QLab 5.5.3): it completes at once, holding the other
    /// levels it moves where they are (`complete_fades_now`); a paused Fade only loses the
    /// edited gain. Work per call is one comparison per started audio voice, plus the changed
    /// gains. `tightened` is the limits a narrowing of them just put in force: a level a Fade holds
    /// whose model value moved only because of them stays the Fade's (`follow_limits` has seen to
    /// it): the limit is not an operator taking the level over.
    pub(super) fn follow_level_edits(&mut self, tightened: Option<LevelLimits>, cx: &mut Ctx) {
        let mut ended: BTreeSet<(CueId, u64)> = BTreeSet::new();
        let ws = cx.ws;
        let frames = level::edit_frames(self.config.sample_rate().get());
        let tree = self.problems.tree();
        let duplicates = tree.has_duplicates();
        for (id, slot) in &mut self.slots {
            let Slot::Active(run) = slot else { continue };
            let Voice::Started {
                at: started, first, ..
            } = run.voice
            else {
                continue;
            };
            if run.is_broken() {
                continue;
            }
            if run.given.is_none() {
                continue;
            }
            let cue = if duplicates {
                ws.find_cue(*id)
            } else {
                tree.locate(ws, *id)
            };
            let Some(levels) = cue.and_then(|c| c.kind.levels()) else {
                continue;
            };
            let Some(given) = run.given.as_mut() else {
                continue;
            };
            if *levels == given.model {
                continue;
            }
            given.model = levels.clone();
            let edits = level::level_edits(&given.played, &level::played(levels));
            for LevelEdit { param, to } in edits {
                if let Some(limits) = tightened
                    && run.levels.contains_key(&param)
                    && run
                        .given
                        .as_ref()
                        .and_then(|g| g.played.get(&param))
                        .is_some_and(|was| {
                            was.1 == to.1 && level::limit_gain(limits, was.0) == to.0
                        })
                {
                    if let Some(given) = run.given.as_mut() {
                        given.played.insert(param, to);
                    }
                    continue;
                }
                // A fade set it or is moving it: the edit wins, and the model rules it again,
                // as an untouched level.
                if let Some(held) = run.levels.remove(&param) {
                    ended.extend(held.owner);
                }
                if let Some(given) = run.given.as_mut() {
                    given.played.insert(param, to);
                }
                let at = run.commit(cx.now.max(started));
                cx.out.push(Action::Ramp {
                    cue: *id,
                    run: run.run,
                    at,
                    param,
                    gain: to.0,
                    // On the voice's own start sample nothing has been heard yet.
                    frames: if at <= first { 0 } else { frames },
                    curve: FadeCurve::Linear,
                });
            }
        }
        self.follow_geometry_edits(&mut ended, cx);
        self.complete_fades_now(&ended, cx);
    }

    /// An edit of a Video cue's geometry value that a Fade has set or is moving on its playing
    /// picture takes that value over, as an edit of a level does (VID-05): the value leaves the
    /// Fade (whose run goes in `ended`) and goes to the model's over the edit ramp, and so does
    /// every later edit of it while the run lasts (the binary draws it by its ramps once a Fade
    /// has touched it). A value no Fade has touched is the binary's to follow (it draws the
    /// model's). Work: one comparison per run whose picture a Fade has moved.
    fn follow_geometry_edits(&mut self, ended: &mut BTreeSet<(CueId, u64)>, cx: &mut Ctx) {
        let ws = cx.ws;
        let frames = level::edit_frames(self.config.sample_rate().get());
        let tree = self.problems.tree();
        let duplicates = tree.has_duplicates();
        for (id, slot) in &mut self.slots {
            let Slot::Active(run) = slot else { continue };
            let Some(was) = run.geometry else { continue };
            let Voice::Started { at: started, .. } = run.voice else {
                continue;
            };
            let cue = if duplicates {
                ws.find_cue(*id)
            } else {
                tree.locate(ws, *id)
            };
            let Some(CueKind::Video { geometry: now, .. }) = cue.map(|c| &c.kind) else {
                continue;
            };
            if *now == was {
                continue;
            }
            run.geometry = Some(*now);
            for p in PictureParam::ALL {
                let param = LevelParam::Picture(p);
                if p.of(now) == p.of(&was) || !run.levels.contains_key(&param) {
                    continue;
                }
                let at = run.commit(cx.now.max(started));
                let to = p.of(now) as f32;
                // Kept, owned by no Fade: the binary draws this value by its ramps from now on,
                // so every later edit of it reaches the picture the same way.
                if let Some(held) = run.levels.get_mut(&param) {
                    ended.extend(held.owner);
                    *held = Level {
                        owner: None,
                        from: held.value(at),
                        to,
                        start: at,
                        frames,
                        curve: FadeCurve::Linear,
                    };
                }
                cx.out.push(Action::Ramp {
                    cue: *id,
                    run: run.run,
                    at,
                    param,
                    gain: to,
                    frames,
                    curve: FadeCurve::Linear,
                });
            }
        }
    }

    /// A narrowing of the volume limits reaches the levels Fade cues hold or are moving on playing
    /// voices, which no model edit touches:
    /// nothing plays above the maximum 10 ms after it is lowered, and a Fade no longer ends beyond
    /// the limits. A fade on its way down that is above the new maximum is at it within the 10 ms
    /// and goes on down from there, so it still ends where and when it was going to. Returns the
    /// new limits when they narrowed; one comparison when they did not change.
    pub(super) fn follow_limits(&mut self, cx: &mut Ctx) -> Option<LevelLimits> {
        let limits = cx.ws.settings.limits();
        let was = std::mem::replace(&mut self.limits, limits);
        if limits.max_db >= was.max_db && limits.min_db <= was.min_db {
            return None;
        }
        let edit = level::edit_frames(self.config.sample_rate().get());
        let max = level::db_to_gain(limits.max_db);
        for (id, slot) in &mut self.slots {
            let Slot::Active(run) = slot else { continue };
            // A paused Fade's resume, and a re-timed one's end, go no further than the limits.
            if let Some(fade) = run.fade.as_mut() {
                for ramp in &mut fade.ramps {
                    // A picture's value is not a level: no limit applies.
                    if ramp.param.picture().is_none() {
                        ramp.gain = level::limit_gain(limits, ramp.gain);
                    }
                }
            }
            let Voice::Started {
                at: started, first, ..
            } = run.voice
            else {
                continue;
            };
            let voice = run.run;
            let at = run.committed.max(cx.now).max(started);
            let frames = if at <= first { 0 } else { edit };
            let mut last = at;
            for (param, held) in &mut run.levels {
                if param.picture().is_some() {
                    continue;
                }
                let now = held.value(at);
                let to = level::limit_gain(limits, held.to);
                let left = held.end().saturating_sub(at);
                let curve_left = held.curve;
                let mut ramp = |at: SampleTime, gain: f32, frames: SampleTime, curve: FadeCurve| {
                    cx.out.push(Action::Ramp {
                        cue: *id,
                        run: voice,
                        at,
                        param: *param,
                        gain,
                        frames,
                        curve,
                    });
                };
                if now > max {
                    // Loud now: down to the maximum at once.
                    let first_stop = if left > frames { max.max(to) } else { to };
                    ramp(at, first_stop, frames, FadeCurve::Linear);
                    *held = Level {
                        owner: held.owner,
                        from: now,
                        to: first_stop,
                        start: at,
                        frames,
                        curve: FadeCurve::Linear,
                    };
                    if first_stop != to {
                        // Then the rest of the way, on the fade's own time. It may be dated past
                        // the call's horizon; `committed` keeps later commands after it.
                        let on = at.saturating_add(frames);
                        let rest = left.saturating_sub(frames);
                        ramp(on, to, rest, curve_left);
                        *held = Level {
                            owner: held.owner,
                            from: first_stop,
                            to,
                            start: on,
                            frames: rest,
                            curve: curve_left,
                        };
                        last = last.max(on);
                    }
                } else if to != held.to {
                    // Still moving: it keeps its time and curve and arrives at the limit. Held:
                    // a short ramp.
                    let (span, curve) = if left > 0 {
                        (left, held.curve)
                    } else {
                        (frames, FadeCurve::Linear)
                    };
                    ramp(at, to, span, curve);
                    *held = Level {
                        owner: held.owner,
                        from: now,
                        to,
                        start: at,
                        frames: span,
                        curve,
                    };
                }
            }
            run.committed = last;
        }
        Some(limits)
    }

    /// Each Fade cue in `fades` (cue, run) that is running and not paused completes now, as on
    /// its end (its auto-follow fires), except that the levels it is still moving are held where
    /// they are, as for a Fade whose duration was shortened to now, and its "stop target when
    /// done" does not apply: an operator nudging a fader during a fade-out must not cut the sound.
    fn complete_fades_now(&mut self, fades: &BTreeSet<(CueId, u64)>, cx: &mut Ctx) {
        let mut any = false;
        for (cue, seq) in fades {
            if let Some(Slot::Active(run)) = self.slots.get_mut(cue)
                && run.seq == *seq
                && run.paused_at.is_none()
                && run.stop_end.is_none()
                && let (
                    Some(fade),
                    Phase::Action {
                        start,
                        end: Some(_),
                    },
                ) = (run.fade.as_mut(), run.phase)
            {
                fade.retimed = true;
                // The edit is the operator taking the level over: the target plays on.
                fade.stop_target = None;
                run.phase = Phase::Action {
                    start,
                    end: Some(start.max(cx.now)),
                };
                any = true;
            }
        }
        if any {
            self.advance(cx);
        }
    }
}
