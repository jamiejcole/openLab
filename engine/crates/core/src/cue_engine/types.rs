// SPDX-License-Identifier: GPL-3.0-or-later

//! What goes into and comes out of the cue engine. See `docs/cue-engine.md`.

use std::num::NonZeroU32;

use std::fmt;

use crate::cue::{FadeCurve, FadeSpec, MediaRef};
use crate::model::{CueId, CueListId, NetworkPatchId, PatchId};
use crate::settings::PatchResolution;

use super::level::LevelParam;
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
    /// The hotkey or MIDI key that triggers `cue` was released (the binary sends it on key-up).
    /// Acts as a second trigger if the cue has `second_trigger_on_release` set and is active;
    /// otherwise it is ignored without a warning. See `docs/cue-engine.md`, "Second trigger".
    TriggerReleased {
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
    /// Put the playhead on a cue of the list (at any depth); `None` moves it past the end.
    SetPlayhead {
        list: CueListId,
        cue: Option<CueId>,
    },
    /// Put the playhead on the cue of the list with this number (at any depth).
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
    /// Feedback: the run's voice or stream failed before it produced any audio (the stream
    /// could not be opened or primed, its file was gone, the mixer refused the voice's first
    /// start). As `ActionFailed`, but the cue was only skipped, like a cue with static problems
    /// (QLab 5, checks P5/G2): its auto-follow still fires and its group still auto-follows.
    /// A failure after the voice has played audio is `ActionFailed`.
    StartFailed {
        cue: CueId,
        run: RunId,
        reason: BrokenReason,
    },
    /// From the binary, when it starts (or devamps) a voice whose end it knows in advance: the
    /// run's action is `frames` samples long, counted from the action's start (a load-to-time
    /// offset included), so it ends when the voice's position reaches `frames`. The engine
    /// then owns the end as a deadline on that exact sample (shifted by pause/resume, gone with
    /// a stop or a failure), so a continue or playlist move comes on the end sample itself, not
    /// when the mixer's report arrives. The voice's `ActionEnded` still counts if it comes
    /// first (an earlier end); after the deadline it is stale. A later `ActionLength` (after a
    /// devamp) replaces the earlier one. See `docs/cue-engine.md`, "Feedback".
    ActionLength {
        cue: CueId,
        run: RunId,
        frames: SampleTime,
    },
    /// Feedback (`DevampScheduled`): the devamp sent for this run's voice was accepted; the voice
    /// leaves its loop on sample `at`.
    DevampScheduled {
        cue: CueId,
        run: RunId,
        at: SampleTime,
    },
    /// Feedback (`DevampIgnored`): the devamp sent for this run's voice cannot be carried out
    /// (no loop to leave); playback is unchanged.
    DevampIgnored {
        cue: CueId,
        run: RunId,
    },
    /// Preview (audition) an audio cue from its start, outside the show (CUE-05): no pre-wait,
    /// continue, playhead or show state is involved. See `docs/cue-engine.md`, "Preview".
    Preview {
        cue: CueId,
    },
    /// End the cue's preview (`None`: every preview).
    StopPreview {
        cue: Option<CueId>,
    },
    /// Panic (PB-03): every cue fades out over the workspace's `panic_duration` and stops; a
    /// second Panic during that fade stops everything at once.
    Panic,
    /// GO from a given cue (OSC `/go "3"`): the list's playhead moves to `cue`, then GO. One input,
    /// so nothing starts if the cue is not on the list's playhead positions (a stale client
    /// cannot fire the old playhead cue): the engine warns `NotInList` and does nothing else.
    GoFrom {
        list: CueListId,
        cue: CueId,
    },
    /// Stop All: every cue of every list, at every depth, and every preview, stops at once (no
    /// fades, nothing pending fires). Unlike Panic it starts no fade and leaves no panic window.
    StopAll,
    /// One cue's own panic, as its `panic` second trigger: the cue and everything in it fade out
    /// over `panic_duration` (0: stop at once). Other cues and the workspace panic are untouched.
    PanicCue {
        cue: CueId,
    },
    /// Pause every active cue (PB-04).
    PauseAll,
    /// Resume every paused cue (PB-04).
    ResumeAll,
    /// Availability of a media file, from the binary's probe (M3). Every Audio cue whose `media`
    /// is this reference uses it. Damaged-but-playable media is `Available` (its warnings are
    /// the binary's to show).
    MediaStatus {
        media: MediaRef,
        status: MediaStatus,
    },
    /// Where an output patch can play now (`resolve_patch` against the devices present, M1-C).
    /// Every resolution but `Mapped` breaks the Audio cues on that patch.
    PatchStatus {
        patch: PatchId,
        resolution: PatchResolution,
    },
    /// Many `MediaStatus` and `PatchStatus` reports at once (workspace open, a rescan, a drive
    /// that went away): one call, each affected cue recomputed once.
    Availability {
        media: Vec<(MediaRef, MediaStatus)>,
        patches: Vec<(PatchId, PatchResolution)>,
    },
    /// The output device went away (M4.2g, as QLab 5). The binary has already failed the audio
    /// runs with `ActionFailed { device_lost }`; this hard-stops every other active run (memo,
    /// Wait, control, Fade and group cues, and runs broken earlier), so no pre-wait, post-wait,
    /// auto-continue or auto-follow set before the loss fires after the device returns. Loaded
    /// and settled-broken cues keep their state.
    DeviceLost,
}

/// What the binary knows about a media file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaStatus {
    /// Not probed (yet), or forgotten: the engine assumes it plays.
    Unknown,
    /// Probed and playable (possibly with damage warnings).
    Available,
    /// Not found on disk.
    Missing,
    /// Found but cannot be played; `reason` is the probe's message.
    Unsupported { reason: String },
}

/// Why a cue cannot run, found without running it (CUE-06, NFR-07). A cue may have several.
/// `Display` gives an English sentence for logs; the UI localises from the variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CueProblem {
    /// A targeting cue with no target set.
    NoTarget,
    /// The target is not in the workspace.
    TargetMissing {
        target: CueId,
    },
    /// The cue targets itself.
    TargetSelf,
    /// The target is a kind this cue cannot act on (a Fade needs an Audio or Group cue, a Devamp
    /// an Audio cue or a playlist group).
    TargetWrongKind {
        target: CueId,
    },
    /// An Audio cue with no media file.
    NoMedia,
    MediaMissing {
        media: MediaRef,
    },
    MediaUnsupported {
        media: MediaRef,
        reason: String,
    },
    /// An Audio cue's patch is not in the workspace.
    PatchMissing {
        patch: PatchId,
    },
    /// An Audio cue's patch has no device to play on (`resolution` is never `Mapped`).
    DeviceUnavailable {
        patch: PatchId,
        resolution: PatchResolution,
    },
    /// A Network cue's destination is not in the workspace.
    NetworkPatchMissing {
        patch: NetworkPatchId,
    },
    /// It was triggered while it had problems, which have been fixed since; the run it started
    /// (its timing, without its action) has not ended yet. Gone when that run ends.
    StartedBroken,
}

impl fmt::Display for CueProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoTarget => write!(f, "no target set"),
            Self::TargetMissing { target } => write!(f, "target {target} does not exist"),
            Self::TargetSelf => write!(f, "targets itself"),
            Self::TargetWrongKind { target } => {
                write!(f, "target {target} is the wrong kind of cue")
            }
            Self::NoMedia => write!(f, "no media file"),
            Self::MediaMissing { media } => write!(f, "media file {} not found", media.as_str()),
            Self::MediaUnsupported { media, reason } => {
                write!(
                    f,
                    "media file {} cannot be played: {reason}",
                    media.as_str()
                )
            }
            Self::PatchMissing { patch } => write!(f, "patch {patch} does not exist"),
            Self::DeviceUnavailable { patch, resolution } => match resolution {
                PatchResolution::Unmapped => {
                    write!(f, "patch {patch} is not mapped to a device on this machine")
                }
                PatchResolution::DeviceMissing { device } => {
                    write!(
                        f,
                        "audio device \"{device}\" of patch {patch} is not present"
                    )
                }
                PatchResolution::ChannelsMissing {
                    device,
                    needed,
                    available,
                } => write!(
                    f,
                    "audio device \"{device}\" has {available} outputs; patch {patch} needs {needed}"
                ),
                PatchResolution::OutputCountMismatch { mapped, outputs } => write!(
                    f,
                    "patch {patch} has {outputs} outputs but its mapping lists {mapped}"
                ),
                PatchResolution::Mapped { .. } => write!(f, "patch {patch} is available"),
            },
            Self::StartedBroken => write!(
                f,
                "was broken when it was triggered; fixed since, it runs normally next time"
            ),
            Self::NetworkPatchMissing { patch } => {
                write!(f, "network patch {patch} does not exist")
            }
        }
    }
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
    /// Its action is under way and a running Fade cue is moving its levels.
    Fading,
    /// Fading out before it stops.
    Stopping,
    /// It broke while it ran, or it has static problems (`CueProblem`) and no runtime.
    Broken,
}

impl CueState {
    /// Whether the cue is doing anything (pre-waiting through stopping).
    pub fn is_active(self) -> bool {
        !matches!(self, Self::Idle | Self::Loaded | Self::Broken)
    }
}

/// Why a cue broke while it ran (feedback). Problems found without running it are
/// [`CueProblem`]s.
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
    /// Ramp one gain of the run's voice to `gain` (linear amplitude, as the mixer's `Ramp`) over
    /// `frames` samples from sample `at`, with `curve`. The mixer starts the ramp from the value
    /// the gain has on `at`, so an interrupted ramp continues smoothly. `frames` 0 sets the gain
    /// (de-zippered, or at once on the voice's start sample).
    Ramp {
        cue: CueId,
        run: RunId,
        at: SampleTime,
        param: LevelParam,
        gain: f32,
        frames: SampleTime,
        curve: FadeCurve,
    },
    /// Leave the run's voice's current loop at the end of the play under way when the command
    /// applies on sample `at` (the mixer's `Devamp`); `stop` ends the voice there. The audio side
    /// answers with [`Input::DevampScheduled`] or [`Input::DevampIgnored`].
    DevampVoice {
        cue: CueId,
        run: RunId,
        at: SampleTime,
        stop: bool,
    },
    /// An Arm/Disarm cue changed whether `cue` is armed. The binary applies it to the model as a
    /// runtime change (not an undo step); the engine honours it at once, and until the model
    /// shows it.
    SetArmed {
        cue: CueId,
        armed: bool,
    },
    /// Start the preview voice of `cue` on sample `at`, from the start of the cue's action. The
    /// binary plays it on the audition output (PB-09; until there is one, on the cue's own
    /// patch) and tags feedback with `run`: `Input::ActionEnded` / `ActionFailed` for it end the
    /// preview. A preview's `run` never equals a show run's.
    PreviewStart {
        cue: CueId,
        run: RunId,
        at: SampleTime,
    },
    /// Stop the preview voice on sample `at` (never before its start), fading out if `fade`.
    PreviewStop {
        cue: CueId,
        run: RunId,
        at: SampleTime,
        fade: Option<StopFade>,
    },
    /// The cue started or stopped being previewed (a preview is not a cue state: the cue's own
    /// state is untouched). Also sent when the preview ends by itself or fails.
    PreviewChanged {
        cue: CueId,
        active: bool,
        at: SampleTime,
    },
    /// The cue's static problems changed (CUE-06): `problems` is the full new list, empty when
    /// it is no longer broken. A cue that has problems and no runtime reports `broken`.
    ProblemsChanged {
        cue: CueId,
        problems: Vec<CueProblem>,
    },
    /// A group gained or lost a descendant (any depth) shown with static problems: `warning`
    /// is the new `CueEngine::has_broken_descendant`. It is why the group shows as broken (QLab 5;
    /// a group with no runtime reports the state `broken` meanwhile) although it has no problems
    /// of its own and still starts. Also sent, with `false`, once for a deleted group that had
    /// one.
    GroupWarningChanged {
        group: CueId,
        warning: bool,
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
    /// The sample's chain budget was already spent (a trigger cycle earlier in the call, or in
    /// an earlier call at the same `now`), so the cue could not be started: nothing happened (no
    /// start, no playhead move, no double-GO interval) and the operator can press GO again.
    ChainLimit,
}

/// Why a preview did not start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewIgnoredReason {
    /// Only audio cues can be previewed.
    NotAudio,
    /// The cue is being previewed already.
    AlreadyPreviewing,
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
    /// The playhead can only stand on a cue of its own list.
    NotInList {
        list: CueListId,
        cue: CueId,
    },
    NumberNotFound {
        list: CueListId,
        number: String,
    },
    /// A broken cue was asked to start or load. A cue broken at run time stays broken until
    /// reset and its start does nothing; one with static problems stays broken until they are
    /// fixed, and its start keeps its timing (its auto-continue fires).
    CueBroken {
        cue: CueId,
    },
    /// Start/GO/continue on a cue that is already active (pre-waiting, running, post-waiting,
    /// fading or stopping) did nothing; `state` is the state it was in. Its second-trigger option
    /// is `nothing`, or it is `play_next`/`play_previous` and the playlist was not playing a
    /// child it could move on from (not in its action, stopping or devamped).
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
    /// More than `limit` cue starts at one `now` (a trigger cycle): this start was dropped.
    ChainLimit {
        cue: CueId,
        limit: usize,
    },
    /// A running cue was deleted from the workspace and has been stopped.
    CueRemoved {
        cue: CueId,
    },
    /// A control or Fade cue acted with no target set; it did nothing.
    NoTarget {
        cue: CueId,
    },
    /// A control or Fade cue's target is not in the workspace; it did nothing.
    TargetMissing {
        cue: CueId,
        target: CueId,
    },
    /// A Fade or Devamp cue's target is a kind it cannot act on; it did nothing.
    TargetWrongKind {
        cue: CueId,
        target: CueId,
    },
    /// A Fade or Devamp cue's target (or, for a Fade on a group, every audio cue in it) was not
    /// playing when it acted; it did nothing.
    TargetNotRunning {
        cue: CueId,
        target: CueId,
    },
    /// A preview was asked for and not started.
    PreviewIgnored {
        cue: CueId,
        reason: PreviewIgnoredReason,
    },
    /// A preview's voice or stream failed; the preview is over.
    PreviewFailed {
        cue: CueId,
        reason: BrokenReason,
    },
    /// A devamp of `cue` could not be carried out: the audio side ignored it (no loop to leave),
    /// a devamp of it is already under way, or it is a playlist group that does not loop.
    DevampIgnored {
        cue: CueId,
    },
}
