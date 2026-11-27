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
            play_count: 1.into(),
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
fn a_target_cues_missing_or_unusable_new_target_is_a_warning_and_still_saves() {
    let retarget = |n: u128, new: u128| {
        cue(
            n,
            CueKind::Target {
                target: Some(cue_id(1)),
                new_target: Some(cue_id(new)),
            },
        )
    };
    let mut ws = workspace(vec![
        numbered(
            1,
            "1",
            CueKind::Devamp {
                target: None,
                start_next_cue: false,
                stop_target_when_slice_ends: false,
                devamp_mode: DevampMode::CurrentSlice,
            },
        ),
        numbered(2, "2", CueKind::Memo),
    ]);
    round_trip(&mut ws, add(pos(None, 2), retarget(10, 99)));
    round_trip(&mut ws, add(pos(None, 3), retarget(11, 2)));
    let issues = validate(&ws);
    let kinds: Vec<_> = issues.iter().map(|i| (i.cue_id, i.kind.clone())).collect();
    assert!(kinds.contains(&(
        Some(cue_id(10)),
        IssueKind::TargetMissing { target: cue_id(99) }
    )));
    // A Devamp cannot act on a Memo.
    assert!(kinds.contains(&(
        Some(cue_id(11)),
        IssueKind::TargetWrongKind { target: cue_id(2) }
    )));
    assert!(issues.iter().all(|i| i.severity == Severity::Warning));
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
    auto_at(n, 0)
}

fn auto_at(n: u128, index: usize) -> Command {
    Command::AddCue {
        at: pos(None, index),
        cue: memo(n),
        auto_number: true,
    }
}

/// A list with these numbers ("-" = none), in order; cue `i` (1-based) has the i-th number.
fn list_of(numbers: &[&str]) -> Workspace {
    workspace(
        numbers
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let id = i as u128 + 1;
                if *n == "-" {
                    memo(id)
                } else {
                    numbered(id, n, CueKind::Memo)
                }
            })
            .collect(),
    )
}

/// The number a new cue added after cue `after` (1-based; 0 = first in the list) gets, given a
/// list and increment; `None` when it gets none.
fn auto_number_after(numbers: &[&str], after: usize, increment: f64) -> Option<String> {
    let mut ws = list_of(numbers);
    ws.settings.auto_number_increment = increment;
    ws.apply(auto_at(99, after)).unwrap();
    number(&ws, 99)
}

/// Like `auto_number_after`, with the new cue after the cue numbered `selected`.
fn after_number(numbers: &[&str], selected: &str, increment: f64) -> Option<String> {
    let index = numbers.iter().position(|n| *n == selected).unwrap() + 1;
    auto_number_after(numbers, index, increment)
}

#[test]
fn auto_numbering_steps_by_the_workspace_increment() {
    let mut ws = workspace(vec![]);
    ws.settings.auto_number_increment = 5.0;
    for n in 1..=3 {
        ws.apply(auto_at(n, n as usize - 1)).unwrap();
    }
    assert_eq!(number(&ws, 1).as_deref(), Some("5"));
    assert_eq!(number(&ws, 2).as_deref(), Some("10"));
    assert_eq!(number(&ws, 3).as_deref(), Some("15"));
    assert_clean(&ws);
}

#[test]
fn auto_numbering_has_no_float_noise() {
    let mut ws = workspace(vec![]);
    ws.settings.auto_number_increment = 0.1;
    for n in 1..=4 {
        ws.apply(auto_at(n, n as usize - 1)).unwrap();
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

/// QLab 5, manual p. 100: with an increment of 1 and cues 2, 4 and 6, a cue added after 6 is 7,
/// one between 2 and 4 is 3, and one then added between 2 and 3 is 2.5.
#[test]
fn auto_number_new_cue_between_neighbours_takes_the_gap() {
    let mut ws = list_of(&["2", "4", "6"]);
    ws.apply(auto_at(10, 3)).unwrap();
    assert_eq!(number(&ws, 10).as_deref(), Some("7"));
    ws.apply(auto_at(11, 1)).unwrap();
    assert_eq!(number(&ws, 11).as_deref(), Some("3"));
    ws.apply(auto_at(12, 1)).unwrap();
    assert_eq!(number(&ws, 12).as_deref(), Some("2.5"));
    assert_clean(&ws);
}

// The cases below were run in QLab 5 (5.6.5) on 2026-11-27 (issue "Owner's notes 5").

/// N1 / N2a: the owner's list, 0.05 selected, default increment.
#[test]
fn n1_audio_cue_after_0_05_in_the_owners_list_gets_0_07() {
    let list = [
        "0.05", "0.1", "0.06", "0.2", "0.5", "1", "2", "0.6", "3.5", "3", "4", "5", "5.5",
    ];
    assert_eq!(after_number(&list, "0.05", 1.0).as_deref(), Some("0.07"));
}

#[test]
fn n2b_integer_before_with_room_adds_one() {
    assert_eq!(
        after_number(&["1", "2", "4"], "2", 1.0).as_deref(),
        Some("3")
    );
}

#[test]
fn n2c_integer_before_and_next_integer_after_takes_half() {
    assert_eq!(
        after_number(&["2", "3", "4", "5"], "2", 1.0).as_deref(),
        Some("2.5")
    );
}

#[test]
fn n2d_a_number_used_further_down_is_skipped() {
    assert_eq!(
        after_number(&["2", "5", "3"], "2", 1.0).as_deref(),
        Some("4")
    );
}

#[test]
fn n2e_decimal_before_steps_by_the_largest_step_that_fits() {
    assert_eq!(
        after_number(&["2.5", "5"], "2.5", 1.0).as_deref(),
        Some("3.5")
    );
    assert_eq!(
        after_number(&["2.5", "3"], "2.5", 1.0).as_deref(),
        Some("2.6")
    );
    assert_eq!(
        after_number(&["2.5", "2.6"], "2.5", 1.0).as_deref(),
        Some("2.55")
    );
}

#[test]
fn n2f_used_numbers_past_the_next_cue_are_stepped_over() {
    assert_eq!(
        after_number(&["2.5", "2.7", "2.6"], "2.5", 1.0).as_deref(),
        Some("2.8")
    );
    assert_eq!(
        after_number(&["2.5", "2.7", "2.6", "2.8", "3"], "2.5", 1.0).as_deref(),
        Some("2.9")
    );
}

#[test]
fn n2g_finer_steps_for_decimals_with_little_room() {
    assert_eq!(
        after_number(&["2", "2.1", "2.05"], "2", 1.0).as_deref(),
        Some("2.15")
    );
    assert_eq!(
        after_number(&["2.05", "3"], "2.05", 1.0).as_deref(),
        Some("2.55")
    );
    assert_eq!(
        after_number(&["1.25", "2"], "1.25", 1.0).as_deref(),
        Some("1.75")
    );
}

#[test]
fn n2h_a_lower_number_after_gives_no_number() {
    assert_eq!(after_number(&["5", "1"], "5", 1.0), None);
    assert_eq!(after_number(&["7", "6", "8"], "7", 1.0), None);
}

#[test]
fn n2i_no_numbered_cue_before_counts_from_zero() {
    assert_eq!(auto_number_after(&["-", "5"], 1, 1.0).as_deref(), Some("1"));
    assert_eq!(
        auto_number_after(&["-", "1"], 1, 1.0).as_deref(),
        Some("0.5")
    );
    assert_eq!(auto_number_after(&[], 0, 1.0).as_deref(), Some("1"));
}

#[test]
fn n2j_an_unnumbered_cue_before_looks_back_to_the_last_number() {
    assert_eq!(
        auto_number_after(&["3", "-", "5"], 2, 1.0).as_deref(),
        Some("4")
    );
    assert_eq!(auto_number_after(&["3", "-", "-", "2"], 2, 1.0), None);
}

#[test]
fn n2k_non_numeric_numbers_are_skipped() {
    assert_eq!(
        auto_number_after(&["4", "A", "9"], 2, 1.0).as_deref(),
        Some("5")
    );
    assert_eq!(
        auto_number_after(&["A", "1"], 1, 1.0).as_deref(),
        Some("0.5")
    );
    assert_eq!(
        auto_number_after(&["1", "A", "2"], 1, 1.0).as_deref(),
        Some("1.5")
    );
}

#[test]
fn n2l_increment_5() {
    let run = |list: &[&str], sel: &str| after_number(list, sel, 5.0);
    assert_eq!(run(&["10", "20"], "10").as_deref(), Some("15"));
    assert_eq!(run(&["10", "16"], "10").as_deref(), Some("15"));
    assert_eq!(run(&["10", "15"], "10").as_deref(), Some("11"));
    assert_eq!(run(&["2", "7.5"], "2").as_deref(), Some("7"));
    assert_eq!(run(&["2", "7"], "2").as_deref(), Some("3"));
    assert_eq!(run(&["2", "3"], "2").as_deref(), Some("2.5"));
    assert_eq!(auto_number_after(&[], 0, 5.0).as_deref(), Some("5"));
}

#[test]
fn n2m_increment_10() {
    let run = |list: &[&str]| after_number(list, "2", 10.0);
    assert_eq!(run(&["2", "20"]).as_deref(), Some("12"));
    assert_eq!(run(&["2", "8"]).as_deref(), Some("7"));
    assert_eq!(run(&["2", "6"]).as_deref(), Some("3"));
}

#[test]
fn n2n_increment_0_25() {
    let run = |next: &str| after_number(&["2", next], "2", 0.25);
    assert_eq!(run("3").as_deref(), Some("2.25"));
    assert_eq!(run("2.2").as_deref(), Some("2.1"));
    assert_eq!(run("2.1").as_deref(), Some("2.05"));
    assert_eq!(run("2.05").as_deref(), Some("2.01"));
    assert_eq!(auto_number_after(&[], 0, 0.25).as_deref(), Some("0.25"));
}

#[test]
fn n2o_increment_0_1() {
    assert_eq!(after_number(&["1", "5"], "1", 0.1).as_deref(), Some("1.1"));
    assert_eq!(
        after_number(&["1", "1.1"], "1", 0.1).as_deref(),
        Some("1.05")
    );
}

#[test]
fn n2p_three_decimals_are_the_limit() {
    assert_eq!(
        after_number(&["1", "1.0011"], "1", 1.0).as_deref(),
        Some("1.001")
    );
    assert_eq!(after_number(&["1", "1.001"], "1", 1.0), None);
    assert_eq!(after_number(&["1", "1.0001"], "1", 1.0), None);
}

/// Numbers beyond the limit auto-numbering reads, and huge lists of used numbers, never hang or
/// panic: the result is a number or none.
#[test]
fn auto_number_with_huge_neighbours_still_answers() {
    assert!(after_number(&["1", "1e300"], "1", 1.0).is_some());
    assert!(after_number(&["1e300"], "1e300", 1.0).is_some());
    assert!(after_number(&["1"], "1", 1e-9).is_some());
}

/// With auto-numbering off a cue made with `auto_number` keeps the number it has (none for a
/// new cue); paste and the clipboard never number.
#[test]
fn auto_number_off_leaves_a_new_cue_without_a_number_and_a_paste_is_never_numbered() {
    let mut ws = workspace(vec![numbered(1, "3", CueKind::Memo)]);
    ws.settings.auto_number_new_cues = false;
    ws.apply(Command::AddCue {
        at: pos(None, 1),
        cue: memo(2),
        auto_number: true,
    })
    .unwrap();
    assert_eq!(number(&ws, 2), None);
    ws.settings.auto_number_new_cues = true;
    ws.apply(Command::Paste {
        at: pos(None, 2),
        cues: vec![memo(3)],
    })
    .unwrap();
    assert_eq!(number(&ws, 3), None, "a paste is not auto-numbered");
}

/// The workspace switches that only the app and OSC read are plain settings: set, undone and
/// kept.
#[test]
fn the_general_switches_are_workspace_settings_with_undo() {
    let mut ws = workspace(vec![]);
    let before = ws.clone();
    let mut history = History::new();
    for setting in [
        WorkspaceSetting::AutoNumberNewCues { value: false },
        WorkspaceSetting::LockPlayhead { value: false },
        WorkspaceSetting::GoNeedsKeyUp { value: true },
        WorkspaceSetting::OscEnabled { value: false },
        WorkspaceSetting::CopyMediaOnAdd { value: false },
    ] {
        history
            .execute(&mut ws, Command::SetWorkspaceSetting { setting })
            .unwrap();
    }
    assert!(!ws.settings.auto_number_new_cues && !ws.settings.lock_playhead);
    assert!(ws.settings.go_needs_key_up && !ws.settings.osc_enabled);
    assert!(!ws.settings.copy_media_on_add);
    while history.undo(&mut ws).unwrap() {}
    assert_eq!(ws, before);
}

/// Workspace Settings, General, "new cues auto-load" (QLab 5, manual p. 128): a cue made
/// afterwards has auto-load set (a group's children with it); cues already there, a pasted copy
/// and an undo keep what they had.
#[test]
fn a_cue_made_with_new_cues_auto_load_on_auto_loads_and_existing_cues_are_unchanged() {
    let mut ws = workspace(vec![memo(1)]);
    let mut history = History::new();
    history
        .execute(
            &mut ws,
            Command::SetWorkspaceSetting {
                setting: WorkspaceSetting::NewCuesAutoLoad { value: true },
            },
        )
        .unwrap();
    assert!(ws.settings.new_cues_auto_load);
    assert!(
        !ws.find_cue(cue_id(1)).unwrap().auto_load,
        "existing cue unchanged"
    );
    history
        .execute(&mut ws, add(pos(None, 1), grp(2, vec![memo(3)])))
        .unwrap();
    assert!(ws.find_cue(cue_id(2)).unwrap().auto_load);
    assert!(ws.find_cue(cue_id(3)).unwrap().auto_load);
    // A paste is a copy: it keeps the copied cue's value.
    history
        .execute(
            &mut ws,
            Command::Paste {
                at: pos(None, 2),
                cues: vec![memo(4)],
            },
        )
        .unwrap();
    assert!(!ws.find_cue(cue_id(4)).unwrap().auto_load);
    // Undo and redo put back what was there.
    let after = ws.clone();
    assert!(history.undo(&mut ws).unwrap());
    assert!(history.undo(&mut ws).unwrap());
    assert!(ws.find_cue(cue_id(2)).is_none());
    assert!(history.redo(&mut ws).unwrap());
    assert!(history.redo(&mut ws).unwrap());
    assert_eq!(ws, after);
    // Off again: a new cue comes as the client sent it.
    history
        .execute(
            &mut ws,
            Command::SetWorkspaceSetting {
                setting: WorkspaceSetting::NewCuesAutoLoad { value: false },
            },
        )
        .unwrap();
    history
        .execute(&mut ws, add(pos(None, 3), memo(5)))
        .unwrap();
    assert!(!ws.find_cue(cue_id(5)).unwrap().auto_load);
}

#[test]
fn auto_numbering_with_invalid_settings_is_refused() {
    for increment in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let mut ws = workspace(vec![]);
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

// ---- new group from the selected cues (ED-03) -------------------------------------------------

/// What the app sends for a new Group with `cues` selected (list order): group 9 goes at `at` in
/// `parent`, then each cue moves into it in order, as one transaction.
fn wrap(parent: Option<u128>, at: usize, cues: &[u128]) -> Command {
    let mut commands = vec![add(pos(parent, at), grp(9, vec![]))];
    commands.extend(
        cues.iter()
            .enumerate()
            .map(|(i, n)| reparent(*n, pos(Some(9), i))),
    );
    Command::Transaction { commands }
}

#[test]
fn a_new_group_takes_in_contiguous_selected_cues_at_the_first_ones_place() {
    let mut ws = workspace(vec![memo(1), memo(2), memo(3), memo(4)]);
    let before = ws.clone();
    let mut history = History::new();
    history.execute(&mut ws, wrap(None, 1, &[2, 3])).unwrap();
    assert_eq!(top(&ws), "1 9[2 3] 4");
    assert_clean(&ws);
    // One undo step puts everything back exactly; redo makes the group again.
    history.undo(&mut ws).unwrap();
    assert_eq!(ws, before);
    assert_eq!(bytes(&ws), bytes(&before));
    history.redo(&mut ws).unwrap();
    assert_eq!(top(&ws), "1 9[2 3] 4");
}

#[test]
fn a_new_group_takes_in_scattered_cues_from_different_parents() {
    // 1, 2[3, 4[5]], 6: 3 and 6 are selected; the group goes where 3 is, inside 2.
    let mut ws = sample();
    round_trip(&mut ws, wrap(Some(2), 0, &[3, 6]));
    assert_eq!(top(&ws), "1 2[9[3 6] 4[5]]");
    // A selected group brings its children: 1 and group 4 (5 with it).
    let mut ws = sample();
    round_trip(&mut ws, wrap(None, 0, &[1, 4]));
    assert_eq!(top(&ws), "9[1 4[5]] 2[3] 6");
    assert_clean(&ws);
}

#[test]
fn a_new_group_that_cannot_take_a_cue_in_is_refused_whole() {
    // Wrapping the deepest allowed chain would nest one level too deep: nothing changes.
    let mut ws = workspace(vec![memo(1), chain(MAX_GROUP_DEPTH)]);
    assert_clean(&ws);
    let error = refused(&mut ws, wrap(None, 0, &[1, 101]));
    assert_eq!(
        error,
        CommandError::InTransaction {
            index: 2,
            source: Box::new(CommandError::NestingTooDeep)
        }
    );
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
        CueProperty::SecondColor {
            value: CueColor::Raspberry,
        },
        CueProperty::Flagged { value: true },
        CueProperty::Armed { value: false },
        CueProperty::AutoLoad { value: true },
        CueProperty::FadeAndStopOthers {
            value: FadeAndStopOthers {
                enabled: true,
                scope: OthersScope::All,
                duration: 3.0,
            },
        },
        CueProperty::DuckOthers {
            value: DuckOthers {
                enabled: true,
                db: 6.0,
                duration: 0.5,
            },
        },
        CueProperty::SkipIfDisarmed { value: true },
        CueProperty::PreWait { value: 1.5 },
        CueProperty::PostWait { value: 2.5 },
        CueProperty::ContinueMode {
            value: ContinueMode::AutoFollow,
        },
        CueProperty::Triggers {
            value: Triggers {
                hotkey: Some(hotkey),
                hotkey_enabled: false,
                wall_clock: Default::default(),
            },
        },
        CueProperty::Triggers {
            value: Triggers {
                hotkey: None,
                hotkey_enabled: true,
                wall_clock: WallClockTrigger {
                    enabled: true,
                    hours: 23,
                    minutes: 59,
                    seconds: 59,
                    days: Weekdays::NONE.with(Weekday::Saturday, true),
                    twelve_hour: true,
                },
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

fn fields(json: serde_json::Value) -> CueProperty {
    let serde_json::Value::Object(value) = json else {
        panic!("not an object");
    };
    CueProperty::KindFields { value }
}

#[test]
fn kind_fields_keep_the_kind_fields_not_named() {
    let mut ws = workspace(vec![cue(1, audio())]);
    // Another client moves the trim; a sender that read the kind before then names only its own
    // field and must not put the old trim back.
    ws.apply(set(
        1,
        fields(serde_json::json!({ "start_time": 0.5, "end_time": 2.0 })),
    ))
    .unwrap();
    let before = ws.clone();
    let inverse = ws
        .apply(set(
            1,
            fields(serde_json::json!({ "last_slice_play_count": 4 })),
        ))
        .unwrap();
    match kind_of(&ws, 1) {
        CueKind::Audio {
            start_time,
            end_time,
            last_slice_play_count,
            ..
        } => {
            assert_eq!((start_time, end_time), (0.5, Some(2.0)));
            assert_eq!(last_slice_play_count, 4.into());
        }
        other => panic!("{other:?}"),
    }
    ws.apply(inverse).unwrap();
    assert_eq!(ws, before, "undo restores the kind");
    assert_clean(&ws);
}

#[test]
fn a_target_cues_new_target_is_set_and_cleared_by_its_kind_field_and_undone() {
    let mut ws = workspace(vec![
        cue(
            1,
            CueKind::Target {
                target: None,
                new_target: None,
            },
        ),
        numbered(2, "2", CueKind::Memo),
    ]);
    let before = ws.clone();
    let inverse = ws
        .apply(set(
            1,
            fields(serde_json::json!({ "new_target": cue_id(2) })),
        ))
        .unwrap();
    assert_eq!(kind_of(&ws, 1).new_target(), Some(cue_id(2)));
    ws.apply(set(1, fields(serde_json::json!({ "new_target": null }))))
        .unwrap();
    assert_eq!(kind_of(&ws, 1).new_target(), None);
    ws.apply(set(
        1,
        fields(serde_json::json!({ "new_target": cue_id(2) })),
    ))
    .unwrap();
    ws.apply(inverse).unwrap();
    assert_eq!(ws, before, "undo restores it");
    // Not a field of another kind.
    assert!(
        ws.apply(set(
            2,
            fields(serde_json::json!({ "new_target": cue_id(1) }))
        ))
        .is_err()
    );
}

/// A play count set by an edit of the kind's fields (what the app's count field sends) is a
/// count, until devamped or skipped; a value that is none of them is refused and the cue keeps
/// what it had. Every slice skipped is accepted: the cue is then broken, not the edit.
#[test]
fn kind_fields_set_a_play_count_to_a_number_endless_or_skip_and_refuse_the_rest() {
    let mut ws = workspace(vec![cue(1, audio())]);
    ws.apply(set(1, fields(serde_json::json!({ "end_time": 2.0 }))))
        .unwrap();
    let counts = |ws: &Workspace| match kind_of(ws, 1) {
        CueKind::Audio {
            slices,
            last_slice_play_count,
            ..
        } => (
            slices.iter().map(|m| m.play_count).collect::<Vec<_>>(),
            last_slice_play_count,
        ),
        other => panic!("{other:?}"),
    };
    for (stored, value) in [
        (serde_json::json!(3), PlayCount::from_stored(3)),
        (serde_json::json!(0), PlayCount::Endless),
        (serde_json::json!("skip"), PlayCount::Skip),
    ] {
        let before = ws.clone();
        let inverse = ws
            .apply(set(
                1,
                fields(serde_json::json!({
                    "slices": [{ "time": 1.0, "play_count": stored }],
                    "last_slice_play_count": stored,
                })),
            ))
            .unwrap();
        assert_eq!(counts(&ws), (vec![value], value), "{stored}");
        assert_clean(&ws);
        ws.apply(inverse).unwrap();
        assert_eq!(ws, before, "{stored}: undo");
    }
    let before = ws.clone();
    for bad in [
        serde_json::json!(-1),
        serde_json::json!(1.5),
        serde_json::json!(4_294_967_296u64),
        serde_json::json!("loop"),
        serde_json::Value::Null,
    ] {
        for edit in [
            serde_json::json!({ "last_slice_play_count": bad }),
            serde_json::json!({ "slices": [{ "time": 1.0, "play_count": bad }] }),
        ] {
            assert!(
                matches!(
                    refused(&mut ws, set(1, fields(edit.clone()))),
                    CommandError::InvalidValue(_)
                ),
                "{edit}"
            );
            assert_eq!(ws, before, "{edit}");
        }
    }
}

#[test]
fn kind_fields_refuse_bad_fields_groups_and_batches() {
    let mut ws = workspace(vec![cue(1, audio()), grp(2, vec![]), cue(3, fade(1))]);
    for bad in [
        serde_json::json!({ "type": "memo" }),
        serde_json::json!({ "media": "x.wav" }),
        serde_json::json!({ "children": [] }),
        serde_json::json!({ "last_slice_play_count": "many" }),
    ] {
        assert!(
            matches!(
                refused(&mut ws, set(1, fields(bad.clone()))),
                CommandError::InvalidValue(_)
            ),
            "{bad}"
        );
    }
    assert_eq!(
        refused(
            &mut ws,
            set(1, fields(serde_json::json!({ "curve": "linear" })))
        ),
        CommandError::PropertyNotApplicable(cue_id(1))
    );
    assert_eq!(
        refused(
            &mut ws,
            set(2, fields(serde_json::json!({ "mode": "timeline" })))
        ),
        CommandError::KindChangeInvolvesGroup(cue_id(2))
    );
    // The merged kind is judged like a whole kind.
    assert!(matches!(
        refused(
            &mut ws,
            set(3, fields(serde_json::json!({ "duration": -1.0 })))
        ),
        CommandError::InvalidCue(_)
    ));
    assert_eq!(
        refused(
            &mut ws,
            Command::BatchSet {
                ids: vec![cue_id(1)],
                property: fields(serde_json::json!({ "preload": true })),
            }
        ),
        CommandError::NotBatchable
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
                        hotkey_enabled: true,
                        wall_clock: Default::default(),
                    },
                }
            )
        ),
        CommandError::InvalidCue(_)
    ));
    // A wall-clock time past 23:59:59, or no day to fire on, even with the trigger off.
    for (enabled, hours, minutes, seconds, days) in [
        (true, 24, 0, 0, Weekdays::EVERY_DAY),
        (false, 0, 60, 0, Weekdays::EVERY_DAY),
        (true, 0, 0, 60, Weekdays::EVERY_DAY),
        (true, 12, 0, 0, Weekdays::NONE),
    ] {
        let wall_clock = WallClockTrigger {
            enabled,
            hours,
            minutes,
            seconds,
            days,
            twelve_hour: false,
        };
        let before = ws.clone();
        assert!(matches!(
            refused(
                &mut ws,
                set(
                    2,
                    CueProperty::Triggers {
                        value: Triggers {
                            hotkey: None,
                            hotkey_enabled: false,
                            wall_clock,
                        },
                    }
                )
            ),
            CommandError::InvalidCue(_)
        ));
        assert_eq!(ws, before);
    }
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
        prefix: String::new(),
        suffix: String::new(),
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

/// Flow: a number a cue outside the selection already has is skipped, like a prefix and a suffix
/// around the number (QLab 5 manual p. 150: cue 11 elsewhere gives 10, 12, 13).
#[test]
fn renumber_skips_numbers_used_by_cues_outside_the_selection() {
    let mut ws = workspace(vec![
        memo(1),
        memo(2),
        memo(3),
        numbered(4, "11", CueKind::Memo),
    ]);
    round_trip(&mut ws, renumber(&[1, 2, 3], 10.0, 1.0));
    let numbers: Vec<_> = [1, 2, 3, 4].map(|n| number(&ws, n).unwrap()).to_vec();
    assert_eq!(numbers, ["10", "12", "13", "11"]);
    // The selection's own old numbers are free again.
    let mut ws = workspace(vec![numbered(1, "2", CueKind::Memo), memo(2)]);
    round_trip(&mut ws, renumber(&[1, 2], 1.0, 1.0));
    assert_eq!(number(&ws, 1).unwrap(), "1");
    assert_eq!(number(&ws, 2).unwrap(), "2");
}

#[test]
fn renumber_puts_the_prefix_and_suffix_around_each_number_and_skips_by_full_text() {
    let mut ws = workspace(vec![
        memo(1),
        memo(2),
        memo(3),
        numbered(4, "Test-11bak", CueKind::Memo),
        numbered(5, "12", CueKind::Memo),
    ]);
    let command = Command::Renumber {
        ids: vec![cue_id(1), cue_id(2), cue_id(3)],
        start: 10.0,
        increment: 1.0,
        prefix: "Test-".into(),
        suffix: "bak".into(),
    };
    round_trip(&mut ws, command);
    let numbers: Vec<_> = [1, 2, 3].map(|n| number(&ws, n).unwrap()).to_vec();
    assert_eq!(numbers, ["Test-10bak", "Test-12bak", "Test-13bak"]);
    assert_eq!(number(&ws, 5).unwrap(), "12");
    assert_clean(&ws);
    // Too long an affix is refused on this path.
    let long = Command::Renumber {
        ids: vec![cue_id(1)],
        start: 1.0,
        increment: 1.0,
        prefix: "x".repeat(65),
        suffix: String::new(),
    };
    assert!(matches!(
        refused(&mut ws, long),
        CommandError::InvalidNumbering(_)
    ));
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
        CommandError::InvalidNumbering(_)
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
fn paste_points_a_target_cues_new_target_at_the_copy_or_keeps_or_clears_it() {
    let ws = workspace(vec![
        numbered(1, "1", CueKind::Start { target: None }),
        numbered(2, "2", CueKind::Memo),
    ]);
    let retarget = |new: u128| {
        cue(
            10,
            CueKind::Target {
                target: Some(cue_id(1)),
                new_target: Some(cue_id(new)),
            },
        )
    };
    // Copied with it: the copy; in the workspace: kept; neither: cleared.
    let copy = prepare_paste(
        &ws,
        &[retarget(11), numbered(11, "9", CueKind::Memo)],
        fresh_ids(1000),
    )
    .unwrap();
    assert_eq!(copy[0].kind.new_target(), Some(cue_id(1002)));
    let copy = prepare_paste(&ws, &[retarget(2)], fresh_ids(2000)).unwrap();
    assert_eq!(copy[0].kind.new_target(), Some(cue_id(2)));
    let copy = prepare_paste(&ws, &[retarget(99)], fresh_ids(3000)).unwrap();
    assert_eq!(copy[0].kind.new_target(), None);
    assert_eq!(copy[0].kind.target(), Some(cue_id(1)));
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
                prefix: String::new(),
                suffix: String::new(),
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
            prefix: String::new(),
            suffix: String::new(),
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

// ---- durations and load time (CUE-02, CT-14, CT-20) ----------------------------------

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

/// A Wait cue's post-wait is its duration (QLab 5, manual p. 688): setting it sets the
/// duration, on its own or in a batch with other kinds, and undo puts the duration back.
#[test]
fn a_wait_cues_post_wait_sets_its_duration() {
    let mut ws = timed_cues();
    let post = |value| CueProperty::PostWait { value };
    round_trip(&mut ws, set(1, post(4.0)));
    assert_eq!(kind_of(&ws, 1), CueKind::Wait { duration: 4.0 });
    assert_eq!(ws.find_cue(cue_id(1)).unwrap().post_wait, 0.0);
    // The duration it already has is no edit.
    let inverse = ws.apply(set(1, post(4.0))).unwrap();
    assert_eq!(inverse, Command::Transaction { commands: vec![] });
    round_trip(
        &mut ws,
        Command::BatchSet {
            ids: vec![cue_id(1), cue_id(5)],
            property: post(1.5),
        },
    );
    assert_eq!(kind_of(&ws, 1), CueKind::Wait { duration: 1.5 });
    assert_eq!(ws.find_cue(cue_id(5)).unwrap().post_wait, 1.5);
    // Out of range is refused like any post-wait.
    refused(&mut ws, set(1, post(-1.0)));
    assert_eq!(kind_of(&ws, 1), CueKind::Wait { duration: 1.5 });
    assert_clean(&ws);
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
    // A load time counts back from the target's end when negative (CT-14).
    for value in [-0.001, -MAX_TIME] {
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
        -MAX_TIME - 0.001,
        1e300,
        f64::MAX,
    ] {
        for (n, property, field) in [
            (1, duration(bad), "duration"),
            (2, duration(bad), "duration"),
            (3, load_time(bad), "load_to_time"),
        ] {
            // A negative load time within the limit is valid (counted back from the end).
            if n == 3 && (-MAX_TIME..0.0).contains(&bad) {
                continue;
            }
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
        curve.audio_domain = AudioDomain::Linear;
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

/// "Fade and stop others" holds a time like a pre-wait: every path that sets it keeps it finite,
/// 0 to 24 h. A command and a paste refuse a bad one; a file has it put in range with a warning.
#[test]
fn a_bad_fade_and_stop_others_time_is_refused_by_a_command_and_a_paste_and_repaired_on_load() {
    let field = "fade_and_stop_others.duration";
    let with = |duration: f64| FadeAndStopOthers {
        enabled: true,
        scope: OthersScope::List,
        duration,
    };
    let mut ws = workspace(vec![memo(1)]);
    for bad in [f64::NAN, f64::INFINITY, -0.5, MAX_TIME + 1.0] {
        let error = refused(
            &mut ws,
            set(1, CueProperty::FadeAndStopOthers { value: with(bad) }),
        );
        let CommandError::InvalidCue(issues) = error else {
            panic!("{bad}: {error:?}")
        };
        assert_eq!(
            issues.iter().map(|i| i.kind.clone()).collect::<Vec<_>>(),
            vec![IssueKind::InvalidTime { field }],
            "{bad}"
        );
        let mut pasted = memo(2);
        pasted.fade_and_stop_others = with(bad);
        assert!(matches!(
            refused(
                &mut ws,
                Command::Paste {
                    at: pos(None, 1),
                    cues: vec![pasted],
                }
            ),
            CommandError::InvalidCue(_)
        ));
    }
    round_trip(
        &mut ws,
        set(1, CueProperty::FadeAndStopOthers { value: with(2.5) }),
    );
    // A show file: the time is put in range (NaN to 0) and the cue named.
    let mut bad = memo(1);
    bad.fade_and_stop_others = with(-3.0);
    let loaded = load_show(&show_text(&workspace(vec![bad]))).unwrap();
    assert_eq!(
        loaded.workspace.cue_lists[0].cues[0]
            .fade_and_stop_others
            .duration,
        0.0
    );
    assert!(
        loaded
            .warnings
            .iter()
            .any(|w| w.cue_id == Some(cue_id(1)) && w.kind == IssueKind::InvalidTime { field })
    );
}

/// "Duck or boost others": dB from -120 to +12 and a time like a pre-wait, on every path that sets
/// them. A command and a paste refuse a bad one; a file has it put in range with a warning.
#[test]
fn a_bad_duck_level_or_time_is_refused_by_a_command_and_a_paste_and_repaired_on_load() {
    let with = |db: f64, duration: f64| DuckOthers {
        enabled: true,
        db,
        duration,
    };
    let mut ws = workspace(vec![memo(1)]);
    for (bad, field) in [
        (
            with(f64::NAN, 1.0),
            IssueKind::InvalidLevel {
                field: "duck_others.db",
            },
        ),
        (
            with(12.5, 1.0),
            IssueKind::InvalidLevel {
                field: "duck_others.db",
            },
        ),
        (
            with(-130.0, 1.0),
            IssueKind::InvalidLevel {
                field: "duck_others.db",
            },
        ),
        (
            with(-6.0, -1.0),
            IssueKind::InvalidTime {
                field: "duck_others.duration",
            },
        ),
        (
            with(-6.0, f64::INFINITY),
            IssueKind::InvalidTime {
                field: "duck_others.duration",
            },
        ),
    ] {
        let error = refused(&mut ws, set(1, CueProperty::DuckOthers { value: bad }));
        let CommandError::InvalidCue(issues) = error else {
            panic!("{bad:?}: {error:?}")
        };
        assert_eq!(
            issues.iter().map(|i| i.kind.clone()).collect::<Vec<_>>(),
            vec![field],
            "{bad:?}"
        );
        let mut pasted = memo(2);
        pasted.duck_others = bad;
        assert!(matches!(
            refused(
                &mut ws,
                Command::Paste {
                    at: pos(None, 1),
                    cues: vec![pasted],
                }
            ),
            CommandError::InvalidCue(_)
        ));
    }
    round_trip(
        &mut ws,
        set(
            1,
            CueProperty::DuckOthers {
                value: with(12.0, 0.0),
            },
        ),
    );
    round_trip(
        &mut ws,
        set(
            1,
            CueProperty::DuckOthers {
                value: with(-120.0, 2.0),
            },
        ),
    );
    let mut bad = memo(1);
    bad.duck_others = with(40.0, 1.0);
    let loaded = load_show(&show_text(&workspace(vec![bad]))).unwrap();
    assert_eq!(loaded.workspace.cue_lists[0].cues[0].duck_others.db, 12.0);
    assert!(loaded.warnings.iter().any(|w| w.cue_id == Some(cue_id(1))
        && w.kind
            == IssueKind::InvalidLevel {
                field: "duck_others.db"
            }));
}

#[test]
fn a_fade_curve_it_may_not_hold_is_refused_by_an_edit_an_add_and_a_paste() {
    let mut ws = workspace(vec![sound(1), cue(2, fade(1))]);
    let mut bad = fade(1);
    if let CueKind::Fade { curve, .. } = &mut bad {
        curve.falling.points = vec![CurvePoint { t: 0.5, v: -0.1 }];
    }
    let CueKind::Fade { curve, .. } = &bad else {
        panic!("not a fade");
    };
    let mut value = serde_json::Map::new();
    value.insert("curve".into(), serde_json::to_value(curve).unwrap());
    refused(&mut ws, set(2, CueProperty::KindFields { value }));
    refused(&mut ws, set(2, CueProperty::Kind { value: bad.clone() }));
    refused(&mut ws, add(pos(None, 2), cue(3, bad.clone())));
    refused(
        &mut ws,
        Command::Paste {
            at: pos(None, 2),
            cues: vec![cue(3, bad)],
        },
    );
}
