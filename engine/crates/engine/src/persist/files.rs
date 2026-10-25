// SPDX-License-Identifier: GPL-3.0-or-later

//! Small, bounded file reads.

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::time::UNIX_EPOCH;

use super::PersistError;

/// Reads a UTF-8 text file of at most `limit` bytes. Larger files are refused without being
/// read in full, so a huge or hostile file cannot exhaust memory.
pub fn read_text(path: &Path, limit: usize) -> Result<String, PersistError> {
    let file = File::open(path).map_err(PersistError::io("open", path))?;
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
    String::from_utf8(bytes).map_err(|_| PersistError::NotText(path.to_path_buf()))
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
