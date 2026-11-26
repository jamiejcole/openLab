// SPDX-License-Identifier: GPL-3.0-or-later

//! What a QLab import reports (the `qlab_report` answer, docs/protocol.md) and what the
//! operator chooses (the `import_qlab` request).

use serde::{Deserialize, Serialize};

use crate::settings::PatchMapping;

/// Most notes one report lists; `notes_omitted` counts the rest.
pub const MAX_QLAB_NOTES: usize = 2000;

/// The report of a QLab workspace: what cannot be converted, what changes, and the devices it
/// was set up for.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct QlabReport {
    /// The QLab workspace's name, without `.qlab5`: the new workspace's name.
    pub workspace_name: String,
    /// The QLab version that saved it, as it says (`"5.6.5"`).
    pub qlab_version: String,
    /// Cues read, cue lists and carts not counted.
    pub cue_count: u32,
    /// Cues openLab cannot convert (types it lacks) or could not (damaged data), and carts, in
    /// workspace order. Each becomes a placeholder or is deleted (`import_qlab`).
    pub unsupported: Vec<QlabUnsupported>,
    /// Properties dropped or changed, at most [`MAX_QLAB_NOTES`].
    pub notes: Vec<QlabNote>,
    /// Notes left out of `notes`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub notes_omitted: u32,
    /// The QLab audio patches, in QLab's order, for the device step.
    pub audio_patches: Vec<QlabAudioPatch>,
    /// The QLab video output routes, in QLab's order, for the display step.
    pub video_outputs: Vec<QlabVideoOutput>,
    /// The QLab video stages (empty when QLab saved it with its single built-in stage).
    pub stages: Vec<QlabStage>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// One cue (or cart) openLab cannot convert.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QlabUnsupported {
    /// QLab's unique id of the cue: what `import_qlab` names it by.
    pub qlab_id: String,
    pub number: String,
    pub name: String,
    /// QLab's kind of cue in our words (`"Light"`, `"MIDI File"`, `"Cart"`, ...), not its class.
    pub qlab_type: String,
    pub reason: QlabUnsupportedReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QlabUnsupportedReason {
    /// A cue type openLab does not have (Light, MIDI, MIDI File, Timecode, Script, Camera, Mic,
    /// Text, or a class this reader does not know).
    CueType,
    /// A cart: openLab has cue lists only. Its cues convert into a cue list as a placeholder.
    Cart,
    /// A Network cue sending something other than an OSC message.
    NetworkMessage,
    /// A Fade or Reset cue aimed at a patch or audio map instead of a cue.
    PatchTarget,
    /// The cue's data could not be read.
    Damaged,
}

/// A property dropped or changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QlabNote {
    /// The cue's QLab unique id; absent for the workspace's own settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qlab_id: Option<String>,
    pub number: String,
    pub name: String,
    pub kind: QlabNoteKind,
    /// One plain English sentence saying what was dropped or changed, shown as it is.
    pub detail: String,
}

/// What a note is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QlabNoteKind {
    /// Audio effects, video effects or patch effects.
    Effects,
    /// Playback rate or pitch.
    Rate,
    /// Audio objects or audio maps.
    AudioObjects,
    /// MIDI or timecode triggers.
    Trigger,
    /// An Audio cue's integrated fade envelope.
    IntegratedFade,
    /// A trim or an input level folded into the levels.
    LevelFolded,
    /// A level, gang or mute openLab has no place for.
    LevelDropped,
    /// A whole-cue play count or loop on a cue with slices.
    PlayCount,
    /// A playlist's crossfade.
    Crossfade,
    /// A fade curve shape or audio domain mapped to the nearest openLab curve.
    FadeCurve,
    /// A video geometry or display setting openLab does not have.
    Video,
    /// A hotkey openLab cannot name.
    Hotkey,
    /// A workspace setting, key map entry or OSC access passcode not converted.
    Setting,
    /// A name or text cut to openLab's limit.
    Truncated,
    /// A cue that appeared twice in the file, or nested too deep: left out.
    Structure,
    /// A cue's target that did not convert: the cue has none.
    Target,
}

/// One QLab audio patch, as QLab knew its device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QlabAudioPatch {
    pub qlab_id: String,
    pub name: String,
    /// The patch's cue outputs: the openLab patch's output count.
    pub outputs: u32,
    /// The system output rather than a named device.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub system: bool,
    /// The device's id as macOS gives it (Core Audio UID); empty when QLab saved none.
    pub device_uid: String,
    pub device_name: String,
    /// The device's outputs, as QLab last saw it.
    pub device_outputs: u32,
}

/// One QLab video output route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QlabVideoOutput {
    pub qlab_id: String,
    pub name: String,
    /// The display it was on, by name (empty when QLab saved none).
    pub screen: String,
    /// The route's raster in pixels.
    pub width: u32,
    pub height: u32,
}

/// One QLab video stage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QlabStage {
    pub qlab_id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
}

/// What becomes of one unsupported cue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QlabCueChoice {
    pub qlab_id: String,
    pub action: QlabCueAction,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QlabCueAction {
    Delete,
    /// A red Memo cue keeping its number, name, notes, waits and continue mode (a cart: a cue
    /// list of its cues).
    #[default]
    Placeholder,
}

impl QlabCueAction {
    pub fn is_placeholder(&self) -> bool {
        *self == Self::Placeholder
    }
}

/// Where a QLab audio patch plays on this machine: `mapping` absent leaves it unmapped.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QlabPatchChoice {
    pub qlab_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapping: Option<PatchMapping>,
}

/// Which display a QLab video output route is shown on here: `display` absent, none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QlabOutputChoice {
    pub qlab_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}
