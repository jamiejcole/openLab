// SPDX-License-Identifier: GPL-3.0-or-later

//! Cues and their kinds. Times are seconds (f64), levels are dB (f64).

use std::borrow::Cow;

use serde::{Deserialize, Deserializer, Serialize};

use crate::media_path::file_name;
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
    /// What a trigger does while the cue is already active (QLab 5's "second trigger").
    pub second_trigger: SecondTrigger,
    /// Releasing the hotkey or MIDI key that triggers the cue counts as a second trigger.
    pub second_trigger_on_release: bool,
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

    /// The name to show for the cue (M2.29c, QLab check R2). Assigning a file never writes
    /// `name`: an empty `name` means "named after the file", so a rename of the file shows at
    /// once and a name the user typed is never touched. The file name keeps its extension and
    /// is shown as stored (no case or Unicode-form change). OSC and the model still see `name`.
    pub fn display_name(&self) -> Cow<'_, str> {
        display_name(self)
    }
}

/// See [`Cue::display_name`].
pub fn display_name(cue: &Cue) -> Cow<'_, str> {
    if !cue.name.is_empty() {
        return Cow::Borrowed(&cue.name);
    }
    match &cue.kind {
        // Split on `/` only: a backslash is a legal macOS file-name character, and Windows
        // separators were converted when the `MediaRef` was made.
        CueKind::Audio {
            media: Some(media), ..
        } => Cow::Borrowed(file_name(media.as_str())),
        _ => Cow::Borrowed(""),
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
    /// Start the next cue when this one's post-wait elapses, counted from the start of its
    /// action (after the pre-wait); post-wait 0 = when the action starts.
    AutoContinue,
    /// Start the next cue when this one's action completes; the post-wait is not used.
    AutoFollow,
}

/// Which loop of an Audio target a Devamp cue leaves: QLab 5's "Devamp currently looping slice"
/// and "Devamp looping cue" (check D7, M2.28). openLab has no play count for the whole cue: a
/// cue without slice markers loops as a whole through `last_slice_play_count`, which is the
/// cue's loop; with markers, every loop is a slice's. Rules: `docs/cue-engine.md`, "Devamp".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DevampMode {
    /// "Devamp currently looping slice": leaves whatever loop is playing and moves on to the
    /// next slice (what openLab did before the modes; shows from version 2 get it).
    #[default]
    CurrentSlice,
    /// "Devamp looping cue": leaves the cue's own loop; on a cue with slice markers (whose
    /// loops are all slices' loops) it does nothing.
    LoopingCue,
}

/// What a trigger (GO, a Start cue, a continue, a hotkey, OSC, …) does to a cue that is already
/// active: pre-waiting, running, post-waiting, paused, fading or stopping. Rules:
/// `docs/cue-engine.md`, "Second trigger".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecondTrigger {
    /// Ignored (a paused cue resumes).
    #[default]
    Nothing,
    /// Fade out over the workspace's panic duration, then stop.
    Panic,
    Stop,
    HardStop,
    /// Hard stop, then start again from the top.
    HardStopAndRestart,
    /// Leave the loop after the current pass; nothing if the cue cannot devamp.
    Devamp,
    /// Playlist groups only: play the child after the one playing.
    PlayNext,
    /// Playlist groups only: play the child before the one playing.
    PlayPrevious,
}

impl SecondTrigger {
    /// Whether the option only means something on a Playlist group (elsewhere it acts as
    /// [`SecondTrigger::Nothing`] and validation warns).
    pub fn playlist_only(self) -> bool {
        matches!(self, Self::PlayNext | Self::PlayPrevious)
    }

    /// The option as it applies to a cue of `kind`: a playlist-only option on any other cue is
    /// [`SecondTrigger::Nothing`].
    pub fn effective(self, kind: &CueKind) -> Self {
        let playlist = matches!(
            kind,
            CueKind::Group {
                mode: GroupMode::Playlist { .. },
                ..
            }
        );
        if self.playlist_only() && !playlist {
            Self::Nothing
        } else {
            self
        }
    }
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
/// legal in macOS file names. Native paths from the OS go through `portable_ref`, which also converts relative Windows paths.
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
        /// Which loop it leaves (QLab 5's two Devamp modes, check D7). Since version 3.
        devamp_mode: DevampMode,
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
