// SPDX-License-Identifier: GPL-3.0-or-later

//! Writer threads whose end is waited for, but only so long: a write stuck on a hung volume
//! must not keep quit (or a relaunch) waiting. The save writer and the settings writer both
//! end their threads through [`BoundedThreads::finish`].

use std::io;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

/// Threads started with [`BoundedThreads::spawn`]. Each holds a clone of one sender, so the
/// receiver disconnects when the last of them ends.
pub(crate) struct BoundedThreads {
    token: Option<Sender<()>>,
    ended: Receiver<()>,
    handles: Vec<JoinHandle<()>>,
}

impl BoundedThreads {
    pub(crate) fn new() -> Self {
        let (token, ended) = crossbeam_channel::bounded(0);
        Self {
            token: Some(token),
            ended,
            handles: Vec::new(),
        }
    }

    /// Starts `body` on a thread named `name`.
    pub(crate) fn spawn(
        &mut self,
        name: &str,
        body: impl FnOnce() + Send + 'static,
    ) -> io::Result<()> {
        let Some(token) = self.token.clone() else {
            return Err(io::Error::other("the writer is stopping"));
        };
        let handle = thread::Builder::new().name(name.into()).spawn(move || {
            let _token = token;
            body();
        })?;
        // Finished threads need no join: dropping a handle only detaches.
        self.handles.retain(|h| !h.is_finished());
        self.handles.push(handle);
        Ok(())
    }

    /// Waits at most `wait` for every thread to end (the caller has told them to) and joins
    /// them if they did. Otherwise they are left running, detached, and the process may exit
    /// under them. True when they all ended.
    pub(crate) fn finish(&mut self, wait: Duration) -> bool {
        self.token = None;
        let ended = matches!(
            self.ended.recv_timeout(wait),
            Err(RecvTimeoutError::Disconnected)
        );
        for handle in self.handles.drain(..) {
            if ended {
                let _ = handle.join();
            }
        }
        ended
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_joins_threads_that_end_and_gives_up_on_one_that_does_not() {
        let mut threads = BoundedThreads::new();
        threads.spawn("quick", || {}).unwrap();
        assert!(threads.finish(Duration::from_secs(20)));

        let mut threads = BoundedThreads::new();
        let (release_tx, release) = crossbeam_channel::bounded::<()>(0);
        threads
            .spawn("stuck", move || {
                let _ = release.recv();
            })
            .unwrap();
        assert!(!threads.finish(Duration::ZERO));
        assert!(threads.spawn("late", || {}).is_err());
        drop(release_tx);
    }
}
