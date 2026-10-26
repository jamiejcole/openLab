// SPDX-License-Identifier: GPL-3.0-or-later

//! What an edit changed, as the `model_changed` message a client applies (NFR-01, ED-07).
//!
//! The control thread must tell clients exactly what each command, undo or redo did, without
//! diffing a 2,000-cue workspace. Commands touch cues by id and nothing else, so a [`Footprint`]
//! read off the command (or the undo step, which is a command too) names every cue that may
//! differ; [`model_changes`] then reads those cues out of the new workspace. What is emitted is
//! the size of the footprint, not of the show.

use std::collections::BTreeSet;

use crate::command::Command;
use crate::cue::{Cue, CueKind};
use crate::model::{CueId, CueList, Workspace};
use crate::protocol::{Delete, Upsert};

/// The cues a command may have changed, by how.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Footprint {
    /// New cues (the top of what was added; a group's descendants come with it).
    pub added: BTreeSet<CueId>,
    /// Removed (a group takes its descendants with it).
    pub removed: BTreeSet<CueId>,
    /// Moved to another place in the tree.
    pub moved: BTreeSet<CueId>,
    /// Same place, different content.
    pub edited: BTreeSet<CueId>,
}

impl Footprint {
    /// A runtime change to some cues' content (`SetArmed`).
    pub fn edited(ids: impl IntoIterator<Item = CueId>) -> Self {
        Self {
            edited: ids.into_iter().collect(),
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.moved.is_empty()
            && self.edited.is_empty()
    }
}

impl Command {
    /// Every cue this command can change. A refused command changes nothing, so this is only
    /// read for commands that were applied.
    pub fn footprint(&self) -> Footprint {
        let mut footprint = Footprint::default();
        self.collect(&mut footprint);
        footprint
    }

    fn collect(&self, fp: &mut Footprint) {
        match self {
            Self::AddCue { cue, .. } => {
                fp.added.insert(cue.id);
            }
            Self::Paste { cues, .. } => fp.added.extend(cues.iter().map(|c| c.id)),
            Self::RemoveCue { id } => {
                fp.removed.insert(*id);
            }
            Self::MoveCue { id, .. } | Self::ReparentCue { id, .. } => {
                fp.moved.insert(*id);
            }
            Self::SetCueProperty { id, .. } => {
                fp.edited.insert(*id);
            }
            Self::BatchSet { ids, .. } | Self::Renumber { ids, .. } => {
                fp.edited.extend(ids.iter().copied());
            }
            Self::SetCueNumbers { numbers } => fp.edited.extend(numbers.iter().map(|n| n.id)),
            Self::Transaction { commands } => {
                for command in commands {
                    command.collect(fp);
                }
            }
        }
    }
}

/// The `model_changed` payload `(upserts, deletes)` for an edit with `footprint`, read from the
/// workspace after it.
///
/// Clients apply `deletes` first, then `upserts` in order (`protocol.md`). Every removed or
/// moved cue is deleted first, so a client never has to reason about where a mover used to be;
/// the upserts then put each cue that exists at its final index, parents before children and
/// ascending within one container, so each lands where the workspace has it. A touched cue
/// inside a touched group is not upserted again: the group's upsert carries its subtree.
pub fn model_changes(workspace: &Workspace, footprint: &Footprint) -> (Vec<Upsert>, Vec<Delete>) {
    let deletes = footprint
        .removed
        .union(&footprint.moved)
        .map(|&id| Delete::Cue { id })
        .collect();
    let wanted: BTreeSet<CueId> = footprint
        .added
        .iter()
        .chain(&footprint.moved)
        .chain(&footprint.edited)
        .chain(&footprint.removed)
        .copied()
        .collect();
    let mut upserts = Vec::new();
    if wanted.is_empty() {
        return (upserts, deletes);
    }
    for list in &workspace.cue_lists {
        // Iterative pre-order walk: a hostile tree cannot overflow the stack. Each entry is the
        // next cue to visit: its container, the container's group, its index, and whether a
        // touched group above it already carries it.
        let mut work: Vec<(&[Cue], Option<CueId>, usize, bool)> =
            vec![(&list.cues, None, 0, false)];
        while let Some((cues, parent, index, covered)) = work.pop() {
            let Some(cue) = cues.get(index) else {
                continue;
            };
            work.push((cues, parent, index + 1, covered));
            let touched = wanted.contains(&cue.id);
            if touched && !covered {
                upserts.push(Upsert::Cue {
                    list: list.id,
                    parent,
                    index: index as u64,
                    cue: Box::new(cue.clone()),
                });
            }
            if matches!(cue.kind, CueKind::Group { .. }) {
                work.push((cue.children(), Some(cue.id), 0, covered || touched));
            }
        }
    }
    (upserts, deletes)
}

/// Why a `model_changed` could not be applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ApplyChangesError {
    #[error("cue list not found")]
    NoList,
    #[error("parent group not found")]
    NoParent,
    #[error("index past the end")]
    BadIndex,
}

impl Workspace {
    /// Applies a `model_changed` the way a client must: `deletes`, then `upserts` in order. A
    /// delete of an id that is not there is ignored; an upsert of a cue already there replaces
    /// it wherever it is. This is the reference for client implementations and what the tests
    /// check the engine's messages against.
    pub fn apply_changes(
        &mut self,
        upserts: &[Upsert],
        deletes: &[Delete],
    ) -> Result<(), ApplyChangesError> {
        for delete in deletes {
            match delete {
                Delete::Cue { id } => {
                    take_cue(self, *id);
                }
                Delete::CueList { id } => self.cue_lists.retain(|l| l.id != *id),
                Delete::Patch { id } => self.patches.retain(|p| p.id != *id),
                Delete::NetworkPatch { id } => self.network_patches.retain(|p| p.id != *id),
            }
        }
        for upsert in upserts {
            match upsert {
                Upsert::Cue {
                    list,
                    parent,
                    index,
                    cue,
                } => {
                    take_cue(self, cue.id);
                    let list_index = self
                        .cue_lists
                        .iter()
                        .position(|l| l.id == *list)
                        .ok_or(ApplyChangesError::NoList)?;
                    let parents = match parent {
                        None => Vec::new(),
                        Some(parent) => {
                            crate::tree::find_path(self, *parent)
                                .filter(|p| p.list == list_index)
                                .ok_or(ApplyChangesError::NoParent)?
                                .indices
                        }
                    };
                    let cues = crate::tree::container_mut(self, list_index, &parents)
                        .ok_or(ApplyChangesError::NoParent)?;
                    let index = usize::try_from(*index).map_err(|_| ApplyChangesError::BadIndex)?;
                    crate::tree::insert_all(cues, index, vec![(**cue).clone()])
                        .map_err(|_| ApplyChangesError::BadIndex)?;
                }
                Upsert::CueList { id, name, index } => {
                    let cues = match self.cue_lists.iter().position(|l| l.id == *id) {
                        Some(at) => self.cue_lists.remove(at).cues,
                        None => Vec::new(),
                    };
                    let list = CueList {
                        id: *id,
                        name: name.clone(),
                        cues,
                    };
                    insert_at(&mut self.cue_lists, *index, list)?;
                }
                Upsert::Patch { index, patch } => {
                    self.patches.retain(|p| p.id != patch.id);
                    insert_at(&mut self.patches, *index, patch.clone())?;
                }
                Upsert::NetworkPatch { index, patch } => {
                    self.network_patches.retain(|p| p.id != patch.id);
                    insert_at(&mut self.network_patches, *index, patch.clone())?;
                }
                Upsert::Settings { name, settings } => {
                    self.name.clone_from(name);
                    self.settings = (**settings).clone();
                }
            }
        }
        Ok(())
    }
}

fn insert_at<T>(items: &mut Vec<T>, index: u64, item: T) -> Result<(), ApplyChangesError> {
    let index = usize::try_from(index).map_err(|_| ApplyChangesError::BadIndex)?;
    if index > items.len() {
        return Err(ApplyChangesError::BadIndex);
    }
    items.insert(index, item);
    Ok(())
}

fn take_cue(ws: &mut Workspace, id: CueId) -> Option<Cue> {
    let path = crate::tree::find_path(ws, id)?;
    let (parents, index) = path.split()?;
    let cues = crate::tree::container_mut(ws, path.list, parents)?;
    crate::tree::take(cues, index)
}
