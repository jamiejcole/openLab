// SPDX-License-Identifier: GPL-3.0-or-later

//! The pre-show check (SM-06, NFR-07): one report of what would keep the show from running,
//! built from state the control thread already holds (the cue engine's problems, the patch
//! resolutions, the workspace warnings). It does no I/O: media status is the latest probe plus
//! the media watch (docs/control.md, Pre-show check).

use std::collections::{BTreeMap, BTreeSet};

use openlab_core::protocol::{
    PreshowCue, PreshowMedia, PreshowMediaStatus, PreshowPatch, PreshowReport, PreshowSummary,
    PreshowTarget, PreshowTargetStatus, WireProblem, WireResolution, WireWarning,
};
use openlab_core::{
    CueId, CueKind, CueListId, CueProblem, NetworkPatchId, PatchId, PatchResolution, Workspace,
    descendants,
};

use super::DocCx;

/// The system warning that is information, not a problem: the file is streamed instead of
/// preloaded and plays fine. Listed in `system`, not counted, never makes the check fail.
const INFO_CODE: &str = "preload_over_budget";

impl DocCx<'_> {
    /// The report, from the current state. Brings the warnings up to date first so the two
    /// agree.
    pub(super) fn preshow_report(&mut self) -> PreshowReport {
        if self.doc.warnings_dirty {
            self.doc.warnings_dirty = false;
            self.refresh_warnings();
        }
        let ws = self.doc.model.ws();
        let places = places(ws);
        let mut broken_cues = Vec::new();
        let mut media: BTreeMap<String, (PreshowMediaStatus, Option<String>, Vec<CueId>)> =
            BTreeMap::new();
        let mut missing_patches: BTreeSet<PatchId> = BTreeSet::new();
        for (cue, problems) in self.doc.model.engine().broken_cues() {
            let Some(&(list, number, name)) = places.get(&cue) else {
                continue;
            };
            for problem in problems {
                match problem {
                    CueProblem::MediaMissing { media: m } => {
                        media
                            .entry(m.as_str().to_owned())
                            .or_insert((PreshowMediaStatus::Missing, None, Vec::new()))
                            .2
                            .push(cue);
                    }
                    CueProblem::MediaUnsupported { media: m, reason } => {
                        media
                            .entry(m.as_str().to_owned())
                            .or_insert((
                                PreshowMediaStatus::Unsupported,
                                Some(reason.clone()),
                                Vec::new(),
                            ))
                            .2
                            .push(cue);
                    }
                    CueProblem::PatchMissing { patch } => {
                        missing_patches.insert(*patch);
                    }
                    _ => {}
                }
            }
            broken_cues.push(PreshowCue {
                cue,
                list,
                number: number.map(str::to_owned),
                name: name.to_owned(),
                problems: problems.iter().map(WireProblem::from).collect(),
            });
        }
        let media: Vec<PreshowMedia> = media
            .into_iter()
            .map(|(path, (status, reason, cues))| PreshowMedia {
                path,
                status,
                reason,
                cues,
            })
            .collect();

        let uses = Uses::of(ws);
        let mut patches = Vec::new();
        let mut used_patches = 0u64;
        for patch in &ws.patches {
            let resolution = self.eng.patches.resolution(self.doc.id, patch.id);
            if matches!(resolution, Some(PatchResolution::Mapped { .. })) {
                continue;
            }
            let device = match resolution {
                Some(
                    PatchResolution::DeviceMissing { device }
                    | PatchResolution::ChannelsMissing { device, .. },
                ) => Some(device.clone()),
                _ => None,
            };
            let cues = uses.patch.get(&patch.id).cloned().unwrap_or_default();
            // A spare patch nothing plays on is listed, not a problem for this show.
            used_patches += u64::from(!cues.is_empty());
            patches.push(PreshowPatch {
                patch: patch.id,
                name: Some(patch.name.clone()),
                device,
                mapped: !matches!(resolution, None | Some(PatchResolution::Unmapped)),
                resolution: resolution.map(WireResolution::from),
                cues,
            });
        }
        // A deleted patch some cue still names, which is only in the cues' problems.
        for patch in missing_patches {
            if ws.patches.iter().any(|p| p.id == patch) {
                continue;
            }
            used_patches += 1;
            patches.push(PreshowPatch {
                patch,
                name: None,
                device: None,
                mapped: false,
                resolution: None,
                cues: uses.patch.get(&patch).cloned().unwrap_or_default(),
            });
        }

        let mut network = Vec::new();
        for target in &ws.network_patches {
            if let Some(cues) = uses.network.get(&target.id) {
                network.push(PreshowTarget {
                    patch: target.id,
                    name: Some(target.name.clone()),
                    host: Some(target.host.clone()),
                    port: Some(target.port),
                    protocol: Some(target.protocol),
                    status: PreshowTargetStatus::NotChecked,
                    cues: cues.clone(),
                });
            }
        }
        for (id, cues) in &uses.network {
            if ws.network_patches.iter().all(|t| t.id != *id) {
                network.push(PreshowTarget {
                    patch: *id,
                    name: None,
                    host: None,
                    port: None,
                    protocol: None,
                    status: PreshowTargetStatus::Missing,
                    cues: cues.clone(),
                });
            }
        }

        let mut system: Vec<WireWarning> = self.doc.warnings.to_vec();
        system.sort_by(|a, b| notice_key(a).cmp(&notice_key(b)));

        let unchecked = network
            .iter()
            .filter(|t| t.status == PreshowTargetStatus::NotChecked)
            .count() as u64;
        let network_problems = network.len() as u64 - unchecked;
        let system_problems = system.iter().filter(|w| !is_info(w)).count() as u64;
        let summary = PreshowSummary {
            broken_cues: broken_cues.len() as u64,
            media: media.len() as u64,
            patches: used_patches,
            network: network_problems,
            network_unchecked: unchecked,
            system: system_problems,
            // Media, patches and destinations are causes of broken cues, already counted there.
            total: broken_cues.len() as u64 + system_problems,
        };
        // The causes are checked too, in case one is ever listed without a broken cue.
        let ok = summary.total == 0
            && summary.media == 0
            && summary.patches == 0
            && summary.network == 0;
        PreshowReport {
            ok,
            summary,
            broken_cues,
            media,
            patches,
            network,
            system,
        }
    }
}

fn is_info(w: &WireWarning) -> bool {
    matches!(w, WireWarning::Notice { code, .. } if code == INFO_CODE)
}

/// Sort key of a system warning: its code, its cue, then its text.
fn notice_key(w: &WireWarning) -> (&str, Option<CueId>, &str) {
    match w {
        WireWarning::Notice { code, message, cue } => (code, *cue, message),
        _ => ("", None, ""),
    }
}

type Place<'a> = (CueListId, Option<&'a str>, &'a str);

/// Every cue's list, number and display name.
fn places(ws: &Workspace) -> BTreeMap<CueId, Place<'_>> {
    let mut out = BTreeMap::new();
    for list in &ws.cue_lists {
        for c in descendants(&list.cues) {
            out.insert(c.id, (list.id, c.number.as_deref(), c.name.as_str()));
        }
    }
    out
}

/// The cues that use each patch and network destination, in workspace order.
struct Uses {
    patch: BTreeMap<PatchId, Vec<CueId>>,
    network: BTreeMap<NetworkPatchId, Vec<CueId>>,
}

impl Uses {
    fn of(ws: &Workspace) -> Self {
        let mut uses = Self {
            patch: BTreeMap::new(),
            network: BTreeMap::new(),
        };
        for c in ws.cues() {
            match &c.kind {
                CueKind::Audio { patch, .. } => uses.patch.entry(*patch).or_default().push(c.id),
                CueKind::Network { destination, .. } => {
                    uses.network.entry(*destination).or_default().push(c.id);
                }
                _ => {}
            }
        }
        uses
    }
}
