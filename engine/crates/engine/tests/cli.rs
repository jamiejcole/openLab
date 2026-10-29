// SPDX-License-Identifier: GPL-3.0-or-later

use std::process::Command;

mod common;

const ENGINE: &str = env!("CARGO_BIN_EXE_openlab-engine");

#[test]
fn version_prints_to_stdout() {
    let out = Command::new(ENGINE).arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("openlab-engine {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn version_into_closed_pipe_does_not_panic() {
    // Close the read end before the engine starts, so its write always hits a broken pipe.
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let status = Command::new(ENGINE)
        .arg("--version")
        .stdout(writer)
        .status()
        .unwrap();
    // 101 is Rust's panic exit code.
    assert_ne!(status.code(), Some(101), "engine panicked: {status}");
}

#[test]
fn a_positional_show_file_is_a_usage_error_not_a_run() {
    // M4.15g: the engine starts empty; `--open` replaced the positional show file.
    let out = Command::new(ENGINE)
        .arg("/nonexistent/openlab/show.openlab")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().starts_with("usage"));
}

/// Reads stderr lines until one contains `want`; `None` when stderr ends first.
fn wait_for(lines: &mut impl Iterator<Item = String>, seen: &mut Vec<String>, want: &str) -> bool {
    for line in lines.by_ref() {
        let hit = line.contains(want);
        seen.push(line);
        if hit {
            return true;
        }
    }
    false
}

/// M4.9: `engine.json` is written at start (private, this pid) and removed on a graceful
/// shutdown (SIGTERM on macOS; stdin closing with `--watch-stdin` elsewhere), as is the run
/// marker; `--open` goes through `open` and a file that cannot be opened leaves the engine
/// running. Needs an audio output device: without one the engine refuses to start, and the
/// test says so and stops there.
#[test]
fn engine_json_lives_as_long_as_the_engine_and_a_signal_ends_it_cleanly() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing.openlab");
    let mut child = Command::new(ENGINE)
        .args(["--app-support".as_ref(), root.path().as_os_str()])
        .args(["--open".as_ref(), missing.as_os_str()])
        .args(if cfg!(target_os = "macos") {
            &[][..]
        } else {
            &["--watch-stdin"][..]
        })
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stderr.take().unwrap())
        .lines()
        .map_while(Result::ok);
    let mut seen = Vec::new();
    if !wait_for(&mut lines, &mut seen, "listening on") {
        let status = child.wait().unwrap();
        assert_ne!(status.code(), Some(101), "engine panicked: {seen:?}");
        assert!(
            seen.iter().any(|l| l.contains("audio")),
            "the engine did not start: {seen:?}"
        );
        eprintln!("no audio device here, skipped: {seen:?}");
        return;
    }
    let file = root.path().join("openLab").join("engine.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(v["pid"].as_u64(), Some(u64::from(child.id())));
    assert!(v["port"].as_u64().unwrap() > 0);
    assert_eq!(v["token"].as_str().unwrap().len(), 64);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert!(
        wait_for(&mut lines, &mut seen, "could not open"),
        "{seen:?}"
    );
    if cfg!(target_os = "macos") {
        let sent = Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap();
        assert!(sent.success());
    } else {
        drop(child.stdin.take());
    }
    seen.extend(lines);
    let status = child.wait().unwrap();
    assert!(status.success(), "{status}: {seen:?}");
    assert!(!file.exists(), "engine.json removed");
    let running: Vec<_> = std::fs::read_dir(root.path().join("openLab").join("Running"))
        .map(|d| d.filter_map(Result::ok).map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(running.is_empty(), "run marker removed: {running:?}");
}

/// #164 (M4.4e): `--open` keeps the recovery offer. A show with an autosave newer than the file
/// is not opened (the operator decides in the app) and the engine says so and keeps running.
/// Needs an audio output device, like the test above; without one it says so and stops.
#[test]
fn open_of_a_show_with_a_newer_autosave_says_not_opened_and_keeps_running() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let root = tempfile::tempdir().unwrap();
    let dirs = openlab_engine::persist::AppDirs::new(root.path());
    let ws = common::example();
    let show = root.path().join("shows").join("offer.openlab");
    std::fs::create_dir_all(show.parent().unwrap()).unwrap();
    std::fs::write(&show, openlab_core::save_show(&ws).unwrap()).unwrap();
    let recovery = openlab_engine::persist::write_recovery(&dirs, &ws, Some(&show)).unwrap();
    common::set_mtime(&show, 1_000_000);
    common::set_mtime(&recovery, 2_000_000);

    let mut child = Command::new(ENGINE)
        .args(["--app-support".as_ref(), root.path().as_os_str()])
        .args(["--open".as_ref(), show.as_os_str()])
        .args(if cfg!(target_os = "macos") {
            &[][..]
        } else {
            &["--watch-stdin"][..]
        })
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stderr.take().unwrap())
        .lines()
        .map_while(Result::ok);
    let mut seen = Vec::new();
    if !wait_for(&mut lines, &mut seen, "listening on") {
        let status = child.wait().unwrap();
        assert_ne!(status.code(), Some(101), "engine panicked: {seen:?}");
        assert!(
            seen.iter().any(|l| l.contains("audio")),
            "the engine did not start: {seen:?}"
        );
        eprintln!("no audio device here, skipped: {seen:?}");
        return;
    }
    assert!(wait_for(&mut lines, &mut seen, "not opened"), "{seen:?}");
    let said = seen.last().unwrap();
    assert!(
        said.contains(show.to_str().unwrap()) && said.contains("autosave newer"),
        "{said}"
    );
    assert!(
        !seen.iter().any(|l| l.starts_with("openlab-engine: opened")),
        "{seen:?}"
    );
    // The offer opened nothing and changed nothing: the autosave is still there for the app.
    assert!(recovery.exists());
    if cfg!(target_os = "macos") {
        let sent = Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap();
        assert!(sent.success());
    } else {
        drop(child.stdin.take());
    }
    seen.extend(lines);
    let status = child.wait().unwrap();
    assert!(status.success(), "{status}: {seen:?}");
}

/// M8: one engine per user. While another process holds `engine.lock` (here the test itself,
/// standing for an engine still starting), a start is refused at once, with exit 1 and the
/// message, before it opens a log, a run marker, the audio device or `engine.json`. Needs no
/// audio device: the lock is the first thing a start takes.
#[test]
fn a_second_engine_is_refused_while_another_holds_the_instance_lock() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("openLab");
    std::fs::create_dir_all(&dir).unwrap();
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("engine.lock"))
        .unwrap();
    held.try_lock().unwrap();
    // `--watch-stdin` with no stdin: an engine that wrongly started ends at once, not never.
    let out = Command::new(ENGINE)
        .args(["--app-support".as_ref(), root.path().as_os_str()])
        .arg("--watch-stdin")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("openlab-engine: another openLab engine is running ("),
        "{stderr}"
    );
    for left_alone in ["Running", "Logs", "engine.json"] {
        assert!(!dir.join(left_alone).exists(), "{left_alone} was created");
    }
    drop(held);
}
