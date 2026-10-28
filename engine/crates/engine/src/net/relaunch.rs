// SPDX-License-Identifier: GPL-3.0-or-later

//! UI relaunch after a crash (M4.9, NFR-01; ARCHITECTURE §8): when an app connection (one bound
//! to no workspace) ends without `goodbye`, the engine keeps playing, logs it and, if it was
//! started with a relaunch action, runs it after [`Relaunch::delay`] (1 s) unless an app
//! connection is back by then, at most [`MAX_PER_WINDOW`] times per [`WINDOW`]. The action
//! itself (`open -b <bundle id>` on macOS) is the host's (`platform::relaunch`).

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

/// How the server relaunches a UI that went away without `goodbye`.
#[derive(Clone)]
pub struct Relaunch {
    pub delay: Duration,
    pub action: RelaunchFn,
}

impl Relaunch {
    pub fn new(action: RelaunchFn) -> Self {
        Self {
            delay: DELAY,
            action,
        }
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

    #[test]
    fn one_relaunch_waits_at_a_time() {
        let mut b = Budget::default();
        assert!(b.begin());
        assert!(!b.begin());
        b.end();
        assert!(b.begin());
    }
}
