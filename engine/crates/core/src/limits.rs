// SPDX-License-Identifier: GPL-3.0-or-later

//! The workspace's volume limits (PB-11) applied to the levels it holds. A level set by any
//! command is put within the limits first (QLab 5 limits levels set by fades, scripts and OSC
//! as well as by the faders); changing a limit puts the levels already there within it, as the
//! commands [`Workspace::limit_clamps`] returns, so each is one undoable edit and each reaches
//! clients as an ordinary change of that cue or patch. A file's levels are put within them on
//! load (`validate.rs`), so the model always holds levels within the limits, except a Fade
//! re-aimed from a Group, whose values were changes in dB; the cue engine ends that within them.

use std::collections::HashSet;

use crate::command::{Command, CueProperty};
use crate::command_model::PatchProperty;
use crate::cue::{Cue, CueKind, LevelLimits};
use crate::model::{CueId, Workspace};
use crate::tree::{descendants, walk_mut};

/// Whether a Fade aimed at `target` holds levels; aimed at one of `groups` it holds changes in dB.
pub(crate) fn fade_holds_levels(target: Option<CueId>, groups: &HashSet<CueId>) -> bool {
    target.is_none_or(|t| !groups.contains(&t))
}

/// The ids of every Group among `cues`, at any depth.
pub(crate) fn group_ids<'a>(cues: impl IntoIterator<Item = &'a Cue>, out: &mut HashSet<CueId>) {
    out.extend(
        cues.into_iter()
            .filter(|c| matches!(c.kind, CueKind::Group { .. }))
            .map(|c| c.id),
    );
}

impl Workspace {
    /// Whether `kind` holds levels the limits apply to. A Fade aimed at a Group holds changes in
    /// dB, which the limits do not apply to (the cue engine limits what the group ends at).
    fn limits_apply_to_fades(&self, kind: &CueKind) -> bool {
        let CueKind::Fade { target, .. } = kind else {
            return true;
        };
        !target
            .and_then(|id| self.find_cue(id))
            .is_some_and(|t| matches!(t.kind, CueKind::Group { .. }))
    }

    /// Puts the levels of `kind`, a new value for a cue of this workspace, within its limits.
    pub(crate) fn clamp_kind(&self, kind: &mut CueKind) {
        let fades = self.limits_apply_to_fades(kind);
        kind.clamp_levels(self.settings.limits(), fades);
    }

    /// Puts the levels of cues about to be added within the limits. A Fade may be aimed at a Group
    /// of the workspace or among the new cues.
    pub(crate) fn clamp_new_cues(&self, cues: &mut [Cue]) {
        let mut groups = HashSet::new();
        group_ids(self.cues(), &mut groups);
        group_ids(descendants(cues), &mut groups);
        let limits = self.settings.limits();
        walk_mut(cues, &mut |cue| {
            let fades = match &cue.kind {
                CueKind::Fade { target, .. } => fade_holds_levels(*target, &groups),
                _ => true,
            };
            cue.kind.clamp_levels(limits, fades);
        });
    }

    /// The commands that put every level of the workspace within `limits`: Audio cues and Fade
    /// cues aimed at something other than a Group, and the patches' masters and outputs.
    pub fn limit_clamps(&self, limits: LevelLimits) -> Vec<Command> {
        let mut out = Vec::new();
        for patch in &self.patches {
            let id = patch.id;
            if limits.limit(patch.master_db) != patch.master_db {
                out.push(Command::SetPatchProperty {
                    id,
                    property: PatchProperty::MasterDb {
                        value: limits.limit(patch.master_db),
                    },
                });
            }
            for (output, o) in patch.outputs.iter().enumerate() {
                if limits.limit(o.level_db) != o.level_db {
                    out.push(Command::SetPatchProperty {
                        id,
                        property: PatchProperty::OutputLevel {
                            output,
                            value: limits.limit(o.level_db),
                        },
                    });
                }
            }
        }
        for cue in self.cues() {
            // Not a clone of every group (and its subtree).
            if !matches!(cue.kind, CueKind::Audio { .. } | CueKind::Fade { .. }) {
                continue;
            }
            let mut kind = cue.kind.clone();
            if kind.clamp_levels(limits, self.limits_apply_to_fades(&kind)) {
                out.push(Command::SetCueProperty {
                    id: cue.id,
                    property: CueProperty::Kind { value: kind },
                });
            }
        }
        out
    }
}
