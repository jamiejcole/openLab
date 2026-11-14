// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! The Basics and Triggers tabs' cue fields in the engine (CUE-01, CUE-07, CUE-09): auto-load,
//! skip if disarmed, the second colour's "played" state and the hotkey trigger's switch. Virtual
//! sample clock at 1 kHz: one second is 1000 samples.

use crate::common::*;
use openlab_core::*;

use ContinueMode::{AutoContinue as AC, AutoFollow as AF, DoNotContinue as DNC};

fn grp(n: u128, mode: GroupMode, children: Vec<Cue>) -> Cue {
    cue(n, CueKind::Group { mode, children })
}

fn auto_loading(mut c: Cue) -> Cue {
    c.auto_load = true;
    c
}

/// Disarmed with "skip if disarmed".
fn skipped(mut c: Cue) -> Cue {
    c.armed = false;
    c.skip_if_disarmed = true;
    c
}

fn loads(actions: &[Action]) -> Vec<CueId> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::LoadVoice { cue, .. } => Some(*cue),
            _ => None,
        })
        .collect()
}

fn played(actions: &[Action]) -> Vec<(CueId, bool)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::PlayedChanged { cue, played } => Some((*cue, *played)),
            _ => None,
        })
        .collect()
}

fn started(actions: &[Action]) -> Vec<CueId> {
    starts(actions).into_iter().map(|(c, _)| c).collect()
}

// ----- auto-load -----

#[test]
fn auto_load_is_ticked_on_cue_2_go_on_cue_1_shows_cue_2_loaded() {
    let mut rig = Rig::new(vec![sound(1), auto_loading(sound(2))]);
    let out = rig.go(0);
    assert_eq!(rig.state(1), CueState::Running);
    assert_eq!(
        rig.state(2),
        CueState::Loaded,
        "loaded without the user loading it"
    );
    assert_eq!(loads(&out), vec![id(2)], "its stream is prepared");
    assert!(changes(&out, 2).contains(&(CueState::Loaded, 0)));
    // The next GO plays the loaded run: no second load.
    let loaded = rig.run(2);
    let out = rig.go(100);
    assert!(loads(&out).is_empty());
    assert_eq!(voice_starts(&out), vec![(id(2), 100)]);
    assert_eq!(rig.run(2), loaded);
}

#[test]
fn without_auto_load_the_next_cue_stays_idle() {
    let mut rig = Rig::new(vec![sound(1), sound(2)]);
    let out = rig.go(0);
    assert_eq!(rig.state(2), CueState::Idle);
    assert!(loads(&out).is_empty());
}

#[test]
fn auto_load_skips_a_next_cue_that_is_broken_disarmed_or_already_loaded_or_running() {
    // Broken (no file): left broken, no load and no warning.
    let mut broken = auto_loading(sound(2));
    if let CueKind::Audio { media, .. } = &mut broken.kind {
        *media = None;
    }
    let mut rig = Rig::new(vec![sound(1), broken]);
    let out = rig.go(0);
    assert_eq!(rig.state(2), CueState::Broken);
    assert!(loads(&out).is_empty());
    assert!(warnings(&out).is_empty(), "{:?}", warnings(&out));
    // Disarmed: a disarmed cue plays nothing, so it is not loaded.
    let mut disarmed = auto_loading(sound(2));
    disarmed.armed = false;
    let mut rig = Rig::new(vec![sound(1), disarmed]);
    rig.go(0);
    assert_eq!(rig.state(2), CueState::Idle);
    // Already running: it plays on.
    let mut rig = Rig::new(vec![sound(1), auto_loading(sound(2))]);
    rig.start(0, 2);
    let run = rig.run(2);
    let out = rig.go(10);
    assert_eq!(rig.state(2), CueState::Running);
    assert_eq!(rig.run(2), run);
    assert!(loads(&out).is_empty());
    // Already loaded (to a time): its load is kept, not redone at 0.
    let mut rig = Rig::new(vec![sound(1), auto_loading(sound(2))]);
    rig.at(
        0,
        Input::Load {
            cue: id(2),
            time: 3.0,
        },
    );
    let run = rig.run(2);
    let out = rig.go(10);
    assert!(loads(&out).is_empty());
    assert_eq!(rig.run(2), run);
}

#[test]
fn auto_load_of_a_group_loads_it_as_load_does() {
    let g = auto_loading(grp(2, GroupMode::Timeline, vec![sound(21), sound(22)]));
    let mut rig = Rig::new(vec![sound(1), g, sound(3)]);
    let out = rig.go(0);
    assert_eq!(rig.state(2), CueState::Loaded);
    assert_eq!(rig.state(21), CueState::Loaded);
    assert_eq!(rig.state(22), CueState::Loaded);
    assert_eq!(loads(&out), vec![id(21), id(22)]);
}

#[test]
fn auto_load_passes_over_a_skipped_cue_and_ignores_cues_at_other_levels() {
    let mut rig = Rig::new(vec![
        sound(1),
        skipped(auto_loading(sound(2))),
        auto_loading(sound(3)),
    ]);
    rig.go(0);
    assert_eq!(rig.state(2), CueState::Idle);
    assert_eq!(
        rig.state(3),
        CueState::Loaded,
        "the cue after the skipped one"
    );
    // A cue at another level is not where the GO leaves the playhead: not loaded.
    let g = grp(2, GroupMode::Timeline, vec![auto_loading(sound(21))]);
    let mut rig = Rig::new(vec![sound(1), g]);
    rig.go(0);
    assert_eq!(rig.state(21), CueState::Idle);
}

/// Measured in QLab 5.6.5: the auto-load cue loads as a GO moves the playhead onto it. Placing
/// the playhead on it (or on the cue before it) by hand, or starting the cue before it without a
/// GO, loads nothing.
#[test]
fn auto_load_happens_when_a_go_moves_the_playhead_onto_the_cue_and_at_no_other_time() {
    let mut rig = Rig::new(vec![sound(1), auto_loading(sound(2)), sound(3)]);
    rig.set_playhead(0, 2);
    assert_eq!(rig.state(2), CueState::Idle, "placed on it by hand");
    rig.set_playhead(0, 1);
    assert_eq!(rig.state(2), CueState::Idle, "placed before it by hand");
    let out = rig.start(10, 1);
    assert_eq!(rig.state(1), CueState::Running);
    assert_eq!(rig.state(2), CueState::Idle, "1 started without a GO");
    assert!(loads(&out).is_empty());
    let out = rig.go(20);
    assert_eq!(rig.playhead(), Some(id(2)));
    assert_eq!(
        loads(&out),
        vec![id(2)],
        "the GO moved the playhead onto it"
    );
    assert_eq!(rig.state(2), CueState::Loaded);
}

#[test]
fn a_go_on_a_skipped_cue_auto_loads_the_cue_the_playhead_moves_onto() {
    let mut rig = Rig::new(vec![
        memo(1),
        skipped(memo(2)),
        auto_loading(sound(3)),
        sound(4),
    ]);
    rig.set_playhead(0, 2);
    let out = rig.go(0);
    assert_eq!(rig.playhead(), Some(id(3)));
    assert!(started(&out).is_empty(), "the GO only moves the playhead");
    assert_eq!(loads(&out), vec![id(3)]);
}

// ----- skip if disarmed -----

#[test]
fn a_disarmed_cue_with_skip_if_disarmed_in_an_auto_continue_chain_is_passed_over() {
    // 1 auto-continues after 0.5 s; 2 is skipped (its own auto-follow must not fire); 3 is
    // the next cue that is there.
    let mut rig = Rig::new(vec![
        timed(memo(1), 0.0, 0.5, AC),
        skipped(timed(wait(2, 1.0), 0.0, 0.0, AF)),
        timed(wait(3, 1.0), 0.0, 0.0, DNC),
        memo(4),
    ]);
    let mut out = rig.go(0);
    assert_eq!(rig.playhead(), Some(id(4)), "the chain is 1 then 3");
    out.extend(rig.tick(500));
    out.extend(rig.tick(2000));
    assert_eq!(started(&out), vec![id(1), id(3)]);
    assert!(changes(&out, 2).is_empty(), "2 never runs: {out:?}");
    assert_eq!(starts(&out)[1], (id(3), 500));
    assert_eq!(rig.state(4), CueState::Idle, "3 does not continue");
}

#[test]
fn the_same_cue_armed_ignores_skip_if_disarmed() {
    let mut two = skipped(timed(wait(2, 1.0), 0.0, 0.0, DNC));
    two.armed = true;
    let mut rig = Rig::new(vec![timed(memo(1), 0.0, 0.5, AC), two, memo(3)]);
    let mut out = rig.go(0);
    out.extend(rig.tick(500));
    assert_eq!(started(&out), vec![id(1), id(2)]);
    assert_eq!(rig.playhead(), Some(id(3)));
    // Disarmed without the box: it is still triggered (its timing kept, its action skipped).
    let mut two = timed(wait(2, 1.0), 0.0, 0.0, DNC);
    two.armed = false;
    let mut rig = Rig::new(vec![timed(memo(1), 0.0, 0.5, AC), two, memo(3)]);
    let mut out = rig.go(0);
    out.extend(rig.tick(500));
    assert_eq!(started(&out), vec![id(1), id(2)]);
}

#[test]
fn a_skipped_cue_at_the_end_of_the_list_ends_the_chain() {
    let mut rig = Rig::new(vec![timed(memo(1), 0.0, 0.0, AC), skipped(memo(2))]);
    let out = rig.go(0);
    assert_eq!(started(&out), vec![id(1)]);
    assert_eq!(rig.playhead(), None, "past the end");
}

#[test]
fn a_skipped_cue_as_an_auto_follow_target_passes_the_follow_on() {
    let mut rig = Rig::new(vec![
        timed(wait(1, 0.1), 0.0, 0.0, AF),
        skipped(memo(2)),
        skipped(memo(3)),
        wait(4, 1.0),
    ]);
    let mut out = rig.go(0);
    out.extend(rig.tick(100));
    assert_eq!(starts(&out), vec![(id(1), 0), (id(4), 100)]);
}

/// Measured in QLab 5.6.5: with the playhead put by hand on a skipped cue, GO starts nothing and
/// the playhead moves on to the next cue that is there, which that GO does not start.
#[test]
fn go_with_the_playhead_on_a_skipped_cue_starts_nothing_and_moves_the_playhead_on() {
    let mut rig = Rig::new(vec![
        memo(1),
        skipped(memo(2)),
        skipped(memo(3)),
        timed(memo(4), 0.0, 0.0, AC),
        memo(5),
        memo(6),
    ]);
    rig.set_playhead(0, 2);
    let out = rig.go(0);
    assert!(started(&out).is_empty(), "{out:?}");
    assert_eq!(rig.playhead(), Some(id(4)), "on the next cue that is there");
    let out = rig.go(10);
    assert_eq!(
        started(&out),
        vec![id(4), id(5)],
        "the next GO starts 4 and the cue it continues into"
    );
    assert_eq!(rig.playhead(), Some(id(6)));
    // A trigger of its own (a Start cue, a hotkey) starts nothing.
    let out = rig.start(10, 2);
    assert!(out.is_empty(), "{out:?}");
    assert_eq!(rig.state(2), CueState::Idle);
}

#[test]
fn a_show_whose_first_cue_is_skipped_plays_its_next_cue_on_the_first_go_and_after_reset_all() {
    let mut rig = Rig::new(vec![skipped(memo(1)), wait(2, 1.0), memo(3)]);
    assert_eq!(rig.playhead(), Some(id(1)), "the playhead may rest on it");
    let out = rig.go(0);
    assert!(
        started(&out).is_empty(),
        "the first GO only moves the playhead on"
    );
    assert_eq!(rig.playhead(), Some(id(2)));
    let out = rig.go(5);
    assert_eq!(started(&out), vec![id(2)]);
    assert_eq!(rig.playhead(), Some(id(3)));
    rig.at(10, Input::ResetAll);
    assert_eq!(rig.playhead(), Some(id(1)));
    let out = rig.go(20);
    assert!(started(&out).is_empty());
    assert_eq!(rig.playhead(), Some(id(2)));
}

#[test]
fn a_cue_disarmed_while_standing_by_does_not_make_the_go_dead() {
    let mut rig = Rig::new(vec![memo(1), wait(2, 1.0), wait(3, 1.0), memo(4)]);
    rig.set_playhead(0, 2);
    let two = rig.find_mut(2);
    two.armed = false;
    two.skip_if_disarmed = true;
    rig.engine.model_changed();
    let out = rig.go(10);
    assert!(started(&out).is_empty(), "nothing starts");
    assert_eq!(
        rig.playhead(),
        Some(id(3)),
        "the GO is not dead: the playhead moves on"
    );
    let out = rig.go(20);
    assert_eq!(started(&out), vec![id(3)]);
    assert_eq!(rig.playhead(), Some(id(4)));
}

#[test]
fn go_on_a_skipped_cue_followed_only_by_skipped_cues_does_what_go_past_the_end_does() {
    let mut rig = Rig::new(vec![memo(1), skipped(memo(2)), skipped(memo(3))]);
    rig.set_playhead(0, 2);
    let out = rig.go(0);
    assert!(started(&out).is_empty());
    assert!(out.contains(&Action::GoIgnored {
        list: list(),
        reason: GoIgnoredReason::NoCue
    }));
}

#[test]
fn go_on_a_skipped_child_starts_the_next_child_that_is_there_in_every_group_mode() {
    for mode in [
        GroupMode::Timeline,
        GroupMode::StartFirstAndEnter,
        GroupMode::StartFirstAndGoToNext,
        GroupMode::Playlist {
            loop_: false,
            shuffle: false,
        },
        GroupMode::StartRandom,
    ] {
        let mut rig = Rig::new(vec![
            grp(1, mode, vec![memo(11), skipped(memo(12)), wait(13, 1.0)]),
            memo(2),
        ]);
        rig.set_playhead(0, 12);
        let out = rig.go(0);
        assert!(started(&out).is_empty(), "{mode:?}: {out:?}");
        assert_eq!(rig.playhead(), Some(id(13)), "{mode:?}");
        let out = rig.go(5);
        assert_eq!(started(&out)[0], id(13), "{mode:?}: {out:?}");
        // Past the last child that is there, the GO leaves the group.
        let mut rig = Rig::new(vec![
            grp(1, mode, vec![memo(11), skipped(memo(12))]),
            wait(2, 1.0),
        ]);
        rig.set_playhead(0, 12);
        let out = rig.go(0);
        assert!(started(&out).is_empty(), "{mode:?}");
        assert_eq!(rig.playhead(), Some(id(2)), "{mode:?}");
    }
}

#[test]
fn go_on_a_skipped_cue_keeps_the_pre_wait_of_the_next_cue_that_is_there() {
    let mut rig = Rig::new(vec![
        skipped(memo(1)),
        timed(wait(2, 1.0), 0.5, 0.0, DNC),
        memo(3),
    ]);
    rig.go(0);
    assert_eq!(rig.playhead(), Some(id(2)));
    let out = rig.go(0);
    assert_eq!(changes(&out, 2), vec![(CueState::PreWaiting, 0)]);
    assert_eq!(rig.playhead(), Some(id(3)));
    let out = rig.tick(500);
    assert_eq!(changes(&out, 2), vec![(CueState::Running, 500)]);
}

#[test]
fn a_skipped_child_is_passed_over_in_every_group_mode() {
    // Timeline: the others start, the skipped one does not; the group ends with the others.
    let mut rig = Rig::new(vec![grp(
        1,
        GroupMode::Timeline,
        vec![wait(11, 0.1), skipped(wait(12, 5.0)), wait(13, 0.2)],
    )]);
    let mut out = rig.start(0, 1);
    out.extend(rig.tick(200));
    assert_eq!(started(&out), vec![id(1), id(11), id(13)]);
    assert_eq!(rig.state(1), CueState::Idle, "the group ended at 0.2 s");
    // Start first and enter: the first child that is there starts.
    for mode in [
        GroupMode::StartFirstAndEnter,
        GroupMode::StartFirstAndGoToNext,
    ] {
        let mut rig = Rig::new(vec![
            grp(1, mode, vec![skipped(memo(11)), wait(12, 1.0), memo(13)]),
            memo(2),
        ]);
        let out = rig.go(0);
        assert_eq!(started(&out), vec![id(1), id(12)], "{mode:?}");
        if mode == GroupMode::StartFirstAndEnter {
            assert_eq!(rig.playhead(), Some(id(13)), "past the first child's chain");
        }
    }
    // Playlist: plays on past the skipped child.
    let mut rig = Rig::new(vec![grp(
        1,
        GroupMode::Playlist {
            loop_: false,
            shuffle: false,
        },
        vec![wait(11, 0.1), skipped(wait(12, 5.0)), wait(13, 0.1)],
    )]);
    let mut out = rig.start(0, 1);
    out.extend(rig.tick(100));
    out.extend(rig.tick(200));
    assert_eq!(starts(&out), vec![(id(1), 0), (id(11), 0), (id(13), 100)]);
    assert_eq!(rig.state(1), CueState::Idle);
    // Random: never the skipped child, even when it is the only other one.
    for seed in 0..20 {
        let mut rig = Rig::seeded(
            vec![grp(
                1,
                GroupMode::StartRandom,
                vec![skipped(wait(11, 1.0)), wait(12, 1.0)],
            )],
            seed,
        );
        let out = rig.start(0, 1);
        assert_eq!(started(&out), vec![id(1), id(12)], "seed {seed}");
    }
    // Random with only skipped children: nothing starts and the group ends at once.
    let mut rig = Rig::new(vec![grp(
        1,
        GroupMode::StartRandom,
        vec![skipped(wait(11, 1.0))],
    )]);
    let out = rig.start(0, 1);
    assert_eq!(started(&out), vec![id(1)]);
    assert_eq!(rig.state(1), CueState::Idle);
}

#[test]
fn a_cue_disarmed_by_a_disarm_cue_is_skipped_at_once() {
    // The Disarm cue's change counts for the rest of the call that made it, before the model
    // has it.
    let mut two = timed(wait(2, 1.0), 0.0, 0.0, DNC);
    two.skip_if_disarmed = true;
    let mut rig = Rig::new(vec![
        timed(
            cue(
                1,
                CueKind::Disarm {
                    target: Some(id(2)),
                },
            ),
            0.0,
            0.0,
            AC,
        ),
        two,
        memo(3),
    ]);
    let out = rig.go(0);
    assert_eq!(started(&out), vec![id(1), id(3)]);
}

#[test]
fn a_running_cue_disarmed_and_skipped_still_takes_its_second_trigger() {
    let mut rig = Rig::new(vec![wait(1, 5.0)]);
    rig.start(0, 1);
    let one = rig.find_mut(1);
    one.armed = false;
    one.skip_if_disarmed = true;
    rig.engine.model_changed();
    let out = rig.start(10, 1);
    assert!(
        warnings(&out).contains(&EngineWarning::TriggerIgnored {
            cue: id(1),
            state: CueState::Running
        }),
        "{out:?}"
    );
    assert_eq!(rig.state(1), CueState::Running);
}

// ----- second colour: played until reset -----

#[test]
fn a_cue_is_played_once_started_until_reset_all() {
    let mut rig = Rig::new(vec![wait(1, 0.1), memo(2)]);
    assert_eq!(rig.engine.played().count(), 0);
    let out = rig.go(0);
    assert_eq!(played(&out), vec![(id(1), true)]);
    // Still played after it ends, and a second start says nothing new.
    rig.tick(200);
    let out = rig.start(300, 1);
    assert!(played(&out).is_empty());
    assert_eq!(rig.engine.played().collect::<Vec<_>>(), vec![id(1)]);
    let out = rig.at(400, Input::ResetAll);
    assert_eq!(played(&out), vec![(id(1), false)]);
    assert_eq!(rig.engine.played().count(), 0);
}

#[test]
fn a_reset_cue_or_reset_of_a_group_brings_the_first_colour_back() {
    let mut rig = Rig::new(vec![
        grp(1, GroupMode::Timeline, vec![memo(11), memo(12)]),
        cue(
            2,
            CueKind::Reset {
                target: Some(id(1)),
            },
        ),
        memo(3),
    ]);
    rig.start(0, 1);
    rig.start(0, 3);
    assert_eq!(
        rig.engine.played().collect::<Vec<_>>(),
        vec![id(1), id(3), id(11), id(12)]
    );
    let out = rig.start(10, 2);
    let mut gone = played(&out);
    gone.retain(|(c, _)| *c != id(2));
    gone.sort();
    assert_eq!(gone, vec![(id(1), false), (id(11), false), (id(12), false)]);
    assert_eq!(
        rig.engine.played().collect::<Vec<_>>(),
        vec![id(2), id(3)],
        "the Reset cue itself has played; cue 3 is untouched"
    );
    // The operator's reset of one cue too.
    let out = rig.at(20, Input::Reset { cue: id(3) });
    assert_eq!(played(&out), vec![(id(3), false)]);
}

#[test]
fn a_reopened_show_has_no_played_cues() {
    let mut rig = Rig::new(vec![memo(1)]);
    rig.go(0);
    assert_eq!(rig.engine.played().count(), 1);
    // Opening the show again makes a new engine for it.
    let reopened = Rig::on(rig.ws.clone(), *rig.engine.config());
    assert_eq!(reopened.engine.played().count(), 0);
}

#[test]
fn a_skipped_cue_is_never_played() {
    let mut rig = Rig::new(vec![timed(memo(1), 0.0, 0.0, AC), skipped(memo(2))]);
    let out = rig.go(0);
    assert_eq!(played(&out), vec![(id(1), true)]);
}

// ----- fade and stop others -----

fn fading_others(mut c: Cue, scope: OthersScope, seconds: f64) -> Cue {
    c.fade_and_stop_others = FadeAndStopOthers {
        enabled: true,
        scope,
        duration: seconds,
    };
    c
}

/// The fade a cue's voice was stopped with, by cue.
fn stop_fades(actions: &[Action]) -> Vec<(CueId, SampleTime, Option<StopFade>)> {
    let mut out = voice_stops(actions);
    out.sort_by_key(|(c, ..)| *c);
    out
}

fn linear(frames: SampleTime) -> Option<StopFade> {
    Some(StopFade {
        frames,
        curve: FadeCurve::Linear,
    })
}

#[test]
fn fade_and_stop_others_peers_fades_the_cues_at_its_level_and_cues_in_other_groups_play_on() {
    // 3 sits in group 1 beside 2; group 4 holds 5; 6 is at the top level.
    let mut rig = Rig::new(vec![
        grp(
            1,
            GroupMode::Timeline,
            vec![sound(2), fading_others(sound(3), OthersScope::Peers, 2.0)],
        ),
        grp(4, GroupMode::Timeline, vec![sound(5)]),
        sound(6),
    ]);
    rig.start(0, 2);
    rig.start(0, 4);
    rig.start(0, 6);
    let out = rig.start(100, 3);
    assert_eq!(stop_fades(&out), vec![(id(2), 100, linear(2000))]);
    assert_eq!(rig.state(2), CueState::Stopping);
    assert_eq!(rig.state(5), CueState::Running, "inside another group");
    assert_eq!(rig.state(6), CueState::Running, "not at its level");
    assert_eq!(rig.state(3), CueState::Running);
    // The fade ends the cue on its last sample.
    rig.tick(2100);
    assert_eq!(rig.state(2), CueState::Idle);
}

#[test]
fn fade_and_stop_others_list_and_all_widen_it_as_named() {
    let make = |scope| {
        let mut rig = Rig::new(vec![
            grp(1, GroupMode::Timeline, vec![sound(2)]),
            fading_others(sound(3), scope, 1.0),
            sound(4),
        ]);
        rig.ws.cue_lists.push(CueList {
            id: list_id(2),
            name: "Second".into(),
            cues: vec![sound(7)],
        });
        rig.tick(0);
        for n in [1, 4, 7] {
            rig.start(0, n);
        }
        rig
    };
    // Peers at the top level: the other top-level cues, a group with everything in it.
    let mut rig = make(OthersScope::Peers);
    let out = rig.start(10, 3);
    assert_eq!(
        stop_fades(&out),
        vec![(id(2), 10, linear(1000)), (id(4), 10, linear(1000))]
    );
    assert_eq!(
        rig.state(1),
        CueState::Stopping,
        "the group waits for its child"
    );
    assert_eq!(rig.state(7), CueState::Running);
    // List: the same here (every cue of the list is under a top-level cue).
    let mut rig = make(OthersScope::List);
    let out = rig.start(10, 3);
    assert_eq!(stop_fades(&out).len(), 2);
    assert_eq!(rig.state(7), CueState::Running, "another list");
    // All: the other list too.
    let mut rig = make(OthersScope::All);
    let out = rig.start(10, 3);
    assert_eq!(
        stop_fades(&out),
        vec![
            (id(2), 10, linear(1000)),
            (id(4), 10, linear(1000)),
            (id(7), 10, linear(1000))
        ]
    );
}

#[test]
fn fade_and_stop_others_list_spares_the_groups_around_the_cue_and_reaches_other_groups() {
    let mut rig = Rig::new(vec![
        grp(
            1,
            GroupMode::StartFirstAndGoToNext,
            vec![sound(2), fading_others(sound(3), OthersScope::List, 1.0)],
        ),
        grp(4, GroupMode::Timeline, vec![sound(5)]),
    ]);
    // Group 1 runs with 2 playing; 3 is started on its own after it.
    rig.start(0, 1);
    rig.start(0, 4);
    let out = rig.start(20, 3);
    assert_eq!(
        stop_fades(&out),
        vec![(id(2), 20, linear(1000)), (id(5), 20, linear(1000))]
    );
    assert_eq!(
        rig.state(1),
        CueState::Running,
        "its own group is not stopped"
    );
    assert_eq!(rig.state(3), CueState::Running);
    assert_eq!(rig.state(4), CueState::Stopping);
}

#[test]
fn fade_and_stop_others_stops_paused_pre_waiting_post_waiting_and_loaded_cues_at_once() {
    let mut rig = Rig::new(vec![
        sound(1),
        timed(memo(2), 5.0, 0.0, DNC),
        timed(wait(3, 0.1), 0.0, 5.0, AC),
        sound(4),
        fading_others(memo(5), OthersScope::Peers, 1.0),
        memo(6),
    ]);
    rig.start(0, 1);
    rig.pause(10, 1);
    rig.start(10, 2);
    rig.start(10, 3);
    rig.tick(200);
    assert_eq!(rig.state(3), CueState::PostWaiting);
    rig.at(
        200,
        Input::Load {
            cue: id(4),
            time: 0.0,
        },
    );
    let out = rig.start(300, 5);
    for n in [1, 2, 3] {
        assert_eq!(rig.state(n), CueState::Idle, "cue {n} stopped at once");
    }
    assert_eq!(
        rig.state(4),
        CueState::Idle,
        "a loaded cue is unloaded (QLab 5.6.5, measured)"
    );
    assert_eq!(
        rig.state(6),
        CueState::Idle,
        "3's auto-continue was cancelled"
    );
    assert!(
        stop_fades(&out).iter().all(|(_, _, f)| f.is_none()),
        "{out:?}"
    );
}

#[test]
fn fade_and_stop_others_leaves_a_cue_already_fading_out_to_its_own_fade() {
    let mut rig = Rig::new(vec![
        sound(1),
        fading_others(memo(2), OthersScope::Peers, 2.0),
    ]);
    rig.start(0, 1);
    rig.at(
        10,
        Input::Stop {
            cue: id(1),
            fade_out: Some(FadeSpec {
                duration: 0.5,
                curve: FadeCurve::Linear,
            }),
        },
    );
    let out = rig.start(20, 2);
    assert!(voice_stops(&out).is_empty(), "{out:?}");
    rig.tick(510);
    assert_eq!(rig.state(1), CueState::Idle);
}

#[test]
fn fade_and_stop_others_waits_for_the_pre_wait_and_does_nothing_when_disarmed_or_zero_time() {
    let mut rig = Rig::new(vec![
        sound(1),
        timed(
            fading_others(memo(2), OthersScope::Peers, 1.0),
            0.5,
            0.0,
            DNC,
        ),
    ]);
    rig.start(0, 1);
    let out = rig.start(0, 2);
    assert!(voice_stops(&out).is_empty(), "not during the pre-wait");
    let out = rig.tick(500);
    assert_eq!(stop_fades(&out), vec![(id(1), 500, linear(1000))]);
    // Disarmed: its action is skipped, and with it the fade.
    let mut two = fading_others(memo(2), OthersScope::Peers, 1.0);
    two.armed = false;
    let mut rig = Rig::new(vec![sound(1), two]);
    rig.start(0, 1);
    assert!(voice_stops(&rig.start(10, 2)).is_empty());
    assert_eq!(rig.state(1), CueState::Running);
    // Off: nothing, though its settings are kept.
    let mut off = fading_others(memo(2), OthersScope::All, 1.0);
    off.fade_and_stop_others.enabled = false;
    let mut rig = Rig::new(vec![sound(1), off]);
    rig.start(0, 1);
    assert!(voice_stops(&rig.start(10, 2)).is_empty());
    // A time of 0 stops them at once.
    let mut rig = Rig::new(vec![
        sound(1),
        fading_others(memo(2), OthersScope::Peers, 0.0),
    ]);
    rig.start(0, 1);
    let out = rig.start(10, 2);
    assert_eq!(stop_fades(&out), vec![(id(1), 10, None)]);
    assert_eq!(rig.state(1), CueState::Idle);
}

/// A cue fading out in a panic is not active for the second-trigger rule (a trigger restarts
/// it); disarmed with skip during that fade, a trigger must not give it a new run either.
#[test]
fn a_cue_skipped_during_its_panic_fade_is_not_restarted_by_a_trigger() {
    let mut rig = Rig::panicking(vec![timed(sound(1), 0.0, 0.0, AC), memo(2)]);
    rig.go(0);
    rig.at(10, Input::PanicCue { cue: id(1) });
    let one = rig.find_mut(1);
    one.armed = false;
    one.skip_if_disarmed = true;
    rig.engine.model_changed();
    let out = rig.start(20, 1);
    assert!(starts(&out).is_empty(), "{out:?}");
    assert!(voice_stops(&out).is_empty(), "its fade goes on: {out:?}");
    assert_eq!(rig.state(1), CueState::Stopping);
}

// ----- duck or boost others -----

fn ducking_others(mut c: Cue, db: f64, seconds: f64) -> Cue {
    c.duck_others = DuckOthers {
        enabled: true,
        db,
        duration: seconds,
    };
    c
}

/// Every duck ramp sent: (cue, at, gain, frames).
fn ducks(actions: &[Action]) -> Vec<(CueId, SampleTime, f32, SampleTime)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::DuckVoice {
                cue,
                at,
                gain,
                frames,
                ..
            } => Some((*cue, *at, *gain, *frames)),
            _ => None,
        })
        .collect()
}

fn gain(db: f64) -> f32 {
    db_to_gain(db)
}

#[test]
fn duck_others_by_12_db_over_1_s_ducks_every_other_audio_cue_of_its_list_and_returns_when_it_ends()
{
    let mut rig = Rig::new(vec![
        sound(1),
        sound(3),
        ducking_others(wait(2, 3.0), -12.0, 1.0),
    ]);
    rig.start(0, 1);
    rig.start(0, 3);
    let out = rig.start(100, 2);
    assert_eq!(
        ducks(&out),
        vec![
            (id(1), 100, gain(-12.0), 1000),
            (id(3), 100, gain(-12.0), 1000)
        ]
    );
    // It ends at 3.1 s: the others come back over the same second.
    let out = rig.tick(3100);
    assert_eq!(
        ducks(&out),
        vec![(id(1), 3100, 1.0, 1000), (id(3), 3100, 1.0, 1000)]
    );
    // Nothing more once they are back.
    assert!(ducks(&rig.tick(5000)).is_empty());
}

#[test]
fn a_positive_value_boosts_but_never_past_the_workspace_maximum() {
    let mut loud = sound(1);
    if let CueKind::Audio { levels, .. } = &mut loud.kind {
        levels.master_db = 9.0;
    }
    let mut rig = Rig::new(vec![loud, sound(3), ducking_others(wait(2, 3.0), 6.0, 1.0)]);
    rig.start(0, 1);
    rig.start(0, 3);
    let out = rig.start(10, 2);
    let got = ducks(&out);
    assert_eq!(got.len(), 2);
    // Cue 1's master is at +9 of +12: only +3 of the +6.
    assert!((got[0].2 - gain(3.0)).abs() < 1e-6, "{got:?}");
    assert!((got[1].2 - gain(6.0)).abs() < 1e-6, "{got:?}");
}

#[test]
fn with_a_pre_wait_the_duck_starts_after_the_pre_wait() {
    let mut rig = Rig::new(vec![
        sound(1),
        timed(ducking_others(wait(2, 3.0), -12.0, 1.0), 0.5, 0.0, DNC),
    ]);
    rig.start(0, 1);
    assert!(ducks(&rig.start(0, 2)).is_empty());
    let out = rig.tick(500);
    assert_eq!(ducks(&out), vec![(id(1), 500, gain(-12.0), 1000)]);
}

#[test]
fn a_cue_started_during_the_duck_starts_ducked() {
    let mut rig = Rig::new(vec![sound(1), ducking_others(wait(2, 3.0), -12.0, 1.0)]);
    rig.start(0, 2);
    let out = rig.start(200, 1);
    assert_eq!(voice_starts(&out), vec![(id(1), 200)]);
    assert_eq!(
        ducks(&out),
        vec![(id(1), 200, gain(-12.0), 0)],
        "on its start sample"
    );
}

#[test]
fn two_ducks_at_once_add_in_db_and_come_off_one_by_one() {
    let mut rig = Rig::new(vec![
        sound(1),
        ducking_others(wait(2, 2.0), -6.0, 0.5),
        ducking_others(wait(3, 4.0), -10.0, 1.0),
    ]);
    rig.start(0, 1);
    rig.start(0, 2);
    let out = rig.start(100, 3);
    // Each ducking cue does not duck itself; 2 is a Wait, so only 1 has a voice.
    assert_eq!(ducks(&out), vec![(id(1), 100, gain(-16.0), 1000)]);
    let out = rig.tick(2000);
    assert_eq!(
        ducks(&out),
        vec![(id(1), 2000, gain(-10.0), 500)],
        "2's own time"
    );
    let out = rig.tick(4100);
    assert_eq!(ducks(&out), vec![(id(1), 4100, 1.0, 1000)]);
}

#[test]
fn a_ducking_cue_paused_keeps_its_duck_and_stopped_or_hard_stopped_lets_it_go() {
    let mut rig = Rig::new(vec![sound(1), ducking_others(sound(2), -12.0, 1.0)]);
    rig.start(0, 1);
    rig.start(0, 2);
    assert!(
        ducks(&rig.pause(100, 2)).is_empty(),
        "paused, it still ducks"
    );
    let out = rig.stop(200, 2);
    assert_eq!(ducks(&out), vec![(id(1), 200, 1.0, 1000)]);
    // A stop with a fade lets go when the cue has stopped.
    rig.start(300, 2);
    rig.at(
        400,
        Input::Stop {
            cue: id(2),
            fade_out: Some(FadeSpec {
                duration: 0.5,
                curve: FadeCurve::Linear,
            }),
        },
    );
    let out = rig.tick(900);
    assert_eq!(ducks(&out), vec![(id(1), 900, 1.0, 1000)]);
}

#[test]
fn a_ducking_cue_that_fails_to_start_or_breaks_lets_the_duck_go() {
    let mut rig = Rig::new(vec![sound(1), ducking_others(sound(2), -12.0, 1.0)]);
    rig.start(0, 1);
    rig.start(0, 2);
    let run = rig.run(2);
    let out = rig.at(
        50,
        Input::StartFailed {
            cue: id(2),
            run,
            reason: BrokenReason::StreamFailed,
        },
    );
    assert_eq!(ducks(&out), vec![(id(1), 50, 1.0, 1000)]);
    rig.at(60, Input::Reset { cue: id(2) });
    rig.start(100, 2);
    let out = rig.failed(200, 2);
    assert_eq!(ducks(&out), vec![(id(1), 200, 1.0, 1000)]);
    // The output device going away fails it the same way.
    rig.at(300, Input::Reset { cue: id(2) });
    rig.start(400, 2);
    let run = rig.run(2);
    let out = rig.at(
        500,
        Input::ActionFailed {
            cue: id(2),
            run,
            reason: BrokenReason::DeviceLost,
        },
    );
    assert_eq!(ducks(&out), vec![(id(1), 500, 1.0, 1000)]);
}

#[test]
fn panic_during_a_duck_leaves_no_ducked_level_on_the_next_run() {
    let mut rig = Rig::panicking(vec![sound(1), ducking_others(wait(2, 5.0), -20.0, 1.0)]);
    rig.start(0, 1);
    rig.start(0, 2);
    let out = rig.at(100, Input::Panic);
    // A cue fading out in the panic is not brought back up while it goes: the duck still on its
    // way down is held where it is (a ramp of 0 frames on the panic sample), nothing more.
    assert!(
        ducks(&out)
            .iter()
            .all(|(c, at, _, f)| (*c, *at, *f) == (id(1), 100, 0)),
        "{out:?}"
    );
    let out = rig.tick(1100);
    assert!(ducks(&out).is_empty(), "{out:?}");
    assert_eq!(rig.state(1), CueState::Idle);
    let out = rig.start(1200, 1);
    assert_eq!(voice_starts(&out), vec![(id(1), 1200)]);
    assert!(
        ducks(&out).is_empty(),
        "a new voice plays at no duck: {out:?}"
    );
}

#[test]
fn a_level_edit_or_a_fade_during_the_duck_moves_the_cue_level_not_the_duck() {
    let mut rig = Rig::new(vec![
        sound(1),
        ducking_others(wait(2, 5.0), -12.0, 1.0),
        cue(3, fade(1)),
    ]);
    rig.start(0, 1);
    rig.start(0, 2);
    // A level edit: a ramp of its master, the duck left as it is.
    if let CueKind::Audio { levels, .. } = &mut rig.find_mut(1).kind {
        levels.master_db = -6.0;
    }
    rig.engine.model_changed();
    let out = rig.tick(100);
    assert!(ducks(&out).is_empty(), "{out:?}");
    assert!(out.iter().any(|a| matches!(a,
        Action::Ramp { cue, param: LevelParam::Master, .. } if *cue == id(1))));
    // A Fade cue: the same.
    let out = rig.start(200, 3);
    assert!(ducks(&out).is_empty(), "{out:?}");
    assert!(out.iter().any(|a| matches!(a,
        Action::Ramp { cue, param: LevelParam::Master, .. } if *cue == id(1))));
}

#[test]
fn a_duck_reaches_only_audio_cues_of_its_own_list_and_not_its_own_children() {
    let mut rig = Rig::new(vec![
        sound(1),
        ducking_others(grp(2, GroupMode::Timeline, vec![sound(21)]), -12.0, 1.0),
    ]);
    rig.ws.cue_lists.push(CueList {
        id: list_id(2),
        name: "Second".into(),
        cues: vec![sound(7)],
    });
    rig.tick(0);
    rig.start(0, 1);
    rig.start(0, 7);
    let out = rig.start(10, 2);
    assert_eq!(ducks(&out), vec![(id(1), 10, gain(-12.0), 1000)]);
}

#[test]
fn a_disarmed_ducking_cue_ducks_nothing_and_off_keeps_its_settings() {
    let mut two = ducking_others(wait(2, 1.0), -12.0, 1.0);
    two.armed = false;
    let mut rig = Rig::new(vec![sound(1), two]);
    rig.start(0, 1);
    assert!(ducks(&rig.start(10, 2)).is_empty());
    let mut off = ducking_others(wait(2, 1.0), -12.0, 1.0);
    off.duck_others.enabled = false;
    let mut rig = Rig::new(vec![sound(1), off]);
    rig.start(0, 1);
    assert!(ducks(&rig.start(10, 2)).is_empty());
}

#[test]
fn a_ducking_cue_deleted_mid_duck_lets_the_duck_go() {
    let mut rig = Rig::new(vec![sound(1), ducking_others(wait(2, 5.0), -12.0, 1.0)]);
    rig.start(0, 1);
    rig.start(0, 2);
    rig.cues_mut().remove(1);
    rig.engine.model_changed();
    let out = rig.tick(100);
    assert_eq!(ducks(&out), vec![(id(1), 100, 1.0, 1000)]);
}

#[test]
fn a_boost_comes_down_when_an_edit_raises_the_master_so_the_maximum_holds() {
    let mut rig = Rig::new(vec![sound(1), ducking_others(wait(2, 5.0), 6.0, 1.0)]);
    rig.start(0, 1);
    rig.start(0, 2);
    if let CueKind::Audio { levels, .. } = &mut rig.find_mut(1).kind {
        levels.master_db = 9.0;
    }
    rig.engine.model_changed();
    let out = rig.tick(100);
    let got = ducks(&out);
    assert_eq!(got.len(), 1, "{out:?}");
    assert!((got[0].2 - gain(3.0)).abs() < 1e-6, "{got:?}");
    assert_eq!(got[0].3, 10, "within the edit ramp");
}

/// Duck ramps sent for `n` in `out` that would raise it: any to a gain above `level` with frames,
/// which a voice going away must never get.
fn raises(out: &[Action], n: u128, level: f32) -> Vec<(CueId, SampleTime, f32, SampleTime)> {
    ducks(out)
        .into_iter()
        .filter(|(c, _, g, _)| *c == id(n) && *g > level + 1e-6)
        .collect()
}

#[test]
fn panic_during_a_duck_never_raises_a_ducked_cue_whatever_the_cue_order() {
    for ducker_first in [true, false] {
        let ducker = ducking_others(wait(2, 5.0), -20.0, 1.0);
        let cues = if ducker_first {
            vec![ducker, sound(1)]
        } else {
            vec![sound(1), ducker]
        };
        let mut rig = Rig::panicking(cues);
        rig.start(0, 1);
        rig.start(0, 2);
        let out = rig.at(2000, Input::Panic);
        assert!(
            raises(&out, 1, gain(-20.0)).is_empty(),
            "ducker first {ducker_first}: {out:?}"
        );
        let out = rig.tick(3100);
        assert!(raises(&out, 1, gain(-20.0)).is_empty(), "{out:?}");
    }
}

#[test]
fn a_duck_return_under_way_is_held_where_it_is_by_a_panic_a_stop_fade_or_fade_and_stop_others() {
    // The ducking Wait ends at 3 s; its 2 s return is under way at 3.1 s.
    let setup = |extra: Vec<Cue>| {
        let mut cues = vec![sound(1), ducking_others(wait(2, 3.0), -40.0, 2.0)];
        cues.extend(extra);
        let mut rig = Rig::panicking(cues);
        rig.start(0, 1);
        rig.start(0, 2);
        rig.tick(3000);
        rig
    };
    let check = |out: &[Action], what: &str| {
        let got = ducks(out);
        assert_eq!(got.len(), 1, "{what}: {out:?}");
        let (cue, at, g, frames) = got[0];
        assert_eq!(
            (cue, at, frames),
            (id(1), 3100, 0),
            "{what}: held on that sample"
        );
        assert!(g > gain(-40.0) && g < 1.0, "{what}: where it was, {g}");
    };
    let mut rig = setup(vec![]);
    check(&rig.at(3100, Input::Panic), "panic");
    let mut rig = setup(vec![]);
    let out = rig.at(
        3100,
        Input::Stop {
            cue: id(1),
            fade_out: Some(FadeSpec {
                duration: 1.0,
                curve: FadeCurve::Linear,
            }),
        },
    );
    check(&out, "stop with a fade");
    let mut rig = setup(vec![fading_others(memo(3), OthersScope::Peers, 1.0)]);
    check(&rig.start(3100, 3), "fade and stop others");
    // After it, nothing more is sent to the fading voice.
    assert!(ducks(&rig.tick(5200)).is_empty());
}

#[test]
fn stop_all_during_a_duck_sends_no_duck_ramp_whatever_the_cue_order() {
    for ducker_first in [true, false] {
        let ducker = ducking_others(wait(2, 5.0), -20.0, 1.0);
        let cues = if ducker_first {
            vec![ducker, sound(1)]
        } else {
            vec![sound(1), ducker]
        };
        let mut rig = Rig::new(cues);
        rig.start(0, 1);
        rig.start(0, 2);
        let out = rig.at(2000, Input::StopAll);
        assert!(
            ducks(&out).is_empty(),
            "ducker first {ducker_first}: {out:?}"
        );
    }
}
