// SPDX-License-Identifier: GPL-3.0-or-later

//! Local crash capture (DX-04): a panic hook that writes a report file with the version, the
//! operating system, the open workspace, the panic and the last show-log lines. Nothing is sent
//! anywhere; sending is opt-in and not part of v0.0.1.
//!
//! The hook runs on whichever thread panicked, possibly one that holds the log's lock, so it
//! only ever `try_lock`s ([`Shared::try_tail`]) and falls back to a report without log lines.
//! It never panics itself (a panic in a hook aborts the process) and a re-entrant call writes
//! nothing. It writes at most [`MAX_REPORTS`] files per run: a panic the audio callback catches
//! can repeat every block.

use std::backtrace::Backtrace;
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::panic::{self, PanicHookInfo};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use openlab_core::protocol::LogEntry;

use super::log::{Shared, format_entry};
use super::time;
use crate::control::EpochClock;

/// Log lines in a report.
pub const REPORT_LINES: usize = 200;
/// Reports written by one process.
pub const MAX_REPORTS: usize = 3;

/// What a report says.
pub struct Report<'a> {
    pub version: &'a str,
    pub time_ms: i64,
    pub thread: &'a str,
    pub message: &'a str,
    pub location: Option<&'a str>,
    pub backtrace: &'a str,
    pub workspace: Option<&'a str>,
    /// `None`: the log was busy and could not be read.
    pub log: Option<&'a [LogEntry]>,
}

/// The report text. Plain, so a person can read it and attach it to an issue.
pub fn format_report(r: &Report<'_>) -> String {
    let mut out = String::new();
    // Writing to a String cannot fail.
    let _ = writeln!(out, "openLab crash report");
    let _ = writeln!(out, "version: {}", r.version);
    let _ = writeln!(out, "time: {}", time::iso(r.time_ms));
    let _ = writeln!(
        out,
        "os: {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let _ = writeln!(out, "workspace: {}", r.workspace.unwrap_or("(none open)"));
    let _ = writeln!(out, "thread: {}", r.thread);
    let _ = writeln!(out, "panic: {}", r.message);
    let _ = writeln!(out, "location: {}", r.location.unwrap_or("(unknown)"));
    let _ = writeln!(out, "\nbacktrace:\n{}", r.backtrace);
    match r.log {
        Some(lines) => {
            let _ = writeln!(out, "\nlast {} log line(s), oldest first:", lines.len());
            for entry in lines {
                let _ = writeln!(out, "{}", format_entry(entry));
            }
        }
        None => {
            let _ = writeln!(out, "\nlog lines unavailable: the log was busy");
        }
    }
    out
}

/// Where reports go and what they read.
#[derive(Clone)]
pub struct CrashConfig {
    pub dir: PathBuf,
    pub version: &'static str,
    pub shared: Arc<Shared>,
    pub clock: EpochClock,
}

static WRITING: AtomicBool = AtomicBool::new(false);
static WRITTEN: AtomicUsize = AtomicUsize::new(0);

/// Installs the hook (once per process is the intent; a later call replaces it). The previous
/// hook still runs afterwards, so the message still reaches stderr.
pub fn install(config: CrashConfig) {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        // Re-entrant (a panic while writing the report) or over the cap: leave it to the
        // previous hook.
        if !WRITING.swap(true, Ordering::AcqRel) {
            if WRITTEN.load(Ordering::Relaxed) < MAX_REPORTS
                && write_report(&config, info).is_some()
            {
                WRITTEN.fetch_add(1, Ordering::Relaxed);
            }
            WRITING.store(false, Ordering::Release);
        }
        previous(info);
    }));
}

/// Writes the report for `info`; `None` if the file could not be written.
fn write_report(config: &CrashConfig, info: &PanicHookInfo<'_>) -> Option<PathBuf> {
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("(non-text panic payload)");
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
    let thread = std::thread::current();
    let backtrace = Backtrace::force_capture().to_string();
    let now = config.clock.now_ms();
    let tail = config.shared.try_tail(REPORT_LINES);
    let workspace = config.shared.try_workspace();
    let text = format_report(&Report {
        version: config.version,
        time_ms: now,
        thread: thread.name().unwrap_or("(unnamed)"),
        message,
        location: location.as_deref(),
        backtrace: &backtrace,
        workspace: workspace.as_deref(),
        log: tail.as_deref(),
    });
    write_file(&config.dir, now, &text)
}

/// `crash-<UTC time>-<pid>[-n].txt`, created new (never over an earlier report).
fn write_file(dir: &Path, now_ms: i64, text: &str) -> Option<PathBuf> {
    fs::create_dir_all(dir).ok()?;
    let stem = format!("crash-{}-{}", time::compact(now_ms), std::process::id());
    for n in 0..100 {
        let name = if n == 0 {
            format!("{stem}.txt")
        } else {
            format!("{stem}-{n}.txt")
        };
        let path = dir.join(name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(text.as_bytes()).ok()?;
                // A crash may be followed at once by the process ending: get it to disk.
                let _ = file.sync_all();
                return Some(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use openlab_core::protocol::{LogKind, LogLevel};

    use super::*;

    fn entry(n: i64) -> LogEntry {
        LogEntry {
            time_ms: 1_700_000_000_000 + n,
            show_time: Some(n as f64),
            level: LogLevel::Info,
            kind: LogKind::Go,
            message: format!("line {n}"),
            cue: None,
            source: None,
        }
    }

    fn report<'a>(log: Option<&'a [LogEntry]>, workspace: Option<&'a str>) -> Report<'a> {
        Report {
            version: "9.9.9",
            time_ms: 1_700_000_000_123,
            thread: "openlab-control",
            message: "index out of bounds",
            location: Some("src/x.rs:1:2"),
            backtrace: "  0: frame",
            workspace,
            log,
        }
    }

    #[test]
    fn the_report_names_version_os_workspace_panic_and_last_lines() {
        let lines = [entry(1), entry(2)];
        let text = format_report(&report(Some(&lines), Some("/shows/Hamlet.openlab")));
        assert!(
            text.starts_with("openLab crash report\nversion: 9.9.9\n"),
            "{text}"
        );
        assert!(text.contains("time: 2023-11-14T22:13:20.123Z"));
        assert!(text.contains(&format!(
            "os: {} {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )));
        assert!(text.contains("workspace: /shows/Hamlet.openlab"));
        assert!(text.contains("thread: openlab-control"));
        assert!(text.contains("panic: index out of bounds"));
        assert!(text.contains("location: src/x.rs:1:2"));
        assert!(text.contains("last 2 log line(s), oldest first:"));
        let one = text.find("line 1").unwrap();
        let two = text.find("line 2").unwrap();
        assert!(one < two);
    }

    #[test]
    fn a_busy_log_and_no_workspace_still_give_a_report() {
        let text = format_report(&report(None, None));
        assert!(text.contains("workspace: (none open)"));
        assert!(text.contains("log lines unavailable"));
    }

    #[test]
    fn reports_never_overwrite_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_file(dir.path(), 1_700_000_000_000, "a").unwrap();
        let b = write_file(dir.path(), 1_700_000_000_000, "b").unwrap();
        assert_ne!(a, b);
        assert_eq!(fs::read_to_string(a).unwrap(), "a");
        assert_eq!(fs::read_to_string(b).unwrap(), "b");
    }

    #[test]
    fn an_unwritable_folder_gives_none_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        fs::write(&file, "x").unwrap();
        assert!(write_file(&file.join("sub"), 0, "x").is_none());
    }
}
