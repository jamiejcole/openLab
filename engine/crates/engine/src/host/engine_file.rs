// SPDX-License-Identifier: GPL-3.0-or-later

//! `engine.json` (M4.9; ARCHITECTURE §6, §8): how a client finds the running engine.
//!
//! `<app-support>/openLab/engine.json` holds
//! `{"pid", "port", "token", "version", "protocol_version", "started_ms", "stopping"}`, readable
//! by the user only (mode 0600 on Unix: the token lets a local process drive the show). It is written
//! whole under a temporary name and renamed into place, so a reader never sees half a file,
//! and removed on a graceful exit only while it is still this run's. A file left by a run that
//! crashed is stale: its process holds no run marker any more ([`run_marker::is_live`]); a
//! start replaces it. A start refuses to run while the file names a live engine: one engine per
//! user (one device, one OSC port), and the app attaches to that one.
//!
//! `stopping` turns true when the engine's shutdown begins ([`EngineFile::mark_stopping`]): the
//! file stays until the very end (so no second engine is started into the lock meanwhile), and
//! a client that finds it stopping should wait for that pid to exit, then start or find the
//! next engine, instead of attaching to one about to exit. A file without the field (an older
//! engine's) is read as not stopping.

use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use crate::diag::run_marker;
use crate::persist;

/// The file is a few hundred bytes; more is not ours and is not read.
const MAX_BYTES: u64 = 4096;

/// What `engine.json` says.
#[derive(Clone, PartialEq, Eq)]
pub struct EngineInfo {
    pub pid: u32,
    /// The UI protocol's WebSocket port on `127.0.0.1`.
    pub port: u16,
    /// The session token a client sends in `hello`.
    pub token: String,
    pub version: String,
    pub protocol_version: u32,
    /// Wall clock, milliseconds since the Unix epoch (UTC).
    pub started_ms: i64,
    /// The engine's shutdown has begun (module docs).
    pub stopping: bool,
}

impl std::fmt::Debug for EngineInfo {
    // The token is a secret: never printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineInfo")
            .field("pid", &self.pid)
            .field("port", &self.port)
            .field("version", &self.version)
            .field("stopping", &self.stopping)
            .finish_non_exhaustive()
    }
}

impl EngineInfo {
    fn to_json(&self) -> String {
        serde_json::json!({
            "pid": self.pid,
            "port": self.port,
            "token": self.token,
            "version": self.version,
            "protocol_version": self.protocol_version,
            "started_ms": self.started_ms,
            "stopping": self.stopping,
        })
        .to_string()
    }

    /// `None` unless every field is there with its type (and a port that is not 0); `stopping`
    /// may be missing (false).
    fn from_json(text: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        let port = u16::try_from(v.get("port")?.as_u64()?).ok()?;
        (port != 0).then_some(())?;
        Some(Self {
            pid: u32::try_from(v.get("pid")?.as_u64()?).ok()?,
            port,
            token: v.get("token")?.as_str()?.to_owned(),
            version: v.get("version")?.as_str()?.chars().take(64).collect(),
            protocol_version: u32::try_from(v.get("protocol_version")?.as_u64()?).ok()?,
            started_ms: v.get("started_ms")?.as_i64()?,
            stopping: v
                .get("stopping")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        })
    }
}

/// Reads `path`; `None` when it is missing, not a regular file, too big or not an engine file.
pub fn read(path: &Path) -> Option<EngineInfo> {
    // Not a FIFO or a device: opening one could hang the start. The type is checked on the
    // opened handle, so one swapped in after a check of the path is refused too.
    let mut text = String::new();
    openlab_audio::open_regular(path)
        .ok()?
        .take(MAX_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    EngineInfo::from_json(&text)
}

/// The live engine `path` names, if any: one whose run marker in `markers` a live process
/// holds. Anything else there (no file, a damaged one, a crashed run's) is stale.
pub fn live_engine(path: &Path, markers: &Path) -> Option<EngineInfo> {
    read(path).filter(|info| run_marker::is_live(markers, info.pid))
}

/// This run's `engine.json`, written by [`EngineFile::write`]; [`EngineFile::remove`] on a
/// graceful exit.
#[derive(Debug)]
pub struct EngineFile {
    path: PathBuf,
    info: EngineInfo,
}

impl EngineFile {
    /// Writes `info` to `path` atomically, readable by the user only, replacing a stale file.
    pub fn write(path: &Path, info: EngineInfo) -> io::Result<Self> {
        let dir = path
            .parent()
            .ok_or_else(|| io::Error::other("engine.json has no folder"))?;
        fs::create_dir_all(dir)?;
        persist::write_engine_file(path, info.to_json().as_bytes(), |_| {})
            .map_err(io::Error::other)?;
        Ok(Self {
            path: path.to_path_buf(),
            info,
        })
    }

    /// Rewrites the file (atomically, as [`EngineFile::write`]) with `stopping: true`, if it is
    /// still this run's; another run's is left. Idempotent.
    pub fn mark_stopping(&mut self) -> io::Result<()> {
        if self.info.stopping || read(&self.path).is_none_or(|on_disk| on_disk != self.info) {
            return Ok(());
        }
        let info = EngineInfo {
            stopping: true,
            ..self.info.clone()
        };
        persist::write_engine_file(&self.path, info.to_json().as_bytes(), |_| {})
            .map_err(io::Error::other)?;
        self.info = info;
        Ok(())
    }

    /// Removes the file if it is still this run's (same pid and token); another run's is left.
    pub fn remove(self) -> io::Result<()> {
        if read(&self.path).is_some_and(|on_disk| on_disk == self.info) {
            fs::remove_file(&self.path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::run_marker::{RunInfo, RunMarker};

    fn info(pid: u32) -> EngineInfo {
        EngineInfo {
            pid,
            port: 50123,
            token: "ab".repeat(32),
            version: "0.0.1".into(),
            protocol_version: 2,
            started_ms: 1_700_000_000_000,
            stopping: false,
        }
    }

    #[test]
    fn written_whole_private_and_read_back() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("openLab").join("engine.json");
        let file = EngineFile::write(&path, info(42)).unwrap();
        assert_eq!(read(&path), Some(info(42)));
        let v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        for key in [
            "pid",
            "port",
            "token",
            "version",
            "protocol_version",
            "started_ms",
            "stopping",
        ] {
            assert!(v.get(key).is_some(), "{key}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["engine.json"], "no temporary file left");
        file.remove().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn a_stale_world_readable_file_is_replaced_with_a_private_one() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("engine.json");
        fs::write(&path, "{ half a file").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        }
        assert_eq!(read(&path), None);
        let _file = EngineFile::write(&path, info(7)).unwrap();
        assert_eq!(read(&path), Some(info(7)));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_crashed_runs_temp_file_is_removed_and_a_fresh_one_kept() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("engine.json");
        let (stale, fresh) = (
            root.path().join("engine.json.99999-0.tmp"),
            root.path().join("engine.json.99998-0.tmp"),
        );
        for name in [&stale, &fresh] {
            fs::write(name, "{").unwrap();
        }
        // Long ago by any clock: the temp-file sweep compares file ages with the current time.
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(600);
        fs::File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let _file = EngineFile::write(&path, info(3)).unwrap();
        assert!(!stale.exists());
        assert!(fresh.exists(), "may be another start's write in progress");
    }

    /// The shutdown marks the file stopping in place (the rest kept, still private, still this
    /// run's to remove); a file another run has written since is left alone. A file without the
    /// field (an older engine's) reads as not stopping.
    #[test]
    fn mark_stopping_rewrites_only_this_runs_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("engine.json");
        let mut mine = EngineFile::write(&path, info(1)).unwrap();
        mine.mark_stopping().unwrap();
        let stopping = EngineInfo {
            stopping: true,
            ..info(1)
        };
        assert_eq!(read(&path), Some(stopping.clone()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        mine.mark_stopping().unwrap();
        assert_eq!(read(&path), Some(stopping));
        mine.remove().unwrap();
        assert!(!path.exists(), "removed after the mark");

        let mut mine = EngineFile::write(&path, info(1)).unwrap();
        let _theirs = EngineFile::write(&path, info(2)).unwrap();
        mine.mark_stopping().unwrap();
        assert_eq!(read(&path), Some(info(2)));

        fs::write(
            &path,
            r#"{"pid":5,"port":6,"token":"t","version":"v","protocol_version":2,"started_ms":0}"#,
        )
        .unwrap();
        assert_eq!(read(&path).map(|i| i.stopping), Some(false));
    }

    #[test]
    fn remove_leaves_another_runs_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("engine.json");
        let mine = EngineFile::write(&path, info(1)).unwrap();
        let _theirs = EngineFile::write(&path, info(2)).unwrap();
        mine.remove().unwrap();
        assert_eq!(read(&path), Some(info(2)));
    }

    #[test]
    fn damaged_or_hostile_files_are_not_engines() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("engine.json");
        for bad in [
            "",
            "[]",
            r#"{"pid":1}"#,
            r#"{"pid":1,"port":0,"token":"t","version":"v","protocol_version":2,"started_ms":0}"#,
            r#"{"pid":1,"port":70000,"token":"t","version":"v","protocol_version":2,"started_ms":0}"#,
            r#"{"pid":-1,"port":5,"token":"t","version":"v","protocol_version":2,"started_ms":0}"#,
        ] {
            fs::write(&path, bad).unwrap();
            assert_eq!(read(&path), None, "{bad}");
        }
        let big = format!("{{\"pad\":\"{}\"}}", "x".repeat(10_000));
        fs::write(&path, big).unwrap();
        assert_eq!(read(&path), None);
        assert_eq!(read(&root.path().join("missing.json")), None);
        assert_eq!(read(root.path()), None, "a folder");
    }

    /// A FIFO at the path (also one swapped in after any look at it) is refused without
    /// waiting for a writer, so a start is not blocked.
    #[cfg(unix)]
    #[test]
    fn a_fifo_is_not_an_engine_and_is_not_waited_on() {
        let _serial = crate::diag::run_marker::tests::serialize_spawn_and_lock_release();
        let root = tempfile::tempdir().unwrap();
        let fifo = root.path().join("engine.json");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        let link = root.path().join("link.json");
        std::os::unix::fs::symlink(&fifo, &link).unwrap();
        for path in [fifo.clone(), link] {
            let (tx, rx) = std::sync::mpsc::channel();
            let reading = path.clone();
            std::thread::spawn(move || {
                let _ = tx.send(read(&reading));
            });
            // A hang guard, not a timing assertion.
            let out = rx.recv_timeout(std::time::Duration::from_secs(20));
            if out.is_err() {
                // Give the blocked open its writer so the thread can end.
                let _ = std::fs::OpenOptions::new().write(true).open(&fifo);
                panic!("reading a FIFO blocked: {}", path.display());
            }
            assert_eq!(out, Ok(None), "{}", path.display());
        }
    }

    #[test]
    fn only_a_file_whose_engine_holds_its_run_marker_is_live() {
        let root = tempfile::tempdir().unwrap();
        let (markers, crashes) = (root.path().join("Running"), root.path().join("Crashes"));
        let path = root.path().join("engine.json");
        let _file = EngineFile::write(&path, info(9)).unwrap();
        assert_eq!(live_engine(&path, &markers), None, "a crashed run's: stale");
        let me = RunInfo {
            pid: 9,
            started_ms: 0,
            version: "0.0.1".into(),
        };
        // A second open in this process stands for another engine (the lock is per open file).
        let (marker, _) = RunMarker::start(&markers, &crashes, &me);
        assert_eq!(live_engine(&path, &markers), Some(info(9)));
        marker.unwrap().finish().unwrap();
        assert_eq!(live_engine(&path, &markers), None);
    }
}
