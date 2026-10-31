// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::panic, dead_code)]

//! The one hang guard for a test that waits on a thread it started (a disk worker, the document
//! worker, a watcher). The bound is a count of polls, not a deadline read from the clock: a slow
//! runner only needs more polls, and the bound is a floor on the time a hang is given (every poll
//! after the first thousand sleeps 1 ms). A wait that can be a channel receive or a condvar is
//! that instead.

use std::time::Duration;

/// Polls that only yield: the common case is an answer within microseconds.
const YIELDS: u32 = 1_000;
/// Polls after that, 1 ms apart: at least 30 s before a hang is declared.
const SLEEPS: u32 = 30_000;

/// Polls `done` until it holds; fails the test naming `what` if it never does.
pub fn spin_until(what: &str, mut done: impl FnMut() -> bool) {
    for step in 0..YIELDS + SLEEPS {
        if done() {
            return;
        }
        if step < YIELDS {
            std::thread::yield_now();
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    panic!("hang: {what}");
}
