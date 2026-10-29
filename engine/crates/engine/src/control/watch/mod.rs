// SPDX-License-Identifier: GPL-3.0-or-later

//! The media watch (M2.25, ARCHITECTURE §3.1; docs/control.md "Media watch", docs/persistence.md
//! "Media watch and follow"): the `openlab-watch` thread stats every media file the catalog
//! knows, metadata only, and tells the control thread which went missing, came back, changed or
//! became unreadable. No file watcher: a stat per file per pass works the same on every OS and
//! file system, network volumes included.
//!
//! - [`WatchCore`] holds the per-file state and the pass rules; it is pure apart from the
//!   [`WatchFs`] it is handed, so the rules are tested without threads or a disk.
//! - A pass covers every file once in `max(period, n ms)`, in chunks of [`CHUNK`] spread evenly
//!   over it, and ends in one batch, sent only when something changed: an idle show costs the
//!   control thread nothing.
//! - [`MediaWatch`] is the control side: it turns the catalog's key changes into jobs, drops
//!   batches of an older generation (the catalog was reset since), and runs the watchdog. The
//!   watcher publishes what it is statting; one stat longer than the stall limit (a volume that
//!   stopped answering) makes control abandon the watcher and start a fresh one that skips that
//!   volume until the abandoned one's stat returns. The state lives in a [`WatchCore`] shared by
//!   the watchers (locked only between stats, never during one), so a fresh watcher carries on
//!   where the stuck one stopped and nothing it found is lost.
//!
//! Following a moved file (`locate`, M2.29b) is not done yet: a file that is gone is `Gone`.

use crate::lock;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, UNIX_EPOCH};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use openlab_core::protocol::{LogLevel, WireWarning};

use super::docs::EpochClock;
use super::inbox::Inbox;
use super::media::KeyChange;
use super::notice;

/// The shortest time a pass over every file takes (docs/persistence.md: a file sent to the
/// Trash shows broken within a few seconds).
pub const MIN_PERIOD: Duration = Duration::from_secs(3);
/// Files statted between two pauses of a pass.
pub const CHUNK: usize = 64;
/// One stat taking longer than this (on the injected clock) means its volume does not answer.
pub const WATCH_STALL: Duration = Duration::from_secs(10);
/// Watchers abandoned in a stalled stat, at most; past it the watch stops while one is stuck.
pub const MAX_ABANDONED_WATCHERS: usize = 4;

/// What a stat tells about a file: enough to tell the same file, unchanged, from another one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub modified_ms: i64,
}

/// One stat of a media path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    pub fp: Fingerprint,
    /// A regular file (a folder or a device at the path is not media).
    pub regular: bool,
    /// The path itself is a symbolic link (it is never followed to a new place, M2.29b).
    pub symlink: bool,
}

/// How the watch reads the file system: [`RealFs`] in the engine, [`FakeFs`] in tests.
pub trait WatchFs: Send + Sync {
    /// The file at `path`, following a symbolic link. `NotFound` (or `NotADirectory`): there is
    /// no file; any other error: it cannot be read.
    fn stat(&self, path: &Path) -> io::Result<Stat>;

    /// Where the file last seen as `fp` is now, if the platform can tell (M2.29b). `None` here.
    fn locate(&self, _fp: &Fingerprint) -> Option<PathBuf> {
        None
    }
}

/// The real file system.
#[derive(Clone, Copy, Debug, Default)]
pub struct RealFs;

impl WatchFs for RealFs {
    fn stat(&self, path: &Path) -> io::Result<Stat> {
        // One call for an ordinary file; a link is followed with a second.
        let link = std::fs::symlink_metadata(path)?;
        let symlink = link.file_type().is_symlink();
        let meta = if symlink {
            std::fs::metadata(path)?
        } else {
            link
        };
        let modified_ms = meta
            .modified()
            .ok()
            .map_or(0, |t| match t.duration_since(UNIX_EPOCH) {
                Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
                Err(e) => i64::try_from(e.duration().as_millis()).map_or(i64::MIN, |ms| -ms),
            });
        let (dev, ino) = identity(&meta);
        Ok(Stat {
            fp: Fingerprint {
                dev,
                ino,
                size: meta.len(),
                modified_ms,
            },
            regular: meta.is_file(),
            symlink,
        })
    }
}

#[cfg(unix)]
fn identity(meta: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

/// std has no stable file id on Windows: size and modified time tell changes there (M2.29f).
#[cfg(not(unix))]
fn identity(_meta: &std::fs::Metadata) -> (u64, u64) {
    (0, 0)
}

/// The volume a path is on, from its text alone (a stat could hang on exactly that volume):
/// `/Volumes/<name>` (macOS), `/media/<user>/<name>`, `/run/media/<user>/<name>`,
/// `/mnt/<name>`, a Windows drive or share; anything else is the system volume `/`. The
/// watchdog skips a stalled volume by this name and names it in its notice.
pub fn bucket_of(path: &Path) -> String {
    let mut parts = path.components();
    let Some(first) = parts.next() else {
        return "/".to_owned();
    };
    if let Component::Prefix(p) = first {
        return p.as_os_str().to_string_lossy().into_owned();
    }
    let names: Vec<String> = parts
        .take(5)
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let take = match names.first().map(String::as_str) {
        Some("Volumes" | "mnt") => 2,
        Some("media") => 3,
        Some("run") if names.get(1).map(String::as_str) == Some("media") => 4,
        _ => 0,
    };
    // A file directly in the mount folder is not on a volume of its own.
    if take == 0 || names.len() <= take {
        return "/".to_owned();
    }
    let mut out = String::new();
    for n in names.iter().take(take) {
        out.push('/');
        out.push_str(n);
    }
    out
}

/// A job for the watcher, in the order control sent them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchJob {
    /// The catalog was reset: forget every file; batches carry `generation` from now on.
    Reset {
        generation: u64,
    },
    /// Watch `key` at `path`; `id` comes back in its events (a key watched again gets a new id,
    /// so control can tell an event of its earlier watch).
    Watch {
        key: String,
        path: PathBuf,
        id: u64,
    },
    Unwatch {
        key: String,
    },
    /// Leave the files on this volume alone (it stopped answering); they keep their state.
    Skip {
        bucket: String,
    },
    /// Watch this volume again. `trial`: it was skipped after an error; the result of its
    /// next stat is reported in the batch's `volumes`.
    Unskip {
        bucket: String,
        trial: bool,
    },
    /// The catalog's probe of `key` failed: `missing` (no file), the file is taken as gone, so
    /// a file that appears before the first stat is still reported `Back`; otherwise, while
    /// the path is unreadable, its return is reported even if the file is unchanged.
    ProbeFailed {
        key: String,
        id: u64,
        missing: bool,
    },
}

/// What a pass found out about one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchEvent {
    /// No file at the path (reported on the first pass that sees it gone).
    Gone { key: String, id: u64, trashed: bool },
    /// A file is at the path again, the same on two passes in a row.
    Back {
        key: String,
        id: u64,
        fp: Fingerprint,
    },
    /// Another file (or the file changed) at the path, the same on two passes in a row.
    Changed {
        key: String,
        id: u64,
        fp: Fingerprint,
    },
    /// The path cannot be examined (permissions, an I/O error).
    Unreadable { key: String, id: u64, error: String },
}

impl WatchEvent {
    pub fn key(&self) -> &str {
        match self {
            Self::Gone { key, .. }
            | Self::Back { key, .. }
            | Self::Changed { key, .. }
            | Self::Unreadable { key, .. } => key,
        }
    }

    pub fn id(&self) -> u64 {
        match self {
            Self::Gone { id, .. }
            | Self::Back { id, .. }
            | Self::Changed { id, .. }
            | Self::Unreadable { id, .. } => *id,
        }
    }
}

/// The events of one pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchBatch {
    pub generation: u64,
    pub events: Vec<WatchEvent>,
    /// Volumes on trial (`Unskip { trial }`) and whether their first stat answered (a file or
    /// no file) rather than failed.
    pub volumes: Vec<(String, bool)>,
}

/// What a watcher sends the control thread.
#[derive(Debug)]
pub enum WatchAnswer {
    Batch(WatchBatch),
    /// An abandoned watcher's stalled stat returned (the thread has ended): `healthy` if it
    /// found a file or no file, not an error.
    BucketAnswered {
        bucket: String,
        healthy: bool,
    },
}

mod media_watch;
mod watch_core;

pub use media_watch::*;
pub use watch_core::*;

/// Test support (feature `test-support`): not in the shipping engine.
#[cfg(any(test, feature = "test-support"))]
pub use fake::{FakeFs, Release};

#[cfg(any(test, feature = "test-support"))]
mod fake;

#[cfg(test)]
mod tests;
