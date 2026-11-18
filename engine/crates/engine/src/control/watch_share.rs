// SPDX-License-Identifier: GPL-3.0-or-later

//! One media watch for every open doc. Each
//! doc's catalog names its files by its own keys (media references, relative to that doc's show
//! folder) and its own watch ids; the watch thread is told each file once, by its path, under an
//! id of the engine's. A file two docs use is watched once, counted for both: one doc letting go
//! of it leaves the other's watch running, and every event about it goes to each doc that
//! watches it, under that doc's key and id.
//!
//! With one doc open this is the watch as it was: a doc's reset is the watch's reset (a new
//! generation, so batches of the old one are dropped).
//!
//! A doc opening or closing beside another changes thousands of keys at once. The share takes
//! at most [`CHANGES_PER_WAKE`] of them in one control wake and the rest in the wakes after, in
//! order, so neither costs most of an audio block; the watch thread learns of the files a few
//! wakes later, which only delays its first pass over them. A doc that takes up a file another
//! doc already watches, after the file had an event its change was queued before, is handed
//! the file's last event ([`WatchShare::take_missed`]): its probe may have seen the file as it
//! was before, and the watch does not repeat an event.

use std::collections::hash_map::{self, Entry};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;

use super::DocId;
use super::media::KeyChange;
use super::watch::{MediaWatch, WatchBatch, WatchEvent};

/// Key changes (a doc's, or a closed doc's keys let go of) the share handles in one control
/// wake. A key costs about a microsecond on the slowest runner, so this stays well inside an
/// audio block; 2,000 files take eight wakes.
pub const CHANGES_PER_WAKE: usize = 256;

/// A doc's use of a watched file: its key and its watch id for it.
type Sub = (DocId, String);

/// One doc's keys: the watch key each of its keys is counted under.
type DocKeys = HashMap<String, String>;

/// One file the watch thread stats.
struct Watched {
    path: PathBuf,
    /// The engine's watch id for it (the watch thread reports events under it).
    id: u64,
    /// Every doc key that names this file, with that doc's watch id for it.
    subs: BTreeMap<Sub, u64>,
    /// The file's last event, numbered by the share's count of events then.
    last_event: Option<(u64, WatchEvent)>,
}

/// Changes a doc's catalog handed over at once, with the share's event count then.
struct Batch {
    since: u64,
    changes: VecDeque<KeyChange>,
}

/// What came off the share's maps, only there to be dropped: the caller hands it to the
/// document worker so the control thread does not free thousands of strings.
#[derive(Default)]
struct Gone {
    subs: Vec<Sub>,
    files: Vec<Watched>,
    bulk: Vec<Box<dyn Send>>,
}

impl Gone {
    fn is_empty(&self) -> bool {
        self.subs.is_empty() && self.files.is_empty() && self.bulk.is_empty()
    }
}

pub struct WatchShare {
    watch: MediaWatch,
    /// Watched files by the watch thread's key for them.
    files: HashMap<String, Watched>,
    /// Each open doc's keys.
    keys: HashMap<DocId, DocKeys>,
    /// Each open doc's changes not handled yet, in order.
    queued: BTreeMap<DocId, VecDeque<Batch>>,
    /// Closed docs' keys not let go of yet (a closed doc's events find no doc meanwhile).
    leaving: VecDeque<(DocId, hash_map::IntoIter<String, String>)>,
    /// What this wake may still handle.
    budget: usize,
    gone: Gone,
    next_id: u64,
    /// Events accepted so far.
    events: u64,
    /// Events of files doc keys were taken up for after the event, which their doc did not hear,
    /// as that doc sees them.
    missed: Vec<(DocId, WatchEvent)>,
}

impl WatchShare {
    pub fn new(watch: MediaWatch) -> Self {
        Self {
            watch,
            files: HashMap::new(),
            keys: HashMap::new(),
            queued: BTreeMap::new(),
            leaving: VecDeque::new(),
            budget: CHANGES_PER_WAKE,
            gone: Gone::default(),
            next_id: 0,
            events: 0,
            missed: Vec::new(),
        }
    }

    pub fn watch(&self) -> &MediaWatch {
        &self.watch
    }

    pub fn watch_mut(&mut self) -> &mut MediaWatch {
        &mut self.watch
    }

    /// The control wake is over: the next one may handle up to [`CHANGES_PER_WAKE`] again.
    pub fn end_wake(&mut self) {
        self.budget = CHANGES_PER_WAKE;
    }

    /// Doc `doc`'s catalog changes ([`super::media::MediaCatalog::take_key_changes`]), turned
    /// into the watch thread's jobs, after its earlier ones, within this wake's budget.
    pub fn apply(&mut self, doc: DocId, changes: Vec<KeyChange>) {
        if changes.is_empty() {
            return;
        }
        // A doc opening asks for thousands at once: room for them is made once.
        let watches = changes
            .iter()
            .filter(|c| matches!(c, KeyChange::Watch { .. }))
            .count();
        self.files.reserve(watches);
        self.keys.entry(doc).or_default().reserve(watches);
        let batch = Batch {
            since: self.events,
            changes: VecDeque::from(changes),
        };
        self.queued.entry(doc).or_default().push_back(batch);
        self.pump();
    }

    /// Everything doc `doc` watched is let go (the doc closed): its changes not handled yet are
    /// dropped, and its keys are let go of by [`Self::pump`] in the wakes after (at once when no
    /// other doc watches anything). What comes off the maps is handed out by
    /// [`Self::take_gone`].
    pub fn leave(&mut self, doc: DocId) {
        if let Some(queue) = self.queued.remove(&doc) {
            self.gone.bulk.push(Box::new(queue));
        }
        let mut out = Vec::new();
        if !self.reset_all(doc, &mut out) {
            let keys = self.keys.remove(&doc).unwrap_or_default();
            self.leaving.push_back((doc, keys.into_iter()));
        }
        self.watch.apply(out);
    }

    /// The last event of each file a doc key was taken up for since the last call, when the
    /// event came after the doc's catalog asked for it: the doc did not hear it. As the doc
    /// sees it (its key and watch id).
    pub fn take_missed(&mut self) -> Vec<(DocId, WatchEvent)> {
        std::mem::take(&mut self.missed)
    }

    /// What came off the maps since the last call, for the caller to drop off the control
    /// thread (`None`: nothing).
    pub fn take_gone(&mut self) -> Option<Box<dyn Send>> {
        (!self.gone.is_empty()).then(|| Box::new(std::mem::take(&mut self.gone)) as Box<dyn Send>)
    }

    /// Changes and closed docs' keys still waiting for a wake.
    #[cfg(test)]
    pub fn waiting(&self) -> usize {
        self.queued
            .values()
            .flatten()
            .map(|b| b.changes.len())
            .sum::<usize>()
            + self.leaving.iter().map(|(_, k)| k.len()).sum::<usize>()
    }

    /// Handles what is waiting, closed docs' keys first, until this wake's budget is spent.
    pub fn pump(&mut self) {
        if self.budget == 0 {
            return;
        }
        let mut out = Vec::new();
        while self.budget > 0 {
            let Some((doc, keys)) = self.leaving.front_mut() else {
                break;
            };
            let doc = *doc;
            match keys.next() {
                Some((key, watch_key)) => {
                    self.budget -= 1;
                    let sub = (doc, key);
                    self.gone
                        .files
                        .extend(release(&mut self.files, &sub, watch_key, &mut out));
                    self.gone.subs.push(sub);
                }
                None => {
                    if let Some(done) = self.leaving.pop_front() {
                        self.gone.bulk.push(Box::new(done));
                    }
                }
            }
        }
        while self.budget > 0 {
            let Some(mut queued) = self.queued.first_entry() else {
                break;
            };
            let doc = *queued.key();
            let batches = queued.get_mut();
            let Some(batch) = batches.front_mut() else {
                queued.remove();
                continue;
            };
            let since = batch.since;
            let Some(change) = batch.changes.pop_front() else {
                batches.pop_front();
                continue;
            };
            self.budget -= 1;
            self.change(doc, change, since, &mut out);
        }
        self.queued.retain(|_, q| !q.is_empty());
        self.watch.apply(out);
    }

    /// One of doc `doc`'s changes, handed over when the share had counted `since` events.
    fn change(&mut self, doc: DocId, change: KeyChange, since: u64, out: &mut Vec<KeyChange>) {
        match change {
            KeyChange::Reset => self.reset_doc(doc, out),
            KeyChange::Watch { key, path, id } => {
                self.unsubscribe(doc, &key, out);
                self.subscribe(doc, key, path, id, since, out);
            }
            KeyChange::Unwatch { key } => self.unsubscribe(doc, &key, out),
            KeyChange::ProbeFailed { key, id, missing } => {
                let Some(watch_key) = self.keys.get(&doc).and_then(|k| k.get(&key)) else {
                    return;
                };
                let Some(file) = self.files.get(watch_key) else {
                    return;
                };
                if file.subs.get(&(doc, key)) == Some(&id) {
                    out.push(KeyChange::ProbeFailed {
                        key: watch_key.clone(),
                        id: file.id,
                        missing,
                    });
                }
            }
        }
    }

    /// When no other open doc watches anything, the watch's own reset (a new generation, the
    /// old one's batches dropped): everything comes off at once, closed docs' keys too.
    /// Whether it was.
    fn reset_all(&mut self, doc: DocId, out: &mut Vec<KeyChange>) -> bool {
        if !self
            .keys
            .iter()
            .all(|(d, keys)| *d == doc || keys.is_empty())
        {
            return false;
        }
        let gone = &mut self.gone.bulk;
        gone.push(Box::new(std::mem::take(&mut self.files)));
        gone.push(Box::new(std::mem::take(&mut self.keys)));
        gone.push(Box::new(std::mem::take(&mut self.leaving)));
        out.push(KeyChange::Reset);
        true
    }

    /// A doc's own reset (its catalog starts again): as [`Self::reset_all`] when it is the only
    /// doc watching; else its files are let go of now, before its new keys come.
    fn reset_doc(&mut self, doc: DocId, out: &mut Vec<KeyChange>) {
        if self.reset_all(doc, out) {
            return;
        }
        let keys = self.keys.remove(&doc).unwrap_or_default();
        self.gone.subs.reserve(keys.len());
        for (key, watch_key) in keys {
            let sub = (doc, key);
            self.gone
                .files
                .extend(release(&mut self.files, &sub, watch_key, out));
            self.gone.subs.push(sub);
        }
    }

    fn subscribe(
        &mut self,
        doc: DocId,
        doc_key: String,
        path: PathBuf,
        id: u64,
        since: u64,
        out: &mut Vec<KeyChange>,
    ) {
        let mut key = path.to_string_lossy().into_owned();
        // Two paths that read the same once made text (not valid Unicode): kept apart.
        if self.files.get(&key).is_some_and(|f| f.path != path) {
            self.next_id += 1;
            key = format!("{key}\u{0}{}", self.next_id);
        }
        let file = match self.files.entry(key.clone()) {
            Entry::Occupied(file) => file.into_mut(),
            Entry::Vacant(file) => {
                self.next_id += 1;
                out.push(KeyChange::Watch {
                    key: file.key().clone(),
                    path: path.clone(),
                    id: self.next_id,
                });
                file.insert(Watched {
                    path,
                    id: self.next_id,
                    subs: BTreeMap::new(),
                    last_event: None,
                })
            }
        };
        // The file had an event after the doc's catalog asked for it (and probed it): the doc
        // did not hear it, and the watch will not repeat it.
        if let Some((n, event)) = &file.last_event
            && *n > since
        {
            self.missed.push((doc, retarget(event, &doc_key, id)));
        }
        file.subs.insert((doc, doc_key.clone()), id);
        self.keys.entry(doc).or_default().insert(doc_key, key);
    }

    fn unsubscribe(&mut self, doc: DocId, doc_key: &str, out: &mut Vec<KeyChange>) {
        let Some((doc_key, key)) = self
            .keys
            .get_mut(&doc)
            .and_then(|keys| keys.remove_entry(doc_key))
        else {
            return;
        };
        release(&mut self.files, &(doc, doc_key), key, out);
    }

    /// A batch from the watch thread: its events (of the current generation) for each doc that
    /// watches their file, under the doc's own key and id. Events of a file no doc watches any
    /// more, or of an earlier watch of it, are dropped.
    pub fn accept(&mut self, batch: WatchBatch) -> Vec<(DocId, WatchEvent)> {
        let Some(events) = self.watch.accept(batch) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for event in events {
            let Some(file) = self
                .files
                .get_mut(event.key())
                .filter(|f| f.id == event.id())
            else {
                continue;
            };
            self.events += 1;
            for ((doc, key), id) in &file.subs {
                out.push((*doc, retarget(&event, key, *id)));
            }
            file.last_event = Some((self.events, event));
        }
        out
    }

    /// Files the watch thread is told to watch (a cost measure: a file several docs use counts
    /// once).
    pub fn watched_files(&self) -> usize {
        self.files.len()
    }
}

/// `sub`, no longer in the share's keys, lets go of its file `key`; the last one to do so
/// stops the file's watch and gets the file back.
fn release(
    files: &mut HashMap<String, Watched>,
    sub: &Sub,
    key: String,
    out: &mut Vec<KeyChange>,
) -> Option<Watched> {
    // One hash of the path, whatever becomes of the file, and no walk of `subs` for its last sub
    // (`sub` is in them: it was still in the share's keys).
    let Entry::Occupied(mut file) = files.entry(key) else {
        return None;
    };
    if file.get().subs.len() > 1 {
        file.get_mut().subs.remove(sub);
        return None;
    }
    let (key, file) = file.remove_entry();
    out.push(KeyChange::Unwatch { key });
    Some(file)
}

/// `event` as the doc that names its file `key` (watch id `id`) sees it.
fn retarget(event: &WatchEvent, key: &str, id: u64) -> WatchEvent {
    let key = key.to_owned();
    match event {
        WatchEvent::Gone { trashed, .. } => WatchEvent::Gone {
            key,
            id,
            trashed: *trashed,
        },
        WatchEvent::Back { fp, .. } => WatchEvent::Back { key, id, fp: *fp },
        WatchEvent::Changed { fp, .. } => WatchEvent::Changed { key, id, fp: *fp },
        WatchEvent::Unreadable { error, .. } => WatchEvent::Unreadable {
            key,
            id,
            error: error.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::watch::{FakeFs, WatchConfig, WatchMode};
    use std::sync::Arc;

    const A: DocId = DocId(0);
    const B: DocId = DocId(1);

    fn share() -> (WatchShare, FakeFs) {
        let fs = FakeFs::new();
        let config = WatchConfig {
            mode: WatchMode::Manual,
            fs: Arc::new(fs.clone()),
            ..WatchConfig::default()
        };
        let (tx, _rx) = crossbeam_channel::unbounded();
        let watch = MediaWatch::new(&config, tx).unwrap();
        (WatchShare::new(watch), fs)
    }

    fn watch(key: &str, path: &str, id: u64) -> KeyChange {
        KeyChange::Watch {
            key: key.into(),
            path: PathBuf::from(path),
            id,
        }
    }

    /// One control wake's share of what is waiting, as `Control::tick` gives it.
    fn wake(s: &mut WatchShare) {
        s.end_wake();
        s.pump();
        drop(s.take_gone());
    }

    /// Doc `doc` closes and the wakes after it let go of its keys.
    fn close(s: &mut WatchShare, doc: DocId) {
        s.leave(doc);
        drop(s.take_gone());
        while s.waiting() > 0 {
            wake(s);
        }
    }

    fn pass(s: &mut WatchShare) -> Vec<(DocId, WatchEvent)> {
        match s.watch_mut().manual_pass() {
            Some(batch) => s.accept(batch),
            None => Vec::new(),
        }
    }

    #[test]
    fn a_file_two_docs_use_is_watched_once_and_reported_to_both() {
        let (mut s, fs) = share();
        fs.put("/show/a.wav", 1);
        // Doc A names it relatively, doc B absolutely; each has its own watch id.
        s.apply(A, vec![KeyChange::Reset, watch("a.wav", "/show/a.wav", 3)]);
        s.apply(
            B,
            vec![KeyChange::Reset, watch("/show/a.wav", "/show/a.wav", 9)],
        );
        assert_eq!(s.watched_files(), 1);
        assert!(pass(&mut s).is_empty());
        fs.remove("/show/a.wav");
        let mut events = pass(&mut s);
        events.sort_by_key(|(d, _)| *d);
        assert_eq!(
            events,
            vec![
                (
                    A,
                    WatchEvent::Gone {
                        key: "a.wav".into(),
                        id: 3,
                        trashed: false
                    }
                ),
                (
                    B,
                    WatchEvent::Gone {
                        key: "/show/a.wav".into(),
                        id: 9,
                        trashed: false
                    }
                ),
            ]
        );
    }

    #[test]
    fn one_doc_letting_go_keeps_the_others_watch() {
        let (mut s, fs) = share();
        fs.put("/show/a.wav", 1);
        s.apply(A, vec![watch("a.wav", "/show/a.wav", 1)]);
        s.apply(B, vec![watch("a.wav", "/show/a.wav", 1)]);
        pass(&mut s);
        // A forgets it (an edit), then resets (another workspace); B still watches.
        s.apply(
            A,
            vec![KeyChange::Unwatch {
                key: "a.wav".into(),
            }],
        );
        s.apply(A, vec![KeyChange::Reset]);
        assert_eq!(s.watched_files(), 1);
        fs.remove("/show/a.wav");
        let events = pass(&mut s);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, B);
        // B closes: nothing is watched.
        close(&mut s, B);
        assert_eq!(s.watched_files(), 0);
        fs.put("/show/a.wav", 2);
        assert!(pass(&mut s).is_empty());
    }

    #[test]
    fn a_doc_closing_beside_another_lets_go_of_only_its_own_files() {
        let (mut s, fs) = share();
        for n in 0..100 {
            fs.put(format!("/show/{n}.wav"), 1);
        }
        // A watches 0..60, B watches 40..100: 40..60 are shared.
        let a: Vec<_> = (0..60)
            .map(|n| watch(&format!("{n}.wav"), &format!("/show/{n}.wav"), n + 1))
            .collect();
        let b: Vec<_> = (40..100)
            .map(|n| watch(&format!("{n}.wav"), &format!("/show/{n}.wav"), n + 1))
            .collect();
        s.apply(A, a);
        s.apply(B, b);
        assert_eq!(s.watched_files(), 100);
        assert!(pass(&mut s).is_empty());
        close(&mut s, B);
        assert_eq!(s.watched_files(), 60);
        // No reset: A's watches go on, B's own files are no longer watched.
        for n in [10, 50, 80] {
            fs.remove(format!("/show/{n}.wav"));
        }
        let mut events = pass(&mut s);
        events.sort_by_key(|(_, e)| e.id());
        let keys: Vec<_> = events
            .iter()
            .map(|(d, e)| (*d, e.key().to_owned()))
            .collect();
        assert_eq!(keys, vec![(A, "10.wav".into()), (A, "50.wav".into())]);
        // And A can close too.
        close(&mut s, A);
        assert_eq!(s.watched_files(), 0);
    }

    /// A file's last sub takes it along with no look at who the sub is: with two subs of one doc
    /// each counts, whichever way they go.
    #[test]
    fn a_file_one_doc_names_twice_is_watched_until_both_names_go() {
        let (mut s, fs) = share();
        fs.put("/show/a.wav", 1);
        fs.put("/show/b.wav", 1);
        s.apply(B, vec![watch("b.wav", "/show/b.wav", 1)]);
        let twice = |n| {
            vec![
                watch("a.wav", "/show/a.wav", n),
                watch("/show/a.wav", "/show/a.wav", n + 1),
            ]
        };
        // One name by an edit, then the other.
        s.apply(A, twice(1));
        assert_eq!(s.watched_files(), 2);
        let unwatch = |key: &str| KeyChange::Unwatch { key: key.into() };
        s.apply(A, vec![unwatch("a.wav")]);
        assert_eq!(s.watched_files(), 2, "the other name still watches it");
        s.apply(A, vec![unwatch("/show/a.wav")]);
        assert_eq!(s.watched_files(), 1);
        // Both at once, by the close; B's file stays and is still reported.
        s.apply(A, twice(3));
        assert_eq!(s.watched_files(), 2);
        close(&mut s, A);
        assert_eq!(s.watched_files(), 1);
        pass(&mut s);
        fs.remove("/show/a.wav");
        fs.remove("/show/b.wav");
        let events = pass(&mut s);
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].0, B);
    }

    #[test]
    fn the_same_key_in_two_docs_is_two_files_when_their_folders_differ() {
        let (mut s, fs) = share();
        fs.put("/one/a.wav", 1);
        fs.put("/two/a.wav", 2);
        s.apply(A, vec![watch("a.wav", "/one/a.wav", 1)]);
        s.apply(B, vec![watch("a.wav", "/two/a.wav", 1)]);
        assert_eq!(s.watched_files(), 2);
        pass(&mut s);
        fs.remove("/two/a.wav");
        let events = pass(&mut s);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, B);
    }

    #[test]
    fn a_probe_failure_of_a_stale_watch_is_not_passed_on() {
        let (mut s, fs) = share();
        fs.put("/show/a.wav", 1);
        s.apply(A, vec![watch("a.wav", "/show/a.wav", 5)]);
        pass(&mut s);
        // An older watch id: ignored; the current one marks the file gone for the watch.
        s.apply(
            A,
            vec![KeyChange::ProbeFailed {
                key: "a.wav".into(),
                id: 4,
                missing: true,
            }],
        );
        assert!(pass(&mut s).is_empty());
        s.apply(
            A,
            vec![KeyChange::ProbeFailed {
                key: "a.wav".into(),
                id: 5,
                missing: true,
            }],
        );
        // Taken as gone: the file found at the next passes is reported back.
        pass(&mut s);
        let events = pass(&mut s);
        assert!(
            events
                .iter()
                .any(|(d, e)| *d == A && matches!(e, WatchEvent::Back { id: 5, .. })),
            "{events:?}"
        );
    }

    fn many(from: u64, to: u64) -> Vec<KeyChange> {
        (from..to)
            .map(|n| watch(&format!("{n}.wav"), &format!("/show/{n}.wav"), n + 1))
            .collect()
    }

    /// A doc opening beside another asks for thousands of files at once: one wake handles at
    /// most `CHANGES_PER_WAKE` of them, the wakes after the rest, in order (a probe failure
    /// after its watch is passed on once the watch is there).
    #[test]
    fn a_doc_opening_is_watched_over_a_few_wakes() {
        let (mut s, fs) = share();
        fs.put("/show/b.wav", 1);
        s.apply(B, vec![watch("b.wav", "/show/b.wav", 1)]);
        wake(&mut s);
        let n = 2_000;
        for i in 0..n {
            fs.put(format!("/show/{i}.wav"), 1);
        }
        let mut changes = many(0, n);
        changes.push(KeyChange::ProbeFailed {
            key: format!("{}.wav", n - 1),
            id: n,
            missing: true,
        });
        s.apply(A, changes);
        let mut wakes = 1;
        let mut seen = vec![s.watched_files()];
        while s.waiting() > 0 {
            wake(&mut s);
            wakes += 1;
            seen.push(s.watched_files());
        }
        assert_eq!(s.watched_files(), n as usize + 1);
        assert_eq!(wakes, (n as usize + 1).div_ceil(CHANGES_PER_WAKE));
        assert!(
            seen.windows(2).all(|w| w[1] - w[0] <= CHANGES_PER_WAKE),
            "{seen:?}"
        );
        // Every file is watched, and the failed probe's file is not reported gone again.
        assert!(pass(&mut s).is_empty());
        fs.remove(format!("/show/{}.wav", n - 1));
        fs.remove("/show/0.wav");
        let events = pass(&mut s);
        let keys: Vec<_> = events.iter().map(|(d, e)| (*d, e.key())).collect();
        assert_eq!(keys, vec![(A, "0.wav")], "{events:?}");
    }

    /// A doc closing beside another lets go of its files over the wakes after the close, at
    /// most `CHANGES_PER_WAKE` a wake; what comes off the maps is handed out to be dropped
    /// elsewhere, and the close itself lets go of nothing yet.
    #[test]
    fn a_doc_closing_beside_another_lets_go_over_a_few_wakes() {
        let (mut s, fs) = share();
        fs.put("/show/b.wav", 1);
        s.apply(B, vec![watch("b.wav", "/show/b.wav", 1)]);
        let n = 1_000;
        s.apply(A, many(0, n));
        while s.waiting() > 0 {
            wake(&mut s);
        }
        assert_eq!(s.watched_files(), n as usize + 1);
        s.leave(A);
        assert_eq!(s.watched_files(), n as usize + 1);
        assert_eq!(s.waiting(), n as usize);
        let mut handed = 0;
        let mut seen = vec![s.watched_files()];
        while s.waiting() > 0 {
            s.end_wake();
            s.pump();
            if s.take_gone().is_some() {
                handed += 1;
            }
            seen.push(s.watched_files());
        }
        assert_eq!(s.watched_files(), 1, "B's file stays");
        assert_eq!(handed, (n as usize).div_ceil(CHANGES_PER_WAKE));
        assert!(
            seen.windows(2).all(|w| w[0] - w[1] <= CHANGES_PER_WAKE),
            "{seen:?}"
        );
        // B's file is still reported.
        pass(&mut s);
        fs.remove("/show/b.wav");
        let events = pass(&mut s);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, B);
    }

    /// A doc that closes before its changes were all handled never has the rest watched.
    #[test]
    fn a_doc_closing_before_its_files_were_watched_watches_none_of_the_rest() {
        let (mut s, fs) = share();
        fs.put("/show/b.wav", 1);
        s.apply(B, vec![watch("b.wav", "/show/b.wav", 1)]);
        wake(&mut s);
        s.apply(A, many(0, 1_000));
        assert!(s.waiting() > 0);
        close(&mut s, A);
        for _ in 0..8 {
            wake(&mut s);
        }
        assert_eq!(s.watched_files(), 1);
        assert_eq!(s.waiting(), 0);
    }

    /// A file a closed doc is still letting go of, taken up by another doc meanwhile, stays
    /// watched for that doc once the closed doc's keys are gone.
    #[test]
    fn a_file_taken_up_while_a_closed_doc_lets_go_of_it_stays_watched() {
        const C: DocId = DocId(2);
        let (mut s, fs) = share();
        fs.put("/show/b.wav", 1);
        for i in 0..600 {
            fs.put(format!("/show/{i}.wav"), 1);
        }
        s.apply(B, vec![watch("b.wav", "/show/b.wav", 1)]);
        s.apply(A, many(0, 600));
        while s.waiting() > 0 {
            wake(&mut s);
        }
        s.leave(A);
        // Another doc names one of A's files before A's keys are let go of.
        s.apply(C, vec![watch("599.wav", "/show/599.wav", 7)]);
        while s.waiting() > 0 {
            wake(&mut s);
        }
        assert_eq!(s.watched_files(), 2);
        pass(&mut s);
        fs.remove("/show/599.wav");
        let events = pass(&mut s);
        assert_eq!(
            events,
            vec![(
                C,
                WatchEvent::Gone {
                    key: "599.wav".into(),
                    id: 7,
                    trashed: false
                }
            )]
        );
    }
}
