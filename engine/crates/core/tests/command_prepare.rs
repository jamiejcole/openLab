// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `Command::prepare`: media paths stored the way show files keep them, and a raw clipboard
//! turned into a `Paste` (WS-03, ED-05).

mod common;

use common::*;
use openlab_core::*;

const POSIX: PathStyle = PathStyle::Posix;
const WINDOWS: PathStyle = PathStyle::Windows;

fn pos() -> Position {
    Position {
        list: list_id(1),
        parent: None,
        index: 0,
    }
}

fn audio_at(n: u128, media: &str) -> Cue {
    let mut c = cue(n, audio());
    if let CueKind::Audio { media: m, .. } = &mut c.kind {
        *m = Some(MediaRef::new(media));
    }
    c
}

fn media(c: &Cue) -> &str {
    match &c.kind {
        CueKind::Audio {
            media: Some(m), ..
        } => m.as_str(),
        other => panic!("not an audio cue with a file: {other:?}"),
    }
}

fn prepared(command: Command, dir: Option<&str>, style: PathStyle) -> Command {
    let ws = workspace(vec![]);
    let mut next = 1000;
    command
        .prepare(
            &ws,
            &MediaPaths {
                show_dir: dir,
                style,
            },
            &mut || {
                next += 1;
                cue_id(next)
            },
        )
        .unwrap()
}

fn added_media(command: Command, dir: Option<&str>, style: PathStyle) -> String {
    match prepared(command, dir, style) {
        Command::AddCue { cue, .. } => media(&cue).to_owned(),
        other => panic!("{other:?}"),
    }
}

fn add(media: &str) -> Command {
    Command::AddCue {
        at: pos(),
        cue: audio_at(1, media),
        auto_number: true,
    }
}

#[test]
fn a_file_inside_the_show_folder_is_relative_and_one_outside_is_absolute() {
    assert_eq!(
        added_media(add("/Show/media/a.wav"), Some("/Show"), POSIX),
        "media/a.wav"
    );
    assert_eq!(
        added_media(add("/Other/a.wav"), Some("/Show"), POSIX),
        "/Other/a.wav"
    );
    assert_eq!(
        added_media(add("/Show2/a.wav"), Some("/Show"), POSIX),
        "/Show2/a.wav"
    );
}

#[test]
fn a_show_never_saved_keeps_the_absolute_path() {
    assert_eq!(
        added_media(add("/Show/media/a.wav"), None, POSIX),
        "/Show/media/a.wav"
    );
}

#[test]
fn windows_paths_are_converted_and_made_relative() {
    assert_eq!(
        added_media(add(r"C:\Show\media\a.wav"), Some(r"C:\Show"), WINDOWS),
        "media/a.wav"
    );
    assert_eq!(
        added_media(add(r"D:\Lib\a.wav"), Some(r"C:\Show"), WINDOWS),
        "D:/Lib/a.wav"
    );
    assert_eq!(
        added_media(add(r"\\srv\share\a.wav"), Some(r"C:\Show"), WINDOWS),
        "//srv/share/a.wav"
    );
    assert_eq!(
        added_media(add(r"media\a.wav"), None, WINDOWS),
        "media/a.wav"
    );
}

#[test]
fn a_decomposed_name_is_stored_composed() {
    let nfd = "/Show/cafe\u{301}.wav";
    assert_eq!(
        added_media(add(nfd), Some("/Show"), POSIX),
        "caf\u{e9}.wav"
    );
}

#[test]
fn an_empty_path_stays_empty_rather_than_becoming_the_show_folder() {
    assert_eq!(added_media(add(""), Some("/Show"), POSIX), "");
}

#[test]
fn set_kind_and_set_media_and_batch_set_are_converted() {
    let kind = audio_at(1, "/Show/k.wav").kind;
    let set_kind = Command::SetCueProperty {
        id: cue_id(1),
        property: CueProperty::Kind { value: kind },
    };
    let Command::SetCueProperty {
        property: CueProperty::Kind { value },
        ..
    } = prepared(set_kind, Some("/Show"), POSIX)
    else {
        panic!("kind");
    };
    assert!(matches!(value, CueKind::Audio { media: Some(m), .. } if m.as_str() == "k.wav"));

    let set_media = Command::SetCueProperty {
        id: cue_id(1),
        property: CueProperty::Media {
            value: Some(MediaRef::new("/Show/m.wav")),
        },
    };
    let Command::SetCueProperty {
        property: CueProperty::Media { value },
        ..
    } = prepared(set_media, Some("/Show"), POSIX)
    else {
        panic!("media");
    };
    assert_eq!(value.unwrap().as_str(), "m.wav");

    let batch = Command::BatchSet {
        ids: vec![cue_id(1), cue_id(2)],
        property: CueProperty::Media {
            value: Some(MediaRef::new("/Show/b.wav")),
        },
    };
    let Command::BatchSet {
        property: CueProperty::Media { value },
        ..
    } = prepared(batch, Some("/Show"), POSIX)
    else {
        panic!("batch");
    };
    assert_eq!(value.unwrap().as_str(), "b.wav");

    // Clearing the file needs no path.
    let clear = Command::SetCueProperty {
        id: cue_id(1),
        property: CueProperty::Media { value: None },
    };
    assert_eq!(prepared(clear.clone(), Some("/Show"), POSIX), clear);
}

#[test]
fn transactions_are_prepared_command_by_command() {
    let t = Command::Transaction {
        commands: vec![add("/Show/a.wav"), add("/Out/b.wav")],
    };
    let Command::Transaction { commands } = prepared(t, Some("/Show"), POSIX) else {
        panic!("transaction");
    };
    let paths: Vec<_> = commands
        .into_iter()
        .map(|c| match c {
            Command::AddCue { cue, .. } => media(&cue).to_owned(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(paths, ["a.wav", "/Out/b.wav"]);
}

#[test]
fn a_raw_clipboard_becomes_a_paste_of_fresh_ids_with_converted_media() {
    let command = Command::PasteClipboard {
        at: pos(),
        cues: vec![audio_at(1, "/Show/a.wav"), cue(2, CueKind::Memo)],
    };
    let Command::Paste { cues, .. } = prepared(command, Some("/Show"), POSIX) else {
        panic!("paste");
    };
    assert_eq!(
        cues.iter().map(|c| c.id).collect::<Vec<_>>(),
        [cue_id(1001), cue_id(1002)]
    );
    assert_eq!(media(&cues[0]), "a.wav");
}

#[test]
fn a_prepared_paste_applies_and_an_unprepared_one_is_refused() {
    let mut ws = workspace(vec![]);
    let raw = Command::PasteClipboard {
        at: pos(),
        cues: vec![cue(1, CueKind::Memo)],
    };
    assert_eq!(ws.apply(raw.clone()), Err(CommandError::NotPrepared));
    assert!(ws.cue_lists[0].cues.is_empty());
    let ready = raw
        .prepare(
            &ws,
            &MediaPaths {
                show_dir: None,
                style: POSIX,
            },
            &mut || cue_id(77),
        )
        .unwrap();
    ws.apply(ready).unwrap();
    assert_eq!(ws.cue_lists[0].cues[0].id, cue_id(77));
}
