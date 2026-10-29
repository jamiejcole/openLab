// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `model_changed` contents: what each kind of edit emits, and that a client applying it holds
//! the engine's workspace (NFR-01, ED-07). The randomised version is in `commands_prop.rs`.

mod common;

use common::*;
use openlab_core::protocol::{Delete, Upsert};
use openlab_core::*;

fn pos(list: u128, parent: Option<u128>, index: usize) -> Position {
    Position {
        list: list_id(list),
        parent: parent.map(cue_id),
        index,
    }
}

/// Applies `command` through `history` to `ws`, and the emitted changes to `mirror`.
fn step(
    ws: &mut Workspace,
    mirror: &mut Workspace,
    history: &mut History,
    command: Command,
) -> (Vec<Upsert>, Vec<Delete>) {
    let footprint = command.footprint();
    history.execute(ws, command).unwrap();
    changes(ws, mirror, &footprint)
}

/// The changes for `footprint`, applied to `mirror`; read through a cue index they are
/// the same as the walk's.
fn changes(
    ws: &Workspace,
    mirror: &mut Workspace,
    footprint: &Footprint,
) -> (Vec<Upsert>, Vec<Delete>) {
    let (upserts, deletes) = model_changes(ws, footprint);
    assert_eq!(
        model_changes_indexed(ws, footprint, &CueIndex::build(ws)),
        Some((upserts.clone(), deletes.clone()))
    );
    mirror.apply_changes(&upserts, &deletes).unwrap();
    assert_eq!(mirror, ws);
    (upserts, deletes)
}

fn undo_redo(
    ws: &mut Workspace,
    mirror: &mut Workspace,
    history: &mut History,
    undo: bool,
) -> (Vec<Upsert>, Vec<Delete>) {
    let footprint = if undo {
        history.peek_undo()
    } else {
        history.peek_redo()
    }
    .unwrap()
    .footprint();
    assert!(if undo {
        history.undo(ws).unwrap()
    } else {
        history.redo(ws).unwrap()
    });
    changes(ws, mirror, &footprint)
}

fn memos(range: std::ops::RangeInclusive<u128>) -> Vec<Cue> {
    range.map(|n| cue(n, CueKind::Memo)).collect()
}

fn rename(id: u128, name: &str) -> Command {
    Command::SetCueProperty {
        id: cue_id(id),
        property: CueProperty::Name { value: name.into() },
    }
}

#[test]
fn a_property_edit_is_one_upsert_in_place() {
    let mut ws = workspace(memos(1..=5));
    let (mut mirror, mut history) = (ws.clone(), History::new());
    let (upserts, deletes) = step(&mut ws, &mut mirror, &mut history, rename(3, "x"));
    assert!(deletes.is_empty());
    assert_eq!(upserts.len(), 1);
    assert!(matches!(
        &upserts[0],
        Upsert::Cue { list, parent: None, index: 2, cue } if *list == list_id(1) && cue.name == "x"
    ));
}

#[test]
fn adding_removing_and_their_undo_emit_only_that_cue() {
    let mut ws = workspace(memos(1..=5));
    let (mut mirror, mut history) = (ws.clone(), History::new());
    let add = Command::AddCue {
        at: pos(1, None, 2),
        cue: cue(9, CueKind::Memo),
        auto_number: false,
    };
    let (upserts, deletes) = step(&mut ws, &mut mirror, &mut history, add);
    assert_eq!((upserts.len(), deletes.len()), (1, 0));
    let remove = Command::RemoveCue { id: cue_id(1) };
    let (upserts, deletes) = step(&mut ws, &mut mirror, &mut history, remove);
    assert_eq!(
        (upserts.len(), deletes),
        (0, vec![Delete::Cue { id: cue_id(1) }])
    );
    let (upserts, deletes) = undo_redo(&mut ws, &mut mirror, &mut history, true);
    assert_eq!((upserts.len(), deletes.len()), (1, 0));
    let (upserts, deletes) = undo_redo(&mut ws, &mut mirror, &mut history, true);
    assert_eq!((upserts.len(), deletes.len()), (0, 1));
}

#[test]
fn deleting_a_group_sends_one_delete_and_undo_one_upsert_with_the_subtree() {
    let inner = cue(3, group(memos(4..=5)));
    let outer = cue(2, group(vec![cue(6, CueKind::Memo), inner]));
    let mut ws = workspace(vec![cue(1, CueKind::Memo), outer]);
    let (mut mirror, mut history) = (ws.clone(), History::new());
    let remove = Command::RemoveCue { id: cue_id(2) };
    let (upserts, deletes) = step(&mut ws, &mut mirror, &mut history, remove);
    assert_eq!((upserts.len(), deletes.len()), (0, 1));
    let (upserts, _) = undo_redo(&mut ws, &mut mirror, &mut history, true);
    assert_eq!(upserts.len(), 1);
    let Upsert::Cue { cue: group, .. } = &upserts[0] else {
        panic!("not a cue");
    };
    assert_eq!(group.children().len(), 2);
    assert_eq!(group.children()[1].children().len(), 2);
}

#[test]
fn a_cue_inside_a_changed_group_is_not_sent_twice() {
    let mut ws = workspace(vec![cue(1, group(memos(2..=4)))]);
    let (mut mirror, mut history) = (ws.clone(), History::new());
    let both = Command::Transaction {
        commands: vec![rename(1, "g"), rename(3, "c")],
    };
    let (upserts, _) = step(&mut ws, &mut mirror, &mut history, both);
    assert_eq!(upserts.len(), 1);
}

#[test]
fn several_moves_in_one_step_land_where_the_workspace_has_them() {
    // The case a plain "remove, insert at final index" client gets wrong: two movers.
    let mut ws = workspace(memos(1..=4));
    let (mut mirror, mut history) = (ws.clone(), History::new());
    let moves = Command::Transaction {
        commands: vec![
            Command::MoveCue {
                id: cue_id(1),
                index: 3,
            },
            Command::MoveCue {
                id: cue_id(2),
                index: 3,
            },
            Command::MoveCue {
                id: cue_id(3),
                index: 0,
            },
        ],
    };
    step(&mut ws, &mut mirror, &mut history, moves);
    undo_redo(&mut ws, &mut mirror, &mut history, true);
    undo_redo(&mut ws, &mut mirror, &mut history, false);
}

#[test]
fn moves_between_groups_and_lists_and_out_of_a_removed_group() {
    let mut ws = workspace(vec![
        cue(1, group(memos(2..=3))),
        cue(4, CueKind::Memo),
        cue(5, group(vec![])),
    ]);
    ws.cue_lists.push(CueList {
        id: list_id(2),
        name: "Second".into(),
        cues: vec![],
    });
    let (mut mirror, mut history) = (ws.clone(), History::new());
    let to_other_list = Command::ReparentCue {
        id: cue_id(4),
        to: pos(2, None, 0),
    };
    step(&mut ws, &mut mirror, &mut history, to_other_list);
    // A child leaves its group, which is then removed; another cue goes into the other group.
    let rescue = Command::Transaction {
        commands: vec![
            Command::ReparentCue {
                id: cue_id(2),
                to: pos(2, None, 1),
            },
            Command::RemoveCue { id: cue_id(1) },
            Command::ReparentCue {
                id: cue_id(4),
                to: pos(1, Some(5), 0),
            },
        ],
    };
    step(&mut ws, &mut mirror, &mut history, rescue);
    undo_redo(&mut ws, &mut mirror, &mut history, true);
    undo_redo(&mut ws, &mut mirror, &mut history, true);
    undo_redo(&mut ws, &mut mirror, &mut history, false);
}

#[test]
fn renumbering_sends_the_renumbered_cues_only() {
    let mut ws = workspace(memos(1..=2000));
    let (mut mirror, mut history) = (ws.clone(), History::new());
    let renumber = Command::Renumber {
        ids: vec![cue_id(10), cue_id(11), cue_id(12)],
        start: 100.0,
        increment: 1.0,
    };
    let (upserts, deletes) = step(&mut ws, &mut mirror, &mut history, renumber);
    assert_eq!((upserts.len(), deletes.len()), (3, 0));
    let swap = Command::SetCueNumbers {
        numbers: vec![
            NumberAssignment {
                id: cue_id(10),
                number: Some("a".into()),
            },
            NumberAssignment {
                id: cue_id(11),
                number: None,
            },
        ],
    };
    let (upserts, _) = step(&mut ws, &mut mirror, &mut history, swap);
    assert_eq!(upserts.len(), 2);
}

#[test]
fn an_edit_in_a_2000_cue_show_emits_o_changed_entities() {
    let mut ws = workspace(memos(1..=2000));
    let (mut mirror, mut history) = (ws.clone(), History::new());
    let counts = |u: &[Upsert], d: &[Delete]| u.len() + d.len();
    let (u, d) = step(&mut ws, &mut mirror, &mut history, rename(1000, "x"));
    assert_eq!(counts(&u, &d), 1);
    let batch = Command::BatchSet {
        ids: vec![cue_id(5), cue_id(6)],
        property: CueProperty::Flagged { value: true },
    };
    let (u, d) = step(&mut ws, &mut mirror, &mut history, batch);
    assert_eq!(counts(&u, &d), 2);
    let mv = Command::MoveCue {
        id: cue_id(1),
        index: 1999,
    };
    let (u, d) = step(&mut ws, &mut mirror, &mut history, mv);
    assert_eq!(counts(&u, &d), 2);
    let (u, d) = undo_redo(&mut ws, &mut mirror, &mut history, true);
    assert_eq!(counts(&u, &d), 2);
}

#[test]
fn read_through_an_index_an_edit_costs_its_cues_not_the_show() {
    // The walk visited all 2,000 cues for a rename; through the index it is one lookup of one
    // step (the cue's level), plus finding its position and list.
    let mut ws = workspace(memos(1..=2000));
    let rename = rename(1990, "x");
    let footprint = rename.footprint();
    ws.apply(rename).unwrap();
    let index = CueIndex::build(&ws);
    let before = index.lookup_steps();
    let (upserts, deletes) = model_changes_indexed(&ws, &footprint, &index).unwrap();
    assert_eq!(index.lookup_steps() - before, 1);
    assert!(deletes.is_empty());
    assert!(matches!(&upserts[..], [Upsert::Cue { index: 1989, .. }]));
}

#[test]
fn an_index_with_duplicate_ids_or_out_of_date_is_not_used() {
    let ws = workspace(vec![cue(1, CueKind::Memo), cue(1, CueKind::Memo)]);
    let fp = Footprint::edited([cue_id(1)]);
    assert_eq!(model_changes_indexed(&ws, &fp, &CueIndex::build(&ws)), None);
    // The index has cue 2 inside group 3, the workspace no longer does.
    let old = workspace(vec![cue(3, group(vec![cue(2, CueKind::Memo)]))]);
    let ws = workspace(vec![cue(3, group(vec![])), cue(2, CueKind::Memo)]);
    let fp = Footprint::edited([cue_id(2)]);
    assert_eq!(
        model_changes_indexed(&ws, &fp, &CueIndex::build(&old)),
        None
    );
}

#[test]
fn an_empty_footprint_emits_nothing_and_settings_style_upserts_apply() {
    let ws = workspace(memos(1..=3));
    let (upserts, deletes) = model_changes(&ws, &Footprint::default());
    assert!(upserts.is_empty() && deletes.is_empty());
    assert!(Footprint::default().is_empty());
    // The non-cue upserts of the protocol apply too (no command edits them yet).
    let mut client = ws.clone();
    let mut settings = ws.settings.clone();
    settings.master_db = -6.0;
    let mut patch = ws.patches[0].clone();
    patch.name = "renamed".into();
    client
        .apply_changes(
            &[
                Upsert::Settings {
                    name: "New".into(),
                    settings: Box::new(settings.clone()),
                },
                Upsert::Patch {
                    index: 0,
                    patch: patch.clone(),
                },
                Upsert::CueList {
                    id: list_id(1),
                    name: "Renamed".into(),
                    index: 0,
                },
            ],
            &[],
        )
        .unwrap();
    assert_eq!(client.name, "New");
    assert_eq!(client.settings, settings);
    assert_eq!(client.patches, vec![patch]);
    assert_eq!(client.cue_lists[0].name, "Renamed");
    assert_eq!(client.cue_lists[0].cues.len(), 3);
    client
        .apply_changes(
            &[],
            &[
                Delete::Patch { id: patch_id(1) },
                Delete::CueList { id: list_id(1) },
            ],
        )
        .unwrap();
    assert!(client.patches.is_empty() && client.cue_lists.is_empty());
}

#[test]
fn a_client_refuses_changes_that_do_not_fit() {
    let mut ws = workspace(memos(1..=2));
    let at = |list, parent, index| Upsert::Cue {
        list,
        parent,
        index,
        cue: Box::new(cue(9, CueKind::Memo)),
    };
    assert_eq!(
        ws.apply_changes(&[at(list_id(7), None, 0)], &[]),
        Err(ApplyChangesError::NoList)
    );
    assert_eq!(
        ws.apply_changes(&[at(list_id(1), Some(cue_id(77)), 0)], &[]),
        Err(ApplyChangesError::NoParent)
    );
    assert_eq!(
        ws.apply_changes(&[at(list_id(1), None, 5)], &[]),
        Err(ApplyChangesError::BadIndex)
    );
    // A delete of an unknown id is ignored.
    assert!(
        ws.apply_changes(&[], &[Delete::Cue { id: cue_id(99) }])
            .is_ok()
    );
}
