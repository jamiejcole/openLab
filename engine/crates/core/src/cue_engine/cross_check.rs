// SPDX-License-Identifier: GPL-3.0-or-later

//! The edit cross-check (debug builds, and tests that ask): counts a model change nobody
//! signalled in `unsignalled_model_changes`, so a missed `model_changed` is found. It only
//! counts. What a call does never depends on it, so an engine with it and one without act
//! alike on every input (CLAUDE.md: debug and release behave the same).

use crate::model::Workspace;

use super::CueEngine;

/// What the cross-check compares the next call with.
#[derive(Clone, Debug, Default)]
pub(super) struct CrossCheck {
    /// The workspace as the last call saw it.
    seen: Option<Box<Workspace>>,
    /// The problem cache differed from a rebuild after the last call (counted once, when it
    /// started to).
    stale: bool,
}

impl CueEngine {
    /// End of a call: counts a workspace that changed since the last call without
    /// `model_changed`, and a problem cache that newly differs from a full rebuild (a footprint
    /// that missed a cue), unless this call counted the change already (the outline check, a
    /// refused cue's recheck). Costs a comparison of the whole workspace and a rebuild of the
    /// problems; allocates a copy only when the workspace changed.
    pub(super) fn cross_check_edits(&mut self, ws: &Workspace, signalled: bool, counted: u64) {
        let Some(mut check) = self.cross_check.take() else {
            return;
        };
        // `None`: nothing to compare with yet (a new engine, the check just turned on).
        let changed = check.seen.as_deref().map(|seen| !same(seen, ws));
        if changed != Some(false) {
            check.seen = Some(Box::new(ws.clone()));
        }
        let stale = self.verify_problem_cache(ws).is_err();
        let newly_stale = stale && !check.stale;
        check.stale = stale;
        let unsignalled = changed == Some(true) && !signalled;
        if (unsignalled || newly_stale) && self.unsignalled_model_changes() == counted {
            self.note_unsignalled();
        }
        self.cross_check = Some(check);
    }
}

/// Equal, with a NaN a hostile workspace may hold comparing equal to itself (the debug text,
/// only when `==` says they differ).
fn same(a: &Workspace, b: &Workspace) -> bool {
    a == b || format!("{a:?}") == format!("{b:?}")
}
