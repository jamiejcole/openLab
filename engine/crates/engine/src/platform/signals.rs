// SPDX-License-Identifier: GPL-3.0-or-later

//! Graceful shutdown on `SIGTERM`, `SIGINT` and `SIGHUP` (M4.9, ARCHITECTURE §8): the signals
//! are blocked in every thread ([`block`], called before any thread starts, so each inherits
//! the mask) and one thread takes them with `sigwait` ([`spawn_waiter`]) and asks the control
//! loop to shut down, which stops the show and writes the recovery files before the process
//! exits. No signal handler runs, so nothing has to be async-signal-safe.
//!
//! macOS only. Elsewhere the signals keep their default action (the process ends at once): the
//! Windows engine is started by its UI, which sends `goodbye { shutdown: true }`.

/// The signals a graceful shutdown follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Term,
    Int,
    Hup,
}

impl Signal {
    pub fn name(self) -> &'static str {
        match self {
            Self::Term => "SIGTERM",
            Self::Int => "SIGINT",
            Self::Hup => "SIGHUP",
        }
    }
}

/// Blocks the shutdown signals in the calling thread and so in every thread it starts after.
/// Returns whether they are now taken by [`spawn_waiter`] (false where unsupported or on
/// failure: the signals then end the process as before).
pub fn block() -> bool {
    imp::block()
}

/// Starts the thread that waits for a shutdown signal and calls `on_signal` with it, for each
/// one. Call only after [`block`] returned true.
pub fn spawn_waiter(on_signal: impl Fn(Signal) + Send + 'static) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("openlab-signals".into())
        .spawn(move || {
            while let Some(signal) = imp::wait() {
                on_signal(signal);
            }
        })
        .map(|_| ())
}

#[cfg(target_os = "macos")]
mod imp {
    use std::os::raw::c_int;

    use super::Signal;

    /// `sigset_t` on macOS (both architectures): a 32-bit mask, bit `n - 1` for signal `n`.
    type SigSet = u32;

    const SIGHUP: c_int = 1;
    const SIGINT: c_int = 2;
    const SIGTERM: c_int = 15;
    const SIG_BLOCK: c_int = 1;

    unsafe extern "C" {
        fn pthread_sigmask(how: c_int, set: *const SigSet, old: *mut SigSet) -> c_int;
        fn sigwait(set: *const SigSet, sig: *mut c_int) -> c_int;
    }

    fn set() -> SigSet {
        [SIGHUP, SIGINT, SIGTERM]
            .into_iter()
            .fold(0, |mask, s| mask | (1 << (s - 1)))
    }

    pub fn block() -> bool {
        let set = set();
        // SAFETY: `set` is a valid `sigset_t`; a null `old` is allowed. Only the calling
        // thread's mask changes.
        unsafe { pthread_sigmask(SIG_BLOCK, &set, std::ptr::null_mut()) == 0 }
    }

    /// The next shutdown signal; `None` if `sigwait` fails (the thread then ends).
    pub fn wait() -> Option<Signal> {
        let set = set();
        loop {
            let mut sig: c_int = 0;
            // SAFETY: `set` and `sig` are valid for the call; `sigwait` only reads `set` and
            // writes `sig`. The signals in `set` are blocked (`block`), as it requires.
            let rc = unsafe { sigwait(&set, &mut sig) };
            if rc != 0 {
                return None;
            }
            match sig {
                SIGTERM => return Some(Signal::Term),
                SIGINT => return Some(Signal::Int),
                SIGHUP => return Some(Signal::Hup),
                _ => {}
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::Signal;

    pub fn block() -> bool {
        false
    }

    pub fn wait() -> Option<Signal> {
        None
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    unsafe extern "C" {
        fn pthread_kill(thread: *mut std::ffi::c_void, sig: std::os::raw::c_int) -> i32;
        fn pthread_self() -> *mut std::ffi::c_void;
    }

    /// A blocked signal sent to this thread waits until `sigwait` takes it: it neither ends the
    /// test process nor is lost.
    #[test]
    fn a_blocked_signal_is_taken_by_the_waiter_not_the_default_action() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            assert!(block());
            // SAFETY: signals this thread (pending: blocked above), with a valid thread id.
            let rc = unsafe { pthread_kill(pthread_self(), 15) };
            assert_eq!(rc, 0);
            tx.send(imp::wait()).unwrap();
        });
        assert_eq!(rx.recv().unwrap(), Some(Signal::Term));
    }
}
