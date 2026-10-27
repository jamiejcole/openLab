// SPDX-License-Identifier: GPL-3.0-or-later

//! Group cues (CT-22): how a group's action runs its children, when it completes, and how
//! operations on a group reach its descendants. Rules: `docs/cue-engine.md`, "Groups".

use std::collections::{BTreeSet, HashSet};

use crate::cue::{ContinueMode, Cue, CueKind, FadeSpec, GroupMode};
use crate::model::{CueId, Workspace};
use crate::tree;

use super::run::{Exit, GroupRun, Halt, Phase, PlaylistRun, Run, Slot};
use super::time::SampleTime;
use super::types::EngineWarning;
use super::{Ctx, CueEngine};

/// A random group's round-robin memory (engine only: not saved, gone with the engine).
#[derive(Clone, Debug, Default)]
pub(super) struct RandomCycle {
    /// Children picked in the current cycle.
    played: BTreeSet<CueId>,
    /// The child picked last, so a new cycle does not open with it.
    last: Option<CueId>,
}

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

/// Whether a cue's continue mode triggers its next sibling: always at the top level, in the
/// "start first" modes and in random groups (QLab 5: the chosen child's own continue fires);
/// timeline and playlist groups run their children themselves and ignore the children's
/// continue modes.
pub(super) fn chains_siblings(ws: &Workspace, cue: CueId) -> bool {
    let parent = ancestors(ws, cue).first().and_then(|g| ws.find_cue(*g));
    match parent.and_then(group_mode) {
        None
        | Some(
            GroupMode::StartFirstAndEnter
            | GroupMode::StartFirstAndGoToNext
            | GroupMode::StartRandom,
        ) => true,
        Some(GroupMode::Timeline | GroupMode::Playlist { .. }) => false,
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
    /// busy (no children, all broken, chain limit), the action completes at once. A child that is
    /// broken when the group gets to it is skipped and does not keep the group from
    /// auto-following (QLab 5); only a child that breaks while it runs does (`child_left`).
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
        // Loaded to a time at or past its nominal span, a playlist or random group has nothing
        // left to play: its action ends at once and its post-wait counts from the load time
        // (M4.11b). A span that is unknown (unprobed audio) or endless (a looping playlist)
        // clamps nothing.
        let offset = match self.slots.get(&group) {
            Some(Slot::Active(run)) => run.action_offset,
            _ => 0,
        };
        let spent = |span: Option<SampleTime>| offset > 0 && span.is_some_and(|s| offset >= s);
        match mode {
            GroupMode::Timeline => {
                for child in children {
                    self.start(child.id, at, cx);
                }
            }
            GroupMode::StartFirstAndEnter | GroupMode::StartFirstAndGoToNext => {
                if let Some(first) = children.first() {
                    self.start(first.id, at, cx);
                }
            }
            GroupMode::Playlist { loop_, .. } => {
                let span = (!*loop_)
                    .then(|| {
                        children.iter().try_fold(0, |acc: SampleTime, c| {
                            Some(acc.saturating_add(self.nominal_span(c, 1, cx)?))
                        })
                    })
                    .flatten();
                if !spent(span) {
                    self.playlist_next(group, at, cx);
                }
            }
            GroupMode::StartRandom => {
                if let Some(child) = self.pick_random(group, children) {
                    let span = children
                        .iter()
                        .find(|c| c.id == child)
                        .and_then(|c| self.nominal_span(c, 1, cx));
                    if !spent(span) {
                        self.start(child, at, cx);
                    }
                }
            }
        }
        if !self.any_busy_child(ws, group) {
            self.resolve_group(group, Exit::Completed, at, cx);
        }
    }

    /// One child, round-robin as in QLab 5: every playable child (armed, without static
    /// problems, not already busy) plays once before any plays again. Candidates are the
    /// playable children not yet picked in the current cycle, as the group stands now (a child
    /// added mid-cycle joins it, a removed or disarmed one drops out); when none is left a new
    /// cycle starts, which never opens with the child picked last when there is another. With
    /// no playable child at all the same rule runs over every child, so one child is always
    /// picked (its start then reports it disarmed or broken).
    fn pick_random(&mut self, group: CueId, children: &[Cue]) -> Option<CueId> {
        let playable: Vec<CueId> = children
            .iter()
            .filter(|c| self.is_armed(c) && !self.has_problems(c.id) && !self.busy(c.id))
            .map(|c| c.id)
            .collect();
        let pool = if playable.is_empty() {
            children.iter().map(|c| c.id).collect()
        } else {
            playable
        };
        let cycle = self.random_cycles.entry(group).or_default();
        // Children removed since are forgotten, so the memory stays as small as the group.
        cycle
            .played
            .retain(|id| children.iter().any(|c| c.id == *id));
        let mut candidates: Vec<CueId> = pool
            .iter()
            .copied()
            .filter(|id| !cycle.played.contains(id))
            .collect();
        if candidates.is_empty() {
            cycle.played.clear();
            candidates = pool
                .iter()
                .copied()
                .filter(|id| pool.len() < 2 || Some(*id) != cycle.last)
                .collect();
        }
        let pick = *candidates.get(self.rng.below(candidates.len()))?;
        cycle.played.insert(pick);
        cycle.last = Some(pick);
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
            })) if !g.stopping && g.devamp.is_none() && g.halt.is_none() => g.playlist.take(),
            _ => return,
        };
        // A pass began in this call: ending it again means a whole pass in which nothing started.
        let mut wrapped = false;
        let mut attempts = ids.len().saturating_mul(2).saturating_add(2);
        let mut playing = None;
        while attempts > 0 {
            attempts -= 1;
            // Leave a few starts for the group's own continue (see `PLAYLIST_RESERVE`).
            if cx.budget <= super::PLAYLIST_RESERVE {
                cx.warn(EngineWarning::ChainLimit {
                    cue: group,
                    limit: cx.limit,
                });
                break;
            }
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
            self.start(child, at, cx);
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

    /// Second trigger "play next" / "play previous" (`back`) on a playlist group playing a child
    /// (in its action, not stopping or devamped; a paused one too): the child playing
    /// is hard-stopped and the one after (before) it starts on `at`, wrapping when the playlist
    /// loops. Without loop, "next" after the last child completes the group (as if that child
    /// had ended) and "previous" on the first child stops the group (QLab check S4). Shuffled, the
    /// pass's order is followed (a new pass after its end, as usual; "previous" from its first
    /// child wraps to its last with loop, and stops the group without). A paused playlist
    /// resumes on the new child. The new child's
    /// start is charged to the chain budget like any playlist move.
    pub(super) fn playlist_skip(&mut self, group: CueId, back: bool, at: SampleTime, cx: &mut Ctx) {
        let state = self.state(group);
        let ws = cx.ws;
        let Some(CueKind::Group {
            mode: GroupMode::Playlist { loop_, .. },
            children,
        }) = ws.find_cue(group).map(|g| &g.kind)
        else {
            return cx.warn(EngineWarning::TriggerIgnored { cue: group, state });
        };
        let ids: Vec<CueId> = children.iter().map(|c| c.id).collect();
        let (current, paused) = match self.slots.get(&group) {
            Some(Slot::Active(Run {
                group: Some(g),
                phase: Phase::Action { .. },
                paused_at,
                ..
            })) if !g.stopping && g.devamp.is_none() && g.skipped != Some(at) => {
                match &g.playlist {
                    Some(p) => (p.current, paused_at.is_some()),
                    None => return cx.warn(EngineWarning::TriggerIgnored { cue: group, state }),
                }
            }
            _ => return cx.warn(EngineWarning::TriggerIgnored { cue: group, state }),
        };
        // The child playing is stopped only if the next one can be started (see
        // `PLAYLIST_RESERVE`), so a cut chain leaves the playlist as it was.
        if !self.budget_left(cx) || cx.budget <= super::PLAYLIST_RESERVE {
            return cx.warn(EngineWarning::ChainLimit {
                cue: group,
                limit: cx.limit,
            });
        }
        let loop_ = *loop_;
        // Children a start would refuse at once (static problems, disarmed, broken): going back
        // passes over them, as going forward does.
        let playable: Vec<CueId> = children
            .iter()
            .filter(|c| {
                self.is_armed(c)
                    && !self.has_problems(c.id)
                    && !matches!(self.slots.get(&c.id), Some(Slot::Broken(_)))
            })
            .map(|c| c.id)
            .collect();
        if let Some(Slot::Active(Run {
            group: Some(g),
            phase,
            ..
        })) = self.slots.get_mut(&group)
        {
            g.skipped = Some(at);
            // The operator moves a halted playlist on: it plays on from here, and its action
            // lasts while its children do again.
            if g.halt.take().is_some()
                && let Phase::Action { start, .. } = *phase
            {
                *phase = Phase::Action { start, end: None };
            }
        }
        self.as_of(at, cx, |e, cx| {
            // "Previous" with no child before the current one (and no loop to wrap round) stops
            // the playlist (QLab check S4, M2.28).
            if back && !e.playlist_rewind(group, &ids, &playable, loop_) {
                return e.stop_tree(group, None, cx);
            }
            if let Some((child, _)) = current
                && matches!(e.slots.get(&child), Some(Slot::Active(_)))
            {
                e.stop_tree(child, None, cx);
            }
            if paused {
                // Whatever the paused playlist owed is replaced by the move asked for here.
                if let Some(Slot::Active(Run { group: Some(g), .. })) = e.slots.get_mut(&group) {
                    g.advance = false;
                }
                e.resume_tree(group, at, cx);
            }
            e.playlist_next(group, at, cx);
            if !e.any_busy_child(cx.ws, group) {
                let now = cx.now;
                e.resolve_group(group, Exit::Completed, now, cx);
            }
        });
    }

    /// Sets a playlist's position so that its next move starts the child before the current one
    /// (see `playlist_skip`). Returns `false`, changing nothing, when there is none: no playable
    /// child before the current one and no loop to wrap round.
    fn playlist_rewind(
        &mut self,
        group: CueId,
        ids: &[CueId],
        playable: &[CueId],
        loop_: bool,
    ) -> bool {
        let Some(Slot::Active(Run { group: Some(g), .. })) = self.slots.get_mut(&group) else {
            return true;
        };
        let Some(p) = g.playlist.as_mut() else {
            return true;
        };
        if p.order.is_empty() {
            // A current child removed from the group: `index` is where it stood.
            let here = p
                .current
                .and_then(|(c, _)| ids.iter().position(|i| *i == c))
                .unwrap_or(p.index);
            let valid = |i: &usize| ids.get(*i).is_some_and(|id| playable.contains(id));
            let len = ids.len();
            // The playable child before; with loop, wrapping to the last; else none (the
            // playlist stops). With loop and none playable: where it was.
            let before = (0..here).rev().find(valid);
            if before.is_none() && !loop_ {
                return false;
            }
            p.index = before
                .or_else(|| (0..len).rev().find(valid))
                .unwrap_or(here.min(len.saturating_sub(1)));
        } else {
            let order = &p.order;
            let valid = |pos: &usize| order.get(*pos).is_some_and(|id| playable.contains(id));
            let len = order.len();
            let before = (0..p.pos).rev().find(valid);
            if before.is_none() && !loop_ {
                return false;
            }
            p.pos = before
                .or_else(|| (0..len).rev().find(valid))
                .unwrap_or(p.pos);
        }
        p.current = None;
        true
    }

    /// A restarted cue (run `seq` before) that was its playlist's current child stays the current
    /// child with its new run, so the playlist moves on when the new run ends.
    pub(super) fn keep_playlist_place(&mut self, cue: CueId, seq: u64, cx: &Ctx) {
        let Some(Slot::Active(run)) = self.slots.get(&cue) else {
            return;
        };
        let fresh = run.seq;
        if let Some(&parent) = ancestors(cx.ws, cue).first()
            && let Some(Slot::Active(Run {
                group: Some(GroupRun {
                    playlist: Some(p), ..
                }),
                ..
            })) = self.slots.get_mut(&parent)
            && p.current == Some((cue, seq))
        {
            p.current = Some((cue, fresh));
        }
    }

    /// The action of `cue` (run `seq`) completed, or it broke, on `at`: a playlist around it
    /// moves on to its next child; in a playlist that is not running, the child after it starts
    /// (`follow_through`).
    pub(super) fn child_action_done(&mut self, cue: CueId, seq: u64, at: SampleTime, cx: &mut Ctx) {
        let Some(&parent) = ancestors(cx.ws, cue).first() else {
            return;
        };
        if !matches!(self.slots.get(&parent), Some(Slot::Active(_))) {
            return self.follow_through(parent, cue, at, cx);
        }
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

    /// `cue` (run `seq`), the current child of a running playlist, broke while playing (QLab
    /// check Q2): the playlist halts. It starts no further child, its action ends on `end` (the
    /// child's nominal end; `None`: unknown, so it ends now), and its auto-follow is kept, timed
    /// by the nominal pre-wait and length of every child it would still have played, in its
    /// order (a shuffled pass's order); none if one of them has no known end or the playlist
    /// loops. A devamped or stopping playlist is left alone (it was ending anyway).
    pub(super) fn halt_playlist(
        &mut self,
        cue: CueId,
        seq: u64,
        end: Option<SampleTime>,
        cx: &mut Ctx,
    ) {
        let Some(&parent) = ancestors(cx.ws, cue).first() else {
            return;
        };
        let ws = cx.ws;
        let Some(CueKind::Group {
            mode: GroupMode::Playlist { loop_, .. },
            children,
        }) = ws.find_cue(parent).map(|g| &g.kind)
        else {
            return;
        };
        let Some(Slot::Active(Run {
            group: Some(g),
            phase: Phase::Action { .. },
            ..
        })) = self.slots.get(&parent)
        else {
            return;
        };
        let Some(p) = g.playlist.as_ref() else {
            return;
        };
        if g.stopping || g.devamp.is_some() || g.halt.is_some() || p.current != Some((cue, seq)) {
            return;
        }
        let remaining: Vec<&Cue> = if p.order.is_empty() {
            // After the broken child as the group stands now (where it stood, if it has left).
            let after = children
                .iter()
                .position(|c| c.id == cue)
                .map_or(p.index, |i| i.saturating_add(1));
            children.iter().skip(after).collect()
        } else {
            p.order
                .iter()
                .skip(p.pos.saturating_add(1))
                .filter_map(|id| children.iter().find(|c| c.id == *id))
                .collect()
        };
        let rest = if *loop_ || end.is_none() {
            None
        } else {
            remaining.iter().try_fold(0, |acc: SampleTime, c| {
                Some(acc.saturating_add(self.nominal_span(c, 1, cx)?))
            })
        };
        if let Some(Slot::Active(run)) = self.slots.get_mut(&parent)
            && let Some(g) = run.group.as_mut()
            && let Phase::Action { start, .. } = run.phase
        {
            g.halt = Some(Halt { rest });
            run.phase = Phase::Action {
                start,
                end: end.map(|e| e.max(start)),
            };
        }
    }

    /// The nominal end of `run`, the runtime of the Audio cue `cue`: its known end, else its
    /// nominal length (`Input::CueLengths`) from its action start; `None` if neither is known.
    pub(super) fn nominal_end(&self, cue: CueId, run: &Run) -> Option<SampleTime> {
        let length = || {
            self.lengths
                .get(&cue)
                .map(|l| l.saturating_sub(run.action_offset))
        };
        match run.phase {
            Phase::Action { end: Some(end), .. } => Some(end),
            Phase::Action { start, end: None } => length().map(|l| start.saturating_add(l)),
            Phase::PreWait { until } => length().map(|l| until.saturating_add(l)),
            Phase::Done => None,
        }
    }

    /// How long `cue` would take, pre-wait and action, if a playlist started it now with its
    /// nominal timing; `None` if that is unknown or endless. A cue a start would pass over at
    /// once (broken at run time) takes nothing; a disarmed one or one with static problems
    /// takes its pre-wait only. `depth` bounds the walk into nested groups.
    fn nominal_span(&self, cue: &Cue, depth: usize, cx: &Ctx) -> Option<SampleTime> {
        if matches!(self.slots.get(&cue.id), Some(Slot::Broken(_))) {
            return Some(0);
        }
        let pre = cx.secs(cue.pre_wait, &self.config);
        let action = if !self.is_armed(cue) || self.has_problems(cue.id) {
            0
        } else {
            match &cue.kind {
                CueKind::Audio { .. } => self.lengths.get(&cue.id).copied()?,
                CueKind::Wait { duration } | CueKind::Fade { duration, .. } => {
                    cx.secs(*duration, &self.config)
                }
                CueKind::Group { mode, children } => {
                    if depth > crate::validate::MAX_GROUP_DEPTH {
                        return None;
                    }
                    let span = |c: &Cue| self.nominal_span(c, depth.saturating_add(1), cx);
                    match mode {
                        GroupMode::Timeline => children
                            .iter()
                            .try_fold(0, |acc: SampleTime, c| Some(acc.max(span(c)?)))?,
                        GroupMode::Playlist { loop_, .. } if !*loop_ || children.is_empty() => {
                            children.iter().try_fold(0, |acc: SampleTime, c| {
                                Some(acc.saturating_add(span(c)?))
                            })?
                        }
                        // Looping, or children chaining by their own continues: no known end.
                        GroupMode::Playlist { .. }
                        | GroupMode::StartFirstAndEnter
                        | GroupMode::StartFirstAndGoToNext
                        | GroupMode::StartRandom => return None,
                    }
                }
                CueKind::Memo
                | CueKind::Start { .. }
                | CueKind::Stop { .. }
                | CueKind::Pause { .. }
                | CueKind::Load { .. }
                | CueKind::Reset { .. }
                | CueKind::Devamp { .. }
                | CueKind::GoTo { .. }
                | CueKind::Arm { .. }
                | CueKind::Disarm { .. }
                | CueKind::Network { .. } => 0,
            }
        };
        Some(pre.saturating_add(action))
    }

    /// `cue`, a child of `group`, completed (or broke) on `at` while its group has no run (it
    /// was started on its own): if the group is a playlist, the next child that can start starts
    /// on `at`, as QLab 5 plays on through the rest of a stopped playlist from a child GOed on its
    /// own. The children are taken in list order (shuffle aside, QLab check Q4); after the last
    /// one a looping playlist wraps to its first child and keeps going until stopped (Q4), a
    /// non-looping one ends. The group itself does not run, so it fires no continue of its own.
    /// Children a start refuses (broken at run time and not reset) are passed over, at most one
    /// round of them per call; each start is charged to the chain budget like any continue.
    fn follow_through(&mut self, group: CueId, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        let ws = cx.ws;
        let Some(CueKind::Group {
            mode: GroupMode::Playlist { loop_, .. },
            children,
        }) = ws.find_cue(group).map(|g| &g.kind)
        else {
            return;
        };
        let Some(index) = children.iter().position(|c| c.id == cue) else {
            return;
        };
        let after = index.saturating_add(1);
        // With loop: round to the start and on up to the child that just ended (which plays
        // again if nothing else can).
        let wrapped = children.iter().take(if *loop_ { after } else { 0 });
        for next in children.iter().skip(after).chain(wrapped) {
            if next.id == group {
                continue;
            }
            self.start(next.id, at, cx);
            if matches!(self.slots.get(&next.id), Some(Slot::Active(_))) || cx.budget == 0 {
                return;
            }
        }
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
        // A halted playlist's action lasts until its broken child's nominal end.
        if g.halt.is_some()
            && let Phase::Action { end: Some(end), .. } = run.phase
            && at < end
        {
            return;
        }
        // Samples from now to the group's auto-follow. A halted playlist keeps its auto-follow,
        // timed as if its remaining children had played (QLab check Q2); any other group whose
        // child broke while playing does not follow (rule 2).
        let follow = match g.halt {
            Some(halt) => halt.rest.filter(|_| run.follow),
            None => (run.follow && !g.failed).then_some(0),
        };
        // A devamped playlist that asked for it starts the cue after the Devamp cue now.
        let devamp_next = g.devamp.take().filter(|(_, next)| *next).map(|(by, _)| by);
        run.phase = Phase::Done;
        run.follow = false;
        // A later follow waits as an auto-continue's post-wait does (the group shows
        // post-waiting), so Stop, Pause, Reset and Panic act on it as on any post-wait.
        if let Some(rest) = follow.filter(|r| *r > 0) {
            run.post_wait = Some(at.saturating_add(rest));
        }
        let follow = follow == Some(0);
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
                // A halted playlist waiting for its broken child's nominal end has an end of its
                // own and no busy child: it is not stranded.
                Slot::Active(Run {
                    group: Some(_),
                    phase: Phase::Action { end: None, .. },
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

    /// A cue was started on its own (not by its group) on `at`. As in QLab 5 that does not make
    /// the groups around it run: an idle group stays idle (a playlist's other children follow it,
    /// see `follow_through`), and a group with a run of its own keeps it. The exception is a group
    /// fading out in a panic, which would otherwise keep reporting `stopping` until the new child
    /// ends: it stops waiting and ends now (its other children finish their own panic fades), and
    /// so do the groups around it fading out in the panic.
    pub(super) fn release_panicking_parents(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        for group in ancestors(cx.ws, cue) {
            let panicking = matches!(
                self.slots.get(&group),
                Some(Slot::Active(run)) if run.panicked && run.group.is_some()
            );
            if !panicking {
                return;
            }
            let before = self.state(group);
            self.remove_slot(group, at, Exit::Stopped, cx);
            self.report(group, before, at, cx);
        }
    }

    // ----- operations on a cue and everything inside it (CUE-05) -----

    /// The cue's descendants, if it is a group in the workspace.
    pub(super) fn inside(&self, cue: CueId, cx: &Ctx) -> Vec<CueId> {
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
