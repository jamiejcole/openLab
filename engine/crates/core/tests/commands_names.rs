// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! Names and hosts (M1.20): the workspace rename, and the length and character rule for the
//! names of the workspace, cue lists, patches and network patches and for network hosts. A new
//! value is checked; a loaded one is kept with a warning and can always be undone back to.

mod common;

use common::*;
use openlab_core::protocol::Upsert;
use openlab_core::*;

fn rename(name: &str) -> Command {
    Command::RenameWorkspace { name: name.into() }
}

fn long(n: usize) -> String {
    "n".repeat(n)
}

fn net_with(n: u128, name: &str, host: &str) -> NetworkPatch {
    NetworkPatch {
        id: net_id(n),
        name: name.into(),
        host: host.into(),
        port: 8000,
        protocol: NetworkProtocol::Udp,
    }
}

fn refused(ws: &Workspace, command: Command) -> CommandError {
    let mut work = ws.clone();
    let mut history = History::new();
    let error = history.execute(&mut work, command).unwrap_err();
    assert_eq!(&work, ws);
    assert!(!history.can_undo());
    error
}

/// Every command taking `name` as a new name, for the one value.
fn name_commands(name: &str) -> Vec<Command> {
    let mut patch = patch(7, 2);
    patch.name = name.into();
    vec![
        rename(name),
        Command::RenameCueList {
            id: list_id(1),
            name: name.into(),
        },
        Command::AddCueList {
            index: 0,
            id: list_id(9),
            name: name.into(),
        },
        Command::AddPatch { index: 0, patch },
        Command::SetPatchProperty {
            id: patch_id(1),
            property: PatchProperty::Name { value: name.into() },
        },
        Command::AddNetworkPatch {
            index: 0,
            patch: net_with(9, name, "10.0.0.9"),
        },
        Command::SetNetworkPatchProperty {
            id: net_id(1),
            property: NetworkPatchProperty::Name { value: name.into() },
        },
    ]
}

#[test]
fn renaming_the_workspace_undoes_redoes_and_reaches_the_client() {
    let ws = workspace(vec![]);
    let mut work = ws.clone();
    let mut history = History::new();
    assert!(history.execute(&mut work, rename("Hamlet")).unwrap());
    assert_eq!(work.name, "Hamlet");
    // The settings upsert carries the name.
    let footprint = rename("Hamlet").footprint();
    assert!(footprint.is_model_wide());
    let (upserts, _) = model_changes(&work, &footprint);
    assert!(matches!(
        upserts.as_slice(),
        [Upsert::Settings { name, .. }] if name == "Hamlet"
    ));
    history.undo(&mut work).unwrap();
    assert_eq!(work, ws);
    history.redo(&mut work).unwrap();
    assert_eq!(work.name, "Hamlet");
    // Same name: not an edit.
    assert!(!history.execute(&mut work, rename("Hamlet")).unwrap());
}

#[test]
fn renames_in_a_row_coalesce_into_one_undo_step() {
    let ws = workspace(vec![]);
    let mut work = ws.clone();
    let mut history = History::new();
    for name in ["H", "Ha", "Ham"] {
        history
            .execute_coalescing(&mut work, rename(name), Some("title"))
            .unwrap();
    }
    history.undo(&mut work).unwrap();
    assert_eq!(work, ws);
    assert!(!history.can_undo());
}

#[test]
fn a_name_at_the_limit_is_accepted_in_scalars_not_bytes() {
    let ws = workspace(vec![]);
    // 256 characters of four bytes each is 1024 bytes.
    for name in [
        long(256),
        "\u{1F3AD}".repeat(256),
        String::new(),
        "Ça va".into(),
    ] {
        for command in name_commands(&name) {
            let mut work = ws.clone();
            assert!(
                History::new().execute(&mut work, command.clone()).is_ok(),
                "{command:?}"
            );
        }
    }
}

#[test]
fn an_over_long_or_control_character_name_is_refused_everywhere() {
    let ws = workspace(vec![]);
    for name in [
        long(257),
        "\u{1F3AD}".repeat(257),
        "two\nlines".to_owned(),
        "tab\there".to_owned(),
        "nul\0".to_owned(),
        "esc\u{1b}[0m".to_owned(),
        "next\u{85}line".to_owned(),
    ] {
        for command in name_commands(&name) {
            let error = refused(&ws, command.clone());
            assert!(
                matches!(error, CommandError::InvalidValue(_)),
                "{command:?}"
            );
        }
    }
}

#[test]
fn a_host_is_bounded_in_bytes_and_has_no_control_characters() {
    let ws = workspace(vec![]);
    let ok = |host: &str| {
        let mut work = ws.clone();
        History::new()
            .execute(
                &mut work,
                Command::SetNetworkPatchProperty {
                    id: net_id(1),
                    property: NetworkPatchProperty::Host { value: host.into() },
                },
            )
            .is_ok()
    };
    assert!(ok(&"h".repeat(253)));
    assert!(!ok(&"h".repeat(254)));
    // Two bytes each: 127 of them are over the 253-byte limit.
    assert!(ok(&"\u{e9}".repeat(126)));
    assert!(!ok(&"\u{e9}".repeat(127)));
    assert!(!ok("host\nname"));
    assert!(!ok("host\0"));
    // Addressability is another question (a broken cue, not a refused edit).
    assert!(ok(""));
    assert!(ok("not a host"));
    for host in ["h".repeat(254), "a\tb".into()] {
        let error = refused(
            &ws,
            Command::AddNetworkPatch {
                index: 0,
                patch: net_with(9, "ok", &host),
            },
        );
        assert!(matches!(error, CommandError::InvalidValue(_)));
    }
}

#[test]
fn a_refused_name_inside_a_transaction_leaves_nothing_behind() {
    let ws = workspace(vec![]);
    let error = refused(
        &ws,
        Command::Transaction {
            commands: vec![rename("Fine"), rename(&long(300))],
        },
    );
    assert!(matches!(
        error,
        CommandError::InTransaction { index: 1, .. }
    ));
}

/// A show whose names and hosts are over-long or hold control characters, as a damaged or
/// hand-edited file could.
fn odd_workspace() -> Workspace {
    let mut ws = workspace(vec![]);
    ws.name = "w".repeat(300);
    ws.cue_lists[0].name = "two\nlines".into();
    ws.patches[0].name = "\u{1F3AD}".repeat(300);
    ws.network_patches[0].name = "nul\0".into();
    ws.network_patches[0].host = "h".repeat(300);
    ws
}

#[test]
fn a_show_with_odd_names_loads_with_one_warning_per_field_and_saves_back_unchanged() {
    let ws = odd_workspace();
    let loaded = load_show(&show_text(&ws)).unwrap();
    assert_eq!(loaded.workspace, ws);
    let mut fields: Vec<&str> = loaded
        .warnings
        .iter()
        .filter_map(|w| match w.kind {
            IssueKind::InvalidName { field } => {
                assert_eq!(w.severity, Severity::Warning);
                Some(field)
            }
            _ => None,
        })
        .collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        [
            "cue_lists.name",
            "name",
            "network_patches.host",
            "network_patches.name",
            "patches.name"
        ]
    );
    let again = load_show(&save_show(&loaded.workspace).unwrap()).unwrap();
    assert_eq!(again.workspace, ws);
}

#[test]
fn a_normal_show_raises_no_name_warning() {
    let loaded = load_show(&show_text(&workspace(vec![]))).unwrap();
    assert!(
        !loaded
            .warnings
            .iter()
            .any(|w| matches!(w.kind, IssueKind::InvalidName { .. }))
    );
}

#[test]
fn an_odd_loaded_name_can_be_replaced_and_undone_back() {
    let ws = load_show(&show_text(&odd_workspace())).unwrap().workspace;
    let steps = [
        rename("Fine"),
        Command::RenameCueList {
            id: list_id(1),
            name: "Fine".into(),
        },
        Command::SetPatchProperty {
            id: patch_id(1),
            property: PatchProperty::Name {
                value: "Fine".into(),
            },
        },
        Command::SetNetworkPatchProperty {
            id: net_id(1),
            property: NetworkPatchProperty::Name {
                value: "Fine".into(),
            },
        },
        Command::SetNetworkPatchProperty {
            id: net_id(1),
            property: NetworkPatchProperty::Host {
                value: "10.0.0.1".into(),
            },
        },
    ];
    for step in &steps {
        let mut work = ws.clone();
        let mut history = History::new();
        assert!(history.execute(&mut work, step.clone()).unwrap());
        assert_ne!(work, ws);
        history.undo(&mut work).unwrap();
        assert_eq!(work, ws, "{step:?}");
        history.redo(&mut work).unwrap();
        assert_ne!(work, ws);
    }
    // One transaction doing all of it, undone as one step.
    let mut work = ws.clone();
    let mut history = History::new();
    history
        .execute(
            &mut work,
            Command::Transaction {
                commands: steps.to_vec(),
            },
        )
        .unwrap();
    history.undo(&mut work).unwrap();
    assert_eq!(work, ws);
}

#[test]
fn a_transaction_that_fails_rolls_back_to_an_odd_name() {
    let ws = load_show(&show_text(&odd_workspace())).unwrap().workspace;
    let mut work = ws.clone();
    let error = work
        .apply(Command::Transaction {
            commands: vec![
                rename("Fine"),
                Command::RenameCueList {
                    id: list_id(1),
                    name: "Fine".into(),
                },
                Command::RemoveCueList { id: list_id(404) },
            ],
        })
        .unwrap_err();
    assert!(matches!(
        error,
        CommandError::InTransaction { index: 2, .. }
    ));
    assert_eq!(work, ws, "the rollback put the odd names back");
}

#[test]
fn removing_and_restoring_items_with_odd_names_works() {
    let mut ws = load_show(&show_text(&odd_workspace())).unwrap().workspace;
    // A second list and patch, so the odd ones may go.
    ws.cue_lists.push(CueList {
        id: list_id(2),
        name: "Second".into(),
        cues: vec![],
    });
    ws.patches.push(patch(2, 2));
    let removals = [
        Command::RemoveCueList { id: list_id(1) },
        Command::RemovePatch {
            id: patch_id(1),
            even_if_used: false,
        },
        Command::RemoveNetworkPatch {
            id: net_id(1),
            even_if_used: false,
        },
    ];
    for removal in removals {
        let mut work = ws.clone();
        let mut history = History::new();
        assert!(history.execute(&mut work, removal.clone()).unwrap());
        history.undo(&mut work).unwrap();
        assert_eq!(work, ws, "{removal:?}");
        history.redo(&mut work).unwrap();
        history.undo(&mut work).unwrap();
        assert_eq!(work, ws);
    }
}

#[test]
fn an_unchanged_odd_name_is_a_no_op_and_a_missing_id_wins_over_a_bad_name() {
    let ws = load_show(&show_text(&odd_workspace())).unwrap().workspace;
    let unchanged = [
        rename(&ws.name.clone()),
        Command::RenameCueList {
            id: list_id(1),
            name: ws.cue_lists[0].name.clone(),
        },
        Command::SetPatchProperty {
            id: patch_id(1),
            property: PatchProperty::Name {
                value: ws.patches[0].name.clone(),
            },
        },
        Command::SetNetworkPatchProperty {
            id: net_id(1),
            property: NetworkPatchProperty::Name {
                value: ws.network_patches[0].name.clone(),
            },
        },
        Command::SetNetworkPatchProperty {
            id: net_id(1),
            property: NetworkPatchProperty::Host {
                value: ws.network_patches[0].host.clone(),
            },
        },
    ];
    for command in unchanged {
        let mut work = ws.clone();
        let mut history = History::new();
        assert!(
            !history.execute(&mut work, command.clone()).unwrap(),
            "{command:?}"
        );
        assert!(!history.can_undo());
        assert_eq!(work, ws);
    }
    // A loaded port 0 committed again is a no-op too.
    let mut zero = ws.clone();
    zero.network_patches[0].port = 0;
    let mut history = History::new();
    let same = Command::SetNetworkPatchProperty {
        id: net_id(1),
        property: NetworkPatchProperty::Port { value: 0 },
    };
    assert!(!history.execute(&mut zero, same).unwrap());
    let bad = long(300);
    assert_eq!(
        refused(
            &ws,
            Command::RenameCueList {
                id: list_id(404),
                name: bad.clone()
            }
        ),
        CommandError::CueListNotFound(list_id(404))
    );
    assert_eq!(
        refused(
            &ws,
            Command::SetPatchProperty {
                id: patch_id(404),
                property: PatchProperty::Name { value: bad.clone() }
            }
        ),
        CommandError::PatchNotFound(patch_id(404))
    );
    assert_eq!(
        refused(
            &ws,
            Command::SetNetworkPatchProperty {
                id: net_id(404),
                property: NetworkPatchProperty::Name { value: bad }
            }
        ),
        CommandError::NetworkPatchNotFound(net_id(404))
    );
}
