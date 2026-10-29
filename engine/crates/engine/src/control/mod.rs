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
pub mod devices;
mod docs;
mod faults;
mod inbox;
mod mapping;
mod media;
mod netout;
mod observe;
pub mod offline;
mod patches;
pub mod plan;
mod preshow;
pub mod runner;
mod voice_ids;
mod voices;
mod wake;
pub mod watch;
mod watch_share;

use std::collections::VecDeque;
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
    ClientMessage, CueMeter, CueProblems, CueStateEntry, DeviceEntry, DocumentState, ErrorBody,
    ErrorCode, HotkeyPhase, LogKind, LogLevel, MAX_COALESCE_KEY, OkBody, Outcome, OutputMeter,
    PlayheadEntry, ProgressItem, Request, ResultData, RuntimeChange, ServerMessage, Snapshot,
    TransportAction, WaitProgress, WireProblem, WireWait, WireWarning, WorkspaceInfo,
};
use openlab_core::{
    Action, BrokenReason, Command, CommandError, CueEngine, CueId, CueKind, CueState, DeviceInfo,
    EngineConfig, EngineConfigError, Footprint, History, Input, MachineSettings, MediaStatus,
    NetworkPatchId, OscArg, PatchResolution, SampleTime, Timer, Workspace, descendants,
    model_changes, model_changes_indexed, seconds_to_samples,
};

pub use docs::{
    DocAnswer, DocHook, DocSource, EpochClock, FinalWrites, Loaded, ManualClock, SaveHook,
};
pub use inbox::{
    Inbox, OUTBOUND_CAPACITY, OutboundRx, OutboundTx, Outgoing, ProbeAnswer, ReplyRx, ReplyTx,
    outbound_channel, reply_channel,
};
pub use media::{PROBE_WORKERS, ProbeClock};
pub use netout::{
    Destination, LINK_QUEUE, LookupThreads, NetAnswer, NetTransport, SystemTransport, Transport,
};
pub use wake::{stall_floor, wake_frames};

use crate::persist::AppDirs;
use checks::PlanChecks;
use devices::{Device, DeviceAnswer, DeviceEvent};
use docs::{DocFile, Docs};
use inbox::{Outbound, reply};
use media::{MediaCatalog, ProbePool};
use netout::{NetOut, SendOutcome};
use observe::Observe;
use patches::Patches;
use voice_ids::VoiceIds;
use voices::{Effects, Env, Voices};
use watch::{MediaWatch, WatchAnswer, WatchBatch, WatchEvent};
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

/// New primes one doc makes per pass while the pool is contended: a doc taking over another's
/// share fills its rings a few at a time, not in one disk burst beside a playing show.
const PRIME_BURST: usize = 4;

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

/// `pool` shared out max-min fairly between `wants` (in the same order): a want that fits its
/// fair share is met, and what it leaves goes to the larger ones. The shares sum to at most
/// `pool`, and to the sum of the wants when they fit.
fn share_out(pool: usize, wants: &[usize]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..wants.len()).collect();
    order.sort_by_key(|&i| wants.get(i).copied().unwrap_or(0));
    let mut shares = vec![0; wants.len()];
    let mut left = pool;
    for (k, &i) in order.iter().enumerate() {
        let fair = left / (wants.len() - k);
        let take = wants.get(i).copied().unwrap_or(0).min(fair);
        if let Some(s) = shares.get_mut(i) {
            *s = take;
        }
        left -= take;
    }
    shares
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
            workspace_id: d.ws.id,
            name: d.ws.name.clone(),
            path: d.show_path.clone(),
            dirty: d.is_dirty(),
        }
    }

    fn lists(&self, d: &Doc) -> bool {
        self.doc == d.id
            && self.workspace_id == d.ws.id
            && self.name == d.ws.name
            && self.path == d.show_path
            && self.dirty == d.is_dirty()
    }
}

/// One open workspace (ARCHITECTURE §3.1): its model, undo history, cue engine, voices, media
/// catalog, plan checks, network output, file state (path, saves, autosave) and what its
/// clients have been told (revision, runtime sequence, warnings).
struct Doc {
    id: DocId,
    ws: Workspace,
    history: History,
    engine: CueEngine,
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
        let doc = Doc::new(&control.eng, id, ws, engine, show_path, media);
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
            // A newly opened workspace: tell the engine, report the playheads
            // (docs/cue-engine.md).
            cx.doc.engine.model_changed();
            cx.doc.media.sync(&cx.doc.ws);
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
        let streamer = DiskStreamer::new(config.streamer).map_err(ControlError::Streamer)?;
        // Only a doc has a show path ([`Self::new`]).
        config.show_path = None;
        let docs = Docs::new(&config, inbox.clone())?;
        let watch = WatchShare::new(MediaWatch::new(&config.watch, inbox.clone())?);
        let mut pool = ProbePool::new(
            inbox.clone(),
            config.probe,
            config.probe_timeout,
            config.probe_clock.clone(),
        )?;
        if let Some(volume) = config.probe_volume {
            pool.set_volume_of(volume);
        }
        let cue_outputs = mixer.cue_outputs();
        let patches = Patches::new(config.mixer_patches, cue_outputs, rate);
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

    /// Handles one inbox message. A connection's message goes to the doc it is bound to, or is
    /// one of the app-scope messages ([`Self::app_scope`]); `Inbox::Client` (tests, the offline
    /// driver) addresses the first open doc. A worker's answer goes to the doc it was for, and one for
    /// a doc closed since is dropped.
    pub fn handle(&mut self, msg: Inbox) -> Flow {
        match msg {
            Inbox::Client { message, reply: to } => {
                let first = self.docs.first().map(|d| d.id);
                return self.client(Addr::Doc(first), message, &to);
            }
            Inbox::Conn {
                doc: Some(doc),
                message,
                reply: to,
            } => return self.client(Addr::Doc(Some(doc)), message, &to),
            Inbox::Conn {
                doc: None,
                message,
                reply: to,
            } => return self.client(Addr::App, message, &to),
            Inbox::Probe(doc, answer) => {
                if let Some(mut cx) = self.cx_of(doc) {
                    cx.probe_answer(answer);
                }
            }
            Inbox::Device(answer) => self.device_answer(answer),
            Inbox::Net(doc, answer) => {
                if let Some(mut cx) = self.cx_of(doc) {
                    cx.net_answer(answer);
                }
            }
            Inbox::Doc(answer) => self.doc_answer(answer),
            Inbox::Watch(WatchAnswer::Batch(batch)) => self.eng.watch_batches.push(batch),
            Inbox::Watch(WatchAnswer::BucketAnswered { bucket, healthy }) => {
                self.eng.watch.watch_mut().answered(&bucket, healthy);
            }
            Inbox::Log { level, message } => self.eng.log(level, message),
            Inbox::Trigger { source, detail } => self.eng.observe_trigger(source, detail),
            Inbox::Shutdown => return Flow::Shutdown,
        }
        Flow::Continue
    }

    /// A client message for doc `doc` (what a connection bound to it sends, M4.15f). An unknown
    /// or closed doc answers `no_workspace`.
    pub fn handle_doc(&mut self, doc: DocId, message: ClientMessage, to: &ReplyTx) -> Flow {
        self.client(Addr::Doc(Some(doc)), message, to)
    }

    /// A client message: app-scope ones first, whoever sends them; the rest act on the doc the
    /// sender is bound to.
    fn client(&mut self, from: Addr, message: ClientMessage, to: &ReplyTx) -> Flow {
        let message = match self.app_scope(from, message, to) {
            Ok(flow) => return flow,
            Err(message) => message,
        };
        match from {
            Addr::Doc(Some(doc)) => match message {
                // Saves look at the other docs' show files: the engine's, not the doc's.
                ClientMessage::Request {
                    id,
                    request: request @ (Request::Save | Request::SaveAs { .. }),
                } if self.cx_of(doc).is_some() => self.save(doc, id, request, to),
                // The folder walk is the document worker's, not the doc's.
                ClientMessage::Request {
                    id,
                    request: Request::Relink { folder },
                } if self.cx_of(doc).is_some() => {
                    self.relink(doc, id, folder, to);
                    Flow::Continue
                }
                // The mapping is the machine's: every doc resolves its patches again.
                ClientMessage::Request {
                    id,
                    request: Request::SetPatchMapping { patch, mapping },
                } if self.cx_of(doc).is_some() => {
                    self.set_patch_mapping(doc, id, patch, mapping, to)
                }
                message => match self.cx_of(doc) {
                    Some(mut cx) => cx.client(message, to),
                    None => no_doc(message, to),
                },
            },
            Addr::Doc(None) => no_doc(message, to),
            Addr::App => match message {
                // The one transport an app connection may send: Esc, which is app-scope.
                ClientMessage::Transport {
                    id,
                    action: TransportAction::Panic,
                } => {
                    self.panic_all();
                    reply(to, ok(id, None));
                    Flow::Continue
                }
                // The app connection's "snapshot" is the workspace list.
                ClientMessage::Request {
                    id,
                    request: Request::Snapshot,
                } => {
                    reply(
                        to,
                        ServerMessage::Workspaces {
                            workspaces: self.workspace_list(),
                        },
                    );
                    reply(to, ok(id, None));
                    Flow::Continue
                }
                message => no_doc(message, to),
            },
        }
    }

    /// Carries out an app-scope message (ARCHITECTURE §6: on any connection, whether or not a
    /// doc is open): opening a workspace, the recovery scan, closing one by id, the Esc panic
    /// and hotkeys of every doc, the PB-10 override, the device list and the show log. Any
    /// other message is handed back.
    fn app_scope(
        &mut self,
        from: Addr,
        message: ClientMessage,
        to: &ReplyTx,
    ) -> Result<Flow, ClientMessage> {
        match message {
            ClientMessage::Request { id, request } => match request {
                Request::SetOutputOverrides { network_disabled } => {
                    self.set_network_disabled(network_disabled);
                    reply(to, ok(id, None));
                }
                Request::New {} => self.open_doc(DocSource::New, id, to),
                Request::Open { path, recovery } => self.open_doc(
                    DocSource::Open {
                        path: PathBuf::from(path),
                        recovery,
                    },
                    id,
                    to,
                ),
                Request::OpenRecovery {
                    workspace_id,
                    path_hash,
                    modified_ms,
                } => self.open_doc(
                    DocSource::Recovery {
                        workspace_id,
                        path_hash,
                        modified_ms,
                    },
                    id,
                    to,
                ),
                Request::ScanRecovery => self.scan_recovery(id, to),
                Request::ListDevices => reply(
                    to,
                    ServerMessage::Result {
                        id,
                        outcome: Outcome::Ok(OkBody {
                            revision: None,
                            data: Some(ResultData::Devices {
                                devices: self.eng.device_list(),
                            }),
                        }),
                    },
                ),
                Request::LogTail { max } => {
                    let (entries, dropped, seq) = self.eng.log_tail(max);
                    reply(
                        to,
                        ServerMessage::Result {
                            id,
                            outcome: Outcome::Ok(OkBody {
                                revision: None,
                                data: Some(ResultData::LogTail {
                                    entries,
                                    dropped,
                                    seq,
                                }),
                            }),
                        },
                    );
                }
                request => return Err(ClientMessage::Request { id, request }),
            },
            ClientMessage::Close {
                id,
                doc,
                discard_changes,
            } => {
                let own = match from {
                    Addr::Doc(doc) => doc,
                    Addr::App => None,
                };
                let Some(doc) = doc.or(own) else {
                    fail(
                        to,
                        id,
                        ErrorCode::NoWorkspace,
                        "close names no workspace and this connection is bound to none",
                    );
                    return Ok(Flow::Continue);
                };
                match self.remove_doc(doc, discard_changes) {
                    Ok(()) => {
                        reply(to, ok(id, None));
                        self.closed(doc);
                    }
                    Err(e) => reply(to, failed(id, e)),
                }
            }
            ClientMessage::Panic { id } => {
                self.panic_all();
                reply(to, ok(id, None));
            }
            ClientMessage::Hotkey {
                id,
                key,
                modifiers,
                phase,
            } => {
                let hotkey = openlab_core::Hotkey { key, modifiers };
                match phase {
                    HotkeyPhase::Down => self.hotkey(&hotkey),
                    HotkeyPhase::Up => self.hotkey_released(&hotkey),
                }
                reply(to, ok(id, None));
            }
            message => return Err(message),
        }
        Ok(Flow::Continue)
    }

    /// PB-10: Network cues of every open doc send nothing (or again).
    fn set_network_disabled(&mut self, disabled: bool) {
        self.eng.set_network_disabled(disabled);
    }

    /// One wake of the loop (ARCHITECTURE §3.1, §3.2; docs/audio.md: every tick): the device,
    /// the mixer's events (each to the doc owning the voice), the media watch and this wake's
    /// meter read, once; then each doc ([`DocCx::tick`]: its feedback, deadlines, priming,
    /// reports, saves); then the engine's probe watchdog, slot hand-outs, document worker,
    /// settings, health and status, and `pump`. With no doc open only the engine's steps run.
    pub fn tick(&mut self) {
        let rendered = self.eng.mixer.samples_rendered();
        self.eng.last_tick_stalled = rendered == self.eng.last_tick_rendered;
        self.eng.last_tick_rendered = rendered;
        self.poll_device();
        self.route_mixer_events();
        self.watch_tick();
        self.begin_meter_round();
        self.each_doc(|cx| cx.tick());
        self.end_meter_round();
        self.balance_primes();
        self.follow_key_changes();
        self.probe_watchdog();
        self.hand_out_freed_slots();
        self.eng.check_stalled_read();
        self.eng.check_stalled_save();
        self.eng.poll_clears();
        self.poll_orphan_saves();
        self.eng.resend_closed();
        self.publish_workspaces();
        let awake = self
            .docs
            .iter()
            .any(|d| d.show_mode || d.engine.has_active_cues());
        self.eng.keep_awake(awake);
        self.eng.settings_tick();
        self.health();
        let paths = self.docs.iter().map(|d| d.show_path.as_deref());
        self.eng.observe_tick(paths);
        self.eng.mixer.pump();
    }

    /// The mixer's events, drained once and handed to the doc that owns each voice. An event of
    /// a voice no open doc owns (a closed doc's) is dropped; `PendingFull` means nothing to any
    /// doc; a stage fault is the engine's ([`Self::stage_faulted`]).
    fn route_mixer_events(&mut self) {
        while let Some(e) = self.eng.mixer.poll_event() {
            self.route_mixer_event(e);
        }
    }

    fn route_mixer_event(&mut self, e: Event) {
        if let Event::StageFaulted {
            stage, disabled, ..
        } = e
        {
            self.stage_faulted(stage, disabled);
            return;
        }
        let Some(voice) = event_voice(&e) else {
            return;
        };
        let Some(owner) = self.eng.voice_ids.owner(voice) else {
            return;
        };
        if let Some(doc) = self.docs.iter_mut().find(|d| d.id == owner) {
            doc.events.push(e);
            doc.events_routed += 1;
        }
    }

    /// Each catalog's key changes to the shared watch, then the watch's batches to the docs
    /// that watch their files, its watchdog, log entries and notices.
    fn watch_tick(&mut self) {
        self.follow_key_changes();
        for batch in std::mem::take(&mut self.eng.watch_batches) {
            let events = self.eng.watch.accept(batch);
            let mut touched: Vec<DocId> = Vec::new();
            for (doc, event) in events {
                if let Some(mut cx) = self.cx_of(doc) {
                    cx.watch_event(event);
                    if !touched.contains(&doc) {
                        touched.push(doc);
                    }
                }
            }
            for doc in &mut self.docs {
                if touched.contains(&doc.id) {
                    // Primes of a file gone are dropped; a file probed again near a playhead
                    // is probed first.
                    doc.primes_dirty = true;
                    doc.warnings_dirty = true;
                }
            }
        }
        self.follow_key_changes();
        let watch = self.eng.watch.watch_mut();
        watch.supervise();
        let logs = watch.take_logs();
        let changed = watch.take_notices_changed();
        let stalled: Vec<String> = watch.stalled_volumes().map(str::to_owned).collect();
        self.eng
            .pool
            .set_watch_stalled(stalled.iter().map(String::as_str));
        for (level, message) in logs {
            self.eng.log(level, message);
        }
        if changed {
            self.mark_warnings_dirty();
        }
    }

    /// The docs' prime allowances follow each other's wants: a doc holding more than it may
    /// now lets go (least urgent first, at its next pass), and one held short while another
    /// let go takes its share up. A doc's wants change only at its own passes, so an idle
    /// doc opening beside a busy one makes no doc re-prime.
    fn balance_primes(&mut self) {
        // Nothing to look at on the (usual) wake where no doc primed and none opened or
        // closed; one doc alone never has another to make room for.
        if !std::mem::take(&mut self.eng.primes_changed) || self.docs.len() < 2 {
            return;
        }
        for doc in &mut self.docs {
            let Some(usage) = self.eng.primes.get(&doc.id).copied() else {
                continue;
            };
            let allowed = self.eng.prime_allowance(doc.id);
            // Not on a mere shortfall of primes (a probe pending, a file missing): only a
            // change of allowance, so an unchanged show costs no priming passes.
            if usage.held > allowed || (usage.cap < usage.want && allowed > usage.cap) {
                doc.primes_dirty = true;
            }
        }
    }

    /// Hands every catalog's new key changes to the shared watch.
    fn follow_key_changes(&mut self) {
        for doc in &mut self.docs {
            let changes = doc.media.take_key_changes();
            self.eng.watch.apply(doc.id, changes);
        }
    }

    fn mark_warnings_dirty(&mut self) {
        for doc in &mut self.docs {
            doc.warnings_dirty = true;
        }
    }

    /// Reads the output and voice meters once when a report is due; each doc takes its part in
    /// its tick ([`DocCx::reports`]).
    fn begin_meter_round(&mut self) {
        let rendered = self.eng.mixer.samples_rendered();
        let period = (u64::from(self.eng.rate.get()) / REPORT_HZ).max(1);
        if self.eng.last_meters.is_some_and(|l| rendered < l + period) {
            return;
        }
        self.eng.last_meters = Some(rendered);
        self.eng
            .mixer
            .read_output_meters_into(&mut self.eng.out_peaks);
        let listed = self
            .eng
            .mixer
            .read_voice_meters_into(&mut self.eng.meter_ids, &mut self.eng.meter_peaks);
        let prev = self.eng.last_meter_read.replace(rendered);
        self.eng.meter_round = Some(MeterRound {
            rendered,
            prev,
            listed,
        });
    }

    /// After the docs took their meters: voice ids no doc knows any more are forgotten (their
    /// owners stay recorded only while an event of them can still mean something).
    fn end_meter_round(&mut self) {
        if self.eng.meter_round.take().is_none() {
            return;
        }
        let docs = &self.docs;
        self.eng.voice_ids.retain(|id, owner| {
            docs.iter()
                .find(|d| d.id == owner)
                .is_some_and(|d| d.voices.knows(id))
        });
    }

    /// The probe pool's watchdog: stuck workers replaced, their files reported unplayable in
    /// the doc that asked (from the next tick's `Availability`), volume notes logged once.
    fn probe_watchdog(&mut self) {
        let stuck = self.eng.pool.watchdog();
        if !stuck.is_empty() {
            let message = format!(
                "{} media probe(s) stuck; workers replaced ({} so far)",
                stuck.len(),
                self.eng.pool.abandoned_workers()
            );
            self.eng.log(LogLevel::Error, message);
            for s in stuck {
                if let Some(doc) = self.docs.iter_mut().find(|d| d.id == s.doc) {
                    let mut out = Vec::new();
                    doc.media.give_up(&s.key, s.ticket, &mut out);
                    doc.pending_media.extend(out);
                }
            }
        }
        for (volume, stalled) in self.eng.pool.take_volume_events() {
            // Not a "does not answer" warning: that notice is the media watch's alone.
            let message = if stalled {
                format!(
                    "probes of volume \"{volume}\" are held back: two files did not answer in \
                     time; one is tried at a time"
                )
            } else {
                format!("probes of volume \"{volume}\" run again: it answered")
            };
            self.eng.log(LogLevel::Info, message);
        }
        match self.eng.pool.take_worker_note() {
            Some(media::WorkerNote::SpawnFailed(e)) => self.eng.log(
                LogLevel::Error,
                format!(
                    "could not start a media probe worker ({e}); {} of {} running, retrying",
                    self.eng.pool.workers(),
                    media::PROBE_WORKERS
                ),
            ),
            Some(media::WorkerNote::Restored) => self.eng.log(
                LogLevel::Info,
                "media probe workers running again".to_owned(),
            ),
            None => {}
        }
        if self.eng.pool.take_limit_reached() {
            self.eng.log(
                LogLevel::Error,
                format!(
                    "{} probe workers were stuck and replaced; no more are replaced, so further \
                     stuck probes wait (their starts fail after the hold limit)",
                    media::MAX_ABANDONED_WORKERS
                ),
            );
        }
    }

    /// A mixer slot went back to the free list (a doc's voice ended on a retired slot, or a doc
    /// closed): docs with patches waiting for one sync again.
    fn hand_out_freed_slots(&mut self) {
        // A conflict another doc's patches made or ended shows in every doc's notices.
        if self.eng.patches.take_conflicts_changed() {
            self.mark_warnings_dirty();
        }
        if !self.eng.patches.take_freed() {
            return;
        }
        self.each_doc(|cx| {
            if !cx.eng.patches.exhausted(cx.doc.id).is_empty() {
                let now = cx.now();
                for input in cx.sync_patches(false) {
                    cx.feed(input, now);
                }
            }
        });
    }
}

/// The voice a mixer event is about (none for `PendingFull`).
fn event_voice(e: &Event) -> Option<VoiceId> {
    match e {
        Event::VoiceFinished { voice, .. }
        | Event::Underrun { voice, .. }
        | Event::VoiceRejected { voice, .. }
        | Event::LoopReached { voice, .. }
        | Event::SliceEnded { voice, .. }
        | Event::DevampScheduled { voice, .. }
        | Event::DevampIgnored { voice, .. } => Some(*voice),
        Event::PendingFull { .. } | Event::StageFaulted { .. } => None,
    }
}

/// Who a client message comes from: a sender bound to a doc (`None`: it is closed, or none is
/// open for `Inbox::Client`), or an app connection.
#[derive(Clone, Copy, Debug)]
enum Addr {
    Doc(Option<DocId>),
    App,
}

/// The answer to a workspace message with no doc to act on (none open, the one it names is
/// closed, or an app connection sent it): `no_workspace`; a shutdown still shuts down.
fn no_doc(message: ClientMessage, to: &ReplyTx) -> Flow {
    let id = match message {
        ClientMessage::Goodbye { shutdown: true } => return Flow::Shutdown,
        ClientMessage::Hello { .. } | ClientMessage::Goodbye { .. } => return Flow::Continue,
        ClientMessage::Transport { id, .. }
        | ClientMessage::Command { id, .. }
        | ClientMessage::Undo { id }
        | ClientMessage::Redo { id }
        | ClientMessage::Request { id, .. }
        | ClientMessage::Close { id, .. }
        | ClientMessage::Panic { id }
        | ClientMessage::Hotkey { id, .. } => id,
    };
    fail(to, id, ErrorCode::NoWorkspace, "no workspace is open");
    Flow::Continue
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

    /// Primed streams the whole engine may hold (ARCHITECTURE §3.5): `max_primes`, and never
    /// more than half the disk streamer's open-stream limit, which is the engine's. A start
    /// that finds no permit fails (`stream_failed`), and primes are only a head start, so the
    /// other half is always left for starts (M4.15e review M3). 64 with the defaults.
    fn prime_pool(&self) -> usize {
        self.config
            .max_primes
            .min(self.config.streamer.max_streams / 2)
    }

    /// What doc `doc` may hold of the prime pool now, by demand (review M-C): every doc's
    /// wants as of its last priming pass, shared out max-min fairly ([`share_out`]). While the
    /// wants fit the pool each doc gets all it wants, so an idle doc beside a busy show takes
    /// nothing from it; only when they do not fit is each held to its fair share.
    fn prime_allowance(&self, doc: DocId) -> usize {
        let pool = self.prime_pool();
        let wants: Vec<usize> = self.primes.values().map(|u| u.want).collect();
        let shares = share_out(pool, &wants);
        self.primes
            .keys()
            .zip(shares)
            .find(|(d, _)| **d == doc)
            .map_or(pool, |(_, s)| s)
    }

    /// Whether the docs' prime wants together exceed the pool.
    fn primes_contended(&self) -> bool {
        self.primes.values().map(|u| u.want).sum::<usize>() > self.prime_pool()
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
}

impl Doc {
    /// A doc around a workspace, its cue engine and its catalog, before anything is sent for it.
    fn new(
        eng: &Engine,
        id: DocId,
        ws: Workspace,
        engine: CueEngine,
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
            ws,
            history: History::new(),
            engine,
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
        let group_warnings = self
            .ws
            .cues()
            .filter(|c| matches!(c.kind, CueKind::Group { .. }))
            .map(|c| c.id)
            .filter(|&c| self.engine.has_broken_descendant(c))
            .collect();
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

    /// This doc's part of a wake ([`Control::tick`]).
    fn tick(&mut self) {
        let now = self.now();
        self.flush_media(now);
        let mut fx = Effects::default();
        // The mixer's events of this doc's voices, routed here by the engine this wake.
        let mut events = std::mem::take(&mut self.doc.events);
        for e in events.drain(..) {
            self.doc.voices.event(e, &mut fx);
        }
        self.doc.events = events;
        self.doc.voices.poll_streams(&mut fx, &self.doc.ws);
        let hold = seconds_to_samples(PROBE_HOLD_LIMIT.as_secs_f64(), self.eng.rate);
        self.doc.voices.expire_held(now, hold, &mut fx);
        self.follow(fx, now);
        self.reclaim_slots(now);
        let reports = self.doc.net.tick(now);
        self.network_reports(reports, now);
        self.note_lookup_limit();
        self.deliver_deferred(now);
        if self.doc.engine.is_due(now) {
            self.feed(Input::Tick, now);
        }
        if self.doc.primes_dirty {
            self.doc.primes_dirty = false;
            self.prime(now);
        }
        self.reports();
        if self.doc.warnings_dirty {
            self.doc.warnings_dirty = false;
            self.refresh_warnings();
        }
        self.docs_tick();
    }

    /// A retired patch slot whose last voice has left is freed (zeroed), and a patch waiting
    /// for a slot gets it (docs/control.md "Patch slots").
    fn reclaim_slots(&mut self, now: SampleTime) {
        let doc = self.doc.id;
        if !self.eng.patches.has_retired(doc)
            || !self
                .eng
                .patches
                .reclaimable(doc, &self.doc.voices.slots_in_use())
        {
            return;
        }
        for input in self.sync_patches(false) {
            self.feed(input, now);
        }
    }

    fn client(&mut self, message: ClientMessage, to: &ReplyTx) -> Flow {
        let now = self.now();
        match message {
            ClientMessage::Transport { id, action } => {
                self.transport(action, now);
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
                    fail(
                        to,
                        id,
                        ErrorCode::BadRequest,
                        format!("coalesce key longer than {MAX_COALESCE_KEY} bytes"),
                    );
                    return Flow::Continue;
                }
                // Before the footprint: a raw paste becomes cues with ids only here.
                let command = match self.prepare_command(*command) {
                    Ok(command) => command,
                    Err(e) => {
                        reply(to, refused(id, &e));
                        return Flow::Continue;
                    }
                };
                let footprint = command.footprint();
                match self.doc.history.execute_coalescing(
                    &mut self.doc.ws,
                    command,
                    coalesce.as_deref(),
                ) {
                    Ok(changed) => {
                        // An empty transaction, or an edit that sets what is already there,
                        // changed nothing and is no undo step: no revision, no
                        // `model_changed`, not dirty.
                        if changed {
                            self.edited(now, &footprint);
                        }
                        reply(to, ok(id, Some(self.doc.revision)));
                    }
                    Err(e) => {
                        self.partly_changed(now, &e);
                        reply(to, refused(id, &e));
                    }
                }
            }
            ClientMessage::Undo { id } | ClientMessage::Redo { id } => {
                let undo = matches!(message, ClientMessage::Undo { .. });
                // The step about to be applied says what it changes.
                let footprint = if undo {
                    self.doc.history.peek_undo()
                } else {
                    self.doc.history.peek_redo()
                }
                .map(Command::footprint)
                .unwrap_or_default();
                // A step that would stop a playing cue (an undo of its add, a redo of its
                // delete) is refused and stays on its stack (M5.10d): whoever presses undo
                // cannot predict that, and it must not cut a cue during a show.
                let engine = &self.doc.engine;
                let refuse = |ws: &Workspace| {
                    engine
                        .cue_an_edit_would_stop(ws)
                        .map(CommandError::CueIsActive)
                };
                let result = if undo {
                    self.doc.history.undo_unless(&mut self.doc.ws, refuse)
                } else {
                    self.doc.history.redo_unless(&mut self.doc.ws, refuse)
                };
                match result {
                    Ok(true) => {
                        self.edited(now, &footprint);
                        reply(to, ok(id, Some(self.doc.revision)));
                    }
                    Ok(false) => reply(to, ok(id, None)),
                    Err(CommandError::CueIsActive(cue)) => {
                        let message = self.active_cue_refusal(cue, undo);
                        self.record(
                            LogKind::Engine,
                            LogLevel::Info,
                            message.clone(),
                            Some(cue),
                            None,
                        );
                        fail(to, id, ErrorCode::CommandRefused, message);
                    }
                    Err(e) => {
                        self.partly_changed(now, &e);
                        reply(to, refused(id, &e));
                    }
                }
            }
            ClientMessage::Request { id, request } => match request {
                Request::Snapshot => {
                    reply(
                        to,
                        ServerMessage::Snapshot {
                            snapshot: Box::new(self.doc.snapshot()),
                        },
                    );
                    reply(to, ok(id, None));
                }
                Request::SetShowMode { on } => {
                    if self.doc.show_mode != on {
                        self.doc.show_mode = on;
                        self.document();
                    }
                    reply(to, ok(id, None));
                }
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
                // Saves come through [`Control::save`], which knows the other docs.
                request => self.doc_request(id, request, to, Vec::new()),
            },
            ClientMessage::Goodbye { shutdown: true } => return Flow::Shutdown,
            // The network layer handles the handshake and closing (M4.3).
            ClientMessage::Hello { .. } | ClientMessage::Goodbye { .. } => {}
            // App-scope: [`Control::app_scope`] takes them before any doc does.
            ClientMessage::Close { id, .. }
            | ClientMessage::Panic { id }
            | ClientMessage::Hotkey { id, .. } => fail(
                to,
                id,
                ErrorCode::Internal,
                "an app-scope message reached a workspace",
            ),
        }
        Flow::Continue
    }

    /// A transport action on this doc (from its client, or an app-scope panic or hotkey).
    fn transport(&mut self, action: TransportAction, now: SampleTime) {
        // Every item at this one `now`, in order, before anything else is handled: cues started
        // together start on the same sample (NFR-21). The codec refused any item `batch_cue`
        // does not accept (a nested batch among them); a batch from elsewhere is held to it too.
        if let TransportAction::Batch { actions } = action {
            for item in actions {
                if item.batch_cue().is_some() {
                    self.transport(item, now);
                }
            }
            return;
        }
        let scope = orphan_scope(&action);
        let panic = matches!(action, TransportAction::Panic);
        self.observe_transport(&action);
        if let Some(input) = transport_input(action) {
            self.feed(input, now);
        }
        // During an outage a panic fade could only play once the device is back, so it stops
        // everything at once: a second Panic within the fade is a hard stop (unless this one
        // already was).
        if panic && self.eng.backend.is_down() && self.doc.engine.panic_end().is_some() {
            self.feed(Input::Panic, now);
        }
        if let Some(scope) = scope {
            self.silence_orphans(scope, now);
        }
    }

    /// Stops voices still sounding for cues that no longer own them: a tail an underrun made
    /// longer than the action's known length, after the engine completed the action on its
    /// planned end (docs/control.md, Underrun). The engine has nothing left to stop there, so a
    /// Panic, a hard stop or a reset would leave it sounding.
    fn silence_orphans(&mut self, scope: Scope, now: SampleTime) {
        let engine = &self.doc.engine;
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
                        .doc
                        .ws
                        .find_cue(root)
                        .is_some_and(|r| descendants(r.children()).any(|c| c.id == cue))
            }
        };
        let orphans = self
            .doc
            .voices
            .orphans(|cue, run| in_scope(cue) && !owned(cue, run));
        if orphans.is_empty() {
            return;
        }
        // A Panic fades them out with everything else, ending on the panic's end; otherwise
        // (or past it) they are cut at once.
        let fade = match (scope, self.doc.engine.panic_end()) {
            (Scope::All, Some(end)) if end > now => {
                openlab_audio::Fade::new(end - now, voices::to_curve(openlab_core::PANIC_CURVE))
            }
            _ => openlab_audio::Fade::NONE,
        };
        let floor = self.stop_floor();
        let commands = orphans
            .into_iter()
            .map(|voice| {
                let at = self.doc.voices.stop_date(voice, now, floor);
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

    /// A refused command that is not "nothing changed": a transaction's rollback failed
    /// (`CommandError::RollbackFailed`, only a wrong inverse can cause it), so the workspace is
    /// valid but partly changed and history is cleared. It counts as an edit nobody can undo:
    /// logged as an error, revision bumped (dirty), the cue engine and catalog resynced, and
    /// every client sent the whole model, as for a model it cannot patch from a diff. Any other
    /// refusal does nothing here.
    fn partly_changed(&mut self, now: SampleTime, error: &CommandError) {
        if !matches!(error, CommandError::RollbackFailed { .. }) {
            return;
        }
        self.log(
            LogLevel::Error,
            format!("workspace partly changed: {error}"),
        );
        self.doc.engine.model_changed();
        self.doc.revision += 1;
        // Not `reset`: the files are the same, so their probe results stay (a cue started
        // meanwhile must not find them pending).
        self.doc.media.sync(&self.doc.ws);
        let inputs = self.sync_workspace(false);
        self.document();
        for input in inputs {
            self.feed(input, now);
        }
        self.resend_snapshot();
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
            self.doc.engine.model_changed();
        } else {
            self.doc.engine.model_changed_cues(footprint);
        }
        self.doc.revision += 1;
        // The catalog's cue index follows the edit first; the changes are read through it, at
        // the cost of the edit's cues (M4.12c). The walk is the fallback.
        self.doc.media.sync_cues(&self.doc.ws, footprint);
        let (upserts, deletes) =
            model_changes_indexed(&self.doc.ws, footprint, self.doc.media.index()).unwrap_or_else(
                || {
                    self.doc.model_change_walks += 1;
                    model_changes(&self.doc.ws, footprint)
                },
            );
        self.eng.out.send(
            Some(self.doc.id),
            ServerMessage::ModelChanged {
                revision: self.doc.revision,
                upserts,
                deletes,
            },
        );
        self.sync_workspace(false)
    }

    /// Brings patches and primes in line with the workspace (the media catalog is followed by
    /// the caller: whole for a newly opened workspace, from the footprint for an edit). `all`
    /// reports every patch resolution (a newly opened workspace), else only changes.
    fn sync_workspace(&mut self, all: bool) -> Vec<Input> {
        self.doc.primes_dirty = true;
        let mut inputs = self.sync_patches(all);
        let now = self.now();
        let reports = self.doc.net.sync(&self.doc.ws.network_patches, all, now);
        if !reports.is_empty() {
            inputs.push(Input::NetworkAvailability { patches: reports });
        }
        inputs
    }

    /// Resolves the patches against the playable devices and brings the mixer's matrices (with
    /// the workspace master folded in) and output delays up to date. Returns the
    /// `Availability` for the patches whose resolution changed (`all`: every patch).
    fn sync_patches(&mut self, all: bool) -> Vec<Input> {
        self.map_unmapped_patches();
        // A slot belongs to one doc's patch at a time, so only this doc's voices can be on its
        // slots: its own slots in use are all that hold a retired one back.
        let busy = self.doc.voices.slots_in_use();
        let (cmds, changed) = self.eng.patches.sync(
            self.doc.id,
            &self.doc.ws,
            &self.eng.config.machine,
            &self.eng.playable,
            &busy,
        );
        let now = self.now();
        // One batch (split only past the queue's size), so a master move changes every patch
        // on the same sample.
        self.send_commands(cmds.into_iter().map(|cmd| (now, cmd)).collect());
        // After the routing: a voice whose cue now names another patch follows it (#293).
        let moves = self.doc.voices.follow_patches(
            self.doc.id,
            &self.doc.ws,
            &self.eng.patches,
            &self.doc.engine,
            now,
        );
        if !moves.is_empty() {
            self.send_commands(moves);
        }
        self.doc.warnings_dirty = true;
        let patches = if all {
            self.eng.patches.resolutions(self.doc.id)
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

    /// [`Engine::stop_floor`].
    fn stop_floor(&self) -> Option<SampleTime> {
        self.eng.stop_floor()
    }

    /// This doc's part of a device loss (M4.2g, as QLab 5; [`Control::device_lost`]): the
    /// patches on it are reported missing (loaded cues on them unload, starts are refused as
    /// broken, idle cues show broken), every run with a voice on it ends at once with
    /// `ActionFailed { device_lost }` (hard stop, no fade; the voice and its stream are
    /// released, auto-follow never fires), and every voice the doc has in the mixer is stopped,
    /// so nothing sounds, or resumes, when the device returns. Returns the runs stopped.
    fn device_lost(&mut self) -> usize {
        let now = self.now();
        for input in self.sync_patches(false) {
            self.feed(input, now);
        }
        let mut runs = self.doc.voices.on_device();
        // Audio runs with no voice yet or any more (pre-wait, post-wait, paused) are routed to
        // the device too: nothing scheduled before the outage may start by itself after it.
        // They end as device_lost breaks; `Input::DeviceLost` below stops all the others.
        for (cue, state) in self.doc.engine.cue_states() {
            if matches!(
                state,
                CueState::PreWaiting
                    | CueState::Running
                    | CueState::Paused
                    | CueState::PostWaiting
                    | CueState::Fading
                    | CueState::Stopping
            ) && let Some(run) = self.doc.engine.run_of(cue)
                && matches!(
                    self.doc.ws.find_cue(cue).map(|c| &c.kind),
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
            .doc
            .voices
            .playing_ids()
            .into_iter()
            .map(|voice| {
                let at = self.doc.voices.stop_date(voice, now, floor);
                let fade = openlab_audio::Fade::NONE;
                (at, CommandKind::StopVoice { voice, fade })
            })
            .collect();
        self.send_commands(commands);
        self.doc.warnings_dirty = true;
        failed
    }

    /// One event of the shared media watch about a file of this doc ([`Control::watch_tick`]):
    /// a file gone is `Missing` in this tick's `Availability` (its cues are broken within the
    /// tick); a file back, changed or unreadable is probed again.
    fn watch_event(&mut self, event: WatchEvent) {
        match event {
            WatchEvent::Gone { key, id, .. } => {
                if let Some(status) = self.doc.media.mark_missing(&key, id) {
                    self.doc.pending_media.push(status);
                }
            }
            WatchEvent::Back { key, id, .. }
            | WatchEvent::Changed { key, id, .. }
            | WatchEvent::Unreadable { key, id, .. } => {
                self.doc.media.recheck(&key, id);
            }
        }
    }
}

impl Engine {
    /// Tells doc `doc`'s connections it closed (again on a later tick if that does not fit).
    fn announce_closed(&mut self, doc: DocId) {
        if !self
            .out
            .send_state(Some(doc), ServerMessage::Closed { doc })
        {
            self.closed_unsent.push(doc);
        }
    }

    /// The `closed` messages that did not fit, once there is room.
    fn resend_closed(&mut self) {
        while let Some(&doc) = self.closed_unsent.first() {
            if !self
                .out
                .send_state(Some(doc), ServerMessage::Closed { doc })
            {
                return;
            }
            self.closed_unsent.remove(0);
        }
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

    /// PB-10, for every doc.
    fn set_network_disabled(&mut self, disabled: bool) {
        if self.network_disabled() != disabled {
            self.net_disabled.store(disabled, Ordering::SeqCst);
            let text = if disabled {
                "network output disabled: Network cues send nothing"
            } else {
                "network output enabled"
            };
            self.log(LogLevel::Warn, text.to_owned());
        }
    }
}

impl DocCx<'_> {
    fn probe_answer(&mut self, answer: ProbeAnswer) {
        let key = match &answer {
            ProbeAnswer::Probed { key, .. } => Some(key.clone()),
            ProbeAnswer::Preloaded { .. } => None,
        };
        if let ProbeAnswer::Preloaded { key, .. } = &answer {
            // A preload that lands changes what a stream of the file reads: primes built from
            // the file before it are rebuilt from memory, before the operator's GO needs them.
            let was = self.doc.media.is_preloaded(key);
            let key = key.clone();
            self.doc.media.answer(answer);
            if self.doc.media.is_preloaded(&key) != was {
                self.doc.primes_dirty = true;
            }
            return;
        }
        if let Some(status) = self.doc.media.answer(answer) {
            self.doc.pending_media.push(status);
            if let Some(key) = key {
                self.doc.ready_media.push(key);
            }
            self.doc.warnings_dirty = true;
            self.doc.primes_dirty = true;
        }
    }

    /// Hands the probe answers gathered since the last tick to the engine in one
    /// `Availability`, then starts the voices that waited for them.
    fn flush_media(&mut self, now: SampleTime) {
        if !self.doc.pending_media.is_empty() {
            let media = std::mem::take(&mut self.doc.pending_media);
            self.feed(
                Input::Availability {
                    media,
                    patches: Vec::new(),
                },
                now,
            );
        }
        for key in std::mem::take(&mut self.doc.ready_media) {
            let mut fx = Effects::default();
            let floor = self.stop_floor();
            let mut env = Env {
                doc: self.doc.id,
                ws: &self.doc.ws,
                media: &self.doc.media,
                streamer: &mut self.eng.streamer,
                patches: &self.eng.patches,
                ids: &mut self.eng.voice_ids,
                rate: self.eng.rate,
                cue_outputs: self.eng.cue_outputs,
                now,
                floor,
            };
            self.doc.voices.media_ready(&key, &mut env, &mut fx);
            self.follow(fx, now);
        }
    }

    fn deliver_deferred(&mut self, now: SampleTime) {
        if self.doc.deferred.is_empty() {
            return;
        }
        let (due, wait): (Vec<_>, Vec<_>) = std::mem::take(&mut self.doc.deferred)
            .into_iter()
            .partition(|(at, _)| *at < now);
        self.doc.deferred = wait;
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
                self.eng.seen.feedback_overflow += 1;
                self.log(
                    LogLevel::Error,
                    format!("feedback loop cut after {FEEDBACK_LIMIT} rounds"),
                );
                break;
            }
            let actions = self.doc.engine.handle(&self.doc.ws, input, now);
            let unsignalled = self.doc.engine.unsignalled_model_changes();
            if unsignalled > self.doc.seen_unsignalled {
                self.doc.seen_unsignalled = unsignalled;
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
                doc: self.doc.id,
                ws: &self.doc.ws,
                media: &self.doc.media,
                streamer: &mut self.eng.streamer,
                patches: &self.eng.patches,
                ids: &mut self.eng.voice_ids,
                rate: self.eng.rate,
                cue_outputs: self.eng.cue_outputs,
                now,
                floor,
            };
            for action in actions {
                if let Some(change) = RuntimeChange::from_action(action, &self.doc.engine) {
                    changes.push(change);
                }
                if matches!(action, Action::PlayheadChanged { .. }) {
                    self.doc.primes_dirty = true;
                }
                self.doc.voices.apply(action, &mut env, &mut fx);
            }
        }
        for action in actions {
            match action {
                Action::SetArmed { cue, armed: value }
                    if set_armed(&mut self.doc.ws, *cue, *value) =>
                {
                    armed.push(*cue);
                }
                Action::SendOsc {
                    cue,
                    destination,
                    address,
                    args,
                    ..
                } => self.send_osc(*cue, *destination, address, args),
                _ => {}
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

    /// A Network cue's turn (CT-30): encodes the message and hands it to its destination's
    /// worker. Whatever goes wrong is a warning; the cue completes regardless (QLab: a Network
    /// cue does not wait for, or learn of, delivery).
    fn send_osc(
        &mut self,
        cue: CueId,
        destination: NetworkPatchId,
        address: &str,
        args: &[OscArg],
    ) {
        let Some(patch) = self
            .doc
            .ws
            .network_patches
            .iter()
            .find(|p| p.id == destination)
            .cloned()
        else {
            // Cannot happen: a cue without its patch has a problem and does not send.
            self.net_warning(cue, "network patch missing".to_owned());
            return;
        };
        if self.doc.net.disabled() {
            let label = self.cue_label(cue);
            self.record(
                LogKind::Engine,
                LogLevel::Info,
                format!(
                    "{label}: network output is disabled, {address} to {} not sent",
                    patch.name
                ),
                Some(cue),
                None,
            );
            return;
        }
        let packet = match netout::packet(address, args, patch.protocol) {
            Ok(p) => p,
            Err(e) => return self.net_warning(cue, format!("message not sent: {e}")),
        };
        match self.doc.net.send(&patch, cue, packet) {
            SendOutcome::Queued | SendOutcome::Suppressed => {}
            SendOutcome::Refused(why) => {
                self.net_warning(cue, format!("message to {} not sent: {why}", patch.name));
            }
        }
    }

    /// A worker's answer: a failed send is a warning; a look-up goes to the cue engine.
    fn net_answer(&mut self, answer: NetAnswer) {
        let now = self.now();
        match answer {
            NetAnswer::SendFailed {
                cue,
                ref destination,
                ref reason,
                ..
            } => {
                let message = format!("message to {destination} not sent: {reason}");
                // A TCP failure also asks for a fresh look-up of the host.
                let reports = self.doc.net.answer(answer, now);
                self.network_reports(reports, now);
                self.net_warning(cue, message);
            }
            lookup @ NetAnswer::Lookup { .. } => {
                let reports = self.doc.net.answer(lookup, now);
                self.network_reports(reports, now);
            }
        }
        self.note_lookup_limit();
    }

    fn note_lookup_limit(&mut self) {
        for note in self.doc.net.take_notes() {
            self.log(LogLevel::Warn, note);
        }
        if self.doc.net.take_limit_note() {
            self.log(
                LogLevel::Warn,
                "host look-ups are stuck; names waiting for one are taken to be reachable until one answers"
                    .to_owned(),
            );
        }
    }

    fn network_reports(&mut self, patches: netout::Reports, now: SampleTime) {
        if !patches.is_empty() {
            self.feed(Input::NetworkAvailability { patches }, now);
        }
    }

    /// A show-time notice (`network_send_failed`) for a Network cue, logged and sent to clients.
    fn net_warning(&mut self, cue: CueId, message: String) {
        let changes = vec![RuntimeChange::Warning {
            warning: notice("network_send_failed", message, Some(cue)),
        }];
        self.observe_changes(&changes);
        self.runtime(changes);
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
        self.doc.deferred.extend(deferred);
        for key in reprobe {
            self.doc.media.reprobe(&key);
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

    /// Keeps the cues near each playhead primed (ARCHITECTURE §3.5).
    fn prime(&mut self, now: SampleTime) {
        let wanted = prime_set(&self.doc.ws, &self.doc.engine, self.eng.config.prime_cues);
        // Their files are probed first (H2: a restart mid-show must not leave the cue at the
        // playhead behind a hundred others).
        let keys: Vec<String> = wanted
            .iter()
            .filter_map(|c| match &self.doc.ws.find_cue(*c)?.kind {
                CueKind::Audio { media: Some(m), .. } => Some(m.as_str().to_owned()),
                _ => None,
            })
            .collect();
        self.doc.media.prioritize(keys.iter().map(String::as_str));
        let floor = self.stop_floor();
        let want = wanted.len().min(self.eng.config.max_primes);
        let id = self.doc.id;
        let entry = self.eng.primes.entry(id).or_default();
        let before = (entry.want, entry.held, entry.cap);
        entry.want = want;
        let cap = self.eng.prime_allowance(id);
        // Fills only a few new rings per pass while docs share a short pool.
        let burst = if self.eng.primes_contended() {
            PRIME_BURST
        } else {
            usize::MAX
        };
        let mut env = Env {
            doc: self.doc.id,
            ws: &self.doc.ws,
            media: &self.doc.media,
            streamer: &mut self.eng.streamer,
            patches: &self.eng.patches,
            ids: &mut self.eng.voice_ids,
            rate: self.eng.rate,
            cue_outputs: self.eng.cue_outputs,
            now,
            floor,
        };
        let cut = self.doc.voices.prime(&wanted, cap, burst, &mut env);
        let held = self.doc.voices.primed().count();
        let entry = self.eng.primes.entry(id).or_default();
        entry.held = held;
        entry.cap = cap;
        if before != (want, held, cap) {
            self.eng.primes_changed = true;
        }
        if cut {
            // The rest of the burst-limited fill comes on the next passes.
            self.doc.primes_dirty = true;
        }
    }

    /// Progress and meters, each at most [`REPORT_HZ`] in sample time; reconciliation of lost
    /// voice ends rides on the voice-meter read.
    fn reports(&mut self) {
        let rendered = self.eng.mixer.samples_rendered();
        let period = (u64::from(self.eng.rate.get()) / REPORT_HZ).max(1);
        let due = |last: Option<SampleTime>| last.is_none_or(|l| rendered >= l + period);
        if due(self.doc.last_progress) {
            let mut items: Vec<ProgressItem> = self
                .doc
                .voices
                .progress(rendered)
                .into_iter()
                .map(|(cue, pos, left)| ProgressItem {
                    cue,
                    elapsed: self.seconds(pos),
                    remaining: left.map(|l| self.seconds(l)),
                })
                .collect();
            // Cues without a voice: their actions join the voices' (an audio cue's action is
            // never among them), their pre-waits and post-waits go in `waits` (M4.2e).
            let mut waits: Vec<WaitProgress> = Vec::new();
            let voiced = items.len();
            for t in self.doc.engine.timers(rendered) {
                let elapsed = self.seconds(t.elapsed);
                let wait = match t.timer {
                    Timer::Action => {
                        items.push(ProgressItem {
                            cue: t.cue,
                            elapsed,
                            remaining: t.remaining.map(|l| self.seconds(l)),
                        });
                        continue;
                    }
                    Timer::PreWait => WireWait::PreWait,
                    Timer::PostWait => WireWait::PostWait,
                };
                waits.push(WaitProgress {
                    cue: t.cue,
                    wait,
                    elapsed,
                    remaining: self.seconds(t.remaining.unwrap_or(0)),
                });
            }
            if items.len() > voiced {
                items.sort_by_key(|i| i.cue);
            }
            if !items.is_empty() || !waits.is_empty() {
                self.doc.last_progress = Some(rendered);
                let mut m = ServerMessage::Progress { items, waits };
                m.sanitise();
                self.eng.out.send_lossy(Some(self.doc.id), m);
            }
        }
        // The engine read the meters once this wake ([`Control::begin_meter_round`]): this
        // doc's voices and patches are taken from it.
        if let Some(round) = self.eng.meter_round {
            self.meters(round);
        }
    }

    fn seconds(&self, frames: u64) -> f64 {
        frames as f64 / f64::from(self.eng.rate.get())
    }

    /// This doc's part of the wake's meter read: its cues' peaks, its patches' outputs, and
    /// the reconciliation of its voices against the voices listed (ids are unique engine-wide,
    /// so another doc's listed voice is never taken for one of this doc's).
    fn meters(&mut self, round: MeterRound) {
        let MeterRound {
            rendered,
            prev,
            listed: n,
        } = round;
        let gone = self.doc.voices.take_gone();
        let outs = self.eng.cue_outputs.max(1);
        let mut cues: Vec<CueMeter> = Vec::new();
        let mut listed = Vec::with_capacity(n);
        for (k, id) in self.eng.meter_ids.iter().take(n).enumerate() {
            listed.push(*id);
            let cue = self
                .doc
                .voices
                .cue_of(*id)
                .or_else(|| gone.iter().find(|(g, _)| g == id).map(|(_, c)| *c));
            let Some(cue) = cue else { continue };
            let peaks = self
                .eng
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
        self.reconcile(&listed, rendered, prev);
        let outputs: Vec<OutputMeter> = self
            .doc
            .ws
            .patches
            .iter()
            .filter_map(|p| {
                let channels = self.eng.patches.channels(self.doc.id, p.id)?;
                let peaks = channels
                    .iter()
                    .map(|&c| {
                        usize::try_from(c)
                            .ok()
                            .and_then(|c| self.eng.out_peaks.get(c))
                            .copied()
                            .unwrap_or(0.0)
                    })
                    .collect();
                Some(OutputMeter { patch: p.id, peaks })
            })
            .collect();
        let live = self.doc.voices.has_playing() || !cues.is_empty();
        // Only while audio plays, plus one message after, so meters fall back to silence.
        if live || self.doc.meters_live {
            self.doc.meters_live = live;
            let mut m = ServerMessage::Meters { outputs, cues };
            m.sanitise();
            self.eng.out.send_lossy(Some(self.doc.id), m);
        }
    }

    /// A started voice the mixer has not listed in two meter reads in a row, while mixer
    /// events have been lost, has ended without its report reaching us: report it ended so its
    /// cue does not run forever (docs/control.md, Reconciliation).
    fn reconcile(&mut self, listed: &[VoiceId], rendered: SampleTime, prev: Option<SampleTime>) {
        let Some(prev) = prev else {
            return;
        };
        let lost_events = self.eng.mixer.dropped_events() > 0;
        let started: Vec<(VoiceId, SampleTime)> = self.doc.voices.started().collect();
        let mut fx = Effects::default();
        for (id, start) in started {
            if start >= prev {
                continue;
            }
            let absent = self.doc.voices.mark_absent(id, !listed.contains(&id));
            if absent >= 2 && lost_events {
                self.doc.voices.lost(id, rendered, &mut fx);
            }
        }
        let now = self.now();
        self.follow(fx, now);
    }

    /// This doc's state its clients missed goes again, newest, once there is room
    /// ([`Control::health`]).
    fn resend_state(&mut self) {
        if self.doc.resend_document && self.eng.out.has_room() {
            self.document();
        }
        if self.doc.resend_warnings && self.eng.out.has_room() {
            self.doc.resend_warnings = !self.eng.out.send_state(
                Some(self.doc.id),
                ServerMessage::Warnings {
                    warnings: self.doc.warnings.clone(),
                },
            );
        }
    }

    /// The workspace-level notices in force: damaged media, preloads over budget, cues whose
    /// slices cannot play, audio-thread allocations. Sent when the list changes.
    fn refresh_warnings(&mut self) {
        let mut list = self.doc.file.notices.clone();
        let mut damaged: Vec<(&str, usize)> = self.doc.media.damaged().collect();
        damaged.sort_unstable();
        for (path, n) in damaged {
            list.push(notice(
                "damaged_media",
                format!("{path}: {n} damaged part(s); they play as silence"),
                None,
            ));
        }
        let mut over: Vec<&str> = self.doc.media.over_budget().collect();
        over.sort_unstable();
        for path in over {
            list.push(notice(
                "preload_over_budget",
                format!("{path} does not fit the preload memory budget and is streamed"),
                None,
            ));
        }
        let mut failed: Vec<&str> = self.doc.media.preload_failed().collect();
        failed.sort_unstable();
        for path in failed {
            list.push(notice(
                "preload_failed",
                format!("{path} could not be loaded into memory and is streamed"),
                None,
            ));
        }
        for (cue, e) in self
            .doc
            .checks
            .check(&self.doc.ws, &mut self.doc.media, self.eng.rate)
        {
            list.push(notice(
                "bad_plan",
                format!("the cue's slices cannot be played: {e}"),
                Some(cue),
            ));
        }
        self.device_warnings(&mut list);
        self.eng.watch.watch().notices(&mut list);
        if self.eng.seen.alloc_violations > 0 {
            list.push(notice(
                "realtime_allocation",
                format!(
                    "the audio thread allocated memory {} time(s): report this bug",
                    self.eng.seen.alloc_violations
                ),
                None,
            ));
        }
        if list != self.doc.warnings {
            self.doc.warnings = list.clone();
            self.doc.resend_warnings = !self.eng.out.send_state(
                Some(self.doc.id),
                ServerMessage::Warnings { warnings: list },
            );
        }
        // The plans just checked also give the cues' nominal lengths, which a playlist halted by
        // a broken child times its auto-follow by (docs/cue-engine.md, "Playlist"), and the
        // media lengths clients show (M5.4d): only the cues whose plan changed.
        let changes = self.doc.checks.take_changes();
        if !changes.media.is_empty() {
            self.runtime(
                changes
                    .media
                    .into_iter()
                    .map(RuntimeChange::MediaDuration)
                    .collect(),
            );
        }
        if !changes.lengths.is_empty() {
            let now = self.now();
            self.feed(
                Input::CueLengths {
                    lengths: changes.lengths,
                },
                now,
            );
        }
    }

    /// The device's notices: an outage, a mixer that could not be recovered, patches that map
    /// more device channels than the device has (`outputs_dropped`), patches with no free mixer
    /// slot (`mixer_patches_exhausted`), device outputs that patches give different delays.
    fn device_warnings(&self, list: &mut Vec<WireWarning>) {
        self.eng.stage_faults.warnings(list);
        if let Some(d) = self.eng.backend.device() {
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
                        self.eng.cue_outputs
                    ),
                    None,
                ));
            }
        }
        for patch in &self.doc.ws.patches {
            if let Some(PatchResolution::ChannelsMissing {
                device,
                needed,
                available,
            }) = self.eng.patches.resolution(self.doc.id, patch.id)
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
        for &id in self.eng.patches.exhausted(self.doc.id) {
            let name = self
                .doc
                .ws
                .patches
                .iter()
                .find(|p| p.id == id)
                .map_or("", |p| p.name.as_str());
            list.push(notice(
                "mixer_patches_exhausted",
                format!(
                    "patch \"{name}\" cannot play: all {} mixer patches are in use",
                    self.eng.patches.capacity()
                ),
                None,
            ));
        }
        for output in self.eng.patches.delay_conflicts(self.doc.id) {
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
}

impl Control {
    fn poll_device(&mut self) {
        let events = match &mut self.eng.backend {
            Backend::Device(d) => d.poll(),
            Backend::Offline => return,
        };
        self.device_events(events);
    }

    fn device_answer(&mut self, answer: DeviceAnswer) {
        let events = match &mut self.eng.backend {
            Backend::Device(d) => d.answer(answer),
            Backend::Offline => return,
        };
        self.device_events(events);
    }

    /// The device policy (docs/control.md "Devices", PROGRESS M4.2g): one device for every
    /// doc, so a loss stops every doc and a return makes every doc playable again.
    fn device_events(&mut self, events: Vec<DeviceEvent>) {
        let name = self
            .eng
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
                DeviceEvent::ListChanged => self.each_doc(|cx| {
                    let now = cx.now();
                    for input in cx.sync_patches(false) {
                        cx.feed(input, now);
                    }
                }),
                DeviceEvent::OpenFailed {
                    attempt,
                    error,
                    retry_in,
                } => self.eng.log_device(
                    LogLevel::Warn,
                    format!(
                        "audio device \"{name}\": reopen attempt {attempt} failed ({error}); \
                         next in {:.2} s",
                        retry_in.as_secs_f64()
                    ),
                ),
                DeviceEvent::Warn(message) => self.eng.log_device(LogLevel::Warn, message),
                DeviceEvent::Stalled { ms, log } => {
                    if let Some(earlier) = log {
                        let more = if earlier > 0 {
                            format!(" ({earlier} more stall(s) since the last notice)")
                        } else {
                            String::new()
                        };
                        self.eng.log_device(
                            LogLevel::Warn,
                            format!(
                                "audio device \"{name}\": no audio callback for {ms} ms; the \
                                 device is treated as lost if it stays silent{more}"
                            ),
                        );
                    }
                    self.mark_warnings_dirty();
                }
                DeviceEvent::StallCleared { log } => {
                    if log {
                        self.eng.log_device(
                            LogLevel::Info,
                            format!("audio device \"{name}\": audio callbacks resumed"),
                        );
                    }
                    self.mark_warnings_dirty();
                }
                DeviceEvent::Unrecoverable { why } => {
                    self.eng.log_device(
                        LogLevel::Error,
                        format!("audio device \"{name}\": {why}: restart openLab to play sound"),
                    );
                    self.mark_warnings_dirty();
                }
                DeviceEvent::Unstuck => {
                    self.eng.log_device(
                        LogLevel::Info,
                        format!("audio device \"{name}\": the device thread answers again"),
                    );
                    self.mark_warnings_dirty();
                }
                DeviceEvent::ListStuck => {
                    self.eng.log_device(
                        LogLevel::Error,
                        "reading the output device list does not finish: hot-plug is not \
                         followed until it does"
                            .to_owned(),
                    );
                    self.mark_warnings_dirty();
                }
            }
        }
    }

    /// The device went away (M4.2g, as QLab 5): every doc's cues on it stop at once
    /// ([`DocCx::device_lost`]); the loss is logged once, and each doc's clients get a
    /// `device_lost` notice with its own count.
    fn device_lost(&mut self, name: &str, why: &str) {
        self.eng.playable.clear();
        let mut failed = Vec::with_capacity(self.docs.len());
        self.each_doc(|cx| failed.push(cx.device_lost()));
        let total: usize = failed.iter().sum();
        let text = |n: usize| {
            format!(
                "audio device \"{name}\" lost ({why}): {n} cue(s) playing on it stopped; \
                 they start again from the beginning with GO once the device is back"
            )
        };
        self.eng.log_device(LogLevel::Error, text(total));
        let mut counts = failed.into_iter();
        self.each_doc(|cx| {
            let n = counts.next().unwrap_or(0);
            cx.runtime(vec![RuntimeChange::Warning {
                warning: notice("device_lost", text(n), None),
            }]);
        });
    }

    /// The stream was reopened: patches resolve against the device again (cues that failed
    /// with `device_lost`, and idle cues on it, become playable) in every doc.
    fn device_restored(&mut self, name: &str) {
        let Some(device) = self.eng.backend.device() else {
            return;
        };
        let block = device.info().block_frames;
        self.eng.playable = vec![playable_device(device)];
        // Another buffer size: the lookahead and `now` follow it, so every deadline is still
        // committed before the callback that renders it (commands already sent keep their
        // samples). Every doc's cue engine takes the new lookahead before the engine's block
        // changes (seams note (b)); if one cannot, they all keep the old one.
        if let Some(b) = block
            .map(u64::from)
            .filter(|&b| b > 0 && b != self.eng.block)
        {
            let old = self.eng.block;
            let mut failed = None;
            for doc in &mut self.docs {
                if let Err(e) = doc.engine.set_lookahead(b) {
                    failed = Some(e);
                    break;
                }
            }
            match failed {
                None => {
                    self.eng.now_floor = self.eng.now();
                    self.eng.log_device(
                        LogLevel::Info,
                        format!(
                            "audio device \"{name}\" reopened with {b}-frame buffers (was {old})"
                        ),
                    );
                    self.eng.block = b;
                }
                Some(e) => {
                    for doc in &mut self.docs {
                        // The old lookahead was accepted before: this cannot fail.
                        let _ = doc.engine.set_lookahead(old);
                    }
                    self.eng.log_device(
                        LogLevel::Warn,
                        format!("audio device \"{name}\": {e}; commands may land late"),
                    );
                }
            }
        }
        self.each_doc(|cx| {
            let now = cx.now();
            for input in cx.sync_patches(false) {
                cx.feed(input, now);
            }
        });
        self.eng
            .log_device(LogLevel::Info, format!("audio device \"{name}\" is back"));
        self.mark_warnings_dirty();
    }

    /// Reports counters that must never rise (lost events, mixer faults, allocations on the
    /// audio thread (M3.21), dropped outbound messages) once for the engine, and lets each doc
    /// send again what its clients missed.
    fn health(&mut self) {
        let eng = &mut self.eng;
        let dropped = eng.mixer.dropped_events();
        if dropped > eng.seen.dropped_events {
            eng.seen.dropped_events = dropped;
            eng.log(
                LogLevel::Warn,
                format!("{dropped} mixer event(s) lost; voice ends are reconciled from meters"),
            );
        }
        let faults = eng.mixer.faults();
        if faults > eng.seen.faults {
            eng.seen.faults = faults;
            eng.log(
                LogLevel::Error,
                format!("audio engine fault: {faults} block(s) replaced with silence"),
            );
        }
        let faulted = eng.mixer.faulted_voices();
        if faulted > eng.seen.faulted_voices {
            eng.seen.faulted_voices = faulted;
            eng.log(
                LogLevel::Error,
                format!("audio engine fault: {faulted} voice(s) dropped after repeated faults"),
            );
        }
        // Reports counted at the previous check have been drained by now (the mixer counts a
        // fault just before it queues the report). Each report is logged as it is handled.
        let counted_before =
            std::mem::replace(&mut eng.seen.faulted_stages, eng.mixer.faulted_stages());
        let disabled_now = eng.mixer.disabled_units();
        let disabled_before = std::mem::replace(&mut eng.seen.disabled_units, disabled_now);
        self.lost_stage_reports(counted_before);
        self.lost_disabled_reports(disabled_before, disabled_now);
        let eng = &mut self.eng;
        let allocs = eng.backend.alloc_violations();
        if allocs > eng.seen.alloc_violations {
            eng.seen.alloc_violations = allocs;
            eng.log(
                LogLevel::Error,
                format!("real-time violation: {allocs} allocation(s) on the audio thread"),
            );
            self.mark_warnings_dirty();
        }
        self.each_doc(|cx| cx.resend_state());
        let eng = &mut self.eng;
        let out = eng.out.dropped() + eng.out.lossy_dropped();
        if out > eng.seen.outbound_dropped && eng.out.has_room() {
            let (seq, lossy) = (eng.out.dropped(), eng.out.lossy_dropped());
            eng.seen.outbound_dropped = out;
            eng.log(
                LogLevel::Warn,
                format!(
                    "clients were not keeping up: {seq} message(s) and {lossy} meter/progress \
                     update(s) dropped so far"
                ),
            );
        }
    }

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

    /// App-scope Panic (Esc; ARCHITECTURE §6): every open doc panics, as a `panic` transport in
    /// each would (twice within the fade is a hard stop in each).
    pub fn panic_all(&mut self) {
        self.each_doc(|cx| {
            let now = cx.now();
            cx.transport(TransportAction::Panic, now);
        });
        self.eng.mixer.pump();
    }

    /// App-scope hotkey (ARCHITECTURE §6; QLab 5 fires a hotkey in every open workspace, in
    /// front or not): every cue of every doc with this hotkey starts, as the app's `start`
    /// for each would. A doc whose transport keys use it starts nothing (the workspace action
    /// wins, M2.28); that action is the front window's, not the engine's.
    pub fn hotkey(&mut self, hotkey: &openlab_core::Hotkey) {
        self.each_doc(|cx| {
            let now = cx.now();
            for cue in cx.doc.ws.cues_with_hotkey(hotkey) {
                cx.transport(TransportAction::Start { cue }, now);
            }
        });
        self.eng.mixer.pump();
    }

    /// App-scope hotkey release: the release (`trigger_released`) of every cue of every doc
    /// with this hotkey (a cue whose second trigger is not on release ignores it).
    pub fn hotkey_released(&mut self, hotkey: &openlab_core::Hotkey) {
        self.each_doc(|cx| {
            let now = cx.now();
            for cue in cx.doc.ws.cues_with_hotkey(hotkey) {
                cx.transport(TransportAction::TriggerReleased { cue }, now);
            }
        });
        self.eng.mixer.pump();
    }

    /// The open docs as the app connections list them, in open order.
    pub fn workspace_list(&self) -> Vec<WorkspaceInfo> {
        self.docs
            .iter()
            .map(|d| WorkspaceInfo {
                doc: d.id,
                workspace_id: d.ws.id,
                name: d.ws.name.clone(),
                path: d
                    .show_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
                dirty: d.is_dirty(),
            })
            .collect()
    }

    /// Sends the `workspaces` list to the app connections when a doc opened or closed, or one's
    /// name, path or unsaved state changed since it was last sent (or that send did not fit).
    fn publish_workspaces(&mut self) {
        let same = self.eng.listed.len() == self.docs.len()
            && self
                .eng
                .listed
                .iter()
                .zip(&self.docs)
                .all(|(l, d)| l.lists(d));
        if same && !self.eng.list_unsent {
            return;
        }
        self.eng.listed = self.docs.iter().map(Listed::of).collect();
        let workspaces = self.workspace_list();
        self.eng.list_unsent = !self
            .eng
            .out
            .send_state(None, ServerMessage::Workspaces { workspaces });
    }

    /// The ids of the open docs, in open order.
    pub fn doc_ids(&self) -> Vec<DocId> {
        self.docs.iter().map(|d| d.id).collect()
    }

    /// A read-only view of doc `id`, if it is open.
    pub fn doc(&self, id: DocId) -> Option<DocView<'_>> {
        self.docs
            .iter()
            .find(|d| d.id == id)
            .map(|doc| DocView { doc })
    }

    /// The docs [`Self::open_doc`] installed since the last call, with the request each
    /// answered (the protocol's `opened { doc }` is M4.15f).
    pub fn take_opened(&mut self) -> Vec<(openlab_core::protocol::RequestId, DocId)> {
        std::mem::take(&mut self.eng.opened)
    }

    /// Mixer patch slots no doc holds.
    pub fn free_mixer_patches(&self) -> usize {
        self.eng.patches.free_slots()
    }

    /// Probe workers running: one pool for the engine, whatever the number of docs.
    pub fn probe_workers(&self) -> usize {
        self.eng.pool.workers()
    }

    /// Files the media watch stats (a file several docs use counts once).
    pub fn watched_files(&self) -> usize {
        self.eng.watch.watched_files()
    }

    /// Voice ids whose owner is recorded (a leak measure: it follows the voices alive).
    pub fn voice_owners(&self) -> usize {
        self.eng.voice_ids.recorded()
    }

    /// The first open doc: what the single-workspace accessors below show.
    fn first(&self) -> Option<&Doc> {
        self.docs.first()
    }

    /// Everything a client needs to draw the show (docs/protocol.md, Snapshot), of the first
    /// open doc (an empty workspace when none is open).
    pub fn snapshot(&self) -> Snapshot {
        match self.first() {
            Some(doc) => doc.snapshot(),
            None => Snapshot {
                revision: 0,
                runtime_seq: 0,
                document: DocumentState {
                    path: None,
                    dirty: false,
                    can_undo: false,
                    can_redo: false,
                    show_mode: false,
                },
                workspace: self.none.ws.clone(),
                cue_states: Vec::new(),
                problems: Vec::new(),
                group_warnings: Vec::new(),
                playheads: Vec::new(),
                previewing: Vec::new(),
                warnings: Vec::new(),
                media_durations: Vec::new(),
            },
        }
    }

    /// One media-watch pass on this thread, in manual mode (tests): its batch, if any,
    /// to hand to [`Self::handle`] as `Inbox::Watch`. `None` in thread mode or when the pass
    /// found nothing.
    pub fn watch_pass(&mut self) -> Option<WatchBatch> {
        self.follow_key_changes();
        self.eng.watch.watch_mut().manual_pass()
    }

    /// Watch batches of the current generation taken so far (a cost measure).
    pub fn watch_batches(&self) -> u64 {
        self.eng.watch.watch().batches()
    }

    /// Media watchers abandoned in a stalled stat.
    pub fn abandoned_watchers(&self) -> usize {
        self.eng.watch.watch().abandoned()
    }

    /// Slice plans built for the `bad_plan` warnings so far (a cost measure: an edit rebuilds
    /// only the cues it changed), in the first doc.
    pub fn plan_builds(&self) -> u64 {
        self.first().map_or(0, |d| d.checks.builds())
    }

    /// Cues the `bad_plan` checks have looked at so far (a cost measure: an edit looks at the
    /// cues it touched, M4.12c), in the first doc.
    pub fn plan_cues_examined(&self) -> u64 {
        self.first().map_or(0, |d| d.checks.examined())
    }

    /// Walks of every audio cue the `bad_plan` checks made so far (a newly followed workspace;
    /// an edit or a probe answer makes none, M4.12c), in the first doc.
    pub fn plan_walks(&self) -> u64 {
        self.first().map_or(0, |d| d.checks.walks())
    }

    /// Edits whose `model_changed` walked the workspace instead of reading the cue index (a
    /// cost measure; none for a valid workspace, M4.12c), in the first doc.
    pub fn model_change_walks(&self) -> u64 {
        self.first().map_or(0, |d| d.model_change_walks)
    }

    /// Network look-ups under way plus messages queued or being sent, every doc's (the offline
    /// driver waits for zero).
    pub fn net_busy(&self) -> usize {
        self.docs.iter().map(|d| d.net.busy()).sum()
    }

    /// PB-10: whether the network output override is on (one flag for every doc).
    pub fn network_output_disabled(&self) -> bool {
        self.eng.network_disabled()
    }

    /// The first open doc's workspace.
    pub fn workspace(&self) -> &Workspace {
        self.first().map_or(&self.none.ws, |d| &d.ws)
    }

    /// The first open doc's cue engine.
    pub fn engine(&self) -> &CueEngine {
        self.first().map_or(&self.none.engine, |d| &d.engine)
    }

    /// The machine settings in force, including mappings the engine added (M4.4c).
    pub fn machine(&self) -> &MachineSettings {
        &self.eng.config.machine
    }

    pub fn mixer(&self) -> &MixerController {
        &self.eng.mixer
    }

    /// The first open doc's revision.
    pub fn revision(&self) -> u64 {
        self.first().map_or(0, |d| d.revision)
    }

    /// Voice records alive in every doc (none once a show has stopped and every voice has
    /// ended).
    pub fn live_voices(&self) -> usize {
        self.docs.iter().map(|d| d.voices.live()).sum()
    }

    /// Stream handles the loop holds (voices, loads and primes), every doc's.
    pub fn held_streams(&self) -> usize {
        self.docs.iter().map(|d| d.voices.streams()).sum()
    }

    /// Streams the disk workers have open.
    pub fn open_streams(&self) -> usize {
        self.eng.streamer.open_streams()
    }

    /// Starts that used a primed stream (not a cold start), in the first doc.
    pub fn prime_hits(&self) -> u64 {
        self.first().map_or(0, |d| d.voices.prime_hits())
    }

    /// Whether the first doc's primed stream of the cue reads preloaded samples.
    pub fn primed_from_memory(&self, cue: CueId) -> bool {
        self.first()
            .is_some_and(|d| d.voices.primed_from_memory(cue))
    }

    /// Cues of the first doc with a primed stream.
    pub fn primed(&self) -> Vec<CueId> {
        let mut v: Vec<CueId> = self
            .first()
            .map(|d| d.voices.primed().collect())
            .unwrap_or_default();
        v.sort();
        v
    }

    /// Probes still running, every doc's.
    pub fn probes_pending(&self) -> usize {
        self.docs.iter().map(|d| d.media.pending()).sum()
    }

    /// Preload jobs started and not answered yet, every doc's.
    pub fn preloads_pending(&self) -> usize {
        self.docs.iter().map(|d| d.media.preloads_pending()).sum()
    }

    /// Probe workers replaced because they were stuck (the engine's pool).
    pub fn abandoned_probe_workers(&self) -> usize {
        self.eng.pool.abandoned_workers()
    }

    /// Cues the first doc's media catalog has read from the workspace (see
    /// `MediaCatalog::cues_read`).
    pub fn media_cues_read(&self) -> u64 {
        self.first().map_or(0, |d| d.media.cues_read())
    }

    /// Memory taken from the first doc's preload budget.
    pub fn preload_reserved(&self) -> u64 {
        self.first().map_or(0, |d| d.media.preload_reserved())
    }

    /// Device-thread jobs not answered yet (a test driver waits for them; none offline).
    pub fn device_jobs_pending(&self) -> usize {
        self.eng.backend.device().map_or(0, Device::jobs_in_flight)
    }

    /// The device is down: an outage, sample time standing still.
    pub fn device_down(&self) -> bool {
        self.eng.backend.is_down()
    }

    /// A device list read is under way (a test driver waits for it).
    pub fn device_listing(&self) -> bool {
        self.eng.backend.device().is_some_and(Device::is_listing)
    }

    /// Frames per block the loop schedules with (follows the open stream).
    pub fn block_frames(&self) -> u64 {
        self.eng.block
    }

    /// Feedback waiting for its sample, every doc's.
    pub fn deferred(&self) -> usize {
        self.docs.iter().map(|d| d.deferred.len()).sum()
    }

    /// Voices the mixer is playing or holding paused, from a voice-meter read (diagnostics and
    /// leak tests; the read takes the peaks the next meters message would have had).
    pub fn mixer_voice_count(&mut self) -> usize {
        self.eng
            .mixer
            .read_voice_meters_into(&mut self.eng.meter_ids, &mut self.eng.meter_peaks)
    }

    /// Playing streams of every doc and the frames each must have written before rendered
    /// sample `t` can be rendered without an underrun (offline driver).
    pub fn stream_needs(&self, t: SampleTime) -> Vec<(&openlab_audio::StreamHandle, u64)> {
        self.docs
            .iter()
            .flat_map(|d| d.voices.stream_needs(t))
            .collect()
    }

    /// Streams prepared and not playing yet, every doc's: loads and primes (offline driver).
    pub fn prepared_streams(&self) -> Vec<&openlab_audio::StreamHandle> {
        self.docs
            .iter()
            .flat_map(|d| d.voices.waiting_streams())
            .collect()
    }

    /// No voice, alias or deferred report is left in any doc.
    pub fn is_idle(&self) -> bool {
        self.docs
            .iter()
            .all(|d| d.voices.is_idle() && d.deferred.is_empty())
    }
}

/// What tests (and later the protocol's bound connections) can read of one open doc.
pub struct DocView<'a> {
    doc: &'a Doc,
}

impl<'a> DocView<'a> {
    pub fn id(&self) -> DocId {
        self.doc.id
    }

    pub fn workspace(&self) -> &'a Workspace {
        &self.doc.ws
    }

    pub fn engine(&self) -> &'a CueEngine {
        &self.doc.engine
    }

    pub fn snapshot(&self) -> Snapshot {
        self.doc.snapshot()
    }

    pub fn revision(&self) -> u64 {
        self.doc.revision
    }

    pub fn runtime_seq(&self) -> u64 {
        self.doc.runtime_seq
    }

    pub fn is_unsaved(&self) -> bool {
        self.doc.revision != self.doc.saved_revision
    }

    pub fn show_path(&self) -> Option<&'a std::path::Path> {
        self.doc.show_path.as_deref()
    }

    /// Voice records alive.
    pub fn live_voices(&self) -> usize {
        self.doc.voices.live()
    }

    /// Mixer events routed to this doc so far.
    pub fn events_routed(&self) -> u64 {
        self.doc.events_routed
    }

    /// No voice, alias or deferred report is left.
    pub fn is_idle(&self) -> bool {
        self.doc.voices.is_idle() && self.doc.deferred.is_empty()
    }

    /// Probes still running.
    pub fn probes_pending(&self) -> usize {
        self.doc.media.pending()
    }

    /// The workspace-level notices in force.
    pub fn warnings(&self) -> &'a [WireWarning] {
        &self.doc.warnings
    }

    /// Cues with a primed stream.
    pub fn primed(&self) -> Vec<CueId> {
        let mut v: Vec<CueId> = self.doc.voices.primed().collect();
        v.sort();
        v
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
    }
}

/// The cue-engine input for a transport action (docs/protocol.md, Transport actions); `None`
/// for a `batch`, which is one input per item.
pub fn transport_input(action: TransportAction) -> Option<Input> {
    Some(match action {
        TransportAction::Batch { .. } => return None,
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
    })
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

/// Applies an Arm/Disarm cue's change to the model, as a runtime change (no undo step).
fn set_armed(ws: &mut Workspace, cue: CueId, armed: bool) -> bool {
    ws.find_cue_mut(cue)
        .is_some_and(|c| std::mem::replace(&mut c.armed, armed) != armed)
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
            for c in descendants(std::slice::from_ref(c)) {
                add(c.id, &mut wanted);
            }
        }
    }
    for c in ws.cues() {
        if c.triggers.hotkey.is_some() || c.triggers.osc.is_some() {
            add(c.id, &mut wanted);
        }
    }
    wanted
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlab_core::Cue;

    fn rollback_failed() -> CommandError {
        CommandError::RollbackFailed {
            index: 2,
            source: Box::new(CommandError::Internal),
            rollback: Box::new(CommandError::InvalidValue("inverse")),
        }
    }

    fn memo_show() -> crate::control::offline::OfflineShow {
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

    /// A transaction whose rollback failed left the workspace partly changed: that is an edit
    /// (error logged, revision, dirty, cue engine told, clients sent the whole model) that
    /// nobody can undo. The partial change is simulated by editing behind the history.
    #[test]
    fn a_failed_rollback_is_treated_as_an_edit() {
        let mut s = memo_show();
        let cue = CueId(uuid_zero());
        s.transport(openlab_core::protocol::TransportAction::Start { cue });
        assert_eq!(s.control().engine().state(cue), CueState::PreWaiting);
        let revision = s.control().revision();
        s.take_outbound();
        // What the failed transaction did before it stopped, with history cleared as core does.
        let c = s.control_mut();
        c.docs[0].ws.apply(Command::RemoveCue { id: cue }).unwrap();
        c.docs[0].history.clear();
        let now = s.now();
        s.control_mut()
            .first_cx()
            .partly_changed(now, &rollback_failed());

        let c = s.control_mut();
        assert_eq!(c.revision(), revision + 1);
        assert!(c.docs[0].revision != c.docs[0].saved_revision);
        assert!(!c.docs[0].history.can_undo() && !c.docs[0].history.can_redo());
        assert_eq!(c.engine().unsignalled_model_changes(), 0);
        assert_ne!(c.engine().state(cue), CueState::PreWaiting);
        let (entries, ..) = c.eng.log_tail(None);
        assert!(entries.iter().any(|e| e.level == LogLevel::Error
            && e.kind == LogKind::Engine
            && e.message.contains("command 2")
            && e.message.contains("partly changed")
            && e.message.contains("inverse")));
        let out = s.take_outbound();
        let snapshot = out.iter().find_map(|m| match m {
            ServerMessage::Snapshot { snapshot } => Some(snapshot),
            _ => None,
        });
        let snapshot = snapshot.expect("clients get the whole model");
        assert_eq!(snapshot.revision, revision + 1);
        assert_eq!(&snapshot.workspace, s.control().workspace());
        assert!(out.iter().any(|m| matches!(
            m,
            ServerMessage::Document { document } if document.dirty && !document.can_undo
        )));
    }

    /// Every other refusal changed nothing: no revision, no snapshot.
    #[test]
    fn a_plain_refusal_changes_nothing() {
        let mut s = memo_show();
        let revision = s.control().revision();
        s.take_outbound();
        let now = s.now();
        s.control_mut()
            .first_cx()
            .partly_changed(now, &CommandError::Internal);
        assert_eq!(s.control().revision(), revision);
        assert_eq!(
            s.control().docs[0].revision,
            s.control().docs[0].saved_revision
        );
        assert!(s.take_outbound().is_empty());
    }

    fn two_cue_show() -> crate::control::offline::OfflineShow {
        let mut ws = memo_workspace(10.0);
        let mut second = ws.cue_lists[0].cues[0].clone();
        second.id = CueId(uuid::Uuid::from_u128(1));
        ws.cue_lists[0].cues.push(second);
        let mixer = openlab_audio::MixerConfig {
            sample_rate: 8000,
            output_channels: 2,
            max_block_frames: 64,
            max_voices: 8,
            max_voice_channels: 2,
            patches: 1,
            lookahead_ms: 500,
        };
        crate::control::offline::OfflineShow::new(ws, mixer, ControlConfig::new(8000, 64)).unwrap()
    }

    fn rename_both(a: &str, b: &str) -> Command {
        let rename = |n: u128, value: &str| Command::SetCueProperty {
            id: CueId(uuid::Uuid::from_u128(n)),
            property: openlab_core::CueProperty::Name {
                value: value.into(),
            },
        };
        Command::Transaction {
            commands: vec![rename(0, a), rename(1, b)],
        }
    }

    fn send_ok(s: &mut crate::control::offline::OfflineShow, message: ClientMessage) {
        let replies = s.send(message);
        assert!(replies.iter().any(|m| matches!(
            m,
            ServerMessage::Result {
                outcome: Outcome::Ok(_),
                ..
            }
        )));
    }

    /// The reply and the broadcasts of a request whose rollback failed: `command_refused` that
    /// says so, after the whole model.
    fn assert_partly_changed(
        s: &mut crate::control::offline::OfflineShow,
        replies: &[ServerMessage],
        before: u64,
    ) {
        let error = replies.iter().find_map(|m| match m {
            ServerMessage::Result {
                outcome: Outcome::Error(e),
                ..
            } => Some(e),
            _ => None,
        });
        let error = error.unwrap_or_else(|| panic!("a refusal: {replies:?}"));
        assert_eq!(error.code, ErrorCode::CommandRefused);
        assert!(error.message.contains("partly changed"));
        let c = s.control_mut();
        assert_eq!(c.revision(), before + 1);
        assert!(c.docs[0].revision != c.docs[0].saved_revision);
        assert!(!c.docs[0].history.can_undo() && !c.docs[0].history.can_redo());
        assert_eq!(c.engine().unsignalled_model_changes(), 0);
        let (entries, ..) = c.eng.log_tail(None);
        assert!(
            entries
                .iter()
                .any(|e| e.level == LogLevel::Error && e.message.contains("partly changed"))
        );
        let out = s.take_outbound();
        let snapshot = out.iter().find_map(|m| match m {
            ServerMessage::Snapshot { snapshot } => Some(snapshot),
            _ => None,
        });
        let snapshot = snapshot.expect("clients get the whole model");
        assert_eq!(&snapshot.workspace, s.control().workspace());
        assert!(!snapshot.document.can_undo);
    }

    #[test]
    fn a_command_whose_rollback_fails_is_an_unundoable_edit() {
        let mut s = two_cue_show();
        let before = s.control().revision();
        s.take_outbound();
        openlab_core::fault::inject(0, 1, true);
        let id = s.request_id();
        let replies = s.send(ClientMessage::Command {
            id,
            command: Box::new(rename_both("a", "b")),
            coalesce: None,
        });
        assert_partly_changed(&mut s, &replies, before);
        // The first rename stayed; the second never ran.
        let names: Vec<_> = s.control().workspace().cue_lists[0]
            .cues
            .iter()
            .map(|c| c.name.clone())
            .collect();
        assert_eq!(names, ["a", ""]);
    }

    #[test]
    fn an_undo_whose_rollback_fails_is_an_unundoable_edit() {
        let mut s = two_cue_show();
        let id = s.request_id();
        send_ok(
            &mut s,
            ClientMessage::Command {
                id,
                command: Box::new(rename_both("a", "b")),
                coalesce: None,
            },
        );
        // Behind the history's back: the undo step now refuses at its second command, and its
        // rollback is wrong.
        let c = s.control_mut();
        c.docs[0]
            .ws
            .apply(Command::RemoveCue {
                id: CueId(uuid_zero()),
            })
            .unwrap();
        let before = c.revision();
        s.take_outbound();
        openlab_core::fault::inject(0, usize::MAX, true);
        let id = s.request_id();
        let replies = s.send(ClientMessage::Undo { id });
        assert_partly_changed(&mut s, &replies, before);
    }

    #[test]
    fn a_redo_whose_rollback_fails_is_an_unundoable_edit() {
        let mut s = two_cue_show();
        let id = s.request_id();
        send_ok(
            &mut s,
            ClientMessage::Command {
                id,
                command: Box::new(rename_both("a", "b")),
                coalesce: None,
            },
        );
        let id = s.request_id();
        send_ok(&mut s, ClientMessage::Undo { id });
        let c = s.control_mut();
        c.docs[0]
            .ws
            .apply(Command::RemoveCue {
                id: CueId(uuid::Uuid::from_u128(1)),
            })
            .unwrap();
        let before = c.revision();
        s.take_outbound();
        openlab_core::fault::inject(0, usize::MAX, true);
        let id = s.request_id();
        let replies = s.send(ClientMessage::Redo { id });
        assert_partly_changed(&mut s, &replies, before);
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

    /// `audio_fault` notices sent as runtime changes, and the last warnings list's.
    fn fault_notices(msgs: &[ServerMessage]) -> (Vec<String>, Option<Vec<String>>) {
        let mut runtime = Vec::new();
        let mut list = None;
        let fault = |w: &WireWarning| match w {
            WireWarning::Notice { code, message, .. } if code == "audio_fault" => {
                Some(message.clone())
            }
            _ => None,
        };
        for m in msgs {
            match m {
                ServerMessage::Runtime { changes, .. } => {
                    runtime.extend(changes.iter().filter_map(|c| match c {
                        RuntimeChange::Warning { warning } => fault(warning),
                        _ => None,
                    }));
                }
                ServerMessage::Warnings { warnings } => {
                    list = Some(warnings.iter().filter_map(fault).collect());
                }
                _ => {}
            }
        }
        (runtime, list)
    }

    /// A memo show with one two-output patch "Main", on a mixer (and engine) of two patch
    /// slots, run for a block.
    fn patched_show() -> (crate::control::offline::OfflineShow, openlab_core::PatchId) {
        let mut ws = memo_workspace(10.0);
        let patch = openlab_core::PatchId(uuid::Uuid::from_u128(5));
        let output = openlab_core::PatchOutput {
            level_db: 0.0,
            mute: false,
            delay_ms: 0.0,
            polarity_inverted: false,
        };
        ws.patches.push(openlab_core::OutputPatch {
            id: patch,
            name: "Main".into(),
            master_db: 0.0,
            outputs: vec![output; 2],
        });
        let mixer = openlab_audio::MixerConfig {
            sample_rate: 8000,
            output_channels: 2,
            max_block_frames: 64,
            max_voices: 8,
            max_voice_channels: 2,
            patches: 2,
            lookahead_ms: 500,
        };
        let mut config = ControlConfig::new(8000, 64);
        config.mixer_patches = 2;
        // The patch is mapped onto it by default, so it is heard.
        config.devices = vec![openlab_core::DeviceInfo {
            name: "Dev".into(),
            output_channels: 2,
        }];
        let mut s = crate::control::offline::OfflineShow::new(ws, mixer, config).unwrap();
        s.run(64);
        s.take_outbound();
        (s, patch)
    }

    /// The last rendered sample of each output, after `frames` more frames.
    fn level_after(s: &mut crate::control::offline::OfflineShow, frames: u64) -> [f32; 2] {
        s.run(frames);
        [
            s.channel(0).last().copied().unwrap(),
            s.channel(1).last().copied().unwrap(),
        ]
    }

    /// Sends `kind` straight to the mixer, as the mixer's own reset after a fault would leave it.
    fn mixer_does(s: &mut crate::control::offline::OfflineShow, kind: CommandKind) {
        let eng = &mut s.control_mut().eng;
        let at = eng.now();
        eng.mixer.send(at, kind);
    }

    /// NFR-01 (review of #300, H1): after a fault the mixer has reset a unit, and the control
    /// thread's re-send is what brings the sound back. A voice plays through the patch onto both
    /// outputs; each unit is put in a silent reset state, then the control thread answers.
    #[test]
    fn a_reset_mixer_unit_plays_at_its_level_again_once_the_control_thread_answers() {
        use crate::control::offline::OfflineShow;
        use openlab_audio::FaultStage;
        let (mut s, patch) = patched_show();
        let slot = s
            .control()
            .eng
            .patches
            .mixer_patch(DocId(0), patch)
            .unwrap();
        let (mut tx, rx) = rtrb::RingBuffer::new(1 << 16);
        tx.push_entire_slice(&[0.5; 1 << 16]).unwrap();
        let mut v = openlab_audio::VoiceStart::new(VoiceId(u64::MAX - 1), rx, 1, 2);
        v.patch = slot;
        v.set_crosspoint(0, 0, 1.0);
        v.set_crosspoint(0, 1, 1.0);
        mixer_does(&mut s, CommandKind::StartVoice(Box::new(v)));
        let heard = level_after(&mut s, 640);
        assert!(heard[0] > 0.0 && heard[1] > 0.0, "{heard:?}");
        let zero_slot = |s: &mut OfflineShow| {
            for cue_output in 0..2 {
                for device_output in 0..2 {
                    let kind = CommandKind::SetPatchRouting {
                        patch: slot,
                        cue_output,
                        device_output,
                        gain: 0.0,
                    };
                    mixer_does(s, kind);
                }
            }
        };
        let quiet_output = |s: &mut OfflineShow| {
            let settings = openlab_audio::OutputSettings {
                level: 0.0,
                ..Default::default()
            };
            mixer_does(
                s,
                CommandKind::SetOutput {
                    output: 0,
                    settings,
                },
            );
        };
        let quiet_master =
            |s: &mut OfflineShow| mixer_does(s, CommandKind::SetMaster { gain: 0.0 });

        quiet_master(&mut s);
        assert_eq!(level_after(&mut s, 640), [0.0, 0.0]);
        s.control_mut().stage_faulted(FaultStage::Master, false);
        assert_eq!(level_after(&mut s, 640), heard, "master");

        quiet_output(&mut s);
        assert_eq!(level_after(&mut s, 640), [0.0, heard[1]]);
        s.control_mut().stage_faulted(FaultStage::Output(0), false);
        assert_eq!(level_after(&mut s, 640), heard, "output");

        zero_slot(&mut s);
        assert_eq!(level_after(&mut s, 640), [0.0, 0.0]);
        s.control_mut()
            .stage_faulted(FaultStage::Patch(slot), false);
        assert_eq!(level_after(&mut s, 640), heard, "patch");

        for (k, answer) in ["rebuild", "lost report"].into_iter().enumerate() {
            zero_slot(&mut s);
            quiet_output(&mut s);
            quiet_master(&mut s);
            assert_eq!(level_after(&mut s, 640), [0.0, 0.0]);
            if k == 0 {
                s.control_mut().stage_faulted(FaultStage::Mixer, false);
            } else {
                let counted = s.control().eng.stage_reports + 1;
                s.control_mut().lost_stage_reports(counted);
            }
            assert_eq!(level_after(&mut s, 640), heard, "{answer}");
        }
    }

    /// #316: a disabled unit whose report was lost (seen only in the mixer's published flags)
    /// is answered as if reported: the patch moves off its slot, a disabled output is a warning
    /// in force. One a rebuild re-enabled meanwhile is left alone.
    #[test]
    fn a_disabled_unit_whose_report_was_lost_is_found_from_the_mixers_flags() {
        let (mut s, patch) = patched_show();
        let at = |s: &crate::control::offline::OfflineShow| {
            s.control().eng.patches.mixer_patch(DocId(0), patch)
        };
        let first = at(&s).unwrap();
        let flags = openlab_audio::DisabledUnits {
            patches: 1 << first,
            outputs: 1 << 1,
            ..Default::default()
        };
        s.control_mut()
            .lost_disabled_reports(flags, openlab_audio::DisabledUnits::default());
        assert_eq!(at(&s), Some(first));
        s.control_mut().lost_disabled_reports(flags, flags);
        let moved = at(&s);
        assert!(moved.is_some_and(|m| m != first), "{moved:?}");
        s.run(64);
        let (_, list) = fault_notices(&s.take_outbound());
        assert!(list.unwrap().iter().any(|m| m.contains("device output 2")));
        // Known now: nothing more happens.
        s.control_mut().lost_disabled_reports(flags, flags);
        assert_eq!(at(&s), moved);
    }

    /// NFR-01: a mixer fault outside any voice reaches the workspace, and the unit it hit gets
    /// its settings again; a disabled patch moves to another slot, a disabled output is a
    /// warning until a rebuild, which frees the quarantined slot again.
    #[test]
    fn a_mixer_stage_fault_reaches_the_workspace_and_its_unit_is_set_again() {
        use openlab_audio::FaultStage;
        let (mut s, patch) = patched_show();
        let slot = |s: &crate::control::offline::OfflineShow| {
            s.control().eng.patches.mixer_patch(DocId(0), patch)
        };
        let first = slot(&s).unwrap();

        // As the mixer's event queue hands it over.
        s.control_mut()
            .route_mixer_event(openlab_audio::Event::StageFaulted {
                stage: FaultStage::Patch(first),
                at: 0,
                disabled: false,
            });
        assert_eq!(s.control().eng.stage_reports, 1);
        let (told, _) = fault_notices(&s.take_outbound());
        assert!(told.iter().any(|m| m.contains("\"Main\"")), "{told:?}");
        assert_eq!(slot(&s), Some(first));
        // A unit that keeps faulting is told about once a second, not once a fault.
        s.control_mut()
            .stage_faulted(FaultStage::Patch(first), false);
        assert!(s.take_outbound().is_empty());
        s.run(8000);
        s.take_outbound();
        s.control_mut()
            .stage_faulted(FaultStage::Patch(first), false);
        assert_eq!(fault_notices(&s.take_outbound()).0.len(), 1);

        s.control_mut()
            .stage_faulted(FaultStage::Patch(first), true);
        let moved = slot(&s).unwrap();
        assert_ne!(moved, first);
        assert_eq!(s.control().eng.patches.free_slots(), 0);

        s.control_mut().stage_faulted(FaultStage::Output(1), true);
        s.run(64);
        let (_, list) = fault_notices(&s.take_outbound());
        assert!(
            list.unwrap().iter().any(|m| m.contains("device output 2")),
            "a disabled output is a warning in force"
        );

        s.control_mut().stage_faulted(FaultStage::Mixer, false);
        s.run(64);
        let (told, list) = fault_notices(&s.take_outbound());
        assert!(told.iter().any(|m| m.contains("rebuilt")), "{told:?}");
        assert_eq!(list, Some(Vec::new()));
        assert_eq!(slot(&s), Some(moved));
        assert_eq!(s.control().eng.patches.free_slots(), 1);
    }

    /// A fault report the mixer counted but the control thread never got (a full event queue)
    /// would leave its unit silent: everything is sent again, and it is logged.
    #[test]
    fn a_lost_mixer_fault_report_sends_the_routing_again() {
        let mut s = memo_show();
        s.run(64);
        s.take_outbound();
        s.control_mut().lost_stage_reports(2);
        assert_eq!(s.control().eng.stage_reports, 2);
        let logged = s.take_outbound().iter().any(
            |m| matches!(m, ServerMessage::Log { message, .. } if message.contains("were lost")),
        );
        assert!(logged);
        // Counted again at the next check: nothing more is lost.
        s.control_mut().lost_stage_reports(2);
        assert!(s.take_outbound().is_empty());
    }
}
