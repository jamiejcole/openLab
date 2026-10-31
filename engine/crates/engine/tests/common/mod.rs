// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

//! Helpers for persistence tests.

use std::fs::{self, File};
use std::io::{BufRead as _, BufReader};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, UNIX_EPOCH};

use openlab_core::{MediaRef, Relink, Workspace, load_show, relink_command};
use openlab_engine::persist::AppDirs;
use tempfile::TempDir;

/// The v1 example show: has audio cues (one inside a group) and a workspace id.
pub fn example() -> Workspace {
    let text = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/show/v1-example.openlab"
    ))
    .unwrap();
    load_show(&text).unwrap().workspace
}

/// Points every audio cue (in cue-list order) at the matching path in `paths`.
pub fn set_media(ws: &mut Workspace, paths: &[&str]) {
    let cues: Vec<_> = openlab_core::audio_media(ws)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert!(cues.len() >= paths.len(), "fixture has too few audio cues");
    let relinks: Vec<_> = cues
        .iter()
        .zip(paths)
        .map(|(cue, path)| Relink {
            cue: *cue,
            new: MediaRef::new(*path),
        })
        .collect();
    let command = relink_command(ws, &relinks);
    ws.apply(command).unwrap();
}

pub struct Env {
    pub root: TempDir,
    pub dirs: AppDirs,
}

/// A temp app-support folder and an empty temp show folder inside the same temp dir.
pub fn env() -> Env {
    let root = tempfile::tempdir().unwrap();
    let dirs = AppDirs::new(root.path().join("support"));
    fs::create_dir_all(root.path().join("show")).unwrap();
    Env { root, dirs }
}

impl Env {
    pub fn show_dir(&self) -> std::path::PathBuf {
        self.root.path().join("show")
    }

    pub fn show_path(&self) -> std::path::PathBuf {
        self.show_dir().join("My Show.openlab")
    }
}

/// Sets a file's modification time to `ms` after the Unix epoch.
pub fn set_mtime(path: &Path, ms: u64) {
    let file = File::options().write(true).open(path).unwrap();
    file.set_modified(UNIX_EPOCH + Duration::from_millis(ms))
        .unwrap();
}

/// Sorted file names in `dir`; empty if it does not exist.
pub fn names(dir: &Path) -> Vec<String> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = read
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// Creates a one-byte file, and its folders.
pub fn touch(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"x").unwrap();
}

pub fn rights(view: bool, edit: bool, control: bool) -> openlab_core::OscRights {
    openlab_core::OscRights {
        view,
        edit,
        control,
    }
}

pub fn udp() -> openlab_engine::osc::Peer {
    openlab_engine::osc::Peer::Udp(std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 7)))
}

pub fn text(s: &str) -> rosc::OscType {
    rosc::OscType::String(s.into())
}

pub fn reload(path: &std::path::Path) -> openlab_core::Workspace {
    openlab_core::load_show(
        &openlab_engine::persist::read_text(path, openlab_core::MAX_SHOW_FILE_BYTES).unwrap(),
    )
    .unwrap()
    .workspace
}

pub fn renamed(name: &str) -> openlab_core::Workspace {
    let mut ws = example();
    ws.name = name.into();
    ws
}

/// The engine binary, built for the tests (with test-support).
pub const ENGINE: &str = env!("CARGO_BIN_EXE_openlab-engine");

/// A hang guard for waits on a spawned engine, not a timing assertion: generous for a slow
/// host, so an expected line that never comes fails the test instead of hanging it.
pub const HANG: Duration = Duration::from_secs(30);
/// Most stderr lines read while waiting for one thing; an engine that never stops logging fails.
pub const MAX_LINES: usize = 10_000;

/// A spawned engine whose stderr lines arrive on a channel (read on a thread, so a wait is
/// bounded), killed and reaped on drop: a failed assertion does not leave it running.
pub struct Engine {
    pub child: Child,
    lines: mpsc::Receiver<String>,
    pub seen: Vec<String>,
}

impl Engine {
    /// Spawns `command` with stdin and stderr piped.
    pub fn spawn(command: &mut Command) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            lines,
            seen: Vec::new(),
        }
    }

    /// Reads stderr lines until one contains `want`; `false` when stderr ends first. Panics
    /// if neither happens within [`HANG`].
    pub fn wait_for(&mut self, want: &str) -> bool {
        // Silence for `HANG` is a hang, and so is an engine that never stops logging.
        for _ in 0..MAX_LINES {
            match self.lines.recv_timeout(HANG) {
                Ok(line) => {
                    let hit = line.contains(want);
                    self.seen.push(line);
                    if hit {
                        return true;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return false,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("no {want:?} within {HANG:?}: {:?}", self.seen)
                }
            }
        }
        panic!("no {want:?} in {MAX_LINES} lines: {:?}", self.seen)
    }

    /// Asks the engine to end gracefully (SIGTERM on macOS; stdin closing with
    /// `--watch-stdin` elsewhere) and returns its exit status, reading stderr to its end.
    /// Panics if it is still running after [`HANG`].
    pub fn stop(&mut self) -> ExitStatus {
        if cfg!(target_os = "macos") {
            let sent = Command::new("kill")
                .args(["-TERM", &self.child.id().to_string()])
                .status()
                .unwrap();
            assert!(sent.success());
        } else {
            drop(self.child.stdin.take());
        }
        self.exit_status()
    }

    /// Reads stderr to its end (the engine has exited, or is exiting) and reaps it. Panics if
    /// stderr is still open after [`HANG`].
    pub fn exit_status(&mut self) -> ExitStatus {
        for _ in 0..MAX_LINES {
            match self.lines.recv_timeout(HANG) {
                Ok(line) => self.seen.push(line),
                Err(mpsc::RecvTimeoutError::Disconnected) => return self.child.wait().unwrap(),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("still running after {HANG:?}: {:?}", self.seen)
                }
            }
        }
        panic!("still logging after {MAX_LINES} lines: {:?}", self.seen)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Already reaped after a clean stop: kill then fails harmlessly.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
