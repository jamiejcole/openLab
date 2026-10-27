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
use crate::model::{CueId, CueList, CueListId, NetworkPatchId, PatchId, Workspace};
use crate::protocol::{Delete, Upsert};

/// What a command did to one of the workspace's ordered collections (cue lists, patches,
/// network patches).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Touched<T: Ord> {
    /// Added, renamed, moved or edited: sent as an upsert at their final place.
    pub changed: BTreeSet<T>,
    pub removed: BTreeSet<T>,
    /// Something moved (or several items changed), so every member is sent: each upsert puts its item at its final index,
    /// ascending, which sorts the client's copy whatever the old order was.
    pub reordered: bool,
}

impl<T: Ord> Default for Touched<T> {
    fn default() -> Self {
        Self {
            changed: BTreeSet::new(),
            removed: BTreeSet::new(),
            reordered: false,
        }
    }
}

impl<T: Ord> Touched<T> {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.removed.is_empty() && !self.reordered
    }
}

/// The parts of the model a command may have changed, by how.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Footprint {
    /// Cue lists: added, renamed, moved, removed. Their cues are in the cue sets below.
    pub lists: Touched<CueListId>,
    pub patches: Touched<PatchId>,
    pub network_patches: Touched<NetworkPatchId>,
    /// Workspace settings changed.
    pub settings: bool,
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

    /// Whether the edit touched more than cues: lists, patches or settings. The cue engine then
    /// needs `model_changed` (patches decide which cues are broken), not
    /// `model_changed_cues`.
    pub fn is_model_wide(&self) -> bool {
        !(self.lists.is_empty()
            && self.patches.is_empty()
            && self.network_patches.is_empty()
            && !self.settings)
    }

    pub fn is_empty(&self) -> bool {
        !self.is_model_wide()
            && self.added.is_empty()
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
            Self::SetWorkspaceSetting { .. } => fp.settings = true,
            Self::AddPatch { patch, .. } => {
                fp.patches.changed.insert(patch.id);
            }
            Self::RemovePatch { id, .. } => {
                fp.patches.removed.insert(*id);
            }
            Self::MovePatch { id, .. } => {
                fp.patches.changed.insert(*id);
                fp.patches.reordered = true;
            }
            Self::SetPatchProperty { id, .. } => {
                fp.patches.changed.insert(*id);
            }
            Self::AddNetworkPatch { patch, .. } => {
                fp.network_patches.changed.insert(patch.id);
            }
            Self::RemoveNetworkPatch { id, .. } => {
                fp.network_patches.removed.insert(*id);
            }
            Self::MoveNetworkPatch { id, .. } => {
                fp.network_patches.changed.insert(*id);
                fp.network_patches.reordered = true;
            }
            Self::SetNetworkPatchProperty { id, .. } => {
                fp.network_patches.changed.insert(*id);
            }
            Self::AddCueList { id, .. } | Self::RenameCueList { id, .. } => {
                fp.lists.changed.insert(*id);
            }
            Self::RemoveCueList { id } => {
                fp.lists.removed.insert(*id);
            }
            Self::MoveCueList { id, .. } => {
                fp.lists.changed.insert(*id);
                fp.lists.reordered = true;
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
    let mut deletes: Vec<Delete> = footprint
        .removed
        .union(&footprint.moved)
        .map(|&id| Delete::Cue { id })
        .collect();
    let mut upserts = Vec::new();
    model_parts(workspace, footprint, &mut upserts, &mut deletes);
    let wanted: BTreeSet<CueId> = footprint
        .added
        .iter()
        .chain(&footprint.moved)
        .chain(&footprint.edited)
        .chain(&footprint.removed)
        .copied()
        .collect();
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

/// Settings, cue list, patch and network patch changes of an edit: put before the cue upserts
/// (cues are placed in lists). An item removed and put back in one step is upserted, not
/// deleted: deleting a list takes its cues with it.
fn model_parts(
    workspace: &Workspace,
    footprint: &Footprint,
    upserts: &mut Vec<Upsert>,
    deletes: &mut Vec<Delete>,
) {
    if footprint.settings {
        upserts.push(Upsert::Settings {
            name: workspace.name.clone(),
            settings: Box::new(workspace.settings.clone()),
        });
    }
    collection(
        &workspace.cue_lists,
        |l| l.id,
        &footprint.lists,
        |id| Delete::CueList { id },
        |l, index| Upsert::CueList {
            id: l.id,
            name: l.name.clone(),
            index,
        },
        upserts,
        deletes,
    );
    collection(
        &workspace.patches,
        |p| p.id,
        &footprint.patches,
        |id| Delete::Patch { id },
        |p, index| Upsert::Patch {
            index,
            patch: p.clone(),
        },
        upserts,
        deletes,
    );
    collection(
        &workspace.network_patches,
        |p| p.id,
        &footprint.network_patches,
        |id| Delete::NetworkPatch { id },
        |p, index| Upsert::NetworkPatch {
            index,
            patch: p.clone(),
        },
        upserts,
        deletes,
    );
}

/// Deletes for the `touched` items that are gone from `items`, upserts (in index order) for
/// those that are there, or for all of them if the order changed.
fn collection<T, Id: Ord + Copy>(
    items: &[T],
    id_of: impl Fn(&T) -> Id,
    touched: &Touched<Id>,
    delete: impl Fn(Id) -> Delete,
    upsert: impl Fn(&T, u64) -> Upsert,
    upserts: &mut Vec<Upsert>,
    deletes: &mut Vec<Delete>,
) {
    let present: BTreeSet<Id> = items.iter().map(&id_of).collect();
    deletes.extend(
        touched
            .removed
            .iter()
            .chain(&touched.changed)
            .filter(|id| !present.contains(id))
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(delete),
    );
    // One touched item can be put at its final index on its own: the others keep their relative
    // order. With two or more, upserting just those could leave a client's copy in another
    // order than ours (each upsert moves one item past the still-misplaced other), so the whole
    // collection is sent, ascending: each upsert then places its item after all before it are
    // final.
    let is_touched = |id: &Id| touched.changed.contains(id) || touched.removed.contains(id);
    let everything =
        touched.reordered || items.iter().filter(|i| is_touched(&id_of(i))).count() > 1;
    for (index, item) in items.iter().enumerate() {
        let id = id_of(item);
        if everything || is_touched(&id) {
            upserts.push(upsert(item, index as u64));
        }
    }
}

/// [`model_changes`] read through `index`, an index of `workspace` brought up to date with the
/// edit (M4.12c): it finds each named cue where the index has it instead of walking the cue
/// lists, so it costs the footprint, not the show. `None` when the index cannot serve (it has
/// duplicate ids, or it disagrees with the workspace): the caller then uses [`model_changes`],
/// which is the reference this must equal (property-tested).
pub fn model_changes_indexed(
    workspace: &Workspace,
    footprint: &Footprint,
    index: &crate::cue_index::CueIndex,
) -> Option<(Vec<Upsert>, Vec<Delete>)> {
    use crate::cue_index::CueContainer;
    if index.has_duplicates() {
        return None;
    }
    let mut deletes: Vec<Delete> = footprint
        .removed
        .union(&footprint.moved)
        .map(|&id| Delete::Cue { id })
        .collect();
    let mut upserts = Vec::new();
    model_parts(workspace, footprint, &mut upserts, &mut deletes);
    let wanted: BTreeSet<CueId> = footprint
        .added
        .iter()
        .chain(&footprint.moved)
        .chain(&footprint.edited)
        .chain(&footprint.removed)
        .copied()
        .collect();
    let mut found: Vec<(Vec<usize>, Upsert)> = Vec::new();
    for &id in &wanted {
        // Not in the workspace (removed): nothing to put back.
        let Some(container) = index.container(id) else {
            continue;
        };
        // A touched group above carries it.
        if index.ancestors(id).any(|g| wanted.contains(&g)) {
            continue;
        }
        let cue = index.locate(workspace, id)?;
        let parent = match container {
            CueContainer::Group(g) => Some(g),
            CueContainer::List(_) => None,
        };
        let upsert = Upsert::Cue {
            list: index.list_of(id)?,
            parent,
            index: u64::try_from(index.position(id)?).ok()?,
            cue: Box::new(cue.clone()),
        };
        found.push((index.path(id)?, upsert));
    }
    // Workspace order, as the walk emits them: parents before children, ascending within one
    // container.
    found.sort_by(|a, b| a.0.cmp(&b.0));
    upserts.extend(found.into_iter().map(|(_, u)| u));
    Some((upserts, deletes))
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
