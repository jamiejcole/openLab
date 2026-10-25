// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Show-file format: round trips, fixtures, migration, refusals and hostile input
//! (NFR-03, NFR-32, NFR-40, QA-04).

mod common;

use common::*;
use openlab_core::*;
use proptest::prelude::*;
use proptest::{collection::vec, option};

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
    let loaded = load_show(V1_EXAMPLE).unwrap();
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
    let loaded = load_show(V1_EXAMPLE).unwrap();
    assert_eq!(save_show(&loaded.workspace).unwrap(), V1_EXAMPLE);
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
            .starts_with("{\n  \"format_version\": 1,")
    );
}

#[test]
fn migration_step_failure_is_reported() {
    let err = load_show(r#"{"format_version": 0, "workspace": 7}"#).unwrap_err();
    assert!(matches!(err, LoadError::Migration { from: 0, .. }), "{err}");
}

#[test]
fn migrate_is_identity_from_current_version() {
    let doc: serde_json::Value = serde_json::from_str(V1_EXAMPLE).unwrap();
    assert_eq!(migrate(doc.clone(), FORMAT_VERSION).unwrap(), doc);
}

#[test]
fn too_new_format_version_is_refused_naming_both_versions() {
    let text = V1_EXAMPLE.replacen("\"format_version\": 1", "\"format_version\": 7", 1);
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
    let text = V1_EXAMPLE.replacen("\"name\": \"Example Show\",", "", 1);
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
    let unknown_kind = V1_EXAMPLE.replacen("\"type\": \"memo\"", "\"type\": \"laser\"", 1);
    assert!(matches!(load_show(&unknown_kind), Err(LoadError::Model(_))));
    let overflow = V1_EXAMPLE.replacen("\"pre_wait\": 0.0", "\"pre_wait\": 1e400", 1);
    assert!(load_show(&overflow).is_err());
    let bad_port = V1_EXAMPLE.replacen("\"port\": 53000", "\"port\": 70000", 1);
    assert!(matches!(load_show(&bad_port), Err(LoadError::Model(_))));
    let duplicate_field = V1_EXAMPLE.replacen(
        "\"name\": \"Example Show\",",
        "\"name\": \"A\", \"name\": \"B\",",
        1,
    );
    assert!(load_show(&duplicate_field).is_err());
}

#[test]
fn every_truncation_is_an_error() {
    // The full text minus its trailing newline is still complete; everything shorter is not.
    let complete = V1_EXAMPLE.trim_end();
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
    let text = V1_EXAMPLE
        .replacen("{\n", "{\n  \"future\": {\"deep\": [1, 2]},\n", 1)
        .replacen(
            "\"flagged\": true,",
            "\"flagged\": true, \"sparkle\": 3,",
            1,
        );
    let loaded = load_show(&text).unwrap();
    assert_eq!(loaded.workspace, load_show(V1_EXAMPLE).unwrap().workspace);
}

#[test]
fn windows_paths_are_normalised_on_load() {
    let text = V1_EXAMPLE
        .replacen("\"media/preshow.wav\"", r#""media\\preshow.wav""#, 1)
        .replacen(
            "\"/Volumes/Show/sfx/thunder.aif\"",
            r#""C:\\Show\\sfx\\thunder.aif""#,
            1,
        );
    assert!(text.contains(r"media\\preshow.wav"));
    let ws = load_show(&text).unwrap().workspace;
    let media = |n| match &ws.find_cue(cue_id(n)).unwrap().kind {
        CueKind::Audio { media, .. } => media.clone().unwrap(),
        _ => panic!("not audio"),
    };
    assert_eq!(media(2).as_str(), "media/preshow.wav");
    assert_eq!(media(11).as_str(), "C:/Show/sfx/thunder.aif");
    assert_eq!(MediaRef::new(r"a\b\c.wav").as_str(), "a/b/c.wav");
}

#[test]
fn byte_order_mark_and_crlf_line_endings_are_accepted() {
    let text = format!("\u{feff}{}", V1_EXAMPLE.replace('\n', "\r\n"));
    assert_eq!(
        load_show(&text).unwrap().workspace,
        load_show(V1_EXAMPLE).unwrap().workspace
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

fn time() -> impl Strategy<Value = f64> {
    0.0..1.0e5f64
}

fn level() -> impl Strategy<Value = f64> {
    SILENCE_DB..=MAX_DB
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

fn crosspoint() -> impl Strategy<Value = Crosspoint> {
    (0u32..8, 0u32..8, level()).prop_map(|(input, output, db)| Crosspoint { input, output, db })
}

fn audio_kind() -> impl Strategy<Value = CueKind> {
    let levels = (
        level(),
        vec(level(), 0..4),
        vec(crosspoint(), 0..3),
        vec(vec(0u32..8, 0..3), 0..2),
    )
        .prop_map(|(master_db, output_db, crosspoints, gangs)| CueLevels {
            master_db,
            output_db,
            crosspoints,
            gangs,
        });
    let slices = vec(
        (time(), any::<u32>()).prop_map(|(time, play_count)| SliceMarker { time, play_count }),
        0..3,
    );
    (
        option::of(text().prop_map(MediaRef::new)),
        (0u128..3).prop_map(patch_id),
        time(),
        option::of(0.001..1.0e3f64),
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
                length,
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
                    end_time: length.map(|l| start_time + l),
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
    let levels = (
        option::of(level()),
        vec(option::of(level()), 0..4),
        vec(crosspoint(), 0..3),
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
        (-1.0e6f32..1.0e6).prop_map(|value| OscArg::Float { value }),
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

fn triggers() -> impl Strategy<Value = Triggers> {
    let modifiers = any::<[bool; 4]>().prop_map(|[shift, control, alt, command]| Modifiers {
        shift,
        control,
        alt,
        command,
    });
    let hotkey = (text(), modifiers).prop_map(|(key, modifiers)| Hotkey { key, modifiers });
    (option::of(hotkey), option::of(text())).prop_map(|(hotkey, osc)| Triggers { hotkey, osc })
}

/// A cue with id and number left for [`assign_ids`]; `number` is `Some` when it should get one.
fn with_common(kind: impl Strategy<Value = CueKind>) -> impl Strategy<Value = Cue> {
    (
        (option::of(Just(String::new())), text(), text()),
        (color(), any::<bool>(), any::<bool>()),
        (time(), time(), continue_mode(), triggers()),
        kind,
    )
        .prop_map(
            |(
                (number, name, notes),
                (color, flagged, armed),
                (pre_wait, post_wait, continue_mode, triggers),
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

fn assign_ids(cues: &mut [Cue], next: &mut u128) {
    for cue in cues {
        *next += 1;
        cue.id = cue_id(*next);
        if cue.number.is_some() {
            cue.number = Some(format!("{next}"));
        }
        if let CueKind::Group { children, .. } = &mut cue.kind {
            assign_ids(children, next);
        }
    }
}

fn any_workspace() -> impl Strategy<Value = Workspace> {
    let settings = (time(), time(), time(), option::of(text())).prop_map(
        |(min_go_interval, panic_duration, autosave_interval, osc_passcode)| WorkspaceSettings {
            min_go_interval,
            panic_duration,
            autosave_interval,
            osc_passcode,
        },
    );
    let list = (text(), vec(any_cue(), 0..5));
    let patch = (text(), 0u32..65);
    let network_patch = (text(), text(), any::<u16>(), any::<bool>());
    (
        text(),
        settings,
        vec(list, 0..3),
        vec(patch, 0..3),
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
                .map(|(i, (name, channel_count))| OutputPatch {
                    id: patch_id(i as u128),
                    name,
                    channel_count,
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
    fn save_then_load_round_trips(ws in any_workspace()) {
        let text = save_show(&ws).unwrap();
        let loaded = load_show(&text).unwrap();
        prop_assert_eq!(&loaded.workspace, &ws);
        prop_assert_eq!(save_show(&loaded.workspace).unwrap(), text);
    }
}
