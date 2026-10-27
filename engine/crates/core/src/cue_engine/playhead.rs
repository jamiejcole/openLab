// SPDX-License-Identifier: GPL-3.0-or-later

//! A cue list's playhead (PB-05, CL-06): the cue standing by, or `None` past the end.
//!
//! As in QLab 5 the playhead can stand on any cue of its list, at any depth. Its positions, in
//! list order, are the cues depth-first (a group, then its children).

use std::collections::HashSet;

use crate::cue::{ContinueMode, Cue, CueKind, GroupMode};
use crate::model::CueId;

#[derive(Clone, Debug)]
pub(super) struct Playhead {
    pub cue: Option<CueId>,
    /// The list's positions when last synced, to find where a deleted cue stood.
    order: Vec<CueId>,
}

impl Playhead {
    /// A new list's playhead stands on its first cue.
    pub fn new(cues: &[Cue]) -> Self {
        let order = positions(cues);
        Self {
            cue: order.first().copied(),
            order,
        }
    }

    /// Follows edits to the list. A deleted playhead cue gives way to the first position that
    /// came after it and still exists (none: past the end); a list that was empty and gains cues stands on its
    /// first cue. Returns `(the list's cues changed, the playhead moved)`.
    pub fn sync(&mut self, cues: &[Cue]) -> (bool, bool) {
        let order = positions(cues);
        if order == self.order {
            return (false, false);
        }
        let before = self.cue;
        self.cue = match self.cue {
            Some(id) if order.contains(&id) => Some(id),
            Some(id) => {
                let present: HashSet<CueId> = order.iter().copied().collect();
                self.order
                    .iter()
                    .skip_while(|o| **o != id)
                    .skip(1)
                    .find(|o| present.contains(o))
                    .copied()
            }
            None if self.order.is_empty() => order.first().copied(),
            None => None,
        };
        self.order = order;
        (true, before != self.cue)
    }

    /// One position down; from the last to past the end, where it stays. Only "start first and
    /// enter" groups are stepped into (QLab 5): any other group is passed over as a unit, unless
    /// the playhead already stands inside it (see [`stepping_order`]).
    pub fn next(&mut self, cues: &[Cue]) -> bool {
        let order = stepping_order(cues, self.cue);
        let next = self
            .index(&order)
            .and_then(|i| order.get(i.saturating_add(1)))
            .copied();
        self.set(next)
    }

    /// One position up; from past the end to the last; the first stays. Groups as for `next`, so
    /// stepping back onto a group that is not entered stands on the group itself.
    pub fn previous(&mut self, cues: &[Cue]) -> bool {
        let order = stepping_order(cues, self.cue);
        let previous = match self.index(&order) {
            Some(i) => order.get(i.saturating_sub(1)),
            None => order.last(),
        };
        self.set(previous.copied())
    }

    pub fn set(&mut self, cue: Option<CueId>) -> bool {
        let moved = self.cue != cue;
        self.cue = cue;
        moved
    }

    fn index(&self, order: &[CueId]) -> Option<usize> {
        let id = self.cue?;
        order.iter().position(|c| *c == id)
    }
}

/// The cues the playhead can stand on, in list order: every cue, depth-first. Iterative, so
/// nesting depth cannot overflow the stack.
pub(super) fn position_cues(cues: &[Cue]) -> Vec<&Cue> {
    let mut out = Vec::new();
    let mut stack: Vec<std::slice::Iter<'_, Cue>> = vec![cues.iter()];
    while let Some(level) = stack.last_mut() {
        let Some(cue) = level.next() else {
            stack.pop();
            continue;
        };
        out.push(cue);
        stack.push(cue.children().iter());
    }
    out
}

/// The positions `PlayheadNext`/`PlayheadPrevious` step through from `from`: depth-first, but
/// a group's children are included only for a "start first and enter" group (QLab 5 enters only
/// those) or a group around `from` (the playhead was put inside it explicitly, so stepping goes on
/// among its siblings and then leaves the group, as GO from a child does). Iterative.
fn stepping_order(cues: &[Cue], from: Option<CueId>) -> Vec<CueId> {
    let around: HashSet<CueId> = from
        .and_then(|id| locate(cues, id))
        .map(|path| {
            let outer = path.len().saturating_sub(1);
            path.iter()
                .take(outer)
                .filter_map(|&(level, i)| level.get(i).map(|g| g.id))
                .collect()
        })
        .unwrap_or_default();
    let mut out = Vec::new();
    let mut stack: Vec<std::slice::Iter<'_, Cue>> = vec![cues.iter()];
    while let Some(level) = stack.last_mut() {
        let Some(cue) = level.next() else {
            stack.pop();
            continue;
        };
        out.push(cue.id);
        let entered = matches!(
            cue.kind,
            CueKind::Group {
                mode: GroupMode::StartFirstAndEnter,
                ..
            }
        ) || around.contains(&cue.id);
        if entered {
            stack.push(cue.children().iter());
        }
    }
    out
}

pub(super) fn positions(cues: &[Cue]) -> Vec<CueId> {
    position_cues(cues).into_iter().map(|c| c.id).collect()
}

/// Where a cue is in a list: each level's cues and its index there, from the top level down to
/// the cue's own container. Depth-first and iterative; the first match wins (the same occurrence
/// as in the positions).
pub(super) fn locate(cues: &[Cue], id: CueId) -> Option<Vec<(&[Cue], usize)>> {
    let mut path: Vec<(&[Cue], usize)> = vec![(cues, 0)];
    loop {
        let (level, index) = *path.last()?;
        match level.get(index) {
            None => {
                path.pop();
                if let Some(last) = path.last_mut() {
                    last.1 = last.1.saturating_add(1);
                }
            }
            Some(cue) if cue.id == id => return Some(path),
            Some(cue) => {
                let children = cue.children();
                if children.is_empty() {
                    if let Some(last) = path.last_mut() {
                        last.1 = last.1.saturating_add(1);
                    }
                } else {
                    path.push((children, 0));
                }
            }
        }
    }
}

/// Where the playhead goes after a GO on the cue at `path` (its continue chain, as the model
/// stands, is skipped):
/// - an armed (as `armed` says) "start first and enter" group with children: into the group, to
///   the cue after its first child's continue chain;
/// - a child of a playlist group: out of the group (QLab 5: GO on a child of a stopped playlist
///   plays on through the rest of its children);
/// - otherwise the cue after the cue's continue chain among its siblings (inside timeline groups
///   the children's continue modes are ignored, so the next sibling); past the last sibling, out
///   of the group (and so on up); past the end of the list, `None`. A disarmed group is skipped
///   as a unit like any disarmed cue.
///
/// Leaving a group, the playhead goes to the cue after the group's own continue chain only if
/// the group's continue fires or has fired (`continues`: it has a run, or its last run fired
/// its continue); a group with no run fires none, so the playhead stands on the cue right
/// after it and its continue targets are not lost.
///
/// `broken` says which cues have static problems: a broken Devamp cue starts no next cue (its
/// own continue mode applies instead). A broken cue's auto-continue and auto-follow both still
/// fire (QLab 5: it is skipped), so they continue the chain.
pub(super) fn after_go(
    path: &[(&[Cue], usize)],
    armed: impl Fn(&Cue) -> bool,
    broken: impl Fn(&Cue) -> bool,
    continues: impl Fn(&Cue) -> bool,
) -> Option<CueId> {
    let &(level, index) = path.last()?;
    if let Some(
        group @ Cue {
            kind:
                CueKind::Group {
                    mode: GroupMode::StartFirstAndEnter,
                    children,
                },
            ..
        },
    ) = level.get(index)
        && armed(group)
        && let Some(c) =
            children.get(chain_end(children, 0, true, &armed, &broken).saturating_add(1))
    {
        return Some(c.id);
    }
    (0..path.len()).rev().find_map(|depth| {
        let &(level, index) = path.get(depth)?;
        // The group holding this level decides whether its children chain.
        let mode = match depth.checked_sub(1).and_then(|d| path.get(d)) {
            None => None,
            Some(&(outer, i)) => match outer.get(i).map(|g| &g.kind) {
                Some(CueKind::Group { mode, .. }) => Some(*mode),
                _ => None,
            },
        };
        // Above the GOed cue's own level, `index` is a group the playhead is leaving.
        let leaving = depth.saturating_add(1) < path.len();
        let last = match mode {
            _ if leaving && level.get(index).is_some_and(|g| !continues(g)) => index,
            // The rest of a playlist plays on from a child GOed on its own.
            Some(GroupMode::Playlist { .. }) => level.len().saturating_sub(1),
            Some(GroupMode::Timeline) => index,
            None
            | Some(
                GroupMode::StartFirstAndEnter
                | GroupMode::StartFirstAndGoToNext
                | GroupMode::StartRandom,
            ) => chain_end(level, index, true, &armed, &broken),
        };
        level.get(last.saturating_add(1)).map(|c| c.id)
    })
}

/// Index of the last cue of the continue chain that starts at `index` (just `index` when the
/// level's continue modes do not chain). An armed Devamp cue with "start next cue" ends the
/// chain: the playhead stands on the cue it will start when the loop is left (QLab 5), whatever
/// its own continue mode, which that replaces.
fn chain_end(
    level: &[Cue],
    index: usize,
    chains: bool,
    armed: &impl Fn(&Cue) -> bool,
    broken: &impl Fn(&Cue) -> bool,
) -> usize {
    let mut last = index;
    while chains
        && let Some(c) = level.get(last)
        && !super::starts_next_on_devamp(c, armed(c) && !broken(c))
        && matches!(
            c.continue_mode,
            ContinueMode::AutoContinue | ContinueMode::AutoFollow
        )
        && last.saturating_add(1) < level.len()
    {
        last += 1;
    }
    last
}
