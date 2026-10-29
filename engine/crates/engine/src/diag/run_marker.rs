// SPDX-License-Identifier: GPL-3.0-or-later

//! Unclean exits (DX-04, M4.7b): a "running" marker file per engine process, removed on a
//! graceful shutdown. The next start turns a leftover marker into a show-log record, so a
//! crash the panic hook could not see (an abort, SIGSEGV, SIGKILL, an out-of-memory kill, a
//! power cut) still leaves a trace. Decision record: docs/handoff/decisions/m4-7b.md.
//!
//! Each running engine holds an exclusive lock on its own marker (`File::try_lock`); the OS
//! drops the lock when the process ends, however it ends. A marker that can be locked
//! therefore belongs to a run that is over, and one that cannot belongs to an engine that is
//! still running (a second instance), which is left alone. No signal handler is involved.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use openlab_core::protocol::{LogEntry, LogKind, LogLevel};

use super::{ShowLog, time};
use crate::persist;

/// Marker files are `running-<pid>.json`.
const PREFIX: &str = "running-";
const SUFFIX: &str = ".json";
/// A marker is a few dozen bytes; more is not ours and is not read.
const MAX_MARKER_BYTES: u64 = 4096;
/// Leftover markers handled per start; the rest wait for the next (a folder full of them is
/// not ours to clean up in one go, and a start must not take long).
const MAX_LEFTOVERS: usize = 32;
/// Directory entries looked at, in the marker folder and in the crash folder.
const MAX_SCAN: usize = 10_000;
/// Crash reports named per leftover run.
const MAX_REPORTS_NAMED: usize = 3;

/// What a marker says about its run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunInfo {
    pub pid: u32,
    /// Wall clock, milliseconds since the Unix epoch (UTC).
    pub started_ms: i64,
    pub version: String,
}

impl RunInfo {
    /// The marker's text: `{"pid":..,"started_ms":..,"version":".."}`.
    fn to_json(&self) -> String {
        serde_json::json!({
            "pid": self.pid,
            "started_ms": self.started_ms,
            "version": self.version,
        })
        .to_string()
    }

    /// A marker's text read back; `None` unless every field is there with its type.
    fn from_json(text: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        Some(Self {
            pid: u32::try_from(v.get("pid")?.as_u64()?).ok()?,
            started_ms: v.get("started_ms")?.as_i64()?,
            version: v.get("version")?.as_str()?.chars().take(64).collect(),
        })
    }
}

/// A run that ended without a clean shutdown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviousRun {
    /// `None` when the marker was empty or unreadable (a power cut while it was written).
    pub info: Option<RunInfo>,
    /// Crash reports (file names in the crash folder) that run's panic hook wrote.
    pub crash_reports: Vec<String>,
}

impl PreviousRun {
    /// The show-log message.
    pub fn message(&self) -> String {
        let mut text =
            "previous run ended without a clean shutdown (crash, kill or power loss)".to_owned();
        match &self.info {
            Some(i) => text.push_str(&format!(
                ": openLab engine {}, pid {}, started {}",
                i.version,
                i.pid,
                time::iso(i.started_ms)
            )),
            None => text.push_str(": its details were not readable"),
        }
        if self.crash_reports.is_empty() {
            text.push_str("; no crash report was written");
        } else {
            text.push_str(&format!(
                "; crash report(s) in the Crashes folder: {}",
                self.crash_reports.join(", ")
            ));
        }
        text
    }
}

/// This run's marker. [`RunMarker::finish`] on a graceful shutdown; dropping it without that
/// (or never getting there) leaves the marker for the next start to find.
#[derive(Debug)]
pub struct RunMarker {
    path: PathBuf,
    /// Holds the lock while the process lives.
    file: File,
}

impl RunMarker {
    /// Reports the runs whose markers are left in `dir` (oldest marker files first, at most
    /// [`MAX_LEFTOVERS`]), removes those markers, then writes and locks this run's. `crash_dir`
    /// is searched for reports the earlier runs wrote. The marker is `None` when it could not be
    /// written (the leftovers are still reported): the show goes on without one.
    pub fn start(
        dir: &Path,
        crash_dir: &Path,
        me: &RunInfo,
    ) -> (Option<RunMarker>, Vec<PreviousRun>) {
        let leftovers = take_leftovers(dir)
            .into_iter()
            .map(|info| PreviousRun {
                crash_reports: info
                    .as_ref()
                    .map(|i| crash_reports_of(crash_dir, i))
                    .unwrap_or_default(),
                info,
            })
            .collect();
        (create(dir, me).ok(), leftovers)
    }

    /// A graceful shutdown: the marker goes.
    pub fn finish(self) -> io::Result<()> {
        let RunMarker { path, file } = self;
        // Closed first: Windows refuses to remove an open, locked file.
        drop(file);
        fs::remove_file(path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// The host's start: [`RunMarker::start`], with one `warn` show-log record (kind `engine`) per
/// run that ended without a clean shutdown. The show log is the engine's, not a workspace's, so
/// a UI that opens its log window (`log_tail`) sees it whichever workspace it opens.
pub fn begin(
    dir: &Path,
    crash_dir: &Path,
    me: &RunInfo,
    log: &ShowLog,
    now_ms: i64,
) -> (Option<RunMarker>, Vec<PreviousRun>) {
    let (marker, previous) = RunMarker::start(dir, crash_dir, me);
    for run in &previous {
        log.record(LogEntry {
            time_ms: now_ms,
            show_time: None,
            level: LogLevel::Warn,
            kind: LogKind::Engine,
            message: run.message(),
            cue: None,
            source: None,
        });
    }
    (marker, previous)
}

/// Whether the engine with process id `pid` is running: its marker in `dir` is held by a live
/// process (M4.9, `engine.json`'s single-instance check). `false` for this process's own pid (a
/// file left by an earlier run whose pid this one reuses), for no marker, and where locks are
/// unsupported (as for leftovers: such a run cannot be told from a live one, and a start must
/// not be refused for ever).
pub fn is_live(dir: &Path, pid: u32) -> bool {
    if pid == std::process::id() {
        return false;
    }
    let path = dir.join(marker_name(pid));
    // A FIFO named like a marker must not hang the start (as in `take_one`).
    if !fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
        return false;
    }
    let Ok(file) = OpenOptions::new().read(true).write(true).open(&path) else {
        return false;
    };
    matches!(file.try_lock(), Err(TryLockError::WouldBlock))
}

fn marker_name(pid: u32) -> String {
    format!("{PREFIX}{pid}{SUFFIX}")
}

/// Written and locked under a temporary name (not a marker name), then renamed into place:
/// another engine starting at the same moment never sees this marker empty or unlocked. The
/// lock belongs to the open file, so it survives the rename.
fn create(dir: &Path, me: &RunInfo) -> io::Result<RunMarker> {
    fs::create_dir_all(dir)?;
    let path = dir.join(marker_name(me.pid));
    // Without the lock a second instance would take this run for a dead one; the marker still
    // does its main job, so a filesystem without locks is not an error.
    let file = persist::write_engine_file(&path, me.to_json().as_bytes(), |file| {
        let _ = file.try_lock();
    })
    .map_err(io::Error::other)?;
    Ok(RunMarker { path, file })
}

/// Leftover markers in `dir`: read, then removed. A marker some live process still holds is
/// skipped.
fn take_leftovers(dir: &Path) -> Vec<Option<RunInfo>> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut markers: Vec<(Option<std::time::SystemTime>, PathBuf)> = entries
        .take(MAX_SCAN)
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with(PREFIX) && n.ends_with(SUFFIX))
        })
        .map(|e| (e.metadata().and_then(|m| m.modified()).ok(), e.path()))
        .collect();
    markers.sort();
    let mut found = Vec::new();
    for (_, path) in markers {
        if found.len() >= MAX_LEFTOVERS {
            break;
        }
        if let Some(info) = take_one(&path) {
            found.push(info);
        }
    }
    found
}

/// `Some(details)` for a marker whose run is over (removed here), `None` for a live one or one
/// that cannot be opened.
fn take_one(path: &Path) -> Option<Option<RunInfo>> {
    // Only a regular file is a marker: reading a FIFO (or a device) named like one would hang
    // the start. Checked before opening (opening a FIFO can block) and again on the opened file.
    if !fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    let mut file = OpenOptions::new().read(true).write(true).open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    match file.try_lock() {
        Ok(()) => {}
        // Another engine is running with it.
        Err(TryLockError::WouldBlock) => return None,
        // Locks unsupported here: nobody can hold one, so the run cannot be told apart from a
        // live one; reporting it is the side that never misses a crash.
        Err(TryLockError::Error(_)) => {}
    }
    let mut text = String::new();
    let read = (&mut file)
        .take(MAX_MARKER_BYTES)
        .read_to_string(&mut text)
        .is_ok();
    let info = read.then(|| RunInfo::from_json(&text)).flatten();
    drop(file);
    // A marker that cannot be removed would be reported at every start; still report it now.
    let _ = fs::remove_file(path);
    Some(info)
}

/// Crash reports the run `info` wrote: `crash-<UTC>-<pid>[-n].txt`, not older than its start
/// (a pid is reused), at most [`MAX_REPORTS_NAMED`], oldest first.
fn crash_reports_of(crash_dir: &Path, info: &RunInfo) -> Vec<String> {
    let Ok(entries) = fs::read_dir(crash_dir) else {
        return Vec::new();
    };
    // Second resolution in the name: a report in the start's own second counts.
    let since = time::compact(info.started_ms.div_euclid(1000).saturating_mul(1000));
    let pid = info.pid.to_string();
    let mut names: Vec<String> = entries
        .take(MAX_SCAN)
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| {
            let Some(rest) = name
                .strip_prefix("crash-")
                .and_then(|r| r.strip_suffix(".txt"))
            else {
                return false;
            };
            let mut parts = rest.split('-');
            let (Some(stamp), Some(p)) = (parts.next(), parts.next()) else {
                return false;
            };
            p == pid && stamp.len() == since.len() && stamp >= since.as_str()
        })
        .collect();
    names.sort();
    names.truncate(MAX_REPORTS_NAMED);
    names
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Tests that drop a marker or the instance lock and then expect its lock to be free, and
    /// every test in this crate that spawns a child process (`mkfifo`, `open`), hold this. A child created by `fork`/`posix_spawn` shares every open file
    /// description of the process until its `exec` closes the close-on-exec ones, so a spawn
    /// that overlaps a dropped marker keeps the `flock` held for a moment and the next start sees
    /// `WouldBlock` (seen on CI macOS arm64; ~1% of overlaps when reproduced). The engine itself
    /// spawns no child while it relies on a lock being released (its only child, the app relaunch,
    /// runs while this run's marker is held anyway), so this is only a test-process hazard.
    pub(crate) fn serialize_spawn_and_lock_release() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        crate::lock(&LOCK)
    }

    const T0: i64 = 1_700_000_000_000;

    fn info(pid: u32, started_ms: i64) -> RunInfo {
        RunInfo {
            pid,
            started_ms,
            version: "0.0.1".into(),
        }
    }

    fn markers(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .map(|r| {
                r.filter_map(Result::ok)
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    #[test]
    fn a_clean_shutdown_leaves_nothing_for_the_next_start() {
        let root = tempfile::tempdir().unwrap();
        let (dir, crashes) = (root.path().join("Running"), root.path().join("Crashes"));
        let (marker, previous) = RunMarker::start(&dir, &crashes, &info(10, T0));
        assert!(previous.is_empty(), "a first run finds nothing");
        let marker = marker.expect("written");
        assert_eq!(markers(&dir), ["running-10.json"]);
        marker.finish().unwrap();
        assert!(markers(&dir).is_empty());
        let (marker, previous) = RunMarker::start(&dir, &crashes, &info(11, T0 + 5));
        assert!(previous.is_empty());
        marker.unwrap().finish().unwrap();
    }

    #[test]
    fn a_run_that_ended_without_finishing_is_reported_once_with_its_pid_and_start() {
        let _serial = serialize_spawn_and_lock_release();
        let root = tempfile::tempdir().unwrap();
        let (dir, crashes) = (root.path().join("Running"), root.path().join("Crashes"));
        let (marker, _) = RunMarker::start(&dir, &crashes, &info(4242, T0));
        // A kill or a crash: the process ends, its file handle (and lock) with it.
        drop(marker);
        let (next, previous) = RunMarker::start(&dir, &crashes, &info(4243, T0 + 60_000));
        assert_eq!(
            previous,
            [PreviousRun {
                info: Some(info(4242, T0)),
                crash_reports: vec![],
            }]
        );
        let text = previous[0].message();
        assert!(
            text.starts_with(
                "previous run ended without a clean shutdown (crash, kill or power loss)"
            ),
            "{text}"
        );
        assert!(text.contains("pid 4242"), "{text}");
        assert!(text.contains("started 2023-11-14T22:13:20.000Z"), "{text}");
        assert!(text.contains("no crash report"), "{text}");
        // Reported once: the leftover is gone, this run's marker is the only one.
        assert_eq!(markers(&dir), ["running-4243.json"]);
        next.unwrap().finish().unwrap();
        let (last, previous) = RunMarker::start(&dir, &crashes, &info(4244, T0 + 120_000));
        assert!(previous.is_empty());
        last.unwrap().finish().unwrap();
    }

    #[test]
    fn a_marker_another_live_engine_holds_is_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let (dir, crashes) = (root.path().join("Running"), root.path().join("Crashes"));
        // The lock is per open file, so a second open in this process stands for a second
        // engine.
        let (live, _) = RunMarker::start(&dir, &crashes, &info(1, T0));
        let live = live.unwrap();
        let (other, previous) = RunMarker::start(&dir, &crashes, &info(2, T0 + 1));
        assert!(previous.is_empty(), "{previous:?}");
        assert_eq!(markers(&dir), ["running-1.json", "running-2.json"]);
        other.unwrap().finish().unwrap();
        live.finish().unwrap();
    }

    #[test]
    fn a_run_is_live_while_its_marker_is_held_and_not_after() {
        // Expects the dropped marker's lock to be free at once (see the helper).
        let _serial = serialize_spawn_and_lock_release();
        let root = tempfile::tempdir().unwrap();
        let (dir, crashes) = (root.path().join("Running"), root.path().join("Crashes"));
        assert!(!is_live(&dir, 1), "no marker");
        let (live, _) = RunMarker::start(&dir, &crashes, &info(1, T0));
        let live = live.unwrap();
        assert!(is_live(&dir, 1));
        assert!(
            !is_live(&dir, std::process::id()),
            "this process is never another engine"
        );
        // Ended without a clean shutdown: the file stays, unlocked.
        drop(live);
        assert!(!is_live(&dir, 1));
    }

    #[test]
    fn unreadable_markers_are_still_reported_and_other_files_ignored() {
        let root = tempfile::tempdir().unwrap();
        let (dir, crashes) = (root.path().join("Running"), root.path().join("Crashes"));
        fs::create_dir_all(&dir).unwrap();
        // A power cut while the marker was written, and a hostile oversized one.
        fs::write(dir.join("running-7.json"), "").unwrap();
        fs::write(dir.join("running-8.json"), "x".repeat(100_000)).unwrap();
        fs::write(dir.join("notes.txt"), "not a marker").unwrap();
        let (marker, previous) = RunMarker::start(&dir, &crashes, &info(9, T0));
        assert_eq!(previous.len(), 2);
        assert!(previous.iter().all(|p| p.info.is_none()));
        assert!(previous[0].message().contains("not readable"));
        assert_eq!(markers(&dir), ["notes.txt", "running-9.json"]);
        marker.unwrap().finish().unwrap();
    }

    #[test]
    fn crash_reports_of_that_run_are_named() {
        let _serial = serialize_spawn_and_lock_release();
        let root = tempfile::tempdir().unwrap();
        let (dir, crashes) = (root.path().join("Running"), root.path().join("Crashes"));
        fs::create_dir_all(&crashes).unwrap();
        let start = T0 + 500;
        let same_second = time::compact(T0);
        let later = time::compact(T0 + 30_000);
        let before = time::compact(T0 - 86_400_000);
        for name in [
            format!("crash-{same_second}-77.txt"),
            format!("crash-{later}-77-1.txt"),
            // An older run with the same pid, and another process.
            format!("crash-{before}-77.txt"),
            format!("crash-{later}-78.txt"),
            "crash-garbage.txt".to_owned(),
        ] {
            fs::write(crashes.join(name), "report").unwrap();
        }
        let (marker, _) = RunMarker::start(&dir, &crashes, &info(77, start));
        drop(marker);
        let (next, previous) = RunMarker::start(&dir, &crashes, &info(79, T0 + 90_000));
        let [run] = previous.try_into().unwrap();
        assert_eq!(
            run.crash_reports,
            [
                format!("crash-{same_second}-77.txt"),
                format!("crash-{later}-77-1.txt")
            ]
        );
        assert!(run.message().contains(&format!("crash-{later}-77-1.txt")));
        next.unwrap().finish().unwrap();
    }

    #[test]
    fn begin_records_each_unclean_exit_in_the_show_log() {
        let _serial = serialize_spawn_and_lock_release();
        let root = tempfile::tempdir().unwrap();
        let (dir, crashes) = (root.path().join("Running"), root.path().join("Crashes"));
        let log = ShowLog::new(None);
        let (first, _) = begin(&dir, &crashes, &info(5, T0), &log, T0);
        assert!(log.tail(10).is_empty());
        drop(first);
        let (second, previous) = begin(&dir, &crashes, &info(6, T0 + 9), &log, T0 + 9);
        assert_eq!(previous.len(), 1);
        let [entry] = log.tail(10).try_into().unwrap();
        assert_eq!(entry.level, LogLevel::Warn);
        assert_eq!(entry.kind, LogKind::Engine);
        assert_eq!(entry.time_ms, T0 + 9);
        assert_eq!(entry.show_time, None);
        assert_eq!(entry.message, previous[0].message());
        second.unwrap().finish().unwrap();
    }

    #[test]
    fn the_marker_appears_complete_under_its_name_and_no_temporary_file_is_left() {
        let root = tempfile::tempdir().unwrap();
        let (dir, crashes) = (root.path().join("Running"), root.path().join("Crashes"));
        fs::create_dir_all(&dir).unwrap();
        // A temporary file from a start that died while writing is not a marker, and is
        // cleaned up once it is clearly older than this write.
        let leftover = File::create(dir.join("running-3.json.99999-0.tmp")).unwrap();
        leftover
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(600))
            .unwrap();
        drop(leftover);
        let (marker, previous) = RunMarker::start(&dir, &crashes, &info(3, T0));
        assert!(previous.is_empty(), "{previous:?}");
        let marker = marker.unwrap();
        assert_eq!(markers(&dir), ["running-3.json"]);
        // Windows locks are mandatory, so only Unix can read the marker while it is held; the
        // contents are checked on unlocked leftovers elsewhere.
        if cfg!(unix) {
            let text = fs::read_to_string(marker.path()).unwrap();
            assert_eq!(RunInfo::from_json(&text), Some(info(3, T0)));
        }
        // Still locked after the rename: another start leaves it alone.
        let (other, previous) = RunMarker::start(&dir, &crashes, &info(4, T0 + 1));
        assert!(previous.is_empty(), "{previous:?}");
        other.unwrap().finish().unwrap();
        marker.finish().unwrap();
        assert!(markers(&dir).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_named_like_a_marker_does_not_hang_the_start() {
        let _serial = serialize_spawn_and_lock_release();
        let root = tempfile::tempdir().unwrap();
        let (dir, crashes) = (root.path().join("Running"), root.path().join("Crashes"));
        fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("running-12.json");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        let (marker, previous) = RunMarker::start(&dir, &crashes, &info(13, T0));
        assert!(previous.is_empty(), "{previous:?}");
        assert_eq!(markers(&dir), ["running-12.json", "running-13.json"]);
        marker.unwrap().finish().unwrap();
    }

    #[test]
    fn extreme_start_times_in_a_marker_do_not_panic() {
        let root = tempfile::tempdir().unwrap();
        let (dir, crashes) = (root.path().join("Running"), root.path().join("Crashes"));
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(&crashes).unwrap();
        fs::write(crashes.join("crash-20231114T221320Z-20.txt"), "report").unwrap();
        for (pid, started) in [(20, i64::MIN), (21, i64::MAX)] {
            fs::write(dir.join(marker_name(pid)), info(pid, started).to_json()).unwrap();
        }
        let (marker, previous) = RunMarker::start(&dir, &crashes, &info(22, T0));
        assert_eq!(previous.len(), 2, "{previous:?}");
        for run in &previous {
            let _ = run.message();
        }
        marker.unwrap().finish().unwrap();
    }

    #[test]
    fn an_unwritable_folder_gives_no_marker_and_no_failure() {
        let root = tempfile::tempdir().unwrap();
        // A file where the folder should be.
        let dir = root.path().join("Running");
        fs::write(&dir, "in the way").unwrap();
        let (marker, previous) = RunMarker::start(&dir, root.path(), &info(1, T0));
        assert!(marker.is_none());
        assert!(previous.is_empty());
    }
}
