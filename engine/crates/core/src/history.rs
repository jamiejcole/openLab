// SPDX-License-Identifier: GPL-3.0-or-later

//! Unlimited undo and redo (ED-07). The engine owns one [`History`] per open workspace and
//! changes the workspace only through it.

use crate::command::{Command, CommandError, Inverse};
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

/// One step that undoes `older` and `newer` (the step of the edit after it). Two steps that set
/// the same single value are undone by the older alone; otherwise `newer` is undone first.
/// Chains stay flat, as in a transaction.
fn merge(older: Inverse, newer: Inverse) -> Inverse {
    if older.same_slot(&newer) {
        return older;
    }
    let mut commands = Vec::new();
    for step in [newer, older] {
        match step {
            Command::Transaction { commands: inner } => commands.extend(inner),
            other => commands.push(other),
        }
    }
    Command::Transaction { commands }
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
    match workspace.apply(command.clone()) {
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
