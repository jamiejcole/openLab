// SPDX-License-Identifier: GPL-3.0-or-later

//! Documents (M4.4, M4.15; docs/control.md "Documents"): new, open, open-recovery, close, save,
//! save-as and autosave.
//!
//! Nothing here touches the disk on the control thread. Saves and autosaves go to the
//! [`SaveWriter`] thread (the control thread clones the workspace and polls for the answer each
//! tick); reading a show, the recovery check and the recovery scan run on one document worker
//! thread, whose answer comes back through the inbox ([`DocAnswer`]) and is applied here.
//!
//! Several docs (M4.15e, M4.15f; ARCHITECTURE §3.1, §6): `new`, `open` and `open_recovery` add a
//! doc ([`Control::open_doc`]) beside the ones open, which keep playing. The document
//! worker reads the file (or takes the workspace given) and builds everything the doc needs off
//! the control thread ([`DocParts`]: the workspace, its cue engine with its static problems
//! worked out, the media catalog's cue index), so installing it on the control thread is an
//! insert, the probe jobs and the slot commands. [`Control::close_doc`] stops that doc's cues
//! (Panic twice, a hard stop), frees its mixer slots, clears its autosave when its edits are
//! discarded, and hands the doc to the worker to be dropped there (its preloaded media can be
//! hundreds of megabytes). One read runs at a time engine-wide, and at most [`MAX_DOCS`] docs
//! are open; either limit is `command_refused`. A `relink` folder walk runs on a thread of its
//! own with a slot of its own (one walk at a time), so a walk that takes its whole time budget
//! never holds up an `open` or a `scan_recovery`.

use std::collections::VecDeque;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::Sender;
use openlab_audio::{CommandKind, Fade};
use openlab_core::protocol::{
    ErrorBody, ErrorCode, LogKind, LogLevel, OkBody, Outcome, RecoveryChoice, RecoveryEntryInfo,
    Request, RequestId, ResultData, ServerMessage, WireWarning,
};
use openlab_core::{
    Action, CueEngine, CueListId, EngineConfig, History, Input, LoadedShow, MAX_SHOW_FILE_BYTES,
    MediaRef, PatchId, PathStyle, Relink, Workspace, WorkspaceId, audio_media, autosave_due,
    is_absolute, load_show, portable_ref, relink_command,
};
use openlab_core::{Command, CommandError, CueId, MediaPaths, RelinkPlan};

use super::checks::{NoProbes, PlanChecks};
use super::inbox::reply;
use super::media::{Indexed, MediaCatalog, index_workspace};
use super::model::DocModel;
use super::{
    Control, ControlConfig, ControlError, Doc, DocCx, DocId, Engine, Inbox, MAX_DOCS, ReplyTx,
    Scope, fail, notice, ok,
};
use crate::persist::{
    AppDirs, JobKind, MAX_RECOVERY_ENTRIES, PersistError, RecoveryCheck, SaveReport, SaveWriter,
    ScanLimits, check_recovery, file_key, find_in_folder, lexical_key, media_path, read_text,
    scan_recovery,
};

/// Most `load_repair` notices sent for one opened show; the rest are summed in one more.
const MAX_LOAD_NOTICES: usize = 50;
/// Document workers abandoned while stuck in a read, at most; beyond this, requests that need
/// to read a file are refused until one of them comes back (each stuck thread is a leaked
/// thread, like a stuck probe worker).
const MAX_STUCK_WORKERS: usize = 4;
/// The shortest autosave period honoured, whatever the workspace says.
const MIN_AUTOSAVE_SECS: f64 = 1.0;

/// Where "now" in wall-clock milliseconds since the Unix epoch comes from: names backups and
/// paces autosave. The loop's own time is sample time, which stands still while a device is
/// down, so this is separate; tests inject a [`ManualClock`].
#[derive(Clone)]
pub struct EpochClock(Arc<dyn Fn() -> i64 + Send + Sync>);

impl EpochClock {
    pub fn system() -> Self {
        Self(Arc::new(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|d| i64::try_from(d.as_millis()).ok())
                .unwrap_or(0)
        }))
    }

    pub fn new(now_ms: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        Self(Arc::new(now_ms))
    }

    pub fn now_ms(&self) -> i64 {
        (self.0)()
    }
}

impl fmt::Debug for EpochClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EpochClock")
    }
}

/// A clock that only moves when told to (virtual time for tests).
#[derive(Clone, Debug, Default)]
pub struct ManualClock(Arc<AtomicI64>);

impl ManualClock {
    pub fn new(ms: i64) -> Self {
        Self(Arc::new(AtomicI64::new(ms)))
    }

    pub fn set(&self, ms: i64) {
        self.0.store(ms, Ordering::SeqCst);
    }

    pub fn advance(&self, ms: i64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }

    pub fn clock(&self) -> EpochClock {
        let ms = Arc::clone(&self.0);
        EpochClock::new(move || ms.load(Ordering::SeqCst))
    }
}

/// Called on the writer thread just before each save or autosave runs (tests hold the writer
/// busy in it to show the control thread does not wait for it).
#[derive(Clone)]
pub struct SaveHook(pub Arc<dyn Fn(JobKind) + Send + Sync>);

impl fmt::Debug for SaveHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SaveHook")
    }
}

/// Called on the document worker at the start of each read, and on the relink thread before its
/// walk (tests hold it to simulate a stalled volume).
#[derive(Clone)]
pub struct DocHook(pub Arc<dyn Fn() + Send + Sync>);

impl fmt::Debug for DocHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DocHook")
    }
}

/// A workspace the document worker has read, ready to install.
#[derive(Debug)]
pub struct Loaded {
    workspace: Workspace,
    /// The show file it came from; `None` for an autosave opened as untitled.
    path: Option<PathBuf>,
    /// Has changes the show file lacks (restored from an autosave).
    dirty: bool,
    notices: Vec<WireWarning>,
}

/// Everything a doc being opened needs, built on the document worker (ARCHITECTURE §3.1).
pub struct DocParts {
    /// The workspace and a cue engine that has followed it once (its static problems and
    /// playheads worked out) ...
    model: DocModel,
    /// The show file it came from; `None` for an autosave opened as untitled.
    path: Option<PathBuf>,
    /// Has changes the show file lacks (restored from an autosave).
    dirty: bool,
    notices: Vec<WireWarning>,
    /// ... and what it said doing so, carried out when the doc is installed.
    actions: Vec<Action>,
    /// The media catalog's reading of the workspace.
    indexed: Indexed,
    /// The slice-plan checks, every audio cue walked once (no file is probed yet).
    checks: PlanChecks,
}

impl fmt::Debug for DocParts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DocParts")
            .field("path", &self.path)
            .field("dirty", &self.dirty)
            .finish_non_exhaustive()
    }
}

/// Builds a doc's parts from a workspace read (on the document worker).
fn build_parts(loaded: Loaded, config: EngineConfig) -> DocParts {
    let Loaded {
        workspace,
        path,
        dirty,
        notices,
    } = loaded;
    let mut model = DocModel::new(workspace, CueEngine::new(config));
    // Sample 0: nothing runs yet, so what this decides (problems, playheads) does not depend
    // on the time; the control thread hands the engine the real time from its next call on.
    let actions = model.handle(Input::Tick, 0);
    let show_dir = path.as_deref().and_then(folder_of);
    let indexed = index_workspace(model.ws(), show_dir.as_deref());
    let mut checks = PlanChecks::default();
    checks.check_all(model.ws(), &NoProbes, config.sample_rate());
    DocParts {
        model,
        path,
        dirty,
        notices,
        actions,
        indexed,
        checks,
    }
}

/// What a doc opened with [`Control::open_doc`] starts from.
#[derive(Debug)]
pub enum DocSource {
    /// A new, empty workspace (as `new`).
    New,
    /// A show file (as `open`, recovery offer and choice included).
    Open {
        path: PathBuf,
        recovery: Option<RecoveryChoice>,
    },
    /// An autosave listed by `scan_recovery`, opened untitled and dirty (as `open_recovery`).
    Recovery {
        workspace_id: WorkspaceId,
        path_hash: Option<String>,
        modified_ms: Option<i64>,
    },
    /// A workspace in memory, never saved (tests; a template later).
    Workspace(Box<Workspace>),
}

#[derive(Debug)]
pub enum DocAnswer {
    Loaded {
        ticket: u64,
        loaded: Box<Loaded>,
    },
    /// A doc to add, built off the control thread.
    Built {
        ticket: u64,
        parts: Box<DocParts>,
    },
    /// An autosave newer than the show file exists and the request did not say what to do.
    Offer {
        ticket: u64,
        path: String,
        workspace_name: String,
        modified_ms: i64,
    },
    Entries {
        ticket: u64,
        entries: Vec<RecoveryEntryInfo>,
    },
    /// The folder walk of a `relink`: proposals only, applied on the control thread.
    Relinked {
        ticket: u64,
        found: Box<RelinkFound>,
    },
    Failed {
        ticket: u64,
        code: ErrorCode,
        message: String,
    },
}

/// What the document worker found for a `relink`.
#[derive(Debug)]
pub struct RelinkFound {
    plan: RelinkPlan,
    /// The audio cues' media as the worker's copy of the workspace had them: a proposal is
    /// applied only to a cue that still has that reference (the workspace may have been edited
    /// while the folder was walked).
    before: Vec<(CueId, MediaRef)>,
    truncated: bool,
    skipped: usize,
}

impl DocAnswer {
    fn ticket(&self) -> u64 {
        match self {
            Self::Loaded { ticket, .. }
            | Self::Built { ticket, .. }
            | Self::Offer { ticket, .. }
            | Self::Entries { ticket, .. }
            | Self::Relinked { ticket, .. }
            | Self::Failed { ticket, .. } => *ticket,
        }
    }
}

enum DocJob {
    /// A doc to add: its parts are built from what is read, with this cue engine config.
    Open {
        ticket: u64,
        path: PathBuf,
        recovery: Option<RecoveryChoice>,
        config: EngineConfig,
        /// The show files open (or being saved) in other docs: `already_open` if `path` is one
        /// of them under another spelling (a symlink, `/var` for `/private/var`).
        others: Vec<PathBuf>,
    },
    /// A doc to add from a workspace already in hand.
    Build {
        ticket: u64,
        loaded: Box<Loaded>,
        config: EngineConfig,
    },
    /// A closed doc, dropped here rather than on the control thread.
    Discard(Box<dyn Send>),
    OpenRecovery {
        ticket: u64,
        workspace: WorkspaceId,
        path_hash: Option<String>,
        modified_ms: Option<i64>,
        config: EngineConfig,
    },
    Scan {
        ticket: u64,
    },
}

/// The folder walk of a `relink`, on a copy of the workspace (its own thread: [`RelinkWalk`]).
struct RelinkJob {
    ticket: u64,
    workspace: Box<Workspace>,
    show_dir: Option<PathBuf>,
    folder: PathBuf,
    limits: ScanLimits,
}

/// A `relink` walk under way, on its own thread: the request it answers and that thread's
/// flags (the stall check abandons it like a stuck document worker).
struct RelinkWalk {
    pending: PendingDoc,
    flags: Arc<WalkFlags>,
}

/// What the control loop and a relink walk's thread tell each other. Whether the walk finished
/// or was given up is decided once, by whichever side swaps `handover` first, so exactly one
/// side owns the walk's place in the stuck-reader count: a walk that finishes at the instant
/// its timeout passes is either answered or counted stuck and un-counted by its thread, never
/// counted for ever (the thread ends after its walk; nothing else would take it back).
#[derive(Default)]
struct WalkFlags {
    /// `busy` only (a walk has its own thread, so `abandoned` is not used).
    worker: WorkerFlags,
    handover: std::sync::atomic::AtomicU8,
}

const WALKING: u8 = 0;
const WALK_DONE: u8 = 1;
const WALK_ABANDONED: u8 = 2;

impl WalkFlags {
    /// The walk's thread, once its answer is in the inbox: if the stall check gave up on it
    /// first, it counted the walk stuck, and the thread takes that back now.
    fn finish(&self, stuck: &AtomicUsize) {
        if self
            .handover
            .compare_exchange(WALKING, WALK_DONE, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            stuck.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// The stall check gives up on the walk: `true` and the walk counted stuck, or `false`
    /// when the walk has just finished (its answer is in the inbox) and nothing is counted. The
    /// count goes up before the swap, so the thread's take-back can never come first.
    fn abandon(&self, stuck: &AtomicUsize) -> bool {
        stuck.fetch_add(1, Ordering::SeqCst);
        let abandoned = self
            .handover
            .compare_exchange(WALKING, WALK_ABANDONED, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        if !abandoned {
            stuck.fetch_sub(1, Ordering::SeqCst);
        }
        abandoned
    }
}

/// What a document request is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// The engine's own: a recovery scan.
    Engine,
    /// A doc to add ([`Control::open_doc`]).
    New,
    /// The `relink` of a doc: applied to it when the walk is done, if it is still open.
    Relink(DocId),
}

/// What the request being worked on by the document worker is.
struct PendingDoc {
    target: Target,
    ticket: u64,
    id: RequestId,
    reply: ReplyTx,
    /// The show file an `open` reads: claimed while it is read, so a save-as cannot take it.
    path: Option<PathBuf>,
    /// When the request was handed to the worker ([`ControlConfig::clock`], milliseconds).
    started_ms: i64,
    /// Time allowed on top of the engine's read timeout: a folder walk has its own time limit,
    /// which the stall check must outwait.
    extra_ms: i64,
}

struct PendingSave {
    rx: Receiver<Result<SaveReport, PersistError>>,
    revision: u64,
    path: PathBuf,
    /// A save-as to another folder: relative media references were rebased in the copy saved.
    rebase: bool,
    /// Some of those were relative references (older history entries then name wrong paths).
    respelled: bool,
    waiters: Vec<(RequestId, ReplyTx)>,
}

type AutosaveRx = Receiver<Result<Option<PathBuf>, PersistError>>;

/// What the control loop and one document worker tell each other.
#[derive(Default)]
struct WorkerFlags {
    /// The control loop gave up on the worker's read: it leaves once the read returns.
    abandoned: AtomicBool,
    /// The worker is inside a job (set before the read, cleared once its answer is in the
    /// inbox): a timeout while it is not means the answer is on its way, not that it is stuck.
    busy: AtomicBool,
}

type ClearRx = Receiver<Result<(), PersistError>>;

/// The document worker and the save writer, shared by every workspace.
pub(super) struct Docs {
    writer: Option<SaveWriter>,
    jobs: Sender<DocJob>,
    /// Where new workers take jobs from: every worker shares one queue, so the jobs behind a
    /// stuck read go to the replacement.
    queue: crossbeam_channel::Receiver<DocJob>,
    /// The current worker's flags.
    worker: Arc<WorkerFlags>,
    /// Answers the workers have put in the inbox, ever (tests wait on it).
    answered: Arc<AtomicU64>,
    /// Abandoned workers still stuck in their read.
    stuck: Arc<AtomicUsize>,
    dirs: Option<AppDirs>,
    inbox: Sender<Inbox>,
    hook: Option<DocHook>,
    timeout_ms: i64,
    ticket: u64,
    pending: Option<PendingDoc>,
    /// The `relink` folder walk under way (one at a time, beside `pending`).
    relink: Option<RelinkWalk>,
    /// Autosave removals in flight: they outlive the workspace they were for.
    clears: Vec<ClearRx>,
    /// Saves of docs closed while they were being written: answered when they finish.
    orphans: Vec<PendingSave>,
}

/// One workspace's file state: its saves and autosave in flight, autosave pacing, load notices.
pub(super) struct DocFile {
    saves: Vec<PendingSave>,
    autosave: Option<AutosaveRx>,
    autosave_failed: bool,
    /// The revision last handed to the autosave.
    autosaved_revision: Option<u64>,
    dirty_since: Option<i64>,
    last_autosave: Option<i64>,
    /// Notices about how the workspace was loaded (repairs, recovery), shown with the
    /// workspace-level warnings while it is open.
    pub(super) notices: Vec<WireWarning>,
    /// The broadcast `snapshot` did not fit the outbound channel: send a fresh one when it does.
    pub(super) resend_snapshot: bool,
}

impl DocFile {
    pub(super) fn new() -> Self {
        Self {
            saves: Vec::new(),
            autosave: None,
            autosave_failed: false,
            autosaved_revision: None,
            dirty_since: None,
            last_autosave: None,
            notices: Vec::new(),
            resend_snapshot: false,
        }
    }

    /// Saves and the autosave in flight: tests wait until it is zero.
    pub(super) fn busy(&self) -> usize {
        self.saves.len() + usize::from(self.autosave.is_some())
    }
}

impl Docs {
    pub(super) fn new(config: &ControlConfig, inbox: Sender<Inbox>) -> Result<Self, ControlError> {
        let writer = match (&config.app_dirs, &config.save_hook) {
            (Some(dirs), Some(hook)) => {
                let hook = Arc::clone(&hook.0);
                Some(SaveWriter::spawn_with_hook(dirs.clone(), move |kind| {
                    hook(kind);
                })?)
            }
            (Some(dirs), None) => Some(SaveWriter::spawn(dirs.clone())?),
            (None, _) => None,
        };
        let (jobs, queue) = crossbeam_channel::unbounded();
        let mut docs = Self {
            writer,
            jobs,
            queue,
            worker: Arc::new(WorkerFlags::default()),
            answered: Arc::new(AtomicU64::new(0)),
            stuck: Arc::new(AtomicUsize::new(0)),
            dirs: config.app_dirs.clone(),
            inbox,
            hook: config.doc_hook.clone(),
            timeout_ms: i64::try_from(config.doc_timeout.as_millis()).unwrap_or(i64::MAX),
            ticket: 0,
            pending: None,
            relink: None,
            clears: Vec::new(),
            orphans: Vec::new(),
        };
        docs.spawn_worker()?;
        Ok(docs)
    }

    /// Starts a worker on the shared queue. Never joined: a read stuck on a stalled volume must
    /// not hang shutdown; the thread ends once the control loop (the job sender) is gone and
    /// its current job is done.
    fn spawn_worker(&mut self) -> std::io::Result<()> {
        let flags = Arc::new(WorkerFlags::default());
        self.worker = Arc::clone(&flags);
        let (queue, dirs, inbox, hook, stuck) = (
            self.queue.clone(),
            self.dirs.clone(),
            self.inbox.clone(),
            self.hook.clone(),
            Arc::clone(&self.stuck),
        );
        let answered = Arc::clone(&self.answered);
        thread::Builder::new()
            .name("openlab-docs".into())
            .spawn(move || {
                work(
                    &queue,
                    dirs.as_ref(),
                    &inbox,
                    hook.as_ref(),
                    &flags,
                    &stuck,
                    &answered,
                )
            })
            .map(|_| ())
    }

    /// The read, the relink walk, the autosave removals and closed docs' saves in flight (a
    /// doc's saves: [`DocFile::busy`]).
    pub(super) fn busy(&self) -> usize {
        usize::from(self.pending.is_some())
            + usize::from(self.relink.is_some())
            + self.clears.len()
            + self.orphans.len()
    }

    /// Starts the folder walk of a `relink` on a thread of its own. Never joined, like a
    /// worker: a walk stuck on a stalled volume ends when its read returns, and its answer (a
    /// stale ticket by then) is ignored.
    fn spawn_relink(&self, job: RelinkJob, flags: &Arc<WalkFlags>) -> std::io::Result<()> {
        let (inbox, hook, stuck, answered, flags) = (
            self.inbox.clone(),
            self.hook.clone(),
            Arc::clone(&self.stuck),
            Arc::clone(&self.answered),
            Arc::clone(flags),
        );
        // Busy from the start: the stall check must not take a walk not yet begun for one
        // whose answer is on its way.
        flags.worker.busy.store(true, Ordering::SeqCst);
        thread::Builder::new()
            .name("openlab-relink".into())
            .spawn(move || {
                if let Some(hook) = &hook {
                    (hook.0)();
                }
                let RelinkJob {
                    ticket,
                    workspace,
                    show_dir,
                    folder,
                    limits,
                } = job;
                let answer = relink_scan(ticket, &workspace, show_dir.as_deref(), &folder, limits);
                deliver(answer, &inbox, &flags.worker, &answered);
                flags.finish(&stuck);
            })
            .map(|_| ())
    }

    /// Hands a closed doc to the worker to be dropped there; dropped here if it is gone.
    pub(super) fn discard(&self, doc: Box<dyn Send>) {
        if let Err(crossbeam_channel::SendError(DocJob::Discard(doc))) =
            self.jobs.send(DocJob::Discard(doc))
        {
            drop(doc);
        }
    }
}

// ---- The document worker thread -------------------------------------------------------------

fn work(
    jobs: &crossbeam_channel::Receiver<DocJob>,
    dirs: Option<&AppDirs>,
    inbox: &Sender<Inbox>,
    hook: Option<&DocHook>,
    flags: &WorkerFlags,
    stuck: &AtomicUsize,
    answered: &AtomicU64,
) {
    while let Ok(job) = jobs.recv() {
        if let DocJob::Discard(doc) = job {
            drop(doc);
            continue;
        }
        flags.busy.store(true, Ordering::SeqCst);
        if let Some(hook) = hook {
            (hook.0)();
        }
        let built = |answer: DocAnswer, config: EngineConfig| match answer {
            DocAnswer::Loaded { ticket, loaded } => DocAnswer::Built {
                ticket,
                parts: Box::new(build_parts(*loaded, config)),
            },
            answer => answer,
        };
        let answer = match job {
            DocJob::Open {
                ticket,
                path,
                recovery,
                config,
                others,
            } => {
                let key = file_key(&path);
                if others.iter().any(|o| file_key(o) == key) {
                    failed(
                        ticket,
                        ErrorCode::AlreadyOpen,
                        format!("{} is open already", path.display()),
                    )
                } else {
                    built(open(ticket, &path, recovery, dirs), config)
                }
            }
            DocJob::Build {
                ticket,
                loaded,
                config,
            } => DocAnswer::Built {
                ticket,
                parts: Box::new(build_parts(*loaded, config)),
            },
            DocJob::Discard(_) => continue,
            DocJob::OpenRecovery {
                ticket,
                workspace,
                path_hash,
                modified_ms,
                config,
            } => built(
                open_recovery(ticket, workspace, path_hash.as_deref(), modified_ms, dirs),
                config,
            ),
            DocJob::Scan { ticket } => scan(ticket, dirs),
        };
        if !post(answer, inbox, flags, stuck, answered) {
            break;
        }
    }
}

/// Puts a worker's answer in the inbox; `false` when the thread is done: the control loop is
/// gone, or it gave up on the request while this was reading (the answer is stale, with another
/// ticket, and the worker leaves the work to its replacement).
fn post(
    answer: DocAnswer,
    inbox: &Sender<Inbox>,
    flags: &WorkerFlags,
    stuck: &AtomicUsize,
    answered: &AtomicU64,
) -> bool {
    let gone = flags.abandoned.load(Ordering::SeqCst);
    let sent = deliver(answer, inbox, flags, answered);
    if gone {
        // After the send: a count of zero means the stale answer is already in the inbox.
        stuck.fetch_sub(1, Ordering::SeqCst);
    }
    sent && !gone
}

/// Puts a worker's or a relink walk's answer in the inbox and counts it; `false` when the
/// control loop is gone.
fn deliver(
    answer: DocAnswer,
    inbox: &Sender<Inbox>,
    flags: &WorkerFlags,
    answered: &AtomicU64,
) -> bool {
    let sent = inbox.send(Inbox::Doc(answer)).is_ok();
    // Not busy before the answer counts: whoever sees the count can rely on `busy` being
    // false already (the answer itself is in the inbox from the send above).
    flags.busy.store(false, Ordering::SeqCst);
    answered.fetch_add(1, Ordering::SeqCst);
    sent
}

fn failed(ticket: u64, code: ErrorCode, message: String) -> DocAnswer {
    DocAnswer::Failed {
        ticket,
        code,
        message,
    }
}

/// Reads and loads a show file; the error carries the protocol code.
fn read_show(path: &Path) -> Result<LoadedShow, (ErrorCode, String)> {
    let text = read_text(path, MAX_SHOW_FILE_BYTES).map_err(|e| {
        let code = match &e {
            PersistError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
                ErrorCode::NotFound
            }
            PersistError::Io { .. } => ErrorCode::Io,
            _ => ErrorCode::BadShowFile,
        };
        (code, e.to_string())
    })?;
    load_show(&text).map_err(|e| (ErrorCode::BadShowFile, format!("{}: {e}", path.display())))
}

fn load_notices(shown: &LoadedShow) -> Vec<WireWarning> {
    let mut notices: Vec<WireWarning> = shown
        .warnings
        .iter()
        .take(MAX_LOAD_NOTICES)
        .map(|issue| notice("load_repair", issue.to_string(), issue.cue_id))
        .collect();
    let more = shown.warnings.len().saturating_sub(MAX_LOAD_NOTICES);
    if more > 0 {
        notices.push(notice(
            "load_repair",
            format!("{more} more problem(s) were found in the show file"),
            None,
        ));
    }
    notices
}

fn open(
    ticket: u64,
    path: &Path,
    choice: Option<RecoveryChoice>,
    dirs: Option<&AppDirs>,
) -> DocAnswer {
    let shown = match read_show(path) {
        Ok(shown) => shown,
        Err((code, message)) => return failed(ticket, code, message),
    };
    let mut notices = load_notices(&shown);
    let mut workspace = shown.workspace;
    let mut dirty = false;
    if let Some(dirs) = dirs {
        let report = check_recovery(dirs, workspace.id, path);
        match report.check {
            RecoveryCheck::Offer(offer) => match choice {
                None => {
                    return DocAnswer::Offer {
                        ticket,
                        path: path.to_string_lossy().into_owned(),
                        workspace_name: offer.loaded.workspace.name.clone(),
                        modified_ms: offer.recovery_modified_ms,
                    };
                }
                Some(RecoveryChoice::Restore) => {
                    notices = load_notices(&offer.loaded);
                    workspace = offer.loaded.workspace;
                    dirty = true;
                }
                Some(RecoveryChoice::Ignore) => {}
            },
            RecoveryCheck::Unusable { warning, .. } => {
                notices.push(notice("recovery_unusable", warning, None));
            }
            RecoveryCheck::Stale { .. } | RecoveryCheck::None => {}
        }
        if !report.other_copies.is_empty() {
            notices.push(notice(
                "recovery_other_copies",
                format!(
                    "{} autosave(s) from another copy of this show exist",
                    report.other_copies.len()
                ),
                None,
            ));
        }
    }
    DocAnswer::Loaded {
        ticket,
        loaded: Box::new(Loaded {
            workspace,
            path: Some(path.to_path_buf()),
            dirty,
            notices,
        }),
    }
}

fn open_recovery(
    ticket: u64,
    workspace: WorkspaceId,
    path_hash: Option<&str>,
    modified_ms: Option<i64>,
    dirs: Option<&AppDirs>,
) -> DocAnswer {
    let Some(dirs) = dirs else {
        return failed(
            ticket,
            ErrorCode::NotFound,
            "there is no application folder to recover from".into(),
        );
    };
    let scan = scan_recovery(dirs, MAX_RECOVERY_ENTRIES);
    let entry = scan.entries.iter().find(|e| {
        e.workspace_id == workspace
            && path_hash.is_none_or(|h| e.path_hash.as_deref() == Some(h))
            && modified_ms.is_none_or(|m| e.modified_ms == Some(m))
    });
    let Some(entry) = entry else {
        return failed(
            ticket,
            ErrorCode::NotFound,
            format!("no autosave of workspace {workspace}"),
        );
    };
    let shown = match read_show(&entry.path) {
        Ok(shown) if shown.workspace.id == workspace => shown,
        Ok(_) => {
            return failed(
                ticket,
                ErrorCode::BadShowFile,
                format!("{} belongs to another workspace", entry.path.display()),
            );
        }
        Err((code, message)) => return failed(ticket, code, message),
    };
    DocAnswer::Loaded {
        ticket,
        loaded: Box::new(Loaded {
            notices: load_notices(&shown),
            workspace: shown.workspace,
            path: None,
            dirty: true,
        }),
    }
}

/// Walks `folder` for the files `workspace` lacks (on the relink thread: it can take as long as
/// `limits` allow, and a volume that stops answering is the stall check's business). A folder
/// that is not a folder is `not_found`.
fn relink_scan(
    ticket: u64,
    workspace: &Workspace,
    show_dir: Option<&Path>,
    folder: &Path,
    limits: ScanLimits,
) -> DocAnswer {
    match std::fs::metadata(folder) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => {
            return failed(
                ticket,
                ErrorCode::BadRequest,
                format!("{} is not a folder", folder.display()),
            );
        }
        Err(e) => {
            let code = if e.kind() == std::io::ErrorKind::NotFound {
                ErrorCode::NotFound
            } else {
                ErrorCode::Io
            };
            return failed(ticket, code, format!("{}: {e}", folder.display()));
        }
    }
    let found = find_in_folder(workspace, show_dir, folder, limits);
    DocAnswer::Relinked {
        ticket,
        found: Box::new(RelinkFound {
            plan: found.plan,
            before: audio_media(workspace),
            truncated: found.truncated,
            skipped: found.skipped,
        }),
    }
}

fn scan(ticket: u64, dirs: Option<&AppDirs>) -> DocAnswer {
    let entries = dirs
        .map(|dirs| scan_recovery(dirs, MAX_RECOVERY_ENTRIES).entries)
        .unwrap_or_default()
        .into_iter()
        .map(|e| RecoveryEntryInfo {
            workspace_id: e.workspace_id,
            path_hash: e.path_hash,
            show_name: e.show_name,
            modified_ms: e.modified_ms,
            stale: e.stale,
        })
        .collect();
    DocAnswer::Entries { ticket, entries }
}

// ---- The control loop's side ----------------------------------------------------------------

fn persist_code(e: &PersistError) -> ErrorCode {
    match e {
        PersistError::Io { .. } | PersistError::WriterGone | PersistError::Stalled { .. } => {
            ErrorCode::Io
        }
        PersistError::OpenElsewhere(_) => ErrorCode::AlreadyOpen,
        _ => ErrorCode::Internal,
    }
}

/// Ids for a new workspace, from the OS (core draws no randomness).
pub(super) fn random_id() -> uuid::Uuid {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        // The OS refused: the time and a counter-free fallback are still unique enough for a
        // workspace that has not been saved anywhere yet.
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        bytes = nanos.to_le_bytes();
    }
    uuid::Builder::from_random_bytes(bytes).into_uuid()
}

/// A random number for the new workspace's passcode, from the OS; `None` if the OS refused.
fn os_seed() -> Option<u32> {
    let mut bytes = [0u8; 4];
    getrandom::fill(&mut bytes)
        .is_ok()
        .then(|| u32::from_le_bytes(bytes))
}

/// The seed when the OS gave none: the clock's low bits are as good as a passcode needs to be
/// here (it protects a show nobody has saved yet, and the operator sees and can change it).
fn clock_seed() -> u32 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    u32::try_from(nanos & u128::from(u32::MAX)).unwrap_or(0)
}

/// A new workspace (WS-02) with QLab's default OSC access: no rights without a passcode, and one
/// 4-digit passcode (from `seed`) with all rights, so a new show is never open to the network.
pub(crate) fn new_workspace(
    id: WorkspaceId,
    list: CueListId,
    patch: PatchId,
    network: openlab_core::NetworkPatchId,
    seed: u32,
) -> Workspace {
    let mut workspace = Workspace::new_empty(id, list, patch, network);
    workspace.settings.osc_access = openlab_core::OscAccess::new_workspace(seed);
    workspace
}

/// Media references of `ws` re-spelled for the folder `new_dir` (WS-03): relative ones so they
/// still name the same files from there, and absolute ones made relative when they lie literally
/// inside `new_dir` (the rule of `portable_ref`; a show saved for the first time holds absolute
/// paths, `docs/file-format.md` section 4.3). An absolute path outside `new_dir` is never touched.
/// The flag is true when a relative reference was re-spelled: only those make older history
/// entries wrong (an absolute reference still names its file when an undo puts it back).
fn rebase(ws: &Workspace, old_dir: Option<&Path>, new_dir: &Path) -> (Vec<Relink>, bool) {
    let Some(new_dir) = new_dir.to_str() else {
        return (Vec::new(), false);
    };
    let mut respelled = false;
    let relinks = audio_media(ws)
        .into_iter()
        .filter_map(|(cue, media)| {
            if is_absolute(media.as_str()) {
                let new = portable_ref(media.as_str(), Some(new_dir), PathStyle::host());
                return (!is_absolute(new.as_str()) && new != media).then_some(Relink { cue, new });
            }
            let file = media_path(&media, old_dir)?;
            let new: MediaRef = portable_ref(file.to_str()?, Some(new_dir), PathStyle::host());
            let changed = new != media;
            respelled |= changed;
            changed.then_some(Relink { cue, new })
        })
        .collect();
    (relinks, respelled)
}

/// Whether two paths name the same file, compared like media paths (case, NFC and `..`
/// ignored): the same show reopened under another spelling is still the same file.
fn same_path(a: &Path, b: &Path) -> bool {
    lexical_key(a) == lexical_key(b)
}

fn folder_of(path: &Path) -> Option<PathBuf> {
    path.parent().map(Path::to_path_buf)
}

impl DocCx<'_> {
    /// A save of this doc (`save`, `save_as`); the other document requests are app-scope
    /// ([`Control::app_scope`]).
    /// `others`: the show files of the other docs and their saves and the open in flight,
    /// which this save must not write ([`Control::save`]).
    pub(super) fn doc_request(
        &mut self,
        id: RequestId,
        request: Request,
        to: &ReplyTx,
        others: Vec<PathBuf>,
    ) {
        match request {
            Request::Save => match self.doc.show_path.clone() {
                Some(path) => self.start_save(id, to, path, others),
                None => fail(
                    to,
                    id,
                    ErrorCode::NoWorkspace,
                    "the workspace has never been saved; use save_as",
                ),
            },
            Request::SaveAs { path } => self.start_save(id, to, PathBuf::from(path), others),
            other => fail(
                to,
                id,
                ErrorCode::Internal,
                format!(
                    "{} is not a request of one workspace",
                    super::request_name(&other)
                ),
            ),
        }
    }

    /// Applies a folder walk's matches as one undo step, to the cues that still have the
    /// reference the walk started from, and answers with the counts and whether a limit ended
    /// the walk early.
    fn apply_relink(&mut self, found: RelinkFound, id: RequestId, to: &ReplyTx) {
        let RelinkFound {
            plan,
            before,
            truncated,
            skipped,
        } = found;
        let current = audio_media(self.doc.model.ws());
        let relinks: Vec<Relink> = plan
            .matches
            .into_iter()
            .filter(|m| {
                fn media_of(list: &[(CueId, MediaRef)], cue: CueId) -> Option<&MediaRef> {
                    list.iter().find(|(c, _)| *c == cue).map(|(_, r)| r)
                }
                media_of(&before, m.cue).is_some()
                    && media_of(&before, m.cue) == media_of(&current, m.cue)
            })
            .collect();
        let relinked = relinks.len() as u64;
        let now = self.now();
        let command = relink_command(self.doc.model.ws(), &relinks);
        let footprint = command.footprint();
        let mut edit = self.doc.model.edit();
        let result = self.doc.history.execute(&mut edit, command);
        edit.ended(&footprint, &result);
        let revision = match result {
            Ok(true) => {
                self.edited(now, &footprint);
                self.log(
                    LogLevel::Info,
                    format!(
                        "relinked {relinked} cue(s){}",
                        if truncated { " (search cut short)" } else { "" }
                    ),
                );
                Some(self.doc.revision)
            }
            Ok(false) => Some(self.doc.revision),
            Err(e) => {
                self.partly_changed(now, &e);
                fail(to, id, ErrorCode::CommandRefused, e.to_string());
                return;
            }
        };
        reply(
            to,
            ServerMessage::Result {
                id,
                outcome: Outcome::Ok(OkBody {
                    revision,
                    data: Some(ResultData::Relink {
                        relinked,
                        ambiguous: plan
                            .ambiguous
                            .into_iter()
                            .map(|a| openlab_core::protocol::AmbiguousMatch {
                                cue: a.cue,
                                candidates: a
                                    .candidates
                                    .iter()
                                    .map(|c| c.as_str().to_owned())
                                    .collect(),
                            })
                            .collect(),
                        unmatched: plan.unmatched,
                        truncated,
                        skipped: skipped as u64,
                    }),
                }),
            },
        );
    }

    /// Stops everything that is running, as Panic pressed twice does, and lets go of every
    /// voice, prime and queued feedback: the doc is closing.
    pub(super) fn stop_everything(&mut self) {
        let now = self.now();
        // A second Panic within the first's fade is a hard stop (docs/cue-engine.md, Panic).
        self.feed(Input::Panic, now);
        self.feed(Input::Panic, now);
        self.silence_orphans(Scope::All, now);
        // Whatever the engine did not own any more.
        let floor = self.stop_floor();
        let stops: Vec<_> = self
            .doc
            .voices
            .playing_ids()
            .into_iter()
            .map(|voice| {
                (
                    self.doc.voices.stop_date(voice, now, floor),
                    CommandKind::StopVoice {
                        voice,
                        fade: Fade::NONE,
                    },
                )
            })
            .collect();
        self.send_commands(stops);
        self.doc.voices.forget_all();
        self.doc.deferred.clear();
        self.doc.pending_media.clear();
        self.doc.ready_media.clear();
        self.doc.events.clear();
        self.eng.mixer.pump();
    }

    /// Queues the removal of a workspace's recovery file behind the writer's current job.
    fn clear_autosave_of(&mut self, workspace: WorkspaceId, show_path: Option<PathBuf>) {
        if let Some(writer) = &self.eng.docs.writer {
            self.eng
                .docs
                .clears
                .push(writer.clear_recovery(workspace, show_path));
        }
    }

    /// Sends this doc's snapshot to its clients again (its model changed in a way no
    /// `model_changed` describes, or the first broadcast did not fit).
    pub(super) fn resend_snapshot(&mut self) {
        let snapshot = Box::new(self.doc.snapshot());
        self.doc.file.resend_snapshot = !self
            .eng
            .out
            .send_state(Some(self.doc.id), ServerMessage::Snapshot { snapshot });
    }

    pub(super) fn show_dir(&self) -> Option<PathBuf> {
        self.doc.show_path.as_deref().and_then(folder_of)
    }

    /// What a client sent, made ready to apply (WS-03, ED-05): media paths stored relative to
    /// the show folder when inside it, a raw clipboard turned into cues with fresh ids.
    pub(super) fn prepare_command(&self, command: Command) -> Result<Command, CommandError> {
        let dir = self.show_dir();
        let paths = MediaPaths {
            // A folder that is not UTF-8 cannot be compared with media paths: they stay as given.
            show_dir: dir.as_deref().and_then(Path::to_str),
            style: PathStyle::host(),
        };
        command.prepare(self.doc.model.ws(), &paths, &mut || CueId(random_id()))
    }

    // ---- Saving -----------------------------------------------------------------------------

    fn start_save(&mut self, id: RequestId, to: &ReplyTx, path: PathBuf, others: Vec<PathBuf>) {
        let Some(writer) = &self.eng.docs.writer else {
            fail(
                to,
                id,
                ErrorCode::Io,
                "saving is not available: no application folder",
            );
            return;
        };
        let new_dir = folder_of(&path);
        let old_dir = self.show_dir();
        let moved = self.doc.show_path.as_deref() != Some(path.as_path())
            && new_dir.is_some()
            && new_dir != old_dir;
        let mut copy = self.doc.model.ws().clone();
        let mut rebased = false;
        let mut respelled_relative = false;
        let mut relink_failure = None;
        if moved && let Some(new_dir) = &new_dir {
            let (relinks, respelled) = rebase(&copy, old_dir.as_deref(), new_dir);
            if !relinks.is_empty() {
                let command = relink_command(&copy, &relinks);
                let result = copy.apply(command);
                rebased = result.is_ok();
                respelled_relative = rebased && respelled;
                if let Err(e @ openlab_core::CommandError::RollbackFailed { .. }) = &result {
                    relink_failure = Some(format!("relink of the copy being saved: {e}"));
                }
                if !rebased {
                    // A refusal changes nothing, but a failed rollback leaves some relinks in
                    // the copy: save the workspace as it is.
                    copy = self.doc.model.ws().clone();
                }
            }
        }
        let rx = writer.save_unless(
            copy,
            path.clone(),
            self.config_clock_ms(),
            self.eng.config.machine.backup_keep,
            self.doc.revision,
            others,
        );
        if let Some(message) = relink_failure {
            self.log(LogLevel::Error, message);
        }
        self.doc.file.saves.push(PendingSave {
            rx,
            revision: self.doc.revision,
            path,
            rebase: rebased,
            respelled: respelled_relative,
            waiters: vec![(id, to.clone())],
        });
    }

    fn config_clock_ms(&self) -> i64 {
        self.eng.config.clock.now_ms()
    }

    /// Collects what the writer thread has finished, and starts an autosave when one is due.
    pub(super) fn docs_tick(&mut self) {
        if self.doc.file.resend_snapshot && self.eng.out.has_room() {
            self.resend_snapshot();
        }
        // The read watchdog and autosave removals are the engine's ([`Control::tick`]).
        self.poll_saves();
        self.poll_autosave();
        self.autosave_if_due();
    }

    fn poll_saves(&mut self) {
        for (save, result) in take_finished(&mut self.doc.file.saves) {
            self.save_done(save, result);
        }
    }

    fn save_done(&mut self, save: PendingSave, result: Result<SaveReport, PersistError>) {
        match result {
            Ok(report) => {
                for warning in report.warnings {
                    self.log(LogLevel::Warn, warning);
                }
                self.saved(&save);
                for (id, to) in &save.waiters {
                    reply(to, ok(*id, None));
                }
            }
            Err(e) => {
                self.log_kind(
                    LogKind::Document,
                    LogLevel::Error,
                    format!("save failed: {e}"),
                );
                for (id, to) in &save.waiters {
                    fail(to, *id, persist_code(&e), e.to_string());
                }
            }
        }
    }

    /// A save of this workspace is on disk.
    fn saved(&mut self, save: &PendingSave) {
        let clean_before = self.doc.revision == save.revision;
        self.doc.saved_revision = save.revision;
        self.record(
            LogKind::Document,
            LogLevel::Info,
            format!("saved {}", save.path.display()),
            None,
            None,
        );
        if self.doc.show_path.as_deref() != Some(save.path.as_path()) {
            let old_dir = self.show_dir();
            self.doc.show_path = Some(save.path.clone());
            if self.show_dir() != old_dir {
                self.moved_folder(
                    old_dir.as_deref(),
                    save.rebase,
                    save.respelled,
                    clean_before,
                );
            }
        }
        if !self.doc.is_dirty() {
            self.doc.file.dirty_since = None;
        }
        self.document();
    }

    /// The workspace now lives in another folder: relative media references are re-spelled so
    /// they still name the same files (as in the copy just saved), and the files are probed
    /// again from the new place. That is not an edit the operator made, so history is cleared
    /// (an undo would bring back a reference that is now wrong) and every client gets a snapshot.
    fn moved_folder(
        &mut self,
        old_dir: Option<&Path>,
        rebase_refs: bool,
        respelled: bool,
        clean_before: bool,
    ) {
        let mut changed = false;
        let mut respelled = respelled;
        if rebase_refs && let Some(new_dir) = self.show_dir() {
            let (relinks, now_respelled) = rebase(self.doc.model.ws(), old_dir, &new_dir);
            respelled |= now_respelled;
            if !relinks.is_empty() {
                let command = relink_command(self.doc.model.ws(), &relinks);
                // The engine is told of the whole workspace (the guard's drop): this clears
                // history and resends every client the model anyway.
                let result = self.doc.model.edit().apply(command);
                // A failed rollback leaves some relinks applied: the model did change.
                changed = match &result {
                    Ok(_) => true,
                    Err(e) => matches!(e, openlab_core::CommandError::RollbackFailed { .. }),
                };
                if let Err(e) = result {
                    self.log(LogLevel::Error, format!("relink after the move: {e}"));
                }
            }
        }
        self.doc.media.reset(self.show_dir(), self.doc.model.ws());
        self.doc.waveforms.reset();
        self.eng.waveform_reader.forget(self.doc.id);
        if changed {
            // An undo can restore an old relative spelling that now resolves against the new
            // folder, so history goes on any move from a known folder. Only the first save of an
            // untitled show keeps it: its references were all absolute and still name their files.
            if respelled || old_dir.is_some() {
                self.doc.history = History::new();
            }
            self.doc.revision += 1;
            // The copy on disk has the same references; it lacks only edits made meanwhile.
            if clean_before {
                self.doc.saved_revision = self.doc.revision;
            }
        }
        let inputs = self.sync_workspace(false);
        let now = self.now();
        for input in inputs {
            self.feed(input, now);
        }
        if changed {
            self.resend_snapshot();
        }
    }

    fn poll_autosave(&mut self) {
        let Some(rx) = &self.doc.file.autosave else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(PersistError::WriterGone),
        };
        self.doc.file.autosave = None;
        match result {
            Ok(_) => self.doc.file.autosave_failed = false,
            Err(e) => {
                // Said once per run of failures: it repeats every interval otherwise.
                if !self.doc.file.autosave_failed {
                    self.log(LogLevel::Warn, format!("autosave failed: {e}"));
                }
                self.doc.file.autosave_failed = true;
                // Try the same revision again next time.
                self.doc.file.autosaved_revision = None;
            }
        }
    }

    fn autosave_if_due(&mut self) {
        if !self.doc.is_dirty() {
            self.doc.file.dirty_since = None;
            return;
        }
        let now_ms = self.config_clock_ms();
        let dirty_since = *self.doc.file.dirty_since.get_or_insert(now_ms);
        if self.doc.file.autosave.is_some()
            || self.doc.file.autosaved_revision == Some(self.doc.revision)
        {
            return;
        }
        let seconds = |ms: i64| ms as f64 / 1000.0;
        let interval = self
            .doc
            .model
            .ws()
            .settings
            .autosave_interval
            .max(MIN_AUTOSAVE_SECS);
        if !autosave_due(
            Some(seconds(dirty_since)),
            self.doc.file.last_autosave.map(seconds),
            seconds(now_ms),
            interval,
        ) {
            return;
        }
        let Some(writer) = &self.eng.docs.writer else {
            return;
        };
        let rx = writer.autosave(
            self.doc.model.ws().clone(),
            self.doc.show_path.clone(),
            self.doc.revision,
        );
        self.doc.file.autosave = Some(rx);
        self.doc.file.autosaved_revision = Some(self.doc.revision);
        self.doc.file.last_autosave = Some(now_ms);
    }
}

impl Engine {
    /// A read that has not answered within the timeout (a volume that stopped answering): the
    /// request fails with `io`, the worker is abandoned (it ends when its read returns; if it
    /// never does, it is one leaked thread, bounded by [`MAX_STUCK_WORKERS`]) and a fresh one
    /// takes over the shared queue.
    pub(super) fn check_stalled_read(&mut self) {
        self.check_stalled_walk();
        let stalled = self
            .docs
            .pending
            .as_ref()
            .is_some_and(|p| self.stalled(p, &self.docs.worker));
        if !stalled {
            return;
        }
        let Some(pending) = self.docs.pending.take() else {
            return;
        };
        fail(
            &pending.reply,
            pending.id,
            ErrorCode::Io,
            format!(
                "reading the file did not finish within {} s; its volume may have stopped answering",
                self.docs.timeout_ms / 1000
            ),
        );
        self.docs.worker.abandoned.store(true, Ordering::SeqCst);
        self.docs.stuck.fetch_add(1, Ordering::SeqCst);
        if let Err(e) = self.docs.spawn_worker() {
            self.log(
                LogLevel::Error,
                format!("could not start a replacement document worker: {e}"),
            );
        }
        self.log(
            LogLevel::Error,
            "a document read stalled; its worker was abandoned".to_owned(),
        );
    }

    /// Whether `pending`, worked on by the thread with `flags`, has outlived the timeout (plus
    /// its own allowance). A thread that has answered (the answer is in the inbox, not yet
    /// applied) is not stuck: the request is left to that answer, unless no answer comes for
    /// twice the timeout either (then the thread is gone).
    fn stalled(&self, pending: &PendingDoc, flags: &WorkerFlags) -> bool {
        let elapsed = self
            .config
            .clock
            .now_ms()
            .saturating_sub(pending.started_ms);
        let timeout_ms = self.docs.timeout_ms.saturating_add(pending.extra_ms);
        if elapsed < timeout_ms {
            return false;
        }
        flags.busy.load(Ordering::SeqCst) || elapsed >= timeout_ms.saturating_mul(2)
    }

    /// [`Self::check_stalled_read`] for the relink walk: it fails with `io` and its thread is
    /// abandoned (counted as a stuck reader, so requests that read are refused once too many
    /// are stuck). No replacement is needed: each walk has a thread of its own.
    fn check_stalled_walk(&mut self) {
        let stalled = self
            .docs
            .relink
            .as_ref()
            .is_some_and(|w| self.stalled(&w.pending, &w.flags.worker));
        if !stalled {
            return;
        }
        // The walk may have finished this instant: then its answer is in the inbox and the
        // request is left to it.
        let abandoned = self
            .docs
            .relink
            .as_ref()
            .is_some_and(|w| w.flags.abandon(&self.docs.stuck));
        if !abandoned {
            return;
        }
        let Some(walk) = self.docs.relink.take() else {
            return;
        };
        fail(
            &walk.pending.reply,
            walk.pending.id,
            ErrorCode::Io,
            format!(
                "walking the folder did not finish within {} s; its volume may have stopped answering",
                walk.pending.extra_ms.saturating_add(self.docs.timeout_ms) / 1000
            ),
        );
        self.log(
            LogLevel::Error,
            "a relink folder walk stalled; its thread was abandoned".to_owned(),
        );
    }

    /// A save or autosave that has not finished within the document timeout (a volume that
    /// stopped answering) is answered `io` and its writer thread abandoned, so the other
    /// shows go on saving (`SaveWriter::check_stalled`).
    pub(super) fn check_stalled_save(&mut self) {
        let now = self.config.stall_clock.now();
        let timeout = self.config.doc_timeout;
        let Some(writer) = &mut self.docs.writer else {
            return;
        };
        for message in writer.check_stalled(now, timeout) {
            self.log(LogLevel::Error, message);
        }
    }

    pub(super) fn poll_clears(&mut self) {
        let mut waiting = Vec::new();
        for rx in std::mem::take(&mut self.docs.clears) {
            match rx.try_recv() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => self.log(
                    LogLevel::Warn,
                    format!("could not remove the recovery file of dropped edits: {e}"),
                ),
                Err(TryRecvError::Empty) => waiting.push(rx),
                Err(TryRecvError::Disconnected) => {}
            }
        }
        self.docs.clears = waiting;
    }

    /// Whether a document request may start: only one read at a time engine-wide, and not
    /// while reading is stuck (`reads`: the request needs the document worker).
    fn read_gate(&self, id: RequestId, to: &ReplyTx, reads: bool) -> bool {
        if self.docs.pending.is_some() {
            fail(
                to,
                id,
                ErrorCode::CommandRefused,
                "another document request is still being handled",
            );
            return false;
        }
        !reads || self.stuck_gate(id, to)
    }

    /// Whether a `relink` walk may start: one at a time engine-wide (beside the document
    /// requests, which it never holds up), and not while reading is stuck.
    fn relink_gate(&self, id: RequestId, to: &ReplyTx) -> bool {
        if self.docs.relink.is_some() {
            fail(
                to,
                id,
                ErrorCode::CommandRefused,
                "another relink is still walking a folder",
            );
            return false;
        }
        self.stuck_gate(id, to)
    }

    /// `false` (answered `io`) while [`MAX_STUCK_WORKERS`] readers are stuck.
    fn stuck_gate(&self, id: RequestId, to: &ReplyTx) -> bool {
        if self.docs.stuck.load(Ordering::SeqCst) >= MAX_STUCK_WORKERS {
            fail(
                to,
                id,
                ErrorCode::Io,
                "reading files is stuck: a volume has stopped answering. Try again when it answers, or restart openLab",
            );
            return false;
        }
        true
    }

    fn next_ticket(&mut self, target: Target, id: RequestId, to: &ReplyTx) -> u64 {
        let pending = self.new_pending(target, id, to);
        let ticket = pending.ticket;
        self.docs.pending = Some(pending);
        ticket
    }

    /// A request with a fresh ticket (one counter for the document worker and the relink walk,
    /// so an answer is never taken for the other's).
    fn new_pending(&mut self, target: Target, id: RequestId, to: &ReplyTx) -> PendingDoc {
        self.docs.ticket += 1;
        PendingDoc {
            target,
            ticket: self.docs.ticket,
            id,
            reply: to.clone(),
            path: None,
            started_ms: self.config.clock.now_ms(),
            extra_ms: 0,
        }
    }

    fn send_job(&mut self, job: DocJob, id: RequestId, to: &ReplyTx) {
        if self.docs.jobs.send(job).is_err() {
            self.docs.pending = None;
            fail(
                to,
                id,
                ErrorCode::Internal,
                "the document worker has stopped",
            );
        }
    }

    /// The cue engine settings for doc `doc`: the loop's rate and block, and a seed of its own
    /// (the first doc's is the configured seed, so a one-doc run is as before).
    pub(super) fn engine_config(
        &self,
        doc: DocId,
    ) -> Result<EngineConfig, openlab_core::EngineConfigError> {
        EngineConfig::new(
            self.rate.get(),
            self.block,
            self.config.seed.wrapping_add(doc.0),
        )
    }

    /// A new workspace (WS-02) with fresh ids and a passcode from the OS (from the clock, with a
    /// warning, if the OS refused).
    fn fresh_workspace(&mut self) -> Workspace {
        let seed = os_seed().unwrap_or_else(|| {
            self.log(
                LogLevel::Warn,
                "the system random source failed: the new workspace's OSC passcode comes \
                 from the clock, so it is guessable; change it in the workspace settings"
                    .to_owned(),
            );
            clock_seed()
        });
        new_workspace(
            WorkspaceId(random_id()),
            CueListId(random_id()),
            PatchId(random_id()),
            openlab_core::NetworkPatchId(random_id()),
            seed,
        )
    }
}

fn reply_data(to: &ReplyTx, id: RequestId, data: ResultData) {
    reply(
        to,
        ServerMessage::Result {
            id,
            outcome: Outcome::Ok(OkBody {
                revision: None,
                data: Some(data),
            }),
        },
    );
}

impl Control {
    /// Opens another doc (WS-02; QLab 5 opens a workspace beside the ones open, which keep
    /// playing): `new`, `open` and `open_recovery` on any connection. The file is read and the
    /// doc built on the document worker; the doc is installed when the answer comes, and the
    /// result (`opened { doc }`) goes to `to` ([`Self::take_opened`] tells its [`DocId`] too).
    /// Refused (`command_refused`) while another document request is being handled, or when
    /// [`MAX_DOCS`] docs are open; a show file that is open already answers `already_open`.
    pub fn open_doc(&mut self, source: DocSource, id: RequestId, to: &ReplyTx) {
        let reads = matches!(source, DocSource::Open { .. } | DocSource::Recovery { .. });
        if !self.eng.read_gate(id, to, reads) {
            return;
        }
        if self.docs.len() >= MAX_DOCS {
            fail(
                to,
                id,
                ErrorCode::CommandRefused,
                format!("at most {MAX_DOCS} workspaces can be open at once"),
            );
            return;
        }
        if let DocSource::Open { path, .. } = &source
            && self.claimed(path, None)
        {
            fail(
                to,
                id,
                ErrorCode::AlreadyOpen,
                format!("{} is open already", path.display()),
            );
            return;
        }
        let others = self.claimed_paths(None);
        let config = match self.eng.engine_config(DocId(self.eng.next_doc)) {
            Ok(config) => config,
            Err(e) => {
                fail(to, id, ErrorCode::Internal, e.to_string());
                return;
            }
        };
        let job = match source {
            DocSource::Open { path, recovery } => {
                let ticket = self.eng.next_ticket(Target::New, id, to);
                if let Some(pending) = &mut self.eng.docs.pending {
                    pending.path = Some(path.clone());
                }
                DocJob::Open {
                    ticket,
                    path,
                    recovery,
                    config,
                    others,
                }
            }
            DocSource::Recovery {
                workspace_id,
                path_hash,
                modified_ms,
            } => {
                let ticket = self.eng.next_ticket(Target::New, id, to);
                DocJob::OpenRecovery {
                    ticket,
                    workspace: workspace_id,
                    path_hash,
                    modified_ms,
                    config,
                }
            }
            DocSource::New | DocSource::Workspace(_) => {
                let workspace = match source {
                    DocSource::Workspace(ws) => *ws,
                    _ => self.eng.fresh_workspace(),
                };
                let ticket = self.eng.next_ticket(Target::New, id, to);
                DocJob::Build {
                    ticket,
                    loaded: Box::new(Loaded {
                        workspace,
                        path: None,
                        dirty: false,
                        notices: Vec::new(),
                    }),
                    config,
                }
            }
        };
        self.eng.send_job(job, id, to);
    }

    /// The show files docs other than `except` hold or are writing (saves in flight), and the
    /// one an `open` in flight reads: no other doc may open or save over them (M4.15f).
    fn claimed_paths(&self, except: Option<DocId>) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = self
            .docs
            .iter()
            .filter(|d| Some(d.id) != except)
            .flat_map(|d| {
                d.show_path
                    .iter()
                    .chain(d.file.saves.iter().map(|s| &s.path))
                    .cloned()
            })
            .collect();
        paths.extend(self.eng.docs.pending.as_ref().and_then(|p| p.path.clone()));
        paths.extend(self.eng.docs.orphans.iter().map(|s| s.path.clone()));
        paths
    }

    /// Whether `path` is claimed ([`Self::claimed_paths`]) as spelled (compared as media paths
    /// are; another spelling of the same file is caught off this thread, by `file_key`).
    fn claimed(&self, path: &Path, except: Option<DocId>) -> bool {
        self.claimed_paths(except)
            .iter()
            .any(|p| same_path(p, path))
    }

    /// `save` and `save_as` of doc `doc`: refused with `already_open` when the target is a show
    /// file another doc holds or is writing, or one an `open` is reading; the writer checks again
    /// by the file's identity before it writes (a symlink or other spelling of the same file).
    pub(super) fn save(
        &mut self,
        doc: DocId,
        id: RequestId,
        request: Request,
        to: &ReplyTx,
    ) -> super::Flow {
        if let Request::SaveAs { path } = &request
            && self.claimed(Path::new(path), Some(doc))
        {
            fail(
                to,
                id,
                ErrorCode::AlreadyOpen,
                format!("{path} is open in another workspace"),
            );
            return super::Flow::Continue;
        }
        let others = self.claimed_paths(Some(doc));
        match self.cx_of(doc) {
            Some(mut cx) => cx.doc_request(id, request, to, others),
            None => fail(to, id, ErrorCode::NoWorkspace, "no workspace is open"),
        }
        super::Flow::Continue
    }

    /// `relink` of doc `doc` (WS-03, NFR-04): the folder is walked on a thread of its own, on a
    /// copy of the workspace, so the control thread does no disk I/O and a slow volume holds up
    /// nothing but this answer: `open`, `scan_recovery` and saves go on meanwhile. The matches
    /// are applied as one undoable edit when the walk is done ([`Self::relinked`]). Refused
    /// (`command_refused`) while another relink is walking.
    pub(super) fn relink(&mut self, doc: DocId, id: RequestId, folder: String, to: &ReplyTx) {
        if !self.eng.relink_gate(id, to) {
            return;
        }
        let Some(d) = self.docs.iter().find(|d| d.id == doc) else {
            fail(to, id, ErrorCode::NoWorkspace, "no workspace is open");
            return;
        };
        let (workspace, show_dir) = (
            Box::new(d.model.ws().clone()),
            d.show_path.as_deref().and_then(folder_of),
        );
        let limits = self.eng.config.relink_limits;
        let mut pending = self.eng.new_pending(Target::Relink(doc), id, to);
        pending.extra_ms = i64::try_from(limits.max_duration.as_millis()).unwrap_or(i64::MAX);
        let job = RelinkJob {
            ticket: pending.ticket,
            workspace,
            show_dir,
            folder: PathBuf::from(folder),
            limits,
        };
        let flags = Arc::new(WalkFlags::default());
        match self.eng.docs.spawn_relink(job, &flags) {
            Ok(()) => self.eng.docs.relink = Some(RelinkWalk { pending, flags }),
            Err(e) => fail(
                to,
                id,
                ErrorCode::Internal,
                format!("could not start the folder walk: {e}"),
            ),
        }
    }

    /// The walk is done: apply what it found to the doc, if it is still open.
    fn relinked(&mut self, doc: DocId, found: RelinkFound, id: RequestId, to: &ReplyTx) {
        match self.cx_of(doc) {
            Some(mut cx) => cx.apply_relink(found, id, to),
            None => fail(to, id, ErrorCode::NoWorkspace, "the workspace was closed"),
        }
    }

    /// `scan_recovery` (app-scope): the recovery files, read on the document worker.
    pub(super) fn scan_recovery(&mut self, id: RequestId, to: &ReplyTx) {
        if !self.eng.read_gate(id, to, true) {
            return;
        }
        let ticket = self.eng.next_ticket(Target::Engine, id, to);
        self.eng.send_job(DocJob::Scan { ticket }, id, to);
    }

    /// The document worker's (or the relink walk's) answer, for the request it is for (a doc
    /// being added, a scan, a relink).
    pub(super) fn doc_answer(&mut self, answer: DocAnswer) {
        let ticket = answer.ticket();
        let docs = &mut self.eng.docs;
        let Some(pending) = docs.pending.take_if(|p| p.ticket == ticket).or_else(|| {
            docs.relink
                .take_if(|w| w.pending.ticket == ticket)
                .map(|w| w.pending)
        }) else {
            // A stale answer (its request timed out): a doc built for it is dropped off the
            // control thread like a closed one.
            if let DocAnswer::Built { parts, .. } = answer {
                self.eng.docs.discard(parts);
            }
            return;
        };
        let (id, to) = (pending.id, &pending.reply);
        match (pending.target, answer) {
            (_, DocAnswer::Failed { code, message, .. }) => fail(to, id, code, message),
            (Target::Engine, DocAnswer::Entries { entries, .. }) => {
                reply_data(to, id, ResultData::RecoveryEntries { entries });
            }
            (Target::New, DocAnswer::Built { parts, .. }) => self.install_new(*parts, id, to),
            (Target::Relink(doc), DocAnswer::Relinked { found, .. }) => {
                self.relinked(doc, *found, id, to);
            }
            (
                Target::New,
                DocAnswer::Offer {
                    path,
                    workspace_name,
                    modified_ms,
                    ..
                },
            ) => reply_data(
                to,
                id,
                ResultData::RecoveryOffer {
                    path,
                    workspace_name,
                    modified_ms,
                },
            ),
            (_, answer) => {
                // Cannot happen: each job has its own kind of answer.
                if let DocAnswer::Built { parts, .. } = answer {
                    self.eng.docs.discard(parts);
                }
                fail(
                    to,
                    id,
                    ErrorCode::Internal,
                    "the document worker answered another request",
                );
            }
        }
    }

    /// Installs a doc built off the control thread: an insert, its probe jobs, its slot
    /// commands and its first reports, so it costs no audio block (ARCHITECTURE §3.1). The
    /// requester gets `opened { doc }`; a connection bound to the doc gets its snapshot.
    fn install_new(&mut self, parts: DocParts, id: RequestId, to: &ReplyTx) {
        if self.docs.len() >= MAX_DOCS {
            fail(
                to,
                id,
                ErrorCode::CommandRefused,
                format!("at most {MAX_DOCS} workspaces can be open at once"),
            );
            self.eng.docs.discard(Box::new(parts));
            return;
        }
        // Another doc may have been saved to this path while it was read.
        if let Some(path) = &parts.path
            && self.claimed(path, None)
        {
            fail(
                to,
                id,
                ErrorCode::AlreadyOpen,
                format!("{} is open already", path.display()),
            );
            self.eng.docs.discard(Box::new(parts));
            return;
        }
        let Some(doc_id) = self.eng.take_doc_id() else {
            fail(
                to,
                id,
                ErrorCode::CommandRefused,
                "no more workspaces can be opened in this run",
            );
            self.eng.docs.discard(Box::new(parts));
            return;
        };
        let DocParts {
            mut model,
            path,
            mut dirty,
            notices,
            actions,
            indexed,
            checks,
        } = parts;
        // A copy of an open show (a Finder duplicate) has its workspace id: QLab 5 gives the
        // copy a fresh one and marks it unsaved. Ids must be unique: autosaves, OSC
        // `/workspace/{id}` and the app's windows go by them.
        let duplicate = self.docs.iter().any(|d| d.model.ws().id == model.ws().id);
        if duplicate {
            // The engine's first call saw the old id; the edit tells it.
            model.edit().id = WorkspaceId(random_id());
            dirty = true;
        }
        // The device may have come back with another block size while the doc was built.
        if model.engine().config().lookahead() != self.eng.block
            && let Err(e) = model.set_lookahead(self.eng.block)
        {
            self.eng.log(
                LogLevel::Error,
                format!("cue engine lookahead not set: {e}"),
            );
        }
        let show_dir = path.as_deref().and_then(folder_of);
        let mut media = MediaCatalog::new(
            self.eng.pool.handle(doc_id),
            show_dir,
            self.eng.config.preload_budget,
        );
        media.sync_indexed(indexed);
        // The checks walked the workspace already, and no file has answered since: what the
        // catalog noted for them is in them.
        let _ = media.take_plan_changes();
        let mut doc = Doc::new(&self.eng, doc_id, model, path, media);
        doc.checks = checks;
        doc.file.notices = notices;
        doc.revision = 1;
        doc.saved_revision = if dirty { 0 } else { 1 };
        self.docs.push(doc);
        self.docs_changed();
        let Some(mut cx) = self.cx_of(doc_id) else {
            return;
        };
        cx.log_kind(
            LogKind::Document,
            LogLevel::Info,
            match &cx.doc.show_path {
                Some(p) => format!("workspace opened: {}", p.display()),
                None => "workspace opened: untitled".to_owned(),
            },
        );
        if duplicate {
            cx.log(
                LogLevel::Info,
                "a workspace with the same id is open already (a copy of a show?): this one \
                 gets a new id and is unsaved"
                    .to_owned(),
            );
        }
        let now = cx.now();
        for input in cx.execute(&actions, now) {
            cx.feed(input, now);
        }
        let mut inputs = vec![Input::Tick];
        inputs.extend(cx.sync_workspace(true));
        for input in inputs {
            cx.feed(input, now);
        }
        cx.doc.warnings_dirty = false;
        cx.refresh_warnings();
        reply_data(to, id, ResultData::Opened { doc: doc_id });
        self.eng.opened.push((id, doc_id));
        self.publish_workspaces();
        self.eng.mixer.pump();
    }

    /// Closes doc `doc` (WS-02): refused with `unsaved_changes` while it has unsaved edits,
    /// unless `discard_changes` (then its autosave is removed, after any write of it still in
    /// flight). Its cues stop at once (Panic twice: a hard stop), its voices' late events are
    /// ignored from now on, its mixer slots are zeroed and freed, and it is dropped off the
    /// control thread. The other docs play on untouched. Its clients get `closed { doc }`.
    pub fn close_doc(&mut self, doc: DocId, discard_changes: bool) -> Result<(), ErrorBody> {
        self.remove_doc(doc, discard_changes)?;
        self.closed(doc);
        Ok(())
    }

    /// Tells everyone doc `doc` is gone: its connections get `closed` (the network layer then
    /// closes them), the app connections the list without it. After the reply to a `close`, so
    /// the requester has its result before its connection ends.
    pub(super) fn closed(&mut self, doc: DocId) {
        self.eng.announce_closed(doc);
        self.publish_workspaces();
    }

    /// [`Self::close_doc`] without telling anyone.
    pub(super) fn remove_doc(
        &mut self,
        doc: DocId,
        discard_changes: bool,
    ) -> Result<(), ErrorBody> {
        let Some(index) = self.docs.iter().position(|d| d.id == doc) else {
            return Err(ErrorBody::new(
                ErrorCode::NoWorkspace,
                format!("workspace {doc} is not open"),
            ));
        };
        {
            let Some(mut cx) = self.cx_of(doc) else {
                return Err(ErrorBody::new(
                    ErrorCode::NoWorkspace,
                    format!("workspace {doc} is not open"),
                ));
            };
            if cx.doc.is_dirty() && !discard_changes {
                return Err(ErrorBody::new(
                    ErrorCode::UnsavedChanges,
                    "the workspace has unsaved changes; save them or discard them",
                ));
            }
            if cx.doc.is_dirty() {
                let (id, path) = (cx.doc.model.ws().id, cx.doc.show_path.clone());
                cx.clear_autosave_of(id, path);
            }
            cx.stop_everything();
            cx.log_kind(
                LogKind::Document,
                LogLevel::Info,
                match &cx.doc.show_path {
                    Some(p) => format!("workspace closed: {}", p.display()),
                    None => "workspace closed: untitled".to_owned(),
                },
            );
        }
        let mut closed = self.docs.remove(index);
        self.docs_changed();
        // Dated after the stops the panic sent (at `now` too): the slots go silent as the
        // voices on them end.
        let now = self.eng.now();
        let commands = self
            .eng
            .patches
            .release_doc(doc)
            .into_iter()
            .map(|c| (now, c))
            .collect();
        self.eng.send_commands(commands);
        // Its slots may be a waiting patch's in another doc.
        self.mark_warnings_dirty();
        self.eng.voice_ids.retain(|_, owner| owner != doc);
        self.eng.observe.reset(doc);
        self.eng.watch.leave(doc);
        self.eng.pool.leave(doc);
        self.eng.waveform_reader.forget(doc);
        self.eng.mapping_warned.retain(|(d, _, _, _)| *d != doc);
        // Saves under way still finish on the writer and are answered then.
        self.eng
            .docs
            .orphans
            .extend(std::mem::take(&mut closed.file.saves));
        self.eng.docs.discard(Box::new(closed));
        self.eng.mixer.pump();
        Ok(())
    }

    /// The number of open docs changed.
    fn docs_changed(&mut self) {
        self.eng.open_docs = self.docs.len();
        // A closed doc's primes are back in the pool; a new doc asks for its share at its
        // first priming pass (`balance_primes` makes the others follow, only if they must).
        let open: Vec<DocId> = self.docs.iter().map(|d| d.id).collect();
        self.eng.primes.retain(|d, _| open.contains(d));
        self.eng.primes_changed = true;
    }

    /// Saves of closed docs that finished: answered (and a failure logged).
    pub(super) fn poll_orphan_saves(&mut self) {
        if self.eng.docs.orphans.is_empty() {
            return;
        }
        for (save, result) in take_finished(&mut self.eng.docs.orphans) {
            match result {
                Ok(_) => {
                    for (id, to) in &save.waiters {
                        reply(to, ok(*id, None));
                    }
                }
                Err(e) => {
                    self.eng.log_kind(
                        LogKind::Document,
                        LogLevel::Error,
                        format!("save failed: {e}"),
                    );
                    for (id, to) in &save.waiters {
                        fail(to, *id, persist_code(&e), e.to_string());
                    }
                }
            }
        }
    }
}

/// Takes the saves in `saves` that finished, leaving those still under way. A superseded save
/// is not returned: its waiters move to the newest save of the same file that holds at least
/// its edits, still waiting or finished in this same pass (so every answer is collected before
/// any is forwarded), and are failed only when there is none.
fn take_finished(
    saves: &mut Vec<PendingSave>,
) -> Vec<(PendingSave, Result<SaveReport, PersistError>)> {
    let mut finished = Vec::new();
    let mut superseded = Vec::new();
    for save in std::mem::take(saves) {
        let result = match save.rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => {
                saves.push(save);
                continue;
            }
            Err(TryRecvError::Disconnected) => Err(PersistError::WriterGone),
        };
        if matches!(result, Err(PersistError::Superseded)) {
            superseded.push(save);
        } else {
            finished.push((save, result));
        }
    }
    for mut old in superseded {
        let newer = saves
            .iter_mut()
            .chain(finished.iter_mut().map(|(p, _)| p))
            .filter(|p| p.revision >= old.revision && p.path == old.path)
            .max_by_key(|p| p.revision);
        match newer {
            Some(newer) => newer.waiters.append(&mut old.waiters),
            None => {
                for (id, to) in &old.waiters {
                    fail(to, *id, ErrorCode::Internal, "the save was superseded");
                }
            }
        }
    }
    finished
}

impl Doc {
    pub(super) fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }
}

/// What the final recovery writes at shutdown came to ([`Control::shutdown`]), counted in docs
/// with unsaved changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FinalWrites {
    /// Their recovery file holds their last edit (written now, or by an earlier autosave).
    pub written: usize,
    /// The write failed, or there is nowhere to write (no app-support folder).
    pub failed: usize,
    /// Still being written when the wait ran out (a slow or hung disk). The writer thread goes
    /// on with it; the previous autosave, if any, is the newest copy on disk until it lands.
    pub unfinished: usize,
}

impl Control {
    /// Writes the recovery file of every doc with unsaved changes whose last edit no autosave
    /// holds yet, and waits for them (and for an autosave of the last edit already under way),
    /// in all at most `wait`. The writer discards autosaves still queued when it is dropped,
    /// so this must run before the docs go. A write that fails (a transient disk error) is
    /// tried once more while the wait lasts, since the previous autosave lacks the last edits.
    /// Each failure is logged; a doc whose file was not written in time is logged too, so the
    /// next start's recovery offer is not a surprise.
    pub(super) fn final_recovery_writes(&mut self, wait: Duration) -> FinalWrites {
        let mut counts = FinalWrites::default();
        // Doc index, name, the write's answer, whether it is the second try.
        let mut waiting: VecDeque<(usize, String, AutosaveRx, bool)> = VecDeque::new();
        let mut logs_early: Vec<String> = Vec::new();
        for (index, doc) in self.docs.iter_mut().enumerate() {
            if !doc.is_dirty() {
                continue;
            }
            let name = doc.model.ws().name.clone();
            let file = &mut doc.file;
            let current = file.autosaved_revision == Some(doc.revision);
            match (file.autosave.take(), current) {
                (Some(rx), true) => waiting.push_back((index, name, rx, false)),
                (None, true) => counts.written += 1,
                (_, false) => match &self.eng.docs.writer {
                    Some(writer) => {
                        let rx = writer.autosave(
                            doc.model.ws().clone(),
                            doc.show_path.clone(),
                            doc.revision,
                        );
                        file.autosaved_revision = Some(doc.revision);
                        waiting.push_back((index, name, rx, false));
                    }
                    None => {
                        counts.failed += 1;
                        logs_early.push(name);
                    }
                },
            }
        }
        // One show's write stuck on a slow volume must not hold up the others'.
        if let Some(writer) = &mut self.eng.docs.writer {
            writer.add_spare();
        }
        let deadline = Instant::now() + wait;
        let mut logs: Vec<(LogLevel, String)> = logs_early
            .into_iter()
            .map(|name| {
                (
                    LogLevel::Error,
                    format!(
                        "the unsaved changes of \"{name}\" are lost: no application-support folder to write a recovery file to"
                    ),
                )
            })
            .collect();
        while let Some((index, name, rx, retried)) = waiting.pop_front() {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                // `None`: a manual save of that revision or a later one made it durable.
                Ok(Ok(_)) => counts.written += 1,
                Ok(Err(e))
                    if !retried
                        && Instant::now() < deadline
                        && let (Some(writer), Some(doc)) =
                            (&self.eng.docs.writer, self.docs.get(index)) =>
                {
                    logs.push((
                        LogLevel::Warn,
                        format!(
                            "could not write the recovery file of \"{name}\" at shutdown: {e}; trying once more"
                        ),
                    ));
                    let rx = writer.autosave(
                        doc.model.ws().clone(),
                        doc.show_path.clone(),
                        doc.revision,
                    );
                    waiting.push_back((index, name, rx, true));
                }
                Ok(Err(e)) => {
                    counts.failed += 1;
                    logs.push((
                        LogLevel::Error,
                        format!("could not write the recovery file of \"{name}\" at shutdown: {e}"),
                    ));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    counts.failed += 1;
                    logs.push((
                        LogLevel::Error,
                        format!(
                            "could not write the recovery file of \"{name}\" at shutdown: {}",
                            PersistError::WriterGone
                        ),
                    ));
                }
                Err(RecvTimeoutError::Timeout) => {
                    counts.unfinished += 1;
                    logs.push((
                        LogLevel::Error,
                        format!(
                            "the recovery file of \"{name}\" was still being written when the \
                             engine stopped (slow or hung disk); its latest edits may be lost"
                        ),
                    ));
                }
            }
        }
        if counts.failed == 0 && counts.unfinished == 0 && counts.written > 0 {
            logs.push((
                LogLevel::Info,
                format!(
                    "shutdown: recovery files hold the unsaved changes of {} workspace(s)",
                    counts.written
                ),
            ));
        }
        for (level, message) in logs {
            self.eng.log(level, message);
        }
        counts
    }
}

impl Control {
    /// Answers the document workers have posted so far (tests wait for one to be in the inbox).
    pub fn doc_answers_posted(&self) -> u64 {
        self.eng.docs.answered.load(Ordering::SeqCst)
    }

    /// Document workers abandoned in a stalled read that have not returned yet.
    pub fn stuck_doc_workers(&self) -> usize {
        self.eng.docs.stuck.load(Ordering::SeqCst)
    }

    /// Requests, saves and autosaves still being worked on, the engine's and every doc's.
    pub fn docs_busy(&self) -> usize {
        self.eng.docs.busy() + self.docs.iter().map(|d| d.file.busy()).sum::<usize>()
    }

    /// The show file the first doc's workspace is saved to, if it has one.
    pub fn show_path(&self) -> Option<&Path> {
        self.docs.first().and_then(|d| d.show_path.as_deref())
    }

    /// Whether the first doc has unsaved edits.
    pub fn is_unsaved(&self) -> bool {
        self.docs.first().is_some_and(Doc::is_dirty)
    }

    /// The first doc's runtime sequence.
    pub fn runtime_seq(&self) -> u64 {
        self.docs.first().map_or(0, |d| d.runtime_seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A walk that finishes as its timeout passes is counted stuck by exactly one side, and
    /// taken back by its thread when the stall check won: both orders leave the count at zero.
    #[test]
    fn a_walk_finishing_at_its_timeout_never_leaves_the_stuck_count_up() {
        let stuck = AtomicUsize::new(0);
        // The walk finishes first: the stall check finds it done and counts nothing.
        let flags = WalkFlags::default();
        flags.finish(&stuck);
        assert!(!flags.abandon(&stuck));
        assert_eq!(stuck.load(Ordering::SeqCst), 0);
        // The stall check gives up first: counted stuck until the thread returns.
        let flags = WalkFlags::default();
        assert!(flags.abandon(&stuck));
        assert_eq!(stuck.load(Ordering::SeqCst), 1);
        flags.finish(&stuck);
        assert_eq!(stuck.load(Ordering::SeqCst), 0);
    }

    /// The seed is injected, so a new workspace's passcode is the same for the same seed.
    #[test]
    fn a_new_workspace_gets_the_default_access_from_the_seed() {
        let make = |seed| {
            new_workspace(
                WorkspaceId(uuid::Uuid::from_u128(1)),
                CueListId(uuid::Uuid::from_u128(2)),
                PatchId(uuid::Uuid::from_u128(3)),
                openlab_core::NetworkPatchId(uuid::Uuid::from_u128(4)),
                seed,
            )
        };
        let access = make(4821).settings.osc_access;
        assert_eq!(access, openlab_core::OscAccess::new_workspace(4821));
        assert_eq!(access.passcodes[0].passcode, "4821");
        assert_eq!(make(4821), make(4821));
        assert_ne!(make(1), make(2));
    }

    /// A superseded save whose newer save finished in the same pass is answered by it, whichever
    /// order the two are listed in (closed docs' saves used to fail it as "superseded").
    #[test]
    fn a_superseded_save_is_answered_by_a_newer_one_finished_in_the_same_pass() {
        for newer_first in [false, true] {
            let (to, mut replies) = super::super::inbox::reply_channel();
            let save = |revision, result: Result<SaveReport, PersistError>| {
                let (tx, rx) = std::sync::mpsc::channel();
                tx.send(result).unwrap();
                PendingSave {
                    rx,
                    revision,
                    path: PathBuf::from("/s.openlab"),
                    rebase: false,
                    respelled: false,
                    waiters: vec![(RequestId(revision), to.clone())],
                }
            };
            let old = save(1, Err(PersistError::Superseded));
            let new = save(2, Ok(SaveReport::default()));
            let mut saves = if newer_first {
                vec![new, old]
            } else {
                vec![old, new]
            };
            let finished = take_finished(&mut saves);
            assert!(saves.is_empty());
            assert_eq!(finished.len(), 1);
            let (newer, result) = &finished[0];
            assert!(result.is_ok());
            let ids: Vec<_> = newer.waiters.iter().map(|(id, _)| id.0).collect();
            assert_eq!(ids, [2, 1]);
            assert!(replies.try_recv().is_err(), "nothing failed");
        }
    }
}
