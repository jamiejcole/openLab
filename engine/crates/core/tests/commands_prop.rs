// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Property tests for editing commands: random command sequences against random workspaces.
//! Every command round-trips through undo and redo, a refused command changes nothing, and
//! nothing a command does can make `validate` report a value problem or `save_show` refuse.

mod common;

use common::*;
use openlab_core::*;
use proptest::prelude::*;

/// Random numbers for one command; [`build`] turns them into a command that fits the workspace
/// as it is. Some come out invalid on purpose.
#[derive(Clone, Debug)]
struct Pick {
    op: u8,
    a: u32,
    b: u32,
    c: u32,
    d: u32,
}

fn pick() -> impl Strategy<Value = Pick> {
    (
        0u8..11,
        any::<u32>(),
        any::<u32>(),
        any::<u32>(),
        any::<u32>(),
    )
        .prop_map(|(op, a, b, c, d)| Pick { op, a, b, c, d })
}

fn picks(max: usize) -> impl Strategy<Value = Vec<Pick>> {
    proptest::collection::vec(pick(), 0..max)
}

struct Gen {
    next: u128,
}

impl Gen {
    fn fresh(&mut self) -> CueId {
        self.next += 1;
        cue_id(self.next)
    }
}

fn choose<T: Copy>(items: &[T], n: u32) -> Option<T> {
    items.get(n as usize % items.len().max(1)).copied()
}

fn all_ids(ws: &Workspace) -> Vec<CueId> {
    fn walk(cues: &[Cue], out: &mut Vec<CueId>) {
        for cue in cues {
            out.push(cue.id);
            walk(cue.children(), out);
        }
    }
    let mut out = Vec::new();
    walk(&ws.cue_lists[0].cues, &mut out);
    out
}

fn group_ids(ws: &Workspace) -> Vec<CueId> {
    all_ids(ws)
        .into_iter()
        .filter(|&id| matches!(ws.find_cue(id).unwrap().kind, CueKind::Group { .. }))
        .collect()
}

fn children_len(ws: &Workspace, parent: Option<CueId>) -> usize {
    match parent {
        None => ws.cue_lists[0].cues.len(),
        Some(id) => ws.find_cue(id).map_or(0, |c| c.children().len()),
    }
}

/// The group (or `None` for the list) holding `id`, and how many cues it holds.
fn container_of(ws: &Workspace, id: CueId) -> Option<(Option<CueId>, usize)> {
    fn search(cues: &[Cue], parent: Option<CueId>, id: CueId) -> Option<(Option<CueId>, usize)> {
        if cues.iter().any(|c| c.id == id) {
            return Some((parent, cues.len()));
        }
        cues.iter()
            .find_map(|c| search(c.children(), Some(c.id), id))
    }
    search(&ws.cue_lists[0].cues, None, id)
}

/// A position that is sometimes one or two past the end, and so refused.
fn position(ws: &Workspace, a: u32, b: u32) -> Position {
    let mut parents = vec![None];
    parents.extend(group_ids(ws).into_iter().map(Some));
    let parent = choose(&parents, a).flatten();
    Position {
        list: list_id(1),
        parent,
        index: b as usize % (children_len(ws, parent) + 2),
    }
}

/// Mostly valid; NaN, infinity, negatives and too-large values now and then.
fn time(n: u32) -> f64 {
    match n % 14 {
        0 => f64::NAN,
        1 => f64::INFINITY,
        2 => -1.0,
        3 => 1e9,
        k => [0.0, 0.5, 1.0, 2.5, 30.0, 100.0][k as usize % 6],
    }
}

fn level(n: u32) -> f64 {
    match n % 14 {
        0 => f64::NAN,
        1 => 20.0,
        2 => f64::NEG_INFINITY,
        3 => -200.0,
        k => [0.0, -6.0, -12.0, -120.0, 6.0, 12.0][k as usize % 6],
    }
}

fn number(n: u32) -> Option<String> {
    match n % 12 {
        0..=4 => None,
        11 => Some(String::new()),
        k => Some(k.to_string()),
    }
}

/// Any kind but a Group.
fn kind(ws: &Workspace, p: &Pick) -> CueKind {
    let ids = all_ids(ws);
    match p.a % 7 {
        0 => CueKind::Memo,
        1 => CueKind::Wait {
            duration: time(p.b),
        },
        2 | 3 => {
            let mut kind = audio();
            if let CueKind::Audio {
                levels, start_time, ..
            } = &mut kind
            {
                levels.master_db = level(p.b);
                levels.outputs = vec![out(level(p.c)); (p.d % 4) as usize];
                *start_time = time(p.d / 4);
            }
            kind
        }
        4 => {
            let target = choose(&ids, p.b);
            CueKind::Fade {
                target,
                duration: time(p.c),
                curve: FadeCurve::Linear,
                levels: FadeLevels {
                    master_db: Some(level(p.d)),
                    output_db: vec![],
                    crosspoints: vec![],
                },
                stop_target_when_done: false,
            }
        }
        5 => CueKind::Arm {
            target: choose(&ids, p.b),
        },
        _ => CueKind::Network {
            destination: net_id(1),
            address: "/x".into(),
            args: vec![OscArg::Float {
                value: p.b as f32 / 7.0,
            }],
        },
    }
}

fn make_cue(ws: &Workspace, g: &mut Gen, p: &Pick) -> Cue {
    let id = g.fresh();
    let mut cue = if p.a % 11 == 10 {
        // A group holding a memo and another group.
        let inner = common::cue(
            g.fresh().0.as_u128(),
            group(vec![common::cue(g.fresh().0.as_u128(), CueKind::Memo)]),
        );
        let child = common::cue(g.fresh().0.as_u128(), CueKind::Memo);
        common::cue(id.0.as_u128(), group(vec![child, inner]))
    } else {
        common::cue(id.0.as_u128(), kind(ws, p))
    };
    cue.number = number(p.c);
    cue.name = format!("cue {}", id.0.as_u128());
    cue
}

fn property(ws: &Workspace, variant: u8, p: &Pick) -> CueProperty {
    match variant % 14 {
        0 => CueProperty::Name {
            value: format!("name {}", p.b),
        },
        1 => CueProperty::Notes {
            value: format!("notes {}", p.b),
        },
        2 => CueProperty::Color {
            value: [
                CueColor::None,
                CueColor::Red,
                CueColor::Orange,
                CueColor::Green,
                CueColor::Blue,
                CueColor::Purple,
            ][p.b as usize % 6],
        },
        3 => CueProperty::Flagged {
            value: p.b.is_multiple_of(2),
        },
        4 => CueProperty::Armed {
            value: p.b.is_multiple_of(2),
        },
        5 => CueProperty::PreWait { value: time(p.b) },
        6 => CueProperty::PostWait { value: time(p.b) },
        7 => CueProperty::ContinueMode {
            value: [
                ContinueMode::DoNotContinue,
                ContinueMode::AutoContinue,
                ContinueMode::AutoFollow,
            ][p.b as usize % 3],
        },
        8 => CueProperty::Triggers {
            value: Triggers {
                hotkey: ["a", "f5", "Bad Key", ""]
                    .get(p.b as usize % 5)
                    .map(|key| Hotkey {
                        key: (*key).into(),
                        modifiers: Modifiers::default(),
                    }),
                osc: p.c.is_multiple_of(2).then(|| "/go".into()),
            },
        },
        9 => CueProperty::Target {
            value: choose(&all_ids(ws), p.b),
        },
        10 => CueProperty::GroupMode {
            value: [
                GroupMode::Timeline,
                GroupMode::StartRandom,
                GroupMode::Playlist {
                    loop_: true,
                    shuffle: false,
                },
            ][p.b as usize % 3],
        },
        12 => CueProperty::SecondTrigger {
            value: [
                SecondTrigger::Nothing,
                SecondTrigger::Panic,
                SecondTrigger::Stop,
                SecondTrigger::HardStop,
                SecondTrigger::HardStopAndRestart,
                SecondTrigger::Devamp,
                SecondTrigger::PlayNext,
                SecondTrigger::PlayPrevious,
            ][p.b as usize % 8],
        },
        13 => CueProperty::SecondTriggerOnRelease {
            value: p.b.is_multiple_of(2),
        },
        _ => CueProperty::Kind { value: kind(ws, p) },
    }
}

/// Operations 0–10 are the commands; 100 + n is `SetCueProperty` with property variant n.
fn build(ws: &Workspace, g: &mut Gen, op: u8, p: &Pick) -> Option<Command> {
    let ids = all_ids(ws);
    let id = choose(&ids, p.a);
    Some(match op {
        0 => Command::AddCue {
            at: position(ws, p.b, p.c),
            cue: make_cue(ws, g, p),
            auto_number: false,
        },
        1 => Command::AddCue {
            at: position(ws, p.b, p.c),
            cue: make_cue(ws, g, p),
            auto_number: true,
        },
        2 => {
            let clipboard: Vec<Cue> = [choose(&ids, p.a)?, choose(&ids, p.b)?]
                .iter()
                .take(1 + (p.c % 2) as usize)
                .map(|&id| ws.find_cue(id).unwrap().clone())
                .collect();
            // Copying a cue together with its own descendant would paste it twice: fine, ids
            // are fresh each time.
            let cues = prepare_paste(ws, &clipboard, || g.fresh()).ok()?;
            Command::Paste {
                at: position(ws, p.c, p.d),
                cues,
            }
        }
        3 => Command::RemoveCue { id: id? },
        4 => {
            let id = id?;
            let (_, siblings) = container_of(ws, id)?;
            Command::MoveCue {
                id,
                index: p.b as usize % (siblings + 1),
            }
        }
        5 => Command::ReparentCue {
            id: id?,
            to: position(ws, p.b, p.c),
        },
        6 => Command::SetCueProperty {
            id: id?,
            property: property(ws, (p.d % 14) as u8, p),
        },
        7 => {
            let selected = (0..1 + p.c % 3).filter_map(|n| choose(&ids, p.a.wrapping_add(n * 7)));
            Command::BatchSet {
                ids: selected.collect(),
                // Every variant but `Kind` (11).
                property: property(
                    ws,
                    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 13][p.d as usize % 13],
                    p,
                ),
            }
        }
        8 => Command::SetCueNumbers {
            numbers: (0..1 + p.c % 3)
                .filter_map(|n| {
                    Some(NumberAssignment {
                        id: choose(&ids, p.a.wrapping_add(n * 5))?,
                        number: number(p.b.wrapping_add(n * 3)),
                    })
                })
                .collect(),
        },
        9 => Command::Renumber {
            ids: (0..1 + p.c % 4)
                .filter_map(|n| choose(&ids, p.a.wrapping_add(n * 3)))
                .collect(),
            start: [1.0, 0.0, 10.0, 0.1, -1.0, f64::NAN][p.b as usize % 6],
            increment: [1.0, 5.0, 0.1, 0.0, f64::INFINITY, 0.5][p.d as usize % 6],
        },
        10 => {
            // Built against the workspace as it is now, so later commands may well be refused.
            let commands = (0..2 + p.d % 2)
                .filter_map(|n| {
                    let inner = Pick {
                        a: p.a.wrapping_add(n * 11),
                        b: p.b.wrapping_add(n * 13),
                        ..p.clone()
                    };
                    build(ws, g, (p.c as u8).wrapping_add(n as u8) % 10, &inner)
                })
                .collect();
            Command::Transaction { commands }
        }
        100..=113 => Command::SetCueProperty {
            id: id?,
            property: property(ws, op - 100, p),
        },
        _ => return None,
    })
}

fn base() -> Workspace {
    let mut ws = workspace(vec![]);
    ws.settings.auto_number_start = 1.0;
    ws.settings.auto_number_increment = 1.0;
    ws
}

/// A workspace and id generator after the random `prefix` of commands (refused ones skipped).
fn after_prefix(prefix: &[Pick]) -> (Workspace, Gen) {
    let (mut ws, mut g) = (base(), Gen { next: 1000 });
    for p in prefix {
        if let Some(command) = build(&ws, &mut g, p.op, p) {
            let _ = ws.apply(command);
        }
    }
    (ws, g)
}

fn bytes(ws: &Workspace) -> String {
    serde_json::to_string(ws).unwrap()
}

/// What commands may leave behind: broken references, silent extra outputs and playlist-only
/// second triggers on other cues, never a value
/// problem or an error. And the show saves and loads back identical.
fn check_invariants(ws: &Workspace) -> Result<(), TestCaseError> {
    for issue in validate(ws) {
        prop_assert!(
            matches!(
                issue.kind,
                IssueKind::TargetMissing { .. }
                    | IssueKind::TargetWrongKind { .. }
                    | IssueKind::TargetSelf
                    | IssueKind::UnpatchedOutputs { .. }
                    // Kept and inert, like a wrong-kind target (a group-mode change makes it
                    // apply again).
                    | IssueKind::SecondTriggerNeedsPlaylist
            ) && issue.severity == Severity::Warning,
            "command introduced {issue}"
        );
    }
    let text = save_show(ws).map_err(|e| TestCaseError::fail(e.to_string()))?;
    let loaded = load_show(&text).map_err(|e| TestCaseError::fail(e.to_string()))?;
    prop_assert_eq!(&loaded.workspace, ws);
    Ok(())
}

/// Applies `command` to a copy: refused means unchanged; applied means undo restores the
/// original and redo the result, and the invariants hold. Returns whether it applied.
fn check_command(ws: &Workspace, command: Command) -> Result<bool, TestCaseError> {
    let mut work = ws.clone();
    let Ok(inverse) = work.apply(command) else {
        prop_assert_eq!(&work, ws);
        prop_assert_eq!(bytes(&work), bytes(ws));
        return Ok(false);
    };
    let after = work.clone();
    check_invariants(&after)?;
    let redo = work
        .apply(inverse)
        .map_err(|e| TestCaseError::fail(format!("undo: {e}")))?;
    prop_assert_eq!(&work, ws);
    prop_assert_eq!(bytes(&work), bytes(ws));
    work.apply(redo)
        .map_err(|e| TestCaseError::fail(format!("redo: {e}")))?;
    prop_assert_eq!(&work, &after);
    Ok(true)
}

fn round_trip(prefix: &[Pick], op: u8, p: &Pick) -> Result<(), TestCaseError> {
    let (ws, mut g) = after_prefix(prefix);
    if let Some(command) = build(&ws, &mut g, op, p) {
        check_command(&ws, command)?;
    }
    Ok(())
}

macro_rules! round_trip_tests {
    ($($name:ident: $op:expr,)*) => {
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(64))]
            $(
                #[test]
                fn $name(prefix in picks(30), p in pick()) {
                    round_trip(&prefix, $op, &p)?;
                }
            )*
        }
    };
}

round_trip_tests! {
    add_cue_round_trips: 0,
    add_cue_with_auto_number_round_trips: 1,
    paste_round_trips: 2,
    remove_cue_round_trips: 3,
    move_cue_round_trips: 4,
    reparent_cue_round_trips: 5,
    set_property_round_trips: 6,
    batch_set_round_trips: 7,
    set_cue_numbers_round_trips: 8,
    renumber_round_trips: 9,
    transaction_round_trips: 10,
    set_name_round_trips: 100,
    set_notes_round_trips: 101,
    set_color_round_trips: 102,
    set_flagged_round_trips: 103,
    set_armed_round_trips: 104,
    set_pre_wait_round_trips: 105,
    set_post_wait_round_trips: 106,
    set_continue_mode_round_trips: 107,
    set_triggers_round_trips: 108,
    set_target_round_trips: 109,
    set_group_mode_round_trips: 110,
    set_kind_round_trips: 111,
    set_second_trigger_round_trips: 112,
    set_second_trigger_on_release_round_trips: 113,
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Whatever happens, every step keeps the invariants; undoing everything gives back the
    /// empty workspace step by step, and redoing everything the final one.
    #[test]
    fn random_sequences_undo_and_redo_exactly(sequence in picks(60)) {
        let (mut ws, mut g) = (base(), Gen { next: 1000 });
        let mut history = History::new();
        let mut states = vec![ws.clone()];
        for p in &sequence {
            let Some(command) = build(&ws, &mut g, p.op, p) else { continue };
            let before = ws.clone();
            let recorded = history.undo_len();
            if history.execute(&mut ws, command).is_err() {
                prop_assert_eq!(&ws, &before);
                prop_assert_eq!(bytes(&ws), bytes(&before));
                continue;
            }
            check_invariants(&ws)?;
            if history.undo_len() > recorded {
                states.push(ws.clone());
            }
        }
        let last = ws.clone();
        for expected in states.iter().rev().skip(1) {
            prop_assert!(history.undo(&mut ws).unwrap());
            prop_assert_eq!(&ws, expected);
        }
        prop_assert!(!history.undo(&mut ws).unwrap());
        for expected in states.iter().skip(1) {
            prop_assert!(history.redo(&mut ws).unwrap());
            prop_assert_eq!(&ws, expected);
        }
        prop_assert_eq!(&ws, &last);
    }

    /// A new edit after undo drops everything that could have been redone.
    #[test]
    fn a_new_edit_clears_redo_whatever_came_before(sequence in picks(40), undos in 1usize..6) {
        let (mut ws, mut g) = (base(), Gen { next: 1000 });
        let mut history = History::new();
        for p in &sequence {
            if let Some(command) = build(&ws, &mut g, p.op, p) {
                let _ = history.execute(&mut ws, command);
            }
        }
        for _ in 0..undos {
            history.undo(&mut ws).unwrap();
        }
        let redoable = history.can_redo();
        let fresh = Command::AddCue {
            at: Position { list: list_id(1), parent: None, index: 0 },
            cue: common::cue(999_999, CueKind::Memo),
            auto_number: false,
        };
        history.execute(&mut ws, fresh).unwrap();
        prop_assert!(!history.can_redo(), "redo survived an edit (had redo: {redoable})");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// A client that starts from the workspace and applies each edit's `model_changed` ends
    /// every step with exactly the engine's workspace: commands (groups moved and deleted with
    /// children, renumbering, transactions), undo and redo alike (ED-07, NFR-01).
    #[test]
    fn model_changes_keep_a_mirror_identical(sequence in picks(80)) {
        let (mut ws, mut g) = (base(), Gen { next: 1000 });
        let mut mirror = ws.clone();
        let mut history = History::new();
        for p in &sequence {
            let footprint = match p.d % 9 {
                0 => history.peek_undo().map(Command::footprint),
                1 => history.peek_redo().map(Command::footprint),
                _ => None,
            };
            let changed = match (p.d % 9, footprint) {
                (0, Some(fp)) => history.undo(&mut ws).unwrap().then_some(fp),
                (1, Some(fp)) => history.redo(&mut ws).unwrap().then_some(fp),
                (0 | 1, None) => None,
                _ => {
                    let Some(command) = build(&ws, &mut g, p.op, p) else { continue };
                    let fp = command.footprint();
                    history.execute(&mut ws, command).is_ok().then_some(fp)
                }
            };
            let Some(fp) = changed else { continue };
            let (upserts, deletes) = model_changes(&ws, &fp);
            // O(changed): never more than a delete and an upsert per named cue, plus the
            // subtrees of groups that were named.
            let named = fp.added.len() + fp.removed.len() + fp.moved.len() + fp.edited.len();
            prop_assert!(upserts.len() <= named);
            prop_assert!(deletes.len() <= named);
            mirror.apply_changes(&upserts, &deletes).unwrap();
            prop_assert_eq!(&mirror, &ws);
        }
    }
}

/// The property tests above mean nothing if nearly every random command is refused: check that
/// each command succeeds often enough on the same generator.
#[test]
fn the_generator_exercises_every_command() {
    use std::collections::HashMap;
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        // xorshift: deterministic, no clock or OS randomness
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 16) as u32
    };
    let mut applied: HashMap<u8, usize> = HashMap::new();
    let mut refused: HashMap<u8, usize> = HashMap::new();
    for _ in 0..20 {
        let (mut ws, mut g) = (base(), Gen { next: 1000 });
        for _ in 0..80 {
            let p = Pick {
                op: (next() % 11) as u8,
                a: next(),
                b: next(),
                c: next(),
                d: next(),
            };
            let Some(command) = build(&ws, &mut g, p.op, &p) else {
                continue;
            };
            let tally = if ws.apply(command).is_ok() {
                &mut applied
            } else {
                &mut refused
            };
            *tally.entry(p.op).or_default() += 1;
        }
    }
    for op in 0..11u8 {
        assert!(
            applied.get(&op).copied().unwrap_or(0) >= 20,
            "op {op}: applied {applied:?}, refused {refused:?}"
        );
    }
    // And invalid input is generated, not just valid.
    for op in [0u8, 2, 4, 5, 6, 7, 8, 9, 10] {
        assert!(
            refused.get(&op).copied().unwrap_or(0) >= 3,
            "op {op} never refused"
        );
    }
}
