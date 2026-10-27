// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `Input::ActionLength`: an audio action whose length the binary knows ends on a deadline, so
//! continues and playlist moves come on the end sample itself (docs/cue-engine.md, Feedback).
//! 1 kHz, so one second is 1000 samples.

mod common;

use common::*;
use openlab_core::*;

const RATE: u32 = 1000;

struct Rig {
    ws: Workspace,
    engine: CueEngine,
    last: Option<Workspace>,
}

impl Rig {
    fn new(cues: Vec<Cue>, lookahead: SampleTime) -> Self {
        let mut rig = Self {
            ws: workspace(cues),
            engine: CueEngine::new(EngineConfig::new(RATE, lookahead, 1).unwrap()),
            last: None,
        };
        rig.at(0, Input::Tick);
        rig
    }

    fn at(&mut self, now: SampleTime, input: Input) -> Vec<Action> {
        handle_as_binary(&mut self.engine, &self.ws, &mut self.last, input, now)
    }

    fn run(&self, n: u128) -> RunId {
        self.engine.run_of(cue_id(n)).unwrap()
    }

    fn length(&mut self, now: SampleTime, n: u128, frames: SampleTime) -> Vec<Action> {
        let run = self.run(n);
        self.at(
            now,
            Input::ActionLength {
                cue: cue_id(n),
                run,
                frames,
            },
        )
    }
}

fn sound(n: u128, mode: ContinueMode) -> Cue {
    let mut c = cue(n, audio());
    c.continue_mode = mode;
    c
}

fn starts(actions: &[Action]) -> Vec<(CueId, SampleTime)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::StartVoice { cue, at, .. } => Some((*cue, *at)),
            _ => None,
        })
        .collect()
}

use ContinueMode::{AutoFollow as AF, DoNotContinue as DNC};

#[test]
fn auto_follow_comes_on_the_end_sample() {
    let mut r = Rig::new(vec![sound(1, AF), sound(2, DNC)], 0);
    r.at(0, Input::Go { list: list_id(1) });
    r.length(0, 1, 500);
    assert_eq!(r.engine.next_deadline(), Some(500));
    assert!(starts(&r.at(499, Input::Tick)).is_empty());
    let a = r.at(500, Input::Tick);
    assert_eq!(starts(&a), vec![(cue_id(2), 500)]);
    assert_eq!(r.engine.state(cue_id(1)), CueState::Idle);
    assert!(
        !a.iter()
            .any(|x| matches!(x, Action::Warning(EngineWarning::LateStart { .. })))
    );
}

#[test]
fn with_a_lookahead_the_successor_is_committed_ahead_on_its_sample() {
    let mut r = Rig::new(vec![sound(1, AF), sound(2, DNC)], 100);
    r.at(0, Input::Go { list: list_id(1) });
    r.length(0, 1, 500);
    let a = r.at(450, Input::Tick);
    assert_eq!(starts(&a), vec![(cue_id(2), 500)]);
}

#[test]
fn the_voice_report_after_the_deadline_is_stale() {
    let mut r = Rig::new(vec![sound(1, AF), sound(2, DNC), sound(3, DNC)], 0);
    r.at(0, Input::Go { list: list_id(1) });
    let run = r.run(1);
    r.length(0, 1, 500);
    r.at(500, Input::Tick);
    let a = r.at(
        510,
        Input::ActionEnded {
            cue: cue_id(1),
            run,
            at: 500,
        },
    );
    assert!(a.is_empty(), "{a:?}");
    assert_eq!(r.engine.state(cue_id(3)), CueState::Idle);
}

#[test]
fn an_earlier_end_report_still_wins() {
    let mut r = Rig::new(vec![sound(1, AF), sound(2, DNC)], 0);
    r.at(0, Input::Go { list: list_id(1) });
    let run = r.run(1);
    r.length(0, 1, 500);
    let a = r.at(
        310,
        Input::ActionEnded {
            cue: cue_id(1),
            run,
            at: 300,
        },
    );
    assert_eq!(starts(&a), vec![(cue_id(2), 310)]);
    assert_eq!(r.engine.next_deadline(), None);
}

#[test]
fn a_pause_moves_the_end_by_its_length() {
    let mut r = Rig::new(vec![sound(1, AF), sound(2, DNC)], 0);
    r.at(0, Input::Go { list: list_id(1) });
    r.length(0, 1, 500);
    r.at(200, Input::Pause { cue: cue_id(1) });
    assert_eq!(r.engine.next_deadline(), None);
    r.at(700, Input::Resume { cue: cue_id(1) });
    assert_eq!(r.engine.next_deadline(), Some(1000));
    assert!(starts(&r.at(999, Input::Tick)).is_empty());
    assert_eq!(starts(&r.at(1000, Input::Tick)), vec![(cue_id(2), 1000)]);
}

#[test]
fn a_length_sent_while_paused_counts_from_the_resume() {
    let mut r = Rig::new(vec![sound(1, AF), sound(2, DNC)], 0);
    r.at(0, Input::Go { list: list_id(1) });
    r.at(200, Input::Pause { cue: cue_id(1) });
    r.length(300, 1, 500);
    r.at(700, Input::Resume { cue: cue_id(1) });
    assert_eq!(r.engine.next_deadline(), Some(1000));
}

/// M2.23: a resume committed with the planned end inside the lookahead window completes the run
/// in that same call; the voice still plays to that sample, and its own reports for the fresh id
/// come after the run is over and are stale.
#[test]
fn a_resume_whose_known_end_is_in_the_window_completes_the_run_ahead() {
    let mut r = Rig::new(vec![sound(1, DNC)], 300);
    r.at(0, Input::Go { list: list_id(1) });
    r.at(0, Input::Pause { cue: cue_id(1) });
    r.length(10, 1, 100);
    let a = r.at(500, Input::Resume { cue: cue_id(1) });
    let fresh = a.iter().find_map(|x| match x {
        Action::ResumeVoice { run, at: 500, .. } => Some(*run),
        _ => None,
    });
    let fresh = fresh.expect("resumed on its sample");
    assert!(a.contains(&Action::StateChanged {
        cue: cue_id(1),
        state: CueState::Idle,
        at: 600,
    }));
    assert_eq!(r.engine.run_of(cue_id(1)), None);
    let late = r.at(
        610,
        Input::ActionEnded {
            cue: cue_id(1),
            run: fresh,
            at: 600,
        },
    );
    assert!(late.is_empty(), "{late:?}");
}

#[test]
fn a_stop_cancels_the_planned_end() {
    let mut r = Rig::new(vec![sound(1, AF), sound(2, DNC)], 0);
    r.at(0, Input::Go { list: list_id(1) });
    r.length(0, 1, 500);
    r.at(
        200,
        Input::Stop {
            cue: cue_id(1),
            fade_out: Some(FadeSpec {
                duration: 0.1,
                curve: FadeCurve::Linear,
            }),
        },
    );
    assert_eq!(r.engine.next_deadline(), Some(300));
    let a = r.at(300, Input::Tick);
    assert!(starts(&a).is_empty());
    assert_eq!(r.engine.state(cue_id(1)), CueState::Idle);
    assert_eq!(r.engine.next_deadline(), None);
    // A hard stop leaves nothing either.
    r.at(400, Input::Start { cue: cue_id(1) });
    r.length(400, 1, 500);
    r.at(
        450,
        Input::Stop {
            cue: cue_id(1),
            fade_out: None,
        },
    );
    assert_eq!(r.engine.next_deadline(), None);
}

#[test]
fn a_failure_cancels_the_planned_end() {
    let mut r = Rig::new(vec![sound(1, AF), sound(2, DNC)], 0);
    r.at(0, Input::Go { list: list_id(1) });
    let run = r.run(1);
    r.length(0, 1, 500);
    r.at(
        100,
        Input::ActionFailed {
            cue: cue_id(1),
            run,
            reason: BrokenReason::StreamFailed,
        },
    );
    assert_eq!(r.engine.next_deadline(), None);
    assert!(starts(&r.at(500, Input::Tick)).is_empty());
}

#[test]
fn a_devamp_answer_and_its_length_end_the_loop_on_time() {
    let devamp = cue(
        3,
        CueKind::Devamp {
            target: Some(cue_id(1)),
            start_next_cue: false,
            stop_target_when_slice_ends: false,
            devamp_mode: DevampMode::CurrentSlice,
        },
    );
    let mut r = Rig::new(vec![sound(1, AF), sound(2, DNC), devamp], 0);
    r.at(0, Input::Go { list: list_id(1) });
    let run = r.run(1);
    // Looping: no length yet.
    r.at(600, Input::Start { cue: cue_id(3) });
    r.at(
        600,
        Input::DevampScheduled {
            cue: cue_id(1),
            run,
            at: 800,
        },
    );
    // Leaves the loop on 800 with 200 frames of outro: 1000 in all.
    r.length(600, 1, 1000);
    assert_eq!(starts(&r.at(1000, Input::Tick)), vec![(cue_id(2), 1000)]);
}

#[test]
fn a_playlist_moves_on_on_the_end_sample() {
    let playlist = cue(
        10,
        CueKind::Group {
            mode: GroupMode::Playlist {
                loop_: false,
                shuffle: false,
            },
            children: vec![sound(1, DNC), sound(2, DNC)],
        },
    );
    let mut r = Rig::new(vec![playlist], 0);
    r.at(0, Input::Go { list: list_id(1) });
    r.length(0, 1, 400);
    assert_eq!(starts(&r.at(400, Input::Tick)), vec![(cue_id(2), 400)]);
}

#[test]
fn a_stale_or_unknown_length_is_ignored() {
    let mut r = Rig::new(vec![sound(1, AF), sound(2, DNC)], 0);
    r.at(0, Input::Go { list: list_id(1) });
    let a = r.at(
        0,
        Input::ActionLength {
            cue: cue_id(1),
            run: RunId(9999),
            frames: 10,
        },
    );
    assert!(a.is_empty());
    assert_eq!(r.engine.next_deadline(), None);
}
