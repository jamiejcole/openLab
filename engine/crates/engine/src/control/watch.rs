// SPDX-License-Identifier: GPL-3.0-or-later

//! The media watch (M2.25, ARCHITECTURE §3.1; docs/control.md "Media watch", docs/persistence.md
//! "Media watch and follow"): the `openlab-watch` thread stats every media file the catalog
//! knows, metadata only, and tells the control thread which went missing, came back, changed or
//! became unreadable. No file watcher: a stat per file per pass works the same on every OS and
//! file system, network volumes included.
//!
//! - [`WatchCore`] holds the per-file state and the pass rules; it is pure apart from the
//!   [`WatchFs`] it is handed, so the rules are tested without threads or a disk.
//! - A pass covers every file once in `max(period, n ms)`, in chunks of [`CHUNK`] spread evenly
//!   over it, and ends in one batch, sent only when something changed: an idle show costs the
//!   control thread nothing.
//! - [`MediaWatch`] is the control side: it turns the catalog's key changes into jobs, drops
//!   batches of an older generation (the catalog was reset since), and runs the watchdog. The
//!   watcher publishes what it is statting; one stat longer than the stall limit (a volume that
//!   stopped answering) makes control abandon the watcher and start a fresh one that skips that
//!   volume until the abandoned one's stat returns. The state lives in a [`WatchCore`] shared by
//!   the watchers (locked only between stats, never during one), so a fresh watcher carries on
//!   where the stuck one stopped and nothing it found is lost.
//!
//! Following a moved file (`locate`, M2.29b) is not done yet: a file that is gone is `Gone`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, UNIX_EPOCH};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use openlab_core::protocol::{LogLevel, WireWarning};

use super::docs::EpochClock;
use super::inbox::Inbox;
use super::media::KeyChange;
use super::notice;

/// The shortest time a pass over every file takes (docs/persistence.md: a file sent to the
/// Trash shows broken within a few seconds).
pub const MIN_PERIOD: Duration = Duration::from_secs(3);
/// Files statted between two pauses of a pass.
pub const CHUNK: usize = 64;
/// One stat taking longer than this (on the injected clock) means its volume does not answer.
pub const WATCH_STALL: Duration = Duration::from_secs(10);
/// Watchers abandoned in a stalled stat, at most; past it the watch stops while one is stuck.
pub const MAX_ABANDONED_WATCHERS: usize = 4;

/// What a stat tells about a file: enough to tell the same file, unchanged, from another one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub modified_ms: i64,
}

/// One stat of a media path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    pub fp: Fingerprint,
    /// A regular file (a folder or a device at the path is not media).
    pub regular: bool,
    /// The path itself is a symbolic link (it is never followed to a new place, M2.29b).
    pub symlink: bool,
}

/// How the watch reads the file system: [`RealFs`] in the engine, [`FakeFs`] in tests.
pub trait WatchFs: Send + Sync {
    /// The file at `path`, following a symbolic link. `NotFound` (or `NotADirectory`): there is
    /// no file; any other error: it cannot be read.
    fn stat(&self, path: &Path) -> io::Result<Stat>;

    /// Where the file last seen as `fp` is now, if the platform can tell (M2.29b). `None` here.
    fn locate(&self, _fp: &Fingerprint) -> Option<PathBuf> {
        None
    }
}

/// The real file system.
#[derive(Clone, Copy, Debug, Default)]
pub struct RealFs;

impl WatchFs for RealFs {
    fn stat(&self, path: &Path) -> io::Result<Stat> {
        // One call for an ordinary file; a link is followed with a second.
        let link = std::fs::symlink_metadata(path)?;
        let symlink = link.file_type().is_symlink();
        let meta = if symlink {
            std::fs::metadata(path)?
        } else {
            link
        };
        let modified_ms = meta
            .modified()
            .ok()
            .map_or(0, |t| match t.duration_since(UNIX_EPOCH) {
                Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
                Err(e) => i64::try_from(e.duration().as_millis()).map_or(i64::MIN, |ms| -ms),
            });
        let (dev, ino) = identity(&meta);
        Ok(Stat {
            fp: Fingerprint {
                dev,
                ino,
                size: meta.len(),
                modified_ms,
            },
            regular: meta.is_file(),
            symlink,
        })
    }
}

#[cfg(unix)]
fn identity(meta: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

/// std has no stable file id on Windows: size and modified time tell changes there (M2.29f).
#[cfg(not(unix))]
fn identity(_meta: &std::fs::Metadata) -> (u64, u64) {
    (0, 0)
}

/// The volume a path is on, from its text alone (a stat could hang on exactly that volume):
/// `/Volumes/<name>` (macOS), `/media/<user>/<name>`, `/run/media/<user>/<name>`,
/// `/mnt/<name>`, a Windows drive or share; anything else is the system volume `/`. The
/// watchdog skips a stalled volume by this name and names it in its notice.
pub fn bucket_of(path: &Path) -> String {
    let mut parts = path.components();
    let Some(first) = parts.next() else {
        return "/".to_owned();
    };
    if let Component::Prefix(p) = first {
        return p.as_os_str().to_string_lossy().into_owned();
    }
    let names: Vec<String> = parts
        .take(5)
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let take = match names.first().map(String::as_str) {
        Some("Volumes" | "mnt") => 2,
        Some("media") => 3,
        Some("run") if names.get(1).map(String::as_str) == Some("media") => 4,
        _ => 0,
    };
    // A file directly in the mount folder is not on a volume of its own.
    if take == 0 || names.len() <= take {
        return "/".to_owned();
    }
    let mut out = String::new();
    for n in names.iter().take(take) {
        out.push('/');
        out.push_str(n);
    }
    out
}

/// A job for the watcher, in the order control sent them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchJob {
    /// The catalog was reset: forget every file; batches carry `generation` from now on.
    Reset {
        generation: u64,
    },
    /// Watch `key` at `path`; `id` comes back in its events (a key watched again gets a new id,
    /// so control can tell an event of its earlier watch).
    Watch {
        key: String,
        path: PathBuf,
        id: u64,
    },
    Unwatch {
        key: String,
    },
    /// Leave the files on this volume alone (it stopped answering); they keep their state.
    Skip {
        bucket: String,
    },
    /// Watch this volume again. `trial`: it was skipped after an error; the result of its
    /// next stat is reported in the batch's `volumes`.
    Unskip {
        bucket: String,
        trial: bool,
    },
    /// The catalog's probe of `key` failed: `missing` (no file), the file is taken as gone, so
    /// a file that appears before the first stat is still reported `Back`; otherwise, while
    /// the path is unreadable, its return is reported even if the file is unchanged.
    ProbeFailed {
        key: String,
        id: u64,
        missing: bool,
    },
}

/// What a pass found out about one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchEvent {
    /// No file at the path (reported on the first pass that sees it gone).
    Gone { key: String, id: u64, trashed: bool },
    /// A file is at the path again, the same on two passes in a row.
    Back {
        key: String,
        id: u64,
        fp: Fingerprint,
    },
    /// Another file (or the file changed) at the path, the same on two passes in a row.
    Changed {
        key: String,
        id: u64,
        fp: Fingerprint,
    },
    /// The path cannot be examined (permissions, an I/O error).
    Unreadable { key: String, id: u64, error: String },
}

impl WatchEvent {
    pub fn key(&self) -> &str {
        match self {
            Self::Gone { key, .. }
            | Self::Back { key, .. }
            | Self::Changed { key, .. }
            | Self::Unreadable { key, .. } => key,
        }
    }

    pub fn id(&self) -> u64 {
        match self {
            Self::Gone { id, .. }
            | Self::Back { id, .. }
            | Self::Changed { id, .. }
            | Self::Unreadable { id, .. } => *id,
        }
    }
}

/// The events of one pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchBatch {
    pub generation: u64,
    pub events: Vec<WatchEvent>,
    /// Volumes on trial (`Unskip { trial }`) and whether their first stat answered (a file or
    /// no file) rather than failed.
    pub volumes: Vec<(String, bool)>,
}

/// What a watcher sends the control thread.
#[derive(Debug)]
pub enum WatchAnswer {
    Batch(WatchBatch),
    /// An abandoned watcher's stalled stat returned (the thread has ended): `healthy` if it
    /// found a file or no file, not an error.
    BucketAnswered {
        bucket: String,
        healthy: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ItemState {
    /// Not statted since it was watched: the probe is under way anyway, so a file found is
    /// taken silently.
    Unknown,
    Present,
    Gone,
    Unreadable,
}

#[derive(Debug)]
struct WatchItem {
    path: PathBuf,
    bucket: String,
    id: u64,
    state: ItemState,
    /// The file as it was last accepted.
    seen: Option<Fingerprint>,
    /// A different file seen on the last pass, waiting for a second identical one.
    candidate: Option<Fingerprint>,
    symlink: bool,
    /// The catalog's probe failed while the path was unreadable: its return is reported even
    /// with the file unchanged.
    stale: bool,
}

/// One file a watcher is to stat.
#[derive(Clone, Debug)]
pub struct Target {
    key: String,
    id: u64,
    path: PathBuf,
    bucket: String,
}

/// The watch state and the pass rules (docs/persistence.md "Media watch and follow").
#[derive(Debug, Default)]
pub struct WatchCore {
    generation: u64,
    /// In key order, which is the pass order (files of one folder stay together).
    items: BTreeMap<String, WatchItem>,
    /// The last key a chunk of the current pass reached (`None`: a pass starts).
    cursor: Option<String>,
    skip: BTreeSet<String>,
    /// Volumes whose next stat is reported (`Unskip { trial }`).
    trial: BTreeSet<String>,
    /// Events of the pass under way.
    pending: Vec<WatchEvent>,
    volumes: Vec<(String, bool)>,
}

impl WatchCore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Files on this volume are left alone now.
    pub fn is_skipped(&self, bucket: &str) -> bool {
        self.skip.contains(bucket)
    }

    pub fn apply(&mut self, job: WatchJob) {
        match job {
            WatchJob::Reset { generation } => {
                self.generation = generation;
                self.items.clear();
                self.pending.clear();
                self.cursor = None;
            }
            WatchJob::Watch { key, path, id } => {
                self.pending.retain(|e| e.key() != key);
                let bucket = bucket_of(&path);
                self.items.insert(
                    key,
                    WatchItem {
                        path,
                        bucket,
                        id,
                        state: ItemState::Unknown,
                        seen: None,
                        candidate: None,
                        symlink: false,
                        stale: false,
                    },
                );
            }
            WatchJob::Unwatch { key } => {
                // Found earlier in this pass: no longer of interest.
                self.pending.retain(|e| e.key() != key);
                self.items.remove(&key);
            }
            WatchJob::Skip { bucket } => {
                self.trial.remove(&bucket);
                self.skip.insert(bucket);
            }
            WatchJob::Unskip { bucket, trial } => {
                self.skip.remove(&bucket);
                if trial {
                    self.trial.insert(bucket);
                }
            }
            WatchJob::ProbeFailed { key, id, missing } => {
                if let Some(item) = self.items.get_mut(&key).filter(|i| i.id == id) {
                    if missing {
                        // Control already shows it missing: no event, but a file found from
                        // now on is reported back.
                        item.state = ItemState::Gone;
                        item.candidate = None;
                    } else if item.state == ItemState::Unreadable {
                        item.stale = true;
                    }
                }
            }
        }
    }

    /// The next files of the pass (at most `max`), files on skipped volumes left out; `None`
    /// once the pass has covered every file (the next call starts a new one).
    pub fn next_chunk(&mut self, max: usize) -> Option<Vec<Target>> {
        use std::ops::Bound::{Excluded, Unbounded};
        let mut out = Vec::new();
        let mut last = None;
        let range = match &self.cursor {
            Some(c) => self.items.range::<String, _>((Excluded(c), Unbounded)),
            None => self.items.range::<String, _>(..),
        };
        for (key, item) in range {
            if out.len() >= max {
                break;
            }
            last = Some(key);
            if self.skip.contains(&item.bucket) {
                continue;
            }
            out.push(Target {
                key: key.clone(),
                id: item.id,
                path: item.path.clone(),
                bucket: item.bucket.clone(),
            });
        }
        match last {
            Some(k) => {
                self.cursor = Some(k.clone());
                Some(out)
            }
            None => {
                self.cursor = None;
                None
            }
        }
    }

    /// Applies one stat to its file (the pass rules). A file unwatched (or watched again) since
    /// the target was handed out is left alone.
    pub fn record(&mut self, target: &Target, stat: io::Result<Stat>) {
        let Some(item) = self.items.get_mut(&target.key) else {
            return;
        };
        if item.id != target.id {
            return;
        }
        if self.trial.remove(&item.bucket) {
            self.volumes.push((item.bucket.clone(), answers(&stat)));
        }
        let (key, id) = (target.key.clone(), target.id);
        let event = match stat {
            Ok(st) if st.regular => {
                item.symlink = st.symlink;
                let fp = st.fp;
                match item.state {
                    ItemState::Unknown => {
                        item.state = ItemState::Present;
                        item.seen = Some(fp);
                        item.candidate = None;
                        None
                    }
                    ItemState::Present if item.seen == Some(fp) => {
                        item.candidate = None;
                        None
                    }
                    ItemState::Present if item.candidate == Some(fp) => {
                        item.seen = Some(fp);
                        item.candidate = None;
                        Some(WatchEvent::Changed { key, id, fp })
                    }
                    ItemState::Gone | ItemState::Unreadable if item.candidate == Some(fp) => {
                        // Unreadable for a while and back unchanged: its re-probe (on
                        // Unreadable) already read it, unless that probe failed meanwhile.
                        let report =
                            item.state == ItemState::Gone || item.seen != Some(fp) || item.stale;
                        item.state = ItemState::Present;
                        item.seen = Some(fp);
                        item.candidate = None;
                        item.stale = false;
                        report.then_some(WatchEvent::Back { key, id, fp })
                    }
                    ItemState::Present | ItemState::Gone | ItemState::Unreadable => {
                        // Seen once: it may still be being copied. The next pass decides.
                        item.candidate = Some(fp);
                        None
                    }
                }
            }
            Ok(_) => gone(item, key, id),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                gone(item, key, id)
            }
            Err(e) => {
                item.candidate = None;
                if item.state == ItemState::Unreadable {
                    None
                } else {
                    item.state = ItemState::Unreadable;
                    item.stale = false;
                    Some(WatchEvent::Unreadable {
                        key,
                        id,
                        error: e.to_string(),
                    })
                }
            }
        };
        self.pending.extend(event);
    }

    /// The events of the pass just ended.
    pub fn finish(&mut self) -> Option<WatchBatch> {
        if self.pending.is_empty() && self.volumes.is_empty() {
            return None;
        }
        Some(WatchBatch {
            generation: self.generation,
            events: std::mem::take(&mut self.pending),
            volumes: std::mem::take(&mut self.volumes),
        })
    }

    /// A whole pass, synchronously (tests and the manual mode): what it found.
    pub fn pass(&mut self, fs: &dyn WatchFs) -> Vec<WatchEvent> {
        while let Some(chunk) = self.next_chunk(CHUNK) {
            for t in chunk {
                let stat = fs.stat(&t.path);
                self.record(&t, stat);
            }
        }
        self.finish().map(|b| b.events).unwrap_or_default()
    }
}

/// The volume answered: a file, or no file; not an error (a dead share's ETIMEDOUT).
fn answers(stat: &io::Result<Stat>) -> bool {
    match stat {
        Ok(_) => true,
        Err(e) => matches!(
            e.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
        ),
    }
}

fn gone(item: &mut WatchItem, key: String, id: u64) -> Option<WatchEvent> {
    item.candidate = None;
    if item.state == ItemState::Gone {
        return None;
    }
    item.state = ItemState::Gone;
    Some(WatchEvent::Gone {
        key,
        id,
        trashed: false,
    })
}

/// How long a pass over `n` files takes: one file a millisecond at most, so a show of
/// thousands does not stat its volume flat out.
pub fn pass_period(n: usize, min: Duration) -> Duration {
    min.max(Duration::from_millis(u64::try_from(n).unwrap_or(u64::MAX)))
}

/// The pause after each chunk of a pass over `n` files.
pub fn chunk_gap(n: usize, min: Duration) -> Duration {
    let chunks = n.div_ceil(CHUNK).max(1);
    pass_period(n, min) / u32::try_from(chunks).unwrap_or(u32::MAX)
}

/// How the watch runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchMode {
    /// On the `openlab-watch` thread (the engine).
    Thread,
    /// No thread: a pass runs when the test asks (`OfflineShow::watch_pass`).
    Manual,
}

/// The watch's settings in [`super::ControlConfig`].
#[derive(Clone)]
pub struct WatchConfig {
    pub fs: Arc<dyn WatchFs>,
    /// The shortest pass ([`MIN_PERIOD`]).
    pub period: Duration,
    /// A stat taking longer than this abandons its watcher ([`WATCH_STALL`]).
    pub stall: Duration,
    pub mode: WatchMode,
    /// Milliseconds for the watchdog and the retry backoff: monotonic (a wall-clock step must
    /// not abandon a watcher); tests inject a `ManualClock`.
    pub clock: EpochClock,
    /// A volume skipped after a stall or an error is tried again after `retry_first`, doubling
    /// up to `retry_max` while it keeps failing ([`RETRY_FIRST`], [`RETRY_MAX`]).
    pub retry_first: Duration,
    pub retry_max: Duration,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            fs: Arc::new(RealFs),
            period: MIN_PERIOD,
            stall: WATCH_STALL,
            mode: WatchMode::Thread,
            clock: monotonic_clock(),
            retry_first: RETRY_FIRST,
            retry_max: RETRY_MAX,
        }
    }
}

impl fmt::Debug for WatchConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WatchConfig")
            .field("period", &self.period)
            .field("stall", &self.stall)
            .field("mode", &self.mode)
            .field("retry_first", &self.retry_first)
            .field("retry_max", &self.retry_max)
            .finish_non_exhaustive()
    }
}

/// Milliseconds since the clock was made, from `Instant` (never steps back or jumps).
pub fn monotonic_clock() -> EpochClock {
    let start = Instant::now();
    EpochClock::new(move || i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX))
}

/// A volume skipped after a stall or an error is tried again after this long, doubling up to
/// [`RETRY_MAX`] while it keeps failing.
pub const RETRY_FIRST: Duration = Duration::from_secs(30);
pub const RETRY_MAX: Duration = Duration::from_secs(300);

fn millis(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
struct Busy {
    /// The stat under way: its number (each stat has its own) and its volume.
    stat: Option<(u64, String)>,
    stats: u64,
    abandoned: bool,
}

/// A watcher as the watchdog sees it. The stat under way and the abandonment are read and
/// changed under one lock, so a stat that returns just as the watchdog fires is either taken
/// as answered (not abandoned) or reports its volume (abandoned), never neither.
#[derive(Default)]
struct WatcherState {
    busy: Mutex<Busy>,
}

impl WatcherState {
    fn abandoned(&self) -> bool {
        lock(&self.busy).abandoned
    }

    /// A stat starts; `false` if this watcher was abandoned (it must not stat).
    fn begin(&self, bucket: &str) -> bool {
        let mut b = lock(&self.busy);
        if b.abandoned {
            return false;
        }
        b.stats = b.stats.wrapping_add(1);
        b.stat = Some((b.stats, bucket.to_owned()));
        true
    }

    /// The stat returned; whether the watcher was abandoned meanwhile.
    fn end(&self) -> bool {
        let mut b = lock(&self.busy);
        b.stat = None;
        b.abandoned
    }

    /// The watchdog: abandons the watcher if it is still in the very stat the watchdog judged
    /// stalled (compare and set: a stat that returned meanwhile is not abandoned).
    fn try_abandon(&self, stalled: &(u64, String)) -> bool {
        let mut b = lock(&self.busy);
        if b.stat.as_ref() != Some(stalled) {
            return false;
        }
        b.abandoned = true;
        true
    }

    fn current(&self) -> Option<(u64, String)> {
        lock(&self.busy).stat.clone()
    }
}

struct Threaded {
    jobs: Sender<WatchJob>,
    /// Kept so a replacement watcher can take the same queue (jobs sent while the old one was
    /// stuck are not lost).
    jobs_rx: Receiver<WatchJob>,
    core: Arc<Mutex<WatchCore>>,
    current: Arc<WatcherState>,
    handle: Option<JoinHandle<()>>,
    spawned: usize,
}

enum Runner {
    Thread(Threaded),
    Manual(WatchCore),
}

/// A volume that stalled or failed: skipped (its files keep their state) until it answers.
#[derive(Debug)]
struct Volume {
    /// When it is tried again (`None`: a watcher is stuck on it, or a trial is under way).
    retry_at: Option<i64>,
    /// The next wait before a retry.
    backoff_ms: i64,
}

/// The control thread's side of the watch.
pub struct MediaWatch {
    runner: Runner,
    fs: Arc<dyn WatchFs>,
    period: Duration,
    stall_ms: i64,
    retry_first_ms: i64,
    retry_max_ms: i64,
    clock: EpochClock,
    inbox: Sender<Inbox>,
    generation: u64,
    /// Volumes skipped because they stalled or failed, until they answer.
    volumes: BTreeMap<String, Volume>,
    /// Abandoned watchers whose stat has not returned yet (threads still alive).
    abandoned: usize,
    /// The current watcher is stuck in this stat and no more may be abandoned.
    stuck: Option<(u64, String)>,
    /// The current watcher's stat as first seen by the watchdog, and when: it is stalled once
    /// seen in that one stat for the limit. Timed here, not by the watcher, so the watcher
    /// reads no clock.
    observed: Option<((u64, String), i64)>,
    /// The watcher ended unexpectedly once and was restarted.
    restarted: bool,
    /// No watcher runs any more.
    dead: bool,
    /// Batches with events taken (a cost measure: an unchanged show sends none).
    batches: u64,
    logs: Vec<(LogLevel, String)>,
    notices_changed: bool,
}

impl MediaWatch {
    pub fn new(config: &WatchConfig, inbox: Sender<Inbox>) -> io::Result<Self> {
        let runner = match config.mode {
            WatchMode::Manual => Runner::Manual(WatchCore::new()),
            WatchMode::Thread => {
                let (jobs, jobs_rx) = crossbeam_channel::unbounded();
                Runner::Thread(Threaded {
                    jobs,
                    jobs_rx,
                    core: Arc::new(Mutex::new(WatchCore::new())),
                    current: Arc::new(WatcherState::default()),
                    handle: None,
                    spawned: 0,
                })
            }
        };
        let mut watch = Self {
            runner,
            fs: Arc::clone(&config.fs),
            period: config.period,
            stall_ms: millis(config.stall),
            retry_first_ms: millis(config.retry_first),
            retry_max_ms: millis(config.retry_max),
            clock: config.clock.clone(),
            inbox,
            generation: 0,
            volumes: BTreeMap::new(),
            abandoned: 0,
            stuck: None,
            observed: None,
            restarted: false,
            dead: false,
            batches: 0,
            logs: Vec::new(),
            notices_changed: false,
        };
        watch.spawn()?;
        Ok(watch)
    }

    /// Starts a watcher on the shared state and queue (thread mode).
    fn spawn(&mut self) -> io::Result<()> {
        let Runner::Thread(t) = &mut self.runner else {
            return Ok(());
        };
        let state = Arc::new(WatcherState::default());
        let (core, jobs, inbox, fs, st, period) = (
            Arc::clone(&t.core),
            t.jobs_rx.clone(),
            self.inbox.clone(),
            Arc::clone(&self.fs),
            Arc::clone(&state),
            self.period,
        );
        // Never joined: a watcher stuck in a stat on a dead volume must not hang shutdown. It
        // ends when its stat returns (abandoned) or the job queue closes (control is gone).
        let handle = thread::Builder::new()
            .name(if t.spawned == 0 {
                "openlab-watch".to_owned()
            } else {
                format!("openlab-watch-{}", t.spawned)
            })
            .spawn(move || watcher(&core, &jobs, &inbox, fs.as_ref(), &st, period))?;
        t.spawned += 1;
        t.current = state;
        t.handle = Some(handle);
        Ok(())
    }

    fn send(&mut self, job: WatchJob) {
        match &mut self.runner {
            Runner::Manual(core) => core.apply(job),
            // The receiver is held here too, so this cannot fail.
            Runner::Thread(t) => {
                let _ = t.jobs.send(job);
            }
        }
    }

    /// Turns the catalog's key changes into jobs. A reset starts a new generation: batches of
    /// the old one are dropped from now on.
    pub fn apply(&mut self, changes: Vec<KeyChange>) {
        for change in changes {
            let job = match change {
                KeyChange::Reset => {
                    self.generation += 1;
                    WatchJob::Reset {
                        generation: self.generation,
                    }
                }
                KeyChange::Watch { key, path, id } => WatchJob::Watch { key, path, id },
                KeyChange::Unwatch { key } => WatchJob::Unwatch { key },
                KeyChange::ProbeFailed { key, id, missing } => {
                    WatchJob::ProbeFailed { key, id, missing }
                }
            };
            self.send(job);
        }
    }

    /// The events of a batch of the current generation (`None`: an older one, dropped). The
    /// volumes on trial it reports count whatever its generation.
    pub fn accept(&mut self, batch: WatchBatch) -> Option<Vec<WatchEvent>> {
        for (bucket, healthy) in batch.volumes {
            self.volume_answered(&bucket, healthy);
        }
        if batch.generation != self.generation {
            return None;
        }
        if !batch.events.is_empty() {
            self.batches += 1;
        }
        Some(batch.events)
    }

    /// An abandoned watcher's stat returned (and its thread ended).
    pub fn answered(&mut self, bucket: &str, healthy: bool) {
        self.abandoned = self.abandoned.saturating_sub(1);
        self.volume_answered(bucket, healthy);
    }

    /// A skipped volume's stat came back: watched again if it answered; else it stays
    /// skipped and is tried again after the backoff.
    fn volume_answered(&mut self, bucket: &str, healthy: bool) {
        let now = self.clock.now_ms();
        let Some(v) = self.volumes.get_mut(bucket) else {
            return;
        };
        if healthy {
            self.volumes.remove(bucket);
            self.send(WatchJob::Unskip {
                bucket: bucket.to_owned(),
                trial: false,
            });
            self.logs.push((
                LogLevel::Info,
                format!("volume \"{bucket}\" answers again; its media files are watched again"),
            ));
            self.notices_changed = true;
            return;
        }
        if v.retry_at.is_some() {
            return;
        }
        let wait = v.backoff_ms;
        v.retry_at = Some(now.saturating_add(wait));
        v.backoff_ms = wait.saturating_mul(2).min(self.retry_max_ms);
        self.send(WatchJob::Skip {
            bucket: bucket.to_owned(),
        });
        self.logs.push((
            LogLevel::Warn,
            format!(
                "volume \"{bucket}\" answered with an error; it is tried again in {} s",
                wait.saturating_add(999) / 1000
            ),
        ));
    }

    /// Skips a volume that stalled (`retry_at` stays `None` until the stuck stat returns).
    fn skip_stalled(&mut self, bucket: &str) {
        let fresh = !self.volumes.contains_key(bucket);
        let v = self.volumes.entry(bucket.to_owned()).or_insert(Volume {
            retry_at: None,
            backoff_ms: self.retry_first_ms,
        });
        v.retry_at = None;
        self.send(WatchJob::Skip {
            bucket: bucket.to_owned(),
        });
        if fresh {
            self.logs.push((
                LogLevel::Warn,
                format!(
                    "volume \"{bucket}\" does not answer; its media files keep their last \
                     state until it does"
                ),
            ));
            self.notices_changed = true;
        }
    }

    /// One pass on the caller's thread (manual mode; `None` in thread mode or when nothing
    /// changed).
    pub fn manual_pass(&mut self) -> Option<WatchBatch> {
        let Runner::Manual(core) = &mut self.runner else {
            return None;
        };
        let events = core.pass(self.fs.as_ref());
        (!events.is_empty()).then(|| WatchBatch {
            generation: core.generation(),
            events,
            volumes: Vec::new(),
        })
    }

    /// The watchdog (every tick): a watcher stuck in one stat past the limit is abandoned and
    /// replaced by one that skips its volume; skipped volumes are tried again once their wait
    /// is over; a watcher that ended is restarted once.
    pub fn supervise(&mut self) {
        let Runner::Thread(t) = &mut self.runner else {
            return;
        };
        if self.dead {
            return;
        }
        if t.handle.as_ref().is_some_and(JoinHandle::is_finished) {
            t.handle = None;
            if self.restarted {
                self.dead = true;
                self.notices_changed = true;
                self.logs.push((
                    LogLevel::Error,
                    "the media watch thread ended again: missing media files are no longer \
                     noticed before their cues play"
                        .to_owned(),
                ));
            } else {
                self.restarted = true;
                self.logs.push((
                    LogLevel::Error,
                    "the media watch thread ended unexpectedly; it was restarted".to_owned(),
                ));
                self.restart();
            }
            return;
        }
        let now = self.clock.now_ms();
        let current = Arc::clone(&t.current);
        let busy = current.current();
        if self.observed.as_ref().map(|(b, _)| b) != busy.as_ref() {
            self.observed = busy.clone().map(|b| (b, now));
        }
        let stalled = self
            .observed
            .as_ref()
            .filter(|(_, seen)| now.saturating_sub(*seen) >= self.stall_ms)
            .map(|(b, _)| b.clone());
        if let Some(stuck) = self.stuck.clone() {
            // Stuck with no watcher to spare: once that stat returns, its volume is skipped
            // like a failing one, so it cannot hold the only watcher again.
            if busy.as_ref() != Some(&stuck) {
                self.stuck = None;
                self.notices_changed = true;
                let bucket = stuck.1;
                self.skip_stalled(&bucket);
                self.volume_answered(&bucket, false);
            }
        } else if self.abandoned >= MAX_ABANDONED_WATCHERS {
            if let Some((stat, bucket)) = stalled {
                self.logs.push((
                    LogLevel::Error,
                    format!(
                        "volume \"{bucket}\" does not answer and {MAX_ABANDONED_WATCHERS} \
                         media watchers are already stuck: the media watch stops until it \
                         answers"
                    ),
                ));
                self.stuck = Some((stat, bucket));
                self.notices_changed = true;
            }
        } else if let Some(stat) = stalled
            && current.try_abandon(&stat)
        {
            let bucket = stat.1;
            self.observed = None;
            self.abandoned += 1;
            // Queued before the fresh watcher starts: it skips the volume from its first
            // chunk.
            self.skip_stalled(&bucket);
            self.restart();
        }
        let due: Vec<String> = self
            .volumes
            .iter()
            .filter(|(_, v)| v.retry_at.is_some_and(|at| now >= at))
            .map(|(b, _)| b.clone())
            .collect();
        for bucket in due {
            if let Some(v) = self.volumes.get_mut(&bucket) {
                v.retry_at = None;
            }
            self.send(WatchJob::Unskip {
                bucket,
                trial: true,
            });
        }
    }

    fn restart(&mut self) {
        if let Err(e) = self.spawn() {
            self.dead = true;
            self.notices_changed = true;
            self.logs.push((
                LogLevel::Error,
                format!("could not start a media watch thread: {e}"),
            ));
        }
    }

    /// Log entries for the show log and clients, taken once.
    pub fn take_logs(&mut self) -> Vec<(LogLevel, String)> {
        std::mem::take(&mut self.logs)
    }

    /// The notices changed since this was last asked.
    pub fn take_notices_changed(&mut self) -> bool {
        std::mem::take(&mut self.notices_changed)
    }

    /// The workspace-level notices in force. A volume's notice stands from its first stall
    /// until it answers, through its retries.
    pub fn notices(&self, list: &mut Vec<WireWarning>) {
        for bucket in self.volumes.keys() {
            list.push(notice(
                "media_volume_not_responding",
                format!(
                    "volume \"{bucket}\" does not answer: changes to its media files are not \
                     noticed until it does"
                ),
                None,
            ));
        }
        if let Some((_, bucket)) = &self.stuck {
            list.push(notice(
                "media_watch_stopped",
                format!(
                    "media files are not watched while volume \"{bucket}\" does not answer: a \
                     missing file is noticed only when its cue plays"
                ),
                None,
            ));
        } else if self.dead {
            list.push(notice(
                "media_watch_stopped",
                "media files are no longer watched: a missing file is noticed only when its cue \
                 plays; restart openLab"
                    .to_owned(),
                None,
            ));
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn batches(&self) -> u64 {
        self.batches
    }

    pub fn abandoned(&self) -> usize {
        self.abandoned
    }
}

/// What a watcher does next.
enum Step {
    Go,
    /// Abandoned, or control is gone: end the thread.
    End,
}

/// Stats `targets` the way the watcher does: jobs taken and the volume's skip checked before
/// each stat (a volume skipped meanwhile is left at once, not after the chunk), the busy state
/// around it, then the record.
fn stat_targets(
    core: &Mutex<WatchCore>,
    jobs: &Receiver<WatchJob>,
    inbox: &Sender<Inbox>,
    fs: &dyn WatchFs,
    state: &WatcherState,
    targets: Vec<Target>,
) -> Step {
    for t in targets {
        if !take_jobs(core, jobs) {
            return Step::End;
        }
        if lock(core).is_skipped(&t.bucket) {
            continue;
        }
        if !state.begin(&t.bucket) {
            return Step::End;
        }
        let stat = fs.stat(&t.path);
        if state.end() {
            // A fresh watcher took over and skips this volume until now.
            let _ = inbox.send(Inbox::Watch(WatchAnswer::BucketAnswered {
                healthy: answers(&stat),
                bucket: t.bucket,
            }));
            return Step::End;
        }
        lock(core).record(&t, stat);
    }
    Step::Go
}

/// The `openlab-watch` thread: chunks of stats paced over the pass, jobs taken between them.
fn watcher(
    core: &Mutex<WatchCore>,
    jobs: &Receiver<WatchJob>,
    inbox: &Sender<Inbox>,
    fs: &dyn WatchFs,
    state: &WatcherState,
    period: Duration,
) {
    loop {
        // Abandoned: the queue belongs to the fresh watcher now.
        if state.abandoned() || !take_jobs(core, jobs) {
            return;
        }
        let (chunk, n) = {
            let mut c = lock(core);
            (c.next_chunk(CHUNK), c.len())
        };
        match chunk {
            Some(targets) => {
                if let Step::End = stat_targets(core, jobs, inbox, fs, state, targets) {
                    return;
                }
                if state.abandoned() || !wait_jobs(core, jobs, chunk_gap(n, period)) {
                    return;
                }
            }
            None => {
                let batch = lock(core).finish();
                if let Some(batch) = batch
                    && inbox.send(Inbox::Watch(WatchAnswer::Batch(batch))).is_err()
                {
                    return;
                }
                // Nothing to watch: wait for jobs a whole period rather than spin.
                if n == 0 && !wait_jobs(core, jobs, period) {
                    return;
                }
            }
        }
    }
}

/// A whole pass the way the watcher makes it (jobs, skip checks, busy state, locks),
/// without its pauses: what the release perf gate times.
#[cfg(any(test, feature = "test-support"))]
pub fn timed_pass(core: &Mutex<WatchCore>, fs: &dyn WatchFs) {
    let (_tx, jobs) = crossbeam_channel::unbounded();
    let (inbox, _rx) = crossbeam_channel::unbounded();
    let state = WatcherState::default();
    loop {
        let chunk = lock(core).next_chunk(CHUNK);
        let Some(targets) = chunk else { break };
        if let Step::End = stat_targets(core, &jobs, &inbox, fs, &state, targets) {
            break;
        }
    }
    lock(core).finish();
}

/// Applies the jobs queued; `false` once control is gone.
fn take_jobs(core: &Mutex<WatchCore>, jobs: &Receiver<WatchJob>) -> bool {
    loop {
        match jobs.try_recv() {
            Ok(job) => lock(core).apply(job),
            Err(TryRecvError::Empty) => return true,
            Err(TryRecvError::Disconnected) => return false,
        }
    }
}

/// Waits `gap`, applying jobs as they come; `false` once control is gone.
fn wait_jobs(core: &Mutex<WatchCore>, jobs: &Receiver<WatchJob>, gap: Duration) -> bool {
    let deadline = Instant::now() + gap;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match jobs.recv_timeout(left) {
            Ok(job) => lock(core).apply(job),
            Err(RecvTimeoutError::Timeout) => return true,
            Err(RecvTimeoutError::Disconnected) => return false,
        }
    }
}

/// Test support (feature `test-support`): not in the shipping engine.
#[cfg(any(test, feature = "test-support"))]
pub use fake::{FakeFs, Release};

#[cfg(any(test, feature = "test-support"))]
mod fake {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::sync::Condvar;

    /// An in-memory file system for tests: files by path with an identity, a size and a modified
    /// time; the Trash; paths that refuse access or hang.
    #[derive(Clone, Default)]
    pub struct FakeFs(Arc<FakeShared>);

    #[derive(Default)]
    struct FakeShared {
        state: Mutex<FakeState>,
        changed: Condvar,
    }

    #[derive(Default)]
    struct FakeState {
        files: HashMap<PathBuf, FakeFile>,
        trash: HashMap<PathBuf, FakeFile>,
        denied: HashSet<PathBuf>,
        hung: HashSet<PathBuf>,
        /// Stats blocked on a hung path now.
        hanging: HashMap<PathBuf, usize>,
        next_id: u64,
        stats: u64,
    }

    #[derive(Clone, Copy, Debug)]
    struct FakeFile {
        dev: u64,
        ino: u64,
        size: u64,
        modified_ms: i64,
        symlink: bool,
    }

    /// A hung path; dropping it (or [`Release::release`]) lets its stats return.
    pub struct Release {
        fs: FakeFs,
        path: PathBuf,
    }

    impl Release {
        pub fn release(self) {}
    }

    impl Drop for Release {
        fn drop(&mut self) {
            lock(&self.fs.0.state).hung.remove(&self.path);
            self.fs.0.changed.notify_all();
        }
    }

    impl fmt::Debug for FakeFs {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("FakeFs")
        }
    }

    impl FakeFs {
        pub fn new() -> Self {
            Self::default()
        }

        fn with<R>(&self, f: impl FnOnce(&mut FakeState) -> R) -> R {
            let r = f(&mut lock(&self.0.state));
            self.0.changed.notify_all();
            r
        }

        /// A new file at `path` on device `dev` (a fresh identity).
        pub fn put(&self, path: impl AsRef<Path>, dev: u64) {
            self.with(|s| {
                s.next_id += 1;
                let ino = s.next_id;
                s.files.insert(
                    path.as_ref().to_path_buf(),
                    FakeFile {
                        dev,
                        ino,
                        size: 1000,
                        modified_ms: 1_000_000,
                        symlink: false,
                    },
                );
            });
        }

        pub fn rename(&self, from: impl AsRef<Path>, to: impl AsRef<Path>) {
            self.with(|s| {
                if let Some(f) = s.files.remove(from.as_ref()) {
                    s.files.insert(to.as_ref().to_path_buf(), f);
                }
            });
        }

        pub fn remove(&self, path: impl AsRef<Path>) {
            self.with(|s| s.files.remove(path.as_ref()));
        }

        /// Moves the file to the Trash (remembering where it was, for [`Self::put_back`]).
        pub fn trash(&self, path: impl AsRef<Path>) {
            self.with(|s| {
                if let Some(f) = s.files.remove(path.as_ref()) {
                    s.trash.insert(path.as_ref().to_path_buf(), f);
                }
            });
        }

        pub fn put_back(&self, path: impl AsRef<Path>) {
            self.with(|s| {
                if let Some(f) = s.trash.remove(path.as_ref()) {
                    s.files.insert(path.as_ref().to_path_buf(), f);
                }
            });
        }

        /// Another file takes the place of the one at `path` (a new identity and size).
        pub fn replace(&self, path: impl AsRef<Path>) {
            self.with(|s| {
                s.next_id += 1;
                let ino = s.next_id;
                if let Some(f) = s.files.get_mut(path.as_ref()) {
                    f.ino = ino;
                    f.size += 1;
                    f.modified_ms += 1;
                }
            });
        }

        /// The file was written to (a later modified time).
        pub fn touch(&self, path: impl AsRef<Path>) {
            self.with(|s| {
                if let Some(f) = s.files.get_mut(path.as_ref()) {
                    f.modified_ms += 1;
                }
            });
        }

        pub fn set_size(&self, path: impl AsRef<Path>, size: u64) {
            self.with(|s| {
                if let Some(f) = s.files.get_mut(path.as_ref()) {
                    f.size = size;
                }
            });
        }

        pub fn set_symlink(&self, path: impl AsRef<Path>, symlink: bool) {
            self.with(|s| {
                if let Some(f) = s.files.get_mut(path.as_ref()) {
                    f.symlink = symlink;
                }
            });
        }

        /// Stats of `path` fail with `PermissionDenied`.
        pub fn deny(&self, path: impl AsRef<Path>) {
            self.with(|s| s.denied.insert(path.as_ref().to_path_buf()));
        }

        pub fn allow(&self, path: impl AsRef<Path>) {
            self.with(|s| s.denied.remove(path.as_ref()));
        }

        /// Stats of `path` block until the returned [`Release`] goes.
        pub fn hang(&self, path: impl AsRef<Path>) -> Release {
            self.with(|s| s.hung.insert(path.as_ref().to_path_buf()));
            Release {
                fs: self.clone(),
                path: path.as_ref().to_path_buf(),
            }
        }

        /// Waits (at most `limit`) until a stat is blocked on `path`; whether one is.
        pub fn wait_hanging(&self, path: impl AsRef<Path>, limit: Duration) -> bool {
            let deadline = Instant::now() + limit;
            let mut s = lock(&self.0.state);
            loop {
                if s.hanging.get(path.as_ref()).is_some_and(|n| *n > 0) {
                    return true;
                }
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return false;
                }
                s = self
                    .0
                    .changed
                    .wait_timeout(s, left)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
        }

        /// Waits (at most `limit`) until `n` more stats have been made than `after`; whether they
        /// were.
        pub fn wait_stats(&self, after: u64, n: u64, limit: Duration) -> bool {
            let deadline = Instant::now() + limit;
            let mut s = lock(&self.0.state);
            loop {
                if s.stats >= after.saturating_add(n) {
                    return true;
                }
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return false;
                }
                s = self
                    .0
                    .changed
                    .wait_timeout(s, left)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
        }

        /// Stats made so far.
        pub fn stats(&self) -> u64 {
            lock(&self.0.state).stats
        }
    }

    impl WatchFs for FakeFs {
        fn stat(&self, path: &Path) -> io::Result<Stat> {
            let mut s = lock(&self.0.state);
            s.stats += 1;
            if s.hung.contains(path) {
                *s.hanging.entry(path.to_path_buf()).or_default() += 1;
                self.0.changed.notify_all();
                while s.hung.contains(path) {
                    s = self
                        .0
                        .changed
                        .wait(s)
                        .unwrap_or_else(PoisonError::into_inner);
                }
                if let Some(n) = s.hanging.get_mut(path) {
                    *n = n.saturating_sub(1);
                }
            }
            self.0.changed.notify_all();
            if s.denied.contains(path) {
                return Err(io::Error::from(io::ErrorKind::PermissionDenied));
            }
            let f = s
                .files
                .get(path)
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            Ok(Stat {
                fp: Fingerprint {
                    dev: f.dev,
                    ino: f.ino,
                    size: f.size,
                    modified_ms: f.modified_ms,
                },
                regular: true,
                symlink: f.symlink,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watch(core: &mut WatchCore, key: &str, id: u64) {
        core.apply(WatchJob::Watch {
            key: key.to_owned(),
            path: PathBuf::from(key),
            id,
        });
    }

    /// A core watching `keys` (ids 1..), with every file present and the first pass made.
    fn setup(keys: &[&str]) -> (WatchCore, FakeFs) {
        let fs = FakeFs::new();
        let mut core = WatchCore::new();
        for (i, k) in keys.iter().enumerate() {
            fs.put(k, 1);
            watch(&mut core, k, i as u64 + 1);
        }
        assert!(core.pass(&fs).is_empty(), "the first pass is silent");
        (core, fs)
    }

    const A: &str = "/show/a.wav";
    const B: &str = "/show/b.wav";

    #[test]
    fn an_unchanged_pass_finds_nothing() {
        let (mut core, fs) = setup(&[A, B]);
        for _ in 0..3 {
            assert!(core.pass(&fs).is_empty());
        }
        assert_eq!(core.finish(), None);
    }

    #[test]
    fn a_file_gone_is_reported_on_the_first_pass_once() {
        let (mut core, fs) = setup(&[A, B]);
        fs.trash(A);
        assert_eq!(
            core.pass(&fs),
            vec![WatchEvent::Gone {
                key: A.into(),
                id: 1,
                trashed: false
            }]
        );
        assert!(core.pass(&fs).is_empty());
    }

    #[test]
    fn a_file_missing_from_the_start_is_gone_on_the_first_pass() {
        let fs = FakeFs::new();
        let mut core = WatchCore::new();
        watch(&mut core, A, 7);
        assert_eq!(
            core.pass(&fs),
            vec![WatchEvent::Gone {
                key: A.into(),
                id: 7,
                trashed: false
            }]
        );
    }

    #[test]
    fn a_file_back_is_reported_after_two_identical_passes() {
        let (mut core, fs) = setup(&[A]);
        fs.trash(A);
        assert_eq!(core.pass(&fs).len(), 1);
        fs.put_back(A);
        assert!(core.pass(&fs).is_empty(), "once is not enough");
        let back = core.pass(&fs);
        assert!(
            matches!(&back[..], [WatchEvent::Back { key, id: 1, .. }] if key == A),
            "{back:?}"
        );
        assert!(core.pass(&fs).is_empty());
    }

    #[test]
    fn a_file_still_being_copied_is_not_back() {
        let (mut core, fs) = setup(&[A]);
        fs.remove(A);
        assert_eq!(core.pass(&fs).len(), 1);
        fs.put(A, 1);
        for size in [10, 20, 30, 40] {
            fs.set_size(A, size);
            assert!(core.pass(&fs).is_empty(), "size {size} still growing");
        }
        assert!(matches!(&core.pass(&fs)[..], [WatchEvent::Back { .. }]));
    }

    #[test]
    fn a_changed_file_is_reported_after_two_identical_passes() {
        let (mut core, fs) = setup(&[A]);
        fs.replace(A);
        assert!(core.pass(&fs).is_empty());
        let changed = core.pass(&fs);
        assert!(
            matches!(&changed[..], [WatchEvent::Changed { key, .. }] if key == A),
            "{changed:?}"
        );
        assert!(core.pass(&fs).is_empty());
        fs.touch(A);
        assert!(core.pass(&fs).is_empty());
        assert!(matches!(&core.pass(&fs)[..], [WatchEvent::Changed { .. }]));
    }

    #[test]
    fn a_change_undone_before_the_second_pass_is_not_reported() {
        let (mut core, fs) = setup(&[A]);
        fs.set_size(A, 5);
        assert!(core.pass(&fs).is_empty());
        fs.set_size(A, 1000);
        assert!(core.pass(&fs).is_empty());
        assert!(core.pass(&fs).is_empty());
    }

    #[test]
    fn unreadable_is_reported_once_per_change() {
        let (mut core, fs) = setup(&[A]);
        fs.deny(A);
        let ev = core.pass(&fs);
        assert!(
            matches!(&ev[..], [WatchEvent::Unreadable { key, id: 1, .. }] if key == A),
            "{ev:?}"
        );
        assert!(core.pass(&fs).is_empty());
        assert!(core.pass(&fs).is_empty());
        // Readable again, unchanged: the re-probe on Unreadable already read it (one blip,
        // one re-probe).
        fs.allow(A);
        assert!(core.pass(&fs).is_empty());
        assert!(core.pass(&fs).is_empty());
        // A different file when it comes back: Back.
        fs.deny(A);
        assert!(matches!(
            &core.pass(&fs)[..],
            [WatchEvent::Unreadable { .. }]
        ));
        fs.replace(A);
        fs.allow(A);
        assert!(core.pass(&fs).is_empty());
        assert!(matches!(&core.pass(&fs)[..], [WatchEvent::Back { .. }]));
        fs.deny(A);
        assert!(matches!(
            &core.pass(&fs)[..],
            [WatchEvent::Unreadable { .. }]
        ));
        fs.allow(A);
        fs.remove(A);
        assert!(matches!(&core.pass(&fs)[..], [WatchEvent::Gone { .. }]));
    }

    #[test]
    fn a_return_after_a_failed_reprobe_is_reported_even_unchanged() {
        let (mut core, fs) = setup(&[A]);
        fs.deny(A);
        assert_eq!(core.pass(&fs).len(), 1);
        // Its re-probe could not read it either.
        core.apply(WatchJob::ProbeFailed {
            key: A.into(),
            id: 1,
            missing: false,
        });
        fs.allow(A);
        assert!(core.pass(&fs).is_empty());
        assert!(matches!(&core.pass(&fs)[..], [WatchEvent::Back { .. }]));
        // A failed probe of a readable file changes nothing (no re-probe loop on a file that
        // stats but cannot be decoded).
        core.apply(WatchJob::ProbeFailed {
            key: A.into(),
            id: 1,
            missing: false,
        });
        for _ in 0..3 {
            assert!(core.pass(&fs).is_empty());
        }
    }

    #[test]
    fn a_file_whose_probe_found_nothing_is_reported_back_however_soon_it_appears() {
        // Its first stat after the probe failed: still reported (a late-mounting drive).
        let fs = FakeFs::new();
        let mut core = WatchCore::new();
        watch(&mut core, A, 1);
        core.apply(WatchJob::ProbeFailed {
            key: A.into(),
            id: 1,
            missing: true,
        });
        fs.put(A, 1);
        assert!(core.pass(&fs).is_empty());
        assert!(matches!(
            &core.pass(&fs)[..],
            [WatchEvent::Back { id: 1, .. }]
        ));
        // Seen silently on the first pass, then the probe said missing: the same.
        let (mut core, fs) = setup(&[B]);
        core.apply(WatchJob::ProbeFailed {
            key: B.into(),
            id: 1,
            missing: true,
        });
        assert!(core.pass(&fs).is_empty());
        assert!(matches!(&core.pass(&fs)[..], [WatchEvent::Back { .. }]));
        // A failure of an earlier watch of the key changes nothing; still gone: no event.
        core.apply(WatchJob::ProbeFailed {
            key: B.into(),
            id: 99,
            missing: true,
        });
        assert!(core.pass(&fs).is_empty());
        let fs2 = FakeFs::new();
        let mut core = WatchCore::new();
        watch(&mut core, A, 1);
        core.apply(WatchJob::ProbeFailed {
            key: A.into(),
            id: 1,
            missing: true,
        });
        assert!(core.pass(&fs2).is_empty(), "control knows it is missing");
    }

    #[test]
    fn a_volume_on_trial_reports_its_first_stat() {
        let a = "/Volumes/Show/a.wav";
        let b = "/Volumes/Show/b.wav";
        let (mut core, fs) = setup(&[a, b]);
        core.apply(WatchJob::Skip {
            bucket: "/Volumes/Show".into(),
        });
        assert_eq!(core.finish(), None);
        core.apply(WatchJob::Unskip {
            bucket: "/Volumes/Show".into(),
            trial: true,
        });
        fs.deny(a);
        while let Some(chunk) = core.next_chunk(CHUNK) {
            for t in chunk {
                let stat = fs.stat(&t.path);
                core.record(&t, stat);
            }
        }
        let batch = core.finish().unwrap();
        assert_eq!(batch.volumes, vec![("/Volumes/Show".to_owned(), false)]);
        assert!(core.pass(&fs).is_empty());
    }

    #[test]
    fn abandonment_and_a_returning_stat_never_both_miss() {
        // The stat returns before the watchdog acts on it: not abandoned.
        let w = WatcherState::default();
        assert!(w.begin("/Volumes/A"));
        let seen = w.current().unwrap();
        assert!(!w.end());
        assert!(!w.try_abandon(&seen));
        // Another stat of the same volume since: not the one judged stalled either.
        assert!(w.begin("/Volumes/A"));
        assert!(!w.try_abandon(&seen));
        // The watchdog first: the returning stat sees it and reports its own volume.
        let seen = w.current().unwrap();
        assert!(w.try_abandon(&seen));
        assert!(w.end());
        assert!(!w.begin("/Volumes/B"), "an abandoned watcher stats nothing");
        assert!(w.abandoned());
    }

    #[test]
    fn a_symlink_is_recorded_and_watched_through() {
        let (mut core, fs) = setup(&[A]);
        fs.set_symlink(A, true);
        assert!(core.pass(&fs).is_empty());
        assert!(core.items.get(A).is_some_and(|i| i.symlink));
    }

    #[test]
    fn locate_is_not_used_yet() {
        assert_eq!(
            RealFs.locate(&Fingerprint {
                dev: 1,
                ino: 2,
                size: 3,
                modified_ms: 4
            }),
            None
        );
    }

    #[test]
    fn an_unwatch_mid_pass_drops_its_event() {
        let (mut core, fs) = setup(&[A, B]);
        fs.remove(A);
        fs.remove(B);
        // The first chunk holds both files: record A, then A is unwatched before the pass ends.
        let chunk = core.next_chunk(1).unwrap();
        for t in &chunk {
            core.record(t, fs.stat(&t.path));
        }
        core.apply(WatchJob::Unwatch { key: A.into() });
        let rest = core.next_chunk(CHUNK).unwrap();
        for t in &rest {
            core.record(t, fs.stat(&t.path));
        }
        assert!(core.next_chunk(CHUNK).is_none());
        let batch = core.finish().unwrap();
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.events[0].key(), B);
    }

    #[test]
    fn a_stat_of_an_earlier_watch_of_the_key_is_ignored() {
        let (mut core, fs) = setup(&[A]);
        let chunk = core.next_chunk(CHUNK).unwrap();
        fs.remove(A);
        // Unwatched and watched again (a new id) while the stat was under way.
        core.apply(WatchJob::Unwatch { key: A.into() });
        watch(&mut core, A, 9);
        for t in &chunk {
            core.record(t, fs.stat(&t.path));
        }
        assert!(core.next_chunk(CHUNK).is_none());
        assert_eq!(core.finish(), None);
        assert_eq!(
            core.pass(&fs),
            vec![WatchEvent::Gone {
                key: A.into(),
                id: 9,
                trashed: false
            }]
        );
    }

    #[test]
    fn a_reset_forgets_everything_and_carries_the_generation() {
        let (mut core, fs) = setup(&[A]);
        fs.remove(A);
        core.apply(WatchJob::Reset { generation: 3 });
        assert!(core.is_empty());
        watch(&mut core, A, 2);
        let chunk = core.next_chunk(CHUNK).unwrap();
        for t in &chunk {
            core.record(t, fs.stat(&t.path));
        }
        assert!(core.next_chunk(CHUNK).is_none());
        assert_eq!(core.finish().map(|b| b.generation), Some(3));
    }

    #[test]
    fn a_skipped_volume_keeps_its_state() {
        let a = "/Volumes/Show/a.wav";
        let (mut core, fs) = setup(&[a, B]);
        core.apply(WatchJob::Skip {
            bucket: "/Volumes/Show".into(),
        });
        fs.remove(a);
        fs.remove(B);
        let ev = core.pass(&fs);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].key(), B);
        core.apply(WatchJob::Unskip {
            bucket: "/Volumes/Show".into(),
            trial: false,
        });
        let ev = core.pass(&fs);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].key(), a);
    }

    #[test]
    fn chunks_cover_every_file_once_per_pass() {
        let fs = FakeFs::new();
        let mut core = WatchCore::new();
        for i in 0..200 {
            let k = format!("/show/{i:03}.wav");
            fs.put(&k, 1);
            watch(&mut core, &k, i + 1);
        }
        let mut seen = 0;
        let mut chunks = 0;
        while let Some(c) = core.next_chunk(CHUNK) {
            assert!(c.len() <= CHUNK);
            seen += c.len();
            chunks += 1;
        }
        assert_eq!((seen, chunks), (200, 4));
        // The next call starts the next pass.
        assert_eq!(core.next_chunk(CHUNK).map(|c| c.len()), Some(CHUNK));
    }

    #[test]
    fn pacing_spreads_a_pass_over_its_period() {
        let min = MIN_PERIOD;
        assert_eq!(pass_period(10, min), min);
        assert_eq!(pass_period(3_000, min), Duration::from_secs(3));
        assert_eq!(pass_period(10_000, min), Duration::from_secs(10));
        for n in [0usize, 1, 64, 65, 2_000, 3_000, 10_000] {
            let chunks = n.div_ceil(CHUNK).max(1) as u32;
            let total = chunk_gap(n, min) * chunks;
            // Integer division may round the gap down by under a nanosecond per chunk.
            assert!(
                total + Duration::from_nanos(u64::from(chunks)) >= pass_period(n, min),
                "{n}: {total:?}"
            );
            assert!(total <= pass_period(n, min));
        }
        assert!(chunk_gap(3_000, min) * 47 >= Duration::from_millis(2_999));
    }

    #[test]
    fn buckets_name_the_volume() {
        for (path, bucket) in [
            ("/Volumes/Show Drive/a/b.wav", "/Volumes/Show Drive"),
            ("/Volumes/x.wav", "/"),
            ("/Users/me/a.wav", "/"),
            ("/media/me/USB/a.wav", "/media/me/USB"),
            ("/run/media/me/USB/a.wav", "/run/media/me/USB"),
            ("/mnt/nas/a.wav", "/mnt/nas"),
            ("relative.wav", "/"),
        ] {
            assert_eq!(bucket_of(Path::new(path)), bucket, "{path}");
        }
        #[cfg(windows)]
        {
            assert_eq!(bucket_of(Path::new(r"D:\show\a.wav")), "D:");
            assert_eq!(bucket_of(Path::new(r"\\nas\share\a.wav")), r"\\nas\share");
        }
    }

    #[test]
    fn real_fs_stats_a_file_and_reports_missing_and_folders() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.wav");
        std::fs::write(&f, b"1234").unwrap();
        let st = RealFs.stat(&f).unwrap();
        assert!(st.regular && !st.symlink);
        assert_eq!(st.fp.size, 4);
        assert!(!RealFs.stat(dir.path()).unwrap().regular);
        let e = RealFs.stat(&dir.path().join("gone.wav")).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        #[cfg(unix)]
        {
            let link = dir.path().join("link.wav");
            std::os::unix::fs::symlink(&f, &link).unwrap();
            let st = RealFs.stat(&link).unwrap();
            assert!(st.regular && st.symlink);
            // A path through a file is not a folder: the file is gone, not unreadable.
            let e = RealFs.stat(&f.join("x.wav")).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::NotADirectory);
        }
    }

    // ----- the control side: generations, the watchdog -----

    fn manual() -> (MediaWatch, Receiver<Inbox>, FakeFs) {
        let fs = FakeFs::new();
        let (tx, rx) = crossbeam_channel::unbounded();
        let config = WatchConfig {
            fs: Arc::new(fs.clone()),
            mode: WatchMode::Manual,
            clock: EpochClock::new(|| 0),
            ..WatchConfig::default()
        };
        let w = MediaWatch::new(&config, tx).unwrap();
        (w, rx, fs)
    }

    #[test]
    fn a_failing_volume_waits_30_s_doubling_to_5_min_and_its_notice_stays() {
        let (mut w, _rx, _fs) = manual();
        w.skip_stalled("/Volumes/NAS");
        assert!(w.take_notices_changed());
        let mut waits = Vec::new();
        for _ in 0..6 {
            w.volume_answered("/Volumes/NAS", false);
            waits.push(w.volumes["/Volumes/NAS"].retry_at.unwrap());
            // Its retry was sent and failed again.
            w.volumes.get_mut("/Volumes/NAS").unwrap().retry_at = None;
        }
        assert_eq!(
            waits,
            vec![30_000, 60_000, 120_000, 240_000, 300_000, 300_000]
        );
        assert!(!w.take_notices_changed(), "no flapping while it fails");
        let mut list = Vec::new();
        w.notices(&mut list);
        assert_eq!(list.len(), 1);
        // An answer from another volume changes nothing; its own clears it.
        w.volume_answered("/Volumes/Other", true);
        w.volume_answered("/Volumes/NAS", true);
        assert!(w.take_notices_changed());
        list.clear();
        w.notices(&mut list);
        assert!(list.is_empty());
    }

    #[test]
    fn a_batch_of_an_older_generation_is_dropped() {
        let (mut w, _rx, fs) = manual();
        w.apply(vec![
            KeyChange::Reset,
            KeyChange::Watch {
                key: A.into(),
                path: A.into(),
                id: 1,
            },
        ]);
        let batch = w.manual_pass().unwrap();
        assert_eq!(batch.generation, 1);
        fs.put(A, 1);
        w.apply(vec![KeyChange::Reset]);
        assert_eq!(w.accept(batch), None);
        assert_eq!(w.batches(), 0);
        assert_eq!(w.manual_pass(), None);
    }

    struct Panicky;
    impl WatchFs for Panicky {
        fn stat(&self, _: &Path) -> io::Result<Stat> {
            panic!("a bug in the watcher")
        }
    }

    fn wait_finished(w: &MediaWatch) {
        let Runner::Thread(t) = &w.runner else {
            panic!("not threaded")
        };
        let h = t.handle.as_ref().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !h.is_finished() {
            assert!(Instant::now() < deadline, "the watcher did not end");
            thread::yield_now();
        }
    }

    #[test]
    fn a_watcher_that_ends_is_restarted_once_then_the_watch_stops() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let config = WatchConfig {
            fs: Arc::new(Panicky),
            period: Duration::from_millis(1),
            clock: EpochClock::new(|| 0),
            ..WatchConfig::default()
        };
        let mut w = MediaWatch::new(&config, tx).unwrap();
        w.apply(vec![KeyChange::Watch {
            key: A.into(),
            path: A.into(),
            id: 1,
        }]);
        wait_finished(&w);
        w.supervise();
        assert!(!w.dead);
        let logs = w.take_logs();
        assert!(
            logs.iter()
                .any(|(l, m)| *l == LogLevel::Error && m.contains("restarted"))
        );
        let mut list = Vec::new();
        w.notices(&mut list);
        assert!(list.is_empty());
        wait_finished(&w);
        w.supervise();
        assert!(w.take_notices_changed());
        w.notices(&mut list);
        assert!(
            matches!(&list[..], [WireWarning::Notice { code, .. }] if code == "media_watch_stopped")
        );
        // Nothing more happens.
        w.supervise();
        assert!(w.take_logs().iter().any(|(l, _)| *l == LogLevel::Error));
        assert!(w.take_logs().is_empty());
    }
}
