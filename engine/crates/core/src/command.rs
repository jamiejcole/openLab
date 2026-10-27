// SPDX-License-Identifier: GPL-3.0-or-later

//! Editing commands (ED-03, ED-04, ED-05, ED-07, ED-08). Every mutation of the cue tree is a
//! [`Command`]; applying one returns its [`Inverse`], which is itself a command.
//!
//! Rules every command follows:
//! - A refused command leaves the workspace exactly as it was.
//! - A command never introduces an [`Error`](crate::Severity::Error) or a value problem that
//!   [`repair`](crate::repair) would have to fix (NFR-03): duplicate ids or numbers, nesting
//!   beyond [`MAX_GROUP_DEPTH`], non-finite or out-of-range times and levels. So `save_show`
//!   accepts the result.
//! - References are not checked: a target, patch or destination that does not exist, a wrong
//!   kind of target and trigger cycles are the broken-cue state of ARCHITECTURE §10, which
//!   [`validate`](crate::validate) reports as warnings. Removing a cue leaves such references
//!   behind, and undo has to be able to put back states that contain them. UI code picks targets
//!   from existing cues; [`prepare_paste`] drops references a pasted cue cannot keep.
//! - Applying the inverse of a command restores the workspace exactly, ids and order included.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::command_model::{
    NetworkPatchProperty, PatchProperty, WorkspaceSetting, apply_model, no_change,
};
use crate::cue::{ContinueMode, Cue, CueColor, CueKind, GroupMode, SecondTrigger, Triggers};
use crate::model::{
    CueId, CueListId, NetworkPatch, NetworkPatchId, OutputPatch, PatchId, Workspace,
};
use crate::tree::{
    CuePath, container, container_mut, cue_at, cue_at_mut, find_path, height, insert_all, take,
    walk_mut,
};
use crate::validate::{
    IssueKind, MAX_AUTO_NUMBER, MAX_GROUP_DEPTH, ValidationIssue, subtree_issues, visit,
};

/// What undoes a command. Applying it gives the command that redoes the original.
pub type Inverse = Command;

/// Largest cue number auto-numbering reads from existing cues; larger ones are ignored.
const NUMBERING_LIMIT: f64 = 1e12;

/// Where a cue goes: inside the cue list `list`, directly (no `parent`) or inside the group
/// `parent`, as the child at `index`. `index` counts the cues already there, after the moved
/// cue has been taken out when it is being moved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub list: CueListId,
    pub parent: Option<CueId>,
    pub index: usize,
}

/// One editable property of a cue. Each variant is the whole new value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CueProperty {
    Name {
        value: String,
    },
    Notes {
        value: String,
    },
    Color {
        value: CueColor,
    },
    Flagged {
        value: bool,
    },
    Armed {
        value: bool,
    },
    PreWait {
        value: f64,
    },
    PostWait {
        value: f64,
    },
    ContinueMode {
        value: ContinueMode,
    },
    Triggers {
        value: Triggers,
    },
    /// Any cue. `play_next`/`play_previous` act only on Playlist groups; elsewhere they are kept
    /// but act as `nothing`, and validation warns (a group mode change can make them apply).
    SecondTrigger {
        value: SecondTrigger,
    },
    SecondTriggerOnRelease {
        value: bool,
    },
    /// Only for kinds that have a target.
    Target {
        value: Option<CueId>,
    },
    /// Only for Groups. Children are not touched.
    GroupMode {
        value: GroupMode,
    },
    /// Replaces everything kind-specific: levels, routing, fades, slices, network arguments.
    /// Neither the old nor the new kind may be a Group: add and remove cues for that.
    Kind {
        value: CueKind,
    },
}

/// A cue's number. `None` clears it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NumberAssignment {
    pub id: CueId,
    pub number: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// Adds one cue, with its children if it is a Group. With `auto_number`, its number is
    /// replaced by [`next_auto_number`] (ED-08).
    AddCue {
        at: Position,
        cue: Cue,
        #[serde(default)]
        auto_number: bool,
    },
    /// Adds several cues, in order, as one step. Ids must be fresh: see [`prepare_paste`].
    Paste {
        at: Position,
        cues: Vec<Cue>,
    },
    /// Removes a cue and its children.
    RemoveCue {
        id: CueId,
    },
    /// Moves a cue to `index` among the cues it is already beside.
    MoveCue {
        id: CueId,
        index: usize,
    },
    /// Moves a cue, with its children, to another group or cue list (ED-03).
    ReparentCue {
        id: CueId,
        to: Position,
    },
    SetCueProperty {
        id: CueId,
        property: CueProperty,
    },
    /// Sets one property on several cues (ED-04); all or none. Not for `Kind`.
    BatchSet {
        ids: Vec<CueId>,
        property: CueProperty,
    },
    /// Sets or clears several numbers at once. The numbers after the change must be unique,
    /// so cues may swap numbers.
    SetCueNumbers {
        numbers: Vec<NumberAssignment>,
    },
    /// Numbers `ids` `start`, `start + increment`, … in cue list order, whatever order `ids` is
    /// in (ED-08). A group is numbered before its children, if both are listed.
    Renumber {
        ids: Vec<CueId>,
        start: f64,
        increment: f64,
    },
    /// Several commands as one undo step. If one is refused, none is applied. May not contain
    /// another transaction.
    Transaction {
        commands: Vec<Command>,
    },
    /// One workspace setting (ED-07; PB-02, PB-03, PB-11, NET-02, NFR-05, ED-08, TRG-01).
    SetWorkspaceSetting {
        setting: WorkspaceSetting,
    },
    /// Adds an output patch at `index` (0 to the current count).
    AddPatch {
        index: usize,
        patch: OutputPatch,
    },
    /// Removes an output patch. Refused while a cue routes to it, unless `even_if_used`
    /// (the inverse of an add sets it: undo must restore exactly).
    RemovePatch {
        id: PatchId,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        even_if_used: bool,
    },
    /// Moves a patch to `index` among the patches (counted after taking it out).
    MovePatch {
        id: PatchId,
        index: usize,
    },
    SetPatchProperty {
        id: PatchId,
        property: PatchProperty,
    },
    AddNetworkPatch {
        index: usize,
        patch: NetworkPatch,
    },
    /// As [`Command::RemovePatch`], for the cues' network destinations.
    RemoveNetworkPatch {
        id: NetworkPatchId,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        even_if_used: bool,
    },
    MoveNetworkPatch {
        id: NetworkPatchId,
        index: usize,
    },
    SetNetworkPatchProperty {
        id: NetworkPatchId,
        property: NetworkPatchProperty,
    },
    /// Adds an empty cue list at `index` (0 to the current count).
    AddCueList {
        index: usize,
        id: CueListId,
        name: String,
    },
    /// Removes an empty cue list. Refused if it holds cues (remove or move them first) or if
    /// it is the only list.
    RemoveCueList {
        id: CueListId,
    },
    MoveCueList {
        id: CueListId,
        index: usize,
    },
    RenameCueList {
        id: CueListId,
        name: String,
    },
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum CommandError {
    #[error("cue {0} does not exist")]
    CueNotFound(CueId),
    #[error("cue list {0} does not exist")]
    CueListNotFound(CueListId),
    #[error("cue {parent} is not in cue list {list}")]
    ParentNotInList { parent: CueId, list: CueListId },
    #[error("cue {0} is not a group")]
    NotAGroup(CueId),
    #[error("position {index} is past the {len} cues there")]
    IndexOutOfRange { index: usize, len: usize },
    #[error("cue id {0} is already in use")]
    DuplicateCueId(CueId),
    #[error("cues cannot be nested deeper than {MAX_GROUP_DEPTH} levels")]
    NestingTooDeep,
    #[error("cue {0} cannot be moved into itself")]
    MoveIntoSelf(CueId),
    #[error("number \"{number}\" is already used by cue {used_by}")]
    DuplicateNumber { number: String, used_by: CueId },
    #[error("cue {0}: a number cannot be empty; clear it instead")]
    EmptyNumber(CueId),
    #[error("cue {0} appears twice in one number assignment")]
    DuplicateAssignment(CueId),
    #[error("invalid auto-numbering: {0}")]
    InvalidNumbering(&'static str),
    #[error("cue {0} does not have that property")]
    PropertyNotApplicable(CueId),
    #[error("a cue's kind cannot be replaced by or with a Group (cue {0})")]
    KindChangeInvolvesGroup(CueId),
    #[error("that property cannot be set on several cues at once")]
    NotBatchable,
    #[error("cue values are invalid: {}", join(.0))]
    InvalidCue(Vec<ValidationIssue>),
    #[error("patch {0} does not exist")]
    PatchNotFound(PatchId),
    #[error("network patch {0} does not exist")]
    NetworkPatchNotFound(NetworkPatchId),
    #[error("patch id {0} is already in use")]
    DuplicatePatchId(PatchId),
    #[error("network patch id {0} is already in use")]
    DuplicateNetworkPatchId(NetworkPatchId),
    #[error("cue list id {0} is already in use")]
    DuplicateCueListId(CueListId),
    #[error("patch {patch} is used by cue {cue}")]
    PatchInUse { patch: PatchId, cue: CueId },
    #[error("network patch {patch} is used by cue {cue}")]
    NetworkPatchInUse { patch: NetworkPatchId, cue: CueId },
    /// Refused by the engine: patch positions are mixer slots, so patches cannot be inserted,
    /// removed, moved or resized while any voice is on the device (until M4.15b).
    #[error(
        "audio is playing: patches cannot be inserted, removed, moved or resized until it stops"
    )]
    PatchPlaying,
    #[error("cue list {0} still has cues")]
    CueListNotEmpty(CueListId),
    #[error("cue list {0} is the only one")]
    LastCueList(CueListId),
    #[error("patch {patch} has no output {output}")]
    PatchOutputOutOfRange { patch: PatchId, output: usize },
    #[error("{0}")]
    InvalidValue(&'static str),
    #[error("values are invalid: {}", join(.0))]
    InvalidModel(Vec<ValidationIssue>),
    #[error("transactions cannot be nested")]
    NestedTransaction,
    #[error("command {index} of the transaction was refused: {source}")]
    InTransaction {
        index: usize,
        source: Box<CommandError>,
    },
    /// The cue tree was not as expected. Nothing was changed.
    #[error("internal inconsistency in the cue tree; command refused")]
    Internal,
}

fn join(issues: &[ValidationIssue]) -> String {
    issues
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

impl Workspace {
    /// Applies `command`. On success returns the command that undoes it; on failure the
    /// workspace is exactly as it was.
    pub fn apply(&mut self, command: Command) -> Result<Inverse, CommandError> {
        match command {
            Command::Transaction { commands } => transaction(self, commands, false),
            other => single(self, other, false),
        }
    }

    /// Applies an undo or redo step. It puts back a state the workspace had, so it is not
    /// refused for a value problem that state already had when the show was loaded (a hotkey
    /// name load only warns about): an undo must never get stuck on one.
    pub fn apply_restoring(&mut self, command: Command) -> Result<Inverse, CommandError> {
        match command {
            Command::Transaction { commands } => transaction(self, commands, true),
            other => single(self, other, true),
        }
    }
}

fn transaction(
    ws: &mut Workspace,
    commands: Vec<Command>,
    restoring: bool,
) -> Result<Inverse, CommandError> {
    let snapshot = ws.clone();
    let mut undo_steps = Vec::with_capacity(commands.len());
    for (index, command) in commands.into_iter().enumerate() {
        match single(ws, command, restoring) {
            Ok(inverse) => undo_steps.push(inverse),
            Err(error) => {
                *ws = snapshot;
                return Err(CommandError::InTransaction {
                    index,
                    source: Box::new(error),
                });
            }
        }
    }
    // Undo in reverse order. A step that is itself a transaction (batch edits) is spliced in,
    // so inverses never nest.
    let commands = undo_steps
        .into_iter()
        .rev()
        .flat_map(|step| match step {
            Command::Transaction { commands } => commands,
            other => vec![other],
        })
        .collect();
    Ok(Command::Transaction { commands })
}

/// Everything except a plain transaction. Each arm is atomic.
fn single(ws: &mut Workspace, command: Command, restoring: bool) -> Result<Inverse, CommandError> {
    match command {
        Command::AddCue {
            at,
            mut cue,
            auto_number,
        } => {
            if auto_number {
                cue.number = Some(next_auto_number(ws)?);
            }
            let id = cue.id;
            insert(ws, &at, vec![cue])?;
            Ok(Command::RemoveCue { id })
        }
        Command::Paste { at, cues } => {
            let ids: Vec<CueId> = cues.iter().map(|c| c.id).collect();
            insert(ws, &at, cues)?;
            Ok(Command::Transaction {
                commands: ids
                    .into_iter()
                    .map(|id| Command::RemoveCue { id })
                    .collect(),
            })
        }
        Command::RemoveCue { id } => remove(ws, id),
        Command::MoveCue { id, index } => {
            let path = find_path(ws, id).ok_or(CommandError::CueNotFound(id))?;
            let from = position_of(ws, &path).ok_or(CommandError::Internal)?;
            let to = Position {
                index,
                ..from.clone()
            };
            relocate(ws, id, &path, &to)?;
            if to == from {
                return Ok(no_change());
            }
            Ok(Command::MoveCue {
                id,
                index: from.index,
            })
        }
        Command::ReparentCue { id, to } => {
            let path = find_path(ws, id).ok_or(CommandError::CueNotFound(id))?;
            let from = position_of(ws, &path).ok_or(CommandError::Internal)?;
            relocate(ws, id, &path, &to)?;
            if to == from {
                return Ok(no_change());
            }
            Ok(Command::ReparentCue { id, to: from })
        }
        Command::SetCueProperty { id, property } => set_property(ws, id, property, restoring),
        Command::BatchSet { ids, property } => {
            if matches!(property, CueProperty::Kind { .. }) {
                return Err(CommandError::NotBatchable);
            }
            batch_set(ws, ids, &property, restoring)
        }
        Command::SetCueNumbers { numbers } => set_numbers(ws, numbers),
        Command::Renumber {
            ids,
            start,
            increment,
        } => renumber(ws, &ids, start, increment),
        Command::Transaction { .. } => Err(CommandError::NestedTransaction),
        other => apply_model(ws, &other, restoring).unwrap_or(Err(CommandError::Internal)),
    }
}

// ---------------------------------------------------------------------------------------------
// Positions

/// A resolved [`Position`]: the cue list's index and the path to the container group.
struct Container {
    list: usize,
    parents: Vec<usize>,
}

fn resolve(ws: &Workspace, at: &Position) -> Result<Container, CommandError> {
    let list = ws
        .cue_lists
        .iter()
        .position(|l| l.id == at.list)
        .ok_or(CommandError::CueListNotFound(at.list))?;
    let parents = match at.parent {
        None => Vec::new(),
        Some(parent) => {
            let path = find_path(ws, parent).ok_or(CommandError::CueNotFound(parent))?;
            if path.list != list {
                return Err(CommandError::ParentNotInList {
                    parent,
                    list: at.list,
                });
            }
            let cue = cue_at(ws, &path).ok_or(CommandError::Internal)?;
            if !matches!(cue.kind, CueKind::Group { .. }) {
                return Err(CommandError::NotAGroup(parent));
            }
            path.indices
        }
    };
    Ok(Container { list, parents })
}

fn position_of(ws: &Workspace, path: &CuePath) -> Option<Position> {
    let (parents, index) = path.split()?;
    let parent = if parents.is_empty() {
        None
    } else {
        let parent_path = CuePath {
            list: path.list,
            indices: parents.to_vec(),
        };
        Some(cue_at(ws, &parent_path)?.id)
    };
    Some(Position {
        list: ws.cue_lists.get(path.list)?.id,
        parent,
        index,
    })
}

// ---------------------------------------------------------------------------------------------
// Add, paste, remove, move

fn insert(ws: &mut Workspace, at: &Position, cues: Vec<Cue>) -> Result<(), CommandError> {
    let dest = resolve(ws, at)?;
    let len = container(ws, dest.list, &dest.parents)
        .ok_or(CommandError::Internal)?
        .len();
    if at.index > len {
        return Err(CommandError::IndexOutOfRange {
            index: at.index,
            len,
        });
    }
    // Bound the nesting before anything recurses over the new cues.
    let depth = dest.parents.len() + 1;
    if depth - 1 + height(&cues, MAX_GROUP_DEPTH) > MAX_GROUP_DEPTH {
        return Err(CommandError::NestingTooDeep);
    }
    check_new_cues(ws, &cues, depth)?;
    let list = container_mut(ws, dest.list, &dest.parents).ok_or(CommandError::Internal)?;
    insert_all(list, at.index, cues).map_err(|_| CommandError::Internal)
}

/// Ids and numbers must be unique across the workspace and values valid.
fn check_new_cues(ws: &Workspace, cues: &[Cue], depth: usize) -> Result<(), CommandError> {
    let mut ids = HashSet::new();
    let mut numbers = HashMap::new();
    for list in &ws.cue_lists {
        visit(&list.cues, &mut |cue| {
            ids.insert(cue.id);
            if let Some(number) = &cue.number {
                numbers.insert(number.clone(), cue.id);
            }
        });
    }
    let mut problem = None;
    visit(cues, &mut |cue| {
        if problem.is_some() {
            return;
        }
        if !ids.insert(cue.id) {
            problem = Some(CommandError::DuplicateCueId(cue.id));
        } else if let Some(number) = &cue.number {
            if number.is_empty() {
                problem = Some(CommandError::EmptyNumber(cue.id));
            } else if let Some(&used_by) = numbers.get(number) {
                problem = Some(CommandError::DuplicateNumber {
                    number: number.clone(),
                    used_by,
                });
            } else {
                numbers.insert(number.clone(), cue.id);
            }
        }
    });
    if let Some(error) = problem {
        return Err(error);
    }
    refuse(subtree_issues(ws, cues, depth))
}

fn remove(ws: &mut Workspace, id: CueId) -> Result<Inverse, CommandError> {
    let path = find_path(ws, id).ok_or(CommandError::CueNotFound(id))?;
    let at = position_of(ws, &path).ok_or(CommandError::Internal)?;
    let (parents, index) = path.split().ok_or(CommandError::Internal)?;
    let list = container_mut(ws, path.list, parents).ok_or(CommandError::Internal)?;
    let cue = take(list, index).ok_or(CommandError::Internal)?;
    Ok(Command::AddCue {
        at,
        cue,
        auto_number: false,
    })
}

fn relocate(
    ws: &mut Workspace,
    id: CueId,
    path: &CuePath,
    to: &Position,
) -> Result<(), CommandError> {
    let dest = resolve(ws, to)?;
    if dest.list == path.list && dest.parents.starts_with(&path.indices) {
        return Err(CommandError::MoveIntoSelf(id));
    }
    let cue = cue_at(ws, path).ok_or(CommandError::Internal)?;
    let subtree = height(std::slice::from_ref(cue), MAX_GROUP_DEPTH);
    if dest.parents.len() + subtree > MAX_GROUP_DEPTH {
        return Err(CommandError::NestingTooDeep);
    }
    let (src_parents, src_index) = path.split().ok_or(CommandError::Internal)?;
    let len = container(ws, dest.list, &dest.parents)
        .ok_or(CommandError::Internal)?
        .len();
    let same = dest.list == path.list && dest.parents == src_parents;
    let len = if same { len.saturating_sub(1) } else { len };
    if to.index > len {
        return Err(CommandError::IndexOutOfRange {
            index: to.index,
            len,
        });
    }

    let source = container_mut(ws, path.list, src_parents).ok_or(CommandError::Internal)?;
    let cue = take(source, src_index).ok_or(CommandError::Internal)?;
    // Taking the cue out may have shifted the destination's path: resolve it again.
    let left_over = match resolve(ws, to)
        .ok()
        .and_then(|d| container_mut(ws, d.list, &d.parents))
    {
        Some(list) => insert_all(list, to.index, vec![cue]).err(),
        None => Some(vec![cue]),
    };
    if let Some(cue) = left_over {
        // Not expected after the checks above. Put the cue back; nothing else has changed.
        if let Some(list) = container_mut(ws, path.list, src_parents) {
            let _ = insert_all(list, src_index, cue);
        }
        return Err(CommandError::Internal);
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Properties

impl CueProperty {
    /// The same property with the value `cue` has now, or `None` if `cue` cannot have it.
    fn current(&self, cue: &Cue) -> Option<Self> {
        Some(match self {
            Self::Name { .. } => Self::Name {
                value: cue.name.clone(),
            },
            Self::Notes { .. } => Self::Notes {
                value: cue.notes.clone(),
            },
            Self::Color { .. } => Self::Color { value: cue.color },
            Self::Flagged { .. } => Self::Flagged { value: cue.flagged },
            Self::Armed { .. } => Self::Armed { value: cue.armed },
            Self::PreWait { .. } => Self::PreWait {
                value: cue.pre_wait,
            },
            Self::PostWait { .. } => Self::PostWait {
                value: cue.post_wait,
            },
            Self::ContinueMode { .. } => Self::ContinueMode {
                value: cue.continue_mode,
            },
            Self::Triggers { .. } => Self::Triggers {
                value: cue.triggers.clone(),
            },
            Self::SecondTrigger { .. } => Self::SecondTrigger {
                value: cue.second_trigger,
            },
            Self::SecondTriggerOnRelease { .. } => Self::SecondTriggerOnRelease {
                value: cue.second_trigger_on_release,
            },
            Self::Target { .. } => {
                if !cue.kind.has_target() {
                    return None;
                }
                Self::Target {
                    value: cue.kind.target(),
                }
            }
            Self::GroupMode { .. } => match &cue.kind {
                CueKind::Group { mode, .. } => Self::GroupMode { value: *mode },
                _ => return None,
            },
            Self::Kind { .. } => Self::Kind {
                value: cue.kind.clone(),
            },
        })
    }

    /// Whether `other` sets the same field (`History` coalescing).
    pub(crate) fn same_slot(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }

    /// Sets the property. Returns `false`, changing nothing, if `cue` cannot have it.
    fn write(&self, cue: &mut Cue) -> bool {
        match self {
            Self::Name { value } => cue.name.clone_from(value),
            Self::Notes { value } => cue.notes.clone_from(value),
            Self::Color { value } => cue.color = *value,
            Self::Flagged { value } => cue.flagged = *value,
            Self::Armed { value } => cue.armed = *value,
            Self::PreWait { value } => cue.pre_wait = *value,
            Self::PostWait { value } => cue.post_wait = *value,
            Self::ContinueMode { value } => cue.continue_mode = *value,
            Self::Triggers { value } => cue.triggers.clone_from(value),
            Self::SecondTrigger { value } => cue.second_trigger = *value,
            Self::SecondTriggerOnRelease { value } => cue.second_trigger_on_release = *value,
            Self::Target { value } => return cue.kind.set_target(*value),
            Self::GroupMode { value } => match &mut cue.kind {
                CueKind::Group { mode, .. } => *mode = *value,
                _ => return false,
            },
            Self::Kind { value } => cue.kind = value.clone(),
        }
        true
    }
}

/// A cue with every field valid, to check one new value on its own.
fn blank_cue(id: CueId) -> Cue {
    Cue {
        id,
        number: None,
        name: String::new(),
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
        kind: CueKind::Memo,
    }
}

fn set_property(
    ws: &mut Workspace,
    id: CueId,
    property: CueProperty,
    restoring: bool,
) -> Result<Inverse, CommandError> {
    let path = find_path(ws, id).ok_or(CommandError::CueNotFound(id))?;
    let cue = cue_at(ws, &path).ok_or(CommandError::Internal)?;
    if let CueProperty::Kind { value } = &property
        && (matches!(cue.kind, CueKind::Group { .. }) || matches!(value, CueKind::Group { .. }))
    {
        return Err(CommandError::KindChangeInvolvesGroup(id));
    }
    let old = property
        .current(cue)
        .ok_or(CommandError::PropertyNotApplicable(id))?;

    // The new value has to be valid by itself. Judging it inside the cue would excuse a bad
    // value whenever the cue already had a problem in the same field.
    let mut probe = blank_cue(id);
    if matches!(
        property,
        CueProperty::Kind { .. }
            | CueProperty::PreWait { .. }
            | CueProperty::PostWait { .. }
            | CueProperty::Triggers { .. }
    ) {
        property.write(&mut probe);
        if !restoring {
            // Only what the new value introduces: the old one may carry a warning from load.
            let mut was = blank_cue(id);
            old.write(&mut was);
            let existing = subtree_issues(ws, &[was], path.indices.len());
            let issues = subtree_issues(ws, &[probe], path.indices.len())
                .into_iter()
                .filter(|i| !existing.contains(i))
                .collect();
            refuse(issues)?;
        }
    }

    if old == property {
        return Ok(no_change());
    }
    let cue = cue_at_mut(ws, &path).ok_or(CommandError::Internal)?;
    if !property.write(cue) {
        return Err(CommandError::Internal);
    }
    Ok(Command::SetCueProperty { id, property: old })
}

/// `BatchSet`: applies the property in place, one cue at a time, keeping each cue's old value.
/// If a cue is refused, the cues already written get their old values back (in reverse, so a
/// repeated id also ends as it began). The cost follows the cues touched, not the show; the
/// workspace is not cloned. The inverse is the same flat list of `SetCueProperty` a
/// `Transaction` of the sets would give.
fn batch_set(
    ws: &mut Workspace,
    ids: Vec<CueId>,
    property: &CueProperty,
    restoring: bool,
) -> Result<Inverse, CommandError> {
    let mut undo_steps: Vec<Inverse> = Vec::with_capacity(ids.len());
    for (index, id) in ids.into_iter().enumerate() {
        match set_property(ws, id, property.clone(), restoring) {
            Ok(inverse) => undo_steps.push(inverse),
            Err(error) => {
                // Each inverse is a plain write of a value the cue held, so it cannot be
                // refused; `restoring` skips validation for the same reason.
                while let Some(step) = undo_steps.pop() {
                    if let Command::SetCueProperty { id, property } = step {
                        let _ = set_property(ws, id, property, true);
                    }
                }
                return Err(CommandError::InTransaction {
                    index,
                    source: Box::new(error),
                });
            }
        }
    }
    undo_steps.reverse();
    Ok(Command::Transaction {
        commands: undo_steps
            .into_iter()
            .flat_map(|step| match step {
                Command::Transaction { commands } => commands,
                other => vec![other],
            })
            .collect(),
    })
}

// ---------------------------------------------------------------------------------------------
// Numbers

/// The number a cue created now gets (ED-08): one step above the highest numeric cue number in
/// the workspace, but not below `auto_number_start`.
pub fn next_auto_number(ws: &Workspace) -> Result<String, CommandError> {
    let settings = &ws.settings;
    let (start, increment) = (settings.auto_number_start, settings.auto_number_increment);
    check_numbering(start, increment)?;
    let mut highest: Option<f64> = None;
    let mut used = HashSet::new();
    for list in &ws.cue_lists {
        visit(&list.cues, &mut |cue| {
            let Some(number) = &cue.number else { return };
            used.insert(number.as_str());
            if let Ok(value) = number.trim().parse::<f64>()
                && value.is_finite()
                && (0.0..=NUMBERING_LIMIT).contains(&value)
            {
                highest = Some(highest.map_or(value, |h| h.max(value)));
            }
        });
    }
    let next = highest.map_or(start, |h| start.max(h + increment));
    let text = format_number(next);
    if used.contains(text.as_str()) {
        return Err(CommandError::InvalidNumbering(
            "next number is already used",
        ));
    }
    Ok(text)
}

fn check_numbering(start: f64, increment: f64) -> Result<(), CommandError> {
    if !(start.is_finite() && (0.0..=MAX_AUTO_NUMBER).contains(&start)) {
        return Err(CommandError::InvalidNumbering(
            "start must be between 0 and the maximum",
        ));
    }
    if !(increment.is_finite() && increment > 0.0 && increment <= MAX_AUTO_NUMBER) {
        return Err(CommandError::InvalidNumbering(
            "increment must be above 0 and at most the maximum",
        ));
    }
    Ok(())
}

/// Plain decimal text without float noise: 3 → "3", 0.1 + 0.2 → "0.3".
fn format_number(value: f64) -> String {
    // Adding 0.0 turns −0.0 into 0.0.
    let rounded = (value * 1e9).round() / 1e9 + 0.0;
    rounded.to_string()
}

fn renumber(
    ws: &mut Workspace,
    ids: &[CueId],
    start: f64,
    increment: f64,
) -> Result<Inverse, CommandError> {
    check_numbering(start, increment)?;
    let wanted: HashSet<CueId> = ids.iter().copied().collect();
    let mut numbers = Vec::new();
    let mut found = HashSet::new();
    for list in &ws.cue_lists {
        visit(&list.cues, &mut |cue| {
            if wanted.contains(&cue.id) && found.insert(cue.id) {
                let step = numbers.len() as f64;
                numbers.push(NumberAssignment {
                    id: cue.id,
                    number: Some(format_number(start + step * increment)),
                });
            }
        });
    }
    if let Some(&missing) = ids.iter().find(|id| !found.contains(id)) {
        return Err(CommandError::CueNotFound(missing));
    }
    set_numbers(ws, numbers)
}

fn set_numbers(
    ws: &mut Workspace,
    assignments: Vec<NumberAssignment>,
) -> Result<Inverse, CommandError> {
    let mut wanted: HashMap<CueId, Option<String>> = HashMap::new();
    for NumberAssignment { id, number } in assignments {
        if number.as_deref() == Some("") {
            return Err(CommandError::EmptyNumber(id));
        }
        if wanted.insert(id, number).is_some() {
            return Err(CommandError::DuplicateAssignment(id));
        }
    }

    let mut seen: HashMap<String, CueId> = HashMap::new();
    let mut found = HashSet::new();
    let mut old = Vec::new();
    let mut problem = None;
    for list in &ws.cue_lists {
        visit(&list.cues, &mut |cue| {
            let number = match wanted.get(&cue.id) {
                Some(new) => {
                    found.insert(cue.id);
                    old.push(NumberAssignment {
                        id: cue.id,
                        number: cue.number.clone(),
                    });
                    new
                }
                None => &cue.number,
            };
            if let Some(number) = number
                && problem.is_none()
            {
                match seen.entry(number.clone()) {
                    Entry::Occupied(first) => {
                        problem = Some(CommandError::DuplicateNumber {
                            number: number.clone(),
                            used_by: *first.get(),
                        });
                    }
                    Entry::Vacant(slot) => {
                        slot.insert(cue.id);
                    }
                }
            }
        });
    }
    if let Some(missing) = wanted.keys().find(|id| !found.contains(id)) {
        return Err(CommandError::CueNotFound(*missing));
    }
    if let Some(error) = problem {
        return Err(error);
    }

    if old.iter().all(|o| wanted.get(&o.id) == Some(&o.number)) {
        return Ok(no_change());
    }
    for list in &mut ws.cue_lists {
        walk_mut(&mut list.cues, &mut |cue| {
            if let Some(number) = wanted.get(&cue.id) {
                cue.number.clone_from(number);
            }
        });
    }
    Ok(Command::SetCueNumbers { numbers: old })
}

// ---------------------------------------------------------------------------------------------
// Checks shared by the commands above

/// Turns issues a command would introduce into a refusal. `UnpatchedOutputs` is allowed: the
/// model keeps routing beyond a patch's outputs on purpose.
fn refuse(issues: Vec<ValidationIssue>) -> Result<(), CommandError> {
    let issues: Vec<ValidationIssue> = issues
        .into_iter()
        .filter(|issue| !matches!(issue.kind, IssueKind::UnpatchedOutputs { .. }))
        .collect();
    if issues.is_empty() {
        Ok(())
    } else if issues
        .iter()
        .any(|issue| issue.kind == IssueKind::NestingTooDeep)
    {
        Err(CommandError::NestingTooDeep)
    } else {
        Err(CommandError::InvalidCue(issues))
    }
}

// ---------------------------------------------------------------------------------------------
// Paste

/// Makes a copy of `clipboard` that [`Command::Paste`] accepts in `workspace`, which may be a
/// different workspace from the one it was copied in (ED-05). `new_id` supplies a fresh id for
/// every cue, children included; core cannot make ids itself.
///
/// - Targets inside the copied set follow to the copies; other targets are kept if the cue
///   exists in `workspace` and cleared if not.
/// - Numbers already used in `workspace` (or earlier in the copy) are cleared.
/// - An Audio cue's patch or a Network cue's destination missing from `workspace` becomes the
///   workspace's first one, if it has any.
///
/// Fails only if the clipboard is nested deeper than [`MAX_GROUP_DEPTH`].
pub fn prepare_paste(
    workspace: &Workspace,
    clipboard: &[Cue],
    mut new_id: impl FnMut() -> CueId,
) -> Result<Vec<Cue>, CommandError> {
    if height(clipboard, MAX_GROUP_DEPTH) > MAX_GROUP_DEPTH {
        return Err(CommandError::NestingTooDeep);
    }
    let mut cues = clipboard.to_vec();
    let mut renamed = HashMap::new();
    walk_mut(&mut cues, &mut |cue| {
        let fresh = new_id();
        renamed.insert(cue.id, fresh);
        cue.id = fresh;
    });

    let mut existing = HashSet::new();
    let mut used_numbers = HashSet::new();
    for list in &workspace.cue_lists {
        visit(&list.cues, &mut |cue| {
            existing.insert(cue.id);
            if let Some(number) = &cue.number {
                used_numbers.insert(number.clone());
            }
        });
    }
    let patches: HashSet<PatchId> = workspace.patches.iter().map(|p| p.id).collect();
    let network_patches: HashSet<NetworkPatchId> =
        workspace.network_patches.iter().map(|p| p.id).collect();
    let first_patch = workspace.patches.first().map(|p| p.id);
    let first_network = workspace.network_patches.first().map(|p| p.id);

    walk_mut(&mut cues, &mut |cue| {
        if let Some(number) = &cue.number
            && (number.is_empty() || !used_numbers.insert(number.clone()))
        {
            cue.number = None;
        }
        if let Some(target) = cue.kind.target() {
            let kept = renamed
                .get(&target)
                .copied()
                .or_else(|| existing.contains(&target).then_some(target));
            cue.kind.set_target(kept);
        }
        match &mut cue.kind {
            CueKind::Audio { patch, .. } if !patches.contains(patch) => {
                if let Some(first) = first_patch {
                    *patch = first;
                }
            }
            CueKind::Network { destination, .. } if !network_patches.contains(destination) => {
                if let Some(first) = first_network {
                    *destination = first;
                }
            }
            _ => {}
        }
    });
    Ok(cues)
}
