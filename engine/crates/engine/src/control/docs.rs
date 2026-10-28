// SPDX-License-Identifier: GPL-3.0-or-later

//! Documents (M4.4, docs/control.md "Documents"): new, open, open-recovery, save, save-as,
//! revert and autosave.
//!
//! Nothing here touches the disk on the control thread. Saves and autosaves go to the
//! [`SaveWriter`] thread (the control thread clones the workspace and polls for the answer each
//! tick); reading a show, the recovery check and the recovery scan run on one document worker
//! thread, whose answer comes back through the inbox ([`DocAnswer`]) and is applied here.
//! Replacing the workspace (new, open, revert) stops every running cue first and keeps
//! `revision` and `runtime_seq` rising, then sends the new `snapshot` to the requester and,
//! through the outbound channel, to every other client.
//!
//! Several docs (M4.15e; ARCHITECTURE §3.1): [`Control::open_doc`] adds a doc. The document
//! worker reads the file (or takes the workspace given) and builds everything the doc needs off
//! the control thread ([`DocParts`]: the workspace, its cue engine with its static problems
//! worked out, the media catalog's cue index), so installing it on the control thread is an
//! insert, the probe jobs and the slot commands. [`Control::close_doc`] stops that doc's cues
//! (Panic twice, a hard stop), frees its mixer slots, clears its autosave when its edits are
//! discarded, and hands the doc to the worker to be dropped there (its preloaded media can be
//! hundreds of megabytes). One read runs at a time engine-wide, and at most [`MAX_DOCS`] docs
//! are open; either limit is `command_refused`.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use crossbeam_channel::Sender;
use openlab_audio::{CommandKind, Fade};
use openlab_core::protocol::{
    ErrorBody, ErrorCode, LogKind, LogLevel, OkBody, Outcome, RecoveryChoice, RecoveryEntryInfo,
    Request, RequestId, ResultData, ServerMessage, WireWarning,
};
use openlab_core::{
    Action, CueEngine, CueListId, EngineConfig, History, Input, LoadedShow, MAX_SHOW_FILE_BYTES,
    MediaRef, PatchId, PathStyle, Relink, Workspace, WorkspaceId, audio_media, autosave_due,
    comparison_key, is_absolute, load_show, portable_ref, relink_command, to_portable,
};

use super::checks::{NoProbes, PlanChecks};
use super::inbox::reply;
use super::media::{Indexed, MediaCatalog, index_workspace};
use super::{
    Control, ControlConfig, ControlError, Doc, DocCx, DocId, Engine, Inbox, MAX_DOCS, ReplyTx,
    Scope, notice, ok,
};
use crate::persist::{
    AppDirs, JobKind, MAX_RECOVERY_ENTRIES, PersistError, RecoveryCheck, SaveReport, SaveWriter,
    check_recovery, media_path, read_text, scan_recovery,
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

/// Called on the document worker at the start of each read (tests hold it to simulate a stalled
/// volume).
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
    /// A revert: the autosave of the edits just dropped goes (after any write of it in flight).
    clear_autosave: bool,
}

/// Everything a doc being opened needs, built on the document worker (ARCHITECTURE §3.1).
pub struct DocParts {
    loaded: Loaded,
    /// A cue engine that has followed the workspace once (its static problems and playheads
    /// worked out) ...
    engine: CueEngine,
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
            .field("loaded", &self.loaded)
            .finish_non_exhaustive()
    }
}

/// Builds a doc's parts from a workspace read (on the document worker).
fn build_parts(loaded: Loaded, config: EngineConfig) -> DocParts {
    let mut engine = CueEngine::new(config);
    engine.model_changed();
    // Sample 0: nothing runs yet, so what this decides (problems, playheads) does not depend
    // on the time; the control thread hands the engine the real time from its next call on.
    let actions = engine.handle(&loaded.workspace, Input::Tick, 0);
    let indexed = index_workspace(&loaded.workspace);
    let mut checks = PlanChecks::default();
    checks.check_all(&loaded.workspace, &NoProbes, config.sample_rate());
    DocParts {
        loaded,
        engine,
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
    Failed {
        ticket: u64,
        code: ErrorCode,
        message: String,
    },
}

impl DocAnswer {
    fn ticket(&self) -> u64 {
        match self {
            Self::Loaded { ticket, .. }
            | Self::Built { ticket, .. }
            | Self::Offer { ticket, .. }
            | Self::Entries { ticket, .. }
            | Self::Failed { ticket, .. } => *ticket,
        }
    }
}

enum DocJob {
    Open {
        ticket: u64,
        path: PathBuf,
        recovery: Option<RecoveryChoice>,
        /// A doc to add: its parts are built from what is read, with this cue engine config.
        build: Option<EngineConfig>,
    },
    /// A doc to add from a workspace already in hand.
    Build {
        ticket: u64,
        loaded: Box<Loaded>,
        config: EngineConfig,
    },
    /// A closed doc, dropped here rather than on the control thread.
    Discard(Box<dyn Send>),
    Revert {
        ticket: u64,
        path: PathBuf,
    },
    OpenRecovery {
        ticket: u64,
        workspace: WorkspaceId,
        path_hash: Option<String>,
        modified_ms: Option<i64>,
    },
    Scan {
        ticket: u64,
    },
}

/// Which doc a document request is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// An open doc (its workspace replaced, or a scan it asked for).
    Doc(DocId),
    /// A doc to add ([`Control::open_doc`]).
    New,
}

/// What the request being worked on by the document worker is.
struct PendingDoc {
    target: Target,
    ticket: u64,
    id: RequestId,
    reply: ReplyTx,
    discard: bool,
    /// When the request was handed to the worker ([`ControlConfig::clock`], milliseconds).
    started_ms: i64,
}

struct PendingSave {
    rx: Receiver<Result<SaveReport, PersistError>>,
    revision: u64,
    path: PathBuf,
    /// The document generation it was started in; a save finishing after another workspace was
    /// installed says nothing about the new one.
    epoch: u64,
    /// A save-as to another folder: relative media references were rebased in the copy saved.
    rebase: bool,
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
    epoch: u64,
    /// Notices about how the open workspace was loaded (repairs, recovery), shown with the
    /// workspace-level warnings until another workspace replaces it.
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
            epoch: 0,
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

    /// The read, the autosave removals and closed docs' saves in flight (a doc's saves:
    /// [`DocFile::busy`]).
    pub(super) fn busy(&self) -> usize {
        usize::from(self.pending.is_some()) + self.clears.len() + self.orphans.len()
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
        let answer = match job {
            DocJob::Open {
                ticket,
                path,
                recovery,
                build,
            } => match (open(ticket, &path, recovery, dirs), build) {
                (DocAnswer::Loaded { ticket, loaded }, Some(config)) => DocAnswer::Built {
                    ticket,
                    parts: Box::new(build_parts(*loaded, config)),
                },
                (answer, _) => answer,
            },
            DocJob::Build {
                ticket,
                loaded,
                config,
            } => DocAnswer::Built {
                ticket,
                parts: Box::new(build_parts(*loaded, config)),
            },
            DocJob::Discard(_) => continue,
            DocJob::Revert { ticket, path } => revert(ticket, &path),
            DocJob::OpenRecovery {
                ticket,
                workspace,
                path_hash,
                modified_ms,
            } => open_recovery(ticket, workspace, path_hash.as_deref(), modified_ms, dirs),
            DocJob::Scan { ticket } => scan(ticket, dirs),
        };
        // The control loop gave up on the request while this was reading: its answer is stale
        // (it has another ticket), and this thread leaves the work to the replacement.
        let gone = flags.abandoned.load(Ordering::SeqCst);
        let sent = inbox.send(Inbox::Doc(answer)).is_ok();
        // Not busy before the answer counts: whoever sees the count can rely on `busy` being
        // false already (the answer itself is in the inbox from the send above).
        flags.busy.store(false, Ordering::SeqCst);
        answered.fetch_add(1, Ordering::SeqCst);
        if gone {
            // After the send: a count of zero means the stale answer is already in the inbox.
            stuck.fetch_sub(1, Ordering::SeqCst);
        }
        if !sent || gone {
            break;
        }
    }
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
            clear_autosave: false,
        }),
    }
}

fn revert(ticket: u64, path: &Path) -> DocAnswer {
    let shown = match read_show(path) {
        Ok(shown) => shown,
        Err((code, message)) => return failed(ticket, code, message),
    };
    DocAnswer::Loaded {
        ticket,
        loaded: Box::new(Loaded {
            notices: load_notices(&shown),
            workspace: shown.workspace,
            path: Some(path.to_path_buf()),
            dirty: false,
            // The operator dropped the edits on purpose: their autosave must not come back
            // (the control loop queues the removal behind any write of it).
            clear_autosave: true,
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
            clear_autosave: false,
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

fn fail(to: &ReplyTx, id: RequestId, code: ErrorCode, message: impl Into<String>) {
    reply(
        to,
        ServerMessage::Result {
            id,
            outcome: Outcome::Error(ErrorBody::new(code, message)),
        },
    );
}

fn persist_code(e: &PersistError) -> ErrorCode {
    match e {
        PersistError::Io { .. } | PersistError::WriterGone => ErrorCode::Io,
        _ => ErrorCode::Internal,
    }
}

/// Ids for a new workspace, from the OS (core draws no randomness).
fn random_id() -> uuid::Uuid {
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
    seed: u32,
) -> Workspace {
    let mut workspace = Workspace::new_empty(id, list, patch);
    workspace.settings.osc_access = openlab_core::OscAccess::new_workspace(seed);
    workspace
}

/// Relative media references of `ws` re-spelled so they name the same files from `new_dir`.
fn rebase(ws: &Workspace, old_dir: Option<&Path>, new_dir: &Path) -> Vec<Relink> {
    let Some(new_dir) = new_dir.to_str() else {
        return Vec::new();
    };
    audio_media(ws)
        .into_iter()
        .filter(|(_, media)| !is_absolute(media.as_str()))
        .filter_map(|(cue, media)| {
            let file = media_path(&media, old_dir)?;
            let new: MediaRef = portable_ref(file.to_str()?, Some(new_dir), PathStyle::host());
            (new != media).then_some(Relink { cue, new })
        })
        .collect()
}

/// Whether two paths name the same file, compared like media paths (case, NFC and `..`
/// ignored): the same show reopened under another spelling is still the same file.
fn same_path(a: &Path, b: &Path) -> bool {
    let key = |p: &Path| comparison_key(&to_portable(&p.to_string_lossy(), PathStyle::host()));
    key(a) == key(b)
}

fn folder_of(path: &Path) -> Option<PathBuf> {
    path.parent().map(Path::to_path_buf)
}

impl DocCx<'_> {
    /// A document request (everything in `Request` but `snapshot`, `list_devices`, `relink`).
    pub(super) fn doc_request(&mut self, id: RequestId, request: Request, to: &ReplyTx) {
        match request {
            Request::Save => match self.doc.show_path.clone() {
                Some(path) => self.start_save(id, to, path),
                None => fail(
                    to,
                    id,
                    ErrorCode::NoWorkspace,
                    "the workspace has never been saved; use save_as",
                ),
            },
            Request::SaveAs { path } => self.start_save(id, to, PathBuf::from(path)),
            Request::ScanRecovery => {
                if !self.doc_gate(id, to, true, true) {
                    return;
                }
                let ticket = self.next_ticket(id, to, false);
                self.send_job(DocJob::Scan { ticket }, id, to);
            }
            Request::New { discard_changes } => {
                if !self.doc_gate(id, to, discard_changes, false) {
                    return;
                }
                let workspace = self.eng.fresh_workspace();
                let loaded = Loaded {
                    workspace,
                    path: None,
                    dirty: false,
                    notices: Vec::new(),
                    clear_autosave: false,
                };
                self.install(loaded, discard_changes);
                self.finish_install(id, to);
            }
            Request::Open {
                path,
                discard_changes,
                recovery,
            } => {
                if !self.doc_gate(id, to, discard_changes, true) {
                    return;
                }
                let ticket = self.next_ticket(id, to, discard_changes);
                let job = DocJob::Open {
                    ticket,
                    path: PathBuf::from(path),
                    recovery,
                    build: None,
                };
                self.send_job(job, id, to);
            }
            Request::OpenRecovery {
                workspace_id,
                path_hash,
                modified_ms,
                discard_changes,
            } => {
                if !self.doc_gate(id, to, discard_changes, true) {
                    return;
                }
                let ticket = self.next_ticket(id, to, discard_changes);
                let job = DocJob::OpenRecovery {
                    ticket,
                    workspace: workspace_id,
                    path_hash,
                    modified_ms,
                };
                self.send_job(job, id, to);
            }
            Request::Revert { discard_changes } => {
                let Some(path) = self.doc.show_path.clone() else {
                    fail(
                        to,
                        id,
                        ErrorCode::NoWorkspace,
                        "the workspace has never been saved; there is nothing to revert to",
                    );
                    return;
                };
                if !self.doc_gate(id, to, discard_changes, true) {
                    return;
                }
                let ticket = self.next_ticket(id, to, discard_changes);
                self.send_job(DocJob::Revert { ticket, path }, id, to);
            }
            Request::Snapshot
            | Request::ListDevices
            | Request::SetOutputOverrides { .. }
            | Request::PreshowCheck
            | Request::LogTail { .. }
            | Request::Relink { .. } => {}
        }
    }

    /// Whether a request that replaces the workspace (or reads files) may start: only one at a
    /// time, and not over unsaved edits unless the request discards them. `reads`: the request
    /// needs the document worker.
    fn doc_gate(&self, id: RequestId, to: &ReplyTx, discard: bool, reads: bool) -> bool {
        if !self.eng.read_gate(id, to, reads) {
            return false;
        }
        if self.doc.is_dirty() && !discard {
            fail(
                to,
                id,
                ErrorCode::UnsavedChanges,
                "the workspace has unsaved changes; save them or discard them",
            );
            return false;
        }
        true
    }

    fn next_ticket(&mut self, id: RequestId, to: &ReplyTx, discard: bool) -> u64 {
        let target = Target::Doc(self.doc.id);
        self.eng.next_ticket(target, id, to, discard)
    }

    fn send_job(&mut self, job: DocJob, id: RequestId, to: &ReplyTx) {
        self.eng.send_job(job, id, to);
    }

    /// The document worker's answer to a request of this doc.
    fn doc_answer(&mut self, pending: PendingDoc, answer: DocAnswer) {
        let (id, to) = (pending.id, &pending.reply);
        match answer {
            DocAnswer::Failed { code, message, .. } => fail(to, id, code, message),
            DocAnswer::Offer {
                path,
                workspace_name,
                modified_ms,
                ..
            } => reply_data(
                to,
                id,
                ResultData::RecoveryOffer {
                    path,
                    workspace_name,
                    modified_ms,
                },
            ),
            DocAnswer::Entries { entries, .. } => {
                reply_data(to, id, ResultData::RecoveryEntries { entries });
            }
            // Cannot happen: only an added doc's request builds parts.
            DocAnswer::Built { .. } => fail(
                to,
                id,
                ErrorCode::Internal,
                "the document worker answered another request",
            ),
            DocAnswer::Loaded { loaded, .. } => {
                // The operator may have edited while the file was being read.
                if self.doc.is_dirty() && !pending.discard {
                    fail(
                        to,
                        id,
                        ErrorCode::UnsavedChanges,
                        "the workspace changed while the file was being read; nothing was opened",
                    );
                    return;
                }
                self.install(*loaded, pending.discard);
                self.finish_install(id, to);
            }
        }
    }

    /// Stops everything that is running, as Panic pressed twice does, and lets go of every
    /// voice, prime and queued feedback: they belong to the workspace about to be replaced.
    fn stop_everything(&mut self) {
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

    /// Replaces the workspace: stops the show, rebuilds the cue engine, history, media catalog
    /// and patches around the new one, and keeps `revision` and `runtime_seq` rising.
    /// `discard`: the operator agreed to drop unsaved edits.
    fn install(&mut self, loaded: Loaded, discard: bool) {
        let Loaded {
            workspace,
            path,
            dirty,
            notices,
            clear_autosave,
        } = loaded;
        let same_file = self.doc.ws.id == workspace.id
            && match (&self.doc.show_path, &path) {
                (Some(a), Some(b)) => same_path(a, b),
                (None, None) => true,
                _ => false,
            };
        // The autosave of dropped edits goes, after any write of it still in flight (the writer
        // runs the removal after its current job). Not when the same file is reopened: its
        // autosave may be the one just restored.
        if (discard && self.doc.is_dirty() && !same_file) || clear_autosave {
            let (id, from) = if clear_autosave {
                (workspace.id, path.clone())
            } else {
                (self.doc.ws.id, self.doc.show_path.clone())
            };
            self.clear_autosave_of(id, from);
        }
        self.stop_everything();
        self.doc.file.epoch += 1;
        // A new engine: its run ids restart, which cannot meet the old ones (the voices are
        // forgotten and mixer voice ids are never reused).
        match self.eng.engine_config(self.doc.id) {
            Ok(config) => {
                self.doc.engine = openlab_core::CueEngine::new(config);
                // The new engine counts unsignalled changes from zero.
                self.doc.seen_unsignalled = 0;
            }
            // Cannot happen (the same values built the first engine); the old engine, with
            // everything stopped, is the safe thing to keep.
            Err(e) => self.log(LogLevel::Error, format!("cue engine not rebuilt: {e}")),
        }
        self.doc.ws = workspace;
        self.doc.history = History::new();
        self.doc.checks = super::checks::PlanChecks::default();
        self.doc.show_path = path;
        self.eng.observe.reset(self.doc.id);
        self.log_kind(
            LogKind::Document,
            LogLevel::Info,
            match &self.doc.show_path {
                Some(p) => format!("workspace opened: {}", p.display()),
                None => "workspace opened: untitled".to_owned(),
            },
        );
        self.doc.media.reset(self.show_dir(), &self.doc.ws);
        self.doc.file.notices = notices;
        self.doc.revision += 1;
        self.doc.saved_revision = if dirty {
            self.doc.revision.saturating_sub(1)
        } else {
            self.doc.revision
        };
        self.doc.file.dirty_since = None;
        self.doc.file.last_autosave = None;
        self.doc.file.autosaved_revision = None;
        let now = self.now();
        self.doc.engine.model_changed();
        let mut inputs = vec![Input::Tick];
        inputs.extend(self.sync_workspace(true));
        for input in inputs {
            self.feed(input, now);
        }
        self.doc.warnings_dirty = false;
        self.refresh_warnings();
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

    /// Replies to the requester after [`Self::install`]: the snapshot (also broadcast to every
    /// other client), the workspace warnings, then the result.
    fn finish_install(&mut self, id: RequestId, to: &ReplyTx) {
        let snapshot = self.doc.snapshot();
        reply(
            to,
            ServerMessage::Snapshot {
                snapshot: Box::new(snapshot.clone()),
            },
        );
        self.doc.file.resend_snapshot = !self.eng.out.send_state(ServerMessage::Snapshot {
            snapshot: Box::new(snapshot),
        });
        reply(
            to,
            ServerMessage::Warnings {
                warnings: self.doc.warnings.clone(),
            },
        );
        reply(to, ok(id, Some(self.doc.revision)));
    }

    /// Sends the snapshot to every client again (the first broadcast did not fit).
    pub(super) fn resend_snapshot(&mut self) {
        let snapshot = Box::new(self.doc.snapshot());
        self.doc.file.resend_snapshot = !self
            .eng
            .out
            .send_state(ServerMessage::Snapshot { snapshot });
    }

    pub(super) fn show_dir(&self) -> Option<PathBuf> {
        self.doc.show_path.as_deref().and_then(folder_of)
    }

    // ---- Saving -----------------------------------------------------------------------------

    fn start_save(&mut self, id: RequestId, to: &ReplyTx, path: PathBuf) {
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
        let mut copy = self.doc.ws.clone();
        let mut rebased = false;
        let mut relink_failure = None;
        if moved && let Some(new_dir) = &new_dir {
            let relinks = rebase(&copy, old_dir.as_deref(), new_dir);
            if !relinks.is_empty() {
                let command = relink_command(&copy, &relinks);
                let result = copy.apply(command);
                rebased = result.is_ok();
                if let Err(e @ openlab_core::CommandError::RollbackFailed { .. }) = &result {
                    relink_failure = Some(format!("relink of the copy being saved: {e}"));
                }
                if !rebased {
                    // A refusal changes nothing, but a failed rollback leaves some relinks in
                    // the copy: save the workspace as it is.
                    copy = self.doc.ws.clone();
                }
            }
        }
        let rx = writer.save(
            copy,
            path.clone(),
            self.config_clock_ms(),
            self.eng.config.machine.backup_keep,
            self.doc.revision,
        );
        if let Some(message) = relink_failure {
            self.log(LogLevel::Error, message);
        }
        self.doc.file.saves.push(PendingSave {
            rx,
            revision: self.doc.revision,
            path,
            epoch: self.doc.file.epoch,
            rebase: rebased,
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
        if self.doc.file.saves.is_empty() {
            return;
        }
        // Collect every answer first: a superseded save is answered with the save that replaced
        // it, which may be later in the list (or already finished).
        let mut waiting = Vec::new();
        let mut finished = Vec::new();
        for save in std::mem::take(&mut self.doc.file.saves) {
            match save.rx.try_recv() {
                Ok(result) => finished.push((save, result)),
                Err(TryRecvError::Empty) => waiting.push(save),
                Err(TryRecvError::Disconnected) => {
                    finished.push((save, Err(PersistError::WriterGone)));
                }
            }
        }
        let (superseded, finished): (Vec<_>, Vec<_>) = finished
            .into_iter()
            .partition(|(_, r)| matches!(r, Err(PersistError::Superseded)));
        let mut finished = finished;
        for (mut old, _) in superseded {
            // The newest save of the same file that holds at least these edits.
            let newer = waiting
                .iter_mut()
                .map(|p| (p.revision, &mut p.waiters, &p.path))
                .chain(
                    finished
                        .iter_mut()
                        .map(|(p, _)| (p.revision, &mut p.waiters, &p.path)),
                )
                .filter(|(revision, _, path)| *revision >= old.revision && **path == old.path)
                .max_by_key(|(revision, _, _)| *revision);
            match newer {
                Some((_, waiters, _)) => waiters.append(&mut old.waiters),
                None => {
                    for (id, to) in &old.waiters {
                        fail(to, *id, ErrorCode::Internal, "the save was superseded");
                    }
                }
            }
        }
        for (save, result) in finished {
            self.save_done(save, result);
        }
        self.doc.file.saves.extend(waiting);
    }

    fn save_done(&mut self, save: PendingSave, result: Result<SaveReport, PersistError>) {
        match result {
            Ok(report) => {
                for warning in report.warnings {
                    self.log(LogLevel::Warn, warning);
                }
                if save.epoch == self.doc.file.epoch {
                    self.saved(&save);
                }
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
                self.moved_folder(old_dir.as_deref(), save.rebase, clean_before);
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
    fn moved_folder(&mut self, old_dir: Option<&Path>, rebase_refs: bool, clean_before: bool) {
        let mut changed = false;
        if rebase_refs && let Some(new_dir) = self.show_dir() {
            let relinks = rebase(&self.doc.ws, old_dir, &new_dir);
            if !relinks.is_empty() {
                let command = relink_command(&self.doc.ws, &relinks);
                let result = self.doc.ws.apply(command);
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
        self.doc.media.reset(self.show_dir(), &self.doc.ws);
        if changed {
            self.doc.history = History::new();
            self.doc.revision += 1;
            // The copy on disk has the same references; it lacks only edits made meanwhile.
            if clean_before {
                self.doc.saved_revision = self.doc.revision;
            }
            self.doc.engine.model_changed();
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
            .ws
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
            self.doc.ws.clone(),
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
        let Some(pending) = &self.docs.pending else {
            return;
        };
        let elapsed = self
            .config
            .clock
            .now_ms()
            .saturating_sub(pending.started_ms);
        if elapsed < self.docs.timeout_ms {
            return;
        }
        // The worker has answered and the answer is in the inbox, not yet applied: it is not
        // stuck. Leave the request to that answer.
        // (If no answer comes for twice the timeout either, the worker is gone: fail it.)
        if !self.docs.worker.busy.load(Ordering::SeqCst)
            && elapsed < self.docs.timeout_ms.saturating_mul(2)
        {
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
        if reads && self.docs.stuck.load(Ordering::SeqCst) >= MAX_STUCK_WORKERS {
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

    fn next_ticket(&mut self, target: Target, id: RequestId, to: &ReplyTx, discard: bool) -> u64 {
        self.docs.ticket += 1;
        self.docs.pending = Some(PendingDoc {
            target,
            ticket: self.docs.ticket,
            id,
            reply: to.clone(),
            discard,
            started_ms: self.config.clock.now_ms(),
        });
        self.docs.ticket
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
    fn engine_config(&self, doc: DocId) -> Result<EngineConfig, openlab_core::EngineConfigError> {
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
    /// playing). The file is read and the doc built on the document worker; the doc is
    /// installed when the answer comes (`snapshot`, `warnings` and the result go to `to`;
    /// [`Self::take_opened`] tells its [`DocId`]). Refused (`command_refused`) while another
    /// document request is being handled, or when [`MAX_DOCS`] docs are open.
    pub fn open_doc(&mut self, source: DocSource, id: RequestId, to: &ReplyTx) {
        let reads = matches!(source, DocSource::Open { .. });
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
        let config = match self.eng.engine_config(DocId(self.eng.next_doc)) {
            Ok(config) => config,
            Err(e) => {
                fail(to, id, ErrorCode::Internal, e.to_string());
                return;
            }
        };
        let job = match source {
            DocSource::Open { path, recovery } => {
                let ticket = self.eng.next_ticket(Target::New, id, to, false);
                DocJob::Open {
                    ticket,
                    path,
                    recovery,
                    build: Some(config),
                }
            }
            DocSource::New | DocSource::Workspace(_) => {
                let workspace = match source {
                    DocSource::Workspace(ws) => *ws,
                    _ => self.eng.fresh_workspace(),
                };
                let ticket = self.eng.next_ticket(Target::New, id, to, false);
                DocJob::Build {
                    ticket,
                    loaded: Box::new(Loaded {
                        workspace,
                        path: None,
                        dirty: false,
                        notices: Vec::new(),
                        clear_autosave: false,
                    }),
                    config,
                }
            }
        };
        self.eng.send_job(job, id, to);
    }

    /// The document worker's answer, for the doc whose request it is (an open doc, or one being
    /// added). A doc closed since its request answers `no_workspace`.
    pub(super) fn doc_answer(&mut self, answer: DocAnswer) {
        let Some(pending) = self
            .eng
            .docs
            .pending
            .take_if(|p| p.ticket == answer.ticket())
        else {
            // A stale answer (its request timed out): a doc built for it is dropped off the
            // control thread like a closed one.
            if let DocAnswer::Built { parts, .. } = answer {
                self.eng.docs.discard(parts);
            }
            return;
        };
        match pending.target {
            Target::Doc(doc) => match self.cx_of(doc) {
                Some(mut cx) => cx.doc_answer(pending, answer),
                None => fail(
                    &pending.reply,
                    pending.id,
                    ErrorCode::NoWorkspace,
                    "the workspace was closed",
                ),
            },
            Target::New => {
                let (id, to) = (pending.id, &pending.reply);
                match answer {
                    DocAnswer::Built { parts, .. } => self.install_new(*parts, id, to),
                    DocAnswer::Failed { code, message, .. } => fail(to, id, code, message),
                    DocAnswer::Offer {
                        path,
                        workspace_name,
                        modified_ms,
                        ..
                    } => reply_data(
                        to,
                        id,
                        ResultData::RecoveryOffer {
                            path,
                            workspace_name,
                            modified_ms,
                        },
                    ),
                    DocAnswer::Loaded { .. } | DocAnswer::Entries { .. } => fail(
                        to,
                        id,
                        ErrorCode::Internal,
                        "the document worker answered another request",
                    ),
                }
            }
        }
    }

    /// Installs a doc built off the control thread: an insert, its probe jobs, its slot
    /// commands and its first reports, so it costs no audio block (ARCHITECTURE §3.1).
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
            loaded,
            mut engine,
            actions,
            indexed,
            checks,
        } = parts;
        let Loaded {
            workspace,
            path,
            dirty,
            notices,
            ..
        } = loaded;
        // The device may have come back with another block size while the doc was built.
        if engine.config().lookahead() != self.eng.block
            && let Err(e) = engine.set_lookahead(self.eng.block)
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
        let mut doc = Doc::new(&self.eng, doc_id, workspace, engine, path, media);
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
        // To the requester only: the other clients show another workspace (connections bound
        // to docs, and broadcasts tagged with theirs, are M4.15f).
        reply(
            to,
            ServerMessage::Snapshot {
                snapshot: Box::new(cx.doc.snapshot()),
            },
        );
        reply(
            to,
            ServerMessage::Warnings {
                warnings: cx.doc.warnings.clone(),
            },
        );
        reply(to, ok(id, Some(cx.doc.revision)));
        self.eng.opened.push((id, doc_id));
        self.eng.mixer.pump();
    }

    /// Closes doc `doc` (WS-02): refused with `unsaved_changes` while it has unsaved edits,
    /// unless `discard_changes` (then its autosave is removed, after any write of it still in
    /// flight). Its cues stop at once (Panic twice: a hard stop), its voices' late events are
    /// ignored from now on, its mixer slots are zeroed and freed, and it is dropped off the
    /// control thread. The other docs play on untouched.
    pub fn close_doc(&mut self, doc: DocId, discard_changes: bool) -> Result<(), ErrorBody> {
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
                let (id, path) = (cx.doc.ws.id, cx.doc.show_path.clone());
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
        // Its delays may have made a conflict in another doc, and its slots may be one's.
        self.eng.patches.take_conflicts_changed();
        self.mark_warnings_dirty();
        self.eng.voice_ids.retain(|_, owner| owner != doc);
        self.eng.observe.reset(doc);
        self.eng.watch.leave(doc);
        self.eng.pool.leave(doc);
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
    }

    /// Saves of closed docs that finished: answered (and a failure logged).
    pub(super) fn poll_orphan_saves(&mut self) {
        if self.eng.docs.orphans.is_empty() {
            return;
        }
        let mut finished = Vec::new();
        for save in std::mem::take(&mut self.eng.docs.orphans) {
            match save.rx.try_recv() {
                Ok(result) => finished.push((save, result)),
                Err(TryRecvError::Empty) => self.eng.docs.orphans.push(save),
                Err(TryRecvError::Disconnected) => {
                    finished.push((save, Err(PersistError::WriterGone)));
                }
            }
        }
        for (mut save, result) in finished {
            match result {
                Ok(_) => {
                    for (id, to) in &save.waiters {
                        reply(to, ok(*id, None));
                    }
                }
                // As for an open doc's saves: the newer save of the same file answers for it.
                Err(PersistError::Superseded) => {
                    let newer = self
                        .eng
                        .docs
                        .orphans
                        .iter_mut()
                        .filter(|p| p.revision >= save.revision && p.path == save.path)
                        .max_by_key(|p| p.revision);
                    match newer {
                        Some(newer) => newer.waiters.append(&mut save.waiters),
                        None => {
                            for (id, to) in &save.waiters {
                                fail(to, *id, ErrorCode::Internal, "the save was superseded");
                            }
                        }
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

impl Doc {
    fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
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

    /// The seed is injected, so a new workspace's passcode is the same for the same seed.
    #[test]
    fn a_new_workspace_gets_the_default_access_from_the_seed() {
        let make = |seed| {
            new_workspace(
                WorkspaceId(uuid::Uuid::from_u128(1)),
                CueListId(uuid::Uuid::from_u128(2)),
                PatchId(uuid::Uuid::from_u128(3)),
                seed,
            )
        };
        let access = make(4821).settings.osc_access;
        assert_eq!(access, openlab_core::OscAccess::new_workspace(4821));
        assert_eq!(access.passcodes[0].passcode, "4821");
        assert_eq!(make(4821), make(4821));
        assert_ne!(make(1), make(2));
    }
}
