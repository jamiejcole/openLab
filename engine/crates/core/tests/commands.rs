// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! Editing commands, transactions and undo/redo (ED-03, ED-04, ED-05, ED-07, ED-08, NFR-03).

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

fn add(at: Position, cue: Cue) -> Command {
    Command::AddCue {
        at,
        cue,
        auto_number: false,
    }
}

fn grp(n: u128, children: Vec<Cue>) -> Cue {
    cue(n, group(children))
}

fn set(id: u128, property: CueProperty) -> Command {
    Command::SetCueProperty {
        id: cue_id(id),
        property,
    }
}

/// Ids in list order, parents before children.
fn order(ws: &Workspace) -> Vec<u128> {
    fn walk(cues: &[Cue], out: &mut Vec<u128>) {
        for cue in cues {
            out.push(cue.id.0.as_u128());
            walk(cue.children(), out);
        }
    }
    let mut out = Vec::new();
    for list in &ws.cue_lists {
        walk(&list.cues, &mut out);
    }
    out
}

/// "1 2[3 4[5]] 6": the tree shape, for comparing structure at a glance.
fn shape(cues: &[Cue]) -> String {
    cues.iter()
        .map(|c| {
            if matches!(c.kind, CueKind::Group { .. }) {
                format!("{}[{}]", c.id.0.as_u128(), shape(c.children()))
            } else {
                c.id.0.as_u128().to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn top(ws: &Workspace) -> String {
    shape(&ws.cue_lists[0].cues)
}

fn number(ws: &Workspace, n: u128) -> Option<String> {
    ws.find_cue(cue_id(n)).unwrap().number.clone()
}

fn bytes(ws: &Workspace) -> String {
    serde_json::to_string(ws).unwrap()
}

/// The command is refused and the workspace is unchanged, byte for byte.
#[track_caller]
fn refused(ws: &mut Workspace, command: Command) -> CommandError {
    // Debug text rather than `==`, so a NaN in the workspace compares equal to itself.
    let (before, text) = (format!("{ws:?}"), bytes(ws));
    let error = ws.apply(command).unwrap_err();
    assert_eq!(
        format!("{ws:?}"),
        before,
        "refused command changed the workspace"
    );
    assert_eq!(bytes(ws), text);
    error
}

/// Applies, then checks that undo restores the original and redo the result. Returns nothing:
/// the workspace is left in the applied state.
#[track_caller]
fn round_trip(ws: &mut Workspace, command: Command) {
    let before = ws.clone();
    let inverse = ws.apply(command).unwrap();
    let after = ws.clone();
    let redo = ws.apply(inverse).unwrap();
    assert_eq!(*ws, before, "undo did not restore the workspace");
    assert_eq!(bytes(ws), bytes(&before));
    ws.apply(redo).unwrap();
    assert_eq!(*ws, after, "redo did not restore the result");
}

/// No validation issue at all, and the workspace saves.
#[track_caller]
fn assert_clean(ws: &Workspace) {
    assert_eq!(validate(ws), vec![]);
    save_show(ws).unwrap();
}

/// Groups 101 ⊃ 102 ⊃ … ⊃ 100+`depth`, each nested in the last. Depth `depth` at the bottom.
fn chain(depth: usize) -> Cue {
    let mut cue_ = grp(100 + depth as u128, vec![]);
    for level in (1..depth).rev() {
        cue_ = grp(100 + level as u128, vec![cue_]);
    }
    cue_
}

fn sample() -> Workspace {
    // 1, 2[3, 4[5]], 6
    workspace(vec![
        memo(1),
        grp(2, vec![memo(3), grp(4, vec![memo(5)])]),
        memo(6),
    ])
}

// ---- add -----------------------------------------------------------------------------------

#[test]
fn add_inserts_at_the_index_and_undo_removes() {
    let mut ws = sample();
    round_trip(&mut ws, add(pos(None, 1), memo(10)));
    assert_eq!(top(&ws), "1 10 2[3 4[5]] 6");
    round_trip(&mut ws, add(pos(Some(4), 1), memo(11)));
    assert_eq!(top(&ws), "1 10 2[3 4[5 11]] 6");
    round_trip(&mut ws, add(pos(None, 4), memo(12)));
    assert_clean(&ws);
}

#[test]
fn add_group_with_children_keeps_the_subtree() {
    let mut ws = sample();
    round_trip(
        &mut ws,
        add(
            pos(Some(2), 0),
            grp(10, vec![memo(11), grp(12, vec![memo(13)])]),
        ),
    );
    assert_eq!(top(&ws), "1 2[10[11 12[13]] 3 4[5]] 6");
    assert_clean(&ws);
}

#[test]
fn add_with_a_bad_position_is_refused() {
    let mut ws = sample();
    assert_eq!(
        refused(&mut ws, add(pos(None, 4), memo(10))),
        CommandError::IndexOutOfRange { index: 4, len: 3 }
    );
    assert_eq!(
        refused(&mut ws, add(pos(Some(99), 0), memo(10))),
        CommandError::CueNotFound(cue_id(99))
    );
    assert_eq!(
        refused(&mut ws, add(pos(Some(1), 0), memo(10))),
        CommandError::NotAGroup(cue_id(1))
    );
    let missing_list = Position {
        list: list_id(9),
        parent: None,
        index: 0,
    };
    assert_eq!(
        refused(&mut ws, add(missing_list, memo(10))),
        CommandError::CueListNotFound(list_id(9))
    );
}

// ---- invariants ----------------------------------------------------------------------

#[test]
fn add_refuses_a_duplicate_id() {
    let mut ws = sample();
    assert_eq!(
        refused(&mut ws, add(pos(None, 0), memo(5))),
        CommandError::DuplicateCueId(cue_id(5))
    );
}

#[test]
fn add_refuses_a_duplicate_id_inside_the_new_subtree() {
    let mut ws = sample();
    let twin = grp(10, vec![memo(11), memo(11)]);
    assert_eq!(
        refused(&mut ws, add(pos(None, 0), twin)),
        CommandError::DuplicateCueId(cue_id(11))
    );
    // A child reusing its parent's id.
    let reuse = grp(10, vec![memo(10)]);
    assert!(matches!(
        refused(&mut ws, add(pos(None, 0), reuse)),
        CommandError::DuplicateCueId(_)
    ));
}

#[test]
fn add_refuses_nesting_beyond_the_limit() {
    let mut ws = workspace(vec![chain(MAX_GROUP_DEPTH)]);
    assert_clean(&ws);
    let deepest = 100 + MAX_GROUP_DEPTH as u128;
    assert_eq!(
        refused(&mut ws, add(pos(Some(deepest), 0), memo(10))),
        CommandError::NestingTooDeep
    );
    // A group of two levels does not fit one level from the bottom either.
    let above = deepest - 1;
    assert_eq!(
        refused(&mut ws, add(pos(Some(above), 1), grp(10, vec![memo(11)]))),
        CommandError::NestingTooDeep
    );
    // One cue fits: it lands exactly on the limit.
    round_trip(&mut ws, add(pos(Some(above), 1), memo(10)));
    assert_clean(&ws);
}

#[test]
fn add_refuses_a_new_subtree_that_is_too_deep_by_itself() {
    let mut ws = sample();
    assert_eq!(
        refused(&mut ws, add(pos(None, 0), chain(MAX_GROUP_DEPTH + 1))),
        CommandError::NestingTooDeep
    );
    assert_eq!(
        refused(&mut ws, add(pos(Some(2), 0), chain(MAX_GROUP_DEPTH))),
        CommandError::NestingTooDeep
    );
    round_trip(&mut ws, add(pos(None, 0), chain(MAX_GROUP_DEPTH)));
    assert_clean(&ws);
}

#[test]
fn add_refuses_a_hostile_tree_without_overflowing_the_stack() {
    let mut ws = sample();
    let mut deep = memo(10_000);
    for n in 0..3000 {
        deep = grp(20_000 + n, vec![deep]);
    }
    assert_eq!(
        refused(&mut ws, add(pos(None, 0), deep)),
        CommandError::NestingTooDeep
    );
}

fn with_pre_wait(n: u128, pre_wait: f64) -> Cue {
    Cue {
        pre_wait,
        ..memo(n)
    }
}

fn audio_with_master(n: u128, master_db: f64) -> Cue {
    let mut kind = audio();
    if let CueKind::Audio { levels, .. } = &mut kind {
        levels.master_db = master_db;
    }
    cue(n, kind)
}

#[test]
fn add_refuses_non_finite_and_out_of_range_numbers() {
    let mut ws = sample();
    for bad in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        -1.0,
        MAX_TIME + 1.0,
    ] {
        let error = refused(&mut ws, add(pos(None, 0), with_pre_wait(10, bad)));
        assert!(
            matches!(error, CommandError::InvalidCue(_)),
            "{bad}: {error}"
        );
    }
    for bad in [f64::NAN, f64::INFINITY, MAX_DB + 1.0] {
        let error = refused(&mut ws, add(pos(None, 0), audio_with_master(10, bad)));
        assert!(
            matches!(error, CommandError::InvalidCue(_)),
            "{bad}: {error}"
        );
    }
    let wait = cue(10, CueKind::Wait { duration: f64::NAN });
    assert!(matches!(
        refused(&mut ws, add(pos(None, 0), wait)),
        CommandError::InvalidCue(_)
    ));
    // A bad value deep inside a pasted group is found too.
    let nested = grp(10, vec![grp(11, vec![with_pre_wait(12, f64::NAN)])]);
    assert!(matches!(
        refused(&mut ws, add(pos(None, 0), nested)),
        CommandError::InvalidCue(_)
    ));
    // Silence is a valid level.
    round_trip(
        &mut ws,
        add(pos(None, 0), audio_with_master(10, SILENCE_DB)),
    );
}

#[test]
fn add_refuses_what_repair_would_change_without_a_warning() {
    // Below silence and empty gangs are tidied on load without a warning; a command must not
    // produce them, or the saved show would not load back as the workspace was.
    let mut ws = sample();
    assert!(matches!(
        refused(&mut ws, add(pos(None, 0), audio_with_master(10, -200.0))),
        CommandError::InvalidCue(_)
    ));
    let mut kind = audio();
    if let CueKind::Audio { levels, .. } = &mut kind {
        levels.gangs.push(vec![]);
    }
    assert!(matches!(
        refused(&mut ws, add(pos(None, 0), cue(10, kind))),
        CommandError::InvalidCue(_)
    ));
}

#[test]
fn add_refuses_bad_slices_and_hotkeys() {
    let mut ws = sample();
    let mut kind = audio();
    if let CueKind::Audio { slices, .. } = &mut kind {
        slices.push(SliceMarker {
            time: f64::NAN,
            play_count: 1,
        });
    }
    assert!(matches!(
        refused(&mut ws, add(pos(None, 0), cue(10, kind))),
        CommandError::InvalidCue(_)
    ));
    let mut keyed = memo(10);
    keyed.triggers.hotkey = Some(Hotkey {
        key: "Space".into(),
        modifiers: Modifiers::default(),
    });
    assert!(matches!(
        refused(&mut ws, add(pos(None, 0), keyed)),
        CommandError::InvalidCue(_)
    ));
}

#[test]
fn add_refuses_duplicate_and_empty_numbers() {
    let mut ws = workspace(vec![numbered(1, "7", CueKind::Memo)]);
    assert_eq!(
        refused(&mut ws, add(pos(None, 0), numbered(2, "7", CueKind::Memo))),
        CommandError::DuplicateNumber {
            number: "7".into(),
            used_by: cue_id(1)
        }
    );
    assert_eq!(
        refused(&mut ws, add(pos(None, 0), numbered(2, "", CueKind::Memo))),
        CommandError::EmptyNumber(cue_id(2))
    );
    let pair = grp(
        2,
        vec![
            numbered(3, "8", CueKind::Memo),
            numbered(4, "8", CueKind::Memo),
        ],
    );
    assert!(matches!(
        refused(&mut ws, add(pos(None, 0), pair)),
        CommandError::DuplicateNumber { .. }
    ));
}

#[test]
fn references_are_not_checked_so_undo_can_restore_broken_states() {
    // A dangling target is the broken-cue state: validate warns, the show still saves.
    let mut ws = workspace(vec![cue(1, audio()), memo(2)]);
    round_trip(&mut ws, add(pos(None, 0), cue(10, fade(99))));
    let issues = validate(&ws);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].severity, Severity::Warning);
    assert!(matches!(issues[0].kind, IssueKind::TargetMissing { .. }));
    save_show(&ws).unwrap();
}

#[test]
fn undoing_a_transaction_that_removed_a_target_and_its_fade_restores_both() {
    let mut ws = workspace(vec![cue(1, audio()), cue(2, fade(1))]);
    let before = ws.clone();
    let mut history = History::new();
    let command = Command::Transaction {
        commands: vec![
            Command::RemoveCue { id: cue_id(1) },
            set(2, CueProperty::Target { value: None }),
            Command::RemoveCue { id: cue_id(2) },
        ],
    };
    history.execute(&mut ws, command).unwrap();
    history.undo(&mut ws).unwrap();
    assert_eq!(ws, before);
    history.redo(&mut ws).unwrap();
    history.undo(&mut ws).unwrap();
    assert_eq!(ws, before);
}

// ---- auto-numbering (ED-08) -----------------------------------------------------------------

fn auto(n: u128) -> Command {
    Command::AddCue {
        at: pos(None, 0),
        cue: memo(n),
        auto_number: true,
    }
}

#[test]
fn auto_numbering_follows_the_workspace_settings() {
    let mut ws = workspace(vec![]);
    ws.settings.auto_number_start = 10.0;
    ws.settings.auto_number_increment = 5.0;
    for n in 1..=3 {
        ws.apply(auto(n)).unwrap();
    }
    assert_eq!(number(&ws, 1).as_deref(), Some("10"));
    assert_eq!(number(&ws, 2).as_deref(), Some("15"));
    assert_eq!(number(&ws, 3).as_deref(), Some("20"));
    assert_clean(&ws);
}

#[test]
fn auto_numbering_continues_after_the_highest_numeric_number() {
    let mut ws = workspace(vec![
        numbered(1, "4", CueKind::Memo),
        numbered(2, "Intro", CueKind::Memo),
        numbered(3, "2.5", CueKind::Memo),
    ]);
    ws.apply(auto(10)).unwrap();
    assert_eq!(number(&ws, 10).as_deref(), Some("5"));
    // The start is a floor.
    ws.settings.auto_number_start = 100.0;
    ws.apply(auto(11)).unwrap();
    assert_eq!(number(&ws, 11).as_deref(), Some("100"));
}

#[test]
fn auto_numbering_has_no_float_noise() {
    let mut ws = workspace(vec![]);
    ws.settings.auto_number_start = 0.1;
    ws.settings.auto_number_increment = 0.1;
    for n in 1..=4 {
        ws.apply(auto(n)).unwrap();
    }
    let numbers: Vec<_> = (1..=4).map(|n| number(&ws, n).unwrap()).collect();
    assert_eq!(numbers, ["0.1", "0.2", "0.3", "0.4"]);
}

#[test]
fn auto_number_replaces_the_cues_own_number_and_survives_undo_and_redo() {
    let mut ws = workspace(vec![numbered(1, "3", CueKind::Memo)]);
    let before = ws.clone();
    let mut history = History::new();
    let command = Command::AddCue {
        at: pos(None, 1),
        cue: numbered(2, "99", CueKind::Memo),
        auto_number: true,
    };
    history.execute(&mut ws, command).unwrap();
    assert_eq!(number(&ws, 2).as_deref(), Some("4"));
    let after = ws.clone();
    assert!(history.undo(&mut ws).unwrap());
    assert_eq!(ws, before);
    assert!(history.redo(&mut ws).unwrap());
    assert_eq!(ws, after);
}

#[test]
fn auto_numbering_with_invalid_settings_is_refused() {
    for (start, increment) in [
        (1.0, 0.0),
        (1.0, -1.0),
        (f64::NAN, 1.0),
        (1.0, f64::NAN),
        (-1.0, 1.0),
    ] {
        let mut ws = workspace(vec![]);
        ws.settings.auto_number_start = start;
        ws.settings.auto_number_increment = increment;
        assert!(matches!(
            refused(&mut ws, auto(1)),
            CommandError::InvalidNumbering(_)
        ));
    }
}

// ---- remove ----------------------------------------------------------------------------------

#[test]
fn remove_takes_out_the_whole_subtree_and_undo_restores_it_exactly() {
    let mut ws = sample();
    round_trip(&mut ws, Command::RemoveCue { id: cue_id(2) });
    assert_eq!(top(&ws), "1 6");
    round_trip(&mut ws, Command::RemoveCue { id: cue_id(1) });
    assert_eq!(top(&ws), "6");
    assert_clean(&ws);
}

#[test]
fn remove_a_nested_cue() {
    let mut ws = sample();
    round_trip(&mut ws, Command::RemoveCue { id: cue_id(5) });
    assert_eq!(top(&ws), "1 2[3 4[]] 6");
}

#[test]
fn remove_unknown_cue_is_refused() {
    let mut ws = sample();
    assert_eq!(
        refused(&mut ws, Command::RemoveCue { id: cue_id(99) }),
        CommandError::CueNotFound(cue_id(99))
    );
}

#[test]
fn removing_a_target_leaves_the_fade_broken_and_undo_repairs_it() {
    let mut ws = workspace(vec![cue(1, audio()), cue(2, fade(1))]);
    let mut history = History::new();
    history
        .execute(&mut ws, Command::RemoveCue { id: cue_id(1) })
        .unwrap();
    let issues = validate(&ws);
    assert!(
        issues
            .iter()
            .all(|i| matches!(i.kind, IssueKind::TargetMissing { .. })),
        "{issues:?}"
    );
    assert_eq!(issues.len(), 1);
    save_show(&ws).unwrap();
    history.undo(&mut ws).unwrap();
    assert_clean(&ws);
}

// ---- move and reparent (ED-03) ----------------------------------------------------------------

#[test]
fn move_reorders_within_the_same_parent() {
    let mut ws = workspace(vec![memo(1), memo(2), memo(3), memo(4)]);
    round_trip(
        &mut ws,
        Command::MoveCue {
            id: cue_id(1),
            index: 3,
        },
    );
    assert_eq!(top(&ws), "2 3 4 1");
    round_trip(
        &mut ws,
        Command::MoveCue {
            id: cue_id(1),
            index: 0,
        },
    );
    assert_eq!(top(&ws), "1 2 3 4");
    round_trip(
        &mut ws,
        Command::MoveCue {
            id: cue_id(3),
            index: 1,
        },
    );
    assert_eq!(top(&ws), "1 3 2 4");
    // Staying put is allowed.
    round_trip(
        &mut ws,
        Command::MoveCue {
            id: cue_id(3),
            index: 1,
        },
    );
    assert_eq!(top(&ws), "1 3 2 4");
}

#[test]
fn move_within_a_group() {
    let mut ws = sample();
    round_trip(
        &mut ws,
        Command::MoveCue {
            id: cue_id(3),
            index: 1,
        },
    );
    assert_eq!(top(&ws), "1 2[4[5] 3] 6");
}

#[test]
fn move_past_the_end_is_refused() {
    let mut ws = workspace(vec![memo(1), memo(2), memo(3)]);
    assert_eq!(
        refused(
            &mut ws,
            Command::MoveCue {
                id: cue_id(1),
                index: 3
            }
        ),
        CommandError::IndexOutOfRange { index: 3, len: 2 }
    );
    assert_eq!(
        refused(
            &mut ws,
            Command::MoveCue {
                id: cue_id(9),
                index: 0
            }
        ),
        CommandError::CueNotFound(cue_id(9))
    );
}

fn reparent(id: u128, to: Position) -> Command {
    Command::ReparentCue { id: cue_id(id), to }
}

#[test]
fn reparent_into_and_out_of_groups() {
    let mut ws = sample();
    round_trip(&mut ws, reparent(1, pos(Some(2), 1)));
    assert_eq!(top(&ws), "2[3 1 4[5]] 6");
    round_trip(&mut ws, reparent(5, pos(None, 0)));
    assert_eq!(top(&ws), "5 2[3 1 4[]] 6");
    round_trip(&mut ws, reparent(6, pos(Some(4), 0)));
    assert_eq!(top(&ws), "5 2[3 1 4[6]]");
    assert_clean(&ws);
}

#[test]
fn reparent_a_group_moves_its_children_with_it() {
    let mut ws = sample();
    round_trip(&mut ws, reparent(2, pos(None, 0)));
    assert_eq!(top(&ws), "2[3 4[5]] 1 6");
    round_trip(&mut ws, reparent(4, pos(None, 3)));
    assert_eq!(top(&ws), "2[3] 1 6 4[5]");
    round_trip(&mut ws, reparent(1, pos(Some(4), 1)));
    assert_eq!(top(&ws), "2[3] 6 4[5 1]");
}

#[test]
fn reparent_when_taking_the_cue_out_shifts_the_destination() {
    // The destination group sits after the moved cue, so its index changes when it is removed.
    let mut ws = workspace(vec![memo(1), memo(2), grp(3, vec![memo(4)])]);
    round_trip(&mut ws, reparent(1, pos(Some(3), 1)));
    assert_eq!(top(&ws), "2 3[4 1]");
}

fn two_lists() -> Workspace {
    let mut ws = workspace(vec![memo(1), grp(2, vec![memo(3)])]);
    ws.cue_lists.push(CueList {
        id: list_id(2),
        name: "Second".into(),
        cues: vec![memo(4)],
    });
    ws
}

#[test]
fn reparent_across_cue_lists() {
    let mut ws = two_lists();
    let second = |parent: Option<u128>, index| Position {
        list: list_id(2),
        parent: parent.map(cue_id),
        index,
    };
    round_trip(&mut ws, reparent(2, second(None, 1)));
    assert_eq!(shape(&ws.cue_lists[0].cues), "1");
    assert_eq!(shape(&ws.cue_lists[1].cues), "4 2[3]");
    round_trip(&mut ws, reparent(1, second(Some(2), 0)));
    assert_eq!(shape(&ws.cue_lists[1].cues), "4 2[1 3]");
    assert_clean(&ws);
}

#[test]
fn reparent_refuses_a_parent_in_another_list() {
    let mut ws = two_lists();
    let wrong = Position {
        list: list_id(2),
        parent: Some(cue_id(2)),
        index: 0,
    };
    assert_eq!(
        refused(&mut ws, reparent(4, wrong)),
        CommandError::ParentNotInList {
            parent: cue_id(2),
            list: list_id(2)
        }
    );
}

#[test]
fn reparent_into_itself_or_a_descendant_is_refused() {
    let mut ws = sample();
    assert_eq!(
        refused(&mut ws, reparent(2, pos(Some(2), 0))),
        CommandError::MoveIntoSelf(cue_id(2))
    );
    assert_eq!(
        refused(&mut ws, reparent(2, pos(Some(4), 0))),
        CommandError::MoveIntoSelf(cue_id(2))
    );
    assert_eq!(
        refused(&mut ws, reparent(1, pos(Some(3), 0))),
        CommandError::NotAGroup(cue_id(3))
    );
}

#[test]
fn reparent_refuses_nesting_beyond_the_limit() {
    let mut ws = workspace(vec![
        chain(MAX_GROUP_DEPTH - 1),
        grp(2, vec![grp(5, vec![memo(3)])]),
        memo(4),
    ]);
    assert_clean(&ws);
    let deepest = 100 + (MAX_GROUP_DEPTH - 1) as u128;
    // Group 2 is three levels deep and would end at depth 34.
    assert_eq!(
        refused(&mut ws, reparent(2, pos(Some(deepest), 0))),
        CommandError::NestingTooDeep
    );
    assert_eq!(
        refused(&mut ws, reparent(101, pos(Some(5), 0))),
        CommandError::NestingTooDeep
    );
    // A single cue lands on the limit.
    round_trip(&mut ws, reparent(4, pos(Some(deepest), 0)));
    assert_clean(&ws);
}

// ---- properties (ED-04) -----------------------------------------------------------------------

#[test]
fn every_common_property_can_be_set_and_undone() {
    let hotkey = Hotkey {
        key: "f5".into(),
        modifiers: Modifiers::default(),
    };
    let properties = [
        CueProperty::Name {
            value: "Door slam".into(),
        },
        CueProperty::Notes {
            value: "Check levels".into(),
        },
        CueProperty::Color {
            value: CueColor::Red,
        },
        CueProperty::Flagged { value: true },
        CueProperty::Armed { value: false },
        CueProperty::PreWait { value: 1.5 },
        CueProperty::PostWait { value: 2.5 },
        CueProperty::ContinueMode {
            value: ContinueMode::AutoFollow,
        },
        CueProperty::Triggers {
            value: Triggers {
                hotkey: Some(hotkey),
                osc: Some("/cue/1".into()),
            },
        },
    ];
    for property in properties {
        let mut ws = sample();
        round_trip(&mut ws, set(1, property.clone()));
        assert_ne!(ws, sample(), "{property:?} changed nothing");
        assert_clean(&ws);
    }
}

#[test]
fn set_target_group_mode_and_kind() {
    let mut ws = workspace(vec![
        cue(1, audio()),
        cue(2, fade(1)),
        grp(3, vec![memo(4)]),
        memo(5),
    ]);
    round_trip(
        &mut ws,
        set(
            2,
            CueProperty::Target {
                value: Some(cue_id(3)),
            },
        ),
    );
    assert_eq!(
        ws.find_cue(cue_id(2)).unwrap().kind.target(),
        Some(cue_id(3))
    );
    round_trip(&mut ws, set(2, CueProperty::Target { value: None }));
    round_trip(
        &mut ws,
        set(
            3,
            CueProperty::GroupMode {
                value: GroupMode::StartRandom,
            },
        ),
    );
    round_trip(
        &mut ws,
        set(
            5,
            CueProperty::Kind {
                value: CueKind::Wait { duration: 3.0 },
            },
        ),
    );
    round_trip(&mut ws, set(1, CueProperty::Kind { value: audio() }));
    assert_clean(&ws);
}

#[test]
fn set_property_refuses_the_wrong_kind_of_cue() {
    let mut ws = workspace(vec![memo(1), cue(2, audio()), grp(3, vec![])]);
    assert_eq!(
        refused(&mut ws, set(1, CueProperty::Target { value: None })),
        CommandError::PropertyNotApplicable(cue_id(1))
    );
    assert_eq!(
        refused(
            &mut ws,
            set(
                2,
                CueProperty::GroupMode {
                    value: GroupMode::Timeline
                }
            )
        ),
        CommandError::PropertyNotApplicable(cue_id(2))
    );
    assert_eq!(
        refused(&mut ws, set(99, CueProperty::Flagged { value: true })),
        CommandError::CueNotFound(cue_id(99))
    );
    // Kinds change between non-groups only.
    assert_eq!(
        refused(
            &mut ws,
            set(
                3,
                CueProperty::Kind {
                    value: CueKind::Memo
                }
            )
        ),
        CommandError::KindChangeInvolvesGroup(cue_id(3))
    );
    assert_eq!(
        refused(
            &mut ws,
            set(
                1,
                CueProperty::Kind {
                    value: group(vec![])
                }
            )
        ),
        CommandError::KindChangeInvolvesGroup(cue_id(1))
    );
}

#[test]
fn set_property_refuses_non_finite_and_out_of_range_values() {
    let mut ws = workspace(vec![cue(1, audio()), memo(2)]);
    for bad in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        -0.5,
        MAX_TIME + 1.0,
    ] {
        for property in [
            CueProperty::PreWait { value: bad },
            CueProperty::PostWait { value: bad },
            CueProperty::Kind {
                value: CueKind::Wait { duration: bad },
            },
        ] {
            let error = refused(&mut ws, set(2, property));
            assert!(
                matches!(error, CommandError::InvalidCue(_)),
                "{bad}: {error}"
            );
        }
    }
    let mut loud = audio();
    if let CueKind::Audio { levels, .. } = &mut loud {
        levels.outputs[0].db = f64::NAN;
    }
    assert!(matches!(
        refused(&mut ws, set(1, CueProperty::Kind { value: loud })),
        CommandError::InvalidCue(_)
    ));
    let mut fade_level = fade(1);
    if let CueKind::Fade { levels, .. } = &mut fade_level {
        levels.master_db = Some(f64::INFINITY);
    }
    assert!(matches!(
        refused(&mut ws, set(2, CueProperty::Kind { value: fade_level })),
        CommandError::InvalidCue(_)
    ));
    assert!(matches!(
        refused(
            &mut ws,
            set(
                2,
                CueProperty::Triggers {
                    value: Triggers {
                        hotkey: Some(Hotkey {
                            key: "A".into(),
                            modifiers: Modifiers::default()
                        }),
                        osc: None,
                    },
                }
            )
        ),
        CommandError::InvalidCue(_)
    ));
}

#[test]
fn an_audio_cue_with_more_outputs_than_its_patch_can_still_be_edited() {
    // Kept on purpose, so shrinking a patch by mistake loses no routing: not a refusal.
    let mut ws = workspace(vec![cue(1, audio())]);
    if let CueKind::Audio { levels, .. } = &mut ws.cue_lists[0].cues[0].kind {
        levels.outputs = vec![out(0.0); 4];
    }
    let kind = ws.cue_lists[0].cues[0].kind.clone();
    round_trip(
        &mut ws,
        set(
            1,
            CueProperty::Name {
                value: "Still editable".into(),
            },
        ),
    );
    round_trip(
        &mut ws,
        set(
            1,
            CueProperty::Kind {
                value: kind.clone(),
            },
        ),
    );
    round_trip(&mut ws, add(pos(None, 1), cue(2, kind.clone())));
}

// ---- batch set --------------------------------------------------------------------------------

#[test]
fn batch_set_changes_every_cue_and_undoes_as_one() {
    let mut ws = sample();
    let batch = Command::BatchSet {
        ids: vec![cue_id(1), cue_id(3), cue_id(5)],
        property: CueProperty::Color {
            value: CueColor::Blue,
        },
    };
    round_trip(&mut ws, batch);
    for n in [1, 3, 5] {
        assert_eq!(ws.find_cue(cue_id(n)).unwrap().color, CueColor::Blue);
    }
    assert_eq!(ws.find_cue(cue_id(6)).unwrap().color, CueColor::None);
}

#[test]
fn batch_set_with_one_bad_cue_changes_nothing() {
    let mut ws = sample();
    let missing = Command::BatchSet {
        ids: vec![cue_id(1), cue_id(99)],
        property: CueProperty::Flagged { value: true },
    };
    let error = refused(&mut ws, missing);
    assert_eq!(
        error,
        CommandError::InTransaction {
            index: 1,
            source: Box::new(CommandError::CueNotFound(cue_id(99)))
        }
    );
    // Group 2 has a mode; memo 1 does not.
    let mode = Command::BatchSet {
        ids: vec![cue_id(2), cue_id(1)],
        property: CueProperty::GroupMode {
            value: GroupMode::StartRandom,
        },
    };
    refused(&mut ws, mode);
    let invalid = Command::BatchSet {
        ids: vec![cue_id(1), cue_id(2)],
        property: CueProperty::PreWait { value: f64::NAN },
    };
    refused(&mut ws, invalid);
}

#[test]
fn batch_set_rolls_back_cues_already_written_when_a_later_one_is_refused() {
    let mut ws = sample();
    // Cues 1 and 3 are written before the missing id refuses the batch; 1 is listed twice.
    let before = ws.clone();
    let batch = Command::BatchSet {
        ids: vec![cue_id(1), cue_id(3), cue_id(1), cue_id(99)],
        property: CueProperty::Color {
            value: CueColor::Red,
        },
    };
    let error = refused(&mut ws, batch);
    assert_eq!(
        error,
        CommandError::InTransaction {
            index: 3,
            source: Box::new(CommandError::CueNotFound(cue_id(99)))
        }
    );
    assert_eq!(ws, before);
    assert_eq!(show_text(&ws), show_text(&before));
}

#[test]
fn batch_set_with_a_repeated_cue_undoes_to_the_first_value() {
    let mut ws = sample();
    let before = ws.clone();
    let inverse = ws
        .apply(Command::BatchSet {
            ids: vec![cue_id(1), cue_id(1)],
            property: CueProperty::Flagged { value: true },
        })
        .unwrap();
    ws.apply(inverse).unwrap();
    assert_eq!(ws, before);
}

#[test]
fn batch_set_refuses_kind() {
    let mut ws = sample();
    let batch = Command::BatchSet {
        ids: vec![cue_id(1)],
        property: CueProperty::Kind {
            value: CueKind::Memo,
        },
    };
    assert_eq!(refused(&mut ws, batch), CommandError::NotBatchable);
}

#[test]
fn batch_set_inside_a_transaction_flattens_its_inverse() {
    let mut ws = sample();
    let before = ws.clone();
    let inverse = ws
        .apply(Command::Transaction {
            commands: vec![
                Command::BatchSet {
                    ids: vec![cue_id(1), cue_id(3)],
                    property: CueProperty::Armed { value: false },
                },
                Command::RemoveCue { id: cue_id(6) },
            ],
        })
        .unwrap();
    let Command::Transaction { commands } = &inverse else {
        panic!("inverse of a transaction is a transaction");
    };
    assert!(
        commands
            .iter()
            .all(|c| !matches!(c, Command::Transaction { .. }))
    );
    ws.apply(inverse).unwrap();
    assert_eq!(ws, before);
}

// ---- numbers and renumbering (ED-08) -----------------------------------------------------------

fn assign(id: u128, number: Option<&str>) -> NumberAssignment {
    NumberAssignment {
        id: cue_id(id),
        number: number.map(String::from),
    }
}

#[test]
fn numbers_can_be_set_cleared_and_swapped() {
    let mut ws = workspace(vec![
        numbered(1, "1", CueKind::Memo),
        numbered(2, "2", CueKind::Memo),
        memo(3),
    ]);
    round_trip(
        &mut ws,
        Command::SetCueNumbers {
            numbers: vec![assign(1, Some("2")), assign(2, Some("1"))],
        },
    );
    assert_eq!(
        (number(&ws, 1).as_deref(), number(&ws, 2).as_deref()),
        (Some("2"), Some("1"))
    );
    round_trip(
        &mut ws,
        Command::SetCueNumbers {
            numbers: vec![assign(1, None), assign(3, Some("2"))],
        },
    );
    assert_eq!(number(&ws, 1), None);
    assert_clean(&ws);
}

#[test]
fn setting_numbers_refuses_duplicates_empties_and_unknown_cues() {
    let mut ws = workspace(vec![numbered(1, "1", CueKind::Memo), memo(2), memo(3)]);
    assert_eq!(
        refused(
            &mut ws,
            Command::SetCueNumbers {
                numbers: vec![assign(2, Some("1"))]
            }
        ),
        CommandError::DuplicateNumber {
            number: "1".into(),
            used_by: cue_id(1)
        }
    );
    assert!(matches!(
        refused(
            &mut ws,
            Command::SetCueNumbers {
                numbers: vec![assign(2, Some("5")), assign(3, Some("5"))]
            }
        ),
        CommandError::DuplicateNumber { .. }
    ));
    assert_eq!(
        refused(
            &mut ws,
            Command::SetCueNumbers {
                numbers: vec![assign(2, Some(""))]
            }
        ),
        CommandError::EmptyNumber(cue_id(2))
    );
    assert_eq!(
        refused(
            &mut ws,
            Command::SetCueNumbers {
                numbers: vec![assign(99, Some("9"))]
            }
        ),
        CommandError::CueNotFound(cue_id(99))
    );
    assert_eq!(
        refused(
            &mut ws,
            Command::SetCueNumbers {
                numbers: vec![assign(2, Some("8")), assign(2, Some("9"))]
            }
        ),
        CommandError::DuplicateAssignment(cue_id(2))
    );
}

fn renumber(ids: &[u128], start: f64, increment: f64) -> Command {
    Command::Renumber {
        ids: ids.iter().map(|&n| cue_id(n)).collect(),
        start,
        increment,
    }
}

#[test]
fn renumber_follows_cue_list_order_whatever_order_the_ids_come_in() {
    let mut ws = workspace(vec![
        numbered(1, "9", CueKind::Memo),
        memo(2),
        numbered(3, "1", CueKind::Memo),
    ]);
    round_trip(&mut ws, renumber(&[3, 1, 2], 10.0, 5.0));
    let numbers: Vec<_> = [1, 2, 3].map(|n| number(&ws, n).unwrap()).to_vec();
    assert_eq!(numbers, ["10", "15", "20"]);
    assert_clean(&ws);
}

#[test]
fn renumber_handles_groups_and_nested_groups() {
    let mut ws = workspace(vec![
        numbered(1, "a", CueKind::Memo),
        grp(2, vec![memo(3), grp(4, vec![memo(5), memo(6)]), memo(7)]),
        memo(8),
    ]);
    // A group is numbered before its children, depth first.
    round_trip(&mut ws, renumber(&[8, 7, 6, 5, 4, 3, 2, 1], 1.0, 1.0));
    let numbers: Vec<_> = (1..=8).map(|n| number(&ws, n).unwrap()).collect();
    assert_eq!(numbers, ["1", "2", "3", "4", "5", "6", "7", "8"]);
    // Only the listed cues change; the group itself is skipped.
    round_trip(&mut ws, renumber(&[3, 5, 7], 100.0, 0.5));
    assert_eq!(number(&ws, 2).as_deref(), Some("2"));
    assert_eq!(
        [3, 5, 7].map(|n| number(&ws, n).unwrap()),
        ["100", "100.5", "101"]
    );
    assert_clean(&ws);
}

#[test]
fn renumber_uses_whatever_start_and_increment_it_is_given() {
    let mut ws = workspace(vec![memo(1), memo(2), memo(3)]);
    ws.settings.auto_number_start = 50.0;
    round_trip(&mut ws, renumber(&[1, 2, 3], 0.1, 0.1));
    assert_eq!(
        [1, 2, 3].map(|n| number(&ws, n).unwrap()),
        ["0.1", "0.2", "0.3"]
    );
}

#[test]
fn renumber_refuses_numbers_used_by_cues_outside_the_selection() {
    let mut ws = workspace(vec![memo(1), memo(2), numbered(3, "2", CueKind::Memo)]);
    assert_eq!(
        refused(&mut ws, renumber(&[1, 2], 1.0, 1.0)),
        CommandError::DuplicateNumber {
            number: "2".into(),
            used_by: cue_id(2)
        }
    );
    // Selecting every cue is fine: the old numbers are all replaced.
    round_trip(&mut ws, renumber(&[1, 2, 3], 1.0, 1.0));
}

#[test]
fn renumber_refuses_bad_arguments() {
    let mut ws = workspace(vec![memo(1), memo(2)]);
    for (start, increment) in [
        (f64::NAN, 1.0),
        (f64::INFINITY, 1.0),
        (-1.0, 1.0),
        (MAX_AUTO_NUMBER + 1.0, 1.0),
        (1.0, 0.0),
        (1.0, -1.0),
        (1.0, f64::NAN),
        (1.0, f64::INFINITY),
        (1.0, MAX_AUTO_NUMBER + 1.0),
    ] {
        assert!(
            matches!(
                refused(&mut ws, renumber(&[1, 2], start, increment)),
                CommandError::InvalidNumbering(_)
            ),
            "{start} {increment}"
        );
    }
    assert_eq!(
        refused(&mut ws, renumber(&[1, 99], 1.0, 1.0)),
        CommandError::CueNotFound(cue_id(99))
    );
    // So small an increment that rounding would give two cues the same number.
    assert!(matches!(
        refused(&mut ws, renumber(&[1, 2], 1.0, 1e-12)),
        CommandError::DuplicateNumber { .. }
    ));
}

// ---- paste (ED-05) ----------------------------------------------------------------------------

fn fresh_ids(first: u128) -> impl FnMut() -> CueId {
    let mut next = first;
    move || {
        next += 1;
        cue_id(next)
    }
}

#[test]
fn paste_copies_nested_groups_with_fresh_ids() {
    let mut ws = sample();
    let clipboard = vec![ws.cue_lists[0].cues[1].clone()];
    let copy = prepare_paste(&ws, &clipboard, fresh_ids(1000)).unwrap();
    assert_eq!(shape(&copy), "1001[1002 1003[1004]]");
    round_trip(
        &mut ws,
        Command::Paste {
            at: pos(None, 3),
            cues: copy,
        },
    );
    assert_eq!(top(&ws), "1 2[3 4[5]] 6 1001[1002 1003[1004]]");
    assert_clean(&ws);
}

#[test]
fn paste_several_cues_into_a_group_is_one_undo_step() {
    let mut ws = sample();
    let mut history = History::new();
    let before = ws.clone();
    let clipboard = vec![memo(1), memo(6)];
    let copy = prepare_paste(&ws, &clipboard, fresh_ids(1000)).unwrap();
    history
        .execute(
            &mut ws,
            Command::Paste {
                at: pos(Some(4), 0),
                cues: copy,
            },
        )
        .unwrap();
    assert_eq!(top(&ws), "1 2[3 4[1001 1002 5]] 6");
    let after = ws.clone();
    assert_eq!(history.undo_len(), 1);
    history.undo(&mut ws).unwrap();
    assert_eq!(ws, before);
    history.redo(&mut ws).unwrap();
    assert_eq!(ws, after);
}

#[test]
fn paste_without_fresh_ids_is_refused() {
    let mut ws = sample();
    let stale = ws.cue_lists[0].cues[1].clone();
    assert!(matches!(
        refused(
            &mut ws,
            Command::Paste {
                at: pos(None, 0),
                cues: vec![stale]
            }
        ),
        CommandError::DuplicateCueId(_)
    ));
}

/// Undo and redo skip value checks a loaded show may already fail, never the id check: no
/// command path can give a workspace duplicate cue ids, which the cue engine relies on.
#[test]
fn undo_still_refuses_a_duplicate_id() {
    let mut ws = sample();
    let removed = ws.cue_lists[0].cues[1].clone();
    let mut history = History::new();
    history
        .execute(&mut ws, Command::RemoveCue { id: removed.id })
        .unwrap();
    // A cue with the removed id arrives behind the history's back: undo must not add a second.
    ws.apply(add(pos(None, 0), removed)).unwrap();
    let before = format!("{ws:?}");
    let error = history.undo(&mut ws).unwrap_err();
    let duplicate = match &error {
        CommandError::InTransaction { source, .. } => source.as_ref(),
        other => other,
    };
    assert!(
        matches!(duplicate, CommandError::DuplicateCueId(_)),
        "{error:?}"
    );
    assert_eq!(format!("{ws:?}"), before);
}

#[test]
fn paste_points_copied_targets_at_the_copies_and_clears_numbers_in_use() {
    let mut ws = workspace(vec![
        numbered(1, "1", audio()),
        numbered(2, "2", fade(1)),
        numbered(3, "3", CueKind::Memo),
    ]);
    // Copy a fade together with its target, and another fade without it.
    let clipboard = vec![
        ws.cue_lists[0].cues[0].clone(),
        ws.cue_lists[0].cues[1].clone(),
    ];
    let copy = prepare_paste(&ws, &clipboard, fresh_ids(1000)).unwrap();
    assert_eq!(copy[1].kind.target(), Some(cue_id(1001)));
    assert_eq!(copy[0].number, None);
    round_trip(
        &mut ws,
        Command::Paste {
            at: pos(None, 3),
            cues: copy,
        },
    );

    let alone = vec![ws.cue_lists[0].cues[1].clone()];
    let copy = prepare_paste(&ws, &alone, fresh_ids(2000)).unwrap();
    assert_eq!(copy[0].kind.target(), Some(cue_id(1)));
    round_trip(
        &mut ws,
        Command::Paste {
            at: pos(None, 0),
            cues: copy,
        },
    );
    assert_clean(&ws);
}

#[test]
fn paste_keeps_numbers_that_are_free_and_clears_repeats_within_the_copy() {
    let ws = workspace(vec![numbered(1, "1", CueKind::Memo)]);
    let clipboard = vec![
        numbered(10, "5", CueKind::Memo),
        numbered(11, "5", CueKind::Memo),
        numbered(12, "1", CueKind::Memo),
    ];
    let copy = prepare_paste(&ws, &clipboard, fresh_ids(1000)).unwrap();
    let numbers: Vec<_> = copy.iter().map(|c| c.number.clone()).collect();
    assert_eq!(numbers, [Some("5".into()), None, None]);
}

#[test]
fn paste_into_another_workspace_drops_what_it_does_not_have() {
    let mut source = workspace(vec![cue(1, audio()), cue(2, fade(1)), cue(3, fade(1))]);
    source.patches.push(patch(7, 2));
    if let CueKind::Audio { patch, .. } = &mut source.cue_lists[0].cues[0].kind {
        *patch = patch_id(7);
    }
    let clipboard = vec![
        source.cue_lists[0].cues[0].clone(),
        source.cue_lists[0].cues[1].clone(),
    ];

    let mut target = workspace(vec![memo(50)]);
    let copy = prepare_paste(&target, &clipboard, fresh_ids(1000)).unwrap();
    assert!(matches!(&copy[0].kind, CueKind::Audio { patch, .. } if *patch == patch_id(1)));
    round_trip(
        &mut target,
        Command::Paste {
            at: pos(None, 1),
            cues: copy,
        },
    );
    assert_clean(&target);

    // A fade copied without its target loses the target; it is not left dangling.
    let alone = vec![source.cue_lists[0].cues[2].clone()];
    let copy = prepare_paste(&target, &alone, fresh_ids(2000)).unwrap();
    assert_eq!(copy[0].kind.target(), None);
    round_trip(
        &mut target,
        Command::Paste {
            at: pos(None, 0),
            cues: copy,
        },
    );
    assert_clean(&target);
}

#[test]
fn paste_refuses_nesting_beyond_the_limit() {
    let mut ws = workspace(vec![chain(MAX_GROUP_DEPTH - 1)]);
    let deepest = 100 + (MAX_GROUP_DEPTH - 1) as u128;
    let copy = prepare_paste(&ws, &[grp(1, vec![memo(2)])], fresh_ids(1000)).unwrap();
    assert_eq!(
        refused(
            &mut ws,
            Command::Paste {
                at: pos(Some(deepest), 0),
                cues: copy
            }
        ),
        CommandError::NestingTooDeep
    );
    assert_eq!(
        prepare_paste(&ws, &[chain(MAX_GROUP_DEPTH + 1)], fresh_ids(1000)),
        Err(CommandError::NestingTooDeep)
    );
}

#[test]
fn paste_refuses_a_bad_value_anywhere_in_the_clipboard() {
    let mut ws = sample();
    let bad = grp(10, vec![memo(11), with_pre_wait(12, f64::NAN)]);
    assert!(matches!(
        refused(
            &mut ws,
            Command::Paste {
                at: pos(None, 0),
                cues: vec![bad]
            }
        ),
        CommandError::InvalidCue(_)
    ));
}

#[test]
fn pasting_nothing_is_not_an_edit() {
    let mut ws = sample();
    let mut history = History::new();
    history
        .execute(
            &mut ws,
            Command::Paste {
                at: pos(None, 0),
                cues: vec![],
            },
        )
        .unwrap();
    assert!(!history.can_undo());
}

// ---- transactions ---------------------------------------------------------------------------------

#[test]
fn a_transaction_applies_every_command_and_undoes_as_one() {
    let mut ws = sample();
    let command = Command::Transaction {
        commands: vec![
            add(pos(None, 0), memo(10)),
            add(pos(Some(10), 0), memo(11)),
            reparent(10, pos(Some(4), 0)),
        ],
    };
    // The second command needs 10 to be a group: it is not, so the whole thing is refused.
    refused(&mut ws, command);

    let command = Command::Transaction {
        commands: vec![
            add(pos(None, 0), grp(10, vec![])),
            add(pos(Some(10), 0), memo(11)),
            reparent(10, pos(Some(4), 0)),
            set(
                11,
                CueProperty::Name {
                    value: "inside".into(),
                },
            ),
        ],
    };
    round_trip(&mut ws, command);
    assert_eq!(top(&ws), "1 2[3 4[10[11] 5]] 6");
    assert_clean(&ws);
}

#[test]
fn a_transaction_that_fails_partway_leaves_the_workspace_untouched() {
    let mut ws = sample();
    let command = Command::Transaction {
        commands: vec![
            add(pos(None, 0), memo(10)),
            Command::RemoveCue { id: cue_id(2) },
            set(1, CueProperty::Flagged { value: true }),
            add(pos(None, 0), memo(5)), // duplicate id of a cue that was just removed with 2
            add(pos(None, 0), memo(12)),
        ],
    };
    // Command 3 is fine (5 went with group 2), so make the failure the last command instead.
    let _ = command;
    let command = Command::Transaction {
        commands: vec![
            add(pos(None, 0), memo(10)),
            Command::RemoveCue { id: cue_id(2) },
            set(1, CueProperty::Flagged { value: true }),
            add(pos(None, 0), memo(6)), // 6 exists
        ],
    };
    let error = refused(&mut ws, command);
    assert_eq!(
        error,
        CommandError::InTransaction {
            index: 3,
            source: Box::new(CommandError::DuplicateCueId(cue_id(6)))
        }
    );
    assert_eq!(ws, sample());
}

#[test]
fn a_transaction_may_not_contain_a_transaction() {
    let mut ws = sample();
    let nested = Command::Transaction {
        commands: vec![Command::Transaction { commands: vec![] }],
    };
    assert_eq!(
        refused(&mut ws, nested),
        CommandError::InTransaction {
            index: 0,
            source: Box::new(CommandError::NestedTransaction)
        }
    );
}

#[test]
fn an_empty_transaction_does_nothing() {
    let mut ws = sample();
    round_trip(&mut ws, Command::Transaction { commands: vec![] });
    assert_eq!(ws, sample());
}

#[test]
fn a_transaction_can_swap_numbers_and_move_in_one_step() {
    let mut ws = workspace(vec![
        numbered(1, "1", CueKind::Memo),
        numbered(2, "2", CueKind::Memo),
        grp(3, vec![]),
    ]);
    let command = Command::Transaction {
        commands: vec![
            Command::SetCueNumbers {
                numbers: vec![assign(1, Some("2")), assign(2, Some("1"))],
            },
            reparent(1, pos(Some(3), 0)),
        ],
    };
    round_trip(&mut ws, command);
    assert_eq!(top(&ws), "2 3[1]");
}

// ---- undo and redo ------------------------------------------------------------------------------------

#[test]
fn undo_and_redo_walk_through_the_history() {
    let mut ws = workspace(vec![]);
    let mut history = History::new();
    let mut states = vec![ws.clone()];
    for n in 1..=5 {
        history
            .execute(&mut ws, add(pos(None, (n - 1) as usize), memo(n)))
            .unwrap();
        states.push(ws.clone());
    }
    for expected in states.iter().rev().skip(1) {
        assert!(history.undo(&mut ws).unwrap());
        assert_eq!(&ws, expected);
    }
    assert!(!history.can_undo());
    assert!(!history.undo(&mut ws).unwrap());
    for expected in states.iter().skip(1) {
        assert!(history.redo(&mut ws).unwrap());
        assert_eq!(&ws, expected);
    }
    assert!(!history.can_redo());
    assert!(!history.redo(&mut ws).unwrap());
}

#[test]
fn a_new_edit_clears_redo() {
    let mut ws = sample();
    let mut history = History::new();
    history
        .execute(&mut ws, add(pos(None, 0), memo(10)))
        .unwrap();
    history
        .execute(&mut ws, add(pos(None, 0), memo(11)))
        .unwrap();
    history.undo(&mut ws).unwrap();
    assert!(history.can_redo());
    history
        .execute(&mut ws, add(pos(None, 0), memo(12)))
        .unwrap();
    assert!(!history.can_redo());
    assert_eq!(history.redo_len(), 0);
    assert!(!history.redo(&mut ws).unwrap());
    assert_eq!(top(&ws), "12 10 1 2[3 4[5]] 6");
}

#[test]
fn a_refused_edit_keeps_redo_and_the_history() {
    let mut ws = sample();
    let mut history = History::new();
    history
        .execute(&mut ws, add(pos(None, 0), memo(10)))
        .unwrap();
    history.undo(&mut ws).unwrap();
    let state = ws.clone();
    history
        .execute(&mut ws, Command::RemoveCue { id: cue_id(99) })
        .unwrap_err();
    assert_eq!(ws, state);
    assert!(history.can_redo());
    assert_eq!((history.undo_len(), history.redo_len()), (0, 1));
}

#[test]
fn an_empty_transaction_does_not_clear_redo() {
    let mut ws = sample();
    let mut history = History::new();
    history
        .execute(&mut ws, add(pos(None, 0), memo(10)))
        .unwrap();
    history.undo(&mut ws).unwrap();
    history
        .execute(&mut ws, Command::Transaction { commands: vec![] })
        .unwrap();
    assert!(history.can_redo());
    assert!(!history.can_undo());
}

#[test]
fn a_transaction_is_one_undo_step() {
    let mut ws = sample();
    let before = ws.clone();
    let mut history = History::new();
    let command = Command::Transaction {
        commands: vec![
            add(pos(None, 0), memo(10)),
            Command::RemoveCue { id: cue_id(2) },
            Command::Renumber {
                ids: vec![cue_id(1), cue_id(10)],
                start: 1.0,
                increment: 1.0,
            },
        ],
    };
    history.execute(&mut ws, command).unwrap();
    assert_eq!(history.undo_len(), 1);
    history.undo(&mut ws).unwrap();
    assert_eq!(ws, before);
}

#[test]
fn undo_keeps_going_over_a_thousand_steps() {
    let mut ws = workspace(vec![]);
    let mut history = History::new();
    for n in 0..1000u128 {
        history
            .execute(&mut ws, add(pos(None, n as usize), memo(n + 1)))
            .unwrap();
    }
    assert_eq!(history.undo_len(), 1000);
    while history.undo(&mut ws).unwrap() {}
    assert_eq!(ws, workspace(vec![]));
    while history.redo(&mut ws).unwrap() {}
    assert_eq!(order(&ws).len(), 1000);
}

#[test]
fn an_undo_that_cannot_apply_is_refused_and_stays_on_the_stack() {
    let mut ws = sample();
    let mut history = History::new();
    history
        .execute(&mut ws, add(pos(None, 0), memo(10)))
        .unwrap();
    // The workspace changes behind the history's back.
    ws.apply(Command::RemoveCue { id: cue_id(10) }).unwrap();
    let state = ws.clone();
    assert_eq!(
        history.undo(&mut ws),
        Err(CommandError::CueNotFound(cue_id(10)))
    );
    assert_eq!(ws, state);
    assert_eq!(history.undo_len(), 1);
    history.clear();
    assert!(!history.can_undo() && !history.can_redo());
}

#[test]
fn commands_round_trip_through_json() {
    let command = Command::Transaction {
        commands: vec![
            Command::AddCue {
                at: pos(Some(2), 1),
                cue: grp(10, vec![memo(11)]),
                auto_number: true,
            },
            Command::Paste {
                at: pos(None, 0),
                cues: vec![memo(12)],
            },
            Command::RemoveCue { id: cue_id(1) },
            Command::MoveCue {
                id: cue_id(2),
                index: 0,
            },
            reparent(3, pos(None, 0)),
            set(1, CueProperty::Kind { value: audio() }),
            set(
                1,
                CueProperty::GroupMode {
                    value: GroupMode::Playlist {
                        loop_: true,
                        shuffle: false,
                    },
                },
            ),
            Command::BatchSet {
                ids: vec![cue_id(1)],
                property: CueProperty::Flagged { value: true },
            },
            Command::SetCueNumbers {
                numbers: vec![assign(1, Some("2")), assign(2, None)],
            },
            renumber(&[1, 2], 1.0, 0.5),
        ],
    };
    let text = serde_json::to_string(&command).unwrap();
    assert_eq!(serde_json::from_str::<Command>(&text).unwrap(), command);
}

#[test]
fn a_session_of_edits_still_saves_and_loads_back_unchanged() {
    let mut ws = sample();
    let mut history = History::new();
    for command in [
        add(pos(Some(4), 0), grp(10, vec![memo(11)])),
        Command::Renumber {
            ids: vec![cue_id(1), cue_id(3), cue_id(11)],
            start: 1.0,
            increment: 1.0,
        },
        reparent(10, pos(None, 0)),
        Command::MoveCue {
            id: cue_id(6),
            index: 0,
        },
        Command::BatchSet {
            ids: vec![cue_id(1), cue_id(3)],
            property: CueProperty::Armed { value: false },
        },
        Command::RemoveCue { id: cue_id(4) },
    ] {
        history.execute(&mut ws, command).unwrap();
        assert_clean(&ws);
        let loaded = load_show(&save_show(&ws).unwrap()).unwrap();
        assert_eq!(loaded.workspace, ws);
        assert_eq!(loaded.warnings, vec![]);
    }
}

// ---- durations and load time (M5.6b; CUE-02, CT-14, CT-20) ----------------------------------

/// Wait 1 (2 s), Fade 2 of cue 4 (5 s), Load 3 of cue 4 (1.5 s), Audio 4, Memo 5, Group 6.
fn timed_cues() -> Workspace {
    workspace(vec![
        cue(1, CueKind::Wait { duration: 2.0 }),
        cue(2, fade(4)),
        cue(
            3,
            CueKind::Load {
                target: Some(cue_id(4)),
                load_to_time: 1.5,
            },
        ),
        cue(4, audio()),
        memo(5),
        grp(6, vec![]),
    ])
}

fn kind_of(ws: &Workspace, n: u128) -> CueKind {
    ws.find_cue(cue_id(n)).unwrap().kind.clone()
}

fn duration(value: f64) -> CueProperty {
    CueProperty::Duration { value }
}

fn load_time(value: f64) -> CueProperty {
    CueProperty::LoadToTime { value }
}

#[test]
fn a_wait_or_fade_duration_is_set_alone_and_undone() {
    let mut ws = timed_cues();
    round_trip(&mut ws, set(1, duration(12.25)));
    assert_eq!(kind_of(&ws, 1), CueKind::Wait { duration: 12.25 });
    let mut expected = kind_of(&ws, 2);
    round_trip(&mut ws, set(2, duration(0.5)));
    // Only the duration changes: target, curve, levels and the stop flag stay.
    if let CueKind::Fade { duration, .. } = &mut expected {
        *duration = 0.5;
    }
    assert_eq!(kind_of(&ws, 2), expected);
    assert_clean(&ws);
    // The same value changes nothing and leaves no undo step.
    let inverse = ws.apply(set(1, duration(12.25))).unwrap();
    assert_eq!(inverse, Command::Transaction { commands: vec![] });
}

#[test]
fn a_load_cues_time_is_set_alone_and_undone() {
    let mut ws = timed_cues();
    round_trip(&mut ws, set(3, load_time(90.0)));
    assert_eq!(
        kind_of(&ws, 3),
        CueKind::Load {
            target: Some(cue_id(4)),
            load_to_time: 90.0
        }
    );
    assert_clean(&ws);
}

#[test]
fn durations_and_load_times_at_the_limits_are_accepted() {
    let mut ws = timed_cues();
    for value in [0.0, 0.001, MAX_TIME] {
        ws.apply(set(1, duration(value))).unwrap();
        ws.apply(set(2, duration(value))).unwrap();
        ws.apply(set(3, load_time(value))).unwrap();
        assert_clean(&ws);
    }
}

#[test]
fn bad_durations_and_load_times_are_refused() {
    let mut ws = timed_cues();
    for bad in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        -0.001,
        -1.0,
        MAX_TIME + 0.001,
        1e300,
        f64::MAX,
    ] {
        for (n, property, field) in [
            (1, duration(bad), "duration"),
            (2, duration(bad), "duration"),
            (3, load_time(bad), "load_to_time"),
        ] {
            let error = refused(&mut ws, set(n, property));
            let CommandError::InvalidCue(issues) = error else {
                panic!("{bad}: {error:?}")
            };
            assert_eq!(
                issues.iter().map(|i| i.kind.clone()).collect::<Vec<_>>(),
                vec![IssueKind::InvalidTime { field }],
                "{bad}"
            );
        }
    }
}

#[test]
fn a_duration_or_load_time_on_a_cue_without_one_is_refused() {
    let mut ws = timed_cues();
    for n in [3, 4, 5, 6] {
        assert_eq!(
            refused(&mut ws, set(n, duration(1.0))),
            CommandError::PropertyNotApplicable(cue_id(n))
        );
    }
    for n in [1, 2, 4, 5, 6] {
        assert_eq!(
            refused(&mut ws, set(n, load_time(1.0))),
            CommandError::PropertyNotApplicable(cue_id(n))
        );
    }
    assert_eq!(
        refused(&mut ws, set(99, duration(1.0))),
        CommandError::CueNotFound(cue_id(99))
    );
}

#[test]
fn a_duration_is_batch_set_on_waits_and_fades_all_or_none() {
    let mut ws = timed_cues();
    round_trip(
        &mut ws,
        Command::BatchSet {
            ids: vec![cue_id(1), cue_id(2)],
            property: duration(3.0),
        },
    );
    assert_eq!(kind_of(&ws, 1), CueKind::Wait { duration: 3.0 });
    // A Memo in the batch refuses it all; the Wait written first gets its old value back.
    refused(
        &mut ws,
        Command::BatchSet {
            ids: vec![cue_id(1), cue_id(5)],
            property: duration(4.0),
        },
    );
}

#[test]
fn a_fade_whose_target_is_missing_can_still_have_its_duration_edited() {
    // References are not checked (broken-cue state), so a dangling target does not block it.
    let mut ws = workspace(vec![cue(2, fade(99))]);
    round_trip(&mut ws, set(2, duration(1.0)));
}

#[test]
fn coalesced_duration_edits_are_one_undo_step() {
    let mut ws = timed_cues();
    let before = ws.clone();
    let mut history = History::new();
    for value in [3.0, 4.0, 5.5] {
        history
            .execute_coalescing(&mut ws, set(1, duration(value)), Some("duration:1"))
            .unwrap();
    }
    assert_eq!(kind_of(&ws, 1), CueKind::Wait { duration: 5.5 });
    assert_eq!(history.undo_len(), 1);
    history.undo(&mut ws).unwrap();
    assert_eq!(ws, before);
    history.redo(&mut ws).unwrap();
    assert_eq!(kind_of(&ws, 1), CueKind::Wait { duration: 5.5 });
}

#[test]
fn a_kind_change_after_a_duration_edit_in_one_gesture_still_undoes() {
    // Wait -> Memo -> Wait (3 s), its duration to 5 s, then -> Memo, all with one coalesce key.
    // A Kind replaces the duration, so the last step's Kind may not be dropped in favour of the
    // older chain: that would apply the Duration step to a Memo, and the undo would be refused.
    let mut ws = timed_cues();
    let before = ws.clone();
    let mut history = History::new();
    for property in [
        CueProperty::Kind {
            value: CueKind::Memo,
        },
        CueProperty::Kind {
            value: CueKind::Wait { duration: 3.0 },
        },
        duration(5.0),
        CueProperty::Kind {
            value: CueKind::Memo,
        },
    ] {
        history
            .execute_coalescing(&mut ws, set(1, property), Some("k"))
            .unwrap();
    }
    assert_eq!(history.undo_len(), 1);
    history.undo(&mut ws).unwrap();
    assert_eq!(ws, before);
}

#[test]
fn a_kind_replacement_between_duration_edits_of_one_cue_undoes_to_the_start() {
    // A Kind replaces the duration too, so merging the steps must not drop one the others need.
    let mut ws = timed_cues();
    let before = ws.clone();
    let mut history = History::new();
    let mut kind = kind_of(&ws, 2);
    if let CueKind::Fade { curve, .. } = &mut kind {
        *curve = FadeCurve::Linear;
    }
    for property in [
        duration(9.0),
        CueProperty::Kind { value: kind },
        duration(1.0),
    ] {
        history
            .execute_coalescing(&mut ws, set(2, property), Some("k"))
            .unwrap();
    }
    history.undo(&mut ws).unwrap();
    assert_eq!(ws, before);
}
