// SPDX-License-Identifier: GPL-3.0-or-later

//! Where every cue sits in the workspace, kept up to date from an edit's footprint instead of
//! rebuilt: the part of the control thread's per-edit bookkeeping (static problems, media use)
//! that must know a cue's container, its ancestors and workspace order.
//!
//! An edit names the cues it touched ([`crate::Footprint`]); [`CueIndex::reconcile`] reads only
//! those cues (and the sibling lists they moved between) out of the new workspace, so it reads
//! the edit's cues and their sibling lists, not the whole show (a lookup still scans the
//! sibling list at each level of the path, so a flat list of n cues costs O(n); M4.12c). A cue is found by walking its recorded ancestors and checking
//! each step against the workspace (so a stale record can never give a wrong answer); only a
//! cue the index cannot place that way (it moved, or is gone) costs one allocation-free scan of
//! the cue ids. [`CueIndex::build`] is the reference: after any edit the reconciled index equals
//! a freshly built one ([`CueIndex::matches`]).
//!
//! Cue ids are unique in a valid workspace (commands refuse duplicates, opening repairs them).
//! With duplicates the first cue (depth first) with an id counts, as for `Workspace::find_cue`,
//! and the index says so ([`CueIndex::has_duplicates`]): it is then only ever rebuilt, never
//! reconciled, because a footprint cannot say which of two cues with one id it means.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::cue::{Cue, CueKind};
use crate::model::{CueId, CueListId, Workspace};

/// What holds a cue: a cue list, or a group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CueContainer {
    List(CueListId),
    Group(CueId),
}

/// Cue placement for a whole workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CueIndex {
    /// The lists in workspace order.
    lists: Vec<CueListId>,
    /// The container of every cue.
    place: HashMap<CueId, CueContainer>,
    /// The cues of every non-empty container, in order.
    kids: HashMap<CueContainer, Vec<CueId>>,
    /// Two cues had one id when it was built.
    duplicates: bool,
}

/// What a [`CueIndex::reconcile`] changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndexChange {
    /// The index follows the edit; the cues it names.
    Delta(IndexDelta),
    /// The cue lists themselves differ from the index (a list was added, removed or reordered),
    /// which no command does today: the index was rebuilt and the caller must treat every cue
    /// as changed.
    Rebuilt,
}

/// The cues whose place changed, found by [`CueIndex::reconcile`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndexDelta {
    /// No longer in the workspace (a removed group's descendants included).
    pub gone: Vec<CueId>,
    /// Newly in the workspace (an added group's descendants included).
    pub fresh: Vec<CueId>,
    /// Still there, in another container.
    pub moved: Vec<CueId>,
    /// The groups a cue left (moved out of, or removed from) that are still in the workspace.
    pub former_parents: BTreeSet<CueId>,
}

impl CueIndex {
    /// The reference: the index of `ws`, built from scratch.
    pub fn build(ws: &Workspace) -> Self {
        Self::build_visiting(ws, |_| {})
    }

    /// Builds the index and calls `visit` for each indexed cue in workspace order (lists in
    /// order, cues depth first). Iterative, so a hostile nesting depth cannot overflow the stack.
    pub fn build_visiting<'w>(ws: &'w Workspace, mut visit: impl FnMut(&'w Cue)) -> Self {
        let mut index = Self {
            lists: ws.cue_lists.iter().map(|l| l.id).collect(),
            ..Self::default()
        };
        for list in &ws.cue_lists {
            let list_container = CueContainer::List(list.id);
            // Each level: the cues still to visit and the container they are in.
            let mut stack: Vec<(std::slice::Iter<'w, Cue>, CueContainer)> =
                vec![(list.cues.iter(), list_container)];
            while let Some((level, container)) = stack.last_mut() {
                let container = *container;
                let Some(cue) = level.next() else {
                    stack.pop();
                    continue;
                };
                if let std::collections::hash_map::Entry::Vacant(e) = index.place.entry(cue.id) {
                    e.insert(container);
                    index.kids.entry(container).or_default().push(cue.id);
                    visit(cue);
                } else {
                    index.duplicates = true;
                }
                if matches!(cue.kind, CueKind::Group { .. }) {
                    stack.push((cue.children().iter(), CueContainer::Group(cue.id)));
                }
            }
        }
        index
    }

    /// Whether two cues had the same id when the index was built (an invalid workspace).
    pub fn has_duplicates(&self) -> bool {
        self.duplicates
    }

    pub fn contains(&self, id: CueId) -> bool {
        self.place.contains_key(&id)
    }

    /// How many cues the workspace has.
    pub fn len(&self) -> usize {
        self.place.len()
    }

    pub fn is_empty(&self) -> bool {
        self.place.is_empty()
    }

    /// The container of `id`.
    pub fn container(&self, id: CueId) -> Option<CueContainer> {
        self.place.get(&id).copied()
    }

    /// The group `id` is in (`None` at the top level of a list, or for an unknown cue).
    pub fn parent(&self, id: CueId) -> Option<CueId> {
        match self.place.get(&id) {
            Some(CueContainer::Group(g)) => Some(*g),
            _ => None,
        }
    }

    /// The groups around `id`, nearest first. Bounded by the number of cues, so a damaged index
    /// cannot loop.
    pub fn ancestors(&self, id: CueId) -> impl Iterator<Item = CueId> + '_ {
        std::iter::successors(self.parent(id), |g| self.parent(*g)).take(self.place.len())
    }

    /// The cues of a container, in order.
    pub fn kids(&self, container: CueContainer) -> &[CueId] {
        self.kids.get(&container).map_or(&[], Vec::as_slice)
    }

    /// Every cue's rank in workspace order (lists in order, cues depth first).
    pub fn ranks(&self) -> HashMap<CueId, usize> {
        let mut rank = HashMap::with_capacity(self.place.len());
        let mut stack: Vec<std::slice::Iter<'_, CueId>> = Vec::new();
        for list in self.lists.iter().rev() {
            stack.push(self.kids(CueContainer::List(*list)).iter());
        }
        // Lists were pushed last-first, so the first list is on top and its cues are walked
        // before the next list's. A damaged index (a cue under itself) ends at the cue count.
        while let Some(level) = stack.last_mut() {
            if rank.len() > self.place.len() {
                break;
            }
            match level.next() {
                Some(id) => {
                    let n = rank.len();
                    // Only the first time a cue is met: its kids are walked once.
                    if rank.insert(*id, n).is_none() {
                        stack.push(self.kids(CueContainer::Group(*id)).iter());
                    }
                }
                None => {
                    stack.pop();
                }
            }
        }
        rank
    }

    /// `id`'s place as list index followed by the position in each container down to it:
    /// comparing these orders cues as the workspace does. `None` for an unknown cue.
    pub fn path(&self, id: CueId) -> Option<Vec<usize>> {
        let mut path = Vec::new();
        let mut at = id;
        for _ in 0..=self.place.len() {
            let container = *self.place.get(&at)?;
            path.push(self.kids(container).iter().position(|k| *k == at)?);
            match container {
                CueContainer::Group(g) => at = g,
                CueContainer::List(list) => {
                    path.push(self.lists.iter().position(|l| *l == list)?);
                    path.reverse();
                    return Some(path);
                }
            }
        }
        None
    }

    /// The cue `id` in `ws`, if the index places it correctly there: each step down from its
    /// list is checked against the workspace, so a wrong record gives `None`, never a wrong cue.
    pub fn locate<'w>(&self, ws: &'w Workspace, id: CueId) -> Option<&'w Cue> {
        self.find(ws, id).map(|(_, cue)| cue)
    }

    fn find<'w>(&self, ws: &'w Workspace, id: CueId) -> Option<(CueContainer, &'w Cue)> {
        // The ancestors, nearest first, ending at the cue's list.
        let mut chain = vec![id];
        let first = *self.place.get(&id)?;
        let mut container = first;
        let list = loop {
            match container {
                CueContainer::List(list) => break list,
                CueContainer::Group(g) => {
                    if chain.len() > self.place.len() {
                        return None;
                    }
                    chain.push(g);
                    container = *self.place.get(&g)?;
                }
            }
        };
        let mut cues: &[Cue] = &ws.cue_lists.iter().find(|l| l.id == list)?.cues;
        let mut found = None;
        for step in chain.iter().rev() {
            let cue = cues.iter().find(|c| c.id == *step)?;
            cues = cue.children();
            found = Some(cue);
        }
        found.map(|cue| (first, cue))
    }

    /// Whether the index equals the one built from `ws` (the reference).
    pub fn matches(&self, ws: &Workspace) -> bool {
        *self == Self::build(ws)
    }

    /// Brings the index in line with `ws` after an edit. `touched` names every cue the edit may
    /// have changed; `structural` those it added, removed or moved (a subset of `touched`;
    /// sibling order is only re-read around these). Cost: the touched cues and the sibling lists
    /// of the structural ones, plus one scan of the cue ids when one of them cannot be found
    /// where the index had it.
    ///
    /// `added` is the part of `structural` that was added (the top of each added subtree). An
    /// added subtree is new as a whole: a cue in it that had been in the index (removed and
    /// added again with the same id, alone or inside another group, as a group or not) is
    /// reported gone and fresh, with every descendant of the old cue and of the new one,
    /// whether or not the ids are the same.
    pub fn reconcile(
        &mut self,
        ws: &Workspace,
        touched: &BTreeSet<CueId>,
        structural: &BTreeSet<CueId>,
        added: &BTreeSet<CueId>,
    ) -> IndexChange {
        if self.duplicates
            || !ws
                .cue_lists
                .iter()
                .map(|l| l.id)
                .eq(self.lists.iter().copied())
        {
            *self = Self::build(ws);
            return IndexChange::Rebuilt;
        }
        let mut located: HashMap<CueId, (CueContainer, &Cue)> = HashMap::new();
        let mut missing: Vec<CueId> = Vec::new();
        for &id in touched {
            match self.find(ws, id) {
                Some(found) => {
                    located.insert(id, found);
                }
                None => missing.push(id),
            }
        }
        scan(ws, &mut missing, &mut located);
        // What is still missing is not in the workspace.
        let absent = missing;

        // Cues in an added subtree that the index already has were removed and added again:
        // forget where they were, so they are placed (fresh) like any new cue, and report them
        // gone too. The walk covers everything the old tops held, not just the re-added ids.
        let mut delta = IndexDelta::default();
        let mut new_ids: HashSet<CueId> = HashSet::new();
        for top in added {
            if let Some(&(_, cue)) = located.get(top) {
                subtree_ids(cue, &mut new_ids);
            }
        }
        let mut purged_from: Vec<CueContainer> = Vec::new();
        if !new_ids.is_empty() {
            let mut seen: HashSet<CueId> = HashSet::new();
            let mut stack: Vec<CueId> = structural
                .iter()
                .filter(|id| self.place.contains_key(id))
                .copied()
                .collect();
            while let Some(id) = stack.pop() {
                if !seen.insert(id) {
                    continue;
                }
                if new_ids.contains(&id)
                    && let Some(old) = self.place.remove(&id)
                {
                    if let CueContainer::Group(g) = old {
                        delta.former_parents.insert(g);
                    }
                    purged_from.push(old);
                    delta.gone.push(id);
                }
                if let Some(kids) = self.kids.get(&CueContainer::Group(id)) {
                    stack.extend(kids.iter().copied());
                }
            }
        }

        let mut queued: HashSet<CueContainer> = HashSet::new();
        let mut work: Vec<CueContainer> = Vec::new();
        let mut enqueue = |c: CueContainer, work: &mut Vec<CueContainer>| {
            if queued.insert(c) {
                work.push(c);
            }
        };
        for (&id, &(container, cue)) in &located {
            let before = self.place.get(&id).copied();
            if structural.contains(&id) || before != Some(container) {
                enqueue(container, &mut work);
                if let Some(old) = before {
                    enqueue(old, &mut work);
                }
            }
            // A structural id may be a different cue than the index knew (removed and added
            // again in one edit, as a group, or no longer one): its kids are re-read whenever
            // it is a group now or held kids before.
            if (before.is_none() || structural.contains(&id))
                && (matches!(cue.kind, CueKind::Group { .. })
                    || self.kids.contains_key(&CueContainer::Group(id)))
            {
                enqueue(CueContainer::Group(id), &mut work);
            }
        }
        for id in &absent {
            if let Some(&old) = self.place.get(id) {
                enqueue(old, &mut work);
            }
        }
        for old in purged_from {
            enqueue(old, &mut work);
        }

        // Cues a container no longer lists, to be gone unless another container took them.
        let mut left: Vec<(CueId, CueContainer)> = Vec::new();
        while let Some(container) = work.pop() {
            let cues: &[Cue] = match container {
                CueContainer::List(list) => match ws.cue_lists.iter().find(|l| l.id == list) {
                    Some(l) => &l.cues,
                    None => continue,
                },
                CueContainer::Group(g) => {
                    let found = match located.get(&g) {
                        Some(&(_, cue)) => Some(cue),
                        None => match self.find(ws, g) {
                            Some((c, cue)) => {
                                located.insert(g, (c, cue));
                                Some(cue)
                            }
                            None => {
                                // Not where the index has it (an ancestor moved this edit), or
                                // gone: one scan settles it.
                                let mut one = vec![g];
                                scan(ws, &mut one, &mut located);
                                located.get(&g).map(|&(_, cue)| cue)
                            }
                        },
                    };
                    match found {
                        Some(cue) => cue.children(),
                        None => continue,
                    }
                }
            };
            let new: Vec<CueId> = cues.iter().map(|c| c.id).collect();
            let old = self.kids.get(&container);
            if old.is_some_and(|o| *o == new) && cues.iter().all(|c| self.place.contains_key(&c.id))
            {
                continue;
            }
            let old = if new.is_empty() {
                self.kids.remove(&container)
            } else {
                self.kids.insert(container, new.clone())
            };
            for cue in cues {
                match self.place.insert(cue.id, container) {
                    None => {
                        delta.fresh.push(cue.id);
                        if matches!(cue.kind, CueKind::Group { .. })
                            || self.kids.contains_key(&CueContainer::Group(cue.id))
                        {
                            enqueue(CueContainer::Group(cue.id), &mut work);
                        }
                    }
                    Some(before) if before != container => {
                        delta.moved.push(cue.id);
                        if let CueContainer::Group(g) = before {
                            delta.former_parents.insert(g);
                        }
                    }
                    Some(_) => {}
                }
            }
            if let Some(old) = old {
                let now: HashSet<CueId> = new.into_iter().collect();
                left.extend(
                    old.into_iter()
                        .filter(|id| !now.contains(id))
                        .map(|id| (id, container)),
                );
            }
        }

        // A cue its container dropped and no other took is gone, with what it held.
        let mut tops: Vec<(CueId, CueContainer)> = left
            .into_iter()
            .filter(|(id, from)| self.place.get(id) == Some(from))
            .collect();
        for id in absent {
            if let Some(&from) = self.place.get(&id)
                && !tops.iter().any(|(t, _)| *t == id)
            {
                tops.push((id, from));
            }
        }
        for (top, from) in tops {
            if let CueContainer::Group(g) = from {
                delta.former_parents.insert(g);
            }
            let mut stack = vec![top];
            while let Some(id) = stack.pop() {
                if self.place.remove(&id).is_none() {
                    continue;
                }
                delta.gone.push(id);
                for kid in self
                    .kids
                    .remove(&CueContainer::Group(id))
                    .unwrap_or_default()
                {
                    // A kid that moved elsewhere this edit is no longer this group's.
                    if self.place.get(&kid) == Some(&CueContainer::Group(id)) {
                        stack.push(kid);
                    }
                }
            }
        }
        // A group that is itself gone is not somewhere cues can have left.
        delta.former_parents.retain(|g| self.place.contains_key(g));
        IndexChange::Delta(delta)
    }
}

/// The ids of `cue` and of everything below it.
fn subtree_ids(cue: &Cue, out: &mut HashSet<CueId>) {
    let mut stack = vec![cue];
    while let Some(c) = stack.pop() {
        out.insert(c.id);
        stack.extend(c.children());
    }
}

/// Finds the cues in `wanted` anywhere in `ws`, recording them in `located` and removing them
/// from `wanted` (what is left is not in the workspace). Ids are compared in a depth-first
/// walk that stops once everything is found; nothing is allocated per cue.
fn scan<'w>(
    ws: &'w Workspace,
    wanted: &mut Vec<CueId>,
    located: &mut HashMap<CueId, (CueContainer, &'w Cue)>,
) {
    if wanted.is_empty() {
        return;
    }
    for list in &ws.cue_lists {
        let mut stack: Vec<(std::slice::Iter<'w, Cue>, CueContainer)> =
            vec![(list.cues.iter(), CueContainer::List(list.id))];
        while let Some((level, container)) = stack.last_mut() {
            let container = *container;
            let Some(cue) = level.next() else {
                stack.pop();
                continue;
            };
            if let Some(at) = wanted.iter().position(|w| *w == cue.id) {
                wanted.swap_remove(at);
                located.insert(cue.id, (container, cue));
                if wanted.is_empty() {
                    return;
                }
            }
            if matches!(cue.kind, CueKind::Group { .. }) {
                stack.push((cue.children().iter(), CueContainer::Group(cue.id)));
            }
        }
    }
}
