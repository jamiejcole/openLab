// SPDX-License-Identifier: GPL-3.0-or-later

//! The workspace's outline: what release builds compare on every call to notice a model change
//! nobody signalled (debug builds rebuild everything instead, see `CueEngine::model_changed`).

use crate::model::{CueListId, Workspace};

/// Cue lists (ids and top-level cue counts) and patch counts as of the last call that followed
/// the edits. Checking costs one comparison per cue list and patch kind and allocates nothing,
/// so a Tick on a 2,000-cue show pays a few nanoseconds; recording reuses its buffer. It sees
/// cue lists, top-level cues and patches added or removed, not an edit inside a cue or a group:
/// those are caught where they would refuse or drop a cue (see `CueEngine::model_changed`).
#[derive(Clone, Debug, Default)]
pub(super) struct Shape {
    lists: Vec<(CueListId, usize)>,
    patches: usize,
    network_patches: usize,
    /// `unsignalled_model_changes` when this was recorded: a change counted since (by a
    /// refused cue's recheck) is not counted again here.
    unsignalled: u64,
}

impl Shape {
    pub(super) fn record(&mut self, ws: &Workspace, unsignalled: u64) {
        self.lists.clear();
        self.lists
            .extend(ws.cue_lists.iter().map(|l| (l.id, l.cues.len())));
        self.patches = ws.patches.len();
        self.network_patches = ws.network_patches.len();
        self.unsignalled = unsignalled;
    }

    pub(super) fn matches(&self, ws: &Workspace) -> bool {
        self.patches == ws.patches.len()
            && self.network_patches == ws.network_patches.len()
            && self.lists.len() == ws.cue_lists.len()
            && self
                .lists
                .iter()
                .zip(&ws.cue_lists)
                .all(|(&(id, n), l)| id == l.id && n == l.cues.len())
    }

    pub(super) fn unsignalled(&self) -> u64 {
        self.unsignalled
    }
}
