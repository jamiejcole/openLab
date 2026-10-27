// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Show-file format: round trips, fixtures, migration, refusals and hostile input
//! (NFR-03, NFR-32, NFR-40, QA-04).

mod common;

use common::*;
use openlab_core::*;
use proptest::prelude::*;
use proptest::{collection::vec, option};

/// The current format (golden: saved files must match it byte for byte).
const EXAMPLE: &str = include_str!("../../../fixtures/show/v3-example.openlab");
const V2_EXAMPLE: &str = include_str!("../../../fixtures/show/v2-example.openlab");
const V1_EXAMPLE: &str = include_str!("../../../fixtures/show/v1-example.openlab");
const V0_EXAMPLE: &str = include_str!("../../../fixtures/show/v0-example.openlab");

fn kind_name(kind: &CueKind) -> &'static str {
    match kind {
        CueKind::Audio { .. } => "audio",
        CueKind::Fade { .. } => "fade",
        CueKind::Group { .. } => "group",
        CueKind::Start { .. } => "start",
        CueKind::Stop { .. } => "stop",
        CueKind::Pause { .. } => "pause",
        CueKind::Load { .. } => "load",
        CueKind::Reset { .. } => "reset",
        CueKind::Devamp { .. } => "devamp",
        CueKind::GoTo { .. } => "go_to",
        CueKind::Arm { .. } => "arm",
        CueKind::Disarm { .. } => "disarm",
        CueKind::Wait { .. } => "wait",
        CueKind::Memo => "memo",
        CueKind::Network { .. } => "network",
    }
}

fn walk<'a>(cues: &'a [Cue], f: &mut impl FnMut(&'a Cue)) {
    for cue in cues {
        f(cue);
        walk(cue.children(), f);
    }
}

#[test]
fn golden_fixture_loads_cleanly_and_covers_every_cue_kind_and_group_mode() {
    let loaded = load_show(EXAMPLE).unwrap();
    assert_eq!(loaded.warnings, vec![]);
    let mut kinds = std::collections::BTreeSet::new();
    let mut modes = Vec::new();
    for list in &loaded.workspace.cue_lists {
        walk(&list.cues, &mut |cue| {
            kinds.insert(kind_name(&cue.kind));
            if let CueKind::Group { mode, .. } = &cue.kind {
                modes.push(std::mem::discriminant(mode));
            }
        });
    }
    assert_eq!(kinds.len(), 15, "{kinds:?}");
    modes.sort_by_key(|d| format!("{d:?}"));
    modes.dedup();
    assert_eq!(modes.len(), 5);
}

#[test]
fn golden_fixture_is_in_canonical_form() {
    let loaded = load_show(EXAMPLE).unwrap();
    assert_eq!(save_show(&loaded.workspace).unwrap(), EXAMPLE);
}

#[test]
fn v0_fixture_migrates_to_current() {
    let loaded = load_show(V0_EXAMPLE).unwrap();
    assert_eq!(loaded.warnings, vec![]);
    let ws = loaded.workspace;
    assert_eq!(ws.name, "Old Show");
    assert_eq!(ws.cue_lists.len(), 1);
    assert_eq!(ws.cue_lists[0].name, "Main");
    assert_eq!(ws.cue_lists[0].cues[0].name, "Welcome");
    assert!(
        save_show(&ws)
            .unwrap()
            .starts_with(&format!("{{\n  \"format_version\": {FORMAT_VERSION},"))
    );
}

fn cue_named<'a>(ws: &'a Workspace, name: &str) -> &'a Cue {
    let mut found = None;
    for list in &ws.cue_lists {
        walk(&list.cues, &mut |cue| {
            if cue.name == name {
                found = Some(cue);
            }
        });
    }
    found.unwrap()
}

/// v1 had no second-trigger settings (M2.14): every cue, at every depth, gets the defaults, and
/// apart from them the show is the current example's.
#[test]
fn v1_fixture_migrates_with_default_second_triggers() {
    let loaded = load_show(V1_EXAMPLE).unwrap();
    assert_eq!(loaded.warnings, vec![]);
    let mut ws = loaded.workspace;
    let mut count = 0;
    for list in &ws.cue_lists {
        walk(&list.cues, &mut |cue| {
            assert_eq!(cue.second_trigger, SecondTrigger::Nothing, "{}", cue.name);
            assert!(!cue.second_trigger_on_release, "{}", cue.name);
            count += 1;
        });
    }
    assert!(count > 20);
    // A child inside a group was reached too.
    assert_eq!(
        cue_named(&ws, "Thunder").second_trigger,
        SecondTrigger::Nothing
    );
    // The current example differs only in the settings it sets (and the v3 Devamp mode).
    let current = load_show(EXAMPLE).unwrap().workspace;
    for list in &mut ws.cue_lists {
        for cue in &mut list.cues {
            let other = cue_named(&current, &cue.name);
            cue.second_trigger = other.second_trigger;
            cue.second_trigger_on_release = other.second_trigger_on_release;
        }
    }
    set_devamp_modes(&mut ws, DevampMode::LoopingCue);
    assert_eq!(ws, current);
}

fn set_devamp_modes(ws: &mut Workspace, mode: DevampMode) {
    fn walk_mut(cues: &mut [Cue], mode: DevampMode) {
        for cue in cues {
            match &mut cue.kind {
                CueKind::Devamp { devamp_mode, .. } => *devamp_mode = mode,
                CueKind::Group { children, .. } => walk_mut(children, mode),
                _ => {}
            }
        }
    }
    for list in &mut ws.cue_lists {
        walk_mut(&mut list.cues, mode);
    }
}

fn devamp_modes(ws: &Workspace) -> Vec<DevampMode> {
    let mut out = Vec::new();
    for list in &ws.cue_lists {
        walk(&list.cues, &mut |cue| {
            if let CueKind::Devamp { devamp_mode, .. } = cue.kind {
                out.push(devamp_mode);
            }
        });
    }
    out
}

/// v2 had one Devamp behaviour, leaving whatever loop was playing: every Devamp cue gets the
/// mode `current_slice` (M2.28, QLab check D7), and apart from it the show is the current
/// example's.
#[test]
fn v2_fixture_migrates_with_devamp_mode_current_slice() {
    let loaded = load_show(V2_EXAMPLE).unwrap();
    assert_eq!(loaded.warnings, vec![]);
    let mut ws = loaded.workspace;
    assert_eq!(devamp_modes(&ws), vec![DevampMode::CurrentSlice]);
    let current = load_show(EXAMPLE).unwrap().workspace;
    assert_eq!(devamp_modes(&current), vec![DevampMode::LoopingCue]);
    set_devamp_modes(&mut ws, DevampMode::LoopingCue);
    assert_eq!(ws, current);
}

/// A Devamp cue nested in a group is reached, a `devamp_mode` a hand edit put in a v2 file
/// (which meant nothing then) is replaced, and anything not shaped like a cue is left to the
/// model parse.
#[test]
fn v2_migration_sets_the_mode_of_nested_and_hand_edited_devamp_cues() {
    let mut doc: serde_json::Value = serde_json::from_str(V2_EXAMPLE).unwrap();
    let mut nested = doc["workspace"]["cue_lists"][0]["cues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["kind"]["type"] == "devamp")
        .unwrap()
        .clone();
    nested["id"] = "00000000-0000-0000-0000-0000000000f1".into();
    nested["number"] = serde_json::Value::Null;
    nested["kind"]["devamp_mode"] = "looping_cue".into();
    let group = doc["workspace"]["cue_lists"][0]["cues"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|c| c["kind"]["type"] == "group")
        .unwrap();
    group["kind"]["children"]
        .as_array_mut()
        .unwrap()
        .push(nested);
    let loaded = load_show(&doc.to_string()).unwrap();
    assert_eq!(
        devamp_modes(&loaded.workspace),
        vec![DevampMode::CurrentSlice, DevampMode::CurrentSlice]
    );

    let not_a_cue = r#"{"format_version": 2, "workspace": {"cue_lists": [{"cues": [7, {"kind": {"type": "devamp", "children": [null]}}]}]}}"#;
    assert!(matches!(load_show(not_a_cue), Err(LoadError::Model(_))));
    let no_workspace = r#"{"format_version": 2, "workspace": 3}"#;
    assert!(matches!(
        load_show(no_workspace),
        Err(LoadError::Migration { from: 2, .. })
    ));
}

#[test]
fn devamp_mode_values_are_snake_case_and_required() {
    for (name, value) in [
        ("current_slice", DevampMode::CurrentSlice),
        ("looping_cue", DevampMode::LoopingCue),
    ] {
        assert_eq!(
            serde_json::to_string(&value).unwrap(),
            format!("\"{name}\"")
        );
    }
    let text = EXAMPLE.replacen("\"looping_cue\"", "\"looping_slice\"", 1);
    assert!(matches!(load_show(&text), Err(LoadError::Model(_))));
    // Missing keys are an error in the current version.
    let text = EXAMPLE.replacen(",\n              \"devamp_mode\": \"looping_cue\"", "", 1);
    assert_ne!(text, EXAMPLE);
    assert!(matches!(load_show(&text), Err(LoadError::Model(_))));
}

#[test]
fn golden_fixture_covers_second_trigger_settings() {
    let ws = load_show(EXAMPLE).unwrap().workspace;
    assert_eq!(
        cue_named(&ws, "Preshow music").second_trigger,
        SecondTrigger::Panic
    );
    assert_eq!(
        cue_named(&ws, "Walk-in playlist").second_trigger,
        SecondTrigger::PlayNext
    );
    let bell = cue_named(&ws, "Doorbell");
    assert_eq!(bell.second_trigger, SecondTrigger::HardStopAndRestart);
    assert!(bell.second_trigger_on_release);
}

/// A v1 reader ignored unknown keys, so second-trigger keys a hand edit put in a v1 file meant
/// nothing then and are replaced by the defaults; anything not shaped like a cue is left to the
/// model parse.
#[test]
fn v1_migration_overrides_hand_edited_second_trigger_keys() {
    let text = V1_EXAMPLE.replacen(
        "\"continue_mode\": \"do_not_continue\",",
        "\"continue_mode\": \"do_not_continue\", \"second_trigger\": \"stop\", \"second_trigger_on_release\": true,",
        1,
    );
    let ws = load_show(&text).unwrap().workspace;
    let first = &ws.cue_lists[0].cues[0];
    assert_eq!(first.second_trigger, SecondTrigger::Nothing);
    assert!(!first.second_trigger_on_release);

    let not_a_cue = r#"{"format_version": 1, "workspace": {"cue_lists": [{"cues": [7, {"kind": {"children": [null]}}]}]}}"#;
    assert!(matches!(load_show(not_a_cue), Err(LoadError::Model(_))));
    let no_workspace = r#"{"format_version": 1, "workspace": 3}"#;
    assert!(matches!(
        load_show(no_workspace),
        Err(LoadError::Migration { from: 1, .. })
    ));
}

#[test]
fn second_trigger_values_are_snake_case_and_unknown_ones_refused() {
    for (name, value) in [
        ("nothing", SecondTrigger::Nothing),
        ("panic", SecondTrigger::Panic),
        ("stop", SecondTrigger::Stop),
        ("hard_stop", SecondTrigger::HardStop),
        ("hard_stop_and_restart", SecondTrigger::HardStopAndRestart),
        ("devamp", SecondTrigger::Devamp),
        ("play_next", SecondTrigger::PlayNext),
        ("play_previous", SecondTrigger::PlayPrevious),
    ] {
        assert_eq!(
            serde_json::to_string(&value).unwrap(),
            format!("\"{name}\"")
        );
    }
    let text = EXAMPLE.replacen(
        "\"second_trigger\": \"panic\"",
        "\"second_trigger\": \"explode\"",
        1,
    );
    assert!(matches!(load_show(&text), Err(LoadError::Model(_))));
    // Missing keys are an error in the current version.
    let text = EXAMPLE.replacen("\"second_trigger\": \"panic\",", "", 1);
    assert!(matches!(load_show(&text), Err(LoadError::Model(_))));
}

#[test]
fn migration_step_failure_is_reported() {
    let err = load_show(r#"{"format_version": 0, "workspace": 7}"#).unwrap_err();
    assert!(matches!(err, LoadError::Migration { from: 0, .. }), "{err}");
}

#[test]
fn migrate_is_identity_from_current_version() {
    let doc: serde_json::Value = serde_json::from_str(EXAMPLE).unwrap();
    assert_eq!(migrate(doc.clone(), FORMAT_VERSION).unwrap(), doc);
}

#[test]
fn too_new_format_version_is_refused_naming_both_versions() {
    let text = EXAMPLE.replacen("\"format_version\": 3", "\"format_version\": 7", 1);
    let err = load_show(&text).unwrap_err();
    assert!(matches!(
        err,
        LoadError::TooNew {
            found: 7,
            supported: FORMAT_VERSION
        }
    ));
    let message = err.to_string();
    assert!(
        message.contains('7') && message.contains(&FORMAT_VERSION.to_string()),
        "{message}"
    );

    let huge = r#"{"format_version": 18446744073709551615, "workspace": {}}"#;
    assert!(matches!(load_show(huge), Err(LoadError::TooNew { .. })));
}

#[test]
fn missing_or_malformed_format_version_is_refused() {
    assert!(matches!(
        load_show(r#"{"workspace": {}}"#),
        Err(LoadError::MissingVersion)
    ));
    assert!(matches!(
        load_show(r#"{"format_version": null}"#),
        Err(LoadError::MissingVersion)
    ));
    assert!(matches!(
        load_show(r#"{"format_version": "1"}"#),
        Err(LoadError::Syntax(_))
    ));
    assert!(matches!(
        load_show(r#"{"format_version": -1}"#),
        Err(LoadError::Syntax(_))
    ));
}

#[test]
fn model_errors_name_the_problem() {
    let text = EXAMPLE.replacen("\"name\": \"Example Show\",", "", 1);
    let err = load_show(&text).unwrap_err();
    assert!(matches!(err, LoadError::Model(_)));
    assert!(err.to_string().contains("missing field `name`"), "{err}");
}

#[test]
fn garbage_is_an_error() {
    for text in [
        "",
        " ",
        "{",
        "}",
        "null",
        "[]",
        "42",
        "\"x\"",
        "{\"format_version\": 1}",
        "\u{0}\u{1}",
    ] {
        assert!(load_show(text).is_err(), "{text:?}");
    }
    let unknown_kind = EXAMPLE.replacen("\"type\": \"memo\"", "\"type\": \"laser\"", 1);
    assert!(matches!(load_show(&unknown_kind), Err(LoadError::Model(_))));
    let overflow = EXAMPLE.replacen("\"pre_wait\": 0.0", "\"pre_wait\": 1e400", 1);
    assert!(load_show(&overflow).is_err());
    let bad_port = EXAMPLE.replacen("\"port\": 53000", "\"port\": 70000", 1);
    assert!(matches!(load_show(&bad_port), Err(LoadError::Model(_))));
    let duplicate_field = EXAMPLE.replacen(
        "\"name\": \"Example Show\",",
        "\"name\": \"A\", \"name\": \"B\",",
        1,
    );
    assert!(load_show(&duplicate_field).is_err());
}

#[test]
fn every_truncation_is_an_error() {
    // The full text minus its trailing newline is still complete; everything shorter is not.
    let complete = EXAMPLE.trim_end();
    for (end, _) in complete.char_indices() {
        assert!(
            load_show(&complete[..end]).is_err(),
            "prefix of {end} bytes loaded"
        );
    }
}

#[test]
fn oversized_input_is_refused_before_parsing() {
    let text = " ".repeat(MAX_SHOW_FILE_BYTES + 1);
    assert!(matches!(load_show(&text), Err(LoadError::TooLarge { .. })));
}

#[test]
fn deeply_nested_input_is_an_error_not_a_stack_overflow() {
    for open in ["[", "{\"a\":"] {
        let text = format!(
            "{{\"format_version\": 1, \"workspace\": {}",
            open.repeat(100_000)
        );
        assert!(matches!(load_show(&text), Err(LoadError::Syntax(_))));
    }
}

#[test]
fn unknown_fields_are_ignored() {
    let text = EXAMPLE
        .replacen("{\n", "{\n  \"future\": {\"deep\": [1, 2]},\n", 1)
        .replacen(
            "\"flagged\": true,",
            "\"flagged\": true, \"sparkle\": 3,",
            1,
        );
    let loaded = load_show(&text).unwrap();
    assert_eq!(loaded.workspace, load_show(EXAMPLE).unwrap().workspace);
}

#[test]
fn windows_absolute_paths_are_normalised_on_load() {
    let text = EXAMPLE
        .replacen("\"media/preshow.wav\"", r#""Door\\Slam.wav""#, 1)
        .replacen(
            "\"/Volumes/Show/sfx/thunder.aif\"",
            r#""C:\\Show\\sfx\\thunder.aif""#,
            1,
        )
        .replacen(
            "\"media/walkin/01.mp3\"",
            r#""\\\\server\\share\\01.mp3""#,
            1,
        );
    assert!(text.contains(r"Door\\Slam.wav"));
    let ws = load_show(&text).unwrap().workspace;
    let media = |n| match &ws.find_cue(cue_id(n)).unwrap().kind {
        CueKind::Audio { media, .. } => media.clone().unwrap(),
        _ => panic!("not audio"),
    };
    // A backslash is a legal macOS file-name character; only Windows-shaped paths convert.
    assert_eq!(media(2).as_str(), r"Door\Slam.wav");
    assert_eq!(media(11).as_str(), "C:/Show/sfx/thunder.aif");
    assert_eq!(media(21).as_str(), "//server/share/01.mp3");
    assert_eq!(MediaRef::new(r"a\b\c.wav").as_str(), r"a\b\c.wav");
    assert_eq!(MediaRef::new(r"d:\x.wav").as_str(), "d:/x.wav");
    assert_eq!(MediaRef::new(r"1:\x.wav").as_str(), r"1:\x.wav");
}

#[test]
fn byte_order_mark_and_crlf_line_endings_are_accepted() {
    let text = format!("\u{feff}{}", EXAMPLE.replace('\n', "\r\n"));
    assert_eq!(
        load_show(&text).unwrap().workspace,
        load_show(EXAMPLE).unwrap().workspace
    );
}

#[test]
fn save_refuses_a_workspace_that_would_not_load() {
    let mut memo = cue(1, CueKind::Memo);
    memo.pre_wait = f64::NAN; // JSON has no NaN; it would be written as null.
    let err = save_show(&workspace(vec![memo])).unwrap_err();
    assert!(
        matches!(err, SaveError::WouldNotLoad(LoadError::Model(_))),
        "{err}"
    );
}

// --- Round-trip property -------------------------------------------------------------------
//
// Numbers are mostly in range, sometimes any f64/f32 at all (NaN, infinities, huge, subnormal).
// The property: after `repair`, any workspace saves, loads back unchanged, and saves to the same
// bytes again; and repairing it a second time changes nothing.

fn num(in_range: std::ops::RangeInclusive<f64>) -> impl Strategy<Value = f64> {
    prop_oneof![3 => in_range, 1 => proptest::num::f64::ANY]
}

fn time() -> impl Strategy<Value = f64> {
    num(0.0..=MAX_TIME)
}

fn level() -> impl Strategy<Value = f64> {
    num(SILENCE_DB..=MAX_DB)
}

fn text() -> impl Strategy<Value = String> {
    "\\PC{0,12}|[\"\\\\/\n\t ]{0,4}"
}

fn target() -> impl Strategy<Value = Option<CueId>> {
    option::of((0u128..64).prop_map(cue_id))
}

fn curve() -> impl Strategy<Value = FadeCurve> {
    prop_oneof![
        Just(FadeCurve::Linear),
        Just(FadeCurve::EqualPower),
        Just(FadeCurve::SCurve),
        Just(FadeCurve::Logarithmic),
        Just(FadeCurve::Exponential),
    ]
}

fn fade_spec() -> impl Strategy<Value = FadeSpec> {
    (time(), curve()).prop_map(|(duration, curve)| FadeSpec { duration, curve })
}

fn index() -> impl Strategy<Value = u32> {
    prop_oneof![4 => 0u32..4, 1 => 0u32..70]
}

fn crosspoint() -> impl Strategy<Value = Crosspoint> {
    (index(), index(), level(), any::<bool>()).prop_map(|(input, output, db, mute)| Crosspoint {
        input,
        output,
        db,
        mute,
    })
}

fn audio_kind() -> impl Strategy<Value = CueKind> {
    let outputs = vec(
        (level(), any::<bool>()).prop_map(|(db, mute)| CueOutput { db, mute }),
        0..5,
    );
    let levels = (
        level(),
        outputs,
        vec(crosspoint(), 0..3),
        vec(vec(index(), 0..3), 0..2),
    )
        .prop_map(|(master_db, outputs, crosspoints, gangs)| CueLevels {
            master_db,
            outputs,
            crosspoints,
            gangs,
        });
    let slices = vec(
        (time(), any::<u32>()).prop_map(|(time, play_count)| SliceMarker { time, play_count }),
        0..4,
    );
    (
        option::of(text().prop_map(MediaRef::new)),
        (0u128..3).prop_map(patch_id),
        time(),
        option::of(time()),
        (fade_spec(), fade_spec()),
        slices,
        any::<u32>(),
        levels,
        any::<bool>(),
    )
        .prop_map(
            |(
                media,
                patch,
                start_time,
                end_time,
                (fade_in, fade_out),
                slices,
                last,
                levels,
                preload,
            )| {
                CueKind::Audio {
                    media,
                    patch,
                    start_time,
                    end_time,
                    fade_in,
                    fade_out,
                    slices,
                    last_slice_play_count: last,
                    levels,
                    preload,
                }
            },
        )
}

fn fade_kind() -> impl Strategy<Value = CueKind> {
    let crosspoint = (index(), index(), level()).prop_map(|(input, output, db)| FadeCrosspoint {
        input,
        output,
        db,
    });
    let levels = (
        option::of(level()),
        vec(option::of(level()), 0..4),
        vec(crosspoint, 0..3),
    )
        .prop_map(|(master_db, output_db, crosspoints)| FadeLevels {
            master_db,
            output_db,
            crosspoints,
        });
    (target(), time(), curve(), levels, any::<bool>()).prop_map(
        |(target, duration, curve, levels, stop_target_when_done)| CueKind::Fade {
            target,
            duration,
            curve,
            levels,
            stop_target_when_done,
        },
    )
}

fn osc_arg() -> impl Strategy<Value = OscArg> {
    prop_oneof![
        any::<i32>().prop_map(|value| OscArg::Int { value }),
        prop_oneof![3 => -1.0e6f32..1.0e6, 1 => proptest::num::f32::ANY]
            .prop_map(|value| OscArg::Float { value }),
        text().prop_map(|value| OscArg::String { value }),
        any::<bool>().prop_map(|value| OscArg::Bool { value }),
    ]
}

fn leaf_kind() -> impl Strategy<Value = CueKind> {
    prop_oneof![
        audio_kind(),
        fade_kind(),
        target().prop_map(|target| CueKind::Start { target }),
        (target(), fade_spec()).prop_map(|(target, fade_out)| CueKind::Stop { target, fade_out }),
        target().prop_map(|target| CueKind::Pause { target }),
        (target(), time()).prop_map(|(target, load_to_time)| CueKind::Load {
            target,
            load_to_time
        }),
        target().prop_map(|target| CueKind::Reset { target }),
        (target(), any::<bool>(), any::<bool>()).prop_map(|(target, a, b)| CueKind::Devamp {
            target,
            start_next_cue: a,
            stop_target_when_slice_ends: b,
            devamp_mode: DevampMode::CurrentSlice,
        }),
        (target(), any::<bool>()).prop_map(|(target, then_go)| CueKind::GoTo { target, then_go }),
        target().prop_map(|target| CueKind::Arm { target }),
        target().prop_map(|target| CueKind::Disarm { target }),
        time().prop_map(|duration| CueKind::Wait { duration }),
        Just(CueKind::Memo),
        ((0u128..3).prop_map(net_id), text(), vec(osc_arg(), 0..4)).prop_map(
            |(destination, address, args)| CueKind::Network {
                destination,
                address,
                args
            }
        ),
    ]
}

fn group_mode() -> impl Strategy<Value = GroupMode> {
    prop_oneof![
        Just(GroupMode::Timeline),
        Just(GroupMode::StartFirstAndEnter),
        Just(GroupMode::StartFirstAndGoToNext),
        (any::<bool>(), any::<bool>())
            .prop_map(|(loop_, shuffle)| GroupMode::Playlist { loop_, shuffle }),
        Just(GroupMode::StartRandom),
    ]
}

fn color() -> impl Strategy<Value = CueColor> {
    prop_oneof![
        Just(CueColor::None),
        Just(CueColor::Red),
        Just(CueColor::Orange),
        Just(CueColor::Green),
        Just(CueColor::Blue),
        Just(CueColor::Purple),
    ]
}

fn continue_mode() -> impl Strategy<Value = ContinueMode> {
    prop_oneof![
        Just(ContinueMode::DoNotContinue),
        Just(ContinueMode::AutoContinue),
        Just(ContinueMode::AutoFollow),
    ]
}

fn hotkey() -> impl Strategy<Value = Hotkey> {
    let modifiers = any::<[bool; 4]>().prop_map(|[shift, control, alt, command]| Modifiers {
        shift,
        control,
        alt,
        command,
    });
    (text(), modifiers).prop_map(|(key, modifiers)| Hotkey { key, modifiers })
}

fn triggers() -> impl Strategy<Value = Triggers> {
    (option::of(hotkey()), option::of(text())).prop_map(|(hotkey, osc)| Triggers { hotkey, osc })
}

/// A cue with id and number left for [`assign_ids`]; `number` is `Some` when it should get one.
fn second_trigger() -> impl Strategy<Value = SecondTrigger> {
    prop_oneof![
        Just(SecondTrigger::Nothing),
        Just(SecondTrigger::Panic),
        Just(SecondTrigger::Stop),
        Just(SecondTrigger::HardStop),
        Just(SecondTrigger::HardStopAndRestart),
        Just(SecondTrigger::Devamp),
        Just(SecondTrigger::PlayNext),
        Just(SecondTrigger::PlayPrevious),
    ]
}

fn with_common(kind: impl Strategy<Value = CueKind>) -> impl Strategy<Value = Cue> {
    (
        (option::of(Just(String::new())), text(), text()),
        (color(), any::<bool>(), any::<bool>()),
        (time(), time(), continue_mode(), triggers()),
        (second_trigger(), any::<bool>()),
        kind,
    )
        .prop_map(
            |(
                (number, name, notes),
                (color, flagged, armed),
                (pre_wait, post_wait, continue_mode, triggers),
                (second_trigger, second_trigger_on_release),
                kind,
            )| {
                Cue {
                    number,
                    name,
                    notes,
                    color,
                    flagged,
                    armed,
                    pre_wait,
                    post_wait,
                    continue_mode,
                    triggers,
                    second_trigger,
                    second_trigger_on_release,
                    ..cue(0, kind)
                }
            },
        )
}

fn any_cue() -> impl Strategy<Value = Cue> {
    with_common(leaf_kind()).prop_recursive(4, 48, 4, |inner| {
        with_common(
            (group_mode(), vec(inner, 0..4))
                .prop_map(|(mode, children)| CueKind::Group { mode, children }),
        )
    })
}

/// Unique ids; numbers are unique too, except that some cues share number "7" so the
/// duplicate-number repair is exercised.
fn assign_ids(cues: &mut [Cue], next: &mut u128) {
    for cue in cues {
        *next += 1;
        cue.id = cue_id(*next);
        if cue.number.is_some() {
            cue.number = Some(if next.is_multiple_of(5) {
                "7".into()
            } else {
                format!("n{next}")
            });
        }
        if let CueKind::Group { children, .. } = &mut cue.kind {
            assign_ids(children, next);
        }
    }
}

fn any_settings() -> impl Strategy<Value = WorkspaceSettings> {
    let hotkeys = vec(option::of(hotkey()), 6).prop_map(|keys| {
        let [
            go,
            stop_all,
            pause_all,
            resume_all,
            playhead_next,
            playhead_previous,
        ] = <[Option<Hotkey>; 6]>::try_from(keys).unwrap();
        TransportHotkeys {
            go,
            stop_all,
            pause_all,
            resume_all,
            playhead_next,
            playhead_previous,
        }
    });
    (
        (num(0.0..=10.0), num(0.0..=60.0), num(5.0..=3600.0)),
        option::of(text()),
        level(),
        hotkeys,
        (num(0.0..=100.0), num(0.1..=10.0)),
    )
        .prop_map(
            |(
                (min_go_interval, panic_duration, autosave_interval),
                osc_passcode,
                master_db,
                hotkeys,
                (start, step),
            )| {
                WorkspaceSettings {
                    min_go_interval,
                    panic_duration,
                    autosave_interval,
                    osc_passcode,
                    master_db,
                    hotkeys,
                    auto_number_start: start,
                    auto_number_increment: step,
                }
            },
        )
}

fn any_patch() -> impl Strategy<Value = (String, f64, Vec<PatchOutput>)> {
    let output = (
        level(),
        any::<bool>(),
        num(0.0..=MAX_DELAY_MS),
        any::<bool>(),
    )
        .prop_map(
            |(level_db, mute, delay_ms, polarity_inverted)| PatchOutput {
                level_db,
                mute,
                delay_ms,
                polarity_inverted,
            },
        );
    (text(), level(), vec(output, 0..5))
}

fn any_workspace() -> impl Strategy<Value = Workspace> {
    let list = (text(), vec(any_cue(), 0..5));
    let network_patch = (text(), text(), any::<u16>(), any::<bool>());
    (
        text(),
        any_settings(),
        vec(list, 0..3),
        vec(any_patch(), 0..3),
        vec(network_patch, 0..3),
    )
        .prop_map(|(name, settings, lists, patches, network_patches)| {
            let mut ws = workspace(vec![]);
            ws.name = name;
            ws.settings = settings;
            let mut next = 0;
            ws.cue_lists = lists
                .into_iter()
                .enumerate()
                .map(|(i, (name, mut cues))| {
                    assign_ids(&mut cues, &mut next);
                    CueList {
                        id: list_id(i as u128),
                        name,
                        cues,
                    }
                })
                .collect();
            ws.patches = patches
                .into_iter()
                .enumerate()
                .map(|(i, (name, master_db, outputs))| OutputPatch {
                    id: patch_id(i as u128),
                    name,
                    master_db,
                    outputs,
                })
                .collect();
            ws.network_patches = network_patches
                .into_iter()
                .enumerate()
                .map(|(i, (name, host, port, tcp))| NetworkPatch {
                    id: net_id(i as u128),
                    name,
                    host,
                    port,
                    protocol: if tcp {
                        NetworkProtocol::Tcp
                    } else {
                        NetworkProtocol::Udp
                    },
                })
                .collect();
            ws
        })
}

proptest! {
    #[test]
    fn repaired_workspaces_save_and_load_back_unchanged(mut ws in any_workspace()) {
        repair(&mut ws);
        let repaired = ws.clone();
        repair(&mut ws);
        prop_assert_eq!(&ws, &repaired, "repair is not idempotent");
        let text = save_show(&ws).unwrap();
        let loaded = load_show(&text).unwrap();
        prop_assert_eq!(&loaded.workspace, &ws);
        prop_assert_eq!(save_show(&loaded.workspace).unwrap(), text);
    }

    /// Any workspace written as v1 (no second-trigger keys) loads as itself with the defaults.
    #[test]
    fn v1_documents_migrate_to_default_second_triggers(mut ws in any_workspace()) {
        repair(&mut ws);
        let mut doc: serde_json::Value = serde_json::from_str(&save_show(&ws).unwrap()).unwrap();
        strip_second_triggers(&mut doc);
        doc["format_version"] = 1.into();
        let loaded = load_show(&doc.to_string()).unwrap().workspace;
        for list in &mut ws.cue_lists {
            reset_second_triggers(&mut list.cues);
        }
        prop_assert_eq!(loaded, ws);
    }
}

fn strip_second_triggers(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.remove("second_trigger");
            map.remove("second_trigger_on_release");
            map.values_mut().for_each(strip_second_triggers);
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(strip_second_triggers),
        _ => {}
    }
}

fn reset_second_triggers(cues: &mut [Cue]) {
    for cue in cues {
        cue.second_trigger = SecondTrigger::Nothing;
        cue.second_trigger_on_release = false;
        if let CueKind::Group { children, .. } = &mut cue.kind {
            reset_second_triggers(children);
        }
    }
}
