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

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, select};
use openlab_audio::{Media, MediaError, Preloaded, Probed, preload_within};
use openlab_core::{
    Cue, CueId, CueIndex, CueKind, Footprint, IndexChange, MediaRef, MediaStatus, Workspace,
};

use crate::persist::media_path;

use super::inbox::{Inbox, ProbeAnswer};

/// Probe workers started per control loop. Probes of different files run side by side so one
/// long file does not hold up the rest of a workspace.
pub const PROBE_WORKERS: usize = 2;
/// Most workers abandoned (stuck) and replaced over a catalog's life: past it, stuck workers
/// are not replaced any more, so a dead volume cannot make the engine start threads without end.
pub const MAX_ABANDONED_WORKERS: usize = 16;

/// The monotonic clock the probe watchdog reads. Tests inject one that moves only when told to,
/// so whether a worker counts as stuck never depends on how fast the machine runs.
#[derive(Clone)]
pub struct ProbeClock(Arc<dyn Fn() -> Instant + Send + Sync>);

impl ProbeClock {
    pub fn system() -> Self {
        Self(Arc::new(Instant::now))
    }

    pub fn new(now: impl Fn() -> Instant + Send + Sync + 'static) -> Self {
        Self(Arc::new(now))
    }

    fn now(&self) -> Instant {
        (self.0)()
    }
}

impl std::fmt::Debug for ProbeClock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProbeClock")
    }
}

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
    /// How many cues use the file, and how many of them ask for it to be preloaded.
    users: usize,
    preloaders: usize,
    /// The file is to be preloaded (a cue asks, or its probe advises it): as of the last answer
    /// or change, so a change of mind is told from a steady state.
    wants_preload: bool,
    /// The preload job whose answer counts (0: none); a released preload's late answer is stale.
    preload_ticket: u64,
    /// Decoded into memory (AUD-05), once the preload job has answered.
    preloaded: Option<Arc<Preloaded>>,
    /// Bytes taken from the workspace preload budget for this file.
    reserved: u64,
    /// The file is too big for what is left of the budget: it streams.
    over_budget: bool,
    /// The preload failed (decode error, memory): it streams.
    preload_failed: bool,
    /// The id of the file's watch (its first ticket): an event of an earlier watch of the same
    /// key does not count.
    watch: u64,
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
    clock: ProbeClock,
    entries: HashMap<String, Entry>,
    next_ticket: u64,
    show_dir: Option<PathBuf>,
    budget: u64,
    reserved: u64,
    /// Where the cues are, to follow an edit from its footprint.
    index: CueIndex,
    /// What each audio cue with a file contributes to `entries`' counts.
    uses: HashMap<CueId, Use>,
    /// Cues the catalog has read from the workspace (a full pass reads all, an edit its own).
    cues_read: u64,
    /// What changed for the slice-plan checks since they last looked.
    plan_changes: PlanChanges,
    /// Files to start or stop watching (drained by the control loop into watch jobs).
    key_changes: Vec<KeyChange>,
    /// Jobs sent to the workers (tests wait for exactly that many answers).
    #[cfg(test)]
    jobs: u64,
}

/// What may have changed a cue's slice-plan check (`checks.rs`) since the checks last looked
/// (M4.12c): kept here because the catalog sees every edit's cues and every probe change, so
/// the checks look only at those.
#[derive(Debug, Default)]
pub struct PlanChanges {
    /// The whole workspace was followed again (opened, reset, an edit that could not be
    /// followed from its footprint): every cue may have changed.
    pub all: bool,
    /// Cues an edit added, removed, edited or moved (a group's descendants included when it
    /// was added or removed).
    pub cues: BTreeSet<CueId>,
    /// Files whose probe changed (answered, failed, forgotten, asked afresh).
    pub keys: BTreeSet<String>,
}

impl PlanChanges {
    pub fn is_empty(&self) -> bool {
        !self.all && self.cues.is_empty() && self.keys.is_empty()
    }
}

/// A file the media watch is to start or stop watching (docs/control.md "Media watch").
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyChange {
    /// Everything was forgotten (the catalog was reset).
    Reset,
    Watch {
        key: String,
        path: PathBuf,
        id: u64,
    },
    Unwatch {
        key: String,
    },
    /// The file's probe failed (`missing`: no file), so the watch reports it back once it is
    /// there, even if it appeared before the watch first looked.
    ProbeFailed {
        key: String,
        id: u64,
        missing: bool,
    },
}

/// One cue's use of a media file.
#[derive(Debug, PartialEq, Eq)]
struct Use {
    key: String,
    preload: bool,
}

impl MediaCatalog {
    /// Starts the probe workers; they answer through `inbox`.
    pub fn new(
        inbox: Sender<Inbox>,
        show_dir: Option<PathBuf>,
        preload_budget: u64,
        probe: ProbeFn,
        timeout: Duration,
        clock: ProbeClock,
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
            clock,
            entries: HashMap::new(),
            next_ticket: 0,
            show_dir,
            budget: preload_budget,
            reserved: 0,
            index: CueIndex::default(),
            uses: HashMap::new(),
            cues_read: 0,
            plan_changes: PlanChanges::default(),
            key_changes: Vec::new(),
            #[cfg(test)]
            jobs: 0,
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
        let clock = self.clock.clone();
        let n = self.workers.len() + self.abandoned;
        // Never joined: a worker stuck in a read on a stalled disk must not hang shutdown. It
        // ends once the catalog (the job senders) is gone and its current job is done.
        thread::Builder::new()
            .name(format!("openlab-probe-{n}"))
            .spawn(move || worker(&high, &low, &inbox, probe, &st, &clock))?;
        self.workers.push(state);
        Ok(())
    }

    /// Another workspace replaces the old one (opened, created, reverted), or the show was saved
    /// in another folder: forgets everything known about the files, resolves relative references
    /// against `show_dir` from now on, and follows `ws` from scratch ([`Self::sync`]: the cue
    /// index is rebuilt and every file is probed again). It takes the workspace so the catalog
    /// can never be left empty. Tickets keep rising, so answers to probes asked before the reset
    /// find no entry (or a newer ticket) and are ignored.
    pub fn reset(&mut self, show_dir: Option<PathBuf>, ws: &Workspace) {
        self.entries.clear();
        self.key_changes = vec![KeyChange::Reset];
        self.uses.clear();
        self.reserved = 0;
        self.show_dir = show_dir;
        self.sync(ws);
    }

    /// Follows the whole workspace (a newly opened one; and the reference [`Self::sync_cues`]
    /// must equal): probes media references it has not seen, forgets the ones no cue uses any
    /// more (releasing their preload), and starts or releases the preload of files whose cues
    /// changed their mind. Probes go out in workspace order; files near a playhead are moved
    /// ahead with [`Self::prioritize`].
    pub fn sync(&mut self, ws: &Workspace) {
        self.plan_changes = PlanChanges {
            all: true,
            ..PlanChanges::default()
        };
        let mut uses: HashMap<CueId, Use> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut visited = 0;
        self.index = CueIndex::build_visiting(ws, |cue| {
            visited += 1;
            if let Some((key, preload)) = media_use(cue) {
                if seen.insert(key.to_owned()) {
                    order.push(key.to_owned());
                }
                uses.insert(
                    cue.id,
                    Use {
                        key: key.to_owned(),
                        preload,
                    },
                );
            }
        });
        self.cues_read += visited;
        let mut counts: HashMap<&str, (usize, usize)> = HashMap::new();
        for u in uses.values() {
            let n = counts.entry(u.key.as_str()).or_default();
            n.0 += 1;
            n.1 += usize::from(u.preload);
        }
        let mut freed = 0;
        self.entries.retain(|k, e| match counts.get(k.as_str()) {
            Some(&(users, preloaders)) => {
                e.users = users;
                e.preloaders = preloaders;
                true
            }
            None => {
                freed += e.reserved;
                self.key_changes.push(KeyChange::Unwatch { key: k.clone() });
                false
            }
        });
        self.reserved = self.reserved.saturating_sub(freed);
        let mut asked = Vec::new();
        for key in &order {
            if !self.entries.contains_key(key.as_str()) {
                self.request(key.clone());
                asked.push(key.as_str());
            }
        }
        for key in asked {
            if let (Some(e), Some(&(users, preloaders))) =
                (self.entries.get_mut(key), counts.get(key))
            {
                e.users = users;
                e.preloaders = preloaders;
            }
        }
        let mut keys: Vec<String> = self.entries.keys().cloned().collect();
        keys.sort_unstable();
        for key in keys {
            self.settle_preload(&key);
        }
        self.uses = uses;
    }

    /// Follows an edit from its footprint: the same result as [`Self::sync`], at the cost of the
    /// cues the edit touched. Cues added, removed, edited or moved are re-read; a file whose
    /// last cue went is forgotten, one first used now is probed (in workspace order), and a file
    /// whose cues now ask (or no longer ask) for a preload gets one (or loses it).
    pub fn sync_cues(&mut self, ws: &Workspace, footprint: &Footprint) {
        let structural: BTreeSet<CueId> = footprint
            .added
            .iter()
            .chain(&footprint.removed)
            .chain(&footprint.moved)
            .copied()
            .collect();
        let touched: BTreeSet<CueId> = structural
            .iter()
            .chain(&footprint.edited)
            .copied()
            .collect();
        let delta = match self
            .index
            .reconcile(ws, &touched, &structural, &footprint.added)
        {
            IndexChange::Delta(delta) => delta,
            IndexChange::Rebuilt => return self.sync(ws),
        };
        // Files whose counts changed, and uses of files not probed yet.
        let mut changed: BTreeSet<String> = BTreeSet::new();
        let mut first_uses: Vec<(CueId, Use)> = Vec::new();
        for id in &delta.gone {
            if let Some(old) = self.uses.remove(id) {
                self.drop_use(old, &mut changed);
            }
        }
        // Each cue once: a cue can be both touched and new.
        let seen: BTreeSet<CueId> = touched.iter().chain(&delta.fresh).copied().collect();
        // Trims and markers change without the file: the plan checks look at every cue read.
        self.plan_changes.cues.extend(seen.iter().copied());
        self.plan_changes.cues.extend(delta.gone.iter().copied());
        for id in &seen {
            if !self.index.contains(*id) {
                continue;
            }
            self.cues_read += 1;
            let Some(cue) = self.index.locate(ws, *id) else {
                // The footprint and the index disagree with the workspace: start over.
                return self.sync(ws);
            };
            let new = media_use(cue);
            if new == self.uses.get(id).map(|u| (u.key.as_str(), u.preload)) {
                continue;
            }
            if let Some(old) = self.uses.remove(id) {
                self.drop_use(old, &mut changed);
            }
            if let Some((key, preload)) = new {
                let new = Use {
                    key: key.to_owned(),
                    preload,
                };
                match self.entries.get_mut(key) {
                    Some(e) => {
                        e.users += 1;
                        e.preloaders += usize::from(preload);
                        changed.insert(key.to_owned());
                        self.uses.insert(*id, new);
                    }
                    None => first_uses.push((*id, new)),
                }
            }
        }
        // Files nobody uses now are forgotten; the preload they held goes back.
        for key in &changed {
            if self.entries.get(key.as_str()).is_some_and(|e| e.users == 0)
                && let Some(e) = self.entries.remove(key.as_str())
            {
                self.reserved = self.reserved.saturating_sub(e.reserved);
                self.key_changes
                    .push(KeyChange::Unwatch { key: key.clone() });
                self.plan_changes.keys.insert(key.clone());
            }
        }
        // New files in workspace order (the order a full sync asks in).
        first_uses.sort_by_cached_key(|(id, _)| self.index.path(*id));
        for (id, new) in first_uses {
            if !self.entries.contains_key(new.key.as_str()) {
                self.request(new.key.clone());
            }
            if let Some(e) = self.entries.get_mut(new.key.as_str()) {
                e.users += 1;
                e.preloaders += usize::from(new.preload);
            }
            changed.insert(new.key.clone());
            self.uses.insert(id, new);
        }
        // Preloads follow the counts, in the order a full sync settles them.
        for key in changed {
            self.settle_preload(&key);
        }
    }

    /// One cue stopped using its file.
    fn drop_use(&mut self, old: Use, changed: &mut BTreeSet<String>) {
        if let Some(e) = self.entries.get_mut(old.key.as_str()) {
            e.users = e.users.saturating_sub(1);
            e.preloaders = e.preloaders.saturating_sub(usize::from(old.preload));
        }
        changed.insert(old.key);
    }

    /// Starts the preload of a probed file some cue now asks for, or releases the one of a file
    /// no cue (and not its probe) wants any more, so it does not hold budget or memory for
    /// nothing. Acts on a change of mind only: a file that stays wanted (or unwanted) keeps
    /// whatever its last attempt came to (preloaded, over budget, failed).
    fn settle_preload(&mut self, key: &str) {
        let Some(e) = self.entries.get_mut(key) else {
            return;
        };
        let advised = matches!(&e.probe, Probe::Ready(r) if r.probed.preload_advised());
        let want = e.preloaders > 0 || advised;
        if want == e.wants_preload {
            return;
        }
        e.wants_preload = want;
        // Until it is probed there is nothing to preload: the answer decides.
        if !matches!(e.probe, Probe::Ready(_)) {
            return;
        }
        if want {
            self.start_preload(key);
        } else {
            self.release_preload(key);
        }
    }

    /// Reserves the preload memory for a probed file and sends the job; a file that does not fit
    /// what is left of the budget streams instead.
    fn start_preload(&mut self, key: &str) {
        let Some(e) = self.entries.get_mut(key) else {
            return;
        };
        let Probe::Ready(ready) = &e.probe else {
            return;
        };
        let (path, bytes) = (ready.path.clone(), preload_bytes(&ready.probed));
        self.reserved = self.reserved.saturating_sub(e.reserved);
        e.reserved = 0;
        e.preloaded = None;
        e.preload_failed = false;
        e.over_budget = false;
        if self.reserved.saturating_add(bytes) > self.budget {
            e.over_budget = true;
            e.preload_ticket = 0;
            return;
        }
        self.next_ticket += 1;
        e.preload_ticket = self.next_ticket;
        self.reserved += bytes;
        e.reserved = bytes;
        let job = Job::Preload {
            key: key.to_owned(),
            ticket: e.preload_ticket,
            path,
            max_bytes: bytes,
        };
        if self.low.send(job).is_err() {
            self.reserved = self.reserved.saturating_sub(bytes);
            e.reserved = 0;
            e.preload_ticket = 0;
        } else {
            #[cfg(test)]
            {
                self.jobs += 1;
            }
        }
    }

    /// Gives back what a preload holds: the budget, the decoded samples (voices playing from
    /// them keep their own reference) and any answer still to come.
    fn release_preload(&mut self, key: &str) {
        let Some(e) = self.entries.get_mut(key) else {
            return;
        };
        self.reserved = self.reserved.saturating_sub(e.reserved);
        e.reserved = 0;
        e.preloaded = None;
        e.over_budget = false;
        e.preload_failed = false;
        e.preload_ticket = 0;
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
        let asked = match &path {
            Some(path) => self
                .low
                .send(Job::Probe {
                    key: key.clone(),
                    ticket,
                    path: path.clone(),
                })
                .is_ok(),
            // A relative reference in a show that was never saved has no folder to be in.
            None => false,
        };
        #[cfg(test)]
        {
            self.jobs += u64::from(asked);
        }
        // A new entry, or one that fails at once: what `media` gives may change.
        if !asked || !self.entries.contains_key(&key) {
            self.plan_changes.keys.insert(key.clone());
        }
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
                // Watched from its first use; a reference with no place on disk cannot be.
                if let Some(path) = path {
                    self.key_changes.push(KeyChange::Watch {
                        key: key.clone(),
                        path,
                        id: ticket,
                    });
                }
                self.entries.insert(
                    key,
                    Entry {
                        probe: if asked { Probe::Pending } else { Probe::Failed },
                        ticket,
                        asking: asked,
                        urgent: false,
                        timed_out: false,
                        users: 0,
                        preloaders: 0,
                        wants_preload: false,
                        preload_ticket: 0,
                        preloaded: None,
                        reserved: 0,
                        over_budget: false,
                        preload_failed: false,
                        watch: ticket,
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
                && self.clock.now().saturating_duration_since(*since) >= self.timeout
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
                self.plan_changes.keys.insert(key.clone());
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
    /// stale answer or a preload. A probed file is preloaded if a cue asks (the catalog counts
    /// them, see [`Self::sync_cues`]) or its probe advises it.
    pub fn answer(&mut self, answer: ProbeAnswer) -> Option<(MediaRef, MediaStatus)> {
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
                let (probe, status) = match result {
                    Ok((path, probed)) => (
                        Probe::Ready(Arc::new(Ready { path, probed })),
                        MediaStatus::Available,
                    ),
                    Err(e) => {
                        self.key_changes.push(KeyChange::ProbeFailed {
                            key: key.clone(),
                            id: entry.watch,
                            missing: matches!(e, MediaError::NotFound(_)),
                        });
                        (Probe::Failed, status_of(&e))
                    }
                };
                let ready = matches!(probe, Probe::Ready(_));
                entry.probe = probe;
                self.plan_changes.keys.insert(key.clone());
                entry.preloaded = None;
                self.reserved = self.reserved.saturating_sub(entry.reserved);
                entry.reserved = 0;
                entry.over_budget = false;
                entry.preload_failed = false;
                entry.preload_ticket = 0;
                entry.wants_preload = false;
                if ready {
                    // The cues' wishes (and the probe's advice) are known: preload if wanted.
                    self.settle_preload(&key);
                }
                Some((MediaRef::new(key), status))
            }
            ProbeAnswer::Preloaded {
                key,
                ticket,
                result,
            } => {
                // Only the job in flight counts: a released preload's answer is stale.
                let entry = self
                    .entries
                    .get_mut(&key)
                    .filter(|e| e.preload_ticket == ticket && ticket != 0)?;
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

    /// The watch found `key`'s file gone (its watch `id`): as a probe that found no file, at
    /// once. The probe fails, the preload goes (its budget back), and an answer still to come
    /// is ignored (the ticket moves on). Returns the status for the cue engine, or `None` for
    /// an event of an earlier watch of the key or a file no cue uses any more.
    pub fn mark_missing(&mut self, key: &str, id: u64) -> Option<(MediaRef, MediaStatus)> {
        let entry = self.entries.get_mut(key).filter(|e| e.watch == id)?;
        self.next_ticket += 1;
        entry.ticket = self.next_ticket;
        entry.asking = false;
        entry.urgent = false;
        entry.timed_out = false;
        entry.probe = Probe::Failed;
        self.reserved = self.reserved.saturating_sub(entry.reserved);
        entry.reserved = 0;
        entry.preloaded = None;
        entry.over_budget = false;
        entry.preload_failed = false;
        entry.preload_ticket = 0;
        entry.wants_preload = false;
        self.plan_changes.keys.insert(key.to_owned());
        Some((MediaRef::new(key), MediaStatus::Missing))
    }

    /// The watch found `key`'s file back, changed or unreadable (its watch `id`): probes it
    /// afresh, even if a probe is under way (it may have read what was there before). The last
    /// answer stands until then. `false` for an event of an earlier watch.
    pub fn recheck(&mut self, key: &str, id: u64) -> bool {
        if !self.entries.get(key).is_some_and(|e| e.watch == id) {
            return false;
        }
        self.request(key.to_owned());
        true
    }

    /// Files to start or stop watching since this was last called, in order.
    pub fn take_key_changes(&mut self) -> Vec<KeyChange> {
        std::mem::take(&mut self.key_changes)
    }

    pub fn get(&self, key: &str) -> Option<&Probe> {
        self.entries.get(key).map(|e| &e.probe)
    }

    /// The probe of `key`, if it is playable (what a plan is built from; no path is cloned).
    pub fn ready(&self, key: &str) -> Option<&Arc<Ready>> {
        match &self.entries.get(key)?.probe {
            Probe::Ready(ready) => Some(ready),
            Probe::Pending | Probe::Failed => None,
        }
    }

    /// Where every cue is, in step with the workspace after `sync` or `sync_cues`.
    pub fn index(&self) -> &CueIndex {
        &self.index
    }

    /// What may have changed the slice-plan checks since this was last called.
    pub fn take_plan_changes(&mut self) -> PlanChanges {
        std::mem::take(&mut self.plan_changes)
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

    /// Whether `key` is held in memory (a preload finished and not released).
    pub fn is_preloaded(&self, key: &str) -> bool {
        self.entries.get(key).is_some_and(|e| e.preloaded.is_some())
    }

    /// Preload jobs started and not answered yet.
    pub fn preloads_pending(&self) -> usize {
        self.entries
            .values()
            .filter(|e| e.preload_ticket != 0 && e.preloaded.is_none() && !e.preload_failed)
            .count()
    }

    /// Probes still running (re-probes included).
    pub fn pending(&self) -> usize {
        self.entries
            .values()
            .filter(|e| e.asking && !e.timed_out)
            .count()
    }

    /// How many cues the catalog has read from the workspace so far: a pin on the cost of
    /// answers and edits (an open reads every cue once; nothing else may read them all).
    pub fn cues_read(&self) -> u64 {
        self.cues_read
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

/// The file an audio cue plays and whether it asks for it to be preloaded.
fn media_use(cue: &Cue) -> Option<(&str, bool)> {
    match &cue.kind {
        CueKind::Audio {
            media: Some(m),
            preload,
            ..
        } => Some((m.as_str(), *preload)),
        _ => None,
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
    clock: &ProbeClock,
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
            *busy = Some((clock.now(), key.to_owned(), ticket));
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
        let mut cat = MediaCatalog::new(
            tx,
            None,
            0,
            fake,
            Duration::from_secs(30),
            ProbeClock::system(),
        )
        .unwrap();
        let key = "/abs/a.wav";
        cat.request(key.to_owned());
        assert_eq!(cat.pending(), 1);
        let a = answer(&rx);
        assert!(cat.answer(a).is_some());
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
        assert!(cat.answer(a).is_some());
        assert_eq!(cat.pending(), 0);
    }

    // ----- following edits from their footprints (M4.12a) and preload changes (M4.12b) -----

    use openlab_core::{Command, CueProperty, GroupMode, History, Position, load_show};
    use std::collections::BTreeMap;

    /// A mono 32-bit float WAV of 100 frames (400 bytes preloaded).
    fn write_wav(dir: &Path, name: &str) -> String {
        write_wav_frames(dir, name, 100)
    }

    /// A mono 32-bit float WAV of `frames` frames at 8 kHz.
    fn write_wav_frames(dir: &Path, name: &str, frames: u32) -> String {
        let data_len = frames * 4;
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data_len).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&3u16.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&8000u32.to_le_bytes());
        v.extend_from_slice(&32_000u32.to_le_bytes());
        v.extend_from_slice(&4u16.to_le_bytes());
        v.extend_from_slice(&32u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_len.to_le_bytes());
        v.resize(v.len() + data_len as usize, 0);
        let path = dir.join(name);
        std::fs::write(&path, v).unwrap();
        // The catalog keys files by `MediaRef`'s portable form (`/` on Windows too).
        MediaRef::new(path.to_str().unwrap()).as_str().to_owned()
    }

    fn catalog(budget: u64) -> (MediaCatalog, Receiver<Inbox>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let probe: ProbeFn = openlab_audio::probe;
        let cat = MediaCatalog::new(
            tx,
            None,
            budget,
            probe,
            Duration::from_secs(60),
            ProbeClock::system(),
        )
        .unwrap();
        (cat, rx)
    }

    /// Hands the catalog the answers of every job it has sent, in a fixed order (the two
    /// workers answer in any order, and the budget goes to whoever is answered first), until
    /// nothing is in flight: answers can start preloads.
    fn settle(cat: &mut MediaCatalog, rx: &Receiver<Inbox>, received: &mut u64) {
        while *received < cat.jobs {
            let mut round = Vec::new();
            while *received < cat.jobs {
                round.push(answer(rx));
                *received += 1;
            }
            round.sort_by_key(|a| match a {
                ProbeAnswer::Probed { key, .. } => (key.clone(), 0),
                ProbeAnswer::Preloaded { key, .. } => (key.clone(), 1),
            });
            for a in round {
                cat.answer(a);
            }
        }
    }

    /// Applies the catalog's key changes to `watched` as the watch would, and checks the watch
    /// then watches exactly the catalog's files, each under its current watch id.
    fn follow_keys(cat: &mut MediaCatalog, watched: &mut BTreeMap<String, u64>) {
        for change in cat.take_key_changes() {
            match change {
                KeyChange::Reset => watched.clear(),
                KeyChange::Watch { key, id, .. } => {
                    assert!(watched.insert(key, id).is_none(), "watched twice");
                }
                KeyChange::Unwatch { key } => {
                    assert!(watched.remove(&key).is_some(), "unwatched unknown");
                }
                KeyChange::ProbeFailed { key, id, .. } => {
                    assert_eq!(
                        watched.get(&key),
                        Some(&id),
                        "a failed probe of a watched file"
                    );
                }
            }
        }
        let expected: BTreeMap<String, u64> = cat
            .entries
            .iter()
            .map(|(k, e)| (k.clone(), e.watch))
            .collect();
        assert_eq!(*watched, expected);
    }

    #[test]
    fn the_watch_follows_the_files_the_catalog_knows() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_wav(dir.path(), "a.wav");
        let b = write_wav(dir.path(), "b.wav");
        let (mut ws, template) = template();
        let mut c1 = template.clone();
        c1.id = CueId(uuid::Uuid::from_u128(901));
        c1.number = None;
        set_media(&mut c1, &a, false);
        let mut c2 = c1.clone();
        c2.id = CueId(uuid::Uuid::from_u128(902));
        set_media(&mut c2, &b, false);
        ws.cue_lists[0].cues = vec![c1, c2];
        let (mut cat, rx) = catalog(0);
        let mut got = 0;
        cat.sync(&ws);
        let mut watched = BTreeMap::new();
        follow_keys(&mut cat, &mut watched);
        assert_eq!(watched.len(), 2);
        settle(&mut cat, &rx, &mut got);
        // A re-probe is not a new watch.
        cat.reprobe(&a);
        assert!(cat.take_key_changes().is_empty());
        settle(&mut cat, &rx, &mut got);
        // A reset starts over: everything watched again under new ids.
        let old = watched.clone();
        cat.reset(None, &ws);
        let changes = cat.take_key_changes();
        assert_eq!(changes.first(), Some(&KeyChange::Reset));
        watched.clear();
        for c in changes.into_iter().skip(1) {
            let KeyChange::Watch { key, id, path } = c else {
                panic!("{c:?}")
            };
            assert_eq!(path, PathBuf::from(&key));
            watched.insert(key, id);
        }
        assert_eq!(watched.len(), 2);
        assert!(watched.iter().all(|(k, id)| old.get(k) != Some(id)));
        settle(&mut cat, &rx, &mut got);
        // A relative reference in a show never saved has no path: not watched.
        let mut c3 = template.clone();
        c3.id = CueId(uuid::Uuid::from_u128(903));
        set_media(&mut c3, "sounds/rel.wav", false);
        ws.cue_lists[0].cues.push(c3);
        cat.sync(&ws);
        assert!(cat.take_key_changes().is_empty());
        assert!(matches!(cat.get("sounds/rel.wav"), Some(Probe::Failed)));
    }

    #[test]
    fn a_file_marked_missing_fails_at_once_and_ignores_its_probe_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_wav(dir.path(), "a.wav");
        let (mut ws, template) = template();
        let mut c1 = template.clone();
        c1.id = CueId(uuid::Uuid::from_u128(911));
        c1.number = None;
        set_media(&mut c1, &a, true);
        ws.cue_lists[0].cues = vec![c1];
        let (mut cat, rx) = catalog(10_000);
        let mut got = 0;
        cat.sync(&ws);
        let id = match cat.take_key_changes().as_slice() {
            [KeyChange::Watch { id, .. }] => *id,
            other => panic!("{other:?}"),
        };
        settle(&mut cat, &rx, &mut got);
        assert!(cat.is_preloaded(&a));
        assert_eq!(cat.preload_reserved(), 400);
        // An event of another watch of the key changes nothing.
        assert_eq!(cat.mark_missing(&a, id + 1000), None);
        assert!(!cat.recheck(&a, id + 1000));
        assert!(cat.ready(&a).is_some());
        // Gone while a re-probe is under way: failed now, the budget back, the answer ignored.
        cat.reprobe(&a);
        assert_eq!(
            cat.mark_missing(&a, id),
            Some((MediaRef::new(a.as_str()), MediaStatus::Missing))
        );
        assert!(matches!(cat.get(&a), Some(Probe::Failed)));
        assert!(!cat.is_preloaded(&a));
        assert_eq!(cat.preload_reserved(), 0);
        assert_eq!(cat.pending(), 0);
        assert!(cat.take_plan_changes().keys.contains(&a));
        let late = answer(&rx);
        got += 1;
        assert_eq!(
            cat.answer(late),
            None,
            "the answer of a probe before the loss"
        );
        assert!(matches!(cat.get(&a), Some(Probe::Failed)));
        // Back: probed afresh, and preloaded again.
        assert!(cat.recheck(&a, id));
        settle(&mut cat, &rx, &mut got);
        assert!(cat.ready(&a).is_some());
        assert!(cat.is_preloaded(&a));
        assert_eq!(cat.preload_reserved(), 400);
        // A file no cue uses any more is not marked.
        ws.cue_lists[0].cues.clear();
        cat.sync(&ws);
        assert_eq!(cat.mark_missing(&a, id), None);
    }

    /// Everything the catalog knows about its files, for comparing two catalogs.
    type View = (
        u64,
        BTreeMap<String, (usize, usize, bool, bool, bool, bool, u64, u64, u64)>,
        BTreeMap<CueId, (String, bool)>,
    );

    fn view(cat: &MediaCatalog) -> View {
        let entries = cat
            .entries
            .iter()
            .map(|(k, e)| {
                (
                    k.clone(),
                    (
                        e.users,
                        e.preloaders,
                        e.wants_preload,
                        e.over_budget,
                        e.preload_failed,
                        e.preloaded.is_some(),
                        e.reserved,
                        e.ticket,
                        e.preload_ticket,
                    ),
                )
            })
            .collect();
        let uses = cat
            .uses
            .iter()
            .map(|(id, u)| (*id, (u.key.clone(), u.preload)))
            .collect();
        (cat.reserved, entries, uses)
    }

    /// Random walks for the twin tests: `default` for CI, `OPENLAB_PROP_CASES` for a sweep.
    fn sweep_seeds(default: u64) -> u64 {
        std::env::var("OPENLAB_PROP_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) as usize % n.max(1)
        }
    }

    fn all_cues(ws: &Workspace) -> Vec<(CueId, bool)> {
        let mut out = Vec::new();
        for list in &ws.cue_lists {
            let mut stack: Vec<&[Cue]> = vec![&list.cues];
            while let Some(cues) = stack.pop() {
                for c in cues {
                    out.push((c.id, matches!(c.kind, CueKind::Group { .. })));
                    stack.push(c.children());
                }
            }
        }
        out
    }

    /// One random edit of `ws` built the way the tests of commands do: always plausible,
    /// sometimes refused.
    fn random_command(
        ws: &Workspace,
        rng: &mut Rng,
        template: &Cue,
        files: &[String],
        next: &mut u128,
    ) -> Command {
        let cues = all_cues(ws);
        let pick = |rng: &mut Rng| cues[rng.below(cues.len())];
        let groups: Vec<CueId> = cues.iter().filter(|c| c.1).map(|c| c.0).collect();
        let position = |rng: &mut Rng| Position {
            list: ws.cue_lists[rng.below(ws.cue_lists.len())].id,
            parent: if groups.is_empty() || rng.below(2) == 0 {
                None
            } else {
                Some(groups[rng.below(groups.len())])
            },
            index: rng.below(4),
        };
        let mut new_cue = |rng: &mut Rng, group: bool| {
            *next += 1;
            let mut c = template.clone();
            c.id = CueId(uuid::Uuid::from_u128(*next));
            c.number = None;
            c.kind = if group {
                *next += 1;
                let mut inner = template.clone();
                inner.id = CueId(uuid::Uuid::from_u128(*next));
                inner.number = None;
                set_media(
                    &mut inner,
                    &files[rng.below(files.len())],
                    rng.below(2) == 0,
                );
                CueKind::Group {
                    mode: GroupMode::Timeline,
                    children: vec![inner],
                }
            } else {
                template.kind.clone()
            };
            if !group {
                set_media(&mut c, &files[rng.below(files.len())], rng.below(2) == 0);
            }
            c
        };
        // An empty show can only be added to.
        match if cues.is_empty() { 0 } else { rng.below(8) } {
            0 | 1 => Command::AddCue {
                at: position(rng),
                cue: new_cue(rng, false),
                auto_number: false,
            },
            2 => Command::AddCue {
                at: position(rng),
                cue: new_cue(rng, true),
                auto_number: false,
            },
            3 => Command::RemoveCue { id: pick(rng).0 },
            4 => Command::MoveCue {
                id: pick(rng).0,
                index: rng.below(4),
            },
            5 => Command::ReparentCue {
                id: pick(rng).0,
                to: position(rng),
            },
            6 => {
                // A cue removed and added again under its id: as a group with other cues, or
                // as a plain cue.
                let id = pick(rng).0;
                let group = rng.below(2) == 0;
                let mut cue = new_cue(rng, group);
                cue.id = id;
                Command::Transaction {
                    commands: vec![
                        Command::RemoveCue { id },
                        Command::AddCue {
                            at: position(rng),
                            cue,
                            auto_number: false,
                        },
                    ],
                }
            }
            _ => {
                // Another file and preload setting for a cue (whatever it was).
                let mut probe = template.clone();
                set_media(
                    &mut probe,
                    &files[rng.below(files.len())],
                    rng.below(2) == 0,
                );
                Command::SetCueProperty {
                    id: pick(rng).0,
                    property: CueProperty::Kind { value: probe.kind },
                }
            }
        }
    }

    fn set_media(cue: &mut Cue, file: &str, want_preload: bool) {
        if let CueKind::Audio { media, preload, .. } = &mut cue.kind {
            *media = Some(MediaRef::new(file));
            *preload = want_preload;
        }
    }

    fn template() -> (Workspace, Cue) {
        let text = include_str!("../../../../fixtures/show/v1-example.openlab");
        let mut ws = load_show(text).unwrap().workspace;
        let id = openlab_core::audio_media(&ws)[0].0;
        let template = ws.find_cue(id).unwrap().clone();
        for list in &mut ws.cue_lists {
            list.cues.clear();
        }
        (ws, template)
    }

    /// An engine that follows each edit from its footprint keeps exactly the media catalog a
    /// full `sync` of the workspace gives: the same files, counts, preload wishes, budget,
    /// probes and preload jobs asked in the same order (tickets), whatever the edits, undos
    /// and redos, and as the probe answers come in.
    #[test]
    fn following_footprints_equals_a_full_sync_after_any_edits() {
        let dir = tempfile::tempdir().unwrap();
        let files: Vec<String> = (0..5)
            .map(|n| write_wav(dir.path(), &format!("f{n}.wav")))
            .collect();
        let (base, template) = template();
        let (mut preloaded, mut over, mut released, mut cleared) = (0, 0, 0, 0);
        for seed in 1..=sweep_seeds(30) {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            // Room for two of the five files (400 bytes each): the rest are over budget.
            let (mut inc, rx_inc) = catalog(900);
            let (mut full, rx_full) = catalog(900);
            let (mut got_inc, mut got_full) = (0, 0);
            let (mut ws, mut history, mut next) = (base.clone(), History::new(), 10_000u128);
            inc.sync(&ws);
            full.sync(&ws);
            let mut watched = BTreeMap::new();
            follow_keys(&mut inc, &mut watched);
            for step in 0..60 {
                let footprint = match rng.below(9) {
                    0 => history
                        .peek_undo()
                        .map(openlab_core::Command::footprint)
                        .filter(|_| history.undo(&mut ws).unwrap_or(false)),
                    1 => history
                        .peek_redo()
                        .map(openlab_core::Command::footprint)
                        .filter(|_| history.redo(&mut ws).unwrap_or(false)),
                    _ => {
                        let command = random_command(&ws, &mut rng, &template, &files, &mut next);
                        let footprint = command.footprint();
                        history.execute(&mut ws, command).ok().map(|()| footprint)
                    }
                };
                let Some(footprint) = footprint else { continue };
                let before = view(&inc);
                inc.sync_cues(&ws, &footprint);
                full.sync(&ws);
                full.take_key_changes();
                follow_keys(&mut inc, &mut watched);
                assert!(inc.index.matches(&ws), "seed {seed} step {step}");
                assert_eq!(view(&inc), view(&full), "seed {seed} step {step}");
                settle(&mut inc, &rx_inc, &mut got_inc);
                settle(&mut full, &rx_full, &mut got_full);
                let after = view(&inc);
                assert_eq!(after, view(&full), "seed {seed} step {step} after answers");
                // What the walk is meant to reach.
                preloaded += after.1.values().filter(|e| e.5).count();
                over += after.1.values().filter(|e| e.3).count();
                released += usize::from(before.0 > after.0);
                cleared += before
                    .1
                    .values()
                    .filter(|e| e.3)
                    .count()
                    .saturating_sub(after.1.values().filter(|e| e.3).count());
            }
        }
        assert!(preloaded > 100, "preloads reached: {preloaded}");
        assert!(over > 20, "over-budget files reached: {over}");
        assert!(released > 20, "budget given back: {released}");
        assert!(
            cleared > 5,
            "over-budget flags cleared by a release: {cleared}"
        );
    }

    /// A group removed and added again under its id in one edit: the new group's file is
    /// probed and counted, the old one's is forgotten (with its preload), whether the child
    /// has a new id or keeps its own with another file.
    #[test]
    fn a_group_replaced_under_its_id_moves_its_files_users_to_the_new_children() {
        replaced_group(false);
        replaced_group(true);
    }

    fn replaced_group(same_child_id: bool) {
        let dir = tempfile::tempdir().unwrap();
        let (old_file, new_file) = (
            write_wav(dir.path(), "old.wav"),
            write_wav(dir.path(), "new.wav"),
        );
        let (mut ws, template) = template();
        let mut old_child = template.clone();
        set_media(&mut old_child, &old_file, true);
        let mut new_child = template.clone();
        if !same_child_id {
            new_child.id = CueId(uuid::Uuid::from_u128(10_003));
        }
        set_media(&mut new_child, &new_file, true);
        let group = |id: u128, child: Cue| Cue {
            id: CueId(uuid::Uuid::from_u128(id)),
            kind: CueKind::Group {
                mode: GroupMode::Timeline,
                children: vec![child],
            },
            number: None,
            ..template.clone()
        };
        ws.cue_lists[0].cues = vec![group(10_001, old_child)];
        let (mut inc, rx_inc) = catalog(10_000);
        let (mut full, rx_full) = catalog(10_000);
        let (mut got_inc, mut got_full) = (0, 0);
        inc.sync(&ws);
        full.sync(&ws);
        settle(&mut inc, &rx_inc, &mut got_inc);
        settle(&mut full, &rx_full, &mut got_full);
        assert_eq!(inc.preload_reserved(), 400);
        let command = Command::Transaction {
            commands: vec![
                Command::RemoveCue {
                    id: CueId(uuid::Uuid::from_u128(10_001)),
                },
                Command::AddCue {
                    at: Position {
                        list: ws.cue_lists[0].id,
                        parent: None,
                        index: 0,
                    },
                    cue: group(10_001, new_child),
                    auto_number: false,
                },
            ],
        };
        let footprint = command.footprint();
        ws.apply(command).unwrap();
        inc.sync_cues(&ws, &footprint);
        full.sync(&ws);
        assert!(inc.index.matches(&ws));
        assert_eq!(view(&inc), view(&full));
        settle(&mut inc, &rx_inc, &mut got_inc);
        settle(&mut full, &rx_full, &mut got_full);
        assert_eq!(view(&inc), view(&full));
        assert!(
            inc.get(&new_file).is_some(),
            "the new child's file is probed"
        );
        assert!(
            inc.get(&old_file).is_none(),
            "the old child's file is forgotten"
        );
        assert_eq!(inc.preload_reserved(), 400, "one preload, the new file's");
    }

    #[test]
    fn switching_preload_on_for_a_probed_file_preloads_it_and_off_releases_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_wav(dir.path(), "a.wav");
        let (mut ws, template) = template();
        let mut cue = template.clone();
        set_media(&mut cue, &file, false);
        let mut other = cue.clone();
        other.id = CueId(uuid::Uuid::from_u128(10_002));
        let id = cue.id;
        ws.cue_lists[0].cues = vec![cue, other];
        let (mut cat, rx) = catalog(10_000);
        let mut got = 0;
        cat.sync(&ws);
        settle(&mut cat, &rx, &mut got);
        assert_eq!(cat.preload_reserved(), 0, "nothing asks for a preload");
        assert!(cat.get(&file).is_some_and(|p| matches!(p, Probe::Ready(_))));

        let set = |ws: &mut Workspace, id: CueId, on: bool| {
            let mut c = ws.find_cue(id).unwrap().clone();
            set_media(&mut c, &file, on);
            let command = Command::SetCueProperty {
                id,
                property: CueProperty::Kind { value: c.kind },
            };
            let footprint = command.footprint();
            ws.apply(command).unwrap();
            footprint
        };
        // On: the file is already probed, and is now preloaded (it used to wait for the next
        // open).
        let fp = set(&mut ws, id, true);
        cat.sync_cues(&ws, &fp);
        assert_eq!(cat.preload_reserved(), 400);
        settle(&mut cat, &rx, &mut got);
        assert!(matches!(cat.media(&file), Some((_, Media::Preloaded(_)))));
        // A second cue asks too; the first stops asking: the file stays preloaded.
        let other_id = CueId(uuid::Uuid::from_u128(10_002));
        let fp = set(&mut ws, other_id, true);
        cat.sync_cues(&ws, &fp);
        let fp = set(&mut ws, id, false);
        cat.sync_cues(&ws, &fp);
        assert_eq!(cat.preload_reserved(), 400);
        assert!(matches!(cat.media(&file), Some((_, Media::Preloaded(_)))));
        // The last one stops: the reservation and the samples are released.
        let fp = set(&mut ws, other_id, false);
        cat.sync_cues(&ws, &fp);
        assert_eq!(cat.preload_reserved(), 0);
        assert!(matches!(cat.media(&file), Some((_, Media::Path(_)))));
        // And on again: a new preload.
        let fp = set(&mut ws, id, true);
        cat.sync_cues(&ws, &fp);
        assert_eq!(cat.preload_reserved(), 400);
        settle(&mut cat, &rx, &mut got);
        assert!(matches!(cat.media(&file), Some((_, Media::Preloaded(_)))));
    }

    #[test]
    fn a_preload_answer_that_arrives_after_its_release_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_wav(dir.path(), "a.wav");
        let (mut ws, template) = template();
        let mut cue = template.clone();
        set_media(&mut cue, &file, true);
        let id = cue.id;
        ws.cue_lists[0].cues = vec![cue];
        let (mut cat, rx) = catalog(10_000);
        let mut got = 0;
        cat.sync(&ws);
        // The probe answers, which starts the preload job; before its answer is read, the
        // cue stops asking.
        let first = answer(&rx);
        got += 1;
        cat.answer(first);
        assert_eq!(cat.preload_reserved(), 400);
        let mut c = ws.find_cue(id).unwrap().clone();
        set_media(&mut c, &file, false);
        let command = Command::SetCueProperty {
            id,
            property: CueProperty::Kind { value: c.kind },
        };
        let footprint = command.footprint();
        ws.apply(command).unwrap();
        cat.sync_cues(&ws, &footprint);
        assert_eq!(cat.preload_reserved(), 0);
        settle(&mut cat, &rx, &mut got);
        assert!(
            matches!(cat.media(&file), Some((_, Media::Path(_)))),
            "the late answer did not bring the samples back"
        );
        assert_eq!(cat.preload_reserved(), 0);
    }

    // ----- slice-plan checks followed from the catalog's notes (M4.12c) -----

    use super::super::checks::PlanChecks;
    use openlab_core::SliceMarker;
    use std::num::NonZeroU32;

    const PLAN_RATE: NonZeroU32 = NonZeroU32::new(48_000).unwrap();

    /// New trims, markers and counts for an audio cue (its file kept): a start past the end of
    /// a 12.5 ms file fails, the rest plays (a marker past a short file cuts nothing).
    fn plan_edit(ws: &Workspace, rng: &mut Rng) -> Option<Command> {
        let audio: Vec<&Cue> = all_cues(ws)
            .into_iter()
            .filter_map(|(id, _)| ws.find_cue(id))
            .filter(|c| matches!(c.kind, CueKind::Audio { .. }))
            .collect();
        let cue = *audio.get(rng.below(audio.len()))?;
        let mut kind = cue.kind.clone();
        if let CueKind::Audio {
            start_time,
            slices,
            last_slice_play_count,
            ..
        } = &mut kind
        {
            *start_time = [0.0, 0.004, 0.02][rng.below(3)];
            *slices = match rng.below(3) {
                0 => vec![],
                n => vec![SliceMarker {
                    time: 0.3,
                    play_count: u32::try_from(n).unwrap(),
                }],
            };
            *last_slice_play_count = [1, 2][rng.below(2)];
        }
        Some(Command::SetCueProperty {
            id: cue.id,
            property: CueProperty::Kind { value: kind },
        })
    }

    /// Plan checks told only what the catalog noted (the cues of each edit, the files whose
    /// probe changed) answer exactly what a walk of every audio cue answers, after any edits,
    /// undos and redos (groups removed and added again under their ids included), probe
    /// answers, re-probes of files that changed length, and checks with nothing new; and they
    /// look at what changed, not at the show.
    #[test]
    fn plan_checks_following_changes_equal_a_full_walk() {
        let dir = tempfile::tempdir().unwrap();
        let files: Vec<String> = (0..5)
            .map(|n| write_wav(dir.path(), &format!("f{n}.wav")))
            .collect();
        let (base, template) = template();
        let (mut warned, mut cleared, mut reprobed) = (0, 0, 0);
        for seed in 1..=sweep_seeds(30) {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let (mut cat, rx) = catalog(900);
            let mut got = 0;
            let (mut ws, mut history, mut next) = (base.clone(), History::new(), 20_000u128);
            cat.sync(&ws);
            let mut checks = PlanChecks::default();
            let mut last: Vec<(CueId, String)> = Vec::new();
            for step in 0..80 {
                let footprint = match rng.below(12) {
                    0 => history
                        .peek_undo()
                        .map(openlab_core::Command::footprint)
                        .filter(|_| history.undo(&mut ws).unwrap_or(false)),
                    1 => history
                        .peek_redo()
                        .map(openlab_core::Command::footprint)
                        .filter(|_| history.redo(&mut ws).unwrap_or(false)),
                    2..=4 => plan_edit(&ws, &mut rng).and_then(|command| {
                        let footprint = command.footprint();
                        history.execute(&mut ws, command).ok().map(|()| footprint)
                    }),
                    5 => {
                        // A file changes length and is probed again.
                        let n = rng.below(files.len());
                        let frames = [100, 8000][rng.below(2)];
                        write_wav_frames(dir.path(), &format!("f{n}.wav"), frames);
                        cat.reprobe(&files[n]);
                        reprobed += 1;
                        None
                    }
                    6 => None,
                    _ => {
                        let command = random_command(&ws, &mut rng, &template, &files, &mut next);
                        let footprint = command.footprint();
                        history.execute(&mut ws, command).ok().map(|()| footprint)
                    }
                };
                if let Some(footprint) = &footprint {
                    cat.sync_cues(&ws, footprint);
                }
                if rng.below(2) == 0 {
                    settle(&mut cat, &rx, &mut got);
                }
                let got_now = checks.check(&ws, &mut cat, PLAN_RATE);
                let reference = PlanChecks::default().check_all(&ws, &cat, PLAN_RATE);
                assert_eq!(got_now, reference, "seed {seed} step {step}");
                warned += got_now.iter().filter(|w| !last.contains(w)).count();
                cleared += last.iter().filter(|w| !got_now.contains(w)).count();
                last = got_now;
            }
            // Only the first check (a newly followed workspace) walked the show.
            assert_eq!(checks.walks(), 1, "seed {seed}");
        }
        assert!(warned > 50, "warnings reached: {warned}");
        assert!(cleared > 30, "warnings cleared: {cleared}");
        assert!(reprobed > 50, "re-probes: {reprobed}");
    }

    /// The cost pin: in a show of 500 audio cues, an edit of one cue looks at that cue, a probe
    /// answer for one file at the cues on it, and a check with nothing new at none.
    #[test]
    fn plan_checks_look_at_the_edit_not_the_show() {
        let dir = tempfile::tempdir().unwrap();
        let files: Vec<String> = (0..5)
            .map(|n| write_wav(dir.path(), &format!("f{n}.wav")))
            .collect();
        let (mut ws, template) = template();
        let mut cues = Vec::new();
        for n in 0..500u128 {
            let mut c = template.clone();
            c.id = CueId(uuid::Uuid::from_u128(30_000 + n));
            c.number = None;
            set_media(&mut c, &files[usize::try_from(n % 5).unwrap()], false);
            cues.push(c);
        }
        let first = cues[0].id;
        ws.cue_lists[0].cues = cues;
        let (mut cat, rx) = catalog(0);
        let mut got = 0;
        cat.sync(&ws);
        settle(&mut cat, &rx, &mut got);
        let mut checks = PlanChecks::default();
        checks.check(&ws, &mut cat, PLAN_RATE);
        let examined = checks.examined();
        checks.check(&ws, &mut cat, PLAN_RATE);
        assert_eq!(
            checks.examined(),
            examined,
            "nothing new: nothing looked at"
        );

        let mut rng = Rng(7);
        let mut command = plan_edit(&ws, &mut rng).unwrap();
        if let Command::SetCueProperty { id, .. } = &mut command {
            *id = first;
        }
        let footprint = command.footprint();
        ws.apply(command).unwrap();
        cat.sync_cues(&ws, &footprint);
        let builds = checks.builds();
        checks.check(&ws, &mut cat, PLAN_RATE);
        assert_eq!(checks.examined(), examined + 1, "an edit: its cue");
        assert_eq!(checks.builds(), builds + 1);

        // One file probed again: its 100 cues.
        cat.reprobe(&files[1]);
        settle(&mut cat, &rx, &mut got);
        let examined = checks.examined();
        checks.check(&ws, &mut cat, PLAN_RATE);
        assert_eq!(
            checks.examined(),
            examined + 100,
            "a probe: the cues on its file"
        );
        assert_eq!(
            checks.check(&ws, &mut cat, PLAN_RATE),
            PlanChecks::default().check_all(&ws, &cat, PLAN_RATE)
        );
        assert_eq!(checks.walks(), 1);
    }
}
