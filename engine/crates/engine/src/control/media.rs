// SPDX-License-Identifier: GPL-3.0-or-later

//! What the control thread knows about each media file the workspace uses, and the probe workers
//! that find it out. Probing decodes a whole compressed file (1–2 s for 30 minutes, docs/audio.md),
//! and preloading decodes it into memory, so both run on these workers and never on the control
//! thread (ARCHITECTURE §3.1): the control thread sends a job and gets the answer back through
//! its inbox.
//!
//! Probes of files cues near a playhead need come first (a high-priority queue the workers
//! always empty before the other one). A worker stuck on one file (a stalled network volume)
//! longer than `probe_timeout` is abandoned and replaced, so later probes are not queued behind
//! it for ever; its file is reported unplayable meanwhile (a late answer still counts). The
//! abandoned thread is leaked: it ends when its read returns, if it ever does.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, select};
use openlab_audio::{Media, MediaError, Preloaded, Probed, preload_within};
use openlab_core::{Cue, CueKind, MediaRef, MediaStatus, Workspace};

use crate::persist::media_path;

use super::inbox::{Inbox, ProbeAnswer};

/// Probe workers started per control loop. Probes of different files run side by side so one
/// long file does not hold up the rest of a workspace.
pub const PROBE_WORKERS: usize = 2;
/// Most workers abandoned (stuck) and replaced over a catalog's life: past it, stuck workers
/// are not replaced any more, so a dead volume cannot make the engine start threads without end.
pub const MAX_ABANDONED_WORKERS: usize = 16;

/// How a file is probed: [`openlab_audio::probe`] in the engine; tests inject a stand-in.
pub type ProbeFn = fn(&Path) -> Result<Probed, MediaError>;

/// A job for a probe worker.
#[derive(Debug)]
enum Job {
    Probe {
        key: String,
        ticket: u64,
        path: PathBuf,
    },
    Preload {
        key: String,
        ticket: u64,
        path: PathBuf,
        max_bytes: u64,
    },
}

impl Job {
    fn key(&self) -> (&str, u64) {
        match self {
            Self::Probe { key, ticket, .. } | Self::Preload { key, ticket, .. } => (key, *ticket),
        }
    }
}

/// One media file's state on the control thread.
#[derive(Debug)]
pub enum Probe {
    /// Asked; no answer yet.
    Pending,
    /// Playable (possibly damaged: see `probed.warnings`).
    Ready(Arc<Ready>),
    /// Missing or unplayable.
    Failed,
}

#[derive(Debug)]
pub struct Ready {
    pub path: PathBuf,
    pub probed: Probed,
}

#[derive(Debug)]
struct Entry {
    /// The last answer (a re-probe keeps it until the new one comes).
    probe: Probe,
    /// The job whose answer counts: earlier jobs for this key are stale.
    ticket: u64,
    /// That job has not answered yet.
    asking: bool,
    /// Its job went to the high-priority queue too.
    urgent: bool,
    /// The watchdog gave up on its job (a late answer still counts).
    timed_out: bool,
    /// Decoded into memory (AUD-05), once the preload job has answered.
    preloaded: Option<Arc<Preloaded>>,
    /// Bytes taken from the workspace preload budget for this file.
    reserved: u64,
    /// The file is too big for what is left of the budget: it streams.
    over_budget: bool,
    /// The preload failed (decode error, memory): it streams.
    preload_failed: bool,
}

/// A worker as the watchdog sees it.
struct WorkerState {
    /// The job it is on and since when.
    busy: Mutex<Option<(Instant, String, u64)>>,
    abandoned: AtomicBool,
}

/// Probe workers and the catalog of media the workspace uses.
pub struct MediaCatalog {
    high: Sender<Job>,
    low: Sender<Job>,
    high_rx: Receiver<Job>,
    low_rx: Receiver<Job>,
    inbox: Sender<Inbox>,
    probe: ProbeFn,
    workers: Vec<Arc<WorkerState>>,
    abandoned: usize,
    /// [`MAX_ABANDONED_WORKERS`] was reached (reported once).
    limit_reported: bool,
    limit_unread: bool,
    timeout: Duration,
    entries: HashMap<String, Entry>,
    next_ticket: u64,
    show_dir: Option<PathBuf>,
    budget: u64,
    reserved: u64,
}

impl MediaCatalog {
    /// Starts the probe workers; they answer through `inbox`.
    pub fn new(
        inbox: Sender<Inbox>,
        show_dir: Option<PathBuf>,
        preload_budget: u64,
        probe: ProbeFn,
        timeout: Duration,
    ) -> std::io::Result<Self> {
        let (high, high_rx) = crossbeam_channel::unbounded::<Job>();
        let (low, low_rx) = crossbeam_channel::unbounded::<Job>();
        let mut catalog = Self {
            high,
            low,
            high_rx,
            low_rx,
            inbox,
            probe,
            workers: Vec::new(),
            abandoned: 0,
            limit_reported: false,
            limit_unread: false,
            timeout,
            entries: HashMap::new(),
            next_ticket: 0,
            show_dir,
            budget: preload_budget,
            reserved: 0,
        };
        for _ in 0..PROBE_WORKERS {
            catalog.spawn()?;
        }
        Ok(catalog)
    }

    fn spawn(&mut self) -> std::io::Result<()> {
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
        let n = self.workers.len() + self.abandoned;
        // Never joined: a worker stuck in a read on a stalled disk must not hang shutdown. It
        // ends once the catalog (the job senders) is gone and its current job is done.
        thread::Builder::new()
            .name(format!("openlab-probe-{n}"))
            .spawn(move || worker(&high, &low, &inbox, probe, &st))?;
        self.workers.push(state);
        Ok(())
    }

    /// Follows the workspace: probes media references it has not seen and forgets the ones no
    /// cue uses any more (releasing their preload). Probes go out in workspace order; files
    /// near a playhead are moved ahead with [`Self::prioritize`].
    pub fn sync(&mut self, ws: &Workspace) {
        // Borrowed keys: an edit in a show of thousands of cues allocates for new files only.
        let mut used: HashSet<&str> = HashSet::new();
        let mut order: Vec<&str> = Vec::new();
        for list in &ws.cue_lists {
            media_refs(&list.cues, &mut |m| {
                if used.insert(m) {
                    order.push(m);
                }
            });
        }
        let mut freed = 0;
        self.entries.retain(|k, e| {
            let keep = used.contains(k.as_str());
            if !keep {
                freed += e.reserved;
            }
            keep
        });
        self.reserved = self.reserved.saturating_sub(freed);
        for key in order {
            if !self.entries.contains_key(key) {
                self.request(key.to_owned());
            }
        }
    }

    /// Puts the probes of `keys` (files cues near a playhead play) ahead of the others.
    pub fn prioritize<'a>(&mut self, keys: impl IntoIterator<Item = &'a str>) {
        for key in keys {
            let Some(e) = self.entries.get_mut(key) else {
                continue;
            };
            if !e.asking || e.urgent {
                continue;
            }
            e.urgent = true;
            if let Some(path) = media_path(&MediaRef::new(key), self.show_dir.as_deref()) {
                // The same ticket: whichever copy answers first counts, the other is ignored.
                let _ = self.high.send(Job::Probe {
                    key: key.to_owned(),
                    ticket: e.ticket,
                    path,
                });
            }
        }
    }

    /// Probes `key` again (a stream of it failed: the file may have gone or changed). The last
    /// answer stands until the new one comes, so cues on it keep their primes meanwhile.
    pub fn reprobe(&mut self, key: &str) {
        if self
            .entries
            .get(key)
            .is_some_and(|e| e.asking && !e.timed_out)
        {
            return;
        }
        self.request(key.to_owned());
    }

    fn request(&mut self, key: String) {
        self.next_ticket += 1;
        let ticket = self.next_ticket;
        let path = media_path(&MediaRef::new(key.clone()), self.show_dir.as_deref());
        let asked = match path {
            Some(path) => self
                .low
                .send(Job::Probe {
                    key: key.clone(),
                    ticket,
                    path,
                })
                .is_ok(),
            // A relative reference in a show that was never saved has no folder to be in.
            None => false,
        };
        match self.entries.get_mut(&key) {
            Some(e) => {
                e.ticket = ticket;
                e.asking = asked;
                e.urgent = false;
                e.timed_out = false;
                if !asked {
                    e.probe = Probe::Failed;
                }
            }
            None => {
                self.entries.insert(
                    key,
                    Entry {
                        probe: if asked { Probe::Pending } else { Probe::Failed },
                        ticket,
                        asking: asked,
                        urgent: false,
                        timed_out: false,
                        preloaded: None,
                        reserved: 0,
                        over_budget: false,
                        preload_failed: false,
                    },
                );
            }
        }
    }

    /// Replaces workers stuck on one job for longer than the timeout; their files are reported
    /// unplayable until (if ever) the stuck probe answers. Returns those statuses.
    pub fn watchdog(&mut self) -> Vec<(MediaRef, MediaStatus)> {
        let mut out = Vec::new();
        let mut stuck = Vec::new();
        for (i, w) in self.workers.iter().enumerate() {
            let busy = w.busy.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((since, key, ticket)) = busy.as_ref()
                && since.elapsed() >= self.timeout
            {
                stuck.push((i, key.clone(), *ticket));
            }
        }
        for (i, key, ticket) in stuck.into_iter().rev() {
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
            let _ = self.spawn();
            if let Some(e) = self.entries.get_mut(&key)
                && e.ticket == ticket
                && e.asking
                && matches!(e.probe, Probe::Pending)
            {
                e.probe = Probe::Failed;
                e.timed_out = true;
                out.push((
                    MediaRef::new(key),
                    MediaStatus::Unsupported {
                        reason: "the file could not be read in time (the disk does not answer)"
                            .to_owned(),
                    },
                ));
            }
        }
        out
    }

    /// Takes a worker's answer. Returns the status to report to the cue engine, or `None` for a
    /// stale answer or a preload. `wants_preload(key, probed)` says whether a cue asks for it.
    pub fn answer(
        &mut self,
        answer: ProbeAnswer,
        wants_preload: impl Fn(&str, &Probed) -> bool,
    ) -> Option<(MediaRef, MediaStatus)> {
        match answer {
            ProbeAnswer::Probed {
                key,
                ticket,
                result,
            } => {
                let entry = self
                    .entries
                    .get_mut(&key)
                    .filter(|e| e.ticket == ticket && e.asking)?;
                entry.asking = false;
                let (probe, status, path) = match result {
                    Ok((path, probed)) => {
                        let ready = Ready { path, probed };
                        let path = ready.path.clone();
                        (
                            Probe::Ready(Arc::new(ready)),
                            MediaStatus::Available,
                            Some(path),
                        )
                    }
                    Err(e) => {
                        let status = status_of(&e);
                        (Probe::Failed, status, None)
                    }
                };
                entry.probe = probe;
                entry.preloaded = None;
                self.reserved = self.reserved.saturating_sub(entry.reserved);
                entry.reserved = 0;
                entry.over_budget = false;
                entry.preload_failed = false;
                if let (Some(path), Probe::Ready(ready)) = (path, &entry.probe)
                    && wants_preload(&key, &ready.probed)
                {
                    let bytes = preload_bytes(&ready.probed);
                    if self.reserved.saturating_add(bytes) <= self.budget {
                        self.reserved += bytes;
                        entry.reserved = bytes;
                        let job = Job::Preload {
                            key: key.clone(),
                            ticket,
                            path,
                            max_bytes: bytes,
                        };
                        if self.low.send(job).is_err() {
                            self.reserved -= bytes;
                            entry.reserved = 0;
                        }
                    } else {
                        entry.over_budget = true;
                    }
                }
                Some((MediaRef::new(key), status))
            }
            ProbeAnswer::Preloaded {
                key,
                ticket,
                result,
            } => {
                let entry = self.entries.get_mut(&key).filter(|e| e.ticket == ticket)?;
                match result {
                    Ok(p) => entry.preloaded = Some(p),
                    Err(_) => {
                        // It streams instead; the budget it held goes back.
                        self.reserved = self.reserved.saturating_sub(entry.reserved);
                        entry.reserved = 0;
                        entry.preload_failed = true;
                    }
                }
                None
            }
        }
    }

    pub fn get(&self, key: &str) -> Option<&Probe> {
        self.entries.get(key).map(|e| &e.probe)
    }

    /// What a stream of `key` reads: its preloaded samples if it has them, else the file.
    pub fn media(&self, key: &str) -> Option<(Arc<Ready>, Media)> {
        let entry = self.entries.get(key)?;
        let Probe::Ready(ready) = &entry.probe else {
            return None;
        };
        let media = match &entry.preloaded {
            Some(p) => Media::Preloaded(Arc::clone(p)),
            None => Media::Path(ready.path.clone()),
        };
        Some((Arc::clone(ready), media))
    }

    /// Probes still running (re-probes included).
    pub fn pending(&self) -> usize {
        self.entries
            .values()
            .filter(|e| e.asking && !e.timed_out)
            .count()
    }

    /// Bytes of the workspace preload budget in use.
    pub fn preload_reserved(&self) -> u64 {
        self.reserved
    }

    /// Files that asked for a preload and did not fit the budget: they stream.
    pub fn over_budget(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .filter(|(_, e)| e.over_budget)
            .map(|(k, _)| k.as_str())
    }

    /// Files whose preload failed: they stream.
    pub fn preload_failed(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .filter(|(_, e)| e.preload_failed)
            .map(|(k, _)| k.as_str())
    }

    /// Damaged-but-playable files, with how many problems the probe found.
    pub fn damaged(&self) -> impl Iterator<Item = (&str, usize)> {
        self.entries.iter().filter_map(|(k, e)| match &e.probe {
            Probe::Ready(r) if !r.probed.warnings.is_empty() => {
                Some((k.as_str(), r.probed.warnings.len()))
            }
            _ => None,
        })
    }

    /// True once, the first time a stuck worker could not be replaced (the limit is reached).
    pub fn take_limit_reached(&mut self) -> bool {
        std::mem::take(&mut self.limit_unread)
    }

    /// Workers replaced because they were stuck.
    pub fn abandoned_workers(&self) -> usize {
        self.abandoned
    }
}

fn media_refs<'a>(cues: &'a [Cue], f: &mut impl FnMut(&'a str)) {
    for c in cues {
        if let CueKind::Audio { media: Some(m), .. } = &c.kind {
            f(m.as_str());
        }
        media_refs(c.children(), f);
    }
}

/// Memory a preload of the file takes: every frame at its own rate, f32.
fn preload_bytes(p: &Probed) -> u64 {
    p.info
        .frames
        .saturating_mul(u64::from(p.info.channels))
        .saturating_mul(4)
}

/// How the cue engine sees a failed probe (docs/cue-engine.md, Broken cues).
pub fn status_of(e: &MediaError) -> MediaStatus {
    match e {
        MediaError::NotFound(_) => MediaStatus::Missing,
        other => MediaStatus::Unsupported {
            reason: other.to_string(),
        },
    }
}

fn worker(
    high: &Receiver<Job>,
    low: &Receiver<Job>,
    inbox: &Sender<Inbox>,
    probe: ProbeFn,
    state: &WorkerState,
) {
    loop {
        // The high-priority queue first, whenever it has something.
        let job = match high.try_recv() {
            Ok(job) => job,
            Err(_) => select! {
                recv(high) -> j => match j { Ok(j) => j, Err(_) => return },
                recv(low) -> j => match j { Ok(j) => j, Err(_) => return },
            },
        };
        {
            let (key, ticket) = job.key();
            let mut busy = state.busy.lock().unwrap_or_else(PoisonError::into_inner);
            *busy = Some((Instant::now(), key.to_owned(), ticket));
        }
        let answer = match job {
            Job::Probe { key, ticket, path } => ProbeAnswer::Probed {
                result: probe(&path).map(|p| (path, p)),
                key,
                ticket,
            },
            Job::Preload {
                key,
                ticket,
                path,
                max_bytes,
            } => ProbeAnswer::Preloaded {
                result: preload_within(Media::Path(path), max_bytes),
                key,
                ticket,
            },
        };
        *state.busy.lock().unwrap_or_else(PoisonError::into_inner) = None;
        if inbox.send(Inbox::Probe(answer)).is_err() || state.abandoned.load(Ordering::Acquire) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlab_audio::{MediaInfo, Seeking};

    fn fake(_: &Path) -> Result<Probed, MediaError> {
        Ok(Probed {
            info: MediaInfo {
                channels: 1,
                sample_rate: 1000,
                frames: 1000,
                duration: Duration::from_secs(1),
            },
            warnings: vec![],
            seeking: Seeking::Direct,
        })
    }

    fn answer(rx: &Receiver<Inbox>) -> ProbeAnswer {
        match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
            Inbox::Probe(a) => a,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_reprobe_keeps_the_last_answer_until_the_new_one() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut cat = MediaCatalog::new(tx, None, 0, fake, Duration::from_secs(30)).unwrap();
        let key = "/abs/a.wav";
        cat.request(key.to_owned());
        assert_eq!(cat.pending(), 1);
        let a = answer(&rx);
        assert!(cat.answer(a, |_, _| false).is_some());
        let Some(Probe::Ready(first)) = cat.get(key) else {
            panic!("not ready")
        };
        let first = Arc::clone(first);
        cat.reprobe(key);
        assert_eq!(cat.pending(), 1);
        // Meanwhile the last answer stands: the same probe, so primes built on it stay valid.
        let Some(Probe::Ready(still)) = cat.get(key) else {
            panic!("dropped the last answer")
        };
        assert!(Arc::ptr_eq(&first, still));
        let a = answer(&rx);
        assert!(cat.answer(a, |_, _| false).is_some());
        assert_eq!(cat.pending(), 0);
    }
}
