// SPDX-License-Identifier: GPL-3.0-or-later

//! Crash-safe file replacement (NFR-04).

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use super::PersistError;

/// Points in [`write_atomic_with`] where a test can inject a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// The temp file is written but not yet synced.
    TmpWritten,
    /// The temp file is on disk; the rename has not happened.
    TmpSynced,
}

/// Replaces `path` with `bytes` so that a crash or error at any point leaves either the old
/// file or the new one, never a mixture: write `<name>.<pid>-<n>.tmp` beside it, fsync, rename over the
/// original, fsync the folder (where the platform allows).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), PersistError> {
    write_atomic_with(path, bytes, |_| Ok(()))
}

/// [`write_atomic`] with `checkpoint` called at each [`Stage`]; an `Err` from it aborts the
/// save as if that step had failed. The temp file is removed on failure.
pub fn write_atomic_with(
    path: &Path,
    bytes: &[u8],
    mut checkpoint: impl FnMut(Stage) -> io::Result<()>,
) -> Result<(), PersistError> {
    let Some(name) = path.file_name() else {
        return Err(PersistError::io("write", path)(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path has no file name",
        )));
    };
    // Unique per write and created with `create_new`: a second process saving the same show,
    // a user's own `.tmp` file or a planted symlink can never be written through or clobbered.
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut tmp_name = name.to_os_string();
    tmp_name.push(format!(
        ".{}-{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = path.with_file_name(tmp_name);

    let staged = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(bytes)?;
        checkpoint(Stage::TmpWritten)?;
        file.sync_all()?;
        checkpoint(Stage::TmpSynced)?;
        Ok::<(), io::Error>(())
    })();
    if let Err(source) = staged {
        let _ = fs::remove_file(&tmp);
        return Err(PersistError::io("write", &tmp)(source));
    }
    if let Err(source) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(PersistError::io("replace", path)(source));
    }
    sync_parent(path);
    Ok(())
}

/// Makes the rename itself durable. Best effort: the data is already safe, and not every
/// platform or file system lets a folder be opened and synced (Windows does not).
fn sync_parent(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}
