// SPDX-License-Identifier: GPL-3.0-or-later

//! Opening user-chosen files without ever blocking on what is there (#86, #323). Here rather
//! than in the engine because media opens (this crate) and show-file opens (the engine, which
//! depends on this crate) need the same check, and `openlab-core` does no I/O.

use std::fmt;
use std::fs::{self, File};
use std::io;
use std::path::Path;

/// The error inside the [`io::Error`] (kind `InvalidInput`) that [`open_regular`] returns for
/// something that is not a regular file: a FIFO, socket, device or directory.
#[derive(Debug)]
pub struct NotRegularFile;

impl fmt::Display for NotRegularFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not a regular file")
    }
}

impl std::error::Error for NotRegularFile {}

/// Whether `e` is [`open_regular`] refusing something that is not a regular file (rather than
/// the open itself failing).
pub fn is_not_regular(e: &io::Error) -> bool {
    e.get_ref()
        .is_some_and(|inner| inner.is::<NotRegularFile>())
}

/// Opens `path` for reading only if it is a regular file, without ever blocking on what was
/// opened. Opening a FIFO for reading waits for a writer, for ever if none comes, and the
/// thread doing it (a probe or disk-stream worker, the show-file worker) would stop serving
/// everything after it. On Unix the open is non-blocking (a FIFO then opens at once); on
/// Windows opening never waits (a named pipe with no free instance fails at once). Either way
/// the type is then checked through the opened handle, so a FIFO, device or folder swapped in
/// after any earlier look at the path is refused before a byte is read.
pub fn open_regular(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, NotRegularFile));
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
pub const O_NONBLOCK: i32 = 0x0004;
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
pub const O_NONBLOCK: i32 = 0o4000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_regular_file_opens_and_a_folder_is_refused_as_not_regular() {
        let dir = std::env::temp_dir().join(format!("openlab-regular-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.wav");
        fs::write(&file, b"x").unwrap();
        assert!(open_regular(&file).is_ok());
        let e = open_regular(&dir).unwrap_err();
        // Windows refuses to open a folder as a file at all.
        #[cfg(unix)]
        assert!(is_not_regular(&e), "{e}");
        let _ = e;
        let missing = open_regular(&dir.join("missing.wav")).unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
        assert!(!is_not_regular(&missing));
        let _ = fs::remove_dir_all(&dir);
    }
}
