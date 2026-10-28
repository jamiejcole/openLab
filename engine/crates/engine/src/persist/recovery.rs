// SPDX-License-Identifier: GPL-3.0-or-later

//! Autosave recovery files (NFR-05).
//!
//! A recovery file is never deleted because of a timestamp comparison (file times are unreliable
//! on network and removable volumes). Files that look stale are moved aside as
//! `<stem>.<modified-ms>.stale.openlab`, the newest [`KEEP_STALE`] are kept, and they are
//! deleted only after a successful save of that workspace.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use openlab_core::{
    LoadedShow, MAX_SHOW_FILE_BYTES, RecoveryDecision, Workspace, WorkspaceId, decide_recovery,
    load_show, save_show,
};

use super::atomic::rename_no_replace;
use super::{
    AppDirs, PersistError, modified_ms, read_text, recovery_stem, unique_sibling, write_atomic,
};

const KEEP_STALE: usize = 5;
const STALE_SUFFIX: &str = ".stale.openlab";

/// Writes the autosave file for `workspace`, bound to `show_path` (`None` before the first
/// save). Never touches the show file. Like a manual save this serialises and validates, so
/// call it from the writer thread.
pub fn write_recovery(
    dirs: &AppDirs,
    workspace: &Workspace,
    show_path: Option<&Path>,
) -> Result<PathBuf, PersistError> {
    let text = save_show(workspace)?;
    let path = dirs.recovery_path(workspace.id, show_path);
    let dir = dirs.recovery_dir();
    fs::create_dir_all(&dir).map_err(PersistError::io("create", &dir))?;
    write_atomic(&path, text.as_bytes())?;
    Ok(path)
}

/// Removes the recovery file for this workspace and show path, and the stale files moved aside
/// from it. Call after a successful save or a clean close. Missing is fine.
pub fn clear_recovery(
    dirs: &AppDirs,
    workspace: WorkspaceId,
    show_path: Option<&Path>,
) -> Result<(), PersistError> {
    let active = dirs.recovery_path(workspace, show_path);
    let mut result = remove_if_present(&active);
    for stale in stale_files(dirs, &recovery_stem(workspace, show_path)) {
        let removed = remove_if_present(&stale);
        if result.is_ok() {
            result = removed;
        }
    }
    result
}

fn remove_if_present(path: &Path) -> Result<(), PersistError> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(PersistError::io("remove", path)(e)),
        _ => Ok(()),
    }
}

/// Moved-aside files of `stem`, oldest first.
fn stale_files(dirs: &AppDirs, stem: &str) -> Vec<PathBuf> {
    let Ok(read) = fs::read_dir(dirs.recovery_dir()) else {
        return Vec::new();
    };
    let prefix = format!("{stem}.");
    let mut found: Vec<PathBuf> = read
        .filter_map(|e| {
            let e = e.ok()?;
            let name = e.file_name().into_string().ok()?;
            (name.starts_with(&prefix) && name.ends_with(STALE_SUFFIX)).then(|| e.path())
        })
        .collect();
    found.sort();
    found
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
    /// No recovery file for this show.
    None,
    /// The recovery file is not newer than the show file. It was moved aside (not deleted) to
    /// `moved_to`, or left in place if the move failed (`None`).
    Stale { moved_to: Option<PathBuf> },
    /// Offer to restore it (the prompt flow is the UI's, M4.4).
    Offer(Box<RecoveryOffer>),
    /// There is a recovery file but it cannot be used. It was moved aside as `.corrupt` (or
    /// left in place if that failed); `warning` says why.
    Unusable { path: PathBuf, warning: String },
}

#[derive(Debug)]
pub struct RecoveryReport {
    pub check: RecoveryCheck,
    /// Recovery files for the same workspace id but another show path (a duplicated show, or
    /// work from before the first save). Not offered and not touched; M4.4 can surface them.
    pub other_copies: Vec<PathBuf>,
}

/// Looks for a recovery file for `workspace` opened from `show_path` (which may not exist) and
/// decides, from file times alone, whether it is newer than the show file. Never panics or
/// fails: a bad recovery file is reported as [`RecoveryCheck::Unusable`].
pub fn check_recovery(dirs: &AppDirs, workspace: WorkspaceId, show_path: &Path) -> RecoveryReport {
    RecoveryReport {
        check: check_own(dirs, workspace, show_path),
        other_copies: other_copies(dirs, workspace, show_path),
    }
}

fn other_copies(dirs: &AppDirs, workspace: WorkspaceId, show_path: &Path) -> Vec<PathBuf> {
    let Ok(read) = fs::read_dir(dirs.recovery_dir()) else {
        return Vec::new();
    };
    let ours = dirs.recovery_path(workspace, Some(show_path));
    let id = workspace.to_string();
    let mut found: Vec<PathBuf> = read
        .filter_map(|e| {
            let e = e.ok()?;
            let name = e.file_name().into_string().ok()?;
            let stem = name.strip_suffix(".openlab")?;
            let ours_or_related = stem == id || stem.strip_prefix(&id)?.starts_with('-');
            // `<stem>.<ms>.stale` and `.corrupt` names have a dot in the stem.
            (ours_or_related && !stem.contains('.') && e.path() != ours).then(|| e.path())
        })
        .collect();
    found.sort();
    found
}

fn check_own(dirs: &AppDirs, workspace: WorkspaceId, show_path: &Path) -> RecoveryCheck {
    let path = dirs.recovery_path(workspace, Some(show_path));
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
    let recovery_ms = recovery_modified.unwrap_or(i64::MAX);
    match decide_recovery(show_modified, Some(recovery_ms)) {
        RecoveryDecision::None => RecoveryCheck::None,
        RecoveryDecision::Stale => RecoveryCheck::Stale {
            moved_to: move_stale(dirs, workspace, show_path, &path, recovery_modified),
        },
        RecoveryDecision::Offer => match read_text(&path, MAX_SHOW_FILE_BYTES)
            .map_err(|e| e.to_string())
            .and_then(|text| load_show(&text).map_err(|e| e.to_string()))
        {
            Ok(loaded) if loaded.workspace.id == workspace => {
                RecoveryCheck::Offer(Box::new(RecoveryOffer {
                    path,
                    recovery_modified_ms: recovery_ms,
                    show_modified_ms: show_modified,
                    loaded,
                }))
            }
            Ok(_) => set_aside(
                path,
                "recovery file belongs to a different workspace".into(),
            ),
            Err(reason) => set_aside(path, format!("recovery file is unusable: {reason}")),
        },
    }
}

fn move_stale(
    dirs: &AppDirs,
    workspace: WorkspaceId,
    show_path: &Path,
    path: &Path,
    modified: Option<i64>,
) -> Option<PathBuf> {
    let stem = recovery_stem(workspace, Some(show_path));
    let ms = modified.unwrap_or(0).max(0);
    // Two stale files with the same time keep distinct names, and one never replaces another:
    // with every name taken the file stays where it is (offered as stale again next time).
    let target = (0..100)
        .map(|n| {
            dirs.recovery_dir()
                .join(format!("{stem}.{:015}{STALE_SUFFIX}", ms.saturating_add(n)))
        })
        .find_map(|target| match rename_no_replace(path, &target) {
            Ok(()) => Some(Some(target)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => None,
            Err(_) => Some(None),
        })??;
    // Oldest first by name (zero-padded time); keep the newest few.
    let all = stale_files(dirs, &stem);
    for old in all.iter().take(all.len().saturating_sub(KEEP_STALE)) {
        let _ = fs::remove_file(old);
    }
    Some(target)
}

fn set_aside(path: PathBuf, warning: String) -> RecoveryCheck {
    // Best effort, so the same warning does not repeat on every open.
    let _ = fs::rename(&path, unique_sibling(&path, "corrupt"));
    RecoveryCheck::Unusable { path, warning }
}
