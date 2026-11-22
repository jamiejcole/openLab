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

/// The cues whose file `command` sets to a file other than the one the cue has in `workspace`
/// (the workspace the command is about to be applied to): a new cue with a file, a replaced file,
/// a kind with another file. In the order the command names them: what copying on add copies.
/// A command that rewrites a whole kind for another reason (a level limit clamping cues, a
/// pasted property) keeps each cue's file and copies nothing. Pastes are not among them: a copied
/// cue keeps the file its original has.
pub fn media_set_by(workspace: &Workspace, command: &Command) -> Vec<CueId> {
    let mut set = MediaSet {
        workspace,
        current: None,
        found: Vec::new(),
        seen: std::collections::HashSet::new(),
    };
    set.collect(command);
    set.found
}

struct MediaSet<'a> {
    workspace: &'a Workspace,
    /// Each cue's file now, read once and only for a command that names a file.
    current: Option<std::collections::HashMap<CueId, MediaRef>>,
    found: Vec<CueId>,
    seen: std::collections::HashSet<CueId>,
}

impl MediaSet<'_> {
    fn push(&mut self, id: CueId) {
        if self.seen.insert(id) {
            self.found.push(id);
        }
    }

    /// Whether setting `new` on `cue` changes its file.
    fn changes(&mut self, cue: CueId, new: Option<&MediaRef>) -> bool {
        let Some(new) = new else {
            return false;
        };
        let workspace = self.workspace;
        let current = self
            .current
            .get_or_insert_with(|| media_of_cues(workspace).into_iter().collect());
        current.get(&cue) != Some(new)
    }

    fn collect(&mut self, command: &Command) {
        match command {
            Command::AddCue { cue, .. } => {
                for c in crate::tree::descendants(std::slice::from_ref::<Cue>(cue)) {
                    if c.kind.media().is_some() {
                        self.push(c.id);
                    }
                }
            }
            Command::SetCueProperty { id, property } => {
                if self.changes(*id, property_media(property)) {
                    self.push(*id);
                }
            }
            Command::BatchSet { ids, property } => {
                for id in ids {
                    if self.changes(*id, property_media(property)) {
                        self.push(*id);
                    }
                }
            }
            Command::Transaction { commands } => {
                for c in commands {
                    self.collect(c);
                }
            }
            _ => {}
        }
    }
}

/// The file a property sets, if it sets one.
fn property_media(property: &CueProperty) -> Option<&MediaRef> {
    match property {
        CueProperty::Media { value } => value.as_ref(),
        CueProperty::Kind { value } => value.media(),
        _ => None,
    }
}

/// The command that points each listed cue at its copy, as one undo step: only the file
/// changes. Cues that are gone or play no file are skipped. An empty result is an empty
/// transaction, which `History` ignores.
pub fn retarget_command(workspace: &Workspace, retargets: &[(CueId, MediaRef)]) -> Command {
    let wanted: std::collections::HashSet<CueId> = retargets.iter().map(|(cue, _)| *cue).collect();
    // One walk of the show that stops at the last wanted cue, not one per cue.
    let mut file_cues = std::collections::HashSet::new();
    for cue in workspace.cues() {
        if file_cues.len() == wanted.len() {
            break;
        }
        if wanted.contains(&cue.id)
            && matches!(cue.kind, CueKind::Audio { .. } | CueKind::Video { .. })
        {
            file_cues.insert(cue.id);
        }
    }
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

/// The file now set on each of the listed cues that has one, from one walk of the show that
/// stops at the last listed cue: a wake's cost follows the part of the show it touches, not
/// every cue.
pub fn media_of_listed(
    workspace: &Workspace,
    cues: &std::collections::HashSet<CueId>,
) -> std::collections::HashMap<CueId, MediaRef> {
    let mut found = std::collections::HashMap::new();
    let mut seen = 0;
    for cue in workspace.cues() {
        if seen == cues.len() {
            break;
        }
        if cues.contains(&cue.id) {
            seen += 1;
            if let Some(media) = cue.kind.media() {
                found.insert(cue.id, media.clone());
            }
        }
    }
    found
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
