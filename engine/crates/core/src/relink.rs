// SPDX-License-Identifier: GPL-3.0-or-later

//! Relinking missing media (WS-03): matching missing files to candidates found in a folder, and
//! the undoable command that applies a relink. The engine checks which files exist and scans
//! the folder; this module only compares names.

use std::collections::BTreeMap;

use crate::command::{Command, CueProperty};
use crate::cue::{CueKind, MediaRef};
use crate::media_path::file_name_key;
use crate::model::{CueId, Workspace};
use crate::validate::visit;

/// Every Audio and Video cue that has a file assigned, in cue-list order (a group before its
/// children).
pub fn media_of_cues(workspace: &Workspace) -> Vec<(CueId, MediaRef)> {
    let mut found = Vec::new();
    for list in &workspace.cue_lists {
        visit(&list.cues, &mut |cue| {
            if let Some(media) = cue.kind.media() {
                found.push((cue.id, media.clone()));
            }
        });
    }
    found
}

/// A cue whose file could not be found, and where it was supposed to be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingMedia {
    pub cue: CueId,
    pub media: MediaRef,
}

/// One proposed relink: `cue` should use `new`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relink {
    pub cue: CueId,
    pub new: MediaRef,
}

/// A missing file that matched more than one candidate. Nothing is guessed: the operator picks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AmbiguousRelink {
    pub cue: CueId,
    pub candidates: Vec<MediaRef>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RelinkPlan {
    pub matches: Vec<Relink>,
    pub ambiguous: Vec<AmbiguousRelink>,
    /// Cues for which the folder holds no file with that name.
    pub unmatched: Vec<CueId>,
}

/// Matches each missing file to the candidate with the same file name (NFC- and
/// case-insensitive). `candidates` are media paths already in show-file form (see
/// `portable_ref`). Candidates naming the same file (equal comparison keys) count once.
pub fn plan_relinks(missing: &[MissingMedia], candidates: &[MediaRef]) -> RelinkPlan {
    let mut by_name: BTreeMap<String, Vec<&MediaRef>> = BTreeMap::new();
    for candidate in candidates {
        let slot = by_name
            .entry(file_name_key(candidate.as_str()))
            .or_default();
        if !slot
            .iter()
            .any(|known| crate::media_path::paths_equal(known.as_str(), candidate.as_str()))
        {
            slot.push(candidate);
        }
    }
    let mut plan = RelinkPlan::default();
    for item in missing {
        let found = by_name
            .get(&file_name_key(item.media.as_str()))
            .map_or(&[][..], Vec::as_slice);
        match found {
            [] => plan.unmatched.push(item.cue),
            [only] => plan.matches.push(Relink {
                cue: item.cue,
                new: (*only).clone(),
            }),
            many => plan.ambiguous.push(AmbiguousRelink {
                cue: item.cue,
                candidates: many.iter().map(|m| (*m).clone()).collect(),
            }),
        }
    }
    plan
}

/// The command that points each listed Audio or Video cue at its new file, as one undo step
/// (command model). Cues that are gone or play no file are skipped; applying the command to the
/// workspace it was built from cannot be refused for that reason. An empty result is an empty
/// transaction, which `History` ignores.
pub fn relink_command(workspace: &Workspace, relinks: &[Relink]) -> Command {
    let commands = relinks
        .iter()
        .filter_map(|relink| {
            let cue = workspace.find_cue(relink.cue)?;
            let mut kind = cue.kind.clone();
            match &mut kind {
                CueKind::Audio { media, .. } | CueKind::Video { media, .. } => {
                    *media = Some(relink.new.clone());
                }
                _ => return None,
            }
            Some(Command::SetCueProperty {
                id: relink.cue,
                property: CueProperty::Kind { value: kind },
            })
        })
        .collect();
    Command::Transaction { commands }
}
