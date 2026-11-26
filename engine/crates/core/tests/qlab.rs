// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

//! Reading QLab 5 workspace files. The refusals and the operator's choices need no QLab file
//! and run everywhere. The conversion tests read the QLab-saved workspaces in
//! `engine/fixtures/qlab/`, which are never committed (QLab may store identifiers of the Mac
//! that saved them): they are `#[ignore]`d, run with `--ignored` on a Mac that has them, and
//! fail when the folder is missing.

mod common;

use std::path::PathBuf;

use common::*;
use openlab_core::*;
use uuid::Uuid;

fn counter() -> impl FnMut() -> Uuid {
    let mut n = 0u128;
    move || {
        n += 1;
        Uuid::from_u128(n)
    }
}

fn read(bytes: &[u8]) -> Result<QlabImport, QlabError> {
    read_qlab(bytes, &mut counter(), &mut |f| {
        f.relative
            .as_ref()
            .map(|r| MediaRef::new(format!("/q/{r}")))
    })
}

// ---- Refusals: no QLab file needed ------------------------------------------------------------

/// A minimal binary plist: a dictionary of ASCII keys to ASCII values (enough to stand for a
/// keyed archive's top level).
fn bplist_dict(pairs: &[(&str, &str)]) -> Vec<u8> {
    let mut objects: Vec<Vec<u8>> = Vec::new();
    let ascii = |s: &str| {
        let mut o = vec![0x50 | s.len() as u8];
        o.extend_from_slice(s.as_bytes());
        o
    };
    let n = pairs.len();
    let mut dict = vec![0xD0 | n as u8];
    for i in 0..n {
        dict.push((1 + i) as u8);
    }
    for i in 0..n {
        dict.push((1 + n + i) as u8);
    }
    objects.push(dict);
    for (k, _) in pairs {
        objects.push(ascii(k));
    }
    for (_, v) in pairs {
        objects.push(ascii(v));
    }
    let mut out = b"bplist00".to_vec();
    let mut offsets = Vec::new();
    for o in &objects {
        offsets.push(out.len() as u8);
        out.extend_from_slice(o);
    }
    let table = out.len() as u64;
    out.extend_from_slice(&offsets);
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0, 1, 1]);
    out.extend_from_slice(&(objects.len() as u64).to_be_bytes());
    out.extend_from_slice(&0u64.to_be_bytes());
    out.extend_from_slice(&table.to_be_bytes());
    out
}

#[test]
fn garbage_truncated_and_oversized_input_is_a_typed_refusal() {
    assert_eq!(read(b"").unwrap_err(), QlabError::NotPlist);
    assert_eq!(
        read(b"not a plist at all, just text").unwrap_err(),
        QlabError::NotPlist
    );
    let plist = bplist_dict(&[("a", "b")]);
    // A property list that is not a keyed archive.
    assert_eq!(read(&plist).unwrap_err(), QlabError::NotKeyedArchive);
    // Every truncation of it is refused, never a panic.
    for len in 0..plist.len() {
        assert!(read(&plist[..len]).is_err(), "truncated at {len}");
    }
    // Every single-byte corruption is read or refused, never a panic.
    for i in 0..plist.len() {
        let mut bad = plist.clone();
        bad[i] ^= 0xff;
        let _ = read(&bad);
    }
    let huge = vec![0u8; MAX_QLAB_FILE_BYTES + 1];
    assert_eq!(read(&huge).unwrap_err(), QlabError::TooLarge("file"));
}

/// The fuzz seed: a small keyed archive shaped like a QLab workspace, written by a script, not
/// by QLab.
const SEED: &[u8] = include_bytes!("../../../fuzz/qlab-seeds/shaped-like-qlab.bin");

#[test]
fn every_truncation_and_corruption_of_a_workspace_archive_is_read_or_refused() {
    let import = read(SEED).unwrap();
    assert_eq!(import.workspace.cues().count(), 5);
    assert_eq!(import.report.unsupported.len(), 1);
    for len in 0..SEED.len() {
        assert!(read(&SEED[..len]).is_err(), "truncated at {len}");
    }
    for i in 0..SEED.len() {
        for flip in [0x01, 0xff] {
            let mut bad = SEED.to_vec();
            bad[i] ^= flip;
            if let Ok(import) = read(&bad) {
                let (ws, _) = import.finish(&[], QlabCueAction::Placeholder, list_id(99));
                load_show(&save_show(&ws).unwrap()).unwrap();
            }
        }
    }
}

#[test]
fn a_plist_whose_counts_point_outside_the_file_is_refused() {
    let mut plist = bplist_dict(&[("a", "b")]);
    let n = plist.len();
    // Object count far beyond the file.
    plist[n - 24..n - 16].copy_from_slice(&u64::MAX.to_be_bytes());
    assert_eq!(
        read(&plist).unwrap_err(),
        QlabError::Damaged("object count")
    );
}

#[test]
fn qlab_errors_say_which_refusal_it_is() {
    // The engine turns these into distinct protocol codes; the messages name the version.
    assert!(QlabError::TooOld("4.6".into()).to_string().contains("4.6"));
    assert!(QlabError::TooNew("6.0".into()).to_string().contains("6.0"));
}

// ---- The operator's choices: on an openLab workspace -----------------------------------------

fn placeholder(qlab: &str, n: u128) -> QlabPlaceholder {
    QlabPlaceholder {
        qlab_id: qlab.into(),
        at: Placeholder::Cue(cue_id(n)),
    }
}

fn choice(qlab: &str, action: QlabCueAction) -> QlabCueChoice {
    QlabCueChoice {
        qlab_id: qlab.into(),
        action,
    }
}

#[test]
fn delete_removes_the_placeholder_and_placeholder_keeps_it() {
    let mut ws = workspace(vec![cue(1, CueKind::Memo), cue(2, CueKind::Memo)]);
    let places = [placeholder("L1", 1), placeholder("L2", 2)];
    // L2 is not named: it stays a placeholder.
    let removed = apply_qlab_choices(
        &mut ws,
        &places,
        &[choice("L1", QlabCueAction::Delete)],
        QlabCueAction::Placeholder,
        list_id(99),
    );
    assert_eq!(removed, vec![cue_id(1)]);
    assert!(ws.find_cue(cue_id(1)).is_none());
    assert!(ws.find_cue(cue_id(2)).is_some());
    let removed = apply_qlab_choices(
        &mut ws,
        &places,
        &[choice("L2", QlabCueAction::Placeholder)],
        QlabCueAction::Delete,
        list_id(99),
    );
    assert!(removed.is_empty());
    assert!(ws.find_cue(cue_id(2)).is_some());
    // Delete all: the cues not named go too.
    let mut ws = workspace(vec![cue(1, CueKind::Memo), cue(2, CueKind::Memo)]);
    let removed = apply_qlab_choices(&mut ws, &places, &[], QlabCueAction::Delete, list_id(99));
    assert_eq!(removed, vec![cue_id(1), cue_id(2)]);
}

#[test]
fn deleting_a_groups_child_leaves_the_group_and_targets_to_it_become_none() {
    let group = cue(
        10,
        CueKind::Group {
            mode: GroupMode::Timeline,
            children: vec![cue(11, CueKind::Memo), cue(12, CueKind::Memo)],
        },
    );
    let start = cue(
        20,
        CueKind::Start {
            target: Some(cue_id(11)),
        },
    );
    let mut ws = workspace(vec![group, start]);
    apply_qlab_choices(
        &mut ws,
        &[placeholder("M", 11)],
        &[choice("M", QlabCueAction::Delete)],
        QlabCueAction::Placeholder,
        list_id(99),
    );
    assert_eq!(ws.find_cue(cue_id(10)).unwrap().children().len(), 1);
    assert_eq!(ws.find_cue(cue_id(20)).unwrap().kind.target(), None);
}

#[test]
fn deleting_a_cart_removes_its_list_and_keeps_one_list() {
    let mut ws = workspace(vec![cue(1, CueKind::Memo)]);
    let cart = ws.cue_lists[0].id;
    apply_qlab_choices(
        &mut ws,
        &[QlabPlaceholder {
            qlab_id: "C".into(),
            at: Placeholder::List(cart),
        }],
        &[choice("C", QlabCueAction::Delete)],
        QlabCueAction::Placeholder,
        list_id(99),
    );
    assert_eq!(ws.cue_lists.len(), 1);
    assert_eq!(ws.cue_lists[0].id, list_id(99));
    assert!(ws.cue_lists[0].cues.is_empty());
}

// ---- Conversion of QLab-saved workspaces (local only) ----------------------------------------

const LOCAL: &str = "needs the local QLab-saved fixtures in engine/fixtures/qlab";

/// A QLab-saved fixture, converted with every unsupported cue left a placeholder. Fails when
/// the fixtures are not on this machine.
fn fixture(name: &str) -> QlabImport {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/qlab")
        .join(name)
        .join(format!("{name}.qlab5"));
    let bytes =
        std::fs::read(&path).unwrap_or_else(|e| panic!("{LOCAL}: {} ({e})", path.display()));
    read(&bytes).unwrap()
}

fn by_number<'a>(ws: &'a Workspace, number: &str) -> &'a Cue {
    ws.cues()
        .find(|c| c.number.as_deref() == Some(number))
        .unwrap_or_else(|| panic!("no cue {number}"))
}

#[test]
#[ignore = "needs the local QLab-saved fixtures in engine/fixtures/qlab"]
fn every_cue_type_converts_and_unsupported_ones_become_placeholders() {
    let import = fixture("cue-types");
    let ws = &import.workspace;
    let r = &import.report;
    assert_eq!(r.workspace_name, "cue-types");
    assert_eq!(ws.name, "cue-types");
    assert_eq!(r.qlab_version, "5.6.5");
    assert_eq!(ws.cue_lists.len(), 2);
    assert_eq!(ws.cue_lists[1].name, "Second List");

    let tone = by_number(ws, "1");
    assert_eq!(tone.name, "Tone");
    assert_eq!(tone.notes, "Plays the mono tone.");
    assert_eq!(tone.color, CueColor::Red);
    assert!(tone.flagged);
    assert_eq!((tone.pre_wait, tone.post_wait), (0.5, 1.25));
    assert_eq!(tone.continue_mode, ContinueMode::AutoContinue);
    let CueKind::Audio { media, patch, .. } = &tone.kind else {
        panic!("{tone:?}")
    };
    assert_eq!(media.as_ref().unwrap().as_str(), "/q/audio/tone-mono.wav");
    // On the second patch, as QLab had it.
    assert_eq!(*patch, ws.patches[1].id);

    let target = |n: &str| by_number(ws, n).kind.target();
    let CueKind::Fade {
        duration, levels, ..
    } = &by_number(ws, "2").kind
    else {
        panic!()
    };
    assert_eq!(*duration, 3.0);
    assert_eq!(levels.master_db, Some(-10.0));
    assert_eq!(by_number(ws, "2").continue_mode, ContinueMode::AutoFollow);
    for n in ["2", "3", "4", "5", "6", "7", "8"] {
        assert_eq!(target(n), Some(tone.id), "cue {n}");
    }
    assert!(matches!(by_number(ws, "3").kind, CueKind::Start { .. }));
    assert_eq!(by_number(ws, "3").color, CueColor::Green);
    let stop = by_number(ws, "4");
    assert!(matches!(stop.kind, CueKind::Stop { .. }));
    assert!(!stop.armed && stop.skip_if_disarmed);
    assert!(by_number(ws, "5").auto_load);
    assert!(matches!(by_number(ws, "5").kind, CueKind::Pause { .. }));
    assert!(
        matches!(by_number(ws, "6").kind, CueKind::Load { load_to_time, .. } if load_to_time == 0.75)
    );
    assert!(matches!(by_number(ws, "7").kind, CueKind::Reset { .. }));
    assert!(matches!(
        by_number(ws, "8").kind,
        CueKind::Devamp {
            start_next_cue: true,
            stop_target_when_slice_ends: true,
            devamp_mode: DevampMode::LoopingCue,
            ..
        }
    ));
    let start = by_number(ws, "3").id;
    assert!(matches!(by_number(ws, "9").kind, CueKind::GoTo { target: Some(t), .. } if t == start));
    assert!(matches!(by_number(ws, "10").kind, CueKind::Arm { target: Some(t) } if t == stop.id));
    assert!(
        matches!(by_number(ws, "11").kind, CueKind::Disarm { target: Some(t) } if t == stop.id)
    );
    assert!(
        matches!(by_number(ws, "12").kind, CueKind::Target { target: Some(t), .. } if t == start)
    );
    assert!(matches!(by_number(ws, "13").kind, CueKind::Wait { duration } if duration == 2.5));
    assert_eq!(by_number(ws, "14").notes, "Line one\nLine two");
    let CueKind::Network {
        destination,
        address,
        args,
    } = &by_number(ws, "15").kind
    else {
        panic!()
    };
    assert_eq!(address, "/cue/1/start");
    assert!(args.is_empty());
    assert_eq!(*destination, ws.network_patches[1].id);
    let CueKind::Video {
        media,
        stage,
        duration,
        geometry,
        ..
    } = &by_number(ws, "16").kind
    else {
        panic!()
    };
    assert_eq!(media.as_ref().unwrap().as_str(), "/q/video/solid.png");
    assert_eq!(*stage, Some(ws.stages[0].id));
    assert_eq!(*duration, Some(4.0));
    assert_eq!(geometry.opacity, 0.5);
    assert_eq!(geometry.layer, 3);
    assert_eq!(geometry.fill_style, FillStyle::Fill);
    assert_eq!(
        (geometry.translation.x, geometry.translation.y),
        (10.0, -20.0)
    );
    assert_eq!((geometry.scale.x, geometry.scale.y), (0.5, 0.25));
    assert!(!geometry.scale_locked);
    assert_eq!(
        (
            geometry.crop.top,
            geometry.crop.bottom,
            geometry.crop.left,
            geometry.crop.right
        ),
        (1.0, 2.0, 3.0, 4.0)
    );
    assert_eq!(by_number(ws, "17").children().len(), 1);

    // Light, MIDI, MIDI File, Timecode, Script, Camera, Mic, Text and an unnamed Mic.
    let unsupported: Vec<&str> = r.unsupported.iter().map(|u| u.qlab_type.as_str()).collect();
    assert_eq!(
        unsupported,
        [
            "LightCue",
            "MIDICue",
            "MIDIFileCue",
            "TimecodeCue",
            "ScriptCue",
            "CameraCue",
            "MicCue",
            "TitlesCue",
            "MicCue"
        ]
    );
    assert!(
        r.unsupported
            .iter()
            .all(|u| u.reason == QlabUnsupportedReason::CueType)
    );
    let light = by_number(ws, "20");
    assert!(matches!(light.kind, CueKind::Memo));
    assert_eq!(light.color, CueColor::Red);
    assert_eq!(light.name, "Unsupported light");
    assert_eq!(by_number(ws, "30").name, "QLab Mic cue (not converted)");

    // Two audio patches (the second on a device this Mac lacks) and two network patches.
    let p = &r.audio_patches;
    assert_eq!(p.len(), 2);
    assert_eq!((p[1].name.as_str(), p[1].outputs), ("Patch 2", 4));
    assert_eq!(p[1].device_uid, "OpenLabFixtureDevice_UID");
    assert_eq!(
        (p[1].device_name.as_str(), p[1].device_outputs),
        ("Fixture Interface", 8)
    );
    assert!(p[0].system && !p[1].system);
    assert_eq!(ws.patches[1].outputs.len(), 4);
    assert!(
        ws.patches[0].crosspoints.is_empty(),
        "QLab's default routing"
    );
    let net = &ws.network_patches[1];
    assert_eq!(
        (net.name.as_str(), net.host.as_str(), net.port),
        ("Lighting desk", "10.0.0.5", 9000)
    );
    assert_eq!(net.protocol, NetworkProtocol::Tcp);
    assert_eq!(r.video_outputs.len(), 1);
    assert_eq!(
        (r.video_outputs[0].width, r.video_outputs[0].height),
        (1920, 1080)
    );
    assert_eq!(ws.routes.len(), 1);
    assert_eq!(ws.stages[0].regions[0].route, Some(ws.routes[0].id));

    // Delete the Light cue, keep the rest: the converted workspace loads without errors.
    let light_id = r.unsupported[0].qlab_id.clone();
    let (ws, warnings) = import.finish(
        &[choice(&light_id, QlabCueAction::Delete)],
        QlabCueAction::Placeholder,
        CueListId(Uuid::from_u128(9999)),
    );
    assert!(ws.cues().all(|c| c.number.as_deref() != Some("20")));
    assert!(warnings.iter().all(|w| w.severity == Severity::Warning));
    load_show(&save_show(&ws).unwrap()).unwrap();
}

#[test]
#[ignore = "needs the local QLab-saved fixtures in engine/fixtures/qlab"]
fn audio_levels_crosspoints_gangs_trims_slices_and_loops_map_to_db() {
    let import = fixture("audio-levels");
    let ws = &import.workspace;
    let CueKind::Audio { levels, .. } = &by_number(ws, "1").kind else {
        panic!()
    };
    assert_eq!(levels.master_db, -6.0);
    assert_eq!(levels.outputs[0].db, -3.0);
    // QLab stores no cell for -inf.
    assert_eq!(levels.outputs[1].db, SILENCE_DB);
    // A trim of 0.5 (about -6 dB) folds into its output's level.
    assert_eq!(levels.outputs[3].db, -6.021);
    // Input 1's level (-1.5 dB) folds into its crosspoints.
    let xp = |i: u32, o: u32| {
        levels
            .crosspoints
            .iter()
            .find(|c| c.input == i && c.output == o)
            .map(|c| c.db)
    };
    assert_eq!(xp(0, 0), Some(-1.5));
    assert_eq!(xp(0, 1), Some(-13.5));
    assert_eq!(xp(1, 0), None);
    assert_eq!(xp(1, 1), Some(0.0));
    assert_eq!(levels.gangs, vec![vec![0, 2]]);
    let kinds: Vec<QlabNoteKind> = import
        .report
        .notes
        .iter()
        .filter(|n| n.number == "1")
        .map(|n| n.kind)
        .collect();
    assert!(kinds.contains(&QlabNoteKind::LevelFolded));

    let CueKind::Audio {
        start_time,
        end_time,
        slices,
        last_slice_play_count,
        levels,
        ..
    } = &by_number(ws, "2").kind
    else {
        panic!()
    };
    assert_eq!((*start_time, *end_time), (0.1, Some(0.9)));
    assert_eq!(
        slices,
        &[
            SliceMarker {
                time: 0.3,
                play_count: PlayCount::from_stored(2)
            },
            SliceMarker {
                time: 0.6,
                play_count: PlayCount::ONCE
            },
        ]
    );
    assert_eq!(*last_slice_play_count, PlayCount::from_stored(3));
    assert_eq!(
        levels
            .crosspoints
            .iter()
            .find(|c| c.input == 1 && c.output == 1)
            .unwrap()
            .db,
        -4.0
    );
    // A loop of the whole cue, without slices, loops its one slice; rate is reported.
    assert!(matches!(
        by_number(ws, "3").kind,
        CueKind::Audio {
            last_slice_play_count: PlayCount::Endless,
            end_time: None,
            ..
        }
    ));
    assert!(
        import
            .report
            .notes
            .iter()
            .any(|n| n.number == "3" && n.kind == QlabNoteKind::Rate)
    );
    assert!(matches!(
        by_number(ws, "4").kind,
        CueKind::Audio { last_slice_play_count: c, .. } if c == PlayCount::from_stored(2)
    ));
    assert!(matches!(
        by_number(ws, "5").kind,
        CueKind::Audio {
            last_slice_play_count: PlayCount::Endless,
            ..
        }
    ));
    assert!(matches!(
        by_number(ws, "6").kind,
        CueKind::Audio { media: None, .. }
    ));
}

#[test]
#[ignore = "needs the local QLab-saved fixtures in engine/fixtures/qlab"]
fn group_modes_continue_modes_and_triggers_convert() {
    let import = fixture("groups-triggers");
    let ws = &import.workspace;
    let mode = |n: &str| match &by_number(ws, n).kind {
        CueKind::Group { mode, .. } => *mode,
        other => panic!("{other:?}"),
    };
    assert_eq!(mode("G1"), GroupMode::StartFirstAndEnter);
    assert_eq!(mode("G2"), GroupMode::StartFirstAndGoToNext);
    assert_eq!(mode("G3"), GroupMode::Timeline);
    assert_eq!(mode("G4"), GroupMode::StartRandom);
    assert_eq!(
        mode("G6"),
        GroupMode::Playlist {
            loop_: true,
            shuffle: true
        }
    );
    assert_eq!(by_number(ws, "G3").children().len(), 3);
    assert_eq!(
        by_number(ws, "G3.3").children()[0].number.as_deref(),
        Some("G3.3.1")
    );
    assert_eq!(by_number(ws, "G6").second_trigger, SecondTrigger::PlayNext);
    assert!(
        import
            .report
            .notes
            .iter()
            .any(|n| n.number == "G6" && n.kind == QlabNoteKind::Crossfade)
    );

    assert_eq!(
        by_number(ws, "C0").continue_mode,
        ContinueMode::DoNotContinue
    );
    assert_eq!(
        by_number(ws, "C1").continue_mode,
        ContinueMode::AutoContinue
    );
    assert_eq!(by_number(ws, "C1").post_wait, 2.0);
    assert_eq!(by_number(ws, "C2").continue_mode, ContinueMode::AutoFollow);
    let seconds = [
        SecondTrigger::Nothing,
        SecondTrigger::Panic,
        SecondTrigger::Stop,
        SecondTrigger::HardStop,
        SecondTrigger::HardStopAndRestart,
        SecondTrigger::Devamp,
    ];
    for (n, want) in seconds.iter().enumerate() {
        assert_eq!(by_number(ws, &format!("T{n}")).second_trigger, *want);
    }
    assert!(by_number(ws, "T3").second_trigger_on_release);
    let duck = by_number(ws, "D1").duck_others;
    assert!(duck.enabled);
    assert_eq!((duck.db, duck.duration), (-8.0, 2.0));
    assert_eq!(by_number(ws, "D2").duck_others.db, 6.0);
    let fso = |n: &str| by_number(ws, n).fade_and_stop_others;
    assert_eq!(
        (fso("F1").enabled, fso("F1").scope),
        (true, OthersScope::Peers)
    );
    assert_eq!(
        (fso("F2").scope, fso("F2").duration),
        (OthersScope::List, 2.0)
    );
    assert_eq!(fso("F3").scope, OthersScope::All);
    assert!(!fso("C0").enabled);

    // The twenty colours, by QLab's names, in order.
    let names = [
        "berry", "blue", "crimson", "cyan", "forest", "gray", "green", "hot pink", "indigo",
        "lavender", "magenta", "midnight", "olive", "orange", "peach", "plum", "purple", "red",
        "sky blue", "yellow",
    ];
    let colours = [
        CueColor::Raspberry,
        CueColor::Blue,
        CueColor::DarkRed,
        CueColor::Cyan,
        CueColor::DarkGreen,
        CueColor::Grey,
        CueColor::Green,
        CueColor::Pink,
        CueColor::Indigo,
        CueColor::Lilac,
        CueColor::Magenta,
        CueColor::Navy,
        CueColor::Olive,
        CueColor::Orange,
        CueColor::Apricot,
        CueColor::Plum,
        CueColor::Purple,
        CueColor::Red,
        CueColor::LightBlue,
        CueColor::Yellow,
    ];
    for (n, (name, colour)) in names.iter().zip(colours).enumerate() {
        let cue = by_number(ws, &format!("K{n}"));
        assert_eq!(cue.name, *name);
        assert_eq!(cue.color, colour, "{name}");
    }
    assert_eq!(by_number(ws, "K20").second_color, CueColor::Red);

    let key = |n: &str| by_number(ws, n).triggers.clone();
    assert_eq!(key("H0").hotkey.unwrap().key, "a");
    let h1 = key("H1").hotkey.unwrap();
    assert_eq!(h1.key, "1");
    assert!(h1.modifiers.shift && h1.modifiers.command && !h1.modifiers.alt);
    assert_eq!(key("H2").hotkey.unwrap().key, "f5");
    // A key set but switched off is kept and does not start the cue.
    let h3 = key("H3");
    assert_eq!(h3.hotkey.as_ref().unwrap().key, "space");
    assert!(
        h3.hotkey.as_ref().unwrap().modifiers.alt && h3.hotkey.as_ref().unwrap().modifiers.control
    );
    assert!(!h3.hotkey_enabled);
    let wall = key("W1").wall_clock;
    assert!(wall.enabled);
    assert_eq!((wall.hours, wall.minutes, wall.seconds), (19, 30, 15));
    let days: Vec<Weekday> = wall.days.into();
    assert_eq!(days, [Weekday::Monday, Weekday::Wednesday, Weekday::Friday]);

    let s = &ws.settings;
    assert_eq!((s.panic_duration, s.min_go_interval), (2.5, 0.5));
    assert_eq!(s.auto_number_increment, 10.0);
    assert!(!s.auto_number_new_cues && !s.go_needs_key_up && !s.lock_playhead);
    assert!(s.new_cues_auto_load && !s.copy_media_on_add);
    let go = s.hotkeys.go.as_ref().unwrap();
    assert_eq!(go.key, "g");
    assert!(go.modifiers.shift);
    assert_eq!(s.hotkeys.pause_all.as_ref().unwrap().key, "[");
    assert_eq!(s.hotkeys.stop_all, None);
}

#[test]
#[ignore = "needs the local QLab-saved fixtures in engine/fixtures/qlab"]
fn fade_levels_modes_curves_and_domains_convert() {
    let import = fixture("fades");
    let ws = &import.workspace;
    let tone = by_number(ws, "1").id;
    let fade = |n: &str| match &by_number(ws, n).kind {
        CueKind::Fade {
            target,
            duration,
            curve,
            levels,
            stop_target_when_done,
            mode,
            geometry,
        } => {
            assert_eq!(
                *target,
                Some(by_number(ws, if n == "F20" { "2" } else { "1" }).id)
            );
            (
                *duration,
                *curve,
                levels.clone(),
                *stop_target_when_done,
                *mode,
                *geometry,
            )
        }
        other => panic!("{other:?}"),
    };
    let _ = tone;
    let (duration, curve, levels, stop, mode, _) = fade("F1");
    assert_eq!(
        (duration, curve, stop, mode),
        (2.0, FadeCurve::SCurve, false, FadeMode::Absolute)
    );
    assert_eq!(levels.master_db, Some(-20.0));
    let (_, _, levels, _, mode, _) = fade("F2");
    assert_eq!((mode, levels.master_db), (FadeMode::Relative, Some(-6.0)));
    let (_, _, levels, stop, _, _) = fade("F3");
    assert!(stop);
    assert_eq!(levels.master_db, None);
    let (_, _, levels, _, _, _) = fade("F4");
    assert_eq!(levels.master_db, None);
    assert_eq!(levels.output_db, vec![Some(-10.0)]);
    assert_eq!(
        levels.crosspoints,
        vec![
            FadeCrosspoint {
                input: 0,
                output: 0,
                db: -3.0
            },
            FadeCrosspoint {
                input: 1,
                output: 1,
                db: 0.0
            },
        ]
    );
    // Shape types 0 custom, 1 S-curve, 2 parametric, 4 path; audio domains 0 slider,
    // 2 linear, 1 decibel; a custom curve with a control point.
    assert_eq!(fade("F5").1, FadeCurve::SCurve);
    assert_eq!(fade("F6").1, FadeCurve::SCurve);
    assert_eq!(fade("F7").1, FadeCurve::EqualPower);
    assert_eq!(fade("F8").1, FadeCurve::Linear);
    assert_eq!(fade("F11").1, FadeCurve::SCurve);
    let notes = |n: &str| {
        import
            .report
            .notes
            .iter()
            .filter(|x| x.number == n && x.kind == QlabNoteKind::FadeCurve)
            .count()
    };
    assert_eq!(
        (notes("F5"), notes("F6"), notes("F8"), notes("F9")),
        (1, 0, 1, 0)
    );
    assert_eq!((notes("F10"), notes("F12")), (1, 1));
    let (_, _, _, _, _, geometry) = fade("F20");
    assert!(geometry.fade_opacity && geometry.fade_translation && !geometry.fade_scale);
    assert_eq!(geometry.opacity, 0.25);
    assert_eq!(
        (geometry.translation.x, geometry.translation.y),
        (100.0, 50.0)
    );
}
