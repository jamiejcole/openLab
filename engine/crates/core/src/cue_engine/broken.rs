// SPDX-License-Identifier: GPL-3.0-or-later

//! Static broken-cue detection (CUE-06, NFR-07): problems that keep a cue from running, found
//! before it is triggered. Core does no I/O, so media and device availability come from the
//! binary as inputs; target and reference problems come from the model. Rules:
//! `docs/cue-engine.md`, "Broken cues".
//!
//! Incremental, so a show of thousands of cues costs nothing per tick and an edit costs what the
//! edit touched: the model's part (targets, patches, media references) is brought up to date
//! only when the binary signals a model change. [`CueEngine::model_changed_cues`] names the
//! cues the edit touched (its [`Footprint`]) and only those, plus the cues whose problems
//! depend on them, are recomputed; [`CueEngine::model_changed`] rebuilds everything (a newly
//! opened workspace, a change with no footprint) and is the reference the incremental path must
//! equal. A status input recomputes only the cues on that media file or patch (indexes below).
//!
//! Dependencies between cues (docs/cue-engine.md, "Broken cues"): a cue's problems come from its
//! own content, from the existence and kind of its target (a fade, stop, devamp, ... names
//! another cue; `can_target` reads the target's kind and a group's mode), and from the patch
//! and the network patches (id, host and port: cue facts depend on them). So an edit refreshes, besides the cues it touched and the cues it
//! added (with a group's descendants), every cue that targets a touched, added or removed cue
//! (`by_target`); an edit that changes the patches or network patches (any difference) rebuilds everything. Group warnings
//! (`below`) are recounted for the groups around every cue that was added, removed, moved or
//! changed whether it is shown with problems.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::changes::Footprint;
use crate::cue::{Cue, CueKind, MediaRef};
use crate::cue_index::{CueContainer, CueIndex, IndexChange, IndexDelta};
use crate::model::{CueId, NetworkPatch, NetworkPatchId, PatchId, Workspace};
use crate::network::{HostKind, check_endpoint, check_message};
use crate::settings::PatchResolution;
use crate::validate::can_target;

use super::run::Slot;
use super::types::{Action, BrokenReason, CueProblem, MediaStatus, NetworkReachability};
use super::{Ctx, CueEngine};

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
    /// Network cues: what is wrong with the destination and the message, and whether the
    /// destination's host name has to resolve.
    network: Option<NetworkFacts>,
    /// Fade cues: the fade sets no level, so there is nothing to fade (QLab F4).
    nothing_to_fade: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct NetworkFacts {
    destination: NetworkPatchId,
    /// Problems the model alone shows: a missing patch, an address that cannot be used, a
    /// malformed message.
    fixed: Vec<CueProblem>,
    /// The destination is a host name: the binary's lookup (`NetworkAvailability`) decides.
    lookup: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct AudioFacts {
    media: Option<MediaRef>,
    patch: PatchId,
    patch_exists: bool,
}

/// The model's part of every cue's problems, with the indexes a status input needs.
#[derive(Clone, Debug, Default, PartialEq)]
struct ModelFacts {
    /// Every cue's place: its group (for the groups' warnings), its rank (the pre-show list's
    /// order) and whether it is in the workspace.
    tree: CueIndex,
    facts: BTreeMap<CueId, Facts>,
    by_media: HashMap<String, BTreeSet<CueId>>,
    by_patch: HashMap<PatchId, BTreeSet<CueId>>,
    /// Network cues by the network patch they send to, for the ones whose host is looked up.
    by_network: HashMap<NetworkPatchId, BTreeSet<CueId>>,
    /// The cues that name a cue as their target, by that cue; and the reverse. Empty sets are
    /// removed, so a rebuilt model equals an incrementally kept one.
    by_target: HashMap<CueId, BTreeSet<CueId>>,
    target_of: HashMap<CueId, CueId>,
    /// The patch ids `facts` were computed against (a change rebuilds everything).
    patch_ids: Vec<PatchId>,
    /// The network patches `facts` were computed against (any change rebuilds everything).
    network_patches: Vec<NetworkPatch>,
}

/// What the model changed since the problems were last brought up to date.
#[derive(Clone, Debug, Default)]
enum Pending {
    /// Nothing.
    Clean,
    /// Everything (the engine is new, or a change had no footprint).
    #[default]
    All,
    /// These cues; `structural` is the part of `touched` that was added, removed or moved.
    Cues {
        touched: BTreeSet<CueId>,
        structural: BTreeSet<CueId>,
        /// The part of `structural` that was added (the top of each added subtree).
        added: BTreeSet<CueId>,
    },
}

/// Static problems of the workspace's cues and what they are computed from.
#[derive(Clone, Debug)]
pub(super) struct Problems {
    /// What changed in the model since the problems were last brought up to date.
    pending: Pending,
    model: ModelFacts,
    media: BTreeMap<String, MediaStatus>,
    patches: BTreeMap<PatchId, PatchResolution>,
    /// Failed host lookups; a network patch that is not here is taken to be reachable.
    network: BTreeMap<NetworkPatchId, String>,
    /// Every cue that has problems, with them.
    current: BTreeMap<CueId, Vec<CueProblem>>,
    /// Cues shown with [`CueProblem::StartedBroken`].
    started_broken: BTreeSet<CueId>,
    /// Groups with a descendant shown with problems, and how many (QLab 5: a broken child puts
    /// a warning on its group). Kept up to date cue by cue; recounted when the model is rebuilt.
    below: BTreeMap<CueId, usize>,
    /// `below` is being recounted: changes cue by cue are not applied meanwhile, but the cues
    /// that started or stopped being shown are noted in `flips`.
    recounting: bool,
    flips: Vec<CueId>,
    /// Cues whose problems were computed, since the engine was made (a cost measure).
    checks: u64,
    /// Calls that found the model changed without `model_changed` (every build; see
    /// `unsignalled_model_changes`).
    unsignalled: u64,
}

impl Default for Problems {
    fn default() -> Self {
        Self {
            pending: Pending::All,
            model: ModelFacts::default(),
            media: BTreeMap::new(),
            patches: BTreeMap::new(),
            network: BTreeMap::new(),
            current: BTreeMap::new(),
            started_broken: BTreeSet::new(),
            below: BTreeMap::new(),
            recounting: false,
            flips: Vec::new(),
            checks: 0,
            unsignalled: 0,
        }
    }
}

impl Problems {
    /// Where every cue is, as of the last time the problems were brought up to date: in step
    /// with the workspace after the start of a call that follows edits.
    pub(super) fn tree(&self) -> &CueIndex {
        &self.model.tree
    }
}

impl CueEngine {
    /// How many times a cue's problems have been computed since the engine was made. A status
    /// input costs one per cue on that media file or patch; a call with nothing new costs none.
    pub fn problem_checks(&self) -> u64 {
        self.problems.checks
    }

    /// Times a model change was found that `model_changed` did not signal, i.e. a binary (or
    /// test) that broke the contract. Every build checks the workspace's outline on every call
    /// (`shape.rs`: then the change is followed) and a cue about to be refused for its problems
    /// (then its problems are rebuilt); with the edit cross-check on (debug builds,
    /// [`Self::set_edit_cross_check`]) any other change is counted too, and left as it is. The
    /// binary should log any increase.
    pub fn unsignalled_model_changes(&self) -> u64 {
        self.problems.unsignalled
    }

    /// The model's part of the problems is out of date: rebuild it on the next call.
    pub(super) fn mark_problems_dirty(&mut self) {
        self.problems.pending = Pending::All;
    }

    /// The model changed in the cues of `footprint` only: bring the problems up to date for
    /// those (and what depends on them) on the next call. Added to what is already pending; a
    /// pending full rebuild stays one.
    pub(super) fn mark_problems_changed(&mut self, footprint: &Footprint) {
        let pending = &mut self.problems.pending;
        let (touched, structural, added) = match pending {
            Pending::All => return,
            Pending::Clean => {
                *pending = Pending::Cues {
                    touched: BTreeSet::new(),
                    structural: BTreeSet::new(),
                    added: BTreeSet::new(),
                };
                let Pending::Cues {
                    touched,
                    structural,
                    added,
                } = pending
                else {
                    return;
                };
                (touched, structural, added)
            }
            Pending::Cues {
                touched,
                structural,
                added,
            } => (touched, structural, added),
        };
        added.extend(footprint.added.iter().copied());
        let moved = footprint
            .added
            .iter()
            .chain(&footprint.removed)
            .chain(&footprint.moved);
        structural.extend(moved.copied());
        touched.extend(structural.iter().copied());
        touched.extend(footprint.edited.iter().copied());
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
        let rank = self.problems.model.tree.ranks();
        out.sort_by_key(|(id, _)| (rank.get(id).copied().unwrap_or(usize::MAX), *id));
        out
    }

    /// Whether a cue inside the group, at any depth, is shown with static problems (CUE-06). As
    /// in QLab 5 the group then shows as broken too (its state is `broken` while it has no
    /// runtime), but it has no problem of its own: it still starts, and its broken children are
    /// skipped as usual. `Action::GroupWarningChanged` reports each change. False for a cue that
    /// is not a group.
    pub fn has_broken_descendant(&self, cue: CueId) -> bool {
        self.problems.below.contains_key(&cue)
    }

    /// Every group with a descendant shown with problems.
    pub(super) fn problems_below(&self) -> Vec<CueId> {
        self.problems.below.keys().copied().collect()
    }

    /// Whether the patch of an audio cue exists and was last reported `Mapped`: a `device_lost`
    /// break clears then (docs/cue-engine.md, Broken cues). An unreported patch does not count,
    /// so a break the binary reports without any patch status stays until a reset.
    pub(super) fn device_back(&self, cue: CueId) -> bool {
        let p = &self.problems;
        p.model
            .facts
            .get(&cue)
            .and_then(|f| f.audio.as_ref())
            .is_some_and(|a| {
                a.patch_exists
                    && matches!(
                        p.patches.get(&a.patch),
                        Some(PatchResolution::Mapped { .. })
                    )
            })
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
                    |n| ws.network_patches.iter().find(|x| x.id == n),
                )
            })
            .map(|f| {
                combine(
                    &f,
                    &self.problems.media,
                    &self.problems.patches,
                    &self.problems.network,
                )
            })
            .unwrap_or_default();
        if self.problems.current.get(&cue) != Some(&fresh) {
            self.problems.unsignalled = self.problems.unsignalled.saturating_add(1);
            self.problems.pending = Pending::All;
            self.sync_problems(cx);
        }
        self.has_problems(cue)
    }

    /// Start of a call: bring the model's part up to date if the model changed.
    pub(super) fn sync_problems(&mut self, cx: &mut Ctx) {
        match std::mem::replace(&mut self.problems.pending, Pending::Clean) {
            Pending::Clean => {}
            Pending::All => self.rebuild_problems(cx),
            Pending::Cues {
                touched,
                structural,
                added,
            } => {
                if !structural.is_empty() {
                    self.playheads_before_edit(cx.ws, &structural);
                }
                if !self.update_problems(&touched, &structural, &added, cx) {
                    self.rebuild_problems(cx);
                }
            }
        }
    }

    /// The reference: the model's part and every cue's problems from scratch, then the groups'
    /// warnings recounted.
    fn rebuild_problems(&mut self, cx: &mut Ctx) {
        self.problems.pending = Pending::Clean;
        // The playheads that leave their positions to the old index keep them first.
        self.keep_playhead_orders();
        self.problems.model = model_facts(cx.ws);
        self.problems.recounting = true;
        let cues: BTreeSet<CueId> = self
            .problems
            .current
            .keys()
            .chain(self.problems.model.facts.keys())
            .chain(&self.problems.started_broken)
            .copied()
            .collect();
        self.refresh(cues, cx);
        self.recount_below(cx);
    }

    /// Brings the model's part and the problems up to date for the edit that touched `touched`
    /// (`structural`: added, removed or moved), recomputing only those cues, the cues that
    /// target them and the groups around them. Returns false if the model cannot be followed
    /// from the footprint (the cue lists or patches changed, or the footprint did not name a
    /// cue that changed): the caller then rebuilds everything.
    fn update_problems(
        &mut self,
        touched: &BTreeSet<CueId>,
        structural: &BTreeSet<CueId>,
        added: &BTreeSet<CueId>,
        cx: &mut Ctx,
    ) -> bool {
        let ws = cx.ws;
        let model = &mut self.problems.model;
        let same_patches = ws
            .patches
            .iter()
            .map(|p| p.id)
            .eq(model.patch_ids.iter().copied())
            && ws.network_patches == model.network_patches;
        if !same_patches {
            return false;
        }
        let delta = match model.tree.reconcile(ws, touched, structural, added) {
            IndexChange::Delta(delta) => delta,
            IndexChange::Rebuilt => return false,
        };
        // Cues whose own facts are recomputed: those touched or new, then the cues that target
        // any of them or a cue that is gone.
        let mut recompute: BTreeSet<CueId> = touched.iter().chain(&delta.fresh).copied().collect();
        for id in touched.iter().chain(&delta.fresh).chain(&delta.gone) {
            if let Some(dependents) = model.by_target.get(id) {
                recompute.extend(dependents.iter().copied());
            }
        }
        // What is gone has no facts to compute.
        recompute.retain(|id| model.tree.contains(*id));
        for id in &delta.gone {
            model.forget(*id);
        }
        let mut computed = Vec::with_capacity(recompute.len());
        for id in &recompute {
            let Some(cue) = model.tree.locate(ws, *id) else {
                return false;
            };
            let facts = facts_of(
                cue,
                |t| model.tree.locate(ws, t).map(|t| &t.kind),
                |p| ws.patches.iter().any(|x| x.id == p),
                |n| ws.network_patches.iter().find(|x| x.id == n),
            );
            computed.push((*id, facts, own_target(cue)));
        }
        for (id, facts, target) in computed {
            model.set(id, facts, target);
        }

        // What a cue shows also depends on its runtime (a break by a lost device clears once
        // its patch maps; a run started broken shows `started_broken` once its problems are
        // fixed), but each of those changes only on an event that already refreshes the cue: a
        // patch or media status, or an edit that touches the cue or what it targets. A removed
        // cue is refreshed once more so it stops being shown.
        let mut refresh = recompute;
        refresh.extend(delta.gone.iter().copied());
        self.problems.recounting = true;
        self.problems.flips.clear();
        self.refresh(refresh, cx);
        self.recount_changed(&delta, cx);
        true
    }

    /// Recounts the groups whose descendants changed: those around a cue that was added,
    /// removed, moved or started or stopped being shown with problems (`flips`), by what each of
    /// their cues holds, deepest first. Reports the groups whose warning changed.
    fn recount_changed(&mut self, delta: &IndexDelta, cx: &mut Ctx) {
        let p = &mut self.problems;
        p.recounting = false;
        let flips = std::mem::take(&mut p.flips);
        let tree = &p.model.tree;
        let mut groups: BTreeSet<CueId> = BTreeSet::new();
        for cue in flips.iter().chain(&delta.fresh).chain(&delta.moved) {
            groups.extend(tree.ancestors(*cue));
        }
        // A new group has no count yet.
        groups.extend(
            delta
                .fresh
                .iter()
                .copied()
                .filter(|id| !tree.kids(CueContainer::Group(*id)).is_empty()),
        );
        for former in &delta.former_parents {
            groups.insert(*former);
            groups.extend(tree.ancestors(*former));
        }
        let mut was: BTreeMap<CueId, bool> = groups
            .iter()
            .map(|g| (*g, p.below.contains_key(g)))
            .collect();
        for gone in &delta.gone {
            if p.below.remove(gone).is_some() {
                was.insert(*gone, true);
            }
        }
        let mut by_depth: Vec<(usize, CueId)> = groups
            .into_iter()
            .map(|g| (tree.ancestors(g).count(), g))
            .collect();
        by_depth.sort_by(|a, b| b.cmp(a));
        for (_, group) in by_depth {
            let held: usize = tree
                .kids(CueContainer::Group(group))
                .iter()
                .map(|k| {
                    usize::from(p.current.contains_key(k) || p.started_broken.contains(k))
                        .saturating_add(p.below.get(k).copied().unwrap_or(0))
                })
                .fold(0, usize::saturating_add);
            if held == 0 {
                p.below.remove(&group);
            } else {
                p.below.insert(group, held);
            }
        }
        let changed: Vec<(CueId, bool)> = was
            .into_iter()
            .map(|(group, before)| (group, before, p.below.contains_key(&group)))
            .filter(|(_, before, warning)| warning != before)
            .map(|(group, _, warning)| (group, warning))
            .collect();
        for (group, warning) in changed {
            self.report_group_warning(group, warning, cx);
        }
    }

    /// Recounts every group's broken descendants from the rebuilt model and reports the groups
    /// whose warning changed (a deleted group that had one reports it gone, once).
    fn recount_below(&mut self, cx: &mut Ctx) {
        let p = &mut self.problems;
        p.recounting = false;
        p.flips.clear();
        let mut below: BTreeMap<CueId, usize> = BTreeMap::new();
        for cue in p.current.keys().chain(&p.started_broken) {
            for group in p.model.tree.ancestors(*cue) {
                let n = below.entry(group).or_default();
                *n = n.saturating_add(1);
            }
        }
        let old = std::mem::replace(&mut p.below, below);
        let flipped: BTreeSet<CueId> = old
            .keys()
            .filter(|g| !p.below.contains_key(g))
            .chain(p.below.keys().filter(|g| !old.contains_key(g)))
            .copied()
            .collect();
        let changed: Vec<(CueId, bool)> = flipped
            .into_iter()
            .map(|group| (group, p.below.contains_key(&group)))
            .collect();
        for (group, warning) in changed {
            self.report_group_warning(group, warning, cx);
        }
    }

    /// The cue started (`shown`) or stopped being shown with problems: updates the warning of
    /// every group around it, reporting those that changed.
    fn note_shown(&mut self, cue: CueId, shown: bool, cx: &mut Ctx) {
        let p = &mut self.problems;
        if p.recounting {
            p.flips.push(cue);
            return;
        }
        let mut changed = Vec::new();
        for group in p.model.tree.ancestors(cue) {
            let warning = if shown {
                let n = p.below.entry(group).or_default();
                *n = n.saturating_add(1);
                *n == 1
            } else {
                match p.below.get_mut(&group) {
                    Some(n) if *n > 1 => {
                        *n -= 1;
                        false
                    }
                    Some(_) => {
                        p.below.remove(&group);
                        true
                    }
                    None => false,
                }
            };
            if warning {
                changed.push(group);
            }
        }
        for group in changed {
            self.report_group_warning(group, shown, cx);
        }
    }

    /// A group's warning changed: reports it and, for a group with no runtime, the state it now
    /// shows (QLab 5: a group shows as broken while a descendant does; G6).
    fn report_group_warning(&mut self, group: CueId, warning: bool, cx: &mut Ctx) {
        cx.out.push(Action::GroupWarningChanged { group, warning });
        if !self.slots.contains_key(&group) && !self.has_problems(group) {
            let state = if warning {
                super::CueState::Broken
            } else {
                super::CueState::Idle
            };
            cx.out.push(Action::StateChanged {
                cue: group,
                state,
                at: cx.now,
            });
        }
    }

    /// Checks the problem bookkeeping against a from-scratch rebuild for `ws`: the model's part
    /// (places, facts, the indexes), every cue's problems and the groups' warning counts. Call
    /// it with nothing pending (after a `handle`); `Err` says what differs. It rebuilds the
    /// whole model, so it is for tests and the edit cross-check (debug builds), where it is how a
    /// missed `model_changed` is counted and how the incremental path is held to the full rebuild.
    pub fn verify_problem_cache(&self, ws: &Workspace) -> Result<(), String> {
        let p = &self.problems;
        let fresh = model_facts(ws);
        let m = &p.model;
        let differs = [
            ("tree", fresh.tree == m.tree),
            ("facts", fresh.facts == m.facts),
            ("by_media", fresh.by_media == m.by_media),
            ("by_patch", fresh.by_patch == m.by_patch),
            ("by_network", fresh.by_network == m.by_network),
            ("by_target", fresh.by_target == m.by_target),
            ("target_of", fresh.target_of == m.target_of),
            ("patch_ids", fresh.patch_ids == m.patch_ids),
            (
                "network_patches",
                fresh.network_patches == m.network_patches,
            ),
        ]
        .into_iter()
        .find(|(_, same)| !same);
        if let Some((what, _)) = differs {
            return Err(format!("the model's {what} differ from a rebuild"));
        }
        let current: BTreeMap<CueId, Vec<CueProblem>> = fresh
            .facts
            .iter()
            .map(|(id, f)| (*id, combine(f, &p.media, &p.patches, &p.network)))
            .filter(|(_, v)| !v.is_empty())
            .collect();
        if current != p.current {
            return Err("the cues' problems differ from a rebuild".to_owned());
        }
        let mut below: BTreeMap<CueId, usize> = BTreeMap::new();
        for cue in p.current.keys().chain(&p.started_broken) {
            for group in fresh.tree.ancestors(*cue) {
                let n = below.entry(group).or_default();
                *n = n.saturating_add(1);
            }
        }
        if below != p.below {
            return Err("the groups' warning counts differ from a recount".to_owned());
        }
        Ok(())
    }

    /// Records what the binary reported about media files and patches, and recomputes the
    /// problems of the cues on them only.
    pub(super) fn availability(
        &mut self,
        media: Vec<(MediaRef, MediaStatus)>,
        patches: Vec<(PatchId, PatchResolution)>,
        network: Vec<(NetworkPatchId, NetworkReachability)>,
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
        for (patch, reachability) in network {
            let changed = match reachability {
                NetworkReachability::Unresolved { reason } => {
                    p.network.insert(patch, reason.clone()) != Some(reason)
                }
                NetworkReachability::Resolved | NetworkReachability::Unknown => {
                    p.network.remove(&patch).is_some()
                }
            };
            if changed && let Some(on) = p.model.by_network.get(&patch) {
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
            // A cue broken by a lost device can play again once its patch is mapped again.
            if matches!(
                self.slots.get(&cue),
                Some(Slot::Broken(BrokenReason::DeviceLost))
            ) && self.device_back(cue)
            {
                let before = self.state(cue);
                self.slots.remove(&cue);
                self.report(cue, before, cx.now, cx);
            }
            let p = &mut self.problems;
            p.checks = p.checks.saturating_add(1);
            let new = p
                .model
                .facts
                .get(&cue)
                .map(|f| combine(f, &p.media, &p.patches, &p.network))
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
            let was_shown = !self.problems(cue).is_empty();
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
            if self.problems.model.tree.contains(cue) {
                cx.out.push(Action::ProblemsChanged {
                    cue,
                    problems: shown.clone(),
                });
            }
            if was_shown != !shown.is_empty() {
                self.note_shown(cue, !shown.is_empty(), cx);
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
            self.note_shown(cue, false, cx);
        }
    }
}

/// A cue's problems in their order: target, nothing to fade, media, patch or device, network
/// destination.
fn combine(
    f: &Facts,
    media: &BTreeMap<String, MediaStatus>,
    patches: &BTreeMap<PatchId, PatchResolution>,
    network: &BTreeMap<NetworkPatchId, String>,
) -> Vec<CueProblem> {
    let mut found: Vec<CueProblem> = f.target.iter().cloned().collect();
    if f.nothing_to_fade {
        found.push(CueProblem::NothingToFade);
    }
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
    if let Some(n) = &f.network {
        found.extend(n.fixed.iter().cloned());
        if n.lookup
            && let Some(reason) = network.get(&n.destination)
        {
            found.push(CueProblem::NetworkHostUnresolved {
                patch: n.destination,
                reason: reason.clone(),
            });
        }
    }
    found
}

/// The model's part of every cue's problems, from scratch (the reference the incremental
/// updates must equal). With duplicate ids the first cue with the id counts (`CueIndex`).
fn model_facts(ws: &Workspace) -> ModelFacts {
    let mut order: Vec<&Cue> = Vec::new();
    let tree = CueIndex::build_visiting(ws, |c| order.push(c));
    let by_id: HashMap<CueId, &Cue> = order.iter().map(|c| (c.id, *c)).collect();
    let patches: HashSet<PatchId> = ws.patches.iter().map(|p| p.id).collect();
    let network: HashMap<NetworkPatchId, &NetworkPatch> =
        ws.network_patches.iter().map(|p| (p.id, p)).collect();
    let mut out = ModelFacts {
        tree,
        patch_ids: ws.patches.iter().map(|p| p.id).collect(),
        network_patches: ws.network_patches.clone(),
        ..ModelFacts::default()
    };
    for cue in order {
        let facts = facts_of(
            cue,
            |t| by_id.get(&t).map(|c| &c.kind),
            |p| patches.contains(&p),
            |n| network.get(&n).copied(),
        );
        out.set(cue.id, facts, own_target(cue));
    }
    out
}

impl ModelFacts {
    /// Replaces what the model says about `id`, keeping the indexes in step.
    fn set(&mut self, id: CueId, facts: Option<Facts>, target: Option<CueId>) {
        self.forget(id);
        if let Some(facts) = &facts
            && let Some(audio) = &facts.audio
        {
            if let Some(file) = &audio.media {
                self.by_media
                    .entry(file.as_str().to_owned())
                    .or_default()
                    .insert(id);
            }
            self.by_patch.entry(audio.patch).or_default().insert(id);
        }
        if let Some(facts) = &facts
            && let Some(network) = &facts.network
            && network.lookup
        {
            self.by_network
                .entry(network.destination)
                .or_default()
                .insert(id);
        }
        if let Some(facts) = facts {
            self.facts.insert(id, facts);
        }
        if let Some(target) = target {
            self.by_target.entry(target).or_default().insert(id);
            self.target_of.insert(id, target);
        }
    }

    /// Drops everything the model says about `id`: its facts and its entries in the indexes.
    /// What other cues say about `id` as their target stays (they are recomputed when it
    /// matters).
    fn forget(&mut self, id: CueId) {
        let removed = self.facts.remove(&id);
        if let Some(network) = removed.as_ref().and_then(|f| f.network.as_ref())
            && let Some(on) = self.by_network.get_mut(&network.destination)
        {
            on.remove(&id);
            if on.is_empty() {
                self.by_network.remove(&network.destination);
            }
        }
        if let Some(audio) = removed.and_then(|f| f.audio) {
            if let Some(file) = &audio.media
                && let Some(on) = self.by_media.get_mut(file.as_str())
            {
                on.remove(&id);
                if on.is_empty() {
                    self.by_media.remove(file.as_str());
                }
            }
            if let Some(on) = self.by_patch.get_mut(&audio.patch) {
                on.remove(&id);
                if on.is_empty() {
                    self.by_patch.remove(&audio.patch);
                }
            }
        }
        if let Some(target) = self.target_of.remove(&id)
            && let Some(on) = self.by_target.get_mut(&target)
        {
            on.remove(&id);
            if on.is_empty() {
                self.by_target.remove(&target);
            }
        }
    }
}

/// The cue `cue` names as its target, if it is a targeting kind with one set.
fn own_target(cue: &Cue) -> Option<CueId> {
    if cue.kind.has_target() {
        cue.kind.target()
    } else {
        None
    }
}

/// What the model says about `cue`, or `None` for a kind that cannot have problems. `kind_of`
/// gives a target's kind (`None`: not in the workspace).
fn facts_of<'w>(
    cue: &Cue,
    kind_of: impl Fn(CueId) -> Option<&'w CueKind>,
    patch_exists: impl Fn(PatchId) -> bool,
    network_patch: impl Fn(NetworkPatchId) -> Option<&'w NetworkPatch>,
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
        CueKind::Network {
            destination,
            address,
            args,
        } => {
            let patch = *destination;
            let mut fixed = Vec::new();
            let mut lookup = false;
            match network_patch(patch) {
                None => fixed.push(CueProblem::NetworkPatchMissing { patch }),
                Some(p) => match check_endpoint(&p.host, p.port) {
                    Ok(HostKind::Name) => lookup = true,
                    Ok(HostKind::Ip) => {}
                    Err(issue) => fixed.push(CueProblem::NetworkHostInvalid {
                        patch,
                        reason: issue.to_string(),
                    }),
                },
            }
            if let Err(issue) = check_message(address, args) {
                fixed.push(CueProblem::NetworkMessageInvalid {
                    reason: issue.to_string(),
                });
            }
            (!fixed.is_empty() || lookup).then_some(NetworkFacts {
                destination: patch,
                fixed,
                lookup,
            })
        }
        _ => None,
    };
    let nothing_to_fade = matches!(&cue.kind, CueKind::Fade { levels, .. }
        if levels.master_db.is_none()
            && levels.output_db.iter().all(Option::is_none)
            && levels.crosspoints.is_empty());
    (target.is_some() || audio.is_some() || network.is_some() || nothing_to_fade).then_some(Facts {
        target,
        audio,
        network,
        nothing_to_fade,
    })
}
