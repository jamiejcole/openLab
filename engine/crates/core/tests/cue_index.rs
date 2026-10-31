// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The cue index: what it says after an edit is what a rebuild says. The random
//! version is `commands_prop::the_cue_index_follows_footprints`; these are named cases and the
//! cases the random one cannot make (duplicate ids, other cue lists).

mod common;

use std::collections::BTreeSet;

use common::*;
use openlab_core::*;

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
    let IndexChange::Delta(delta) = index.reconcile(ws, &touched, &structural, &fp.added) else {
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
    let change = index.reconcile(&ws, &touched, &BTreeSet::new(), &BTreeSet::new());
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
        index.reconcile(&ws, &touched, &touched, &BTreeSet::new()),
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
        assert_eq!(gone, vec![id(2), id(3), id(4), id(5)]);
        // The re-added cue is new too, with whatever it holds now.
        assert!(delta.fresh.contains(&id(2)));
        assert_eq!(delta.fresh.len(), kids.len() + 1);
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
    let mut fresh = delta.fresh.clone();
    fresh.sort();
    assert_eq!(fresh, vec![id(1), id(9)]);
}

#[test]
fn a_re_added_group_is_new_all_the_way_down_even_where_ids_are_kept() {
    // Nested: group 2 { 3, group 4 { 5 } } becomes group 2 { 3, group 4 { 7 } }.
    let mut ws = sample();
    let mut index = CueIndex::build(&ws);
    let new = cue(2, group(vec![memo(3), cue(4, group(vec![memo(7)]))]));
    let delta = replace(&mut ws, &mut index, pos(None, 1), new);
    let mut gone = delta.gone.clone();
    gone.sort();
    let mut fresh = delta.fresh.clone();
    fresh.sort();
    assert_eq!(gone, vec![id(2), id(3), id(4), id(5)]);
    assert_eq!(fresh, vec![id(2), id(3), id(4), id(7)]);
    assert!(!index.contains(id(5)) && index.contains(id(7)));
    // The very same ids and shape: every cue is still reported new (its content may differ).
    let new = cue(2, group(vec![memo(3), cue(4, group(vec![memo(7)]))]));
    let delta = replace(&mut ws, &mut index, pos(None, 1), new);
    assert_eq!(delta.fresh.len(), 4);
    assert_eq!(delta.gone.len(), 4);
}

#[test]
fn an_added_group_that_holds_ids_removed_in_the_same_edit_takes_them_as_new() {
    // Group 2 { 3, group 4 { 5 } } removed; group 8 { 5 } added: 5 changes container.
    let mut ws = sample();
    let mut index = CueIndex::build(&ws);
    let command = Command::Transaction {
        commands: vec![
            Command::RemoveCue { id: id(2) },
            Command::AddCue {
                at: pos(None, 1),
                cue: cue(8, group(vec![memo(5)])),
                auto_number: false,
            },
        ],
    };
    ws.apply(command.clone()).unwrap();
    let delta = follow(&mut index, &ws, &command);
    assert!(delta.gone.contains(&id(5)) && delta.fresh.contains(&id(5)));
    assert!(delta.fresh.contains(&id(8)));
}

#[test]
fn a_cue_moved_out_of_a_re_added_group_in_the_same_edit_is_new_with_its_subtree() {
    // Group 2 { 3, group 4 { 5 } } removed and added again, then group 4 moved to the top: 4
    // and 5 are under no added top, yet they are the re-added content.
    let mut ws = sample();
    let mut index = CueIndex::build(&ws);
    let command = Command::Transaction {
        commands: vec![
            Command::RemoveCue { id: id(2) },
            Command::AddCue {
                at: pos(None, 1),
                cue: cue(2, group(vec![memo(3), cue(4, group(vec![memo(5)]))])),
                auto_number: false,
            },
            Command::ReparentCue {
                id: id(4),
                to: pos(None, 0),
            },
        ],
    };
    ws.apply(command.clone()).unwrap();
    let delta = follow(&mut index, &ws, &command);
    for n in [2, 3, 4, 5] {
        assert!(delta.gone.contains(&id(n)), "{n} gone");
        assert!(delta.fresh.contains(&id(n)), "{n} fresh");
    }
    // Cue 1, untouched, is neither.
    assert!(!delta.gone.contains(&id(1)) && !delta.fresh.contains(&id(1)));
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
        index.reconcile(&ws, &BTreeSet::new(), &BTreeSet::new(), &BTreeSet::new()),
        IndexChange::Rebuilt
    );
    assert!(index.matches(&ws));
    assert_eq!(index.path(id(10)), Some(vec![1, 0]));
}

// ----- positions -----

/// A flat list of `n` memos, ids 1..=n, then a group 100_000 { group 100_001 { 100_002 } }.
fn flat(n: u128) -> Workspace {
    let mut cues: Vec<Cue> = (1..=n).map(memo).collect();
    cues.push(cue(
        100_000,
        group(vec![cue(100_001, group(vec![memo(100_002)]))]),
    ));
    workspace(cues)
}

#[test]
fn a_lookup_costs_one_step_per_level_however_many_siblings() {
    // Before positions were kept, finding the last of 5,000 cues compared 5,000 ids.
    let mut ws = flat(5_000);
    let mut index = CueIndex::build(&ws);
    let cost = |index: &CueIndex, ws: &Workspace, n: u128| {
        let before = index.lookup_steps();
        assert_eq!(index.locate(ws, id(n)).map(|c| c.id), Some(id(n)));
        index.lookup_steps() - before
    };
    assert_eq!(cost(&index, &ws, 5_000), 1);
    assert_eq!(cost(&index, &ws, 1), 1);
    assert_eq!(cost(&index, &ws, 100_002), 3);
    assert_eq!(index.path(id(5_000)), Some(vec![0, 4_999]));
    assert_eq!(index.position(id(100_001)), Some(0));

    // A rename is followed by finding the one cue: one step.
    let rename = Command::SetCueProperty {
        id: id(4_000),
        property: CueProperty::Name { value: "x".into() },
    };
    ws.apply(rename.clone()).unwrap();
    let before = index.lookup_steps();
    follow(&mut index, &ws, &rename);
    assert_eq!(index.lookup_steps() - before, 1);

    // A cue added at the front shifts every position: the index follows, and lookups stay one
    // step per level.
    let add = Command::AddCue {
        at: pos(None, 0),
        cue: memo(9_999),
        auto_number: false,
    };
    ws.apply(add.clone()).unwrap();
    follow(&mut index, &ws, &add);
    assert_eq!(index.position(id(5_000)), Some(5_000));
    assert_eq!(cost(&index, &ws, 5_000), 1);
    assert_eq!(cost(&index, &ws, 100_002), 3);
}

#[test]
fn a_stale_position_finds_the_cue_by_looking_along_never_another_cue() {
    let mut ws = flat(10);
    let index = CueIndex::build(&ws);
    // Two cues added at the front behind the index's back: every recorded position is off by
    // two, so the cue recorded at a position is another cue.
    ws.cue_lists[0].cues.insert(0, memo(500));
    ws.cue_lists[0].cues.insert(0, memo(501));
    for n in 1..=10 {
        assert_eq!(index.locate(&ws, id(n)).map(|c| c.id), Some(id(n)));
    }
    assert_eq!(
        index.locate(&ws, id(100_002)).map(|c| c.id),
        Some(id(100_002))
    );
    // Gone behind its back: not found, nothing else returned.
    ws.cue_lists[0].cues.retain(|c| c.id != id(7));
    assert!(index.locate(&ws, id(7)).is_none());
}

#[test]
fn a_cue_removed_and_added_again_elsewhere_in_its_list_gets_its_new_position() {
    let mut ws = flat(5);
    let mut index = CueIndex::build(&ws);
    replace(&mut ws, &mut index, pos(None, 4), memo(1));
    assert_eq!(index.position(id(1)), Some(4));
    assert_eq!(index.position(id(2)), Some(0));
    assert_eq!(index.next_in_list(id(1)), Some(id(100_000)));
    // Into a nested group, as a group.
    replace(
        &mut ws,
        &mut index,
        pos(Some(100_001), 0),
        cue(3, group(vec![memo(30)])),
    );
    assert_eq!(index.position(id(3)), Some(0));
    assert_eq!(index.position(id(100_002)), Some(1));
    assert_eq!(index.path(id(30)), Some(vec![0, 4, 0, 0, 0]));
    assert_eq!(index.next_in_list(id(30)), Some(id(100_002)));
    assert_eq!(index.next_in_list(id(100_002)), None);
}

#[test]
fn list_order_and_next_follow_depth_first_order() {
    let ws = sample();
    let index = CueIndex::build(&ws);
    let order = index.list_order(list_id(1));
    assert_eq!(order, [id(1), id(2), id(3), id(4), id(5), id(6)]);
    let nexts: Vec<Option<CueId>> = order.iter().map(|c| index.next_in_list(*c)).collect();
    assert_eq!(
        nexts,
        [
            Some(id(2)),
            Some(id(3)),
            Some(id(4)),
            Some(id(5)),
            Some(id(6)),
            None
        ]
    );
    assert_eq!(index.list_of(id(5)), Some(list_id(1)));
    assert_eq!(index.list_of(id(99)), None);
    assert!(index.list_order(list_id(9)).is_empty());
}

// ----- the shared tree walk (cull M24) -----

#[test]
fn the_tree_walk_is_depth_first_in_order_and_finds_nested_cues() {
    let mut ws = sample();
    ws.cue_lists.push(CueList {
        id: list_id(2),
        name: "B".into(),
        cues: vec![memo(10)],
    });
    let order: Vec<CueId> = ws.cues().map(|c| c.id).collect();
    assert_eq!(order, [1, 2, 3, 4, 5, 6, 10].map(id));
    let in_group: Vec<CueId> = descendants(ws.cue_lists[0].cues[1].children())
        .map(|c| c.id)
        .collect();
    assert_eq!(in_group, [3, 4, 5].map(id));

    // A cue two groups deep and one in another list are found and changed in place.
    ws.find_cue_mut(id(5)).unwrap().armed = false;
    ws.find_cue_mut(id(10)).unwrap().armed = false;
    assert!(!ws.find_cue(id(5)).unwrap().armed);
    assert!(!ws.find_cue(id(10)).unwrap().armed);
    assert!(ws.find_cue(id(3)).unwrap().armed);
    assert!(ws.find_cue_mut(id(99)).is_none());

    // Deep nesting is walked without recursion.
    let mut deep = memo(1_000);
    for n in (1_001..=2_000).rev() {
        deep = cue(n, group(vec![deep]));
    }
    assert_eq!(descendants(std::slice::from_ref(&deep)).count(), 1_001);
}
