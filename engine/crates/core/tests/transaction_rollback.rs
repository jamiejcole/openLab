// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! A refused transaction is rolled back with the inverses of the commands it already applied
//!, not with a copy of the workspace: these check that every kind of command it can hold
//! comes back exactly, and what the history does around it.

mod common;

use common::*;
use openlab_core::*;

fn pos(parent: Option<u128>, index: usize) -> Position {
    Position {
        list: list_id(1),
        parent: parent.map(cue_id),
        index,
    }
}

fn memo(n: u128) -> Cue {
    Cue {
        name: format!("memo {n}"),
        ..cue(n, CueKind::Memo)
    }
}

fn add(at: Position, cue: Cue) -> Command {
    Command::AddCue {
        at,
        cue,
        auto_number: false,
    }
}

fn set(id: u128, property: CueProperty) -> Command {
    Command::SetCueProperty {
        id: cue_id(id),
        property,
    }
}

fn bytes(ws: &Workspace) -> String {
    serde_json::to_string(ws).unwrap()
}

/// 1, 2[3, 4[5]], 6 (numbered), three lists (the middle one empty), two patches, two network
/// patches, a passcode.
fn sample() -> Workspace {
    let mut ws = workspace(vec![
        numbered(1, "1", CueKind::Memo),
        cue(
            2,
            group(vec![
                memo(3),
                cue(4, group(vec![numbered(5, "5", audio())])),
            ]),
        ),
        numbered(6, "6", audio()),
    ]);
    ws.patches.push(patch(2, 4));
    ws.network_patches.push(NetworkPatch {
        id: net_id(3),
        name: "Video".into(),
        host: "10.0.0.3".into(),
        port: 9000,
        protocol: NetworkProtocol::Udp,
    });
    ws.cue_lists.push(CueList {
        id: list_id(4),
        name: "Empty".into(),
        cues: vec![],
    });
    ws.cue_lists.push(CueList {
        id: list_id(2),
        name: "Second".into(),
        cues: vec![memo(20)],
    });
    ws.apply(Command::AddOscPasscode {
        index: 0,
        passcode: "1234".into(),
        rights: OscRights {
            view: true,
            edit: false,
            control: true,
        },
    })
    .unwrap();
    ws
}

/// `command` is refused at `index` and the workspace is as before: equal, same Debug text
/// (so -0.0 and NaN count) and same bytes.
#[track_caller]
fn refused_at(ws: &mut Workspace, command: Command, index: usize) {
    let (before, debug, text) = (ws.clone(), format!("{ws:?}"), bytes(ws));
    match ws.apply(command) {
        Err(CommandError::InTransaction { index: at, .. }) => assert_eq!(at, index),
        other => panic!("expected a refusal at {index}, got {other:?}"),
    }
    assert_eq!(*ws, before);
    assert_eq!(format!("{ws:?}"), debug);
    assert_eq!(bytes(ws), text);
}

fn missing() -> Command {
    Command::RemoveCue {
        id: cue_id(987_654),
    }
}

/// Every command kind a transaction can hold, before the failing one: tree edits, a paste and
/// a batch edit (whose inverses are transactions), a no-op (an empty one), settings, the
/// workspace name, patches, network patches, cue lists and the passcode table. Not
/// `PasteClipboard` (`apply` refuses it unprepared; it reaches a transaction as a `Paste`) nor
/// a nested `Transaction` (refused; `a_nested_transaction_after_other_commands_rolls_them_back`).
fn everything() -> Vec<Command> {
    let ws = sample();
    let pasted = prepare_paste(&ws, &[ws.find_cue(cue_id(2)).unwrap().clone()], {
        let mut n = 500;
        move || {
            n += 1;
            cue_id(n)
        }
    })
    .unwrap();
    vec![
        add(pos(None, 1), memo(10)),
        Command::Paste {
            at: pos(None, 0),
            cues: pasted,
        },
        Command::RemoveCue { id: cue_id(4) },
        Command::MoveCue {
            id: cue_id(6),
            index: 0,
        },
        Command::ReparentCue {
            id: cue_id(1),
            to: pos(Some(2), 0),
        },
        Command::BatchSet {
            ids: vec![cue_id(3), cue_id(6), cue_id(3)],
            property: CueProperty::Flagged { value: true },
        },
        // Already the value: the inverse is an empty transaction.
        set(3, CueProperty::Armed { value: true }),
        Command::SetCueNumbers {
            numbers: vec![
                NumberAssignment {
                    id: cue_id(1),
                    number: Some("6".into()),
                },
                NumberAssignment {
                    id: cue_id(6),
                    number: Some("1".into()),
                },
            ],
        },
        Command::Renumber {
            ids: vec![cue_id(10), cue_id(3)],
            start: 100.0,
            increment: 10.0,
        },
        set(
            6,
            CueProperty::Kind {
                value: CueKind::Memo,
            },
        ),
        Command::SetWorkspaceSetting {
            setting: WorkspaceSetting::MasterDb { value: -6.0 },
        },
        Command::AddPatch {
            index: 0,
            patch: patch(3, 1),
        },
        Command::MovePatch {
            id: patch_id(2),
            index: 0,
        },
        Command::SetPatchProperty {
            id: patch_id(1),
            property: PatchProperty::OutputLevel {
                output: 1,
                value: -3.0,
            },
        },
        Command::RemovePatch {
            id: patch_id(1),
            even_if_used: true,
        },
        Command::AddNetworkPatch {
            index: 1,
            patch: NetworkPatch {
                id: net_id(2),
                name: "Lights".into(),
                host: "10.0.0.2".into(),
                port: 8000,
                protocol: NetworkProtocol::Udp,
            },
        },
        Command::SetNetworkPatchProperty {
            id: net_id(1),
            property: NetworkPatchProperty::Port { value: 9000 },
        },
        // A patch the sample had: the add's inverse would hide a wrong move of the added one.
        Command::MoveNetworkPatch {
            id: net_id(3),
            index: 0,
        },
        Command::RemoveNetworkPatch {
            id: net_id(1),
            even_if_used: true,
        },
        Command::AddCueList {
            index: 0,
            id: list_id(3),
            name: "Third".into(),
        },
        Command::MoveCueList {
            id: list_id(2),
            index: 0,
        },
        Command::RenameCueList {
            id: list_id(1),
            name: "Renamed".into(),
        },
        // Only an empty list can go; its inverse must put it back by name and place.
        Command::RemoveCueList { id: list_id(4) },
        Command::RenameWorkspace {
            name: "Renamed show".into(),
        },
        Command::SetOscRights {
            target: OscRightsTarget::NoPasscode,
            rights: OscRights {
                view: true,
                edit: true,
                control: false,
            },
        },
        Command::RemoveOscPasscode {
            passcode: "1234".into(),
        },
        Command::AddOscPasscode {
            index: 0,
            passcode: "99".into(),
            rights: OscRights {
                view: true,
                edit: false,
                control: false,
            },
        },
    ]
}

#[test]
fn every_kind_of_command_is_rolled_back_exactly() {
    // The prefix applies in full, so a failure after it really has everything to undo.
    let mut probe = sample();
    probe
        .apply(Command::Transaction {
            commands: everything(),
        })
        .unwrap();

    let mut ws = sample();
    let mut commands = everything();
    let len = commands.len();
    commands.push(missing());
    commands.push(add(pos(None, 0), memo(77)));
    refused_at(&mut ws, Command::Transaction { commands }, len);
}

#[test]
fn a_failure_at_every_position_rolls_back_exactly() {
    let len = everything().len();
    for at in 0..=len {
        let mut commands = everything();
        commands.insert(at, missing());
        refused_at(&mut sample(), Command::Transaction { commands }, at);
    }
}

#[test]
fn a_nested_transaction_after_other_commands_rolls_them_back() {
    let mut ws = sample();
    let command = Command::Transaction {
        commands: vec![
            add(pos(None, 0), memo(10)),
            Command::SetWorkspaceSetting {
                setting: WorkspaceSetting::PanicDuration { value: 9.0 },
            },
            Command::Transaction { commands: vec![] },
        ],
    };
    refused_at(&mut ws, command, 2);
}

/// A remove and a re-add of the same id inside the transaction: the rollback must put the
/// original cue (with its children and number) back where it was.
#[test]
fn removing_and_re_adding_an_id_rolls_back_to_the_original() {
    let mut ws = sample();
    let command = Command::Transaction {
        commands: vec![
            Command::RemoveCue { id: cue_id(2) },
            add(pos(None, 0), memo(2)),
            add(pos(None, 0), memo(4)),
            missing(),
        ],
    };
    refused_at(&mut ws, command, 3);
}

#[test]
fn an_empty_transaction_is_no_step_and_keeps_redo() {
    let mut ws = sample();
    let mut history = History::new();
    history
        .execute(&mut ws, set(3, CueProperty::Flagged { value: true }))
        .unwrap();
    history.undo(&mut ws).unwrap();
    let before = ws.clone();
    assert!(
        !history
            .execute(&mut ws, Command::Transaction { commands: vec![] })
            .unwrap()
    );
    assert_eq!(ws, before);
    assert_eq!((history.undo_len(), history.redo_len()), (0, 1));
}

/// A refused transaction is no edit: no undo step, redo kept, and a gesture in progress
/// (a fader drag) stays open, so its next value still joins its step.
#[test]
fn a_refused_transaction_leaves_the_history_alone() {
    let mut ws = sample();
    let mut history = History::new();
    let level = |db: f64| Command::SetWorkspaceSetting {
        setting: WorkspaceSetting::MasterDb { value: db },
    };
    history
        .execute(&mut ws, set(3, CueProperty::Flagged { value: true }))
        .unwrap();
    history.undo(&mut ws).unwrap();
    history
        .execute_coalescing(&mut ws, level(-1.0), Some("drag"))
        .unwrap();
    // The new edit cleared redo; record the state with a gesture open.
    let before = ws.clone();
    let mut commands = everything();
    commands.push(missing());
    assert!(matches!(
        history.execute_coalescing(&mut ws, Command::Transaction { commands }, Some("drag")),
        Err(CommandError::InTransaction { .. })
    ));
    assert_eq!(ws, before);
    assert_eq!((history.undo_len(), history.redo_len()), (1, 0));
    history
        .execute_coalescing(&mut ws, level(-2.0), Some("drag"))
        .unwrap();
    assert_eq!(history.undo_len(), 1, "the gesture was closed");
    history.undo(&mut ws).unwrap();
    assert_eq!(ws, sample());

    // Redo survives a refused transaction too.
    let redo_before = history.redo_len();
    let mut commands = vec![add(pos(None, 0), memo(10))];
    commands.push(missing());
    assert!(
        history
            .execute(&mut ws, Command::Transaction { commands })
            .is_err()
    );
    assert_eq!(history.redo_len(), redo_before);
    assert_eq!(ws, sample());
}

/// An undo step is a transaction applied in restoring mode: if it is refused partway (the
/// workspace was changed behind the history's back), the part already undone is redone by
/// rollback, and the step stays to be tried again.
#[test]
fn an_undo_refused_partway_is_rolled_back_and_kept() {
    let mut ws = sample();
    let mut history = History::new();
    history
        .execute(
            &mut ws,
            Command::Transaction {
                commands: vec![
                    add(pos(None, 0), memo(10)),
                    add(pos(None, 0), memo(11)),
                    set(3, CueProperty::Name { value: "x".into() }),
                ],
            },
        )
        .unwrap();
    // Undo removes 11 then 10 (after renaming 3 back): take 10 away so the last part fails.
    ws.apply(Command::RemoveCue { id: cue_id(10) }).unwrap();
    let before = (format!("{ws:?}"), bytes(&ws));
    assert!(matches!(
        history.undo(&mut ws),
        Err(CommandError::InTransaction { index: 2, .. })
    ));
    assert_eq!((format!("{ws:?}"), bytes(&ws)), before);
    assert_eq!((history.undo_len(), history.redo_len()), (1, 0));
}

/// Measurement, not a check (no wall-clock assertion): the cost of a transaction on a
/// 2,000-cue show, refused at its last command and applied, against cloning the show. Before
/// Both paid one clone. Run in release, where the debug-build check clone is gone:
/// `cargo test --release -p openlab-core --test transaction_rollback -- --ignored --nocapture`.
#[test]
#[ignore = "measurement; prints timings"]
fn measure_transaction_cost_on_a_large_show() {
    use std::time::Instant;
    let cues: Vec<Cue> = (0..2_000u128)
        .map(|n| Cue {
            name: format!("Cue {n} with a realistic name"),
            notes: "Stand by on the book; GO on the actor's exit.".into(),
            ..numbered(100 + n, &n.to_string(), audio())
        })
        .collect();
    let ws = workspace(cues);
    let rounds = 500u32;
    let edits = || {
        vec![
            set(1_100, CueProperty::Name { value: "x".into() }),
            Command::MoveCue {
                id: cue_id(105),
                index: 0,
            },
        ]
    };

    let mut work = ws.clone();
    let start = Instant::now();
    for _ in 0..rounds {
        let mut commands = edits();
        commands.push(missing());
        let _ = work.apply(Command::Transaction { commands });
    }
    let refused = start.elapsed() / rounds;

    let start = Instant::now();
    for _ in 0..rounds {
        let undo = work
            .apply(Command::Transaction { commands: edits() })
            .unwrap();
        work.apply(undo).unwrap();
    }
    let applied = start.elapsed() / (2 * rounds);

    let start = Instant::now();
    for _ in 0..rounds {
        std::hint::black_box(ws.clone());
    }
    let clone = start.elapsed() / rounds;
    assert_eq!(work, ws);
    println!(
        "2000 cues: refused transaction {refused:?}, applied transaction {applied:?}, \
         workspace clone {clone:?}"
    );
}

/// A loaded show may hold a cue value that load only warns about (a hotkey name in capitals).
fn with_loaded_bad_hotkey() -> Workspace {
    let mut cue_ = memo(30);
    cue_.triggers.hotkey = Some(Hotkey {
        key: "A".into(),
        modifiers: Modifiers::default(),
    });
    let mut ws = sample();
    ws.cue_lists[0].cues.push(cue_);
    let loaded = load_show(&show_text(&ws)).unwrap().workspace;
    assert!(
        loaded
            .find_cue(cue_id(30))
            .unwrap()
            .triggers
            .hotkey
            .is_some()
    );
    loaded
}

/// Undoing the removal of such a cue puts it back as it was: undo is not refused for a value
/// the state already had (it used to be, so the cue could not be brought back).
#[test]
fn undoing_a_remove_restores_a_cue_with_a_value_load_kept() {
    let mut ws = with_loaded_bad_hotkey();
    let before = ws.clone();
    let mut history = History::new();
    history
        .execute(&mut ws, Command::RemoveCue { id: cue_id(30) })
        .unwrap();
    assert!(history.undo(&mut ws).unwrap());
    assert_eq!(ws, before);
    assert!(history.redo(&mut ws).unwrap());
    assert!(ws.find_cue(cue_id(30)).is_none());

    // A new cue with that value is still refused: only putting a state back is excused.
    let mut fresh = memo(31);
    fresh.triggers = before.find_cue(cue_id(30)).unwrap().triggers.clone();
    assert!(matches!(
        ws.apply(add(pos(None, 0), fresh)),
        Err(CommandError::InvalidCue(_))
    ));
}

/// And a transaction that removed it before a refused command is rolled back with it there.
#[test]
fn a_rollback_restores_a_removed_cue_with_a_value_load_kept() {
    let mut ws = with_loaded_bad_hotkey();
    let command = Command::Transaction {
        commands: vec![Command::RemoveCue { id: cue_id(30) }, missing()],
    };
    refused_at(&mut ws, command, 1);
}

/// A show saved and loaded with a network patch on port 0 (load accepts any port) that a
/// Network cue uses, after the sample's network patches.
fn with_loaded_port_zero() -> Workspace {
    let mut ws = sample();
    ws.network_patches.push(NetworkPatch {
        id: net_id(2),
        name: "Zero".into(),
        host: "10.0.0.9".into(),
        port: 0,
        protocol: NetworkProtocol::Udp,
    });
    ws.cue_lists[0].cues.push(cue(
        40,
        CueKind::Network {
            destination: net_id(1),
            address: "/go".into(),
            args: vec![],
        },
    ));
    let loaded = load_show(&save_show(&ws).unwrap()).unwrap();
    assert_eq!(zero_patch(&loaded.workspace).port, 0);
    loaded.workspace
}

fn zero_patch(ws: &Workspace) -> &NetworkPatch {
    ws.network_patches
        .iter()
        .find(|p| p.id == net_id(2))
        .unwrap()
}

/// Removing the port-0 patch, then a patch in use, is refused and rolled back
/// with the port-0 patch put back where it was.
#[test]
fn a_rollback_restores_a_loaded_network_patch_on_port_zero() {
    let mut ws = with_loaded_port_zero();
    let command = Command::Transaction {
        commands: vec![
            Command::RemoveNetworkPatch {
                id: net_id(2),
                even_if_used: false,
            },
            Command::RemoveNetworkPatch {
                id: net_id(1),
                even_if_used: false,
            },
        ],
    };
    refused_at(&mut ws, command, 1);
}

/// Setting that patch's port, then a refused command, rolls the port back to 0.
#[test]
fn a_rollback_restores_a_loaded_port_of_zero() {
    let mut ws = with_loaded_port_zero();
    let command = Command::Transaction {
        commands: vec![
            Command::SetNetworkPatchProperty {
                id: net_id(2),
                property: NetworkPatchProperty::Port { value: 9000 },
            },
            missing(),
        ],
    };
    refused_at(&mut ws, command, 1);
}

/// Undo of a port edit puts port 0 back, and undo of removing the patch brings it back; a new
/// port 0 is still refused.
#[test]
fn undo_restores_a_loaded_port_of_zero() {
    let mut ws = with_loaded_port_zero();
    let before = ws.clone();
    let mut history = History::new();
    history
        .execute(
            &mut ws,
            Command::SetNetworkPatchProperty {
                id: net_id(2),
                property: NetworkPatchProperty::Port { value: 9000 },
            },
        )
        .unwrap();
    history
        .execute(
            &mut ws,
            Command::RemoveNetworkPatch {
                id: net_id(2),
                even_if_used: false,
            },
        )
        .unwrap();
    assert!(history.undo(&mut ws).unwrap());
    assert!(history.undo(&mut ws).unwrap());
    assert_eq!(ws, before);
    assert!(history.redo(&mut ws).unwrap());
    assert!(history.redo(&mut ws).unwrap());
    assert!(ws.network_patches.iter().all(|p| p.id != net_id(2)));

    let zero = Command::SetNetworkPatchProperty {
        id: net_id(1),
        property: NetworkPatchProperty::Port { value: 0 },
    };
    assert!(matches!(ws.apply(zero), Err(CommandError::InvalidValue(_))));
    let mut patch = zero_patch(&before).clone();
    patch.id = net_id(5);
    assert!(matches!(
        ws.apply(Command::AddNetworkPatch { index: 0, patch }),
        Err(CommandError::InvalidValue(_))
    ));
}
