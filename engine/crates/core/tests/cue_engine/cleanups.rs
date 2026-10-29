// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Regression tests for the cue engine cleanups (small defects found in review of the cue engine).
//! Virtual sample clock at 1 kHz: one second is 1000 samples.

use crate::common::*;
use openlab_core::*;

use ContinueMode::{AutoContinue as AC, AutoFollow as AF, DoNotContinue as DNC};

fn start_cue(n: u128, target: u128) -> Cue {
    cue(
        n,
        CueKind::Start {
            target: Some(id(target)),
        },
    )
}

// ----- a GO the chain budget cannot start -----

/// A wait that, when it ends at 1 s, starts a cycle of two Start cues with no time in it: the
/// whole chain budget goes in that deadline pass.
fn cycle_after_a_wait() -> Rig {
    let w = timed(cue(1, CueKind::Wait { duration: 1.0 }), 0.0, 0.0, AF);
    let mut rig = Rig::new(vec![w, start_cue(2, 3), start_cue(3, 2), memo(4)]);
    rig.ws.settings.min_go_interval = 0.5;
    rig
}

#[test]
fn a_go_after_a_spent_chain_budget_is_ignored_and_changes_nothing() {
    // Regression: the GO's start was dropped with only a ChainLimit warning while the
    // playhead and the double-GO interval still advanced.
    let mut rig = cycle_after_a_wait();
    rig.go(0);
    // GO at 0 started the wait; its chain (auto-follow into cue 2) put the playhead on cue 3.
    assert_eq!(rig.engine.playhead(list_id(1)), Some(id(3)));
    let go = rig.at(1000, Input::Go { list: list_id(1) });
    assert!(
        warnings(&go)
            .iter()
            .any(|w| matches!(w, EngineWarning::ChainLimit { .. })),
        "the wait's cycle was cut"
    );
    assert!(go.contains(&Action::GoIgnored {
        list: list_id(1),
        reason: GoIgnoredReason::ChainLimit
    }));
    assert_eq!(
        rig.engine.playhead(list_id(1)),
        Some(id(3)),
        "playhead stays"
    );
    // The ignored GO did not restart the double-GO interval: 100 samples later (but 1100 after
    // the last accepted GO) the operator's GO is accepted, not a double-GO.
    let again = rig.go(1100);
    assert!(
        !again.iter().any(|a| matches!(a, Action::GoIgnored { .. })),
        "{again:?}"
    );
    assert_eq!(rig.engine.playhead(list_id(1)), Some(id(4)));
}

// ----- a broken cue that was paused -----

#[test]
fn start_resumes_a_cue_that_broke_while_paused() {
    // Regression: a cue broken while paused only shows Broken and GO/Start refused it
    // (`CueBroken`) while its pending auto-continue stayed frozen until Resume.
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 2.0, AC), memo(2)]);
    rig.start(0, 1);
    rig.at(100, Input::Pause { cue: id(1) });
    let run = rig.engine.run_of(id(1)).unwrap();
    rig.at(
        200,
        Input::ActionFailed {
            cue: id(1),
            run,
            reason: BrokenReason::StreamFailed,
        },
    );
    assert_eq!(rig.engine.state(id(1)), CueState::Broken);
    assert_eq!(rig.engine.next_deadline(), None, "frozen while paused");
    let start = rig.start(300, 1);
    assert!(warnings(&start).is_empty(), "{start:?}");
    // 100 samples of the post-wait had run when it paused; 200 samples of pause shift it.
    assert_eq!(rig.engine.next_deadline(), Some(2200));
    let tick = rig.at(2200, Input::Tick);
    assert_eq!(
        changes(&tick, 2),
        vec![
            (CueState::PreWaiting, 2200),
            (CueState::Running, 2200),
            (CueState::Idle, 2200)
        ]
    );
    // Never an auto-follow from a broken cue; still broken.
    assert_eq!(rig.engine.state(id(1)), CueState::Broken);
}

#[test]
fn start_still_refuses_a_broken_cue_that_is_not_paused() {
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 2.0, AC), memo(2)]);
    rig.start(0, 1);
    let run = rig.engine.run_of(id(1)).unwrap();
    rig.at(
        200,
        Input::ActionFailed {
            cue: id(1),
            run,
            reason: BrokenReason::StreamFailed,
        },
    );
    let start = rig.start(300, 1);
    assert_eq!(
        warnings(&start),
        vec![EngineWarning::CueBroken { cue: id(1) }]
    );
    assert_eq!(rig.engine.next_deadline(), Some(2000));
}

// ----- run_of of a broken cue -----

#[test]
fn run_of_is_none_for_every_kind_of_broken_cue() {
    // Regression: a run-time broken cue with a pending continue still had a run id,
    // one that settled into Slot::Broken did not.
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 2.0, AC), memo(2)]);
    rig.start(0, 1);
    let run = rig.engine.run_of(id(1)).unwrap();
    rig.at(
        200,
        Input::ActionFailed {
            cue: id(1),
            run,
            reason: BrokenReason::DeviceLost,
        },
    );
    assert_eq!(rig.engine.state(id(1)), CueState::Broken);
    assert_eq!(rig.engine.run_of(id(1)), None, "broken, continue pending");
    rig.at(2000, Input::Tick);
    assert_eq!(rig.engine.run_of(id(1)), None, "broken, settled");

    // Started while it had problems: no voice either.
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 2.0, AC), memo(2)]);
    rig.at(
        1,
        Input::MediaStatus {
            media: MediaRef::new("media/a.wav"),
            status: MediaStatus::Missing,
        },
    );
    rig.start(2, 1);
    assert_eq!(rig.engine.state(id(1)), CueState::Broken);
    assert_eq!(rig.engine.run_of(id(1)), None, "started broken");
}

// ----- Resume of a cue that is not paused -----

#[test]
fn resume_of_a_cue_that_is_not_paused_takes_no_run_id() {
    // Regression: every Resume consumed a RunId, paused or not.
    let mut rig = Rig::new(vec![sound(1), sound(2)]);
    rig.start(0, 1);
    let first = rig.engine.run_of(id(1)).unwrap();
    rig.at(10, Input::Resume { cue: id(1) });
    rig.at(20, Input::Resume { cue: id(2) });
    rig.at(30, Input::ResumeAll);
    rig.start(40, 2);
    assert_eq!(rig.engine.run_of(id(1)), Some(first), "no new segment");
    assert_eq!(rig.engine.run_of(id(2)), Some(RunId(first.0 + 1)));
}

// ----- the pre-wait split of a loaded cue is frozen at load -----

#[test]
fn a_loaded_cue_starts_from_the_offset_it_was_loaded_with_after_a_pre_wait_edit() {
    // Regression: LoadVoice used the pre-wait at load time and StartVoice the
    // pre-wait at start, so an edit between them made the primed stream disagree with the start.
    let mut rig = Rig::new(vec![timed(sound(1), 2.0, 0.0, DNC)]);
    let load = rig.at(
        0,
        Input::Load {
            cue: id(1),
            time: 3.0,
        },
    );
    let loaded_offset = load
        .iter()
        .find_map(|a| match a {
            Action::LoadVoice { offset, .. } => Some(*offset),
            _ => None,
        })
        .unwrap();
    assert_eq!(loaded_offset, 1000);
    rig.ws.cue_lists[0].cues[0].pre_wait = 5.0;
    let start = rig.start(10, 1);
    let started: Vec<_> = start
        .iter()
        .filter_map(|a| match a {
            Action::StartVoice { at, offset, .. } => Some((*at, *offset)),
            _ => None,
        })
        .collect();
    // The pre-wait had been served by the load-to-time: it starts at once, from the primed offset.
    assert_eq!(started, vec![(10, loaded_offset)]);
}

#[test]
fn a_cue_loaded_inside_its_pre_wait_keeps_the_pre_wait_left_at_load() {
    let mut rig = Rig::new(vec![timed(sound(1), 2.0, 0.0, DNC)]);
    rig.at(
        0,
        Input::Load {
            cue: id(1),
            time: 0.5,
        },
    );
    rig.ws.cue_lists[0].cues[0].pre_wait = 0.0;
    let start = rig.start(100, 1);
    assert!(
        !start.iter().any(|a| matches!(a, Action::StartVoice { .. })),
        "1.5 s of pre-wait left, as at load"
    );
    assert_eq!(rig.engine.next_deadline(), Some(1600));
    let tick = rig.at(1600, Input::Tick);
    assert!(tick.iter().any(|a| matches!(
        a,
        Action::StartVoice {
            at: 1600,
            offset: 0,
            ..
        }
    )));
}

// ----- Whole-workspace passes only after a model change -----

#[test]
fn the_per_call_workspace_passes_run_only_after_model_changed() {
    let mut rig = Rig::new(vec![memo(1), sound(2)]);
    assert_eq!(rig.engine.model_syncs(), 1, "the first call");
    for now in 1..50 {
        rig.at(now, Input::Tick);
    }
    rig.go(60);
    rig.start(70, 2);
    assert_eq!(
        rig.engine.model_syncs(),
        1,
        "nothing signalled, nothing rebuilt"
    );
    // An edit, signalled as the binary does, is followed once.
    rig.ws.cue_lists[0].cues.push(memo(3));
    rig.at(80, Input::Tick);
    assert_eq!(rig.engine.model_syncs(), 2);
    rig.at(90, Input::Tick);
    assert_eq!(rig.engine.model_syncs(), 2);
    assert_eq!(rig.engine.unsignalled_model_changes(), 0);
}

#[test]
fn signalled_edits_are_still_followed() {
    // The skip must not change what edits do: a deleted playhead cue gives way to the next one,
    // a deleted running cue is stopped.
    let mut rig = Rig::new(vec![sound(1), memo(2), memo(3)]);
    rig.start(0, 1);
    rig.ws.cue_lists[0].cues.remove(0);
    let tick = rig.at(10, Input::Tick);
    assert!(
        tick.iter()
            .any(|a| matches!(a, Action::StopVoice { cue, .. } if *cue == id(1)))
    );
    assert!(warnings(&tick).contains(&EngineWarning::CueRemoved { cue: id(1) }));
    assert_eq!(rig.engine.playhead(list_id(1)), Some(id(2)));
}
