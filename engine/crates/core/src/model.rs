// SPDX-License-Identifier: GPL-3.0-or-later

//! Workspace-level model: ids, workspace, cue lists, patches, settings.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::cue::{
    Cue, DEFAULT_OUTPUT_MAX_DB, DEFAULT_OUTPUT_MIN_DB, Hotkey, LevelLimits, Modifiers,
};
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
    /// The workspace was in Show Mode when saved (SM-01): it reopens in Show Mode. Only the saved
    /// copy carries it: the engine keeps the live mode beside the model (not undoable, and a
    /// change does not mark the workspace unsaved) and writes it into each copy it saves, and
    /// takes it back out when a file is opened, so a workspace on the wire or in the model is
    /// always `false`. Absent from the file means Edit Mode; a value that is not a boolean reads
    /// as Edit Mode (`load_show` warns).
    #[serde(
        default,
        skip_serializing_if = "is_false",
        deserialize_with = "bool_or_false"
    )]
    pub show_mode: bool,
    /// Last so the readable parts of the file come first.
    pub cue_lists: Vec<CueList>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// `true` for a `true`, `false` for anything else: a damaged `show_mode` must not refuse the
/// show. Read as raw text, not as a `Value`, which would refuse a number out of range or
/// nesting past the parser's limit (the same value under an unknown key loads fine).
fn bool_or_false<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    let raw = Option::<Box<serde_json::value::RawValue>>::deserialize(deserializer)?;
    Ok(raw.is_some_and(|r| r.get() == "true"))
}

impl Workspace {
    /// A new, empty workspace (WS-02): one empty cue list, one stereo patch and one network patch
    /// that sends OSC to this computer over UDP (QLab 5 manual p. 111: a new workspace starts with
    /// a network patch set up to send to itself), the defaults of docs/file-format.md section 4.
    /// The ids come from the caller because core draws no randomness. Only new workspaces get
    /// the network patch; a loaded file keeps the patches it has.
    pub fn new_empty(
        id: WorkspaceId,
        list: CueListId,
        patch: PatchId,
        network: NetworkPatchId,
    ) -> Self {
        let output = PatchOutput {
            level_db: 0.0,
            mute: false,
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
                output_max_db: DEFAULT_OUTPUT_MAX_DB,
                output_min_db: DEFAULT_OUTPUT_MIN_DB,
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
            network_patches: vec![NetworkPatch {
                id: network,
                name: "Network 1".to_owned(),
                host: "127.0.0.1".to_owned(),
                port: 53000,
                protocol: NetworkProtocol::Udp,
            }],
            show_mode: false,
            cue_lists: vec![CueList {
                id: list,
                name: "Main Cue List".to_owned(),
                cues: Vec::new(),
            }],
        }
    }

    /// Finds a cue by id in any list, at any group depth.
    pub fn find_cue(&self, id: CueId) -> Option<&Cue> {
        self.cues().find(|cue| cue.id == id)
    }

    /// [`Workspace::find_cue`], for changing the cue.
    pub fn find_cue_mut(&mut self, id: CueId) -> Option<&mut Cue> {
        let path = crate::tree::find_path(self, id)?;
        crate::tree::cue_at_mut(self, &path)
    }

    /// Every cue of every list, lists in order, each list depth-first
    /// ([`crate::descendants`]).
    pub fn cues(&self) -> impl Iterator<Item = &Cue> {
        self.cue_lists
            .iter()
            .flat_map(|list| crate::tree::descendants(&list.cues))
    }

    /// Every cue whose hotkey is `hotkey` (same key and modifiers), in workspace order (lists in
    /// order, cues depth-first). A press starts all of them: as in QLab 5, cues may share a
    /// hotkey and that is not a problem (QLab check K1). None when a workspace transport
    /// key is `hotkey`: the workspace action wins and no cue is started by that key (QLab check
    /// K1b; validation warns `HotkeyUsedByWorkspace`). Iterative, so nesting depth cannot
    /// overflow the stack.
    pub fn cues_with_hotkey(&self, hotkey: &Hotkey) -> Vec<CueId> {
        let mut out = Vec::new();
        if self.settings.hotkeys.action_of(hotkey).is_some() {
            return out;
        }
        out.extend(
            self.cues()
                .filter(|cue| cue.triggers.hotkey.as_ref() == Some(hotkey))
                .map(|cue| cue.id),
        );
        out
    }
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
    /// One entry per output; the patch's channel count is its length, 1 to
    /// [`MAX_CUE_OUTPUTS`].
    ///
    /// [`MAX_CUE_OUTPUTS`]: crate::MAX_CUE_OUTPUTS
    pub outputs: Vec<PatchOutput>,
}

impl OutputPatch {
    /// Puts the master and every output level within `limits`.
    pub fn limit_levels(&mut self, limits: LevelLimits) {
        self.master_db = limits.limit(self.master_db);
        for output in &mut self.outputs {
            output.level_db = limits.limit(output.level_db);
        }
    }
}

/// One output of a patch. Muting keeps `level_db`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PatchOutput {
    pub level_db: f64,
    pub mute: bool,
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
    /// Highest level any fader in the workspace may hold, -30 to +12 dB (PB-11).
    pub output_max_db: f64,
    /// Levels at or below this are silent (-inf), -180 to -40 dB (PB-11).
    pub output_min_db: f64,
    pub hotkeys: TransportHotkeys,
    /// Number given to the first auto-numbered cue (ED-08).
    pub auto_number_start: f64,
    /// Step between auto-numbered cues; greater than 0.
    pub auto_number_increment: f64,
}

impl WorkspaceSettings {
    /// The volume limits every level in the workspace lives in.
    pub fn limits(&self) -> LevelLimits {
        LevelLimits {
            max_db: self.output_max_db,
            min_db: self.output_min_db,
        }
    }
}

/// Remappable workspace keys (TRG-01, TRG-07): the transport keys, the selected-cue keys and the
/// cue-editing keys of QLab 5's keyboard map (Workspace Settings, Controls, Keyboard). `None` = no key. Esc is not among them:
/// Panic All is the fixed panic key, as in QLab. Neither is a Next / Previous Cue key (format v6
/// dropped them): QLab's key map has none and takes no Up or Down arrow, which move the
/// selection (and with it the playhead).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransportHotkeys {
    pub go: Option<Hotkey>,
    /// Stop All. Since format v5 no key by default: Esc is panic, outside this map.
    pub stop_all: Option<Hotkey>,
    pub pause_all: Option<Hotkey>,
    pub resume_all: Option<Hotkey>,
    /// Load the selected cue (QLab: Load Last Selected, `L`).
    pub load_selected: Option<Hotkey>,
    /// Pause or resume the selected cues (QLab: `P`).
    pub pause_resume_selected: Option<Hotkey>,
    pub start_selected: Option<Hotkey>,
    pub stop_selected: Option<Hotkey>,
    pub reset_selected: Option<Hotkey>,
    /// Preview the selected cue (QLab: Preview, `V`).
    pub preview_selected: Option<Hotkey>,
    /// Panic the selected cue (QLab: Panic Selected, `S`). Panic All (Esc) stays outside the map.
    pub panic_selected: Option<Hotkey>,
    /// The cue-editing keys (TRG-07): each acts on the cues selected in the app's cue list and
    /// does nothing in the engine, which only keeps the key from a cue's hotkey. Edit the
    /// selected cue's number (QLab: `N`).
    pub edit_number: Option<Hotkey>,
    /// Edit its name (`Q`).
    pub edit_name: Option<Hotkey>,
    /// Edit its notes (`O`).
    pub edit_notes: Option<Hotkey>,
    /// Edit its target (`T`).
    pub edit_target: Option<Hotkey>,
    /// Edit its pre-wait (`E`).
    pub edit_pre_wait: Option<Hotkey>,
    /// Edit its duration (`D`).
    pub edit_duration: Option<Hotkey>,
    /// Edit its post-wait (`W`).
    pub edit_post_wait: Option<Hotkey>,
    /// Step its continue mode (`C`).
    pub cycle_continue: Option<Hotkey>,
    /// Flag or unflag it (`F`).
    pub toggle_flag: Option<Hotkey>,
}

impl TransportHotkeys {
    /// Every key with its field name (`"go"`, `"stop_all"`, ...), in declaration order.
    pub fn slots(&self) -> [(&'static str, &Option<Hotkey>); 20] {
        [
            ("go", &self.go),
            ("stop_all", &self.stop_all),
            ("pause_all", &self.pause_all),
            ("resume_all", &self.resume_all),
            ("load_selected", &self.load_selected),
            ("pause_resume_selected", &self.pause_resume_selected),
            ("start_selected", &self.start_selected),
            ("stop_selected", &self.stop_selected),
            ("reset_selected", &self.reset_selected),
            ("preview_selected", &self.preview_selected),
            ("panic_selected", &self.panic_selected),
            ("edit_number", &self.edit_number),
            ("edit_name", &self.edit_name),
            ("edit_notes", &self.edit_notes),
            ("edit_target", &self.edit_target),
            ("edit_pre_wait", &self.edit_pre_wait),
            ("edit_duration", &self.edit_duration),
            ("edit_post_wait", &self.edit_post_wait),
            ("cycle_continue", &self.cycle_continue),
            ("toggle_flag", &self.toggle_flag),
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
        "load_selected" => "settings.hotkeys.load_selected",
        "pause_resume_selected" => "settings.hotkeys.pause_resume_selected",
        "start_selected" => "settings.hotkeys.start_selected",
        "stop_selected" => "settings.hotkeys.stop_selected",
        "reset_selected" => "settings.hotkeys.reset_selected",
        "preview_selected" => "settings.hotkeys.preview_selected",
        "panic_selected" => "settings.hotkeys.panic_selected",
        "edit_number" => "settings.hotkeys.edit_number",
        "edit_name" => "settings.hotkeys.edit_name",
        "edit_notes" => "settings.hotkeys.edit_notes",
        "edit_target" => "settings.hotkeys.edit_target",
        "edit_pre_wait" => "settings.hotkeys.edit_pre_wait",
        "edit_duration" => "settings.hotkeys.edit_duration",
        "edit_post_wait" => "settings.hotkeys.edit_post_wait",
        "cycle_continue" => "settings.hotkeys.cycle_continue",
        "toggle_flag" => "settings.hotkeys.toggle_flag",
        _ => "settings.hotkeys",
    }
}

/// Keys no workspace key may be: Esc and Up / Down, each with any modifiers (QLab 5 refuses Esc
/// as the fixed panic key and the arrows whatever is held).
pub fn is_reserved_key(hotkey: &Hotkey) -> bool {
    hotkey.key == "escape" || matches!(hotkey.key.as_str(), "up" | "down")
}

impl Default for TransportHotkeys {
    /// A new workspace's keys, QLab 5's defaults; Stop All has no key (Esc is panic).
    fn default() -> Self {
        let key = |key: &str| {
            Some(Hotkey {
                key: key.into(),
                modifiers: Modifiers::default(),
            })
        };
        Self {
            go: key("space"),
            stop_all: None,
            pause_all: key("["),
            resume_all: key("]"),
            load_selected: key("l"),
            pause_resume_selected: key("p"),
            start_selected: None,
            stop_selected: None,
            reset_selected: None,
            preview_selected: key("v"),
            panic_selected: key("s"),
            edit_number: key("n"),
            edit_name: key("q"),
            edit_notes: key("o"),
            edit_target: key("t"),
            edit_pre_wait: key("e"),
            edit_duration: key("d"),
            edit_post_wait: key("w"),
            cycle_continue: key("c"),
            toggle_flag: key("f"),
        }
    }
}
