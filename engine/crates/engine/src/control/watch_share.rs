// SPDX-License-Identifier: GPL-3.0-or-later

//! One media watch for every open doc (ARCHITECTURE §3.1; docs/control.md "Media watch"). Each
//! doc's catalog names its files by its own keys (media references, relative to that doc's show
//! folder) and its own watch ids; the watch thread is told each file once, by its path, under an
//! id of the engine's. A file two docs use is watched once, counted for both: one doc letting go
//! of it leaves the other's watch running, and every event about it goes to each doc that
//! watches it, under that doc's key and id.
//!
//! With one doc open this is the watch as it was: a doc's reset is the watch's reset (a new
//! generation, so batches of the old one are dropped).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use super::DocId;
use super::media::KeyChange;
use super::watch::{MediaWatch, WatchBatch, WatchEvent};

/// A doc's use of a watched file: its key and its watch id for it.
type Sub = (DocId, String);

/// One file the watch thread stats.
struct Watched {
    path: PathBuf,
    /// The engine's watch id for it (the watch thread reports events under it).
    id: u64,
    /// Every doc key that names this file, with that doc's watch id for it.
    subs: BTreeMap<Sub, u64>,
}

pub struct WatchShare {
    watch: MediaWatch,
    /// Watched files by the watch thread's key for them.
    files: HashMap<String, Watched>,
    /// The watch key each doc key is counted under.
    keys: HashMap<Sub, String>,
    next_id: u64,
}

impl WatchShare {
    pub fn new(watch: MediaWatch) -> Self {
        Self {
            watch,
            files: HashMap::new(),
            keys: HashMap::new(),
            next_id: 0,
        }
    }

    pub fn watch(&self) -> &MediaWatch {
        &self.watch
    }

    pub fn watch_mut(&mut self) -> &mut MediaWatch {
        &mut self.watch
    }

    /// Doc `doc`'s catalog changes ([`super::media::MediaCatalog::take_key_changes`]), turned
    /// into the watch thread's jobs.
    pub fn apply(&mut self, doc: DocId, changes: Vec<KeyChange>) {
        if changes.is_empty() {
            return;
        }
        let mut out = Vec::with_capacity(changes.len());
        // A doc opening asks for thousands at once: room for them is made once.
        let watches = changes
            .iter()
            .filter(|c| matches!(c, KeyChange::Watch { .. }))
            .count();
        self.keys.reserve(watches);
        self.files.reserve(watches);
        for change in changes {
            match change {
                KeyChange::Reset => self.reset_doc(doc, &mut out),
                KeyChange::Watch { key, path, id } => {
                    let sub = (doc, key);
                    self.unsubscribe(&sub, &mut out);
                    self.subscribe(sub, path, id, &mut out);
                }
                KeyChange::Unwatch { key } => self.unsubscribe(&(doc, key), &mut out),
                KeyChange::ProbeFailed { key, id, missing } => {
                    let sub = (doc, key);
                    let Some(file) = self.keys.get(&sub).and_then(|k| self.files.get(k)) else {
                        continue;
                    };
                    if file.subs.get(&sub) == Some(&id) {
                        out.push(KeyChange::ProbeFailed {
                            key: watch_key_of(&self.keys, &sub),
                            id: file.id,
                            missing,
                        });
                    }
                }
            }
        }
        self.watch.apply(out);
    }

    /// Everything doc `doc` watched is let go (the doc closed).
    pub fn leave(&mut self, doc: DocId) {
        let mut out = Vec::new();
        self.reset_doc(doc, &mut out);
        self.watch.apply(out);
    }

    /// A doc's reset. When no other doc watches anything, the watch's own reset (a new
    /// generation, the old one's batches dropped); else only this doc's files are let go.
    fn reset_doc(&mut self, doc: DocId, out: &mut Vec<KeyChange>) {
        if self.keys.keys().all(|(d, _)| *d == doc) {
            self.files.clear();
            self.keys.clear();
            out.push(KeyChange::Reset);
            return;
        }
        // Taken out in one pass, with no copy of its thousands of keys: closing a 2,000-file
        // doc beside another must fit in an audio block (M4.15i).
        for (sub, key) in self.keys.extract_if(|(d, _), _| *d == doc) {
            release(&mut self.files, &sub, key, out);
        }
    }

    fn subscribe(&mut self, sub: Sub, path: PathBuf, id: u64, out: &mut Vec<KeyChange>) {
        let mut key = path.to_string_lossy().into_owned();
        // Two paths that read the same once made text (not valid Unicode): kept apart.
        if self.files.get(&key).is_some_and(|f| f.path != path) {
            self.next_id += 1;
            key = format!("{key}\u{0}{}", self.next_id);
        }
        let file = match self.files.get_mut(&key) {
            Some(file) => file,
            None => {
                self.next_id += 1;
                out.push(KeyChange::Watch {
                    key: key.clone(),
                    path: path.clone(),
                    id: self.next_id,
                });
                self.files.entry(key.clone()).or_insert(Watched {
                    path,
                    id: self.next_id,
                    subs: BTreeMap::new(),
                })
            }
        };
        file.subs.insert(sub.clone(), id);
        self.keys.insert(sub, key);
    }

    fn unsubscribe(&mut self, sub: &Sub, out: &mut Vec<KeyChange>) {
        let Some(key) = self.keys.remove(sub) else {
            return;
        };
        release(&mut self.files, sub, key, out);
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
            let Some(file) = self.files.get(event.key()).filter(|f| f.id == event.id()) else {
                continue;
            };
            for ((doc, key), id) in &file.subs {
                out.push((*doc, retarget(&event, key, *id)));
            }
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
/// stops the file's watch.
fn release(files: &mut HashMap<String, Watched>, sub: &Sub, key: String, out: &mut Vec<KeyChange>) {
    let Some(file) = files.get_mut(&key) else {
        return;
    };
    file.subs.remove(sub);
    if file.subs.is_empty() {
        files.remove(&key);
        out.push(KeyChange::Unwatch { key });
    }
}

fn watch_key_of(keys: &HashMap<Sub, String>, sub: &Sub) -> String {
    keys.get(sub).cloned().unwrap_or_default()
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
        s.leave(B);
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
        s.leave(B);
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
        s.leave(A);
        assert_eq!(s.watched_files(), 0);
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
}
