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
    // Opening a FIFO (or a device) for reading can block for ever: only regular files are read.
    // `metadata` does not open the file, so it cannot hang on a pipe. The opened file is checked
    // again, which only guards a directory or device swapped in between: `File::open` on a FIFO
    // blocks before that check can run, so a swap to a FIFO in that window is not caught (no
    // portable O_NONBLOCK without libc).
    let meta = fs::metadata(path).map_err(PersistError::io("open", path))?;
    if !meta.is_file() {
        return Err(PersistError::io("open", path)(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        )));
    }
    let file = File::open(path).map_err(PersistError::io("open", path))?;
    let opened = file.metadata().map_err(PersistError::io("open", path))?;
    if !opened.is_file() {
        return Err(PersistError::io("open", path)(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        )));
    }
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
