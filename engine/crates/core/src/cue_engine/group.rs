// SPDX-License-Identifier: GPL-3.0-or-later

//! Group cues (CT-22): how a group's action runs its children, when it completes, and how
//! operations on a group reach its descendants. Rules: `docs/cue-engine.md`, "Groups".

use std::collections::HashSet;

use crate::cue::{ContinueMode, Cue, CueKind, FadeSpec, GroupMode};
use crate::model::{CueId, Workspace};
use crate::tree;

use super::run::{Exit, GroupRun, Phase, PlaylistRun, Run, Slot};
use super::time::SampleTime;
use super::{Ctx, CueEngine};

/// The ids of the groups around `cue`, nearest first (the first place it occurs).
pub(super) fn ancestors(ws: &Workspace, cue: CueId) -> Vec<CueId> {
    let mut out = Vec::new();
    let Some(path) = tree::find_path(ws, cue) else {
        return out;
    };
    let Some((parents, _)) = path.split() else {
        return out;
    };
    let Some(mut level) = ws.cue_lists.get(path.list).map(|l| l.cues.as_slice()) else {
        return out;
    };
    for &i in parents {
        let Some(group) = level.get(i) else {
            break;
        };
        out.push(group.id);
        level = group.children();
    }
    out.reverse();
    out
}

/// Every cue inside `cue`, depth-first, parents before children. Iterative.
pub(super) fn descendants(cue: &Cue) -> Vec<CueId> {
    let mut out = Vec::new();
    let mut stack: Vec<std::slice::Iter<'_, Cue>> = vec![cue.children().iter()];
    while let Some(level) = stack.last_mut() {
        match level.next() {
            Some(child) => {
                out.push(child.id);
                stack.push(child.children().iter());
            }
            None => {
                stack.pop();
            }
        }
    }
    out
}

fn group_mode(cue: &Cue) -> Option<GroupMode> {
    match &cue.kind {
        CueKind::Group { mode, .. } => Some(*mode),
        _ => None,
    }
}

/// Whether a cue's continue mode triggers its next sibling: always at the top level and in the
/// "start first" modes; timeline, playlist and random groups run their children themselves and
/// ignore the children's continue modes.
pub(super) fn chains_siblings(ws: &Workspace, cue: CueId) -> bool {
    let parent = ancestors(ws, cue).first().and_then(|g| ws.find_cue(*g));
    match parent.and_then(group_mode) {
        None | Some(GroupMode::StartFirstAndEnter | GroupMode::StartFirstAndGoToNext) => true,
        Some(GroupMode::Timeline | GroupMode::Playlist { .. } | GroupMode::StartRandom) => false,
    }
}

/// The continue mode that applies to `model` where it stands.
pub(super) fn effective_continue(ws: &Workspace, model: &Cue) -> ContinueMode {
    if chains_siblings(ws, model.id) {
        model.continue_mode
    } else {
        ContinueMode::DoNotContinue
    }
}

impl CueEngine {
    /// Whether the cue has a runtime that keeps its group running: active, paused, stopping, or
    /// broken with a continue still pending.
    pub(super) fn busy(&self, cue: CueId) -> bool {
        matches!(self.slots.get(&cue), Some(Slot::Active(_)))
    }

    /// Whether a child of the group is busy. A child with the group's own id (a duplicate id
    /// in an invalid workspace) shares its runtime and does not count, or the group would keep
    /// itself running.
    pub(super) fn any_busy_child(&self, ws: &Workspace, group: CueId) -> bool {
        ws.find_cue(group).is_some_and(|g| {
            g.children()
                .iter()
                .any(|c| c.id != group && self.busy(c.id))
        })
    }

    /// The group's action has started on `at`: start its children per its mode. If none ends up
    /// busy (no children, all broken, chain limit), the action completes at once.
    pub(super) fn start_children(&mut self, group: CueId, at: SampleTime, cx: &mut Ctx) {
        let ws = cx.ws;
        let Some(CueKind::Group { mode, children }) = ws.find_cue(group).map(|g| &g.kind) else {
            return;
        };
        // A stopping group never starts children (a stop before the action ends the run at
        // once; this only guards that rule).
        if matches!(self.slots.get(&group), Some(Slot::Active(run)) if run.is_group_stopping()) {
            return;
        }
        match mode {
            GroupMode::Timeline => {
                for child in children {
                    self.start_child(group, child.id, at, cx);
                }
            }
            GroupMode::StartFirstAndEnter | GroupMode::StartFirstAndGoToNext => {
                if let Some(first) = children.first() {
                    self.start_child(group, first.id, at, cx);
                }
            }
            GroupMode::Playlist { .. } => self.playlist_next(group, at, cx),
            GroupMode::StartRandom => {
                if let Some(child) = self.pick_random(group, children) {
                    self.start_child(group, child, at, cx);
                }
            }
        }
        if !self.any_busy_child(ws, group) {
            self.resolve_group(group, Exit::Completed, at, cx);
        }
    }

    /// Starts a child for its group; a child that cannot start because it is broken marks the
    /// group's run as failed.
    fn start_child(&mut self, group: CueId, child: CueId, at: SampleTime, cx: &mut Ctx) {
        self.start(child, at, cx);
        if self.broken_reason(child).is_some()
            && let Some(Slot::Active(Run { group: Some(g), .. })) = self.slots.get_mut(&group)
        {
            g.failed = true;
        }
    }

    /// One child, never the one picked last time when there is another (QLab does not repeat
    /// a random child immediately).
    fn pick_random(&mut self, group: CueId, children: &[Cue]) -> Option<CueId> {
        let last = self.last_random.get(&group).copied();
        let candidates: Vec<CueId> = children
            .iter()
            .map(|c| c.id)
            .filter(|id| children.len() < 2 || Some(*id) != last)
            .collect();
        let pick = *candidates.get(self.rng.below(candidates.len()))?;
        self.last_random.insert(group, pick);
        Some(pick)
    }

    /// Starts the playlist's next child on `at` (its first when nothing has played yet); a
    /// devamped playlist starts none, so it completes with its current child. Broken
    /// children are skipped; after the last child a looping playlist starts a new pass (shuffled
    /// afresh). Stops trying after a pass in which nothing could start, or when the chain budget
    /// is spent, so a playlist of broken or zero-length children cannot spin.
    fn playlist_next(&mut self, group: CueId, at: SampleTime, cx: &mut Ctx) {
        let ws = cx.ws;
        let Some(CueKind::Group {
            mode: GroupMode::Playlist { loop_, shuffle },
            children,
        }) = ws.find_cue(group).map(|g| &g.kind)
        else {
            return;
        };
        let ids: Vec<CueId> = children.iter().map(|c| c.id).collect();
        let mut state = match self.slots.get_mut(&group) {
            Some(Slot::Active(Run {
                group: Some(g),
                paused_at: None,
                phase: Phase::Action { .. },
                ..
            })) if !g.stopping && !g.adopted && g.devamp.is_none() => g.playlist.take(),
            _ => return,
        };
        // A pass began in this call: ending it again means a whole pass in which nothing started.
        let mut wrapped = false;
        let mut attempts = ids.len().saturating_mul(2).saturating_add(2);
        let mut playing = None;
        while attempts > 0 {
            attempts -= 1;
            let next = match state.as_mut() {
                None => None,
                Some(p) if p.order.is_empty() => {
                    // After the current child as the group stands now; if it has left the group,
                    // the child that took its place.
                    let index = p
                        .current
                        .and_then(|(c, _)| ids.iter().position(|i| *i == c))
                        .map_or(p.index, |i| i.saturating_add(1));
                    ids.get(index).map(|id| (*id, index))
                }
                Some(p) => {
                    let start = if p.current.is_some() {
                        p.pos.saturating_add(1)
                    } else {
                        p.pos
                    };
                    p.order
                        .iter()
                        .enumerate()
                        .skip(start)
                        .find(|(_, id)| ids.contains(id))
                        .map(|(pos, id)| (*id, pos))
                }
            };
            let (child, index) = match next {
                Some(n) => n,
                None => {
                    // Start of the run, or the end of a pass.
                    let first_pass = state.is_none();
                    if ids.is_empty() || (!first_pass && (!loop_ || wrapped)) {
                        break;
                    }
                    wrapped = true;
                    let mut order = Vec::new();
                    if *shuffle {
                        order = ids.clone();
                        self.rng.shuffle(&mut order);
                        // A new pass never opens with the child that closed the last one.
                        let last = state.as_ref().and_then(|p| p.current).map(|(c, _)| c);
                        if order.len() > 1 && order.first().copied() == last {
                            let j = self.rng.below(order.len() - 1).saturating_add(1);
                            order.swap(0, j);
                        }
                    }
                    state = Some(PlaylistRun {
                        current: None,
                        index: 0,
                        order,
                        pos: 0,
                    });
                    continue;
                }
            };
            let Some(p) = state.as_mut() else {
                break;
            };
            if p.order.is_empty() {
                p.index = index;
            } else {
                p.pos = index;
            }
            p.current = None;
            self.start_child(group, child, at, cx);
            if let Some(Slot::Active(run)) = self.slots.get(&child) {
                playing = Some((child, run.seq));
                break;
            }
            // Mark it passed, so the next attempt moves on from it.
            p.current = Some((child, u64::MAX));
            if cx.budget == 0 {
                break;
            }
        }
        if let Some(p) = state.as_mut() {
            p.current = playing;
        }
        if let Some(Slot::Active(Run { group: Some(g), .. })) = self.slots.get_mut(&group) {
            g.playlist = state;
        }
    }

    /// The action of `cue` (run `seq`) completed, or it broke, on `at`: a playlist around it
    /// moves on to its next child.
    pub(super) fn child_action_done(&mut self, cue: CueId, seq: u64, at: SampleTime, cx: &mut Ctx) {
        let Some(&parent) = ancestors(cx.ws, cue).first() else {
            return;
        };
        let Some(Slot::Active(Run {
            group:
                Some(
                    g @ GroupRun {
                        playlist: Some(_), ..
                    },
                ),
            paused_at,
            ..
        })) = self.slots.get_mut(&parent)
        else {
            return;
        };
        if g.playlist.as_ref().and_then(|p| p.current) != Some((cue, seq)) {
            return;
        }
        if paused_at.is_some() {
            // The end report was in flight when the playlist was paused: move on at resume.
            g.advance = true;
            return;
        }
        self.playlist_next(parent, at, cx);
    }

    /// Carries out what groups must do now that some of their children's runtimes have ended.
    pub(super) fn drain(&mut self, cx: &mut Ctx) {
        let mut i = 0;
        while let Some(&(cue, at, exit)) = cx.vacated.get(i) {
            i += 1;
            if let Some(&parent) = ancestors(cx.ws, cue).first() {
                self.child_left(parent, exit, at, cx);
            }
        }
        cx.vacated.clear();
    }

    /// A child of `group` stopped being busy on `at`.
    fn child_left(&mut self, group: CueId, exit: Exit, at: SampleTime, cx: &mut Ctx) {
        let any_busy = self.any_busy_child(cx.ws, group);
        let Some(Slot::Active(run)) = self.slots.get_mut(&group) else {
            return;
        };
        let Some(g) = run.group.as_mut() else {
            return;
        };
        if exit == Exit::Broken {
            g.failed = true;
        }
        if !matches!(run.phase, Phase::Action { .. }) || any_busy {
            return;
        }
        if run.paused_at.is_some() {
            g.pending = Some(exit);
            return;
        }
        self.resolve_group(group, exit, at, cx);
    }

    /// No child of the group is busy any more: a stopping group, or one whose last child was
    /// stopped, stops; otherwise its action completes.
    pub(super) fn resolve_group(&mut self, group: CueId, exit: Exit, at: SampleTime, cx: &mut Ctx) {
        let before = self.state(group);
        let Some(Slot::Active(run)) = self.slots.get_mut(&group) else {
            return;
        };
        let Some(g) = run.group.as_mut() else {
            return;
        };
        if !matches!(run.phase, Phase::Action { .. }) {
            return;
        }
        g.pending = None;
        if g.stopping || exit == Exit::Stopped {
            self.remove_slot(group, at, Exit::Stopped, cx);
            return self.report(group, before, at, cx);
        }
        let follow = run.follow && !g.failed && !g.adopted;
        // A devamped playlist that asked for it starts the cue after the Devamp cue now.
        let devamp_next = g.devamp.take().filter(|(_, next)| *next).map(|(by, _)| by);
        run.phase = Phase::Done;
        run.follow = false;
        let seq = run.seq;
        self.settle(group, at, cx);
        self.report(group, before, at, cx);
        if follow {
            self.continue_after(group, at, cx);
        }
        if let Some(by) = devamp_next {
            self.continue_after(by, at, cx);
        }
        self.child_action_done(group, seq, at, cx);
    }

    /// Ends groups whose action has no busy child left although no child told them (a busy
    /// child moved out of the group, a mode change, or duplicate ids): they stop at `now`.
    /// Returns whether any did.
    pub(super) fn sweep_groups(&mut self, cx: &mut Ctx) -> bool {
        let stranded: Vec<CueId> = self
            .slots
            .iter()
            .filter_map(|(id, slot)| match slot {
                Slot::Active(Run {
                    group: Some(_),
                    phase: Phase::Action { .. },
                    paused_at: None,
                    ..
                }) if !self.any_busy_child(cx.ws, *id) => Some(*id),
                _ => None,
            })
            .collect();
        let any = !stranded.is_empty();
        for group in stranded {
            self.resolve_group(group, Exit::Stopped, cx.now, cx);
        }
        any
    }

    /// A cue started on its own (not by its group) on `at`: every idle group around it starts
    /// running as an adopted group, and a post-waiting one runs again, so a group reports
    /// running while any child is.
    pub(super) fn adopt_parents(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        for group in ancestors(cx.ws, cue) {
            let before = self.state(group);
            match self.slots.get_mut(&group) {
                Some(Slot::Active(run)) => {
                    let post_waiting = run.phase == Phase::Done
                        && run.paused_at.is_none()
                        && run.group.as_ref().is_some_and(|g| !g.stopping);
                    if post_waiting {
                        run.phase = Phase::Action {
                            start: at,
                            end: None,
                        };
                        run.follow = false;
                        self.report(group, before, at, cx);
                    }
                    return;
                }
                Some(Slot::Broken(_)) => return,
                Some(Slot::Loaded(_)) | None => {
                    let run = match self.slots.get(&group) {
                        Some(Slot::Loaded(loaded)) => loaded.run,
                        _ => self.new_run(),
                    };
                    let seq = self.next_seq;
                    self.next_seq = self.next_seq.saturating_add(1);
                    let adopted = GroupRun {
                        adopted: true,
                        ..GroupRun::default()
                    };
                    let phase = Phase::Action {
                        start: at,
                        end: None,
                    };
                    self.slots
                        .insert(group, Slot::Active(Run::new(run, seq, phase, Some(adopted))));
                    self.report(group, before, at, cx);
                }
            }
        }
    }

    // ----- operations on a cue and everything inside it (CUE-05) -----

    /// The cue's descendants, if it is a group in the workspace.
    fn inside(&self, cue: CueId, cx: &Ctx) -> Vec<CueId> {
        cx.ws.find_cue(cue).map(descendants).unwrap_or_default()
    }

    /// Stops the cue and every descendant, children before their groups.
    pub(super) fn stop_tree(&mut self, cue: CueId, fade_out: Option<FadeSpec>, cx: &mut Ctx) {
        for id in self.inside(cue, cx).into_iter().rev() {
            if self.slots.contains_key(&id) {
                self.stop_one(id, fade_out, cx);
            }
        }
        self.stop_one(cue, fade_out, cx);
    }

    pub(super) fn reset_tree(&mut self, cue: CueId, cx: &mut Ctx) {
        for id in self.inside(cue, cx).into_iter().rev() {
            if self.slots.contains_key(&id) {
                self.reset_one(id, cx);
            }
        }
        self.reset_one(cue, cx);
    }

    pub(super) fn pause_tree(&mut self, cue: CueId, cx: &mut Ctx) {
        self.pause_one(cue, cx);
        for id in self.inside(cue, cx) {
            self.pause_one(id, cx);
        }
    }

    /// Resumes the cue and every paused descendant as of `at`, then ends groups whose last
    /// child left while they were paused.
    pub(super) fn resume_tree(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        let inside = self.inside(cue, cx);
        self.resume_one(cue, at, cx);
        for id in &inside {
            self.resume_one(*id, at, cx);
        }
        for id in inside.iter().rev().chain(std::iter::once(&cue)) {
            // A playlist owed a move-on while paused: its next child starts at the resume
            // sample (as if the playlist had moved on just as it was paused).
            let advance = match self.slots.get_mut(id) {
                Some(Slot::Active(Run {
                    group: Some(g),
                    paused_at: None,
                    ..
                })) if g.advance => {
                    g.advance = false;
                    true
                }
                _ => false,
            };
            if advance {
                self.playlist_next(*id, at, cx);
                // A child started: the result recorded during the pause no longer applies.
                // None started (that was the last child): the group ends with it below.
                if self.any_busy_child(cx.ws, *id)
                    && let Some(Slot::Active(Run { group: Some(g), .. })) = self.slots.get_mut(id)
                {
                    g.pending = None;
                }
            }
            let pending = match self.slots.get(id) {
                Some(Slot::Active(Run {
                    group: Some(g),
                    phase: Phase::Action { .. },
                    paused_at: None,
                    ..
                })) => g.pending,
                _ => None,
            };
            if let Some(exit) = pending
                && !self.any_busy_child(cx.ws, *id)
            {
                self.resolve_group(*id, exit, at, cx);
            }
        }
    }

    /// Loads the cue; a group also loads the children its action will start first (timeline:
    /// all; start-first modes and an unshuffled playlist: the first), at the time left after the
    /// group's pre-wait. Iterative over nested groups; each cue is loaded at most once, so a
    /// duplicate id cannot make it loop.
    pub(super) fn load_tree(&mut self, cue: CueId, time: f64, cx: &mut Ctx) {
        let mut work = vec![(cue, time)];
        let mut done = HashSet::new();
        while let Some((id, time)) = work.pop() {
            if !done.insert(id) {
                continue;
            }
            self.load(id, time, cx);
            if !matches!(self.slots.get(&id), Some(Slot::Loaded(_))) {
                continue;
            }
            let Some(model) = cx.ws.find_cue(id) else {
                continue;
            };
            let CueKind::Group { mode, children } = &model.kind else {
                continue;
            };
            let inner = (time - model.pre_wait).max(0.0);
            let first = match mode {
                GroupMode::Timeline => children.len(),
                GroupMode::StartFirstAndEnter
                | GroupMode::StartFirstAndGoToNext
                | GroupMode::Playlist { shuffle: false, .. } => 1,
                GroupMode::Playlist { shuffle: true, .. } | GroupMode::StartRandom => 0,
            };
            work.extend(children.iter().take(first).rev().map(|c| (c.id, inner)));
        }
    }
}
