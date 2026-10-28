// SPDX-License-Identifier: GPL-3.0-or-later

use std::process::Command;

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
