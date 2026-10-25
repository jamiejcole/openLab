// SPDX-License-Identifier: GPL-3.0-or-later

//! Cues and their kinds. Times are seconds (f64), levels are dB (f64).

use serde::{Deserialize, Deserializer, Serialize};

use crate::model::{CueId, NetworkPatchId, PatchId};

/// Levels at or below this are silence (−∞). The file writes this value for silence.
pub const SILENCE_DB: f64 = -120.0;
/// Highest level a fader may hold. Validation clamps anything above it.
pub const MAX_DB: f64 = 12.0;
/// Longest time any time field may hold (24 h). Validation clamps anything above it.
pub const MAX_TIME: f64 = 24.0 * 60.0 * 60.0;
/// Most channels a patch, cue or media file may have (AUD-03).
pub const MAX_CHANNELS: u32 = 64;

/// One cue. Fields common to every kind live here; the rest is in `kind`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cue {
    pub id: CueId,
    /// Free text, unique within the workspace when set (CUE-01).
    pub number: Option<String>,
    pub name: String,
    pub notes: String,
    pub color: CueColor,
    pub flagged: bool,
    /// Disarmed cues are skipped when triggered (CUE-07).
    pub armed: bool,
    pub pre_wait: f64,
    pub post_wait: f64,
    pub continue_mode: ContinueMode,
    pub triggers: Triggers,
    pub kind: CueKind,
}

impl Cue {
    /// Child cues of a Group; empty for every other kind.
    pub fn children(&self) -> &[Cue] {
        match &self.kind {
            CueKind::Group { children, .. } => children,
            _ => &[],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CueColor {
    None,
    Red,
    Orange,
    Green,
    Blue,
    Purple,
}

/// What happens after this cue starts (CUE-03).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinueMode {
    DoNotContinue,
    /// Start the next cue when this one starts (after its post-wait).
    AutoContinue,
    /// Start the next cue when this one's action completes (after its post-wait).
    AutoFollow,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Triggers {
    pub hotkey: Option<Hotkey>,
    /// OSC address that starts this cue (TRG-03).
    pub osc: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hotkey {
    /// Lower-case key name as defined by the UI, e.g. `"a"`, `"space"`, `"f5"`.
    pub key: String,
    pub modifiers: Modifiers,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    /// Option on macOS.
    pub alt: bool,
    pub command: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FadeCurve {
    Linear,
    EqualPower,
    SCurve,
    Logarithmic,
    Exponential,
}

/// A fade's length and shape. Duration 0 = no fade.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FadeSpec {
    pub duration: f64,
    pub curve: FadeCurve,
}

/// Path to a media file as stored in the show file: forward slashes, relative to the show
/// file's folder or absolute (NFR-32). Windows absolute paths (`C:\…`, `\\server\…`) are
/// converted to forward slashes on construction and on load. Other backslashes are kept: they are
/// legal in macOS file names. Converting a native relative Windows path is the caller's job (M1.7).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct MediaRef(String);

impl MediaRef {
    pub fn new(path: impl Into<String>) -> Self {
        let path = path.into();
        let bytes = path.as_bytes();
        let drive = matches!(bytes, [letter, b':', b'\\', ..] if letter.is_ascii_alphabetic());
        if drive || path.starts_with(r"\\") {
            Self(path.replace('\\', "/"))
        } else {
            Self(path)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for MediaRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::new)
    }
}

/// End of a slice. The slice that ends here plays `play_count` times; 0 = loop until devamped.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SliceMarker {
    /// Seconds from the start of the file.
    pub time: f64,
    pub play_count: u32,
}

/// A routing-matrix crosspoint: file channel `input` → cue output `output`, both 0-based.
/// Muting keeps `db`, so unmuting restores the level.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Crosspoint {
    pub input: u32,
    pub output: u32,
    pub db: f64,
    pub mute: bool,
}

/// One cue output's level. Muting keeps `db`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CueOutput {
    pub db: f64,
    pub mute: bool,
}

/// An Audio cue's levels and routing (AUD-06, AUD-08).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CueLevels {
    pub master_db: f64,
    /// Cue output `i` feeds output `i` of the cue's patch. At most [`MAX_CHANNELS`]; outputs
    /// beyond the patch's are kept but silent.
    pub outputs: Vec<CueOutput>,
    /// Sparse: crosspoints not listed are silent.
    pub crosspoints: Vec<Crosspoint>,
    /// Sets of cue output indices whose levels move together.
    pub gangs: Vec<Vec<u32>>,
}

/// A crosspoint level a Fade cue moves to.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FadeCrosspoint {
    pub input: u32,
    pub output: u32,
    pub db: f64,
}

/// The levels a Fade cue moves its target to. Only parameters that are set are faded; mutes are
/// never changed by a fade.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FadeLevels {
    pub master_db: Option<f64>,
    /// Indexed by cue output; `None` leaves that output alone.
    pub output_db: Vec<Option<f64>>,
    /// Listed crosspoints fade to their `db`; unlisted ones are left alone.
    pub crosspoints: Vec<FadeCrosspoint>,
}

/// How a Group starts its children (CT-22).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GroupMode {
    /// Start all children together, each after its own pre-wait.
    Timeline,
    StartFirstAndEnter,
    StartFirstAndGoToNext,
    /// Play children one after another.
    Playlist {
        #[serde(rename = "loop")]
        loop_: bool,
        shuffle: bool,
    },
    StartRandom,
}

/// A typed OSC argument.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OscArg {
    Int { value: i32 },
    Float { value: f32 },
    String { value: String },
    Bool { value: bool },
}

/// The v0.0.1 cue kinds. Targets are cue ids, never numbers; `None` = not yet set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CueKind {
    /// Plays a media file (CT-01). Duration is not stored; it comes from the media.
    Audio {
        /// `None` until a file is assigned.
        media: Option<MediaRef>,
        patch: PatchId,
        /// Trim in point, seconds from the start of the file.
        start_time: f64,
        /// Trim out point; `None` = end of file.
        end_time: Option<f64>,
        fade_in: FadeSpec,
        fade_out: FadeSpec,
        slices: Vec<SliceMarker>,
        /// Play count of the slice after the last marker; 0 = loop until devamped.
        last_slice_play_count: u32,
        levels: CueLevels,
        /// Decode the whole file into memory when loaded (AUD-05).
        preload: bool,
    },
    /// Fades a target's levels (CT-10).
    Fade {
        target: Option<CueId>,
        duration: f64,
        curve: FadeCurve,
        levels: FadeLevels,
        stop_target_when_done: bool,
    },
    Group {
        mode: GroupMode,
        children: Vec<Cue>,
    },
    Start {
        target: Option<CueId>,
    },
    Stop {
        target: Option<CueId>,
        /// Fade the target out before stopping it (CT-12). Duration 0 = stop at once.
        fade_out: FadeSpec,
    },
    Pause {
        target: Option<CueId>,
    },
    Load {
        target: Option<CueId>,
        load_to_time: f64,
    },
    Reset {
        target: Option<CueId>,
    },
    Devamp {
        target: Option<CueId>,
        start_next_cue: bool,
        stop_target_when_slice_ends: bool,
    },
    GoTo {
        target: Option<CueId>,
        /// GO after moving the playhead (CT-17).
        then_go: bool,
    },
    Arm {
        target: Option<CueId>,
    },
    Disarm {
        target: Option<CueId>,
    },
    Wait {
        duration: f64,
    },
    Memo,
    /// Sends one OSC message (CT-30).
    Network {
        destination: NetworkPatchId,
        address: String,
        args: Vec<OscArg>,
    },
}

impl CueKind {
    /// Sets the target of a targeting kind. Returns `false`, changing nothing, for kinds that
    /// have no target.
    pub fn set_target(&mut self, new: Option<CueId>) -> bool {
        match self {
            Self::Fade { target, .. }
            | Self::Start { target }
            | Self::Stop { target, .. }
            | Self::Pause { target }
            | Self::Load { target, .. }
            | Self::Reset { target }
            | Self::Devamp { target, .. }
            | Self::GoTo { target, .. }
            | Self::Arm { target }
            | Self::Disarm { target } => {
                *target = new;
                true
            }
            Self::Audio { .. }
            | Self::Group { .. }
            | Self::Wait { .. }
            | Self::Memo
            | Self::Network { .. } => false,
        }
    }

    /// Whether this kind has a target to set, even if none is set yet.
    pub fn has_target(&self) -> bool {
        !matches!(
            self,
            Self::Audio { .. }
                | Self::Group { .. }
                | Self::Wait { .. }
                | Self::Memo
                | Self::Network { .. }
        )
    }

    /// The target of a targeting kind, if one is set.
    pub fn target(&self) -> Option<CueId> {
        match self {
            Self::Fade { target, .. }
            | Self::Start { target }
            | Self::Stop { target, .. }
            | Self::Pause { target }
            | Self::Load { target, .. }
            | Self::Reset { target }
            | Self::Devamp { target, .. }
            | Self::GoTo { target, .. }
            | Self::Arm { target }
            | Self::Disarm { target } => *target,
            Self::Audio { .. }
            | Self::Group { .. }
            | Self::Wait { .. }
            | Self::Memo
            | Self::Network { .. } => None,
        }
    }
}
