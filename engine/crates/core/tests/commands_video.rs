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
            ..
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
    assert!(history.execute(&mut ws, stage(Some(stage_id(2)))).unwrap());
    assert_eq!(kind(&ws, 1).1, Some(stage_id(2)));
    assert!(!history.execute(&mut ws, stage(Some(stage_id(2)))).unwrap());
    // "none" is a stage choice too; a stage the workspace lacks is accepted (the cue breaks).
    assert!(history.execute(&mut ws, stage(None)).unwrap());
    assert_eq!(kind(&ws, 1).1, None);
    assert!(history.execute(&mut ws, stage(Some(stage_id(9)))).unwrap());
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
    refused_time(ws.apply(set(3, CueProperty::Kind { value: bad().kind })));
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
    assert_eq!(
        (kind(&ws, 1).1, kind(&ws, 2).1),
        (Some(stage_id(2)), Some(stage_id(2)))
    );
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

fn geometry(ws: &Workspace, n: u128) -> VideoGeometry {
    match &ws.find_cue(cue_id(n)).unwrap().kind {
        CueKind::Video { geometry, .. } => *geometry,
        other => panic!("{other:?}"),
    }
}

fn fields(pairs: &[(&str, serde_json::Value)]) -> CueProperty {
    let mut map = serde_json::Map::new();
    for (k, v) in pairs {
        map.insert((*k).into(), v.clone());
    }
    CueProperty::KindFields { value: map }
}

fn refused_value(result: Result<Inverse, CommandError>, field: &str) {
    match result {
        Err(CommandError::InvalidCue(issues)) => assert!(
            issues
                .iter()
                .any(|i| matches!(i.kind, IssueKind::InvalidValue { field: f } if f == field)),
            "{issues:?}"
        ),
        other => panic!("{field}: {other:?}"),
    }
}

/// Geometry fields are fields of the kind (VID-04): each is set on its own, so a field another
/// client set meanwhile is kept; undone and redone; the value it has already is no edit.
#[test]
fn geometry_fields_are_set_one_at_a_time_undone_and_redone() {
    use serde_json::json;
    let mut ws = ws();
    assert_eq!(geometry(&ws, 1), VideoGeometry::default());
    ws.apply(set(1, fields(&[("opacity", json!(0.5))])))
        .unwrap();
    let undo = ws
        .apply(set(
            1,
            fields(&[
                ("fill_stage", json!(false)),
                ("translation", json!({"x": 10.0, "y": -20.0})),
                ("rotation", json!(45)),
                (
                    "crop",
                    json!({"top": 1, "bottom": 2, "left": 3, "right": 4}),
                ),
                ("layer", json!(7)),
                ("smooth", json!(false)),
                ("fill_style", json!("stretch")),
            ]),
        ))
        .unwrap();
    let g = geometry(&ws, 1);
    assert_eq!(g.opacity, 0.5);
    assert!(!g.fill_stage && !g.smooth);
    assert_eq!(g.translation, Pair { x: 10.0, y: -20.0 });
    assert_eq!(g.rotation, 45.0);
    assert_eq!(g.crop.right, 4.0);
    assert_eq!(g.layer, 7);
    assert_eq!(g.fill_style, FillStyle::Stretch);
    let redo = ws.apply(undo).unwrap();
    assert_eq!(
        geometry(&ws, 1),
        VideoGeometry {
            opacity: 0.5,
            ..VideoGeometry::default()
        }
    );
    ws.apply(redo).unwrap();
    assert_eq!(geometry(&ws, 1), g);
    let same = ws.apply(set(1, fields(&[("layer", json!(7))]))).unwrap();
    assert_eq!(same, Command::Transaction { commands: vec![] });
}

/// Out of range is refused, field by field, and nothing changes; a value of the wrong type
/// (a layer beyond a whole number's range, text, a crop missing an edge) is refused too.
#[test]
fn geometry_out_of_range_is_refused_field_by_field() {
    use serde_json::json;
    let mut ws = ws();
    let before = ws.clone();
    for (key, value, field) in [
        ("opacity", json!(1.01), "opacity"),
        ("opacity", json!(-0.01), "opacity"),
        ("layer", json!(1001), "layer"),
        ("rotation", json!(360.5), "rotation"),
        ("scale", json!({"x": 101, "y": 1}), "scale.x"),
        ("scale", json!({"x": 1, "y": -101}), "scale.y"),
        ("translation", json!({"x": 0, "y": 70000}), "translation.y"),
        ("anchor", json!({"x": -70000, "y": 0}), "anchor.x"),
        (
            "crop",
            json!({"top": -1, "bottom": 0, "left": 0, "right": 0}),
            "crop.top",
        ),
        (
            "crop",
            json!({"top": 0, "bottom": 0, "left": 0, "right": 16385}),
            "crop.right",
        ),
    ] {
        refused_value(ws.apply(set(1, fields(&[(key, value)]))), field);
    }
    for (key, value) in [
        ("layer", json!(-1)),
        ("layer", json!(70000)),
        ("layer", json!(1.5)),
        ("opacity", json!("half")),
        ("fill_style", json!("zoom")),
        ("crop", json!({"middle": 1})),
        ("scale", json!({"z": 1})),
        ("translation", json!(5)),
    ] {
        assert!(
            matches!(
                ws.apply(set(1, fields(&[(key, value.clone())]))),
                Err(CommandError::InvalidValue(_))
            ),
            "{key} {value}"
        );
    }
    assert_eq!(ws, before);
}

/// Every path that creates or replaces a Video cue refuses geometry out of range.
#[test]
fn every_path_that_makes_a_video_cue_refuses_bad_geometry() {
    let bad = || {
        let mut c = cue(10, video());
        if let CueKind::Video { geometry, .. } = &mut c.kind {
            geometry.opacity = 2.0;
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
    refused_value(
        ws.apply(Command::AddCue {
            at: at.clone(),
            cue: bad(),
            auto_number: false,
        }),
        "opacity",
    );
    refused_value(
        ws.apply(Command::Paste {
            at: at.clone(),
            cues: vec![cue(11, group(vec![bad()]))],
        }),
        "opacity",
    );
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
    refused_value(ws.apply(pasted), "opacity");
    refused_value(
        ws.apply(set(3, CueProperty::Kind { value: bad().kind })),
        "opacity",
    );
    assert_eq!(ws, before);
}

/// A pair or the crop may name only the parts that change: two such edits made one after the
/// other (sent before either was answered) both hold, and undoing one puts back only its part.
#[test]
fn a_part_of_a_pair_or_one_crop_edge_changes_alone() {
    use serde_json::json;
    let mut ws = ws();
    ws.apply(set(1, fields(&[("crop", json!({"top": 4}))])))
        .unwrap();
    let undo_left = ws
        .apply(set(1, fields(&[("crop", json!({"left": 3}))])))
        .unwrap();
    ws.apply(set(1, fields(&[("translation", json!({"y": -7}))])))
        .unwrap();
    ws.apply(set(1, fields(&[("scale", json!({"x": 2}))])))
        .unwrap();
    let g = geometry(&ws, 1);
    assert_eq!((g.crop.top, g.crop.left, g.crop.right), (4.0, 3.0, 0.0));
    assert_eq!(g.translation, Pair { x: 0.0, y: -7.0 });
    assert_eq!(g.scale, Pair { x: 2.0, y: 1.0 });
    let before = ws.clone();
    refused_value(
        ws.apply(set(1, fields(&[("crop", json!({"left": -1}))]))),
        "crop.left",
    );
    assert_eq!(ws, before);
    ws.apply(undo_left).unwrap();
    let g = geometry(&ws, 1);
    assert_eq!((g.crop.top, g.crop.left), (4.0, 0.0));
}

fn timing(ws: &Workspace, n: u128) -> VideoTiming {
    match &ws.find_cue(cue_id(n)).unwrap().kind {
        CueKind::Video { timing, .. } => (**timing).clone(),
        other => panic!("{other:?}"),
    }
}

/// A Video cue's trims, play count, slices, fades and hold at end (VID-07) are fields of its
/// kind under the Audio cue's names: each is set on its own, undone and redone.
#[test]
fn time_and_loops_fields_are_set_undone_and_redone() {
    use serde_json::json;
    let mut ws = ws();
    let undo = ws
        .apply(set(
            1,
            fields(&[
                ("start_time", json!(1.0)),
                ("end_time", json!(3.0)),
                ("last_slice_play_count", json!(0)),
                ("slices", json!([{"time": 2.0, "play_count": 2}])),
                ("fade_in", json!({"duration": 0.5, "curve": "s_curve"})),
                ("hold_at_end", json!(true)),
            ]),
        ))
        .unwrap();
    let t = timing(&ws, 1);
    assert_eq!((t.start_time, t.end_time), (1.0, Some(3.0)));
    assert_eq!(t.last_slice_play_count, 0);
    assert_eq!(
        t.slices,
        vec![SliceMarker {
            time: 2.0,
            play_count: 2
        }]
    );
    assert_eq!(t.fade_in.curve, FadeCurve::SCurve);
    assert!(t.hold_at_end);
    let redo = ws.apply(undo).unwrap();
    assert_eq!(timing(&ws, 1), VideoTiming::default());
    ws.apply(redo).unwrap();
    assert_eq!(timing(&ws, 1), t);
}

/// Trims, fades and slices out of range are refused on every edit path as an Audio cue's are,
/// and nothing changes.
#[test]
fn time_and_loops_out_of_range_are_refused() {
    use serde_json::json;
    let mut ws = ws();
    let before = ws.clone();
    let refused = |result: Result<Inverse, CommandError>, want: IssueKind| match result {
        Err(CommandError::InvalidCue(issues)) => {
            assert!(issues.iter().any(|i| i.kind == want), "{issues:?}");
        }
        other => panic!("{want:?}: {other:?}"),
    };
    let edit =
        |ws: &mut Workspace, pairs: &[(&str, serde_json::Value)]| ws.apply(set(1, fields(pairs)));
    refused(
        edit(&mut ws, &[("start_time", json!(-1.0))]),
        IssueKind::InvalidTime {
            field: "start_time",
        },
    );
    refused(
        edit(
            &mut ws,
            &[("start_time", json!(5.0)), ("end_time", json!(2.0))],
        ),
        IssueKind::InvalidEndTime,
    );
    refused(
        edit(
            &mut ws,
            &[("fade_out", json!({"duration": 1e9, "curve": "linear"}))],
        ),
        IssueKind::InvalidTime {
            field: "fade_out.duration",
        },
    );
    refused(
        edit(
            &mut ws,
            &[
                ("end_time", json!(4.0)),
                ("slices", json!([{"time": 6.0, "play_count": 1}])),
            ],
        ),
        IssueKind::SliceOutsideTrim { time: 6.0 },
    );
    assert!(matches!(
        edit(&mut ws, &[("hold_at_end", json!("yes"))]),
        Err(CommandError::InvalidValue(_))
    ));
    // A whole new kind (a paste, a kind change) is judged the same way.
    let mut bad = video();
    if let CueKind::Video { timing, .. } = &mut bad {
        timing.start_time = f64::INFINITY;
    }
    refused(
        ws.apply(set(3, CueProperty::Kind { value: bad })),
        IssueKind::InvalidTime {
            field: "start_time",
        },
    );
    assert_eq!(ws, before);
}

/// A Video cue with sound's patch, levels and audio track `edit` gives it.
fn sounding(n: u128, edit: impl Fn(&mut VideoSound)) -> Cue {
    let mut c = video_cue(n);
    if let CueKind::Video { sound, .. } = &mut c.kind {
        edit(sound);
    }
    c
}

fn sound_of(ws: &Workspace, n: u128) -> VideoSound {
    match &ws.find_cue(cue_id(n)).unwrap().kind {
        CueKind::Video { sound, .. } => (**sound).clone(),
        other => panic!("{other:?}"),
    }
}

fn sound_fields(value: serde_json::Value) -> CueProperty {
    let serde_json::Value::Object(value) = value else {
        panic!("an object");
    };
    CueProperty::KindFields { value }
}

/// A Video cue's sound (VID-06): its patch, levels and audio track are set, undone and saved
/// as an Audio cue's levels are, and a level outside what a cue can hold is refused on every
/// path that sets it (the fields, the whole kind, add, paste), changing nothing.
#[test]
fn a_video_cues_patch_levels_and_audio_track_are_edited_and_checked_on_every_path() {
    let mut ws = ws();
    let mut history = History::default();
    let levels = |master: f64, input: u32| {
        serde_json::json!({
            "master_db": master,
            "outputs": [{"db": 0.0, "mute": false}, {"db": -3.0, "mute": true}],
            "crosspoints": [{"input": input, "output": 0, "db": 0.0, "mute": false}],
            "gangs": []
        })
    };
    let set_sound = sound_fields(serde_json::json!({
        "patch": patch_id(1), "levels": levels(-6.0, 1), "audio_track": 2
    }));
    assert!(history.execute(&mut ws, set(1, set_sound)).unwrap());
    let stored = sound_of(&ws, 1);
    assert_eq!((stored.patch, stored.audio_track), (Some(patch_id(1)), 2));
    assert_eq!(stored.levels.master_db, -6.0);
    assert!(stored.levels.outputs[1].mute);
    // Saved and loaded with the show.
    let saved = save_show(&ws).unwrap();
    assert_eq!(sound_of(&load_show(&saved).unwrap().workspace, 1), stored);
    // "none" is a patch choice.
    let none = sound_fields(serde_json::json!({"patch": null}));
    assert!(history.execute(&mut ws, set(1, none)).unwrap());
    assert_eq!(sound_of(&ws, 1).patch, None);
    assert!(history.undo(&mut ws).unwrap());
    assert_eq!(sound_of(&ws, 1), stored);
    assert!(history.undo(&mut ws).unwrap());
    assert_eq!(sound_of(&ws, 1), VideoSound::default());

    // A level that is not a number a cue can hold, on every path.
    let before = ws.clone();
    let bad = || {
        sounding(20, |s| {
            s.levels.master_db = f64::NAN;
        })
    };
    let at = Position {
        list: list_id(1),
        parent: None,
        index: 0,
    };
    let refused = |r: Result<Inverse, CommandError>| {
        assert!(matches!(r, Err(CommandError::InvalidCue(_))), "{r:?}");
    };
    refused(ws.apply(Command::AddCue {
        at: at.clone(),
        cue: bad(),
        auto_number: false,
    }));
    refused(ws.apply(Command::Paste {
        at,
        cues: vec![cue(21, group(vec![bad()]))],
    }));
    refused(ws.apply(set(1, CueProperty::Kind { value: bad().kind })));
    let from_no_channel = sound_fields(serde_json::json!({"levels": levels(0.0, 64)}));
    assert!(ws.apply(set(1, from_no_channel)).is_err());
    let negative = sound_fields(serde_json::json!({"audio_track": -1}));
    assert!(ws.apply(set(1, negative)).is_err());
    assert_eq!(ws, before);
}

/// A Video cue's patch counts as a use of it: the patch cannot be removed while the cue names
/// it, and a cue pasted from a workspace with other patches takes this one's first ("none"
/// stays none).
#[test]
fn a_video_cues_patch_keeps_it_from_removal_and_a_paste_repairs_it() {
    let mut ws = workspace(vec![
        cue(1, CueKind::Memo),
        sounding(7, |s| s.patch = Some(patch_id(1))),
    ]);
    ws.stages = vec![Stage::new(stage_id(1), "Stage 1", 1920, 1080)];
    let removed = ws.clone().apply(Command::RemovePatch {
        id: patch_id(1),
        even_if_used: false,
    });
    assert!(
        matches!(removed, Err(CommandError::PatchInUse { cue, .. }) if cue == cue_id(7)),
        "{removed:?}"
    );
    let paths = MediaPaths {
        show_dir: None,
        style: PathStyle::Posix,
    };
    let mut next = 600;
    let Command::Paste { cues, .. } = (Command::PasteClipboard {
        at: Position {
            list: list_id(1),
            parent: None,
            index: 0,
        },
        cues: vec![
            sounding(30, |s| s.patch = Some(patch_id(55))),
            sounding(31, |s| s.patch = None),
        ],
        source_dir: None,
    })
    .prepare(&ws, &paths, &mut || {
        next += 1;
        cue_id(next)
    })
    .unwrap() else {
        panic!()
    };
    assert_eq!(cues[0].kind.sound_patch(), Some(patch_id(1)));
    assert_eq!(cues[1].kind.sound_patch(), None);
}

/// A lowered volume limit brings a Video cue's levels within it, as an Audio cue's.
#[test]
fn a_volume_limit_applies_to_a_video_cues_levels() {
    let mut ws = workspace(vec![sounding(1, |s| s.levels.master_db = 6.0)]);
    let mut limits = ws.settings.limits();
    limits.max_db = 0.0;
    let clamps = ws.limit_clamps(limits);
    assert_eq!(clamps.len(), 1, "{clamps:?}");
    for c in clamps {
        ws.apply(c).unwrap();
    }
    assert_eq!(sound_of(&ws, 1).levels.master_db, 0.0);
}
