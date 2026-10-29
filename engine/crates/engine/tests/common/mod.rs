// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

//! Helpers for persistence tests.

use std::fs::{self, File};
use std::path::Path;
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
