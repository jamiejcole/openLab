// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ItemState {
    /// Not statted since it was watched: the probe is under way anyway, so a file found is
    /// taken silently.
    Unknown,
    Present,
    Gone,
    Unreadable,
}

#[derive(Debug)]
pub(super) struct WatchItem {
    path: PathBuf,
    bucket: String,
    id: u64,
    state: ItemState,
    /// The file as it was last accepted.
    seen: Option<Fingerprint>,
    /// A different file seen on the last pass, waiting for a second identical one.
    candidate: Option<Fingerprint>,
    pub(super) symlink: bool,
    /// The catalog's probe failed while the path was unreadable: its return is reported even
    /// with the file unchanged.
    stale: bool,
}

/// One file a watcher is to stat.
#[derive(Clone, Debug)]
pub struct Target {
    key: String,
    id: u64,
    pub(super) path: PathBuf,
    pub(super) bucket: String,
}

/// The watch state and the pass rules (docs/persistence.md "Media watch and follow").
#[derive(Debug, Default)]
pub struct WatchCore {
    generation: u64,
    /// In key order, which is the pass order (files of one folder stay together).
    pub(super) items: BTreeMap<String, WatchItem>,
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
pub(super) fn answers(stat: &io::Result<Stat>) -> bool {
    match stat {
        Ok(_) => true,
        Err(e) => matches!(
            e.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
        ),
    }
}

pub(super) fn gone(item: &mut WatchItem, key: String, id: u64) -> Option<WatchEvent> {
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

/// Time a pass allows per file. A stat costs 1 us on a dev machine and 4-7 us on the Intel CI
/// runner (a syscall: nothing left to trim), so 2 ms keeps the watch near 0.3% of a core there.
pub const PER_FILE: Duration = Duration::from_millis(2);

/// How long a pass over `n` files takes: [`PER_FILE`] a file at least, so a show of thousands
/// does not stat its volume flat out.
pub fn pass_period(n: usize, min: Duration) -> Duration {
    min.max(PER_FILE.saturating_mul(u32::try_from(n).unwrap_or(u32::MAX)))
}

/// The pause after each chunk of a pass over `n` files.
pub fn chunk_gap(n: usize, min: Duration) -> Duration {
    let chunks = n.div_ceil(CHUNK).max(1);
    pass_period(n, min) / u32::try_from(chunks).unwrap_or(u32::MAX)
}
