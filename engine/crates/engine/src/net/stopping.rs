// SPDX-License-Identifier: GPL-3.0-or-later

//! The engine's shutdown has begun (docs/control.md "Process lifecycle"). Set once, by whoever
//! sees it first: a `goodbye { shutdown: true }` on any connection (the server), a signal or
//! stdin closing (the host), or the end of the control loop (the host, as a backstop). From
//! then on an app connection that ends is no crash, so the app is not relaunched into an engine
//! about to exit, and the host marks `engine.json` as stopping.
//!
//! The server's own `shutdown` comes much later: after the final recovery writes and the bounded
//! drops (up to `SHUTDOWN_WAIT` + `DROP_WAIT`), which is the window this closes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::{Receiver, Sender};

/// Clones share one flag. [`Stopping::default`] has no one to tell (tests, a server alone).
#[derive(Clone, Default)]
pub struct Stopping(Arc<Inner>);

#[derive(Default)]
struct Inner {
    set: AtomicBool,
    /// Told once, when the flag is set (the host's `engine.json` marking).
    notify: Option<Sender<()>>,
}

impl Stopping {
    /// A flag and the receiver that gets one message when it is set (and is disconnected once
    /// every clone is gone without it having been set).
    pub fn new() -> (Self, Receiver<()>) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let flag = Self(Arc::new(Inner {
            set: AtomicBool::new(false),
            notify: Some(tx),
        }));
        (flag, rx)
    }

    /// The shutdown has begun. Only the first call tells the receiver; never blocks.
    pub fn begin(&self) {
        if !self.0.set.swap(true, Ordering::SeqCst)
            && let Some(notify) = &self.0.notify
        {
            let _ = notify.try_send(());
        }
    }

    pub fn is_set(&self) -> bool {
        self.0.set.load(Ordering::SeqCst)
    }
}

impl std::fmt::Debug for Stopping {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Stopping").field(&self.is_set()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_begin_sets_every_clone_and_tells_the_receiver_once() {
        let (flag, rx) = Stopping::new();
        let other = flag.clone();
        assert!(!other.is_set());
        assert!(rx.try_recv().is_err());
        flag.begin();
        other.begin();
        assert!(other.is_set() && flag.is_set());
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err(), "told once");
        // A flag nobody listens to is still a flag.
        let alone = Stopping::default();
        alone.begin();
        assert!(alone.is_set());
    }

    #[test]
    fn a_flag_dropped_unset_disconnects_its_receiver() {
        let (flag, rx) = Stopping::new();
        drop(flag);
        assert!(matches!(rx.recv(), Err(crossbeam_channel::RecvError)));
    }
}
