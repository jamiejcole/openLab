// SPDX-License-Identifier: GPL-3.0-or-later

//! Feedback from the audio side: an action ended, its real length became known, or it failed
//! (docs/cue-engine.md, "Feedback", "Broken cues").

use crate::cue::{CueKind, GroupMode};
use crate::model::{CueId, Workspace};

use super::group;
use super::run::{DevampWait, Exit, Phase, Slot, Voice};
use super::time::SampleTime;
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
        // sample is an end (M2.34): the mixer dates it on the first sample after the voice's
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
        // Dated before or on the pause, arriving after it (M2.33): a paused show starts nothing,
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
        self.child_action_done(cue, seq, at, cx);
    }

    /// The binary knows how long the run's action is: its end becomes a deadline. Ignored for a
    /// run that is not playing its voice in its action (stale, stopping, broken, paused before
    /// its start), and for previews (their end changes nothing in the show).
    pub(super) fn action_length(&mut self, cue: CueId, run_id: RunId, frames: SampleTime) {
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
            Some(Slot::Broken(_)) | None => false,
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
            // Being stopped with a fade, or fading out in a panic (a workspace Panic or the
            // cue's own): it was leaving anyway and the failure only ends that early. It ends as
            // its stop would have, but broken so the failure shows: its group hears a stop, and
            // nothing moves on from it (no continue, no playlist step, no halt), so a failure
            // never starts a cue during a panic or after a stop (M2.31).
            // A lost device (M4.2g, as QLab) ends the run the same way: the device loss stops
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
                cx.vacated.push((cue, cx.now, Exit::Stopped));
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
                // the next child of a playlist it was started in on its own (which has no run to
                // hold it, as a running playlist does: `GroupRun::advance`). A stop or panic
                // cancels it.
                if let (Some(paused), Phase::Action { start, .. }) = (run.paused_at, run.phase)
                    && (follow || in_runless_playlist(self, cue, cx.ws))
                {
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
                self.slots.insert(cue, Slot::Active(run));
                self.settle(cue, cx.now, cx);
                self.report(cue, before, cx.now, cx);
                if follow {
                    self.continue_after(cue, cx.now, cx);
                }
                if let Some(end) = halt {
                    self.halt_playlist(cue, seq, end, cx);
                }
                // A playlist moves on past a child that broke (unless it halted).
                self.child_action_done(cue, seq, cx.now, cx);
            }
            Slot::Loaded(_) | Slot::Broken(_) => {
                self.slots.insert(cue, Slot::Broken(reason));
                self.report(cue, before, cx.now, cx);
            }
        }
    }
}

/// Whether `cue` is a child of a playlist group with no run of its own, which a child's end
/// carries on (`follow_through`).
fn in_runless_playlist(engine: &CueEngine, cue: CueId, ws: &Workspace) -> bool {
    group::ancestors(ws, cue).first().is_some_and(|parent| {
        !matches!(engine.slots.get(parent), Some(Slot::Active(_)))
            && matches!(
                ws.find_cue(*parent).map(|g| &g.kind),
                Some(CueKind::Group {
                    mode: GroupMode::Playlist { .. },
                    ..
                })
            )
    })
}
