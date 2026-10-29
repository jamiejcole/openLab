// SPDX-License-Identifier: GPL-3.0-or-later

//! The writer thread: saves and autosaves run here, never on the control thread
//! (`save_show` serialises and re-validates, up to about a second for a large show).
//!
//! The queue is bounded by coalescing, because the disk may be slow or hung (network share,
//! sleeping USB drive) while autosave keeps firing:
//! - at most one pending manual save per target file; a newer one replaces it;
//! - at most one pending autosave per recovery target (workspace id plus show path, which is
//!   what names the recovery file); a newer one replaces only its own target's, so several
//!   open workspaces never displace each other's autosaves;
//! - at most [`MAX_PENDING_TARGETS`] targets wait in the normal queue. A new target beyond that
//!   pushes the oldest waiting autosave into a write-through queue that runs before the rest,
//!   so the cap itself never costs an autosave. That queue still coalesces per target, and is
//!   capped at [`MAX_FORCED_TARGETS`]: during a long disk hang closed workspaces and Save As
//!   paths add targets beyond the open ones (8 by WS-02), and when it is full the oldest
//!   forced autosave is shed (answered `Ok(None)`, counted by
//!   [`SaveWriter::shed_autosaves`]). That is safe: an autosave is a recovery convenience and
//!   the next autosave of a still-open workspace replaces it anyway;
//! - manual saves run before pending autosaves, and an autosave whose revision is not newer
//!   than the last durably saved one of its workspace is dropped (no recovery file for
//!   already-saved state).
//!
//! A job stuck on a hung volume must not hold up every other show's saves and autosaves. The
//! control loop calls [`SaveWriter::check_stalled`] each tick: a job running longer than the
//! timeout is answered [`PersistError::Stalled`] and its thread abandoned (it carries on and
//! leaves once the write returns, if ever), and a fresh thread takes over the queue, at most
//! [`MAX_WRITERS`] threads in all. Two threads never write the same files at once: a job
//! waits while another thread runs one that writes the same show file or recovery file
//! ([`Target`]), so a stalled write that finishes late can never land over a newer one. While
//! a stalled write is still out, saves and autosaves of the files it holds are refused at once
//! with `Stalled` (a recovery-file removal waits for it); every other file is written as usual.
//! A stalled save holds only its show file: it no longer removes the recovery files when it
//! finishes (an autosave since then holds newer edits), so the same workspace can still be
//! saved elsewhere and autosaved, at shutdown too. Once the stalled write returns, its file
//! saves again. A late answer of an abandoned job is not reported (the request was already
//! answered).
//!
//! Dropping the writer finishes the running jobs and any pending manual saves and discards
//! pending autosaves, so shutdown never waits behind a backlog. It waits for that at most
//! [`DROP_WAIT`]: a save stuck on a hung volume must not keep the engine process alive, so the
//! threads are then left to end by themselves (M4.15g). A final recovery write is queued and
//! waited for before the drop (`Control::shutdown`), with a spare thread
//! ([`SaveWriter::add_spare`]) so that one show's slow write does not hold up the others'.

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use openlab_core::{Workspace, WorkspaceId};

use super::save::save_workspace_inner;
use super::threads::BoundedThreads;
use super::{AppDirs, PersistError, SaveReport, clear_recovery, recovery_stem, write_recovery};

/// What a writer job does; passed to the test hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    Save,
    Autosave,
}

type SaveReply = Sender<Result<SaveReport, PersistError>>;
/// `Ok(None)`: nothing written, because a newer autosave replaced this one, the state is
/// already saved, or the writer is shutting down.
type AutosaveReply = Sender<Result<Option<PathBuf>, PersistError>>;

struct SaveJob {
    workspace: Box<Workspace>,
    path: PathBuf,
    now_ms: i64,
    keep: u32,
    revision: u64,
    /// Show files of other open workspaces: the save is refused if `path` is one of them.
    others: Vec<PathBuf>,
    reply: SaveReply,
}

/// Hard cap on targets waiting in the normal autosave queue; see the module docs.
pub const MAX_PENDING_TARGETS: usize = 16;
/// Hard cap on the write-through queue; see the module docs.
pub const MAX_FORCED_TARGETS: usize = 32;
/// How long dropping the writer waits for its thread to finish the running job and the pending
/// manual saves.
pub const DROP_WAIT: Duration = Duration::from_secs(5);
/// Writer threads at most, stalled ones included: the one serving the queue and up to sixteen
/// left stuck on hung volumes, so every open show (`control::MAX_DOCS`, 8) can have both its
/// show file and its recovery file stuck and the other shows still save. With all of them
/// stuck, saves and autosaves are refused with [`PersistError::Stalled`] until one returns.
pub const MAX_WRITERS: usize = 17;

/// The files a job writes, as far as keeping two threads apart goes: the show file (and its
/// backups, which are kept per workspace and show file) and recovery files, by their names'
/// stems ([`recovery_stem`]). Two jobs overlap when they share either.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
    workspace: WorkspaceId,
    show: Option<PathBuf>,
    recovery: Vec<String>,
}

impl Target {
    fn overlaps(&self, other: &Self) -> bool {
        (self.show.is_some() && self.show == other.show)
            || self.recovery.iter().any(|r| other.recovery.contains(r))
    }

    /// Only the show file: a stalled save that will not touch the recovery files.
    fn show_only(&self) -> Self {
        Self {
            recovery: Vec::new(),
            ..self.clone()
        }
    }

    fn describe(&self) -> String {
        match &self.show {
            Some(path) => path.display().to_string(),
            None => format!("the recovery file of workspace {}", self.workspace),
        }
    }
}

/// Where a running job's answer goes, so a stalled one can be answered without its thread.
enum Answer {
    Save(SaveReply),
    Autosave(AutosaveReply),
    Clear(ClearReply),
}

impl Answer {
    fn fail(&self, error: PersistError) {
        match self {
            Self::Save(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Autosave(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Clear(reply) => {
                let _ = reply.send(Err(error));
            }
        }
    }
}

/// A job a thread is running.
struct Running {
    id: u64,
    target: Target,
    answer: Answer,
    /// When [`SaveWriter::check_stalled`] first saw it (a monotonic clock).
    seen: Option<Instant>,
    /// Answered `Stalled`; its thread is abandoned.
    stalled: bool,
    /// A save that has begun removing the recovery files (it holds them until it ends, even
    /// if it stalls there); before that, a stalled save skips the removal.
    clearing: bool,
}

impl Running {
    /// The files it still holds: a stalled save that has not begun removing the recovery
    /// files never will, so it holds only its show file.
    fn holds(&self) -> Target {
        if self.stalled && !self.clearing && matches!(self.answer, Answer::Save(_)) {
            self.target.show_only()
        } else {
            self.target.clone()
        }
    }
}

struct AutosaveJob {
    workspace: Box<Workspace>,
    show_path: Option<PathBuf>,
    revision: u64,
    reply: AutosaveReply,
}

struct ClearJob {
    workspace: WorkspaceId,
    show_path: Option<PathBuf>,
    reply: ClearReply,
}

type ClearReply = Sender<Result<(), PersistError>>;

impl AutosaveJob {
    fn targets(&self, workspace: WorkspaceId, show_path: &Option<PathBuf>) -> bool {
        self.workspace.id == workspace && self.show_path == *show_path
    }

    fn target(&self) -> Target {
        Target {
            workspace: self.workspace.id,
            show: None,
            recovery: vec![recovery_stem(self.workspace.id, self.show_path.as_deref())],
        }
    }
}

impl SaveJob {
    /// The show file, and the two recovery files a save removes once it is in place.
    fn target(&self) -> Target {
        let id = self.workspace.id;
        Target {
            workspace: id,
            show: Some(self.path.clone()),
            recovery: vec![recovery_stem(id, Some(&self.path)), recovery_stem(id, None)],
        }
    }
}

impl ClearJob {
    fn target(&self) -> Target {
        Target {
            workspace: self.workspace,
            show: None,
            recovery: vec![recovery_stem(self.workspace, self.show_path.as_deref())],
        }
    }
}

#[derive(Default)]
struct State {
    saves: VecDeque<SaveJob>,
    clears: VecDeque<ClearJob>,
    /// Pending autosaves, at most one per target, oldest first.
    autosaves: VecDeque<AutosaveJob>,
    /// Autosaves pushed out of `autosaves` by the cap; written before it, at most one per target.
    forced: VecDeque<AutosaveJob>,
    /// Autosaves shed because `forced` was full.
    shed: u64,
    /// Newest revision per workspace that a manual save has made durable (the most recent
    /// [`MAX_PENDING_TARGETS`] workspaces; an evicted one only risks one redundant autosave).
    saved: VecDeque<(WorkspaceId, u64)>,
    closing: bool,
    /// Jobs being run, one per busy thread.
    running: Vec<Running>,
    next_id: u64,
    /// Writer threads alive, stalled ones included.
    workers: usize,
    /// The stall timeout last used, in seconds (for the refusals).
    stall_secs: u64,
}

impl State {
    fn stalled(&self) -> usize {
        self.running.iter().filter(|r| r.stalled).count()
    }

    /// Threads that are not stuck in a stalled job.
    fn live(&self) -> usize {
        self.workers.saturating_sub(self.stalled())
    }

    /// Another thread is writing files this job writes: it waits.
    fn blocked(&self, target: &Target) -> bool {
        self.running.iter().any(|r| r.holds().overlaps(target))
    }

    /// Why a new save or autosave of `target` is answered at once instead of queued.
    fn refusal(&self, target: &Target) -> Option<PersistError> {
        let stuck = self
            .running
            .iter()
            .any(|r| r.stalled && r.holds().overlaps(target));
        (stuck || self.live() == 0).then_some(PersistError::Stalled {
            secs: self.stall_secs,
        })
    }

    fn begin(&mut self, target: Target, answer: Answer) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.running.push(Running {
            id,
            target,
            answer,
            seen: None,
            stalled: false,
            clearing: false,
        });
        id
    }

    /// Answers `Stalled` to every queued save and autosave `doomed` picks (removals wait).
    fn fail_queued(&mut self, doomed: impl Fn(&Target) -> bool) {
        let secs = self.stall_secs;
        for job in std::mem::take(&mut self.saves) {
            if doomed(&job.target()) {
                let _ = job.reply.send(Err(PersistError::Stalled { secs }));
            } else {
                self.saves.push_back(job);
            }
        }
        for queue in [&mut self.forced, &mut self.autosaves] {
            for job in std::mem::take(queue) {
                if doomed(&job.target()) {
                    let _ = job.reply.send(Err(PersistError::Stalled { secs }));
                } else {
                    queue.push_back(job);
                }
            }
        }
    }
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic elsewhere must not stop saving.
        crate::lock(&self.state)
    }
}

type Hook = Arc<dyn Fn(JobKind) + Send + Sync>;

/// Owns the threads and their queue.
pub struct SaveWriter {
    shared: Arc<Shared>,
    threads: BoundedThreads,
    dirs: AppDirs,
    hook: Option<Hook>,
    drop_wait: Duration,
}

impl SaveWriter {
    /// How many pending autosaves were shed because the write-through queue was full.
    pub fn shed_autosaves(&self) -> u64 {
        self.shared.lock().shed
    }

    pub fn spawn(dirs: AppDirs) -> io::Result<Self> {
        Self::spawn_inner(dirs, None)
    }

    /// Like [`spawn`](Self::spawn), with `hook` called on the writer thread just before each
    /// job runs. Tests block in it to hold the writer busy without sleeping.
    pub fn spawn_with_hook(
        dirs: AppDirs,
        hook: impl Fn(JobKind) + Send + Sync + 'static,
    ) -> io::Result<Self> {
        Self::spawn_inner(dirs, Some(Arc::new(hook)))
    }

    fn spawn_inner(dirs: AppDirs, hook: Option<Hook>) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                workers: 1,
                ..State::default()
            }),
            wake: Condvar::new(),
        });
        let mut writer = Self {
            shared,
            threads: BoundedThreads::new(),
            dirs,
            hook,
            drop_wait: DROP_WAIT,
        };
        writer.spawn_thread()?;
        Ok(writer)
    }

    /// Starts one more thread on the queue; the caller has counted it in `workers`.
    fn spawn_thread(&mut self) -> io::Result<()> {
        let (shared, dirs, hook) = (
            Arc::clone(&self.shared),
            self.dirs.clone(),
            self.hook.clone(),
        );
        self.threads.spawn("openlab-writer", move || {
            run(&shared, &dirs, hook.as_deref());
        })
    }

    /// Adds a thread, counted first so the others see it; uncounted again if it cannot start.
    fn add_thread(&mut self) -> io::Result<()> {
        self.shared.lock().workers += 1;
        let started = self.spawn_thread();
        if started.is_err() {
            let mut state = self.shared.lock();
            state.workers = state.workers.saturating_sub(1);
        }
        started
    }

    /// The stall watchdog, called by the control loop each tick with its clock: a job first
    /// seen running `timeout` ago or more is answered [`PersistError::Stalled`], queued saves
    /// and autosaves of its target too, and a fresh thread takes over the queue (module docs).
    /// Returns what to log.
    pub fn check_stalled(&mut self, now: Instant, timeout: Duration) -> Vec<String> {
        let secs = timeout.as_secs();
        let mut state = self.shared.lock();
        state.stall_secs = secs;
        let mut stalled = Vec::new();
        for job in state.running.iter_mut().filter(|r| !r.stalled) {
            let seen = *job.seen.get_or_insert(now);
            if now.saturating_duration_since(seen) >= timeout {
                job.stalled = true;
                job.answer.fail(PersistError::Stalled { secs });
                stalled.push(job.holds());
            }
        }
        if stalled.is_empty() {
            return Vec::new();
        }
        let mut logs: Vec<String> = stalled
            .iter()
            .map(|t| {
                format!(
                    "writing {} did not finish within {secs} s (its volume may have stopped \
                     answering); other shows go on saving",
                    t.describe()
                )
            })
            .collect();
        state.fail_queued(|t| stalled.iter().any(|s| s.overlaps(t)));
        let replace = state.live() == 0;
        let room = state.workers < MAX_WRITERS;
        drop(state);
        if replace {
            let failure = if room {
                self.add_thread().err().map(|e| {
                    format!("could not start a replacement save thread ({e}); saves are refused")
                })
            } else {
                Some(format!(
                    "{MAX_WRITERS} writes are stuck on volumes that stopped answering; saves \
                     and autosaves are refused until one finishes"
                ))
            };
            if let Some(failure) = failure {
                self.shared.lock().fail_queued(|_| true);
                logs.push(failure);
            }
        }
        logs
    }

    /// Threads abandoned in a stalled write that have not returned yet.
    pub fn stalled_threads(&self) -> usize {
        self.shared.lock().stalled()
    }

    /// Adds a thread when every one is busy (and the limit allows), so jobs of other targets
    /// do not wait behind a slow one: for the final recovery writes at shutdown.
    pub fn add_spare(&mut self) {
        let busy = {
            let state = self.shared.lock();
            !state.closing && state.running.len() >= state.workers && state.workers < MAX_WRITERS
        };
        if busy {
            let _ = self.add_thread();
        }
    }

    /// How long dropping the writer waits for its thread ([`DROP_WAIT`] by default).
    pub fn set_drop_wait(&mut self, wait: Duration) {
        self.drop_wait = wait;
    }

    /// Queues a manual save of `workspace` (a clone taken by the caller, so edits can go on),
    /// tagged with the workspace's edit `revision`. A save still pending for the same `path`
    /// is replaced and answers [`PersistError::Superseded`].
    pub fn save(
        &self,
        workspace: Workspace,
        path: PathBuf,
        now_ms: i64,
        keep: u32,
        revision: u64,
    ) -> Receiver<Result<SaveReport, PersistError>> {
        self.save_unless(workspace, path, now_ms, keep, revision, Vec::new())
    }

    /// As [`Self::save`], refused with [`PersistError::OpenElsewhere`] (nothing written) when
    /// `path` names the same file as one of `others` (compared by [`super::file_key`] on this
    /// thread, so a symlinked or otherwise re-spelled path is caught too).
    pub fn save_unless(
        &self,
        workspace: Workspace,
        path: PathBuf,
        now_ms: i64,
        keep: u32,
        revision: u64,
        others: Vec<PathBuf>,
    ) -> Receiver<Result<SaveReport, PersistError>> {
        let (reply, answer) = mpsc::channel();
        let mut state = self.shared.lock();
        if state.closing {
            let _ = reply.send(Err(PersistError::WriterGone));
            return answer;
        }
        let job = SaveJob {
            workspace: Box::new(workspace),
            path,
            now_ms,
            keep,
            revision,
            others,
            reply,
        };
        if let Some(refused) = state.refusal(&job.target()) {
            let _ = job.reply.send(Err(refused));
            return answer;
        }
        match state.saves.iter_mut().find(|p| p.path == job.path) {
            Some(pending) => {
                let old = std::mem::replace(pending, job);
                let _ = old.reply.send(Err(PersistError::Superseded));
            }
            None => state.saves.push_back(job),
        }
        drop(state);
        self.shared.wake.notify_all();
        answer
    }

    /// Queues an autosave of `workspace` to its recovery file, bound to `show_path` (`None`
    /// before the first save). It is skipped (`Ok(None)`) if a newer autosave of the same
    /// workspace and show path replaces it (other targets are untouched), or
    /// a manual save of this revision or a later one has completed by the time it would run.
    pub fn autosave(
        &self,
        workspace: Workspace,
        show_path: Option<PathBuf>,
        revision: u64,
    ) -> Receiver<Result<Option<PathBuf>, PersistError>> {
        let (reply, answer) = mpsc::channel();
        let mut state = self.shared.lock();
        if state.closing {
            let _ = reply.send(Err(PersistError::WriterGone));
            return answer;
        }
        let job = AutosaveJob {
            workspace: Box::new(workspace),
            show_path,
            revision,
            reply,
        };
        if let Some(refused) = state.refusal(&job.target()) {
            let _ = job.reply.send(Err(refused));
            return answer;
        }
        let same = |a: &AutosaveJob| a.targets(job.workspace.id, &job.show_path);
        if let Some(pending) = state.forced.iter_mut().find(|a| same(a)) {
            let old = std::mem::replace(pending, job);
            let _ = old.reply.send(Ok(None));
        } else if let Some(pending) = state.autosaves.iter_mut().find(|a| same(a)) {
            let old = std::mem::replace(pending, job);
            let _ = old.reply.send(Ok(None));
        } else {
            if state.autosaves.len() >= MAX_PENDING_TARGETS
                && let Some(oldest) = state.autosaves.pop_front()
            {
                if state.forced.len() >= MAX_FORCED_TARGETS
                    && let Some(shed) = state.forced.pop_front()
                {
                    state.shed += 1;
                    let _ = shed.reply.send(Ok(None));
                }
                state.forced.push_back(oldest);
            }
            state.autosaves.push_back(job);
        }
        drop(state);
        self.shared.wake.notify_all();
        answer
    }

    /// Queues the removal of the recovery file (and its moved-aside copies) of `workspace` at
    /// `show_path`: the operator dropped the edits it holds. It runs on the writer thread after
    /// any job already running (an autosave of those edits that is mid-write finishes first and
    /// is then removed), and a pending autosave of that workspace and path is cancelled, so the
    /// dropped edits cannot reappear as a recovery offer.
    pub fn clear_recovery(
        &self,
        workspace: WorkspaceId,
        show_path: Option<PathBuf>,
    ) -> Receiver<Result<(), PersistError>> {
        let (reply, answer) = mpsc::channel();
        let mut state = self.shared.lock();
        if state.closing {
            let _ = reply.send(Err(PersistError::WriterGone));
            return answer;
        }
        // Only this target's pending autosave goes; every other target's stays queued.
        let State {
            autosaves, forced, ..
        } = &mut *state;
        for queue in [forced, autosaves] {
            if let Some(i) = queue.iter().position(|a| a.targets(workspace, &show_path))
                && let Some(old) = queue.remove(i)
            {
                let _ = old.reply.send(Ok(None));
            }
        }
        state.clears.push_back(ClearJob {
            workspace,
            show_path,
            reply,
        });
        drop(state);
        self.shared.wake.notify_all();
        answer
    }
}

enum Next {
    Save(SaveJob),
    Clear(ClearJob),
    Autosave(AutosaveJob),
}

/// The next job this thread may run (none while another thread writes the same files), or
/// `None` when the writer is closing and nothing is left for it (the thread then ends).
fn next_job(shared: &Shared) -> Option<(u64, Next)> {
    let mut state = shared.lock();
    loop {
        let st = &*state;
        if let Some(i) = st.saves.iter().position(|j| !st.blocked(&j.target()))
            && let Some(job) = state.saves.remove(i)
        {
            let id = state.begin(job.target(), Answer::Save(job.reply.clone()));
            return Some((id, Next::Save(job)));
        }
        let st = &*state;
        if let Some(i) = st.clears.iter().position(|j| !st.blocked(&j.target()))
            && let Some(job) = state.clears.remove(i)
        {
            let id = state.begin(job.target(), Answer::Clear(job.reply.clone()));
            return Some((id, Next::Clear(job)));
        }
        if state.closing {
            let State {
                autosaves, forced, ..
            } = &mut *state;
            for job in forced.drain(..).chain(autosaves.drain(..)) {
                let _ = job.reply.send(Ok(None));
            }
            // What waits behind a running job is that thread's to finish; with none left
            // running, nothing will.
            if state.running.is_empty() {
                for job in state.saves.drain(..) {
                    let _ = job.reply.send(Err(PersistError::WriterGone));
                }
                for job in state.clears.drain(..) {
                    let _ = job.reply.send(Err(PersistError::WriterGone));
                }
            }
            state.workers = state.workers.saturating_sub(1);
            return None;
        }
        let st = &*state;
        let forced = st.forced.iter().position(|j| !st.blocked(&j.target()));
        let normal = st.autosaves.iter().position(|j| !st.blocked(&j.target()));
        let job = match (forced, normal) {
            (Some(i), _) => state.forced.remove(i),
            (None, Some(i)) => state.autosaves.remove(i),
            (None, None) => None,
        };
        if let Some(job) = job {
            let covered = state
                .saved
                .iter()
                .any(|&(id, rev)| id == job.workspace.id && job.revision <= rev);
            if covered {
                let _ = job.reply.send(Ok(None));
                continue;
            }
            let id = state.begin(job.target(), Answer::Autosave(job.reply.clone()));
            return Some((id, Next::Autosave(job)));
        }
        state = shared
            .wake
            .wait(state)
            .unwrap_or_else(PoisonError::into_inner);
    }
}

/// A save is about to remove the recovery files: false if it was given up on (it then
/// leaves them: an autosave since may hold newer edits); otherwise it holds them from now on.
fn begin_clearing(shared: &Shared, id: u64) -> bool {
    let mut state = shared.lock();
    match state.running.iter_mut().find(|r| r.id == id) {
        Some(job) if !job.stalled => {
            job.clearing = true;
            true
        }
        _ => false,
    }
}

/// The job `id` is done. True when this thread should end: another thread that is not stuck
/// serves the queue (a replacement took over while this one was stalled, or a spare).
fn finish(shared: &Shared, id: u64) -> bool {
    let mut state = shared.lock();
    state.running.retain(|r| r.id != id);
    let leave = !state.closing && state.live() > 1;
    if leave {
        state.workers = state.workers.saturating_sub(1);
    }
    drop(state);
    // Jobs that waited for this one's files may run now.
    shared.wake.notify_all();
    leave
}

fn run(shared: &Shared, dirs: &AppDirs, hook: Option<&(dyn Fn(JobKind) + Send + Sync)>) {
    while let Some((id, next)) = next_job(shared) {
        // A dropped receiver just means nobody waits for the answer.
        match next {
            Next::Save(job) => {
                if let Some(hook) = hook {
                    hook(JobKind::Save);
                }
                let target = super::file_key(&job.path);
                let result = if job.others.iter().any(|o| super::file_key(o) == target) {
                    Err(PersistError::OpenElsewhere(job.path.clone()))
                } else {
                    let may_clear = || begin_clearing(shared, id);
                    save_workspace_inner(
                        dirs,
                        &job.path,
                        &job.workspace,
                        job.now_ms,
                        job.keep,
                        |_| Ok(()),
                        &may_clear,
                    )
                };
                if result.is_ok() {
                    let mut state = shared.lock();
                    let id = job.workspace.id;
                    let newest = match state.saved.iter().position(|&(i, _)| i == id) {
                        Some(i) => state.saved.remove(i).map_or(0, |(_, rev)| rev),
                        None => 0,
                    }
                    .max(job.revision);
                    state.saved.push_back((id, newest));
                    if state.saved.len() > MAX_PENDING_TARGETS {
                        state.saved.pop_front();
                    }
                }
                let _ = job.reply.send(result);
            }
            Next::Clear(job) => {
                let result = clear_recovery(dirs, job.workspace, job.show_path.as_deref());
                let _ = job.reply.send(result);
            }
            Next::Autosave(job) => {
                if let Some(hook) = hook {
                    hook(JobKind::Autosave);
                }
                let result =
                    write_recovery(dirs, &job.workspace, job.show_path.as_deref()).map(Some);
                let _ = job.reply.send(result);
            }
        }
        if finish(shared, id) {
            return;
        }
    }
}

impl Drop for SaveWriter {
    fn drop(&mut self) {
        self.shared.lock().closing = true;
        self.shared.wake.notify_all();
        // A timeout leaves stuck threads running (detached), still writing what they have,
        // and the process may exit under them.
        self.threads.finish(self.drop_wait);
    }
}
