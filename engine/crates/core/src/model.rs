// SPDX-License-Identifier: GPL-3.0-or-later

//! Workspace-level model: ids, workspace, cue lists, patches, settings.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::cue::{Cue, Hotkey, Modifiers};
use crate::osc_access::OscAccess;

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
                osc_access: OscAccess::default(),
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

    /// Every cue whose hotkey is `hotkey` (same key and modifiers), in workspace order (lists in
    /// order, cues depth-first). A press starts all of them: as in QLab 5, cues may share a
    /// hotkey and that is not a problem (M2.24, QLab check K1). None when a workspace transport
    /// key is `hotkey`: the workspace action wins and no cue is started by that key (QLab check
    /// K1b, M2.28; validation warns `HotkeyUsedByWorkspace`). Iterative, so nesting depth cannot
    /// overflow the stack.
    pub fn cues_with_hotkey(&self, hotkey: &Hotkey) -> Vec<CueId> {
        let mut out = Vec::new();
        if self.settings.hotkeys.action_of(hotkey).is_some() {
            return out;
        }
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
    /// Who may do what over OSC (NET-02): the passcode table with each passcode's rights.
    pub osc_access: OscAccess,
    /// Global master level (PB-11).
    pub master_db: f64,
    pub hotkeys: TransportHotkeys,
    /// Number given to the first auto-numbered cue (ED-08).
    pub auto_number_start: f64,
    /// Step between auto-numbered cues; greater than 0.
    pub auto_number_increment: f64,
}

/// Remappable workspace keys (TRG-01): the transport keys and the selected-cue keys of QLab 5's
/// keyboard map (Workspace Settings, Controls, Keyboard). `None` = no key. Esc is not among them:
/// it is the fixed panic key, as in QLab.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransportHotkeys {
    pub go: Option<Hotkey>,
    /// Stop All. Since format v5 no key by default: Esc is panic, outside this map.
    pub stop_all: Option<Hotkey>,
    pub pause_all: Option<Hotkey>,
    pub resume_all: Option<Hotkey>,
    pub playhead_next: Option<Hotkey>,
    pub playhead_previous: Option<Hotkey>,
    /// Load the selected cue (QLab: Load Last Selected, `L`).
    pub load_selected: Option<Hotkey>,
    /// Pause or resume the selected cues (QLab: `P`).
    pub pause_resume_selected: Option<Hotkey>,
    pub start_selected: Option<Hotkey>,
    pub stop_selected: Option<Hotkey>,
    pub reset_selected: Option<Hotkey>,
}

impl TransportHotkeys {
    /// Every key with its field name (`"go"`, `"stop_all"`, ...), in declaration order.
    pub fn slots(&self) -> [(&'static str, &Option<Hotkey>); 11] {
        [
            ("go", &self.go),
            ("stop_all", &self.stop_all),
            ("pause_all", &self.pause_all),
            ("resume_all", &self.resume_all),
            ("playhead_next", &self.playhead_next),
            ("playhead_previous", &self.playhead_previous),
            ("load_selected", &self.load_selected),
            ("pause_resume_selected", &self.pause_resume_selected),
            ("start_selected", &self.start_selected),
            ("stop_selected", &self.stop_selected),
            ("reset_selected", &self.reset_selected),
        ]
    }

    /// The workspace action whose key is `hotkey` (same key and modifiers), by its field name
    /// (`"go"`, `"stop_all"`, `"load_selected"`, ...), or `None`.
    pub fn action_of(&self, hotkey: &Hotkey) -> Option<&'static str> {
        self.slots()
            .into_iter()
            .find_map(|(name, key)| (key.as_ref() == Some(hotkey)).then_some(name))
    }
}

/// The show-file path of the key named `name` by [`TransportHotkeys::slots`], for warnings and
/// refusals.
pub(crate) fn hotkey_field(name: &str) -> &'static str {
    match name {
        "go" => "settings.hotkeys.go",
        "stop_all" => "settings.hotkeys.stop_all",
        "pause_all" => "settings.hotkeys.pause_all",
        "resume_all" => "settings.hotkeys.resume_all",
        "playhead_next" => "settings.hotkeys.playhead_next",
        "playhead_previous" => "settings.hotkeys.playhead_previous",
        "load_selected" => "settings.hotkeys.load_selected",
        "pause_resume_selected" => "settings.hotkeys.pause_resume_selected",
        "start_selected" => "settings.hotkeys.start_selected",
        "stop_selected" => "settings.hotkeys.stop_selected",
        "reset_selected" => "settings.hotkeys.reset_selected",
        _ => "settings.hotkeys",
    }
}

/// Keys no workspace key may be: Esc with any modifiers (the fixed panic key; QLab 5: a single
/// Esc is always panic) and Up / Down without Command, Control or Option (they move the
/// selection, and with it the playhead; QLab 5 does not allow the up and down arrows). Shift
/// alone extends the selection, so Shift-Up counts as bare too, as in the app's recorder.
pub fn is_reserved_key(hotkey: &Hotkey) -> bool {
    let m = hotkey.modifiers;
    hotkey.key == "escape"
        || (matches!(hotkey.key.as_str(), "up" | "down") && !(m.command || m.control || m.alt))
}

impl Default for TransportHotkeys {
    /// A new workspace's keys (format v5), QLab 5's defaults: Next / Previous Cue are QLab's
    /// menu keys Shift-Command-Down / Up; Stop All has no key (Esc is panic).
    fn default() -> Self {
        let key = |key: &str| {
            Some(Hotkey {
                key: key.into(),
                modifiers: Modifiers::default(),
            })
        };
        let shift_command = |key: &str| {
            Some(Hotkey {
                key: key.into(),
                modifiers: Modifiers {
                    shift: true,
                    command: true,
                    ..Modifiers::default()
                },
            })
        };
        Self {
            go: key("space"),
            stop_all: None,
            pause_all: key("["),
            resume_all: key("]"),
            playhead_next: shift_command("down"),
            playhead_previous: shift_command("up"),
            load_selected: key("l"),
            pause_resume_selected: key("p"),
            start_selected: None,
            stop_selected: None,
            reset_selected: None,
        }
    }
}
