// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

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

pub(super) fn millis(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

#[derive(Default)]
pub(super) struct Busy {
    /// The stat under way: its number (each stat has its own) and its volume.
    stat: Option<(u64, String)>,
    stats: u64,
    abandoned: bool,
}

/// A watcher as the watchdog sees it. The stat under way and the abandonment are read and
/// changed under one lock, so a stat that returns just as the watchdog fires is either taken
/// as answered (not abandoned) or reports its volume (abandoned), never neither.
#[derive(Default)]
pub(super) struct WatcherState {
    busy: Mutex<Busy>,
}

impl WatcherState {
    pub(super) fn abandoned(&self) -> bool {
        lock(&self.busy).abandoned
    }

    /// A stat starts; `false` if this watcher was abandoned (it must not stat).
    pub(super) fn begin(&self, bucket: &str) -> bool {
        let mut b = lock(&self.busy);
        if b.abandoned {
            return false;
        }
        b.stats = b.stats.wrapping_add(1);
        b.stat = Some((b.stats, bucket.to_owned()));
        true
    }

    /// The stat returned; whether the watcher was abandoned meanwhile.
    pub(super) fn end(&self) -> bool {
        let mut b = lock(&self.busy);
        b.stat = None;
        b.abandoned
    }

    /// The watchdog: abandons the watcher if it is still in the very stat the watchdog judged
    /// stalled (compare and set: a stat that returned meanwhile is not abandoned).
    pub(super) fn try_abandon(&self, stalled: &(u64, String)) -> bool {
        let mut b = lock(&self.busy);
        if b.stat.as_ref() != Some(stalled) {
            return false;
        }
        b.abandoned = true;
        true
    }

    pub(super) fn current(&self) -> Option<(u64, String)> {
        lock(&self.busy).stat.clone()
    }
}

pub(super) struct Threaded {
    jobs: Sender<WatchJob>,
    /// Kept so a replacement watcher can take the same queue (jobs sent while the old one was
    /// stuck are not lost).
    jobs_rx: Receiver<WatchJob>,
    core: Arc<Mutex<WatchCore>>,
    current: Arc<WatcherState>,
    pub(super) handle: Option<JoinHandle<()>>,
    spawned: usize,
}

pub(super) enum Runner {
    Thread(Threaded),
    Manual(WatchCore),
}

/// A volume that stalled or failed: skipped (its files keep their state) until it answers.
#[derive(Debug)]
pub(super) struct Volume {
    /// When it is tried again (`None`: a watcher is stuck on it, or a trial is under way).
    pub(super) retry_at: Option<i64>,
    /// The next wait before a retry.
    backoff_ms: i64,
}

/// The control thread's side of the watch.
pub struct MediaWatch {
    pub(super) runner: Runner,
    fs: Arc<dyn WatchFs>,
    period: Duration,
    stall_ms: i64,
    retry_first_ms: i64,
    retry_max_ms: i64,
    clock: EpochClock,
    inbox: Sender<Inbox>,
    generation: u64,
    /// Volumes skipped because they stalled or failed, until they answer.
    pub(super) volumes: BTreeMap<String, Volume>,
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
    pub(super) dead: bool,
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
    pub(super) fn volume_answered(&mut self, bucket: &str, healthy: bool) {
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
    pub(super) fn skip_stalled(&mut self, bucket: &str) {
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

    /// The volumes skipped now (stalled or failing): the probes defer to them too, so the
    /// watch's notice is the only one.
    pub fn stalled_volumes(&self) -> impl Iterator<Item = &str> + Clone {
        self.volumes.keys().map(String::as_str)
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
pub(super) enum Step {
    Go,
    /// Abandoned, or control is gone: end the thread.
    End,
}

/// Stats `targets` the way the watcher does: jobs taken and the volume's skip checked before
/// each stat (a volume skipped meanwhile is left at once, not after the chunk), the busy state
/// around it, then the record.
pub(super) fn stat_targets(
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
pub(super) fn watcher(
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
pub(super) fn take_jobs(core: &Mutex<WatchCore>, jobs: &Receiver<WatchJob>) -> bool {
    loop {
        match jobs.try_recv() {
            Ok(job) => lock(core).apply(job),
            Err(TryRecvError::Empty) => return true,
            Err(TryRecvError::Disconnected) => return false,
        }
    }
}

/// Waits `gap`, applying jobs as they come; `false` once control is gone.
pub(super) fn wait_jobs(core: &Mutex<WatchCore>, jobs: &Receiver<WatchJob>, gap: Duration) -> bool {
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
