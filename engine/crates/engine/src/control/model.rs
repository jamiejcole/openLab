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

    /// Ends a `History` edit that `result` reports on. Changed (`Ok(true)`): [`Self::touched`]
    /// with `footprint`, the edit's. Unchanged (`Ok(false)`: a value set to what it was, an
    /// empty transaction, nothing to undo or redo) or refused: `History` changed nothing (its
    /// contract, property-tested in core's `commands_prop`), so no cue is named and the
    /// follow-up costs nothing, even for a model-wide command a client repeats. A failed
    /// rollback may have left any part changed: the whole workspace.
    pub(super) fn ended(self, footprint: &Footprint, result: &Result<bool, CommandError>) {
        match result {
            Ok(true) => self.touched(footprint),
            Err(CommandError::RollbackFailed { .. }) => {}
            Ok(false) | Err(_) => self.touched(&Footprint::default()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use openlab_core::{CueId, CueListId, EngineConfig, PatchId, WorkspaceId};
    use uuid::Uuid;

    /// An empty workspace whose engine has followed it once.
    fn model() -> DocModel {
        let ws = Workspace::new_empty(
            WorkspaceId(Uuid::from_u128(1)),
            CueListId(Uuid::from_u128(2)),
            PatchId(Uuid::from_u128(3)),
        );
        let engine = CueEngine::new(EngineConfig::new(48_000, 64, 1).unwrap());
        let mut m = DocModel::new(ws, engine);
        m.handle(Input::Tick, 0);
        m
    }

    /// `(model_syncs, whole_show_passes)` before or after a call.
    fn counts(m: &DocModel) -> (u64, u64) {
        (m.engine().model_syncs(), m.engine().whole_show_passes())
    }

    fn after_a_call(m: &mut DocModel) -> (u64, u64) {
        m.handle(Input::Tick, 0);
        counts(m)
    }

    #[test]
    fn a_call_with_no_edit_follows_nothing() {
        let mut m = model();
        let before = counts(&m);
        assert_eq!(after_a_call(&mut m), before);
    }

    #[test]
    fn a_dropped_edit_tells_the_engine_of_the_whole_workspace() {
        let mut m = model();
        let (syncs, passes) = counts(&m);
        m.edit().name = "renamed".into();
        let (s, p) = after_a_call(&mut m);
        assert_eq!(s, syncs + 1);
        assert!(p > passes, "a full rebuild walks the workspace");
    }

    #[test]
    fn an_edit_ended_with_a_cue_footprint_tells_the_engine_of_those_cues_alone() {
        let mut m = model();
        let (syncs, passes) = counts(&m);
        m.edit()
            .touched(&Footprint::edited([CueId(Uuid::from_u128(9))]));
        assert_eq!(after_a_call(&mut m), (syncs + 1, passes));
    }

    /// A refusal or a no-op changed nothing: even a model-wide command's costs no rebuild, so a
    /// client repeating one cannot load the control thread with them.
    #[test]
    fn a_refused_or_unchanged_edit_names_no_cue_whatever_its_footprint() {
        let mut m = model();
        let (syncs, passes) = counts(&m);
        let wide = Footprint {
            settings: true,
            ..Footprint::default()
        };
        let refused: Result<bool, CommandError> = Err(CommandError::Internal);
        m.edit().ended(&wide, &refused);
        assert_eq!(after_a_call(&mut m), (syncs + 1, passes));
        // Applied but changed nothing (a setting set to what it was): the same.
        m.edit().ended(&wide, &Ok(false));
        assert_eq!(after_a_call(&mut m), (syncs + 2, passes));
    }

    #[test]
    fn a_model_wide_footprint_or_a_failed_rollback_tells_the_engine_of_the_whole_workspace() {
        let mut m = model();
        let (_, passes) = counts(&m);
        let wide = Footprint {
            settings: true,
            ..Footprint::default()
        };
        m.edit().touched(&wide);
        let (_, after_wide) = after_a_call(&mut m);
        assert!(after_wide > passes);
        let failed: Result<bool, CommandError> = Err(CommandError::RollbackFailed {
            index: 0,
            source: Box::new(CommandError::Internal),
            rollback: Box::new(CommandError::Internal),
        });
        m.edit().ended(&Footprint::default(), &failed);
        let (_, after_failed) = after_a_call(&mut m);
        assert!(after_failed > after_wide);
    }
}
