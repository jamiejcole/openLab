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

    // 50 ms: the show file is repaired (the marker that ends it moves to make it 51 ms long),
    // and an edit that would make one is refused, as QLab's editor refuses it.
    let short = sliced(0.0, 2.0, &[(1.0, 1), (1.05, 0)]);
    let mut ws = workspace(vec![cue(1, short.clone())]);
    let issues = repair(&mut ws);
    assert!(
        issues.iter().any(|i| i.kind
            == IssueKind::SliceMoved {
                from: 1.05,
                to: 1.0 + MIN_SLICE_SECONDS
            }),
        "{issues:?}"
    );
    assert_eq!(slices_of(&ws), vec![1.0, 1.0 + MIN_SLICE_SECONDS]);
    // Also against the trim points: a marker 50 ms after the start or before the end.
    for (marks, to) in [
        ([(0.55, 1)], 0.5 + MIN_SLICE_SECONDS),
        ([(1.95, 1)], 2.0 - MIN_SLICE_SECONDS),
    ] {
        let mut ws = workspace(vec![cue(1, sliced(0.5, 2.0, &marks))]);
        assert!(
            repair(&mut ws)
                .iter()
                .any(|i| matches!(i.kind, IssueKind::SliceMoved { .. }))
        );
        assert_eq!(slices_of(&ws), vec![to]);
    }
    let mut ws = workspace(vec![sound(1)]);
    let refused = ws.apply(Command::SetCueProperty {
        id: id(1),
        property: CueProperty::Kind { value: short },
    });
    assert!(
        matches!(&refused, Err(CommandError::InvalidCue(issues))
            if issues.iter().any(|i| matches!(i.kind, IssueKind::SliceMoved { .. }))),
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

// ----- fix round 1 (review of M2.24) -----

#[test]
fn h1_go_on_a_playlist_child_stands_on_the_cue_after_the_group_without_losing_its_continue() {
    // The group never runs, so its auto-follow (2) never fires: the playhead stands on 2.
    let p = timed(
        grp(1, playlist(), vec![wait(10, 1.0), wait(11, 1.0)]),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::new(vec![p, memo(2), memo(3)]);
    rig.set_playhead(10);
    rig.go(0);
    assert_eq!(rig.playhead(), Some(id(2)));
    assert_eq!(starts(&rig.tick(1000)), vec![(id(11), 1000)]);
    assert!(starts(&rig.tick(2000)).is_empty());
    assert_eq!(starts(&rig.go(2100)), vec![(id(2), 2100)]);
    assert_eq!(rig.playhead(), Some(id(3)));
}

#[test]
fn h1_leaving_a_start_first_group_that_never_ran_stands_on_its_continue_target() {
    let g = timed(
        grp(
            1,
            GroupMode::StartFirstAndGoToNext,
            vec![memo(10), wait(11, 1.0)],
        ),
        0.0,
        0.0,
        AC,
    );
    let mut rig = Rig::new(vec![g, memo(2), memo(3)]);
    rig.set_playhead(11);
    rig.go(0);
    assert_eq!(rig.playhead(), Some(id(2)));
    // A group that ran and fired its continue: its target has played, so it is skipped.
    let g = timed(
        grp(1, GroupMode::StartFirstAndEnter, vec![memo(10), memo(11)]),
        0.0,
        0.0,
        AC,
    );
    let mut rig = Rig::new(vec![g, memo(2), memo(3)]);
    let go = rig.go(0);
    assert!(starts(&go).contains(&(id(2), 0)));
    assert_eq!(rig.playhead(), Some(id(11)));
    rig.go(100);
    assert_eq!(rig.playhead(), Some(id(3)));
}

/// A slice marker as `(time, play count)`.
type Mark = (f64, u32);

fn audio_of(ws: &Workspace) -> (Option<f64>, Vec<(f64, u32)>, u32) {
    match &ws.cue_lists[0].cues[0].kind {
        CueKind::Audio {
            end_time,
            slices,
            last_slice_play_count,
            ..
        } => (
            *end_time,
            slices.iter().map(|s| (s.time, s.play_count)).collect(),
            *last_slice_play_count,
        ),
        _ => panic!("not audio"),
    }
}

#[test]
fn m1_d6_repair_keeps_a_short_trim_and_the_loops_it_merges() {
    // A 40 ms trim is moved to the 51 ms floor, not cleared (which would play the whole file).
    let mut trim = sliced(1.0, 1.04, &[]);
    let mut ws = workspace(vec![cue(1, trim.clone())]);
    let issues = repair(&mut ws);
    assert_eq!(audio_of(&ws).0, Some(1.0 + MIN_SLICE_SECONDS));
    assert!(
        issues
            .iter()
            .any(|i| matches!(i.kind, IssueKind::EndTimeTooShort { .. }))
    );
    let text = save_show(&ws).unwrap();
    assert_eq!(load_show(&text).unwrap().workspace, ws);
    assert!(validate(&ws).is_empty());
    // Not after the start at all: cleared, as before.
    if let CueKind::Audio { end_time, .. } = &mut trim {
        *end_time = Some(0.5);
    }
    let mut ws = workspace(vec![cue(1, trim)]);
    assert!(
        repair(&mut ws)
            .iter()
            .any(|i| i.kind == IssueKind::InvalidEndTime)
    );
    assert_eq!(audio_of(&ws).0, None);

    // A short slice is lengthened to 51 ms where there is room, keeping its loop or vamp, so
    // the cue's length barely changes; where there is none, it merges and the merged slice
    // plays as the longer part did. Each repaired show saves and loads back unchanged.
    let m = MIN_SLICE_SECONDS;
    let cases: [(&[Mark], Vec<Mark>, u32); 6] = [
        // A 30 ms stutter (x3) before a long rest of the song: still a 51 ms stutter.
        (&[(1.0, 1), (1.03, 3)], vec![(1.0, 1), (1.0 + m, 3)], 1),
        // A 30 ms vamp: still a vamp, 51 ms long.
        (&[(1.0, 1), (1.03, 0)], vec![(1.0, 1), (1.0 + m, 0)], 1),
        // Consecutive short slices: the first two merge (no room), then that slice is
        // lengthened; the long slice after keeps its single play.
        (&[(0.02, 2), (0.04, 3), (0.5, 1)], vec![(m, 3), (0.5, 1)], 1),
        // Too close to the end: the last marker moves earlier, the vamp before keeps its count.
        (&[(1.0, 1), (1.98, 0)], vec![(1.0, 1), (2.0 - m, 0)], 1),
        // No room at the end either: merged, and the longer part (the vamp) gives the count.
        (&[(1.93, 1), (1.98, 0)], vec![(1.93, 1)], 0),
        // A short loop next to a longer single play with no room: the single play wins, the
        // loop is not spread over it.
        (&[(1.9, 1), (1.92, 4), (1.96, 1)], vec![(1.9, 1)], 1),
    ];
    for (marks, kept, last) in cases {
        let mut ws = workspace(vec![cue(1, sliced(0.0, 2.0, marks))]);
        let issues = repair(&mut ws);
        assert!(
            issues.iter().all(|i| matches!(
                i.kind,
                IssueKind::SliceTooShort { .. } | IssueKind::SliceMoved { .. }
            )),
            "{issues:?}"
        );
        assert_eq!(audio_of(&ws), (Some(2.0), kept, last), "{marks:?}");
        let text = save_show(&ws).unwrap();
        assert_eq!(load_show(&text).unwrap().workspace, ws);
        assert!(validate(&ws).is_empty(), "{marks:?}: {:?}", validate(&ws));
    }
}

fn failed_before_playing(rig: &mut Rig, now: SampleTime, n: u128) -> Vec<Action> {
    let run = rig.engine.run_of(id(n)).unwrap();
    rig.at(
        now,
        Input::StartFailed {
            cue: id(n),
            run,
            reason: BrokenReason::StreamFailed,
        },
    )
}

#[test]
fn m2_an_auto_follow_cue_whose_stream_fails_before_playing_starts_the_next_cue() {
    // Its file went away after the probe: found only when its stream opens (QLab check P5).
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 0.0, AF), wait(2, 1.0), memo(3)]);
    rig.go(0);
    assert_eq!(rig.playhead(), Some(id(3)));
    let failed = failed_before_playing(&mut rig, 50, 1);
    assert_eq!(changes(&failed, 1), vec![(CueState::Broken, 50)]);
    assert_eq!(starts(&failed), vec![(id(2), 50)]);
    assert_eq!(
        rig.engine.broken_reason(id(1)),
        Some(BrokenReason::StreamFailed)
    );

    // A failure after it has played still cancels its auto-follow (rule 2).
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 0.0, AF), wait(2, 1.0), memo(3)]);
    rig.go(0);
    let run = rig.engine.run_of(id(1)).unwrap();
    let failed = rig.at(
        500,
        Input::ActionFailed {
            cue: id(1),
            run,
            reason: BrokenReason::StreamFailed,
        },
    );
    assert!(starts(&failed).is_empty());
}

#[test]
fn m2_a_playlist_skips_a_child_whose_stream_fails_before_playing_and_still_auto_follows() {
    let p = timed(
        grp(1, playlist(), vec![sound(10), wait(11, 0.5)]),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::new(vec![p, memo(2)]);
    rig.go(0);
    let failed = failed_before_playing(&mut rig, 50, 10);
    assert_eq!(starts(&failed), vec![(id(11), 50)]);
    let end = rig.tick(550);
    assert_eq!(starts(&end), vec![(id(2), 550)]);
}

// ----- fix round 2 (re-review of M2.24) -----

#[test]
fn h1_a_group_that_ran_earlier_does_not_hide_its_continue_from_a_later_go_on_a_child() {
    let p = timed(
        grp(1, playlist(), vec![wait(10, 1.0), wait(11, 1.0)]),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::new(vec![p, memo(2), memo(3)]);
    rig.go(0);
    rig.tick(1000);
    // The playlist completes and its auto-follow starts 2.
    assert!(starts(&rig.tick(2000)).contains(&(id(2), 2000)));
    // Later (a re-run): the playhead is put on 11 and GOed. The group does not run now, so
    // its continue does not fire: the playhead stands on 2, which is not lost.
    rig.at(
        3000,
        Input::SetPlayhead {
            list: list(),
            cue: Some(id(11)),
        },
    );
    rig.go(3000);
    assert_eq!(rig.playhead(), Some(id(2)));
    assert!(starts(&rig.tick(4000)).is_empty());
    assert_eq!(starts(&rig.go(4100)), vec![(id(2), 4100)]);
}

#[test]
fn h1_leaving_a_group_whose_own_run_fired_its_continue_does_not_play_it_twice() {
    let g = timed(
        grp(
            1,
            GroupMode::StartFirstAndEnter,
            vec![wait(10, 1.0), wait(11, 1.0)],
        ),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::new(vec![g, memo(2), memo(3)]);
    rig.go(0);
    assert_eq!(rig.playhead(), Some(id(11)));
    // The group's run completes with its first child and fires its auto-follow.
    assert!(starts(&rig.tick(1000)).contains(&(id(2), 1000)));
    let go = rig.go(1500);
    assert_eq!(starts(&go), vec![(id(11), 1500)]);
    // 2 has played: the playhead passes over it.
    assert_eq!(rig.playhead(), Some(id(3)));
}

#[test]
fn m2_a_paused_cue_whose_stream_fails_before_playing_follows_only_on_resume() {
    let list_of = || vec![timed(sound(1), 0.0, 0.0, AF), wait(2, 1.0), memo(3)];
    // Pause, then Resume.
    let mut rig = Rig::new(list_of());
    rig.go(0);
    rig.at(10, Input::Pause { cue: id(1) });
    let failed = failed_before_playing(&mut rig, 20, 1);
    assert!(starts(&failed).is_empty());
    assert_eq!(rig.state(1), CueState::Broken);
    assert!(rig.engine.is_paused(id(1)));
    assert!(starts(&rig.tick(500)).is_empty());
    let resume = rig.at(1000, Input::Resume { cue: id(1) });
    assert_eq!(starts(&resume), vec![(id(2), 1000)]);
    assert_eq!(rig.state(1), CueState::Broken);
    // Pause All, then Resume All.
    let mut rig = Rig::new(list_of());
    rig.go(0);
    rig.at(10, Input::PauseAll);
    assert!(starts(&failed_before_playing(&mut rig, 20, 1)).is_empty());
    assert_eq!(starts(&rig.at(700, Input::ResumeAll)), vec![(id(2), 700)]);
    // Paused, then stopped: the held follow is cancelled.
    let mut rig = Rig::new(list_of());
    rig.go(0);
    rig.at(10, Input::Pause { cue: id(1) });
    failed_before_playing(&mut rig, 20, 1);
    rig.at(
        30,
        Input::Stop {
            cue: id(1),
            fade_out: None,
        },
    );
    assert!(starts(&rig.at(40, Input::Resume { cue: id(1) })).is_empty());
    assert!(starts(&rig.tick(5000)).is_empty());
    assert!(rig.engine.next_deadline().is_none());
    // Paused, then a panic: cancelled too.
    let mut rig = Rig::new(list_of());
    rig.go(0);
    rig.at(10, Input::Pause { cue: id(1) });
    failed_before_playing(&mut rig, 20, 1);
    rig.at(30, Input::Panic);
    assert!(starts(&rig.at(40, Input::Resume { cue: id(1) })).is_empty());
    assert!(starts(&rig.tick(10_000)).is_empty());
}

// ----- fix round 3 (final re-review of M2.24) -----

fn children_of(rig: &mut Rig, n: u128) -> &mut Vec<Cue> {
    let cues = &mut rig.ws.cue_lists[0].cues;
    match cues.iter_mut().find(|c| c.id == id(n)).map(|c| &mut c.kind) {
        Some(CueKind::Group { children, .. }) => children,
        _ => panic!("no group {n}"),
    }
}

#[test]
fn h1_deleting_the_standby_cue_inside_a_group_keeps_the_visit_and_plays_nothing_twice() {
    let g = timed(
        grp(
            1,
            GroupMode::StartFirstAndEnter,
            vec![wait(10, 1.0), memo(11), memo(12)],
        ),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::new(vec![g, memo(2), memo(3)]);
    rig.go(0);
    assert_eq!(rig.playhead(), Some(id(11)));
    assert!(starts(&rig.tick(1000)).contains(&(id(2), 1000)));
    // The operator deletes the standby cue: the playhead moves to 12, still in the group.
    children_of(&mut rig, 1).retain(|c| c.id != id(11));
    rig.tick(1100);
    assert_eq!(rig.playhead(), Some(id(12)));
    rig.go(1200);
    // 2 has played in this visit: passed over, not played again.
    assert_eq!(rig.playhead(), Some(id(3)));
}

#[test]
fn h1_a_standby_cue_dragged_into_a_group_enters_it_afresh() {
    let p = timed(
        grp(1, playlist(), vec![wait(10, 1.0), wait(11, 1.0)]),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::new(vec![p, memo(2), memo(3), wait(12, 1.0)]);
    rig.go(0);
    rig.tick(1000);
    assert!(starts(&rig.tick(2000)).contains(&(id(2), 2000)));
    rig.at(
        2100,
        Input::SetPlayhead {
            list: list(),
            cue: Some(id(12)),
        },
    );
    // The standby cue is dragged into the playlist: the playhead is now inside it.
    let c = rig.ws.cue_lists[0].cues.pop().unwrap();
    children_of(&mut rig, 1).push(c);
    rig.tick(2200);
    assert_eq!(rig.playhead(), Some(id(12)));
    rig.go(2300);
    // The group does not run in this visit, so 2 will not fire: the playhead stands on it.
    assert_eq!(rig.playhead(), Some(id(2)));
}

#[test]
fn h1_a_cue_moved_into_the_group_just_before_it_enters_it_although_the_order_is_unchanged() {
    // Found by the property suite: [G [10], 12, 2, 3] → [G [10, 12], 2, 3] keeps every cue's
    // place in the list's order, but the playhead on 12 is now inside G.
    let g = timed(grp(1, GroupMode::Timeline, vec![memo(10)]), 0.0, 0.0, AC);
    let mut rig = Rig::new(vec![g, memo(12), memo(2), memo(3)]);
    let go = rig.go(0);
    // G completes at once and its auto-continue starts 12.
    assert!(starts(&go).contains(&(id(12), 0)));
    rig.at(
        100,
        Input::SetPlayhead {
            list: list(),
            cue: Some(id(12)),
        },
    );
    let c = rig.ws.cue_lists[0].cues.remove(1);
    children_of(&mut rig, 1).push(c);
    rig.tick(200);
    rig.go(300);
    // A fresh visit: G's continue does not fire now, so the playhead stands on 2.
    assert_eq!(rig.playhead(), Some(id(2)));
}
