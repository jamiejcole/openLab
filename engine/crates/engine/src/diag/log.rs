// SPDX-License-Identifier: GPL-3.0-or-later

//! The show log (DX-02): one structured record per event, kept in a bounded in-memory ring (for
//! `log_tail` and the crash report) and offered to the disk writer ([`super::writer`]).
//!
//! Recording never blocks and never fails: the ring is a short critical section, and a record
//! the writer has no room for is dropped and counted.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError, TryLockError};

use openlab_core::protocol::{LogEntry, LogLevel};

use super::writer::LogSink;

/// Entries kept in memory.
pub const RING_CAPACITY: usize = 1000;
/// A message longer than this is cut (OSC strings and file paths are not ours to bound).
pub const MAX_MESSAGE_BYTES: usize = 2048;

/// State the crash hook reads. Only `try_lock`ed there: the thread that panicked may hold it.
#[derive(Debug, Default)]
pub struct Shared {
    ring: Mutex<VecDeque<LogEntry>>,
    workspace: Mutex<Option<String>>,
}

impl Shared {
    /// The newest `max` entries, oldest first, without waiting: `None` if the lock is held.
    pub fn try_tail(&self, max: usize) -> Option<Vec<LogEntry>> {
        let ring = match self.ring.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => return None,
        };
        let skip = ring.len().saturating_sub(max);
        Some(ring.iter().skip(skip).cloned().collect())
    }

    /// The open workspace's path, without waiting: `None` if unknown or the lock is held.
    pub fn try_workspace(&self) -> Option<String> {
        match self.workspace.try_lock() {
            Ok(guard) => guard.clone(),
            Err(TryLockError::Poisoned(p)) => p.into_inner().clone(),
            Err(TryLockError::WouldBlock) => None,
        }
    }
}

/// The control thread's handle on the log. Clones share the ring and the sink.
#[derive(Clone, Debug, Default)]
pub struct ShowLog {
    shared: Arc<Shared>,
    sink: Option<LogSink>,
}

impl ShowLog {
    /// A log that keeps the ring and, with a `sink`, writes to disk.
    pub fn new(sink: Option<LogSink>) -> Self {
        Self {
            shared: Arc::default(),
            sink,
        }
    }

    /// What the crash hook reads.
    pub fn shared(&self) -> Arc<Shared> {
        Arc::clone(&self.shared)
    }

    /// Adds one record. A non-finite `show_time` is left out (it cannot be encoded) and long
    /// text is cut.
    pub fn record(&self, mut entry: LogEntry) {
        entry.show_time = entry.show_time.filter(|t| t.is_finite());
        entry.message = cut(entry.message);
        entry.source = entry.source.map(cut);
        if let Some(sink) = &self.sink {
            sink.offer(&entry);
        }
        let mut ring = self
            .shared
            .ring
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if ring.len() >= RING_CAPACITY {
            ring.pop_front();
        }
        ring.push_back(entry);
    }

    /// The newest `max` entries, oldest first.
    pub fn tail(&self, max: usize) -> Vec<LogEntry> {
        let ring = self
            .shared
            .ring
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let skip = ring.len().saturating_sub(max);
        ring.iter().skip(skip).cloned().collect()
    }

    /// Records the disk writer had no room for (or could not write) so far.
    pub fn dropped(&self) -> u64 {
        self.sink.as_ref().map_or(0, LogSink::dropped)
    }

    /// Tells the crash hook which workspace is open.
    pub fn set_workspace(&self, path: Option<String>) {
        *self
            .shared
            .workspace
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = path;
    }
}

fn cut(mut text: String) -> String {
    if text.len() > MAX_MESSAGE_BYTES {
        let mut end = MAX_MESSAGE_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("...");
    }
    text
}

/// One line of text for a crash report or a terminal:
/// `2026-10-27T12:00:00.123Z +12.345s INFO go [cue ...] [osc 10.0.0.2] message`.
pub fn format_entry(e: &LogEntry) -> String {
    let level = match e.level {
        LogLevel::Error => "ERROR",
        LogLevel::Warn => "WARN",
        LogLevel::Info => "INFO",
        LogLevel::Debug => "DEBUG",
    };
    let kind = serde_json::to_value(e.kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    let mut line = format!("{} ", super::time::iso(e.time_ms));
    if let Some(t) = e.show_time {
        line.push_str(&format!("+{t:.3}s "));
    }
    line.push_str(&format!("{level} {kind}"));
    if let Some(cue) = e.cue {
        line.push_str(&format!(" [cue {cue}]"));
    }
    if let Some(source) = &e.source {
        line.push_str(&format!(" [{source}]"));
    }
    line.push(' ');
    line.push_str(&e.message);
    line
}

#[cfg(test)]
mod tests {
    use openlab_core::protocol::LogKind;

    use super::*;

    fn rec(log: &ShowLog, n: i64, message: &str) {
        log.record(LogEntry {
            time_ms: n,
            show_time: Some(n as f64),
            level: LogLevel::Info,
            kind: LogKind::Engine,
            message: message.to_owned(),
            cue: None,
            source: None,
        });
    }

    #[test]
    fn the_ring_keeps_the_newest_entries() {
        let log = ShowLog::new(None);
        for n in 0..(RING_CAPACITY as i64 + 5) {
            rec(&log, n, "x");
        }
        let all = log.tail(usize::MAX);
        assert_eq!(all.len(), RING_CAPACITY);
        assert_eq!(all[0].time_ms, 5);
        let last = log.tail(2);
        assert_eq!(last.len(), 2);
        assert_eq!(last[1].time_ms, RING_CAPACITY as i64 + 4);
        assert!(log.tail(0).is_empty());
    }

    #[test]
    fn long_messages_are_cut_on_a_character_boundary() {
        let log = ShowLog::new(None);
        // 3-byte characters, so the limit falls inside one.
        rec(&log, 0, &"\u{20ac}".repeat(MAX_MESSAGE_BYTES));
        let m = &log.tail(1)[0].message;
        assert!(m.len() <= MAX_MESSAGE_BYTES + 3);
        assert!(m.ends_with("..."));
    }

    #[test]
    fn a_non_finite_show_time_is_left_out() {
        let log = ShowLog::new(None);
        log.record(LogEntry {
            time_ms: 0,
            show_time: Some(f64::NAN),
            level: LogLevel::Info,
            kind: LogKind::Engine,
            message: String::new(),
            cue: None,
            source: None,
        });
        assert_eq!(log.tail(1)[0].show_time, None);
    }

    #[test]
    fn the_crash_view_reads_without_waiting() {
        let log = ShowLog::new(None);
        rec(&log, 1, "a");
        log.set_workspace(Some("/shows/x.openlab".into()));
        let shared = log.shared();
        assert_eq!(shared.try_tail(10).map(|t| t.len()), Some(1));
        assert_eq!(shared.try_workspace().as_deref(), Some("/shows/x.openlab"));
        // A held lock (the panicking thread's own) gives up instead of waiting.
        let _held = shared.ring.lock().unwrap();
        assert!(shared.try_tail(10).is_none());
        let _held = shared.workspace.lock().unwrap();
        assert!(shared.try_workspace().is_none());
    }

    #[test]
    fn lines_show_time_kind_cue_and_source() {
        let e = LogEntry {
            time_ms: 1_700_000_000_123,
            show_time: Some(12.3456),
            level: LogLevel::Warn,
            kind: LogKind::Trigger,
            message: "go".into(),
            cue: None,
            source: Some("osc 10.0.0.2".into()),
        };
        assert_eq!(
            format_entry(&e),
            "2023-11-14T22:13:20.123Z +12.346s WARN trigger [osc 10.0.0.2] go"
        );
    }
}
