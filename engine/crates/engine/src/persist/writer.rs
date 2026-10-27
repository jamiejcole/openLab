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
//!   pushes the oldest waiting autosave into a write-through queue that runs before the rest:
//!   it is written, never dropped. The write-through queue still coalesces per target, so the
//!   total is bounded by the number of distinct targets in use (at most the open workspaces,
//!   8 by WS-02);
//! - manual saves run before pending autosaves, and an autosave whose revision is not newer
//!   than the last durably saved one of its workspace is dropped (no recovery file for
//!   already-saved state).
//!
//! Dropping the writer finishes the running job and any pending manual saves and discards
//! pending autosaves, so shutdown never waits behind a backlog.

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};

use openlab_core::{Workspace, WorkspaceId};

use super::{AppDirs, PersistError, SaveReport, clear_recovery, save_workspace, write_recovery};

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
    reply: SaveReply,
}

/// Hard cap on targets waiting in the normal autosave queue; see the module docs.
pub const MAX_PENDING_TARGETS: usize = 16;

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
}

#[derive(Default)]
struct State {
    saves: VecDeque<SaveJob>,
    clears: VecDeque<ClearJob>,
    /// Pending autosaves, at most one per target, oldest first.
    autosaves: VecDeque<AutosaveJob>,
    /// Autosaves pushed out of `autosaves` by the cap; written before it, at most one per target.
    forced: VecDeque<AutosaveJob>,
    /// Newest revision per workspace that a manual save has made durable (the most recent
    /// [`MAX_PENDING_TARGETS`] workspaces; an evicted one only risks one redundant autosave).
    saved: VecDeque<(WorkspaceId, u64)>,
    closing: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic elsewhere must not stop saving.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

type Hook = Box<dyn Fn(JobKind) + Send>;

/// Owns the thread and its queue.
pub struct SaveWriter {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl SaveWriter {
    pub fn spawn(dirs: AppDirs) -> io::Result<Self> {
        Self::spawn_inner(dirs, None)
    }

    /// Like [`spawn`](Self::spawn), with `hook` called on the writer thread just before each
    /// job runs. Tests block in it to hold the writer busy without sleeping.
    pub fn spawn_with_hook(
        dirs: AppDirs,
        hook: impl Fn(JobKind) + Send + 'static,
    ) -> io::Result<Self> {
        Self::spawn_inner(dirs, Some(Box::new(hook)))
    }

    fn spawn_inner(dirs: AppDirs, hook: Option<Hook>) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("openlab-writer".into())
            .spawn(move || run(&worker, &dirs, hook.as_deref()))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
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
            reply,
        };
        match state.saves.iter_mut().find(|p| p.path == job.path) {
            Some(pending) => {
                let old = std::mem::replace(pending, job);
                let _ = old.reply.send(Err(PersistError::Superseded));
            }
            None => state.saves.push_back(job),
        }
        drop(state);
        self.shared.wake.notify_one();
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
                state.forced.push_back(oldest);
            }
            state.autosaves.push_back(job);
        }
        drop(state);
        self.shared.wake.notify_one();
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
        self.shared.wake.notify_one();
        answer
    }
}

enum Next {
    Save(SaveJob),
    Clear(ClearJob),
    Autosave(AutosaveJob),
}

fn next_job(shared: &Shared) -> Option<Next> {
    let mut state = shared.lock();
    loop {
        if let Some(job) = state.saves.pop_front() {
            return Some(Next::Save(job));
        }
        if let Some(job) = state.clears.pop_front() {
            return Some(Next::Clear(job));
        }
        if state.closing {
            let State {
                autosaves, forced, ..
            } = &mut *state;
            for job in forced.drain(..).chain(autosaves.drain(..)) {
                let _ = job.reply.send(Ok(None));
            }
            return None;
        }
        if let Some(job) = state
            .forced
            .pop_front()
            .or_else(|| state.autosaves.pop_front())
        {
            let covered = state
                .saved
                .iter()
                .any(|&(id, rev)| id == job.workspace.id && job.revision <= rev);
            if covered {
                let _ = job.reply.send(Ok(None));
                continue;
            }
            return Some(Next::Autosave(job));
        }
        state = shared
            .wake
            .wait(state)
            .unwrap_or_else(PoisonError::into_inner);
    }
}

fn run(shared: &Shared, dirs: &AppDirs, hook: Option<&(dyn Fn(JobKind) + Send)>) {
    while let Some(next) = next_job(shared) {
        // A dropped receiver just means nobody waits for the answer.
        match next {
            Next::Save(job) => {
                if let Some(hook) = hook {
                    hook(JobKind::Save);
                }
                let result = save_workspace(dirs, &job.path, &job.workspace, job.now_ms, job.keep);
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
    }
}

impl Drop for SaveWriter {
    fn drop(&mut self) {
        self.shared.lock().closing = true;
        self.shared.wake.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
