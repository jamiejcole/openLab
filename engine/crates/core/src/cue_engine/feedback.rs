// SPDX-License-Identifier: GPL-3.0-or-later

//! Feedback from the audio side: an action ended, its real length became known, or it failed.

use crate::cue::{CueKind, GroupMode};
use crate::model::{CueId, Workspace};

use super::group;
use super::run::{BrokePaused, DevampWait, Exit, Lost, Phase, Run, Slot, Voice};
use super::time::SampleTime;
use super::timers::Timer;
use super::types::{BrokenReason, RunId};
use super::{Ctx, CueEngine, release};

impl CueEngine {
    // ----- feedback -----

    pub(super) fn action_ended(&mut self, cue: CueId, run_id: RunId, at: SampleTime, cx: &mut Ctx) {
        if self.preview_ended(cue, run_id, at, cx) {
            return;
        }
        let before = self.state(cue);
        // Whether the end of a paused run would start a sibling or a runless playlist's next
        // child (looked up before the run is borrowed).
        let paused = matches!(self.slots.get(&cue), Some(Slot::Active(r)) if r.paused_at.is_some());
        let chains = paused && group::chains_siblings(cx.ws, cue);
        let runless = paused && in_runless_playlist(self, cue, cx.ws);
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return;
        };
        if run.run != run_id {
            return;
        }
        // A paused voice cannot end by itself; an end dated after the pause is the pause being
        // carried out (e.g. as a stop), not the action completing. An end dated on the pause
        // sample is an end: the mixer dates it on the first sample after the voice's
        // last, so the voice had nothing left to play when the pause applied, and a known end
        // on that sample (`ActionLength`) is carried out before the pause, too.
        if run.paused_at.is_some_and(|paused| at > paused) {
            return;
        }
        if run.stop_end.is_some() {
            self.remove_slot(cue, at, Exit::Stopped, cx);
            return self.report(cue, before, at, cx);
        }
        // An end known in advance (`ActionLength`) is a deadline; a report before it is an
        // earlier end and still completes the action.
        let (Phase::Action { start, .. }, Voice::Started { .. }) = (run.phase, run.voice) else {
            return;
        };
        let at = at.max(start);
        // Dated before or on the pause, arriving after it: a paused show starts nothing,
        // so when this end would start a cue (its auto-follow, its Devamp cue's next cue, or the
        // next child of a playlist it was started in on its own), the end is held as a deadline
        // on the pause sample, which the resume moves to the resume sample, as for a failure
        // while paused. The voice is gone, so the resume sends nothing for it. A stop, reset or
        // panic cancels it.
        if let Some(paused) = run.paused_at {
            let start_next = matches!(
                run.devamp,
                Some(DevampWait {
                    start_next: true,
                    at: Some(_),
                    ..
                })
            );
            if (run.follow && chains) || start_next || runless {
                run.voice = Voice::None;
                run.length = None;
                run.phase = Phase::Action {
                    start,
                    end: Some(paused.max(start)),
                };
                // Its next cue starts there too (`step` carries out the devamp first).
                run.devamp = run
                    .devamp
                    .filter(|d| d.start_next && d.at.is_some())
                    .map(|d| DevampWait {
                        at: Some(paused),
                        ..d
                    });
                return self.report(cue, before, cx.now, cx);
            }
        }
        run.phase = Phase::Done;
        run.voice = Voice::None;
        let follow = run.follow;
        let seq = run.seq;
        let started_in = run.started_in;
        // The voice ended before the devamp's deadline was carried out (it left the loop
        // there): the Devamp cue's next cue still starts, on the devamp sample.
        let devamp = run.devamp.take();
        self.settle(cue, at, cx);
        self.report(cue, before, at, cx);
        if follow {
            self.continue_after(cue, at, cx);
        }
        if let Some(DevampWait {
            by,
            start_next: true,
            at: Some(left),
            ..
        }) = devamp
        {
            self.continue_after(by, left.min(at), cx);
        }
        self.child_action_done(cue, seq, started_in, at, cx);
    }

    /// The binary knows how long the run's action is: its end becomes a deadline. Ignored for a
    /// run that is not playing its voice in its action (stale, stopping, broken, paused before
    /// its start). A Video cue's preview takes it as its end (`preview_length`); an Audio cue's ends
    /// with its voice.
    pub(super) fn action_length(&mut self, cue: CueId, run_id: RunId, frames: SampleTime) {
        if self.preview_length(cue, run_id, frames) {
            return;
        }
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return;
        };
        if run.run != run_id || run.is_broken() || run.stop_end.is_some() {
            return;
        }
        let (Phase::Action { start, .. }, Voice::Started { .. }) = (run.phase, run.voice) else {
            return;
        };
        // Counted from the voice's (re)start; a resume counts it again from the resumed voice.
        run.length = Some(frames);
        run.phase = Phase::Action {
            start,
            end: run.planned_end(),
        };
    }

    /// The run's voice or stream failed; `unplayed`: before it produced any audio, so the cue
    /// is skipped as a cue with static problems is (its auto-follow fires, its group is not
    /// failed) instead of breaking at run time (rule 2).
    pub(super) fn action_failed(
        &mut self,
        cue: CueId,
        run_id: RunId,
        reason: BrokenReason,
        unplayed: bool,
        cx: &mut Ctx,
    ) {
        if self.preview_failed(cue, run_id, reason, cx) {
            return;
        }
        let before = self.state(cue);
        // A group owns no voice, so no feedback can concern it.
        let group = cx
            .ws
            .find_cue(cue)
            .is_some_and(|c| matches!(c.kind, CueKind::Group { .. }));
        let current = match self.slots.get(&cue) {
            // Nor a running fade (it owns no voice either).
            // Nor a run whose voice already ended while it is paused, its end held for the
            // resume (`action_ended`): the voice is gone, so a failure report cannot concern it.
            Some(Slot::Active(run)) => {
                run.run == run_id
                    && !run.is_broken()
                    && run.group.is_none()
                    && run.fade.is_none()
                    && !(run.paused_at.is_some()
                        && run.voice == Voice::None
                        && matches!(run.phase, Phase::Action { .. }))
            }
            Some(Slot::Loaded(loaded)) => loaded.run == run_id && !group,
            Some(Slot::Broken(_) | Slot::Lost(_)) | None => false,
        };
        if !current {
            return;
        }
        // Silence and free the failed run now: a broken cue owns no voice, so nothing can keep
        // sounding after it (a failed stream still plays out its ring otherwise).
        let Some(old) = self.slots.remove(&cue) else {
            return;
        };
        release(cue, &old, cx);
        match old {
            // Its device went away while it played or waited (QLab 5, measured: playing in its
            // action, fading, or counting its pre-wait; its post-wait assumed like the
            // pre-wait), and was not paused or on its way out (a voice that could not start,
            // `StartFailed`, never played: broken): it shows paused with its time
            // standing still. It keeps nothing else: its voice is released above and its
            // pre-wait, post-wait and follow are gone, so nothing of it sounds or fires when the
            // device returns. Its group hears a stop, as for any device loss.
            Slot::Active(run)
                if reason == BrokenReason::DeviceLost
                    && !unplayed
                    && let Some(lost) = self.lost_at(cue, &run, cx.now) =>
            {
                // A wait whose device is back already has nothing to show.
                if !(lost.clears_on_return && self.device_back(cue)) {
                    self.slots.insert(cue, Slot::Lost(lost));
                }
                // One that was playing leaves its group paused with it.
                let exit = if lost.clears_on_return {
                    Exit::Stopped
                } else {
                    Exit::Lost
                };
                cx.vacated.push((run.started_in, cx.now, exit));
                self.report(cue, before, cx.now, cx);
            }
            // Being stopped with a fade, or fading out in a panic (a workspace Panic or the
            // cue's own): it was leaving anyway and the failure only ends that early. It ends as
            // its stop would have, but broken so the failure shows: its group hears a stop, and
            // nothing moves on from it (no continue, no playlist step, no halt), so a failure
            // never starts a cue during a panic or after a stop.
            // A lost device (as QLab) ends the run the same way: the device loss stops
            // everything a moment later, so nothing may move on in between (its pre-wait and
            // post-wait are cancelled too, so nothing it scheduled starts after the device
            // returns). The binary's patch update can have marked every audio cue broken by
            // then, but a control cue would still act.
            Slot::Active(run)
                if run.stop_end.is_some() || run.panicked || reason == BrokenReason::DeviceLost =>
            {
                if !(reason == BrokenReason::DeviceLost && self.device_back(cue)) {
                    self.slots.insert(cue, Slot::Broken(reason));
                }
                cx.vacated.push((run.started_in, cx.now, Exit::Stopped));
                self.report(cue, before, cx.now, cx);
            }
            // Auto-continue is time-based: an armed or still-to-be-armed post-wait fires on its
            // schedule. Auto-follow waits for a successful completion, so it never fires.
            Slot::Active(mut run) => {
                // A child that broke while playing halts a running playlist around it (QLab
                // check Q2).
                let halt = (!unplayed).then(|| self.nominal_end(cue, &run));
                run.broken = Some(reason);
                run.unplayed = unplayed;
                run.voice = Voice::None;
                // Skipped before it played: it follows as of now (as a cue skipped for static
                // problems does at its action start); one that broke while playing never does.
                let follow = run.follow && unplayed;
                run.devamp = None;
                // Paused: a paused show starts nothing, so the action's end is held until the
                // resume (as a deadline on the pause sample, which the resume moves to the
                // resume sample), and with it what that end starts: a skipped cue's follow, or
                // the next child of a playlist it is in. A playlist child shows `paused` until
                // then (QLab 5.5.3) and its playlist hears of the failure on the resume: a
                // running one halts there, as for a child that broke while playing. A stop or
                // panic cancels it.
                let playlist =
                    run.paused_at.is_some() && in_playlist(self, cx.ws, cue, run.started_in);
                if let (Some(paused), Phase::Action { start, .. }) = (run.paused_at, run.phase)
                    && (follow || playlist)
                {
                    if playlist {
                        run.broke_paused = Some(BrokePaused {
                            halt: halt.map(|end| end.map(|e| e.saturating_sub(paused))),
                        });
                    }
                    run.follow = follow;
                    run.phase = Phase::Action {
                        start,
                        end: Some(paused),
                    };
                    run.length = None;
                    self.slots.insert(cue, Slot::Active(run));
                    return self.report(cue, before, cx.now, cx);
                }
                run.follow = false;
                if matches!(run.phase, Phase::Action { .. }) {
                    run.phase = Phase::Done;
                }
                let seq = run.seq;
                let started_in = run.started_in;
                self.slots.insert(cue, Slot::Active(run));
                self.settle(cue, cx.now, cx);
                self.report(cue, before, cx.now, cx);
                if follow {
                    self.continue_after(cue, cx.now, cx);
                }
                if let Some(end) = halt {
                    self.halt_playlist(cue, seq, started_in, end, cx);
                }
                // A playlist moves on past a child that broke (unless it halted).
                self.child_action_done(cue, seq, started_in, cx.now, cx);
            }
            Slot::Loaded(_) | Slot::Broken(_) | Slot::Lost(_) => {
                self.slots.insert(cue, Slot::Broken(reason));
                self.report(cue, before, cx.now, cx);
            }
        }
    }

    /// What a run whose device is lost on `now` shows (`Slot::Lost`): the timer that was
    /// counting, frozen there. `None` for a run that shows broken instead: one paused by the
    /// operator (QLab 5, measured), being stopped or panicked (it was leaving), or with nothing
    /// counting.
    pub(super) fn lost_at(&self, cue: CueId, run: &Run, now: SampleTime) -> Option<Lost> {
        if run.paused_at.is_some() || run.stop_end.is_some() || run.panicked {
            return None;
        }
        let countdown = |timer, until: SampleTime, total: SampleTime| {
            let remaining = until.saturating_sub(now).min(total);
            Lost {
                timer,
                elapsed: total.saturating_sub(remaining),
                remaining: Some(remaining),
                clears_on_return: true,
            }
        };
        match run.phase {
            Phase::PreWait { until } => Some(countdown(Timer::PreWait, until, run.pre_wait_total)),
            // Without a voice (a group, a cue waiting for its probe) its time is the clock's
            // since its action started, as `timers` counts it while it runs.
            Phase::Action { start, end } if matches!(run.voice, Voice::None) => {
                let at = end.map_or(now, |end| now.min(end)).max(start);
                Some(Lost {
                    timer: Timer::Action,
                    elapsed: run.action_offset.saturating_add(at.saturating_sub(start)),
                    remaining: end.map(|end| end.saturating_sub(at)),
                    clears_on_return: false,
                })
            }
            Phase::Action { .. } => {
                let elapsed = run.position(now);
                Some(Lost {
                    timer: Timer::Action,
                    elapsed,
                    remaining: run
                        .length
                        .or_else(|| self.lengths.get(&cue).copied())
                        .map(|length| length.saturating_sub(elapsed)),
                    clears_on_return: false,
                })
            }
            Phase::Done => run
                .post_wait
                .map(|until| countdown(Timer::PostWait, until, run.post_wait_total)),
        }
    }
}

/// Whether `cue`'s run started in, and is still in, a playlist group with no run of its own,
/// which the run's end carries on (`follow_through`).
fn in_runless_playlist(engine: &CueEngine, cue: CueId, ws: &Workspace) -> bool {
    let started_in = match engine.slots.get(&cue) {
        Some(Slot::Active(run)) => run.started_in,
        _ => None,
    };
    group::ancestors(ws, cue).first().is_some_and(|parent| {
        started_in == Some(*parent)
            && !matches!(engine.slots.get(parent), Some(Slot::Active(_)))
            && is_playlist(ws, *parent)
    })
}

/// Whether the playlist `cue`'s run started in (`started_in`) hears of the run's end: a running
/// one does wherever the cue has been moved since; one with no run only while the cue is still
/// its child (`follow_through`).
fn in_playlist(engine: &CueEngine, ws: &Workspace, cue: CueId, started_in: Option<CueId>) -> bool {
    started_in.is_some_and(|g| {
        is_playlist(ws, g)
            && (matches!(engine.slots.get(&g), Some(Slot::Active(_)))
                || group::ancestors(ws, cue).first() == Some(&g))
    })
}

fn is_playlist(ws: &Workspace, group: CueId) -> bool {
    matches!(
        ws.find_cue(group).map(|g| &g.kind),
        Some(CueKind::Group {
            mode: GroupMode::Playlist { .. },
            ..
        })
    )
}
