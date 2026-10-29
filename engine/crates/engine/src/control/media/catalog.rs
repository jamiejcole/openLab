// SPDX-License-Identifier: GPL-3.0-or-later

use crate::lock;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use openlab_audio::{Media, MediaError, Preloaded, Probed};
use openlab_core::{
    Cue, CueId, CueIndex, CueKind, Footprint, IndexChange, MediaRef, MediaStatus, Workspace,
};

use crate::persist::media_path;

use super::super::inbox::ProbeAnswer;
use super::Job;
use super::pool::*;

#[cfg(test)]
use super::super::DocId;
#[cfg(test)]
use super::super::inbox::Inbox;
#[cfg(test)]
use super::{ProbeClock, ProbeFn};
#[cfg(test)]
use crossbeam_channel::Sender;
#[cfg(test)]
use std::time::Duration;

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
pub(super) struct Entry {
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

/// What [`index_workspace`] reads from a workspace for a catalog: built off the control thread
/// for a doc being opened (ARCHITECTURE §3.1, `DocParts`), so installing it there costs only
/// the probe jobs.
pub struct Indexed {
    index: CueIndex,
    uses: HashMap<CueId, Use>,
    /// Files in workspace order (the order their probes go out), each with its path resolved
    /// against `show_dir` ([`media_path`]): resolving 2,000 references took about 40% of a
    /// doc's install on the control thread (M4.15i).
    order: Vec<(String, Option<PathBuf>)>,
    show_dir: Option<PathBuf>,
    visited: u64,
}

/// Reads every cue of `ws` once: the cue index and each audio cue's file, resolved against
/// `show_dir`.
pub fn index_workspace(ws: &Workspace, show_dir: Option<&Path>) -> Indexed {
    let mut uses: HashMap<CueId, Use> = HashMap::new();
    let mut order: Vec<(String, Option<PathBuf>)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut visited = 0;
    let index = CueIndex::build_visiting(ws, |cue| {
        visited += 1;
        if let Some((key, preload)) = media_use(cue) {
            if seen.insert(key.to_owned()) {
                order.push((
                    key.to_owned(),
                    media_path(&MediaRef::new(key.to_owned()), show_dir),
                ));
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
    Indexed {
        index,
        uses,
        order,
        show_dir: show_dir.map(Path::to_path_buf),
        visited,
    }
}

/// The catalog of media one doc's workspace uses; its probes run on the engine's
/// [`ProbePool`].
pub struct MediaCatalog {
    pool: PoolHandle,
    /// Unit tests of one catalog run it on a pool of its own.
    #[cfg(test)]
    own_pool: Option<ProbePool>,
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
pub(super) struct Use {
    key: String,
    preload: bool,
}

impl MediaCatalog {
    /// A catalog for doc `pool.doc`, its media resolved against `show_dir`.
    pub fn new(pool: PoolHandle, show_dir: Option<PathBuf>, preload_budget: u64) -> Self {
        {
            let mut gate = lock(&pool.gate);
            gate.members.insert(pool.doc);
            if let Some(dir) = &show_dir {
                gate.show_dirs.insert(pool.doc, dir.clone());
            }
        }
        Self {
            pool,
            #[cfg(test)]
            own_pool: None,
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
        }
    }

    /// A catalog with a pool of its own (unit tests of one catalog).
    #[cfg(test)]
    fn with_own_pool(
        inbox: Sender<Inbox>,
        show_dir: Option<PathBuf>,
        preload_budget: u64,
        probe: ProbeFn,
        timeout: Duration,
        clock: ProbeClock,
    ) -> std::io::Result<Self> {
        let pool = ProbePool::new(inbox, probe, timeout, clock)?;
        let mut catalog = Self::new(pool.handle(DocId(0)), show_dir, preload_budget);
        catalog.own_pool = Some(pool);
        Ok(catalog)
    }

    /// The own pool's watchdog, its give-ups applied here (as the control loop does).
    #[cfg(all(test, unix))]
    fn watchdog(&mut self) -> Vec<(MediaRef, MediaStatus)> {
        let stuck = self
            .own_pool
            .as_mut()
            .map(ProbePool::watchdog)
            .unwrap_or_default();
        let mut out = Vec::new();
        for s in stuck {
            self.give_up(&s.key, s.ticket, &mut out);
        }
        out
    }

    #[cfg(all(test, unix))]
    fn take_volume_events(&mut self) -> Vec<(String, bool)> {
        self.own_pool
            .as_mut()
            .map(ProbePool::take_volume_events)
            .unwrap_or_default()
    }

    #[cfg(all(test, unix))]
    fn abandoned_workers(&self) -> usize {
        self.own_pool
            .as_ref()
            .map_or(0, ProbePool::abandoned_workers)
    }

    /// Another workspace replaces the old one (opened, created, reverted), or the show was saved
    /// in another folder: forgets everything known about the files, resolves relative references
    /// against `show_dir` from now on, and follows `ws` from scratch ([`Self::sync`]: the cue
    /// index is rebuilt and every file is probed again). It takes the workspace so the catalog
    /// can never be left empty. Tickets keep rising, so answers to probes asked before the reset
    /// find no entry (or a newer ticket) and are ignored.
    pub fn reset(&mut self, show_dir: Option<PathBuf>, ws: &Workspace) {
        self.entries.clear();
        lock(&self.pool.gate).reset_doc(self.pool.doc, show_dir.clone());
        self.key_changes = vec![KeyChange::Reset];
        self.uses.clear();
        let all = self.reserved;
        give_back(&mut self.reserved, &self.pool.preloads, all);
        self.show_dir = show_dir;
        self.sync(ws);
    }

    /// Follows the whole workspace (a newly opened one; and the reference [`Self::sync_cues`]
    /// must equal): probes media references it has not seen, forgets the ones no cue uses any
    /// more (releasing their preload), and starts or releases the preload of files whose cues
    /// changed their mind. Probes go out in workspace order; files near a playhead are moved
    /// ahead with [`Self::prioritize`].
    pub fn sync(&mut self, ws: &Workspace) {
        let indexed = index_workspace(ws, self.show_dir.as_deref());
        self.sync_indexed(indexed);
    }

    /// [`Self::sync`] from a workspace already read ([`index_workspace`], off the control
    /// thread for a doc being opened).
    pub fn sync_indexed(&mut self, indexed: Indexed) {
        self.plan_changes = PlanChanges {
            all: true,
            ..PlanChanges::default()
        };
        let Indexed {
            index,
            uses,
            order,
            show_dir,
            visited,
        } = indexed;
        // Paths resolved against another folder than this catalog's are resolved again.
        let resolved = show_dir == self.show_dir;
        self.index = index;
        self.cues_read += visited;
        let mut counts: HashMap<&str, (usize, usize)> = HashMap::with_capacity(order.len());
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
        give_back(&mut self.reserved, &self.pool.preloads, freed);
        let mut asked = Vec::new();
        self.entries
            .reserve(order.len().saturating_sub(self.entries.len()));
        for (key, path) in &order {
            if !self.entries.contains_key(key.as_str()) {
                if resolved {
                    self.request_at(key.clone(), path.clone());
                } else {
                    self.request(key.clone());
                }
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
        // `settle_preload` of a file not probed yet only notes whether a cue wants it
        // preloaded: done in place, so a fresh catalog's thousands of keys are not copied.
        let mut keys: Vec<String> = Vec::new();
        for (key, e) in &mut self.entries {
            if matches!(e.probe, Probe::Ready(_)) {
                keys.push(key.clone());
            } else {
                e.wants_preload = e.preloaders > 0;
            }
        }
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
                give_back(&mut self.reserved, &self.pool.preloads, e.reserved);
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
        give_back(&mut self.reserved, &self.pool.preloads, e.reserved);
        e.reserved = 0;
        e.preloaded = None;
        e.preload_failed = false;
        e.over_budget = false;
        // The budget is the engine's: every doc's preloads together (AUD-05, M4.15e1).
        if self
            .pool
            .preloads
            .load(Ordering::SeqCst)
            .saturating_add(bytes)
            > self.budget
        {
            e.over_budget = true;
            e.preload_ticket = 0;
            return;
        }
        self.next_ticket += 1;
        e.preload_ticket = self.next_ticket;
        take_budget(&mut self.reserved, &self.pool.preloads, bytes);
        e.reserved = bytes;
        let job = Job::Preload {
            doc: self.pool.doc,
            key: key.to_owned(),
            ticket: e.preload_ticket,
            path,
            max_bytes: bytes,
        };
        if self.pool.low.send(job).is_err() {
            give_back(&mut self.reserved, &self.pool.preloads, bytes);
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
        give_back(&mut self.reserved, &self.pool.preloads, e.reserved);
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
            // A worker has this job already (or answered it): a second copy would only be
            // dropped, or probe the file twice.
            if lock(&self.pool.gate)
                .claimed
                .get(&(self.pool.doc, true, key.to_owned()))
                .is_some_and(|t| *t >= e.ticket)
            {
                continue;
            }
            if let Some(path) = media_path(&MediaRef::new(key), self.show_dir.as_deref()) {
                // The same ticket: whichever copy answers first counts, the other is ignored.
                let _ = self.pool.high.send(Job::Probe {
                    doc: self.pool.doc,
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
        let path = media_path(&MediaRef::new(key.clone()), self.show_dir.as_deref());
        self.request_at(key, path);
    }

    /// [`Self::request`] of `key`, already resolved to `path`.
    fn request_at(&mut self, key: String, path: Option<PathBuf>) {
        self.next_ticket += 1;
        let ticket = self.next_ticket;
        let asked = match &path {
            Some(path) => self
                .pool
                .low
                .send(Job::Probe {
                    doc: self.pool.doc,
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
        // A new entry, or one that fails at once: what `media` gives may change (when every
        // cue is to be checked again anyway, the checks do not read the keys).
        if !self.plan_changes.all && (!asked || !self.entries.contains_key(&key)) {
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

    /// Reports `key`'s probe `ticket` unplayable, if it is still the one asked and not answered
    /// (the pool's watchdog gave up on it).
    pub fn give_up(&mut self, key: &str, ticket: u64, out: &mut Vec<(MediaRef, MediaStatus)>) {
        if let Some(e) = self.entries.get_mut(key)
            && e.ticket == ticket
            && e.asking
            && matches!(e.probe, Probe::Pending)
        {
            e.probe = Probe::Failed;
            e.timed_out = true;
            self.plan_changes.keys.insert(key.to_owned());
            out.push((
                MediaRef::new(key),
                MediaStatus::Unsupported {
                    reason: "the file could not be read in time (the disk does not answer)"
                        .to_owned(),
                },
            ));
        }
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
                give_back(&mut self.reserved, &self.pool.preloads, entry.reserved);
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
                        give_back(&mut self.reserved, &self.pool.preloads, entry.reserved);
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
        give_back(&mut self.reserved, &self.pool.preloads, entry.reserved);
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
}

impl Drop for MediaCatalog {
    /// A closed doc's preloads give their part of the engine's budget back (the catalog is
    /// dropped off the control thread; the count is atomic).
    fn drop(&mut self) {
        let all = self.reserved;
        give_back(&mut self.reserved, &self.pool.preloads, all);
    }
}

/// Takes `bytes` of the engine's preload budget for one doc.
pub(super) fn take_budget(reserved: &mut u64, used: &AtomicU64, bytes: u64) {
    *reserved = reserved.saturating_add(bytes);
    used.fetch_add(bytes, Ordering::SeqCst);
}

/// Gives back up to `bytes` of what one doc took.
pub(super) fn give_back(reserved: &mut u64, used: &AtomicU64, bytes: u64) {
    let bytes = bytes.min(*reserved);
    *reserved -= bytes;
    // `used` is the sum of every doc's `reserved`, so it holds at least `bytes`.
    used.fetch_sub(bytes, Ordering::SeqCst);
}

/// The file an audio cue plays and whether it asks for it to be preloaded.
pub(super) fn media_use(cue: &Cue) -> Option<(&str, bool)> {
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
pub(super) fn preload_bytes(p: &Probed) -> u64 {
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

#[cfg(test)]
mod tests;
