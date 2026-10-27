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
};

use super::Control;

/// The notice code the warnings feed gives a broken cue (one per cue, with the cue id).
pub(super) const CUE_BROKEN: &str = "cue_broken";

impl Control {
    /// The report, from the current state. Brings the warnings up to date first so the two
    /// agree.
    pub(super) fn preshow_report(&mut self) -> PreshowReport {
        if self.warnings_dirty {
            self.warnings_dirty = false;
            self.refresh_warnings();
        }
        let ws = &self.ws;
        let places = places(ws);
        let mut broken_cues = Vec::new();
        let mut media: BTreeMap<String, (PreshowMediaStatus, Option<String>, Vec<CueId>)> =
            BTreeMap::new();
        let mut missing_patches: BTreeSet<PatchId> = BTreeSet::new();
        for (cue, problems) in self.engine.broken_cues() {
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
        for patch in &ws.patches {
            let resolution = self.patches.resolution(patch.id);
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
            patches.push(PreshowPatch {
                patch: patch.id,
                name: Some(patch.name.clone()),
                device,
                mapped: !matches!(resolution, None | Some(PatchResolution::Unmapped)),
                resolution: resolution.map(WireResolution::from),
                cues: uses.patch.get(&patch.id).cloned().unwrap_or_default(),
            });
        }
        // A deleted patch some cue still names, which is only in the cues' problems.
        for patch in missing_patches {
            if ws.patches.iter().any(|p| p.id == patch) {
                continue;
            }
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

        let mut system: Vec<WireWarning> = self
            .warnings
            .iter()
            .filter(|w| !matches!(w, WireWarning::Notice { code, .. } if code == CUE_BROKEN))
            .cloned()
            .collect();
        system.sort_by(|a, b| notice_key(a).cmp(&notice_key(b)));

        let unchecked = network
            .iter()
            .filter(|t| t.status == PreshowTargetStatus::NotChecked)
            .count() as u64;
        let network_problems = network.len() as u64 - unchecked;
        let mut summary = PreshowSummary {
            broken_cues: broken_cues.len() as u64,
            media: media.len() as u64,
            patches: patches.len() as u64,
            network: network_problems,
            network_unchecked: unchecked,
            system: system.len() as u64,
            total: 0,
        };
        summary.total = summary.broken_cues
            + summary.media
            + summary.patches
            + summary.network
            + summary.system;
        PreshowReport {
            ok: summary.total == 0,
            summary,
            broken_cues,
            media,
            patches,
            network,
            system,
        }
    }
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
        super::visit(&list.cues, &mut |c| {
            out.insert(c.id, (list.id, c.number.as_deref(), c.name.as_str()));
        });
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
        for list in &ws.cue_lists {
            super::visit(&list.cues, &mut |c| match &c.kind {
                CueKind::Audio { patch, .. } => uses.patch.entry(*patch).or_default().push(c.id),
                CueKind::Network { destination, .. } => {
                    uses.network.entry(*destination).or_default().push(c.id);
                }
                _ => {}
            });
        }
        uses
    }
}
