// SPDX-License-Identifier: GPL-3.0-or-later

//! The file work of copying a cue's file into the show folder (WS-04): the copy is written under
//! a temp name in its destination folder and renamed into place only when complete, so a failed,
//! cancelled or crashed copy never leaves a file a cue could be pointed at. A file already in
//! the folder with the same content (size first, then byte for byte) is reused instead of copied
//! again. Which files to copy and the retarget are core's (`media_copy.rs`); when this runs and
//! what the cues become is the control loop's.

use std::fs::{self, File};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use openlab_core::{MAX_COPY_NUMBER, copy_name_ok, numbered_name};

use super::atomic::{
    STALE_TMP_AGE, create_tmp_beside, rename_no_replace, split_tmp_name, sync_parent,
};

/// Bytes read and written per step. Also the copy's whole memory: one buffer, halved for a
/// comparison.
pub const COPY_CHUNK: usize = 1024 * 1024;

/// Pause after each chunk, so a copy never takes a disk for long stretches from the streams
/// that play from it (a stream reads about two seconds ahead).
pub const COPY_PAUSE: Duration = Duration::from_millis(2);

/// A copy is flushed to its disk every this many bytes, so its last sync is short: the stall
/// check measures progress, and one sync of gigabytes on a slow USB disk must not look stuck.
pub const SYNC_EVERY: u64 = 64 * 1024 * 1024;

/// Most entries of a destination folder looked at for an identical file. Past it the rest are
/// not looked at, so a copy may be made of a file the folder already holds (never the reverse).
pub const MAX_INDEX_ENTRIES: usize = 20_000;

/// How a copy is paced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CopyPacing {
    pub chunk: usize,
    pub pause: Duration,
}

impl Default for CopyPacing {
    fn default() -> Self {
        Self {
            chunk: COPY_CHUNK,
            pause: COPY_PAUSE,
        }
    }
}

/// What the control loop and a copy's thread share: the request to stop, and how far it got.
#[derive(Debug, Default)]
pub struct CopyProgress {
    cancel: AtomicBool,
    files_done: AtomicU64,
    bytes_done: AtomicU64,
    /// The sizes of the files to copy, once the copy's thread has looked (0 until then).
    bytes_total: AtomicU64,
    /// Rises with every chunk read or written: the stall check sees a thread stuck in one call.
    beats: AtomicU64,
}

impl CopyProgress {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    pub fn files_done(&self) -> u64 {
        self.files_done.load(Ordering::SeqCst)
    }

    pub fn bytes_done(&self) -> u64 {
        self.bytes_done.load(Ordering::SeqCst)
    }

    pub fn bytes_total(&self) -> u64 {
        self.bytes_total.load(Ordering::SeqCst)
    }

    /// Adds one file's size to the total, a beat each: thousands of stats on a slow share are
    /// progress, not a stall.
    pub fn add_bytes_total(&self, size: u64) {
        self.bytes_total.fetch_add(size, Ordering::SeqCst);
        self.beat();
    }

    pub fn beats(&self) -> u64 {
        self.beats.load(Ordering::SeqCst)
    }

    /// One file is done with (copied, reused, left or failed).
    pub fn file_done(&self) {
        self.files_done.fetch_add(1, Ordering::SeqCst);
        self.beat();
    }

    /// Something moved (a stat, a chunk, a sync): the stall check sees the copy is not stuck.
    pub fn beat(&self) {
        self.beats.fetch_add(1, Ordering::SeqCst);
    }

    fn add_bytes(&self, n: u64) {
        self.bytes_done.fetch_add(n, Ordering::SeqCst);
        self.beat();
    }
}

/// Why a file was not copied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyFailure {
    /// The file is not there.
    Missing,
    /// The file could not be read (permissions, a damaged disk, not a regular file).
    Unreadable,
    /// The show folder's volume is full.
    DiskFull,
    /// The show folder cannot be written.
    ReadOnly,
    /// The file's name cannot be a file name in the show folder.
    BadName,
    /// The copy was stopped (the show closed or went into Show Mode).
    Cancelled,
    /// Anything else (the message says what).
    Other,
}

#[derive(Debug)]
pub struct CopyError {
    pub failure: CopyFailure,
    pub message: String,
}

impl CopyError {
    fn new(failure: CopyFailure, message: impl Into<String>) -> Self {
        Self {
            failure,
            message: message.into(),
        }
    }

    fn cancelled() -> Self {
        Self::new(CopyFailure::Cancelled, "the copy was stopped")
    }

    /// A read of the source (or its open) that failed.
    fn reading(e: &io::Error) -> Self {
        let failure = match e.kind() {
            io::ErrorKind::NotFound => CopyFailure::Missing,
            _ => CopyFailure::Unreadable,
        };
        Self::new(failure, format!("could not read the file: {e}"))
    }

    /// A write into the show folder that failed.
    fn writing(e: &io::Error) -> Self {
        let failure = match e.kind() {
            io::ErrorKind::StorageFull
            | io::ErrorKind::QuotaExceeded
            | io::ErrorKind::FileTooLarge => CopyFailure::DiskFull,
            io::ErrorKind::ReadOnlyFilesystem | io::ErrorKind::PermissionDenied => {
                CopyFailure::ReadOnly
            }
            _ => CopyFailure::Other,
        };
        Self::new(
            failure,
            format!("could not write into the show folder: {e}"),
        )
    }
}

/// Where a copy went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Copied {
    /// A new file was written at this path.
    New(PathBuf),
    /// The folder already held a file with the same content, at this path.
    Reused(PathBuf),
}

/// A test's hold before a file is copied.
#[cfg(any(test, feature = "test-support"))]
pub type BeforeFileHook = std::sync::Arc<dyn Fn(&Path) + Send + Sync>;
/// A test's fault for a chunk's write.
#[cfg(any(test, feature = "test-support"))]
pub type WriteFaultHook = std::sync::Arc<dyn Fn(&Path) -> Option<io::Error> + Send + Sync>;

/// Test support (feature `test-support`, never in the shipping engine): faults and holds at
/// the points a test needs.
#[derive(Clone, Default)]
pub struct CopyHooks {
    /// Called on the copy's thread before each file (a test holds it there).
    #[cfg(any(test, feature = "test-support"))]
    pub before_file: Option<BeforeFileHook>,
    /// Called before each chunk is written to `dest`: an error is that write's error (a disk
    /// filling up).
    #[cfg(any(test, feature = "test-support"))]
    pub write_fault: Option<WriteFaultHook>,
}

impl std::fmt::Debug for CopyHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CopyHooks")
    }
}

impl CopyHooks {
    fn before_file(&self, _path: &Path) {
        #[cfg(any(test, feature = "test-support"))]
        if let Some(hook) = &self.before_file {
            hook(_path);
        }
    }

    fn write_fault(&self, _dest: &Path) -> Option<io::Error> {
        #[cfg(any(test, feature = "test-support"))]
        if let Some(hook) = &self.write_fault {
            return hook(_dest);
        }
        None
    }
}

/// One destination folder's regular files and their sizes, read once per copy job and kept up
/// to date with the copies it makes, so finding an identical file costs no walk per file.
#[derive(Debug, Default)]
pub struct FolderIndex {
    dir: PathBuf,
    entries: Vec<(String, u64)>,
}

impl FolderIndex {
    /// The files directly in `dir` (an absent folder is empty). Temp files of copies under way or
    /// left by a crash, symlinks, folders and names that are not UTF-8 are left out.
    fn read(dir: &Path) -> io::Result<Self> {
        let mut entries = Vec::new();
        let listing = match fs::read_dir(dir) {
            Ok(listing) => listing,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Self {
                    dir: dir.to_path_buf(),
                    entries,
                });
            }
            Err(e) => return Err(e),
        };
        for entry in listing.take(MAX_INDEX_ENTRIES) {
            let Ok(entry) = entry else { continue };
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if !kind.is_file() {
                continue;
            }
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if let Some((_, pid)) = split_tmp_name(&name) {
                // A copy this engine left when it crashed: never finished, never pointed at, and
                // possibly gigabytes. One of this process's own is a copy under way (another doc's
                // could share the folder) is left alone, and so is another process's written to within
                // `STALE_TMP_AGE` (another instance may be copying). Best effort.
                if pid != std::process::id() && is_stale(&entry) {
                    let _ = fs::remove_file(entry.path());
                }
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                entries.push((name, meta.len()));
            }
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            entries,
        })
    }
}

/// Whether a temp file was last written more than [`STALE_TMP_AGE`] ago (as the show-file
/// cleanup judges it; an unreadable time counts as fresh).
fn is_stale(entry: &fs::DirEntry) -> bool {
    entry
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| std::time::SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age >= STALE_TMP_AGE)
}

/// The indexes of the destination folders one job writes into.
#[derive(Debug, Default)]
pub struct FolderIndexes {
    folders: Vec<FolderIndex>,
}

impl FolderIndexes {
    fn get(&mut self, dir: &Path) -> io::Result<&mut FolderIndex> {
        if !self.folders.iter().any(|f| f.dir == dir) {
            let index = FolderIndex::read(dir)?;
            self.folders.push(index);
        }
        self.folders
            .iter_mut()
            .find(|f| f.dir == dir)
            .ok_or_else(|| io::Error::other("folder index lost"))
    }
}

/// Everything one copy job works with.
pub struct CopyWork<'a> {
    pub progress: &'a CopyProgress,
    pub pacing: CopyPacing,
    pub hooks: &'a CopyHooks,
    /// The job's one buffer, `pacing.chunk` long.
    pub buf: &'a mut Vec<u8>,
    pub indexes: &'a mut FolderIndexes,
}

/// Copies `source` into `show_dir/folder`, or finds it there already.
///
/// `always_new`: a new numbered copy is made even if the folder holds the same content (the
/// operator set a file already inside the show folder as a target; QLab makes a second copy).
/// Otherwise a file there with the same size and bytes is reused and nothing is written.
/// A new copy takes the source's name, numbered (`kick 2.wav`) if that name is taken, by a
/// rename that never replaces a file. On any failure nothing new is left in the folder.
pub fn copy_into(
    source: &Path,
    show_dir: &Path,
    folder: &str,
    always_new: bool,
    work: &mut CopyWork<'_>,
) -> Result<Copied, CopyError> {
    work.hooks.before_file(source);
    if work.progress.cancelled() {
        return Err(CopyError::cancelled());
    }
    let name = source
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| copy_name_ok(n))
        .ok_or_else(|| {
            CopyError::new(
                CopyFailure::BadName,
                "the file's name cannot be used in the show folder",
            )
        })?
        .to_owned();
    if !copy_name_ok(folder) {
        return Err(CopyError::new(CopyFailure::Other, "bad destination folder"));
    }
    let mut file = openlab_audio::open_regular(source).map_err(|e| CopyError::reading(&e))?;
    let size = file.metadata().map_err(|e| CopyError::reading(&e))?.len();
    let dest_dir = show_dir.join(folder);
    ensure_folder(&dest_dir)?;
    let index = work
        .indexes
        .get(&dest_dir)
        .map_err(|e| CopyError::writing(&e))?;
    if !always_new {
        let candidates: Vec<String> = index
            .entries
            .iter()
            .filter(|(_, s)| *s == size)
            .map(|(n, _)| n.clone())
            .collect();
        for candidate in candidates {
            let path = dest_dir.join(&candidate);
            if same_content(&mut file, &path, work)? {
                work.progress.add_bytes(size);
                return Ok(Copied::Reused(path));
            }
            file.rewind().map_err(|e| CopyError::reading(&e))?;
        }
    }
    let copied = write_copy(&mut file, size, &dest_dir, &name, work)?;
    if let Ok(index) = work.indexes.get(&dest_dir)
        && let Some(n) = copied.file_name().and_then(|n| n.to_str())
    {
        index.entries.push((n.to_owned(), size));
    }
    Ok(Copied::New(copied))
}

/// `dir` exists as a real folder (made if absent). A symlink or file in its place is refused:
/// a copy must land inside the show folder, never through a link to somewhere else.
fn ensure_folder(dir: &Path) -> Result<(), CopyError> {
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(CopyError::new(
            CopyFailure::Other,
            format!(
                "{} in the show folder is not a folder",
                dir.file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
            ),
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => match fs::create_dir(dir) {
            Ok(()) => {
                sync_parent(dir);
                Ok(())
            }
            // Another copy made it meanwhile.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
            Err(e) => Err(CopyError::writing(&e)),
        },
        Err(e) => Err(CopyError::writing(&e)),
    }
}

/// Whether `candidate` holds exactly the bytes `file` holds from its current place (the two are
/// known to be the same size). Stops at the first difference.
fn same_content(
    file: &mut File,
    candidate: &Path,
    work: &mut CopyWork<'_>,
) -> Result<bool, CopyError> {
    // A candidate that cannot be read is simply not the same file.
    let Ok(mut other) = openlab_audio::open_regular(candidate) else {
        return Ok(false);
    };
    let half = (work.pacing.chunk / 2).max(1);
    work.buf.resize(half * 2, 0);
    let (a, b) = work.buf.split_at_mut(half);
    loop {
        if work.progress.cancelled() {
            return Err(CopyError::cancelled());
        }
        let n = read_full(file, a).map_err(|e| CopyError::reading(&e))?;
        let Ok(m) = read_full(&mut other, b) else {
            return Ok(false);
        };
        work.progress.beat();
        // Both read to full chunks until their ends: a different count is a different length.
        if n != m || a.get(..n) != b.get(..m) {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
        pause(work.pacing);
    }
}

/// Reads until `buf` is full or the file ends; the count read.
fn read_full(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut read = 0;
    while read < buf.len() {
        let Some(rest) = buf.get_mut(read..) else {
            break;
        };
        match file.read(rest) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(read)
}

fn pause(pacing: CopyPacing) {
    if !pacing.pause.is_zero() {
        std::thread::sleep(pacing.pause);
    }
}

/// Writes `file`'s bytes to a temp file in `dest_dir`, syncs it, and renames it to `name` or the
/// first free numbered name. The temp file is removed on every failure.
fn write_copy(
    file: &mut File,
    size: u64,
    dest_dir: &Path,
    name: &str,
    work: &mut CopyWork<'_>,
) -> Result<PathBuf, CopyError> {
    let first = dest_dir.join(name);
    let (tmp, mut out) = create_tmp_beside(&first).map_err(|e| CopyError::writing(&e))?;
    let result = fill(file, size, &mut out, &tmp, work);
    drop(out);
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    for n in 1..=MAX_COPY_NUMBER {
        let candidate = if n == 1 {
            first.clone()
        } else {
            dest_dir.join(numbered_name(name, n))
        };
        match rename_no_replace(&tmp, &candidate) {
            Ok(()) => {
                sync_parent(&candidate);
                return Ok(candidate);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                return Err(CopyError::writing(&e));
            }
        }
    }
    let _ = fs::remove_file(&tmp);
    Err(CopyError::new(
        CopyFailure::Other,
        format!("every numbered name up to {MAX_COPY_NUMBER} is taken"),
    ))
}

fn fill(
    file: &mut File,
    size: u64,
    out: &mut File,
    tmp: &Path,
    work: &mut CopyWork<'_>,
) -> Result<(), CopyError> {
    work.buf.resize(work.pacing.chunk.max(1), 0);
    let mut written: u64 = 0;
    let mut unsynced: u64 = 0;
    loop {
        if work.progress.cancelled() {
            return Err(CopyError::cancelled());
        }
        let n = read_full(file, work.buf).map_err(|e| CopyError::reading(&e))?;
        if n == 0 {
            break;
        }
        if let Some(e) = work.hooks.write_fault(tmp) {
            return Err(CopyError::writing(&e));
        }
        let chunk = work.buf.get(..n).unwrap_or_default();
        out.write_all(chunk).map_err(|e| CopyError::writing(&e))?;
        written += n as u64;
        unsynced += n as u64;
        work.progress.add_bytes(n as u64);
        if unsynced >= SYNC_EVERY {
            out.sync_data().map_err(|e| CopyError::writing(&e))?;
            unsynced = 0;
            work.progress.beat();
        }
        pause(work.pacing);
    }
    // A file written to while it was copied gives a copy of neither version.
    if written != size {
        return Err(CopyError::new(
            CopyFailure::Unreadable,
            "the file changed while it was being copied",
        ));
    }
    out.sync_all().map_err(|e| CopyError::writing(&e))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct Rig {
        _tmp: tempfile::TempDir,
        lib: PathBuf,
        show: PathBuf,
        progress: CopyProgress,
        hooks: CopyHooks,
        buf: Vec<u8>,
        indexes: FolderIndexes,
    }

    fn rig() -> Rig {
        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path().join("lib");
        let show = tmp.path().join("show");
        fs::create_dir_all(&lib).unwrap();
        fs::create_dir_all(&show).unwrap();
        Rig {
            _tmp: tmp,
            lib,
            show,
            progress: CopyProgress::default(),
            hooks: CopyHooks::default(),
            buf: Vec::new(),
            indexes: FolderIndexes::default(),
        }
    }

    impl Rig {
        fn file(&self, rel: &str, bytes: &[u8]) -> PathBuf {
            let path = self.lib.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, bytes).unwrap();
            path
        }

        fn copy(&mut self, source: &Path, always_new: bool) -> Result<Copied, CopyError> {
            let mut work = CopyWork {
                progress: &self.progress,
                // A small chunk so a few bytes take several steps.
                pacing: CopyPacing {
                    chunk: 4,
                    pause: Duration::ZERO,
                },
                hooks: &self.hooks,
                buf: &mut self.buf,
                indexes: &mut self.indexes,
            };
            copy_into(source, &self.show, "audio", always_new, &mut work)
        }

        fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(self.show.join("audio"))
                .map(|d| {
                    d.map(|e| e.unwrap().file_name().into_string().unwrap())
                        .collect()
                })
                .unwrap_or_default();
            names.sort();
            names
        }
    }

    #[test]
    fn a_file_is_copied_whole_under_its_own_name_and_the_original_is_untouched() {
        let mut r = rig();
        let src = r.file("kick.wav", b"0123456789");
        let Copied::New(dest) = r.copy(&src, false).unwrap() else {
            panic!("not a new copy")
        };
        assert_eq!(dest, r.show.join("audio").join("kick.wav"));
        assert_eq!(fs::read(&dest).unwrap(), b"0123456789");
        assert_eq!(fs::read(&src).unwrap(), b"0123456789");
        assert_eq!(r.progress.bytes_done(), 10);
        assert_eq!(r.names(), ["kick.wav"]);
    }

    #[test]
    fn identical_content_under_any_name_is_reused_and_nothing_is_written() {
        let mut r = rig();
        let first = r.file("a/kick.wav", b"same bytes");
        let other_name = r.file("b/snare.wav", b"same bytes");
        r.copy(&first, false).unwrap();
        assert_eq!(
            r.copy(&other_name, false).unwrap(),
            Copied::Reused(r.show.join("audio").join("kick.wav"))
        );
        assert_eq!(r.names(), ["kick.wav"]);
    }

    #[test]
    fn a_different_file_with_a_taken_name_is_numbered_and_nothing_is_replaced() {
        let mut r = rig();
        let a = r.file("a/kick.wav", b"first take");
        let b = r.file("b/kick.wav", b"second tak");
        let c = r.file("c/kick.wav", b"third");
        r.copy(&a, false).unwrap();
        assert_eq!(
            r.copy(&b, false).unwrap(),
            Copied::New(r.show.join("audio").join("kick 2.wav"))
        );
        assert_eq!(
            r.copy(&c, false).unwrap(),
            Copied::New(r.show.join("audio").join("kick 3.wav"))
        );
        let audio = r.show.join("audio");
        assert_eq!(fs::read(audio.join("kick.wav")).unwrap(), b"first take");
        assert_eq!(fs::read(audio.join("kick 2.wav")).unwrap(), b"second tak");
        // A longer file that starts the same is not the same file.
        let longer = r.file("d/kick.wav", b"first take, longer");
        assert!(matches!(r.copy(&longer, false).unwrap(), Copied::New(_)));
    }

    #[test]
    fn a_file_already_inside_set_again_gets_a_numbered_second_copy() {
        let mut r = rig();
        let inside = r.show.join("audio").join("kick.wav");
        fs::create_dir_all(inside.parent().unwrap()).unwrap();
        fs::write(&inside, b"bytes").unwrap();
        assert_eq!(
            r.copy(&inside, true).unwrap(),
            Copied::New(r.show.join("audio").join("kick 2.wav"))
        );
        assert_eq!(
            r.copy(&inside, true).unwrap(),
            Copied::New(r.show.join("audio").join("kick 3.wav"))
        );
    }

    #[test]
    fn a_missing_file_fails_as_missing_and_leaves_nothing() {
        let mut r = rig();
        let gone = r.lib.join("gone.wav");
        let e = r.copy(&gone, false).unwrap_err();
        assert_eq!(e.failure, CopyFailure::Missing);
        assert!(r.names().is_empty());
    }

    #[test]
    fn a_folder_is_not_a_file_to_copy() {
        let mut r = rig();
        let dir = r.lib.join("folder.wav");
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            r.copy(&dir, false).unwrap_err().failure,
            CopyFailure::Unreadable
        );
    }

    #[test]
    fn a_full_disk_part_way_leaves_no_half_written_file() {
        let mut r = rig();
        let src = r.file("long.wav", b"0123456789abcdef");
        let writes = Arc::new(AtomicU64::new(0));
        let seen = Arc::clone(&writes);
        r.hooks.write_fault = Some(Arc::new(move |_| {
            (seen.fetch_add(1, Ordering::SeqCst) == 2)
                .then(|| io::Error::from(io::ErrorKind::StorageFull))
        }));
        let e = r.copy(&src, false).unwrap_err();
        assert_eq!(e.failure, CopyFailure::DiskFull);
        assert!(r.names().is_empty(), "{:?}", r.names());
    }

    #[test]
    fn a_cancelled_copy_leaves_no_file() {
        let mut r = rig();
        let src = r.file("long.wav", b"0123456789abcdef");
        let progress = Arc::new(CopyProgress::default());
        let p = Arc::clone(&progress);
        r.hooks.write_fault = Some(Arc::new(move |_| {
            p.cancel();
            None
        }));
        let mut work = CopyWork {
            progress: &progress,
            pacing: CopyPacing {
                chunk: 4,
                pause: Duration::ZERO,
            },
            hooks: &r.hooks,
            buf: &mut r.buf,
            indexes: &mut r.indexes,
        };
        let e = copy_into(&src, &r.show, "audio", false, &mut work).unwrap_err();
        assert_eq!(e.failure, CopyFailure::Cancelled);
        assert!(r.names().is_empty());
    }

    #[test]
    fn a_link_in_place_of_the_media_folder_is_refused() {
        let mut r = rig();
        let src = r.file("kick.wav", b"x");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&r.lib, r.show.join("audio")).unwrap();
        #[cfg(not(unix))]
        fs::write(r.show.join("audio"), b"a file, not a folder").unwrap();
        let e = r.copy(&src, false).unwrap_err();
        assert_eq!(e.failure, CopyFailure::Other);
        assert_eq!(
            fs::read_dir(&r.lib).unwrap().count(),
            1,
            "nothing written through the link"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_show_folder_fails_as_read_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let mut r = rig();
        let src = r.file("kick.wav", b"x");
        fs::set_permissions(&r.show, fs::Permissions::from_mode(0o555)).unwrap();
        let e = r.copy(&src, false);
        fs::set_permissions(&r.show, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(e.unwrap_err().failure, CopyFailure::ReadOnly);
    }

    /// Review #779 L1: a copy left by a crashed engine (another pid) goes when the folder is next
    /// read; one of this process's own (a copy under way) stays.
    #[test]
    fn a_crashed_copy_s_temp_file_is_removed_and_a_live_one_is_kept() {
        let mut r = rig();
        let audio = r.show.join("audio");
        fs::create_dir_all(&audio).unwrap();
        let other = std::process::id().wrapping_add(1);
        let crashed = audio.join(format!("big.wav.{other}-3.tmp"));
        let live = audio.join(format!("big.wav.{}-9.tmp", std::process::id()));
        let fresh = audio.join(format!("big.wav.{other}-4.tmp"));
        fs::write(&crashed, b"half").unwrap();
        fs::write(&live, b"half").unwrap();
        fs::write(&fresh, b"half").unwrap();
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(86_400);
        fs::File::options()
            .write(true)
            .open(&crashed)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let src = r.file("kick.wav", b"x");
        r.copy(&src, false).unwrap();
        assert!(!crashed.exists(), "the crashed copy is gone");
        assert!(live.exists(), "a copy under way is left alone");
        assert!(
            fresh.exists(),
            "another process's fresh temp file is left alone"
        );
    }

    #[test]
    fn temp_files_in_the_folder_are_never_taken_for_an_identical_copy() {
        let mut r = rig();
        let audio = r.show.join("audio");
        fs::create_dir_all(&audio).unwrap();
        fs::write(audio.join("kick.wav.123-4.tmp"), b"bytes").unwrap();
        let src = r.file("kick.wav", b"bytes");
        assert_eq!(
            r.copy(&src, false).unwrap(),
            Copied::New(audio.join("kick.wav"))
        );
    }
}
