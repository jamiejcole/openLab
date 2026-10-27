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
    CueListId, History, Input, LoadedShow, MAX_SHOW_FILE_BYTES, MediaRef, PatchId, PathStyle,
    Relink, Workspace, WorkspaceId, audio_media, autosave_due, comparison_key, is_absolute,
    load_show, portable_ref, relink_command, to_portable,
};

use super::inbox::reply;
use super::{Control, ControlConfig, ControlError, Inbox, ReplyTx, Scope, notice, ok};
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

#[derive(Debug)]
pub enum DocAnswer {
    Loaded {
        ticket: u64,
        loaded: Box<Loaded>,
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
    },
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

/// What the request being worked on by the document worker is.
struct PendingDoc {
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

/// Document state of the control loop.
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
    saves: Vec<PendingSave>,
    clears: Vec<ClearRx>,
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
            saves: Vec::new(),
            clears: Vec::new(),
            autosave: None,
            autosave_failed: false,
            autosaved_revision: None,
            dirty_since: None,
            last_autosave: None,
            epoch: 0,
            notices: Vec::new(),
            resend_snapshot: false,
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

    /// Requests being worked on (a read, a save): tests wait until it is zero.
    pub(super) fn busy(&self) -> usize {
        usize::from(self.pending.is_some())
            + self.saves.len()
            + self.clears.len()
            + usize::from(self.autosave.is_some())
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
        flags.busy.store(true, Ordering::SeqCst);
        if let Some(hook) = hook {
            (hook.0)();
        }
        let answer = match job {
            DocJob::Open {
                ticket,
                path,
                recovery,
            } => open(ticket, &path, recovery, dirs),
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

impl Control {
    fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    /// A document request (everything in `Request` but `snapshot`, `list_devices`, `relink`).
    pub(super) fn doc_request(&mut self, id: RequestId, request: Request, to: &ReplyTx) {
        match request {
            Request::Save => match self.config.show_path.clone() {
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
                let workspace = Workspace::new_empty(
                    WorkspaceId(random_id()),
                    CueListId(random_id()),
                    PatchId(random_id()),
                );
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
                let Some(path) = self.config.show_path.clone() else {
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
            | Request::PreshowCheck
            | Request::LogTail { .. }
            | Request::Relink { .. } => {}
        }
    }

    /// Whether a request that replaces the workspace (or reads files) may start: only one at a
    /// time, and not over unsaved edits unless the request discards them. `reads`: the request
    /// needs the document worker.
    fn doc_gate(&self, id: RequestId, to: &ReplyTx, discard: bool, reads: bool) -> bool {
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
        if self.is_dirty() && !discard {
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
        self.docs.ticket += 1;
        self.docs.pending = Some(PendingDoc {
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

    /// The document worker's answer.
    pub(super) fn doc_answer(&mut self, answer: DocAnswer) {
        let Some(pending) = self.docs.pending.take_if(|p| p.ticket == answer.ticket()) else {
            return;
        };
        let (id, to) = (pending.id, &pending.reply);
        match answer {
            DocAnswer::Failed { code, message, .. } => fail(to, id, code, message),
            DocAnswer::Offer {
                path,
                workspace_name,
                modified_ms,
                ..
            } => self.reply_data(
                to,
                id,
                ResultData::RecoveryOffer {
                    path,
                    workspace_name,
                    modified_ms,
                },
            ),
            DocAnswer::Entries { entries, .. } => {
                self.reply_data(to, id, ResultData::RecoveryEntries { entries });
            }
            DocAnswer::Loaded { loaded, .. } => {
                // The operator may have edited while the file was being read.
                if self.is_dirty() && !pending.discard {
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

    fn reply_data(&self, to: &ReplyTx, id: RequestId, data: ResultData) {
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
            .voices
            .playing_ids()
            .into_iter()
            .map(|voice| {
                (
                    self.voices.stop_date(voice, now, floor),
                    CommandKind::StopVoice {
                        voice,
                        fade: Fade::NONE,
                    },
                )
            })
            .collect();
        self.send_commands(stops);
        self.voices.forget_all();
        self.deferred.clear();
        self.pending_media.clear();
        self.ready_media.clear();
        self.mixer.pump();
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
        let same_file = self.ws.id == workspace.id
            && match (&self.config.show_path, &path) {
                (Some(a), Some(b)) => same_path(a, b),
                (None, None) => true,
                _ => false,
            };
        // The autosave of dropped edits goes, after any write of it still in flight (the writer
        // runs the removal after its current job). Not when the same file is reopened: its
        // autosave may be the one just restored.
        if (discard && self.is_dirty() && !same_file) || clear_autosave {
            let (id, from) = if clear_autosave {
                (workspace.id, path.clone())
            } else {
                (self.ws.id, self.config.show_path.clone())
            };
            self.clear_autosave_of(id, from);
        }
        self.stop_everything();
        self.docs.epoch += 1;
        // A new engine: its run ids restart, which cannot meet the old ones (the voices are
        // forgotten and mixer voice ids are never reused).
        match openlab_core::EngineConfig::new(self.rate.get(), self.block, self.config.seed) {
            Ok(config) => self.engine = openlab_core::CueEngine::new(config),
            // Cannot happen (the same values built the first engine); the old engine, with
            // everything stopped, is the safe thing to keep.
            Err(e) => self.log(LogLevel::Error, format!("cue engine not rebuilt: {e}")),
        }
        self.ws = workspace;
        self.history = History::new();
        self.checks = super::checks::PlanChecks::default();
        self.config.show_path = path;
        self.observe.reset();
        self.log_kind(
            LogKind::Document,
            LogLevel::Info,
            match &self.config.show_path {
                Some(p) => format!("workspace opened: {}", p.display()),
                None => "workspace opened: untitled".to_owned(),
            },
        );
        self.media.reset(self.show_dir(), &self.ws);
        self.docs.notices = notices;
        self.revision += 1;
        self.saved_revision = if dirty {
            self.revision.saturating_sub(1)
        } else {
            self.revision
        };
        self.docs.dirty_since = None;
        self.docs.last_autosave = None;
        self.docs.autosaved_revision = None;
        let now = self.now();
        self.engine.model_changed();
        let mut inputs = vec![Input::Tick];
        inputs.extend(self.sync_workspace(true));
        for input in inputs {
            self.feed(input, now);
        }
        self.warnings_dirty = false;
        self.refresh_warnings();
        self.mixer.pump();
    }

    /// Queues the removal of a workspace's recovery file behind the writer's current job.
    fn clear_autosave_of(&mut self, workspace: WorkspaceId, show_path: Option<PathBuf>) {
        if let Some(writer) = &self.docs.writer {
            self.docs
                .clears
                .push(writer.clear_recovery(workspace, show_path));
        }
    }

    /// Replies to the requester after [`Self::install`]: the snapshot (also broadcast to every
    /// other client), the workspace warnings, then the result.
    fn finish_install(&mut self, id: RequestId, to: &ReplyTx) {
        let snapshot = self.snapshot();
        reply(
            to,
            ServerMessage::Snapshot {
                snapshot: Box::new(snapshot.clone()),
            },
        );
        self.docs.resend_snapshot = !self.out.send_state(ServerMessage::Snapshot {
            snapshot: Box::new(snapshot),
        });
        reply(
            to,
            ServerMessage::Warnings {
                warnings: self.warnings.clone(),
            },
        );
        reply(to, ok(id, Some(self.revision)));
    }

    /// Sends the snapshot to every client again (the first broadcast did not fit).
    pub(super) fn resend_snapshot(&mut self) {
        let snapshot = Box::new(self.snapshot());
        self.docs.resend_snapshot = !self.out.send_state(ServerMessage::Snapshot { snapshot });
    }

    fn show_dir(&self) -> Option<PathBuf> {
        self.config.show_path.as_deref().and_then(folder_of)
    }

    // ---- Saving -----------------------------------------------------------------------------

    fn start_save(&mut self, id: RequestId, to: &ReplyTx, path: PathBuf) {
        let Some(writer) = &self.docs.writer else {
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
        let moved = self.config.show_path.as_deref() != Some(path.as_path())
            && new_dir.is_some()
            && new_dir != old_dir;
        let mut copy = self.ws.clone();
        let mut rebased = false;
        if moved && let Some(new_dir) = &new_dir {
            let relinks = rebase(&copy, old_dir.as_deref(), new_dir);
            if !relinks.is_empty() {
                let command = relink_command(&copy, &relinks);
                rebased = copy.apply(command).is_ok();
            }
        }
        let rx = writer.save(
            copy,
            path.clone(),
            self.config_clock_ms(),
            self.config.machine.backup_keep,
            self.revision,
        );
        self.docs.saves.push(PendingSave {
            rx,
            revision: self.revision,
            path,
            epoch: self.docs.epoch,
            rebase: rebased,
            waiters: vec![(id, to.clone())],
        });
    }

    fn config_clock_ms(&self) -> i64 {
        self.config.clock.now_ms()
    }

    /// Collects what the writer thread has finished, and starts an autosave when one is due.
    pub(super) fn docs_tick(&mut self) {
        if self.docs.resend_snapshot && self.out.has_room() {
            self.resend_snapshot();
        }
        self.check_stalled_read();
        self.poll_saves();
        self.poll_clears();
        self.poll_autosave();
        self.autosave_if_due();
    }

    fn poll_saves(&mut self) {
        if self.docs.saves.is_empty() {
            return;
        }
        // Collect every answer first: a superseded save is answered with the save that replaced
        // it, which may be later in the list (or already finished).
        let mut waiting = Vec::new();
        let mut finished = Vec::new();
        for save in std::mem::take(&mut self.docs.saves) {
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
        self.docs.saves.extend(waiting);
    }

    fn save_done(&mut self, save: PendingSave, result: Result<SaveReport, PersistError>) {
        match result {
            Ok(report) => {
                for warning in report.warnings {
                    self.log(LogLevel::Warn, warning);
                }
                if save.epoch == self.docs.epoch {
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
        let clean_before = self.revision == save.revision;
        self.saved_revision = save.revision;
        self.record(
            LogKind::Document,
            LogLevel::Info,
            format!("saved {}", save.path.display()),
            None,
            None,
        );
        if self.config.show_path.as_deref() != Some(save.path.as_path()) {
            let old_dir = self.show_dir();
            self.config.show_path = Some(save.path.clone());
            if self.show_dir() != old_dir {
                self.moved_folder(old_dir.as_deref(), save.rebase, clean_before);
            }
        }
        if !self.is_dirty() {
            self.docs.dirty_since = None;
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
            let relinks = rebase(&self.ws, old_dir, &new_dir);
            if !relinks.is_empty() {
                let command = relink_command(&self.ws, &relinks);
                changed = self.ws.apply(command).is_ok();
            }
        }
        self.media.reset(self.show_dir(), &self.ws);
        if changed {
            self.history = History::new();
            self.revision += 1;
            // The copy on disk has the same references; it lacks only edits made meanwhile.
            if clean_before {
                self.saved_revision = self.revision;
            }
            self.engine.model_changed();
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

    /// A read that has not answered within the timeout (a volume that stopped answering): the
    /// request fails with `io`, the worker is abandoned (it ends when its read returns; if it
    /// never does, it is one leaked thread, bounded by [`MAX_STUCK_WORKERS`]) and a fresh one
    /// takes over the shared queue.
    fn check_stalled_read(&mut self) {
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

    fn poll_clears(&mut self) {
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

    fn poll_autosave(&mut self) {
        let Some(rx) = &self.docs.autosave else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(PersistError::WriterGone),
        };
        self.docs.autosave = None;
        match result {
            Ok(_) => self.docs.autosave_failed = false,
            Err(e) => {
                // Said once per run of failures: it repeats every interval otherwise.
                if !self.docs.autosave_failed {
                    self.log(LogLevel::Warn, format!("autosave failed: {e}"));
                }
                self.docs.autosave_failed = true;
                // Try the same revision again next time.
                self.docs.autosaved_revision = None;
            }
        }
    }

    fn autosave_if_due(&mut self) {
        if !self.is_dirty() {
            self.docs.dirty_since = None;
            return;
        }
        let now_ms = self.config_clock_ms();
        let dirty_since = *self.docs.dirty_since.get_or_insert(now_ms);
        if self.docs.autosave.is_some() || self.docs.autosaved_revision == Some(self.revision) {
            return;
        }
        let seconds = |ms: i64| ms as f64 / 1000.0;
        let interval = self.ws.settings.autosave_interval.max(MIN_AUTOSAVE_SECS);
        if !autosave_due(
            Some(seconds(dirty_since)),
            self.docs.last_autosave.map(seconds),
            seconds(now_ms),
            interval,
        ) {
            return;
        }
        let Some(writer) = &self.docs.writer else {
            return;
        };
        let rx = writer.autosave(
            self.ws.clone(),
            self.config.show_path.clone(),
            self.revision,
        );
        self.docs.autosave = Some(rx);
        self.docs.autosaved_revision = Some(self.revision);
        self.docs.last_autosave = Some(now_ms);
    }

    /// Answers the document workers have posted so far (tests wait for one to be in the inbox).
    pub fn doc_answers_posted(&self) -> u64 {
        self.docs.answered.load(Ordering::SeqCst)
    }

    /// Document workers abandoned in a stalled read that have not returned yet.
    pub fn stuck_doc_workers(&self) -> usize {
        self.docs.stuck.load(Ordering::SeqCst)
    }

    /// Requests, saves and autosaves still being worked on.
    pub fn docs_busy(&self) -> usize {
        self.docs.busy()
    }

    /// The show file the workspace is saved to, if it has one.
    pub fn show_path(&self) -> Option<&Path> {
        self.config.show_path.as_deref()
    }

    pub fn is_unsaved(&self) -> bool {
        self.is_dirty()
    }

    pub fn runtime_seq(&self) -> u64 {
        self.runtime_seq
    }
}
