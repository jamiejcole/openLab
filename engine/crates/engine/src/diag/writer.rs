// SPDX-License-Identifier: GPL-3.0-or-later

//! The show log's disk side (ARCHITECTURE §3.1 "Writer"): a bounded channel from the control
//! thread to a writer thread that appends JSON lines to a size-rotated file.
//!
//! The control thread only ever `try_send`s. When the writer is behind (a slow or stuck disk)
//! the line is dropped and counted; the writer notes the loss in the file once it catches up.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, bounded};
use openlab_core::protocol::{LogEntry, LogKind, LogLevel};

use crate::control::EpochClock;
use crate::persist::remove_if_present;

/// Lines that may wait for the writer.
pub const QUEUE_CAPACITY: usize = 4096;

/// The active file; older ones are `openlab.1.log` (newest) to `openlab.<n-1>.log`.
const ACTIVE: &str = "openlab";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogLimits {
    /// A file is rotated before it would grow past this.
    pub max_file_bytes: u64,
    /// Files kept, the active one included (at least 1).
    pub max_files: usize,
}

impl Default for LogLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 4 * 1024 * 1024,
            max_files: 8,
        }
    }
}

/// The control thread's end of the channel.
#[derive(Clone, Debug)]
pub struct LogSink {
    tx: Sender<String>,
    dropped: Arc<AtomicU64>,
}

impl LogSink {
    /// Queues one record; never waits. A full queue (or an entry that cannot be encoded)
    /// counts as dropped.
    pub fn offer(&self, entry: &LogEntry) {
        let queued = serde_json::to_string(entry)
            .ok()
            .map(|line| self.tx.try_send(line));
        if !matches!(queued, Some(Ok(()))) {
            // A disconnected writer (it has stopped) also loses the line.
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Records lost so far: queue full, writer gone, or the file could not be written.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// A sink with no writer behind it: whatever is offered waits in the returned receiver,
    /// and past `capacity` lines is dropped (tests of a stuck disk).
    pub fn detached(capacity: usize) -> (Self, Receiver<String>) {
        let (tx, rx) = bounded(capacity.max(1));
        (
            Self {
                tx,
                dropped: Arc::default(),
            },
            rx,
        )
    }
}

/// The writer thread. It ends when every [`LogSink`] clone is gone, after writing what is queued.
#[derive(Debug)]
pub struct LogWriter {
    thread: Option<JoinHandle<()>>,
    /// Signalled when the thread has written what was queued and ended.
    done: Receiver<()>,
}

impl LogWriter {
    /// Starts writing into `dir` (created if missing). An unwritable folder is not an error
    /// here: the writer counts every line as dropped and tries again on the next one.
    pub fn start(
        dir: PathBuf,
        limits: LogLimits,
        clock: EpochClock,
    ) -> io::Result<(LogSink, Self)> {
        let (sink, rx) = LogSink::detached(QUEUE_CAPACITY);
        let dropped = Arc::clone(&sink.dropped);
        let (done_tx, done) = bounded(1);
        let thread = thread::Builder::new()
            .name("openlab-log".into())
            .spawn(move || {
                run(&rx, RotatingFile::new(dir, limits), &dropped, &clock);
                let _ = done_tx.send(());
            })?;
        Ok((
            sink,
            Self {
                thread: Some(thread),
                done,
            },
        ))
    }

    /// Waits up to `wait` for the thread to write what is queued and end; returns whether it
    /// did. A hung disk, or a sink that outlives the control loop, must not hang the engine's
    /// shutdown: the thread is then left behind (the process is about to exit).
    pub fn finish(mut self, wait: Duration) -> bool {
        let ended = self.done.recv_timeout(wait).is_ok();
        if let Some(thread) = self.thread.take()
            && ended
        {
            let _ = thread.join();
        }
        ended
    }
}

fn run(rx: &Receiver<String>, mut file: RotatingFile, dropped: &AtomicU64, clock: &EpochClock) {
    let mut reported = 0;
    while let Ok(line) = rx.recv() {
        let now = dropped.load(Ordering::Relaxed);
        if now > reported {
            reported = now;
            let note = LogEntry {
                time_ms: clock.now_ms(),
                show_time: None,
                level: LogLevel::Warn,
                kind: LogKind::Engine,
                message: format!(
                    "{now} log record(s) were lost so far: the disk could not keep up or the \
                     log file could not be written"
                ),
                cue: None,
                source: None,
            };
            if let Ok(text) = serde_json::to_string(&note) {
                let _ = file.write_line(&text);
            }
        }
        if file.write_line(&line).is_err() {
            dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// An append-only file that rotates by size.
#[derive(Debug)]
pub struct RotatingFile {
    dir: PathBuf,
    limits: LogLimits,
    open: Option<(File, u64)>,
}

impl RotatingFile {
    pub fn new(dir: PathBuf, limits: LogLimits) -> Self {
        Self {
            dir,
            limits: LogLimits {
                max_files: limits.max_files.max(1),
                ..limits
            },
            open: None,
        }
    }

    fn path(&self, index: usize) -> PathBuf {
        if index == 0 {
            self.dir.join(format!("{ACTIVE}.log"))
        } else {
            self.dir.join(format!("{ACTIVE}.{index}.log"))
        }
    }

    /// Appends `line` and a newline. On an error the file is closed, so the next call opens it
    /// afresh (the folder may have come back).
    pub fn write_line(&mut self, line: &str) -> io::Result<()> {
        let result = self.write_inner(line);
        if result.is_err() {
            self.open = None;
        }
        result
    }

    fn write_inner(&mut self, line: &str) -> io::Result<()> {
        let need = line.len() as u64 + 1;
        if self.open.is_none() {
            fs::create_dir_all(&self.dir)?;
            let path = self.path(0);
            let file = OpenOptions::new().create(true).append(true).open(&path)?;
            let size = file.metadata()?.len();
            self.open = Some((file, size));
        }
        if self.open.as_ref().is_some_and(|(_, size)| {
            *size > 0 && size.saturating_add(need) > self.limits.max_file_bytes
        }) {
            self.rotate()?;
        }
        let mut text = String::with_capacity(line.len() + 1);
        text.push_str(line);
        text.push('\n');
        let Some((file, size)) = self.open.as_mut() else {
            return Err(io::Error::other("log file is not open"));
        };
        file.write_all(text.as_bytes())?;
        *size += need;
        Ok(())
    }

    /// `openlab.log` becomes `openlab.1.log`, `.1` becomes `.2`, and so on; the oldest goes.
    fn rotate(&mut self) -> io::Result<()> {
        self.open = None;
        let oldest = self.limits.max_files - 1;
        remove_if_present(&self.path(oldest))?;
        for index in (0..oldest).rev() {
            match fs::rename(self.path(index), self.path(index + 1)) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path(0))?;
        self.open = Some((file, 0));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn rotation_keeps_the_newest_files_within_the_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let limits = LogLimits {
            max_file_bytes: 100,
            max_files: 3,
        };
        let mut f = RotatingFile::new(dir.path().to_path_buf(), limits);
        // 29 bytes + newline = 30: three lines fit per file.
        for n in 0..20 {
            f.write_line(&format!("{n:0>29}")).unwrap();
        }
        assert_eq!(
            names(dir.path()),
            ["openlab.1.log", "openlab.2.log", "openlab.log"]
        );
        for name in names(dir.path()) {
            let len = fs::metadata(dir.path().join(&name)).unwrap().len();
            assert!(len <= 100, "{name}: {len}");
        }
        // Newest lines in the active file, older ones further back; the oldest are gone.
        let read = |n: &str| fs::read_to_string(dir.path().join(n)).unwrap();
        assert!(read("openlab.log").contains(&format!("{:0>29}", 19)));
        assert!(read("openlab.2.log").lines().count() == 3);
        assert!(!read("openlab.2.log").contains(&format!("{:0>29}", 0)));
    }

    #[test]
    fn one_file_is_truncated_not_grown() {
        let dir = tempfile::tempdir().unwrap();
        let limits = LogLimits {
            max_file_bytes: 50,
            max_files: 1,
        };
        let mut f = RotatingFile::new(dir.path().to_path_buf(), limits);
        for n in 0..10 {
            f.write_line(&format!("{n:0>29}")).unwrap();
        }
        assert_eq!(names(dir.path()), ["openlab.log"]);
        assert!(fs::metadata(dir.path().join("openlab.log")).unwrap().len() <= 50);
    }

    #[test]
    fn an_existing_file_is_appended_to_and_counted() {
        let dir = tempfile::tempdir().unwrap();
        let limits = LogLimits {
            max_file_bytes: 100,
            max_files: 2,
        };
        fs::write(dir.path().join("openlab.log"), "x".repeat(90)).unwrap();
        let mut f = RotatingFile::new(dir.path().to_path_buf(), limits);
        f.write_line("0123456789").unwrap();
        // The old 90 bytes were counted: this line did not fit, so the file rotated first.
        assert_eq!(names(dir.path()), ["openlab.1.log", "openlab.log"]);
        assert_eq!(
            fs::read_to_string(dir.path().join("openlab.log")).unwrap(),
            "0123456789\n"
        );
    }

    #[test]
    fn an_unwritable_folder_is_an_error_not_a_panic_and_recovers() {
        let dir = tempfile::tempdir().unwrap();
        // A file where the folder should be.
        let blocker = dir.path().join("logs");
        fs::write(&blocker, "not a folder").unwrap();
        let mut f = RotatingFile::new(blocker.clone(), LogLimits::default());
        assert!(f.write_line("a").is_err());
        fs::remove_file(&blocker).unwrap();
        assert!(f.write_line("b").is_ok());
    }

    fn entry(n: i64) -> LogEntry {
        LogEntry {
            time_ms: n,
            show_time: None,
            level: LogLevel::Info,
            kind: LogKind::Engine,
            message: format!("m{n}"),
            cue: None,
            source: None,
        }
    }

    #[test]
    fn a_full_queue_drops_and_counts_without_waiting() {
        let (sink, rx) = LogSink::detached(3);
        for n in 0..10 {
            sink.offer(&entry(n));
        }
        assert_eq!(sink.dropped(), 7);
        assert_eq!(rx.len(), 3);
        // Once the reader is gone every line is lost, still without waiting.
        drop(rx);
        sink.offer(&entry(11));
        assert_eq!(sink.dropped(), 8);
    }

    #[test]
    fn the_writer_thread_writes_lines_and_notes_losses() {
        let dir = tempfile::tempdir().unwrap();
        let (sink, writer) = LogWriter::start(
            dir.path().join("Logs"),
            LogLimits::default(),
            EpochClock::new(|| 1_700_000_000_000),
        )
        .unwrap();
        sink.dropped.fetch_add(5, Ordering::Relaxed);
        sink.offer(&entry(1));
        sink.offer(&entry(2));
        drop(sink);
        assert!(writer.finish(Duration::from_secs(30)));
        let text = fs::read_to_string(dir.path().join("Logs/openlab.log")).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        let note: LogEntry = serde_json::from_str(lines[0]).unwrap();
        assert!(note.message.starts_with("5 log record(s) were lost"));
        assert_eq!(note.level, LogLevel::Warn);
        let first: LogEntry = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(first, entry(1));
    }

    #[test]
    fn finish_gives_up_when_a_sink_outlives_the_control_loop() {
        let dir = tempfile::tempdir().unwrap();
        let (sink, writer) = LogWriter::start(
            dir.path().join("Logs"),
            LogLimits::default(),
            EpochClock::new(|| 1_700_000_000_000),
        )
        .unwrap();
        // The sink is still alive, so the thread keeps waiting for lines: shutdown must not.
        assert!(!writer.finish(Duration::from_millis(50)));
        drop(sink);
    }
}
