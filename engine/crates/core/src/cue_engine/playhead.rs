// SPDX-License-Identifier: GPL-3.0-or-later

//! A cue list's playhead (PB-05, CL-06): the cue standing by, or `None` past the end.
//!
//! As in QLab 5 the playhead can stand on any cue of its list, at any depth. Its positions, in
//! list order, are the cues depth-first (a group, then its children).

use std::collections::{BTreeSet, HashSet};

use crate::cue::{ContinueMode, Cue, CueKind, GroupMode};
use crate::cue_index::{CueContainer, CueIndex};
use crate::model::{CueId, CueListId};

#[derive(Clone, Debug)]
pub(super) struct Playhead {
    pub cue: Option<CueId>,
    /// The list's positions when last synced, to find where a deleted cue stood. `None` while
    /// the engine's cue index holds them: it is then in step with what the playhead last saw,
    /// and an edit is followed from its footprint (M4.12c, see `CueEngine::sync_playheads`).
    order: Option<Vec<CueId>>,
    /// What the edit being followed did around the playhead, read from the cue index before
    /// the index followed the edit.
    pub followed: Option<Followed>,
    /// The groups around the playhead cue, outermost first, as of the last time the engine
    /// looked (after each move and each edit of the list): entering a group is a group in
    /// this set now that was not before, whether the playhead moved or the list was edited
    /// around it (see `CueEngine::forget_continues`).
    pub around: Vec<CueId>,
}

/// What an edit did around a playhead whose positions the cue index holds, read from the index
/// before it follows the edit: enough to place the playhead afterwards as [`Playhead::follow`]
/// would from the whole list (M4.12c).
#[derive(Clone, Debug)]
pub(super) struct Followed {
    /// The list had no cues.
    pub was_empty: bool,
    /// The positions after the playhead cue that the edit added, removed or moved (or that sit
    /// in a group it did), in order, when the playhead cue or a group around it was one of
    /// those: the cue may be gone, and these may be gone too.
    pub maybe: Vec<CueId>,
    /// The first position after those that the edit did not touch: it is still in the list.
    pub sure: Option<CueId>,
}

impl Playhead {
    /// A new list's playhead stands on its first cue.
    pub fn new(cues: &[Cue]) -> Self {
        let order = positions(cues);
        Self {
            cue: order.first().copied(),
            order: Some(order),
            followed: None,
            around: Vec::new(),
        }
    }

    /// The positions the playhead keeps itself (`None`: the cue index holds them).
    pub fn take_order(&mut self) -> Option<Vec<CueId>> {
        self.order.take()
    }

    pub fn keeps_order(&self) -> bool {
        self.order.is_some()
    }

    /// Keeps `order` as the list's positions (the cue index cannot be trusted to hold them).
    pub fn keep_order(&mut self, order: Vec<CueId>) {
        self.order = Some(order);
    }

    /// Follows edits to the list, from `old` (its positions when the playhead last looked) to
    /// `order` (now). A deleted playhead cue gives way to the first position that came after it
    /// and still exists (none: past the end); a list that was empty and gains cues stands on
    /// its first cue. Keeps `order`. Returns `(the list's cues changed, the playhead moved)`.
    pub fn follow(&mut self, old: &[CueId], order: Vec<CueId>) -> (bool, bool) {
        if order == old {
            self.order = Some(order);
            return (false, false);
        }
        let before = self.cue;
        self.cue = match self.cue {
            Some(id) if order.contains(&id) => Some(id),
            Some(id) => {
                let present: HashSet<CueId> = order.iter().copied().collect();
                old.iter()
                    .skip_while(|o| **o != id)
                    .skip(1)
                    .find(|o| present.contains(o))
                    .copied()
            }
            None if old.is_empty() => order.first().copied(),
            None => None,
        };
        self.order = Some(order);
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

/// Reads, from `tree` as it stands before following an edit whose added, removed and moved
/// cues are `structural`, what [`Followed`] needs for the playhead of `list` standing on `cue`.
/// Costs the touched positions right after the playhead (at most one untouched one), not the
/// list: a playhead whose cue and groups the edit did not touch is where it was.
pub(super) fn followed(
    tree: &CueIndex,
    list: CueListId,
    cue: Option<CueId>,
    structural: &BTreeSet<CueId>,
) -> Followed {
    let touched =
        |c: CueId| structural.contains(&c) || tree.ancestors(c).any(|g| structural.contains(&g));
    let mut followed = Followed {
        was_empty: tree.kids(CueContainer::List(list)).is_empty(),
        maybe: Vec::new(),
        sure: None,
    };
    let Some(cue) = cue.filter(|c| touched(*c)) else {
        return followed;
    };
    let mut at = tree.next_in_list(cue);
    // Bounded by the cue count, so a damaged index cannot loop.
    for _ in 0..=tree.len() {
        let Some(next) = at else { break };
        if !touched(next) {
            followed.sure = Some(next);
            break;
        }
        followed.maybe.push(next);
        at = tree.next_in_list(next);
    }
    followed
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
        .map(|id| groups_around(cues, id).into_iter().collect())
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

/// The groups around the cue `id` in a list, outermost first (none if it is not in the list).
pub(super) fn groups_around(cues: &[Cue], id: CueId) -> Vec<CueId> {
    locate(cues, id)
        .map(|path| {
            let outer = path.len().saturating_sub(1);
            path.iter()
                .take(outer)
                .filter_map(|&(level, i)| level.get(i).map(|g| g.id))
                .collect()
        })
        .unwrap_or_default()
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
/// the group's continue fires or has fired (`continues`: it has a run, or its run fired its
/// continue during the playhead's current visit); a group with no run fires none, so the
/// playhead stands on the cue right after it and its continue targets are not lost.
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

#[cfg(test)]
mod follow_tests {
    use super::*;
    use uuid::Uuid;

    fn id(n: u128) -> CueId {
        CueId(Uuid::from_u128(n))
    }

    fn standing_on(cue: Option<CueId>, order: &[CueId]) -> Playhead {
        Playhead {
            cue,
            order: Some(order.to_vec()),
            followed: None,
            around: Vec::new(),
        }
    }

    #[test]
    fn a_deleted_playhead_cue_gives_way_to_the_next_position_that_survives() {
        let old = [id(1), id(2), id(3), id(4)];
        let mut p = standing_on(Some(id(2)), &old);
        assert_eq!(p.follow(&old, vec![id(1), id(4)]), (true, true));
        assert_eq!(p.cue, Some(id(4)));
        // Past the end when nothing after it survives.
        let mut p = standing_on(Some(id(4)), &old);
        assert_eq!(p.follow(&old, vec![id(1), id(2)]), (true, true));
        assert_eq!(p.cue, None);
    }

    #[test]
    fn a_surviving_cue_stays_and_an_unchanged_list_changes_nothing() {
        let old = [id(1), id(2), id(3)];
        let mut p = standing_on(Some(id(2)), &old);
        assert_eq!(p.follow(&old, old.to_vec()), (false, false));
        assert_eq!(p.follow(&old, vec![id(3), id(2), id(1)]), (true, false));
        assert_eq!(p.cue, Some(id(2)));
    }

    #[test]
    fn an_empty_list_that_gains_cues_stands_on_the_first_but_past_the_end_stays() {
        let mut p = standing_on(None, &[]);
        assert_eq!(p.follow(&[], vec![id(5), id(6)]), (true, true));
        assert_eq!(p.cue, Some(id(5)));
        let mut p = standing_on(None, &[id(1)]);
        assert_eq!(p.follow(&[id(1)], vec![id(1), id(2)]), (true, false));
        assert_eq!(p.cue, None);
    }
}
