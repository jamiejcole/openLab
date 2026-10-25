// SPDX-License-Identifier: GPL-3.0-or-later

//! The writer thread: saves and autosaves run here, never on the control thread
//! (`save_show` serialises and re-validates, up to about a second for a large show).
//!
//! The queue is bounded by coalescing, because the disk may be slow or hung (network share,
//! sleeping USB drive) while autosave keeps firing:
//! - at most one pending manual save per target file; a newer one replaces it;
//! - at most one pending autosave; a newer one replaces it;
//! - manual saves run before a pending autosave, and an autosave whose revision is not newer
//!   than the last durably saved one is dropped (no recovery file for already-saved state).
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

use super::{AppDirs, PersistError, SaveReport, save_workspace, write_recovery};

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

struct AutosaveJob {
    workspace: Box<Workspace>,
    show_path: Option<PathBuf>,
    revision: u64,
    reply: AutosaveReply,
}

#[derive(Default)]
struct State {
    saves: VecDeque<SaveJob>,
    autosave: Option<AutosaveJob>,
    /// Newest revision of a workspace that a manual save has made durable.
    saved: Option<(WorkspaceId, u64)>,
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
    /// before the first save). It is skipped (`Ok(None)`) if a newer autosave replaces it, or
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
        if let Some(old) = state.autosave.replace(job) {
            let _ = old.reply.send(Ok(None));
        }
        drop(state);
        self.shared.wake.notify_one();
        answer
    }
}

enum Next {
    Save(SaveJob),
    Autosave(AutosaveJob),
}

fn next_job(shared: &Shared) -> Option<Next> {
    let mut state = shared.lock();
    loop {
        if let Some(job) = state.saves.pop_front() {
            return Some(Next::Save(job));
        }
        if state.closing {
            if let Some(job) = state.autosave.take() {
                let _ = job.reply.send(Ok(None));
            }
            return None;
        }
        if let Some(job) = state.autosave.take() {
            let covered = state
                .saved
                .is_some_and(|(id, rev)| id == job.workspace.id && job.revision <= rev);
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
                    let newest = match state.saved {
                        Some((id, rev)) if id == job.workspace.id => rev.max(job.revision),
                        _ => job.revision,
                    };
                    state.saved = Some((job.workspace.id, newest));
                }
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
