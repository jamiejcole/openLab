// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::super::DocId;
use super::super::watch::{RETRY_FIRST, RETRY_MAX, bucket_of};
use super::{Job, JobKind};

/// The volume of everything not on a mount of its own (Unix); a probe-side verdict on it is
/// meaningless, see [`Gate::exempt`].
pub(super) const SYSTEM_VOLUME: &str = "/";

/// The system drive's name as the media watch spells it (`C:`): `SystemDrive` from the
/// environment (`env`), else `C:`. Only Windows reads it; the rule is tested on any host.
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) fn system_drive(env: impl Fn(&str) -> Option<String>) -> String {
    env("SystemDrive")
        .map(|d| d.trim_end_matches(['\\', '/']).to_owned())
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "C:".to_owned())
}

/// Different files with a stuck probe that make a volume stalled on the probes' word alone.
pub(super) const CORROBORATION: usize = 2;

/// A volume with jobs in flight or a stall to remember.
#[derive(Debug, Default)]
pub(super) struct Volume {
    in_flight: usize,
    /// Files whose probe was given up on and none has answered since (preloads never count).
    stuck: HashSet<String>,
    /// The trial job running (a stalled volume has at most one), by id.
    trial: Option<u64>,
    /// No trial before this (set when a trial got stuck or failed).
    retry_at: Option<Instant>,
    backoff: Duration,
}

impl Volume {
    fn next_wait(&mut self) -> Duration {
        let wait = self.backoff.max(RETRY_FIRST);
        self.backoff = wait.saturating_mul(2).min(RETRY_MAX);
        wait
    }

    fn trial_due(&self, now: Instant) -> bool {
        self.trial.is_none() && self.retry_at.is_none_or(|t| now >= t)
    }
}

/// A job a worker has taken.
pub(super) struct Taken {
    pub(super) job: Job,
    /// The gate's epoch then: a job taken before a reset does not touch the new state.
    pub(super) epoch: u64,
    pub(super) trial: Option<u64>,
    pub(super) bucket: String,
}

/// What the workers and the control thread share about the volumes (never held across I/O).
#[derive(Debug, Default)]
pub(super) struct Gate {
    /// The highest ticket a worker took per (doc, job kind, file): a copy of a job already
    /// taken (the high-priority copy [`MediaCatalog::prioritize`] sends) is dropped, not run
    /// again. Tickets are each doc's own.
    pub(super) claimed: HashMap<(DocId, JobKind, String), u64>,
    pub(super) volumes: HashMap<String, Volume>,
    /// Jobs for stalled volumes (and whether they came from the high-priority queue).
    pub(super) deferred: Vec<(Job, bool)>,
    /// Volumes the probes alone found stalled (`true`) or answering again (`false`), for the log.
    pub(super) events: Vec<(String, bool)>,
    /// Volumes the media watch reports stalled (its notice is the only one).
    pub(super) watch_stalled: HashSet<String>,
    /// Which volume a path is on (the media watch's `bucket_of` unless a test injects one).
    pub(super) volume: Option<fn(&Path) -> String>,
    /// The system drive on Windows (`C:`); the Unix system volume `/` is always exempt.
    pub(super) system_drive: Option<String>,
    /// The folder of each open doc's show file.
    pub(super) show_dirs: BTreeMap<DocId, PathBuf>,
    /// The docs whose catalogs send jobs.
    pub(super) members: BTreeSet<DocId>,
    /// Docs closed: their jobs still in the queues are dropped when a worker takes them
    /// (`DocId`s are never reused, so this only grows by one per close).
    pub(super) left: BTreeSet<DocId>,
    pub(super) epoch: u64,
    pub(super) next_trial: u64,
}

impl Gate {
    fn bucket(&self, path: &Path) -> String {
        self.volume.map_or_else(|| bucket_of(path), |f| f(path))
    }

    /// Volumes never held back: the system volume (`/`, or the system drive) mixes the local
    /// disk with automounts and cloud folders, and the volume holding the open show file is
    /// the one an operator cannot do without. Only other volumes (removable drives, network
    /// shares, other mounts) can be stalled for probe scheduling.
    ///
    /// With several docs open the show file's volume is exempt no more (M4.15e review M2): the
    /// workers are shared, and a doc whose show volume hangs would otherwise take every worker,
    /// one 30 s abandonment after another, until no worker is left for the other docs. It is
    /// then held back only once two of its files' probes are stuck past the probe watchdog
    /// (`stalled`), never on the media watch's word alone (review M-B: one slow stat, a disk
    /// spinning up, must not slow the show's own probes because another workspace is open).
    pub(super) fn exempt(&self, bucket: &str) -> bool {
        bucket == SYSTEM_VOLUME
            || self
                .system_drive
                .as_ref()
                .is_some_and(|d| d.eq_ignore_ascii_case(bucket))
            || self.members.len() <= 1
                && self
                    .show_dirs
                    .values()
                    // A file in the folder: `bucket_of` calls a path directly inside a mount
                    // folder (a show at the root of a volume) part of `/`.
                    .any(|d| self.bucket(&d.join("show")).eq_ignore_ascii_case(bucket))
    }

    /// An open show file's volume (whether or not it is [`Self::exempt`]).
    fn show_volume(&self, bucket: &str) -> bool {
        self.show_dirs
            .values()
            .any(|d| self.bucket(&d.join("show")).eq_ignore_ascii_case(bucket))
    }

    pub(super) fn stalled(&self, bucket: &str) -> bool {
        if self.exempt(bucket) {
            return false;
        }
        let stuck = self
            .volumes
            .get(bucket)
            .is_some_and(|v| v.stuck.len() >= CORROBORATION);
        // A show's own volume: on its probes' word only (see `exempt`).
        stuck || (!self.show_volume(bucket) && self.watch_stalled.contains(bucket))
    }

    /// May a job start on `bucket` now: it works, or it is stalled and a trial is due.
    fn may_start(&self, bucket: &str, now: Instant) -> bool {
        !self.stalled(bucket) || self.volumes.get(bucket).is_none_or(|v| v.trial_due(now))
    }

    /// Takes a job for a worker: `Some` to run it, `None` if it was dropped (taken already) or
    /// deferred (its volume is stalled and no trial is due).
    pub(super) fn admit(&mut self, job: Job, high: bool, now: Instant) -> Option<Taken> {
        let (key, ticket) = job.key();
        // A closed doc's job, still in a queue: nobody wants its answer.
        if self.left.contains(&job.doc()) {
            return None;
        }
        let claim = (job.doc(), job.kind(), key.to_owned());
        if self.claimed.get(&claim).is_some_and(|t| *t >= ticket) {
            return None;
        }
        let bucket = self.bucket(job.path());
        if !self.may_start(&bucket, now) {
            self.defer(job, high);
            return None;
        }
        let trial = self.stalled(&bucket).then(|| {
            self.next_trial += 1;
            self.next_trial
        });
        self.claimed.insert(claim, ticket);
        let v = self.volumes.entry(bucket.clone()).or_default();
        v.in_flight += 1;
        v.trial = trial.or(v.trial);
        Some(Taken {
            job,
            epoch: self.epoch,
            trial,
            bucket,
        })
    }

    /// One deferred job per file and kind: a newer ticket replaces an older one, and a job
    /// asked for urgently stays urgent.
    fn defer(&mut self, job: Job, high: bool) {
        let (key, ticket) = job.key();
        let same = |j: &Job| j.doc() == job.doc() && j.kind() == job.kind() && j.key().0 == key;
        if let Some((_, h)) = self
            .deferred
            .iter_mut()
            .find(|(j, _)| same(j) && j.key().1 >= ticket)
        {
            *h |= high;
            return;
        }
        let high = high || self.deferred.iter().any(|(j, h)| same(j) && *h);
        self.deferred.retain(|(j, _)| !same(j));
        self.deferred.push((job, high));
    }

    fn tidy(&mut self, bucket: &str) {
        if self
            .volumes
            .get(bucket)
            .is_some_and(|v| v.in_flight == 0 && v.stuck.is_empty() && v.trial.is_none())
        {
            self.volumes.remove(bucket);
        }
    }

    /// A job taken in `epoch` ended. An answer (even a file that is missing or damaged) shows
    /// the volume works and ends the probe-side stall; an I/O error does not, so a failed
    /// trial waits before the next.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn finished(
        &mut self,
        epoch: u64,
        bucket: &str,
        key: &str,
        probe: bool,
        trial: Option<u64>,
        answered: bool,
        now: Instant,
    ) {
        if epoch != self.epoch {
            return;
        }
        let watch = self.watch_stalled.contains(bucket);
        let Some(v) = self.volumes.get_mut(bucket) else {
            return;
        };
        v.in_flight = v.in_flight.saturating_sub(1);
        let was_stalled = v.stuck.len() >= CORROBORATION;
        if probe {
            v.stuck.remove(key);
        }
        let mine = trial.is_some() && v.trial == trial;
        if mine {
            v.trial = None;
        }
        if answered {
            if was_stalled && !watch {
                self.events.push((bucket.to_owned(), false));
            }
            v.stuck.clear();
            v.retry_at = None;
            v.backoff = Duration::ZERO;
        } else if mine {
            let wait = v.next_wait();
            v.retry_at = Some(now + wait);
        }
        self.tidy(bucket);
    }

    /// A job taken in `epoch` is stuck (the watchdog gave up on its worker). A stuck probe is
    /// counted against its volume; a stuck trial stops blocking the next one, after a wait.
    pub(super) fn timed_out(
        &mut self,
        epoch: u64,
        bucket: &str,
        key: &str,
        probe: bool,
        trial: Option<u64>,
        now: Instant,
    ) {
        if epoch != self.epoch || self.exempt(bucket) {
            return;
        }
        let watch = self.watch_stalled.contains(bucket);
        let v = self.volumes.entry(bucket.to_owned()).or_default();
        if probe {
            let before = v.stuck.len();
            v.stuck.insert(key.to_owned());
            if before < CORROBORATION && v.stuck.len() >= CORROBORATION && !watch {
                self.events.push((bucket.to_owned(), true));
            }
        }
        if trial.is_some() && v.trial == trial {
            v.trial = None;
            let wait = v.next_wait();
            v.retry_at = Some(now + wait);
        }
    }

    /// The deferred jobs that may run now: all on a volume that works, one trial on a stalled
    /// volume that is due.
    pub(super) fn release_due(&mut self, now: Instant) -> Vec<(Job, bool)> {
        let mut tried: HashSet<String> = HashSet::new();
        let mut out = Vec::new();
        // Urgent jobs (files near a playhead) first, so a volume's one trial is theirs and
        // they go out ahead of the rest (review M-B); each kind in the order it came.
        let (high, low): (Vec<_>, Vec<_>) = std::mem::take(&mut self.deferred)
            .into_iter()
            .partition(|(_, h)| *h);
        for (job, high) in high.into_iter().chain(low) {
            let bucket = self.bucket(job.path());
            let go = if self.stalled(&bucket) {
                self.may_start(&bucket, now) && tried.insert(bucket)
            } else {
                true
            };
            if go {
                out.push((job, high));
            } else {
                self.deferred.push((job, high));
            }
        }
        out
    }

    /// Another workspace: nothing known about the volumes carries over, and jobs still running
    /// do not report into the new state.
    fn reset(&mut self) {
        self.epoch += 1;
        self.claimed.clear();
        self.volumes.clear();
        self.deferred.clear();
        self.events.clear();
    }

    /// Doc `doc` replaced its workspace, now in `show_dir`. When it is the only doc, as
    /// [`Self::reset`]; with other docs open, what is known about the volumes is theirs too
    /// and stays: only this doc's claims and waiting jobs go (its tickets keep rising, so its
    /// jobs still running give stale answers).
    pub(super) fn reset_doc(&mut self, doc: DocId, show_dir: Option<PathBuf>) {
        if self.members.iter().all(|&d| d == doc) {
            self.reset();
        } else {
            self.forget(doc);
        }
        self.members.insert(doc);
        match show_dir {
            Some(dir) => {
                self.show_dirs.insert(doc, dir);
            }
            None => {
                self.show_dirs.remove(&doc);
            }
        }
    }

    /// Drops a doc's claims and waiting jobs.
    fn forget(&mut self, doc: DocId) {
        self.claimed.retain(|(d, _, _), _| *d != doc);
        self.deferred.retain(|(j, _)| j.doc() != doc);
    }

    /// A doc closed: it sends no more jobs, and its show folder is no longer exempt.
    pub(super) fn leave(&mut self, doc: DocId) {
        self.forget(doc);
        self.left.insert(doc);
        self.members.remove(&doc);
        self.show_dirs.remove(&doc);
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    fn job(key: &str, ticket: u64) -> Job {
        Job::Probe {
            doc: DocId(0),
            key: key.to_owned(),
            ticket,
            path: PathBuf::from(key),
        }
    }

    fn preload(key: &str, ticket: u64) -> Job {
        Job::Preload {
            doc: DocId(0),
            key: key.to_owned(),
            ticket,
            path: PathBuf::from(key),
            max_bytes: 1,
        }
    }

    pub(in super::super) const DEAD: &str = "/mnt/dead";
    const SEC: Duration = Duration::from_secs(1);

    /// Takes `job` as a worker would, and has it stuck.
    fn stuck(gate: &mut Gate, key: &str, ticket: u64, now: Instant) -> Taken {
        let t = gate.admit(job(key, ticket), false, now).unwrap();
        gate.timed_out(t.epoch, &bucket_of(Path::new(key)), key, true, t.trial, now);
        t
    }

    #[test]
    fn one_stuck_probe_or_any_stuck_preloads_do_not_stall_a_volume() {
        let t0 = Instant::now();
        let mut gate = Gate::default();
        stuck(&mut gate, "/Users/me/Show/slow.mp3", 1, t0);
        // The system disk still takes every other file.
        for n in 2..6 {
            assert!(
                gate.admit(job(&format!("/Users/me/other/{n}.wav"), n), false, t0)
                    .is_some()
            );
        }
        // Stuck preloads (the same or different files) never count.
        for n in 10..14 {
            let key = format!("/Users/me/pre/{n}.wav");
            let t = gate.admit(preload(&key, n), false, t0).unwrap();
            gate.timed_out(t.epoch, "/", &key, false, t.trial, t0);
        }
        assert!(!gate.stalled("/"));
        assert!(gate.admit(job("/Users/me/x.wav", 20), false, t0).is_some());
        assert!(gate.events.is_empty() && gate.deferred.is_empty());
        // The same file stuck twice is still one file.
        gate.timed_out(gate.epoch, "/", "/Users/me/Show/slow.mp3", true, None, t0);
        assert!(!gate.stalled("/"));
    }

    #[test]
    fn two_stuck_files_stall_a_volume_one_trial_at_a_time_with_backoff() {
        let t0 = Instant::now();
        let mut gate = Gate::default();
        stuck(&mut gate, "/mnt/dead/a.wav", 1, t0);
        stuck(&mut gate, "/mnt/dead/b.wav", 2, t0);
        assert_eq!(gate.events, vec![(DEAD.to_owned(), true)]);
        // Healthy volumes always proceed.
        assert!(gate.admit(job("/mnt/ok/c.wav", 3), false, t0).is_some());
        assert!(gate.admit(job("/Users/me/c.wav", 3), false, t0).is_some());
        // The stuck probes do not block the first trial; then the rest wait.
        let trial = gate.admit(job("/mnt/dead/c.wav", 4), false, t0).unwrap();
        assert!(trial.trial.is_some());
        assert!(gate.admit(job("/mnt/dead/d.wav", 5), true, t0).is_none());
        assert!(gate.admit(job("/mnt/dead/e.wav", 6), false, t0).is_none());
        assert!(gate.release_due(t0).is_empty());
        // The trial gets stuck too: an abandoned job does not block the next one, after 30 s,
        // then 60 s, doubling to 300 s.
        let mut now = t0;
        let mut ticket = 10;
        let mut trial = trial;
        for wait in [30, 60, 120, 240, 300, 300] {
            gate.timed_out(trial.epoch, DEAD, trial.job.key().0, true, trial.trial, now);
            let early = now + Duration::from_secs(wait - 1);
            assert!(gate.release_due(early).is_empty(), "{wait}");
            now += Duration::from_secs(wait);
            let mut out = gate.release_due(now);
            assert_eq!(out.len(), 1, "one trial per stalled volume");
            ticket += 1;
            trial = gate.admit(out.remove(0).0, false, now).unwrap();
            assert!(trial.trial.is_some());
            // Others still wait while it runs.
            assert!(gate.release_due(now).is_empty());
            ticket += 1;
            assert!(
                gate.admit(job("/mnt/dead/y.wav", ticket), false, now)
                    .is_none()
            );
        }
        // The trial answers (the file is missing: the volume works): everything is released.
        gate.finished(
            trial.epoch,
            DEAD,
            trial.job.key().0,
            true,
            trial.trial,
            true,
            now,
        );
        assert_eq!(gate.events.last(), Some(&(DEAD.to_owned(), false)));
        assert!(!gate.stalled(DEAD));
        assert!(!gate.release_due(now).is_empty());
        assert!(gate.deferred.is_empty());
    }

    #[test]
    fn a_failing_trial_waits_and_a_late_return_of_a_stuck_job_clears_the_stall() {
        let t0 = Instant::now();
        let mut gate = Gate::default();
        let a = stuck(&mut gate, "/mnt/dead/a.wav", 1, t0);
        stuck(&mut gate, "/mnt/dead/b.wav", 2, t0);
        let t = gate.admit(job("/mnt/dead/c.wav", 3), false, t0).unwrap();
        gate.finished(t.epoch, DEAD, "/mnt/dead/c.wav", true, t.trial, false, t0);
        assert!(
            gate.admit(job("/mnt/dead/d.wav", 4), false, t0 + 29 * SEC)
                .is_none()
        );
        assert!(
            gate.admit(job("/mnt/dead/d.wav", 4), false, t0 + 30 * SEC)
                .is_some()
        );
        // A stuck job comes back with an answer: the volume works.
        gate.finished(
            a.epoch,
            DEAD,
            "/mnt/dead/a.wav",
            true,
            a.trial,
            true,
            t0 + 31 * SEC,
        );
        assert!(!gate.stalled(DEAD));
        assert_eq!(gate.events.last(), Some(&(DEAD.to_owned(), false)));
    }

    #[test]
    fn a_reset_forgets_the_volumes_and_late_jobs_do_not_touch_the_new_state() {
        let t0 = Instant::now();
        let mut gate = Gate::default();
        // Two jobs that never return (a FIFO, a hard-mounted share).
        let a = stuck(&mut gate, "/mnt/dead/a.wav", 1, t0);
        stuck(&mut gate, "/mnt/dead/b.wav", 2, t0);
        assert!(gate.stalled(DEAD));
        gate.reset();
        assert!(!gate.stalled(DEAD));
        assert!(
            gate.admit(job("/mnt/dead/a.wav", 3), false, t0)
                .unwrap()
                .trial
                .is_none()
        );
        // The old job ends now: the new state is not changed (no stall, no in-flight count).
        gate.finished(a.epoch, DEAD, "/mnt/dead/a.wav", true, None, false, t0);
        gate.timed_out(a.epoch, DEAD, "/mnt/dead/a.wav", true, None, t0);
        assert_eq!(gate.volumes[DEAD].in_flight, 1);
        assert!(gate.volumes[DEAD].stuck.is_empty());
    }

    fn job_of(doc: u64, key: &str, ticket: u64) -> Job {
        Job::Probe {
            doc: DocId(doc),
            key: key.to_owned(),
            ticket,
            path: PathBuf::from(key),
        }
    }

    /// Two docs' tickets are their own: one doc's job is never taken for a copy of another's.
    #[test]
    fn the_same_file_in_two_docs_is_probed_for_each() {
        let t0 = Instant::now();
        let mut gate = Gate::default();
        assert!(gate.admit(job_of(0, "/a/k.wav", 5), false, t0).is_some());
        assert!(gate.admit(job_of(1, "/a/k.wav", 3), false, t0).is_some());
        // Copies of either are still dropped.
        assert!(gate.admit(job_of(0, "/a/k.wav", 5), true, t0).is_none());
        assert!(gate.admit(job_of(1, "/a/k.wav", 3), true, t0).is_none());
    }

    /// The volumes are the engine's: one doc replacing its workspace forgets only its own
    /// claims and waiting jobs while another doc is open; alone, it forgets everything.
    /// With two docs open, a show's own volume is never held back on the media watch's word:
    /// its probes (the playhead's first) run at once; only two of its probes stuck past the
    /// watchdog hold it back (review M-B, M2).
    #[test]
    fn a_show_volume_is_held_back_on_stuck_probes_only_while_several_docs_are_open() {
        let t0 = Instant::now();
        let mut gate = Gate {
            members: BTreeSet::from([DocId(0), DocId(1)]),
            show_dirs: BTreeMap::from([(DocId(0), PathBuf::from("/Volumes/Shows/Hamlet"))]),
            ..Gate::default()
        };
        // The watch saw one slow stat there (a disk spinning up).
        gate.watch_stalled.insert("/Volumes/Shows".to_owned());
        assert!(!gate.exempt("/Volumes/Shows"), "several docs: not exempt");
        assert!(!gate.stalled("/Volumes/Shows"));
        for n in 1..=5 {
            let taken = gate.admit(job(&format!("/Volumes/Shows/Hamlet/{n}.wav"), n), true, t0);
            assert!(
                taken.is_some_and(|t| t.trial.is_none()),
                "file {n} held back"
            );
        }
        // Another volume the watch reports is held back as before.
        gate.watch_stalled.insert("/Volumes/Other".to_owned());
        assert!(gate.stalled("/Volumes/Other"));
        // Two of the show's probes stuck past the watchdog: now it is held back.
        stuck(&mut gate, "/Volumes/Shows/Hamlet/a.wav", 10, t0);
        stuck(&mut gate, "/Volumes/Shows/Hamlet/b.wav", 11, t0);
        assert!(gate.stalled("/Volumes/Shows"));
    }

    /// Jobs held back go out urgent ones first: the trial of a stalled volume is the file at
    /// the playhead, not the next of hundreds that came before it (review M-B).
    #[test]
    fn held_back_jobs_are_released_urgent_first() {
        let t0 = Instant::now();
        let mut gate = Gate::default();
        stuck(&mut gate, "/mnt/dead/a.wav", 1, t0);
        stuck(&mut gate, "/mnt/dead/b.wav", 2, t0);
        assert!(gate.stalled(DEAD));
        // The one trial goes now; everything after it waits, the urgent one last to arrive.
        assert!(gate.admit(job("/mnt/dead/c.wav", 3), false, t0).is_some());
        for n in 4..20 {
            assert!(
                gate.admit(job(&format!("/mnt/dead/{n}.wav"), n), false, t0)
                    .is_none()
            );
        }
        assert!(
            gate.admit(job("/mnt/dead/playhead.wav", 30), true, t0)
                .is_none()
        );
        // The trial fails; after its wait the next one is due: it is the urgent job.
        gate.finished(
            gate.epoch,
            DEAD,
            "/mnt/dead/c.wav",
            true,
            Some(1),
            false,
            t0,
        );
        let due = gate.release_due(t0 + Duration::from_secs(600));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].0.key().0, "/mnt/dead/playhead.wav");
        assert!(due[0].1);
    }

    #[test]
    fn one_docs_reset_keeps_what_the_others_know_about_the_volumes() {
        let t0 = Instant::now();
        let mut gate = Gate {
            members: BTreeSet::from([DocId(0), DocId(1)]),
            ..Gate::default()
        };
        stuck(&mut gate, "/mnt/dead/a.wav", 1, t0);
        stuck(&mut gate, "/mnt/dead/b.wav", 2, t0);
        assert!(gate.stalled(DEAD));
        // Jobs of both docs wait for the stalled volume (a trial took the only turn).
        assert!(
            gate.admit(job_of(0, "/mnt/dead/c.wav", 3), false, t0)
                .is_some()
        );
        assert!(
            gate.admit(job_of(0, "/mnt/dead/d.wav", 4), false, t0)
                .is_none()
        );
        assert!(
            gate.admit(job_of(1, "/mnt/dead/e.wav", 1), false, t0)
                .is_none()
        );
        assert!(gate.admit(job_of(1, "/ok/f.wav", 2), false, t0).is_some());
        gate.reset_doc(DocId(0), None);
        assert!(gate.stalled(DEAD), "doc 1 still needs to know");
        assert_eq!(gate.deferred.len(), 1);
        assert_eq!(gate.deferred[0].0.doc(), DocId(1));
        assert!(gate.claimed.keys().all(|(d, _, _)| *d == DocId(1)));
        // Doc 1 closes; doc 0, alone, resets: nothing carries over.
        gate.leave(DocId(1));
        assert!(gate.deferred.is_empty());
        gate.reset_doc(DocId(0), Some(PathBuf::from("/Volumes/Show/x")));
        assert!(!gate.stalled(DEAD));
        assert!(gate.exempt("/Volumes/Show"));
        gate.leave(DocId(0));
        assert!(
            !gate.exempt("/Volumes/Show"),
            "a closed show's volume is no longer exempt"
        );
    }

    #[test]
    fn a_probe_and_a_preload_of_one_file_do_not_shadow_each_other() {
        let t0 = Instant::now();
        let mut gate = Gate::default();
        // The preload (ticket 13) is taken before the probe (ticket 12): both run.
        assert!(gate.admit(preload("/a/k.wav", 13), false, t0).is_some());
        assert!(gate.admit(job("/a/k.wav", 12), false, t0).is_some());
        // A copy of either is dropped.
        assert!(gate.admit(job("/a/k.wav", 12), true, t0).is_none());
        assert!(gate.admit(preload("/a/k.wav", 13), true, t0).is_none());
        assert!(gate.deferred.is_empty());
    }

    #[test]
    fn a_watch_stall_defers_probes_and_makes_no_notice_of_its_own() {
        let t0 = Instant::now();
        let mut gate = Gate::default();
        gate.watch_stalled.insert(DEAD.to_owned());
        let trial = gate.admit(job("/mnt/dead/a.wav", 1), false, t0).unwrap();
        assert!(trial.trial.is_some());
        assert!(gate.admit(job("/mnt/dead/b.wav", 2), false, t0).is_none());
        assert!(gate.admit(job("/mnt/ok/b.wav", 2), false, t0).is_some());
        // The stuck probes add up to the same verdict: the watch's notice is the only one.
        gate.timed_out(trial.epoch, DEAD, "/mnt/dead/a.wav", true, trial.trial, t0);
        gate.timed_out(gate.epoch, DEAD, "/mnt/dead/c.wav", true, None, t0);
        assert!(gate.events.is_empty());
        // The trial answered: the next waits no backoff, one at a time while the watch says so.
        gate.finished(
            trial.epoch,
            DEAD,
            "/mnt/dead/a.wav",
            true,
            trial.trial,
            true,
            t0,
        );
        assert_eq!(gate.release_due(t0).len(), 1);
    }

    #[test]
    fn a_deferred_job_asked_for_urgently_stays_urgent() {
        let t0 = Instant::now();
        let mut gate = Gate::default();
        gate.watch_stalled.insert(DEAD.to_owned());
        assert!(gate.admit(job("/mnt/dead/a.wav", 1), false, t0).is_some());
        assert!(gate.admit(job("/mnt/dead/b.wav", 2), false, t0).is_none());
        assert!(gate.admit(job("/mnt/dead/b.wav", 2), true, t0).is_none());
        assert!(gate.admit(job("/mnt/dead/b.wav", 2), false, t0).is_none());
        assert_eq!(gate.deferred.len(), 1);
        assert!(gate.deferred[0].1);
    }

    #[test]
    fn the_system_drive_and_the_show_files_volume_are_never_held_back() {
        // The system drive comes from the environment, `C:` if it is not there.
        assert_eq!(system_drive(|_| Some("D:".to_owned())), "D:");
        assert_eq!(system_drive(|_| Some("D:\\".to_owned())), "D:");
        assert_eq!(system_drive(|_| Some(String::new())), "C:");
        assert_eq!(system_drive(|_| None), "C:");
        let t0 = Instant::now();
        let mut gate = Gate {
            system_drive: Some(system_drive(|_| None)),
            show_dirs: BTreeMap::from([(DocId(0), PathBuf::from("/Volumes/Shows/Hamlet"))]),
            ..Gate::default()
        };
        // Stuck probes on the system drive (any case), the show file's volume and a
        // watch-reported stall of either gate nothing.
        for v in ["C:", "c:", "/Volumes/Shows"] {
            gate.watch_stalled.insert(v.to_owned());
            for n in 0..3 {
                gate.timed_out(gate.epoch, v, &format!("{v}/{n}.wav"), true, None, t0);
            }
            assert!(!gate.stalled(v), "{v}");
        }
        assert!(gate.events.is_empty());
        // Another volume still can be.
        for n in 0..2 {
            gate.timed_out(
                gate.epoch,
                "/Volumes/Other",
                &format!("{n}.wav"),
                true,
                None,
                t0,
            );
        }
        assert!(gate.stalled("/Volumes/Other"));
        let taken = gate.admit(job("/Volumes/Shows/Hamlet/media/a.wav", 1), true, t0);
        assert!(taken.is_some_and(|t| t.trial.is_none()));
        // Another show is opened from elsewhere: the old show's volume is open to the verdict.
        gate.show_dirs = BTreeMap::from([(DocId(0), PathBuf::from("/Users/me/Show"))]);
        gate.watch_stalled.clear();
        gate.timed_out(gate.epoch, "/Volumes/Shows", "x.wav", true, None, t0);
        gate.timed_out(gate.epoch, "/Volumes/Shows", "y.wav", true, None, t0);
        assert!(gate.stalled("/Volumes/Shows"));
    }

    #[test]
    fn the_system_disk_is_never_held_back() {
        let t0 = Instant::now();
        let mut gate = Gate::default();
        gate.watch_stalled.insert("/".to_owned());
        // Any number of stuck probes and trials on `/`: the next job still runs at once.
        for n in 0..5 {
            let key = format!("/Users/me/Show/long{n}.mp3");
            stuck(&mut gate, &key, n + 1, t0);
        }
        assert!(!gate.stalled("/"));
        let t = gate
            .admit(job("/Users/me/Show/short.wav", 9), true, t0)
            .unwrap();
        assert!(t.trial.is_none());
        assert!(gate.events.is_empty() && gate.deferred.is_empty());
        assert!(gate.volumes["/"].stuck.is_empty());
    }
}
