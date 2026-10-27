// SPDX-License-Identifier: GPL-3.0-or-later

//! The cue engine's states, problems, warnings and changes as they appear on the wire. They
//! mirror the engine's own types (which are not `Serialize`, so the protocol can stay stable
//! while the engine changes), with times in seconds and ids as the model's.

use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

use crate::cue::MediaRef;
use crate::cue_engine::{
    Action, BrokenReason, CueEngine, CueProblem, CueState, EngineWarning, GoIgnoredReason,
    PreviewIgnoredReason, SampleTime,
};
use crate::model::{CueId, CueListId, NetworkPatchId, PatchId};
use crate::settings::PatchResolution;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireCueState {
    Idle,
    Loaded,
    PreWaiting,
    Running,
    Paused,
    PostWaiting,
    Fading,
    Stopping,
    Broken,
}

impl From<CueState> for WireCueState {
    fn from(state: CueState) -> Self {
        match state {
            CueState::Idle => Self::Idle,
            CueState::Loaded => Self::Loaded,
            CueState::PreWaiting => Self::PreWaiting,
            CueState::Running => Self::Running,
            CueState::Paused => Self::Paused,
            CueState::PostWaiting => Self::PostWaiting,
            CueState::Fading => Self::Fading,
            CueState::Stopping => Self::Stopping,
            CueState::Broken => Self::Broken,
        }
    }
}

/// Why a cue broke while it ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireBrokenReason {
    VoiceRejected,
    StreamFailed,
    DeviceLost,
}

impl From<BrokenReason> for WireBrokenReason {
    fn from(reason: BrokenReason) -> Self {
        match reason {
            BrokenReason::VoiceRejected => Self::VoiceRejected,
            BrokenReason::StreamFailed => Self::StreamFailed,
            BrokenReason::DeviceLost => Self::DeviceLost,
        }
    }
}

/// Where a patch can play now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireResolution {
    Mapped {
        device: String,
        channels: Vec<u32>,
    },
    Unmapped,
    DeviceMissing {
        device: String,
    },
    ChannelsMissing {
        device: String,
        needed: u32,
        available: u32,
    },
    OutputCountMismatch {
        mapped: u64,
        outputs: u64,
    },
}

impl From<&PatchResolution> for WireResolution {
    fn from(resolution: &PatchResolution) -> Self {
        match resolution {
            PatchResolution::Mapped { device, channels } => Self::Mapped {
                device: device.clone(),
                channels: channels.clone(),
            },
            PatchResolution::Unmapped => Self::Unmapped,
            PatchResolution::DeviceMissing { device } => Self::DeviceMissing {
                device: device.clone(),
            },
            PatchResolution::ChannelsMissing {
                device,
                needed,
                available,
            } => Self::ChannelsMissing {
                device: device.clone(),
                needed: *needed,
                available: *available,
            },
            PatchResolution::OutputCountMismatch { mapped, outputs } => Self::OutputCountMismatch {
                mapped: *mapped as u64,
                outputs: *outputs as u64,
            },
        }
    }
}

/// A static problem that makes a cue broken (CUE-06). The UI localises from the variant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireProblem {
    NoTarget,
    TargetMissing {
        target: CueId,
    },
    TargetSelf,
    TargetWrongKind {
        target: CueId,
    },
    NoMedia,
    MediaMissing {
        media: MediaRef,
    },
    MediaUnsupported {
        media: MediaRef,
        reason: String,
    },
    PatchMissing {
        patch: PatchId,
    },
    DeviceUnavailable {
        patch: PatchId,
        resolution: WireResolution,
    },
    NetworkPatchMissing {
        patch: NetworkPatchId,
    },
    StartedBroken,
}

impl From<&CueProblem> for WireProblem {
    fn from(problem: &CueProblem) -> Self {
        match problem {
            CueProblem::NoTarget => Self::NoTarget,
            CueProblem::TargetMissing { target } => Self::TargetMissing { target: *target },
            CueProblem::TargetSelf => Self::TargetSelf,
            CueProblem::TargetWrongKind { target } => Self::TargetWrongKind { target: *target },
            CueProblem::NoMedia => Self::NoMedia,
            CueProblem::MediaMissing { media } => Self::MediaMissing {
                media: media.clone(),
            },
            CueProblem::MediaUnsupported { media, reason } => Self::MediaUnsupported {
                media: media.clone(),
                reason: reason.clone(),
            },
            CueProblem::PatchMissing { patch } => Self::PatchMissing { patch: *patch },
            CueProblem::DeviceUnavailable { patch, resolution } => Self::DeviceUnavailable {
                patch: *patch,
                resolution: resolution.into(),
            },
            CueProblem::NetworkPatchMissing { patch } => {
                Self::NetworkPatchMissing { patch: *patch }
            }
            CueProblem::StartedBroken => Self::StartedBroken,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireGoIgnored {
    DoubleGo,
    NoCue,
    UnknownList,
    ChainLimit,
}

impl From<GoIgnoredReason> for WireGoIgnored {
    fn from(reason: GoIgnoredReason) -> Self {
        match reason {
            GoIgnoredReason::DoubleGo => Self::DoubleGo,
            GoIgnoredReason::NoCue => Self::NoCue,
            GoIgnoredReason::UnknownList => Self::UnknownList,
            GoIgnoredReason::ChainLimit => Self::ChainLimit,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WirePreviewIgnored {
    NotAudio,
    AlreadyPreviewing,
}

impl From<PreviewIgnoredReason> for WirePreviewIgnored {
    fn from(reason: PreviewIgnoredReason) -> Self {
        match reason {
            PreviewIgnoredReason::NotAudio => Self::NotAudio,
            PreviewIgnoredReason::AlreadyPreviewing => Self::AlreadyPreviewing,
        }
    }
}

/// Something the operator should hear about; never stops the show. Mirrors the cue engine's
/// `EngineWarning` and adds what only the binary knows (`notice`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireWarning {
    UnknownList {
        list: CueListId,
    },
    UnknownCue {
        cue: CueId,
    },
    NotInList {
        list: CueListId,
        cue: CueId,
    },
    NumberNotFound {
        list: CueListId,
        number: String,
    },
    CueBroken {
        cue: CueId,
    },
    TriggerIgnored {
        cue: CueId,
        state: WireCueState,
    },
    LoadIgnored {
        cue: CueId,
    },
    Unsupported {
        cue: CueId,
    },
    /// `late_ms` milliseconds after its deadline, rounded down.
    LateStart {
        cue: CueId,
        late_ms: u64,
    },
    ChainLimit {
        cue: CueId,
        limit: u64,
    },
    CueRemoved {
        cue: CueId,
    },
    NoTarget {
        cue: CueId,
    },
    TargetMissing {
        cue: CueId,
        target: CueId,
    },
    TargetWrongKind {
        cue: CueId,
        target: CueId,
    },
    TargetNotRunning {
        cue: CueId,
        target: CueId,
    },
    PreviewIgnored {
        cue: CueId,
        reason: WirePreviewIgnored,
    },
    PreviewFailed {
        cue: CueId,
        reason: WireBrokenReason,
    },
    DevampIgnored {
        cue: CueId,
    },
    GoIgnored {
        list: CueListId,
        reason: WireGoIgnored,
    },
    /// From the binary: file repairs on load, damaged media, a failed autosave. `code` is a
    /// stable snake_case key the UI can localise; `message` is English for logs.
    Notice {
        code: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cue: Option<CueId>,
    },
}

impl WireWarning {
    /// `rate` is the device sample rate, to turn a late start in samples into time.
    pub fn from_engine(warning: &EngineWarning, rate: NonZeroU32) -> Self {
        match warning {
            EngineWarning::UnknownList { list } => Self::UnknownList { list: *list },
            EngineWarning::UnknownCue { cue } => Self::UnknownCue { cue: *cue },
            EngineWarning::NotInList { list, cue } => Self::NotInList {
                list: *list,
                cue: *cue,
            },
            EngineWarning::NumberNotFound { list, number } => Self::NumberNotFound {
                list: *list,
                number: number.clone(),
            },
            EngineWarning::CueBroken { cue } => Self::CueBroken { cue: *cue },
            EngineWarning::TriggerIgnored { cue, state } => Self::TriggerIgnored {
                cue: *cue,
                state: (*state).into(),
            },
            EngineWarning::LoadIgnored { cue } => Self::LoadIgnored { cue: *cue },
            EngineWarning::Unsupported { cue } => Self::Unsupported { cue: *cue },
            EngineWarning::LateStart { cue, late_by } => Self::LateStart {
                cue: *cue,
                late_ms: millis(*late_by, rate),
            },
            EngineWarning::ChainLimit { cue, limit } => Self::ChainLimit {
                cue: *cue,
                limit: *limit as u64,
            },
            EngineWarning::CueRemoved { cue } => Self::CueRemoved { cue: *cue },
            EngineWarning::NoTarget { cue } => Self::NoTarget { cue: *cue },
            EngineWarning::TargetMissing { cue, target } => Self::TargetMissing {
                cue: *cue,
                target: *target,
            },
            EngineWarning::TargetWrongKind { cue, target } => Self::TargetWrongKind {
                cue: *cue,
                target: *target,
            },
            EngineWarning::TargetNotRunning { cue, target } => Self::TargetNotRunning {
                cue: *cue,
                target: *target,
            },
            EngineWarning::PreviewIgnored { cue, reason } => Self::PreviewIgnored {
                cue: *cue,
                reason: (*reason).into(),
            },
            EngineWarning::PreviewFailed { cue, reason } => Self::PreviewFailed {
                cue: *cue,
                reason: (*reason).into(),
            },
            EngineWarning::DevampIgnored { cue } => Self::DevampIgnored { cue: *cue },
        }
    }
}

fn millis(samples: SampleTime, rate: NonZeroU32) -> u64 {
    // u128 so a huge sample count cannot overflow the multiplication.
    u64::try_from(u128::from(samples) * 1000 / u128::from(rate.get())).unwrap_or(u64::MAX)
}

/// One change to what the operator sees of the running show, event-driven. Carried by
/// `runtime { seq, changes }`; applied in order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeChange {
    /// `reason` is set when `state` is `broken` because of a run-time failure (not for static
    /// problems, which come as `problems`).
    CueState {
        cue: CueId,
        state: WireCueState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<WireBrokenReason>,
    },
    /// The cue's full list of static problems; empty when it has none.
    Problems {
        cue: CueId,
        problems: Vec<WireProblem>,
    },
    /// A group gained or lost a descendant with static problems: why the group shows as broken
    /// (QLab 5), although it has no problems of its own and still starts.
    GroupWarning {
        group: CueId,
        warning: bool,
    },
    /// The standing-by cue of a list; `null` = past the end or an empty list.
    Playhead {
        list: CueListId,
        cue: Option<CueId>,
    },
    /// The cue started or stopped being previewed.
    Preview {
        cue: CueId,
        active: bool,
    },
    Warning {
        warning: WireWarning,
    },
}

impl RuntimeChange {
    /// The change an engine action stands for, or `None` for actions that are for the mixer
    /// (voices, ramps) or the model (`SetArmed`). Call it after `handle` with the engine that
    /// returned the actions: a `StateChanged` to `Broken` takes its `reason` from
    /// `CueEngine::broken_reason`, which is `None` for a cue broken by static problems (those
    /// come as `problems`) and for a cue that has been reset since.
    pub fn from_action(action: &Action, engine: &CueEngine) -> Option<Self> {
        let rate = engine.config().sample_rate();
        Some(match action {
            Action::StateChanged { cue, state, .. } => Self::CueState {
                cue: *cue,
                state: (*state).into(),
                reason: match state {
                    CueState::Broken => engine.broken_reason(*cue).map(Into::into),
                    _ => None,
                },
            },
            Action::PlayheadChanged { list, cue } => Self::Playhead {
                list: *list,
                cue: *cue,
            },
            Action::GoIgnored { list, reason } => Self::Warning {
                warning: WireWarning::GoIgnored {
                    list: *list,
                    reason: (*reason).into(),
                },
            },
            Action::PreviewChanged { cue, active, .. } => Self::Preview {
                cue: *cue,
                active: *active,
            },
            Action::ProblemsChanged { cue, problems } => Self::Problems {
                cue: *cue,
                problems: problems.iter().map(WireProblem::from).collect(),
            },
            Action::GroupWarningChanged { group, warning } => Self::GroupWarning {
                group: *group,
                warning: *warning,
            },
            Action::Warning(warning) => Self::Warning {
                warning: WireWarning::from_engine(warning, rate),
            },
            Action::LoadVoice { .. }
            | Action::UnloadVoice { .. }
            | Action::StartVoice { .. }
            | Action::StopVoice { .. }
            | Action::PauseVoice { .. }
            | Action::ResumeVoice { .. }
            | Action::Ramp { .. }
            | Action::DevampVoice { .. }
            | Action::SetArmed { .. }
            | Action::PreviewStart { .. }
            | Action::PreviewStop { .. } => return None,
        })
    }
}
