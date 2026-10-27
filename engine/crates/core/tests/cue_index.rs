// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The cue index (M4.12a): what it says after an edit is what a rebuild says. The random
//! version is `commands_prop::the_cue_index_follows_footprints`; these are named cases and the
//! cases the random one cannot make (duplicate ids, other cue lists).

mod common;

use std::collections::BTreeSet;

use common::*;
use openlab_core::*;

fn id(n: u128) -> CueId {
    cue_id(n)
}

fn memo(n: u128) -> Cue {
    cue(n, CueKind::Memo)
}

/// 1, group 2 { 3, group 4 { 5 } }, 6.
fn sample() -> Workspace {
    workspace(vec![
        memo(1),
        cue(2, group(vec![memo(3), cue(4, group(vec![memo(5)]))])),
        memo(6),
    ])
}

fn follow(index: &mut CueIndex, ws: &Workspace, command: &Command) -> IndexDelta {
    let fp = command.footprint();
    let structural: BTreeSet<CueId> = fp
        .added
        .iter()
        .chain(&fp.removed)
        .chain(&fp.moved)
        .copied()
        .collect();
    let touched = structural.iter().chain(&fp.edited).copied().collect();
    let IndexChange::Delta(delta) = index.reconcile(ws, &touched, &structural) else {
        panic!("rebuilt");
    };
    assert!(index.matches(ws));
    delta
}

fn pos(parent: Option<u128>, index: usize) -> Position {
    Position {
        list: list_id(1),
        parent: parent.map(id),
        index,
    }
}

#[test]
fn a_built_index_knows_places_ancestors_and_order() {
    let ws = sample();
    let index = CueIndex::build(&ws);
    assert_eq!(index.len(), 6);
    assert_eq!(index.parent(id(5)), Some(id(4)));
    assert_eq!(index.parent(id(1)), None);
    assert_eq!(index.ancestors(id(5)).collect::<Vec<_>>(), [id(4), id(2)]);
    assert_eq!(index.kids(CueContainer::Group(id(2))), [id(3), id(4)]);
    assert_eq!(
        index.kids(CueContainer::List(list_id(1))),
        [id(1), id(2), id(6)]
    );
    // Rank and path agree on workspace order (depth first).
    let ranks = index.ranks();
    let mut by_rank: Vec<CueId> = ranks.keys().copied().collect();
    by_rank.sort_by_key(|c| ranks[c]);
    assert_eq!(by_rank, [id(1), id(2), id(3), id(4), id(5), id(6)]);
    let mut by_path = by_rank.clone();
    by_path.sort_by_key(|c| index.path(*c).unwrap());
    assert_eq!(by_path, by_rank);
    assert_eq!(index.path(id(99)), None);
}

#[test]
fn a_moved_group_keeps_its_descendants_and_names_where_it_left() {
    let mut ws = sample();
    let mut index = CueIndex::build(&ws);
    let command = Command::ReparentCue {
        id: id(4),
        to: pos(None, 0),
    };
    ws.apply(command.clone()).unwrap();
    let delta = follow(&mut index, &ws, &command);
    assert_eq!(delta.moved, [id(4)]);
    assert!(delta.gone.is_empty() && delta.fresh.is_empty());
    assert_eq!(delta.former_parents, BTreeSet::from([id(2)]));
    assert_eq!(index.parent(id(5)), Some(id(4)));
    assert_eq!(index.parent(id(4)), None);
}

#[test]
fn a_removed_group_takes_its_descendants_and_an_added_one_brings_them() {
    let mut ws = sample();
    let mut index = CueIndex::build(&ws);
    let remove = Command::RemoveCue { id: id(2) };
    let undo = ws.apply(remove.clone()).unwrap();
    let delta = follow(&mut index, &ws, &remove);
    let mut gone = delta.gone.clone();
    gone.sort();
    assert_eq!(gone, [id(2), id(3), id(4), id(5)]);
    assert!(delta.former_parents.is_empty(), "the group itself is gone");
    assert_eq!(index.len(), 2);
    // The undo adds the whole subtree back, though its footprint names the group only.
    let readd = undo.clone();
    ws.apply(undo).unwrap();
    let delta = follow(&mut index, &ws, &readd);
    let mut fresh = delta.fresh.clone();
    fresh.sort();
    assert_eq!(fresh, [id(2), id(3), id(4), id(5)]);
    assert_eq!(index.len(), 6);
}

#[test]
fn a_child_moved_out_of_a_group_removed_in_the_same_edit_survives() {
    let mut ws = sample();
    let mut index = CueIndex::build(&ws);
    let command = Command::Transaction {
        commands: vec![
            Command::ReparentCue {
                id: id(5),
                to: pos(None, 0),
            },
            Command::RemoveCue { id: id(2) },
        ],
    };
    ws.apply(command.clone()).unwrap();
    let delta = follow(&mut index, &ws, &command);
    assert!(index.contains(id(5)));
    assert_eq!(index.parent(id(5)), None);
    let mut gone = delta.gone;
    gone.sort();
    assert_eq!(gone, [id(2), id(3), id(4)]);
}

#[test]
fn a_cue_added_and_removed_in_one_edit_leaves_no_trace() {
    let mut ws = sample();
    let mut index = CueIndex::build(&ws);
    let command = Command::Transaction {
        commands: vec![
            Command::AddCue {
                at: pos(Some(2), 0),
                cue: cue(7, group(vec![memo(8)])),
                auto_number: false,
            },
            Command::RemoveCue { id: id(7) },
        ],
    };
    ws.apply(command.clone()).unwrap();
    let delta = follow(&mut index, &ws, &command);
    assert!(delta.gone.is_empty() && delta.fresh.is_empty());
    assert!(!index.contains(id(7)) && !index.contains(id(8)));
}

#[test]
fn an_index_that_is_out_of_date_never_finds_a_wrong_cue() {
    let mut ws = sample();
    let index = CueIndex::build(&ws);
    assert_eq!(index.locate(&ws, id(5)).map(|c| c.id), Some(id(5)));
    // Cue 5 moves to the top level behind the index's back: its recorded ancestors no
    // longer lead to it.
    ws.apply(Command::ReparentCue {
        id: id(5),
        to: pos(None, 0),
    })
    .unwrap();
    assert!(index.locate(&ws, id(5)).is_none());
    assert!(!index.matches(&ws));
    // Cues whose places did not change are still found.
    assert_eq!(index.locate(&ws, id(3)).map(|c| c.id), Some(id(3)));
    assert!(index.locate(&ws, id(99)).is_none());
}

#[test]
fn a_footprint_that_names_a_moved_cue_only_as_edited_is_still_followed() {
    // The footprint calls cue 5 "edited" but it moved: the index finds it by scanning.
    let mut ws = sample();
    let mut index = CueIndex::build(&ws);
    ws.apply(Command::ReparentCue {
        id: id(5),
        to: pos(None, 0),
    })
    .unwrap();
    let touched = BTreeSet::from([id(5)]);
    let change = index.reconcile(&ws, &touched, &BTreeSet::new());
    assert!(matches!(change, IndexChange::Delta(_)));
    assert!(index.matches(&ws));
}

#[test]
fn duplicate_ids_are_flagged_and_always_rebuilt() {
    // An invalid workspace: the index builds without a panic or a loop, says so, and never
    // tries to follow a footprint (it cannot tell which of two cues one means).
    let mut ws = workspace(vec![memo(1), cue(2, group(vec![memo(1), memo(2)]))]);
    let mut index = CueIndex::build(&ws);
    assert!(index.has_duplicates());
    assert!(index.contains(id(1)) && index.contains(id(2)));
    assert_eq!(index.ranks().len(), 2);
    let touched = BTreeSet::from([id(1)]);
    assert_eq!(
        index.reconcile(&ws, &touched, &touched),
        IndexChange::Rebuilt
    );
    // A cue under itself is no loop either.
    ws.cue_lists[0].cues = vec![cue(2, group(vec![cue(2, group(vec![memo(2)]))]))];
    let index = CueIndex::build(&ws);
    assert!(index.has_duplicates());
    assert_eq!(index.ranks().len(), 1);
    assert_eq!(index.ancestors(id(2)).count(), 0);
}

fn replace(ws: &mut Workspace, index: &mut CueIndex, at: Position, new: Cue) -> IndexDelta {
    let command = Command::Transaction {
        commands: vec![
            Command::RemoveCue { id: new.id },
            Command::AddCue {
                at,
                cue: new,
                auto_number: false,
            },
        ],
    };
    ws.apply(command.clone()).unwrap();
    follow(index, ws, &command)
}

#[test]
fn a_cue_removed_and_added_again_under_its_id_gets_its_new_kids_and_loses_the_old() {
    // A group replaced by a group with other kids, and by a plain cue.
    for (new, kids) in [
        (cue(2, group(vec![memo(9)])), vec![9u128]),
        (memo(2), vec![]),
    ] {
        let mut ws = sample();
        let mut index = CueIndex::build(&ws);
        let delta = replace(&mut ws, &mut index, pos(None, 1), new);
        let mut gone = delta.gone.clone();
        gone.sort();
        assert_eq!(gone, vec![id(3), id(4), id(5)]);
        assert_eq!(delta.fresh.len(), kids.len());
        for kid in kids {
            assert!(index.contains(id(kid)));
        }
        assert!(!index.contains(id(3)) && !index.contains(id(5)));
    }
    // A plain cue replaced by a group of the same id.
    let mut ws = sample();
    let mut index = CueIndex::build(&ws);
    let delta = replace(
        &mut ws,
        &mut index,
        pos(None, 0),
        cue(1, group(vec![memo(9)])),
    );
    assert_eq!(delta.fresh, vec![id(9)]);
}

#[test]
fn a_changed_set_of_cue_lists_rebuilds() {
    let mut ws = sample();
    let mut index = CueIndex::build(&ws);
    ws.cue_lists.push(CueList {
        id: list_id(2),
        name: "B".into(),
        cues: vec![memo(10)],
    });
    assert_eq!(
        index.reconcile(&ws, &BTreeSet::new(), &BTreeSet::new()),
        IndexChange::Rebuilt
    );
    assert!(index.matches(&ws));
    assert_eq!(index.path(id(10)), Some(vec![1, 0]));
}
