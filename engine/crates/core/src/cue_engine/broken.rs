// SPDX-License-Identifier: GPL-3.0-or-later

//! Static broken-cue detection (CUE-06, NFR-07): problems that keep a cue from running, found
//! before it is triggered. Core does no I/O, so media and device availability come from the
//! binary as inputs; target and reference problems come from the model. Rules:
//! `docs/cue-engine.md`, "Broken cues".
//!
//! Incremental, so a show of thousands of cues costs nothing per tick: the model's part (targets,
//! patches, media references) is rebuilt only when the binary signals a model change
//! ([`CueEngine::model_changed`]); a status input recomputes only the cues on that media file or
//! patch (indexes below).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::cue::{Cue, CueKind, MediaRef};
use crate::model::{CueId, NetworkPatchId, PatchId, Workspace};
use crate::settings::PatchResolution;
use crate::validate::can_target;

use super::run::Slot;
use super::types::{Action, CueProblem, MediaStatus};
use super::{Ctx, CueEngine, each_cue};

/// Shown for a cue whose problems were fixed while the run it was started broken with is still
/// pending: it reports `broken` until that run ends, and the UI needs a reason.
const STARTED_BROKEN: &[CueProblem] = &[CueProblem::StartedBroken];

/// What the model says about one cue that can have problems.
#[derive(Clone, Debug, PartialEq)]
struct Facts {
    /// Target problems (at most one).
    target: Option<CueProblem>,
    /// Audio cues: the media reference and patch, and whether the patch exists.
    audio: Option<AudioFacts>,
    /// Network cues: a missing destination.
    network: Option<NetworkPatchId>,
}

#[derive(Clone, Debug, PartialEq)]
struct AudioFacts {
    media: Option<MediaRef>,
    patch: PatchId,
    patch_exists: bool,
}

/// The model's part of every cue's problems, with the indexes a status input needs.
#[derive(Clone, Debug, Default)]
struct ModelFacts {
    facts: BTreeMap<CueId, Facts>,
    by_media: HashMap<String, Vec<CueId>>,
    by_patch: HashMap<PatchId, Vec<CueId>>,
    /// Every cue's place in the workspace (lists in order, cues depth-first): the pre-show
    /// list's order.
    rank: HashMap<CueId, usize>,
}

/// Static problems of the workspace's cues and what they are computed from.
#[derive(Clone, Debug)]
pub(super) struct Problems {
    /// The model changed (or the engine is new): rebuild the model's part on the next call.
    dirty: bool,
    model: ModelFacts,
    media: BTreeMap<String, MediaStatus>,
    patches: BTreeMap<PatchId, PatchResolution>,
    /// Every cue that has problems, with them.
    current: BTreeMap<CueId, Vec<CueProblem>>,
    /// Cues shown with [`CueProblem::StartedBroken`].
    started_broken: BTreeSet<CueId>,
    /// Cues whose problems were computed, since the engine was made (a cost measure).
    checks: u64,
    /// Debug builds: calls that found the model changed without `model_changed`.
    unsignalled: u64,
}

impl Default for Problems {
    fn default() -> Self {
        Self {
            dirty: true,
            model: ModelFacts::default(),
            media: BTreeMap::new(),
            patches: BTreeMap::new(),
            current: BTreeMap::new(),
            started_broken: BTreeSet::new(),
            checks: 0,
            unsignalled: 0,
        }
    }
}

impl CueEngine {
    /// How many times a cue's problems have been computed since the engine was made. A status
    /// input costs one per cue on that media file or patch; a call with nothing new costs none.
    pub fn problem_checks(&self) -> u64 {
        self.problems.checks
    }

    /// Times the model's problems were found out of date although `model_changed` was not
    /// called, i.e. a binary (or test) that broke the contract; each was corrected. Release
    /// builds check only a cue about to be refused for its problems; debug builds check every
    /// call. The binary should log any increase.
    pub fn unsignalled_model_changes(&self) -> u64 {
        self.problems.unsignalled
    }

    /// The model's part of the problems is out of date: rebuild it on the next call.
    pub(super) fn mark_problems_dirty(&mut self) {
        self.problems.dirty = true;
    }

    /// Counts a model change that was not signalled (see `unsignalled_model_changes`).
    pub(super) fn note_unsignalled(&mut self) {
        self.problems.unsignalled = self.problems.unsignalled.saturating_add(1);
    }

    /// The cue's static problems (CUE-06); empty when it has none.
    pub fn problems(&self, cue: CueId) -> &[CueProblem] {
        match self.problems.current.get(&cue) {
            Some(p) => p,
            None if self.problems.started_broken.contains(&cue) => STARTED_BROKEN,
            None => &[],
        }
    }

    /// Every cue shown with static problems and its problems, in workspace order (lists in
    /// order, cues depth-first): what the pre-show check lists (NFR-07). Runtime breaks are in
    /// `broken_reason`.
    pub fn broken_cues(&self) -> Vec<(CueId, &[CueProblem])> {
        let mut out: Vec<(CueId, &[CueProblem])> = self
            .problems
            .current
            .keys()
            .chain(&self.problems.started_broken)
            .map(|id| (*id, self.problems(*id)))
            .collect();
        let rank = &self.problems.model.rank;
        out.sort_by_key(|(id, _)| (rank.get(id).copied().unwrap_or(usize::MAX), *id));
        out
    }

    /// Whether the cue has static problems now (it reports `broken` while it has no runtime, and
    /// does not start).
    pub(super) fn has_problems(&self, cue: CueId) -> bool {
        self.problems.current.contains_key(&cue)
    }

    /// Whether `cue` is to be refused for static problems. Checks first that its cached
    /// problems still hold for the current workspace (a cue refused because of a model change
    /// the binary did not signal must not stay refused): if they do not, the problems are rebuilt
    /// (counted in `unsignalled_model_changes`). Only a cue about to be refused pays for this.
    pub(super) fn refuses(&mut self, cue: CueId, cx: &mut Ctx) -> bool {
        if !self.has_problems(cue) {
            return false;
        }
        let ws = cx.ws;
        let fresh = ws
            .find_cue(cue)
            .and_then(|c| {
                facts_of(
                    c,
                    |t| ws.find_cue(t).map(|t| &t.kind),
                    |p| ws.patches.iter().any(|x| x.id == p),
                    |n| ws.network_patches.iter().any(|x| x.id == n),
                )
            })
            .map(|f| combine(&f, &self.problems.media, &self.problems.patches))
            .unwrap_or_default();
        if self.problems.current.get(&cue) != Some(&fresh) {
            self.problems.unsignalled = self.problems.unsignalled.saturating_add(1);
            self.problems.dirty = true;
            self.sync_problems(cx);
        }
        self.has_problems(cue)
    }

    /// Start of a call: rebuild the model's part if the model changed.
    pub(super) fn sync_problems(&mut self, cx: &mut Ctx) {
        if !self.problems.dirty {
            #[cfg(debug_assertions)]
            self.check_signalled(cx);
            return;
        }
        self.problems.dirty = false;
        self.problems.model = model_facts(cx.ws);
        let cues: BTreeSet<CueId> = self
            .problems
            .current
            .keys()
            .chain(self.problems.model.facts.keys())
            .chain(&self.problems.started_broken)
            .copied()
            .collect();
        self.refresh(cues, cx);
    }

    /// Debug builds: a model change the binary did not signal is caught (and counted) here.
    #[cfg(debug_assertions)]
    fn check_signalled(&mut self, cx: &mut Ctx) {
        let model = model_facts(cx.ws);
        let p = &self.problems;
        let fresh: BTreeMap<CueId, Vec<CueProblem>> = model
            .facts
            .iter()
            .map(|(id, f)| (*id, combine(f, &p.media, &p.patches)))
            .filter(|(_, v)| !v.is_empty())
            .collect();
        if model.facts != p.model.facts || model.rank != p.model.rank || fresh != p.current {
            self.problems.unsignalled = self.problems.unsignalled.saturating_add(1);
            self.problems.dirty = true;
            self.sync_problems(cx);
        }
    }

    /// Records what the binary reported about media files and patches, and recomputes the
    /// problems of the cues on them only.
    pub(super) fn availability(
        &mut self,
        media: Vec<(MediaRef, MediaStatus)>,
        patches: Vec<(PatchId, PatchResolution)>,
        cx: &mut Ctx,
    ) {
        let mut cues = BTreeSet::new();
        let p = &mut self.problems;
        for (file, status) in media {
            let key = file.as_str();
            let changed = match status {
                MediaStatus::Unknown => p.media.remove(key).is_some(),
                status => p.media.insert(key.to_owned(), status.clone()) != Some(status),
            };
            if changed && let Some(on) = p.model.by_media.get(key) {
                cues.extend(on.iter().copied());
            }
        }
        for (patch, resolution) in patches {
            if p.patches.insert(patch, resolution.clone()) != Some(resolution)
                && let Some(on) = p.model.by_patch.get(&patch)
            {
                cues.extend(on.iter().copied());
            }
        }
        self.refresh(cues, cx);
    }

    /// Recomputes the problems of `cues` and reports those that changed. A loaded cue that
    /// became broken is unloaded (it cannot start). A cue with a runtime keeps it: its runtime
    /// state is reported until it ends.
    fn refresh(&mut self, cues: BTreeSet<CueId>, cx: &mut Ctx) {
        for cue in cues {
            let p = &mut self.problems;
            p.checks = p.checks.saturating_add(1);
            let new = p
                .model
                .facts
                .get(&cue)
                .map(|f| combine(f, &p.media, &p.patches))
                .unwrap_or_default();
            let started_broken = new.is_empty()
                && matches!(self.slots.get(&cue), Some(Slot::Active(r)) if r.static_broken);
            let shown_now: &[CueProblem] = match () {
                _ if !new.is_empty() => &new,
                _ if started_broken => STARTED_BROKEN,
                _ => &[],
            };
            // What is shown follows from the stored problems, so nothing else changes either.
            if self.problems(cue) == shown_now {
                continue;
            }
            let before = self.state(cue);
            let p = &mut self.problems;
            if new.is_empty() {
                p.current.remove(&cue);
            } else {
                p.current.insert(cue, new);
            }
            if started_broken {
                p.started_broken.insert(cue);
            } else {
                p.started_broken.remove(&cue);
            }
            let shown = self.problems(cue).to_vec();
            if self.problems.model.rank.contains_key(&cue) {
                cx.out.push(Action::ProblemsChanged {
                    cue,
                    problems: shown.clone(),
                });
            }
            if !shown.is_empty()
                && matches!(self.slots.get(&cue), Some(Slot::Loaded(_)))
                && let Some(slot) = self.slots.remove(&cue)
            {
                super::release(cue, &slot, cx);
            }
            self.report(cue, before, cx.now, cx);
        }
    }

    /// A state change was reported: a cue shown as started broken whose run has ended has no
    /// problem left to show.
    pub(super) fn settle_problems(&mut self, cue: CueId, state: super::CueState, cx: &mut Ctx) {
        if state != super::CueState::Broken && self.problems.started_broken.remove(&cue) {
            cx.out.push(Action::ProblemsChanged {
                cue,
                problems: Vec::new(),
            });
        }
    }
}

/// A cue's problems in their order: target, media, patch or device, network destination.
fn combine(
    f: &Facts,
    media: &BTreeMap<String, MediaStatus>,
    patches: &BTreeMap<PatchId, PatchResolution>,
) -> Vec<CueProblem> {
    let mut found: Vec<CueProblem> = f.target.iter().cloned().collect();
    if let Some(a) = &f.audio {
        match &a.media {
            None => found.push(CueProblem::NoMedia),
            Some(file) => match media.get(file.as_str()) {
                Some(MediaStatus::Missing) => {
                    found.push(CueProblem::MediaMissing {
                        media: file.clone(),
                    });
                }
                Some(MediaStatus::Unsupported { reason }) => {
                    found.push(CueProblem::MediaUnsupported {
                        media: file.clone(),
                        reason: reason.clone(),
                    });
                }
                Some(MediaStatus::Available | MediaStatus::Unknown) | None => {}
            },
        }
        if !a.patch_exists {
            found.push(CueProblem::PatchMissing { patch: a.patch });
        } else if let Some(resolution) = patches.get(&a.patch)
            && !matches!(resolution, PatchResolution::Mapped { .. })
        {
            found.push(CueProblem::DeviceUnavailable {
                patch: a.patch,
                resolution: resolution.clone(),
            });
        }
    }
    if let Some(patch) = f.network {
        found.push(CueProblem::NetworkPatchMissing { patch });
    }
    found
}

/// The model's part of every cue's problems. With duplicate ids the first cue with the id
/// counts, as for `Workspace::find_cue`.
fn model_facts(ws: &Workspace) -> ModelFacts {
    let mut by_id: HashMap<CueId, &Cue> = HashMap::new();
    let mut order: Vec<&Cue> = Vec::new();
    for list in &ws.cue_lists {
        each_cue(&list.cues, |c| {
            if let std::collections::hash_map::Entry::Vacant(e) = by_id.entry(c.id) {
                e.insert(c);
                order.push(c);
            }
        });
    }
    let patches: HashSet<PatchId> = ws.patches.iter().map(|p| p.id).collect();
    let network: HashSet<NetworkPatchId> = ws.network_patches.iter().map(|p| p.id).collect();
    let mut out = ModelFacts::default();
    for (i, cue) in order.iter().enumerate() {
        out.rank.insert(cue.id, i);
        if let CueKind::Audio { media, patch, .. } = &cue.kind {
            if let Some(file) = media {
                out.by_media
                    .entry(file.as_str().to_owned())
                    .or_default()
                    .push(cue.id);
            }
            out.by_patch.entry(*patch).or_default().push(cue.id);
        }
        let facts = facts_of(
            cue,
            |t| by_id.get(&t).map(|c| &c.kind),
            |p| patches.contains(&p),
            |n| network.contains(&n),
        );
        if let Some(facts) = facts {
            out.facts.insert(cue.id, facts);
        }
    }
    out
}

/// What the model says about `cue`, or `None` for a kind that cannot have problems. `kind_of`
/// gives a target's kind (`None`: not in the workspace).
fn facts_of<'w>(
    cue: &Cue,
    kind_of: impl Fn(CueId) -> Option<&'w CueKind>,
    patch_exists: impl Fn(PatchId) -> bool,
    network_exists: impl Fn(NetworkPatchId) -> bool,
) -> Option<Facts> {
    let target = if cue.kind.has_target() {
        match cue.kind.target() {
            None => Some(CueProblem::NoTarget),
            Some(target) if target == cue.id => Some(CueProblem::TargetSelf),
            Some(target) => match kind_of(target) {
                None => Some(CueProblem::TargetMissing { target }),
                Some(kind) if !can_target(&cue.kind, kind) => {
                    Some(CueProblem::TargetWrongKind { target })
                }
                Some(_) => None,
            },
        }
    } else {
        None
    };
    let audio = match &cue.kind {
        CueKind::Audio { media, patch, .. } => Some(AudioFacts {
            media: media.clone(),
            patch: *patch,
            patch_exists: patch_exists(*patch),
        }),
        _ => None,
    };
    let network = match &cue.kind {
        CueKind::Network { destination, .. } if !network_exists(*destination) => Some(*destination),
        _ => None,
    };
    (target.is_some() || audio.is_some() || network.is_some()).then_some(Facts {
        target,
        audio,
        network,
    })
}
