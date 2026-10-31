// SPDX-License-Identifier: GPL-3.0-or-later

//! Which audio cues' slices cannot be played with their file (the cue engine's
//! `slices_unplayable` problem, `Input::SliceProblems`, #410), kept per
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
//! What changed is taken with [`PlanChecks::take_changes`]: the cues whose slices became
//! unplayable or playable again, which the cue engine is told (`Input::SliceProblems`); each
//! cue's nominal length, which it is told too (`Input::CueLengths`); and its media lengths,
//! which clients are told (`media_duration`, M5.4d). Only cues whose plan was rebuilt or
//! dropped are looked at for them, so a probe answer costs the cues on that file.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::num::NonZeroU32;
use std::sync::Arc;

use openlab_audio::total_frames;
use openlab_core::protocol::CueMediaDuration;
use openlab_core::{Cue, CueId, CueKind, SampleTime, SliceMarker, Workspace};

use super::media::{MediaCatalog, PlanChanges, Ready};
use super::plan::{PlanFailure, PlanSpec, cue_slices};

/// Where the checks find a file's probe: the doc's catalog, or [`NoProbes`] for a workspace
/// whose files are not probed yet (a doc being built off the control thread).
pub trait Probes {
    fn ready(&self, key: &str) -> Option<&Arc<Ready>>;
}

impl Probes for MediaCatalog {
    fn ready(&self, key: &str) -> Option<&Arc<Ready>> {
        MediaCatalog::ready(self, key)
    }
}

/// No file is probed yet.
pub struct NoProbes;

impl Probes for NoProbes {
    fn ready(&self, _: &str) -> Option<&Arc<Ready>> {
        None
    }
}

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
    /// One pass of the trimmed range (every slice once); `None` if there is no plan.
    trimmed: Option<u64>,
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
    /// The rate the checks were made at.
    rate: Option<NonZeroU32>,
    builds: u64,
    examined: u64,
    walks: u64,
    /// The lengths the cue engine was last told (`take_changes`).
    told: BTreeMap<CueId, SampleTime>,
    /// The media lengths clients were last told (`take_changes`), for the snapshot.
    sent: BTreeMap<CueId, CueMediaDuration>,
    /// The slice problems the cue engine was last told (`take_changes`).
    told_errors: BTreeMap<CueId, String>,
    /// Cues whose plan was rebuilt or dropped since then, so the change costs what it touched.
    stale: BTreeSet<CueId>,
}

impl PlanChecks {
    /// The cues whose slices cannot be played, with why: the cues the catalog noted since the
    /// last call are checked again, the others keep their answer.
    pub fn check(
        &mut self,
        ws: &Workspace,
        media: &mut MediaCatalog,
        rate: NonZeroU32,
    ) -> &BTreeMap<CueId, String> {
        let changes = media.take_plan_changes();
        if changes.all || self.rate != Some(rate) || media.index().has_duplicates() {
            return self.check_all(ws, media, rate);
        }
        if !changes.is_empty() && self.follow(ws, media, rate, changes).is_none() {
            // The index disagrees with the workspace: start over.
            return self.check_all(ws, media, rate);
        }
        &self.errors
    }

    /// Checks the cues `changes` names, and those on the files it names. `None` if the index
    /// cannot place a cue it has.
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
        Some(())
    }

    /// The reference: every audio cue walked in workspace order and checked (a cue whose file
    /// and trims are as last checked keeps its answer), the cues no longer there forgotten.
    pub fn check_all(
        &mut self,
        ws: &Workspace,
        media: &impl Probes,
        rate: NonZeroU32,
    ) -> &BTreeMap<CueId, String> {
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
        for list in &ws.cue_lists {
            each_cue(&list.cues, &mut |c| {
                if audio_spec(c).is_none() {
                    return;
                }
                seen.insert(c.id);
                self.update(c.id, Some(c), media, rate);
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
        &self.errors
    }

    /// Brings what is kept about `id` in line with `cue` (`None`: gone) and its file's probe.
    fn update(&mut self, id: CueId, cue: Option<&Cue>, media: &impl Probes, rate: NonZeroU32) {
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
                let (error, length, trimmed) = match cue_slices(&spec, &ready.probed.info, rate, 0)
                {
                    // From the top no slice is split, so the entries' frames add up to the
                    // trimmed range.
                    Ok(table) => (
                        None,
                        total_frames(&table),
                        Some(table.iter().fold(0u64, |a, s| a.saturating_add(s.frames))),
                    ),
                    Err(PlanFailure::Plan(e)) => (Some(e.to_string()), None, None),
                };
                tracked.entry = Some(Entry {
                    probe: Arc::clone(ready),
                    start: spec.start_time.to_bits(),
                    end: spec.end_time.map(f64::to_bits),
                    marks: marks(spec.slices),
                    last: spec.last_slice_play_count,
                    error: error.clone(),
                    length,
                    trimmed,
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

    /// What changed since the last call, as of the last `check`, in id order. Looks only at
    /// the cues whose plan was rebuilt or dropped since then.
    ///
    /// `slices`: the slice problems for the cue engine. A cue whose plan cannot be built gets
    /// why; one told that before whose plan builds now, or that has no plan any more (gone,
    /// another file, its file not probed or not playable), gets `None`.
    ///
    /// `lengths`: the nominal lengths for the cue engine. A cue with a finite plan now gets its
    /// length; one that had a length and has none now (gone, its file not probed or not
    /// playable, a plan that loops forever or cannot be built) gets `None`.
    ///
    /// `media`: the media lengths for clients. A cue whose file is probed and playable gets
    /// them; one that had them and has not now (gone, its file missing, not probed again yet,
    /// another file) gets a change with every field absent, so a client never keeps a stale
    /// length.
    pub fn take_changes(&mut self) -> CheckChanges {
        let mut out = CheckChanges::default();
        let rate = self.rate;
        for id in std::mem::take(&mut self.stale) {
            let entry = self.cues.get(&id).and_then(|t| t.entry.as_ref());
            let error = self.errors.get(&id);
            if self.told_errors.get(&id) != error {
                match error {
                    Some(e) => self.told_errors.insert(id, e.clone()),
                    None => self.told_errors.remove(&id),
                };
                out.slices.push((id, error.cloned()));
            }
            let now = entry.and_then(|e| e.length);
            if self.told.get(&id).copied() != now {
                match now {
                    Some(l) => self.told.insert(id, l),
                    None => self.told.remove(&id),
                };
                out.lengths.push((id, now));
            }
            let media = entry.map(|e| {
                let info = &e.probe.probed.info;
                CueMediaDuration {
                    cue: id,
                    file: seconds(info.frames, info.sample_rate),
                    trimmed: e.trimmed.zip(rate).and_then(|(f, r)| seconds(f, r.get())),
                    duration: e.length.zip(rate).and_then(|(f, r)| seconds(f, r.get())),
                }
            });
            if self.sent.get(&id) == media.as_ref() {
                continue;
            }
            match media {
                Some(m) => {
                    self.sent.insert(id, m.clone());
                    out.media.push(m);
                }
                None => {
                    self.sent.remove(&id);
                    out.media.push(CueMediaDuration {
                        cue: id,
                        file: None,
                        trimmed: None,
                        duration: None,
                    });
                }
            }
        }
        out
    }

    /// The media lengths clients were last told, by cue id (the snapshot's `media_durations`).
    pub fn media_durations(&self) -> Vec<CueMediaDuration> {
        self.sent.values().cloned().collect()
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

/// What [`PlanChecks::take_changes`] found.
#[derive(Debug, Default)]
pub struct CheckChanges {
    pub slices: Vec<(CueId, Option<String>)>,
    pub lengths: Vec<(CueId, Option<SampleTime>)>,
    pub media: Vec<CueMediaDuration>,
}

/// `frames` at `rate` in seconds, rounded down to the millisecond so a length is told again only
/// when it changes by one (a device rate change re-plans every cue); `None` at rate 0.
fn seconds(frames: u64, rate: u32) -> Option<f64> {
    if rate == 0 {
        return None;
    }
    let ms = u128::from(frames) * 1000 / u128::from(rate);
    // Below 2^53 ms (285 000 years) the conversion is exact.
    Some(u64::try_from(ms).unwrap_or(u64::MAX) as f64 / 1000.0)
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

/// Every cue in `cues`, depth first ([`openlab_core::descendants`]).
fn each_cue<'a>(cues: &'a [Cue], f: &mut impl FnMut(&'a Cue)) {
    openlab_core::descendants(cues).for_each(f);
}

fn marks(slices: &[SliceMarker]) -> Vec<(u64, u32)> {
    slices
        .iter()
        .map(|m| (m.time.to_bits(), m.play_count))
        .collect()
}
