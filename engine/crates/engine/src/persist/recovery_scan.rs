// SPDX-License-Identifier: GPL-3.0-or-later

//! Listing the autosave files in `Recovery/` (NFR-05), for a show whose file cannot be loaded
//! and so has no workspace id to give `check_recovery`. Read-only: nothing is moved or deleted.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use openlab_core::{MAX_SHOW_FILE_BYTES, WorkspaceId, load_show, parse_recovery_file_name};

use super::{AppDirs, modified_ms, read_text};

/// A sensible `max_entries` for [`scan_recovery`].
pub const MAX_RECOVERY_ENTRIES: usize = 500;
/// Directory entries looked at before the scan gives up, so a folder full of junk cannot hang
/// it.
const MAX_VISITED: usize = 5_000;

/// A recovery file found by [`scan_recovery`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryEntry {
    /// From the file name, so it is known even when the file itself is damaged.
    pub workspace_id: WorkspaceId,
    /// Hash of the show path the file is bound to; `None` for a never-saved workspace.
    pub path_hash: Option<String>,
    /// The workspace's name, if the file could be read and holds this workspace.
    pub show_name: Option<String>,
    /// Modification time, milliseconds since the Unix epoch, if the platform says.
    pub modified_ms: Option<i64>,
    pub path: PathBuf,
}

#[derive(Debug, Default)]
pub struct RecoveryScan {
    /// Newest first (unknown times last), then by file name.
    pub entries: Vec<RecoveryEntry>,
    /// Entries that were skipped or only partly read, and why.
    pub warnings: Vec<String>,
    /// The scan stopped at a bound; there may be more files.
    pub truncated: bool,
}

/// Lists the autosave files in `Recovery/`. `Recovery/` has no subfolders, so only the folder
/// itself is read; at most `max_entries` files are returned, and a file that cannot be read is
/// still listed, without a name. Reading names parses each file, so call this off the control
/// thread.
pub fn scan_recovery(dirs: &AppDirs, max_entries: usize) -> RecoveryScan {
    let dir = dirs.recovery_dir();
    let mut scan = RecoveryScan::default();
    let read = match fs::read_dir(&dir) {
        Ok(read) => read,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return scan,
        Err(e) => {
            scan.warnings
                .push(format!("cannot read {}: {e}", dir.display()));
            return scan;
        }
    };
    let mut found = Vec::new();
    for item in read {
        if found.len() >= MAX_VISITED {
            scan.truncated = true;
            break;
        }
        match item {
            Ok(entry) => found.push(entry),
            Err(e) => scan.warnings.push(format!("unreadable entry: {e}")),
        }
    }
    // A stable order, so which files a bound cuts off does not depend on the file system.
    found.sort_by_key(|e| e.file_name());
    for entry in found {
        if scan.entries.len() >= max_entries {
            scan.truncated = true;
            break;
        }
        let path = entry.path();
        let Ok(name) = entry.file_name().into_string() else {
            scan.warnings
                .push(format!("skipped {}: name is not UTF-8", path.display()));
            continue;
        };
        // Files this module moved aside itself are expected, not worth a warning.
        if name.ends_with(".stale.openlab") || name.contains(".corrupt") || name.ends_with(".tmp") {
            continue;
        }
        let parsed = match parse_recovery_file_name(&name) {
            Ok(parsed) => parsed,
            Err(e) => {
                scan.warnings.push(format!("skipped {name}: {e}"));
                continue;
            }
        };
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            scan.warnings
                .push(format!("skipped {name}: not a regular file"));
            continue;
        }
        let show_name = match read_name(&path, parsed.workspace_id) {
            Ok(show_name) => Some(show_name),
            Err(reason) => {
                scan.warnings
                    .push(format!("{name} is listed without a name: {reason}"));
                None
            }
        };
        scan.entries.push(RecoveryEntry {
            workspace_id: parsed.workspace_id,
            path_hash: parsed.path_hash,
            show_name,
            modified_ms: modified_ms(&path),
            path,
        });
    }
    // Stable sort: ties keep the name order from above.
    scan.entries
        .sort_by_key(|e| std::cmp::Reverse(e.modified_ms));
    scan
}

fn read_name(path: &Path, expected: WorkspaceId) -> Result<String, String> {
    let text = read_text(path, MAX_SHOW_FILE_BYTES).map_err(|e| e.to_string())?;
    let loaded = load_show(&text).map_err(|e| e.to_string())?;
    if loaded.workspace.id == expected {
        Ok(loaded.workspace.name)
    } else {
        Err("it holds a different workspace".into())
    }
}
