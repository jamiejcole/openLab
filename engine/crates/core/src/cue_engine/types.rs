// SPDX-License-Identifier: GPL-3.0-or-later

//! What goes into and comes out of the cue engine. See `docs/cue-engine.md`.

use std::num::NonZeroU32;

use crate::cue::{FadeCurve, FadeSpec};
use crate::model::{CueId, CueListId};

use super::time::SampleTime;

/// Highest device rate the engine accepts (the mixer's limit).
pub const MAX_SAMPLE_RATE: u32 = 384_000;

/// Engine settings that come from the machine, not the show file. Built with [`EngineConfig::new`],
/// which checks them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineConfig {
    sample_rate: NonZeroU32,
    lookahead: SampleTime,
    seed: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EngineConfigError {
    #[error("sample rate {0} Hz is outside 1 to {MAX_SAMPLE_RATE} Hz")]
    SampleRate(u32),
    #[error("lookahead of {lookahead} samples is longer than one second ({max} samples)")]
    Lookahead {
        lookahead: SampleTime,
        max: SampleTime,
    },
}

impl EngineConfig {
    /// `sample_rate` is the audio device rate (1 to [`MAX_SAMPLE_RATE`] Hz); every show time is
    /// converted at it.
    ///
    /// `lookahead` is how far ahead of `now` a call commits deadlines: those in
    /// `now .. now + lookahead` (end excluded; lookahead 0 = only `now` and earlier). So their
    /// commands carry their exact sample even though the control loop wakes with jitter. The
    /// control loop sets it to one block (the mixer must never hold voice starts beyond the next
    /// block) and calls [`Input::Tick`] when [`CueEngine::is_due`](super::CueEngine::is_due). At
    /// most one second of samples.
    ///
    /// `seed` seeds the engine's random choices (random groups, shuffled playlists), so the same
    /// seed and inputs always give the same show. Tests pass a fixed seed; the engine binary
    /// passes fresh entropy each run.
    pub fn new(
        sample_rate: u32,
        lookahead: SampleTime,
        seed: u64,
    ) -> Result<Self, EngineConfigError> {
        let rate = NonZeroU32::new(sample_rate)
            .filter(|r| r.get() <= MAX_SAMPLE_RATE)
            .ok_or(EngineConfigError::SampleRate(sample_rate))?;
        let max = SampleTime::from(sample_rate);
        if lookahead > max {
            return Err(EngineConfigError::Lookahead { lookahead, max });
        }
        Ok(Self {
            sample_rate: rate,
            lookahead,
            seed,
        })
    }

    pub fn sample_rate(&self) -> NonZeroU32 {
        self.sample_rate
    }

    pub fn lookahead(&self) -> SampleTime {
        self.lookahead
    }

    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// The last sample a call at `now` commits deadlines on.
    pub(super) fn horizon(&self, now: SampleTime) -> SampleTime {
        now.saturating_add(self.lookahead.saturating_sub(1))
    }
}

/// Identifies one voice segment of a cue's run: from load or start until it stops or pauses, and
/// again from each resume (which gets a fresh id). Unique for the engine's lifetime, so it can be
/// used as the mixer's voice id. Feedback carries it back, and feedback whose id is no longer
/// current (an earlier run, or the segment before a resume) is ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RunId(pub u64);

/// Something that happened: an operator command, a timer tick or feedback from the audio side.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// Nothing new; carry out the deadlines that are due.
    Tick,
    /// GO on a cue list (PB-01).
    Go {
        list: CueListId,
    },
    /// Start a cue directly, without moving any playhead (CUE-05).
    Start {
        cue: CueId,
    },
    /// Stop a cue. With a fade of non-zero length a playing audio cue fades out first.
    Stop {
        cue: CueId,
        fade_out: Option<FadeSpec>,
    },
    Pause {
        cue: CueId,
    },
    Resume {
        cue: CueId,
    },
    /// Load a cue so it is ready to start at `time` seconds into its timeline (pre-wait, then
    /// action). `time` 0 is a plain load.
    Load {
        cue: CueId,
        time: f64,
    },
    /// Return a cue to idle, clearing a runtime failure.
    Reset {
        cue: CueId,
    },
    PlayheadNext {
        list: CueListId,
    },
    PlayheadPrevious {
        list: CueListId,
    },
    /// Put the playhead on a cue of the list it can stand on (a top-level cue, or a cue inside
    /// "start first and enter" groups); `None` moves it past the end.
    SetPlayhead {
        list: CueListId,
        cue: Option<CueId>,
    },
    /// Put the playhead on the cue of the list with this number (where it can stand on it).
    SetPlayheadByNumber {
        list: CueListId,
        number: String,
    },
    /// Feedback: the run's voice ended by itself (or its fade-out stop finished) at sample `at`.
    ActionEnded {
        cue: CueId,
        run: RunId,
        at: SampleTime,
    },
    /// Feedback: the run's voice or stream failed.
    ActionFailed {
        cue: CueId,
        run: RunId,
        reason: BrokenReason,
    },
}

/// The single state reported for a cue (CUE-04).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CueState {
    Idle,
    Loaded,
    PreWaiting,
    Running,
    Paused,
    /// The action is complete; the post-wait is still counting down to the next cue.
    PostWaiting,
    /// Its levels are being faded (entered from M2.6 Fade cues; not produced yet).
    Fading,
    /// Fading out before it stops.
    Stopping,
    Broken,
}

impl CueState {
    /// Whether the cue is doing anything (pre-waiting through stopping).
    pub fn is_active(self) -> bool {
        !matches!(self, Self::Idle | Self::Loaded | Self::Broken)
    }
}

/// Why a cue is broken. Static detection (missing media, device, target) comes with M2.8.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrokenReason {
    /// The mixer refused the voice.
    VoiceRejected,
    /// The disk stream could not continue or could not be prepared.
    StreamFailed,
    /// The output device went away.
    DeviceLost,
}

/// A fade-out applied by a stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StopFade {
    pub frames: SampleTime,
    pub curve: FadeCurve,
}

/// What the engine binary must do. Voice actions concern audio cues only; `offset` is in
/// device samples from the start of the cue's action (the trim in point is the binary's job).
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Prepare the run's stream so it can start at `offset`.
    LoadVoice {
        cue: CueId,
        run: RunId,
        offset: SampleTime,
    },
    /// Release a loaded run that never started.
    UnloadVoice {
        cue: CueId,
        run: RunId,
    },
    /// Start the run's voice on sample `at`, `offset` samples into the action.
    StartVoice {
        cue: CueId,
        run: RunId,
        at: SampleTime,
        offset: SampleTime,
    },
    /// Stop the run's voice on sample `at` (never before its start), optionally fading out.
    StopVoice {
        cue: CueId,
        run: RunId,
        at: SampleTime,
        fade: Option<StopFade>,
    },
    /// Pause the run's voice on sample `at`; it is then `offset` samples into the action.
    PauseVoice {
        cue: CueId,
        run: RunId,
        at: SampleTime,
        offset: SampleTime,
    },
    /// Resume the run's voice on sample `at` from `offset` samples into the action.
    ResumeVoice {
        cue: CueId,
        /// The resumed segment's id; feedback from now on must carry it.
        run: RunId,
        /// The id the voice had when it was paused.
        paused: RunId,
        at: SampleTime,
        offset: SampleTime,
    },
    /// The cue's reported state changed; `at` is the sample the change belongs to.
    StateChanged {
        cue: CueId,
        state: CueState,
        at: SampleTime,
    },
    /// The list's standing-by cue (CL-06) changed; `None` = past the end or an empty list.
    PlayheadChanged {
        list: CueListId,
        cue: Option<CueId>,
    },
    GoIgnored {
        list: CueListId,
        reason: GoIgnoredReason,
    },
    Warning(EngineWarning),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoIgnoredReason {
    /// Within `min_go_interval` of the previous accepted GO (PB-02).
    DoubleGo,
    /// The playhead is past the end of the list, or the list is empty.
    NoCue,
    UnknownList,
}

/// Things the operator or the log should hear about. Nothing here stops the show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineWarning {
    UnknownList {
        list: CueListId,
    },
    /// An operation named a cue the workspace does not have.
    UnknownCue {
        cue: CueId,
    },
    /// The playhead can only stand on a cue of its own list that is top-level or inside "start
    /// first and enter" groups only.
    NotInList {
        list: CueListId,
        cue: CueId,
    },
    NumberNotFound {
        list: CueListId,
        number: String,
    },
    /// A broken cue was asked to start or load; it stays broken until reset.
    CueBroken {
        cue: CueId,
    },
    /// Start/GO/continue on a cue that is already active (pre-waiting, running, post-waiting,
    /// fading or stopping) does nothing; `state` is the state it was in.
    TriggerIgnored {
        cue: CueId,
        state: CueState,
    },
    /// Load on a cue that is running (pre-waiting through stopping) does nothing.
    LoadIgnored {
        cue: CueId,
    },
    /// This cue kind's action is not implemented yet; it completed at once and did nothing.
    Unsupported {
        cue: CueId,
    },
    /// The voice started `late_by` samples after its deadline (a late tick). Later cues keep
    /// their timing.
    LateStart {
        cue: CueId,
        late_by: SampleTime,
    },
    /// More than `limit` cue starts in one call (a trigger cycle): this start was dropped.
    ChainLimit {
        cue: CueId,
        limit: usize,
    },
    /// A running cue was deleted from the workspace and has been stopped.
    CueRemoved {
        cue: CueId,
    },
}
