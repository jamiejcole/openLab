// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Planning a copy of media into the show folder (WS-04): which cues' files a command sets,
//! which files a copy takes and where they go, and the retarget as one undo step.

mod common;

use common::*;
use openlab_core::{
    AUDIO_FOLDER, Command, CopyMode, CueKind, CueProperty, History, MediaRef, Position,
    VIDEO_FOLDER, copy_items, media_set_by, retarget_command,
};

fn with_media(mut kind: CueKind, path: &str) -> CueKind {
    match &mut kind {
        CueKind::Audio { media, .. } | CueKind::Video { media, .. } => {
            *media = Some(MediaRef::new(path));
        }
        _ => {}
    }
    kind
}

fn at_end() -> Position {
    Position {
        list: list_id(1),
        parent: None,
        index: 0,
    }
}

#[test]
fn the_cues_whose_file_a_command_sets_are_the_ones_copied_on_add() {
    let added = cue(1, with_media(audio(), "/Lib/a.wav"));
    let no_file = cue(2, CueKind::Memo);
    let in_group = cue(4, with_media(video(), "/Lib/v.mov"));
    let grouped = cue(3, group(vec![in_group]));
    let command = Command::Transaction {
        commands: vec![
            Command::AddCue {
                at: at_end(),
                cue: added,
                auto_number: false,
            },
            Command::AddCue {
                at: at_end(),
                cue: no_file,
                auto_number: false,
            },
            Command::AddCue {
                at: at_end(),
                cue: grouped,
                auto_number: false,
            },
            Command::SetCueProperty {
                id: cue_id(5),
                property: CueProperty::Media {
                    value: Some(MediaRef::new("/Lib/b.wav")),
                },
            },
            // Clearing a file copies nothing.
            Command::SetCueProperty {
                id: cue_id(6),
                property: CueProperty::Media { value: None },
            },
            Command::BatchSet {
                ids: vec![cue_id(7), cue_id(1)],
                property: CueProperty::Media {
                    value: Some(MediaRef::new("/Lib/c.wav")),
                },
            },
            Command::SetCueProperty {
                id: cue_id(8),
                property: CueProperty::Kind {
                    value: with_media(audio(), "/Lib/d.wav"),
                },
            },
            Command::SetCueProperty {
                id: cue_id(9),
                property: CueProperty::Name { value: "x".into() },
            },
        ],
    };
    assert_eq!(
        media_set_by(&workspace(vec![]), &command),
        vec![cue_id(1), cue_id(4), cue_id(5), cue_id(7), cue_id(8)]
    );
    // A paste keeps the file its original has: nothing is copied for it.
    let paste = Command::Paste {
        at: at_end(),
        cues: vec![cue(10, with_media(audio(), "/Lib/a.wav"))],
    };
    assert!(media_set_by(&workspace(vec![]), &paste).is_empty());
}

/// A command that rewrites a whole kind for another reason (a level limit clamping a cue, a
/// pasted property, a batch edit) keeps the cue's file: nothing is copied. Only another file is.
#[test]
fn a_whole_kind_or_file_edit_that_keeps_the_cue_s_file_copies_nothing() {
    let ws = workspace(vec![
        cue(1, with_media(audio(), "audio/hit.wav")),
        cue(2, with_media(audio(), "/Lib/b.wav")),
    ]);
    let mut louder = with_media(audio(), "audio/hit.wav");
    if let CueKind::Audio { levels, .. } = &mut louder {
        levels.master_db = -6.0;
    }
    let same_kind = Command::Transaction {
        commands: vec![Command::SetCueProperty {
            id: cue_id(1),
            property: CueProperty::Kind { value: louder },
        }],
    };
    assert!(media_set_by(&ws, &same_kind).is_empty());
    let same_file = Command::BatchSet {
        ids: vec![cue_id(1), cue_id(2)],
        property: CueProperty::Media {
            value: Some(MediaRef::new("/Lib/b.wav")),
        },
    };
    assert_eq!(
        media_set_by(&ws, &same_file),
        vec![cue_id(1)],
        "cue 2 has that file already"
    );
    let other_file = Command::SetCueProperty {
        id: cue_id(1),
        property: CueProperty::Kind {
            value: with_media(audio(), "/Lib/new.wav"),
        },
    };
    assert_eq!(media_set_by(&ws, &other_file), vec![cue_id(1)]);
}

#[test]
fn on_demand_takes_every_file_and_marks_the_ones_already_inside() {
    let mut ws = workspace(vec![
        cue(1, with_media(audio(), "/Lib/a.wav")),
        cue(2, with_media(audio(), "audio/b.wav")),
        cue(3, with_media(audio(), "../Elsewhere/c.wav")),
        cue(4, CueKind::Memo),
    ]);
    ws.cue_lists[0]
        .cues
        .push(cue(5, with_media(video(), "/Lib/v.mov")));
    let items = copy_items(&ws, CopyMode::OnDemand, &[]);
    let seen: Vec<_> = items
        .iter()
        .map(|i| (i.cue, i.media.as_str(), i.folder, i.in_folder))
        .collect();
    assert_eq!(
        seen,
        vec![
            (cue_id(1), "/Lib/a.wav", AUDIO_FOLDER, false),
            (cue_id(2), "audio/b.wav", AUDIO_FOLDER, true),
            (cue_id(3), "../Elsewhere/c.wav", AUDIO_FOLDER, false),
            (cue_id(5), "/Lib/v.mov", VIDEO_FOLDER, false),
        ]
    );
    // On add only the cues named are taken, inside the folder or not.
    let added = copy_items(&ws, CopyMode::Added, &[cue_id(2), cue_id(4)]);
    assert_eq!(added.len(), 1);
    assert_eq!((added[0].cue, added[0].in_folder), (cue_id(2), true));
}

#[test]
fn the_retarget_is_one_undo_step_that_changes_only_the_file() {
    let mut ws = workspace(vec![
        cue(1, with_media(audio(), "/Lib/a.wav")),
        cue(2, with_media(audio(), "/Lib/b.wav")),
        cue(3, CueKind::Memo),
    ]);
    let before = ws.clone();
    let command = retarget_command(
        &ws,
        &[
            (cue_id(1), MediaRef::new("audio/a.wav")),
            (cue_id(2), MediaRef::new("audio/b.wav")),
            // Not a file cue, and a cue that is gone: skipped, never refused.
            (cue_id(3), MediaRef::new("audio/x.wav")),
            (cue_id(99), MediaRef::new("audio/y.wav")),
        ],
    );
    let mut history = History::new();
    assert!(history.execute(&mut ws, command).unwrap());
    let media = |ws: &openlab_core::Workspace, n| {
        ws.find_cue(cue_id(n))
            .and_then(|c| c.kind.media())
            .map(|m| m.as_str().to_owned())
    };
    assert_eq!(media(&ws, 1).as_deref(), Some("audio/a.wav"));
    assert_eq!(media(&ws, 2).as_deref(), Some("audio/b.wav"));
    assert!(history.undo(&mut ws).unwrap());
    assert_eq!(ws, before, "one undo puts every cue back");
    assert!(history.redo(&mut ws).unwrap());
    assert_eq!(media(&ws, 2).as_deref(), Some("audio/b.wav"));
}
