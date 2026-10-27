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
    /// Finds a cue by id in any list, at any group depth.
    pub fn find_cue(&self, id: CueId) -> Option<&Cue> {
        self.cue_lists
            .iter()
            .find_map(|list| find_in(&list.cues, id))
    }

    /// Every cue whose hotkey is `hotkey` (same key and modifiers), in workspace order (lists in
    /// order, cues depth-first). A press starts all of them: as in QLab 5, cues may share a
    /// hotkey and that is not a problem (M2.24, QLab check K1). Iterative, so nesting depth
    /// cannot overflow the stack.
    pub fn cues_with_hotkey(&self, hotkey: &Hotkey) -> Vec<CueId> {
        let mut out = Vec::new();
        for list in &self.cue_lists {
            let mut stack: Vec<std::slice::Iter<'_, Cue>> = vec![list.cues.iter()];
            while let Some(level) = stack.last_mut() {
                let Some(cue) = level.next() else {
                    stack.pop();
                    continue;
                };
                if cue.triggers.hotkey.as_ref() == Some(hotkey) {
                    out.push(cue.id);
                }
                stack.push(cue.children().iter());
            }
        }
        out
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
