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
//!
//! The same plans give each cue's nominal length, which the cue engine is told when it changes
//! (`Input::CueLengths`, [`PlanChecks::length_changes`]); only cues whose plan was rebuilt or
//! dropped are looked at for it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::num::NonZeroU32;
use std::sync::Arc;

use openlab_audio::total_frames;
use openlab_core::{Cue, CueId, CueKind, SampleTime, SliceMarker, Workspace};

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
    /// The action's length from the top; `None` if it loops forever or has no plan.
    length: Option<u64>,
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
    /// The lengths the cue engine was last told (`length_changes`).
    told: BTreeMap<CueId, SampleTime>,
    /// Cues whose plan was rebuilt or dropped since then, so the change costs what it touched.
    stale: BTreeSet<CueId>,
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
            self.stale.extend(self.cues.keys().copied());
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
            if tracked.entry.take().is_some() {
                self.stale.insert(id);
            }
            self.errors.remove(&id);
            return;
        };
        let error = match &tracked.entry {
            Some(e) if e.matches(ready, &spec) => e.error.clone(),
            _ => {
                self.builds += 1;
                self.stale.insert(id);
                let (error, length) = match cue_slices(&spec, &ready.probed.info, rate, 0) {
                    Ok(table) => (None, total_frames(&table)),
                    Err(PlanFailure::Plan(e)) => (Some(e.to_string()), None),
                };
                tracked.entry = Some(Entry {
                    probe: Arc::clone(ready),
                    start: spec.start_time.to_bits(),
                    end: spec.end_time.map(f64::to_bits),
                    marks: marks(spec.slices),
                    last: spec.last_slice_play_count,
                    error: error.clone(),
                    length,
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
        if let Some(old) = self.cues.remove(&id) {
            self.stale.insert(id);
            if let Some(on) = self.by_key.get_mut(&old.key) {
                on.remove(&id);
                if on.is_empty() {
                    self.by_key.remove(&old.key);
                }
            }
        }
        self.errors.remove(&id);
    }

    /// The nominal lengths that changed since the last call, as of the last `check`: a cue with
    /// a finite plan now gets its length, one that had a length and has none now (gone, its file
    /// not probed or not playable, a plan that loops forever or cannot be built) gets `None`.
    /// In id order. Looks only at the cues whose plan was rebuilt or dropped since the last call.
    pub fn length_changes(&mut self) -> Vec<(CueId, Option<SampleTime>)> {
        let mut out = Vec::new();
        for id in std::mem::take(&mut self.stale) {
            let now = self
                .cues
                .get(&id)
                .and_then(|t| t.entry.as_ref())
                .and_then(|e| e.length);
            if self.told.get(&id).copied() == now {
                continue;
            }
            match now {
                Some(l) => self.told.insert(id, l),
                None => self.told.remove(&id),
            };
            out.push((id, now));
        }
        out
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
