// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! Media paths stored relative to the show folder, the media-only property and the raw-clipboard
//! paste, through the control loop (WS-03, ED-05).

use crate::common;
use std::path::Path;

use crate::show::*;
use openlab_core::protocol::{ClientMessage, ErrorCode, Outcome, ServerMessage};
use openlab_core::{
    Command, CueId, CueKind, CueProperty, MAX_GROUP_DEPTH, MediaRef, Position, Workspace,
};
use openlab_engine::control::offline::OfflineShow;

fn at(index: usize) -> Position {
    Position {
        list: list_id(),
        parent: None,
        index,
    }
}

fn send(s: &mut OfflineShow, command: Command) -> Vec<ServerMessage> {
    let id = s.request_id();
    s.send(ClientMessage::Command {
        id,
        command: Box::new(command),
        coalesce: None,
    })
}

fn ok(replies: &[ServerMessage]) {
    assert!(
        replies.iter().all(|m| !matches!(
            m,
            ServerMessage::Result {
                outcome: Outcome::Error(_),
                ..
            }
        )),
        "{replies:?}"
    );
}

fn error_code(replies: &[ServerMessage]) -> ErrorCode {
    replies
        .iter()
        .find_map(|m| match m {
            ServerMessage::Result {
                outcome: Outcome::Error(e),
                ..
            } => Some(e.code),
            _ => None,
        })
        .expect("an error result")
}

fn find(cues: &[openlab_core::Cue], target: CueId) -> Option<&openlab_core::Cue> {
    for cue in cues {
        if cue.id == target {
            return Some(cue);
        }
        if let CueKind::Group { children, .. } = &cue.kind
            && let Some(found) = find(children, target)
        {
            return Some(found);
        }
    }
    None
}

fn media_of(ws: &Workspace, n: u128) -> Option<String> {
    match &find(&ws.cue_lists[0].cues, id(n))?.kind {
        CueKind::Audio { media, .. } => media.as_ref().map(|m| m.as_str().to_owned()),
        _ => None,
    }
}

fn add(s: &mut OfflineShow, n: u128, file: &Path) {
    ok(&send(
        s,
        Command::AddCue {
            at: at(0),
            cue: audio(n, file, 0),
            auto_number: false,
        },
    ));
}

/// A show saved in `dir`'s `show` folder.
fn saved_show(env: &common::Env) -> OfflineShow {
    let ws = workspace(vec![audio(1, Path::new("old.wav"), 0)]);
    let mut c = config(&ws);
    c.show_path = Some(env.show_path());
    c.app_dirs = Some(env.dirs.clone());
    show_with(ws, c)
}

#[test]
fn add_stores_a_file_inside_the_show_folder_relative_and_one_outside_absolute() {
    let env = common::env();
    let mut s = saved_show(&env);
    let inside = env.show_dir().join("media").join("thunder.wav");
    let outside = env.root.path().join("elsewhere").join("rain.wav");
    add(&mut s, 2, &inside);
    add(&mut s, 3, &outside);
    let ws = s.control().workspace();
    assert_eq!(media_of(ws, 2).as_deref(), Some("media/thunder.wav"));
    assert_eq!(
        media_of(ws, 3).as_deref(),
        Some(outside.to_str().unwrap().replace('\\', "/").as_str())
    );
}

#[test]
fn add_to_a_show_never_saved_keeps_the_path_absolute() {
    let env = common::env();
    let ws = workspace(vec![]);
    let mut s = show(ws);
    let file = env.show_dir().join("media").join("thunder.wav");
    add(&mut s, 2, &file);
    assert_eq!(
        media_of(s.control().workspace(), 2).as_deref(),
        Some(file.to_str().unwrap().replace('\\', "/").as_str())
    );
}

#[test]
fn set_kind_stores_the_media_relative_too() {
    let env = common::env();
    let mut s = saved_show(&env);
    let mut kind = find(&s.control().workspace().cue_lists[0].cues, id(1))
        .unwrap()
        .kind
        .clone();
    let file = env.show_dir().join("new.wav");
    if let CueKind::Audio { media, .. } = &mut kind {
        *media = Some(MediaRef::new(file.to_str().unwrap()));
    }
    ok(&send(
        &mut s,
        Command::SetCueProperty {
            id: id(1),
            property: CueProperty::Kind { value: kind },
        },
    ));
    assert_eq!(
        media_of(s.control().workspace(), 1).as_deref(),
        Some("new.wav")
    );
}

#[test]
fn media_property_replaces_only_the_file_and_is_undoable() {
    let env = common::env();
    let mut s = saved_show(&env);
    let before = find(&s.control().workspace().cue_lists[0].cues, id(1))
        .unwrap()
        .clone();
    let file = env.show_dir().join("sfx").join("door.wav");
    ok(&send(
        &mut s,
        Command::SetCueProperty {
            id: id(1),
            property: CueProperty::Media {
                value: Some(MediaRef::new(file.to_str().unwrap())),
            },
        },
    ));
    let after = find(&s.control().workspace().cue_lists[0].cues, id(1))
        .unwrap()
        .clone();
    assert_eq!(
        media_of(s.control().workspace(), 1).as_deref(),
        Some("sfx/door.wav")
    );
    // Everything but the file is as it was.
    let (CueKind::Audio { media: m0, .. }, CueKind::Audio { media: m1, .. }) =
        (&before.kind, &after.kind)
    else {
        panic!("audio cues");
    };
    assert_ne!(m0, m1);
    let mut restored = after.clone();
    if let CueKind::Audio { media, .. } = &mut restored.kind {
        media.clone_from(m0);
    }
    assert_eq!(restored, before);

    let undo_id = s.request_id();
    ok(&s.send(ClientMessage::Undo { id: undo_id }));
    assert_eq!(
        find(&s.control().workspace().cue_lists[0].cues, id(1)),
        Some(&before)
    );
}

#[test]
fn media_property_on_another_kind_of_cue_is_refused() {
    let mut s = show(workspace(vec![cue(1, CueKind::Memo)]));
    let replies = send(
        &mut s,
        Command::SetCueProperty {
            id: id(1),
            property: CueProperty::Media {
                value: Some(MediaRef::new("/x.wav")),
            },
        },
    );
    assert_eq!(error_code(&replies), ErrorCode::CommandRefused);
}

fn clipboard() -> Vec<openlab_core::Cue> {
    // Ids, numbers, a patch and a target that mean nothing in the show they are pasted into.
    let mut fade_cue = cue(
        501,
        CueKind::Fade {
            target: Some(id(500)),
            duration: 1.0,
            curve: openlab_core::FadeCurve::Linear,
            levels: openlab_core::FadeLevels {
                master_db: None,
                output_db: vec![],
                crosspoints: vec![],
            },
            stop_target_when_done: false,
        },
    );
    fade_cue.number = Some("9".into());
    let mut a = audio(500, Path::new("/lib/a.wav"), 0);
    a.number = Some("1".into());
    if let CueKind::Audio { patch, .. } = &mut a.kind {
        *patch = openlab_core::PatchId(uuid::Uuid::from_u128(777));
    }
    vec![a, fade_cue]
}

#[test]
fn raw_paste_gives_fresh_ids_follows_targets_and_repairs_what_the_show_lacks() {
    let mut s = show(workspace(vec![audio(1, Path::new("/x.wav"), 0)])); // number "1" is in use
    ok(&send(
        &mut s,
        Command::PasteClipboard {
            at: at(1),
            cues: clipboard(),
            source_dir: None,
        },
    ));
    let ws = s.control().workspace();
    let cues = &ws.cue_lists[0].cues;
    assert_eq!(cues.len(), 3);
    let (a, f) = (&cues[1], &cues[2]);
    assert_ne!(a.id, id(500));
    assert_ne!(f.id, id(501));
    assert_ne!(a.id, f.id);
    assert_eq!(f.kind.target(), Some(a.id), "the target follows the copy");
    assert_eq!(a.number, None, "1 is used here");
    assert_eq!(f.number.as_deref(), Some("9"));
    assert!(matches!(&a.kind, CueKind::Audio { patch, .. } if *patch == patch_id()));

    // One undo step removes both.
    let undo_id = s.request_id();
    ok(&s.send(ClientMessage::Undo { id: undo_id }));
    assert_eq!(s.control().workspace().cue_lists[0].cues.len(), 1);
}

#[test]
fn pasting_the_same_clipboard_twice_never_repeats_an_id() {
    let mut s = show(workspace(vec![]));
    for _ in 0..2 {
        ok(&send(
            &mut s,
            Command::PasteClipboard {
                at: at(0),
                cues: clipboard(),
                source_dir: None,
            },
        ));
    }
    let cues = &s.control().workspace().cue_lists[0].cues;
    let mut ids: Vec<_> = cues.iter().map(|c| c.id).collect();
    ids.sort_by_key(|c| c.0);
    ids.dedup();
    assert_eq!(ids.len(), 4);
}

#[test]
fn raw_paste_stores_absolute_media_inside_the_show_folder_relative() {
    let env = common::env();
    let mut s = saved_show(&env);
    let inside = env.show_dir().join("media").join("a.wav");
    ok(&send(
        &mut s,
        Command::PasteClipboard {
            at: at(0),
            cues: vec![audio(500, &inside, 0)],
            source_dir: None,
        },
    ));
    let pasted = &s.control().workspace().cue_lists[0].cues[0];
    assert!(matches!(
        &pasted.kind,
        CueKind::Audio { media: Some(m), .. } if m.as_str() == "media/a.wav"
    ));
}

#[test]
fn a_clipboard_nested_too_deep_is_refused_and_changes_nothing() {
    let mut s = show(workspace(vec![]));
    let mut deepest = cue(1000, CueKind::Memo);
    for n in 0..=MAX_GROUP_DEPTH as u128 {
        deepest = cue(
            1001 + n,
            CueKind::Group {
                mode: openlab_core::GroupMode::Timeline,
                children: vec![deepest],
            },
        );
    }
    let replies = send(
        &mut s,
        Command::PasteClipboard {
            at: at(0),
            cues: vec![deepest],
            source_dir: None,
        },
    );
    assert_eq!(error_code(&replies), ErrorCode::CommandRefused);
    assert!(s.control().workspace().cue_lists[0].cues.is_empty());
}

#[test]
fn a_paste_clipboard_that_reaches_apply_unprepared_is_refused_not_a_panic() {
    let mut ws = workspace(vec![]);
    let error = ws
        .apply(Command::PasteClipboard {
            at: at(0),
            cues: clipboard(),
            source_dir: None,
        })
        .unwrap_err();
    assert!(matches!(error, openlab_core::CommandError::NotPrepared));
}

fn portable(p: &Path) -> String {
    openlab_core::to_portable(p.to_str().unwrap(), openlab_core::PathStyle::host())
}

fn paste_media(s: &mut OfflineShow, media: &str, source_dir: Option<&Path>) {
    let mut c = audio(500, Path::new("x.wav"), 0);
    if let CueKind::Audio { media: m, .. } = &mut c.kind {
        *m = Some(MediaRef::new(media));
    }
    ok(&send(
        s,
        Command::PasteClipboard {
            at: at(0),
            cues: vec![c],
            source_dir: source_dir.map(portable),
        },
    ));
}

fn first_media(s: &OfflineShow) -> String {
    match &s.control().workspace().cue_lists[0].cues[0].kind {
        CueKind::Audio { media, .. } => media.as_ref().unwrap().as_str().to_owned(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_cue_pasted_from_a_show_in_another_folder_keeps_playing_its_file() {
    let env = common::env();
    let source = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(source.path().join("media")).unwrap();
    let file = source.path().join("media").join("a.wav");
    std::fs::write(&file, b"x").unwrap();
    let mut s = saved_show(&env);
    paste_media(&mut s, "media/a.wav", Some(source.path()));
    let stored = first_media(&s);
    assert_eq!(
        stored,
        portable(&file),
        "absolute: the destination is elsewhere"
    );
    assert!(Path::new(&stored).exists());

    // Undo removes the paste in one step.
    let undo_id = s.request_id();
    ok(&s.send(ClientMessage::Undo { id: undo_id }));
    assert_eq!(s.control().workspace().cue_lists[0].cues.len(), 1);
}

#[test]
fn a_cue_pasted_back_into_a_show_whose_folder_holds_the_file_goes_relative() {
    let env = common::env();
    let mut s = saved_show(&env);
    // The source show is a sub-folder of this show's folder.
    let source = env.show_dir().join("sub");
    paste_media(&mut s, "a.wav", Some(&source));
    assert_eq!(first_media(&s), "sub/a.wav");
}

#[test]
fn pasting_between_untitled_shows_keeps_the_absolute_path_() {
    let env = common::env();
    let inside = env.show_dir().join("media").join("a.wav");
    let outside = env.root.path().join("elsewhere").join("b.wav");
    let mut s = show(workspace(vec![]));
    paste_media(&mut s, &portable(&inside), None);
    assert_eq!(first_media(&s), portable(&inside));
    paste_media(&mut s, &portable(&outside), None);
    assert_eq!(first_media(&s), portable(&outside));
}

#[test]
fn a_hostile_source_folder_is_ignored_not_trusted() {
    let env = common::env();
    let mut s = saved_show(&env);
    // Relative, empty, and a bare root: none may crash or send a relative path outside.
    paste_media(&mut s, "a.wav", Some(Path::new("")));
    assert_eq!(first_media(&s), "a.wav");
    paste_media(&mut s, "a.wav", Some(Path::new("not/absolute")));
    assert_eq!(first_media(&s), "a.wav");
    paste_media(&mut s, "../../x.wav", Some(Path::new("/")));
    assert_eq!(first_media(&s), "/x.wav");
}
