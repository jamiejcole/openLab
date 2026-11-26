// SPDX-License-Identifier: GPL-3.0-or-later

//! Reading a QLab 5 workspace file (`.qlab5`) into a new openLab workspace. Bytes in, workspace
//! and report out: the file is an Apple keyed archive whose cue lists are a second keyed
//! archive held in it as data ([`plist`], [`archive`]). QLab's own class and field names are
//! used here as the file stores them. Any input is either read or refused with a [`QlabError`]: never a panic,
//! never unbounded work ([`MAX_QLAB_FILE_BYTES`], [`plist::MAX_READS`], [`MAX_QLAB_CUES`],
//! [`archive::MAX_ARCHIVES`], [`plist::MAX_STRING_BYTES`]).

mod archive;
mod convert;
mod plist;
mod report;

use std::collections::HashSet;

use uuid::Uuid;

use crate::cue::{CueKind, MediaRef};
use crate::model::{CueId, CueList, CueListId, PatchId, RouteId, Workspace};
use crate::validate::{ValidationIssue, repair};

pub use report::*;

/// Largest QLab file read, bytes (the show-file limit).
pub const MAX_QLAB_FILE_BYTES: usize = crate::show_file::MAX_SHOW_FILE_BYTES;
/// Most cues one QLab file may hold (lists and carts not counted).
pub const MAX_QLAB_CUES: usize = 20_000;
/// The QLab major version this reader knows.
pub const QLAB_MAJOR: u32 = 5;

/// Why a file was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum QlabError {
    #[error("not a property list")]
    NotPlist,
    #[error("not a keyed archive")]
    NotKeyedArchive,
    #[error("not a QLab workspace")]
    NotQlab,
    #[error("saved by QLab {0}; only QLab 5 workspaces can be read")]
    TooOld(String),
    #[error("needs QLab {0} or later, newer than openLab can read")]
    TooNew(String),
    #[error("the file is damaged ({0})")]
    Damaged(&'static str),
    #[error("the file is too large ({0})")]
    TooLarge(&'static str),
    #[error("the file is too complex to read")]
    TooComplex,
}

/// A media file as QLab stored it: relative to the `.qlab5` file's folder, and where it was
/// when last saved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QlabFile {
    pub relative: Option<String>,
    pub last_known: Option<String>,
}

/// What an unsupported cue was converted to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placeholder {
    /// A red Memo cue.
    Cue(CueId),
    /// A cart's cue list.
    List(CueListId),
}

/// One unsupported cue and its placeholder in the converted workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QlabPlaceholder {
    pub qlab_id: String,
    pub at: Placeholder,
}

/// A QLab workspace converted, before the operator's choices.
#[derive(Clone, Debug)]
pub struct QlabImport {
    /// Every unsupported cue as a placeholder; targets resolved; not yet repaired.
    pub workspace: Workspace,
    pub report: QlabReport,
    pub placeholders: Vec<QlabPlaceholder>,
    /// QLab audio patch id to the new patch.
    pub patches: Vec<(String, PatchId)>,
    /// QLab video output route id to the new route.
    pub routes: Vec<(String, RouteId)>,
    /// QLab cue id to the new cue, for every cue converted (placeholders included).
    pub cues: Vec<(String, CueId)>,
}

/// Converts a QLab 5 workspace file. `ids` gives each new id (core draws no randomness);
/// `media` turns a QLab file reference into the cue's media reference (the engine looks for
/// the file; core does no I/O).
pub fn read_qlab(
    bytes: &[u8],
    ids: &mut dyn FnMut() -> Uuid,
    media: &mut dyn FnMut(&QlabFile) -> Option<MediaRef>,
) -> Result<QlabImport, QlabError> {
    if bytes.len() > MAX_QLAB_FILE_BYTES {
        return Err(QlabError::TooLarge("file"));
    }
    convert::convert(bytes, ids, media)
}

impl QlabImport {
    /// Applies the operator's choices (an unsupported cue not named keeps its placeholder),
    /// then repairs the workspace as a load does. `list` is the id of an empty cue list, used
    /// only if every list was deleted. The repairs are the load warnings.
    pub fn finish(
        mut self,
        choices: &[QlabCueChoice],
        list: CueListId,
    ) -> (Workspace, Vec<ValidationIssue>) {
        apply_qlab_choices(&mut self.workspace, &self.placeholders, choices, list);
        let warnings = repair(&mut self.workspace);
        (self.workspace, warnings)
    }
}

/// Deletes the placeholders the operator chose to delete: a cue (a group keeps its other
/// children), or a cart's cue list with its cues. A target that named a deleted cue becomes
/// none. The workspace keeps at least one cue list (an empty one with id `list`). Returns the
/// cues deleted.
pub fn apply_qlab_choices(
    workspace: &mut Workspace,
    placeholders: &[QlabPlaceholder],
    choices: &[QlabCueChoice],
    list: CueListId,
) -> Vec<CueId> {
    let delete: HashSet<&str> = choices
        .iter()
        .filter(|c| c.action == QlabCueAction::Delete)
        .map(|c| c.qlab_id.as_str())
        .collect();
    let mut cues: HashSet<CueId> = HashSet::new();
    let mut lists: HashSet<CueListId> = HashSet::new();
    for p in placeholders
        .iter()
        .filter(|p| delete.contains(p.qlab_id.as_str()))
    {
        match p.at {
            Placeholder::Cue(id) => {
                cues.insert(id);
            }
            Placeholder::List(id) => {
                lists.insert(id);
            }
        }
    }
    let mut removed = Vec::new();
    for l in workspace.cue_lists.iter().filter(|l| lists.contains(&l.id)) {
        removed.extend(crate::tree::descendants(&l.cues).map(|c| c.id));
    }
    workspace.cue_lists.retain(|l| !lists.contains(&l.id));
    for l in &mut workspace.cue_lists {
        remove_cues(&mut l.cues, &cues, &mut removed);
    }
    if workspace.cue_lists.is_empty() {
        workspace.cue_lists.push(CueList {
            id: list,
            name: "Main Cue List".to_owned(),
            cues: Vec::new(),
        });
    }
    let gone: HashSet<CueId> = removed.iter().copied().collect();
    let mut stack: Vec<&mut Vec<crate::Cue>> = workspace
        .cue_lists
        .iter_mut()
        .map(|l| &mut l.cues)
        .collect();
    while let Some(cues) = stack.pop() {
        for cue in cues.iter_mut() {
            if cue.kind.target().is_some_and(|t| gone.contains(&t)) {
                cue.kind.set_target(None);
            }
            if let CueKind::Target { new_target, .. } = &mut cue.kind
                && new_target.is_some_and(|t| gone.contains(&t))
            {
                *new_target = None;
            }
            if let CueKind::Group { children, .. } = &mut cue.kind {
                stack.push(children);
            }
        }
    }
    removed
}

/// Removes the cues in `ids` from `cues` at any depth (iteratively), recording every cue
/// removed with what was inside it.
fn remove_cues(cues: &mut Vec<crate::Cue>, ids: &HashSet<CueId>, removed: &mut Vec<CueId>) {
    let mut stack: Vec<&mut Vec<crate::Cue>> = vec![cues];
    while let Some(level) = stack.pop() {
        for cue in level.iter().filter(|c| ids.contains(&c.id)) {
            removed.push(cue.id);
            removed.extend(crate::tree::descendants(cue.children()).map(|c| c.id));
        }
        level.retain(|c| !ids.contains(&c.id));
        for cue in level.iter_mut() {
            if let CueKind::Group { children, .. } = &mut cue.kind {
                stack.push(children);
            }
        }
    }
}
