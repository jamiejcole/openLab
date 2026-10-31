// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! GO on a group's child, the playhead after groups, broken and failing children, and the
//! auto-follow timing of a halted playlist, where QLab 5 was measured (answers of 2026-10-27) and
//! openLab now does what QLab does. Virtual sample clock at 1 kHz: one second is 1000 samples.

use crate::common::*;
use openlab_core::*;
use proptest::prelude::*;

use ContinueMode::{AutoContinue as AC, AutoFollow as AF};

/// An audio cue with no media file: it has static problems (`no_media`), so it shows as broken.
fn broken_sound(n: u128) -> Cue {
    let mut c = sound(n);
    if let CueKind::Audio { media, .. } = &mut c.kind {
        *media = None;
    }
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

// ----- G1: a random group's chosen child keeps its own continue mode -----

#[test]
fn random_child_keeps_its_own_continue_mode() {
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
fn playlist_skips_a_broken_child_and_still_auto_follows_out_of_the_group() {
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
    rig.set_playhead(0, 11);
    rig
}

#[test]
fn playlist_go_on_a_child_plays_it_and_follows_through_the_rest() {
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
fn timeline_go_on_a_child_plays_only_that_child_and_the_group_does_not_run() {
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
fn playhead_after_go_on_a_child_of_a_stopped_group_follows_the_mode() {
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
fn a_group_with_a_broken_child_shows_broken_and_can_still_be_goed() {
    let p = grp(1, playlist(), vec![broken_sound(10), wait(11, 1.0)]);
    let mut rig = Rig {
        ws: workspace(vec![p, memo(2)]),
        engine: CueEngine::new(EngineConfig::new(RATE, 0, 1).unwrap()),
        last: None,
        check_cache: false,
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
fn go_on_a_devamp_with_start_next_moves_the_playhead_onto_that_cue() {
    let devamp = cue(
        2,
        CueKind::Devamp {
            target: Some(id(1)),
            start_next_cue: true,
            stop_target_when_slice_ends: false,
            devamp_mode: DevampMode::CurrentSlice,
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

// ----- D6: slices and loops are at least 50 ms -----

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
fn a_40_ms_slice_is_refused_and_a_50_ms_loop_plays() {
    assert_eq!(MIN_SLICE_SECONDS, 0.05);

    // 40 ms: the show file is repaired (the marker that ends it moves to make it 50 ms long),
    // and an edit that would make one is refused, as QLab's editor refuses it.
    let short = sliced(0.0, 2.0, &[(1.0, 1), (1.04, 0)]);
    let mut ws = workspace(vec![cue(1, short.clone())]);
    let issues = repair(&mut ws);
    assert!(
        issues.iter().any(|i| i.kind
            == IssueKind::SliceMoved {
                from: 1.04,
                to: 1.0 + MIN_SLICE_SECONDS
            }),
        "{issues:?}"
    );
    assert_eq!(slices_of(&ws), vec![1.0, 1.0 + MIN_SLICE_SECONDS]);
    // Also against the trim points: a marker 40 ms after the start or before the end.
    for (marks, to) in [
        ([(0.54, 1)], 0.5 + MIN_SLICE_SECONDS),
        ([(1.96, 1)], 2.0 - MIN_SLICE_SECONDS),
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

    // 50 ms: accepted everywhere, and it plays (an infinite loop of it).
    let ok = sliced(0.0, 2.0, &[(1.0, 1), (1.05, 0)]);
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
        check_cache: false,
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
            &[frames(1.0), frames(1.05)],
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
fn go_on_a_broken_auto_follow_cue_starts_the_next_cue_and_moves_past_it() {
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
fn two_cues_with_the_same_hotkey_both_start_with_no_warning() {
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

// ----- later review cases -----

#[test]
fn go_on_a_playlist_child_stands_on_the_cue_after_the_group_without_losing_its_continue() {
    // The group never runs, so its auto-follow (2) never fires: the playhead stands on 2.
    let p = timed(
        grp(1, playlist(), vec![wait(10, 1.0), wait(11, 1.0)]),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::new(vec![p, memo(2), memo(3)]);
    rig.set_playhead(0, 10);
    rig.go(0);
    assert_eq!(rig.playhead(), Some(id(2)));
    assert_eq!(starts(&rig.tick(1000)), vec![(id(11), 1000)]);
    assert!(starts(&rig.tick(2000)).is_empty());
    assert_eq!(starts(&rig.go(2100)), vec![(id(2), 2100)]);
    assert_eq!(rig.playhead(), Some(id(3)));
}

#[test]
fn leaving_a_start_first_group_that_never_ran_stands_on_its_continue_target() {
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
    rig.set_playhead(0, 11);
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
fn repair_keeps_a_short_trim_and_the_loops_it_merges() {
    // A 40 ms trim is moved to the 50 ms floor, not cleared (which would play the whole file).
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

    // A short slice is lengthened to 50 ms where there is room, keeping its loop or vamp, so
    // the cue's length barely changes; where there is none, it merges and the merged slice
    // plays as the longer part did. Each repaired show saves and loads back unchanged.
    let m = MIN_SLICE_SECONDS;
    let cases: [(&[Mark], Vec<Mark>, u32); 6] = [
        // A 30 ms stutter (x3) before a long rest of the song: still a 50 ms stutter.
        (&[(1.0, 1), (1.03, 3)], vec![(1.0, 1), (1.0 + m, 3)], 1),
        // A 30 ms vamp: still a vamp, 50 ms long.
        (&[(1.0, 1), (1.03, 0)], vec![(1.0, 1), (1.0 + m, 0)], 1),
        // Consecutive short slices: the first two merge (no room), then that slice is
        // lengthened; the long slice after keeps its single play.
        (&[(0.02, 2), (0.04, 3), (0.5, 1)], vec![(m, 3), (0.5, 1)], 1),
        // Too close to the end: the last marker moves earlier, the vamp before keeps its count.
        (&[(1.0, 1), (1.98, 0)], vec![(1.0, 1), (2.0 - m, 0)], 1),
        // No room at the end either: merged, and the longer part (the vamp) gives the count.
        (&[(1.93, 1), (1.98, 0)], vec![(1.93, 1)], 0),
        // A short loop next to single plays with little room: the single plays win and the
        // loop is not spread over them (the remaining marker is moved to leave 50 ms).
        (
            &[(1.9, 1), (1.92, 4), (1.96, 1)],
            vec![(1.9, 1), (1.95, 1)],
            1,
        ),
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
fn an_auto_follow_cue_whose_stream_fails_before_playing_starts_the_next_cue() {
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
fn a_playlist_skips_a_child_whose_stream_fails_before_playing_and_still_auto_follows() {
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

// ----- later review cases, continued -----

#[test]
fn a_group_that_ran_earlier_does_not_hide_its_continue_from_a_later_go_on_a_child() {
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
fn leaving_a_group_whose_own_run_fired_its_continue_does_not_play_it_twice() {
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
fn a_paused_cue_whose_stream_fails_before_playing_follows_only_on_resume() {
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

// ----- later review cases, last -----

fn children_of(rig: &mut Rig, n: u128) -> &mut Vec<Cue> {
    let cues = &mut rig.ws.cue_lists[0].cues;
    match cues.iter_mut().find(|c| c.id == id(n)).map(|c| &mut c.kind) {
        Some(CueKind::Group { children, .. }) => children,
        _ => panic!("no group {n}"),
    }
}

#[test]
fn deleting_the_standby_cue_inside_a_group_keeps_the_visit_and_plays_nothing_twice() {
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
fn a_standby_cue_dragged_into_a_group_enters_it_afresh() {
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
fn a_cue_moved_into_the_group_just_before_it_enters_it_although_the_order_is_unchanged() {
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

// ----- the QLab checks left open (answers 2026-10-27) -----

/// Tells the engine the nominal length of Audio cues, in seconds, as the binary does from its
/// plans (`None`: unknown or endless).
fn lengths(rig: &mut Rig, now: SampleTime, of: &[(u128, Option<f64>)]) {
    let lengths = of
        .iter()
        .map(|(n, s)| {
            (
                id(*n),
                s.map(|s| (s * f64::from(RATE)).round() as SampleTime),
            )
        })
        .collect();
    rig.at(now, Input::CueLengths { lengths });
}

/// The binary's `ActionLength` for the cue's run, as it sends it when the voice starts.
fn action_length(rig: &mut Rig, now: SampleTime, n: u128, frames: SampleTime) {
    let run = rig.engine.run_of(id(n)).unwrap();
    rig.at(
        now,
        Input::ActionLength {
            cue: id(n),
            run,
            frames,
        },
    );
}

fn failed_while_playing(rig: &mut Rig, now: SampleTime, n: u128) -> Vec<Action> {
    let run = rig.engine.run_of(id(n)).unwrap();
    rig.at(
        now,
        Input::ActionFailed {
            cue: id(n),
            run,
            reason: BrokenReason::StreamFailed,
        },
    )
}

fn stop(rig: &mut Rig, now: SampleTime, n: u128) -> Vec<Action> {
    rig.at(
        now,
        Input::Stop {
            cue: id(n),
            fade_out: None,
        },
    )
}

/// QLab check Q2's setup: an auto-follow playlist of three 10 s sounds (the third given), then
/// cue 2. GO; the first child plays out, the second starts at 10 s and breaks at 15 s.
fn halted_rig(third: Cue, third_length: Option<f64>, loop_: bool) -> (Rig, Vec<Action>) {
    let p = timed(
        grp(
            1,
            GroupMode::Playlist {
                loop_,
                shuffle: false,
            },
            vec![sound(10), sound(11), third],
        ),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::new(vec![p, memo(2)]);
    lengths(
        &mut rig,
        0,
        &[(10, Some(10.0)), (11, Some(10.0)), (12, third_length)],
    );
    rig.go(0);
    action_length(&mut rig, 0, 10, 10_000);
    let next = rig.tick(10_000);
    assert_eq!(starts(&next), vec![(id(11), 10_000)]);
    action_length(&mut rig, 10_000, 11, 10_000);
    let broke = failed_while_playing(&mut rig, 15_000, 11);
    (rig, broke)
}

#[test]
fn a_playlist_child_that_breaks_while_playing_halts_the_playlist_and_still_follows() {
    let (mut rig, broke) = halted_rig(sound(12), Some(10.0), false);
    // QLab: the broken child shows broken and the playlist does not move on.
    assert_eq!(changes(&broke, 11), vec![(CueState::Broken, 15_000)]);
    assert!(starts(&broke).is_empty());
    // The group keeps running to the broken child's nominal end...
    assert_eq!(rig.state(1), CueState::Running);
    assert_eq!(rig.engine.next_deadline(), Some(20_000));
    assert!(rig.tick(19_999).is_empty());
    let end = rig.tick(20_000);
    // ...then its action is over (it waits for its follow as for a post-wait).
    assert_eq!(changes(&end, 1), vec![(CueState::PostWaiting, 20_000)]);
    assert!(starts(&end).is_empty());
    // The cue after the group starts when the group would have ended had every child played its
    // nominal length: 30 s after the group's start, on that exact sample.
    assert_eq!(rig.engine.next_deadline(), Some(30_000));
    assert!(starts(&rig.tick(29_999)).is_empty());
    let follow = rig.tick(30_000);
    assert_eq!(starts(&follow), vec![(id(2), 30_000)]);
    assert_eq!(changes(&follow, 1), vec![(CueState::Idle, 30_000)]);
    // The third child never played.
    assert_eq!(rig.state(12), CueState::Idle);
    assert!(rig.engine.next_deadline().is_none());
}

#[test]
fn the_broken_childs_end_comes_from_its_nominal_length_when_its_start_did_not_tell() {
    // No `ActionLength` for the broken child: its nominal length (`CueLengths`) dates its end
    // from its action start.
    let p = timed(grp(1, playlist(), vec![sound(10), sound(11)]), 0.0, 0.0, AF);
    let mut rig = Rig::new(vec![p, memo(2)]);
    lengths(&mut rig, 0, &[(10, Some(4.0)), (11, Some(3.0))]);
    rig.go(0);
    let broke = failed_while_playing(&mut rig, 1000, 10);
    assert!(starts(&broke).is_empty());
    assert_eq!(rig.engine.next_deadline(), Some(4000));
    assert!(changes(&rig.tick(4000), 1).contains(&(CueState::PostWaiting, 4000)));
    assert_eq!(starts(&rig.tick(7000)), vec![(id(2), 7000)]);
}

#[test]
fn the_follow_counts_the_remaining_childrens_pre_waits_and_any_kind_of_child() {
    // The rest: a Wait with a 2 s pre-wait (2 + 3 s), a disarmed sound (its pre-wait only, 1 s),
    // a memo (0) and a sound with static problems (skipped: its 0.5 s pre-wait only).
    let mut disarmed = timed(sound(13), 1.0, 0.0, ContinueMode::DoNotContinue);
    disarmed.armed = false;
    let p = timed(
        grp(
            1,
            playlist(),
            vec![
                sound(10),
                timed(wait(12, 3.0), 2.0, 0.0, ContinueMode::DoNotContinue),
                disarmed,
                memo(14),
                timed(broken_sound(15), 0.5, 0.0, ContinueMode::DoNotContinue),
            ],
        ),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::new(vec![p, memo(2)]);
    lengths(&mut rig, 0, &[(10, Some(10.0))]);
    rig.go(0);
    action_length(&mut rig, 0, 10, 10_000);
    failed_while_playing(&mut rig, 2000, 10);
    rig.tick(10_000);
    assert_eq!(rig.engine.next_deadline(), Some(16_500));
    assert_eq!(starts(&rig.tick(16_500)), vec![(id(2), 16_500)]);
    for n in [12, 13, 14] {
        assert_eq!(rig.state(n), CueState::Idle);
    }
}

#[test]
fn a_remaining_child_with_no_known_end_cancels_the_follow() {
    // The third child's length is unknown (not probed): the group could never say when it
    // would have ended, so it does not follow (open question in docs/cue-engine.md).
    let (mut rig, broke) = halted_rig(sound(12), None, false);
    assert!(starts(&broke).is_empty());
    let end = rig.tick(20_000);
    assert_eq!(changes(&end, 1), vec![(CueState::Idle, 20_000)]);
    assert!(rig.engine.next_deadline().is_none());
    assert!(starts(&rig.tick(60_000)).is_empty());
    assert_eq!(rig.state(2), CueState::Idle);
}

#[test]
fn a_remaining_group_that_loops_or_chains_by_continues_has_no_known_end() {
    for mode in [
        GroupMode::Playlist {
            loop_: true,
            shuffle: false,
        },
        GroupMode::StartFirstAndGoToNext,
        GroupMode::StartRandom,
    ] {
        let inner = grp(12, mode, vec![wait(20, 1.0)]);
        let (mut rig, _) = halted_rig(inner, None, false);
        rig.tick(20_000);
        assert!(rig.engine.next_deadline().is_none(), "{mode:?}");
    }
    // A timeline or plain playlist inside has one: its longest child, the sum of its children.
    for (mode, end) in [
        (GroupMode::Timeline, 23_000),
        (
            GroupMode::Playlist {
                loop_: false,
                shuffle: false,
            },
            25_000,
        ),
    ] {
        let inner = grp(12, mode, vec![wait(20, 2.0), wait(21, 3.0)]);
        let (mut rig, _) = halted_rig(inner, None, false);
        rig.tick(20_000);
        assert_eq!(rig.engine.next_deadline(), Some(end), "{mode:?}");
    }
}

#[test]
fn a_looping_playlist_that_halts_never_follows() {
    let (mut rig, broke) = halted_rig(sound(12), Some(10.0), true);
    assert!(starts(&broke).is_empty());
    let end = rig.tick(20_000);
    assert_eq!(changes(&end, 1), vec![(CueState::Idle, 20_000)]);
    assert!(rig.engine.next_deadline().is_none());
}

#[test]
fn stop_or_panic_while_the_halted_playlist_waits_cancels_its_follow() {
    // During the rest of the broken child, and during the wait for the follow.
    for at in [17_000, 25_000] {
        let (mut rig, _) = halted_rig(sound(12), Some(10.0), false);
        rig.tick(at.min(20_000));
        let stopped = stop(&mut rig, at, 1);
        assert_eq!(changes(&stopped, 1), vec![(CueState::Idle, at)]);
        assert!(rig.engine.next_deadline().is_none());
        assert!(starts(&rig.tick(30_000)).is_empty());

        let (mut rig, _) = halted_rig(sound(12), Some(10.0), false);
        rig.tick(at.min(20_000));
        rig.at(at, Input::Panic);
        assert_eq!(rig.state(1), CueState::Idle);
        assert!(rig.engine.next_deadline().is_none());
        assert!(starts(&rig.tick(30_000)).is_empty());
    }
}

#[test]
fn a_pause_while_the_halted_playlist_waits_moves_its_follow_by_the_pause() {
    let (mut rig, _) = halted_rig(sound(12), Some(10.0), false);
    rig.at(17_000, Input::Pause { cue: id(1) });
    assert!(rig.tick(20_000).is_empty());
    rig.at(21_000, Input::Resume { cue: id(1) });
    assert_eq!(rig.engine.next_deadline(), Some(24_000));
    rig.tick(24_000);
    rig.at(25_000, Input::Pause { cue: id(1) });
    rig.at(26_000, Input::Resume { cue: id(1) });
    assert_eq!(rig.engine.next_deadline(), Some(35_000));
    assert_eq!(starts(&rig.tick(35_000)), vec![(id(2), 35_000)]);
}

#[test]
fn play_next_on_a_halted_playlist_moves_it_on() {
    // The second trigger "play next" is the operator moving the playlist on: the next child
    // plays, the halt is over, and as a child broke while playing the group does not follow.
    let mut p = timed(
        grp(1, playlist(), vec![sound(10), sound(11), wait(12, 1.0)]),
        0.0,
        0.0,
        AF,
    );
    p.second_trigger = SecondTrigger::PlayNext;
    let mut rig = Rig::new(vec![p, memo(2)]);
    lengths(&mut rig, 0, &[(10, Some(10.0)), (11, Some(10.0))]);
    rig.go(0);
    action_length(&mut rig, 0, 10, 10_000);
    failed_while_playing(&mut rig, 5000, 10);
    let next = rig.start(6000, 1);
    assert_eq!(starts(&next), vec![(id(11), 6000)]);
    action_length(&mut rig, 6000, 11, 10_000);
    rig.tick(16_000);
    let end = rig.tick(17_000);
    assert_eq!(changes(&end, 1), vec![(CueState::Idle, 17_000)]);
    assert!(starts(&end).is_empty());
}

#[test]
fn a_shuffled_playlist_times_its_follow_by_the_rest_of_its_pass() {
    // Lengths 1, 2 and 4 s; the second child of the pass breaks as it starts: the follow comes
    // after its nominal end plus the third child of the pass, whichever that is.
    let ms = |n: u128| -> SampleTime {
        match n {
            10 => 1000,
            11 => 2000,
            _ => 4000,
        }
    };
    let n_of = |c: CueId| (10..13).find(|n| id(*n) == c).unwrap();
    let mut thirds = Vec::new();
    for seed in 1..8 {
        let p = timed(
            grp(
                1,
                GroupMode::Playlist {
                    loop_: false,
                    shuffle: true,
                },
                vec![sound(10), sound(11), sound(12)],
            ),
            0.0,
            0.0,
            AF,
        );
        let mut rig = Rig::seeded(vec![p, memo(2)], seed);
        lengths(
            &mut rig,
            0,
            &[(10, Some(1.0)), (11, Some(2.0)), (12, Some(4.0))],
        );
        let go = rig.go(0);
        let first = n_of(starts(&go)[1].0);
        action_length(&mut rig, 0, first, ms(first));
        let t = ms(first);
        let second = n_of(starts(&rig.tick(t))[0].0);
        let third = (10..13).find(|c| *c != first && *c != second).unwrap();
        thirds.push(third);
        action_length(&mut rig, t, second, ms(second));
        failed_while_playing(&mut rig, t, second);
        let expected = t + ms(second) + ms(third);
        rig.tick(t + ms(second));
        assert_eq!(rig.engine.next_deadline(), Some(expected), "seed {seed}");
        assert_eq!(starts(&rig.tick(expected)), vec![(id(2), expected)]);
        assert_eq!(rig.state(third), CueState::Idle);
    }
    // The pass order, not the list order, decided which child was left.
    assert!(thirds.iter().any(|t| *t != 12), "{thirds:?}");
}

#[test]
fn a_child_that_fails_before_playing_is_still_skipped_with_lengths_known() {
    // `StartFailed` (no audio yet) is unchanged: skipped, the playlist moves on at once.
    let p = timed(
        grp(1, playlist(), vec![sound(10), wait(11, 0.5)]),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::new(vec![p, memo(2)]);
    lengths(&mut rig, 0, &[(10, Some(10.0))]);
    rig.go(0);
    let failed = failed_before_playing(&mut rig, 50, 10);
    assert_eq!(starts(&failed), vec![(id(11), 50)]);
    assert_eq!(starts(&rig.tick(550)), vec![(id(2), 550)]);
}

#[test]
fn a_single_cue_or_a_timeline_child_that_breaks_while_playing_still_cancels_the_follow() {
    // Rule 2 is unchanged outside playlists (QLab-confirmed earlier).
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 0.0, AF), memo(2)]);
    lengths(&mut rig, 0, &[(1, Some(1.0))]);
    rig.go(0);
    action_length(&mut rig, 0, 1, 1000);
    assert!(starts(&failed_while_playing(&mut rig, 500, 1)).is_empty());
    assert!(starts(&rig.tick(5000)).is_empty());

    let t = timed(
        grp(1, GroupMode::Timeline, vec![sound(10), wait(11, 1.0)]),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::new(vec![t, memo(2)]);
    lengths(&mut rig, 0, &[(10, Some(1.0))]);
    rig.go(0);
    failed_while_playing(&mut rig, 500, 10);
    let end = rig.tick(1000);
    assert_eq!(changes(&end, 1), vec![(CueState::Idle, 1000)]);
    assert!(starts(&end).is_empty());
}

// ----- Q4: a playlist played on from a child started on its own -----

fn follow_through_rig(loop_: bool, shuffle: bool) -> Rig {
    let g = grp(
        1,
        GroupMode::Playlist { loop_, shuffle },
        vec![wait(10, 1.0), wait(11, 1.0), wait(12, 1.0)],
    );
    let mut rig = Rig::new(vec![g, memo(2)]);
    rig.set_playhead(0, 11);
    rig
}

#[test]
fn follow_through_with_loop_wraps_to_the_first_child_and_keeps_going() {
    for shuffle in [false, true] {
        let mut rig = follow_through_rig(true, shuffle);
        let mut all = rig.go(0);
        for t in 1..=7 {
            all.extend(rig.tick(t * 1000));
        }
        // List order, also with shuffle on (QLab), round and round; the group never runs.
        let expected: Vec<(CueId, SampleTime)> = [11, 12, 10, 11, 12, 10, 11, 12]
            .iter()
            .zip(0..)
            .map(|(n, t)| (id(*n), t * 1000))
            .collect();
        assert_eq!(starts(&all), expected, "shuffle {shuffle}");
        assert!(changes(&all, 1).is_empty());
        assert_eq!(rig.state(2), CueState::Idle);
        // Stop on the group (it reaches the playing child) ends it.
        stop(&mut rig, 7500, 1);
        assert!(rig.engine.next_deadline().is_none());
        assert!(starts(&rig.tick(9000)).is_empty());
    }
}

#[test]
fn panic_ends_a_looping_follow_through() {
    let mut rig = follow_through_rig(true, false);
    rig.go(0);
    rig.tick(1000);
    rig.tick(2000);
    rig.at(2500, Input::Panic);
    assert!(rig.engine.next_deadline().is_none());
    assert!(starts(&rig.tick(10_000)).is_empty());
}

#[test]
fn follow_through_without_loop_ends_after_the_last_child() {
    let mut rig = follow_through_rig(false, true);
    let mut all = rig.go(0);
    for t in 1..=4 {
        all.extend(rig.tick(t * 1000));
    }
    assert_eq!(starts(&all), vec![(id(11), 0), (id(12), 1000)]);
    assert!(rig.engine.next_deadline().is_none());
}

#[test]
fn a_looping_follow_through_of_zero_length_children_is_bounded_by_the_chain_budget() {
    let g = grp(
        1,
        GroupMode::Playlist {
            loop_: true,
            shuffle: false,
        },
        vec![memo(10), memo(11)],
    );
    let mut rig = Rig::new(vec![g, memo(2)]);
    let out = rig.start(0, 10);
    assert!(
        warnings(&out)
            .iter()
            .any(|w| matches!(w, EngineWarning::ChainLimit { .. })),
        "{out:?}"
    );
    assert!(starts(&out).len() > 2);
    // Cut, not spinning: nothing left to do.
    assert!(rig.engine.next_deadline().is_none());
}

#[test]
fn a_looping_follow_through_passes_over_children_that_cannot_start_and_wraps() {
    // A child broken at run time is passed over on every round; the lone playable child plays
    // round and round.
    let g = grp(
        1,
        GroupMode::Playlist {
            loop_: true,
            shuffle: false,
        },
        vec![sound(10), wait(11, 1.0)],
    );
    let mut rig = Rig::new(vec![g, memo(2)]);
    rig.start(0, 10);
    failed_while_playing(&mut rig, 100, 10);
    // The failure followed through to 11 (the group has no run of its own).
    assert_eq!(rig.state(11), CueState::Running);
    let mut all = Vec::new();
    for t in [1100, 2100, 3100] {
        all.extend(rig.tick(t));
    }
    assert_eq!(
        starts(&all),
        vec![(id(11), 1100), (id(11), 2100), (id(11), 3100)]
    );
}

// ----- a running cue moved into a playlist that did not start it -----

#[test]
fn a_running_cue_moved_into_an_idle_playlist_starts_none_of_its_children_when_it_ends() {
    let p = grp(1, playlist(), vec![wait(10, 1.0), wait(11, 1.0)]);
    let mut rig = Rig::new(vec![p, wait(3, 1.0)]);
    rig.start(0, 3);
    // The operator drags the running Wait to the front of the idle playlist.
    let c = rig.ws.cue_lists[0].cues.remove(1);
    children_of(&mut rig, 1).insert(0, c);
    rig.tick(200);
    let end = rig.tick(1000);
    assert_eq!(rig.state(3), CueState::Idle);
    // The playlist never started it, so its end carries nothing on.
    assert!(starts(&end).is_empty(), "{:?}", starts(&end));
    assert!(starts(&rig.tick(3000)).is_empty());
    assert_eq!(rig.state(10), CueState::Idle);
    assert_eq!(rig.state(11), CueState::Idle);
}

#[test]
fn a_moved_cues_end_reported_after_pause_all_is_not_held_for_the_idle_playlist() {
    let p = grp(1, playlist(), vec![wait(10, 1.0), wait(11, 1.0)]);
    let mut rig = Rig::new(vec![p, sound(3)]);
    rig.start(0, 3);
    let c = rig.ws.cue_lists[0].cues.remove(1);
    children_of(&mut rig, 1).insert(0, c);
    rig.tick(200);
    rig.at(500, Input::PauseAll);
    // Its end, dated before the pause, arrives after it: it starts nothing, so nothing is held.
    rig.ended(600, 3, 450);
    assert_eq!(rig.state(3), CueState::Idle);
    let mut all = rig.at(700, Input::ResumeAll);
    all.extend(rig.tick(3000));
    assert!(starts(&all).is_empty(), "{:?}", starts(&all));
}

#[test]
fn a_held_failed_playlist_child_moved_into_an_idle_playlist_starts_nothing_there_on_resume() {
    let playing = grp(1, playlist(), vec![sound(20), sound(21)]);
    let idle = grp(5, playlist(), vec![wait(10, 1.0), wait(11, 1.0)]);
    let mut rig = Rig::new(vec![playing, idle]);
    rig.go(0);
    assert_eq!(rig.state(20), CueState::Running);
    rig.at(100, Input::PauseAll);
    // Its stream fails while Pause All holds: the end is held for the resume (QLab 5.5.3).
    failed_while_playing(&mut rig, 200, 20);
    assert_eq!(rig.state(20), CueState::Paused);
    // The operator drags it to the front of the idle playlist, then resumes.
    let c = children_of(&mut rig, 1).remove(0);
    children_of(&mut rig, 5).insert(0, c);
    rig.tick(300);
    let mut all = rig.at(400, Input::ResumeAll);
    all.extend(rig.tick(3000));
    for n in [10, 11] {
        assert!(
            !starts(&all).iter().any(|(c, _)| *c == id(n)),
            "{n} started: {:?}",
            starts(&all)
        );
        assert_eq!(rig.state(n), CueState::Idle);
    }
}

// ----- Q2 against a reference: the follow of a halted playlist -----

/// One child of the playlist the property builds: a sound of `len` ms or a Wait of `len` ms,
/// with a pre-wait, armed or not.
#[derive(Clone, Debug)]
struct Kid {
    sound: bool,
    len: u64,
    pre: u64,
    armed: bool,
}

/// Plays an auto-follow playlist of `kids` (then cue 2) from a GO at 0, as the binary would:
/// every voice tells its length as it starts, and child `broken` fails `after` ms into its
/// action. Returns every action.
fn play_halted(kids: &[Kid], broken: usize, after: u64) -> (Rig, Vec<Action>) {
    let children: Vec<Cue> = kids
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let n = 10 + i as u128;
            let base = if k.sound {
                sound(n)
            } else {
                wait(n, k.len as f64 / 1000.0)
            };
            let mut c = timed(
                base,
                k.pre as f64 / 1000.0,
                0.0,
                ContinueMode::DoNotContinue,
            );
            c.armed = k.armed;
            c
        })
        .collect();
    let p = timed(grp(1, playlist(), children), 0.0, 0.0, AF);
    let mut rig = Rig::new(vec![p, memo(2)]);
    let known: Vec<(u128, Option<f64>)> = kids
        .iter()
        .enumerate()
        .filter(|(_, k)| k.sound)
        .map(|(i, k)| (10 + i as u128, Some(k.len as f64 / 1000.0)))
        .collect();
    lengths(&mut rig, 0, &known);
    let mut all = Vec::new();
    let mut out = rig.go(0);
    let mut fail_at = None;
    for _ in 0..200 {
        for a in &out {
            if let Action::StartVoice { cue, at, .. } = a {
                let i = (10..10 + kids.len() as u128)
                    .position(|n| id(n) == *cue)
                    .unwrap();
                let now = *at;
                action_length(&mut rig, now, 10 + i as u128, kids[i].len);
                if i == broken {
                    fail_at = Some(at + after);
                }
            }
        }
        all.extend(out);
        let next = match (rig.engine.next_deadline(), fail_at) {
            (Some(d), Some(f)) => Some(d.min(f)),
            (d, f) => d.or(f),
        };
        let Some(next) = next else {
            break;
        };
        out = if fail_at == Some(next) {
            fail_at = None;
            failed_while_playing(&mut rig, next, 10 + broken as u128)
        } else {
            rig.tick(next)
        };
    }
    (rig, all)
}

fn kid() -> impl Strategy<Value = Kid> {
    (
        any::<bool>(),
        1u64..3000,
        prop_oneof![0u64..1, 1u64..800],
        0u8..8,
    )
        .prop_map(|(sound, len, pre, armed)| Kid {
            sound,
            len,
            pre,
            armed: armed != 0,
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// QLab check Q2: whichever child breaks while playing and whatever follows it, the
    /// playlist starts no further child, its action ends on the broken child's nominal end, and
    /// the cue after the group starts on the sample the group would have ended on had every
    /// child played its nominal length (each pre-wait plus each armed child's length).
    #[test]
    fn q2_a_halted_playlist_follows_when_its_nominal_run_would_have_ended(
        mut kids in prop::collection::vec(kid(), 1..6),
        broken in any::<prop::sample::Index>(),
        after in any::<prop::sample::Index>(),
    ) {
        let b = broken.index(kids.len());
        kids[b].sound = true;
        kids[b].armed = true;
        kids[b].len = kids[b].len.max(2);
        let after = 1 + after.index(usize::try_from(kids[b].len - 1).unwrap()) as u64;
        // The reference: the nominal schedule of the whole playlist.
        let span = |k: &Kid| k.pre + if k.armed { k.len } else { 0 };
        let action_start: u64 = kids[..b].iter().map(span).sum::<u64>() + kids[b].pre;
        let end = action_start + kids[b].len;
        let follow = end + kids[b + 1..].iter().map(span).sum::<u64>();

        let (rig, all) = play_halted(&kids, b, after);
        let started: Vec<(CueId, SampleTime)> = starts(&all);
        for later in b + 1..kids.len() {
            prop_assert!(
                started.iter().all(|(c, _)| *c != id(10 + later as u128)),
                "child {later} started after the break: {started:?}"
            );
        }
        prop_assert_eq!(
            started.iter().copied().filter(|(c, _)| *c == id(2)).collect::<Vec<_>>(),
            vec![(id(2), follow)]
        );
        let group: Vec<(CueState, SampleTime)> = changes(&all, 1);
        let ended = group
            .iter()
            .find(|(s, _)| matches!(s, CueState::PostWaiting | CueState::Idle))
            .map(|(_, at)| *at);
        prop_assert_eq!(ended, Some(end), "{:?}", group);
        prop_assert_eq!(rig.state(1), CueState::Idle);
        prop_assert!(rig.engine.next_deadline().is_none());
    }
}

// ----- Q1, Q3, Q5: QLab-confirmed as openLab had them (regression tests) -----

#[test]
fn go_on_a_child_or_on_a_stopped_auto_follow_playlist_moves_the_playhead_as_qlab() {
    let rig_on = |n: u128| {
        let p = timed(
            grp(1, playlist(), vec![wait(10, 1.0), wait(11, 1.0)]),
            0.0,
            0.0,
            AF,
        );
        let mut rig = Rig::new(vec![p, memo(2), memo(3)]);
        rig.set_playhead(0, n);
        rig
    };
    // GO on child 10: 10 then 11 play, the playhead goes to 2, and 2 does not start.
    let mut rig = rig_on(10);
    let mut all = rig.go(0);
    all.extend(rig.tick(1000));
    all.extend(rig.tick(2000));
    assert_eq!(starts(&all), vec![(id(10), 0), (id(11), 1000)]);
    assert_eq!(rig.playhead(), Some(id(2)));
    assert_eq!(rig.state(2), CueState::Idle);
    // GO on the playlist: the playhead goes to 3, and 2 auto-follows the group.
    let mut rig = rig_on(1);
    rig.go(0);
    assert_eq!(rig.playhead(), Some(id(3)));
    rig.tick(1000);
    assert_eq!(starts(&rig.tick(2000)), vec![(id(2), 2000)]);
    // GO on child 11: only 11 plays (it is the last), the playhead goes to 2.
    let mut rig = rig_on(11);
    let mut all = rig.go(0);
    all.extend(rig.tick(1000));
    assert_eq!(starts(&all), vec![(id(11), 0)]);
    assert_eq!(rig.playhead(), Some(id(2)));
}

#[test]
fn go_on_a_child_of_a_stopped_random_or_start_first_group_plays_only_that_child() {
    for mode in [
        GroupMode::StartRandom,
        GroupMode::StartFirstAndGoToNext,
        GroupMode::StartFirstAndEnter,
    ] {
        let g = grp(1, mode, vec![wait(10, 1.0), wait(11, 1.0), wait(12, 1.0)]);
        let mut rig = Rig::new(vec![g, memo(2)]);
        rig.set_playhead(0, 10);
        let mut all = rig.go(0);
        all.extend(rig.tick(1000));
        assert_eq!(starts(&all), vec![(id(10), 0)], "{mode:?}");
        assert_eq!(rig.playhead(), Some(id(11)), "{mode:?}");
        assert_eq!(rig.state(1), CueState::Idle, "{mode:?}");
    }
}

#[test]
fn a_child_that_ends_during_its_playlists_post_wait_does_not_carry_the_playlist_on() {
    // The playlist runs its children (0–1 s) and then post-waits to 3 s; a child started on
    // its own at 1.5 s ends at 2 s, inside that post-wait: nothing follows it.
    let p = timed(
        grp(1, playlist(), vec![wait(10, 0.5), wait(11, 0.5)]),
        0.0,
        3.0,
        AC,
    );
    let mut rig = Rig::new(vec![p, memo(2)]);
    rig.go(0);
    rig.tick(500);
    rig.tick(1000);
    assert_eq!(rig.state(1), CueState::PostWaiting);
    rig.start(1500, 10);
    let end = rig.tick(2000);
    assert!(starts(&end).is_empty());
    assert_eq!(rig.state(11), CueState::Idle);
    assert_eq!(starts(&rig.tick(3000)), vec![(id(2), 3000)]);
}

#[test]
fn go_from_into_a_groups_child_clears_the_visit_memory_as_set_playhead_then_go_does() {
    // Group visits: a group whose continue fired is remembered for the playhead's visit.
    // `GoFrom` (OSC `/go "3"`) moves the playhead through the same code as `SetPlayhead`, so the
    // result must be the same as `SetPlayhead` then `Go`.
    let scenario = |from: bool| {
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
        // The group's continue fires (2 starts) while the playhead is inside it.
        assert!(starts(&rig.tick(1000)).contains(&(id(2), 1000)));
        let out = if from {
            rig.at(
                1100,
                Input::GoFrom {
                    list: list(),
                    cue: id(12),
                },
            )
        } else {
            rig.at(
                1100,
                Input::SetPlayhead {
                    list: list(),
                    cue: Some(id(12)),
                },
            );
            rig.go(1100)
        };
        (starts(&out), rig.playhead())
    };
    let by_set = scenario(false);
    assert_eq!(scenario(true), by_set);
    // Child 12 starts; 2 already played in this visit and is passed over: the playhead stands on 3.
    assert_eq!(by_set, (vec![(id(12), 1100)], Some(id(3))));
}
