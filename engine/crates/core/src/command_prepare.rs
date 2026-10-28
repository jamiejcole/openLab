// SPDX-License-Identifier: GPL-3.0-or-later

//! What the engine does to a client's command before applying it (WS-03, ED-05): media paths
//! are stored the way show files keep them (relative to the show folder when inside it, forward
//! slashes, NFC; `docs/file-format.md` section 4.3), and a raw clipboard becomes a [`Paste`] of
//! cues with fresh ids. Core cannot make ids or know the folder, so the caller passes both in.
//!
//! [`Paste`]: Command::Paste

use crate::command::{Command, CommandError, CueProperty, prepare_paste};
use crate::cue::{Cue, CueKind, MediaRef};
use crate::media_path::{PathStyle, portable_ref};
use crate::model::{CueId, Workspace};
use crate::tree::walk_mut;

/// How media paths are stored in this workspace.
#[derive(Clone, Copy, Debug)]
pub struct MediaPaths<'a> {
    /// The show file's folder; `None` for a show never saved, whose paths stay as given (made
    /// portable only).
    pub show_dir: Option<&'a str>,
    /// How the host spells paths.
    pub style: PathStyle,
}

impl MediaPaths<'_> {
    fn store(&self, media: &MediaRef) -> MediaRef {
        // An empty path is "no file yet" to the cue engine; `portable_ref` would turn it into the
        // show folder.
        if media.as_str().is_empty() {
            return media.clone();
        }
        portable_ref(media.as_str(), self.show_dir, self.style)
    }

    fn store_cues(&self, cues: &mut [Cue]) {
        walk_mut(cues, &mut |cue| self.store_kind(&mut cue.kind));
    }

    fn store_kind(&self, kind: &mut CueKind) {
        if let CueKind::Audio {
            media: Some(media), ..
        } = kind
        {
            *media = self.store(media);
        }
    }

    fn store_property(&self, property: &mut CueProperty) {
        match property {
            CueProperty::Kind { value } => self.store_kind(value),
            CueProperty::Media { value: Some(media) } => *media = self.store(media),
            _ => {}
        }
    }
}

impl Command {
    /// The command to apply for what a client sent: media paths in `AddCue`, `Paste`,
    /// `SetCueProperty` and `BatchSet` stored per `paths`, and a `PasteClipboard` turned into a
    /// `Paste` by [`prepare_paste`] with ids from `new_id`. Inside a `Transaction` every command
    /// is prepared against the workspace as it is now, not as the commands before it leave it.
    ///
    /// Fails only when a clipboard is nested deeper than the limit; every other command passes
    /// through for `apply` to judge.
    pub fn prepare(
        self,
        workspace: &Workspace,
        paths: &MediaPaths<'_>,
        new_id: &mut impl FnMut() -> CueId,
    ) -> Result<Self, CommandError> {
        Ok(match self {
            Self::AddCue {
                at,
                mut cue,
                auto_number,
            } => {
                paths.store_cues(std::slice::from_mut(&mut cue));
                Self::AddCue {
                    at,
                    cue,
                    auto_number,
                }
            }
            Self::Paste { at, mut cues } => {
                paths.store_cues(&mut cues);
                Self::Paste { at, cues }
            }
            Self::PasteClipboard { at, cues } => {
                let mut cues = prepare_paste(workspace, &cues, &mut *new_id)?;
                paths.store_cues(&mut cues);
                Self::Paste { at, cues }
            }
            Self::SetCueProperty { id, mut property } => {
                paths.store_property(&mut property);
                Self::SetCueProperty { id, property }
            }
            Self::BatchSet { ids, mut property } => {
                paths.store_property(&mut property);
                Self::BatchSet { ids, property }
            }
            Self::Transaction { commands } => Self::Transaction {
                commands: commands
                    .into_iter()
                    // A transaction inside one is left for `apply` to refuse.
                    .map(|c| match c {
                        Self::Transaction { .. } => Ok(c),
                        other => other.prepare(workspace, paths, new_id),
                    })
                    .collect::<Result<_, _>>()?,
            },
            other => other,
        })
    }
}
