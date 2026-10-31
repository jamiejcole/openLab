// SPDX-License-Identifier: GPL-3.0-or-later

//! A doc's workspace and the cue engine that plays it, kept together so the engine can never
//! miss an edit (docs/control.md, "Model changes"). The workspace is reachable for writing only
//! through [`DocModel::edit`], whose guard tells the engine when it goes: every edit is
//! signalled by construction, and the engine's next `handle` follows it at the cost of a flag
//! check when nothing changed.

use std::ops::{Deref, DerefMut};

use openlab_core::{
    Action, CommandError, CueEngine, EngineConfigError, Footprint, Input, SampleTime, Workspace,
};

/// A doc's workspace and cue engine. Both fields are private to this module: the rest of the
/// control loop reads them through [`Self::ws`] and [`Self::engine`] and changes the workspace
/// only through [`Self::edit`].
#[derive(Debug)]
pub(super) struct DocModel {
    ws: Workspace,
    engine: CueEngine,
}

impl DocModel {
    /// `ws` played by `engine`, which is told the workspace is new (its next call follows all
    /// of it).
    pub(super) fn new(ws: Workspace, mut engine: CueEngine) -> Self {
        engine.model_changed();
        Self { ws, engine }
    }

    pub(super) fn ws(&self) -> &Workspace {
        &self.ws
    }

    pub(super) fn engine(&self) -> &CueEngine {
        &self.engine
    }

    /// [`CueEngine::handle`] on this workspace.
    pub(super) fn handle(&mut self, input: Input, now: SampleTime) -> Vec<Action> {
        self.engine.handle(&self.ws, input, now)
    }

    /// [`CueEngine::set_lookahead`].
    pub(super) fn set_lookahead(&mut self, lookahead: SampleTime) -> Result<(), EngineConfigError> {
        self.engine.set_lookahead(lookahead)
    }

    /// Write access to the workspace. The engine is told of the edit when the guard goes: of
    /// the whole workspace unless [`WorkspaceEdit::touched`] names what changed.
    pub(super) fn edit(&mut self) -> WorkspaceEdit<'_> {
        WorkspaceEdit {
            ws: &mut self.ws,
            engine: &mut self.engine,
            told: false,
        }
    }
}

/// A write borrow of a doc's workspace (see [`DocModel::edit`]). Dropping it tells the cue
/// engine the whole workspace may have changed.
pub(super) struct WorkspaceEdit<'a> {
    ws: &'a mut Workspace,
    engine: &'a mut CueEngine,
    /// The engine was told already ([`Self::touched`]); dropping adds nothing.
    told: bool,
}

impl WorkspaceEdit<'_> {
    /// Ends the edit, telling the engine it changed only what `footprint` names (a command,
    /// undo or redo: `Command::footprint`), so its static problems are brought up to date for
    /// those cues alone (NFR-24). A model-wide footprint (lists, patches, settings) tells it the
    /// whole workspace changed. A footprint that misses a change is counted by the edit
    /// cross-check (docs/cue-engine.md), as for a missed signal.
    pub(super) fn touched(mut self, footprint: &Footprint) {
        if !footprint.is_model_wide() {
            self.engine.model_changed_cues(footprint);
            self.told = true;
        }
    }

    /// Ends an edit that `result` reports on: [`Self::touched`] with `footprint`, the edit's
    /// (an edit refused for anything but a failed rollback changed nothing, so it holds), or
    /// the whole workspace after a failed rollback, which may have left any part changed.
    pub(super) fn ended<T>(self, footprint: &Footprint, result: &Result<T, CommandError>) {
        if !matches!(result, Err(CommandError::RollbackFailed { .. })) {
            self.touched(footprint);
        }
    }
}

impl Deref for WorkspaceEdit<'_> {
    type Target = Workspace;

    fn deref(&self) -> &Workspace {
        self.ws
    }
}

impl DerefMut for WorkspaceEdit<'_> {
    fn deref_mut(&mut self) -> &mut Workspace {
        self.ws
    }
}

impl Drop for WorkspaceEdit<'_> {
    fn drop(&mut self) {
        if !self.told {
            self.engine.model_changed();
        }
    }
}
