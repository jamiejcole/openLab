// SPDX-License-Identifier: GPL-3.0-or-later

//! One engine per user (M4.9): `<app-support>/openLab/engine.lock`, locked exclusively as the
//! first act of a start and held, with the open file, for the life of the process. A second
//! engine that finds it held exits before it opens the log, a run marker, the audio device,
//! the UI port or `engine.json`, so it touches nothing of the running one's. The lock goes with
//! the process (a crash too), so a stale file never refuses a start.
//!
//! The `engine.json` check (`engine_file::live_engine`) stays behind it for a filesystem
//! without locks; there the start goes ahead without this one, as the run marker does.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Seek as _, Write as _};
use std::path::Path;

/// The pid the lock file holds is a few bytes; more is not ours and is not read.
const MAX_BYTES: u64 = 64;

/// The held lock. Dropping it (the end of the process) unlocks it; the file itself stays.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
}

/// Why [`acquire`] did not take the lock.
#[derive(Debug)]
pub enum Refused {
    /// Another process holds it; its pid, when the file says (not readable on Windows, where
    /// the lock also bars reading).
    Held { pid: Option<u32> },
    /// The file could not be opened or locked here (unwritable folder, no lock support, not a
    /// regular file): the start goes ahead without it.
    Unavailable(io::Error),
}

/// Takes the lock at `path`, creating the file (and its folder) if needed, and writes this
/// process's pid into it.
pub fn acquire(path: &Path) -> Result<InstanceLock, Refused> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(Refused::Unavailable)?;
    }
    // A FIFO or device named like the lock must not hang the start (opening one can block).
    if let Ok(meta) = fs::symlink_metadata(path)
        && !meta.is_file()
    {
        return Err(Refused::Unavailable(io::Error::other(
            "engine.lock is not a regular file",
        )));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(Refused::Unavailable)?;
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(Refused::Held {
                pid: read_pid(&mut file),
            });
        }
        Err(std::fs::TryLockError::Error(e)) => return Err(Refused::Unavailable(e)),
    }
    // Only for the message a second start prints: a failure here does not matter.
    let _ = file
        .set_len(0)
        .and_then(|()| file.rewind())
        .and_then(|()| file.write_all(std::process::id().to_string().as_bytes()));
    Ok(InstanceLock { _file: file })
}

fn read_pid(file: &mut File) -> Option<u32> {
    let mut text = String::new();
    file.take(MAX_BYTES).read_to_string(&mut text).ok()?;
    text.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_lock_is_refused_with_the_holders_pid_until_the_first_goes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("openLab").join("engine.lock");
        // The lock belongs to the open file, so a second open in this process stands for a
        // second engine.
        let first = acquire(&path).unwrap();
        match acquire(&path) {
            Err(Refused::Held { pid }) => {
                if cfg!(unix) {
                    assert_eq!(pid, Some(std::process::id()));
                }
            }
            other => panic!("{other:?}"),
        }
        drop(first);
        assert!(acquire(&path).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_named_like_the_lock_does_not_hang_the_start() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("engine.lock");
        let made = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap();
        assert!(made.success());
        assert!(matches!(acquire(&path), Err(Refused::Unavailable(_))));
    }
}
