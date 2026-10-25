// SPDX-License-Identifier: GPL-3.0-or-later

//! Static broken-cue detection (CUE-06, NFR-07): problems that keep a cue from running, found
//! before it is triggered. Core does no I/O, so media and device availability come from the
//! binary as inputs; target and reference problems come from the model. Rules:
//! `docs/cue-engine.md`, "Broken cues".

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::cue::{Cue, CueKind};
use crate::model::{CueId, NetworkPatchId, PatchId, Workspace};
use crate::settings::PatchResolution;
use crate::validate::can_target;

use super::run::Slot;
use super::types::{Action, CueProblem, MediaStatus};
use super::{Ctx, CueEngine, each_cue};

impl CueEngine {
    /// Records what the binary reported about a media file and recomputes problems.
    pub(super) fn media_status(&mut self, media: &str, status: MediaStatus, cx: &mut Ctx) {
        match status {
            MediaStatus::Unknown => self.media.remove(media),
            status => self.media.insert(media.to_owned(), status),
        };
        self.sync_problems(cx);
    }

    /// Records where a patch can play now and recomputes problems.
    pub(super) fn patch_status(
        &mut self,
        patch: PatchId,
        resolution: PatchResolution,
        cx: &mut Ctx,
    ) {
        self.patches.insert(patch, resolution);
        self.sync_problems(cx);
    }

    /// Recomputes every cue's problems from the model and the availability reported so far, and
    /// reports the cues whose problems changed. A loaded cue that became broken is unloaded (it
    /// cannot start). A cue with a runtime keeps it: its runtime state is reported until it ends.
    pub(super) fn sync_problems(&mut self, cx: &mut Ctx) {
        let fresh = problems_of(cx.ws, &self.media, &self.patches);
        let mut changed: Vec<CueId> = self
            .problems
            .iter()
            .filter(|(id, old)| fresh.get(id) != Some(old))
            .map(|(id, _)| *id)
            .collect();
        changed.extend(
            fresh
                .iter()
                .filter(|(id, new)| self.problems.get(id) != Some(new))
                .map(|(id, _)| *id),
        );
        changed.sort_unstable();
        changed.dedup();
        if changed.is_empty() {
            return;
        }
        let before: Vec<_> = changed.iter().map(|id| self.state(*id)).collect();
        self.problems = fresh;
        for (cue, before) in changed.into_iter().zip(before) {
            let problems = self.problems.get(&cue).cloned().unwrap_or_default();
            if cx.ws.find_cue(cue).is_some() {
                cx.out.push(Action::ProblemsChanged {
                    cue,
                    problems: problems.clone(),
                });
            }
            if !problems.is_empty()
                && matches!(self.slots.get(&cue), Some(Slot::Loaded(_)))
                && let Some(slot) = self.slots.remove(&cue)
            {
                super::release(cue, &slot, cx);
            }
            self.report(cue, before, cx.now, cx);
        }
    }

    /// Whether the cue has static problems (it reports `broken` while it has no runtime).
    pub(super) fn has_problems(&self, cue: CueId) -> bool {
        self.problems.contains_key(&cue)
    }
}

/// Every cue's problems (cues without any are left out). With duplicate ids the first cue with
/// the id counts, as for `Workspace::find_cue`.
fn problems_of(
    ws: &Workspace,
    media: &BTreeMap<String, MediaStatus>,
    patch_status: &BTreeMap<PatchId, PatchResolution>,
) -> BTreeMap<CueId, Vec<CueProblem>> {
    let mut by_id: HashMap<CueId, &Cue> = HashMap::new();
    for list in &ws.cue_lists {
        each_cue(&list.cues, |c| {
            by_id.entry(c.id).or_insert(c);
        });
    }
    let patches: HashSet<PatchId> = ws.patches.iter().map(|p| p.id).collect();
    let network: HashSet<NetworkPatchId> = ws.network_patches.iter().map(|p| p.id).collect();
    let mut out = BTreeMap::new();
    for (id, cue) in &by_id {
        let mut found = Vec::new();
        if cue.kind.has_target() {
            match cue.kind.target() {
                None => found.push(CueProblem::NoTarget),
                Some(target) if target == *id => found.push(CueProblem::TargetSelf),
                Some(target) => match by_id.get(&target) {
                    None => found.push(CueProblem::TargetMissing { target }),
                    Some(t) if !can_target(&cue.kind, &t.kind) => {
                        found.push(CueProblem::TargetWrongKind { target });
                    }
                    Some(_) => {}
                },
            }
        }
        match &cue.kind {
            CueKind::Audio {
                media: file, patch, ..
            } => {
                match file {
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
                if !patches.contains(patch) {
                    found.push(CueProblem::PatchMissing { patch: *patch });
                } else if let Some(resolution) = patch_status.get(patch)
                    && !matches!(resolution, PatchResolution::Mapped { .. })
                {
                    found.push(CueProblem::DeviceUnavailable {
                        patch: *patch,
                        resolution: resolution.clone(),
                    });
                }
            }
            CueKind::Network { destination, .. } if !network.contains(destination) => {
                found.push(CueProblem::NetworkPatchMissing {
                    patch: *destination,
                });
            }
            _ => {}
        }
        if !found.is_empty() {
            out.insert(*id, found);
        }
    }
    out
}
