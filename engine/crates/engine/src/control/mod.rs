// SPDX-License-Identifier: GPL-3.0-or-later

//! The control thread (ARCHITECTURE §3.1, §3.2; docs/control.md): owns the workspace, its undo
//! history, the cue engine and the mixer controller, and wires them to the disk streams.
//!
//! [`Control`] is the loop's state and logic, with no thread or clock of its own: time is the
//! mixer's sample counter (`samples_rendered`), whether a device callback ([`runner::run`]) or
//! the offline render driver ([`offline::OfflineShow`], tests) advances it. Both call the same
//! [`Control::handle`] for each inbox message and [`Control::tick`] on every wake.

mod inbox;
mod media;
pub mod offline;
mod patches;
pub mod plan;
pub mod runner;
mod voices;

use std::collections::VecDeque;
use std::hash::{BuildHasher, RandomState};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crossbeam_channel::Sender;
use openlab_audio::{
    COMMAND_CAPACITY, CommandKind, DeviceMonitor, DiskStreamer, MediaError, MixerController,
    StreamerConfig, VoiceId,
};
use openlab_core::protocol::{
    ClientMessage, CueMeter, CueProblems, CueStateEntry, DocumentState, ErrorBody, ErrorCode,
    LogLevel, OkBody, Outcome, OutputMeter, PlayheadEntry, ProgressItem, Request, RuntimeChange,
    ServerMessage, Snapshot, TransportAction, WireProblem, WireWarning,
};
use openlab_core::{
    Action, CommandError, Cue, CueEngine, CueId, CueKind, CueState, DeviceInfo, EngineConfig,
    EngineConfigError, History, Input, MachineSettings, MediaStatus, SampleTime, Workspace,
};

pub use inbox::{Inbox, OUTBOUND_CAPACITY, ProbeAnswer};
pub use media::PROBE_WORKERS;

use inbox::{Outbound, reply};
use media::MediaCatalog;
use patches::Patches;
use plan::{PlanFailure, PlanSpec, cue_slices};
use voices::{Effects, Env, Voices};

/// The longest the loop sleeps without a reason to wake (ARCHITECTURE §3.2).
pub const MAX_WAIT: Duration = Duration::from_millis(5);
/// Progress and meters go out at most this often (docs/protocol.md §4).
pub const REPORT_HZ: u64 = 30;
/// Cues primed from each playhead (ARCHITECTURE §3.5).
pub const PRIME_CUES: usize = 16;
/// Most streams kept primed at once.
pub const MAX_PRIMES: usize = 64;
/// Feedback rounds one input may cause before the loop stops following them (a safety net; the
/// cue engine bounds its own chains).
const FEEDBACK_LIMIT: usize = 4096;

/// How the control loop reaches the audio side, besides the mixer controller. The clock is the
/// mixer's either way; this carries what only a device has.
pub enum Backend {
    /// A device stream opened with `open_output`. Device loss and reopen (`needs_reopen`,
    /// `MixerPending`) are M4.2b: the monitor is where that policy plugs in.
    Device { monitor: DeviceMonitor },
    /// The offline render driver: nothing but the clock.
    Offline,
}

impl Backend {
    fn alloc_violations(&self) -> u64 {
        match self {
            Self::Device { monitor } => monitor.alloc_violations(),
            Self::Offline => 0,
        }
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
    /// Devices present (fixed until M4.2b's device list).
    pub devices: Vec<DeviceInfo>,
    /// Output patch matrices the mixer was built with.
    pub mixer_patches: usize,
    pub streamer: StreamerConfig,
    /// Memory all preloaded media may take together (AUD-05).
    pub preload_budget: u64,
    pub prime_cues: usize,
    pub max_primes: usize,
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
            prime_cues: PRIME_CUES,
            max_primes: MAX_PRIMES,
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
    patches: Patches,
    out: Outbound,
    backend: Backend,
    config: ControlConfig,
    rate: NonZeroU32,
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
    seen: Seen,
    last_progress: Option<SampleTime>,
    last_meters: Option<SampleTime>,
    meters_live: bool,
    /// Rendered sample of the previous voice-meter read (reconciliation).
    last_meter_read: Option<SampleTime>,
    out_peaks: Vec<f32>,
    meter_ids: Vec<VoiceId>,
    meter_peaks: Vec<f32>,
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
        outbound: Sender<ServerMessage>,
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
        let media = MediaCatalog::new(inbox, show_dir, config.preload_budget)?;
        let cue_outputs = mixer.cue_outputs();
        let patches = Patches::new(config.mixer_patches, cue_outputs);
        let mut control = Self {
            ws,
            history: History::new(),
            engine,
            mixer,
            streamer,
            media,
            voices: Voices::new(),
            patches,
            out: Outbound::new(outbound),
            backend,
            config,
            rate,
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
            seen: Seen::default(),
            last_progress: None,
            last_meters: None,
            meters_live: false,
            last_meter_read: None,
            out_peaks: vec![0.0; cue_outputs],
            meter_ids: Vec::new(),
            meter_peaks: Vec::new(),
        };
        let cap = control.mixer.voice_meter_capacity();
        control.meter_ids = vec![VoiceId(0); cap];
        control.meter_peaks = vec![0.0; cap * cue_outputs.max(1)];
        let now = control.now();
        // A newly opened workspace: tell the engine, report the playheads (docs/cue-engine.md).
        control.engine.model_changed();
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
            .saturating_add(self.config.block_frames.max(1))
    }

    /// Handles one inbox message.
    pub fn handle(&mut self, msg: Inbox) -> Flow {
        match msg {
            Inbox::Client { message, reply } => return self.client(message, &reply),
            Inbox::Probe(answer) => self.probe_answer(answer),
            Inbox::Shutdown => return Flow::Shutdown,
        }
        Flow::Continue
    }

    /// One wake of the loop: feedback from the mixer, the streams and the probes, due
    /// deadlines, priming, reports, then `pump` (ARCHITECTURE §3.2; docs/audio.md: every tick).
    pub fn tick(&mut self) {
        let now = self.now();
        self.flush_media(now);
        let mut fx = Effects::default();
        while let Some(e) = self.mixer.poll_event() {
            self.voices.event(e, &mut fx);
        }
        self.voices.poll_streams(&mut fx, &self.ws);
        self.follow(fx, now);
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
        self.health();
        self.mixer.pump();
    }

    /// How long the loop may wait for the inbox before it must tick again: until the next
    /// deadline enters the lookahead window, a deferred report becomes due, or [`MAX_WAIT`].
    pub fn wait_hint(&self) -> Duration {
        let rendered = self.mixer.samples_rendered();
        let block = self.config.block_frames.max(1);
        let mut frames = u64::MAX;
        if let Some(d) = self.engine.next_deadline() {
            // Due once `d < rendered + 2 * block`.
            frames = frames.min(
                d.saturating_add(1)
                    .saturating_sub(2 * block)
                    .saturating_sub(rendered),
            );
        }
        for (at, _) in &self.deferred {
            frames = frames.min(
                at.saturating_add(1)
                    .saturating_sub(block)
                    .saturating_sub(rendered),
            );
        }
        let secs = frames as f64 / f64::from(self.rate.get());
        Duration::from_secs_f64(secs.min(MAX_WAIT.as_secs_f64()))
    }

    fn client(&mut self, message: ClientMessage, to: &Sender<ServerMessage>) -> Flow {
        let now = self.now();
        match message {
            ClientMessage::Transport { id, action } => {
                self.feed(transport_input(action), now);
                reply(to, ok(id, None));
            }
            ClientMessage::Command { id, command } => {
                match self.history.execute(&mut self.ws, *command) {
                    Ok(()) => {
                        self.edited(now);
                        reply(to, ok(id, Some(self.revision)));
                    }
                    Err(e) => reply(to, refused(id, &e)),
                }
            }
            ClientMessage::Undo { id } | ClientMessage::Redo { id } => {
                let undo = matches!(message, ClientMessage::Undo { .. });
                let result = if undo {
                    self.history.undo(&mut self.ws)
                } else {
                    self.history.redo(&mut self.ws)
                };
                match result {
                    Ok(true) => {
                        self.edited(now);
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
                // Documents and devices are M4.4.
                _ => reply(
                    to,
                    ServerMessage::Result {
                        id,
                        outcome: Outcome::Error(ErrorBody::new(
                            ErrorCode::Unsupported,
                            format!("{} is not available yet", request_name(&request)),
                        )),
                    },
                ),
            },
            ClientMessage::Goodbye { shutdown: true } => return Flow::Shutdown,
            // The network layer handles the handshake and closing (M4.3).
            ClientMessage::Hello { .. } | ClientMessage::Goodbye { .. } => {}
        }
        Flow::Continue
    }

    /// A command, undo or redo changed the workspace.
    fn edited(&mut self, now: SampleTime) {
        let inputs = self.model_changed();
        self.document();
        for input in inputs {
            self.feed(input, now);
        }
        // Follow the edit at once: a deleted running cue stops now, not at the next deadline.
        self.feed(Input::Tick, now);
    }

    /// Signals a workspace change to the engine (before its next `handle`, M4.2 contract) and
    /// the clients (a revision bump: the upserts are M4.1a), and follows it on the audio side.
    /// Returns the inputs that must follow (patch resolutions).
    /// An Arm/Disarm change (`SetArmed`) goes through here too: it is saved with the show, so it
    /// makes the document dirty, but it is not an undo step (QLab confirmation: PROGRESS M2.17).
    fn model_changed(&mut self) -> Vec<Input> {
        self.engine.model_changed();
        self.revision += 1;
        self.out.send(ServerMessage::ModelChanged {
            revision: self.revision,
            upserts: Vec::new(),
            deletes: Vec::new(),
        });
        self.sync_workspace(false)
    }

    /// Brings media, patches and primes in line with the workspace. `all` reports every patch
    /// resolution (a newly opened workspace), else only changes.
    fn sync_workspace(&mut self, all: bool) -> Vec<Input> {
        self.media.sync(&self.ws);
        let (cmds, changed) =
            self.patches
                .sync(&self.ws, &self.config.machine, &self.config.devices);
        let now = self.now();
        for cmd in cmds {
            self.mixer.send(now, cmd);
        }
        self.primes_dirty = true;
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

    fn probe_answer(&mut self, answer: ProbeAnswer) {
        let ws = &self.ws;
        let wants = |key: &str, probed: &openlab_audio::Probed| {
            probed.preload_advised() || cue_wants_preload(ws, key)
        };
        let key = match &answer {
            ProbeAnswer::Probed { key, .. } => Some(key.clone()),
            ProbeAnswer::Preloaded { .. } => None,
        };
        if let Some(status) = self.media.answer(answer, wants) {
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
            let mut env = Env {
                ws: &self.ws,
                media: &self.media,
                streamer: &mut self.streamer,
                patches: &self.patches,
                rate: self.rate,
                cue_outputs: self.cue_outputs,
                now,
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
        let mut armed = false;
        let mut fx = Effects::default();
        {
            let mut env = Env {
                ws: &self.ws,
                media: &self.media,
                streamer: &mut self.streamer,
                patches: &self.patches,
                rate: self.rate,
                cue_outputs: self.cue_outputs,
                now,
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
            if let Action::SetArmed { cue, armed: value } = action {
                armed |= set_armed(&mut self.ws, *cue, *value);
            }
        }
        if !changes.is_empty() {
            self.runtime(changes);
        }
        let mut feedback = self.after(fx);
        if armed {
            feedback.extend(self.model_changed());
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
                .collect();
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
        self.out.send(ServerMessage::Document { document });
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

    fn log(&mut self, level: LogLevel, message: String) {
        self.out.send(ServerMessage::Log { level, message });
    }

    /// Keeps the cues near each playhead primed (ARCHITECTURE §3.5).
    fn prime(&mut self, now: SampleTime) {
        let wanted = prime_set(&self.ws, &self.engine, self.config.prime_cues);
        let mut env = Env {
            ws: &self.ws,
            media: &self.media,
            streamer: &mut self.streamer,
            patches: &self.patches,
            rate: self.rate,
            cue_outputs: self.cue_outputs,
            now,
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
                self.out.send(m);
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
            self.out.send(m);
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
        let out = self.out.dropped();
        if out > self.seen.outbound_dropped {
            self.seen.outbound_dropped = out;
        }
    }

    /// The workspace-level notices in force: damaged media, preloads over budget, cues whose
    /// slices cannot play, audio-thread allocations. Sent when the list changes.
    fn refresh_warnings(&mut self) {
        let mut list = Vec::new();
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
        for (cue, media) in openlab_core::audio_media(&self.ws) {
            let Some((ready, _)) = self.media.media(media.as_str()) else {
                continue;
            };
            let Some(c) = self.ws.find_cue(cue) else {
                continue;
            };
            if let Some(spec) = spec_of(c)
                && let Err(PlanFailure::Plan(e)) =
                    cue_slices(&spec, &ready.probed.info, self.rate, 0)
            {
                list.push(notice(
                    "bad_plan",
                    format!("the cue's slices cannot be played: {e}"),
                    Some(cue),
                ));
            }
        }
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
            self.out.send(ServerMessage::Warnings { warnings: list });
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

    /// Memory taken from the preload budget.
    pub fn preload_reserved(&self) -> u64 {
        self.media.preload_reserved()
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
        Request::New => "new",
        Request::Open { .. } => "open",
        Request::Save => "save",
        Request::SaveAs { .. } => "save_as",
        Request::Revert => "revert",
        Request::Relink { .. } => "relink",
        Request::ListDevices => "list_devices",
        Request::Snapshot => "snapshot",
    }
}

/// The cue-engine input for a transport action (docs/protocol.md, Transport actions).
pub fn transport_input(action: TransportAction) -> Input {
    match action {
        TransportAction::Go { list } => Input::Go { list },
        TransportAction::Panic => Input::Panic,
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

fn spec_of(c: &Cue) -> Option<PlanSpec<'_>> {
    match &c.kind {
        CueKind::Audio {
            start_time,
            end_time,
            slices,
            last_slice_play_count,
            ..
        } => Some(PlanSpec {
            start_time: *start_time,
            end_time: *end_time,
            slices,
            last_slice_play_count: *last_slice_play_count,
        }),
        _ => None,
    }
}

fn cue_wants_preload(ws: &Workspace, key: &str) -> bool {
    let mut wants = false;
    for list in &ws.cue_lists {
        visit(&list.cues, &mut |c| {
            if let CueKind::Audio {
                media: Some(m),
                preload: true,
                ..
            } = &c.kind
                && m.as_str() == key
            {
                wants = true;
            }
        });
    }
    wants
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
