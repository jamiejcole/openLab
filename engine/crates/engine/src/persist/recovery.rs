// SPDX-License-Identifier: GPL-3.0-or-later

//! Autosave recovery files (NFR-05).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use openlab_core::{
    LoadedShow, MAX_SHOW_FILE_BYTES, RecoveryDecision, Workspace, WorkspaceId, decide_recovery,
    load_show, save_show,
};

use super::{AppDirs, PersistError, modified_ms, read_text, write_atomic};

/// Writes the autosave file for `workspace`. Never touches the show file. Like a manual save
/// this serialises and validates, so call it from the writer thread.
pub fn write_recovery(dirs: &AppDirs, workspace: &Workspace) -> Result<PathBuf, PersistError> {
    let text = save_show(workspace)?;
    let path = dirs.recovery_path(workspace.id);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(PersistError::io("create", dir))?;
    }
    write_atomic(&path, text.as_bytes())?;
    Ok(path)
}

/// Removes the recovery file after a save or a clean close. Missing is fine.
pub fn clear_recovery(dirs: &AppDirs, workspace: WorkspaceId) -> Result<(), PersistError> {
    let path = dirs.recovery_path(workspace);
    match fs::remove_file(&path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(PersistError::io("remove", &path)(e)),
        _ => Ok(()),
    }
}

/// A recovery file that holds newer work than the show file.
#[derive(Debug)]
pub struct RecoveryOffer {
    pub path: PathBuf,
    /// Modification times, milliseconds since the Unix epoch.
    pub recovery_modified_ms: i64,
    pub show_modified_ms: Option<i64>,
    /// The recovered workspace, already parsed and validated.
    pub loaded: LoadedShow,
}

#[derive(Debug)]
pub enum RecoveryCheck {
    /// No recovery file.
    None,
    /// A recovery file older than (or as old as) the show file was found and removed.
    Stale,
    /// Offer to restore it (the prompt flow is the UI's, M4.4).
    Offer(Box<RecoveryOffer>),
    /// There is a recovery file but it cannot be used. It was moved aside as `.corrupt` (or
    /// left in place if that failed); `warning` says why.
    Unusable { path: PathBuf, warning: String },
}

/// Looks for a recovery file for `workspace` and decides, from file times alone, whether it is
/// newer than the show file at `show_path` (which may not exist). Never panics or fails: a bad
/// recovery file is reported as [`RecoveryCheck::Unusable`].
pub fn check_recovery(dirs: &AppDirs, workspace: WorkspaceId, show_path: &Path) -> RecoveryCheck {
    let path = dirs.recovery_path(workspace);
    let recovery_modified = match fs::metadata(&path) {
        Ok(meta) if meta.is_file() => modified_ms(&path),
        Ok(_) => {
            return RecoveryCheck::Unusable {
                path,
                warning: "recovery path is not a file".into(),
            };
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return RecoveryCheck::None,
        Err(e) => {
            return RecoveryCheck::Unusable {
                warning: format!("recovery file cannot be examined: {e}"),
                path,
            };
        }
    };
    let show_modified = modified_ms(show_path);
    // A recovery file whose time the platform cannot give is treated as new: restoring is the
    // safe side of the error.
    let decision = decide_recovery(show_modified, Some(recovery_modified.unwrap_or(i64::MAX)));
    match decision {
        RecoveryDecision::None => RecoveryCheck::None,
        RecoveryDecision::Stale => {
            let _ = fs::remove_file(&path);
            RecoveryCheck::Stale
        }
        RecoveryDecision::Offer => {
            match read_text(&path, MAX_SHOW_FILE_BYTES)
                .map_err(|e| e.to_string())
                .and_then(|text| load_show(&text).map_err(|e| e.to_string()))
            {
                Ok(loaded) if loaded.workspace.id == workspace => {
                    RecoveryCheck::Offer(Box::new(RecoveryOffer {
                        path,
                        recovery_modified_ms: recovery_modified.unwrap_or(i64::MAX),
                        show_modified_ms: show_modified,
                        loaded,
                    }))
                }
                Ok(_) => set_aside(
                    path,
                    "recovery file belongs to a different workspace".into(),
                ),
                Err(reason) => set_aside(path, format!("recovery file is unusable: {reason}")),
            }
        }
    }
}

fn set_aside(path: PathBuf, warning: String) -> RecoveryCheck {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".corrupt");
    // Best effort, so the same warning does not repeat on every open.
    let _ = fs::rename(&path, path.with_file_name(name));
    RecoveryCheck::Unusable { path, warning }
}
