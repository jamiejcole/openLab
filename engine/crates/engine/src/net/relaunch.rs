// SPDX-License-Identifier: GPL-3.0-or-later

//! UI relaunch after a crash (M4.9, NFR-01; ARCHITECTURE §8): when an app connection (one bound
//! to no workspace) ends without `goodbye`, the engine keeps playing, logs it and, if it was
//! started with a relaunch action, runs it after [`Relaunch::delay`] (1 s) unless an app
//! connection is back by then, at most [`MAX_PER_WINDOW`] times per [`WINDOW`]. The action
//! itself (`open -b <bundle id>` on macOS, which brings the app to the front) is the host's
//! (`platform::relaunch`).
//!
//! An app that is still running (reconnecting slowly, or hung) is not relaunched (#168): the
//! app says its pid in `hello`, the engine notes that process's name ([`AppProcess`]), and
//! while a process with that pid and name lives the engine waits, checking again every
//! [`Relaunch::delay`], until an app connection is back or the process is gone (then it
//! relaunches). Matching the name too means a pid reused by another program is not taken for
//! the app.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;

/// Relaunches per [`WINDOW`], at most.
pub const MAX_PER_WINDOW: usize = 3;
pub const WINDOW: Duration = Duration::from_secs(60);
/// The wait before relaunching (ARCHITECTURE §8).
pub const DELAY: Duration = Duration::from_secs(1);

/// The action that starts the app again; `Err` says why it could not. Runs on a blocking
/// thread of the network runtime.
pub type RelaunchFn = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

/// The name of the process with this pid, `None` when there is none (`platform::relaunch::
/// process_name`; tests inject their own).
pub type ProcessNameFn = Arc<dyn Fn(u32) -> Option<String> + Send + Sync>;

/// How the server relaunches a UI that went away without `goodbye`.
#[derive(Clone)]
pub struct Relaunch {
    /// The wait before the first check, and between checks while the app still runs.
    pub delay: Duration,
    pub action: RelaunchFn,
    pub process_name: ProcessNameFn,
}

impl Relaunch {
    pub fn new(action: RelaunchFn) -> Self {
        Self {
            delay: DELAY,
            action,
            process_name: Arc::new(crate::platform::relaunch::process_name),
        }
    }

    /// The app process a `hello` names: `None` when it gave no pid or no process has it.
    pub fn identify(&self, pid: Option<u32>) -> Option<AppProcess> {
        let pid = pid?;
        let name = (self.process_name)(pid)?;
        Some(AppProcess { pid, name })
    }

    /// Whether `app` still runs: its pid has a process of the same name.
    pub fn alive(&self, app: &AppProcess) -> bool {
        (self.process_name)(app.pid).is_some_and(|n| n == app.name)
    }
}

/// The app's process, as its last app connection's `hello` named it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppProcess {
    pub pid: u32,
    pub name: String,
}

/// What a check after the delay decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// An app connection is back, or the server has stopped: nothing to do.
    Done,
    /// The engine is shutting down and no app is back: nothing to do, and the log says why.
    Stopping,
    /// The app's process still runs (slow to reconnect, or hung): check again later. Launching
    /// it would only bring it to the front over what the operator is using.
    Wait,
    /// The app is gone: start it.
    Relaunch,
}

/// The decision, from what the check saw: an app connection back, the server stopped, the
/// engine's shutdown begun (`net::Stopping`), the app's process alive.
pub fn next(app_back: bool, server_stopped: bool, engine_stopping: bool, app_alive: bool) -> Next {
    if app_back || server_stopped {
        Next::Done
    } else if engine_stopping {
        Next::Stopping
    } else if app_alive {
        Next::Wait
    } else {
        Next::Relaunch
    }
}

impl std::fmt::Debug for Relaunch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Relaunch")
            .field("delay", &self.delay)
            .finish_non_exhaustive()
    }
}

/// The relaunches allowed: at most [`MAX_PER_WINDOW`] in any [`WINDOW`], and one waiting at a
/// time (several app connections of one crashed app relaunch it once).
#[derive(Debug, Default)]
pub struct Budget {
    times: VecDeque<Instant>,
    waiting: bool,
}

impl Budget {
    /// A loss to act on: `false` while a relaunch is already waiting.
    pub fn begin(&mut self) -> bool {
        !std::mem::replace(&mut self.waiting, true)
    }

    /// The wait is over (whatever it decides).
    pub fn end(&mut self) {
        self.waiting = false;
    }

    /// Takes one relaunch at `now`, or `false` when the window's are spent.
    pub fn take(&mut self, now: Instant) -> bool {
        while self
            .times
            .front()
            .is_some_and(|&t| now.saturating_duration_since(t) >= WINDOW)
        {
            self.times.pop_front();
        }
        if self.times.len() >= MAX_PER_WINDOW {
            return false;
        }
        self.times.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_per_minute_then_none_until_the_oldest_is_a_minute_old() {
        let t0 = Instant::now();
        let mut b = Budget::default();
        let at = |s: u64| t0 + Duration::from_secs(s);
        assert!(b.take(at(0)));
        assert!(b.take(at(10)));
        assert!(b.take(at(20)));
        assert!(!b.take(at(30)));
        assert!(!b.take(at(59)));
        assert!(b.take(at(60)), "the first is a minute old");
        assert!(!b.take(at(61)));
        assert!(b.take(at(200)));
    }

    /// #168: while the app's process lives it is not relaunched (it is reconnecting, or hung);
    /// once it is gone it is. A connection back, or a stopped server, ends the wait; so does
    /// the engine's shutdown, which is said only when no app is back (an app that reconnected
    /// was not "not relaunched").
    #[test]
    fn a_running_app_is_waited_for_and_a_gone_one_relaunched() {
        assert_eq!(next(false, false, false, true), Next::Wait);
        assert_eq!(next(false, false, false, false), Next::Relaunch);
        for alive in [false, true] {
            assert_eq!(next(true, false, false, alive), Next::Done);
            assert_eq!(next(false, true, false, alive), Next::Done);
            assert_eq!(next(false, false, true, alive), Next::Stopping);
            assert_eq!(
                next(true, false, true, alive),
                Next::Done,
                "the app is back"
            );
        }
    }

    /// The app is alive only while its pid has a process of the name it had at hello: a pid
    /// reused by another program, or one with no process, is not the app.
    #[test]
    fn the_app_is_its_pid_and_name() {
        let names = Arc::new(std::sync::Mutex::new(Some("openLab".to_owned())));
        let probe = names.clone();
        let mut r = Relaunch::new(Arc::new(|| Ok(())));
        r.process_name = Arc::new(move |pid| {
            if pid == 812 {
                probe.lock().unwrap().clone()
            } else {
                None
            }
        });
        assert_eq!(r.identify(None), None, "no pid in hello");
        assert_eq!(r.identify(Some(9)), None, "no such process");
        let app = r.identify(Some(812)).unwrap();
        assert_eq!(app.name, "openLab");
        assert!(r.alive(&app));
        *names.lock().unwrap() = Some("zsh".into());
        assert!(!r.alive(&app), "the pid was reused");
        *names.lock().unwrap() = None;
        assert!(!r.alive(&app), "the process is gone");
    }

    #[test]
    fn one_relaunch_waits_at_a_time() {
        let mut b = Budget::default();
        assert!(b.begin());
        assert!(!b.begin());
        b.end();
        assert!(b.begin());
    }
}
