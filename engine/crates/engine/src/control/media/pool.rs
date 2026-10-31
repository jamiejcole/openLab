// SPDX-License-Identifier: GPL-3.0-or-later

use crate::lock;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, select};
use openlab_audio::{Media, MediaError, preload_within};

use super::super::DocId;
use super::super::inbox::{Inbox, ProbeAnswer};
use super::gate::*;
use super::{Job, MAX_ABANDONED_WORKERS, PROBE_WORKERS, ProbeClock, ProbeFn};

/// The job a worker is on.
pub(super) struct Busy {
    doc: DocId,
    since: Instant,
    key: String,
    ticket: u64,
    bucket: String,
    probe: bool,
    epoch: u64,
    trial: Option<u64>,
    /// The watchdog has judged it stuck (once).
    reported: bool,
}

/// A worker as the watchdog sees it.
pub(super) struct WorkerState {
    /// The job it is on.
    busy: Mutex<Option<Busy>>,
    abandoned: AtomicBool,
}

/// The probe workers every doc shares, their watchdog and what they know about the volumes
/// (ARCHITECTURE §3.1: one pool of [`PROBE_WORKERS`]).
pub struct ProbePool {
    high: Sender<Job>,
    low: Sender<Job>,
    high_rx: Receiver<Job>,
    low_rx: Receiver<Job>,
    inbox: Sender<Inbox>,
    probe: ProbeFn,
    gate: Arc<Mutex<Gate>>,
    workers: Vec<Arc<WorkerState>>,
    abandoned: usize,
    /// [`MAX_ABANDONED_WORKERS`] was reached (reported once).
    limit_reported: bool,
    limit_unread: bool,
    /// A replacement worker could not be started: tried again every watchdog tick. Set while
    /// that lasts; the change is reported once each way ([`ProbePool::take_worker_note`]).
    spawn_failing: bool,
    worker_note: Option<WorkerNote>,
    /// Tests: the next this many [`ProbePool::spawn`] calls fail as the OS would.
    #[cfg(test)]
    pub(super) fail_spawns: u32,
    timeout: Duration,
    clock: ProbeClock,
    /// Bytes every doc's preloads hold together (AUD-05: one budget for the engine).
    preloads: Arc<AtomicU64>,
}

/// How one doc's catalog reaches the pool: its jobs carry the doc.
#[derive(Clone)]
pub struct PoolHandle {
    pub(super) doc: DocId,
    /// Bytes every doc's preloads hold together, against the one preload budget.
    pub(super) preloads: Arc<AtomicU64>,
    pub(super) high: Sender<Job>,
    pub(super) low: Sender<Job>,
    pub(super) gate: Arc<Mutex<Gate>>,
}

/// A change in whether the pool can start the workers it replaces stuck ones with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkerNote {
    /// A replacement could not be started (the OS's reason); retried every tick.
    SpawnFailed(String),
    /// The pool has all its workers again.
    Restored,
}

/// A probe the watchdog gave up on: the doc and file it was for.
pub struct Stuck {
    pub doc: DocId,
    pub key: String,
    pub ticket: u64,
}

impl ProbePool {
    /// Starts the probe workers; they answer through `inbox`.
    pub fn new(
        inbox: Sender<Inbox>,
        probe: ProbeFn,
        timeout: Duration,
        clock: ProbeClock,
    ) -> std::io::Result<Self> {
        let (high, high_rx) = crossbeam_channel::unbounded::<Job>();
        let (low, low_rx) = crossbeam_channel::unbounded::<Job>();
        let mut pool = Self {
            high,
            low,
            high_rx,
            low_rx,
            inbox,
            probe,
            gate: Arc::default(),
            workers: Vec::new(),
            abandoned: 0,
            limit_reported: false,
            limit_unread: false,
            spawn_failing: false,
            worker_note: None,
            #[cfg(test)]
            fail_spawns: 0,
            timeout,
            clock,
            preloads: Arc::default(),
        };
        #[cfg(windows)]
        {
            lock(&pool.gate).system_drive = Some(system_drive(|k| std::env::var(k).ok()));
        }
        for _ in 0..PROBE_WORKERS {
            pool.spawn()?;
        }
        Ok(pool)
    }

    /// Where doc `doc`'s catalog sends its jobs.
    pub fn handle(&self, doc: DocId) -> PoolHandle {
        PoolHandle {
            doc,
            preloads: Arc::clone(&self.preloads),
            high: self.high.clone(),
            low: self.low.clone(),
            gate: Arc::clone(&self.gate),
        }
    }

    /// A doc closed: its waiting jobs go, its folder is no longer exempt. Its jobs already
    /// taken run to their end; their answers find no doc.
    pub fn leave(&self, doc: DocId) {
        lock(&self.gate).leave(doc);
    }

    fn spawn(&mut self) -> std::io::Result<()> {
        #[cfg(test)]
        if self.fail_spawns > 0 {
            self.fail_spawns -= 1;
            return Err(std::io::Error::other("injected spawn failure"));
        }
        let state = Arc::new(WorkerState {
            busy: Mutex::new(None),
            abandoned: AtomicBool::new(false),
        });
        let (high, low, inbox, probe) = (
            self.high_rx.clone(),
            self.low_rx.clone(),
            self.inbox.clone(),
            self.probe,
        );
        let st = Arc::clone(&state);
        let clock = self.clock.clone();
        let gate = Arc::clone(&self.gate);
        let n = self.workers.len() + self.abandoned;
        // Never joined: a worker stuck in a read on a stalled disk must not hang shutdown. It
        // ends once the pool and every catalog (the job senders) are gone and its current job
        // is done.
        thread::Builder::new()
            .name(format!("openlab-probe-{n}"))
            .spawn(move || worker(&high, &low, &inbox, probe, &st, &clock, &gate))?;
        self.workers.push(state);
        Ok(())
    }

    /// Replaces workers stuck on one job for longer than the timeout. Returns the probes given
    /// up on: their docs report the files unplayable until (if ever) the stuck probe answers.
    pub fn watchdog(&mut self) -> Vec<Stuck> {
        let mut out = Vec::new();
        let mut stuck = Vec::new();
        let now = self.clock.now();
        for (i, w) in self.workers.iter().enumerate() {
            let mut busy = lock(&w.busy);
            if let Some(b) = busy.as_mut()
                && !b.reported
                && now.saturating_duration_since(b.since) >= self.timeout
            {
                b.reported = true;
                stuck.push((
                    i,
                    b.doc,
                    b.key.clone(),
                    b.ticket,
                    b.bucket.clone(),
                    b.probe,
                    b.epoch,
                    b.trial,
                ));
            }
        }
        {
            let mut gate = lock(&self.gate);
            for (_, _, key, _, bucket, probe, epoch, trial) in &stuck {
                gate.timed_out(*epoch, bucket, key, *probe, *trial, now);
            }
        }
        for (i, doc, key, ticket, ..) in stuck.into_iter().rev() {
            if self.abandoned >= MAX_ABANDONED_WORKERS {
                if !self.limit_reported {
                    self.limit_reported = true;
                    self.limit_unread = true;
                }
                break;
            }
            let w = self.workers.remove(i);
            w.abandoned.store(true, Ordering::Release);
            self.abandoned += 1;
            out.push(Stuck { doc, key, ticket });
        }
        self.replenish();
        // Jobs that waited for a stalled volume run when it works again, or one at a time as
        // trials: a trial that answers (or a stalled volume the watch sees healthy again) ends
        // the wait for all of them.
        let released = lock(&self.gate).release_due(now);
        for (job, high) in released {
            let queue = if high { &self.high } else { &self.low };
            let _ = queue.send(job);
        }
        out
    }

    /// Starts workers until there are [`PROBE_WORKERS`] again (stuck ones replaced). A start
    /// that fails (out of threads or memory) is noted and tried again on the next tick; the
    /// waiting probes run once a worker is up.
    fn replenish(&mut self) {
        while self.workers.len() < PROBE_WORKERS {
            if let Err(e) = self.spawn() {
                if !self.spawn_failing {
                    self.spawn_failing = true;
                    self.worker_note = Some(WorkerNote::SpawnFailed(e.to_string()));
                }
                return;
            }
        }
        if self.spawn_failing {
            self.spawn_failing = false;
            self.worker_note = Some(WorkerNote::Restored);
        }
    }

    /// What changed about starting replacement workers since the last call, if anything.
    pub fn take_worker_note(&mut self) -> Option<WorkerNote> {
        self.worker_note.take()
    }

    /// The volumes the media watch reports stalled: the probes defer to them too.
    pub fn set_watch_stalled<'a>(&mut self, volumes: impl IntoIterator<Item = &'a str> + Clone) {
        let mut gate = lock(&self.gate);
        // Nothing is built on the (usual) tick where nothing changed.
        let mut count = 0;
        let same = volumes.clone().into_iter().all(|v| {
            count += 1;
            gate.watch_stalled.contains(v)
        });
        if !same || count != gate.watch_stalled.len() {
            gate.watch_stalled = volumes.into_iter().map(str::to_owned).collect();
        }
    }

    /// Tests inject which volume a path is on.
    pub fn set_volume_of(&mut self, volume: fn(&Path) -> String) {
        lock(&self.gate).volume = Some(volume);
    }

    /// Volumes that stalled (`true`) or answered again (`false`) since the last call.
    pub fn take_volume_events(&mut self) -> Vec<(String, bool)> {
        std::mem::take(&mut lock(&self.gate).events)
    }

    /// True once, the first time a stuck worker could not be replaced (the limit is reached).
    pub fn take_limit_reached(&mut self) -> bool {
        std::mem::take(&mut self.limit_unread)
    }

    /// Workers replaced because they were stuck.
    pub fn abandoned_workers(&self) -> usize {
        self.abandoned
    }

    /// Workers running (stuck ones excluded): [`PROBE_WORKERS`] whatever the number of docs.
    pub fn workers(&self) -> usize {
        self.workers.len()
    }
}

pub(super) fn worker(
    high: &Receiver<Job>,
    low: &Receiver<Job>,
    inbox: &Sender<Inbox>,
    probe: ProbeFn,
    state: &WorkerState,
    clock: &ProbeClock,
    gate: &Mutex<Gate>,
) {
    loop {
        // The high-priority queue first, whenever it has something.
        let (job, from_high) = match high.try_recv() {
            Ok(job) => (job, true),
            Err(_) => select! {
                recv(high) -> j => match j { Ok(j) => (j, true), Err(_) => return },
                recv(low) -> j => match j { Ok(j) => (j, false), Err(_) => return },
            },
        };
        // A job for a stalled volume waits for the control thread to release it; a copy of a
        // job already taken is dropped.
        let Some(Taken {
            job,
            epoch,
            trial,
            bucket,
        }) = lock(gate).admit(job, from_high, clock.now())
        else {
            continue;
        };
        let probe_job = job.is_probe();
        let doc = job.doc();
        let key = job.key().0.to_owned();
        *lock(&state.busy) = Some(Busy {
            doc,
            since: clock.now(),
            key: key.clone(),
            ticket: job.key().1,
            bucket: bucket.clone(),
            probe: probe_job,
            epoch,
            trial,
            reported: false,
        });
        let (answer, answered) = match job {
            Job::Probe {
                key, ticket, path, ..
            } => {
                let result = probe(&path).map(|p| (path, p));
                let answered = result.as_ref().err().is_none_or(volume_answered);
                (
                    ProbeAnswer::Probed {
                        result,
                        key,
                        ticket,
                    },
                    answered,
                )
            }
            Job::Preload {
                key,
                ticket,
                path,
                max_bytes,
                ..
            } => {
                let result = preload_within(Media::Path(path), max_bytes);
                let answered = result.as_ref().err().is_none_or(volume_answered);
                (
                    ProbeAnswer::Preloaded {
                        result,
                        key,
                        ticket,
                    },
                    answered,
                )
            }
        };
        *lock(&state.busy) = None;
        lock(gate).finished(
            epoch,
            &bucket,
            &key,
            probe_job,
            trial,
            answered,
            clock.now(),
        );
        if inbox.send(Inbox::Probe(doc, answer)).is_err() || state.abandoned.load(Ordering::Acquire)
        {
            return;
        }
    }
}

/// An error that still shows the volume answered: the file is missing or bad, not unreachable.
pub(super) fn volume_answered(e: &MediaError) -> bool {
    !matches!(e, MediaError::Io(_) | MediaError::Worker)
}
