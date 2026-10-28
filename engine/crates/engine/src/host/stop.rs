// SPDX-License-Identifier: GPL-3.0-or-later

//! How a shutdown signal ends the engine, whatever it is doing (docs/control.md "Process
//! lifecycle"). The signal thread starts right after the instance lock, so a start stuck in the
//! audio system (a device open that never returns) still answers SIGTERM and Ctrl-C:
//!
//! - while starting (no control loop yet, so no workspace and nothing to save): exit at once;
//! - while running: the graceful shutdown (the control loop stops the show and writes the
//!   recovery files), with a watchdog that forces the exit after [`FORCE_AFTER_SIGNAL`];
//! - while stopping: a second signal forces the exit at once ("press Ctrl-C again").
//!
//! However the loop ended (a signal, `goodbye`, stdin), once it has ended the rest of the exit
//! is bounded by [`FORCE_AFTER_LOOP`]. A forced exit removes this run's `engine.json` (a client
//! must not try a port nobody serves), leaves the run marker (the next start reports the
//! unclean end) and runs no exit handlers ([`exit_now`]).
//!
//! [`exit_now`]: crate::platform::exit::exit_now

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use crossbeam_channel::Sender;
use openlab_core::protocol::LogLevel;

use super::engine_file;
use crate::control::Inbox;
use crate::platform::signals::Signal;

/// From a signal to a forced exit. The graceful path fits well inside it (final recovery
/// writes 10 s, the writers' bounded drops, the log flush 3 s), and it stays under the 25 s the
/// app gives an engine after SIGTERM before it sends SIGKILL.
pub const FORCE_AFTER_SIGNAL: Duration = Duration::from_secs(20);
/// From the end of the control loop to a forced exit: the server stops, the log is flushed
/// (at most 3 s) and the files go.
pub const FORCE_AFTER_LOOP: Duration = Duration::from_secs(8);

/// Exit status of a forced exit (a signal while starting is a clean, early end: 0).
const FORCED: i32 = 1;

enum Phase {
    Starting,
    /// The control loop is about to run (or runs): a signal asks it to shut down.
    Running(Sender<Inbox>),
    Stopping,
}

/// Shared by the host and the signal thread.
pub struct Stop {
    phase: Mutex<Phase>,
    /// This run's `engine.json`, removed by a forced exit when it still names this process.
    engine_file: Option<PathBuf>,
    after_signal: Duration,
    after_loop: Duration,
    exit: Box<dyn Fn(i32) + Send + Sync>,
}

impl Stop {
    /// `exit` ends the process (tests pass one that records the call instead).
    pub fn new(
        engine_file: Option<PathBuf>,
        exit: impl Fn(i32) + Send + Sync + 'static,
    ) -> Arc<Self> {
        Self::with_bounds(engine_file, FORCE_AFTER_SIGNAL, FORCE_AFTER_LOOP, exit)
    }

    fn with_bounds(
        engine_file: Option<PathBuf>,
        after_signal: Duration,
        after_loop: Duration,
        exit: impl Fn(i32) + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            phase: Mutex::new(Phase::Starting),
            engine_file,
            after_signal,
            after_loop,
            exit: Box::new(exit),
        })
    }

    /// The control loop is about to run: from now on a signal is a graceful shutdown.
    pub fn running(&self, inbox: Sender<Inbox>) {
        let mut phase = self.lock();
        if matches!(*phase, Phase::Starting) {
            *phase = Phase::Running(inbox);
        }
    }

    /// The control loop has ended: the rest of the exit is bounded.
    pub fn loop_ended(self: &Arc<Self>) {
        *self.lock() = Phase::Stopping;
        self.force_later(
            self.after_loop,
            "the engine did not exit after its shutdown",
        );
    }

    /// A shutdown signal arrived (module docs).
    pub fn signal(self: &Arc<Self>, signal: Signal) {
        let mut phase = self.lock();
        match std::mem::replace(&mut *phase, Phase::Stopping) {
            Phase::Starting => {
                drop(phase);
                self.force(0, &format!("{}: exiting while starting", signal.name()));
            }
            Phase::Running(inbox) => {
                drop(phase);
                let _ = inbox.send(Inbox::Log {
                    level: LogLevel::Info,
                    message: format!("{}: shutting down", signal.name()),
                });
                let _ = inbox.send(Inbox::Shutdown);
                self.force_later(
                    self.after_signal,
                    "the shutdown did not finish in time; exiting",
                );
            }
            Phase::Stopping => {
                drop(phase);
                self.force(FORCED, &format!("{} again: exiting now", signal.name()));
            }
        }
    }

    fn force_later(self: &Arc<Self>, after: Duration, why: &'static str) {
        let stop = Arc::clone(self);
        let spawned = thread::Builder::new()
            .name("openlab-exit".into())
            .spawn(move || {
                thread::sleep(after);
                stop.force(FORCED, why);
            });
        if spawned.is_err() {
            super::say("could not start the exit watchdog; a hung shutdown needs SIGKILL");
        }
    }

    fn force(&self, code: i32, why: &str) {
        if let Some(path) = &self.engine_file
            && engine_file::read(path).is_some_and(|info| info.pid == std::process::id())
        {
            let _ = fs::remove_file(path);
        }
        super::say(why);
        (self.exit)(code);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Phase> {
        self.phase.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::host::engine_file::{EngineFile, EngineInfo};

    fn recorded(after: Duration) -> (Arc<Stop>, crossbeam_channel::Receiver<i32>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let stop = Stop::with_bounds(None, after, after, move |code| {
            let _ = tx.send(code);
        });
        (stop, rx)
    }

    /// A hang guard, not a timing assertion.
    const GUARD: Duration = Duration::from_secs(20);

    #[test]
    fn a_signal_while_starting_exits_at_once_and_cleanly() {
        let (stop, exits) = recorded(GUARD);
        stop.signal(Signal::Term);
        assert_eq!(exits.try_recv(), Ok(0));
    }

    #[test]
    fn a_signal_while_running_asks_the_loop_and_a_second_one_forces_the_exit() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("engine.json");
        let info = EngineInfo {
            pid: std::process::id(),
            port: 50_000,
            token: "t".into(),
            version: "0.0.1".into(),
            protocol_version: 1,
            started_ms: 0,
        };
        let _file = EngineFile::write(&path, info).unwrap();
        let (tx, exits) = crossbeam_channel::unbounded();
        let stop = Stop::with_bounds(Some(path.clone()), GUARD, GUARD, move |code| {
            let _ = tx.send(code);
        });
        let (inbox_tx, inbox) = crossbeam_channel::unbounded();
        stop.running(inbox_tx);
        stop.signal(Signal::Int);
        let sent: Vec<Inbox> = inbox.try_iter().collect();
        assert!(matches!(sent.last(), Some(Inbox::Shutdown)), "{sent:?}");
        assert!(
            exits.try_recv().is_err(),
            "the graceful shutdown runs first"
        );
        assert!(path.exists());
        stop.signal(Signal::Int);
        assert_eq!(exits.try_recv(), Ok(FORCED));
        assert!(
            !path.exists(),
            "a forced exit removes this run's engine.json"
        );
    }

    #[test]
    fn a_shutdown_that_hangs_after_a_signal_is_forced_after_the_bound() {
        let (stop, exits) = recorded(Duration::ZERO);
        let (inbox_tx, _inbox) = crossbeam_channel::unbounded();
        stop.running(inbox_tx);
        stop.signal(Signal::Term);
        assert_eq!(exits.recv_timeout(GUARD), Ok(FORCED));
    }

    #[test]
    fn an_exit_that_hangs_after_the_loop_ended_is_forced_after_the_bound() {
        let (stop, exits) = recorded(Duration::ZERO);
        let (inbox_tx, _inbox) = crossbeam_channel::unbounded();
        stop.running(inbox_tx);
        stop.loop_ended();
        assert_eq!(exits.recv_timeout(GUARD), Ok(FORCED));
    }
}
