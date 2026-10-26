// SPDX-License-Identifier: GPL-3.0-or-later

//! Workspace-wide transport: Panic (PB-03), Pause All and Resume All (PB-04). They reach every
//! cue of every list at every depth through the per-cue operations, so every rule of those
//! holds. Rules: `docs/cue-engine.md`, "Panic" and "Pause All and Resume All".

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
                super::each_cue(std::slice::from_ref(top), |c| {
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
        let second = self.panic_end.is_some_and(|end| cx.now < end);
        let hard = frames == 0 || second;
        let end = cx.now.saturating_add(frames);
        self.panic_end = (!hard).then_some(end);
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

    /// One cue's part of a panic ending on `end`: a playing voice fades out to `end` (one already
    /// fading out that ends sooner keeps its fade); a group in its action whose children are
    /// fading out waits for them; anything else stops at once.
    fn panic_one(&mut self, cue: CueId, end: SampleTime, cx: &mut Ctx) {
        let before = self.state(cue);
        let now = cx.now;
        let busy_children = self.any_busy_child(cx.ws, cue);
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

    /// Pause All: pauses every active cue, groups before their children (as Pause does).
    pub(super) fn pause_all(&mut self, cx: &mut Ctx) {
        for cue in Self::trees(cx).into_iter().flatten() {
            self.pause_one(cue, cx);
        }
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
    }

    /// The sample the panic fade under way ends on: a Panic before it stops everything at once.
    pub fn panic_end(&self) -> Option<SampleTime> {
        self.panic_end
    }
}
