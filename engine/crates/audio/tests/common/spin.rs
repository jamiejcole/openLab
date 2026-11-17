// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::panic, clippy::expect_used, dead_code)]

//! The one hang guard for a test that waits on a thread or a program it started (a disk worker,
//! the document worker, a watcher, the stage host making test media). The bound is a count of
//! polls, not a deadline read from the clock: a slow runner only needs more polls, and the bound
//! is a floor on the time a hang is given (every poll after the first thousand sleeps 1 ms). A
//! wait that can be a channel receive or a condvar is that instead.

use std::io::Read;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::Duration;

/// Polls that only yield: the common case is an answer within microseconds.
const YIELDS: u32 = 1_000;
/// Polls after that, 1 ms apart: at least 30 s before a hang is declared.
const SLEEPS: u32 = 30_000;

/// Polls `done` until it holds; fails the test naming `what` if it never does.
pub fn spin_until(what: &str, done: impl FnMut() -> bool) {
    if !spin_for(Duration::from_millis(u64::from(SLEEPS)), done) {
        panic!("hang: {what}");
    }
}

/// Polls `done` until it holds, for at least `at_least` (in 1 ms sleeps after the yields);
/// whether it did.
fn spin_for(at_least: Duration, mut done: impl FnMut() -> bool) -> bool {
    let sleeps = u32::try_from(at_least.as_millis()).unwrap_or(u32::MAX);
    for step in 0..YIELDS.saturating_add(sleeps) {
        if done() {
            return true;
        }
        if step < YIELDS {
            std::thread::yield_now();
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    done()
}

/// Runs `command` as [`Command::output`] does (no input, its output and errors read), failing
/// the test naming the command if it has not ended after at least `at_least`: a program that
/// hangs (a platform framework stalled) is killed instead of holding the test run.
pub fn output_within(command: &mut Command, at_least: Duration) -> Output {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .unwrap_or_else(|e| panic!("{command:?} did not start: {e}"));
    // Read as it is written, so a program with much to say never blocks on a full pipe.
    let stdout = read_all(child.stdout.take());
    let stderr = read_all(child.stderr.take());
    let status = end_within(command, &mut child, at_least);
    Output {
        status,
        stdout: stdout.join().expect("the output reader"),
        stderr: stderr.join().expect("the error reader"),
    }
}

/// Runs `command` as [`Command::status`] does, failing the test naming the command if it has
/// not ended after at least `at_least` (it is killed).
pub fn status_within(command: &mut Command, at_least: Duration) -> ExitStatus {
    let mut child = command
        .spawn()
        .unwrap_or_else(|e| panic!("{command:?} did not start: {e}"));
    end_within(command, &mut child, at_least)
}

/// Everything `pipe` gives until it ends, read on a thread of its own.
fn read_all(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    })
}

fn end_within(command: &Command, child: &mut Child, at_least: Duration) -> ExitStatus {
    let mut status = None;
    spin_for(at_least, || {
        status = child.try_wait().expect("the program's state");
        status.is_some()
    });
    status.unwrap_or_else(|| {
        let _ = child.kill();
        let _ = child.wait();
        panic!("hang: {command:?} did not end within {at_least:?}");
    })
}
