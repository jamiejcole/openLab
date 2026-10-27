// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The crash hook (M4.7; DX-04) in a real panic: the test binary runs itself again as a child
//! with an environment variable set, the child installs the hook and panics, and the parent
//! reads the report the hook left. The report text itself is unit-tested in `diag::crash`.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;

use openlab_core::protocol::{LogEntry, LogKind, LogLevel};
use openlab_engine::control::EpochClock;
use openlab_engine::diag::ShowLog;
use openlab_engine::diag::crash::{self, CrashConfig, MAX_REPORTS};

const DIR: &str = "OPENLAB_CRASH_TEST_DIR";
const PANICS: &str = "OPENLAB_CRASH_TEST_PANICS";
const CHILD: &str = "the_child_that_panics";
const T0: i64 = 1_700_000_000_000;

/// Not a test on its own: does nothing unless a parent test started it with [`DIR`] set.
#[test]
fn the_child_that_panics() {
    let Some(dir) = std::env::var_os(DIR) else {
        return;
    };
    let panics: usize = std::env::var(PANICS)
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(1);
    let log = ShowLog::new(None);
    for n in 0..300 {
        log.record(LogEntry {
            time_ms: T0 + n,
            show_time: Some(n as f64 / 10.0),
            level: LogLevel::Info,
            kind: LogKind::Go,
            message: format!("line {n}"),
            cue: None,
            source: None,
        });
    }
    log.set_workspace(Some("/shows/Hamlet.openlab".into()));
    crash::install(CrashConfig {
        dir: dir.into(),
        version: "9.9.9",
        shared: log.shared(),
        clock: EpochClock::new(|| T0 + 1000),
    });
    for k in 0..panics {
        let outcome = thread::Builder::new()
            .name("openlab-control".into())
            .spawn(move || {
                let empty: Vec<u8> = Vec::new();
                // A real index panic, like a bug would make.
                let at = std::hint::black_box(k + 7);
                empty[at]
            })
            .unwrap()
            .join();
        assert!(outcome.is_err());
    }
}

fn run_child(dir: &Path, panics: usize) -> String {
    let exe = std::env::current_exe().unwrap();
    let out = Command::new(exe)
        .args([CHILD, "--exact", "--nocapture", "--test-threads=1"])
        .env(DIR, dir)
        .env(PANICS, panics.to_string())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn reports(dir: &Path) -> Vec<String> {
    let mut files: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    files
        .iter()
        .map(|p| fs::read_to_string(p).unwrap())
        .collect()
}

#[test]
fn a_panic_leaves_a_report_with_the_workspace_and_the_last_log_lines() {
    let dir = tempfile::tempdir().unwrap();
    let stderr = run_child(dir.path(), 1);
    // The previous hook still ran: the message reaches stderr.
    assert!(stderr.contains("index out of bounds"), "{stderr}");
    let reports = reports(dir.path());
    let [text] = reports.as_slice() else {
        panic!("{reports:?}");
    };
    assert!(
        text.starts_with("openLab crash report\nversion: 9.9.9\n"),
        "{text}"
    );
    assert!(text.contains("time: 2023-11-14T22:13:21.000Z"));
    assert!(text.contains("workspace: /shows/Hamlet.openlab"));
    assert!(text.contains("thread: openlab-control"));
    assert!(text.contains("panic: index out of bounds"), "{text}");
    assert!(text.contains("diag_crash.rs"), "the location: {text}");
    assert!(text.contains("backtrace:"));
    // The last 200 of the 300 lines, oldest first.
    assert!(text.contains("last 200 log line(s), oldest first:"));
    assert!(!text.contains("line 99 "), "older than the last 200");
    let first = text.find("line 100").expect("line 100");
    let last = text.find("line 299").expect("line 299");
    assert!(first < last);
    assert!(text.contains("+29.900s INFO go line 299"), "{text}");
}

#[test]
fn a_panic_that_repeats_writes_a_few_reports_not_one_per_panic() {
    let dir = tempfile::tempdir().unwrap();
    run_child(dir.path(), MAX_REPORTS + 4);
    assert_eq!(reports(dir.path()).len(), MAX_REPORTS);
}
