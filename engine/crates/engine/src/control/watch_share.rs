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
//! Every key change is taken in the wake it comes: a doc's key joins its file's entry at once
//! (made then if no doc had it), so it is counted for every event of the file from then on (its
//! probe was sent before) and no order of other docs' changes can leave it out. Only telling the
//! watch thread of new files, and letting go of a closed doc's keys, is paced: at most
//! [`CHANGES_PER_WAKE`] a wake, the rest in the wakes after, so a doc opening or closing beside
//! another costs no wake most of an audio block; the watch thread learns of new files a few
//! wakes later, which only delays its first pass over them. The maps are keyed by text with a
//! hash worked out where the change was made ([`KeyHashes`]), so joining a file hashes no text
//! on the control thread (a key spelled other than its file's path, a relative one, is copied
//! once).

use std::borrow::Borrow;
use std::collections::hash_map::{self, Entry};
use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;

use super::DocId;
use super::media::{KeyChange, KeyHashes, text_hash};
use super::watch::{MediaWatch, WatchBatch, WatchEvent};

/// New files the watch thread is told of, and closed docs' keys let go of, in one control wake.
/// Each costs about a microsecond on the slowest runner, so this stays well inside an audio
/// block; 2,000 new files take eight wakes.
pub const CHANGES_PER_WAKE: usize = 256;

/// Text with its hash ([`text_hash`]), as the share's maps key it.
#[derive(Clone, Debug)]
struct Text {
    hash: u64,
    text: Arc<str>,
}

/// A map key, owned ([`Text`]) or a borrowed text with its hash, so a lookup copies nothing.
trait Hashed {
    fn hash_value(&self) -> u64;
    fn text(&self) -> &str;
}

impl Hashed for Text {
    fn hash_value(&self) -> u64 {
        self.hash
    }
    fn text(&self) -> &str {
        &self.text
    }
}

impl Hashed for (u64, &str) {
    fn hash_value(&self) -> u64 {
        self.0
    }
    fn text(&self) -> &str {
        self.1
    }
}

impl<'a> Borrow<dyn Hashed + 'a> for Text {
    fn borrow(&self) -> &(dyn Hashed + 'a) {
        self
    }
}

impl Hash for dyn Hashed + '_ {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.hash_value());
    }
}

impl PartialEq for dyn Hashed + '_ {
    fn eq(&self, other: &Self) -> bool {
        self.hash_value() == other.hash_value() && self.text() == other.text()
    }
}

impl Eq for dyn Hashed + '_ {}

// As the borrowed form, so a lookup by either finds the same entry.
impl Hash for Text {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.hash);
    }
}

impl PartialEq for Text {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash && self.text == other.text
    }
}

impl Eq for Text {}

/// Hands on the keyed hash a [`Text`] carries: the maps do not hash it again.
#[derive(Default)]
struct HashedHasher(u64);

impl Hasher for HashedHasher {
    fn write_u64(&mut self, n: u64) {
        self.0 = n;
    }
    fn write(&mut self, bytes: &[u8]) {
        // Only `write_u64` is used; anything else still hashes, slowly.
        for b in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(*b);
        }
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

type Map<V> = HashMap<Text, V, BuildHasherDefault<HashedHasher>>;

fn lookup(hash: u64, text: &str) -> (u64, &str) {
    (hash, text)
}

/// A doc's use of a watched file: its key and its watch id for it.
type Sub = (DocId, Arc<str>);

/// One doc's keys: the watch key each of its keys is counted under.
type DocKeys = Map<Text>;

/// One file the watch thread stats, or is to be told of.
struct Watched {
    key: Text,
    path: PathBuf,
    /// The engine's watch id for it (the watch thread reports events under it).
    id: u64,
    /// The watch thread was told of it (else it waits its turn in `WatchShare::told`).
    told: bool,
    /// A subscriber's probe failed before the watch thread was told of it (`missing`: no
    /// file): told right after it.
    failed: Option<bool>,
    /// Every doc key that names this file, with that doc's watch id for it.
    subs: Subs,
}

/// The doc keys naming one file, with each doc's watch id for it: a few at most (one per doc
/// using the file, mostly), kept in the order they came.
#[derive(Default)]
struct Subs(Vec<(Sub, u64)>);

impl Subs {
    fn insert(&mut self, sub: Sub, id: u64) {
        match self.0.iter_mut().find(|(s, _)| *s == sub) {
            Some((_, old)) => *old = id,
            None => {
                self.0.reserve_exact(1);
                self.0.push((sub, id));
            }
        }
    }

    fn get(&self, sub: &Sub) -> Option<u64> {
        self.0.iter().find(|(s, _)| s == sub).map(|(_, id)| *id)
    }

    fn remove(&mut self, sub: &Sub) {
        self.0.retain(|(s, _)| s != sub);
    }

    fn len(&self) -> usize {
        self.0.len()
    }
}

/// What came off the share's maps, only there to be dropped: the caller hands it to the
/// document worker so the control thread does not free thousands of strings.
#[derive(Default)]
struct Gone {
    subs: Vec<Sub>,
    files: Vec<Watched>,
    paths: Vec<PathBuf>,
    strings: Vec<String>,
    keys: Vec<Text>,
    bulk: Vec<Box<dyn Send>>,
}

impl Gone {
    fn is_empty(&self) -> bool {
        self.subs.is_empty()
            && self.files.is_empty()
            && self.paths.is_empty()
            && self.strings.is_empty()
            && self.keys.is_empty()
            && self.bulk.is_empty()
    }
}

pub struct WatchShare {
    watch: MediaWatch,
    /// Every file a doc names, by the watch thread's key for it.
    files: Map<Watched>,
    /// Files the watch thread was told of.
    told: usize,
    /// Each open doc's keys.
    keys: HashMap<DocId, DocKeys>,
    /// New files the watch thread is still to be told of, in the order they came, with their
    /// watch id (an entry let go of, or made again, meanwhile is passed over).
    untold: VecDeque<(Text, u64)>,
    /// Closed docs' keys not let go of yet (a closed doc's events find no doc meanwhile).
    leaving: VecDeque<(DocId, hash_map::IntoIter<Text, Text>)>,
    /// What this wake may still do.
    budget: usize,
    gone: Gone,
    next_id: u64,
}

impl WatchShare {
    pub fn new(watch: MediaWatch) -> Self {
        Self {
            watch,
            files: Map::default(),
            told: 0,
            keys: HashMap::new(),
            untold: VecDeque::new(),
            leaving: VecDeque::new(),
            budget: CHANGES_PER_WAKE,
            gone: Gone::default(),
            next_id: 0,
        }
    }

    pub fn watch(&self) -> &MediaWatch {
        &self.watch
    }

    pub fn watch_mut(&mut self) -> &mut MediaWatch {
        &mut self.watch
    }

    /// The control wake is over: the next one may do up to [`CHANGES_PER_WAKE`] again.
    pub fn end_wake(&mut self) {
        self.budget = CHANGES_PER_WAKE;
    }

    /// Doc `doc`'s catalog changes ([`super::media::MediaCatalog::take_key_changes`]), all
    /// taken now: the watch thread hears of new files within this wake's budget and the next
    /// wakes'.
    pub fn apply(&mut self, doc: DocId, changes: Vec<KeyChange>) {
        if changes.is_empty() {
            return;
        }
        // A doc opening asks for thousands at once: room is made once.
        let watches = changes
            .iter()
            .filter(|c| matches!(c, KeyChange::Watch { .. }))
            .count();
        self.files.reserve(watches);
        let mut keys = self.keys.remove(&doc).unwrap_or_default();
        keys.reserve(watches);
        let mut out = Vec::new();
        for change in changes {
            match change {
                KeyChange::Reset => {
                    self.keys.insert(doc, keys);
                    self.reset_doc(doc, &mut out);
                    keys = self.keys.remove(&doc).unwrap_or_default();
                }
                KeyChange::Watch {
                    key,
                    path,
                    id,
                    hashes,
                } => self.subscribe(&mut keys, doc, key, path, id, hashes, &mut out),
                KeyChange::Unwatch { key } => {
                    let hash = text_hash(&key);
                    self.unsubscribe(&mut keys, doc, hash, &key, &mut out);
                }
                KeyChange::ProbeFailed { key, id, missing } => {
                    self.probe_failed(&keys, doc, &key, id, missing, &mut out);
                }
            }
        }
        self.keys.insert(doc, keys);
        self.watch.apply(out);
        self.pump();
    }

    /// Everything doc `doc` watched is let go (the doc closed): its keys are let go of by
    /// [`Self::pump`] in the wakes after (at once when no other doc watches anything). What
    /// comes off the maps is handed out by [`Self::take_gone`].
    pub fn leave(&mut self, doc: DocId) {
        let mut out = Vec::new();
        if !self.reset_all(doc, &mut out) {
            let keys = self.keys.remove(&doc).unwrap_or_default();
            self.leaving.push_back((doc, keys.into_iter()));
        }
        self.watch.apply(out);
    }

    /// What came off the maps since the last call, for the caller to drop off the control
    /// thread (`None`: nothing).
    pub fn take_gone(&mut self) -> Option<Box<dyn Send>> {
        (!self.gone.is_empty()).then(|| Box::new(std::mem::take(&mut self.gone)) as Box<dyn Send>)
    }

    /// New files not told to the watch thread yet and closed docs' keys not let go of yet.
    pub fn waiting(&self) -> usize {
        self.untold.len() + self.leaving.iter().map(|(_, k)| k.len()).sum::<usize>()
    }

    /// Lets go of closed docs' keys, then tells the watch thread of new files, until this
    /// wake's budget is spent.
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
                    let sub = (doc, key.text);
                    self.release(&sub, watch_key, &mut out);
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
            let Some((key, id)) = self.untold.pop_front() else {
                break;
            };
            // Let go of (or let go of and made again) before its turn: nothing to tell.
            if let Some(file) = self.files.get_mut(&key).filter(|f| f.id == id && !f.told) {
                self.budget -= 1;
                file.told = true;
                self.told += 1;
                out.push(KeyChange::Watch {
                    key: key.text.to_string(),
                    path: file.path.clone(),
                    id,
                    hashes: KeyHashes {
                        key: key.hash,
                        path: key.hash,
                    },
                });
                if let Some(missing) = file.failed.take() {
                    out.push(KeyChange::ProbeFailed {
                        key: key.text.to_string(),
                        id,
                        missing,
                    });
                }
            }
            self.gone.keys.push(key);
        }
        self.watch.apply(out);
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
        gone.push(Box::new(std::mem::take(&mut self.untold)));
        self.told = 0;
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
            let sub = (doc, key.text);
            self.release(&sub, watch_key, out);
            self.gone.subs.push(sub);
        }
    }

    /// Doc `doc`'s key `doc_key` (watch id `id`) for `path` joins its file's entry, made now if
    /// no doc has it (the watch thread is told of it in its turn).
    #[allow(clippy::too_many_arguments)]
    fn subscribe(
        &mut self,
        keys: &mut DocKeys,
        doc: DocId,
        doc_key: String,
        path: PathBuf,
        id: u64,
        hashes: KeyHashes,
        out: &mut Vec<KeyChange>,
    ) {
        let mut hash = hashes.path;
        let found = path
            .to_str()
            .and_then(|text| self.files.get(&lookup(hash, text) as &dyn Hashed))
            .filter(|f| f.path == path)
            .map(|f| f.key.clone());
        let key = if let Some(key) = found {
            // Freed off the control thread, with what the share lets go of.
            self.gone.paths.push(path);
            key
        } else {
            let lossy = path.to_string_lossy();
            let text: Arc<str> =
                // Two paths that read the same once made text (not valid Unicode): kept apart.
                if self.files.contains_key(&lookup(hash, &lossy) as &dyn Hashed) {
                    self.next_id += 1;
                    let text = format!("{lossy}\u{0}{}", self.next_id);
                    hash = text_hash(&text);
                    text.into()
                } else {
                    Arc::from(&*lossy)
                };
            let key = Text { hash, text };
            self.next_id += 1;
            self.untold.push_back((key.clone(), self.next_id));
            self.files.insert(
                key.clone(),
                Watched {
                    key: key.clone(),
                    path,
                    id: self.next_id,
                    told: false,
                    failed: None,
                    subs: Subs::default(),
                },
            );
            key
        };
        let Some(file) = self.files.get_mut(&key) else {
            return;
        };
        // Most keys read as their file's path: they share its text.
        let doc_key = if *doc_key == *key.text {
            self.gone.strings.push(doc_key);
            key.clone()
        } else {
            Text {
                hash: hashes.key,
                text: doc_key.into(),
            }
        };
        file.subs.insert((doc, Arc::clone(&doc_key.text)), id);
        match keys.entry(doc_key) {
            Entry::Vacant(e) => {
                e.insert(key);
            }
            // It named another file (an edit it missed): that one is let go of.
            Entry::Occupied(mut e) => {
                if *e.get() != key {
                    let old = std::mem::replace(e.get_mut(), key);
                    let sub = (doc, Arc::clone(&e.key().text));
                    self.release(&sub, old, out);
                }
            }
        }
    }

    /// Doc `doc` lets go of its key `doc_key` (hashed `hash`), if it has it.
    fn unsubscribe(
        &mut self,
        keys: &mut DocKeys,
        doc: DocId,
        hash: u64,
        doc_key: &str,
        out: &mut Vec<KeyChange>,
    ) {
        let Some((doc_key, key)) = keys.remove_entry(&lookup(hash, doc_key) as &dyn Hashed) else {
            return;
        };
        let sub = (doc, doc_key.text);
        self.release(&sub, key, out);
        self.gone.subs.push(sub);
    }

    /// Doc `doc`'s probe of its key's file failed: the watch is told, under its own key and
    /// id (once it is told of the file), if the key still names the file under that watch id.
    fn probe_failed(
        &mut self,
        keys: &DocKeys,
        doc: DocId,
        key: &str,
        id: u64,
        missing: bool,
        out: &mut Vec<KeyChange>,
    ) {
        let Some((doc_key, watch_key)) =
            keys.get_key_value(&lookup(text_hash(key), key) as &dyn Hashed)
        else {
            return;
        };
        let Some(file) = self.files.get_mut(watch_key) else {
            return;
        };
        if file.subs.get(&(doc, Arc::clone(&doc_key.text))) != Some(id) {
            return;
        }
        if file.told {
            out.push(KeyChange::ProbeFailed {
                key: watch_key.text.to_string(),
                id: file.id,
                missing,
            });
        } else {
            file.failed = Some(missing);
        }
    }

    /// `sub`, no longer in the share's keys, lets go of its file `key`; the last one to do so
    /// removes the file (and stops its watch, if the watch thread was told of it).
    fn release(&mut self, sub: &Sub, key: Text, out: &mut Vec<KeyChange>) {
        // One look-up of the path, whatever becomes of the file, and no walk of `subs` for its
        // last sub (`sub` is in them: it was still in the share's keys).
        let Entry::Occupied(mut file) = self.files.entry(key) else {
            return;
        };
        if file.get().subs.len() > 1 {
            file.get_mut().subs.remove(sub);
            return;
        }
        let (key, file) = file.remove_entry();
        if file.told {
            self.told -= 1;
            out.push(KeyChange::Unwatch {
                key: key.text.to_string(),
            });
        }
        self.gone.files.push(file);
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
            let key = lookup(text_hash(event.key()), event.key());
            let Some(file) = self
                .files
                .get(&key as &dyn Hashed)
                .filter(|f| f.id == event.id())
            else {
                continue;
            };
            for ((doc, key), id) in &file.subs.0 {
                out.push((*doc, retarget(&event, key, *id)));
            }
        }
        out
    }

    /// Files the watch thread was told to watch (a cost measure: a file several docs use counts
    /// once).
    pub fn watched_files(&self) -> usize {
        self.told
    }
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
            hashes: KeyHashes::of(key, std::path::Path::new(path)),
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

    /// A doc's keys for files the watch has already are taken up in the wake they come, however
    /// many: only keys for new files wait their turn.
    #[test]
    fn keys_for_files_already_watched_are_taken_up_at_once() {
        let (mut s, fs) = share();
        for i in 0..2_000 {
            fs.put(format!("/show/{i}.wav"), 1);
        }
        s.apply(A, many(0, 2_000));
        while s.waiting() > 0 {
            wake(&mut s);
        }
        wake(&mut s);
        // B names every one of A's files and one of its own.
        let mut b = many(0, 2_000);
        b.push(watch("new.wav", "/show/new.wav", 1));
        s.apply(B, b);
        assert_eq!(s.waiting(), 0);
        assert_eq!(s.watched_files(), 2_001);
        pass(&mut s);
        fs.remove("/show/0.wav");
        let docs: Vec<_> = pass(&mut s).into_iter().map(|(d, _)| d).collect();
        assert_eq!(docs, vec![A, B]);
    }

    /// A file whose only key is let go of before the watch thread was told of it is never
    /// watched, nor are a reset doc's files from before the reset.
    #[test]
    fn a_waiting_key_let_go_of_or_reset_is_never_watched() {
        let (mut s, fs) = share();
        fs.put("/show/b.wav", 1);
        s.apply(B, vec![watch("b.wav", "/show/b.wav", 1)]);
        wake(&mut s);
        s.apply(A, many(0, 1_000));
        s.apply(
            A,
            vec![KeyChange::Unwatch {
                key: "999.wav".into(),
            }],
        );
        while s.waiting() > 0 {
            wake(&mut s);
        }
        assert_eq!(s.watched_files(), 1_000, "B's file and 999 of A's");
        s.apply(A, many(1_000, 2_000));
        assert!(s.waiting() > 0);
        s.apply(A, vec![KeyChange::Reset, watch("b.wav", "/show/b.wav", 9)]);
        while s.waiting() > 0 {
            wake(&mut s);
        }
        assert_eq!(s.watched_files(), 1, "only the file both docs name");
    }

    /// Two docs opening on the same new files, one right after the other: the second doc's
    /// keys join the files the first one made before the watch thread hears of them, so an
    /// event of any of them goes to both.
    #[test]
    fn two_docs_opening_on_the_same_new_files_both_hear_their_events() {
        let (mut s, fs) = share();
        for i in 0..2_000 {
            fs.put(format!("/show/{i}.wav"), 1);
        }
        s.apply(A, many(0, 2_000));
        s.apply(B, many(0, 2_000));
        while s.watched_files() < 2_000 {
            wake(&mut s);
        }
        assert!(pass(&mut s).is_empty());
        fs.remove("/show/1999.wav");
        let mut docs: Vec<DocId> = pass(&mut s).into_iter().map(|(d, _)| d).collect();
        while s.waiting() > 0 {
            wake(&mut s);
        }
        docs.extend(pass(&mut s).into_iter().map(|(d, _)| d));
        docs.extend(pass(&mut s).into_iter().map(|(d, _)| d));
        docs.sort();
        assert_eq!(docs, vec![A, B], "both hear 1999.wav went");
    }
}
