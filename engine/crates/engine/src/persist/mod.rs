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
mod writer;

use std::io;
use std::path::{Path, PathBuf};

use openlab_core::{SaveError, WorkspaceId};

pub use atomic::{Stage, write_atomic, write_atomic_with};
pub use backup::backup_existing;
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
pub use writer::{JobKind, MAX_FORCED_TARGETS, MAX_PENDING_TARGETS, SaveWriter};

/// `<app-support>/openLab`.
#[derive(Clone, Debug)]
pub struct AppDirs {
    root: PathBuf,
}

impl AppDirs {
    /// `app_support` is the platform's application-support folder (or a temp dir in tests).
    pub fn new(app_support: impl AsRef<Path>) -> Self {
        Self {
            root: app_support.as_ref().join("openLab"),
        }
    }

    /// `~/Library/Application Support` on macOS, `%APPDATA%` on Windows. `None` if the
    /// platform does not say.
    pub fn system() -> Option<Self> {
        dirs::data_dir().map(Self::new)
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
        self.root.join("Logs")
    }

    /// Crash reports, kept on this machine only.
    pub fn crash_dir(&self) -> PathBuf {
        self.root.join("Crashes")
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
}

impl PersistError {
    pub(crate) fn io(op: &'static str, path: &Path) -> impl FnOnce(io::Error) -> Self {
        let path = path.to_path_buf();
        move |source| Self::Io { op, path, source }
    }
}
