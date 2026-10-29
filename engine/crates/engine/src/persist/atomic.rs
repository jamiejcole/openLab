// SPDX-License-Identifier: GPL-3.0-or-later

//! Crash-safe file replacement (NFR-04).

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use super::PersistError;

/// Points in [`write_atomic_with`] where a test can inject a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// The temp file is written but not yet synced.
    TmpWritten,
    /// The temp file is on disk; the rename has not happened.
    TmpSynced,
}

/// Replaces `path` with `bytes` so that a crash or error at any point leaves either the old
/// file or the new one, never a mixture: write `<name>.<pid>-<n>.tmp` beside it, fsync, rename over the
/// original, fsync the folder (where the platform allows).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), PersistError> {
    write_atomic_with(path, bytes, |_| Ok(()))
}

/// [`write_atomic`] with `checkpoint` called at each [`Stage`]; an `Err` from it aborts the
/// save as if that step had failed. The temp file is removed on failure.
///
/// On Unix the destination's permission mode is kept, a symlinked destination is written
/// through (the link stays a link and its target is replaced), and a read-only destination is
/// refused rather than replaced. Extended attributes and Finder tags are not carried over.
pub fn write_atomic_with(
    path: &Path,
    bytes: &[u8],
    checkpoint: impl FnMut(Stage) -> io::Result<()>,
) -> Result<(), PersistError> {
    write_atomic_inner(
        path,
        bytes,
        checkpoint,
        &mut |from, to| fs::rename(from, to),
        &mut thread::sleep,
        RenameRetry::HOST,
    )
}

/// Bounded retry for the final rename. On Windows an antivirus scanner or search indexer can
/// hold the destination open for a moment, and the rename then fails with a sharing or access
/// error that goes away by itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RenameRetry {
    /// Total tries including the first; 1 means no retry.
    attempts: u32,
    /// Wait before the second try; doubles each time.
    first_backoff: Duration,
}

impl RenameRetry {
    /// 5 tries, waiting 10 + 20 + 40 + 80 ms = 150 ms at most.
    const WINDOWS: Self = Self {
        attempts: 5,
        first_backoff: Duration::from_millis(10),
    };
    const NONE: Self = Self {
        attempts: 1,
        first_backoff: Duration::ZERO,
    };
    /// Only Windows holds files open against a rename; elsewhere a permission error is real.
    const HOST: Self = if cfg!(windows) {
        Self::WINDOWS
    } else {
        Self::NONE
    };
}

/// Errors that a lock held by another program can cause and that may clear on their own.
fn is_transient_lock(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::PermissionDenied {
        return true;
    }
    // ERROR_ACCESS_DENIED (5), ERROR_SHARING_VIOLATION (32), ERROR_LOCK_VIOLATION (33).
    // Raw codes mean something else elsewhere, so only Windows reads them.
    cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32 | 33))
}

fn rename_with_retry(
    from: &Path,
    to: &Path,
    rename: &mut dyn FnMut(&Path, &Path) -> io::Result<()>,
    sleep: &mut dyn FnMut(Duration),
    retry: RenameRetry,
) -> io::Result<()> {
    let mut backoff = retry.first_backoff;
    let mut attempt = 1;
    loop {
        match rename(from, to) {
            Err(e) if attempt < retry.attempts && is_transient_lock(&e) => {
                sleep(backoff);
                backoff = backoff.saturating_mul(2);
                attempt += 1;
            }
            other => return other,
        }
    }
}

/// Where a write to `path` really goes: the target of a symlink, else `path` itself.
#[cfg(unix)]
fn resolve_destination(path: &Path) -> io::Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => fs::canonicalize(path)
            .map_err(|e| io::Error::new(e.kind(), format!("symlink target cannot be used: {e}"))),
        _ => Ok(path.to_path_buf()),
    }
}

#[cfg(not(unix))]
fn resolve_destination(path: &Path) -> io::Result<PathBuf> {
    Ok(path.to_path_buf())
}

pub(crate) fn write_atomic_inner(
    path: &Path,
    bytes: &[u8],
    checkpoint: impl FnMut(Stage) -> io::Result<()>,
    rename: &mut dyn FnMut(&Path, &Path) -> io::Result<()>,
    sleep: &mut dyn FnMut(Duration),
    retry: RenameRetry,
) -> Result<(), PersistError> {
    replace(
        path,
        bytes,
        Owner::User,
        |_| {},
        checkpoint,
        rename,
        sleep,
        retry,
    )
    .map(drop)
}

/// Whose file [`replace`] writes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner {
    /// A document the operator may have linked, protected or given a mode: a symlink is written
    /// through, a read-only file refused, the old mode kept.
    User,
    /// A file only the engine writes and that may hold a secret: whatever is at the path is
    /// replaced, and the new file is readable by the user only (mode 0600 on Unix).
    Engine,
}

/// [`write_atomic`] for a file the engine owns outright (`engine.json`, run markers): replaced
/// as a whole, mode 0600 on Unix, stale temp files of a crashed run of another pid removed
/// ([`remove_stale_tmp`]: for `engine.json`; a marker's temp always carries its own pid, so
/// `diag::run_marker` removes crashed runs' marker temps itself). `prepare` runs on
/// the temp file before anything is written to it (a run marker takes its lock there, so it is
/// never seen unlocked under its name); the open file is returned.
pub(crate) fn write_engine_file(
    path: &Path,
    bytes: &[u8],
    prepare: impl FnOnce(&File),
) -> Result<File, PersistError> {
    let file = replace(
        path,
        bytes,
        Owner::Engine,
        prepare,
        |_| Ok(()),
        &mut |from, to| fs::rename(from, to),
        &mut thread::sleep,
        RenameRetry::HOST,
    )?;
    remove_stale_tmp(path);
    Ok(file)
}

#[allow(clippy::too_many_arguments)]
fn replace(
    path: &Path,
    bytes: &[u8],
    owner: Owner,
    prepare: impl FnOnce(&File),
    mut checkpoint: impl FnMut(Stage) -> io::Result<()>,
    rename: &mut dyn FnMut(&Path, &Path) -> io::Result<()>,
    sleep: &mut dyn FnMut(Duration),
    retry: RenameRetry,
) -> Result<File, PersistError> {
    let resolved;
    let path = if owner == Owner::User {
        resolved = resolve_destination(path).map_err(PersistError::io("replace", path))?;
        &resolved
    } else {
        path
    };
    let Some(name) = path.file_name() else {
        return Err(PersistError::io("write", path)(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path has no file name",
        )));
    };
    // A user's replacement keeps the old file's mode. A read-only file is the operator's way
    // of saying "do not change this", and a rename would silently override it.
    #[cfg(unix)]
    let new_permissions = match (owner, fs::metadata(path)) {
        (Owner::Engine, _) => {
            use std::os::unix::fs::PermissionsExt as _;
            Some(fs::Permissions::from_mode(0o600))
        }
        (Owner::User, Ok(meta)) if meta.permissions().readonly() => {
            return Err(PersistError::io("replace", path)(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the file is read-only",
            )));
        }
        (Owner::User, Ok(meta)) => Some(meta.permissions()),
        (Owner::User, Err(_)) => None,
    };

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let (tmp, mut file) = create_tmp(path, name, &COUNTER)?;
    let staged = (|| {
        #[cfg(unix)]
        if let Some(permissions) = new_permissions {
            file.set_permissions(permissions)?;
        }
        prepare(&file);
        file.write_all(bytes)?;
        checkpoint(Stage::TmpWritten)?;
        file.sync_all()?;
        checkpoint(Stage::TmpSynced)?;
        Ok::<(), io::Error>(())
    })();
    if let Err(source) = staged {
        drop(file);
        let _ = fs::remove_file(&tmp);
        return Err(PersistError::io("write", &tmp)(source));
    }
    if let Err(source) = rename_with_retry(&tmp, path, rename, sleep, retry) {
        drop(file);
        let _ = fs::remove_file(&tmp);
        return Err(PersistError::io("replace", path)(source));
    }
    sync_parent(path);
    Ok(file)
}

/// Writes `bytes` to the first of `candidates` that does not exist, never replacing a file:
/// written and synced under a temp name beside the first candidate, then put in place with
/// [`rename_no_replace`]. Returns the name used. Every candidate taken is `AlreadyExists`.
pub(crate) fn write_new(
    candidates: impl IntoIterator<Item = PathBuf>,
    bytes: &[u8],
) -> Result<PathBuf, PersistError> {
    let mut candidates = candidates.into_iter().peekable();
    let Some(first) = candidates.peek().cloned() else {
        return Err(PersistError::io("write", Path::new(""))(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no file name to write",
        )));
    };
    let Some(name) = first.file_name() else {
        return Err(PersistError::io("write", &first)(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path has no file name",
        )));
    };
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let (tmp, mut file) = create_tmp(&first, name, &COUNTER)?;
    if let Err(source) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&tmp);
        return Err(PersistError::io("write", &tmp)(source));
    }
    drop(file);
    for candidate in candidates {
        match rename_no_replace(&tmp, &candidate) {
            Ok(()) => {
                sync_parent(&candidate);
                return Ok(candidate);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                let _ = fs::remove_file(&tmp);
                return Err(PersistError::io("write", &candidate)(source));
            }
        }
    }
    let _ = fs::remove_file(&tmp);
    Err(PersistError::io("write", &first)(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "every free name is taken",
    )))
}

/// Moves `from` to `to` only if nothing is at `to` (`AlreadyExists` then, `from` left as it
/// was): a backup or a set-aside recovery file must never replace another. A hard link is the
/// portable rename that refuses an existing name, atomically. Where the file system has no hard
/// links (FAT, exFAT, some shares) it falls back to a check, then a rename, which only another
/// process creating that very name in between can defeat (the instance lock keeps a second
/// engine out).
pub(crate) fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    match fs::hard_link(from, to) {
        Ok(()) => match fs::remove_file(from) {
            Ok(()) => Ok(()),
            Err(e) => {
                // Not a move after all: take the new name back so `from` is still the only one.
                let _ = fs::remove_file(to);
                Err(e)
            }
        },
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e),
        Err(_) => {
            if fs::symlink_metadata(to).is_ok() {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists));
            }
            fs::rename(from, to)
        }
    }
}

/// How many temp names one write tries before giving up.
const TMP_NAME_TRIES: u32 = 16;

/// Creates the temp file `<name>.<pid>-<n>.tmp` beside `path`, with `create_new`, so a second
/// process saving the same show, a user's own `.tmp` file or a planted symlink can never be
/// written through or clobbered. A name that exists already (a crash leftover of an earlier
/// process that had this pid, which stale-file cleanup skips as "our own") is skipped for the
/// next `n`, a bounded number of times.
fn create_tmp(
    path: &Path,
    name: &OsStr,
    counter: &AtomicU64,
) -> Result<(PathBuf, File), PersistError> {
    let mut tries = 0;
    loop {
        let mut tmp_name = name.to_os_string();
        tmp_name.push(format!(
            ".{}-{}.tmp",
            std::process::id(),
            counter.fetch_add(1, Ordering::Relaxed)
        ));
        let tmp = path.with_file_name(tmp_name);
        tries += 1;
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(file) => return Ok((tmp, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && tries < TMP_NAME_TRIES => {}
            Err(e) => return Err(PersistError::io("write", &tmp)(e)),
        }
    }
}

/// Makes the rename itself durable. Best effort: the data is already safe, and not every
/// platform or file system lets a folder be opened and synced (Windows does not).
fn sync_parent(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        // Non-blocking: a FIFO swapped in for the folder must not hang the writer.
        use std::os::unix::fs::OpenOptionsExt as _;
        if let Ok(dir) = OpenOptions::new()
            .read(true)
            .custom_flags(super::files::O_NONBLOCK)
            .open(parent)
        {
            let _ = dir.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Splits `<base>.<pid>-<n>.tmp`, the name [`write_atomic_with`] gives its temp files, into
/// `base` and `pid`. Strict, so a user's own `.tmp` file never matches.
pub(crate) fn split_tmp_name(name: &str) -> Option<(&str, u32)> {
    let (base, unique) = name.strip_suffix(".tmp")?.rsplit_once('.')?;
    let (pid, n) = unique.split_once('-')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if base.is_empty() || !digits(pid) || !digits(n) {
        return None;
    }
    Some((base, pid.parse().ok()?))
}

/// How much older than the freshly saved file a leftover temp file must be to be removed.
pub(crate) const STALE_TMP_AGE: Duration = Duration::from_secs(60);

/// Removes temp files this module left beside `path` before a crash. Only ones from another
/// process (this process's own writes finish or clean up themselves) that are at least
/// [`STALE_TMP_AGE`] older than `path`, which has just been saved: a younger one could be
/// another instance's write still in progress. Best effort; returns how many were removed.
pub(crate) fn remove_stale_tmp(path: &Path) -> usize {
    // A symlinked show keeps its temp files beside the target.
    let path = &resolve_destination(path).unwrap_or_else(|_| path.to_path_buf());
    let (Some(name), Some(dir)) = (path.file_name().and_then(|n| n.to_str()), path.parent()) else {
        return 0;
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let Ok(saved) = fs::metadata(path).and_then(|m| m.modified()) else {
        return 0;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some((base, pid)) = file_name.to_str().and_then(split_tmp_name) else {
            continue;
        };
        if base != name || pid == std::process::id() {
            continue;
        }
        let old_enough = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| saved.duration_since(t).ok())
            .is_some_and(|age| age >= STALE_TMP_AGE);
        if old_enough && fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn denied() -> io::Error {
        io::Error::from(io::ErrorKind::PermissionDenied)
    }

    fn run(
        results: Vec<io::Result<()>>,
        retry: RenameRetry,
    ) -> (io::Result<()>, usize, Vec<Duration>) {
        let results = RefCell::new(results.into_iter());
        let calls = Cell::new(0);
        let sleeps = RefCell::new(Vec::new());
        let out = rename_with_retry(
            Path::new("a"),
            Path::new("b"),
            &mut |_, _| {
                calls.set(calls.get() + 1);
                results.borrow_mut().next().unwrap_or(Ok(()))
            },
            &mut |d| sleeps.borrow_mut().push(d),
            retry,
        );
        (out, calls.get(), sleeps.into_inner())
    }

    #[test]
    fn transient_lock_is_retried_with_doubling_backoff_then_succeeds() {
        let (out, calls, sleeps) = run(
            vec![Err(denied()), Err(denied()), Ok(())],
            RenameRetry::WINDOWS,
        );
        assert!(out.is_ok());
        assert_eq!(calls, 3);
        assert_eq!(
            sleeps,
            vec![Duration::from_millis(10), Duration::from_millis(20)]
        );
    }

    #[test]
    fn retry_is_bounded_and_total_wait_is_well_under_a_second() {
        let (out, calls, sleeps) = run(
            (0..20).map(|_| Err(denied())).collect(),
            RenameRetry::WINDOWS,
        );
        assert_eq!(out.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(calls, 5);
        assert!(sleeps.iter().sum::<Duration>() <= Duration::from_millis(200));
    }

    #[test]
    fn other_errors_fail_at_once() {
        for kind in [
            io::ErrorKind::NotFound,
            io::ErrorKind::InvalidInput,
            io::ErrorKind::Other,
        ] {
            let (out, calls, sleeps) = run(vec![Err(io::Error::from(kind))], RenameRetry::WINDOWS);
            assert_eq!(out.unwrap_err().kind(), kind);
            assert_eq!(calls, 1);
            assert!(sleeps.is_empty());
        }
    }

    #[test]
    fn no_retry_policy_fails_at_once_even_for_permission_denied() {
        let (out, calls, sleeps) = run(vec![Err(denied()), Ok(())], RenameRetry::NONE);
        assert!(out.is_err());
        assert_eq!(calls, 1);
        assert!(sleeps.is_empty());
    }

    #[test]
    fn host_policy_retries_only_on_windows() {
        assert_eq!(RenameRetry::HOST.attempts > 1, cfg!(windows));
    }

    #[cfg(windows)]
    #[test]
    fn sharing_violation_codes_count_as_transient() {
        for code in [5, 32, 33] {
            assert!(is_transient_lock(&io::Error::from_raw_os_error(code)));
        }
        assert!(!is_transient_lock(&io::Error::from_raw_os_error(2)));
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("openlab-atomic-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn tmp_for(dir: &Path, n: u64) -> PathBuf {
        dir.join(format!("show.openlab.{}-{n}.tmp", std::process::id()))
    }

    #[test]
    fn an_existing_temp_name_is_skipped_and_left_alone() {
        let dir = scratch("tmp-taken");
        let target = dir.join("show.openlab");
        for n in 5..8 {
            fs::write(tmp_for(&dir, n), b"leftover").unwrap();
        }
        let counter = AtomicU64::new(5);
        let (tmp, _file) = create_tmp(&target, OsStr::new("show.openlab"), &counter).unwrap();
        assert_eq!(tmp, tmp_for(&dir, 8));
        for n in 5..8 {
            assert_eq!(fs::read(tmp_for(&dir, n)).unwrap(), b"leftover");
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn temp_names_are_tried_a_bounded_number_of_times() {
        let dir = scratch("tmp-all-taken");
        let target = dir.join("show.openlab");
        let tries = u64::from(TMP_NAME_TRIES);
        for n in 0..tries {
            fs::write(tmp_for(&dir, n), b"leftover").unwrap();
        }
        let counter = AtomicU64::new(0);
        let out = create_tmp(&target, OsStr::new("show.openlab"), &counter);
        assert!(
            matches!(&out, Err(PersistError::Io { source, .. }) if source.kind() == io::ErrorKind::AlreadyExists),
            "{out:?}"
        );
        assert_eq!(counter.load(Ordering::Relaxed), tries);
        for n in 0..tries {
            assert_eq!(fs::read(tmp_for(&dir, n)).unwrap(), b"leftover");
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn write_atomic_retries_a_locked_rename_and_leaves_no_temp_file() {
        let dir = scratch("retry");
        let target = dir.join("retry.openlab");
        let calls = Cell::new(0);
        let slept = Cell::new(0);
        let out = write_atomic_inner(
            &target,
            b"new",
            |_| Ok(()),
            &mut |from, to| {
                calls.set(calls.get() + 1);
                if calls.get() < 3 {
                    Err(denied())
                } else {
                    fs::rename(from, to)
                }
            },
            &mut |_| slept.set(slept.get() + 1),
            RenameRetry::WINDOWS,
        );
        assert!(out.is_ok());
        assert_eq!((calls.get(), slept.get()), (3, 2));
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1, "no temp file left");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn exhausted_retry_removes_the_temp_file_and_keeps_the_original() {
        let dir = scratch("exhausted");
        let target = dir.join("locked.openlab");
        fs::write(&target, b"old").unwrap();
        let out = write_atomic_inner(
            &target,
            b"new",
            |_| Ok(()),
            &mut |_, _| Err(denied()),
            &mut |_| {},
            RenameRetry::WINDOWS,
        );
        assert!(matches!(out, Err(PersistError::Io { op: "replace", .. })));
        assert_eq!(fs::read(&target).unwrap(), b"old");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rename_no_replace_refuses_an_existing_name_and_leaves_both_files() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("new"), dir.path().join("old"));
        fs::write(&from, "new").unwrap();
        fs::write(&to, "old").unwrap();
        let err = rename_no_replace(&from, &to).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&from).unwrap(), "new");
        assert_eq!(fs::read_to_string(&to).unwrap(), "old");
        fs::remove_file(&to).unwrap();
        rename_no_replace(&from, &to).unwrap();
        assert!(!from.exists());
        assert_eq!(fs::read_to_string(&to).unwrap(), "new");
    }

    #[test]
    fn tmp_names_are_parsed_strictly() {
        assert_eq!(
            split_tmp_name("a.openlab.12-3.tmp"),
            Some(("a.openlab", 12))
        );
        for bad in [
            "a.openlab.tmp",
            "a.openlab.x-3.tmp",
            "a.openlab.12-.tmp",
            "a.openlab.12-3.tmp.bak",
            ".12-3.tmp",
            "a.12.tmp",
            "a.99999999999-1.tmp",
        ] {
            assert_eq!(split_tmp_name(bad), None, "{bad}");
        }
    }
}
