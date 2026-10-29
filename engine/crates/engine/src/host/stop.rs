// SPDX-License-Identifier: GPL-3.0-or-later

//! How a shutdown signal ends the engine, whatever it is doing (docs/control.md "Process
//! lifecycle"). The signal thread starts right after the instance lock, so a start stuck in the
//! audio system (a device open that never returns) still answers SIGTERM and Ctrl-C:
//!
//! - while starting (no control loop yet, so no workspace and nothing to save): exit at once;
//! - while running: the graceful shutdown (the control loop stops the show and writes the
//!   recovery files);
//! - while stopping: the first signal changes nothing (the shutdown is under way, perhaps after
//!   `goodbye`); a second one forces the exit at once ("press Ctrl-C again").
//!
//! From the first signal while running, or the end of the control loop however it ended
//! (`goodbye`, stdin), whichever comes first, one deadline bounds the whole exit:
//! [`EXIT_DEADLINE`]. A forced exit removes this run's `engine.json` (a client must not try a
//! port nobody serves), leaves the run marker (the next start reports the unclean end) and runs
//! no exit handlers ([`exit_now`]).
//!
//! [`exit_now`]: crate::platform::exit::exit_now

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_channel::Sender;
use openlab_core::protocol::LogLevel;

use super::{LOG_FLUSH_WAIT, engine_file};
use crate::control::{Inbox, SHUTDOWN_WAIT};
use crate::net::Stopping;
use crate::persist::DROP_WAIT;
use crate::platform::signals::Signal;

/// From the start of the shutdown to a forced exit. It covers the graceful path at its worst
/// (final recovery writes, then the save and settings writers' bounded drops one after the
/// other, then the log flush: 10 + 5 + 5 + 3 s) with a second to spare, and stays under the
/// 25 s the app gives an engine after SIGTERM before it sends SIGKILL.
pub const EXIT_DEADLINE: Duration = SHUTDOWN_WAIT
    .saturating_add(DROP_WAIT)
    .saturating_add(DROP_WAIT)
    .saturating_add(LOG_FLUSH_WAIT)
    .saturating_add(Duration::from_secs(1));

/// Exit status of a forced exit (a signal while starting is a clean, early end: 0).
const FORCED: i32 = 1;

enum Phase {
    Starting,
    /// The control loop is about to run (or runs): a signal asks it to shut down, and says the
    /// shutdown has begun.
    Running(Sender<Inbox>, Stopping),
    /// The shutdown is under way; `signalled`: a signal has been seen since it began (the next
    /// one forces the exit).
    Stopping {
        signalled: bool,
    },
}

struct State {
    phase: Phase,
    /// The deadline's watchdog is running.
    armed: bool,
}

/// Shared by the host and the signal thread.
pub struct Stop {
    state: Mutex<State>,
    /// This run's `engine.json`, removed by a forced exit when it still names this process.
    engine_file: Option<PathBuf>,
    deadline: Duration,
    exit: Box<dyn Fn(i32) + Send + Sync>,
}

impl Stop {
    /// `exit` ends the process (tests pass one that records the call instead).
    pub fn new(
        engine_file: Option<PathBuf>,
        exit: impl Fn(i32) + Send + Sync + 'static,
    ) -> Arc<Self> {
        Self::with_deadline(engine_file, EXIT_DEADLINE, exit)
    }

    fn with_deadline(
        engine_file: Option<PathBuf>,
        deadline: Duration,
        exit: impl Fn(i32) + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                phase: Phase::Starting,
                armed: false,
            }),
            engine_file,
            deadline,
            exit: Box::new(exit),
        })
    }

    /// The control loop is about to run: from now on a signal is a graceful shutdown, which
    /// sets `stopping` first.
    pub fn running(&self, inbox: Sender<Inbox>, stopping: Stopping) {
        let mut state = self.lock();
        if matches!(state.phase, Phase::Starting) {
            state.phase = Phase::Running(inbox, stopping);
        }
    }

    /// The control loop has ended: the rest of the exit is bounded (if a signal has not
    /// started the deadline already).
    pub fn loop_ended(self: &Arc<Self>) {
        let mut state = self.lock();
        let signalled = matches!(state.phase, Phase::Stopping { signalled: true });
        state.phase = Phase::Stopping { signalled };
        let arm = !std::mem::replace(&mut state.armed, true);
        drop(state);
        if arm {
            self.force_later();
        }
    }

    /// A shutdown signal arrived (module docs).
    pub fn signal(self: &Arc<Self>, signal: Signal) {
        let mut state = self.lock();
        let before = std::mem::replace(&mut state.phase, Phase::Stopping { signalled: true });
        match before {
            Phase::Starting => {
                drop(state);
                self.force(0, &format!("{}: exiting while starting", signal.name()));
            }
            Phase::Running(inbox, stopping) => {
                let arm = !std::mem::replace(&mut state.armed, true);
                drop(state);
                stopping.begin();
                let _ = inbox.send(Inbox::Log {
                    level: LogLevel::Info,
                    message: format!("{}: shutting down", signal.name()),
                });
                let _ = inbox.send(Inbox::Shutdown);
                if arm {
                    self.force_later();
                }
            }
            Phase::Stopping { signalled: false } => {
                drop(state);
                super::say(&format!(
                    "{}: already shutting down; send it again to exit now",
                    signal.name()
                ));
            }
            Phase::Stopping { signalled: true } => {
                drop(state);
                self.force(FORCED, &format!("{} again: exiting now", signal.name()));
            }
        }
    }

    fn force_later(self: &Arc<Self>) {
        let stop = Arc::clone(self);
        let spawned = thread::Builder::new()
            .name("openlab-exit".into())
            .spawn(move || {
                thread::sleep(stop.deadline);
                stop.force(FORCED, "the shutdown did not finish in time; exiting");
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

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        crate::lock(&self.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::host::engine_file::{EngineFile, EngineInfo};

    fn recorded(after: Duration) -> (Arc<Stop>, crossbeam_channel::Receiver<i32>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let stop = Stop::with_deadline(None, after, move |code| {
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
            stopping: false,
        };
        let _file = EngineFile::write(&path, info).unwrap();
        let (tx, exits) = crossbeam_channel::unbounded();
        let stop = Stop::with_deadline(Some(path.clone()), GUARD, move |code| {
            let _ = tx.send(code);
        });
        let (inbox_tx, inbox) = crossbeam_channel::unbounded();
        let stopping = Stopping::default();
        stop.running(inbox_tx, stopping.clone());
        stop.signal(Signal::Int);
        let sent: Vec<Inbox> = inbox.try_iter().collect();
        assert!(matches!(sent.last(), Some(Inbox::Shutdown)), "{sent:?}");
        assert!(stopping.is_set(), "the shutdown has begun: no app relaunch");
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
        stop.running(inbox_tx, Stopping::default());
        stop.signal(Signal::Term);
        assert_eq!(exits.recv_timeout(GUARD), Ok(FORCED));
    }

    #[test]
    fn an_exit_that_hangs_after_the_loop_ended_is_forced_after_the_bound() {
        let (stop, exits) = recorded(Duration::ZERO);
        let (inbox_tx, _inbox) = crossbeam_channel::unbounded();
        stop.running(inbox_tx, Stopping::default());
        stop.loop_ended();
        assert_eq!(exits.recv_timeout(GUARD), Ok(FORCED));
    }

    /// After the loop ended (say after `goodbye`), a first signal (the app ending an engine
    /// that no longer answers) lets the exit finish: only a second one forces it.
    #[test]
    fn after_the_loop_ended_only_a_second_signal_forces_the_exit() {
        let (stop, exits) = recorded(GUARD);
        let (inbox_tx, _inbox) = crossbeam_channel::unbounded();
        stop.running(inbox_tx, Stopping::default());
        stop.loop_ended();
        stop.signal(Signal::Term);
        assert!(
            exits.try_recv().is_err(),
            "the first signal forced the exit"
        );
        stop.signal(Signal::Term);
        assert_eq!(exits.try_recv(), Ok(FORCED));
    }

    /// The deadline covers the graceful path at its worst and beats the app's SIGKILL (25 s).
    #[test]
    fn the_exit_deadline_covers_the_graceful_path_and_beats_the_apps_kill() {
        let worst = SHUTDOWN_WAIT + DROP_WAIT + DROP_WAIT + LOG_FLUSH_WAIT;
        assert!(EXIT_DEADLINE > worst, "{EXIT_DEADLINE:?} <= {worst:?}");
        assert!(EXIT_DEADLINE < Duration::from_secs(25), "{EXIT_DEADLINE:?}");
    }
}
