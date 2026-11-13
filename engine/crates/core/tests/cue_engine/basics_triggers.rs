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
    let out = rig.start(10, 1);
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
    let out = rig.start(10, 1);
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
    // A group's first child is not the cue "after" anything at its level: starting the group
    // starts it; the second child loads when the first starts (start-first modes).
    let g = grp(
        2,
        GroupMode::StartFirstAndGoToNext,
        vec![sound(21), auto_loading(sound(22))],
    );
    let mut rig = Rig::new(vec![sound(1), g]);
    rig.start(0, 2);
    assert_eq!(rig.state(21), CueState::Running);
    assert_eq!(rig.state(22), CueState::Loaded);
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

#[test]
fn go_with_the_playhead_on_a_skipped_cue_starts_nothing_and_moves_on() {
    let mut rig = Rig::new(vec![memo(1), skipped(memo(2)), skipped(memo(3)), memo(4)]);
    rig.set_playhead(0, 2);
    let out = rig.go(0);
    assert!(started(&out).is_empty(), "{out:?}");
    assert_eq!(rig.playhead(), Some(id(4)));
    // A trigger of its own (a Start cue, a hotkey) starts nothing either.
    let out = rig.start(10, 2);
    assert!(out.is_empty(), "{out:?}");
    assert_eq!(rig.state(2), CueState::Idle);
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
