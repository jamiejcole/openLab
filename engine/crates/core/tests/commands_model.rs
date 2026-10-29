// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! Commands for settings, output patches, network patches and cue lists, and undo coalescing
//! (ED-07). The randomised version is in `commands_prop.rs`.

mod common;

use common::*;
use openlab_core::protocol::{Delete, Upsert};
use openlab_core::*;

fn setting(setting: WorkspaceSetting) -> Command {
    Command::SetWorkspaceSetting { setting }
}

fn patch_prop(id: u128, property: PatchProperty) -> Command {
    Command::SetPatchProperty {
        id: patch_id(id),
        property,
    }
}

fn net(n: u128) -> NetworkPatch {
    NetworkPatch {
        id: net_id(n),
        name: format!("Net {n}"),
        host: "10.0.0.1".into(),
        port: 8000,
        protocol: NetworkProtocol::Tcp,
    }
}

fn network_cue(n: u128, destination: u128) -> Cue {
    let mut c = cue(n, CueKind::Memo);
    c.kind = CueKind::Network {
        destination: net_id(destination),
        address: "/go".into(),
        args: vec![],
    };
    c
}

fn audio_on(n: u128, patch: u128) -> Cue {
    let mut c = cue(n, audio());
    if let CueKind::Audio { patch: p, .. } = &mut c.kind {
        *p = patch_id(patch);
    }
    c
}

/// The workspace saves and loads back unchanged, with no value problem.
fn assert_clean(ws: &Workspace) {
    let issues: Vec<_> = validate(ws)
        .into_iter()
        .filter(|i| i.severity == Severity::Error)
        .collect();
    assert!(issues.is_empty(), "{issues:?}");
    let mut repaired = ws.clone();
    let values: Vec<_> = repair(&mut repaired)
        .into_iter()
        .filter(|i| {
            !matches!(
                i.kind,
                IssueKind::PatchMissing { .. }
                    | IssueKind::NetworkPatchMissing { .. }
                    | IssueKind::UnpatchedOutputs { .. }
            )
        })
        .collect();
    assert!(values.is_empty(), "{values:?}");
    assert_eq!(&repaired, ws);
}

/// Runs `command`, then undo and redo, checking exact restoration. Returns the changed workspace.
fn round_trip(ws: &Workspace, command: Command) -> Workspace {
    let mut work = ws.clone();
    let mut history = History::new();
    assert!(history.execute(&mut work, command).unwrap());
    let after = work.clone();
    assert_ne!(&after, ws);
    assert_clean(&after);
    assert!(history.undo(&mut work).unwrap());
    assert_eq!(&work, ws, "undo restores exactly");
    assert!(history.redo(&mut work).unwrap());
    assert_eq!(work, after, "redo restores exactly");
    after
}

/// Refused: an error, and the workspace and the history are as they were.
fn refused(ws: &Workspace, command: Command) -> CommandError {
    let mut work = ws.clone();
    let mut history = History::new();
    let error = history.execute(&mut work, command).unwrap_err();
    assert_eq!(&work, ws);
    assert!(!history.can_undo());
    error
}

// ---- Settings ----

#[test]
fn every_setting_can_be_set_and_undone() {
    let ws = workspace(vec![]);
    let key = Hotkey {
        key: "f5".into(),
        modifiers: Modifiers::default(),
    };
    for command in [
        setting(WorkspaceSetting::MinGoInterval { value: 2.5 }),
        setting(WorkspaceSetting::PanicDuration { value: 0.0 }),
        setting(WorkspaceSetting::PanicDuration { value: 60.0 }),
        setting(WorkspaceSetting::AutosaveInterval { value: 300.0 }),
        setting(WorkspaceSetting::MasterDb { value: -12.0 }),
        setting(WorkspaceSetting::AutoNumberStart { value: 100.0 }),
        setting(WorkspaceSetting::AutoNumberIncrement { value: 0.5 }),
        setting(WorkspaceSetting::Hotkey {
            key: TransportKey::Go,
            value: Some(key.clone()),
        }),
        setting(WorkspaceSetting::Hotkey {
            key: TransportKey::PlayheadPrevious,
            value: None,
        }),
    ] {
        round_trip(&ws, command);
    }
}

#[test]
fn a_setting_lands_in_its_field_and_nowhere_else() {
    let ws = workspace(vec![]);
    let changed = round_trip(&ws, setting(WorkspaceSetting::PanicDuration { value: 3.0 }));
    let mut expected = ws.clone();
    expected.settings.panic_duration = 3.0;
    assert_eq!(changed, expected);
    let changed = round_trip(
        &ws,
        setting(WorkspaceSetting::Hotkey {
            key: TransportKey::PauseAll,
            value: None,
        }),
    );
    let mut expected = ws.clone();
    expected.settings.hotkeys.pause_all = None;
    assert_eq!(changed, expected);
}

// ---- Key map (TRG-01) ----

fn plain(key: &str) -> Hotkey {
    Hotkey {
        key: key.into(),
        modifiers: Modifiers::default(),
    }
}

fn with_mods(key: &str, shift: bool, control: bool, alt: bool, command: bool) -> Hotkey {
    Hotkey {
        key: key.into(),
        modifiers: Modifiers {
            shift,
            control,
            alt,
            command,
        },
    }
}

fn set_key(key: TransportKey, value: Option<Hotkey>) -> Command {
    setting(WorkspaceSetting::Hotkey { key, value })
}

fn refusal(ws: &Workspace, command: Command) -> (&'static str, HotkeyClash, String) {
    let error = refused(ws, command);
    let message = error.to_string();
    match error {
        CommandError::HotkeyRefused { field, reason } => (field, reason, message),
        other => panic!("expected a key-map refusal, got {other:?}"),
    }
}

#[test]
fn every_selected_cue_key_can_be_set_and_undone() {
    let ws = workspace(vec![]);
    for (key, f, action) in [
        (TransportKey::LoadSelected, "f1", "load_selected"),
        (
            TransportKey::PauseResumeSelected,
            "f2",
            "pause_resume_selected",
        ),
        (TransportKey::StartSelected, "f3", "start_selected"),
        (TransportKey::StopSelected, "f4", "stop_selected"),
        (TransportKey::ResetSelected, "f5", "reset_selected"),
    ] {
        let after = round_trip(&ws, set_key(key, Some(plain(f))));
        assert_eq!(after.settings.hotkeys.action_of(&plain(f)), Some(action));
    }
    let after = round_trip(&ws, set_key(TransportKey::LoadSelected, None));
    assert_eq!(after.settings.hotkeys.load_selected, None);
}

#[test]
fn esc_and_up_down_with_any_modifiers_are_refused_as_workspace_keys() {
    let ws = workspace(vec![]);
    for (key, value) in [
        (TransportKey::StopAll, plain("escape")),
        (
            TransportKey::Go,
            with_mods("escape", true, true, true, true),
        ),
        (TransportKey::PlayheadNext, plain("down")),
        (TransportKey::PlayheadPrevious, plain("up")),
        (
            TransportKey::LoadSelected,
            with_mods("up", true, false, false, false),
        ),
        // QLab 5 refuses the arrows whatever is held (answer R21).
        (
            TransportKey::PlayheadNext,
            with_mods("down", false, false, true, false),
        ),
        (
            TransportKey::PlayheadPrevious,
            with_mods("up", false, true, false, false),
        ),
        (
            TransportKey::StartSelected,
            with_mods("down", true, true, true, true),
        ),
    ] {
        let (field, reason, message) = refusal(&ws, set_key(key, Some(value)));
        assert_eq!(reason, HotkeyClash::Reserved);
        assert!(message.starts_with(field), "{message}");
        assert!(message.contains("Esc is panic"), "{message}");
    }
    // Left / Right are keys like any other.
    round_trip(
        &ws,
        set_key(TransportKey::StartSelected, Some(plain("right"))),
    );
    // Reset to the defaults is never refused, though Next / Previous Cue default to arrows.
    let mut moved = ws.clone();
    moved.settings.hotkeys.playhead_next = Some(plain("n"));
    moved.settings.hotkeys.playhead_previous = Some(plain("m"));
    let defaults = ws.settings.hotkeys.clone();
    let reset = round_trip(
        &moved,
        setting(WorkspaceSetting::Hotkeys {
            value: Box::new(defaults.clone()),
        }),
    );
    assert_eq!(reset.settings.hotkeys, defaults);
}

#[test]
fn a_key_already_in_the_map_is_refused_naming_its_action() {
    let ws = workspace(vec![]);
    let (field, reason, message) =
        refusal(&ws, set_key(TransportKey::PauseAll, Some(plain("space"))));
    assert_eq!(field, "settings.hotkeys.pause_all");
    assert_eq!(
        reason,
        HotkeyClash::Workspace {
            other: "settings.hotkeys.go"
        }
    );
    assert!(message.contains("settings.hotkeys.go"), "{message}");
    // A selected-cue key against a transport key, and the other way round.
    let (_, reason, _) = refusal(&ws, set_key(TransportKey::StopSelected, Some(plain("["))));
    assert_eq!(
        reason,
        HotkeyClash::Workspace {
            other: "settings.hotkeys.pause_all"
        }
    );
    let (_, reason, _) = refusal(&ws, set_key(TransportKey::Go, Some(plain("l"))));
    assert_eq!(
        reason,
        HotkeyClash::Workspace {
            other: "settings.hotkeys.load_selected"
        }
    );
    // The same key with other modifiers is another key.
    round_trip(
        &ws,
        set_key(
            TransportKey::PauseAll,
            Some(with_mods("space", true, false, false, false)),
        ),
    );
}

#[test]
fn a_cue_hotkey_is_refused_as_a_workspace_key_naming_the_cue() {
    let mut numbered_cue = numbered(1, "12.5", audio());
    numbered_cue.triggers.hotkey = Some(plain("f6"));
    let mut child = cue(3, audio());
    child.triggers.hotkey = Some(plain("f7"));
    let ws = workspace(vec![numbered_cue, cue(2, group(vec![child]))]);
    let (field, reason, message) =
        refusal(&ws, set_key(TransportKey::StartSelected, Some(plain("f6"))));
    assert_eq!(field, "settings.hotkeys.start_selected");
    assert_eq!(
        reason,
        HotkeyClash::Cue {
            cue: cue_id(1),
            number: Some("12.5".into())
        }
    );
    assert!(message.contains("cue 12.5"), "{message}");
    // A cue without a number is named by its id; a child in a group is found.
    let (_, reason, message) = refusal(&ws, set_key(TransportKey::Go, Some(plain("f7"))));
    assert_eq!(
        reason,
        HotkeyClash::Cue {
            cue: cue_id(3),
            number: None
        }
    );
    assert!(message.contains(&cue_id(3).to_string()), "{message}");
}

#[test]
fn a_clash_the_show_was_loaded_with_does_not_block_other_key_edits() {
    // A hand-edited file: GO and Pause All share a key, Stop All is Esc.
    let mut ws = workspace(vec![]);
    ws.settings.hotkeys.pause_all = Some(plain("space"));
    ws.settings.hotkeys.stop_all = Some(plain("escape"));
    let ws = round_trip(&ws, set_key(TransportKey::ResumeAll, Some(plain("f8"))));
    // Fixing one side of the clash is accepted, and so is undoing the fix.
    round_trip(&ws, set_key(TransportKey::PauseAll, Some(plain("f9"))));
    // But the edit may not make a new one with the old key.
    let (_, reason, _) = refusal(
        &ws,
        set_key(TransportKey::StartSelected, Some(plain("space"))),
    );
    assert_eq!(
        reason,
        HotkeyClash::Workspace {
            other: "settings.hotkeys.go"
        }
    );
}

#[test]
fn the_whole_map_swaps_and_resets_in_one_undo_step() {
    let ws = workspace(vec![]);
    let defaults = ws.settings.hotkeys.clone();
    let mut swapped = defaults.clone();
    swapped.go = defaults.pause_all.clone();
    swapped.pause_all = defaults.go.clone();
    let after = round_trip(
        &ws,
        setting(WorkspaceSetting::Hotkeys {
            value: Box::new(swapped.clone()),
        }),
    );
    assert_eq!(after.settings.hotkeys, swapped);
    // Reset from the swapped map: one step, which one key at a time could not do.
    let reset = round_trip(
        &after,
        setting(WorkspaceSetting::Hotkeys {
            value: Box::new(defaults),
        }),
    );
    assert_eq!(reset, ws);
    // The map is judged by the same rules.
    let mut bad = swapped.clone();
    bad.reset_selected = Some(plain("escape"));
    let (field, reason, _) = refusal(
        &ws,
        setting(WorkspaceSetting::Hotkeys {
            value: Box::new(bad),
        }),
    );
    assert_eq!(
        (field, reason),
        ("settings.hotkeys.reset_selected", HotkeyClash::Reserved)
    );
    let mut dup = swapped;
    dup.stop_selected = Some(plain("]"));
    let (field, reason, _) = refusal(
        &ws,
        setting(WorkspaceSetting::Hotkeys {
            value: Box::new(dup),
        }),
    );
    // The changed key is the one refused, naming the key it clashes with.
    assert_eq!(field, "settings.hotkeys.stop_selected");
    assert_eq!(
        reason,
        HotkeyClash::Workspace {
            other: "settings.hotkeys.resume_all"
        }
    );
    // A bad key name in the map is refused like a single key's.
    let mut named = ws.settings.hotkeys.clone();
    named.load_selected = Some(plain("L"));
    assert!(matches!(
        refused(
            &ws,
            setting(WorkspaceSetting::Hotkeys {
                value: Box::new(named)
            })
        ),
        CommandError::InvalidModel(_)
    ));
}

#[test]
fn undo_puts_back_a_key_the_rules_would_refuse_now() {
    // Stop All = Esc, as a v4 hand edit could leave it; clearing it and undoing restores Esc.
    let mut ws = workspace(vec![]);
    ws.settings.hotkeys.stop_all = Some(plain("escape"));
    round_trip(&ws, set_key(TransportKey::StopAll, None));
    // A cue hotkey that became a key of the map later: clearing the key and undoing works.
    let mut c = cue(1, audio());
    c.triggers.hotkey = Some(plain("l"));
    let ws = workspace(vec![c]);
    let after = round_trip(&ws, set_key(TransportKey::LoadSelected, None));
    // With L free, the cue fires from it; undone, the workspace key wins again (K1b).
    assert_eq!(after.cues_with_hotkey(&plain("l")), vec![cue_id(1)]);
    assert!(ws.cues_with_hotkey(&plain("l")).is_empty());
}

fn osc_ws() -> Workspace {
    let mut ws = workspace(vec![]);
    ws.settings.osc_access = OscAccess::new_workspace(1234);
    ws
}

fn rights(view: bool, edit: bool, control: bool) -> OscRights {
    OscRights {
        view,
        edit,
        control,
    }
}

fn add_pass(index: usize, passcode: &str, rights: OscRights) -> Command {
    Command::AddOscPasscode {
        index,
        passcode: passcode.into(),
        rights,
    }
}

fn remove_pass(passcode: &str) -> Command {
    Command::RemoveOscPasscode {
        passcode: passcode.into(),
    }
}

fn set_rights(passcode: Option<&str>, rights: OscRights) -> Command {
    Command::SetOscRights {
        target: match passcode {
            Some(p) => OscRightsTarget::Passcode { passcode: p.into() },
            None => OscRightsTarget::NoPasscode,
        },
        rights,
    }
}

#[test]
fn passcodes_can_be_added_removed_and_given_rights_and_each_is_undone() {
    let ws = osc_ws();
    // Add: at the front, in the middle (index = length), position kept by undo.
    let after = round_trip(&ws, add_pass(0, "5678", rights(true, false, false)));
    assert_eq!(after.settings.osc_access.passcodes[0].passcode, "5678");
    assert_eq!(after.settings.osc_access.passcodes[1].passcode, "1234");
    let after = round_trip(&ws, add_pass(1, "5678", OscRights::ALL));
    assert_eq!(after.settings.osc_access.passcodes[1].passcode, "5678");
    // Remove: the row comes back where it was, with its rights.
    let two = after;
    let after = round_trip(&two, remove_pass("1234"));
    assert_eq!(after.settings.osc_access.passcodes.len(), 1);
    // Even the last row can go (undo brings it back).
    round_trip(&ws, remove_pass("1234"));
    // Rights of a row and of No Passcode.
    let after = round_trip(&ws, set_rights(Some("1234"), rights(true, true, false)));
    assert_eq!(
        after.settings.osc_access.passcodes[0].rights,
        rights(true, true, false)
    );
    let after = round_trip(&ws, set_rights(None, rights(true, false, false)));
    assert_eq!(
        after.settings.osc_access.no_passcode,
        rights(true, false, false)
    );
    assert!(!after.settings.osc_access.requires_passcode());
}

#[test]
fn osc_table_commands_land_in_the_settings_footprint() {
    let ws = osc_ws();
    for command in [
        add_pass(0, "5678", OscRights::ALL),
        remove_pass("1234"),
        set_rights(None, OscRights::ALL),
    ] {
        assert!(command.footprint().settings);
        let mut work = ws.clone();
        work.apply(command).unwrap();
    }
}

#[test]
fn a_table_edit_is_announced_as_a_settings_upsert_carrying_the_table() {
    let mut ws = osc_ws();
    let command = add_pass(1, "42", rights(true, true, false));
    ws.apply(command.clone()).unwrap();
    let (upserts, deletes) = model_changes(&ws, &command.footprint());
    assert!(deletes.is_empty());
    assert!(matches!(
        upserts.as_slice(),
        [Upsert::Settings { settings, .. }] if settings.osc_access == ws.settings.osc_access
    ));
}

#[test]
fn bad_passcodes_and_rights_are_refused_and_change_nothing() {
    let ws = osc_ws();
    for passcode in [
        "",
        "12a4",
        "abcd",
        "١٢٣٤",
        &"9".repeat(MAX_OSC_PASSCODE_DIGITS + 1),
    ] {
        assert!(
            matches!(
                refused(&ws, add_pass(0, passcode, OscRights::ALL)),
                CommandError::InvalidValue(_)
            ),
            "{passcode:?}"
        );
    }
    // The longest allowed one is fine.
    round_trip(
        &ws,
        add_pass(0, &"9".repeat(MAX_OSC_PASSCODE_DIGITS), OscRights::ALL),
    );
    // A repeat, a missing row, an index past the end.
    assert!(matches!(
        refused(&ws, add_pass(0, "1234", OscRights::ALL)),
        CommandError::InvalidValue(_)
    ));
    assert!(matches!(
        refused(&ws, remove_pass("9999")),
        CommandError::InvalidValue(_)
    ));
    assert!(matches!(
        refused(&ws, set_rights(Some("9999"), OscRights::ALL)),
        CommandError::InvalidValue(_)
    ));
    assert!(matches!(
        refused(&ws, add_pass(3, "5678", OscRights::ALL)),
        CommandError::IndexOutOfRange { .. }
    ));
    // Edit or Control without View.
    for bad in [rights(false, true, false), rights(false, false, true)] {
        for command in [
            add_pass(0, "5678", bad),
            set_rights(Some("1234"), bad),
            set_rights(None, bad),
        ] {
            assert!(matches!(
                refused(&ws, command),
                CommandError::InvalidValue(_)
            ));
        }
    }
    // The error never repeats a passcode.
    let text = refused(&ws, remove_pass("9999")).to_string();
    assert!(!text.contains("9999"));
}

#[test]
fn the_table_holds_at_most_sixteen_passcodes() {
    let mut ws = workspace(vec![]);
    for n in 0..MAX_OSC_PASSCODES {
        ws.apply(add_pass(n, &format!("{n}"), OscRights::ALL))
            .unwrap();
    }
    assert!(matches!(
        refused(&ws, add_pass(0, "99", OscRights::ALL)),
        CommandError::InvalidValue(_)
    ));
}

#[test]
fn setting_the_same_rights_is_not_an_edit() {
    let ws = osc_ws();
    let mut work = ws.clone();
    let mut history = History::new();
    assert!(
        !history
            .execute(&mut work, set_rights(None, OscRights::NONE))
            .unwrap()
    );
    assert!(
        !history
            .execute(&mut work, set_rights(Some("1234"), OscRights::ALL))
            .unwrap()
    );
    assert!(!history.can_undo());
}

/// A passcode that is not digits came from a file made before the table existed (migration
/// keeps it). Undo and redo of edits around it work, and it can be removed and put back.
#[test]
fn a_legacy_passcode_survives_edits_and_undo() {
    let mut ws = osc_ws();
    ws.settings.osc_access.passcodes.push(OscPasscode {
        passcode: "hunter2".into(),
        rights: OscRights::ALL,
    });
    round_trip_unchecked(&ws, remove_pass("hunter2"));
    // An unrelated setting is still accepted next to it.
    let mut work = ws.clone();
    work.apply(setting(WorkspaceSetting::PanicDuration { value: 3.0 }))
        .unwrap();
    // Removing and undoing puts it back although a new one like it would be refused.
    let mut history = History::new();
    history.execute(&mut work, remove_pass("hunter2")).unwrap();
    assert!(history.undo(&mut work).unwrap());
    assert!(
        work.settings
            .osc_access
            .passcodes
            .iter()
            .any(|p| p.passcode == "hunter2")
    );
}

/// As `round_trip`, without the check that the result loads back clean (the legacy passcode is
/// a load warning).
fn round_trip_unchecked(ws: &Workspace, command: Command) {
    let mut work = ws.clone();
    let mut history = History::new();
    assert!(history.execute(&mut work, command).unwrap());
    assert!(history.undo(&mut work).unwrap());
    assert_eq!(&work, ws);
    assert!(history.redo(&mut work).unwrap());
    assert!(history.undo(&mut work).unwrap());
    assert_eq!(&work, ws);
}

#[test]
fn out_of_range_settings_are_refused_whole() {
    let ws = workspace(vec![]);
    for bad in [
        WorkspaceSetting::MinGoInterval { value: 10.5 },
        WorkspaceSetting::MinGoInterval { value: -1.0 },
        WorkspaceSetting::MinGoInterval { value: f64::NAN },
        WorkspaceSetting::PanicDuration { value: 61.0 },
        WorkspaceSetting::PanicDuration {
            value: f64::INFINITY,
        },
        WorkspaceSetting::AutosaveInterval { value: 4.0 },
        WorkspaceSetting::AutosaveInterval { value: 3601.0 },
        WorkspaceSetting::MasterDb { value: 30.0 },
        WorkspaceSetting::MasterDb { value: f64::NAN },
        WorkspaceSetting::AutoNumberStart { value: -1.0 },
        WorkspaceSetting::AutoNumberIncrement { value: 0.0 },
        WorkspaceSetting::AutoNumberIncrement { value: 2_000_000.0 },
    ] {
        assert!(
            matches!(
                refused(&ws, setting(bad.clone())),
                CommandError::InvalidModel(_)
            ),
            "{bad:?}"
        );
    }
}

#[test]
fn master_level_below_silence_is_refused_not_silently_repaired() {
    // Repair would normalise it to silence: a command must not leave anything for repair to do.
    let ws = workspace(vec![]);
    let error = refused(
        &ws,
        setting(WorkspaceSetting::MasterDb {
            value: SILENCE_DB - 10.0,
        }),
    );
    assert!(matches!(error, CommandError::InvalidModel(_)));
}

#[test]
fn setting_the_value_it_already_has_is_not_an_edit() {
    let mut ws = workspace(vec![]);
    let mut history = History::new();
    let same = setting(WorkspaceSetting::PanicDuration { value: 5.0 });
    assert!(!history.execute(&mut ws, same).unwrap());
    assert!(!history.can_undo());
    assert!(!history.can_redo());
}

// ---- No-op edits are not undo steps ----

#[test]
fn cue_edits_that_change_nothing_are_not_undo_steps() {
    let mut c = cue(1, CueKind::Memo);
    c.name = "A".into();
    c.number = Some("1".into());
    let mut ws = workspace(vec![c, cue(2, CueKind::Memo)]);
    let mut history = History::new();
    let noops = [
        Command::SetCueProperty {
            id: cue_id(1),
            property: CueProperty::Name { value: "A".into() },
        },
        Command::BatchSet {
            ids: vec![cue_id(1), cue_id(2)],
            property: CueProperty::Armed { value: true },
        },
        Command::SetCueNumbers {
            numbers: vec![NumberAssignment {
                id: cue_id(1),
                number: Some("1".into()),
            }],
        },
        Command::MoveCue {
            id: cue_id(1),
            index: 0,
        },
        Command::ReparentCue {
            id: cue_id(2),
            to: Position {
                list: list_id(1),
                parent: None,
                index: 1,
            },
        },
        Command::Transaction { commands: vec![] },
    ];
    for command in noops {
        assert!(
            !history.execute(&mut ws, command.clone()).unwrap(),
            "{command:?}"
        );
    }
    assert!(!history.can_undo());
}

#[test]
fn a_no_op_inside_a_transaction_adds_nothing_to_the_step() {
    let mut ws = workspace(vec![cue(1, CueKind::Memo)]);
    let mut history = History::new();
    let command = Command::Transaction {
        commands: vec![
            Command::SetCueProperty {
                id: cue_id(1),
                property: CueProperty::Armed { value: true },
            },
            Command::SetCueProperty {
                id: cue_id(1),
                property: CueProperty::Name { value: "B".into() },
            },
        ],
    };
    assert!(history.execute(&mut ws, command).unwrap());
    assert_eq!(history.undo_len(), 1);
    let Some(Command::Transaction { commands }) = history.peek_undo() else {
        panic!("not a transaction");
    };
    assert_eq!(commands.len(), 1);
}

#[test]
fn a_no_op_does_not_clear_redo() {
    let mut ws = workspace(vec![cue(1, CueKind::Memo)]);
    let mut history = History::new();
    let name = |v: &str| Command::SetCueProperty {
        id: cue_id(1),
        property: CueProperty::Name { value: v.into() },
    };
    history.execute(&mut ws, name("B")).unwrap();
    history.undo(&mut ws).unwrap();
    assert!(history.can_redo());
    assert!(!history.execute(&mut ws, name("")).unwrap());
    assert!(history.can_redo());
}

// ---- Output patches ----

#[test]
fn a_patch_can_be_added_anywhere_and_undone() {
    let ws = workspace(vec![]);
    for index in [0, 1] {
        let after = round_trip(
            &ws,
            Command::AddPatch {
                index,
                patch: patch(2, 6),
            },
        );
        assert_eq!(after.patches.len(), 2);
        assert_eq!(after.patches[index].id, patch_id(2));
    }
}

#[test]
fn bad_patches_are_not_added() {
    let ws = workspace(vec![]);
    let add = |index, p| Command::AddPatch { index, patch: p };
    assert!(matches!(
        refused(&ws, add(2, patch(2, 2))),
        CommandError::IndexOutOfRange { index: 2, len: 1 }
    ));
    assert!(matches!(
        refused(&ws, add(0, patch(1, 2))),
        CommandError::DuplicatePatchId(_)
    ));
    assert!(matches!(
        refused(&ws, add(0, patch(2, 0))),
        CommandError::InvalidModel(_)
    ));
    assert!(matches!(
        refused(&ws, add(0, patch(2, 65))),
        CommandError::InvalidModel(_)
    ));
    let mut loud = patch(2, 2);
    loud.master_db = 40.0;
    assert!(matches!(
        refused(&ws, add(0, loud)),
        CommandError::InvalidModel(_)
    ));
    let mut late = patch(2, 2);
    late.outputs[1].delay_ms = 5000.0;
    assert!(matches!(
        refused(&ws, add(0, late)),
        CommandError::InvalidModel(_)
    ));
}

#[test]
fn an_unused_patch_can_be_removed_and_comes_back_in_place() {
    let mut ws = workspace(vec![]);
    ws.patches.push(patch(2, 4));
    ws.patches.push(patch(3, 2));
    let after = round_trip(
        &ws,
        Command::RemovePatch {
            id: patch_id(2),
            even_if_used: false,
        },
    );
    assert_eq!(
        after.patches.iter().map(|p| p.id).collect::<Vec<_>>(),
        vec![patch_id(1), patch_id(3)]
    );
}

#[test]
fn a_patch_a_cue_uses_cannot_be_removed() {
    let mut ws = workspace(vec![cue(1, CueKind::Memo), audio_on(7, 1)]);
    ws.patches.push(patch(2, 2));
    let remove = |id| Command::RemovePatch {
        id: patch_id(id),
        even_if_used: false,
    };
    assert_eq!(
        refused(&ws, remove(1)),
        CommandError::PatchInUse {
            patch: patch_id(1),
            cue: cue_id(7)
        }
    );
    // A cue inside a group counts.
    let mut nested = workspace(vec![cue(1, group(vec![audio_on(8, 1)]))]);
    nested.patches.push(patch(2, 2));
    assert!(matches!(
        refused(&nested, remove(1)),
        CommandError::PatchInUse { .. }
    ));
    // Another patch is free.
    round_trip(&ws, remove(2));
    assert!(matches!(
        refused(&ws, remove(9)),
        CommandError::PatchNotFound(_)
    ));
}

#[test]
fn removing_a_patch_in_use_is_possible_only_when_forced() {
    let ws = workspace(vec![audio_on(7, 1)]);
    let after = round_trip(
        &ws,
        Command::RemovePatch {
            id: patch_id(1),
            even_if_used: true,
        },
    );
    assert!(after.patches.is_empty());
}

#[test]
fn undoing_an_add_that_fixed_a_dangling_cue_is_not_blocked_by_the_cue() {
    // The cue points at a patch the workspace lacks (a broken cue). Adding the patch mends it;
    // undo must still take the patch away again, exactly.
    let mut ws = workspace(vec![audio_on(7, 5)]);
    let mut history = History::new();
    let before = ws.clone();
    history
        .execute(
            &mut ws,
            Command::AddPatch {
                index: 1,
                patch: patch(5, 2),
            },
        )
        .unwrap();
    assert!(history.undo(&mut ws).unwrap());
    assert_eq!(ws, before);
    assert!(history.redo(&mut ws).unwrap());
    assert_eq!(ws.patches.len(), 2);
}

#[test]
fn patches_can_be_moved_and_a_move_in_place_is_not_an_edit() {
    let mut ws = workspace(vec![]);
    ws.patches.push(patch(2, 2));
    ws.patches.push(patch(3, 2));
    let moved = round_trip(
        &ws,
        Command::MovePatch {
            id: patch_id(1),
            index: 2,
        },
    );
    assert_eq!(
        moved.patches.iter().map(|p| p.id).collect::<Vec<_>>(),
        vec![patch_id(2), patch_id(3), patch_id(1)]
    );
    let mut history = History::new();
    assert!(
        !history
            .execute(
                &mut ws,
                Command::MovePatch {
                    id: patch_id(2),
                    index: 1
                }
            )
            .unwrap()
    );
    assert!(matches!(
        refused(
            &ws,
            Command::MovePatch {
                id: patch_id(1),
                index: 3
            }
        ),
        CommandError::IndexOutOfRange { index: 3, len: 2 }
    ));
    assert!(matches!(
        refused(
            &ws,
            Command::MovePatch {
                id: patch_id(9),
                index: 0
            }
        ),
        CommandError::PatchNotFound(_)
    ));
}

#[test]
fn patch_properties_can_be_edited_and_undone() {
    let ws = workspace(vec![]);
    for property in [
        PatchProperty::Name {
            value: "FOH".into(),
        },
        PatchProperty::MasterDb { value: -6.0 },
        PatchProperty::OutputLevel {
            output: 1,
            value: -3.0,
        },
        PatchProperty::OutputMute {
            output: 0,
            value: true,
        },
        PatchProperty::OutputDelay {
            output: 1,
            value: 12.5,
        },
        PatchProperty::OutputPolarity {
            output: 0,
            value: true,
        },
    ] {
        round_trip(&ws, patch_prop(1, property));
    }
}

#[test]
fn muting_an_output_keeps_its_level() {
    let ws = workspace(vec![]);
    let after = round_trip(
        &ws,
        patch_prop(
            1,
            PatchProperty::OutputMute {
                output: 0,
                value: true,
            },
        ),
    );
    assert_eq!(after.patches[0].outputs[0].level_db, 0.0);
    assert!(after.patches[0].outputs[0].mute);
}

#[test]
fn the_channel_count_grows_and_shrinks_and_undo_restores_what_shrinking_dropped() {
    let mut ws = workspace(vec![]);
    ws.patches[0].outputs[1].level_db = -9.0;
    ws.patches[0].outputs[1].delay_ms = 4.0;
    let grown = round_trip(&ws, patch_prop(1, PatchProperty::ChannelCount { value: 4 }));
    assert_eq!(grown.patches[0].outputs.len(), 4);
    assert_eq!(grown.patches[0].outputs[3].level_db, 0.0);
    assert_eq!(grown.patches[0].outputs[1].level_db, -9.0);
    let shrunk = round_trip(&ws, patch_prop(1, PatchProperty::ChannelCount { value: 1 }));
    assert_eq!(shrunk.patches[0].outputs.len(), 1);
    // Shrink, then undo: the dropped output is back with its settings.
    let mut work = ws.clone();
    let mut history = History::new();
    history
        .execute(
            &mut work,
            patch_prop(1, PatchProperty::ChannelCount { value: 1 }),
        )
        .unwrap();
    history.undo(&mut work).unwrap();
    assert_eq!(work.patches[0].outputs[1].delay_ms, 4.0);
}

#[test]
fn shrinking_a_patch_leaves_cue_routing_to_the_dropped_outputs() {
    let ws = workspace(vec![audio_on(7, 1)]);
    let shrunk = round_trip(&ws, patch_prop(1, PatchProperty::ChannelCount { value: 1 }));
    // Routing beyond the patch is kept on purpose (silent at runtime): only a warning.
    assert_eq!(shrunk.cue_lists[0].cues, ws.cue_lists[0].cues);
}

#[test]
fn bad_patch_edits_are_refused() {
    let ws = workspace(vec![]);
    for (property, want) in [
        (PatchProperty::ChannelCount { value: 0 }, "value"),
        (PatchProperty::ChannelCount { value: 65 }, "value"),
        (PatchProperty::ChannelCount { value: usize::MAX }, "value"),
        (PatchProperty::Outputs { value: vec![] }, "model"),
        (
            PatchProperty::Outputs {
                value: vec![patch_output(); 65],
            },
            "model",
        ),
        (PatchProperty::MasterDb { value: 20.0 }, "model"),
        (PatchProperty::MasterDb { value: f64::NAN }, "model"),
        (
            PatchProperty::OutputLevel {
                output: 0,
                value: 13.0,
            },
            "model",
        ),
        (
            PatchProperty::OutputDelay {
                output: 0,
                value: 1000.5,
            },
            "model",
        ),
        (
            PatchProperty::OutputDelay {
                output: 0,
                value: -1.0,
            },
            "model",
        ),
        (
            PatchProperty::OutputLevel {
                output: 2,
                value: 0.0,
            },
            "range",
        ),
        (
            PatchProperty::OutputMute {
                output: usize::MAX,
                value: true,
            },
            "range",
        ),
    ] {
        let error = refused(&ws, patch_prop(1, property.clone()));
        let kind = match error {
            CommandError::InvalidValue(_) => "value",
            CommandError::InvalidModel(_) => "model",
            CommandError::PatchOutputOutOfRange { .. } => "range",
            other => panic!("{property:?}: {other}"),
        };
        assert_eq!(kind, want, "{property:?}");
    }
    assert!(matches!(
        refused(
            &ws,
            patch_prop(9, PatchProperty::Name { value: "x".into() })
        ),
        CommandError::PatchNotFound(_)
    ));
}

#[test]
fn a_patch_edit_to_the_same_value_is_not_an_edit() {
    let mut ws = workspace(vec![]);
    let mut history = History::new();
    for property in [
        PatchProperty::Name {
            value: "Main".into(),
        },
        PatchProperty::ChannelCount { value: 2 },
        PatchProperty::OutputLevel {
            output: 0,
            value: 0.0,
        },
        PatchProperty::Outputs {
            value: vec![patch_output(); 2],
        },
    ] {
        assert!(
            !history
                .execute(&mut ws, patch_prop(1, property.clone()))
                .unwrap(),
            "{property:?}"
        );
    }
    assert!(!history.can_undo());
}

// ---- Network patches ----

#[test]
fn network_patches_can_be_added_removed_moved_and_edited() {
    let mut ws = workspace(vec![]);
    ws.network_patches.push(net(2));
    let added = round_trip(
        &ws,
        Command::AddNetworkPatch {
            index: 1,
            patch: net(3),
        },
    );
    assert_eq!(added.network_patches[1].id, net_id(3));
    let removed = round_trip(
        &ws,
        Command::RemoveNetworkPatch {
            id: net_id(2),
            even_if_used: false,
        },
    );
    assert_eq!(removed.network_patches.len(), 1);
    let moved = round_trip(
        &ws,
        Command::MoveNetworkPatch {
            id: net_id(1),
            index: 1,
        },
    );
    assert_eq!(moved.network_patches[1].id, net_id(1));
    for property in [
        NetworkPatchProperty::Name {
            value: "Lights".into(),
        },
        NetworkPatchProperty::Host {
            value: "192.168.1.9".into(),
        },
        NetworkPatchProperty::Port { value: 9000 },
        NetworkPatchProperty::Protocol {
            value: NetworkProtocol::Tcp,
        },
    ] {
        round_trip(
            &ws,
            Command::SetNetworkPatchProperty {
                id: net_id(1),
                property,
            },
        );
    }
}

#[test]
fn bad_network_patch_edits_are_refused_or_ignored() {
    let ws = workspace(vec![network_cue(7, 1)]);
    assert!(matches!(
        refused(
            &ws,
            Command::AddNetworkPatch {
                index: 0,
                patch: net(1)
            }
        ),
        CommandError::DuplicateNetworkPatchId(_)
    ));
    assert!(matches!(
        refused(
            &ws,
            Command::AddNetworkPatch {
                index: 2,
                patch: net(2)
            }
        ),
        CommandError::IndexOutOfRange { .. }
    ));
    assert_eq!(
        refused(
            &ws,
            Command::RemoveNetworkPatch {
                id: net_id(1),
                even_if_used: false
            }
        ),
        CommandError::NetworkPatchInUse {
            patch: net_id(1),
            cue: cue_id(7)
        }
    );
    assert!(matches!(
        refused(
            &ws,
            Command::SetNetworkPatchProperty {
                id: net_id(9),
                property: NetworkPatchProperty::Port { value: 1 }
            }
        ),
        CommandError::NetworkPatchNotFound(_)
    ));
    // The same value is no edit.
    let mut work = ws.clone();
    let mut history = History::new();
    let same = Command::SetNetworkPatchProperty {
        id: net_id(1),
        property: NetworkPatchProperty::Port { value: 53000 },
    };
    assert!(!history.execute(&mut work, same).unwrap());
}

// ---- Cue lists ----

#[test]
fn cue_lists_can_be_added_renamed_moved_and_removed() {
    let ws = workspace(vec![cue(1, CueKind::Memo)]);
    let added = round_trip(
        &ws,
        Command::AddCueList {
            index: 0,
            id: list_id(2),
            name: "Understudy".into(),
        },
    );
    assert_eq!(added.cue_lists[0].id, list_id(2));
    assert!(added.cue_lists[0].cues.is_empty());
    let renamed = round_trip(
        &ws,
        Command::RenameCueList {
            id: list_id(1),
            name: "Act 1".into(),
        },
    );
    assert_eq!(renamed.cue_lists[0].name, "Act 1");
    // Two lists: move and remove the empty one.
    let two = added;
    let moved = round_trip(
        &two,
        Command::MoveCueList {
            id: list_id(2),
            index: 1,
        },
    );
    assert_eq!(moved.cue_lists[1].id, list_id(2));
    let removed = round_trip(&two, Command::RemoveCueList { id: list_id(2) });
    assert_eq!(removed, ws);
}

#[test]
fn a_cue_list_with_cues_cannot_be_removed() {
    let mut ws = workspace(vec![cue(1, CueKind::Memo)]);
    ws.cue_lists.push(CueList {
        id: list_id(2),
        name: "B".into(),
        cues: vec![],
    });
    assert_eq!(
        refused(&ws, Command::RemoveCueList { id: list_id(1) }),
        CommandError::CueListNotEmpty(list_id(1))
    );
    // Move the cue out first, as one step with the removal.
    let command = Command::Transaction {
        commands: vec![
            Command::ReparentCue {
                id: cue_id(1),
                to: Position {
                    list: list_id(2),
                    parent: None,
                    index: 0,
                },
            },
            Command::RemoveCueList { id: list_id(1) },
        ],
    };
    let after = round_trip(&ws, command);
    assert_eq!(after.cue_lists.len(), 1);
    assert_eq!(after.cue_lists[0].cues.len(), 1);
}

#[test]
fn the_last_cue_list_cannot_be_removed() {
    let ws = workspace(vec![]);
    assert_eq!(
        refused(&ws, Command::RemoveCueList { id: list_id(1) }),
        CommandError::LastCueList(list_id(1))
    );
}

#[test]
fn bad_cue_list_edits_are_refused() {
    let ws = workspace(vec![]);
    assert!(matches!(
        refused(
            &ws,
            Command::AddCueList {
                index: 0,
                id: list_id(1),
                name: "x".into()
            }
        ),
        CommandError::DuplicateCueListId(_)
    ));
    assert!(matches!(
        refused(
            &ws,
            Command::AddCueList {
                index: 5,
                id: list_id(2),
                name: "x".into()
            }
        ),
        CommandError::IndexOutOfRange { index: 5, len: 1 }
    ));
    for command in [
        Command::RemoveCueList { id: list_id(9) },
        Command::RenameCueList {
            id: list_id(9),
            name: "x".into(),
        },
        Command::MoveCueList {
            id: list_id(9),
            index: 0,
        },
    ] {
        assert!(matches!(
            refused(&ws, command),
            CommandError::CueListNotFound(_)
        ));
    }
    assert!(matches!(
        refused(
            &ws,
            Command::MoveCueList {
                id: list_id(1),
                index: 1
            }
        ),
        CommandError::IndexOutOfRange { index: 1, len: 0 }
    ));
    let mut work = ws.clone();
    let mut history = History::new();
    let same = Command::RenameCueList {
        id: list_id(1),
        name: "Main".into(),
    };
    assert!(!history.execute(&mut work, same).unwrap());
}

// ---- Transactions mixing everything ----

#[test]
fn one_step_can_change_settings_patches_lists_and_cues_together() {
    let mut ws = workspace(vec![cue(1, CueKind::Memo)]);
    let before = ws.clone();
    let mut history = History::new();
    let command = Command::Transaction {
        commands: vec![
            Command::AddPatch {
                index: 1,
                patch: patch(2, 8),
            },
            patch_prop(
                2,
                PatchProperty::Name {
                    value: "Sub".into(),
                },
            ),
            Command::AddCueList {
                index: 1,
                id: list_id(2),
                name: "B".into(),
            },
            setting(WorkspaceSetting::MasterDb { value: -3.0 }),
            Command::SetCueProperty {
                id: cue_id(1),
                property: CueProperty::Name { value: "x".into() },
            },
        ],
    };
    history.execute(&mut ws, command).unwrap();
    let after = ws.clone();
    assert_eq!(history.undo_len(), 1);
    history.undo(&mut ws).unwrap();
    assert_eq!(ws, before);
    history.redo(&mut ws).unwrap();
    assert_eq!(ws, after);
    // A refused command in the middle undoes the rest.
    let bad = Command::Transaction {
        commands: vec![
            setting(WorkspaceSetting::MasterDb { value: -9.0 }),
            Command::RemovePatch {
                id: patch_id(9),
                even_if_used: false,
            },
        ],
    };
    let error = history.execute(&mut ws, bad).unwrap_err();
    assert!(matches!(
        error,
        CommandError::InTransaction { index: 1, .. }
    ));
    assert_eq!(ws, after);
}

// ---- Coalescing ----

fn level(db: f64) -> Command {
    setting(WorkspaceSetting::MasterDb { value: db })
}

#[test]
fn edits_with_the_same_key_are_one_undo_step() {
    let mut ws = workspace(vec![]);
    let mut history = History::new();
    for db in [-1.0, -2.0, -3.0, -4.0] {
        assert!(
            history
                .execute_coalescing(&mut ws, level(db), Some("drag"))
                .unwrap()
        );
    }
    assert_eq!(ws.settings.master_db, -4.0);
    assert_eq!(history.undo_len(), 1);
    // The merged step is still one command: a long drag does not grow the history.
    assert!(matches!(
        history.peek_undo(),
        Some(Command::SetWorkspaceSetting { .. })
    ));
    assert!(history.undo(&mut ws).unwrap());
    assert_eq!(ws.settings.master_db, 0.0);
    assert!(!history.can_undo());
    assert!(history.redo(&mut ws).unwrap());
    assert_eq!(ws.settings.master_db, -4.0);
}

#[test]
fn a_different_key_or_none_starts_a_new_step() {
    let mut ws = workspace(vec![]);
    let mut history = History::new();
    history
        .execute_coalescing(&mut ws, level(-1.0), Some("a"))
        .unwrap();
    history
        .execute_coalescing(&mut ws, level(-2.0), Some("b"))
        .unwrap();
    history
        .execute_coalescing(&mut ws, level(-3.0), None)
        .unwrap();
    history.execute(&mut ws, level(-4.0)).unwrap();
    assert_eq!(history.undo_len(), 4);
    // Without a key, nothing merges, even the same value being dragged.
    history
        .execute_coalescing(&mut ws, level(-5.0), Some("c"))
        .unwrap();
    history.execute(&mut ws, level(-6.0)).unwrap();
    history
        .execute_coalescing(&mut ws, level(-7.0), Some("c"))
        .unwrap();
    assert_eq!(history.undo_len(), 7);
}

#[test]
fn undo_and_redo_end_a_gesture() {
    let mut ws = workspace(vec![]);
    let mut history = History::new();
    history
        .execute_coalescing(&mut ws, level(-1.0), Some("k"))
        .unwrap();
    history
        .execute_coalescing(&mut ws, level(-2.0), Some("k"))
        .unwrap();
    history.undo(&mut ws).unwrap();
    history
        .execute_coalescing(&mut ws, level(-3.0), Some("k"))
        .unwrap();
    // The undo closed the gesture: the new edit is its own step and clears redo.
    assert_eq!(history.undo_len(), 1);
    assert!(!history.can_redo());
    history.clear();
    history
        .execute_coalescing(&mut ws, level(-4.0), Some("k"))
        .unwrap();
    history.undo(&mut ws).unwrap();
    history.redo(&mut ws).unwrap();
    history
        .execute_coalescing(&mut ws, level(-5.0), Some("k"))
        .unwrap();
    assert_eq!(history.undo_len(), 2);
}

#[test]
fn clearing_the_history_forgets_an_open_gesture() {
    let mut ws = workspace(vec![]);
    let mut history = History::new();
    history
        .execute_coalescing(&mut ws, level(-1.0), Some("k"))
        .unwrap();
    history.clear();
    history
        .execute_coalescing(&mut ws, level(-2.0), Some("k"))
        .unwrap();
    assert_eq!(history.undo_len(), 1);
    history.undo(&mut ws).unwrap();
    // Only the second edit was undone: the history never reached back before the clear.
    assert_eq!(ws.settings.master_db, -1.0);
}

#[test]
fn a_no_op_edit_leaves_a_gesture_open() {
    let mut ws = workspace(vec![]);
    let mut history = History::new();
    history
        .execute_coalescing(&mut ws, level(-1.0), Some("k"))
        .unwrap();
    assert!(
        !history
            .execute_coalescing(&mut ws, level(-1.0), Some("k"))
            .unwrap()
    );
    history
        .execute_coalescing(&mut ws, level(-2.0), Some("k"))
        .unwrap();
    assert_eq!(history.undo_len(), 1);
}

#[test]
fn a_refused_edit_changes_neither_the_workspace_nor_the_gesture() {
    let mut ws = workspace(vec![]);
    let mut history = History::new();
    history
        .execute_coalescing(&mut ws, level(-1.0), Some("k"))
        .unwrap();
    assert!(
        history
            .execute_coalescing(&mut ws, level(99.0), Some("k"))
            .is_err()
    );
    assert_eq!(ws.settings.master_db, -1.0);
    history
        .execute_coalescing(&mut ws, level(-2.0), Some("k"))
        .unwrap();
    assert_eq!(history.undo_len(), 1);
}

#[test]
fn a_gesture_over_different_values_chains_and_undoes_in_reverse() {
    let mut ws = workspace(vec![cue(1, CueKind::Memo)]);
    let before = ws.clone();
    let mut history = History::new();
    let commands = [
        level(-1.0),
        patch_prop(1, PatchProperty::MasterDb { value: -2.0 }),
        Command::SetCueProperty {
            id: cue_id(1),
            property: CueProperty::Name { value: "n".into() },
        },
        level(-3.0),
        patch_prop(1, PatchProperty::MasterDb { value: -4.0 }),
        patch_prop(1, PatchProperty::MasterDb { value: -5.0 }),
    ];
    for command in commands {
        history
            .execute_coalescing(&mut ws, command, Some("multi"))
            .unwrap();
    }
    let after = ws.clone();
    assert_eq!(history.undo_len(), 1);
    assert!(history.undo(&mut ws).unwrap());
    assert_eq!(ws, before);
    assert!(history.redo(&mut ws).unwrap());
    assert_eq!(ws, after);
}

#[test]
fn a_cue_level_drag_coalesces_into_one_step() {
    let mut ws = workspace(vec![cue(1, audio())]);
    let before = ws.clone();
    let mut history = History::new();
    for db in [-1.0, -2.0, -3.0] {
        let mut kind = audio();
        if let CueKind::Audio { levels, .. } = &mut kind {
            levels.master_db = db;
        }
        history
            .execute_coalescing(
                &mut ws,
                Command::SetCueProperty {
                    id: cue_id(1),
                    property: CueProperty::Kind { value: kind },
                },
                Some("fader:1"),
            )
            .unwrap();
    }
    assert_eq!(history.undo_len(), 1);
    history.undo(&mut ws).unwrap();
    assert_eq!(ws, before);
}

#[test]
fn moves_in_one_gesture_undo_to_where_the_cue_started() {
    let cues = (1..=5).map(|n| cue(n, CueKind::Memo)).collect();
    let mut ws = workspace(cues);
    let before = ws.clone();
    let mut history = History::new();
    for index in [1, 2, 4, 3] {
        history
            .execute_coalescing(
                &mut ws,
                Command::MoveCue {
                    id: cue_id(1),
                    index,
                },
                Some("drag"),
            )
            .unwrap();
    }
    assert_eq!(history.undo_len(), 1);
    history.undo(&mut ws).unwrap();
    assert_eq!(ws, before);
}

#[test]
fn an_edit_of_another_cue_in_the_gesture_is_not_collapsed() {
    let mut ws = workspace(vec![cue(1, CueKind::Memo), cue(2, CueKind::Memo)]);
    let before = ws.clone();
    let mut history = History::new();
    let name = |id, v: &str| Command::SetCueProperty {
        id: cue_id(id),
        property: CueProperty::Name { value: v.into() },
    };
    for command in [name(1, "a"), name(2, "b"), name(1, "c")] {
        history
            .execute_coalescing(&mut ws, command, Some("k"))
            .unwrap();
    }
    history.undo(&mut ws).unwrap();
    assert_eq!(ws, before, "both cues are back, not just the last one");
}

// ---- model_changed ----

fn mirror_step(ws: &mut Workspace, mirror: &mut Workspace, history: &mut History, c: Command) {
    let footprint = c.footprint();
    assert!(history.execute(ws, c).unwrap());
    let (upserts, deletes) = model_changes(ws, &footprint);
    mirror.apply_changes(&upserts, &deletes).unwrap();
    assert_eq!(mirror, ws);
}

fn mirror_undo_redo(ws: &mut Workspace, mirror: &mut Workspace, history: &mut History, undo: bool) {
    let footprint = if undo {
        history.peek_undo()
    } else {
        history.peek_redo()
    }
    .unwrap()
    .footprint();
    if undo {
        history.undo(ws).unwrap();
    } else {
        history.redo(ws).unwrap();
    }
    let (upserts, deletes) = model_changes(ws, &footprint);
    mirror.apply_changes(&upserts, &deletes).unwrap();
    assert_eq!(mirror, ws);
}

#[test]
fn a_client_following_model_changed_holds_the_workspace_after_model_edits() {
    let mut ws = workspace(vec![cue(1, CueKind::Memo)]);
    ws.patches.push(patch(2, 2));
    ws.patches.push(patch(3, 2));
    ws.network_patches.push(net(2));
    let (mut mirror, mut history) = (ws.clone(), History::new());
    let commands = vec![
        level(-3.0),
        add_pass(0, "4821", rights(true, true, false)),
        Command::AddPatch {
            index: 0,
            patch: patch(4, 4),
        },
        Command::MovePatch {
            id: patch_id(4),
            index: 3,
        },
        Command::MovePatch {
            id: patch_id(1),
            index: 2,
        },
        patch_prop(2, PatchProperty::Name { value: "Z".into() }),
        Command::RemovePatch {
            id: patch_id(3),
            even_if_used: false,
        },
        Command::AddNetworkPatch {
            index: 0,
            patch: net(3),
        },
        Command::MoveNetworkPatch {
            id: net_id(1),
            index: 2,
        },
        Command::SetNetworkPatchProperty {
            id: net_id(2),
            property: NetworkPatchProperty::Port { value: 1 },
        },
        Command::RemoveNetworkPatch {
            id: net_id(3),
            even_if_used: false,
        },
        Command::AddCueList {
            index: 0,
            id: list_id(2),
            name: "B".into(),
        },
        Command::AddCueList {
            index: 2,
            id: list_id(3),
            name: "C".into(),
        },
        Command::MoveCueList {
            id: list_id(3),
            index: 0,
        },
        Command::RenameCueList {
            id: list_id(2),
            name: "BB".into(),
        },
        Command::ReparentCue {
            id: cue_id(1),
            to: Position {
                list: list_id(2),
                parent: None,
                index: 0,
            },
        },
        Command::RemoveCueList { id: list_id(1) },
    ];
    for command in commands {
        mirror_step(&mut ws, &mut mirror, &mut history, command);
    }
    while history.can_undo() {
        mirror_undo_redo(&mut ws, &mut mirror, &mut history, true);
    }
    while history.can_redo() {
        mirror_undo_redo(&mut ws, &mut mirror, &mut history, false);
    }
}

#[test]
fn model_changed_for_model_edits_names_just_what_changed() {
    let mut ws = workspace(vec![cue(1, CueKind::Memo)]);
    ws.patches.push(patch(2, 2));
    let mut history = History::new();
    let sent = |ws: &Workspace, c: &Command| model_changes(ws, &c.footprint());

    let c = level(-3.0);
    history.execute(&mut ws, c.clone()).unwrap();
    let (upserts, deletes) = sent(&ws, &c);
    assert!(deletes.is_empty());
    assert!(
        matches!(upserts.as_slice(), [Upsert::Settings { settings, .. }] if settings.master_db == -3.0)
    );

    let c = patch_prop(
        2,
        PatchProperty::Name {
            value: "Sub".into(),
        },
    );
    history.execute(&mut ws, c.clone()).unwrap();
    let (upserts, deletes) = sent(&ws, &c);
    assert!(deletes.is_empty());
    assert!(matches!(
        upserts.as_slice(),
        [Upsert::Patch { index: 1, patch }] if patch.name == "Sub"
    ));

    let c = Command::RemovePatch {
        id: patch_id(2),
        even_if_used: false,
    };
    history.execute(&mut ws, c.clone()).unwrap();
    assert_eq!(
        sent(&ws, &c),
        (vec![], vec![Delete::Patch { id: patch_id(2) }])
    );

    let c = Command::RenameCueList {
        id: list_id(1),
        name: "Z".into(),
    };
    history.execute(&mut ws, c.clone()).unwrap();
    assert_eq!(
        sent(&ws, &c),
        (
            vec![Upsert::CueList {
                id: list_id(1),
                name: "Z".into(),
                index: 0
            }],
            vec![]
        )
    );
}

#[test]
fn moving_a_cue_list_keeps_its_cues_on_the_client() {
    let mut ws = workspace(vec![cue(1, CueKind::Memo), cue(2, CueKind::Memo)]);
    let (mut mirror, mut history) = (ws.clone(), History::new());
    mirror_step(
        &mut ws,
        &mut mirror,
        &mut history,
        Command::AddCueList {
            index: 1,
            id: list_id(2),
            name: "B".into(),
        },
    );
    mirror_step(
        &mut ws,
        &mut mirror,
        &mut history,
        Command::MoveCueList {
            id: list_id(2),
            index: 0,
        },
    );
    assert_eq!(mirror.cue_lists[1].cues.len(), 2);
}

#[test]
fn footprints_say_when_an_edit_is_more_than_cues() {
    let cue_edit = Command::SetCueProperty {
        id: cue_id(1),
        property: CueProperty::Armed { value: false },
    };
    assert!(!cue_edit.footprint().is_model_wide());
    for command in [
        level(-1.0),
        Command::RemovePatch {
            id: patch_id(1),
            even_if_used: false,
        },
        Command::MoveNetworkPatch {
            id: net_id(1),
            index: 0,
        },
        Command::RemoveCueList { id: list_id(1) },
        Command::Transaction {
            commands: vec![cue_edit.clone(), level(-1.0)],
        },
    ] {
        let footprint = command.footprint();
        assert!(footprint.is_model_wide(), "{command:?}");
        assert!(!footprint.is_empty());
    }
    assert!(
        Command::Transaction { commands: vec![] }
            .footprint()
            .is_empty()
    );
}

// ---- Review fixes ----

fn with_bad_hotkey() -> Workspace {
    let mut ws = workspace(vec![]);
    ws.settings.hotkeys.go = Some(Hotkey {
        key: "Space".into(),
        modifiers: Modifiers::default(),
    });
    ws
}

#[test]
fn a_warning_the_show_was_loaded_with_does_not_block_settings_or_undo() {
    let mut ws = with_bad_hotkey();
    let mut history = History::new();
    // (a) Another setting is not held up by it.
    assert!(history.execute(&mut ws, level(-3.0)).unwrap());
    // (b) Fixing the key and undoing the fix are both accepted.
    let fix = setting(WorkspaceSetting::Hotkey {
        key: TransportKey::Go,
        value: Some(Hotkey {
            key: "space".into(),
            modifiers: Modifiers::default(),
        }),
    });
    assert!(history.execute(&mut ws, fix).unwrap());
    assert!(history.undo(&mut ws).unwrap());
    assert_eq!(ws.settings.hotkeys.go.as_ref().unwrap().key, "Space");
    assert!(history.undo(&mut ws).unwrap());
    assert_eq!(ws.settings.master_db, 0.0);
    assert!(history.redo(&mut ws).unwrap());
    assert!(history.redo(&mut ws).unwrap());
    assert_eq!(ws.settings.hotkeys.go.as_ref().unwrap().key, "space");
    // What the edit itself introduces is still refused, next to the old warning.
    let error = refused(
        &with_bad_hotkey(),
        setting(WorkspaceSetting::PanicDuration { value: 99.0 }),
    );
    assert!(matches!(error, CommandError::InvalidModel(_)));
    let error = refused(
        &with_bad_hotkey(),
        setting(WorkspaceSetting::MasterDb {
            value: SILENCE_DB - 5.0,
        }),
    );
    assert!(matches!(error, CommandError::InvalidModel(_)));
}

#[test]
fn port_zero_is_refused() {
    let ws = workspace(vec![]);
    assert!(matches!(
        refused(
            &ws,
            Command::SetNetworkPatchProperty {
                id: net_id(1),
                property: NetworkPatchProperty::Port { value: 0 }
            }
        ),
        CommandError::InvalidValue(_)
    ));
    let mut zero = net(2);
    zero.port = 0;
    assert!(matches!(
        refused(
            &ws,
            Command::AddNetworkPatch {
                index: 0,
                patch: zero
            }
        ),
        CommandError::InvalidValue(_)
    ));
    round_trip(
        &ws,
        Command::SetNetworkPatchProperty {
            id: net_id(1),
            property: NetworkPatchProperty::Port { value: 65535 },
        },
    );
}

/// A multi-cue level drag: one `Kind` per selected cue, at every event.
fn drag(cues: u128, step: u32) -> Command {
    Command::Transaction {
        commands: (1..=cues)
            .map(|n| {
                let mut kind = audio();
                if let CueKind::Audio { levels, .. } = &mut kind {
                    levels.master_db = -f64::from(step % 50);
                }
                Command::SetCueProperty {
                    id: cue_id(n),
                    property: CueProperty::Kind { value: kind },
                }
            })
            .collect(),
    }
}

#[test]
fn a_multi_cue_gesture_stays_one_command_per_cue() {
    let mut ws = workspace((1..=40).map(|n| cue(n, audio())).collect());
    let before = ws.clone();
    let mut history = History::new();
    for step in 1..=600 {
        history
            .execute_coalescing(&mut ws, drag(40, step), Some("drag"))
            .unwrap();
    }
    assert_eq!(history.undo_len(), 1);
    let Some(Command::Transaction { commands }) = history.peek_undo() else {
        panic!("not a transaction");
    };
    assert_eq!(commands.len(), 40);
    let after = ws.clone();
    history.undo(&mut ws).unwrap();
    assert_eq!(ws, before);
    history.redo(&mut ws).unwrap();
    assert_eq!(ws, after);
}

#[test]
fn a_gesture_that_adds_new_slots_midway_keeps_every_slot_once() {
    let mut ws = workspace((1..=3).map(|n| cue(n, CueKind::Memo)).collect());
    let before = ws.clone();
    let mut history = History::new();
    let name = |id, v: &str| Command::SetCueProperty {
        id: cue_id(id),
        property: CueProperty::Name { value: v.into() },
    };
    let steps = [
        Command::Transaction {
            commands: vec![name(1, "a"), name(2, "a")],
        },
        Command::Transaction {
            commands: vec![name(2, "b"), name(3, "b")],
        },
        Command::Transaction {
            commands: vec![name(1, "c"), name(3, "c"), level(-1.0)],
        },
    ];
    for step in steps {
        history
            .execute_coalescing(&mut ws, step, Some("k"))
            .unwrap();
    }
    let Some(Command::Transaction { commands }) = history.peek_undo() else {
        panic!("not a transaction");
    };
    assert_eq!(commands.len(), 4);
    history.undo(&mut ws).unwrap();
    assert_eq!(ws, before);
}

#[test]
fn a_merged_step_touching_several_lists_keeps_a_client_in_order() {
    // Add L3, remove L1, add L1 again at the end: one gesture. Undoing and redoing it touches
    // L1 and L3, and upserting only those two would leave a client with [L3, L2, L1].
    let mut ws = workspace(vec![]);
    ws.cue_lists.push(CueList {
        id: list_id(2),
        name: "Second".into(),
        cues: vec![],
    });
    let (mut mirror, mut history) = (ws.clone(), History::new());
    let steps = [
        Command::AddCueList {
            index: 2,
            id: list_id(3),
            name: "C".into(),
        },
        Command::RemoveCueList { id: list_id(1) },
        Command::AddCueList {
            index: 2,
            id: list_id(1),
            name: "Main".into(),
        },
    ];
    for step in steps {
        let footprint = step.footprint();
        history
            .execute_coalescing(&mut ws, step, Some("g"))
            .unwrap();
        let (upserts, deletes) = model_changes(&ws, &footprint);
        mirror.apply_changes(&upserts, &deletes).unwrap();
        assert_eq!(mirror, ws);
    }
    assert_eq!(history.undo_len(), 1);
    mirror_undo_redo(&mut ws, &mut mirror, &mut history, true);
    mirror_undo_redo(&mut ws, &mut mirror, &mut history, false);
    assert_eq!(
        mirror.cue_lists.iter().map(|l| l.id).collect::<Vec<_>>(),
        vec![list_id(2), list_id(3), list_id(1)]
    );
}

#[test]
fn a_new_invalid_transport_hotkey_is_refused_but_an_undo_may_restore_one() {
    for key in ["Not A Key", "Space", ""] {
        let error = refused(
            &workspace(vec![]),
            setting(WorkspaceSetting::Hotkey {
                key: TransportKey::Go,
                value: Some(Hotkey {
                    key: key.into(),
                    modifiers: Modifiers::default(),
                }),
            }),
        );
        assert!(matches!(error, CommandError::InvalidModel(_)), "{key:?}");
    }
    // (b) of the settings case again: undo restores the bad loaded key.
    let mut ws = with_bad_hotkey();
    let mut history = History::new();
    let fix = setting(WorkspaceSetting::Hotkey {
        key: TransportKey::Go,
        value: Some(Hotkey {
            key: "space".into(),
            modifiers: Modifiers::default(),
        }),
    });
    history.execute(&mut ws, fix).unwrap();
    assert!(history.undo(&mut ws).unwrap());
    assert!(history.redo(&mut ws).unwrap());
}

#[test]
fn undo_is_not_stuck_on_a_cue_trigger_warning_the_show_was_loaded_with() {
    let hotkey = |key: &str| Triggers {
        hotkey: Some(Hotkey {
            key: key.into(),
            modifiers: Modifiers::default(),
        }),
        osc: None,
    };
    let mut loaded = cue(1, CueKind::Memo);
    loaded.triggers = hotkey("Space");
    let mut ws = workspace(vec![loaded]);
    let before = ws.clone();
    let mut history = History::new();
    let set = |t: Triggers| Command::SetCueProperty {
        id: cue_id(1),
        property: CueProperty::Triggers { value: t },
    };
    history.execute(&mut ws, set(hotkey("space"))).unwrap();
    assert!(history.undo(&mut ws).unwrap());
    assert_eq!(ws, before);
    assert!(history.redo(&mut ws).unwrap());
    // Setting a new bad name is still refused.
    assert!(history.execute(&mut ws, set(hotkey("Bad Key"))).is_err());
}

fn order(ws: &Workspace) -> Vec<CueId> {
    ws.cue_lists[0].cues.iter().map(|c| c.id).collect()
}

#[test]
fn a_multi_move_step_after_a_single_move_undoes_to_the_start() {
    let mut ws = workspace((1..=5).map(|n| cue(n, CueKind::Memo)).collect());
    let before = ws.clone();
    let mut history = History::new();
    let mv = |id, index| Command::MoveCue {
        id: cue_id(id),
        index,
    };
    let steps = [
        mv(3, 0),
        Command::Transaction {
            commands: vec![mv(4, 0), mv(3, 4)],
        },
    ];
    for step in steps {
        history
            .execute_coalescing(&mut ws, step, Some("g"))
            .unwrap();
    }
    history.undo(&mut ws).unwrap();
    assert_eq!(order(&ws), order(&before));
    assert_eq!(ws, before);
}

#[test]
fn a_kind_a_later_command_of_the_same_step_needs_is_not_dropped() {
    let mut start = cue(1, CueKind::Memo);
    start.kind = CueKind::Start {
        target: Some(cue_id(10)),
    };
    let mut ws = workspace(vec![start, cue(10, CueKind::Memo), cue(11, CueKind::Memo)]);
    let before = ws.clone();
    let mut history = History::new();
    let kind = |k: CueKind| Command::SetCueProperty {
        id: cue_id(1),
        property: CueProperty::Kind { value: k },
    };
    let target = |t: u128| Command::SetCueProperty {
        id: cue_id(1),
        property: CueProperty::Target {
            value: Some(cue_id(t)),
        },
    };
    let steps = [
        kind(CueKind::Start {
            target: Some(cue_id(11)),
        }),
        Command::Transaction {
            commands: vec![target(12), kind(CueKind::Memo)],
        },
    ];
    for step in steps {
        history
            .execute_coalescing(&mut ws, step, Some("g"))
            .unwrap();
    }
    assert!(history.undo(&mut ws).unwrap());
    assert_eq!(ws, before);
}

#[test]
fn a_kind_a_coalesced_media_set_needs_is_not_dropped() {
    // A Media set applies only to Audio cues, so the merged step must keep the Kind that made
    // the cue Audio again before it, or undo is refused as not applicable.
    let mut ws = workspace(vec![audio_on(1, 1)]);
    let before = ws.clone();
    let mut history = History::new();
    let kind = |k: CueKind| Command::SetCueProperty {
        id: cue_id(1),
        property: CueProperty::Kind { value: k },
    };
    let audio = audio_on(1, 1).kind;
    let steps = [
        kind(CueKind::Memo),
        kind(audio),
        Command::SetCueProperty {
            id: cue_id(1),
            property: CueProperty::Media {
                value: Some(MediaRef::new("b.wav")),
            },
        },
        kind(CueKind::Memo),
    ];
    for step in steps {
        history
            .execute_coalescing(&mut ws, step, Some("g"))
            .unwrap();
    }
    assert!(history.undo(&mut ws).unwrap());
    assert_eq!(ws, before);
}

#[test]
fn a_ganged_drag_stays_bounded() {
    let mut ws = workspace(vec![]);
    let before = ws.clone();
    let mut history = History::new();
    for event in 0..20_000u32 {
        let db = -f64::from(event % 60);
        let step = Command::Transaction {
            commands: vec![
                patch_prop(
                    1,
                    PatchProperty::OutputLevel {
                        output: 0,
                        value: db,
                    },
                ),
                patch_prop(
                    1,
                    PatchProperty::OutputLevel {
                        output: 1,
                        value: db,
                    },
                ),
            ],
        };
        history
            .execute_coalescing(&mut ws, step, Some("gang"))
            .unwrap();
    }
    let Some(Command::Transaction { commands }) = history.peek_undo() else {
        panic!("not a transaction");
    };
    assert!(commands.len() <= 2, "{}", commands.len());
    history.undo(&mut ws).unwrap();
    assert_eq!(ws, before);
    // Two settings at once, too.
    let mut history = History::new();
    for event in 0..1000u32 {
        let step = Command::Transaction {
            commands: vec![
                level(-f64::from(event % 50)),
                setting(WorkspaceSetting::PanicDuration {
                    value: f64::from(event % 50) / 10.0,
                }),
            ],
        };
        history
            .execute_coalescing(&mut ws, step, Some("two"))
            .unwrap();
    }
    assert!(
        matches!(history.peek_undo(), Some(Command::Transaction { commands }) if commands.len() <= 2)
    );
}

#[test]
fn a_hotkey_being_set_is_judged_on_its_own_value() {
    // The loaded show already has a bad key on Go; setting Go to another bad one is refused,
    // while an unrelated setting still is not held up by it.
    let ws = with_bad_hotkey();
    for key in ["Space", "Other Bad"] {
        let error = refused(
            &ws,
            setting(WorkspaceSetting::Hotkey {
                key: TransportKey::Go,
                value: Some(Hotkey {
                    key: key.into(),
                    modifiers: Modifiers::default(),
                }),
            }),
        );
        assert!(matches!(error, CommandError::InvalidModel(_)), "{key}");
    }
    let mut work = ws.clone();
    assert!(History::new().execute(&mut work, level(-3.0)).unwrap());
}
