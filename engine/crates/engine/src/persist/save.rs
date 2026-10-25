// SPDX-License-Identifier: GPL-3.0-or-later

//! The manual-save pipeline: serialise, back up the previous version, replace atomically,
//! rotate backups, drop the recovery file.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use openlab_core::{Workspace, save_show};

use super::backup::{Rotation, backup_existing, rotate};
use super::recovery::clear_recovery;
use super::{AppDirs, PersistError, Stage, write_atomic_with};

#[derive(Debug, Default)]
pub struct SaveReport {
    /// The backup of the previous version, if one was made.
    pub backup: Option<PathBuf>,
    /// Backups rotated out.
    pub backups_deleted: usize,
    /// Things that went wrong without losing the save: a failed backup, a leftover recovery
    /// file that could not be removed.
    pub warnings: Vec<String>,
}

/// Saves `workspace` to `path`. On error the file at `path` is exactly as it was: the new
/// version is written beside it and renamed over it only when complete. `now_ms` names the
/// backup; `keep` is the number of backups to retain (0 = no backups).
///
/// Runs `save_show`, which re-validates by loading the text back and can take about a second
/// for a large show, so call it from the writer thread ([`super::SaveWriter`]), never from the
/// control thread.
pub fn save_workspace(
    dirs: &AppDirs,
    path: &Path,
    workspace: &Workspace,
    now_ms: i64,
    keep: u32,
) -> Result<SaveReport, PersistError> {
    save_workspace_with(dirs, path, workspace, now_ms, keep, |_| Ok(()))
}

/// [`save_workspace`] with a failure-injection hook for the atomic write (see [`Stage`]).
pub fn save_workspace_with(
    dirs: &AppDirs,
    path: &Path,
    workspace: &Workspace,
    now_ms: i64,
    keep: u32,
    checkpoint: impl FnMut(Stage) -> io::Result<()>,
) -> Result<SaveReport, PersistError> {
    // A workspace that would not load back writes nothing at all, not even a backup.
    let text = save_show(workspace)?;
    let mut report = SaveReport::default();

    match backup_existing(dirs, workspace.id, path, now_ms, keep) {
        Ok(backup) => report.backup = backup,
        // A failed backup must not stop the save: the operator's changes matter more.
        Err(e) => report.warnings.push(format!("backup failed: {e}")),
    }

    if let Err(e) = write_atomic_with(path, text.as_bytes(), checkpoint) {
        // The backup duplicates the file that is still in place; do not let failed saves
        // push real older backups out of the rotation.
        if let Some(backup) = report.backup.take() {
            let _ = fs::remove_file(backup);
        }
        return Err(e);
    }

    let rotated = if keep > 0 {
        rotate(dirs, workspace.id, keep)
    } else {
        Rotation::default()
    };
    report.backups_deleted = rotated.deleted;
    report.warnings.extend(
        rotated
            .delete_failures
            .into_iter()
            .map(|f| format!("could not remove old backup {f}")),
    );
    // The saved file now holds everything the recovery file did.
    if let Err(e) = clear_recovery(dirs, workspace.id) {
        report
            .warnings
            .push(format!("could not remove recovery file: {e}"));
    }
    Ok(report)
}
