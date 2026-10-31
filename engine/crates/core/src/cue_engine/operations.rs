// SPDX-License-Identifier: GPL-3.0-or-later

//! The per-cue operations: start, stop, pause, resume, load and reset, and the chain budget a
//! start spends (docs/cue-engine.md, "Operations", "Chains and the chain bound").

use crate::cue::{CueKind, FadeSpec};
use crate::model::CueId;

use super::run::{self, Exit, GroupRun, Loaded, Phase, Run, Slot, Voice};
use super::time::SampleTime;
use super::types::{Action, EngineWarning, StopFade};
use super::{Ctx, CueEngine, release};

impl CueEngine {
    // ----- operations (CUE-05) -----

    /// Whether the call may start another cue. When the budget is spent, the limit is recounted
    /// once per call (calls skip the count when nothing was signalled, see `model_changed`): if
    /// cues were added without the signal the limit was too low, and the budget is raised to the
    /// real one instead of dropping a legitimate start. A genuine cycle (the recount agrees) costs
    /// one recount per call.
    pub(super) fn budget_left(&mut self, cx: &mut Ctx) -> bool {
        if cx.budget > 0 {
            return true;
        }
        if !cx.limit_verified {
            cx.limit_verified = true;
            let fresh = Self::chain_limit(cx.ws);
            if fresh > cx.limit {
                cx.budget = fresh - cx.limit;
                cx.limit = fresh;
                self.limit = fresh;
                self.note_unsignalled();
                return true;
            }
        }
        false
    }

    /// Starts `cue` at sample `at`. On a cue that is already active its second-trigger option
    /// applies (`second.rs`); with `nothing` the trigger is ignored, or a paused cue resumes
    /// (docs/cue-engine.md, "Second trigger").
    pub(super) fn start(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        let Some(model) = cx.ws.find_cue(cue) else {
            return cx.warn(EngineWarning::UnknownCue { cue });
        };
        // A cue fading out in a panic is not active for the second-trigger rule: the trigger
        // ends that fade at once and starts the cue afresh.
        let panicked = matches!(self.slots.get(&cue), Some(Slot::Active(r)) if r.panicked);
        // Triggers that start nothing cost no budget: the budget is shared by every call at one
        // `now`, so ignored triggers (a stuck controller key) must not use it up for a GO. A
        // cycle still ends, since each of its laps creates or resumes a runtime, which is charged
        // below (a second trigger that restarts or moves a playlist on is charged for its
        // start), and a trigger back to an active cue starts nothing else.
        if !panicked {
            match self.slots.get(&cue) {
                Some(Slot::Broken(_)) => return cx.warn(EngineWarning::CueBroken { cue }),
                // A broken run keeps the rule as it was: a paused one resumes.
                Some(Slot::Active(run)) if run.is_broken() => {
                    if run.paused_at.is_none() {
                        return cx.warn(EngineWarning::CueBroken { cue });
                    }
                }
                // A paused cue resumes whatever its option (QLab check S1, M2.28): the option
                // applies only to a cue that is playing (or waiting).
                Some(Slot::Active(run)) if run.paused_at.is_none() => {
                    let option = model.second_trigger.effective(&model.kind);
                    return self.second_trigger(cue, option, at, cx);
                }
                Some(Slot::Active(_)) => {}
                Some(Slot::Loaded(_)) | None => {}
            }
        }
        if !self.budget_left(cx) {
            return cx.warn(EngineWarning::ChainLimit {
                cue,
                limit: cx.limit,
            });
        }
        cx.budget -= 1;
        if panicked {
            self.as_of(at, cx, |e, cx| e.stop_tree(cue, None, cx));
        }
        let before = self.state(cue);
        let (run, split, voice) = match self.slots.get(&cue) {
            Some(Slot::Broken(_)) => return cx.warn(EngineWarning::CueBroken { cue }),
            // Also a broken one: its frozen timers (a pending auto-continue) run again.
            Some(Slot::Active(run)) if run.paused_at.is_some() => {
                return self.resume_tree(cue, at, cx);
            }
            Some(Slot::Active(run)) if run.is_broken() => {
                return cx.warn(EngineWarning::CueBroken { cue });
            }
            Some(Slot::Active(run)) => {
                return cx.warn(EngineWarning::TriggerIgnored {
                    cue,
                    state: run.state(),
                });
            }
            Some(Slot::Loaded(loaded)) => (
                loaded.run,
                (
                    loaded.pre_wait_left,
                    loaded.action_offset,
                    loaded.pre_wait_total,
                ),
                if loaded.voice {
                    Voice::Loaded
                } else {
                    Voice::None
                },
            ),
            None => {
                let pre_wait = cx.secs(model.pre_wait, &self.config);
                (self.new_run(), (pre_wait, 0, pre_wait), Voice::None)
            }
        };
        let (pre_wait_left, action_offset, pre_wait_total) = split;
        let until = at.saturating_add(pre_wait_left);
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        let group = matches!(model.kind, CueKind::Group { .. }).then(GroupRun::default);
        self.continued.remove(&cue);
        let mut new = Run::new(run, seq, Phase::PreWait { until }, group);
        new.action_offset = action_offset;
        new.pre_wait_total = pre_wait_total;
        new.voice = voice;
        new.started = at;
        new.started_in = super::group::ancestors(cx.ws, cue).first().copied();
        // A statically broken cue does not start, but keeps its timing as a disarmed cue does,
        // so its auto-continue fires on schedule (user decision, M2.13).
        if self.refuses(cue, cx) {
            cx.warn(EngineWarning::CueBroken { cue });
            new.static_broken = true;
        }
        self.slots.insert(cue, Slot::Active(new));
        self.report(cue, before, at, cx);
        self.release_panicking_parents(cue, at, cx);
    }

    /// Stops one cue (a group: itself only; `stop_tree` stops its descendants first).
    pub(super) fn stop_one(&mut self, cue: CueId, fade_out: Option<FadeSpec>, cx: &mut Ctx) {
        let before = self.state(cue);
        let now = cx.now;
        let frames = fade_out.map_or(0, |f| cx.secs(f.duration, &self.config));
        let busy_children = self.any_busy_child(cx.ws, cue);
        match self.slots.get_mut(&cue) {
            None | Some(Slot::Broken(_)) => return,
            Some(Slot::Active(run)) if run.broken.is_some() => {
                // It owns no voice; stopping cancels its pending auto-continue.
                if let Some(reason) = run.broken {
                    self.slots.insert(cue, Slot::Broken(reason));
                    cx.vacated.push((cue, now, Exit::Stopped));
                }
                return self.report(cue, before, now, cx);
            }
            // Started broken: it owns no voice; stopping ends it (and its pending continue).
            Some(Slot::Active(run)) if run.static_broken => {}
            // A group whose children are fading out ends when the last of them has stopped.
            // Only during its action: a group that has not reached it (pre-wait) stops at once,
            // so its pending action can never start children after the stop.
            Some(Slot::Active(run @ Run { group: Some(_), .. }))
                if frames > 0
                    && busy_children
                    && run.paused_at.is_none()
                    && matches!(run.phase, Phase::Action { .. }) =>
            {
                if let Some(g) = run.group.as_mut()
                    && !g.stopping
                {
                    g.stopping = true;
                    run.post_wait = None;
                    run.follow = false;
                    return self.report(cue, before, now, cx);
                }
            }
            Some(Slot::Active(run))
                if frames > 0
                    && run.stop_end.is_none()
                    && run.paused_at.is_none()
                    && matches!(run.phase, Phase::Action { .. }) =>
            {
                if let (Voice::Started { at: started, .. }, Some(curve)) =
                    (run.voice, fade_out.map(|f| f.curve))
                {
                    let at = run.commit(now.max(started), cx.horizon);
                    cx.out.push(Action::StopVoice {
                        cue,
                        run: run.run,
                        at,
                        fade: Some(StopFade { frames, curve }),
                    });
                    run.stop_end = Some(at.saturating_add(frames));
                    run.post_wait = None;
                    run.follow = false;
                    // A stopped target cancels a pending devamp and its next cue.
                    run.devamp = None;
                    return self.report(cue, before, now, cx);
                }
            }
            Some(_) => {}
        }
        if let Some(slot) = self.remove_slot(cue, now, Exit::Stopped, cx) {
            release(cue, &slot, cx);
        }
        self.report(cue, before, now, cx);
    }

    pub(super) fn pause_one(&mut self, cue: CueId, cx: &mut Ctx) {
        let before = self.state(cue);
        let now = cx.now;
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return;
        };
        if run.paused_at.is_some() || run.stop_end.is_some() || run.is_group_stopping() {
            return;
        }
        // Not before a command already committed for the voice in the lookahead window (a ramp,
        // a devamp): the pause comes after it, and the timers freeze there.
        let now = now.max(run.floor(cx.horizon));
        run.paused_at = Some(now);
        if let (Voice::Started { at: started, .. }, Phase::Action { .. }) = (run.voice, run.phase) {
            let at = run.commit(now.max(started), cx.horizon);
            cx.out.push(Action::PauseVoice {
                cue,
                run: run.run,
                at,
                offset: run.position(at),
            });
        }
        // A paused Fade cue holds its ramps where they are.
        if let Some(fade) = run.fade.clone() {
            let seq = run.seq;
            self.hold_fade(cue, seq, &fade, now, true, cx);
        }
        self.report(cue, before, now, cx);
    }

    /// Resumes a paused cue as of sample `at`: every timer keeps the time it had left.
    pub(super) fn resume_one(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        let before = self.state(cue);
        let now = cx.now;
        // Only a paused cue takes a fresh id (a resume of anything else changes nothing).
        if !matches!(self.slots.get(&cue), Some(Slot::Active(run)) if run.paused_at.is_some()) {
            return;
        }
        let fresh = self.new_run();
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return;
        };
        let Some(paused_at) = run.paused_at.take() else {
            return;
        };
        // Where the action was frozen. A resume dated before the pause (a trigger from a late
        // deadline, or an input before a pause committed in the lookahead window) resumes as of
        // the pause: nothing for the voice goes backwards in time.
        let at = at.max(run::after_restart(paused_at, cx.horizon));
        let offset = run.position(paused_at);
        run.shift(at.saturating_sub(paused_at));
        let mut devamp = None;
        if let (Voice::Started { .. }, Phase::Action { .. }) = (run.voice, run.phase) {
            let voice_at = run.commit(at.max(now), cx.horizon);
            // The resumed voice is a new segment with its own id: feedback still on its way for
            // the paused one (e.g. the end report of a pause carried out as a stop) is then
            // stale and ignored, whenever it arrives.
            cx.out.push(Action::ResumeVoice {
                cue,
                run: fresh,
                paused: run.run,
                at: voice_at,
                offset,
            });
            run.run = fresh;
            let first = match run.voice {
                Voice::Started { first, .. } => first,
                Voice::None | Voice::Loaded => voice_at,
            };
            run.voice = Voice::Started {
                at: voice_at,
                from: offset,
                first,
            };
            // A known end follows the voice, which resumes on its own sample.
            if let (Some(end), Phase::Action { start, .. }) = (run.planned_end(), run.phase) {
                run.phase = Phase::Action {
                    start,
                    end: Some(end),
                };
            }
            // A devamp still pending is sent again for the resumed segment (QLab keeps it
            // through a pause); answers for the paused segment's id are stale.
            if let Some(d) = run.devamp.as_mut() {
                d.at = None;
                devamp = Some(Action::DevampVoice {
                    cue,
                    run: fresh,
                    at: voice_at,
                    stop: d.stop,
                });
            }
        }
        cx.out.extend(devamp);
        if run.fade.is_some() {
            self.resume_fade(cue, at, cx);
        }
        self.report(cue, before, at, cx);
    }

    pub(super) fn load(&mut self, cue: CueId, time: f64, cx: &mut Ctx) {
        let Some(model) = cx.ws.find_cue(cue) else {
            return cx.warn(EngineWarning::UnknownCue { cue });
        };
        if matches!(self.slots.get(&cue), Some(Slot::Loaded(_)) | None) && self.refuses(cue, cx) {
            return cx.warn(EngineWarning::CueBroken { cue });
        }
        match self.slots.get(&cue) {
            Some(Slot::Broken(_)) => return cx.warn(EngineWarning::CueBroken { cue }),
            Some(Slot::Active(run)) if run.is_broken() => {
                return cx.warn(EngineWarning::CueBroken { cue });
            }
            Some(Slot::Active(_)) => return cx.warn(EngineWarning::LoadIgnored { cue }),
            Some(Slot::Loaded(_)) | None => {}
        }
        let before = self.state(cue);
        if let Some(old) = self.slots.remove(&cue) {
            release(cue, &old, cx);
        }
        let run = self.new_run();
        let offset = cx.secs(time, &self.config);
        let voice = self.is_armed(model) && matches!(model.kind, CueKind::Audio { .. });
        // The split between pre-wait and action is fixed here, for the stream and the start.
        let pre_wait = cx.secs(model.pre_wait, &self.config);
        let (pre_wait_left, action_offset) = if offset < pre_wait {
            (pre_wait - offset, 0)
        } else {
            (0, offset - pre_wait)
        };
        if voice {
            cx.out.push(Action::LoadVoice {
                cue,
                run,
                offset: action_offset,
            });
        }
        self.slots.insert(
            cue,
            Slot::Loaded(Loaded {
                run,
                pre_wait_left,
                action_offset,
                pre_wait_total: pre_wait,
                voice,
            }),
        );
        self.report(cue, before, cx.now, cx);
    }

    pub(super) fn reset_one(&mut self, cue: CueId, cx: &mut Ctx) {
        let before = self.state(cue);
        self.continued.remove(&cue);
        if let Some(slot) = self.remove_slot(cue, cx.now, Exit::Stopped, cx) {
            release(cue, &slot, cx);
        }
        self.report(cue, before, cx.now, cx);
    }
}
