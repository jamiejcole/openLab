// SPDX-License-Identifier: GPL-3.0-or-later

//! Workspace-level model: ids, workspace, cue lists, patches, settings.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::cue::Cue;

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);
    };
}

id_type!(
    /// Stable workspace id; names the recovery and backup files.
    WorkspaceId
);
id_type!(
    /// Stable cue id, independent of the cue number (CUE-08). Targets refer to it.
    CueId
);
id_type!(
    /// Stable cue list id.
    CueListId
);
id_type!(
    /// Stable output patch id.
    PatchId
);
id_type!(
    /// Stable network patch id.
    NetworkPatchId
);

/// A complete show document (WS-01).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub name: String,
    pub settings: WorkspaceSettings,
    pub patches: Vec<OutputPatch>,
    pub network_patches: Vec<NetworkPatch>,
    /// Last so the readable parts of the file come first.
    pub cue_lists: Vec<CueList>,
}

impl Workspace {
    /// Finds a cue by id in any list, at any group depth.
    pub fn find_cue(&self, id: CueId) -> Option<&Cue> {
        self.cue_lists
            .iter()
            .find_map(|list| find_in(&list.cues, id))
    }
}

fn find_in(cues: &[Cue], id: CueId) -> Option<&Cue> {
    cues.iter().find_map(|cue| {
        (cue.id == id)
            .then_some(cue)
            .or_else(|| find_in(cue.children(), id))
    })
}

/// A cue list with its own playhead (CL-01).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CueList {
    pub id: CueListId,
    pub name: String,
    pub cues: Vec<Cue>,
}

/// A logical output. Mapping to device channels is per machine (WS-07), not stored here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutputPatch {
    pub id: PatchId,
    pub name: String,
    pub channel_count: u32,
}

/// A destination for Network cues.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NetworkPatch {
    pub id: NetworkPatchId,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub protocol: NetworkProtocol,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkProtocol {
    Udp,
    Tcp,
}

/// Settings that travel with the show file (WS-06). Times in seconds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceSettings {
    /// Double-GO protection: GOs closer together than this are ignored (PB-02). 0 = off.
    pub min_go_interval: f64,
    /// Panic fade-out time (PB-03).
    pub panic_duration: f64,
    /// Autosave period while dirty. 0 = off.
    pub autosave_interval: f64,
    /// Required by `/connect` before OSC control is accepted (NET-02). `None` = open.
    pub osc_passcode: Option<String>,
}
