// SPDX-License-Identifier: GPL-3.0-or-later

//! Following a model edit: cues gone from the workspace are stopped and forgotten, their groups
//! react, and each list's playhead keeps its place (docs/cue-engine.md, "Model edits").

use std::collections::{BTreeSet, HashSet};

use crate::cue::CueKind;
use crate::cue_index::CueContainer;
use crate::model::{CueId, Workspace};

use super::playhead::{self, Playhead};
use super::run::{Exit, Phase, Slot};
use super::types::{Action, EngineWarning};
use super::{Ctx, CueEngine, collect_ids, release};

impl CueEngine {
    // ----- model edits -----

    /// Stops and forgets cues that are no longer in the workspace, then lets their groups react
    /// (and ends groups an edit left with no busy child).
    pub(super) fn sync_model(&mut self, cx: &mut Ctx) -> bool {
        let previews_ended = self.sync_previews(cx);
        if self.slots.is_empty() {
            return previews_ended;
        }
        // The cue index is in step with the workspace here (the problems followed the edits);
        // with duplicate ids it is not the reference, and the workspace is walked.
        let ws = cx.ws;
        let tree = self.problems.tree();
        let walked: Option<HashSet<CueId>> = tree.has_duplicates().then(|| {
            let mut present = HashSet::new();
            for list in &ws.cue_lists {
                collect_ids(&list.cues, &mut present);
            }
            present
        });
        if walked.is_some() {
            self.whole_show_passes = self.whole_show_passes.saturating_add(1);
        }
        let tree = self.problems.tree();
        let present = |id: &CueId| match &walked {
            Some(present) => present.contains(id),
            None => tree.contains(*id),
        };
        let is_group = |id: CueId| {
            let cue = match &walked {
                Some(_) => ws.find_cue(id),
                None => tree.locate(ws, id),
            };
            cue.is_some_and(|c| matches!(c.kind, CueKind::Group { .. }))
        };
        self.random_cycles.retain(|group, _| present(group));
        self.continued.retain(|group| present(group));
        // Gone, or now a cue of the other sort (group or not) than the one running: a runtime
        // only fits the kind it was started as.
        let gone: Vec<CueId> = self
            .slots
            .iter()
            .filter(|(id, slot)| {
                !present(id)
                    || matches!(slot, Slot::Active(run) if run.group.is_some() != is_group(**id))
            })
            .map(|(id, _)| *id)
            .collect();
        let changed = !gone.is_empty() || previews_ended;
        for cue in gone {
            let before = self.state(cue);
            if let Some(slot) = self.remove_slot(cue, cx.now, Exit::Stopped, cx) {
                release(cue, &slot, cx);
                if matches!(slot, Slot::Active(_)) {
                    cx.warn(EngineWarning::CueRemoved { cue });
                }
            }
            self.report(cue, before, cx.now, cx);
        }
        self.drain(cx);
        // Each round removes at least one group's run and starts nothing; the guard is a second
        // line of defence.
        let mut guard = self.slots.len().saturating_add(1);
        while guard > 0 && self.sweep_groups(cx) {
            guard -= 1;
            self.drain(cx);
        }
        changed
    }

    /// A running (or paused) Wait or Fade whose duration was edited takes the new duration, as
    /// QLab 5 does: its action ends at the new length from its start, or at once if it is past
    /// that already (on the resume, if paused). A Fade keeps its rate: its ramps go on as
    /// planned and it lets go of its levels at the new end, where its curve has got to
    /// (`fade_finish`). One look per running Wait or Fade.
    pub(super) fn follow_duration_edits(&mut self, cx: &mut Ctx) {
        let ws = cx.ws;
        let tree = self.problems.tree();
        let duplicates = tree.has_duplicates();
        for (id, slot) in &mut self.slots {
            let Slot::Active(run) = slot else { continue };
            let (
                Some(old),
                Phase::Action {
                    start,
                    end: Some(_),
                },
            ) = (run.duration, run.phase)
            else {
                continue;
            };
            if run.is_broken() || run.stop_end.is_some() {
                continue;
            }
            let cue = if duplicates {
                ws.find_cue(*id)
            } else {
                tree.locate(ws, *id)
            };
            let duration = match cue.map(|c| &c.kind) {
                Some(CueKind::Wait { duration } | CueKind::Fade { duration, .. }) => {
                    cx.secs(*duration, &self.config)
                }
                _ => continue,
            };
            if duration == old {
                continue;
            }
            run.duration = Some(duration);
            // Not before now: a paused cue is frozen on its pause sample, and the resume moves
            // its end on by the pause.
            let floor = run.paused_at.unwrap_or(cx.now);
            let end = start
                .saturating_add(duration.saturating_sub(run.action_offset))
                .max(floor);
            run.phase = Phase::Action {
                start,
                end: Some(end),
            };
            if let Some(fade) = run.fade.as_mut() {
                fade.retimed = true;
            }
        }
    }

    /// Follows edits to the cue lists. Returns whether any list's cues (or the set of lists)
    /// differ from the last pass (as far as a call nothing was signalled for needs to know).
    ///
    /// Called once the problems follow the edits, so the engine's cue index is in step with the
    /// workspace. A playhead that keeps its list's positions (after a full rebuild, or while the
    /// index has duplicate ids) compares them with the list's positions now, as it always did.
    /// One that leaves them to the index follows an edit from its footprint (M4.12c): the
    /// index, before it followed the edit, said what the edit touched around the playhead
    /// ([`playhead::Followed`]); with nothing added, removed or moved nothing is read at all.
    /// Both ways place the playhead alike (property-tested against `model_changed`).
    pub(super) fn sync_playheads(&mut self, cx: &mut Ctx) -> bool {
        let before = self.playheads.len();
        self.playheads
            .retain(|id, _| cx.ws.cue_lists.iter().any(|l| l.id == *id));
        let mut changed = self.playheads.len() != before;
        for list in &cx.ws.cue_lists {
            let tree = self.problems.tree();
            let (moved, around) = match self.playheads.get_mut(&list.id) {
                Some(playhead) => {
                    let followed = playhead.followed.take();
                    if let Some(old) = playhead.take_order() {
                        self.whole_show_passes = self.whole_show_passes.saturating_add(1);
                        let (edited, moved) =
                            playhead.follow(&old, playhead::positions(&list.cues));
                        changed |= edited;
                        // An edit can put the playhead into a group without moving it (the cue
                        // dragged into the group, which can leave the positions' order as it
                        // was) or move it within one (its cue deleted): look whenever edits
                        // are followed.
                        let around = playhead
                            .cue
                            .map(|id| playhead::groups_around(&list.cues, id))
                            .unwrap_or_default();
                        (moved, Some(around))
                    } else if let Some(followed) = followed {
                        changed = true;
                        let in_list = |id: &CueId| tree.list_of(*id) == Some(list.id);
                        let cue = match playhead.cue {
                            Some(id) if in_list(&id) => Some(id),
                            Some(_) => followed
                                .maybe
                                .iter()
                                .copied()
                                .find(in_list)
                                .or(followed.sure.filter(in_list)),
                            None if followed.was_empty => {
                                tree.kids(CueContainer::List(list.id)).first().copied()
                            }
                            None => None,
                        };
                        let moved = playhead.set(cue);
                        let mut around: Vec<CueId> = cue
                            .map(|id| tree.ancestors(id).collect())
                            .unwrap_or_default();
                        around.reverse();
                        (moved, Some(around))
                    } else {
                        // Nothing added, removed or moved since the playhead last looked: its
                        // list's positions and the groups around it are as they were.
                        (false, None)
                    }
                }
                None => {
                    changed = true;
                    self.whole_show_passes = self.whole_show_passes.saturating_add(1);
                    self.playheads.insert(list.id, Playhead::new(&list.cues));
                    (true, None)
                }
            };
            if moved {
                cx.out.push(Action::PlayheadChanged {
                    list: list.id,
                    cue: self.playhead(list.id),
                });
            }
            if let Some(around) = around {
                self.enter_groups(list.id, around, None);
            }
        }
        // The index holds every list's positions from here, unless it cannot be trusted with
        // them (duplicate ids: it counts each id once, the positions do not).
        let trusted = !self.problems.tree().has_duplicates();
        for list in &cx.ws.cue_lists {
            if let Some(playhead) = self.playheads.get_mut(&list.id) {
                if trusted {
                    playhead.take_order();
                } else if !playhead.keeps_order() {
                    playhead.keep_order(playhead::positions(&list.cues));
                }
            }
        }
        changed
    }

    /// Before the cue index follows an edit that added, removed or moved `structural`: each
    /// playhead that leaves its positions to the index notes what the edit touched around it
    /// (`sync_playheads` places it once the index has followed).
    pub(super) fn playheads_before_edit(&mut self, ws: &Workspace, structural: &BTreeSet<CueId>) {
        let tree = self.problems.tree();
        for (list, playhead) in &mut self.playheads {
            // Only one edit is followed between two passes; one already noted stays.
            if !playhead.keeps_order() && playhead.followed.is_none() {
                // A playhead cue the edit did not touch but the workspace no longer has was
                // deleted by an edit nobody signalled (the caller's breach). The footprint
                // knows nothing of it, so keep the old list order and let the pass compare it
                // with the list as it stands, as a full rebuild would.
                let breached = playhead.cue.is_some_and(|c| {
                    !structural.contains(&c)
                        && !tree.ancestors(c).any(|g| structural.contains(&g))
                        && tree.locate(ws, c).is_none()
                });
                if breached {
                    playhead.keep_order(tree.list_order(*list));
                } else {
                    playhead.followed =
                        Some(playhead::followed(tree, *list, playhead.cue, structural));
                }
            }
        }
    }

    /// Before the cue index is rebuilt: each playhead that leaves its positions to the index and
    /// noted nothing about an edit keeps them itself, so `sync_playheads` can compare.
    pub(super) fn keep_playhead_orders(&mut self) {
        let tree = self.problems.tree();
        let mut kept = 0u64;
        for (list, playhead) in &mut self.playheads {
            if !playhead.keeps_order() && playhead.followed.is_none() {
                playhead.keep_order(tree.list_order(*list));
                kept += 1;
            }
        }
        self.whole_show_passes = self.whole_show_passes.saturating_add(kept);
    }
}
