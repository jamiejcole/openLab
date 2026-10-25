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
    /// first cue. Returns whether the playhead moved.
    pub fn sync(&mut self, cues: &[Cue]) -> bool {
        let order = positions(cues);
        if order == self.order {
            return false;
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
        before != self.cue
    }

    /// One position down; from the last to past the end, where it stays.
    pub fn next(&mut self, cues: &[Cue]) -> bool {
        let order = positions(cues);
        let next = self
            .index(&order)
            .and_then(|i| order.get(i.saturating_add(1)))
            .copied();
        self.set(next)
    }

    /// One position up; from past the end to the last; the first stays.
    pub fn previous(&mut self, cues: &[Cue]) -> bool {
        let order = positions(cues);
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
/// - an armed (as `armed` says) "start first and enter" group with children: into the group, to the cue after its
///   first child's continue chain;
/// - otherwise the cue after the cue's continue chain among its siblings (inside timeline,
///   playlist and random groups the children's continue modes are ignored, so the next
///   sibling); past the last sibling, out of the group, to the cue after the group's own
///   continue chain (and so on up); past the end of the list, `None`. A disarmed group is skipped
///   as a unit like any disarmed cue.
pub(super) fn after_go(path: &[(&[Cue], usize)], armed: impl Fn(&Cue) -> bool) -> Option<CueId> {
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
        && let Some(c) = children.get(chain_end(children, 0, true).saturating_add(1))
    {
        return Some(c.id);
    }
    (0..path.len()).rev().find_map(|depth| {
        let &(level, index) = path.get(depth)?;
        // The group holding this level decides whether its children chain.
        let chains = match depth.checked_sub(1).and_then(|d| path.get(d)) {
            None => true,
            Some(&(outer, i)) => matches!(
                outer.get(i).map(|g| &g.kind),
                Some(CueKind::Group {
                    mode: GroupMode::StartFirstAndEnter | GroupMode::StartFirstAndGoToNext,
                    ..
                })
            ),
        };
        level
            .get(chain_end(level, index, chains).saturating_add(1))
            .map(|c| c.id)
    })
}

/// Index of the last cue of the continue chain that starts at `index` (just `index` when the
/// level's continue modes do not chain).
fn chain_end(level: &[Cue], index: usize, chains: bool) -> usize {
    let mut last = index;
    while chains
        && let Some(c) = level.get(last)
        && c.continue_mode != ContinueMode::DoNotContinue
        && last.saturating_add(1) < level.len()
    {
        last += 1;
    }
    last
}
