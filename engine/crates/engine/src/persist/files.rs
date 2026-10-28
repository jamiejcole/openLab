// SPDX-License-Identifier: GPL-3.0-or-later

//! Small, bounded file reads.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use super::PersistError;

/// Reads a UTF-8 text file of at most `limit` bytes. Larger files are refused without being
/// read in full, so a huge or hostile file cannot exhaust memory.
pub fn read_text(path: &Path, limit: usize) -> Result<String, PersistError> {
    let bytes = read_bytes(path, limit)?;
    String::from_utf8(bytes).map_err(|_| PersistError::NotText(path.to_path_buf()))
}

/// [`read_text`] without the UTF-8 check.
pub(crate) fn read_bytes(path: &Path, limit: usize) -> Result<Vec<u8>, PersistError> {
    let file = open_regular(path)?;
    let limit = limit as u64;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(PersistError::io("read", path))?;
    if bytes.len() as u64 > limit {
        let size = fs::metadata(path).map_or(limit.saturating_add(1), |m| m.len());
        return Err(PersistError::TooLarge {
            path: path.to_path_buf(),
            size,
            limit,
        });
    }
    Ok(bytes)
}

/// Opens `path` for reading only if it is a regular file, without ever blocking on what was
/// opened. Opening a FIFO for reading waits for a writer, for ever if none comes, and the
/// worker doing it would stop serving every later save or open. On Unix the open is
/// non-blocking (a FIFO then opens at once); on Windows opening never waits (a named pipe with
/// no free instance fails at once). Either way the type is then checked through the opened
/// handle, so a FIFO, device or folder swapped in after any earlier look at the path is
/// refused before a byte is read.
pub(crate) fn open_regular(path: &Path) -> Result<File, PersistError> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(O_NONBLOCK);
    }
    let file = options.open(path).map_err(PersistError::io("open", path))?;
    let opened = file.metadata().map_err(PersistError::io("open", path))?;
    if !opened.is_file() {
        return Err(PersistError::io("open", path)(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        )));
    }
    // O_NONBLOCK does not change reads of a regular file, so the flag can stay set.
    Ok(file)
}

/// `O_NONBLOCK` from the C headers (std has no constant for it, and it differs by platform).
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
pub(super) const O_NONBLOCK: i32 = 0x0004;
#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    any(
        target_arch = "x86",
        target_arch = "x86_64",
        target_arch = "arm",
        target_arch = "aarch64",
        target_arch = "riscv64",
        target_arch = "powerpc64",
        target_arch = "loongarch64",
        target_arch = "s390x"
    )
))]
pub(super) const O_NONBLOCK: i32 = 0o4000;

/// Whether `path` is a regular file: `Ok(true)`, absent (`Ok(false)`), or something else (a FIFO,
/// socket, device or directory, also through a symlink), which is an error. Never opens the path
/// (opening a FIFO blocks), so it cannot hang. A dangling symlink counts as absent.
pub(crate) fn regular_file_or_absent(path: &Path) -> Result<bool, PersistError> {
    match fs::metadata(path) {
        Ok(meta) if meta.is_file() => Ok(true),
        Ok(_) => Err(PersistError::io("use", path)(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(PersistError::io("inspect", path)(e)),
    }
}

/// Modification time in milliseconds since the Unix epoch, or `None` if the file is missing or
/// the platform cannot say.
pub fn modified_ms(path: &Path) -> Option<i64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    Some(match modified.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_millis()).ok()?,
        Err(before) => -i64::try_from(before.duration().as_millis()).ok()?,
    })
}

/// [`openlab_core::path_hash`] of a file path, spelled the way show files spell paths.
pub(crate) fn path_key(path: &Path) -> String {
    let text = path.to_string_lossy();
    openlab_core::path_hash(&openlab_core::to_portable(
        &text,
        openlab_core::PathStyle::host(),
    ))
}

/// A name beside `path` ending `.<suffix>`, `.<suffix>.1`, ... that does not exist yet, so
/// evidence kept from one failure is not overwritten by the next. After a few tries the last
/// name is reused, so repeated failures cannot fill the disk.
pub fn unique_sibling(path: &Path, suffix: &str) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_os_string();
    let mut candidate = path.to_path_buf();
    for n in 0..8 {
        let mut next = name.clone();
        next.push(format!(".{suffix}"));
        if n > 0 {
            next.push(format!(".{n}"));
        }
        candidate = path.with_file_name(next);
        if !candidate.exists() {
            break;
        }
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A FIFO in place of the show file when the worker opens it (swapped in after any check
    /// of the path) is refused at once, not waited on: the open itself must not block.
    #[cfg(unix)]
    #[test]
    fn opening_a_fifo_is_refused_without_blocking() {
        let _serial = crate::diag::run_marker::tests::serialize_spawn_and_lock_release();
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("show.openlab");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        let (tx, rx) = std::sync::mpsc::channel();
        let path = fifo.clone();
        std::thread::spawn(move || {
            let _ = tx.send(open_regular(&path).map(|_| ()));
        });
        // A hang guard, not a timing assertion.
        let out = rx.recv_timeout(std::time::Duration::from_secs(20));
        if out.is_err() {
            // Give the blocked open its writer so the thread can end.
            let _ = fs::OpenOptions::new().write(true).open(&fifo);
            panic!("opening a FIFO blocked");
        }
        assert!(
            matches!(&out, Ok(Err(PersistError::Io { source, .. })) if source.kind() == std::io::ErrorKind::InvalidInput),
            "{out:?}"
        );
    }

    #[test]
    fn a_regular_file_opens_and_a_folder_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("show.openlab");
        fs::write(&file, "x").unwrap();
        assert_eq!(read_bytes(&file, 10).unwrap(), b"x");
        assert!(open_regular(dir.path()).is_err());
    }
}
