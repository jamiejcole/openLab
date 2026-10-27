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
