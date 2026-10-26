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
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies `command` as one undo step and clears redo. A refused command changes nothing,
    /// neither workspace nor history. An empty transaction is not an edit and is ignored.
    pub fn execute(
        &mut self,
        workspace: &mut Workspace,
        command: Command,
    ) -> Result<(), CommandError> {
        let inverse = workspace.apply(command)?;
        if !is_empty_transaction(&inverse) {
            self.undo.push(inverse);
            self.redo.clear();
        }
        Ok(())
    }

    /// Undoes the latest step. `Ok(false)` if there is nothing to undo. If the step is refused
    /// it stays on the stack and the workspace is unchanged.
    pub fn undo(&mut self, workspace: &mut Workspace) -> Result<bool, CommandError> {
        step(&mut self.undo, &mut self.redo, workspace)
    }

    /// Redoes the latest undone step. `Ok(false)` if there is nothing to redo.
    pub fn redo(&mut self, workspace: &mut Workspace) -> Result<bool, CommandError> {
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
    }
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
