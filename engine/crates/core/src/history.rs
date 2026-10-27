// SPDX-License-Identifier: GPL-3.0-or-later

//! Unlimited undo and redo (ED-07). The engine owns one [`History`] per open workspace and
//! changes the workspace only through it.

use crate::command::{Command, CommandError, CueProperty, Inverse};
use crate::command_model::PatchProperty;
use crate::model::Workspace;

/// The undo and redo stacks. Each entry is the command that reverses one step, so redo needs no
/// memory of the original command.
///
/// If the workspace is replaced or changed behind the history's back (reload, revert), call
/// [`History::clear`]: entries refer to cues by id and would be refused or misapply.
#[derive(Clone, Debug, Default)]
pub struct History {
    undo: Vec<Inverse>,
    redo: Vec<Inverse>,
    /// The coalesce key of the gesture whose step is on top of `undo`, while it is open.
    open: Option<String>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies `command` as one undo step and clears redo. A refused command changes nothing,
    /// neither workspace nor history. Returns whether the command changed anything: a change
    /// to the value a field already has (and an empty transaction) is not an edit and is not
    /// recorded.
    pub fn execute(
        &mut self,
        workspace: &mut Workspace,
        command: Command,
    ) -> Result<bool, CommandError> {
        self.execute_coalescing(workspace, command, None)
    }

    /// As [`execute`](Self::execute). With a `key`, an edit that follows another edit with the
    /// same key (nothing else, no undo or redo in between) joins that edit's undo step: one
    /// fader drag is one step however many values it sent. A client uses a fresh key for each
    /// gesture. Steps that set the same single value collapse to the oldest (so a long drag
    /// stays one command); others are chained, newest undone first.
    pub fn execute_coalescing(
        &mut self,
        workspace: &mut Workspace,
        command: Command,
        key: Option<&str>,
    ) -> Result<bool, CommandError> {
        let inverse = workspace.apply(command)?;
        if is_empty_transaction(&inverse) {
            // Not an edit: an open gesture stays open.
            return Ok(false);
        }
        self.redo.clear();
        let joins = key.is_some() && self.open.as_deref() == key;
        match self.undo.pop() {
            Some(older) if joins => self.undo.push(merge(older, inverse)),
            Some(older) => {
                self.undo.push(older);
                self.undo.push(inverse);
            }
            None => self.undo.push(inverse),
        }
        self.open = key.map(str::to_owned);
        Ok(true)
    }

    /// Undoes the latest step. `Ok(false)` if there is nothing to undo. If the step is refused
    /// it stays on the stack and the workspace is unchanged.
    pub fn undo(&mut self, workspace: &mut Workspace) -> Result<bool, CommandError> {
        self.open = None;
        step(&mut self.undo, &mut self.redo, workspace)
    }

    /// Redoes the latest undone step. `Ok(false)` if there is nothing to redo.
    pub fn redo(&mut self, workspace: &mut Workspace) -> Result<bool, CommandError> {
        self.open = None;
        step(&mut self.redo, &mut self.undo, workspace)
    }

    /// The command the next [`undo`](Self::undo) applies: its [`footprint`](Command::footprint)
    /// is what that undo changes.
    pub fn peek_undo(&self) -> Option<&Command> {
        self.undo.last()
    }

    /// As [`peek_undo`](Self::peek_undo), for [`redo`](Self::redo).
    pub fn peek_redo(&self) -> Option<&Command> {
        self.redo.last()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.open = None;
    }
}

/// One step that undoes `older` and `newer` (the step of the edit after it), `newer` first.
///
/// Dropping a command of `newer` is only done where it is provably redundant, because applying
/// the rest of the chain around it would otherwise see a different state:
/// - `newer` and `older` are each one command setting the same value (or moving the same item):
///   `older` alone is the step.
/// - `newer` only sets values (cue properties, settings, patch and list names), one command
///   per item, with no add, remove or move: each of its commands for a value `older` also sets
///   is left out. The values are independent, so `older`, which comes later, wins. This keeps
///   a multi-cue level drag at one command per cue however long it runs.
///
/// Anything else is chained flat, which is always right.
fn merge(older: Inverse, newer: Inverse) -> Inverse {
    let flat = |step: Inverse| match step {
        Command::Transaction { commands } => commands,
        other => vec![other],
    };
    let (older, newer) = (flat(older), flat(newer));
    if let ([a], [b]) = (older.as_slice(), newer.as_slice())
        && a.same_slot(b)
    {
        return a.clone();
    }
    let droppable = only_value_sets(&newer);
    let mut commands: Vec<Command> = newer
        .into_iter()
        .filter(|step| {
            !(droppable && older.iter().any(|o| o.same_slot(step)) && kind_is_safe(step, &older))
        })
        .collect();
    commands.extend(older);
    match <[Command; 1]>::try_from(commands) {
        Ok([only]) => only,
        Err(commands) => Command::Transaction { commands },
    }
}

/// Whether every command only sets a value, and no two are about the same item.
fn only_value_sets(commands: &[Command]) -> bool {
    let mut items = std::collections::HashSet::new();
    commands.iter().all(|c| {
        let item = match c {
            Command::SetCueProperty { id, .. } => id.0,
            Command::SetWorkspaceSetting { .. } => uuid::Uuid::nil(),
            Command::SetPatchProperty {
                property: PatchProperty::ChannelCount { .. } | PatchProperty::Outputs { .. },
                ..
            } => return false,
            Command::SetPatchProperty { id, .. } => id.0,
            Command::SetNetworkPatchProperty { id, .. } => id.0,
            Command::RenameCueList { id, .. } => id.0,
            _ => return false,
        };
        items.insert(item)
    })
}

/// A `Kind` replaces a cue's target and group mode too: it may only be dropped if the older
/// chain, which then runs on the state before it, sets neither on that cue.
fn kind_is_safe(step: &Command, older: &[Command]) -> bool {
    let Command::SetCueProperty {
        id,
        property: CueProperty::Kind { .. },
    } = step
    else {
        return true;
    };
    !older.iter().any(|c| {
        matches!(
            c,
            Command::SetCueProperty {
                id: other,
                property: CueProperty::Target { .. } | CueProperty::GroupMode { .. },
            } if other == id
        )
    })
}

fn is_empty_transaction(command: &Command) -> bool {
    matches!(command, Command::Transaction { commands } if commands.is_empty())
}

/// Pops `from`, applies it, and pushes what reverses it onto `to`.
fn step(
    from: &mut Vec<Inverse>,
    to: &mut Vec<Inverse>,
    workspace: &mut Workspace,
) -> Result<bool, CommandError> {
    let Some(command) = from.pop() else {
        return Ok(false);
    };
    // Applying consumes the command, so keep a copy to put back if it is refused.
    match workspace.apply_restoring(command.clone()) {
        Ok(reverse) => {
            to.push(reverse);
            Ok(true)
        }
        Err(error) => {
            from.push(command);
            Err(error)
        }
    }
}
