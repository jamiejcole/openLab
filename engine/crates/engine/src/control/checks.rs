// SPDX-License-Identifier: GPL-3.0-or-later

//! Which audio cues' slices cannot be played with their file (`bad_plan` warnings), kept per
//! cue and recomputed only when the cue's trims and markers or its file's probe change: an edit
//! of one cue in a show of thousands builds one plan, not thousands (docs/control.md).
//!
//! Only those cues are looked at, too (M4.12c): the media catalog notes the cues each edit
//! touched and the files whose probe changed ([`PlanChanges`]), and the checks keep the files'
//! cues, so a check after an edit costs the edit, not a walk of every audio cue. A newly
//! followed workspace (and an index with duplicate ids) walks every cue as before:
//! [`PlanChecks::check_all`], the reference the incremental path equals (property-tested in
//! `media.rs`).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::num::NonZeroU32;
use std::sync::Arc;

use openlab_core::{Cue, CueId, CueKind, SliceMarker, Workspace};

use super::media::{MediaCatalog, PlanChanges, Ready};
use super::plan::{PlanFailure, PlanSpec, cue_slices};

/// A cue's last plan check against a probe of its file.
struct Entry {
    /// Held, not just compared by address: a freed probe's address can come back for another.
    probe: Arc<Ready>,
    start: u64,
    end: Option<u64>,
    marks: Vec<(u64, u32)>,
    last: u32,
    error: Option<String>,
}

impl Entry {
    fn matches(&self, probe: &Arc<Ready>, spec: &PlanSpec<'_>) -> bool {
        Arc::ptr_eq(&self.probe, probe)
            && self.start == spec.start_time.to_bits()
            && self.end == spec.end_time.map(f64::to_bits)
            && self.last == spec.last_slice_play_count
            && self.marks.len() == spec.slices.len()
            && self
                .marks
                .iter()
                .zip(spec.slices)
                .all(|(&(t, n), m)| t == m.time.to_bits() && n == m.play_count)
    }
}

/// An audio cue with a file: the file, and the check once the file is probed.
struct Tracked {
    key: String,
    entry: Option<Entry>,
}

#[derive(Default)]
pub struct PlanChecks {
    cues: HashMap<CueId, Tracked>,
    /// The cues of `cues` on each file, for a probe change.
    by_key: HashMap<String, BTreeSet<CueId>>,
    /// The cues whose slices cannot be played, with why.
    errors: BTreeMap<CueId, String>,
    /// The last answer, in workspace order.
    out: Vec<(CueId, String)>,
    /// The rate the checks were made at.
    rate: Option<NonZeroU32>,
    builds: u64,
    examined: u64,
    walks: u64,
}

impl PlanChecks {
    /// The cues (in workspace order) whose slices cannot be played, with why: the cues the
    /// catalog noted since the last call are checked again, the others keep their answer.
    pub fn check(
        &mut self,
        ws: &Workspace,
        media: &mut MediaCatalog,
        rate: NonZeroU32,
    ) -> Vec<(CueId, String)> {
        let changes = media.take_plan_changes();
        if changes.all || self.rate != Some(rate) || media.index().has_duplicates() {
            return self.check_all(ws, media, rate);
        }
        if !changes.is_empty() && self.follow(ws, media, rate, changes).is_none() {
            // The index disagrees with the workspace: start over.
            return self.check_all(ws, media, rate);
        }
        self.out.clone()
    }

    /// Checks the cues `changes` names, and those on the files it names; then orders the
    /// answer by the index. `None` if the index cannot place a cue it has.
    fn follow(
        &mut self,
        ws: &Workspace,
        media: &MediaCatalog,
        rate: NonZeroU32,
        changes: PlanChanges,
    ) -> Option<()> {
        let index = media.index();
        let mut dirty = changes.cues;
        for key in &changes.keys {
            if let Some(on) = self.by_key.get(key) {
                dirty.extend(on.iter().copied());
            }
        }
        for id in dirty {
            let cue = if index.contains(id) {
                Some(index.locate(ws, id)?)
            } else {
                None
            };
            self.update(id, cue, media, rate);
        }
        // Moves change the order without changing the cues, so it is found again; only the
        // cues with a warning are placed.
        let mut placed: Vec<(Vec<usize>, CueId)> = Vec::with_capacity(self.errors.len());
        for id in self.errors.keys() {
            placed.push((index.path(*id)?, *id));
        }
        placed.sort();
        self.out = placed
            .into_iter()
            .filter_map(|(_, id)| self.errors.get(&id).map(|e| (id, e.clone())))
            .collect();
        Some(())
    }

    /// The reference: every audio cue walked in workspace order and checked (a cue whose file
    /// and trims are as last checked keeps its answer), the cues no longer there forgotten.
    pub fn check_all(
        &mut self,
        ws: &Workspace,
        media: &MediaCatalog,
        rate: NonZeroU32,
    ) -> Vec<(CueId, String)> {
        self.walks += 1;
        if self.rate != Some(rate) {
            // Every plan depends on the rate.
            self.cues.clear();
            self.by_key.clear();
            self.errors.clear();
            self.rate = Some(rate);
        }
        let mut seen: BTreeSet<CueId> = BTreeSet::new();
        let mut out = Vec::new();
        for list in &ws.cue_lists {
            each_cue(&list.cues, &mut |c| {
                if audio_spec(c).is_none() {
                    return;
                }
                seen.insert(c.id);
                self.update(c.id, Some(c), media, rate);
                if let Some(e) = self.errors.get(&c.id) {
                    out.push((c.id, e.clone()));
                }
            });
        }
        let gone: Vec<CueId> = self
            .cues
            .keys()
            .filter(|id| !seen.contains(id))
            .copied()
            .collect();
        for id in gone {
            self.update(id, None, media, rate);
        }
        self.out.clone_from(&out);
        out
    }

    /// Brings what is kept about `id` in line with `cue` (`None`: gone) and its file's probe.
    fn update(&mut self, id: CueId, cue: Option<&Cue>, media: &MediaCatalog, rate: NonZeroU32) {
        self.examined += 1;
        let Some((key, spec)) = cue.and_then(audio_spec) else {
            self.forget(id);
            return;
        };
        if self.cues.get(&id).is_none_or(|t| t.key != key) {
            self.forget(id);
            self.cues.insert(
                id,
                Tracked {
                    key: key.to_owned(),
                    entry: None,
                },
            );
            self.by_key.entry(key.to_owned()).or_default().insert(id);
        }
        let Some(tracked) = self.cues.get_mut(&id) else {
            return;
        };
        let Some(ready) = media.ready(key) else {
            // Not playable (yet): nothing to check, and the next probe is a new one.
            tracked.entry = None;
            self.errors.remove(&id);
            return;
        };
        let error = match &tracked.entry {
            Some(e) if e.matches(ready, &spec) => e.error.clone(),
            _ => {
                self.builds += 1;
                let error = match cue_slices(&spec, &ready.probed.info, rate, 0) {
                    Ok(_) => None,
                    Err(PlanFailure::Plan(e)) => Some(e.to_string()),
                };
                tracked.entry = Some(Entry {
                    probe: Arc::clone(ready),
                    start: spec.start_time.to_bits(),
                    end: spec.end_time.map(f64::to_bits),
                    marks: marks(spec.slices),
                    last: spec.last_slice_play_count,
                    error: error.clone(),
                });
                error
            }
        };
        match error {
            Some(e) => {
                self.errors.insert(id, e);
            }
            None => {
                self.errors.remove(&id);
            }
        }
    }

    /// Drops everything kept about `id`.
    fn forget(&mut self, id: CueId) {
        if let Some(old) = self.cues.remove(&id)
            && let Some(on) = self.by_key.get_mut(&old.key)
        {
            on.remove(&id);
            if on.is_empty() {
                self.by_key.remove(&old.key);
            }
        }
        self.errors.remove(&id);
    }

    /// Plans built so far (a cost measure for tests).
    pub fn builds(&self) -> u64 {
        self.builds
    }

    /// Cues looked at so far (a cost measure for tests: an edit looks at its own cues).
    pub fn examined(&self) -> u64 {
        self.examined
    }

    /// Walks of every audio cue so far ([`Self::check_all`]; a cost measure for tests).
    pub fn walks(&self) -> u64 {
        self.walks
    }
}

/// The file and plan inputs of an audio cue with a file.
fn audio_spec(c: &Cue) -> Option<(&str, PlanSpec<'_>)> {
    let CueKind::Audio {
        media: Some(m),
        start_time,
        end_time,
        slices,
        last_slice_play_count,
        ..
    } = &c.kind
    else {
        return None;
    };
    Some((
        m.as_str(),
        PlanSpec {
            start_time: *start_time,
            end_time: *end_time,
            slices,
            last_slice_play_count: *last_slice_play_count,
        },
    ))
}

/// Every cue in `cues`, depth first. Iterative, so nesting depth cannot overflow the stack.
fn each_cue<'a>(cues: &'a [Cue], f: &mut impl FnMut(&'a Cue)) {
    let mut stack: Vec<std::slice::Iter<'a, Cue>> = vec![cues.iter()];
    while let Some(level) = stack.last_mut() {
        match level.next() {
            Some(cue) => {
                f(cue);
                stack.push(cue.children().iter());
            }
            None => {
                stack.pop();
            }
        }
    }
}

fn marks(slices: &[SliceMarker]) -> Vec<(u64, u32)> {
    slices
        .iter()
        .map(|m| (m.time.to_bits(), m.play_count))
        .collect()
}
