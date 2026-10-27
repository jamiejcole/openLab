// SPDX-License-Identifier: GPL-3.0-or-later

//! The OSC side's copy of the workspace (docs/osc.md, "Updates"). It is built from a `snapshot`
//! and kept current from the `model_changed`, `runtime` and `document` broadcasts with the
//! reference client logic (`Workspace::apply_changes`), so answering an OSC message never asks
//! the control thread anything. A gap in the numbering, or an edit that does not apply, means
//! the copy cannot be trusted: the caller asks for a new snapshot.

use std::collections::HashMap;

use openlab_core::protocol::{Delete, DocumentState, RuntimeChange, Snapshot, Upsert};
use openlab_core::{Cue, CueId, CueListId, Workspace, WorkspaceId};
use serde_json::{Value, json};

use crate::osc::{CueField, OscShow};

/// Most per-cue updates one change produces. Past it one workspace-level update is sent
/// instead ("something changed, ask again"), so pasting 500 cues does not become 500 datagrams
/// for every subscriber.
pub const MAX_CUE_UPDATES: usize = 32;

/// What changed, for the update messages (NET-05).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Update {
    /// The workspace changed beyond one cue's properties (cues added, removed or moved, a
    /// list changed, a snapshot replaced it).
    Workspace,
    /// One cue changed: an edit, or it started or stopped.
    Cue(CueId),
    /// A cue list's playhead moved (`None`: past the end).
    Playhead { list: CueListId, cue: Option<CueId> },
}

/// How a broadcast met the copy.
#[derive(Debug, PartialEq, Eq)]
pub enum Applied {
    /// Already contained (at or below the copy's revision or seq): ignored.
    Stale,
    /// Applied.
    Updates(Vec<Update>),
    /// A message was missed, or an edit did not apply: a new snapshot is needed.
    Resync,
}

#[derive(Debug)]
pub struct Mirror {
    workspace: Workspace,
    revision: u64,
    runtime_seq: u64,
    document: DocumentState,
    playheads: HashMap<CueListId, Option<CueId>>,
}

impl Mirror {
    pub fn from_snapshot(snapshot: Snapshot) -> Self {
        Self {
            playheads: snapshot.playheads.iter().map(|p| (p.list, p.cue)).collect(),
            workspace: snapshot.workspace,
            revision: snapshot.revision,
            runtime_seq: snapshot.runtime_seq,
            document: snapshot.document,
        }
    }

    /// The model revision the copy is at.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The last `runtime` seq folded in.
    pub fn runtime_seq(&self) -> u64 {
        self.runtime_seq
    }

    pub fn workspace_id(&self) -> WorkspaceId {
        self.workspace.id
    }

    pub fn passcode(&self) -> Option<&str> {
        self.workspace.settings.osc_passcode.as_deref()
    }

    /// What a replacement of this copy by `new` means for subscribers: one workspace update if
    /// anything differs, and a playhead update for each list whose playhead moved.
    pub fn updates_to(&self, new: &Mirror) -> Vec<Update> {
        let mut updates = Vec::new();
        if self.workspace != new.workspace {
            updates.push(Update::Workspace);
        }
        for list in &new.workspace.cue_lists {
            let before = self.playheads.get(&list.id).copied().flatten();
            let after = new.playheads.get(&list.id).copied().flatten();
            if before != after {
                updates.push(Update::Playhead {
                    list: list.id,
                    cue: after,
                });
            }
        }
        updates
    }

    pub fn model_changed(
        &mut self,
        revision: u64,
        upserts: &[Upsert],
        deletes: &[Delete],
    ) -> Applied {
        if revision <= self.revision {
            return Applied::Stale;
        }
        if revision != self.revision + 1 {
            return Applied::Resync;
        }
        let mut updates = Updates::default();
        // Judged before the edit: a cue that was not there, or is deleted, changes the shape
        // of the workspace; one that is only upserted over itself changed its properties.
        for delete in deletes {
            match delete {
                Delete::Cue { id } => {
                    if self.workspace.find_cue(*id).is_some() {
                        updates.structure = true;
                    }
                    updates.cue(*id);
                }
                _ => updates.structure = true,
            }
        }
        for upsert in upserts {
            match upsert {
                Upsert::Cue { cue, .. } => {
                    if self.workspace.find_cue(cue.id).is_none() {
                        updates.structure = true;
                    }
                    updates.cue(cue.id);
                }
                _ => updates.structure = true,
            }
        }
        if self.workspace.apply_changes(upserts, deletes).is_err() {
            return Applied::Resync;
        }
        self.revision = revision;
        Applied::Updates(updates.finish())
    }

    pub fn runtime(&mut self, seq: u64, changes: &[RuntimeChange]) -> Applied {
        if seq <= self.runtime_seq {
            return Applied::Stale;
        }
        if seq != self.runtime_seq + 1 {
            return Applied::Resync;
        }
        self.runtime_seq = seq;
        let mut updates = Updates::default();
        let mut playheads = Vec::new();
        for change in changes {
            match change {
                RuntimeChange::CueState { cue, .. } => updates.cue(*cue),
                // Only a move is news: the insert says what was there.
                RuntimeChange::Playhead { list, cue }
                    if self.playheads.insert(*list, *cue) != Some(*cue) =>
                {
                    playheads.push(Update::Playhead {
                        list: *list,
                        cue: *cue,
                    });
                }
                _ => {}
            }
        }
        let mut out = updates.finish();
        out.extend(playheads);
        Applied::Updates(out)
    }

    /// A save-as or another file: the workspace may be a different one, which only a snapshot
    /// tells (a `document` message does not say what was opened).
    pub fn document(&mut self, document: &DocumentState) -> Applied {
        if document.path == self.document.path {
            self.document = document.clone();
            Applied::Updates(Vec::new())
        } else {
            Applied::Resync
        }
    }
}

/// Collects the cue updates of one change, in order, without repeats.
#[derive(Default)]
struct Updates {
    cues: Vec<CueId>,
    structure: bool,
    overflow: bool,
}

impl Updates {
    fn cue(&mut self, id: CueId) {
        if self.overflow || self.cues.contains(&id) {
            return;
        }
        if self.cues.len() >= MAX_CUE_UPDATES {
            self.overflow = true;
            return;
        }
        self.cues.push(id);
    }

    fn finish(self) -> Vec<Update> {
        if self.overflow {
            return vec![Update::Workspace];
        }
        let mut out = Vec::new();
        if self.structure {
            out.push(Update::Workspace);
        }
        out.extend(self.cues.into_iter().map(Update::Cue));
        out
    }
}

/// The dispatcher's view of the show: the copy, read-only.
pub struct MirrorShow<'a>(pub &'a Mirror);

impl MirrorShow<'_> {
    /// First cue, depth-first, for which `pick` says yes. Iterative: a hostile show cannot
    /// overflow the stack.
    fn find(&self, pick: impl Fn(&Cue) -> bool) -> Option<CueId> {
        let mut stack: Vec<&[Cue]> = self
            .0
            .workspace
            .cue_lists
            .iter()
            .rev()
            .map(|l| l.cues.as_slice())
            .collect();
        while let Some(cues) = stack.pop() {
            let Some((cue, rest)) = cues.split_first() else {
                continue;
            };
            if pick(cue) {
                return Some(cue.id);
            }
            // The children come before the siblings after the group.
            stack.push(rest);
            stack.push(cue.children());
        }
        None
    }
}

impl OscShow for MirrorShow<'_> {
    fn workspace_id(&self) -> WorkspaceId {
        self.0.workspace.id
    }

    fn passcode(&self) -> Option<&str> {
        self.0.passcode()
    }

    /// The first cue list: the engine has no other notion of an active list, and the main list
    /// is first.
    fn active_list(&self) -> Option<CueListId> {
        self.0.workspace.cue_lists.first().map(|l| l.id)
    }

    fn cue_by_number(&self, number: &str) -> Option<CueId> {
        self.find(|c| c.number.as_deref() == Some(number))
    }

    fn cue_exists(&self, id: CueId) -> bool {
        self.0.workspace.find_cue(id).is_some()
    }

    fn cue_property(&self, id: CueId, field: CueField) -> Option<Value> {
        let cue = self.0.workspace.find_cue(id)?;
        Some(match field {
            CueField::Name => json!(cue.name),
            CueField::Number => json!(cue.number.as_deref().unwrap_or("")),
            CueField::Notes => json!(cue.notes),
            CueField::PreWait => json!(cue.pre_wait),
            CueField::PostWait => json!(cue.post_wait),
            CueField::Armed => json!(cue.armed),
            CueField::Flagged => json!(cue.flagged),
        })
    }

    fn cue_by_osc_trigger(&self, address: &str) -> Option<CueId> {
        self.find(|c| c.triggers.osc.as_deref() == Some(address))
    }
}

#[cfg(test)]
mod tests {
    use openlab_core::protocol::PlayheadEntry;
    use openlab_core::{
        ContinueMode, CueColor, CueKind, CueList, GroupMode, SecondTrigger, TransportHotkeys,
        Triggers, WorkspaceSettings,
    };
    use uuid::Uuid;

    use super::*;

    fn cue_id(n: u128) -> CueId {
        CueId(Uuid::from_u128(n))
    }

    fn list_id() -> CueListId {
        CueListId(Uuid::from_u128(900))
    }

    fn cue(n: u128) -> Cue {
        Cue {
            id: cue_id(n),
            number: Some(n.to_string()),
            name: format!("cue {n}"),
            notes: String::new(),
            color: CueColor::None,
            flagged: false,
            armed: true,
            pre_wait: 0.0,
            post_wait: 0.0,
            continue_mode: ContinueMode::DoNotContinue,
            triggers: Triggers {
                hotkey: None,
                osc: None,
            },
            second_trigger: SecondTrigger::Nothing,
            second_trigger_on_release: false,
            kind: CueKind::Group {
                mode: GroupMode::Timeline,
                children: vec![],
            },
        }
    }

    fn mirror(cues: u128) -> Mirror {
        let workspace = Workspace {
            id: WorkspaceId(Uuid::from_u128(1)),
            name: "t".into(),
            settings: WorkspaceSettings {
                min_go_interval: 0.0,
                panic_duration: 0.1,
                autosave_interval: 30.0,
                osc_passcode: None,
                master_db: 0.0,
                hotkeys: TransportHotkeys::default(),
                auto_number_start: 1.0,
                auto_number_increment: 1.0,
            },
            patches: vec![],
            network_patches: vec![],
            cue_lists: vec![CueList {
                id: list_id(),
                name: "Main".into(),
                cues: (1..=cues).map(cue).collect(),
            }],
        };
        Mirror::from_snapshot(Snapshot {
            revision: 0,
            runtime_seq: 0,
            document: DocumentState {
                path: None,
                dirty: false,
                can_undo: false,
                can_redo: false,
            },
            workspace,
            cue_states: vec![],
            problems: vec![],
            group_warnings: vec![],
            playheads: vec![PlayheadEntry {
                list: list_id(),
                cue: Some(cue_id(1)),
            }],
            previewing: vec![],
            warnings: vec![],
        })
    }

    fn upsert(n: u128, index: u64) -> Upsert {
        Upsert::Cue {
            list: list_id(),
            parent: None,
            index,
            cue: Box::new(cue(n)),
        }
    }

    fn upserts(range: std::ops::RangeInclusive<u128>) -> Vec<Upsert> {
        range
            .map(|i| upsert(i, u64::try_from(i - 1).unwrap_or(0)))
            .collect()
    }

    #[test]
    fn revisions_are_stale_applied_or_a_gap() {
        let mut m = mirror(2);
        assert_eq!(m.model_changed(0, &[], &[]), Applied::Stale);
        assert_eq!(m.model_changed(2, &[], &[]), Applied::Resync);
        assert_eq!(m.model_changed(1, &[], &[]), Applied::Updates(vec![]));
        assert_eq!(m.model_changed(1, &[], &[]), Applied::Stale);
        assert_eq!(m.model_changed(2, &[], &[]), Applied::Updates(vec![]));
    }

    #[test]
    fn an_edit_is_a_cue_update_and_a_change_of_shape_a_workspace_update_too() {
        let mut m = mirror(2);
        // The cue is there: only its properties changed.
        assert_eq!(
            m.model_changed(1, &[upsert(1, 0)], &[]),
            Applied::Updates(vec![Update::Cue(cue_id(1))])
        );
        // A cue that was not there.
        assert_eq!(
            m.model_changed(2, &[upsert(3, 2)], &[]),
            Applied::Updates(vec![Update::Workspace, Update::Cue(cue_id(3))])
        );
        // A delete of one that is there; of one that is not, nothing about the shape.
        assert_eq!(
            m.model_changed(3, &[], &[Delete::Cue { id: cue_id(3) }]),
            Applied::Updates(vec![Update::Workspace, Update::Cue(cue_id(3))])
        );
        assert_eq!(
            m.model_changed(4, &[], &[Delete::Cue { id: cue_id(77) }]),
            Applied::Updates(vec![Update::Cue(cue_id(77))])
        );
        // A move is a delete and an upsert of the same cue: one cue update, and the shape.
        assert_eq!(
            m.model_changed(5, &[upsert(1, 1)], &[Delete::Cue { id: cue_id(1) }]),
            Applied::Updates(vec![Update::Workspace, Update::Cue(cue_id(1))])
        );
        // Other entities are workspace updates.
        let patch = Delete::Patch {
            id: openlab_core::PatchId(Uuid::from_u128(5)),
        };
        assert_eq!(
            m.model_changed(6, &[], &[patch]),
            Applied::Updates(vec![Update::Workspace])
        );
    }

    #[test]
    fn more_than_the_cap_of_cue_updates_become_one_workspace_update() {
        let n = MAX_CUE_UPDATES as u128;
        let mut m = mirror(n + 8);
        let Applied::Updates(updates) = m.model_changed(1, &upserts(1..=n), &[]) else {
            panic!("not applied")
        };
        assert_eq!(updates.len(), MAX_CUE_UPDATES);
        assert_eq!(
            m.model_changed(2, &upserts(1..=n + 1), &[]),
            Applied::Updates(vec![Update::Workspace])
        );
    }

    #[test]
    fn an_edit_that_does_not_apply_is_a_resync_and_the_revision_stays() {
        let mut m = mirror(1);
        let bad = Upsert::Cue {
            list: CueListId(Uuid::from_u128(12345)),
            parent: None,
            index: 0,
            cue: Box::new(cue(9)),
        };
        assert_eq!(m.model_changed(1, &[bad], &[]), Applied::Resync);
        assert_eq!(m.revision, 0);
    }

    #[test]
    fn runtime_seq_and_playheads() {
        let mut m = mirror(2);
        let head = |cue| RuntimeChange::Playhead {
            list: list_id(),
            cue,
        };
        let moved = |cue| {
            Applied::Updates(vec![Update::Playhead {
                list: list_id(),
                cue,
            }])
        };
        assert_eq!(m.runtime(0, &[]), Applied::Stale);
        assert_eq!(m.runtime(2, &[]), Applied::Resync);
        // The playhead was on cue 1 in the snapshot: staying there says nothing.
        assert_eq!(
            m.runtime(1, &[head(Some(cue_id(1)))]),
            Applied::Updates(vec![])
        );
        assert_eq!(
            m.runtime(2, &[head(Some(cue_id(2)))]),
            moved(Some(cue_id(2)))
        );
        assert_eq!(
            m.runtime(3, &[head(Some(cue_id(2)))]),
            Applied::Updates(vec![])
        );
        assert_eq!(m.runtime(4, &[head(None)]), moved(None));
        // Cue states: one update per cue, cue updates before playhead updates.
        let state = |n| RuntimeChange::CueState {
            cue: cue_id(n),
            state: openlab_core::protocol::WireCueState::Running,
            reason: None,
        };
        assert_eq!(
            m.runtime(5, &[head(Some(cue_id(1))), state(2), state(2)]),
            Applied::Updates(vec![
                Update::Cue(cue_id(2)),
                Update::Playhead {
                    list: list_id(),
                    cue: Some(cue_id(1))
                }
            ])
        );
    }

    #[test]
    fn a_document_with_another_path_is_a_resync() {
        let mut m = mirror(1);
        let doc = |path: Option<&str>, dirty| DocumentState {
            path: path.map(Into::into),
            dirty,
            can_undo: false,
            can_redo: false,
        };
        assert_eq!(m.document(&doc(None, true)), Applied::Updates(vec![]));
        assert_eq!(m.document(&doc(Some("/a"), false)), Applied::Resync);
    }

    #[test]
    fn a_replacement_reports_the_workspace_and_the_moved_playheads() {
        let old = mirror(2);
        assert!(old.updates_to(&mirror(2)).is_empty());
        let mut changed = mirror(3);
        changed.playheads.insert(list_id(), Some(cue_id(3)));
        assert_eq!(
            old.updates_to(&changed),
            vec![
                Update::Workspace,
                Update::Playhead {
                    list: list_id(),
                    cue: Some(cue_id(3))
                }
            ]
        );
    }

    #[test]
    fn lookups_walk_groups_in_tree_order() {
        let mut m = mirror(2);
        let mut child = cue(11);
        child.number = Some("2".into());
        child.triggers.osc = Some("/x".into());
        let mut group = cue(10);
        group.kind = CueKind::Group {
            mode: GroupMode::Timeline,
            children: vec![child],
        };
        m.workspace.cue_lists[0].cues.insert(0, group);
        let show = MirrorShow(&m);
        // The group's child comes before the later cue numbered "2".
        assert_eq!(show.cue_by_number("2"), Some(cue_id(11)));
        assert_eq!(show.cue_by_number("1"), Some(cue_id(1)));
        assert_eq!(show.cue_by_number("nope"), None);
        assert_eq!(show.cue_by_osc_trigger("/x"), Some(cue_id(11)));
        assert!(show.cue_exists(cue_id(11)));
        assert_eq!(show.active_list(), Some(list_id()));
    }
}
