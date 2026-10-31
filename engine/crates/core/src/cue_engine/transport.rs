// SPDX-License-Identifier: GPL-3.0-or-later

//! Workspace-wide transport: Panic (PB-03), Stop All, Reset All, Pause All and Resume All
//! (PB-04). They reach every cue of every list at every depth through the per-cue operations,
//! so every rule of those holds. Rules: `docs/cue-engine.md`, "Panic", "Reset All" and "Pause
//! All and Resume All".

use std::collections::HashSet;

use crate::cue::FadeCurve;
use crate::model::CueId;

use super::run::{Phase, Run, Slot, Voice};
use super::time::SampleTime;
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

    /// Panic: every playing voice fades out to silence by the same end sample, then stops there;
    /// everything else stops at once, so nothing pending fires. A second Panic before that end,
    /// or a `panic_duration` of 0, stops everything at once. Children go before their groups
    /// (as in Stop), so a group sees which of its children are still fading out.
    pub(super) fn panic(&mut self, cx: &mut Ctx) {
        let frames = cx.secs(cx.ws.settings.panic_duration, &self.config);
        let second = self.panic.is_some_and(|(_, end)| cx.now < end);
        let hard = frames == 0 || second;
        let end = cx.now.saturating_add(frames);
        self.panic = (!hard).then_some((cx.now, end));
        // Previews are the operator's own listening, not the show, but Panic means everything
        // stops: they fade out with the show (QLab checks V4, V6).
        self.panic_previews((!hard).then_some(end), cx);
        // Only cues with a runtime are touched: with none, the walk of every cue is skipped
        // (closing an idle 2,000-cue doc panics it twice, within one audio block: M4.15i).
        if self.slots.is_empty() {
            return;
        }
        for tree in Self::trees(cx) {
            for cue in tree.into_iter().rev() {
                if !self.slots.contains_key(&cue) {
                    continue;
                }
                if hard {
                    self.stop_one(cue, None, cx);
                } else {
                    self.panic_one(cue, end, cx);
                }
            }
        }
    }

    /// Stop All: everything stops at once, children before their groups (as in Stop), and so do
    /// the previews. No fade, no panic window (an earlier panic's is over).
    pub(super) fn stop_all(&mut self, cx: &mut Ctx) {
        // Also the voices of previews still fading out in a panic.
        self.panic_previews(None, cx);
        self.panic = None;
        for tree in Self::trees(cx) {
            for cue in tree.into_iter().rev() {
                if self.slots.contains_key(&cue) {
                    self.stop_one(cue, None, cx);
                }
            }
        }
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
        let ws = cx.ws;
        for list in &ws.cue_lists {
            let first = super::playhead::positions(&list.cues).first().copied();
            self.move_playhead(list.id, first, cx);
        }
    }

    /// A lost device (M4.2g): every active run stops at once, children before their groups, so
    /// nothing scheduled before the loss starts by itself after the device returns. Loaded cues
    /// and settled broken cues stay as they are.
    pub(super) fn device_lost(&mut self, cx: &mut Ctx) {
        for tree in Self::trees(cx) {
            for cue in tree.into_iter().rev() {
                if matches!(self.slots.get(&cue), Some(Slot::Active(_))) {
                    self.stop_one(cue, None, cx);
                }
            }
        }
    }

    /// One cue's part of a panic ending on `end`: a playing voice fades out to `end` (one already
    /// fading out that ends sooner keeps its fade); a group in its action whose children are
    /// fading out waits for them; anything else stops at once.
    fn panic_one(&mut self, cue: CueId, end: SampleTime, cx: &mut Ctx) {
        let before = self.state(cue);
        let now = cx.now;
        self.release_moved_runs(cue, cx.ws);
        let busy_children = self.any_busy_child(cue);
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return self.stop_one(cue, None, cx);
        };
        let in_action = !run.is_broken()
            && run.paused_at.is_none()
            && matches!(run.phase, Phase::Action { .. });
        match (in_action, run.voice, run.group.as_mut()) {
            (true, _, Some(g)) if busy_children => {
                g.stopping = true;
                g.devamp = None;
                run.post_wait = None;
                run.follow = false;
                run.panicked = true;
                self.report(cue, before, now, cx);
            }
            (true, Voice::Started { at: started, .. }, None) => {
                // Part of the panic either way (see `Run::panicked`).
                run.panicked = true;
                if run.stop_end.is_some_and(|e| e <= end) {
                    return;
                }
                let at = run.commit(now.max(started), cx.horizon);
                if at >= end {
                    return self.stop_one(cue, None, cx);
                }
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
                run.post_wait = None;
                run.follow = false;
                // Its Devamp cue's next cue must not start during the panic.
                run.devamp = None;
                self.report(cue, before, now, cx);
            }
            _ => self.stop_one(cue, None, cx),
        }
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
        let frames = cx.secs(cx.ws.settings.panic_duration, &self.config);
        if frames == 0 {
            return self.stop_tree(cue, None, cx);
        }
        let end = cx.now.saturating_add(frames);
        for id in self.inside(cue, cx).into_iter().rev() {
            if self.slots.contains_key(&id) {
                self.panic_one(id, end, cx);
            }
        }
        self.panic_one(cue, end, cx);
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
                Some(Slot::Active(Run {
                    paused_at: Some(_),
                    ..
                }))
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
