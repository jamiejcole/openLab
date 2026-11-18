// SPDX-License-Identifier: GPL-3.0-or-later

//! Workspace-wide transport: Panic (PB-03), Stop All, Reset All, Pause All and Resume All
//! (PB-04). They reach every cue of every list at every depth through the per-cue operations,
//! so every rule of those holds.

use std::collections::HashSet;

use crate::cue::{Cue, FadeCurve};
use crate::model::{CueId, Workspace};

use super::run::{Phase, Run, Slot, Voice};
use super::time::{SampleTime, seconds_to_samples};
use super::types::{Action, StopFade};
use super::{Ctx, CueEngine};

/// The panic fade's curve (the workspace setting has only a duration).
pub const PANIC_CURVE: FadeCurve = FadeCurve::Linear;

impl CueEngine {
    /// Every cue of every list, by top-level cue: each top-level cue's tree depth-first (a group
    /// before its children), each id once. Walks the model itself, so even a group whose id is
    /// duplicated (an invalid workspace) has its children reached.
    fn trees(cx: &Ctx) -> Vec<Vec<CueId>> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for list in &cx.ws.cue_lists {
            for top in &list.cues {
                let mut tree = Vec::new();
                crate::tree::descendants(std::slice::from_ref(top)).for_each(|c| {
                    if seen.insert(c.id) {
                        tree.push(c.id);
                    }
                });
                out.push(tree);
            }
        }
        out
    }

    /// The workspace's panic time in samples (0: a panic stops at once).
    pub(super) fn panic_frames(&self, ws: &Workspace) -> SampleTime {
        seconds_to_samples(ws.settings.panic_duration, self.config.sample_rate())
    }

    /// Panic: every playing voice fades out to silence by the same end sample, then stops there;
    /// everything else stops at once, so nothing pending fires. A second Panic before that end,
    /// or a `panic_duration` of 0, stops everything at once. Children go before their groups
    /// (as in Stop), so a group sees which of its children are still fading out.
    pub(super) fn panic(&mut self, cx: &mut Ctx) {
        let frames = self.panic_frames(cx.ws);
        let second = self.panic.is_some_and(|(_, end)| cx.now < end);
        let hard = frames == 0 || second;
        let end = cx.now.saturating_add(frames);
        self.panic = (!hard).then_some((cx.now, end));
        // Previews are the operator's own listening, not the show, but Panic means everything
        // stops: they fade out with the show (QLab checks V4, V6).
        self.panic_previews((!hard).then_some(end), cx);
        // Only cues with a runtime are touched: with none, the walk of every cue is skipped
        // (closing an idle 2,000-cue doc panics it twice, within one audio block).
        if self.slots.is_empty() {
            return;
        }
        self.with_ducks_held(cx, |e, cx| {
            for tree in Self::trees(cx) {
                for cue in tree.into_iter().rev() {
                    if !e.slots.contains_key(&cue) {
                        continue;
                    }
                    if hard {
                        e.stop_one(cue, None, cx);
                    } else {
                        e.panic_one(cue, end, cx);
                    }
                }
            }
        });
    }

    /// Stop All: everything stops at once, children before their groups (as in Stop), and so do
    /// the previews. No fade, no panic window (an earlier panic's is over).
    pub(super) fn stop_all(&mut self, cx: &mut Ctx) {
        // Also the voices of previews still fading out in a panic.
        self.panic_previews(None, cx);
        self.panic = None;
        self.with_ducks_held(cx, |e, cx| {
            for tree in Self::trees(cx) {
                for cue in tree.into_iter().rev() {
                    if e.slots.contains_key(&cue) {
                        e.stop_one(cue, None, cx);
                    }
                }
            }
        });
    }

    /// Reset All (QLab's sidebar reset, manual pp. 97-98): a hard stop of everything (as Stop
    /// All), then a Reset of every cue that still has a runtime (broken at run time, loaded),
    /// children before their groups; random groups start a fresh cycle and every list's
    /// playhead goes back to its first cue, as when the workspace was opened. Priming the cue
    /// now standing by is the binary's job, as after any playhead move.
    pub(super) fn reset_all(&mut self, cx: &mut Ctx) {
        self.stop_all(cx);
        if !self.slots.is_empty() {
            for tree in Self::trees(cx) {
                for cue in tree.into_iter().rev() {
                    if self.slots.contains_key(&cue) {
                        self.reset_one(cue, cx);
                    }
                }
            }
        }
        self.continued.clear();
        self.random_cycles.clear();
        // Every row shows its first colour again (QLab 5, manual p. 127).
        for cue in std::mem::take(&mut self.played) {
            cx.out.push(Action::PlayedChanged { cue, played: false });
        }
        let ws = cx.ws;
        for list in &ws.cue_lists {
            let first = super::playhead::positions(&list.cues).first().copied();
            self.move_playhead(list.id, first, cx);
        }
    }

    /// A lost device: every active run stops at once, children before their groups, so
    /// nothing scheduled before the loss starts by itself after the device returns. Loaded cues
    /// and settled broken cues stay as they are.
    pub(super) fn device_lost(&mut self, cx: &mut Ctx) {
        self.with_ducks_held(cx, |e, cx| {
            for tree in Self::trees(cx) {
                for cue in tree.into_iter().rev() {
                    // Children come first: a group whose child the loss left paused shows
                    // paused with it rather than stopped.
                    if matches!(e.slots.get(&cue), Some(Slot::Active(_)))
                        && !e.end_group_as_lost(cue, cx.now, cx)
                    {
                        e.stop_one(cue, None, cx);
                    }
                }
            }
        });
    }

    /// One cue's part of a panic ending on `end`: a playing voice fades out to `end` (one already
    /// fading out that ends sooner keeps its fade); a group in its action whose children are
    /// fading out waits for them; anything else stops at once.
    fn panic_one(&mut self, cue: CueId, end: SampleTime, cx: &mut Ctx) {
        let before = self.state(cue);
        let now = cx.now;
        let rate = self.config.sample_rate().get();
        self.release_moved_runs(cue, cx.ws);
        let busy_children = self.any_busy_child(cue);
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return self.stop_one(cue, None, cx);
        };
        if !panic_keeps(run, busy_children, now, end) {
            return self.stop_one(cue, None, cx);
        }
        match (run.voice, run.group.as_mut()) {
            (_, Some(g)) => {
                g.stopping = true;
                g.devamp = None;
                run.post_wait = None;
                run.follow = false;
                run.panicked = true;
                self.report(cue, before, now, cx);
            }
            (Voice::Started { at: started, .. }, None) => {
                // Part of the panic either way (see `Run::panicked`).
                run.panicked = true;
                if run.stop_end.is_some_and(|e| e <= end) {
                    return;
                }
                let at = run.commit(now.max(started));
                cx.out.push(Action::StopVoice {
                    cue,
                    run: run.run,
                    at,
                    fade: Some(StopFade {
                        frames: end - at,
                        curve: PANIC_CURVE,
                    }),
                });
                run.stop_end = Some(end);
                super::others::hold_duck(cue, run, at, rate, cx);
                run.post_wait = None;
                run.follow = false;
                // Its Devamp cue's next cue must not start during the panic.
                run.devamp = None;
                self.report(cue, before, now, cx);
            }
            _ => self.stop_one(cue, None, cx),
        }
    }

    /// Whether the panic of `cue` that a trigger at `now` would set off (its own panic, ending
    /// `end`) leaves its run going rather than stopping it at once: [`panic_keeps`] for it and,
    /// for a group, for the children that would be left fading out, worked out children before
    /// their groups as [`Self::panic_tree`] goes. Changes nothing.
    pub(super) fn panic_tree_keeps(&self, cue: &Cue, now: SampleTime, end: SampleTime) -> bool {
        let tree = self.problems.tree();
        // The runs left fading out so far, with the group each started in.
        let mut kept: Vec<(CueId, Option<CueId>)> = Vec::new();
        let keeps = |id: CueId, kept: &[(CueId, Option<CueId>)]| {
            let Some(Slot::Active(run)) = self.slots.get(&id) else {
                return None;
            };
            // As `release_moved_runs`: a run that started in `id` but was moved out of it is
            // not waited on.
            let busy = kept.iter().any(|(k, started_in)| {
                *started_in == Some(id) && tree.ancestors(*k).any(|a| a == id)
            });
            panic_keeps(run, busy, now, end).then_some(run.started_in)
        };
        for id in super::group::descendants(cue).into_iter().rev() {
            if let Some(started_in) = keeps(id, &kept) {
                kept.push((id, started_in));
            }
        }
        keeps(cue.id, &kept).is_some()
    }

    /// One press of a cue's panic: a second press on a cue already fading out in a panic stops
    /// it at once (QLab check Q7), else it fades. The cues it sets fading are noted for the rest
    /// of the batch, whose later items count them as not yet panicked: a batch of a group and
    /// its child is one press of each, so the child fades with its group instead of being cut.
    pub(super) fn press_panic(&mut self, cue: CueId, cx: &mut Ctx) {
        if self.fading_in_panic(cue, cx) {
            return self.stop_tree(cue, None, cx);
        }
        let mut tree = self.inside(cue, cx);
        tree.push(cue);
        let calm: Vec<CueId> = (tree.into_iter())
            .filter(|id| matches!(self.slots.get(id), Some(Slot::Active(r)) if !r.panicked))
            .collect();
        self.panic_tree(cue, cx);
        for id in calm {
            if matches!(self.slots.get(&id), Some(Slot::Active(r)) if r.panicked) {
                self.batch_panicked.insert(id);
            }
        }
    }

    /// A press of `PanicCue` is a second one when the cue itself is fading out in a panic (a
    /// workspace Panic or its own), or it has active descendants and every one of them is. A group
    /// with only some children already fading is on its first press: the rest fade. A cue an
    /// earlier item of the same batch set fading does not count as fading (`press_panic`).
    pub(super) fn fading_in_panic(&self, cue: CueId, cx: &Ctx) -> bool {
        let panicked = |id: &CueId| match self.slots.get(id) {
            Some(Slot::Active(run)) => Some(run.panicked && !self.batch_panicked.contains(id)),
            _ => None,
        };
        if panicked(&cue) == Some(true) {
            return true;
        }
        let below: Vec<bool> = self.inside(cue, cx).iter().filter_map(panicked).collect();
        !below.is_empty() && below.iter().all(|p| *p)
    }

    /// One cue's own panic (its second-trigger option): the cue and everything in it as in a
    /// Panic, ending `panic_duration` from now (0: at once). Not a workspace panic: other cues and
    /// `panic_end` are untouched. Its cues count as fading out in a panic, so a further trigger
    /// restarts them.
    pub(super) fn panic_tree(&mut self, cue: CueId, cx: &mut Ctx) {
        let frames = self.panic_frames(cx.ws);
        if frames == 0 {
            return self.stop_tree(cue, None, cx);
        }
        let end = cx.now.saturating_add(frames);
        self.with_ducks_held(cx, |e, cx| {
            for id in e.inside(cue, cx).into_iter().rev() {
                if e.slots.contains_key(&id) {
                    e.panic_one(id, end, cx);
                }
            }
            e.panic_one(cue, end, cx);
        });
    }

    /// Pause All: pauses every active cue, groups before their children (as Pause does), and
    /// every preview (QLab check V4).
    pub(super) fn pause_all(&mut self, cx: &mut Ctx) {
        for cue in Self::trees(cx).into_iter().flatten() {
            self.pause_one(cue, cx);
        }
        self.pause_previews(cx);
    }

    /// Resume All: resumes every paused cue, however it was paused (QLab), outermost first so a
    /// group resumes with its children.
    pub(super) fn resume_all(&mut self, cx: &mut Ctx) {
        for cue in Self::trees(cx).into_iter().flatten() {
            // A group resumed earlier in this loop has already resumed this one.
            if matches!(
                self.slots.get(&cue),
                Some(
                    Slot::Active(Run {
                        paused_at: Some(_),
                        ..
                    }) | Slot::Lost(_)
                )
            ) {
                self.resume_tree(cue, cx.now, cx);
            }
        }
        self.resume_previews(cx);
    }

    /// The sample the panic fade under way ends on: a Panic before it stops everything at once.
    pub fn panic_end(&self) -> Option<SampleTime> {
        self.panic.map(|(_, end)| end)
    }
}

/// What a panic ending on `end`, as of `now`, does with one run (the single test for both a
/// panic and the playlist mark): true when the run goes on, a group in its action waiting for
/// `busy_children` still fading out, or a voice in its action fading out to `end` (or keeping a
/// stop fade that ends sooner). Anything else stops at once: a pre-wait, a paused
/// or broken run, a cue with no voice playing (a Wait, a Fade, a Memo), a group with no child
/// fading out, a voice that would only start on or after `end`.
pub(super) fn panic_keeps(run: &Run, busy_children: bool, now: SampleTime, end: SampleTime) -> bool {
    if run.is_broken() || run.paused_at.is_some() || !matches!(run.phase, Phase::Action { .. }) {
        return false;
    }
    match (run.voice, &run.group) {
        (_, Some(_)) => busy_children,
        (Voice::Started { at: started, .. }, None) => {
            run.stop_end.is_some_and(|e| e <= end) || now.max(started).max(run.committed) < end
        }
        _ => false,
    }
}
