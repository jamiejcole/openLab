// SPDX-License-Identifier: GPL-3.0-or-later

//! The control thread (ARCHITECTURE §3.1, §3.2; docs/control.md): owns the workspace, its undo
//! history, the cue engine and the mixer controller, and wires them to the disk streams.
//!
//! The state is split as ARCHITECTURE §3.1 assigns it: an `Engine` every workspace shares (mixer,
//! device, disk streamer, voice-id and patch-slot allocators, machine settings, document worker,
//! outbound channel, show log, probe pool, media watch) and a `Doc` per open workspace (model,
//! history, cue engine, voices, media catalog, file state), at most [`MAX_DOCS`]. Per-doc work
//! runs on a `DocCx` (one doc and the engine); engine-wide work (the device, mixer events and
//! meters, the watch, the probe watchdog, health, status) runs once per wake and hands each doc
//! its part. The client protocol still addresses one doc, the first open (several connections,
//! each bound to a doc, are M4.15f); [`Control::open_doc`], [`Control::close_doc`] and
//! [`Control::handle_doc`] address any doc by its [`DocId`].
//!
//! [`Control`] is the loop's state and logic, with no thread or clock of its own: time is the
//! mixer's sample counter (`samples_rendered`), whether a device callback ([`runner::run`]) or
//! the offline render driver ([`offline::OfflineShow`], tests) advances it. Both call the same
//! [`Control::handle`] for each inbox message and [`Control::tick`] on every wake.

mod checks;
mod client;
mod device_events;
pub mod devices;
mod docs;
mod edits;
mod execute;
mod faults;
mod inbox;
mod inspect;
mod mapping;
mod media;
mod model;
mod netout;
mod network;
mod observe;
pub mod offline;
mod patches;
pub mod plan;
mod preshow;
mod prime;
mod reports;
pub mod runner;
mod tick;
mod transport;
mod voice_ids;
mod voices;
mod wake;
pub mod watch;
mod watch_share;
mod waveforms;

use std::hash::{BuildHasher, RandomState};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use crossbeam_channel::Sender;
use openlab_audio::{
    COMMAND_CAPACITY, CommandKind, DiskStreamer, Event, MediaError, MixerController,
    StreamerConfig, VoiceId,
};
use openlab_core::protocol::{
    CueProblems, CueStateEntry, DocumentState, ErrorBody, ErrorCode, LogKind, LogLevel, OkBody,
    Outcome, PlayheadEntry, Request, RuntimeChange, ServerMessage, Snapshot, WireProblem,
    WireWarning,
};
use openlab_core::{
    CommandError, CueEngine, CueId, CueKind, CueState, DeviceInfo, EngineConfig, EngineConfigError,
    History, Input, MachineSettings, MediaStatus, SampleTime, Workspace,
};

pub use docs::{
    DocAnswer, DocHook, DocSource, EpochClock, FinalWrites, Loaded, ManualClock, SaveHook,
};
pub use inbox::{
    Inbox, OUTBOUND_CAPACITY, OutboundRx, OutboundTx, Outgoing, ProbeAnswer, ReplyRx, ReplyTx,
    outbound_channel, reply_channel,
};
pub use inspect::DocView;
pub use media::{PROBE_WORKERS, ProbeClock};
pub use netout::{
    Destination, LINK_QUEUE, LookupThreads, NetAnswer, NetTransport, SystemTransport, Transport,
};
pub use transport::transport_input;
pub use wake::{stall_floor, wake_frames};

use crate::persist::AppDirs;
use checks::PlanChecks;
use device_events::playable_device;
use devices::Device;
use docs::{DocFile, Docs};
use inbox::{Outbound, reply};
use media::{MediaCatalog, ProbePool};
use model::DocModel;
use netout::NetOut;
use observe::Observe;
use patches::Patches;
use transport::Scope;
use voice_ids::VoiceIds;
use voices::Voices;
use watch::{MediaWatch, WatchBatch};
use watch_share::WatchShare;

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
/// A started voice whose stream has not delivered its first chunk after this long (sample
/// time; an open that hangs on a dead volume) is stopped and its cue breaks, instead of
/// running silent and playing late if the volume ever answers (docs/control.md).
pub const STREAM_START_LIMIT: Duration = Duration::from_secs(5);
/// A document read stuck this long fails with `io` and its worker is replaced.
pub const DOC_TIMEOUT: Duration = Duration::from_secs(20);
/// How long shutdown waits for the final recovery writes of the docs with unsaved changes
/// (M4.15g): a whole-show serialisation on a slow machine plus a write to a slow disk, and
/// short enough that a hung volume does not keep a quitting engine alive.
pub const SHUTDOWN_WAIT: Duration = Duration::from_secs(10);
/// A probe worker stuck on one file this long is replaced.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// Feedback rounds one input may cause before the loop stops following them (a safety net; the
/// cue engine bounds its own chains).
const FEEDBACK_LIMIT: usize = 4096;
/// Workspaces open at once (REQUIREMENTS §21); one more is `command_refused`.
pub const MAX_DOCS: usize = 8;
// Every open show's show file and recovery file may be stuck on a hung volume with a writer
// thread still serving the others (#327).
const _: () = assert!(crate::persist::MAX_WRITERS > 2 * MAX_DOCS);

/// An open doc (workspace) in this engine run: the protocol's (ids never reused).
pub use openlab_core::protocol::DocId;

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
    /// The first doc's show file, if saved: relative media references resolve against its
    /// folder. Only the initial path: `Control::new` takes it into the doc, so it is `None`
    /// in the engine's config afterwards.
    pub show_path: Option<PathBuf>,
    pub machine: MachineSettings,
    /// False when `machine` is defaults standing in for a settings file that could not be read:
    /// the engine then keeps default mappings in memory and never writes the file (M4.4c).
    pub machine_writable: bool,
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
    /// Bounds on a `relink` folder walk (WS-03); tests shrink them.
    pub relink_limits: crate::persist::ScanLimits,
    /// How long [`Control::shutdown`] waits for the final recovery writes ([`SHUTDOWN_WAIT`]).
    pub shutdown_wait: Duration,
    /// How files are probed (tests inject a stand-in).
    pub probe: media::ProbeFn,
    /// A probe worker busy on one file longer than this is replaced (a stalled volume).
    pub probe_timeout: Duration,
    /// How waveforms are read on the probe workers (tests inject a stand-in).
    pub waveform: media::WaveformFn,
    /// The clock `probe_timeout` is measured on (tests inject one that only moves when told).
    pub probe_clock: media::ProbeClock,
    /// The monotonic clock the save watchdog measures `doc_timeout` on (a wall-clock step must
    /// not fail a save or postpone the watchdog; tests inject one).
    pub stall_clock: media::ProbeClock,
    /// Which volume a media path is on, for probe scheduling (tests inject one; the media
    /// watch's `bucket_of` by default).
    pub probe_volume: Option<fn(&std::path::Path) -> String>,
    /// The show log: its ring always, and the disk writer's sink when the host started one
    /// (docs/logging.md).
    pub show_log: crate::diag::ShowLog,
    /// Send `status` messages (DX-03). `None`: never (offline tests; the host turns it on).
    pub status: Option<crate::diag::StatusConfig>,
    /// Push each new show-log record as a `log_entry` message (DX-02, M4.7c). Off by default
    /// (offline tests); the host turns it on.
    pub push_log: bool,
    /// Process CPU seconds for `status` (tests inject a fake).
    pub cpu_probe: fn() -> Option<f64>,
    /// What Network cues send through and host names are looked up with (tests inject a
    /// stand-in).
    pub net: NetTransport,
    /// The media watch (M2.25): how files are statted, how often, and on which thread.
    pub watch: watch::WatchConfig,
    /// Where the loop says whether the computer must stay awake (SM-07: a doc in Show Mode or
    /// a cue running; `platform::power`). `None`: nobody listens (offline tests).
    pub awake: Option<Sender<bool>>,
}

impl ControlConfig {
    pub fn new(sample_rate: u32, block_frames: u64) -> Self {
        Self {
            sample_rate,
            block_frames,
            seed: entropy_seed(),
            show_path: None,
            machine: MachineSettings::default(),
            machine_writable: true,
            devices: Vec::new(),
            mixer_patches: 1,
            streamer: StreamerConfig::new(sample_rate),
            preload_budget: openlab_audio::DEFAULT_PRELOAD_BUDGET,
            app_dirs: None,
            clock: EpochClock::system(),
            save_hook: None,
            doc_hook: None,
            doc_timeout: DOC_TIMEOUT,
            relink_limits: crate::persist::ScanLimits::default(),
            shutdown_wait: SHUTDOWN_WAIT,
            prime_cues: PRIME_CUES,
            max_primes: MAX_PRIMES,
            probe: openlab_audio::probe,
            probe_timeout: PROBE_TIMEOUT,
            waveform: media::read_waveform,
            probe_clock: media::ProbeClock::system(),
            stall_clock: media::ProbeClock::system(),
            probe_volume: None,
            show_log: crate::diag::ShowLog::default(),
            status: None,
            push_log: false,
            cpu_probe: crate::diag::cpu::process_cpu_seconds,
            net: NetTransport::default(),
            watch: watch::WatchConfig::default(),
            awake: None,
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

/// The control loop's state: the [`Engine`] every workspace shares and the open [`Doc`]s, in
/// open order (ARCHITECTURE §1, §3.1). See the module docs and docs/control.md.
pub struct Control {
    eng: Engine,
    docs: Vec<Doc>,
    /// What the first-doc accessors show while no doc is open.
    none: NoDoc,
}

/// An empty workspace and an idle cue engine, for the accessors of "the first doc" when none is
/// open (the engine itself runs with zero docs; nothing is played from these).
struct NoDoc {
    ws: Workspace,
    engine: CueEngine,
}

/// A doc's use of the engine's prime pool.
#[derive(Clone, Copy, Debug, Default)]
struct PrimeUse {
    /// Cues it would keep primed (at most `max_primes`).
    want: usize,
    /// Primes it holds.
    held: usize,
    /// The allowance its last pass kept to: below `want`, it takes more when the allowance
    /// grows (another doc let go or closed).
    cap: usize,
}

/// One meter read of this wake, shared by every doc's report (seams note (d): read once,
/// handed out per doc).
#[derive(Clone, Copy)]
struct MeterRound {
    rendered: SampleTime,
    /// The previous read, if any (reconciliation needs two in a row).
    prev: Option<SampleTime>,
    /// Voices the read listed.
    listed: usize,
}

/// What every open workspace shares (ARCHITECTURE §3.1, §3.5): the mixer and its clock, the
/// device, the disk streamer, the voice-id and patch-slot allocators, the machine settings,
/// the document worker and the save writer, the outbound channel, the show log and status.
struct Engine {
    mixer: MixerController,
    streamer: DiskStreamer,
    /// Mixer voice ids: engine-wide, never reused (shared by every workspace with M4.15).
    voice_ids: VoiceIds,
    patches: Patches,
    /// What patches resolve against: the device the mixer plays on (none during an outage).
    playable: Vec<DeviceInfo>,
    /// Saves the machine settings when the engine changes them (default patch mappings).
    settings_writer: Option<crate::persist::SettingsWriter>,
    /// Patches and devices the default mapping already warned about, per doc.
    mapping_warned: std::collections::HashSet<(
        DocId,
        openlab_core::WorkspaceId,
        openlab_core::PatchId,
        String,
    )>,
    /// The unwritable-settings warning went out.
    unwritable_warned: bool,
    out: Outbound,
    /// The document worker (one read at a time, engine-wide) and the save writer.
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
    seen: Seen,
    /// Mixer stage fault reports handled ([`Control::stage_faulted`]).
    stage_reports: u64,
    /// Mixer units disabled after repeated faults, and when each stage was last told.
    stage_faults: faults::StageFaults,
    last_meters: Option<SampleTime>,
    /// Rendered sample of the previous voice-meter read (reconciliation).
    last_meter_read: Option<SampleTime>,
    /// This wake's meter read, while the docs take their parts of it.
    meter_round: Option<MeterRound>,
    out_peaks: Vec<f32>,
    meter_ids: Vec<VoiceId>,
    meter_peaks: Vec<f32>,
    /// Show-log and status bookkeeping (`observe.rs`).
    observe: Observe,
    /// The probe workers every doc's catalog uses (one pool, ARCHITECTURE §3.1).
    pool: ProbePool,
    /// The one media watch, shared by the docs with per-file counts.
    watch: WatchShare,
    /// Watch batches taken from the inbox, handled at the next tick.
    watch_batches: Vec<WatchBatch>,
    /// PB-10: Network cues of every doc send nothing (runtime state, never saved).
    net_disabled: Arc<AtomicBool>,
    /// Host look-up threads alive, every doc's (at most `netout::MAX_LOOKUPS`).
    lookups: LookupThreads,
    /// Where workers answer (each new doc's network output gets a clone).
    inbox: Sender<Inbox>,
    /// The next doc's id; never reused.
    next_doc: u64,
    /// Docs open (log lines name their doc while there are several).
    open_docs: usize,
    /// Each doc's use of the prime pool as of its last priming pass.
    primes: std::collections::BTreeMap<DocId, PrimeUse>,
    /// `primes` changed since the docs' allowances were last looked at.
    primes_changed: bool,
    /// Docs installed by [`Control::open_doc`] and the request each answered (until taken).
    opened: Vec<(openlab_core::protocol::RequestId, DocId)>,
    /// The open docs as the last `workspaces` message listed them.
    listed: Vec<Listed>,
    /// That message did not fit the outbound channel: send the list again.
    list_unsent: bool,
    /// Closed docs whose `closed` message did not fit the outbound channel yet.
    closed_unsent: Vec<DocId>,
    /// What was last said on `config.awake` (SM-07).
    awake: bool,
}

/// One doc as the `workspaces` list showed it, to tell when the list changes without
/// building it (checked every tick).
#[derive(PartialEq)]
struct Listed {
    doc: DocId,
    workspace_id: openlab_core::WorkspaceId,
    name: String,
    path: Option<PathBuf>,
    dirty: bool,
}

impl Listed {
    fn of(d: &Doc) -> Self {
        Self {
            doc: d.id,
            workspace_id: d.model.ws().id,
            name: d.model.ws().name.clone(),
            path: d.show_path.clone(),
            dirty: d.is_dirty(),
        }
    }

    fn lists(&self, d: &Doc) -> bool {
        self.doc == d.id
            && self.workspace_id == d.model.ws().id
            && self.name == d.model.ws().name
            && self.path == d.show_path
            && self.dirty == d.is_dirty()
    }
}

/// One open workspace (ARCHITECTURE §3.1): its model, undo history, cue engine, voices, media
/// catalog, plan checks, network output, file state (path, saves, autosave) and what its
/// clients have been told (revision, runtime sequence, warnings).
struct Doc {
    id: DocId,
    /// The workspace and its cue engine: an edit of the workspace always reaches the engine.
    model: DocModel,
    history: History,
    media: MediaCatalog,
    /// Network cues' output and the look-ups of their hosts (CT-30, PB-10, NFR-07).
    net: NetOut,
    /// Mixer events of this doc's voices, routed here by the engine this wake.
    events: Vec<Event>,
    /// Mixer events routed to this doc, ever (tests: an event reaches its owner only).
    events_routed: u64,
    /// Model changes nobody signalled, already reported (seams note (c)).
    seen_unsignalled: u64,
    voices: Voices,
    checks: PlanChecks,
    /// The show file, if saved: relative media references resolve against its folder.
    show_path: Option<PathBuf>,
    /// Saves, autosave and load notices of this workspace.
    file: DocFile,
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
    last_progress: Option<SampleTime>,
    meters_live: bool,
    /// Waveforms read for `waveform` requests (AUD-14) and reads under way.
    waveforms: waveforms::Waveforms,
    /// Show Mode (SM-01, `set_show_mode`): in memory only; holds off sleep (SM-07).
    show_mode: bool,
}

/// Counters already reported, so each increase is reported once.
#[derive(Default)]
struct Seen {
    dropped_events: u64,
    faults: u64,
    faulted_voices: u64,
    faulted_stages: u64,
    disabled_units: openlab_audio::DisabledUnits,
    alloc_violations: u64,
    outbound_dropped: u64,
    feedback_overflow: u64,
}

impl Control {
    /// Builds the control state for `ws` around a mixer controller whose mixer is driven by
    /// `backend`: [`Self::empty`] with `ws` installed as the first doc ([`DocId`] 0), its
    /// patches sent to the mixer and the cue engine, and every media file of the show probed.
    /// The engine itself starts empty (M4.15g); this is for tests and the offline driver.
    pub fn new(
        ws: Workspace,
        mixer: MixerController,
        backend: Backend,
        mut config: ControlConfig,
        inbox: Sender<Inbox>,
        outbound: OutboundTx,
    ) -> Result<Self, ControlError> {
        // The doc's from here on (a save-as moves it); the config keeps only engine settings.
        let show_path = config.show_path.take();
        let mut control = Self::empty(mixer, backend, config, inbox, outbound)?;
        let id = control.eng.take_doc_id().unwrap_or(DocId(0));
        let show_dir = show_path
            .as_ref()
            .and_then(|p| p.parent())
            .map(PathBuf::from);
        let media = MediaCatalog::new(
            control.eng.pool.handle(id),
            show_dir,
            control.eng.config.preload_budget,
        );
        let engine = CueEngine::new(control.eng.engine_config(id)?);
        let doc = Doc::new(
            &control.eng,
            id,
            DocModel::new(ws, engine),
            show_path,
            media,
        );
        control.docs.push(doc);
        control.eng.open_docs = control.docs.len();
        // Before the doc's first reports: the app connections (and the OSC side, which follows
        // the first doc) learn of the doc before anything about it.
        control.publish_workspaces();
        if let Some(doc) = control.docs.first_mut() {
            let mut cx = DocCx {
                eng: &mut control.eng,
                doc,
            };
            let now = cx.now();
            // A newly opened workspace (the engine was told by `DocModel::new`): report the
            // playheads (docs/cue-engine.md).
            cx.doc.media.sync(cx.doc.model.ws());
            let mut inputs = vec![Input::Tick];
            inputs.extend(cx.sync_workspace(true));
            for input in inputs {
                cx.feed(input, now);
            }
        }
        control.eng.mixer.pump();
        Ok(control)
    }

    /// The control state with no doc open, as the engine starts (M4.15g; ARCHITECTURE §8): the
    /// client opens what it wants (`new`, `open`, which offers recovery, `open_recovery`).
    /// Starts the disk workers, the probe workers and the document worker (which answer on
    /// `inbox`).
    pub fn empty(
        mixer: MixerController,
        backend: Backend,
        mut config: ControlConfig,
        inbox: Sender<Inbox>,
        outbound: OutboundTx,
    ) -> Result<Self, ControlError> {
        let block = config.block_frames.max(1);
        let engine_config = EngineConfig::new(config.sample_rate, block, config.seed)?;
        let rate = engine_config.sample_rate();
        // Stream opens are told apart by the probe gate's volumes, so opens hung on one volume
        // never take every opener (docs/audio.md, Disk streaming).
        let mut streamer_config = config.streamer;
        streamer_config.volume_of = streamer_config
            .volume_of
            .or(config.probe_volume)
            .or(Some(watch::bucket_of));
        let streamer = DiskStreamer::new(streamer_config).map_err(ControlError::Streamer)?;
        // Only a doc has a show path ([`Self::new`]).
        config.show_path = None;
        let docs = Docs::new(&config, inbox.clone())?;
        let watch = WatchShare::new(MediaWatch::new(&config.watch, inbox.clone())?);
        let mut pool = ProbePool::new(
            inbox.clone(),
            config.probe,
            config.waveform,
            config.probe_timeout,
            config.probe_clock.clone(),
        )?;
        if let Some(volume) = config.probe_volume {
            pool.set_volume_of(volume);
        }
        let cue_outputs = mixer.cue_outputs();
        let patches = Patches::new(config.mixer_patches, cue_outputs);
        let playable = match backend.device() {
            Some(d) => vec![playable_device(d)],
            None => config.devices.clone(),
        };
        // Not essential: without the thread default mappings stay in memory (warned below).
        let (settings_writer, writer_error) = match &config.app_dirs {
            Some(dirs) => match crate::persist::SettingsWriter::spawn(dirs.clone()) {
                Ok(w) => (Some(w), None),
                Err(e) => (None, Some(e.to_string())),
            },
            None => (None, None),
        };
        let observe = Observe::new(&config);
        let none = NoDoc {
            ws: Workspace::new_empty(
                openlab_core::WorkspaceId(uuid::Uuid::nil()),
                openlab_core::CueListId(uuid::Uuid::nil()),
                openlab_core::PatchId(uuid::Uuid::nil()),
            ),
            engine: CueEngine::new(engine_config),
        };
        let mut eng = Engine {
            mixer,
            streamer,
            voice_ids: VoiceIds::new(),
            patches,
            playable,
            settings_writer,
            mapping_warned: std::collections::HashSet::new(),
            unwritable_warned: false,
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
            seen: Seen::default(),
            stage_reports: 0,
            stage_faults: faults::StageFaults::default(),
            last_meters: None,
            last_meter_read: None,
            meter_round: None,
            out_peaks: vec![0.0; cue_outputs],
            meter_ids: Vec::new(),
            meter_peaks: Vec::new(),
            observe,
            pool,
            watch,
            watch_batches: Vec::new(),
            net_disabled: Arc::new(AtomicBool::new(false)),
            lookups: LookupThreads::default(),
            inbox,
            next_doc: 0,
            open_docs: 0,
            primes: std::collections::BTreeMap::new(),
            primes_changed: false,
            opened: Vec::new(),
            listed: Vec::new(),
            list_unsent: false,
            closed_unsent: Vec::new(),
            awake: false,
        };
        if let Some(e) = writer_error {
            eng.record(
                LogKind::Engine,
                LogLevel::Warn,
                format!(
                    "could not start the settings writer ({e}); this machine's settings will not be saved"
                ),
                None,
                None,
            );
        }
        eng.record(
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
        let cap = eng.mixer.voice_meter_capacity();
        eng.meter_ids = vec![VoiceId(0); cap];
        eng.meter_peaks = vec![0.0; cap * cue_outputs.max(1)];
        let mut control = Self {
            eng,
            docs: Vec::new(),
            none,
        };
        control.eng.mixer.pump();
        Ok(control)
    }

    /// The first doc with the engine (unit tests).
    #[cfg(test)]
    fn first_cx(&mut self) -> DocCx<'_> {
        DocCx {
            eng: &mut self.eng,
            doc: &mut self.docs[0],
        }
    }

    /// Doc `id` with the engine.
    fn cx_of(&mut self, id: DocId) -> Option<DocCx<'_>> {
        let doc = self.docs.iter_mut().find(|d| d.id == id)?;
        Some(DocCx {
            eng: &mut self.eng,
            doc,
        })
    }

    /// Runs `f` on every open doc in open order.
    fn each_doc(&mut self, mut f: impl FnMut(&mut DocCx<'_>)) {
        let eng = &mut self.eng;
        for doc in &mut self.docs {
            f(&mut DocCx {
                eng: &mut *eng,
                doc,
            });
        }
    }

    /// The open docs, in open order.
    fn open_docs(&self) -> impl Iterator<Item = &Doc> {
        self.docs.iter()
    }
}

impl Engine {
    /// The scheduling time: the earliest sample a command can still hit.
    fn now(&self) -> SampleTime {
        self.mixer
            .samples_rendered()
            .saturating_add(self.block)
            .max(self.now_floor)
    }

    /// Says on `config.awake` whether the computer must stay awake, when that changes. Never
    /// blocks (unbounded channel; a power thread that is gone is ignored).
    fn keep_awake(&mut self, awake: bool) {
        if awake != self.awake {
            self.awake = awake;
            if let Some(tx) = &self.config.awake {
                let _ = tx.send(awake);
            }
        }
    }

    /// A fresh doc id (never reused); `None` once the counter is spent (2^64 opens).
    fn take_doc_id(&mut self) -> Option<DocId> {
        let id = self.next_doc;
        self.next_doc = id.checked_add(1)?;
        Some(DocId(id))
    }

    /// PB-10: whether the network output override is on.
    fn network_disabled(&self) -> bool {
        self.net_disabled.load(Ordering::SeqCst)
    }

    /// A message for the show log and every client's `log`.
    fn log(&mut self, level: LogLevel, message: String) {
        self.log_kind(LogKind::Engine, level, message);
    }

    fn log_device(&mut self, level: LogLevel, message: String) {
        self.log_kind(LogKind::Device, level, message);
    }

    /// While the device is down, voice stops are dated on the first sample the mixer renders
    /// when it is back (sample time stands still meanwhile, and `now` is one block past it):
    /// a stopped voice must not play one more block after the outage.
    fn stop_floor(&self) -> Option<SampleTime> {
        self.backend
            .is_down()
            .then(|| self.mixer.samples_rendered())
    }

    /// Sends commands as batches the mixer applies whole (docs/audio.md, Scheduling horizon).
    fn send_commands(&mut self, commands: Vec<(SampleTime, CommandKind)>) {
        for mut commands in stops_apart(commands) {
            while !commands.is_empty() {
                let rest = commands.split_off(commands.len().min(COMMAND_CAPACITY));
                if self.mixer.send_batch(commands).is_err() {
                    // Cannot happen: the batch is at most the queue's size.
                    self.log(LogLevel::Error, "mixer batch refused".to_owned());
                }
                commands = rest;
            }
        }
    }
}

/// Stops sent with nothing but ramps (a Panic or Stop All that also holds a running Fade cue's
/// levels where they are) go first, as a batch of their own, so they can overtake ramps held
/// ahead of them (`MixerController::pump`). Their order against those ramps does not matter: a
/// stop and a ramp move different gains, and a ramp of a voice already gone is ignored. Any
/// other mix (a start, a move) is sent as it is.
fn stops_apart(commands: Vec<(SampleTime, CommandKind)>) -> Vec<Vec<(SampleTime, CommandKind)>> {
    let stop = |k: &CommandKind| matches!(k, CommandKind::StopVoice { .. });
    let ramp = |k: &CommandKind| matches!(k, CommandKind::Ramp { .. });
    if !commands.iter().all(|(_, k)| stop(k) || ramp(k)) {
        return vec![commands];
    }
    let (stops, ramps): (Vec<_>, Vec<_>) = commands.into_iter().partition(|(_, k)| stop(k));
    [stops, ramps]
        .into_iter()
        .filter(|b| !b.is_empty())
        .collect()
}

impl Doc {
    /// A doc around a workspace, its cue engine and its catalog, before anything is sent for it.
    fn new(
        eng: &Engine,
        id: DocId,
        model: DocModel,
        show_path: Option<PathBuf>,
        media: MediaCatalog,
    ) -> Self {
        let net = NetOut::shared(
            eng.config.net.clone(),
            eng.inbox.clone(),
            eng.config.sample_rate,
            id,
            Arc::clone(&eng.net_disabled),
            eng.lookups.clone(),
        );
        Self {
            id,
            model,
            history: History::new(),
            media,
            net,
            events: Vec::new(),
            events_routed: 0,
            seen_unsignalled: 0,
            voices: Voices::new(),
            checks: PlanChecks::default(),
            show_path,
            file: DocFile::new(),
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
            last_progress: None,
            meters_live: false,
            waveforms: waveforms::Waveforms::default(),
            show_mode: false,
        }
    }

    fn document_state(&self) -> DocumentState {
        DocumentState {
            path: self
                .show_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            dirty: self.revision != self.saved_revision,
            can_undo: self.history.can_undo(),
            can_redo: self.history.can_redo(),
            show_mode: self.show_mode,
        }
    }

    /// Everything a client needs to draw the show (docs/protocol.md, Snapshot).
    fn snapshot(&self) -> Snapshot {
        let cue_states = self
            .model
            .engine()
            .cue_states()
            .map(|(cue, state)| CueStateEntry {
                cue,
                state: state.into(),
                reason: match state {
                    CueState::Broken => self.model.engine().broken_reason(cue).map(Into::into),
                    _ => None,
                },
            })
            .collect();
        let problems = self
            .model
            .engine()
            .broken_cues()
            .into_iter()
            .map(|(cue, problems)| CueProblems {
                cue,
                problems: problems.iter().map(WireProblem::from).collect(),
            })
            .collect();
        let group_warnings = self
            .model
            .ws()
            .cues()
            .filter(|c| matches!(c.kind, CueKind::Group { .. }))
            .map(|c| c.id)
            .filter(|&c| self.model.engine().has_broken_descendant(c))
            .collect();
        Snapshot {
            revision: self.revision,
            runtime_seq: self.runtime_seq,
            document: self.document_state(),
            workspace: self.model.ws().clone(),
            cue_states,
            problems,
            group_warnings,
            playheads: self
                .model
                .ws()
                .cue_lists
                .iter()
                .map(|l| PlayheadEntry {
                    list: l.id,
                    cue: self.model.engine().playhead(l.id),
                })
                .collect(),
            previewing: self.model.engine().previewing().collect(),
            warnings: self.warnings.clone(),
            media_durations: self.checks.media_durations(),
        }
    }
}

/// One doc and the engine it plays on: the control loop's work for one workspace.
struct DocCx<'a> {
    eng: &'a mut Engine,
    doc: &'a mut Doc,
}

impl DocCx<'_> {
    /// The scheduling time ([`Engine::now`]).
    fn now(&self) -> SampleTime {
        self.eng.now()
    }

    /// [`Engine::stop_floor`].
    fn stop_floor(&self) -> Option<SampleTime> {
        self.eng.stop_floor()
    }
}

impl DocCx<'_> {
    /// [`Engine::send_commands`].
    fn send_commands(&mut self, commands: Vec<(SampleTime, CommandKind)>) {
        self.eng.send_commands(commands);
    }

    fn runtime(&mut self, changes: Vec<RuntimeChange>) {
        self.doc.runtime_seq += 1;
        self.eng.out.send(
            Some(self.doc.id),
            ServerMessage::Runtime {
                seq: self.doc.runtime_seq,
                changes,
            },
        );
    }

    fn document(&mut self) {
        let document = self.doc.document_state();
        self.doc.resend_document = !self
            .eng
            .out
            .send_state(Some(self.doc.id), ServerMessage::Document { document });
    }

    /// A message for the show log and every client's `log`.
    fn log(&mut self, level: LogLevel, message: String) {
        let message = self.tagged(message);
        self.eng.log(level, message);
    }
}

impl Control {
    /// Stops the show for shutdown: a hard stop of everything in every doc, carried out by the
    /// mixer; then a final recovery write of every doc with unsaved changes, waited for at most
    /// [`ControlConfig::shutdown_wait`] (M4.15g, ARCHITECTURE §8). With no doc open, only the
    /// mixer's queue is pumped.
    pub fn shutdown(&mut self) -> FinalWrites {
        self.each_doc(|cx| {
            let now = cx.now();
            // A second panic within the fade is a hard stop (docs/cue-engine.md, Panic).
            cx.feed(Input::Panic, now);
            cx.feed(Input::Panic, now);
            cx.doc.voices.clear_primes();
        });
        self.eng.mixer.pump();
        self.eng.keep_awake(false);
        let wait = self.eng.config.shutdown_wait;
        self.final_recovery_writes(wait)
    }

    /// Whether the loop last asked to keep the computer awake (SM-07; tests).
    pub fn keeps_awake(&self) -> bool {
        self.eng.awake
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

/// The one way an error reply is built.
fn failed(id: openlab_core::protocol::RequestId, error: ErrorBody) -> ServerMessage {
    ServerMessage::Result {
        id,
        outcome: Outcome::Error(error),
    }
}

fn fail(
    to: &ReplyTx,
    id: openlab_core::protocol::RequestId,
    code: ErrorCode,
    message: impl Into<String>,
) {
    reply(to, failed(id, ErrorBody::new(code, message)));
}

fn refused(id: openlab_core::protocol::RequestId, e: &CommandError) -> ServerMessage {
    failed(id, ErrorBody::from(e))
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
        Request::Relink { .. } => "relink",
        Request::ListDevices => "list_devices",
        Request::PreshowCheck => "preshow_check",
        Request::SetShowMode { .. } => "set_show_mode",
        Request::SetPatchMapping { .. } => "set_patch_mapping",
        Request::LogTail { .. } => "log_tail",
        Request::SetOutputOverrides { .. } => "set_output_overrides",
        Request::Snapshot => "snapshot",
        Request::Waveform { .. } => "waveform",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlab_core::Cue;

    /// #291: a Panic's stops sent with a Fade cue's hold ramps go apart, first, so they can
    /// overtake ramps held ahead of them; with anything else in the batch it is kept whole.
    #[test]
    fn stops_sent_with_only_ramps_go_first_in_a_batch_of_their_own() {
        use openlab_audio::{Fade, GainTarget, VoiceId};
        let stop = |v| CommandKind::StopVoice {
            voice: VoiceId(v),
            fade: Fade::NONE,
        };
        let ramp = |v| CommandKind::Ramp {
            voice: VoiceId(v),
            target: GainTarget::Master,
            gain: 0.5,
            fade: Fade::NONE,
        };
        let shape = |b: Vec<Vec<(SampleTime, CommandKind)>>| -> Vec<Vec<(SampleTime, bool)>> {
            b.into_iter()
                .map(|b| {
                    b.into_iter()
                        .map(|(at, k)| (at, matches!(k, CommandKind::StopVoice { .. })))
                        .collect()
                })
                .collect()
        };
        let panic = vec![(1, ramp(1)), (2, stop(2)), (3, ramp(3)), (4, stop(4))];
        assert_eq!(
            shape(stops_apart(panic)),
            [vec![(2, true), (4, true)], vec![(1, false), (3, false)]]
        );
        assert_eq!(shape(stops_apart(vec![(5, stop(1))])), [vec![(5, true)]]);
        let mixed = vec![(1, stop(1)), (1, CommandKind::SetMaster { gain: 1.0 })];
        assert_eq!(stops_apart(mixed).len(), 1);
    }

    pub(super) fn memo_show() -> crate::control::offline::OfflineShow {
        let mixer = openlab_audio::MixerConfig {
            sample_rate: 8000,
            output_channels: 2,
            max_block_frames: 64,
            max_voices: 8,
            max_voice_channels: 2,
            patches: 1,
            lookahead_ms: 500,
        };
        crate::control::offline::OfflineShow::new(
            memo_workspace(10.0),
            mixer,
            ControlConfig::new(8000, 64),
        )
        .unwrap()
    }

    pub(super) fn memo_workspace(pre_wait: f64) -> Workspace {
        Workspace {
            id: openlab_core::WorkspaceId(uuid_zero()),
            name: String::new(),
            settings: openlab_core::WorkspaceSettings {
                min_go_interval: 0.0,
                panic_duration: 1.0,
                autosave_interval: 30.0,
                osc_access: openlab_core::OscAccess::default(),
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

    pub(super) fn uuid_zero() -> uuid::Uuid {
        uuid::Uuid::from_u128(0)
    }
}
