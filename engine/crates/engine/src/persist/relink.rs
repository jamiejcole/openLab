// SPDX-License-Identifier: GPL-3.0-or-later

//! Finding missing media and proposing relinks (WS-03). Applying a relink is a core command
//! (`relink_command`), so it is undoable like any other edit.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use openlab_core::{
    MediaRef, MissingMedia, PathStyle, RelinkPlan, Workspace, audio_media, file_name,
    file_name_key, plan_relinks, portable_ref, resolve,
};

/// Bounds on a folder scan, so a huge or deeply nested drive cannot hang the engine.
#[derive(Clone, Copy, Debug)]
pub struct ScanLimits {
    /// Folders below the scanned one that are entered; 0 scans only the folder itself.
    pub max_depth: usize,
    /// Files collected before the scan stops.
    pub max_files: usize,
    /// Folders read before the scan stops; a drive full of empty folders costs time without
    /// ever reaching `max_files`.
    pub max_dirs: usize,
    /// Wall-clock budget; a slow or hung network volume ends the scan instead of the show.
    pub max_duration: Duration,
}

impl Default for ScanLimits {
    fn default() -> Self {
        Self {
            max_depth: 16,
            max_files: 200_000,
            max_dirs: 50_000,
            max_duration: Duration::from_secs(20),
        }
    }
}

#[derive(Debug, Default)]
pub struct FolderScan {
    /// Files found, in a stable order (sorted by name within each folder, folders after the
    /// files beside them).
    pub files: Vec<PathBuf>,
    /// A limit was hit, so the list may be incomplete.
    pub truncated: bool,
    /// Entries that could not be read.
    pub unreadable: usize,
}

/// Lists the files under `root`. Symlinked folders are not entered, so a link back up the tree
/// cannot loop; symlinks to files count as files. Unreadable folders are skipped and counted.
///
/// Stops with `truncated` set when a limit in `limits` is reached, including the time budget.
pub fn scan_folder(root: &Path, limits: ScanLimits) -> FolderScan {
    let start = Instant::now();
    scan_folder_with(root, limits, &mut || start.elapsed())
}

/// [`scan_folder`] with the clock injected: `elapsed` returns the time since the scan began.
/// It is asked before each folder is read and every 1024 entries within one.
pub fn scan_folder_with(
    root: &Path,
    limits: ScanLimits,
    elapsed: &mut dyn FnMut() -> Duration,
) -> FolderScan {
    let mut scan = FolderScan::default();
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    let mut dirs_read = 0usize;
    while let Some((dir, depth)) = pending.pop() {
        if dirs_read >= limits.max_dirs || elapsed() >= limits.max_duration {
            scan.truncated = true;
            return scan;
        }
        dirs_read += 1;
        let Ok(read) = fs::read_dir(&dir) else {
            scan.unreadable += 1;
            continue;
        };
        let mut entries: Vec<_> = read.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());
        let mut subdirs = Vec::new();
        for (i, entry) in entries.into_iter().enumerate() {
            if i % 1024 == 1023 && elapsed() >= limits.max_duration {
                scan.truncated = true;
                return scan;
            }
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                scan.unreadable += 1;
                continue;
            };
            let is_file = if kind.is_symlink() {
                fs::metadata(&path).is_ok_and(|m| m.is_file())
            } else {
                kind.is_file()
            };
            if is_file {
                if scan.files.len() >= limits.max_files {
                    scan.truncated = true;
                    return scan;
                }
                scan.files.push(path);
            } else if kind.is_dir() {
                if depth < limits.max_depth {
                    subdirs.push((path, depth + 1));
                } else {
                    scan.truncated = true;
                }
            }
        }
        // Reverse so the stack pops them in name order.
        pending.extend(subdirs.into_iter().rev());
    }
    scan
}

/// The file a media reference points at, or `None` if it is relative and the workspace has
/// not been saved yet (so there is no folder to resolve against).
pub fn media_path(media: &MediaRef, show_dir: Option<&Path>) -> Option<PathBuf> {
    let text = media.as_str();
    if openlab_core::is_absolute(text) {
        return Some(PathBuf::from(resolve(text, "")));
    }
    let dir = openlab_core::to_portable(show_dir?.to_str()?, PathStyle::host());
    Some(PathBuf::from(resolve(text, &dir)))
}

/// Audio cues whose file does not exist on disk. A path that cannot be examined counts as
/// missing.
pub fn missing_media(workspace: &Workspace, show_dir: Option<&Path>) -> Vec<MissingMedia> {
    audio_media(workspace)
        .into_iter()
        .filter(|(_, media)| !media_path(media, show_dir).is_some_and(|p| p.exists()))
        .map(|(cue, media)| MissingMedia { cue, media })
        .collect()
}

#[derive(Debug)]
pub struct FoundRelinks {
    pub plan: RelinkPlan,
    /// The folder scan hit a limit; files beyond it were not considered.
    pub truncated: bool,
    /// Folders or files the scan could not read, plus candidates whose names are not UTF-8.
    pub skipped: usize,
}

/// Batch "find in folder": scans `folder` and matches every missing file in `workspace` by
/// file name. Proposals only; nothing is changed. Apply with `relink_command`.
pub fn find_in_folder(
    workspace: &Workspace,
    show_dir: Option<&Path>,
    folder: &Path,
    limits: ScanLimits,
) -> FoundRelinks {
    let missing = missing_media(workspace, show_dir);
    let wanted: BTreeSet<String> = missing
        .iter()
        .map(|m| file_name_key(m.media.as_str()))
        .collect();
    let scan = scan_folder(folder, limits);
    let style = PathStyle::host();
    let show_dir_text = show_dir.and_then(Path::to_str);
    let mut skipped = scan.unreadable;
    let mut candidates = Vec::new();
    for path in &scan.files {
        let Some(text) = path.to_str() else {
            skipped += 1;
            continue;
        };
        let portable = openlab_core::to_portable(text, style);
        if wanted.contains(&file_name_key(file_name(&portable))) {
            candidates.push(portable_ref(text, show_dir_text, style));
        }
    }
    FoundRelinks {
        plan: plan_relinks(&missing, &candidates),
        truncated: scan.truncated,
        skipped,
    }
}
