// SPDX-License-Identifier: GPL-3.0-or-later

//! Rolling timestamped backups (NFR-06).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use openlab_core::{WorkspaceId, backup_file_name, parse_backup_name, select_backups_to_delete};

use super::atomic::split_tmp_name;
use super::{AppDirs, PersistError, write_atomic};

#[derive(Debug, Default)]
pub(crate) struct Rotation {
    /// Old backups removed to stay within the limit.
    pub deleted: usize,
    /// Old backups that could not be removed; they are retried on the next save.
    pub delete_failures: Vec<String>,
}

/// Copies the current contents of `show_path` into the workspace's backup folder, named for
/// `now_ms`. Does nothing if `keep` is 0 (backups off) or the show file does not exist yet. A
/// show file that cannot be read is an error. Rotation is separate: see [`rotate`].
pub fn backup_existing(
    dirs: &AppDirs,
    workspace: WorkspaceId,
    show_path: &Path,
    now_ms: i64,
    keep: u32,
) -> Result<Option<PathBuf>, PersistError> {
    if keep == 0 {
        return Ok(None);
    }
    let bytes = match fs::read(show_path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(PersistError::io("read", show_path)(e)),
    };
    let dir = dirs.backups_dir(workspace, show_path);
    fs::create_dir_all(&dir).map_err(PersistError::io("create", &dir))?;
    // Two saves in the same millisecond must not overwrite each other's backup.
    let mut ms = now_ms;
    let mut path = dir.join(backup_file_name(ms));
    for _ in 0..1000 {
        if !path.exists() {
            break;
        }
        ms = ms.saturating_add(1);
        path = dir.join(backup_file_name(ms));
    }
    write_atomic(&path, &bytes)?;
    Ok(Some(path))
}

/// Deletes all but the `keep` newest backups of `workspace`, and the temp files a crash left
/// behind. "Newest" is the time in the file name, never the file's modification time, which a
/// clock step backwards would turn upside down. A temp file goes when its backup name is not
/// newer than the newest finished backup: one for a later time may be another instance's write
/// in progress. Files in the folder that are neither are left alone. Never fails: what could not
/// be removed is reported.
pub(crate) fn rotate(
    dirs: &AppDirs,
    workspace: WorkspaceId,
    show_path: &Path,
    keep: u32,
) -> Rotation {
    let dir = dirs.backups_dir(workspace, show_path);
    let mut outcome = Rotation::default();
    let Ok(entries) = fs::read_dir(&dir) else {
        return outcome;
    };
    // Non-UTF-8 names cannot be ours.
    let names: Vec<String> = entries
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .collect();
    let newest = names.iter().filter_map(|n| parse_backup_name(n)).max();
    let mut doomed = select_backups_to_delete(&names, keep as usize);
    if let Some(newest) = newest {
        doomed.extend(
            names
                .iter()
                .filter(|name| {
                    split_tmp_name(name)
                        .and_then(|(base, _)| parse_backup_name(base))
                        .is_some_and(|ms| ms <= newest)
                })
                .cloned(),
        );
    }
    for name in doomed {
        match fs::remove_file(dir.join(&name)) {
            Ok(()) => outcome.deleted += 1,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => outcome.delete_failures.push(format!("{name}: {e}")),
        }
    }
    outcome
}
