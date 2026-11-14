// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! Editing a Video cue (CT-03): its file, its stage and a still's duration, on every path that
//! creates or changes one (add, paste, clipboard paste, kind change, batch, the property alone).

mod common;

use common::*;
use openlab_core::*;

fn video_cue(n: u128) -> Cue {
    cue(n, video())
}

fn ws() -> Workspace {
    let mut ws = workspace(vec![video_cue(1), video_cue(2), cue(3, audio())]);
    ws.stages = vec![
        Stage::new(stage_id(1), "Stage 1", 1920, 1080),
        Stage::new(stage_id(2), "Stage 2", 1280, 720),
    ];
    ws
}

fn set(id: u128, property: CueProperty) -> Command {
    Command::SetCueProperty {
        id: cue_id(id),
        property,
    }
}

fn kind(ws: &Workspace, n: u128) -> (Option<String>, Option<StageId>, Option<f64>) {
    match &ws.find_cue(cue_id(n)).unwrap().kind {
        CueKind::Video {
            media,
            stage,
            duration,
        } => (
            media.as_ref().map(|m| m.as_str().to_owned()),
            *stage,
            *duration,
        ),
        other => panic!("{other:?}"),
    }
}

fn duration_fields(value: serde_json::Value) -> CueProperty {
    let mut fields = serde_json::Map::new();
    fields.insert("duration".into(), value);
    CueProperty::KindFields { value: fields }
}

fn refused_time(result: Result<Inverse, CommandError>) {
    match result {
        Err(CommandError::InvalidCue(issues)) => assert!(
            issues
                .iter()
                .any(|i| i.kind == IssueKind::InvalidTime { field: "duration" }),
            "{issues:?}"
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_stage_is_set_undone_and_redone_and_the_same_stage_is_no_edit() {
    let mut ws = ws();
    let mut history = History::default();
    let stage = |s| set(1, CueProperty::Stage { value: s });
    assert!(
        history
            .execute(&mut ws, stage(Some(stage_id(2))))
            .unwrap()
    );
    assert_eq!(kind(&ws, 1).1, Some(stage_id(2)));
    assert!(
        !history
            .execute(&mut ws, stage(Some(stage_id(2))))
            .unwrap()
    );
    // "none" is a stage choice too; a stage the workspace lacks is accepted (the cue breaks).
    assert!(history.execute(&mut ws, stage(None)).unwrap());
    assert_eq!(kind(&ws, 1).1, None);
    assert!(
        history
            .execute(&mut ws, stage(Some(stage_id(9))))
            .unwrap()
    );
    assert!(history.undo(&mut ws).unwrap());
    assert!(history.undo(&mut ws).unwrap());
    assert_eq!(kind(&ws, 1).1, Some(stage_id(2)));
    assert!(history.redo(&mut ws).unwrap());
    assert_eq!(kind(&ws, 1).1, None);
}

#[test]
fn stage_is_refused_on_a_cue_that_is_not_a_video_cue() {
    let mut ws = ws();
    let before = ws.clone();
    assert!(matches!(
        ws.apply(set(
            3,
            CueProperty::Stage {
                value: Some(stage_id(1))
            }
        )),
        Err(CommandError::PropertyNotApplicable(_))
    ));
    assert_eq!(ws, before);
}

#[test]
fn the_file_alone_is_replaced_and_keeps_the_stage() {
    let mut ws = ws();
    let undo = ws
        .apply(set(
            1,
            CueProperty::Media {
                value: Some(MediaRef::new("b.png")),
            },
        ))
        .unwrap();
    assert_eq!(
        kind(&ws, 1),
        (Some("b.png".into()), Some(stage_id(1)), None)
    );
    ws.apply(undo).unwrap();
    assert_eq!(kind(&ws, 1).0.as_deref(), Some("media/v.mov"));
}

#[test]
fn a_still_duration_is_set_cleared_and_refused_out_of_range() {
    let mut ws = ws();
    ws.apply(set(1, duration_fields(serde_json::json!(5.0))))
        .unwrap();
    assert_eq!(kind(&ws, 1).2, Some(5.0));
    ws.apply(set(1, duration_fields(serde_json::Value::Null)))
        .unwrap();
    assert_eq!(kind(&ws, 1).2, None);
    let before = ws.clone();
    refused_time(ws.apply(set(1, duration_fields(serde_json::json!(-1.0)))));
    refused_time(ws.apply(set(1, duration_fields(serde_json::json!(1e9)))));
    assert_eq!(ws, before);
}

/// Every path that creates or replaces a Video cue refuses an out-of-range duration.
#[test]
fn every_path_that_makes_a_video_cue_refuses_a_bad_duration() {
    let bad = || {
        let mut c = cue(10, video());
        if let CueKind::Video { duration, .. } = &mut c.kind {
            *duration = Some(-5.0);
        }
        c
    };
    let at = Position {
        list: list_id(1),
        parent: None,
        index: 0,
    };
    let mut ws = ws();
    let before = ws.clone();
    refused_time(ws.apply(Command::AddCue {
        at: at.clone(),
        cue: bad(),
        auto_number: false,
    }));
    refused_time(ws.apply(Command::Paste {
        at: at.clone(),
        cues: vec![cue(11, group(vec![bad()]))],
    }));
    let pasted = Command::PasteClipboard {
        at,
        cues: vec![bad()],
        source_dir: None,
    }
    .prepare(
        &ws,
        &MediaPaths {
            show_dir: None,
            style: PathStyle::Posix,
        },
        &mut || cue_id(500),
    )
    .unwrap();
    refused_time(ws.apply(pasted));
    refused_time(ws.apply(set(
        3,
        CueProperty::Kind {
            value: bad().kind,
        },
    )));
    assert_eq!(ws, before);
}

#[test]
fn a_batch_sets_the_stage_on_every_video_cue_or_none() {
    let mut ws = ws();
    ws.apply(Command::BatchSet {
        ids: vec![cue_id(1), cue_id(2)],
        property: CueProperty::Stage {
            value: Some(stage_id(2)),
        },
    })
    .unwrap();
    assert_eq!((kind(&ws, 1).1, kind(&ws, 2).1), (Some(stage_id(2)), Some(stage_id(2))));
    let before = ws.clone();
    assert!(
        ws.apply(Command::BatchSet {
            ids: vec![cue_id(1), cue_id(3)],
            property: CueProperty::Stage { value: None },
        })
        .is_err()
    );
    assert_eq!(ws, before);
}

/// A Video cue's file is stored the way an Audio cue's is: relative inside the show folder,
/// and a clipboard from another show's folder is read against that folder.
#[test]
fn a_video_cues_file_is_stored_as_an_audio_cues_is() {
    let ws = ws();
    let paths = MediaPaths {
        show_dir: Some("/Shows/Play"),
        style: PathStyle::Posix,
    };
    let mut c = cue(10, video());
    if let CueKind::Video { media, .. } = &mut c.kind {
        *media = Some(MediaRef::new("/Shows/Play/video/intro.mov"));
    }
    let Command::AddCue { cue: added, .. } = (Command::AddCue {
        at: Position {
            list: list_id(1),
            parent: None,
            index: 0,
        },
        cue: c.clone(),
        auto_number: false,
    })
    .prepare(&ws, &paths, &mut || cue_id(600))
    .unwrap() else {
        panic!()
    };
    assert_eq!(added.kind.media().unwrap().as_str(), "video/intro.mov");
    let Command::SetCueProperty { property, .. } = set(
        1,
        CueProperty::Media {
            value: Some(MediaRef::new("/Shows/Play/still.png")),
        },
    )
    .prepare(&ws, &paths, &mut || cue_id(601))
    .unwrap() else {
        panic!()
    };
    assert_eq!(
        property,
        CueProperty::Media {
            value: Some(MediaRef::new("still.png"))
        }
    );
    if let CueKind::Video { media, .. } = &mut c.kind {
        *media = Some(MediaRef::new("video/intro.mov"));
    }
    let Command::Paste { cues, .. } = (Command::PasteClipboard {
        at: Position {
            list: list_id(1),
            parent: None,
            index: 0,
        },
        cues: vec![c],
        source_dir: Some("/Shows/Other".into()),
    })
    .prepare(&ws, &paths, &mut || cue_id(602))
    .unwrap() else {
        panic!()
    };
    assert_eq!(
        cues[0].kind.media().unwrap().as_str(),
        "/Shows/Other/video/intro.mov"
    );
}

/// Relink finds a Video cue's missing file and points it at the new one, as for audio.
#[test]
fn relink_covers_video_cues() {
    let mut ws = ws();
    let found = media_of_cues(&ws);
    assert!(found.iter().any(|(id, _)| *id == cue_id(1)), "{found:?}");
    let command = relink_command(
        &ws,
        &[Relink {
            cue: cue_id(1),
            new: MediaRef::new("found/v.mov"),
        }],
    );
    ws.apply(command).unwrap();
    assert_eq!(kind(&ws, 1).0.as_deref(), Some("found/v.mov"));
}
