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

mod numbers;
mod properties;
mod structure;

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::command_model::{
    HotkeyClash, NetworkPatchProperty, OscRightsTarget, PatchProperty, WorkspaceSetting,
    apply_model, no_change,
};
use crate::cue::{
    ContinueMode, Cue, CueColor, CueKind, GroupMode, MediaRef, OscArg, SecondTrigger, Triggers,
};
use crate::model::{
    CueId, CueListId, NetworkPatch, NetworkPatchId, OutputPatch, PatchId, Workspace,
};
use crate::network::MessageIssue;
use crate::osc_access::OscRights;
use crate::tree::{find_path, height, walk_mut};
use crate::validate::{IssueKind, MAX_GROUP_DEPTH, ValidationIssue, visit};
pub use numbers::next_auto_number;

use numbers::{renumber, set_numbers};
use properties::{batch_set, set_property};
use structure::{insert, position_of, relocate, remove};

/// What undoes a command. Applying it gives the command that redoes the original.
pub type Inverse = Command;

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
    /// Only for Audio cues: the file alone, so a replaced file leaves levels, routing, fades and
    /// slices as they are. `None` clears it. The engine stores a path inside the show folder
    /// relative to it ([`Command::prepare`]).
    Media {
        value: Option<MediaRef>,
    },
    /// Only for Network cues. A new value must name an existing network patch.
    Destination {
        value: NetworkPatchId,
    },
    /// Only for Network cues. An OSC address pattern (`network::check_message`).
    Address {
        value: String,
    },
    /// Only for Network cues. At most `MAX_OSC_ARGS` arguments within the message size limits.
    Args {
        value: Vec<OscArg>,
    },
    /// Only for Wait and Fade cues: how long the action lasts, in seconds (CUE-02, CT-20, CT-10).
    /// Finite, from 0 to `MAX_TIME`. A running cue keeps the end it had when its action started
    /// (docs/cue-engine.md, Model edits).
    Duration {
        value: f64,
    },
    /// Only for Load cues: the time it loads its target to, in seconds (CT-14). Finite, from 0 to
    /// `MAX_TIME`; read when the Load cue's action starts.
    LoadToTime {
        value: f64,
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
    /// Adds the cues of a clipboard copied anywhere, in order, as one step (ED-05). The engine
    /// turns it into a [`Command::Paste`] with [`prepare_paste`] and fresh ids before applying
    /// it ([`Command::prepare`]); core's `apply` refuses it with [`CommandError::NotPrepared`].
    PasteClipboard {
        at: Position,
        cues: Vec<Cue>,
        /// The folder of the show the cues were copied from, absolute, as the client knows it;
        /// absent for a show never saved. The cues' relative media paths are read against it, so
        /// the files still play when pasted into a show in another folder (ED-05, WS-03).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_dir: Option<String>,
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
    /// Renames the workspace: the show title (`name` of the settings upsert), not the file.
    RenameWorkspace {
        name: String,
    },
    /// Adds a row to the OSC passcode table at `index` (0 to the current count; NET-02). The
    /// passcode is 1 to 32 digits, not already in the table; Edit and Control need View; the
    /// table holds at most 16.
    AddOscPasscode {
        index: usize,
        passcode: String,
        rights: OscRights,
    },
    /// Removes a row. Any passcode may go, the last included: with no row and No Passcode
    /// without rights nobody can connect, which the user can undo.
    RemoveOscPasscode {
        passcode: String,
    },
    /// Sets the rights of a row, or of the No Passcode row. Edit and Control need View.
    SetOscRights {
        target: OscRightsTarget,
        rights: OscRights,
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
    #[error("cue list {0} still has cues")]
    CueListNotEmpty(CueListId),
    #[error("cue list {0} is the only one")]
    LastCueList(CueListId),
    #[error("patch {patch} has no output {output}")]
    PatchOutputOutOfRange { patch: PatchId, output: usize },
    #[error("{0}")]
    InvalidValue(&'static str),
    /// A workspace key edit broke a key-map rule (TRG-01); `field` is the key's show-file path.
    #[error("{field}: {reason}")]
    HotkeyRefused {
        field: &'static str,
        reason: HotkeyClash,
    },
    #[error("the Network cue's message is invalid: {0}")]
    InvalidMessage(MessageIssue),
    #[error("values are invalid: {}", join(.0))]
    InvalidModel(Vec<ValidationIssue>),
    #[error("transactions cannot be nested")]
    NestedTransaction,
    #[error("command {index} of the transaction was refused: {source}")]
    InTransaction {
        index: usize,
        source: Box<CommandError>,
    },
    /// Command `index` of a transaction was refused and undoing the commands before it failed
    /// too (`rollback`), which only a wrong inverse can cause. Unlike every other error the
    /// workspace is partly changed (still valid: each command is atomic); [`History`] drops its
    /// steps, which no longer fit it.
    ///
    /// [`History`]: crate::History
    #[error(
        "command {index} of the transaction was refused ({source}) and undoing the commands \
         before it failed ({rollback}); the workspace is partly changed"
    )]
    RollbackFailed {
        index: usize,
        source: Box<CommandError>,
        rollback: Box<CommandError>,
    },
    /// An undo or redo would stop this cue, which is active or being previewed (M5.10d). Its
    /// step stays on the stack and nothing was changed. The binary words the message the client
    /// sees with the cue's number and name.
    #[error("cue {0} is playing; stop it, then try again")]
    CueIsActive(CueId),
    /// A `PasteClipboard` reached `apply` without [`Command::prepare`]: a bug in the caller.
    #[error("the pasted cues were not prepared")]
    NotPrepared,
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
    /// name load only warns about): an undo must never get stuck on one. For [`History`] only:
    /// it skips value checks, so it must never be given a command from a client.
    ///
    /// [`History`]: crate::History
    pub(crate) fn apply_restoring(&mut self, command: Command) -> Result<Inverse, CommandError> {
        match command {
            Command::Transaction { commands } => transaction(self, commands, true),
            other => single(self, other, true),
        }
    }
}

/// Applies `commands` in order as one step. If one is refused, the steps already applied are
/// undone with their own inverses, newest first: every inverse restores exactly what its command
/// changed, so this costs what the transaction touched, not the size of the show. Debug and
/// test builds also clone the workspace up front and check the rollback against it.
fn transaction(
    ws: &mut Workspace,
    commands: Vec<Command>,
    restoring: bool,
) -> Result<Inverse, CommandError> {
    #[cfg(feature = "fault-injection")]
    let (commands, rollback_fails) = fault::apply(commands);
    transaction_with(ws, commands, restoring, |ws, step| {
        #[cfg(feature = "fault-injection")]
        if rollback_fails {
            return Err(CommandError::Internal);
        }
        single(ws, step, true)
    })
}

/// Test seam (feature `fault-injection`, per thread): the next transaction after `skip` others
/// has command `at` replaced by one that is refused and, if `rollback_fails`, every rollback
/// step refused too, which is what a wrong inverse does. One shot.
#[cfg(feature = "fault-injection")]
pub mod fault {
    use std::cell::Cell;

    use super::{Command, CueId};

    thread_local! {
        static NEXT: Cell<Option<(usize, usize, bool)>> = const { Cell::new(None) };
    }

    pub fn inject(skip: usize, at: usize, rollback_fails: bool) {
        NEXT.with(|n| n.set(Some((skip, at, rollback_fails))));
    }

    pub(super) fn apply(mut commands: Vec<Command>) -> (Vec<Command>, bool) {
        let Some((skip, at, rollback_fails)) = NEXT.with(Cell::take) else {
            return (commands, false);
        };
        if skip > 0 {
            NEXT.with(|n| n.set(Some((skip - 1, at, rollback_fails))));
            return (commands, false);
        }
        if let Some(command) = commands.get_mut(at) {
            *command = Command::RemoveCue {
                id: CueId(uuid::Uuid::from_u128(u128::MAX)),
            };
        }
        (commands, rollback_fails)
    }
}

/// [`transaction`] with the rollback's `apply` as a parameter, so a test can make it fail.
fn transaction_with(
    ws: &mut Workspace,
    commands: Vec<Command>,
    restoring: bool,
    undo: impl FnMut(&mut Workspace, Command) -> Result<Inverse, CommandError>,
) -> Result<Inverse, CommandError> {
    #[cfg(debug_assertions)]
    let snapshot = ws.clone();
    let mut undo_steps = Vec::with_capacity(commands.len());
    for (index, command) in commands.into_iter().enumerate() {
        match single(ws, command, restoring) {
            Ok(inverse) => undo_steps.push(inverse),
            Err(error) => {
                let source = Box::new(error);
                roll_back(ws, undo_steps, undo).map_err(|rollback| {
                    CommandError::RollbackFailed {
                        index,
                        source: source.clone(),
                        rollback: Box::new(rollback),
                    }
                })?;
                // Debug text rather than `==`, so a NaN a hostile workspace may hold compares
                // equal to itself and -0.0 differs from 0.0.
                #[cfg(debug_assertions)]
                debug_assert_eq!(
                    format!("{ws:?}"),
                    format!("{snapshot:?}"),
                    "transaction rollback did not restore the workspace exactly"
                );
                return Err(CommandError::InTransaction { index, source });
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

/// Applies `undo_steps` newest first; a step that is a transaction (the inverse of a paste or
/// batch edit) is applied in its own order. Each step runs on exactly the state its command
/// left, so it is never refused unless a command's inverse is wrong. If one is, rolling back
/// stops there: the workspace is valid (every command is atomic) but partly changed, and the
/// error says so.
fn roll_back(
    ws: &mut Workspace,
    undo_steps: Vec<Inverse>,
    mut apply: impl FnMut(&mut Workspace, Command) -> Result<Inverse, CommandError>,
) -> Result<(), CommandError> {
    for step in undo_steps.into_iter().rev() {
        let commands = match step {
            Command::Transaction { commands } => commands,
            other => vec![other],
        };
        for command in commands {
            apply(ws, command)?;
        }
    }
    Ok(())
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
            insert(ws, &at, vec![cue], restoring)?;
            Ok(Command::RemoveCue { id })
        }
        Command::Paste { at, cues } => {
            let ids: Vec<CueId> = cues.iter().map(|c| c.id).collect();
            insert(ws, &at, cues, restoring)?;
            Ok(Command::Transaction {
                commands: ids
                    .into_iter()
                    .map(|id| Command::RemoveCue { id })
                    .collect(),
            })
        }
        Command::PasteClipboard { .. } => Err(CommandError::NotPrepared),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WorkspaceId;
    use uuid::Uuid;

    fn list(n: u128) -> CueListId {
        CueListId(Uuid::from_u128(n))
    }

    fn sample() -> Workspace {
        Workspace::new_empty(
            WorkspaceId(Uuid::from_u128(1)),
            list(1),
            PatchId(Uuid::from_u128(1)),
        )
    }

    /// A rollback step that is refused (only a wrong inverse could be) stops the rollback and
    /// says so, with both errors; it does not panic, even in a debug build, and the workspace
    /// is left as far as the rollback got: valid, partly changed.
    #[test]
    fn a_refused_rollback_step_is_reported_not_hidden() {
        let mut ws = sample();
        let commands = vec![
            Command::RenameCueList {
                id: list(1),
                name: "Renamed".into(),
            },
            Command::AddCueList {
                index: 1,
                id: list(2),
                name: "Second".into(),
            },
            Command::RemoveCueList { id: list(99) },
        ];
        let mut calls = Vec::new();
        let error = transaction_with(&mut ws, commands, false, |_, step| {
            calls.push(step);
            Err(CommandError::Internal)
        })
        .unwrap_err();
        assert_eq!(
            error,
            CommandError::RollbackFailed {
                index: 2,
                source: Box::new(CommandError::CueListNotFound(list(99))),
                rollback: Box::new(CommandError::Internal),
            }
        );
        // Newest first, and nothing after the refused step.
        assert_eq!(calls, vec![Command::RemoveCueList { id: list(2) }]);
        assert_eq!(ws.cue_lists.len(), 2);
        assert_eq!(ws.cue_lists.first().unwrap().name, "Renamed");
        assert!(error.to_string().contains("partly changed"));
    }

    /// Steps are undone newest first; a step that is a transaction (the inverse of a paste or a
    /// batch edit) is applied in its own order, not reversed.
    #[test]
    fn rollback_applies_steps_newest_first_and_a_transaction_step_in_its_own_order() {
        let mut ws = sample();
        let name = |n: &str| Command::RenameCueList {
            id: list(1),
            name: n.into(),
        };
        let steps = vec![
            name("a"),
            Command::Transaction {
                commands: vec![name("b"), name("c")],
            },
            name("d"),
        ];
        let mut seen = Vec::new();
        roll_back(&mut ws, steps, |ws, step| {
            seen.push(step.clone());
            single(ws, step, true)
        })
        .unwrap();
        assert_eq!(seen, vec![name("d"), name("b"), name("c"), name("a")]);
        assert_eq!(ws.cue_lists.first().unwrap().name, "a");
    }
}
