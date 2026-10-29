// SPDX-License-Identifier: GPL-3.0-or-later

//! Listing the autosave files in `Recovery/` (NFR-05), for a show whose file cannot be loaded
//! and so has no workspace id to give `check_recovery`. Read-only: nothing is moved or deleted.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use openlab_core::{WorkspaceId, load_show, parse_recovery_file_name, parse_store_name};

use super::recovery::STALE_SUFFIX;
use super::{AppDirs, modified_ms, read_text};

/// A sensible `max_entries` for [`scan_recovery`].
pub const MAX_RECOVERY_ENTRIES: usize = 500;
/// Directory entries looked at before the scan gives up, so a folder full of junk cannot hang
/// it.
const MAX_VISITED: usize = 5_000;
/// Files larger than this are listed without a name instead of being parsed, so a scan of many
/// big autosaves stays quick. (Real shows are far smaller; the limit for loading is larger.)
pub const MAX_NAME_READ_BYTES: usize = 4 * 1024 * 1024;

/// A recovery file found by [`scan_recovery`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryEntry {
    /// From the file name, so it is known even when the file itself is damaged.
    pub workspace_id: WorkspaceId,
    /// Hash of the show path the file is bound to; `None` for a never-saved workspace.
    pub path_hash: Option<String>,
    /// The workspace's name, if the file could be read and holds this workspace.
    pub show_name: Option<String>,
    /// Modification time, milliseconds since the Unix epoch, if known. For a stale file this
    /// is the time in its name (the autosave's own modification time), else the file's.
    pub modified_ms: Option<i64>,
    /// A file moved aside as not newer than the show file when it was opened. It may still be
    /// the only copy of newer work (file times are unreliable), so it is listed.
    pub stale: bool,
    pub path: PathBuf,
}

#[derive(Debug, Default)]
pub struct RecoveryScan {
    /// Newest first (unknown times last).
    pub entries: Vec<RecoveryEntry>,
    /// Entries that were skipped or only partly read, and why.
    pub warnings: Vec<String>,
    /// The scan stopped at a bound; there may be more files.
    pub truncated: bool,
}

/// Lists the autosave files in `Recovery/`. `Recovery/` has no subfolders, so only the folder
/// itself is read. At most `max_entries` files are returned, chosen newest first among the
/// first 5000 directory entries (a folder with more is `truncated` and which of the excess are
/// seen depends on the file system). A file that cannot be read, or is over
/// [`MAX_NAME_READ_BYTES`], is still listed, without a name. Reading names parses files, so
/// call this off the control thread.
pub fn scan_recovery(dirs: &AppDirs, max_entries: usize) -> RecoveryScan {
    scan_with(dirs, max_entries, MAX_VISITED)
}

fn scan_with(dirs: &AppDirs, max_entries: usize, max_visited: usize) -> RecoveryScan {
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
        if found.len() >= max_visited {
            scan.truncated = true;
            break;
        }
        match item {
            Ok(entry) => {
                let mtime = modified_ms(&entry.path());
                found.push((mtime, entry));
            }
            Err(e) => scan.warnings.push(format!("unreadable entry: {e}")),
        }
    }
    // Newest first, so a bound cuts off the oldest; names break ties deterministically.
    found.sort_by_key(|(mtime, e)| (std::cmp::Reverse(*mtime), e.file_name()));
    for (mtime, entry) in found {
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
        // Files this module set aside as unusable, and write_atomic's leftovers, are expected.
        if is_corrupt_name(&name) || name.ends_with(".tmp") {
            continue;
        }
        let (parsed, stale_ms) = match parse_name(&name) {
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
            modified_ms: stale_ms.or(mtime),
            stale: name.ends_with(STALE_SUFFIX),
            path,
        });
    }
    scan.entries
        .sort_by_key(|e| std::cmp::Reverse(e.modified_ms));
    scan
}

/// `.corrupt` or `.corrupt.<n>` (see `unique_sibling`), not any name that merely contains it.
fn is_corrupt_name(name: &str) -> bool {
    let base = match name.rsplit_once('.') {
        Some((base, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => base,
        _ => name,
    };
    base.ends_with(".corrupt")
}

/// Plain `<stem>.openlab`, or `<stem>.<ms>.stale.openlab` (the time is returned when it parses).
fn parse_name(
    name: &str,
) -> Result<(openlab_core::StoreName, Option<i64>), openlab_core::StoreNameError> {
    let Some(rest) = name.strip_suffix(STALE_SUFFIX) else {
        return parse_recovery_file_name(name).map(|p| (p, None));
    };
    let (stem, ms) = rest
        .rsplit_once('.')
        .ok_or(openlab_core::StoreNameError::NotRecoveryFile)?;
    Ok((parse_store_name(stem)?, ms.parse().ok()))
}

fn read_name(path: &Path, expected: WorkspaceId) -> Result<String, String> {
    let text = read_text(path, MAX_NAME_READ_BYTES).map_err(|e| e.to_string())?;
    let loaded = load_show(&text).map_err(|e| e.to_string())?;
    if loaded.workspace.id == expected {
        Ok(loaded.workspace.name)
    } else {
        Err("it holds a different workspace".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_names_are_matched_exactly() {
        assert!(is_corrupt_name("a.openlab.corrupt"));
        assert!(is_corrupt_name("a.openlab.corrupt.3"));
        assert!(!is_corrupt_name("my.corrupt.notes.openlab"));
        assert!(!is_corrupt_name("a.openlab"));
        assert!(!is_corrupt_name("a.corruptx"));
    }

    #[test]
    fn visited_bound_stops_the_scan() {
        let root = tempfile::tempdir().unwrap();
        let dirs = AppDirs::new(root.path());
        std::fs::create_dir_all(dirs.recovery_dir()).unwrap();
        for n in 0..20 {
            std::fs::write(dirs.recovery_dir().join(format!("junk{n}.txt")), "x").unwrap();
        }
        let scan = scan_with(&dirs, 100, 7);
        assert!(scan.truncated);
        assert_eq!(scan.warnings.len(), 7);
    }
}
