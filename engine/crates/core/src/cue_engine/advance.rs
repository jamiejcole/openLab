// SPDX-License-Identifier: GPL-3.0-or-later

//! Deadlines: each due cue steps through pre-wait, action and post-wait, its action begins, and
//! its continue fires (docs/cue-engine.md, "Timing and continue modes", "Deadlines, lookahead
//! and late ticks").

use crate::cue::{ContinueMode, CueKind};
use crate::model::{CueId, Workspace};
use crate::tree;

use super::run::{self, Exit, Phase, Slot, Voice};
use super::time::SampleTime;
use super::types::{Action, BrokenReason, CueState, EngineWarning, RunId};
use super::{Ctx, CueEngine, group, level, starts_next_on_devamp};

impl CueEngine {
    // ----- timers -----

    /// Carries out every deadline up to the horizon, earliest first, each at its own sample.
    pub(super) fn advance(&mut self, cx: &mut Ctx) {
        // Each step consumes a pending deadline. Deadlines come only from starts (bounded by the
        // budget), a run's own progress (pre-wait, action end, post-wait) and devamp answers (at
        // most one per run), so this ends; the guard is a second line of defence against a bug
        // turning into a hung show.
        let mut steps = 0usize;
        while let Some((cue, at)) = self.earliest_due(cx.horizon) {
            // Read each round: a stale limit can be raised while this runs (`budget_left`).
            let guard = self
                .slots
                .len()
                .saturating_add(cx.limit)
                .saturating_add(1)
                .saturating_mul(4);
            if steps >= guard {
                return cx.warn(EngineWarning::ChainLimit {
                    cue,
                    limit: cx.limit,
                });
            }
            steps += 1;
            self.step(cue, at, cx);
            self.drain(cx);
        }
    }

    fn earliest_due(&self, horizon: SampleTime) -> Option<(CueId, SampleTime)> {
        self.slots
            .iter()
            .filter_map(|(id, slot)| match slot {
                Slot::Active(run) => run
                    .next_deadline()
                    .filter(|d| *d <= horizon)
                    .map(|d| (d, run.seq, *id)),
                Slot::Loaded(_) | Slot::Broken(_) => None,
            })
            .min()
            .map(|(d, _, id)| (id, d))
    }

    /// Carries out the deadline of `cue` that falls on `at`.
    fn step(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        let before = self.state(cue);
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return;
        };
        if run.stop_end.is_some_and(|end| end <= at) {
            self.remove_slot(cue, at, Exit::Stopped, cx);
            return self.report(cue, before, at, cx);
        }
        if let Some(d) = run.devamp
            && d.at.is_some_and(|left| left <= at)
        {
            // The voice leaves its loop here: the Devamp cue's next cue starts on this sample.
            run.devamp = None;
            if d.start_next {
                self.continue_after(d.by, at, cx);
            }
            return;
        }
        if let Phase::PreWait { until } = run.phase {
            if until <= at {
                self.begin_action(cue, at, cx);
            }
            return;
        }
        if run.post_wait.is_some_and(|p| p <= at) {
            run.post_wait = None;
            self.settle(cue, at, cx);
            self.report(cue, before, at, cx);
            return self.continue_after(cue, at, cx);
        }
        if let Phase::Action { end: Some(end), .. } = run.phase
            && end <= at
        {
            // A group's action has an end only while a halted playlist waits out its broken
            // child's nominal end (`halt_playlist`).
            if run.group.is_some() {
                return self.resolve_group(cue, Exit::Completed, at, cx);
            }
            run.phase = Phase::Done;
            // An audio action ended on its known length: its voice is done with, and the
            // mixer's report that follows is stale.
            run.voice = Voice::None;
            run.length = None;
            let follow = run.follow;
            let seq = run.seq;
            if run.fade.is_some() {
                self.fade_finish(cue, at, cx);
                // Stopping its target stopped the Fade cue too (it is inside the group); that
                // stop reported it.
                if !matches!(self.slots.get(&cue), Some(Slot::Active(r)) if r.seq == seq) {
                    return;
                }
            }
            self.settle(cue, at, cx);
            self.report(cue, before, at, cx);
            if follow {
                self.continue_after(cue, at, cx);
            }
            self.child_action_done(cue, seq, at, cx);
        }
    }

    /// The pre-wait has elapsed: the action starts on sample `at`.
    fn begin_action(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        let before = self.state(cue);
        let ws = cx.ws;
        let Some(model) = ws.find_cue(cue) else {
            // Cannot happen (missing cues are dropped at the start of each call), but a run left
            // due would never let the deadline loop move on.
            self.remove_slot(cue, at, Exit::Stopped, cx);
            return self.report(cue, before, at, cx);
        };
        let post_wait = cx.secs(model.post_wait, &self.config);
        // A run that broke during its pre-wait, or a cue that has static problems now, skips its
        // action but keeps its timing.
        let problems = self.refuses(cue, cx);
        // Broke at run time (feedback during its pre-wait): it never auto-follows (rule 2),
        // unless it failed before playing anything (skipped, as for static problems).
        let failed = matches!(self.slots.get(&cue),
            Some(Slot::Active(r)) if r.broken.is_some() && !r.unplayed);
        let broken =
            problems || matches!(self.slots.get(&cue), Some(Slot::Active(r)) if r.is_broken());
        let act = match &model.kind {
            _ if !self.is_armed(model) || broken => Act::Silent(0),
            CueKind::Audio { .. } => Act::Voice,
            CueKind::Wait { duration } => Act::Silent(cx.secs(*duration, &self.config)),
            CueKind::Memo => Act::Silent(0),
            CueKind::Group { .. } => Act::Group,
            CueKind::Fade { duration, .. } => Act::Fade(cx.secs(*duration, &self.config)),
            CueKind::Start { .. }
            | CueKind::Stop { .. }
            | CueKind::Pause { .. }
            | CueKind::Load { .. }
            | CueKind::Reset { .. }
            | CueKind::Devamp { .. }
            | CueKind::GoTo { .. }
            | CueKind::Arm { .. }
            | CueKind::Disarm { .. } => Act::Control,
            CueKind::Network { .. } => Act::Send,
        };
        // QLab: a Devamp cue's "start next cue" is its continue, fired when the loop is left
        // (see `devamp`), so its own continue mode does not also start the next cue. A broken
        // one sends no devamp, so, like a disarmed one, its own continue mode applies.
        let continue_mode = if starts_next_on_devamp(model, self.is_armed(model) && !broken) {
            ContinueMode::DoNotContinue
        } else {
            group::effective_continue(ws, model)
        };
        let now = cx.now;
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return;
        };
        let offset = run.action_offset;
        if problems && run.broken.is_none() {
            run.static_broken = true;
        }
        match act {
            Act::Voice => {
                let voice_at = run.commit(at.max(now), cx.horizon);
                if at < now {
                    cx.warn(EngineWarning::LateStart {
                        cue,
                        late_by: now - at,
                    });
                }
                cx.out.push(Action::StartVoice {
                    cue,
                    run: run.run,
                    at: voice_at,
                    offset,
                });
                run.voice = Voice::Started {
                    at: voice_at,
                    from: offset,
                    first: voice_at,
                };
                // The binary starts the voice with the model's levels as they are now.
                if let CueKind::Audio { levels, .. } = &model.kind {
                    run.given = Some(run::Given {
                        model: levels.clone(),
                        played: level::played(levels),
                    });
                }
            }
            Act::Send => {
                if let CueKind::Network {
                    destination,
                    address,
                    args,
                } = &model.kind
                {
                    cx.out.push(Action::SendOsc {
                        cue,
                        at,
                        destination: *destination,
                        address: address.clone(),
                        args: args.clone(),
                    });
                }
                if run.voice == Voice::Loaded {
                    cx.out.push(Action::UnloadVoice { cue, run: run.run });
                }
                run.voice = Voice::None;
            }
            Act::Silent(_) | Act::Group | Act::Fade(_) | Act::Control => {
                if run.voice == Voice::Loaded {
                    cx.out.push(Action::UnloadVoice { cue, run: run.run });
                }
                run.voice = Voice::None;
            }
        }
        run.phase = Phase::Action {
            start: at,
            end: match act {
                Act::Silent(d) | Act::Fade(d) => Some(at.saturating_add(d.saturating_sub(offset))),
                Act::Control | Act::Send => Some(at),
                Act::Voice | Act::Group => None,
            },
        };
        run.post_wait = (continue_mode == ContinueMode::AutoContinue)
            .then(|| at.saturating_add(post_wait.saturating_sub(offset)));
        run.post_wait_total = post_wait;
        // A cue skipped for static problems follows as a disarmed one does (QLab 5: GO on a
        // broken auto-follow cue starts the next cue); one that broke at run time does not.
        run.follow = !failed && continue_mode == ContinueMode::AutoFollow;
        self.settle(cue, at, cx);
        self.report(cue, before, at, cx);
        // The cue is in its action while it acts, so a trigger cycle back to it is ignored.
        match act {
            Act::Group => self.start_children(cue, at, cx),
            Act::Fade(_) => self.fade_begin(cue, at, cx),
            Act::Control => self.control(cue, at, cx),
            Act::Voice | Act::Silent(_) | Act::Send => {}
        }
    }

    /// Triggers the cue after `cue` in its list or group, on sample `at`. Inside timeline,
    /// playlist and random groups the group runs its children, so nothing is triggered.
    pub(super) fn continue_after(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        if !group::chains_siblings(cx.ws, cue) {
            return;
        }
        if cx
            .ws
            .find_cue(cue)
            .is_some_and(|c| matches!(c.kind, CueKind::Group { .. }))
        {
            self.continued.insert(cue);
        }
        if let Some(next) = next_sibling(cx.ws, cue) {
            self.start(next, at, cx);
        }
    }

    /// Forgets a run that has nothing left to do (on sample `at`); a broken one stays broken.
    pub(super) fn settle(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        if let Some(Slot::Active(run)) = self.slots.get(&cue)
            && run.is_finished()
        {
            // A run skipped for static problems completed as far as its group is concerned (it
            // is passed over, as a disarmed cue is); one that broke at run time did not.
            let failed = run.group.as_ref().is_some_and(|g| g.failed);
            let exit = if (run.broken.is_some() && !run.unplayed) || failed {
                Exit::Broken
            } else {
                Exit::Completed
            };
            match run.broken {
                // Its device is back already: nothing keeps it from playing.
                Some(BrokenReason::DeviceLost) if self.device_back(cue) => self.slots.remove(&cue),
                Some(reason) => self.slots.insert(cue, Slot::Broken(reason)),
                None => self.slots.remove(&cue),
            };
            cx.vacated.push((cue, at, exit));
        }
    }

    /// Removes a cue's runtime; if it was active, its group hears that it ended on `at`.
    pub(super) fn remove_slot(
        &mut self,
        cue: CueId,
        at: SampleTime,
        exit: Exit,
        cx: &mut Ctx,
    ) -> Option<Slot> {
        let slot = self.slots.remove(&cue)?;
        if let Slot::Active(run) = &slot {
            cx.vacated.push((cue, at, exit));
            // A Fade cue stopped mid-fade leaves its targets' levels where they are.
            if let Some(fade) = &run.fade {
                self.hold_fade(cue, run.seq, fade, at.max(cx.now), false, cx);
            }
        }
        Some(slot)
    }

    pub(super) fn report(&mut self, cue: CueId, before: CueState, at: SampleTime, cx: &mut Ctx) {
        let state = self.state(cue);
        if state != before {
            cx.out.push(Action::StateChanged { cue, state, at });
            self.settle_problems(cue, state, cx);
        }
    }

    pub(super) fn new_run(&mut self) -> RunId {
        let run = RunId(self.next_run);
        self.next_run = self.next_run.saturating_add(1);
        run
    }
}

fn next_sibling(ws: &Workspace, cue: CueId) -> Option<CueId> {
    let path = tree::find_path(ws, cue)?;
    let (parents, index) = path.split()?;
    tree::container(ws, path.list, parents)?
        .get(index.checked_add(1)?)
        .map(|c| c.id)
}

/// What a cue's action does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Act {
    /// Plays a voice; completes on feedback.
    Voice,
    /// Completes after this many samples.
    Silent(SampleTime),
    /// Fades its target's levels; completes after this many samples.
    Fade(SampleTime),
    /// Acts on its target at once and completes.
    Control,
    /// Sends a Network cue's OSC message and completes at once.
    Send,
    /// Runs the group's children; completes when none is busy.
    Group,
}
