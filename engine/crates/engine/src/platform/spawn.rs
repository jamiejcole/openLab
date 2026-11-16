// SPDX-License-Identifier: GPL-3.0-or-later

//! Starting a child process whose pipes no other child of the engine inherits.
//!
//! Where the system cannot make a pipe that is closed on exec in one step (macOS), the standard
//! library makes it and marks it in two. A child started by another thread in between inherits
//! the pipe's ends. A long-lived child (the stage host) holding the write end of a probe
//! process's output keeps that output from ever ending for its reader. One holding the write
//! end of the stage host's input keeps the stage host alive after the engine. Every process
//! the engine starts, with pipes of its own or not, is therefore started here, one at a time.

use std::process::{Child, Command};
use std::sync::Mutex;

static SPAWNING: Mutex<()> = Mutex::new(());

/// Starts `command`. Its pipes are made and the child started with no other [`spawn`] in
/// between.
pub fn spawn(command: &mut Command) -> std::io::Result<Child> {
    let _one_at_a_time = crate::lock(&SPAWNING);
    command.spawn()
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::io::Read;
    use std::process::Stdio;

    use super::*;

    /// How many files a child started here has open: it lists them itself.
    fn files_open_in_a_child() -> usize {
        let mut command = Command::new("/bin/ls");
        command
            .arg("/dev/fd")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = spawn(&mut command).unwrap();
        let mut listed = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut listed)
            .unwrap();
        child.wait().unwrap();
        listed.lines().count()
    }

    /// Children started from several threads at once, each with a pipe, have only their own
    /// files open, as one started alone has: none inherits another's pipe. (Started without
    /// the lock, a few in a thousand inherit one or two.)
    #[test]
    fn children_started_at_once_never_inherit_each_others_pipes() {
        let _serial = crate::diag::run_marker::tests::serialize_spawn_and_lock_release();
        let alone = files_open_in_a_child();
        let threads: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| (0..100).map(|_| files_open_in_a_child()).max().unwrap_or(0))
            })
            .collect();
        for t in threads {
            assert_eq!(t.join().unwrap(), alone, "a child inherited a pipe");
        }
    }
}
