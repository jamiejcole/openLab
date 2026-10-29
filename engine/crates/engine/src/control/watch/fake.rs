// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

use std::collections::{HashMap, HashSet};
use std::sync::Condvar;

/// An in-memory file system for tests: files by path with an identity, a size and a modified
/// time; the Trash; paths that refuse access or hang.
#[derive(Clone, Default)]
pub struct FakeFs(Arc<FakeShared>);

#[derive(Default)]
struct FakeShared {
    state: Mutex<FakeState>,
    changed: Condvar,
}

#[derive(Default)]
struct FakeState {
    files: HashMap<PathBuf, FakeFile>,
    trash: HashMap<PathBuf, FakeFile>,
    denied: HashSet<PathBuf>,
    hung: HashSet<PathBuf>,
    /// Stats blocked on a hung path now.
    hanging: HashMap<PathBuf, usize>,
    next_id: u64,
    stats: u64,
}

#[derive(Clone, Copy, Debug)]
struct FakeFile {
    dev: u64,
    ino: u64,
    size: u64,
    modified_ms: i64,
    symlink: bool,
}

/// A hung path; dropping it (or [`Release::release`]) lets its stats return.
pub struct Release {
    fs: FakeFs,
    path: PathBuf,
}

impl Release {
    pub fn release(self) {}
}

impl Drop for Release {
    fn drop(&mut self) {
        lock(&self.fs.0.state).hung.remove(&self.path);
        self.fs.0.changed.notify_all();
    }
}

impl fmt::Debug for FakeFs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FakeFs")
    }
}

impl FakeFs {
    pub fn new() -> Self {
        Self::default()
    }

    fn with<R>(&self, f: impl FnOnce(&mut FakeState) -> R) -> R {
        let r = f(&mut lock(&self.0.state));
        self.0.changed.notify_all();
        r
    }

    /// A new file at `path` on device `dev` (a fresh identity).
    pub fn put(&self, path: impl AsRef<Path>, dev: u64) {
        self.with(|s| {
            s.next_id += 1;
            let ino = s.next_id;
            s.files.insert(
                path.as_ref().to_path_buf(),
                FakeFile {
                    dev,
                    ino,
                    size: 1000,
                    modified_ms: 1_000_000,
                    symlink: false,
                },
            );
        });
    }

    pub fn rename(&self, from: impl AsRef<Path>, to: impl AsRef<Path>) {
        self.with(|s| {
            if let Some(f) = s.files.remove(from.as_ref()) {
                s.files.insert(to.as_ref().to_path_buf(), f);
            }
        });
    }

    pub fn remove(&self, path: impl AsRef<Path>) {
        self.with(|s| s.files.remove(path.as_ref()));
    }

    /// Moves the file to the Trash (remembering where it was, for [`Self::put_back`]).
    pub fn trash(&self, path: impl AsRef<Path>) {
        self.with(|s| {
            if let Some(f) = s.files.remove(path.as_ref()) {
                s.trash.insert(path.as_ref().to_path_buf(), f);
            }
        });
    }

    pub fn put_back(&self, path: impl AsRef<Path>) {
        self.with(|s| {
            if let Some(f) = s.trash.remove(path.as_ref()) {
                s.files.insert(path.as_ref().to_path_buf(), f);
            }
        });
    }

    /// Another file takes the place of the one at `path` (a new identity and size).
    pub fn replace(&self, path: impl AsRef<Path>) {
        self.with(|s| {
            s.next_id += 1;
            let ino = s.next_id;
            if let Some(f) = s.files.get_mut(path.as_ref()) {
                f.ino = ino;
                f.size += 1;
                f.modified_ms += 1;
            }
        });
    }

    /// The file was written to (a later modified time).
    pub fn touch(&self, path: impl AsRef<Path>) {
        self.with(|s| {
            if let Some(f) = s.files.get_mut(path.as_ref()) {
                f.modified_ms += 1;
            }
        });
    }

    pub fn set_size(&self, path: impl AsRef<Path>, size: u64) {
        self.with(|s| {
            if let Some(f) = s.files.get_mut(path.as_ref()) {
                f.size = size;
            }
        });
    }

    pub fn set_symlink(&self, path: impl AsRef<Path>, symlink: bool) {
        self.with(|s| {
            if let Some(f) = s.files.get_mut(path.as_ref()) {
                f.symlink = symlink;
            }
        });
    }

    /// Stats of `path` fail with `PermissionDenied`.
    pub fn deny(&self, path: impl AsRef<Path>) {
        self.with(|s| s.denied.insert(path.as_ref().to_path_buf()));
    }

    pub fn allow(&self, path: impl AsRef<Path>) {
        self.with(|s| s.denied.remove(path.as_ref()));
    }

    /// Stats of `path` block until the returned [`Release`] goes.
    pub fn hang(&self, path: impl AsRef<Path>) -> Release {
        self.with(|s| s.hung.insert(path.as_ref().to_path_buf()));
        Release {
            fs: self.clone(),
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Waits (at most `limit`) until a stat is blocked on `path`; whether one is.
    pub fn wait_hanging(&self, path: impl AsRef<Path>, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        let mut s = lock(&self.0.state);
        loop {
            if s.hanging.get(path.as_ref()).is_some_and(|n| *n > 0) {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            s = self
                .0
                .changed
                .wait_timeout(s, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    /// Waits (at most `limit`) until `n` more stats have been made than `after`; whether they
    /// were.
    pub fn wait_stats(&self, after: u64, n: u64, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        let mut s = lock(&self.0.state);
        loop {
            if s.stats >= after.saturating_add(n) {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            s = self
                .0
                .changed
                .wait_timeout(s, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    /// Stats made so far.
    pub fn stats(&self) -> u64 {
        lock(&self.0.state).stats
    }
}

impl WatchFs for FakeFs {
    fn stat(&self, path: &Path) -> io::Result<Stat> {
        let mut s = lock(&self.0.state);
        s.stats += 1;
        if s.hung.contains(path) {
            *s.hanging.entry(path.to_path_buf()).or_default() += 1;
            self.0.changed.notify_all();
            while s.hung.contains(path) {
                s = self
                    .0
                    .changed
                    .wait(s)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            if let Some(n) = s.hanging.get_mut(path) {
                *n = n.saturating_sub(1);
            }
        }
        self.0.changed.notify_all();
        if s.denied.contains(path) {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied));
        }
        let f = s
            .files
            .get(path)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        Ok(Stat {
            fp: Fingerprint {
                dev: f.dev,
                ino: f.ino,
                size: f.size,
                modified_ms: f.modified_ms,
            },
            regular: true,
            symlink: f.symlink,
        })
    }
}
