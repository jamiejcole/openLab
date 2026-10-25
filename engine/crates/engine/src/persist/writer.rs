// SPDX-License-Identifier: GPL-3.0-or-later

//! The writer thread: saves and autosaves run here, never on the control thread
//! (`save_show` serialises and re-validates, up to about a second for a large show).

use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use openlab_core::Workspace;

use super::{AppDirs, PersistError, SaveReport, save_workspace, write_recovery};

type Reply<T> = Sender<Result<T, PersistError>>;

enum Job {
    Save {
        workspace: Box<Workspace>,
        path: PathBuf,
        now_ms: i64,
        keep: u32,
        reply: Reply<SaveReport>,
    },
    Autosave {
        workspace: Box<Workspace>,
        reply: Reply<PathBuf>,
    },
}

/// Owns the thread and its queue. Jobs run in order. Dropping the writer finishes queued jobs
/// and joins the thread.
pub struct SaveWriter {
    jobs: Option<Sender<Job>>,
    thread: Option<JoinHandle<()>>,
}

impl SaveWriter {
    pub fn spawn(dirs: AppDirs) -> io::Result<Self> {
        let (jobs, queue) = mpsc::channel::<Job>();
        let thread = thread::Builder::new()
            .name("openlab-writer".into())
            .spawn(move || {
                for job in queue {
                    // A dropped receiver just means nobody waits for the answer.
                    match job {
                        Job::Save {
                            workspace,
                            path,
                            now_ms,
                            keep,
                            reply,
                        } => {
                            let _ =
                                reply.send(save_workspace(&dirs, &path, &workspace, now_ms, keep));
                        }
                        Job::Autosave { workspace, reply } => {
                            let _ = reply.send(write_recovery(&dirs, &workspace));
                        }
                    }
                }
            })?;
        Ok(Self {
            jobs: Some(jobs),
            thread: Some(thread),
        })
    }

    /// Queues a manual save of `workspace` (a clone taken by the caller, so edits can go on).
    /// The result arrives on the returned channel.
    pub fn save(
        &self,
        workspace: Workspace,
        path: PathBuf,
        now_ms: i64,
        keep: u32,
    ) -> Receiver<Result<SaveReport, PersistError>> {
        let (reply, answer) = mpsc::channel();
        self.submit(
            Job::Save {
                workspace: Box::new(workspace),
                path,
                now_ms,
                keep,
                reply: reply.clone(),
            },
            || {
                let _ = reply.send(Err(PersistError::WriterGone));
            },
        );
        answer
    }

    /// Queues an autosave of `workspace` to its recovery file.
    pub fn autosave(&self, workspace: Workspace) -> Receiver<Result<PathBuf, PersistError>> {
        let (reply, answer) = mpsc::channel();
        self.submit(
            Job::Autosave {
                workspace: Box::new(workspace),
                reply: reply.clone(),
            },
            || {
                let _ = reply.send(Err(PersistError::WriterGone));
            },
        );
        answer
    }

    fn submit(&self, job: Job, on_gone: impl FnOnce()) {
        let sent = self
            .jobs
            .as_ref()
            .is_some_and(|jobs| jobs.send(job).is_ok());
        if !sent {
            on_gone();
        }
    }
}

impl Drop for SaveWriter {
    fn drop(&mut self) {
        drop(self.jobs.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
