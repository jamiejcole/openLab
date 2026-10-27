// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! M2.24: where the QLab 5 checks (answers of 2026-10-27) showed openLab behaving differently,
//! openLab now does what QLab does. One regression test per point, named after the check's id.
//! Virtual sample clock at 1 kHz: one second is 1000 samples.

mod common;

use common::*;
use openlab_core::*;

use ContinueMode::{AutoContinue as AC, AutoFollow as AF};

const RATE: u32 = 1000;

fn id(n: u128) -> CueId {
    cue_id(n)
}

fn list() -> CueListId {
    list_id(1)
}

fn memo(n: u128) -> Cue {
    cue(n, CueKind::Memo)
}

fn wait(n: u128, seconds: f64) -> Cue {
    cue(n, CueKind::Wait { duration: seconds })
}

fn sound(n: u128) -> Cue {
    cue(n, audio())
}

/// An audio cue with no media file: it has static problems (`no_media`), so it shows as broken.
fn broken_sound(n: u128) -> Cue {
    let mut c = sound(n);
    if let CueKind::Audio { media, .. } = &mut c.kind {
        *media = None;
    }
    c
}

fn timed(mut c: Cue, pre: f64, post: f64, mode: ContinueMode) -> Cue {
    c.pre_wait = pre;
    c.post_wait = post;
    c.continue_mode = mode;
    c
}

fn grp(n: u128, mode: GroupMode, children: Vec<Cue>) -> Cue {
    cue(n, CueKind::Group { mode, children })
}

fn playlist() -> GroupMode {
    GroupMode::Playlist {
        loop_: false,
        shuffle: false,
    }
}

struct Rig {
    ws: Workspace,
    engine: CueEngine,
    last: Option<Workspace>,
}

impl Rig {
    fn new(cues: Vec<Cue>) -> Self {
        Self::seeded(cues, 1)
    }

    fn seeded(cues: Vec<Cue>, seed: u64) -> Self {
        let mut rig = Self {
            ws: workspace(cues),
            engine: CueEngine::new(EngineConfig::new(RATE, 0, seed).unwrap()),
            last: None,
        };
        rig.tick(0);
        rig
    }

    fn at(&mut self, now: SampleTime, input: Input) -> Vec<Action> {
        handle_as_binary(&mut self.engine, &self.ws, &mut self.last, input, now)
    }

    fn tick(&mut self, now: SampleTime) -> Vec<Action> {
        self.at(now, Input::Tick)
    }

    fn go(&mut self, now: SampleTime) -> Vec<Action> {
        self.at(now, Input::Go { list: list() })
    }

    fn start(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(now, Input::Start { cue: id(n) })
    }

    fn set_playhead(&mut self, n: u128) {
        self.at(
            0,
            Input::SetPlayhead {
                list: list(),
                cue: Some(id(n)),
            },
        );
    }

    fn state(&self, n: u128) -> CueState {
        self.engine.state(id(n))
    }

    fn playhead(&self) -> Option<CueId> {
        self.engine.playhead(list())
    }
}

fn changes(actions: &[Action], n: u128) -> Vec<(CueState, SampleTime)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::StateChanged { cue, state, at } if *cue == id(n) => Some((*state, *at)),
            _ => None,
        })
        .collect()
}

/// `(cue, at)` of every start (a report of pre-waiting).
fn starts(actions: &[Action]) -> Vec<(CueId, SampleTime)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::StateChanged {
                cue,
                state: CueState::PreWaiting,
                at,
            } => Some((*cue, *at)),
            _ => None,
        })
        .collect()
}

fn warnings(actions: &[Action]) -> Vec<EngineWarning> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Warning(w) => Some(w.clone()),
            _ => None,
        })
        .collect()
}

// ----- G1: a random group's chosen child keeps its own continue mode -----

#[test]
fn g1_random_child_keeps_its_own_continue_mode() {
    // Child 10 auto-continues 0.5 s into its action; 11 is a plain wait. Round-robin picks each
    // once in the first two starts.
    let r = grp(
        1,
        GroupMode::StartRandom,
        vec![timed(memo(10), 0.0, 0.5, AC), wait(11, 1.0)],
    );
    let mut rig = Rig::new(vec![r]);
    let mut picked_10 = false;
    for t in [0, 10_000] {
        let start = rig.start(t, 1);
        let mut later = rig.tick(t + 500);
        later.extend(rig.tick(t + 2000));
        let first: Vec<CueId> = starts(&start)
            .into_iter()
            .map(|(c, _)| c)
            .filter(|c| *c != id(1))
            .collect();
        assert_eq!(first.len(), 1, "one child picked: {first:?}");
        if first[0] == id(10) {
            picked_10 = true;
            // QLab: the chosen child's auto-continue fires into the cue after it.
            assert_eq!(starts(&later), vec![(id(11), t + 500)]);
        } else {
            assert!(starts(&later).is_empty());
        }
        assert_eq!(rig.state(1), CueState::Idle);
    }
    assert!(picked_10);
}

// ----- G2: a playlist with a broken child skips it and still auto-follows -----

#[test]
fn g2_playlist_skips_a_broken_child_and_still_auto_follows_out_of_the_group() {
    let p = timed(
        grp(
            1,
            playlist(),
            vec![wait(10, 0.5), broken_sound(11), wait(12, 0.5)],
        ),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::new(vec![p, memo(2)]);
    rig.go(0);
    let skip = rig.tick(500);
    assert!(warnings(&skip).contains(&EngineWarning::CueBroken { cue: id(11) }));
    assert!(starts(&skip).contains(&(id(12), 500)));
    let end = rig.tick(1000);
    // QLab: the playlist auto-follows out of the group onto the next cue.
    assert_eq!(starts(&end), vec![(id(2), 1000)]);
    // Done, it shows as broken again (G6: its child is broken).
    assert_eq!(changes(&end, 1).last(), Some(&(CueState::Broken, 1000)));
}

// ----- G4/G5: GO on a child of a stopped group -----

fn three_children(mode: GroupMode) -> Rig {
    let g = grp(1, mode, vec![wait(10, 1.0), wait(11, 1.0), wait(12, 1.0)]);
    let mut rig = Rig::new(vec![g, memo(2), memo(3)]);
    rig.set_playhead(11);
    rig
}

#[test]
fn g4_playlist_go_on_a_child_plays_it_and_follows_through_the_rest() {
    let mut rig = three_children(playlist());
    let go = rig.go(0);
    assert_eq!(starts(&go), vec![(id(11), 0)]);
    // The group does not show as running.
    assert!(changes(&go, 1).is_empty());
    assert_eq!(rig.state(1), CueState::Idle);
    let next = rig.tick(1000);
    assert_eq!(starts(&next), vec![(id(12), 1000)]);
    assert_eq!(rig.state(1), CueState::Idle);
    let end = rig.tick(2000);
    assert!(starts(&end).is_empty());
    assert_eq!(changes(&end, 12), vec![(CueState::Idle, 2000)]);
    assert!(changes(&end, 1).is_empty());
    // It does not wrap to the first child, nor fire anything after the group.
    assert_eq!(rig.state(10), CueState::Idle);
    assert_eq!(rig.state(2), CueState::Idle);
    assert!(rig.engine.next_deadline().is_none());
}

#[test]
fn g4_timeline_go_on_a_child_plays_only_that_child_and_the_group_does_not_run() {
    let mut rig = three_children(GroupMode::Timeline);
    let go = rig.go(0);
    assert_eq!(starts(&go), vec![(id(11), 0)]);
    assert!(changes(&go, 1).is_empty());
    assert_eq!(rig.state(1), CueState::Idle);
    assert_eq!(rig.state(11), CueState::Running);
    let end = rig.tick(1000);
    assert!(starts(&end).is_empty());
    assert!(changes(&end, 1).is_empty());
    assert_eq!(rig.state(11), CueState::Idle);
}

#[test]
fn g5_playhead_after_go_on_a_child_of_a_stopped_group_follows_the_mode() {
    // Playlist: the playhead goes to the next cue outside the group.
    let mut rig = three_children(playlist());
    rig.go(0);
    assert_eq!(rig.playhead(), Some(id(2)));
    // Timeline: to the next sibling.
    let mut rig = three_children(GroupMode::Timeline);
    rig.go(0);
    assert_eq!(rig.playhead(), Some(id(12)));
}

// ----- G6: a group with a broken child is broken, but can still be GOed -----

#[test]
fn g6_a_group_with_a_broken_child_shows_broken_and_can_still_be_goed() {
    let p = grp(1, playlist(), vec![broken_sound(10), wait(11, 1.0)]);
    let mut rig = Rig {
        ws: workspace(vec![p, memo(2)]),
        engine: CueEngine::new(EngineConfig::new(RATE, 0, 1).unwrap()),
        last: None,
    };
    let first = rig.tick(0);
    assert_eq!(changes(&first, 1), vec![(CueState::Broken, 0)]);
    assert_eq!(rig.state(1), CueState::Broken);
    assert!(rig.engine.has_broken_descendant(id(1)));
    assert!(
        rig.engine
            .cue_states()
            .any(|(c, s)| c == id(1) && s == CueState::Broken)
    );
    // The group itself has no problem of its own; the pre-show list names the child.
    assert!(rig.engine.problems(id(1)).is_empty());

    let go = rig.go(0);
    // It starts; its broken child is skipped (it shows broken throughout) and the next plays.
    assert_eq!(starts(&go), vec![(id(1), 0), (id(11), 0)]);
    assert!(warnings(&go).contains(&EngineWarning::CueBroken { cue: id(10) }));
    assert_eq!(rig.state(1), CueState::Running);
    let end = rig.tick(1000);
    assert_eq!(changes(&end, 1), vec![(CueState::Broken, 1000)]);

    // Fixed: the group is no longer broken.
    if let CueKind::Group { children, .. } = &mut rig.ws.cue_lists[0].cues[0].kind {
        children[0] = sound(10);
    }
    let fixed = rig.tick(2000);
    assert_eq!(changes(&fixed, 1), vec![(CueState::Idle, 2000)]);
    assert_eq!(rig.state(1), CueState::Idle);
}

// ----- D5: GO on a Devamp with "start next cue" stands on the cue it will start -----

#[test]
fn d5_go_on_a_devamp_with_start_next_moves_the_playhead_onto_that_cue() {
    let devamp = cue(
        2,
        CueKind::Devamp {
            target: Some(id(1)),
            start_next_cue: true,
            stop_target_when_slice_ends: false,
        },
    );
    let mut rig = Rig::new(vec![sound(1), devamp, sound(3), memo(4)]);
    rig.go(0);
    assert_eq!(rig.playhead(), Some(id(2)));
    rig.go(100);
    // QLab: the playhead moves to song B (3) at once, before B starts.
    assert_eq!(rig.playhead(), Some(id(3)));
    assert_eq!(rig.state(3), CueState::Idle);
    let run = rig.engine.run_of(id(1)).unwrap();
    rig.at(
        200,
        Input::DevampScheduled {
            cue: id(1),
            run,
            at: 500,
        },
    );
    let left = rig.tick(500);
    assert_eq!(starts(&left), vec![(id(3), 500)]);
    // It stays on B after B starts.
    assert_eq!(rig.playhead(), Some(id(3)));
}

// ----- D6: slices and loops are at least 51 ms -----

fn sliced(start: f64, end: f64, marks: &[(f64, u32)]) -> CueKind {
    let mut kind = audio();
    if let CueKind::Audio {
        start_time,
        end_time,
        slices,
        last_slice_play_count,
        ..
    } = &mut kind
    {
        *start_time = start;
        *end_time = Some(end);
        *slices = marks
            .iter()
            .map(|&(time, play_count)| SliceMarker { time, play_count })
            .collect();
        *last_slice_play_count = 1;
    }
    kind
}

fn slices_of(ws: &Workspace) -> Vec<f64> {
    match &ws.cue_lists[0].cues[0].kind {
        CueKind::Audio { slices, .. } => slices.iter().map(|s| s.time).collect(),
        _ => panic!("not audio"),
    }
}

#[test]
fn d6_a_50_ms_slice_is_refused_and_a_51_ms_loop_plays() {
    assert_eq!(MIN_SLICE_SECONDS, 0.051);

    // 50 ms: the show file is repaired (the marker that ends it is removed), and an edit that
    // would make one is refused, as QLab's editor refuses it.
    let short = sliced(0.0, 2.0, &[(1.0, 1), (1.05, 0)]);
    let mut ws = workspace(vec![cue(1, short.clone())]);
    let issues = repair(&mut ws);
    assert!(
        issues
            .iter()
            .any(|i| i.kind == IssueKind::SliceTooShort { time: 1.05 }),
        "{issues:?}"
    );
    assert_eq!(slices_of(&ws), vec![1.0]);
    // Also against the trim points: a marker 50 ms after the start or before the end.
    for marks in [[(0.55, 1)], [(1.95, 1)]] {
        let mut ws = workspace(vec![cue(1, sliced(0.5, 2.0, &marks))]);
        assert!(
            repair(&mut ws)
                .iter()
                .any(|i| matches!(i.kind, IssueKind::SliceTooShort { .. }))
        );
        assert!(slices_of(&ws).is_empty());
    }
    let mut ws = workspace(vec![sound(1)]);
    let refused = ws.apply(Command::SetCueProperty {
        id: id(1),
        property: CueProperty::Kind { value: short },
    });
    assert!(
        matches!(&refused, Err(CommandError::InvalidCue(issues))
            if issues.iter().any(|i| matches!(i.kind, IssueKind::SliceTooShort { .. }))),
        "{refused:?}"
    );

    // 51 ms: accepted everywhere, and it plays (an infinite loop of it).
    let ok = sliced(0.0, 2.0, &[(1.0, 1), (1.051, 0)]);
    let mut ws = workspace(vec![sound(1)]);
    ws.apply(Command::SetCueProperty {
        id: id(1),
        property: CueProperty::Kind { value: ok },
    })
    .unwrap();
    assert!(validate(&ws).is_empty(), "{:?}", validate(&ws));
    let mut rig = Rig {
        ws,
        engine: CueEngine::new(EngineConfig::new(RATE, 0, 1).unwrap()),
        last: None,
    };
    rig.tick(0);
    let go = rig.go(0);
    assert!(
        go.iter()
            .any(|a| matches!(a, Action::StartVoice { cue, .. } if *cue == id(1)))
    );
    // The audio side's planner takes such a loop at the usual device rates.
    for rate in [44_100u32, 48_000, 96_000] {
        let rate_nz = std::num::NonZeroU32::new(rate).unwrap();
        let frames = |s: f64| seconds_to_samples(s, rate_nz);
        let plan = openlab_audio::plan_at_rate(
            0..frames(2.0),
            &[frames(1.0), frames(1.051)],
            &[
                openlab_audio::Plays::Times(1),
                openlab_audio::Plays::Infinite,
                openlab_audio::Plays::Times(1),
            ],
            0,
            rate,
        );
        assert!(plan.is_ok(), "{rate} Hz: {plan:?}");
    }
}

// ----- F4: see the report; needs a protocol change (a new problem) -----

// ----- P5: GO on a broken auto-follow cue -----

#[test]
fn p5_go_on_a_broken_auto_follow_cue_starts_the_next_cue_and_moves_past_it() {
    let mut rig = Rig::new(vec![
        timed(broken_sound(1), 0.0, 0.0, AF),
        wait(2, 1.0),
        memo(3),
    ]);
    let go = rig.go(0);
    assert!(warnings(&go).contains(&EngineWarning::CueBroken { cue: id(1) }));
    // QLab: the broken cue does not play, but the cue it auto-follows into does.
    assert!(
        !go.iter()
            .any(|a| matches!(a, Action::StartVoice { cue, .. } if *cue == id(1)))
    );
    assert!(starts(&go).contains(&(id(2), 0)));
    assert_eq!(rig.state(2), CueState::Running);
    assert_eq!(rig.playhead(), Some(id(3)));
}

// ----- K1: two cues with the same hotkey both start, with no warning -----

#[test]
fn k1_two_cues_with_the_same_hotkey_both_start_with_no_warning() {
    let w = Hotkey {
        key: "w".into(),
        modifiers: Modifiers::default(),
    };
    let keyed = |c: Cue| Cue {
        triggers: Triggers {
            hotkey: Some(w.clone()),
            osc: None,
        },
        ..c
    };
    let mut rig = Rig::new(vec![
        keyed(wait(1, 1.0)),
        memo(2),
        grp(3, GroupMode::Timeline, vec![keyed(wait(30, 1.0))]),
    ]);
    // No validation warning for the shared key.
    assert!(validate(&rig.ws).is_empty(), "{:?}", validate(&rig.ws));
    // A press starts every cue with that key, in list order.
    let cues = rig.ws.cues_with_hotkey(&w);
    assert_eq!(cues, vec![id(1), id(30)]);
    let shift_w = Hotkey {
        modifiers: Modifiers {
            shift: true,
            ..Modifiers::default()
        },
        ..w.clone()
    };
    assert!(rig.ws.cues_with_hotkey(&shift_w).is_empty());
    let mut out = Vec::new();
    for c in cues {
        out.extend(rig.at(100, Input::Start { cue: c }));
    }
    assert!(warnings(&out).is_empty(), "{out:?}");
    assert_eq!(rig.state(1), CueState::Running);
    assert_eq!(rig.state(30), CueState::Running);
}
