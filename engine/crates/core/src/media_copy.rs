// SPDX-License-Identifier: GPL-3.0-or-later

//! Copying a show's media into its show folder (WS-04; QLab 5 Workspace Settings, General,
//! File Management, and its on-demand command): which cues' files to copy, into which folder,
//! the names copies take, and the undoable retarget. The engine does the file work; this module
//! only decides from the model.

use crate::command::{Command, CueProperty};
use crate::cue::{Cue, CueKind, MediaRef};
use crate::model::{CueId, Workspace};
use crate::relink::media_of_cues;

/// The show folder's folder for Audio cues' copies.
pub const AUDIO_FOLDER: &str = "audio";
/// The show folder's folder for Video cues' copies.
pub const VIDEO_FOLDER: &str = "video";

/// Most numbered names tried for one copy (`kick 2.wav` up to `kick 1000.wav`).
pub const MAX_COPY_NUMBER: u32 = 1000;

/// Why files are being copied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyMode {
    /// The operator set the cues' files with the workspace setting on: every file is copied,
    /// one already inside the show folder as a numbered second copy (QLab measured, row C-R5).
    Added,
    /// The on-demand command: files already inside the show folder are left where they are.
    OnDemand,
}

/// One cue's file to copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyItem {
    pub cue: CueId,
    /// The reference as the cue holds it when the copy is planned.
    pub media: MediaRef,
    /// [`AUDIO_FOLDER`] or [`VIDEO_FOLDER`].
    pub folder: &'static str,
    /// The reference names a file inside the show folder already ([`in_show_folder`]).
    pub in_folder: bool,
}

/// Whether a stored reference names a file inside the show folder: relative, with no `.` or
/// `..` component (the stored form, docs/file-format.md section 4.3, makes every file literally
/// under the folder relative).
pub fn in_show_folder(media: &MediaRef) -> bool {
    let text = media.as_str();
    !text.is_empty()
        && !crate::media_path::is_absolute(text)
        && text
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..")
}

/// The files to copy, in cue-list order. `OnDemand`: every Audio and Video cue with a file
/// (`cues` is ignored); `Added`: the cues listed that have one.
pub fn copy_items(workspace: &Workspace, mode: CopyMode, cues: &[CueId]) -> Vec<CopyItem> {
    let wanted = |cue: CueId| mode == CopyMode::OnDemand || cues.contains(&cue);
    media_of_cues(workspace)
        .into_iter()
        .filter(|(cue, _)| wanted(*cue))
        .filter_map(|(cue, media)| {
            let folder = match workspace.find_cue(cue)?.kind {
                CueKind::Video { .. } => VIDEO_FOLDER,
                _ => AUDIO_FOLDER,
            };
            let in_folder = in_show_folder(&media);
            Some(CopyItem {
                cue,
                media,
                folder,
                in_folder,
            })
        })
        .collect()
}

/// Whether `name` can be a copy's file name inside its folder: one component, not empty, not
/// `.` or `..`, no separator (either platform's) and no NUL. A source's own last component
/// always is, but a copy must never land outside its folder whatever the reference holds.
pub fn copy_name_ok(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0'])
}

/// The `n`th name for a copy of `name` (`n` from 2): the number goes before the extension, after
/// a space, as Finder numbers copies (`kick.wav`, 2 -> `kick 2.wav`; `notes`, 3 -> `notes 3`).
/// A name that is only an extension (`.wav`) takes the number at its end.
pub fn numbered_name(name: &str, n: u32) -> String {
    match name.rfind('.') {
        Some(dot) if dot > 0 => format!("{} {n}{}", &name[..dot], &name[dot..]),
        _ => format!("{name} {n}"),
    }
}

/// The cues whose file `command` sets to a file (a new cue with one, a replaced file, a kind
/// with one), in the order the command names them: what copying on add copies. Pastes are not
/// among them: a copied cue keeps the file its original has.
pub fn media_set_by(command: &Command) -> Vec<CueId> {
    let mut found = Vec::new();
    collect_media_set(command, &mut found);
    found
}

fn collect_media_set(command: &Command, found: &mut Vec<CueId>) {
    let mut push = |id: CueId| {
        if !found.contains(&id) {
            found.push(id);
        }
    };
    match command {
        Command::AddCue { cue, .. } => {
            for c in crate::tree::descendants(std::slice::from_ref::<Cue>(cue)) {
                if c.kind.media().is_some() {
                    push(c.id);
                }
            }
        }
        Command::SetCueProperty { id, property } if property_sets_media(property) => push(*id),
        Command::BatchSet { ids, property } if property_sets_media(property) => {
            ids.iter().copied().for_each(push);
        }
        Command::Transaction { commands } => {
            for c in commands {
                collect_media_set(c, found);
            }
        }
        _ => {}
    }
}

fn property_sets_media(property: &CueProperty) -> bool {
    match property {
        CueProperty::Media { value } => value.is_some(),
        CueProperty::Kind { value } => value.media().is_some(),
        _ => false,
    }
}

/// The command that points each listed cue at its copy, as one undo step: only the file
/// changes. Cues that are gone or play no file are skipped. An empty result is an empty
/// transaction, which `History` ignores.
pub fn retarget_command(workspace: &Workspace, retargets: &[(CueId, MediaRef)]) -> Command {
    // One walk of the show, not one per cue: a copy may retarget thousands.
    let file_cues: std::collections::HashSet<CueId> = workspace
        .cues()
        .filter(|c| matches!(c.kind, CueKind::Audio { .. } | CueKind::Video { .. }))
        .map(|c| c.id)
        .collect();
    let commands = retargets
        .iter()
        .filter(|(cue, _)| file_cues.contains(cue))
        .map(|(cue, media)| Command::SetCueProperty {
            id: *cue,
            property: CueProperty::Media {
                value: Some(media.clone()),
            },
        })
        .collect();
    Command::Transaction { commands }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: &str) -> MediaRef {
        MediaRef::new(s)
    }

    #[test]
    fn only_a_plain_relative_reference_is_inside_the_show_folder() {
        assert!(in_show_folder(&r("audio/kick.wav")));
        assert!(in_show_folder(&r("kick.wav")));
        assert!(!in_show_folder(&r("../Audio/kick.wav")));
        assert!(!in_show_folder(&r("audio/../../kick.wav")));
        assert!(!in_show_folder(&r("./kick.wav")));
        assert!(!in_show_folder(&r("/Lib/kick.wav")));
        assert!(!in_show_folder(&r("C:/Lib/kick.wav")));
        assert!(!in_show_folder(&r("//srv/share/kick.wav")));
        assert!(!in_show_folder(&r("")));
        assert!(!in_show_folder(&r("audio//kick.wav")));
    }

    #[test]
    fn copies_are_numbered_before_the_extension() {
        assert_eq!(numbered_name("kick.wav", 2), "kick 2.wav");
        assert_eq!(numbered_name("kick.final.wav", 3), "kick.final 3.wav");
        assert_eq!(numbered_name("notes", 2), "notes 2");
        assert_eq!(numbered_name(".wav", 2), ".wav 2");
        assert_eq!(numbered_name("track1.aif", 2), "track1 2.aif");
    }

    #[test]
    fn a_copy_name_is_one_plain_component() {
        assert!(copy_name_ok("kick.wav"));
        assert!(copy_name_ok("kick 2.wav"));
        for bad in ["", ".", "..", "a/b.wav", r"a\b.wav", "a\0b"] {
            assert!(!copy_name_ok(bad), "{bad:?}");
        }
    }
}
