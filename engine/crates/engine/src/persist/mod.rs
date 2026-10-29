// SPDX-License-Identifier: GPL-3.0-or-later

//! Persistence I/O (ARCHITECTURE §5): atomic saves, rolling backups, autosave recovery files,
//! per-machine settings and media relinking. The decisions (names, rotation, which file is
//! newer, path conversion) are pure and live in `openlab-core`; this module does the file work.
//!
//! Every function takes its directories as arguments; nothing here finds the real user
//! directories except [`AppDirs::system`], so tests use temp dirs.

mod atomic;
mod backup;
mod files;
mod recovery;
mod recovery_scan;
mod relink;
mod save;
mod settings;
mod threads;
mod writer;

use std::io;
use std::path::{Path, PathBuf};

use openlab_core::{SaveError, WorkspaceId};

pub(crate) use atomic::write_engine_file;
pub use atomic::{Stage, write_atomic, write_atomic_with};
pub use backup::backup_existing;
pub(crate) use files::remove_if_present;
pub use files::{modified_ms, read_text, unique_sibling};
pub use recovery::{
    RecoveryCheck, RecoveryOffer, RecoveryReport, check_recovery, clear_recovery, write_recovery,
};
pub use recovery_scan::{
    MAX_NAME_READ_BYTES, MAX_RECOVERY_ENTRIES, RecoveryEntry, RecoveryScan, scan_recovery,
};
pub use relink::{
    FolderScan, FoundRelinks, ScanLimits, find_in_folder, media_path, missing_media, scan_folder,
    scan_folder_with,
};
pub use save::{SaveReport, save_workspace, save_workspace_with};
pub use settings::{
    SettingsWriter, load_machine_settings, load_machine_settings_checked, save_machine_settings,
};
pub use writer::{
    DROP_WAIT, JobKind, MAX_FORCED_TARGETS, MAX_PENDING_TARGETS, MAX_WRITERS, SaveWriter,
};

/// `<app-support>/openLab`, and where the logs and crash reports go.
#[derive(Clone, Debug)]
pub struct AppDirs {
    root: PathBuf,
    /// `<root>/Logs`, or `~/Library/Logs/openLab` on macOS ([`AppDirs::system`]).
    logs: PathBuf,
    /// `<root>/Crashes`, or `~/Library/Logs/openLab/Crashes` on macOS.
    crashes: PathBuf,
}

impl AppDirs {
    /// `app_support` is the platform's application-support folder (or a temp dir in tests).
    /// Logs and crash reports go under it (`Logs`, `Crashes`).
    pub fn new(app_support: impl AsRef<Path>) -> Self {
        let root = app_support.as_ref().join("openLab");
        Self {
            logs: root.join("Logs"),
            crashes: root.join("Crashes"),
            root,
        }
    }

    /// `~/Library/Application Support` on macOS, `%APPDATA%` on Windows. `None` if the
    /// platform does not say. On macOS the logs and crash reports go to
    /// `~/Library/Logs/openLab`, where Console.app and people look for them (M4.7c).
    pub fn system() -> Option<Self> {
        dirs::data_dir().map(|data| Self::layout(data, dirs::home_dir(), cfg!(target_os = "macos")))
    }

    /// [`AppDirs::system`]'s layout for a platform, from its folders.
    fn layout(app_support: PathBuf, home: Option<PathBuf>, macos: bool) -> Self {
        let mut dirs = Self::new(app_support);
        if macos && let Some(home) = home {
            let logs = home.join("Library").join("Logs").join("openLab");
            dirs.crashes = logs.join("Crashes");
            dirs.logs = logs;
        }
        dirs
    }

    /// `engine.json`: how a client finds the running engine (M4.9, ARCHITECTURE §6, §8).
    pub fn engine_file_path(&self) -> PathBuf {
        self.root.join("engine.json")
    }

    /// The single-instance lock a running engine holds (M4.9, `host::instance_lock`).
    pub fn engine_lock_path(&self) -> PathBuf {
        self.root.join("engine.lock")
    }

    pub fn settings_path(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    /// `Backups/<workspace-id>-<hash of show path>`: a duplicated show has the same workspace
    /// id, and its backups must not rotate the original's out.
    pub fn backups_dir(&self, workspace: WorkspaceId, show_path: &Path) -> PathBuf {
        self.root
            .join("Backups")
            .join(format!("{workspace}-{}", files::path_key(show_path)))
    }

    /// The show log files (`openlab.log`, rotated to `openlab.1.log`...).
    pub fn logs_dir(&self) -> PathBuf {
        self.logs.clone()
    }

    /// Crash reports, kept on this machine only: `Crashes` beside the log files' folder on
    /// macOS (`~/Library/Logs/openLab/Crashes`), `<root>/Crashes` elsewhere.
    pub fn crash_dir(&self) -> PathBuf {
        self.crashes.clone()
    }

    /// One "running" marker per live engine process (`diag::run_marker`); a leftover means a
    /// run ended without a clean shutdown.
    pub fn run_markers_dir(&self) -> PathBuf {
        self.root.join("Running")
    }

    pub fn recovery_dir(&self) -> PathBuf {
        self.root.join("Recovery")
    }

    /// `Recovery/<workspace-id>-<hash of show path>.openlab`, or `<workspace-id>.openlab` for a
    /// workspace that has never been saved. Binding the file to its show path stops a copy of
    /// the show from being offered another copy's unsaved work.
    pub fn recovery_path(&self, workspace: WorkspaceId, show_path: Option<&Path>) -> PathBuf {
        self.recovery_dir()
            .join(format!("{}.openlab", recovery_stem(workspace, show_path)))
    }
}

pub(crate) fn recovery_stem(workspace: WorkspaceId, show_path: Option<&Path>) -> String {
    match show_path {
        Some(path) => format!("{workspace}-{}", files::path_key(path)),
        None => workspace.to_string(),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    /// A queued save was replaced by a newer one for the same file before it ran.
    #[error("superseded by a newer save")]
    Superseded,
    #[error("could not {op} {}: {source}", path.display())]
    Io {
        op: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    #[error(transparent)]
    Serialize(#[from] SaveError),
    #[error("{} is not valid UTF-8 text", .0.display())]
    NotText(PathBuf),
    #[error("{} is {size} bytes; the limit is {limit}", path.display())]
    TooLarge {
        path: PathBuf,
        size: u64,
        limit: u64,
    },
    #[error("the save thread has stopped")]
    WriterGone,
    /// A write did not finish within the stall timeout (`SaveWriter::check_stalled`), or one
    /// of the same show still has not: its volume may have stopped answering.
    #[error(
        "a write of this show did not finish within {secs} s; its volume may have stopped answering"
    )]
    Stalled { secs: u64 },
    /// A save would write a show file another open workspace has (M4.15f): refused, nothing
    /// written.
    #[error("{} is open in another workspace", .0.display())]
    OpenElsewhere(PathBuf),
}

/// What names one file however it is spelled: the canonical path (symlinks, `/var` and
/// `/private/var`, `.` and `..` resolved) when the file, or else its folder, exists; the path
/// as given otherwise; then compared as media paths are. Touches the disk (it can block on a
/// stalled volume): call it on a worker or the writer, never on the control thread.
pub fn file_key(path: &Path) -> String {
    let real = std::fs::canonicalize(path)
        .ok()
        .or_else(|| {
            let folder = std::fs::canonicalize(path.parent()?).ok()?;
            Some(folder.join(path.file_name()?))
        })
        .unwrap_or_else(|| path.to_path_buf());
    lexical_key(&real)
}

/// [`file_key`] without the disk: the path as spelled, compared as media paths are (case,
/// Unicode form and `.`/`..` ignored).
pub fn lexical_key(path: &Path) -> String {
    use openlab_core::{PathStyle, comparison_key, to_portable};
    comparison_key(&to_portable(&path.to_string_lossy(), PathStyle::host()))
}

impl PersistError {
    pub(crate) fn io(op: &'static str, path: &Path) -> impl FnOnce(io::Error) -> Self {
        let path = path.to_path_buf();
        move |source| Self::Io { op, path, source }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_keeps_logs_and_crash_reports_under_library_logs() {
        let support = PathBuf::from("/Users/a/Library/Application Support");
        let home = PathBuf::from("/Users/a");
        let mac = AppDirs::layout(support.clone(), Some(home.clone()), true);
        assert_eq!(
            mac.logs_dir(),
            home.join("Library").join("Logs").join("openLab")
        );
        assert_eq!(
            mac.crash_dir(),
            home.join("Library")
                .join("Logs")
                .join("openLab")
                .join("Crashes")
        );
        // Everything else stays under Application Support.
        let root = support.join("openLab");
        assert_eq!(mac.settings_path(), root.join("settings.json"));
        assert_eq!(mac.recovery_dir(), root.join("Recovery"));
        assert_eq!(mac.run_markers_dir(), root.join("Running"));
    }

    #[test]
    fn other_platforms_and_tests_keep_everything_under_app_support() {
        let support = PathBuf::from("/data");
        let root = support.join("openLab");
        for dirs in [
            AppDirs::layout(support.clone(), Some(PathBuf::from("/home/a")), false),
            // macOS without a home folder: the app-support layout, not a relative path.
            AppDirs::layout(support.clone(), None, true),
            AppDirs::new(&support),
        ] {
            assert_eq!(dirs.logs_dir(), root.join("Logs"));
            assert_eq!(dirs.crash_dir(), root.join("Crashes"));
            assert_eq!(dirs.run_markers_dir(), root.join("Running"));
        }
    }
}
