// SPDX-License-Identifier: GPL-3.0-or-later

//! Workspace-level model: ids, workspace, cue lists, patches, settings.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::cue::{Cue, Hotkey, Modifiers};

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
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
    /// A new, empty workspace (WS-02): one empty cue list and one stereo patch, the defaults of
    /// docs/file-format.md section 4.2. The ids come from the caller because core draws no
    /// randomness.
    pub fn new_empty(id: WorkspaceId, list: CueListId, patch: PatchId) -> Self {
        let output = PatchOutput {
            level_db: 0.0,
            mute: false,
            delay_ms: 0.0,
            polarity_inverted: false,
        };
        Self {
            id,
            name: "Untitled".to_owned(),
            settings: WorkspaceSettings {
                min_go_interval: 0.0,
                panic_duration: 1.0,
                autosave_interval: 30.0,
                osc_passcode: None,
                master_db: 0.0,
                hotkeys: TransportHotkeys::default(),
                auto_number_start: 1.0,
                auto_number_increment: 1.0,
            },
            patches: vec![OutputPatch {
                id: patch,
                name: "Main Outputs".to_owned(),
                master_db: 0.0,
                outputs: vec![output; 2],
            }],
            network_patches: Vec::new(),
            cue_lists: vec![CueList {
                id: list,
                name: "Main Cue List".to_owned(),
                cues: Vec::new(),
            }],
        }
    }

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

/// A named set of logical outputs (AUD-07). Mapping to device channels is per machine (WS-07),
/// not stored here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutputPatch {
    pub id: PatchId,
    pub name: String,
    /// Patch master level (PB-11).
    pub master_db: f64,
    /// One entry per output; the patch's channel count is its length, 1 to [`MAX_CHANNELS`].
    ///
    /// [`MAX_CHANNELS`]: crate::MAX_CHANNELS
    pub outputs: Vec<PatchOutput>,
}

/// One output of a patch. Muting keeps `level_db`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PatchOutput {
    pub level_db: f64,
    pub mute: bool,
    /// Speaker-alignment delay, milliseconds.
    pub delay_ms: f64,
    pub polarity_inverted: bool,
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
    /// Double-GO protection: GOs closer together than this are ignored (PB-02). 0 to 10 s; 0 = off.
    pub min_go_interval: f64,
    /// Panic fade-out time (PB-03), 0 to 60 s.
    pub panic_duration: f64,
    /// Autosave period while dirty (NFR-05), 5 s to 1 h.
    pub autosave_interval: f64,
    /// Required by `/connect` before OSC control is accepted (NET-02). `None` = open.
    pub osc_passcode: Option<String>,
    /// Global master level (PB-11).
    pub master_db: f64,
    pub hotkeys: TransportHotkeys,
    /// Number given to the first auto-numbered cue (ED-08).
    pub auto_number_start: f64,
    /// Step between auto-numbered cues; greater than 0.
    pub auto_number_increment: f64,
}

/// Remappable transport keys (TRG-01). `None` = no key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransportHotkeys {
    pub go: Option<Hotkey>,
    /// Stop All / Panic.
    pub stop_all: Option<Hotkey>,
    pub pause_all: Option<Hotkey>,
    pub resume_all: Option<Hotkey>,
    pub playhead_next: Option<Hotkey>,
    pub playhead_previous: Option<Hotkey>,
}

impl Default for TransportHotkeys {
    fn default() -> Self {
        let key = |key: &str| {
            Some(Hotkey {
                key: key.into(),
                modifiers: Modifiers::default(),
            })
        };
        Self {
            go: key("space"),
            stop_all: key("escape"),
            pause_all: key("["),
            resume_all: key("]"),
            playhead_next: key("down"),
            playhead_previous: key("up"),
        }
    }
}
