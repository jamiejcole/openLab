// SPDX-License-Identifier: GPL-3.0-or-later

//! The child each Playlist group plays next (QLab 5.5 marks it in the cue list), kept for the
//! client and reported as it changes (`Action::PlaylistNextChanged`).

use std::collections::{BTreeSet, HashMap};

use crate::cue::{Cue, CueKind, GroupMode};
use crate::model::CueId;

use super::run::{Phase, Run, Slot};
use super::{Action, Ctx, CueEngine};

/// What an edit not yet followed changed, for the marks: every cue, or these.
pub(super) enum MarksEdit {
    All,
    Cues(BTreeSet<CueId>),
}

impl CueEngine {
    /// The child the Playlist group `group` plays next: the first child that a start does not
    /// pass over (skipped, broken at run time, left paused by a lost device still away) while
    /// it is not running, the one after its current
    /// child as the group stands while it plays (wrapping when it loops), none once it is
    /// stopping, devamped or halted, past its last child, or for a shuffled playlist whose order
    /// is not drawn yet (not running, or between passes). `None` too for any other cue.
    /// `Action::PlaylistNextChanged` reports each change.
    pub fn next_in_playlist(&self, group: CueId) -> Option<CueId> {
        self.playlist_marks.get(&group).copied()
    }

    /// Every Playlist group with a next child, in group id order.
    pub fn playlist_marks(&self) -> impl Iterator<Item = (CueId, CueId)> + '_ {
        self.playlist_marks.iter().map(|(g, c)| (*g, *c))
    }

    /// The cues the pending edit touched, with the groups they were in before it (the index is
    /// not yet brought up to date): taken before the edit is followed.
    pub(super) fn marks_edit_before_sync(&self) -> MarksEdit {
        match self.pending_cues() {
            None => MarksEdit::All,
            Some(touched) => {
                let tree = self.problems.tree();
                let parents: Vec<CueId> = touched.iter().filter_map(|c| tree.parent(*c)).collect();
                let mut cues: BTreeSet<CueId> = touched.into_iter().collect();
                cues.extend(parents);
                MarksEdit::Cues(cues)
            }
        }
    }

    /// Brings the marks up to date at the end of a call and reports each that changed, in group id
    /// order. Looked at: every group after an edit with no footprint; else the cues an edit touched,
    /// the groups they were and are in, and the groups inside a touched group (an added subtree);
    /// the groups that had a mark (one may be gone); every group with a run; and each cue whose
    /// state or arming changed in this call, with its group. So a call costs what it touched.
    pub(super) fn sync_playlist_marks(&mut self, edit: Option<MarksEdit>, cx: &mut Ctx) {
        let ws = cx.ws;
        let tree = self.problems.tree();
        let mut look: BTreeSet<CueId> = BTreeSet::new();
        match edit {
            None => {}
            Some(MarksEdit::All) => {
                self.whole_show_passes = self.whole_show_passes.saturating_add(1);
                look.extend(ws.cues().filter(|c| is_playlist(c)).map(|c| c.id));
                look.extend(self.playlist_marks.keys().copied());
            }
            Some(MarksEdit::Cues(cues)) => {
                for id in &cues {
                    look.insert(*id);
                    look.extend(tree.parent(*id));
                    if let Some(cue) = tree.locate(ws, *id)
                        && !cue.children().is_empty()
                    {
                        look.extend(super::group::descendants(cue));
                    }
                }
                look.extend(self.playlist_marks.keys().copied());
            }
        }
        for (id, slot) in &self.slots {
            match slot {
                Slot::Active(Run { group: Some(_), .. }) => {
                    look.insert(*id);
                }
                // Whether it can play again follows its device, which changes with no state
                // change: its group (or it, a group) looks again on every call while it is so.
                Slot::Lost(_) => {
                    look.insert(*id);
                    look.extend(tree.parent(*id));
                }
                _ => {}
            }
        }
        for action in &cx.out {
            let cue = match action {
                Action::StateChanged { cue, .. } | Action::SetArmed { cue, .. } => *cue,
                _ => continue,
            };
            look.insert(cue);
            look.extend(tree.parent(cue));
        }
        let mut changed = Vec::new();
        for id in look {
            let next = tree
                .locate(ws, id)
                .filter(|c| is_playlist(c))
                .and_then(|group| self.compute_next(group));
            if self.playlist_marks.get(&id).copied() != next {
                changed.push((id, next));
            }
        }
        for (group, next) in changed {
            match next {
                Some(child) => self.playlist_marks.insert(group, child),
                None => self.playlist_marks.remove(&group),
            };
            cx.out.push(Action::PlaylistNextChanged { group, next });
        }
    }

    /// [`Self::next_in_playlist`] worked out from the group's run and children now.
    fn compute_next(&self, group: &Cue) -> Option<CueId> {
        let CueKind::Group {
            mode: GroupMode::Playlist { loop_, shuffle },
            children,
        } = &group.kind
        else {
            return None;
        };
        // Whether a start of the child would play it rather than pass over it at once.
        // A child its lost device left paused plays again (cleared by the start) once the device
        // is back.
        let plays = |c: &&Cue| {
            !self.is_skipped(c)
                && match self.slots.get(&c.id) {
                    Some(Slot::Broken(_)) => false,
                    Some(Slot::Lost(_)) => self.lost_may_clear(c.id),
                    _ => true,
                }
        };
        let first = || {
            (!*shuffle)
                .then(|| children.iter().find(plays).map(|c| c.id))
                .flatten()
        };
        let Some(Slot::Active(run)) = self.slots.get(&group.id) else {
            return first();
        };
        let Some(g) = &run.group else {
            return first();
        };
        if g.stopping || g.devamp.is_some() || g.halt.is_some() {
            return None;
        }
        match run.phase {
            Phase::PreWait { .. } => return first(),
            Phase::Done => return None,
            Phase::Action { .. } => {}
        }
        let Some(p) = &g.playlist else {
            return first();
        };
        if !p.order.is_empty() {
            let from = if p.current.is_some() {
                p.pos.saturating_add(1)
            } else {
                p.pos
            };
            // By id, so a long pass of children passed over costs one lookup each.
            let by_id: HashMap<CueId, &Cue> = children.iter().map(|c| (c.id, c)).collect();
            return p
                .order
                .iter()
                .skip(from)
                .filter_map(|id| by_id.get(id).copied())
                .find(plays)
                .map(|c| c.id);
        }
        // As the group stands now: after the current child, or the one that took its place.
        let from = p
            .current
            .and_then(|(c, _)| children.iter().position(|k| k.id == c))
            .map_or(p.index, |i| i.saturating_add(1));
        let after = children.iter().skip(from).find(plays);
        let wrapped = || {
            if *loop_ {
                children.iter().find(plays)
            } else {
                None
            }
        };
        after.or_else(wrapped).map(|c| c.id)
    }
}

fn is_playlist(cue: &Cue) -> bool {
    matches!(
        cue.kind,
        CueKind::Group {
            mode: GroupMode::Playlist { .. },
            ..
        }
    )
}
