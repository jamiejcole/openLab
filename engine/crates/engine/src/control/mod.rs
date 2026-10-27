// SPDX-License-Identifier: GPL-3.0-or-later

//! The control thread (ARCHITECTURE §3.1, §3.2; docs/control.md): owns the workspace, its undo
//! history, the cue engine and the mixer controller, and wires them to the disk streams.
//!
//! [`Control`] is the loop's state and logic, with no thread or clock of its own: time is the
//! mixer's sample counter (`samples_rendered`), whether a device callback ([`runner::run`]) or
//! the offline render driver ([`offline::OfflineShow`], tests) advances it. Both call the same
//! [`Control::handle`] for each inbox message and [`Control::tick`] on every wake.

mod checks;
pub mod devices;
mod docs;
mod inbox;
mod media;
mod observe;
pub mod offline;
mod patches;
pub mod plan;
mod preshow;
pub mod runner;
mod voices;

use std::collections::VecDeque;
use std::hash::{BuildHasher, RandomState};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crossbeam_channel::Sender;
use openlab_audio::{
    COMMAND_CAPACITY, CommandKind, DiskStreamer, MediaError, MixerController, StreamerConfig,
    VoiceId,
};
use openlab_core::protocol::{
    ClientMessage, CueMeter, CueProblems, CueStateEntry, DeviceEntry, DocumentState, ErrorBody,
    ErrorCode, LogKind, LogLevel, MAX_COALESCE_KEY, OkBody, Outcome, OutputMeter, PlayheadEntry,
    ProgressItem, Request, ResultData, RuntimeChange, ServerMessage, Snapshot, TransportAction,
    WireProblem, WireWarning,
};
use openlab_core::{
    Action, BrokenReason, Command, CommandError, Cue, CueEngine, CueId, CueKind, CueState,
    DeviceInfo, EngineConfig, EngineConfigError, Footprint, History, Input, MachineSettings,
    MediaStatus, PatchResolution, SampleTime, Workspace, model_changes, model_changes_indexed,
    seconds_to_samples,
};

pub use docs::{DocAnswer, DocHook, EpochClock, Loaded, ManualClock, SaveHook};
pub use inbox::{
    Inbox, OUTBOUND_CAPACITY, OutboundRx, OutboundTx, ProbeAnswer, ReplyRx, ReplyTx,
    outbound_channel, reply_channel,
};
pub use media::{PROBE_WORKERS, ProbeClock};

use crate::persist::AppDirs;
use checks::PlanChecks;
use devices::{Device, DeviceAnswer, DeviceEvent};
use docs::Docs;
use inbox::{Outbound, reply};
use media::MediaCatalog;
use observe::Observe;
use patches::Patches;
use voices::{Effects, Env, Voices};

/// The longest the loop sleeps without a reason to wake (ARCHITECTURE §3.2).
pub const MAX_WAIT: Duration = Duration::from_millis(5);
/// The longest floor on the wait after a tick that saw sample time standing still (see
/// [`stall_floor`]): the mixer renders whole blocks, so waiting less for a deadline to enter
/// the window only spins the loop (a stalled device before the watchdog declares it lost, or
/// the wake between two callbacks).
pub const MIN_STALL_WAIT: Duration = Duration::from_millis(1);
/// Progress and meters go out at most this often (docs/protocol.md §4).
pub const REPORT_HZ: u64 = 30;
/// Cues primed from each playhead (ARCHITECTURE §3.5).
pub const PRIME_CUES: usize = 16;
/// Most streams kept primed at once.
pub const MAX_PRIMES: usize = 64;
/// A start held up by its file's probe fails after this long (sample time), so the cue goes
/// broken instead of showing as running with no sound (docs/control.md).
pub const PROBE_HOLD_LIMIT: Duration = Duration::from_secs(5);
/// A document read stuck this long fails with `io` and its worker is replaced.
pub const DOC_TIMEOUT: Duration = Duration::from_secs(20);
/// A probe worker stuck on one file this long is replaced.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// Feedback rounds one input may cause before the loop stops following them (a safety net; the
/// cue engine bounds its own chains).
const FEEDBACK_LIMIT: usize = 4096;

/// How the control loop reaches the audio side, besides the mixer controller. The clock is the
/// mixer's either way; this carries what only a device has.
pub enum Backend {
    /// A device stream on the device thread: list, loss and reopen ([`devices`]).
    Device(Box<Device>),
    /// The offline render driver: nothing but the clock.
    Offline,
}

impl Backend {
    fn alloc_violations(&self) -> u64 {
        match self {
            Self::Device(d) => d.alloc_violations(),
            Self::Offline => 0,
        }
    }

    fn device(&self) -> Option<&Device> {
        match self {
            Self::Device(d) => Some(d),
            Self::Offline => None,
        }
    }

    /// The device is down (an outage): sample time stands still.
    fn is_down(&self) -> bool {
        self.device().is_some_and(Device::is_down)
    }
}

/// What the control loop is started with.
#[derive(Clone, Debug)]
pub struct ControlConfig {
    /// The mixer's (device) rate.
    pub sample_rate: u32,
    /// Frames per device block: the scheduling lookahead (`now = samples_rendered + block`).
    pub block_frames: u64,
    /// Seeds random groups and shuffled playlists ([`entropy_seed`] for a show).
    pub seed: u64,
    /// The show file, if saved: relative media references resolve against its folder.
    pub show_path: Option<PathBuf>,
    pub machine: MachineSettings,
    /// Devices present, for the offline backend (a device backend plays on its open stream's
    /// device and lists what the host lists).
    pub devices: Vec<DeviceInfo>,
    /// Output patch matrices the mixer was built with.
    pub mixer_patches: usize,
    pub streamer: StreamerConfig,
    /// Memory all preloaded media may take together (AUD-05).
    pub preload_budget: u64,
    pub prime_cues: usize,
    pub max_primes: usize,
    /// Where backups, autosaves and the settings file live. `None`: nothing can be saved
    /// (saves answer `io`), autosave and recovery are off.
    pub app_dirs: Option<AppDirs>,
    /// Wall-clock milliseconds for backup names and autosave pacing (tests inject one).
    pub clock: EpochClock,
    /// Called on the writer thread before each save (tests only).
    pub save_hook: Option<SaveHook>,
    /// Called on the document worker before each read (tests only).
    pub doc_hook: Option<DocHook>,
    /// A document read that has not answered after this long fails with `io` and its worker is
    /// replaced (a stalled network volume), like [`Self::probe_timeout`].
    pub doc_timeout: Duration,
    /// How files are probed (tests inject a stand-in).
    pub probe: media::ProbeFn,
    /// A probe worker busy on one file longer than this is replaced (a stalled volume).
    pub probe_timeout: Duration,
    /// The clock `probe_timeout` is measured on (tests inject one that only moves when told).
    pub probe_clock: media::ProbeClock,
    /// The show log: its ring always, and the disk writer's sink when the host started one
    /// (docs/logging.md).
    pub show_log: crate::diag::ShowLog,
    /// Send `status` messages (DX-03). `None`: never (offline tests; the host turns it on).
    pub status: Option<crate::diag::StatusConfig>,
    /// Process CPU seconds for `status` (tests inject a fake).
    pub cpu_probe: fn() -> Option<f64>,
}

impl ControlConfig {
    pub fn new(sample_rate: u32, block_frames: u64) -> Self {
        Self {
            sample_rate,
            block_frames,
            seed: entropy_seed(),
            show_path: None,
            machine: MachineSettings::default(),
            devices: Vec::new(),
            mixer_patches: 1,
            streamer: StreamerConfig::new(sample_rate),
            preload_budget: openlab_audio::DEFAULT_PRELOAD_BUDGET,
            app_dirs: None,
            clock: EpochClock::system(),
            save_hook: None,
            doc_hook: None,
            doc_timeout: DOC_TIMEOUT,
            prime_cues: PRIME_CUES,
            max_primes: MAX_PRIMES,
            probe: openlab_audio::probe,
            probe_timeout: PROBE_TIMEOUT,
            probe_clock: media::ProbeClock::system(),
            show_log: crate::diag::ShowLog::default(),
            status: None,
            cpu_probe: crate::diag::cpu::process_cpu_seconds,
        }
    }
}

/// Fresh seed for each run, from the OS-seeded keys of std's `RandomState` mixed with the time
/// and process id (no extra dependency), so shows with random groups differ between runs.
pub fn entropy_seed() -> u64 {
    let time = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    RandomState::new().hash_one((time, std::process::id()))
}

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error("engine settings: {0}")]
    Config(#[from] EngineConfigError),
    #[error("disk streamer: {0}")]
    Streamer(MediaError),
    #[error("could not start a worker thread: {0}")]
    Thread(#[from] std::io::Error),
}

/// Whether the loop goes on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Shutdown,
}

/// The control loop's state. See the module docs and docs/control.md.
pub struct Control {
    ws: Workspace,
    history: History,
    engine: CueEngine,
    mixer: MixerController,
    streamer: DiskStreamer,
    media: MediaCatalog,
    voices: Voices,
    checks: PlanChecks,
    patches: Patches,
    /// What patches resolve against: the device the mixer plays on (none during an outage).
    playable: Vec<DeviceInfo>,
    out: Outbound,
    docs: Docs,
    backend: Backend,
    config: ControlConfig,
    rate: NonZeroU32,
    /// Frames per device block: the lookahead and the `now` offset. It follows the open
    /// stream (a reopen may bring another buffer size).
    block: u64,
    /// `now` never goes back: after a reopen with a smaller block it stays at least where it
    /// was.
    now_floor: SampleTime,
    /// `samples_rendered` at the last tick, and whether that tick saw it unchanged.
    last_tick_rendered: SampleTime,
    last_tick_stalled: bool,
    cue_outputs: usize,
    revision: u64,
    saved_revision: u64,
    runtime_seq: u64,
    /// Feedback dated on a sample the loop has not passed yet.
    deferred: Vec<(SampleTime, Input)>,
    /// Probe answers not yet handed to the engine (sent together as `Availability`).
    pending_media: Vec<(openlab_core::MediaRef, MediaStatus)>,
    ready_media: Vec<String>,
    primes_dirty: bool,
    warnings_dirty: bool,
    warnings: Vec<WireWarning>,
    /// Edits whose `model_changed` was found by walking the workspace (the cue index could
    /// not serve; a cost measure).
    model_change_walks: u64,
    /// A `document` / `warnings` message did not fit the outbound channel: send the latest
    /// again once there is room.
    resend_document: bool,
    resend_warnings: bool,
    seen: Seen,
    last_progress: Option<SampleTime>,
    last_meters: Option<SampleTime>,
    meters_live: bool,
    /// Rendered sample of the previous voice-meter read (reconciliation).
    last_meter_read: Option<SampleTime>,
    out_peaks: Vec<f32>,
    meter_ids: Vec<VoiceId>,
    meter_peaks: Vec<f32>,
    /// Show-log and status bookkeeping (`observe.rs`).
    observe: Observe,
}

/// Counters already reported, so each increase is reported once.
#[derive(Default)]
struct Seen {
    unsignalled: u64,
    dropped_events: u64,
    faults: u64,
    alloc_violations: u64,
    outbound_dropped: u64,
    feedback_overflow: u64,
}

impl Control {
    /// Builds the control state for `ws` around a mixer controller whose mixer is driven by
    /// `backend`. Starts the disk workers and the probe workers (which answer on `inbox`), sends
    /// the patches to the mixer and the cue engine, and probes every media file of the show.
    pub fn new(
        ws: Workspace,
        mixer: MixerController,
        backend: Backend,
        config: ControlConfig,
        inbox: Sender<Inbox>,
        outbound: OutboundTx,
    ) -> Result<Self, ControlError> {
        let block = config.block_frames.max(1);
        let engine = CueEngine::new(EngineConfig::new(config.sample_rate, block, config.seed)?);
        let rate = engine.config().sample_rate();
        let streamer = DiskStreamer::new(config.streamer).map_err(ControlError::Streamer)?;
        let show_dir = config
            .show_path
            .as_ref()
            .and_then(|p| p.parent())
            .map(PathBuf::from);
        let docs = Docs::new(&config, inbox.clone())?;
        let media = MediaCatalog::new(
            inbox,
            show_dir,
            config.preload_budget,
            config.probe,
            config.probe_timeout,
            config.probe_clock.clone(),
        )?;
        let cue_outputs = mixer.cue_outputs();
        let patches = Patches::new(config.mixer_patches, cue_outputs, rate);
        let playable = match backend.device() {
            Some(d) => vec![playable_device(d)],
            None => config.devices.clone(),
        };
        let observe = Observe::new(&config);
        let mut control = Self {
            ws,
            history: History::new(),
            engine,
            mixer,
            streamer,
            media,
            voices: Voices::new(),
            checks: PlanChecks::default(),
            patches,
            playable,
            out: Outbound::new(outbound),
            docs,
            backend,
            config,
            rate,
            block,
            now_floor: 0,
            last_tick_rendered: 0,
            last_tick_stalled: false,
            cue_outputs,
            revision: 0,
            saved_revision: 0,
            runtime_seq: 0,
            deferred: Vec::new(),
            pending_media: Vec::new(),
            ready_media: Vec::new(),
            primes_dirty: true,
            warnings_dirty: true,
            warnings: Vec::new(),
            model_change_walks: 0,
            resend_document: false,
            resend_warnings: false,
            seen: Seen::default(),
            last_progress: None,
            last_meters: None,
            meters_live: false,
            last_meter_read: None,
            out_peaks: vec![0.0; cue_outputs],
            meter_ids: Vec::new(),
            meter_peaks: Vec::new(),
            observe,
        };
        control.record(
            LogKind::Engine,
            LogLevel::Info,
            format!(
                "openlab-engine {} started, {} Hz, {} frames per block",
                env!("CARGO_PKG_VERSION"),
                rate,
                block
            ),
            None,
            None,
        );
        let cap = control.mixer.voice_meter_capacity();
        control.meter_ids = vec![VoiceId(0); cap];
        control.meter_peaks = vec![0.0; cap * cue_outputs.max(1)];
        let now = control.now();
        // A newly opened workspace: tell the engine, report the playheads (docs/cue-engine.md).
        control.engine.model_changed();
        control.media.sync(&control.ws);
        let mut inputs = vec![Input::Tick];
        inputs.extend(control.sync_workspace(true));
        for input in inputs {
            control.feed(input, now);
        }
        control.mixer.pump();
        Ok(control)
    }

    /// The scheduling time: the earliest sample a command can still hit.
    fn now(&self) -> SampleTime {
        self.mixer
            .samples_rendered()
            .saturating_add(self.block)
            .max(self.now_floor)
    }

    /// Handles one inbox message.
    pub fn handle(&mut self, msg: Inbox) -> Flow {
        match msg {
            Inbox::Client { message, reply } => return self.client(message, &reply),
            Inbox::Probe(answer) => self.probe_answer(answer),
            Inbox::Device(answer) => self.device_answer(answer),
            Inbox::Doc(answer) => self.doc_answer(answer),
            Inbox::Log { level, message } => self.log(level, message),
            Inbox::Trigger { source, detail } => self.observe_trigger(source, detail),
            Inbox::Shutdown => return Flow::Shutdown,
        }
        Flow::Continue
    }

    /// One wake of the loop: feedback from the mixer, the streams and the probes, due
    /// deadlines, priming, reports, then `pump` (ARCHITECTURE §3.2; docs/audio.md: every tick).
    pub fn tick(&mut self) {
        let rendered = self.mixer.samples_rendered();
        self.last_tick_stalled = rendered == self.last_tick_rendered;
        self.last_tick_rendered = rendered;
        self.poll_device();
        let now = self.now();
        self.flush_media(now);
        let mut fx = Effects::default();
        while let Some(e) = self.mixer.poll_event() {
            self.voices.event(e, &mut fx);
        }
        self.voices.poll_streams(&mut fx, &self.ws);
        let hold = seconds_to_samples(PROBE_HOLD_LIMIT.as_secs_f64(), self.rate);
        self.voices.expire_held(now, hold, &mut fx);
        self.follow(fx, now);
        let stuck = self.media.watchdog();
        if !stuck.is_empty() {
            self.log(
                LogLevel::Error,
                format!(
                    "{} media probe(s) stuck; workers replaced ({} so far)",
                    stuck.len(),
                    self.media.abandoned_workers()
                ),
            );
            self.pending_media.extend(stuck);
        }
        if self.media.take_limit_reached() {
            self.log(
                LogLevel::Error,
                format!(
                    "{} probe workers were stuck and replaced; no more are replaced, so further \
                     stuck probes wait (their starts fail after the hold limit)",
                    media::MAX_ABANDONED_WORKERS
                ),
            );
        }
        self.deliver_deferred(now);
        if self.engine.is_due(now) {
            self.feed(Input::Tick, now);
        }
        if self.primes_dirty {
            self.primes_dirty = false;
            self.prime(now);
        }
        self.reports();
        if self.warnings_dirty {
            self.warnings_dirty = false;
            self.refresh_warnings();
        }
        self.docs_tick();
        self.health();
        self.observe_tick();
        self.mixer.pump();
    }

    /// How long the loop may wait for the inbox before it must tick again: until the next
    /// deadline enters the lookahead window, a deferred report becomes due, or [`MAX_WAIT`].
    pub fn wait_hint(&self) -> Duration {
        // Nothing becomes due by waiting while sample time stands still.
        if self.backend.is_down() {
            return MAX_WAIT;
        }
        let frames = wake_frames(
            self.engine.next_deadline(),
            self.deferred.iter().map(|(at, _)| *at),
            self.mixer.samples_rendered(),
            self.block,
        );
        let secs = frames as f64 / f64::from(self.rate.get());
        let wait = Duration::from_secs_f64(secs.min(MAX_WAIT.as_secs_f64()));
        // A deadline enters the window only when sample time moves, and it moves in whole
        // blocks. Whenever the last tick saw it standing still (a stalled device, up to the
        // watchdog limit, but also the wake between two ordinary callbacks), waking sooner
        // would spin. The floor is a quarter block at most, so a small device block cannot
        // slip past the window. The first wake after a tick that saw progress is not floored,
        // and a message ends any wait at once.
        if self.last_tick_stalled {
            wait.max(stall_floor(self.block, self.rate.get()))
        } else {
            wait
        }
    }

    fn client(&mut self, message: ClientMessage, to: &ReplyTx) -> Flow {
        let now = self.now();
        match message {
            ClientMessage::Transport { id, action } => {
                let scope = orphan_scope(&action);
                let panic = matches!(action, TransportAction::Panic);
                self.observe_transport(&action);
                self.feed(transport_input(action), now);
                // During an outage a panic fade could only play once the device is back, so
                // it stops everything at once: a second Panic within the fade is a hard stop
                // (unless this one already was).
                if panic && self.backend.is_down() && self.engine.panic_end().is_some() {
                    self.feed(Input::Panic, now);
                }
                if let Some(scope) = scope {
                    self.silence_orphans(scope, now);
                }
                reply(to, ok(id, None));
            }
            ClientMessage::Command {
                id,
                command,
                coalesce,
            } => {
                if coalesce
                    .as_ref()
                    .is_some_and(|k| k.len() > MAX_COALESCE_KEY)
                {
                    reply(
                        to,
                        ServerMessage::Result {
                            id,
                            outcome: Outcome::Error(ErrorBody::new(
                                ErrorCode::BadRequest,
                                format!("coalesce key longer than {MAX_COALESCE_KEY} bytes"),
                            )),
                        },
                    );
                    return Flow::Continue;
                }
                if let Some(e) = self.patch_layout_conflict(Some(&command), false) {
                    reply(to, refused(id, &e));
                    return Flow::Continue;
                }
                let footprint = command.footprint();
                match self
                    .history
                    .execute_coalescing(&mut self.ws, *command, coalesce.as_deref())
                {
                    Ok(changed) => {
                        // An empty transaction, or an edit that sets what is already there,
                        // changed nothing and is no undo step: no revision, no
                        // `model_changed`, not dirty.
                        if changed {
                            self.edited(now, &footprint);
                        }
                        reply(to, ok(id, Some(self.revision)));
                    }
                    Err(e) => reply(to, refused(id, &e)),
                }
            }
            ClientMessage::Undo { id } | ClientMessage::Redo { id } => {
                let undo = matches!(message, ClientMessage::Undo { .. });
                // The step about to be applied says what it changes.
                let footprint = if undo {
                    self.history.peek_undo()
                } else {
                    self.history.peek_redo()
                }
                .map(Command::footprint)
                .unwrap_or_default();
                let step = if undo {
                    self.history.peek_undo()
                } else {
                    self.history.peek_redo()
                };
                if let Some(e) = self.patch_layout_conflict(step, true) {
                    reply(to, refused(id, &e));
                    return Flow::Continue;
                }
                let result = if undo {
                    self.history.undo(&mut self.ws)
                } else {
                    self.history.redo(&mut self.ws)
                };
                match result {
                    Ok(true) => {
                        self.edited(now, &footprint);
                        reply(to, ok(id, Some(self.revision)));
                    }
                    Ok(false) => reply(to, ok(id, None)),
                    Err(e) => reply(to, refused(id, &e)),
                }
            }
            ClientMessage::Request { id, request } => match request {
                Request::Snapshot => {
                    reply(
                        to,
                        ServerMessage::Snapshot {
                            snapshot: Box::new(self.snapshot()),
                        },
                    );
                    reply(to, ok(id, None));
                }
                Request::ListDevices => reply(
                    to,
                    ServerMessage::Result {
                        id,
                        outcome: Outcome::Ok(OkBody {
                            revision: None,
                            data: Some(ResultData::Devices {
                                devices: self.device_list(),
                            }),
                        }),
                    },
                ),
                Request::PreshowCheck => {
                    let report = self.preshow_report();
                    reply(
                        to,
                        ServerMessage::Result {
                            id,
                            outcome: Outcome::Ok(OkBody {
                                revision: None,
                                data: Some(ResultData::PreshowCheck(Box::new(report))),
                            }),
                        },
                    );
                }
                Request::LogTail { max } => {
                    let (entries, dropped) = self.log_tail(max);
                    reply(
                        to,
                        ServerMessage::Result {
                            id,
                            outcome: Outcome::Ok(OkBody {
                                revision: None,
                                data: Some(ResultData::LogTail { entries, dropped }),
                            }),
                        },
                    );
                }
                Request::Relink { .. } => reply(
                    to,
                    ServerMessage::Result {
                        id,
                        outcome: Outcome::Error(ErrorBody::new(
                            ErrorCode::Unsupported,
                            format!("{} is not available yet", request_name(&request)),
                        )),
                    },
                ),
                request => self.doc_request(id, request, to),
            },
            ClientMessage::Goodbye { shutdown: true } => return Flow::Shutdown,
            // The network layer handles the handshake and closing (M4.3).
            ClientMessage::Hello { .. } | ClientMessage::Goodbye { .. } => {}
        }
        Flow::Continue
    }

    /// Patch positions are mixer matrix slots (PROGRESS M4.2f/M4.15b): inserting before the
    /// end, removing, moving or resizing a patch would re-route or silence a voice on the
    /// device, whatever cue it belongs to (a show cue, a preview, a stopping tail, an orphan)
    /// and whatever patch that cue's model names now. So any such edit, undo or redo
    /// (`Command::changes_patch_layout`, judged on the final state of a transaction) is refused
    /// while any voice is on the device. Lifted by the stable slot allocator.
    fn patch_layout_conflict(
        &self,
        step: Option<&Command>,
        restoring: bool,
    ) -> Option<CommandError> {
        let step = step?;
        if self.voices.on_device().is_empty() || !step.changes_patch_layout(&self.ws) {
            return None;
        }
        // A step that would be refused anyway (a patch a cue uses, a bad value) gets that
        // reason, not this one. The dry run clones the workspace, but only for a patch layout
        // edit while audio plays.
        let mut dry = self.ws.clone();
        let applied = if restoring {
            dry.apply_restoring(step.clone())
        } else {
            dry.apply(step.clone())
        };
        applied.is_ok().then_some(CommandError::PatchPlaying)
    }

    /// Stops voices still sounding for cues that no longer own them: a tail an underrun made
    /// longer than the action's known length, after the engine completed the action on its
    /// planned end (docs/control.md, Underrun). The engine has nothing left to stop there, so a
    /// Panic, a hard stop or a reset would leave it sounding.
    fn silence_orphans(&mut self, scope: Scope, now: SampleTime) {
        let engine = &self.engine;
        let owned = |cue: CueId, run: openlab_core::RunId| {
            engine.run_of(cue) == Some(run)
                && matches!(
                    engine.state(cue),
                    CueState::Running | CueState::Paused | CueState::Fading | CueState::Stopping
                )
        };
        let in_scope = |cue: CueId| match scope {
            Scope::All => true,
            Scope::Cue(root) => {
                root == cue
                    || self
                        .ws
                        .find_cue(root)
                        .is_some_and(|r| visit_any(r.children(), cue))
            }
        };
        let orphans = self
            .voices
            .orphans(|cue, run| in_scope(cue) && !owned(cue, run));
        if orphans.is_empty() {
            return;
        }
        // A Panic fades them out with everything else, ending on the panic's end; otherwise
        // (or past it) they are cut at once.
        let fade = match (scope, self.engine.panic_end()) {
            (Scope::All, Some(end)) if end > now => {
                openlab_audio::Fade::new(end - now, voices::to_curve(openlab_core::PANIC_CURVE))
            }
            _ => openlab_audio::Fade::NONE,
        };
        let floor = self.stop_floor();
        let commands = orphans
            .into_iter()
            .map(|voice| {
                let at = self.voices.stop_date(voice, now, floor);
                (at, CommandKind::StopVoice { voice, fade })
            })
            .collect();
        self.send_commands(commands);
    }

    /// A command, undo or redo changed the workspace.
    fn edited(&mut self, now: SampleTime, footprint: &Footprint) {
        let inputs = self.model_changed(footprint);
        self.document();
        for input in inputs {
            self.feed(input, now);
        }
        // Follow the edit at once: a deleted running cue stops now, not at the next deadline.
        self.feed(Input::Tick, now);
    }

    /// Signals a workspace change to the engine (before its next `handle`, M4.2 contract) and
    /// the clients (one `model_changed` for `footprint`'s cues, M4.1a), and follows it on the
    /// audio side. Returns the inputs that must follow (patch resolutions).
    /// An Arm/Disarm change (`SetArmed`) goes through here too: it is saved with the show, so it
    /// makes the document dirty, but it is not an undo step (QLab confirmation: PROGRESS M2.17).
    fn model_changed(&mut self, footprint: &Footprint) -> Vec<Input> {
        // Lists, patches and settings decide which cues are broken and where the playheads are:
        // the cue-scoped shortcut is only for edits that touched cues alone.
        if footprint.is_model_wide() {
            self.engine.model_changed();
        } else {
            self.engine.model_changed_cues(footprint);
        }
        self.revision += 1;
        // The catalog's cue index follows the edit first; the changes are read through it, at
        // the cost of the edit's cues (M4.12c). The walk is the fallback.
        self.media.sync_cues(&self.ws, footprint);
        let (upserts, deletes) = model_changes_indexed(&self.ws, footprint, self.media.index())
            .unwrap_or_else(|| {
                self.model_change_walks += 1;
                model_changes(&self.ws, footprint)
            });
        self.out.send(ServerMessage::ModelChanged {
            revision: self.revision,
            upserts,
            deletes,
        });
        self.sync_workspace(false)
    }

    /// Brings patches and primes in line with the workspace (the media catalog is followed by
    /// the caller: whole for a newly opened workspace, from the footprint for an edit). `all`
    /// reports every patch resolution (a newly opened workspace), else only changes.
    fn sync_workspace(&mut self, all: bool) -> Vec<Input> {
        self.primes_dirty = true;
        self.sync_patches(all)
    }

    /// Resolves the patches against the playable devices and brings the mixer's matrices,
    /// output delays and master level up to date. Returns the `Availability` for the patches
    /// whose resolution changed (`all`: every patch).
    fn sync_patches(&mut self, all: bool) -> Vec<Input> {
        let (cmds, changed) = self
            .patches
            .sync(&self.ws, &self.config.machine, &self.playable);
        let now = self.now();
        for cmd in cmds {
            self.mixer.send(now, cmd);
        }
        self.warnings_dirty = true;
        let patches = if all {
            self.patches.resolutions()
        } else {
            changed
        };
        if patches.is_empty() {
            Vec::new()
        } else {
            vec![Input::Availability {
                media: Vec::new(),
                patches,
            }]
        }
    }

    /// While the device is down, voice stops are dated on the first sample the mixer renders
    /// when it is back (sample time stands still meanwhile, and `now` is one block past it):
    /// a stopped voice must not play one more block after the outage.
    fn stop_floor(&self) -> Option<SampleTime> {
        self.backend
            .is_down()
            .then(|| self.mixer.samples_rendered())
    }

    fn poll_device(&mut self) {
        let events = match &mut self.backend {
            Backend::Device(d) => d.poll(),
            Backend::Offline => return,
        };
        self.device_events(events);
    }

    fn device_answer(&mut self, answer: DeviceAnswer) {
        let events = match &mut self.backend {
            Backend::Device(d) => d.answer(answer),
            Backend::Offline => return,
        };
        self.device_events(events);
    }

    /// The device policy (docs/control.md "Devices", PROGRESS M4.2g).
    fn device_events(&mut self, events: Vec<DeviceEvent>) {
        let name = self
            .backend
            .device()
            .map(|d| d.info().name.clone())
            .unwrap_or_default();
        for event in events {
            match event {
                DeviceEvent::Lost { why } => self.device_lost(&name, &why),
                DeviceEvent::Restored => self.device_restored(&name),
                // Patches resolve against the device the mixer plays on, so a change in the
                // list changes them only through a loss or a reopen; resolving again is cheap.
                DeviceEvent::ListChanged => {
                    let now = self.now();
                    for input in self.sync_patches(false) {
                        self.feed(input, now);
                    }
                }
                DeviceEvent::OpenFailed {
                    attempt,
                    error,
                    retry_in,
                } => self.log_device(
                    LogLevel::Warn,
                    format!(
                        "audio device \"{name}\": reopen attempt {attempt} failed ({error}); \
                         next in {:.2} s",
                        retry_in.as_secs_f64()
                    ),
                ),
                DeviceEvent::Warn(message) => self.log_device(LogLevel::Warn, message),
                DeviceEvent::Stalled { ms, log } => {
                    if let Some(earlier) = log {
                        let more = if earlier > 0 {
                            format!(" ({earlier} more stall(s) since the last notice)")
                        } else {
                            String::new()
                        };
                        self.log_device(
                            LogLevel::Warn,
                            format!(
                                "audio device \"{name}\": no audio callback for {ms} ms; the \
                                 device is treated as lost if it stays silent{more}"
                            ),
                        );
                    }
                    self.warnings_dirty = true;
                }
                DeviceEvent::StallCleared { log } => {
                    if log {
                        self.log_device(
                            LogLevel::Info,
                            format!("audio device \"{name}\": audio callbacks resumed"),
                        );
                    }
                    self.warnings_dirty = true;
                }
                DeviceEvent::Unrecoverable { why } => {
                    self.log_device(
                        LogLevel::Error,
                        format!("audio device \"{name}\": {why}: restart openLab to play sound"),
                    );
                    self.warnings_dirty = true;
                }
                DeviceEvent::Unstuck => {
                    self.log_device(
                        LogLevel::Info,
                        format!("audio device \"{name}\": the device thread answers again"),
                    );
                    self.warnings_dirty = true;
                }
                DeviceEvent::ListStuck => {
                    self.log_device(
                        LogLevel::Error,
                        "reading the output device list does not finish: hot-plug is not \
                         followed until it does"
                            .to_owned(),
                    );
                    self.warnings_dirty = true;
                }
            }
        }
    }

    /// The device went away (M4.2g, as QLab 5): the patches on it are reported missing (loaded
    /// cues on them unload, starts are refused as broken, idle cues show broken), every run
    /// with a voice on it ends at once with `ActionFailed { device_lost }` (hard stop, no
    /// fade; the voice and its stream are released, auto-follow never fires), and every voice
    /// the mixer holds is stopped, so nothing sounds, or resumes, when the device returns.
    fn device_lost(&mut self, name: &str, why: &str) {
        let now = self.now();
        self.playable.clear();
        for input in self.sync_patches(false) {
            self.feed(input, now);
        }
        let mut runs = self.voices.on_device();
        // Audio runs with no voice yet or any more (pre-wait, post-wait, paused) are routed to
        // the device too: nothing scheduled before the outage may start by itself after it.
        // They end as device_lost breaks; `Input::DeviceLost` below stops all the others.
        for (cue, state) in self.engine.cue_states() {
            if matches!(
                state,
                CueState::PreWaiting
                    | CueState::Running
                    | CueState::Paused
                    | CueState::PostWaiting
                    | CueState::Fading
                    | CueState::Stopping
            ) && let Some(run) = self.engine.run_of(cue)
                && matches!(
                    self.ws.find_cue(cue).map(|c| &c.kind),
                    Some(CueKind::Audio { .. })
                )
                && !runs.contains(&(cue, run))
            {
                runs.push((cue, run));
            }
        }
        runs.sort();
        let failed = runs.len();
        for (cue, run) in runs {
            self.feed(
                Input::ActionFailed {
                    cue,
                    run,
                    reason: BrokenReason::DeviceLost,
                },
                now,
            );
        }
        // Nothing else scheduled before the loss may fire after the return: every other active
        // run (memo, Wait, control, Fade, group; broken runs with a pending continue) stops.
        self.feed(Input::DeviceLost, now);
        // Orphaned tails and anything the engine no longer owns. Dated on the first sample the
        // mixer renders after the outage (`stop_floor`), so a voice plays nothing more.
        let floor = self.stop_floor();
        let commands = self
            .voices
            .playing_ids()
            .into_iter()
            .map(|voice| {
                let at = self.voices.stop_date(voice, now, floor);
                let fade = openlab_audio::Fade::NONE;
                (at, CommandKind::StopVoice { voice, fade })
            })
            .collect();
        self.send_commands(commands);
        let message = format!(
            "audio device \"{name}\" lost ({why}): {failed} cue(s) playing on it stopped; \
             they start again from the beginning with GO once the device is back"
        );
        self.log_device(LogLevel::Error, message.clone());
        self.runtime(vec![RuntimeChange::Warning {
            warning: notice("device_lost", message, None),
        }]);
        self.warnings_dirty = true;
    }

    /// The stream was reopened: patches resolve against the device again (cues that
    /// failed with `device_lost`, and idle cues on it, become playable).
    fn device_restored(&mut self, name: &str) {
        let Some(device) = self.backend.device() else {
            return;
        };
        let block = device.info().block_frames;
        self.playable = vec![playable_device(device)];
        // Another buffer size: the lookahead and `now` follow it, so every deadline is still
        // committed before the callback that renders it (commands already sent keep their
        // samples).
        if let Some(b) = block.map(u64::from).filter(|&b| b > 0 && b != self.block) {
            match self.engine.set_lookahead(b) {
                Ok(()) => {
                    self.now_floor = self.now();
                    self.log_device(
                        LogLevel::Info,
                        format!(
                            "audio device \"{name}\" reopened with {b}-frame buffers (was {})",
                            self.block
                        ),
                    );
                    self.block = b;
                }
                Err(e) => self.log_device(
                    LogLevel::Warn,
                    format!("audio device \"{name}\": {e}; commands may land late"),
                ),
            }
        }
        let now = self.now();
        for input in self.sync_patches(false) {
            self.feed(input, now);
        }
        self.log_device(LogLevel::Info, format!("audio device \"{name}\" is back"));
        self.warnings_dirty = true;
    }

    /// The output devices for a `list_devices` request.
    fn device_list(&self) -> Vec<DeviceEntry> {
        match self.backend.device() {
            Some(d) => d
                .listed()
                .iter()
                .map(|l| DeviceEntry {
                    name: l.name.clone(),
                    output_channels: u32::from(l.max_channels()),
                })
                .collect(),
            None => self
                .config
                .devices
                .iter()
                .map(|d| DeviceEntry {
                    name: d.name.clone(),
                    output_channels: d.output_channels,
                })
                .collect(),
        }
    }

    fn probe_answer(&mut self, answer: ProbeAnswer) {
        let key = match &answer {
            ProbeAnswer::Probed { key, .. } => Some(key.clone()),
            ProbeAnswer::Preloaded { .. } => None,
        };
        if let ProbeAnswer::Preloaded { key, .. } = &answer {
            // A preload that lands changes what a stream of the file reads: primes built from
            // the file before it are rebuilt from memory, before the operator's GO needs them.
            let was = self.media.is_preloaded(key);
            let key = key.clone();
            self.media.answer(answer);
            if self.media.is_preloaded(&key) != was {
                self.primes_dirty = true;
            }
            return;
        }
        if let Some(status) = self.media.answer(answer) {
            self.pending_media.push(status);
            if let Some(key) = key {
                self.ready_media.push(key);
            }
            self.warnings_dirty = true;
            self.primes_dirty = true;
        }
    }

    /// Hands the probe answers gathered since the last tick to the engine in one
    /// `Availability`, then starts the voices that waited for them.
    fn flush_media(&mut self, now: SampleTime) {
        if !self.pending_media.is_empty() {
            let media = std::mem::take(&mut self.pending_media);
            self.feed(
                Input::Availability {
                    media,
                    patches: Vec::new(),
                },
                now,
            );
        }
        for key in std::mem::take(&mut self.ready_media) {
            let mut fx = Effects::default();
            let floor = self.stop_floor();
            let mut env = Env {
                ws: &self.ws,
                media: &self.media,
                streamer: &mut self.streamer,
                patches: &self.patches,
                rate: self.rate,
                cue_outputs: self.cue_outputs,
                now,
                floor,
            };
            self.voices.media_ready(&key, &mut env, &mut fx);
            self.follow(fx, now);
        }
    }

    fn deliver_deferred(&mut self, now: SampleTime) {
        if self.deferred.is_empty() {
            return;
        }
        let (due, wait): (Vec<_>, Vec<_>) = std::mem::take(&mut self.deferred)
            .into_iter()
            .partition(|(at, _)| *at < now);
        self.deferred = wait;
        for (_, input) in due {
            self.feed(input, now);
        }
    }

    /// Hands `input` to the engine and carries out what it says, then the feedback that causes.
    fn feed(&mut self, input: Input, now: SampleTime) {
        let mut queue = VecDeque::from([input]);
        let mut rounds = 0;
        while let Some(input) = queue.pop_front() {
            rounds += 1;
            if rounds > FEEDBACK_LIMIT {
                self.seen.feedback_overflow += 1;
                self.log(
                    LogLevel::Error,
                    format!("feedback loop cut after {FEEDBACK_LIMIT} rounds"),
                );
                break;
            }
            let actions = self.engine.handle(&self.ws, input, now);
            let unsignalled = self.engine.unsignalled_model_changes();
            if unsignalled > self.seen.unsignalled {
                self.seen.unsignalled = unsignalled;
                self.log(
                    LogLevel::Error,
                    format!("cue engine saw {unsignalled} model change(s) nobody signalled"),
                );
            }
            queue.extend(self.execute(&actions, now));
        }
    }

    /// Carries out one `handle` call's actions: runtime changes to the clients, `SetArmed` to the
    /// model, voice actions to the mixer and streams. Returns feedback to hand back at once.
    fn execute(&mut self, actions: &[Action], now: SampleTime) -> Vec<Input> {
        let mut changes = Vec::new();
        let mut armed = Vec::new();
        let mut fx = Effects::default();
        {
            let floor = self.stop_floor();
            let mut env = Env {
                ws: &self.ws,
                media: &self.media,
                streamer: &mut self.streamer,
                patches: &self.patches,
                rate: self.rate,
                cue_outputs: self.cue_outputs,
                now,
                floor,
            };
            for action in actions {
                if let Some(change) = RuntimeChange::from_action(action, &self.engine) {
                    changes.push(change);
                }
                if matches!(action, Action::PlayheadChanged { .. }) {
                    self.primes_dirty = true;
                }
                self.voices.apply(action, &mut env, &mut fx);
            }
        }
        for action in actions {
            if let Action::SetArmed { cue, armed: value } = action
                && set_armed(&mut self.ws, *cue, *value)
            {
                armed.push(*cue);
            }
        }
        if !changes.is_empty() {
            self.observe_changes(&changes);
            self.runtime(changes);
        }
        let mut feedback = self.after(fx);
        if !armed.is_empty() {
            feedback.extend(self.model_changed(&Footprint::edited(armed)));
            self.document();
        }
        feedback
    }

    /// Sends the mixer commands and notices of `fx`; queues its deferred feedback; returns its
    /// immediate feedback (which `feed` hands to the engine).
    fn after(&mut self, fx: Effects) -> Vec<Input> {
        let Effects {
            commands,
            feedback,
            deferred,
            notices,
            reprobe,
        } = fx;
        self.send_commands(commands);
        self.deferred.extend(deferred);
        for key in reprobe {
            self.media.reprobe(&key);
        }
        if !notices.is_empty() {
            let changes = notices
                .into_iter()
                .map(|(code, message, cue)| RuntimeChange::Warning {
                    warning: WireWarning::Notice {
                        code: code.to_owned(),
                        message,
                        cue,
                    },
                })
                .collect::<Vec<_>>();
            self.observe_changes(&changes);
            self.runtime(changes);
        }
        feedback
    }

    /// [`Self::after`], then hands the feedback to the engine (outside a `feed`).
    fn follow(&mut self, fx: Effects, now: SampleTime) {
        for input in self.after(fx) {
            self.feed(input, now);
        }
    }

    /// Sends commands as batches the mixer applies whole (docs/audio.md, Scheduling horizon).
    fn send_commands(&mut self, mut commands: Vec<(SampleTime, CommandKind)>) {
        while !commands.is_empty() {
            let rest = commands.split_off(commands.len().min(COMMAND_CAPACITY));
            if self.mixer.send_batch(commands).is_err() {
                // Cannot happen: the batch is at most the queue's size.
                self.log(LogLevel::Error, "mixer batch refused".to_owned());
            }
            commands = rest;
        }
    }

    fn runtime(&mut self, changes: Vec<RuntimeChange>) {
        self.runtime_seq += 1;
        self.out.send(ServerMessage::Runtime {
            seq: self.runtime_seq,
            changes,
        });
    }

    fn document(&mut self) {
        let document = self.document_state();
        self.resend_document = !self.out.send_state(ServerMessage::Document { document });
    }

    fn document_state(&self) -> DocumentState {
        DocumentState {
            path: self
                .config
                .show_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            dirty: self.revision != self.saved_revision,
            can_undo: self.history.can_undo(),
            can_redo: self.history.can_redo(),
        }
    }

    fn log_device(&mut self, level: LogLevel, message: String) {
        self.log_kind(LogKind::Device, level, message);
    }

    /// A message for the show log and every client's `log`.
    fn log(&mut self, level: LogLevel, message: String) {
        self.log_kind(LogKind::Engine, level, message);
    }

    /// Keeps the cues near each playhead primed (ARCHITECTURE §3.5).
    fn prime(&mut self, now: SampleTime) {
        let wanted = prime_set(&self.ws, &self.engine, self.config.prime_cues);
        // Their files are probed first (H2: a restart mid-show must not leave the cue at the
        // playhead behind a hundred others).
        let keys: Vec<String> = wanted
            .iter()
            .filter_map(|c| match &self.ws.find_cue(*c)?.kind {
                CueKind::Audio { media: Some(m), .. } => Some(m.as_str().to_owned()),
                _ => None,
            })
            .collect();
        self.media.prioritize(keys.iter().map(String::as_str));
        let floor = self.stop_floor();
        let mut env = Env {
            ws: &self.ws,
            media: &self.media,
            streamer: &mut self.streamer,
            patches: &self.patches,
            rate: self.rate,
            cue_outputs: self.cue_outputs,
            now,
            floor,
        };
        self.voices.prime(&wanted, self.config.max_primes, &mut env);
    }

    /// Progress and meters, each at most [`REPORT_HZ`] in sample time; reconciliation of lost
    /// voice ends rides on the voice-meter read.
    fn reports(&mut self) {
        let rendered = self.mixer.samples_rendered();
        let period = (u64::from(self.rate.get()) / REPORT_HZ).max(1);
        let due = |last: Option<SampleTime>| last.is_none_or(|l| rendered >= l + period);
        if due(self.last_progress) {
            let items: Vec<ProgressItem> = self
                .voices
                .progress(rendered)
                .into_iter()
                .map(|(cue, pos, left)| ProgressItem {
                    cue,
                    elapsed: self.seconds(pos),
                    remaining: left.map(|l| self.seconds(l)),
                })
                .collect();
            if !items.is_empty() {
                self.last_progress = Some(rendered);
                let mut m = ServerMessage::Progress { items };
                m.sanitise();
                self.out.send_lossy(m);
            }
        }
        if due(self.last_meters) {
            self.last_meters = Some(rendered);
            self.meters(rendered);
        }
    }

    fn seconds(&self, frames: u64) -> f64 {
        frames as f64 / f64::from(self.rate.get())
    }

    fn meters(&mut self, rendered: SampleTime) {
        self.mixer.read_output_meters_into(&mut self.out_peaks);
        let n = self
            .mixer
            .read_voice_meters_into(&mut self.meter_ids, &mut self.meter_peaks);
        let gone = self.voices.take_gone();
        let outs = self.cue_outputs.max(1);
        let mut cues: Vec<CueMeter> = Vec::new();
        let mut listed = Vec::with_capacity(n);
        for (k, id) in self.meter_ids.iter().take(n).enumerate() {
            listed.push(*id);
            let cue = self
                .voices
                .cue_of(*id)
                .or_else(|| gone.iter().find(|(g, _)| g == id).map(|(_, c)| *c));
            let Some(cue) = cue else { continue };
            let peaks = self
                .meter_peaks
                .get(k * outs..(k + 1) * outs)
                .unwrap_or(&[]);
            match cues.iter_mut().find(|m| m.cue == cue) {
                Some(m) => {
                    for (a, b) in m.peaks.iter_mut().zip(peaks) {
                        *a = a.max(*b);
                    }
                }
                None => cues.push(CueMeter {
                    cue,
                    peaks: peaks.to_vec(),
                }),
            }
        }
        self.reconcile(&listed, rendered);
        let outputs: Vec<OutputMeter> = self
            .ws
            .patches
            .iter()
            .filter_map(|p| {
                let channels = self.patches.channels(p.id)?;
                let peaks = channels
                    .iter()
                    .map(|&c| {
                        usize::try_from(c)
                            .ok()
                            .and_then(|c| self.out_peaks.get(c))
                            .copied()
                            .unwrap_or(0.0)
                    })
                    .collect();
                Some(OutputMeter { patch: p.id, peaks })
            })
            .collect();
        let live = self.voices.has_playing() || !cues.is_empty();
        // Only while audio plays, plus one message after, so meters fall back to silence.
        if live || self.meters_live {
            self.meters_live = live;
            let mut m = ServerMessage::Meters { outputs, cues };
            m.sanitise();
            self.out.send_lossy(m);
        }
    }

    /// A started voice the mixer has not listed in two meter reads in a row, while mixer
    /// events have been lost, has ended without its report reaching us: report it ended so its
    /// cue does not run forever (docs/control.md, Reconciliation).
    fn reconcile(&mut self, listed: &[VoiceId], rendered: SampleTime) {
        let Some(prev) = self.last_meter_read.replace(rendered) else {
            return;
        };
        let lost_events = self.mixer.dropped_events() > 0;
        let started: Vec<(VoiceId, SampleTime)> = self.voices.started().collect();
        let mut fx = Effects::default();
        for (id, start) in started {
            if start >= prev {
                continue;
            }
            let absent = self.voices.mark_absent(id, !listed.contains(&id));
            if absent >= 2 && lost_events {
                self.voices.lost(id, rendered, &mut fx);
            }
        }
        let now = self.now();
        self.follow(fx, now);
    }

    /// Reports counters that must never rise: lost events, mixer faults, allocations on the
    /// audio thread (M3.21), dropped outbound messages.
    fn health(&mut self) {
        let dropped = self.mixer.dropped_events();
        if dropped > self.seen.dropped_events {
            self.seen.dropped_events = dropped;
            self.log(
                LogLevel::Warn,
                format!("{dropped} mixer event(s) lost; voice ends are reconciled from meters"),
            );
        }
        let faults = self.mixer.faults();
        if faults > self.seen.faults {
            self.seen.faults = faults;
            self.log(
                LogLevel::Error,
                format!("audio engine fault: {faults} block(s) replaced with silence"),
            );
        }
        let allocs = self.backend.alloc_violations();
        if allocs > self.seen.alloc_violations {
            self.seen.alloc_violations = allocs;
            self.log(
                LogLevel::Error,
                format!("real-time violation: {allocs} allocation(s) on the audio thread"),
            );
            self.warnings_dirty = true;
        }
        // State the clients missed goes again, newest, once there is room.
        if self.resend_document && self.out.has_room() {
            self.document();
        }
        if self.resend_warnings && self.out.has_room() {
            self.resend_warnings = !self.out.send_state(ServerMessage::Warnings {
                warnings: self.warnings.clone(),
            });
        }
        let out = self.out.dropped() + self.out.lossy_dropped();
        if out > self.seen.outbound_dropped && self.out.has_room() {
            let (seq, lossy) = (self.out.dropped(), self.out.lossy_dropped());
            self.seen.outbound_dropped = out;
            self.log(
                LogLevel::Warn,
                format!(
                    "clients were not keeping up: {seq} message(s) and {lossy} meter/progress \
                     update(s) dropped so far"
                ),
            );
        }
    }

    /// The workspace-level notices in force: damaged media, preloads over budget, cues whose
    /// slices cannot play, audio-thread allocations. Sent when the list changes.
    fn refresh_warnings(&mut self) {
        let mut list = self.docs.notices.clone();
        let mut damaged: Vec<(&str, usize)> = self.media.damaged().collect();
        damaged.sort_unstable();
        for (path, n) in damaged {
            list.push(notice(
                "damaged_media",
                format!("{path}: {n} damaged part(s); they play as silence"),
                None,
            ));
        }
        let mut over: Vec<&str> = self.media.over_budget().collect();
        over.sort_unstable();
        for path in over {
            list.push(notice(
                "preload_over_budget",
                format!("{path} does not fit the preload memory budget and is streamed"),
                None,
            ));
        }
        let mut failed: Vec<&str> = self.media.preload_failed().collect();
        failed.sort_unstable();
        for path in failed {
            list.push(notice(
                "preload_failed",
                format!("{path} could not be loaded into memory and is streamed"),
                None,
            ));
        }
        for (cue, e) in self.checks.check(&self.ws, &mut self.media, self.rate) {
            list.push(notice(
                "bad_plan",
                format!("the cue's slices cannot be played: {e}"),
                Some(cue),
            ));
        }
        self.device_warnings(&mut list);
        if self.seen.alloc_violations > 0 {
            list.push(notice(
                "realtime_allocation",
                format!(
                    "the audio thread allocated memory {} time(s): report this bug",
                    self.seen.alloc_violations
                ),
                None,
            ));
        }
        if list != self.warnings {
            self.warnings = list.clone();
            self.resend_warnings = !self
                .out
                .send_state(ServerMessage::Warnings { warnings: list });
        }
        // The plans just checked also give the cues' nominal lengths, which a playlist halted by
        // a broken child times its auto-follow by (docs/cue-engine.md, "Playlist").
        let lengths = self.checks.length_changes();
        if !lengths.is_empty() {
            let now = self.now();
            self.feed(Input::CueLengths { lengths }, now);
        }
    }

    /// The device's notices: an outage, a mixer that could not be recovered, patches that map
    /// more device channels than the device has (`outputs_dropped`), device outputs that
    /// patches give different delays.
    fn device_warnings(&self, list: &mut Vec<WireWarning>) {
        if let Some(d) = self.backend.device() {
            let name = &d.info().name;
            if let Some(why) = d.unrecoverable() {
                list.push(notice(
                    "device_unrecoverable",
                    format!(
                        "audio device \"{name}\": {why}; sound cannot come back: restart \
                         openLab"
                    ),
                    None,
                ));
            } else if d.is_down() {
                list.push(notice(
                    "device_lost",
                    format!(
                        "audio device \"{name}\" is lost: the cues that played on it were \
                         stopped, and cues on it cannot start; reconnecting"
                    ),
                    None,
                ));
            }
            if d.is_stalled() && !d.is_down() {
                list.push(notice(
                    "device_stalled",
                    format!(
                        "audio device \"{name}\" has stopped calling back: it is treated as \
                         lost if it stays silent"
                    ),
                    None,
                ));
            }
            if d.list_stuck() {
                list.push(notice(
                    "device_list_stuck",
                    "reading the output device list does not finish: devices plugged in or out \
                     are not seen"
                        .to_owned(),
                    None,
                ));
            }
            if d.info().outputs_dropped && !d.is_down() {
                list.push(notice(
                    "outputs_dropped",
                    format!(
                        "audio device \"{name}\" has {} output channel(s), fewer than the {} \
                         the show was opened with: the others are not heard",
                        d.info().channels,
                        self.cue_outputs
                    ),
                    None,
                ));
            }
        }
        for patch in &self.ws.patches {
            if let Some(PatchResolution::ChannelsMissing {
                device,
                needed,
                available,
            }) = self.patches.resolution(patch.id)
            {
                list.push(notice(
                    "outputs_dropped",
                    format!(
                        "patch \"{}\" maps {needed} channel(s) of \"{device}\", which has \
                         {available}: its cues cannot play",
                        patch.name
                    ),
                    None,
                ));
            }
        }
        for output in self.patches.delay_conflicts() {
            list.push(notice(
                "output_delay_conflict",
                format!(
                    "device output {} has different delays in different patches; the longest \
                     is used",
                    u32::from(output) + 1
                ),
                None,
            ));
        }
    }

    /// Everything a client needs to draw the show (docs/protocol.md, Snapshot).
    pub fn snapshot(&self) -> Snapshot {
        let cue_states = self
            .engine
            .cue_states()
            .map(|(cue, state)| CueStateEntry {
                cue,
                state: state.into(),
                reason: match state {
                    CueState::Broken => self.engine.broken_reason(cue).map(Into::into),
                    _ => None,
                },
            })
            .collect();
        let problems = self
            .engine
            .broken_cues()
            .into_iter()
            .map(|(cue, problems)| CueProblems {
                cue,
                problems: problems.iter().map(WireProblem::from).collect(),
            })
            .collect();
        let mut group_warnings = Vec::new();
        for list in &self.ws.cue_lists {
            groups(&list.cues, &mut |c| {
                if self.engine.has_broken_descendant(c) {
                    group_warnings.push(c);
                }
            });
        }
        Snapshot {
            revision: self.revision,
            runtime_seq: self.runtime_seq,
            document: self.document_state(),
            workspace: self.ws.clone(),
            cue_states,
            problems,
            group_warnings,
            playheads: self
                .ws
                .cue_lists
                .iter()
                .map(|l| PlayheadEntry {
                    list: l.id,
                    cue: self.engine.playhead(l.id),
                })
                .collect(),
            previewing: self.engine.previewing().collect(),
            warnings: self.warnings.clone(),
        }
    }

    /// Stops the show for shutdown: a hard stop of everything, carried out by the mixer.
    pub fn shutdown(&mut self) {
        let now = self.now();
        // A second panic within the fade is a hard stop (docs/cue-engine.md, Panic).
        self.feed(Input::Panic, now);
        self.feed(Input::Panic, now);
        self.voices.clear_primes();
        self.mixer.pump();
    }

    /// Slice plans built for the `bad_plan` warnings so far (a cost measure: an edit rebuilds
    /// only the cues it changed).
    pub fn plan_builds(&self) -> u64 {
        self.checks.builds()
    }

    /// Cues the `bad_plan` checks have looked at so far (a cost measure: an edit looks at the
    /// cues it touched, M4.12c).
    pub fn plan_cues_examined(&self) -> u64 {
        self.checks.examined()
    }

    /// Walks of every audio cue the `bad_plan` checks made so far (a newly followed workspace;
    /// an edit or a probe answer makes none, M4.12c).
    pub fn plan_walks(&self) -> u64 {
        self.checks.walks()
    }

    /// Edits whose `model_changed` walked the workspace instead of reading the cue index (a
    /// cost measure; none for a valid workspace, M4.12c).
    pub fn model_change_walks(&self) -> u64 {
        self.model_change_walks
    }

    pub fn workspace(&self) -> &Workspace {
        &self.ws
    }

    pub fn engine(&self) -> &CueEngine {
        &self.engine
    }

    pub fn mixer(&self) -> &MixerController {
        &self.mixer
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Voice records alive (none once a show has stopped and every voice has ended).
    pub fn live_voices(&self) -> usize {
        self.voices.live()
    }

    /// Stream handles the loop holds (voices, loads and primes).
    pub fn held_streams(&self) -> usize {
        self.voices.streams()
    }

    /// Streams the disk workers have open.
    pub fn open_streams(&self) -> usize {
        self.streamer.open_streams()
    }

    /// Starts that used a primed stream (not a cold start).
    pub fn prime_hits(&self) -> u64 {
        self.voices.prime_hits()
    }

    /// Whether the cue's primed stream reads preloaded samples.
    pub fn primed_from_memory(&self, cue: CueId) -> bool {
        self.voices.primed_from_memory(cue)
    }

    /// Cues with a primed stream.
    pub fn primed(&self) -> Vec<CueId> {
        let mut v: Vec<CueId> = self.voices.primed().collect();
        v.sort();
        v
    }

    /// Probes still running.
    pub fn probes_pending(&self) -> usize {
        self.media.pending()
    }

    /// Preload jobs started and not answered yet.
    pub fn preloads_pending(&self) -> usize {
        self.media.preloads_pending()
    }

    /// Probe workers replaced because they were stuck.
    pub fn abandoned_probe_workers(&self) -> usize {
        self.media.abandoned_workers()
    }

    /// Cues the media catalog has read from the workspace (see `MediaCatalog::cues_read`).
    pub fn media_cues_read(&self) -> u64 {
        self.media.cues_read()
    }

    /// Memory taken from the preload budget.
    pub fn preload_reserved(&self) -> u64 {
        self.media.preload_reserved()
    }

    /// Device-thread jobs not answered yet (a test driver waits for them; none offline).
    pub fn device_jobs_pending(&self) -> usize {
        self.backend.device().map_or(0, Device::jobs_in_flight)
    }

    /// The device is down: an outage, sample time standing still.
    pub fn device_down(&self) -> bool {
        self.backend.is_down()
    }

    /// A device list read is under way (a test driver waits for it).
    pub fn device_listing(&self) -> bool {
        self.backend.device().is_some_and(Device::is_listing)
    }

    /// Frames per block the loop schedules with (follows the open stream).
    pub fn block_frames(&self) -> u64 {
        self.block
    }

    /// Feedback waiting for its sample.
    pub fn deferred(&self) -> usize {
        self.deferred.len()
    }

    /// Voices the mixer is playing or holding paused, from a voice-meter read (diagnostics and
    /// leak tests; the read takes the peaks the next meters message would have had).
    pub fn mixer_voice_count(&mut self) -> usize {
        self.mixer
            .read_voice_meters_into(&mut self.meter_ids, &mut self.meter_peaks)
    }

    /// Playing streams and the frames each must have written before rendered sample `t` can
    /// be rendered without an underrun (offline driver).
    pub fn stream_needs(&self, t: SampleTime) -> Vec<(&openlab_audio::StreamHandle, u64)> {
        self.voices.stream_needs(t).collect()
    }

    /// Streams prepared and not playing yet: loads and primes (offline driver).
    pub fn prepared_streams(&self) -> Vec<&openlab_audio::StreamHandle> {
        self.voices.waiting_streams().collect()
    }

    /// No voice, alias or deferred report is left.
    pub fn is_idle(&self) -> bool {
        self.voices.is_idle() && self.deferred.is_empty()
    }
}

/// The device patches resolve against while `d` plays: its name, with the mixer outputs that
/// reach it.
fn playable_device(d: &Device) -> DeviceInfo {
    DeviceInfo {
        name: d.info().name.clone(),
        output_channels: u32::from(d.info().channels),
    }
}

/// The shortest wait after a tick that saw no progress: [`MIN_STALL_WAIT`], or a quarter of a
/// device block when that is shorter.
pub fn stall_floor(block: u64, rate: u32) -> Duration {
    let quarter = Duration::from_secs_f64(block as f64 / f64::from(rate.max(1)) / 4.0);
    MIN_STALL_WAIT.min(quarter)
}

/// Frames from `rendered` until the loop must tick: when the engine's next deadline enters the
/// lookahead window (`is_due(rendered + block)`: `deadline < rendered + 2 * block`), or a
/// deferred report becomes deliverable (`at < rendered + block`). 0 = now.
pub fn wake_frames(
    deadline: Option<SampleTime>,
    deferred: impl Iterator<Item = SampleTime>,
    rendered: SampleTime,
    block: SampleTime,
) -> u64 {
    let mut frames = u64::MAX;
    if let Some(d) = deadline {
        frames = frames.min(
            d.saturating_add(1)
                .saturating_sub(block.saturating_mul(2))
                .saturating_sub(rendered),
        );
    }
    for at in deferred {
        frames = frames.min(
            at.saturating_add(1)
                .saturating_sub(block)
                .saturating_sub(rendered),
        );
    }
    frames
}

fn ok(id: openlab_core::protocol::RequestId, revision: Option<u64>) -> ServerMessage {
    ServerMessage::Result {
        id,
        outcome: Outcome::Ok(OkBody {
            revision,
            data: None,
        }),
    }
}

fn refused(id: openlab_core::protocol::RequestId, e: &CommandError) -> ServerMessage {
    ServerMessage::Result {
        id,
        outcome: Outcome::Error(ErrorBody::from(e)),
    }
}

fn notice(code: &str, message: String, cue: Option<CueId>) -> WireWarning {
    WireWarning::Notice {
        code: code.to_owned(),
        message,
        cue,
    }
}

fn request_name(r: &Request) -> &'static str {
    match r {
        Request::New { .. } => "new",
        Request::Open { .. } => "open",
        Request::OpenRecovery { .. } => "open_recovery",
        Request::ScanRecovery => "scan_recovery",
        Request::Save => "save",
        Request::SaveAs { .. } => "save_as",
        Request::Revert { .. } => "revert",
        Request::Relink { .. } => "relink",
        Request::ListDevices => "list_devices",
        Request::PreshowCheck => "preshow_check",
        Request::LogTail { .. } => "log_tail",
        Request::Snapshot => "snapshot",
    }
}

/// The cue-engine input for a transport action (docs/protocol.md, Transport actions).
pub fn transport_input(action: TransportAction) -> Input {
    match action {
        TransportAction::Go { list } => Input::Go { list },
        TransportAction::Panic => Input::Panic,
        TransportAction::GoFrom { list, cue } => Input::GoFrom { list, cue },
        TransportAction::StopAll | TransportAction::HardStopAll => Input::StopAll,
        TransportAction::PanicCue { cue } => Input::PanicCue { cue },
        TransportAction::PauseAll => Input::PauseAll,
        TransportAction::ResumeAll => Input::ResumeAll,
        TransportAction::SetPlayhead { list, cue } => Input::SetPlayhead { list, cue },
        TransportAction::SetPlayheadByNumber { list, number } => {
            Input::SetPlayheadByNumber { list, number }
        }
        TransportAction::PlayheadNext { list } => Input::PlayheadNext { list },
        TransportAction::PlayheadPrevious { list } => Input::PlayheadPrevious { list },
        TransportAction::Start { cue } => Input::Start { cue },
        TransportAction::Stop { cue, fade_out } => Input::Stop { cue, fade_out },
        TransportAction::HardStop { cue } => Input::Stop {
            cue,
            fade_out: None,
        },
        TransportAction::Pause { cue } => Input::Pause { cue },
        TransportAction::Resume { cue } => Input::Resume { cue },
        TransportAction::Load { cue, at } => Input::Load { cue, time: at },
        TransportAction::Reset { cue } => Input::Reset { cue },
        TransportAction::TriggerReleased { cue } => Input::TriggerReleased { cue },
        TransportAction::Preview { cue } => Input::Preview { cue },
        TransportAction::StopPreview { cue } => Input::StopPreview { cue },
    }
}

/// Which voices a transport action must silence besides what the engine stops.
#[derive(Clone, Copy)]
enum Scope {
    All,
    Cue(CueId),
}

fn orphan_scope(action: &TransportAction) -> Option<Scope> {
    match action {
        TransportAction::Panic | TransportAction::StopAll | TransportAction::HardStopAll => {
            Some(Scope::All)
        }
        TransportAction::HardStop { cue }
        | TransportAction::Reset { cue }
        | TransportAction::Stop {
            cue,
            fade_out: None,
        } => Some(Scope::Cue(*cue)),
        _ => None,
    }
}

fn visit_any(cues: &[Cue], id: CueId) -> bool {
    cues.iter()
        .any(|c| c.id == id || visit_any(c.children(), id))
}

/// Applies an Arm/Disarm cue's change to the model, as a runtime change (no undo step).
fn set_armed(ws: &mut Workspace, cue: CueId, armed: bool) -> bool {
    fn find(cues: &mut [Cue], id: CueId) -> Option<&mut Cue> {
        for c in cues {
            if c.id == id {
                return Some(c);
            }
            if let CueKind::Group { children, .. } = &mut c.kind
                && let Some(found) = find(children, id)
            {
                return Some(found);
            }
        }
        None
    }
    for list in &mut ws.cue_lists {
        if let Some(c) = find(&mut list.cues, cue) {
            let changed = c.armed != armed;
            c.armed = armed;
            return changed;
        }
    }
    false
}

fn groups(cues: &[Cue], f: &mut impl FnMut(CueId)) {
    for c in cues {
        if let CueKind::Group { children, .. } = &c.kind {
            f(c.id);
            groups(children, f);
        }
    }
}

fn visit<'a>(cues: &'a [Cue], f: &mut impl FnMut(&'a Cue)) {
    for c in cues {
        f(c);
        visit(c.children(), f);
    }
}

/// The cues to keep primed: from each list's playhead the next `per_list` audio cues, the
/// targets of those cues, then every cue with a hotkey or OSC trigger (ARCHITECTURE §3.5).
/// Disarmed and running cues are left out (a start of a running cue makes a new voice anyway).
fn prime_set(ws: &Workspace, engine: &CueEngine, per_list: usize) -> Vec<CueId> {
    let mut wanted: Vec<CueId> = Vec::new();
    let add = |id: CueId, wanted: &mut Vec<CueId>| {
        let ok = ws
            .find_cue(id)
            .is_some_and(|c| c.armed && matches!(c.kind, CueKind::Audio { media: Some(_), .. }))
            && engine.run_of(id).is_none();
        if ok && !wanted.contains(&id) {
            wanted.push(id);
        }
    };
    let mut targets = Vec::new();
    for list in &ws.cue_lists {
        let Some(head) = engine.playhead(list.id) else {
            continue;
        };
        let positions = CueEngine::playhead_positions(ws, list.id);
        let from = positions.iter().position(|&c| c == head).unwrap_or(0);
        for &id in positions.iter().skip(from).take(per_list) {
            add(id, &mut wanted);
            if let Some(t) = ws.find_cue(id).and_then(|c| c.kind.target()) {
                targets.push(t);
            }
        }
    }
    for t in targets {
        if let Some(c) = ws.find_cue(t) {
            // A group target primes its audio children.
            visit(std::slice::from_ref(c), &mut |c| add(c.id, &mut wanted));
        }
    }
    for list in &ws.cue_lists {
        visit(&list.cues, &mut |c| {
            if c.triggers.hotkey.is_some() || c.triggers.osc.is_some() {
                add(c.id, &mut wanted);
            }
        });
    }
    wanted
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `wake_frames` agrees with the engine's own wake rule (`CueEngine::is_due`, lookahead =
    /// block, `now = rendered + block`) for every rendered position around a deadline: 0
    /// exactly when a tick there is due, otherwise the distance to the first position where it is.
    #[test]
    fn wake_frames_agrees_with_is_due() {
        for block in [1u64, 3, 64] {
            for d in (block..300).step_by(7) {
                let engine = engine_with_deadline(block, d);
                let due = |rendered: u64| engine.is_due(rendered + block);
                for rendered in 0..300u64 {
                    let w =
                        wake_frames(engine.next_deadline(), std::iter::empty(), rendered, block);
                    assert_eq!(w == 0, due(rendered), "d {d} r {rendered} b {block}");
                    if w > 0 {
                        assert!(due(rendered + w));
                        assert!(!due(rendered + w - 1));
                    }
                }
            }
        }
    }

    /// An engine (1 kHz, lookahead `block`) whose only deadline is a Memo's pre-wait ending on
    /// sample `d`.
    fn engine_with_deadline(block: u64, d: u64) -> CueEngine {
        let ws = memo_workspace(d as f64 / 1000.0);
        let mut engine = CueEngine::new(EngineConfig::new(1000, block, 1).unwrap());
        engine.handle(
            &ws,
            Input::Start {
                cue: CueId(uuid_zero()),
            },
            0,
        );
        assert_eq!(engine.next_deadline(), Some(d));
        engine
    }

    #[test]
    fn wake_frames_for_deferred_reports() {
        // Deliverable once `at < now`, now = rendered + block.
        let block = 10;
        for at in 0..100u64 {
            for rendered in 0..100u64 {
                let w = wake_frames(None, std::iter::once(at), rendered, block);
                assert_eq!(w == 0, at < rendered + block, "at {at} r {rendered}");
                if w > 0 {
                    assert!(at < rendered + w + block);
                }
            }
        }
        assert_eq!(wake_frames(None, std::iter::empty(), 5, 10), u64::MAX);
        assert_eq!(
            wake_frames(Some(u64::MAX), std::iter::empty(), 0, 10),
            u64::MAX - 20
        );
    }

    /// The engine's own `is_due` with lookahead = block matches the rule `wake_frames` assumes.
    #[test]
    fn the_wake_rule_matches_the_engine() {
        let block = 64;
        let ws = memo_workspace(0.5);
        let mut engine = CueEngine::new(EngineConfig::new(1000, block, 1).unwrap());
        engine.handle(
            &ws,
            Input::Start {
                cue: CueId(uuid_zero()),
            },
            block,
        );
        let d = engine.next_deadline().unwrap();
        for rendered in 0..d + 10 {
            let w = wake_frames(Some(d), std::iter::empty(), rendered, block);
            assert_eq!(
                w == 0,
                engine.is_due(rendered + block),
                "rendered {rendered}"
            );
        }
    }

    fn memo_workspace(pre_wait: f64) -> Workspace {
        Workspace {
            id: openlab_core::WorkspaceId(uuid_zero()),
            name: String::new(),
            settings: openlab_core::WorkspaceSettings {
                min_go_interval: 0.0,
                panic_duration: 1.0,
                autosave_interval: 30.0,
                osc_passcode: None,
                master_db: 0.0,
                hotkeys: openlab_core::TransportHotkeys::default(),
                auto_number_start: 1.0,
                auto_number_increment: 1.0,
            },
            patches: vec![],
            network_patches: vec![],
            cue_lists: vec![openlab_core::CueList {
                id: openlab_core::CueListId(uuid_zero()),
                name: String::new(),
                cues: vec![Cue {
                    id: CueId(uuid_zero()),
                    number: None,
                    name: String::new(),
                    notes: String::new(),
                    color: openlab_core::CueColor::None,
                    flagged: false,
                    armed: true,
                    pre_wait,
                    post_wait: 0.0,
                    continue_mode: openlab_core::ContinueMode::DoNotContinue,
                    triggers: openlab_core::Triggers {
                        hotkey: None,
                        osc: None,
                    },
                    second_trigger: openlab_core::SecondTrigger::Nothing,
                    second_trigger_on_release: false,
                    kind: CueKind::Memo,
                }],
            }],
        }
    }

    fn uuid_zero() -> uuid::Uuid {
        uuid::Uuid::from_u128(0)
    }
}
