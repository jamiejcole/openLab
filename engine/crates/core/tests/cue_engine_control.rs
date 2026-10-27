// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Control cues (M2.5, M2.15: CT-11..CT-17, CT-19..CT-21), per docs/cue-engine.md "Control
//! cues". Virtual sample clock at 1 kHz: one second is 1000 samples.

mod common;

use common::*;
use openlab_core::*;

const RATE: u32 = 1000;

use ContinueMode::{AutoContinue as AC, AutoFollow as AF, DoNotContinue as DNC};

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

fn timed(mut c: Cue, pre: f64, post: f64, mode: ContinueMode) -> Cue {
    c.pre_wait = pre;
    c.post_wait = post;
    c.continue_mode = mode;
    c
}

fn grp(n: u128, mode: GroupMode, children: Vec<Cue>) -> Cue {
    cue(n, CueKind::Group { mode, children })
}

fn playlist(loop_: bool) -> GroupMode {
    GroupMode::Playlist {
        loop_,
        shuffle: false,
    }
}

fn t(target: u128) -> Option<CueId> {
    Some(id(target))
}

fn start_cue(n: u128, target: u128) -> Cue {
    cue(n, CueKind::Start { target: t(target) })
}

fn stop_cue(n: u128, target: u128, fade: f64) -> Cue {
    cue(
        n,
        CueKind::Stop {
            target: t(target),
            fade_out: FadeSpec {
                duration: fade,
                curve: FadeCurve::Linear,
            },
        },
    )
}

fn pause_cue(n: u128, target: u128) -> Cue {
    cue(n, CueKind::Pause { target: t(target) })
}

fn load_cue(n: u128, target: u128, time: f64) -> Cue {
    cue(
        n,
        CueKind::Load {
            target: t(target),
            load_to_time: time,
        },
    )
}

fn reset_cue(n: u128, target: u128) -> Cue {
    cue(n, CueKind::Reset { target: t(target) })
}

fn goto_cue(n: u128, target: u128, then_go: bool) -> Cue {
    cue(
        n,
        CueKind::GoTo {
            target: t(target),
            then_go,
        },
    )
}

fn devamp_cue(n: u128, target: u128, start_next: bool, stop: bool) -> Cue {
    cue(
        n,
        CueKind::Devamp {
            target: t(target),
            start_next_cue: start_next,
            stop_target_when_slice_ends: stop,
        },
    )
}

fn arm_cue(n: u128, target: u128, arm: bool) -> Cue {
    let target = t(target);
    cue(
        n,
        if arm {
            CueKind::Arm { target }
        } else {
            CueKind::Disarm { target }
        },
    )
}

struct Rig {
    ws: Workspace,
    engine: CueEngine,
    /// The workspace as the engine last saw it (see `handle_as_binary`).
    last: Option<Workspace>,
}

impl Rig {
    fn new(cues: Vec<Cue>) -> Self {
        Self::with_lookahead(cues, 0)
    }

    fn with_lookahead(cues: Vec<Cue>, lookahead: SampleTime) -> Self {
        let mut rig = Self {
            ws: workspace(cues),
            engine: CueEngine::new(EngineConfig::new(RATE, lookahead, 1).unwrap()),
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

    fn pause(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(now, Input::Pause { cue: id(n) })
    }

    fn stop(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(
            now,
            Input::Stop {
                cue: id(n),
                fade_out: None,
            },
        )
    }

    fn run(&self, n: u128) -> RunId {
        self.engine.run_of(id(n)).unwrap()
    }

    fn ended(&mut self, now: SampleTime, n: u128, at: SampleTime) -> Vec<Action> {
        let run = self.run(n);
        self.at(
            now,
            Input::ActionEnded {
                cue: id(n),
                run,
                at,
            },
        )
    }

    fn failed(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        let run = self.run(n);
        self.at(
            now,
            Input::ActionFailed {
                cue: id(n),
                run,
                reason: BrokenReason::StreamFailed,
            },
        )
    }

    fn state(&self, n: u128) -> CueState {
        self.engine.state(id(n))
    }

    fn playhead(&self) -> Option<CueId> {
        self.engine.playhead(list())
    }

    fn set_playhead(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(
            now,
            Input::SetPlayhead {
                list: list(),
                cue: Some(id(n)),
            },
        )
    }

    fn find_mut(&mut self, n: u128) -> &mut Cue {
        fn find(cues: &mut [Cue], id: CueId) -> Option<&mut Cue> {
            for c in cues {
                if c.id == id {
                    return Some(c);
                }
                if let CueKind::Group { children, .. } = &mut c.kind
                    && let Some(found) = find(children, id)
                {
                    return Some(found);
                }
            }
            None
        }
        let list = &mut self.ws.cue_lists[0].cues;
        find(list, id(n)).unwrap()
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

fn voice_starts(actions: &[Action]) -> Vec<(CueId, SampleTime)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::StartVoice { cue, at, .. } => Some((*cue, *at)),
            _ => None,
        })
        .collect()
}

fn voice_stops(actions: &[Action]) -> Vec<(CueId, SampleTime, Option<StopFade>)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::StopVoice { cue, at, fade, .. } => Some((*cue, *at, *fade)),
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

fn devamps(actions: &[Action]) -> Vec<(CueId, SampleTime, bool)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::DevampVoice { cue, at, stop, .. } => Some((*cue, *at, *stop)),
            _ => None,
        })
        .collect()
}

fn set_armed(actions: &[Action]) -> Vec<(CueId, bool)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::SetArmed { cue, armed } => Some((*cue, *armed)),
            _ => None,
        })
        .collect()
}

// ----- Start (CT-11) -----

#[test]
fn start_cue_starts_its_target_when_its_action_starts() {
    let mut rig = Rig::new(vec![
        timed(start_cue(1, 3), 1.0, 0.0, DNC),
        memo(2),
        sound(3),
    ]);
    let go = rig.go(0);
    assert!(voice_starts(&go).is_empty());
    let tick = rig.tick(1000);
    assert_eq!(voice_starts(&tick), vec![(id(3), 1000)]);
    // Its own action completes at once.
    assert_eq!(rig.state(1), CueState::Idle);
    assert_eq!(rig.state(3), CueState::Running);
    // It did not move the playhead (GO did, to the cue after the Start cue).
    assert_eq!(rig.playhead(), Some(id(2)));
}

#[test]
fn start_cue_continue_modes_apply() {
    // Auto-follow: the next cue starts when the Start cue's (instant) action completes.
    let mut rig = Rig::new(vec![
        timed(start_cue(1, 3), 0.0, 0.0, AF),
        memo(2),
        sound(3),
    ]);
    let go = rig.go(0);
    assert_eq!(starts(&go), vec![(id(1), 0), (id(3), 0), (id(2), 0)]);
    // Auto-continue with a post-wait: the next cue starts at the post-wait.
    let mut rig = Rig::new(vec![
        timed(start_cue(1, 3), 0.0, 2.0, AC),
        memo(2),
        sound(3),
    ]);
    rig.go(0);
    assert_eq!(rig.state(1), CueState::PostWaiting);
    assert_eq!(starts(&rig.tick(2000)), vec![(id(2), 2000)]);
}

#[test]
fn start_cue_on_an_active_target_is_ignored() {
    let mut rig = Rig::new(vec![start_cue(1, 3), memo(2), sound(3)]);
    rig.start(0, 3);
    let run = rig.run(3);
    let go = rig.go(100);
    assert_eq!(
        warnings(&go),
        vec![EngineWarning::TriggerIgnored {
            cue: id(3),
            state: CueState::Running
        }]
    );
    assert!(voice_starts(&go).is_empty());
    assert_eq!(rig.run(3), run);
}

#[test]
fn start_cue_resumes_a_paused_target() {
    let mut rig = Rig::new(vec![start_cue(1, 3), memo(2), sound(3)]);
    rig.start(0, 3);
    rig.pause(500, 3);
    let go = rig.go(1000);
    assert!(go.iter().any(|a| matches!(
        a,
        Action::ResumeVoice { cue, at: 1000, offset: 500, .. } if *cue == id(3)
    )));
    assert_eq!(rig.state(3), CueState::Running);
}

#[test]
fn start_cue_on_a_broken_target_warns() {
    let mut rig = Rig::new(vec![start_cue(1, 3), memo(2), sound(3)]);
    rig.start(0, 3);
    rig.failed(10, 3);
    let go = rig.go(100);
    assert_eq!(warnings(&go), vec![EngineWarning::CueBroken { cue: id(3) }]);
    assert_eq!(rig.state(3), CueState::Broken);
}

#[test]
fn control_cue_with_a_missing_target_is_broken_and_keeps_its_auto_continue() {
    // M2.8: a missing or unset target makes the cue broken before it runs. GO does not start
    // it, but its continue keeps its timing (an auto-follow too since M2.24, QLab check P5).
    let mut rig = Rig::new(vec![
        timed(start_cue(1, 99), 0.0, 0.5, AC),
        timed(cue(2, CueKind::Start { target: None }), 0.0, 0.0, AF),
        memo(3),
    ]);
    assert_eq!(
        rig.engine.problems(id(1)),
        &[CueProblem::TargetMissing { target: id(99) }]
    );
    assert_eq!(rig.engine.problems(id(2)), &[CueProblem::NoTarget]);
    let go = rig.go(0);
    assert_eq!(warnings(&go), vec![EngineWarning::CueBroken { cue: id(1) }]);
    assert!(starts(&go).is_empty());
    let tick = rig.tick(500);
    // Cue 1's continue starts cue 2 (broken: no start, but it is skipped and its auto-follow
    // starts cue 3, as in QLab: M2.24, check P5).
    assert_eq!(
        warnings(&tick),
        vec![EngineWarning::CueBroken { cue: id(2) }]
    );
    assert_eq!(starts(&tick), vec![(id(3), 500)]);
    assert_eq!(rig.state(1), CueState::Broken);
    assert!(rig.engine.next_deadline().is_none());
}

#[test]
fn disarmed_control_cue_does_nothing_but_keeps_its_timing() {
    let mut stop = timed(stop_cue(1, 3, 0.0), 0.0, 1.0, AC);
    stop.armed = false;
    let mut rig = Rig::new(vec![stop, memo(2), sound(3)]);
    rig.start(0, 3);
    let go = rig.go(10);
    assert!(voice_stops(&go).is_empty());
    assert_eq!(rig.state(3), CueState::Running);
    assert_eq!(starts(&rig.tick(1010)), vec![(id(2), 1010)]);
}

#[test]
fn start_cue_on_a_group_starts_it_in_its_mode() {
    let mut rig = Rig::new(vec![
        start_cue(1, 2),
        grp(2, GroupMode::Timeline, vec![sound(3), sound(4)]),
    ]);
    let go = rig.go(0);
    assert_eq!(voice_starts(&go), vec![(id(3), 0), (id(4), 0)]);
    assert_eq!(rig.state(2), CueState::Running);
}

#[test]
fn start_cue_on_a_child_starts_only_that_child() {
    // M2.24 (QLab check G4): the group does not run.
    let mut rig = Rig::new(vec![
        start_cue(1, 3),
        grp(2, GroupMode::Timeline, vec![sound(3), sound(4)]),
    ]);
    let go = rig.go(0);
    assert_eq!(voice_starts(&go), vec![(id(3), 0)]);
    assert_eq!(rig.state(2), CueState::Idle);
    assert_eq!(rig.state(3), CueState::Running);
    rig.ended(100, 3, 50);
    assert_eq!(rig.state(2), CueState::Idle);
    assert_eq!(rig.state(4), CueState::Idle);
}

// ----- Stop (CT-12) -----

#[test]
fn stop_cue_stops_its_target_with_its_fade() {
    let mut rig = Rig::new(vec![stop_cue(1, 3, 2.0), memo(2), sound(3)]);
    rig.start(0, 3);
    let go = rig.go(500);
    assert_eq!(
        voice_stops(&go),
        vec![(
            id(3),
            500,
            Some(StopFade {
                frames: 2000,
                curve: FadeCurve::Linear
            })
        )]
    );
    assert_eq!(rig.state(3), CueState::Stopping);
    assert_eq!(rig.state(1), CueState::Idle);
    rig.tick(2499);
    assert_eq!(rig.state(3), CueState::Stopping);
    assert_eq!(changes(&rig.tick(2500), 3), vec![(CueState::Idle, 2500)]);
}

#[test]
fn stop_cue_on_idle_paused_and_broken_targets() {
    let mut rig = Rig::new(vec![stop_cue(1, 3, 2.0), memo(2), sound(3)]);
    // Idle: nothing.
    let go = rig.go(0);
    assert!(go.iter().all(|a| !matches!(a, Action::StopVoice { .. })));
    assert!(warnings(&go).is_empty());
    // Paused: stopped at once (a stop with fade on a paused cue is hard).
    rig.start(100, 3);
    rig.pause(200, 3);
    rig.set_playhead(300, 1);
    let go = rig.go(300);
    assert_eq!(voice_stops(&go), vec![(id(3), 300, None)]);
    assert_eq!(rig.state(3), CueState::Idle);
    // Broken: stays broken.
    rig.start(400, 3);
    rig.failed(410, 3);
    rig.set_playhead(500, 1);
    rig.go(500);
    assert_eq!(rig.state(3), CueState::Broken);
}

#[test]
fn stop_cue_in_the_lookahead_window_acts_on_its_own_sample() {
    let mut rig = Rig::with_lookahead(
        vec![
            timed(stop_cue(1, 3, 0.0), 0.05, 0.0, DNC),
            memo(2),
            sound(3),
        ],
        100,
    );
    rig.start(0, 3);
    let go = rig.go(0);
    // The Stop cue's action starts on sample 50, inside this call's window.
    assert_eq!(voice_stops(&go), vec![(id(3), 50, None)]);
    assert_eq!(changes(&go, 3), vec![(CueState::Idle, 50)]);
}

#[test]
fn stop_cue_on_a_group_stops_every_descendant() {
    let mut rig = Rig::new(vec![
        stop_cue(1, 2, 0.0),
        grp(2, GroupMode::Timeline, vec![sound(3), wait(4, 5.0)]),
    ]);
    rig.start(0, 2);
    rig.set_playhead(100, 1);
    let go = rig.go(100);
    assert_eq!(voice_stops(&go), vec![(id(3), 100, None)]);
    for n in [2, 3, 4] {
        assert_eq!(rig.state(n), CueState::Idle);
    }
}

#[test]
fn stop_cue_inside_the_group_it_stops() {
    // A Stop cue that stops its own group stops itself too; its continue is cancelled.
    let mut rig = Rig::new(vec![
        grp(
            2,
            GroupMode::StartFirstAndGoToNext,
            vec![
                timed(sound(3), 0.0, 0.0, AC),
                timed(stop_cue(4, 2, 0.0), 1.0, 0.0, AC),
                memo(5),
            ],
        ),
        memo(6),
    ]);
    rig.start(0, 2);
    let tick = rig.tick(1000);
    assert_eq!(voice_stops(&tick), vec![(id(3), 1000, None)]);
    assert!(starts(&tick).is_empty());
    for n in [2, 3, 4, 5, 6] {
        assert_eq!(rig.state(n), CueState::Idle);
    }
}

// ----- Pause (CT-13) -----

#[test]
fn pause_cue_pauses_its_target() {
    let mut rig = Rig::new(vec![pause_cue(1, 3), memo(2), sound(3)]);
    rig.start(0, 3);
    let go = rig.go(400);
    assert!(go.iter().any(|a| matches!(
        a,
        Action::PauseVoice { cue, at: 400, offset: 400, .. } if *cue == id(3)
    )));
    assert_eq!(rig.state(3), CueState::Paused);
    // A Pause cue on a paused (or idle) target does nothing.
    rig.set_playhead(500, 1);
    let go = rig.go(500);
    assert!(go.iter().all(|a| !matches!(a, Action::PauseVoice { .. })));
    assert_eq!(rig.state(3), CueState::Paused);
}

#[test]
fn pause_cue_on_a_group_pauses_its_descendants() {
    let mut rig = Rig::new(vec![
        pause_cue(1, 2),
        grp(2, GroupMode::Timeline, vec![sound(3), wait(4, 5.0)]),
    ]);
    rig.start(0, 2);
    rig.set_playhead(100, 1);
    rig.go(100);
    for n in [2, 3, 4] {
        assert_eq!(rig.state(n), CueState::Paused);
    }
    // Resuming the group later keeps the Wait's remaining time.
    rig.at(1100, Input::Resume { cue: id(2) });
    assert_eq!(changes(&rig.tick(5999), 4), vec![]);
    assert_eq!(changes(&rig.tick(6000), 4), vec![(CueState::Idle, 6000)]);
}

// ----- Load (CT-14) -----

#[test]
fn load_cue_loads_its_target_to_its_time() {
    let mut rig = Rig::new(vec![
        load_cue(1, 3, 2.5),
        memo(2),
        timed(sound(3), 1.0, 0.0, DNC),
    ]);
    let go = rig.go(0);
    assert!(go.iter().any(|a| matches!(
        a,
        Action::LoadVoice { cue, offset: 1500, .. } if *cue == id(3)
    )));
    assert_eq!(rig.state(3), CueState::Loaded);
    let start = rig.start(100, 3);
    assert!(start.iter().any(|a| matches!(
        a,
        Action::StartVoice { cue, at: 100, offset: 1500, .. } if *cue == id(3)
    )));
}

#[test]
fn load_cue_on_an_active_or_broken_target_warns() {
    let mut rig = Rig::new(vec![load_cue(1, 3, 0.0), memo(2), sound(3)]);
    rig.start(0, 3);
    assert_eq!(
        warnings(&rig.go(10)),
        vec![EngineWarning::LoadIgnored { cue: id(3) }]
    );
    rig.failed(20, 3);
    rig.set_playhead(30, 1);
    assert_eq!(
        warnings(&rig.go(30)),
        vec![EngineWarning::CueBroken { cue: id(3) }]
    );
}

#[test]
fn load_cue_on_a_group_loads_its_first_children() {
    let mut rig = Rig::new(vec![
        load_cue(1, 2, 0.0),
        grp(
            2,
            GroupMode::StartFirstAndGoToNext,
            vec![sound(3), sound(4)],
        ),
    ]);
    rig.go(0);
    assert_eq!(rig.state(2), CueState::Loaded);
    assert_eq!(rig.state(3), CueState::Loaded);
    assert_eq!(rig.state(4), CueState::Idle);
}

// ----- Reset (CT-15) -----

#[test]
fn reset_cue_resets_running_and_broken_targets() {
    let mut rig = Rig::new(vec![reset_cue(1, 3), memo(2), sound(3)]);
    rig.start(0, 3);
    let go = rig.go(100);
    assert_eq!(voice_stops(&go), vec![(id(3), 100, None)]);
    assert_eq!(rig.state(3), CueState::Idle);
    rig.start(200, 3);
    rig.failed(210, 3);
    rig.set_playhead(300, 1);
    rig.go(300);
    assert_eq!(rig.state(3), CueState::Idle);
    assert!(rig.engine.broken_reason(id(3)).is_none());
}

#[test]
fn reset_cue_on_a_group_resets_its_descendants() {
    let mut rig = Rig::new(vec![
        reset_cue(1, 2),
        grp(2, GroupMode::Timeline, vec![sound(3), sound(4)]),
    ]);
    rig.start(0, 2);
    rig.failed(10, 3);
    rig.set_playhead(100, 1);
    rig.go(100);
    for n in [2, 3, 4] {
        assert_eq!(rig.state(n), CueState::Idle);
    }
}

// ----- Wait (CT-20) and Memo (CT-21) -----

#[test]
fn wait_lasts_its_duration_and_memo_completes_at_once() {
    let mut rig = Rig::new(vec![
        timed(wait(1, 2.5), 0.0, 0.0, AF),
        timed(memo(2), 0.0, 0.0, AF),
        sound(3),
    ]);
    let go = rig.go(0);
    assert_eq!(starts(&go), vec![(id(1), 0)]);
    rig.tick(2499);
    assert_eq!(rig.state(1), CueState::Running);
    let tick = rig.tick(2500);
    assert_eq!(starts(&tick), vec![(id(2), 2500), (id(3), 2500)]);
    assert_eq!(voice_starts(&tick), vec![(id(3), 2500)]);
    assert_eq!(rig.state(2), CueState::Idle);
}

// ----- trigger cycles (runtime bound) -----

fn chain_limit_hit(actions: &[Action]) -> bool {
    warnings(actions)
        .iter()
        .any(|w| matches!(w, EngineWarning::ChainLimit { .. }))
}

#[test]
fn start_cycle_is_cut_within_the_call_and_never_refires() {
    let mut rig = Rig::new(vec![start_cue(1, 2), start_cue(2, 1), memo(3)]);
    let limit = CueEngine::chain_limit(&rig.ws);
    let go = rig.go(0);
    assert!(chain_limit_hit(&go));
    assert!(starts(&go).len() <= limit);
    assert!(rig.engine.next_deadline().is_none());
    for n in [1, 2] {
        assert_eq!(rig.state(n), CueState::Idle);
    }
    // Nothing carries over into later calls.
    for now in [0, 1, 1000, 5000] {
        assert!(starts(&rig.tick(now)).is_empty());
    }
}

#[test]
fn a_start_cue_targeting_itself_is_broken() {
    let mut rig = Rig::new(vec![start_cue(1, 1), memo(2)]);
    assert_eq!(rig.engine.problems(id(1)), &[CueProblem::TargetSelf]);
    let go = rig.go(0);
    assert_eq!(warnings(&go), vec![EngineWarning::CueBroken { cue: id(1) }]);
    assert_eq!(rig.state(1), CueState::Broken);
    assert_eq!(rig.playhead(), Some(id(2)));
}

#[test]
fn a_start_cue_retargeted_to_itself_in_its_pre_wait_does_not_act() {
    // Broken while it pre-waits: its action is skipped when it comes (M2.8).
    let mut rig = Rig::new(vec![timed(start_cue(1, 2), 1.0, 0.0, DNC), memo(2)]);
    rig.go(0);
    rig.find_mut(1).kind.set_target(Some(id(1)));
    let edit = rig.tick(10);
    assert!(changes(&edit, 1).is_empty(), "keeps its runtime state");
    assert_eq!(rig.state(1), CueState::PreWaiting);
    let tick = rig.tick(1000);
    assert_eq!(changes(&tick, 1), vec![(CueState::Broken, 1000)]);
    assert!(starts(&tick).is_empty());
    assert!(warnings(&tick).is_empty());
    assert!(rig.engine.next_deadline().is_none());
    assert_eq!(
        rig.engine.cue_states().collect::<Vec<_>>(),
        vec![(id(1), CueState::Broken)]
    );
}

#[test]
fn goto_cycle_is_cut_within_the_call_and_never_refires() {
    let mut rig = Rig::new(vec![goto_cue(1, 2, true), goto_cue(2, 1, true), memo(3)]);
    let limit = CueEngine::chain_limit(&rig.ws);
    let go = rig.go(0);
    assert!(chain_limit_hit(&go));
    assert!(starts(&go).len() <= limit);
    assert!(rig.engine.next_deadline().is_none());
    for now in [0, 10, 10_000] {
        assert!(starts(&rig.tick(now)).is_empty());
    }
}

#[test]
fn start_cycle_with_time_in_it_runs_one_lap_per_period() {
    // Not a hang: each lap takes a second, so every call does a bounded amount of work.
    let mut rig = Rig::new(vec![
        timed(start_cue(1, 2), 1.0, 0.0, DNC),
        timed(start_cue(2, 1), 1.0, 0.0, DNC),
    ]);
    rig.go(0);
    for lap in 1..=20u64 {
        let tick = rig.tick(lap * 1000);
        assert_eq!(starts(&tick).len(), 1, "lap {lap}");
        assert!(!chain_limit_hit(&tick));
        assert_eq!(rig.engine.next_deadline(), Some((lap + 1) * 1000));
    }
}

#[test]
fn a_cycle_through_audio_feedback_moves_with_the_mixer_clock() {
    // A zero-length sound restarted by a Start cue on its auto-follow: every lap needs the
    // mixer's end report, which is dated at the voice's real start, so time moves on.
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 0.0, AF), start_cue(2, 1)]);
    rig.go(0);
    let mut now = 0;
    for _ in 0..10 {
        now += 5;
        let at = match rig.state(1) {
            CueState::Running => now - 5,
            s => panic!("{s:?}"),
        };
        let ended = rig.ended(now, 1, at);
        let started = voice_starts(&ended);
        assert_eq!(started.len(), 1);
        assert!(started[0].1 >= now);
        assert!(!chain_limit_hit(&ended));
    }
}

#[test]
fn a_cycle_through_feedback_on_its_own_sample_is_cut() {
    // Regression (M2.9 property seed): a zero-length voice is reported ended on its start
    // sample, and the binary may hand that report over at the same `now`. The chain budget
    // spans every call at one sample, so the lap cannot repeat there forever.
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 0.0, AF), start_cue(2, 1)]);
    let limit = CueEngine::chain_limit(&rig.ws);
    rig.go(0);
    let mut laps = 0;
    let mut cut = false;
    while rig.state(1) == CueState::Running {
        laps += 1;
        assert!(
            laps <= limit,
            "still looping after {laps} end reports at one sample"
        );
        cut |= chain_limit_hit(&rig.ended(0, 1, 0));
    }
    assert!(cut);
    assert!(rig.engine.next_deadline().is_none());
    // A later sample has a fresh budget.
    let go = rig.start(1, 1);
    assert_eq!(voice_starts(&go), vec![(id(1), 1)]);
}

#[test]
fn ignored_triggers_do_not_use_up_the_chain_budget_of_their_sample() {
    // Review M1: the budget is shared by every call at one `now` (one audio block), so a stuck
    // controller key repeating Start on a playing cue must not leave the operator's GO in that
    // block with `GoIgnored { chain_limit }`. Starts of unknown cues cost nothing either.
    let mut rig = Rig::new(vec![sound(1), memo(2), memo(3)]);
    let limit = CueEngine::chain_limit(&rig.ws);
    rig.start(400, 1);
    rig.set_playhead(400, 2);
    for _ in 0..limit + 5 {
        let again = rig.start(500, 1);
        assert!(matches!(
            warnings(&again).as_slice(),
            [EngineWarning::TriggerIgnored { .. }]
        ));
    }
    for _ in 0..limit + 5 {
        rig.at(500, Input::Start { cue: id(99) });
    }
    let go = rig.go(500);
    assert_eq!(starts(&go), vec![(id(2), 500)], "{go:?}");
}

// ----- GoTo (CT-17) -----

#[test]
fn goto_moves_the_playhead_without_starting_anything() {
    let mut rig = Rig::new(vec![goto_cue(1, 4, false), memo(2), memo(3), sound(4)]);
    let go = rig.go(0);
    assert_eq!(starts(&go), vec![(id(1), 0)]);
    assert_eq!(rig.playhead(), Some(id(4)));
}

#[test]
fn goto_and_go_starts_the_target_and_moves_on() {
    let mut rig = Rig::new(vec![
        goto_cue(1, 3, true),
        memo(2),
        timed(sound(3), 0.0, 0.0, AF),
        memo(4),
        memo(5),
    ]);
    let go = rig.go(0);
    assert_eq!(voice_starts(&go), vec![(id(3), 0)]);
    // The GO skipped the target's continue chain (3 → 4).
    assert_eq!(rig.playhead(), Some(id(5)));
}

#[test]
fn goto_and_go_is_not_an_operator_go() {
    // QLab: a GoTo cue's GO is not blocked by double-GO protection and does not restart it.
    let mut rig = Rig::new(vec![
        timed(goto_cue(1, 3, true), 0.6, 0.0, DNC),
        memo(2),
        memo(3),
        memo(4),
        memo(5),
    ]);
    rig.ws.settings.min_go_interval = 1.0;
    let go = rig.go(0);
    assert_eq!(starts(&go), vec![(id(1), 0)]);
    // The GoTo's GO at 600 is inside the interval of the operator GO at 0: it still happens.
    let tick = rig.tick(600);
    assert_eq!(starts(&tick), vec![(id(3), 600)]);
    assert_eq!(rig.playhead(), Some(id(4)));
    // The interval still counts from the operator GO at 0, not from 600.
    assert!(rig.go(999).contains(&Action::GoIgnored {
        list: list(),
        reason: GoIgnoredReason::DoubleGo
    }));
    assert_eq!(starts(&rig.go(1000)), vec![(id(4), 1000)]);
}

#[test]
fn goto_into_a_group_stands_on_the_child() {
    let mut rig = Rig::new(vec![
        goto_cue(1, 4, true),
        grp(2, GroupMode::Timeline, vec![sound(3), sound(4), sound(5)]),
        memo(6),
    ]);
    let go = rig.go(0);
    assert_eq!(voice_starts(&go), vec![(id(4), 0)]);
    // The group does not run (M2.24, QLab check G4); the playhead goes to the next sibling
    // (timeline children do not chain).
    assert_eq!(rig.state(2), CueState::Idle);
    assert_eq!(rig.playhead(), Some(id(5)));
    // Without GO it just stands on the child.
    let mut rig = Rig::new(vec![
        goto_cue(1, 4, false),
        grp(2, GroupMode::Timeline, vec![sound(3), sound(4)]),
    ]);
    rig.go(0);
    assert_eq!(rig.playhead(), Some(id(4)));
    assert_eq!(rig.state(4), CueState::Idle);
}

#[test]
fn goto_across_lists_moves_and_goes_on_the_targets_list() {
    let mut rig = Rig::new(vec![goto_cue(1, 21, true), memo(2)]);
    rig.ws.cue_lists.push(CueList {
        id: list_id(2),
        name: "Second".into(),
        cues: vec![memo(20), sound(21), memo(22)],
    });
    rig.tick(0);
    assert_eq!(rig.engine.playhead(list_id(2)), Some(id(20)));
    let go = rig.go(0);
    assert_eq!(voice_starts(&go), vec![(id(21), 0)]);
    assert_eq!(rig.engine.playhead(list_id(2)), Some(id(22)));
    assert_eq!(rig.playhead(), Some(id(2)));
}

#[test]
fn goto_with_a_missing_target_warns() {
    let mut rig = Rig::new(vec![goto_cue(1, 99, true), memo(2)]);
    let go = rig.go(0);
    assert_eq!(warnings(&go), vec![EngineWarning::CueBroken { cue: id(1) }]);
    assert_eq!(rig.playhead(), Some(id(2)));
}

// ----- Arm / Disarm (CT-19) -----

#[test]
fn disarm_is_seen_by_the_rest_of_the_same_chain() {
    let mut rig = Rig::new(vec![timed(arm_cue(1, 2, false), 0.0, 0.0, AC), sound(2)]);
    let go = rig.go(0);
    assert_eq!(set_armed(&go), vec![(id(2), false)]);
    // Cue 2 was triggered by the auto-continue in the same call and skipped (no voice).
    assert_eq!(starts(&go), vec![(id(1), 0), (id(2), 0)]);
    assert!(voice_starts(&go).is_empty());
}

#[test]
fn disarm_lasts_for_the_call_then_the_model_rules() {
    let mut rig = Rig::new(vec![
        timed(arm_cue(1, 3, false), 0.0, 0.0, AC),
        timed(load_cue(2, 3, 0.0), 0.0, 0.0, AC),
        sound(3),
    ]);
    // In the same chain the Load and the start see cue 3 disarmed: no LoadVoice, no voice.
    let go = rig.go(0);
    assert_eq!(set_armed(&go), vec![(id(3), false)]);
    assert!(go.iter().all(|a| !matches!(a, Action::LoadVoice { .. })));
    assert!(voice_starts(&go).is_empty());
    // The binary applies it before the next call; the engine and the model agree.
    rig.find_mut(3).armed = false;
    assert!(voice_starts(&rig.start(400, 3)).is_empty());
    rig.stop(450, 3);
    // An operator edit that arms the cue again counts at once.
    rig.find_mut(3).armed = true;
    assert_eq!(voice_starts(&rig.start(500, 3)), vec![(id(3), 500)]);
}

#[test]
fn arming_an_armed_cue_does_nothing() {
    let mut rig = Rig::new(vec![arm_cue(1, 2, true), sound(2)]);
    let go = rig.go(0);
    assert!(set_armed(&go).is_empty());
    assert!(warnings(&go).is_empty());
}

#[test]
fn arm_cue_on_a_group_changes_where_go_enters_it() {
    // Arm, then a GoTo-and-GO onto the group in the same chain: GO enters it.
    let mut g = grp(3, GroupMode::StartFirstAndEnter, vec![sound(4), sound(5)]);
    g.armed = false;
    let mut rig = Rig::new(vec![
        timed(arm_cue(1, 3, true), 0.0, 0.0, AC),
        goto_cue(2, 3, true),
        g,
        memo(6),
    ]);
    let go = rig.go(0);
    assert_eq!(set_armed(&go), vec![(id(3), true)]);
    assert_eq!(voice_starts(&go), vec![(id(4), 0)]);
    assert_eq!(rig.playhead(), Some(id(5)));
}

#[test]
fn arm_cue_with_a_missing_target_warns() {
    let mut rig = Rig::new(vec![arm_cue(1, 99, true)]);
    assert_eq!(
        rig.engine.problems(id(1)),
        &[CueProblem::TargetMissing { target: id(99) }]
    );
    let go = rig.go(0);
    assert_eq!(warnings(&go), vec![EngineWarning::CueBroken { cue: id(1) }]);
    assert!(set_armed(&go).is_empty());
}

// ----- Devamp (CT-16) -----

/// A looping sound (1), the Devamp cue (2) and the cue after it (3).
fn devamp_rig(start_next: bool, stop: bool, lookahead: SampleTime) -> Rig {
    let mut rig = Rig::with_lookahead(
        vec![
            sound(1),
            devamp_cue(2, 1, start_next, stop),
            sound(3),
            memo(4),
        ],
        lookahead,
    );
    rig.start(0, 1);
    rig.set_playhead(0, 2);
    rig
}

#[test]
fn devamp_starts_the_cue_after_it_on_the_exact_devamp_sample() {
    let mut rig = devamp_rig(true, false, 64);
    let go = rig.go(100);
    assert_eq!(devamps(&go), vec![(id(1), 100, false)]);
    let run = rig.run(1);
    let scheduled = rig.at(
        150,
        Input::DevampScheduled {
            cue: id(1),
            run,
            at: 5000,
        },
    );
    assert!(starts(&scheduled).is_empty());
    assert_eq!(rig.engine.next_deadline(), Some(5000));
    assert!(starts(&rig.tick(4000)).is_empty());
    // The window of the call at 4950 holds sample 5000: the next cue starts exactly there.
    let tick = rig.tick(4950);
    assert_eq!(voice_starts(&tick), vec![(id(3), 5000)]);
    // The looping sound plays on into its next slice.
    assert_eq!(rig.state(1), CueState::Running);
    // Its later end completes it as usual.
    rig.ended(9000, 1, 8000);
    assert_eq!(rig.state(1), CueState::Idle);
}

#[test]
fn devamp_without_start_next_starts_nothing_and_passes_stop() {
    let mut rig = devamp_rig(false, true, 0);
    let go = rig.go(100);
    assert_eq!(devamps(&go), vec![(id(1), 100, true)]);
    let run = rig.run(1);
    rig.at(
        150,
        Input::DevampScheduled {
            cue: id(1),
            run,
            at: 5000,
        },
    );
    assert!(starts(&rig.tick(5000)).is_empty());
    assert!(rig.engine.next_deadline().is_none());
}

#[test]
fn devamp_ignored_by_the_audio_side_warns_and_starts_nothing() {
    // QLab: a devamp of a cue that is not looping has nothing to leave.
    let mut rig = devamp_rig(true, false, 0);
    rig.go(100);
    let run = rig.run(1);
    let ignored = rig.at(150, Input::DevampIgnored { cue: id(1), run });
    assert_eq!(
        warnings(&ignored),
        vec![EngineWarning::DevampIgnored { cue: id(1) }]
    );
    assert!(rig.engine.next_deadline().is_none());
    assert!(starts(&rig.tick(10_000)).is_empty());
    // Stale feedback is ignored silently.
    let stale = rig.at(
        200,
        Input::DevampScheduled {
            cue: id(1),
            run: RunId(9_999),
            at: 300,
        },
    );
    assert!(stale.is_empty());
}

#[test]
fn devamp_feedback_after_the_voice_ended_still_starts_the_next_cue() {
    // The end report raced the deadline: the next cue starts on the devamp sample.
    let mut rig = devamp_rig(true, true, 0);
    rig.go(100);
    let run = rig.run(1);
    rig.at(
        150,
        Input::DevampScheduled {
            cue: id(1),
            run,
            at: 5000,
        },
    );
    let ended = rig.at(
        4000,
        Input::ActionEnded {
            cue: id(1),
            run,
            at: 4000,
        },
    );
    assert_eq!(starts(&ended), vec![(id(3), 4000)]);
}

#[test]
fn a_second_devamp_while_one_is_pending_is_ignored() {
    let mut rig = devamp_rig(true, false, 0);
    rig.go(100);
    rig.set_playhead(200, 2);
    let again = rig.go(200);
    assert!(devamps(&again).is_empty());
    assert_eq!(
        warnings(&again),
        vec![EngineWarning::DevampIgnored { cue: id(1) }]
    );
}

#[test]
fn devamp_of_a_target_that_is_not_playing_warns() {
    let not_running = EngineWarning::TargetNotRunning {
        cue: id(2),
        target: id(1),
    };
    // Idle.
    let mut rig = Rig::new(vec![sound(1), devamp_cue(2, 1, true, false), sound(3)]);
    rig.set_playhead(0, 2);
    let go = rig.go(0);
    assert_eq!(warnings(&go), vec![not_running.clone()]);
    assert!(devamps(&go).is_empty());
    // Pre-waiting, and paused in its pre-wait (a paused target in its action keeps the devamp:
    // see `a_devamp_of_a_paused_target_takes_effect_after_the_resume`).
    rig.find_mut(1).pre_wait = 1.0;
    rig.start(50, 1);
    rig.set_playhead(60, 2);
    assert_eq!(warnings(&rig.go(60)), vec![not_running.clone()]);
    rig.pause(62, 1);
    rig.set_playhead(64, 2);
    assert_eq!(warnings(&rig.go(64)), vec![not_running.clone()]);
    // Broken.
    rig.stop(70, 1);
    rig.find_mut(1).pre_wait = 0.0;
    rig.start(80, 1);
    rig.failed(90, 1);
    rig.set_playhead(100, 2);
    assert_eq!(warnings(&rig.go(100)), vec![not_running]);
}

#[test]
fn devamp_of_a_stopped_target_never_starts_the_next_cue() {
    let mut rig = devamp_rig(true, false, 0);
    rig.go(100);
    let run = rig.run(1);
    rig.at(
        150,
        Input::DevampScheduled {
            cue: id(1),
            run,
            at: 5000,
        },
    );
    rig.stop(2000, 1);
    assert!(starts(&rig.tick(5000)).is_empty());
}

#[test]
fn devamp_deadline_moves_with_a_pause() {
    let mut rig = devamp_rig(true, false, 0);
    rig.go(100);
    let run = rig.run(1);
    rig.at(
        150,
        Input::DevampScheduled {
            cue: id(1),
            run,
            at: 5000,
        },
    );
    rig.pause(1000, 1);
    assert!(rig.engine.next_deadline().is_none());
    // On resume the devamp is sent again for the resumed voice, which answers afresh.
    let resume = rig.at(3000, Input::Resume { cue: id(1) });
    assert_eq!(devamps(&resume), vec![(id(1), 3000, false)]);
    assert!(rig.engine.next_deadline().is_none());
    let run = rig.run(1);
    scheduled(&mut rig, 3010, run, 7000);
    assert_eq!(rig.engine.next_deadline(), Some(7000));
    assert_eq!(starts(&rig.tick(7000)), vec![(id(3), 7000)]);
}

#[test]
fn devamp_of_a_memo_is_the_wrong_kind() {
    let mut rig = Rig::new(vec![memo(1), devamp_cue(2, 1, true, false)]);
    assert_eq!(
        rig.engine.problems(id(2)),
        &[CueProblem::TargetWrongKind { target: id(1) }]
    );
    rig.set_playhead(0, 2);
    assert_eq!(
        warnings(&rig.go(0)),
        vec![EngineWarning::CueBroken { cue: id(2) }]
    );
}

// ----- Devamp of a playlist group (M2.15) -----

/// A looping playlist (1) of two one-second waits, auto-following to memo 4; the Devamp cue
/// (5) and the cue after it (6).
fn playlist_rig(looping: bool, start_next: bool) -> Rig {
    let mut rig = Rig::new(vec![
        timed(
            grp(1, playlist(looping), vec![wait(2, 1.0), wait(3, 1.0)]),
            0.0,
            0.0,
            AF,
        ),
        memo(4),
        devamp_cue(5, 1, start_next, false),
        memo(6),
    ]);
    rig.start(0, 1);
    rig.set_playhead(0, 5);
    rig
}

#[test]
fn devamp_of_a_looping_playlist_finishes_the_current_child_then_completes() {
    let mut rig = playlist_rig(true, true);
    // Second pass, first child.
    rig.tick(2000);
    assert_eq!(rig.state(2), CueState::Running);
    let go = rig.go(2500);
    assert!(devamps(&go).is_empty());
    assert!(warnings(&go).is_empty());
    let tick = rig.tick(3000);
    // The current child finished; the group completed instead of moving on: its auto-follow
    // fires and the Devamp cue's next cue starts, both on that sample.
    assert_eq!(starts(&tick), vec![(id(4), 3000), (id(6), 3000)]);
    assert_eq!(changes(&tick, 1), vec![(CueState::Idle, 3000)]);
    assert_eq!(rig.state(3), CueState::Idle);
    assert!(rig.engine.next_deadline().is_none());
}

#[test]
fn devamp_of_a_paused_looping_playlist_completes_on_resume() {
    let mut rig = playlist_rig(true, false);
    rig.go(500);
    rig.pause(600, 1);
    rig.at(2000, Input::Resume { cue: id(1) });
    let tick = rig.tick(2400);
    assert_eq!(starts(&tick), vec![(id(4), 2400)]);
    assert_eq!(rig.state(1), CueState::Idle);
}

#[test]
fn devamp_of_a_playlist_that_does_not_loop_is_ignored() {
    let mut rig = playlist_rig(false, true);
    assert_eq!(
        warnings(&rig.go(500)),
        vec![EngineWarning::DevampIgnored { cue: id(1) }]
    );
    // It plays on to its last child, then completes as usual.
    assert_eq!(starts(&rig.tick(1000)), vec![(id(3), 1000)]);
    assert_eq!(starts(&rig.tick(2000)), vec![(id(4), 2000)]);
}

#[test]
fn devamp_of_other_groups_or_idle_playlists() {
    // Only a playlist group is a valid Devamp target; any other group makes it broken.
    let mut rig = Rig::new(vec![
        grp(1, GroupMode::Timeline, vec![wait(2, 1.0)]),
        devamp_cue(3, 1, false, false),
        grp(4, playlist(true), vec![wait(5, 1.0)]),
        devamp_cue(6, 4, false, false),
    ]);
    assert_eq!(
        rig.engine.problems(id(3)),
        &[CueProblem::TargetWrongKind { target: id(1) }]
    );
    rig.set_playhead(0, 3);
    assert_eq!(
        warnings(&rig.go(0)),
        vec![EngineWarning::CueBroken { cue: id(3) }]
    );
    rig.set_playhead(0, 6);
    assert_eq!(
        warnings(&rig.go(0)),
        vec![EngineWarning::TargetNotRunning {
            cue: id(6),
            target: id(4)
        }]
    );
    // An edit that makes the target a timeline group breaks the Devamp cue at the next call.
    if let CueKind::Group { mode, .. } = &mut rig.find_mut(4).kind {
        *mode = GroupMode::Timeline;
    }
    let edit = rig.tick(10);
    assert_eq!(changes(&edit, 6), vec![(CueState::Broken, 10)]);
    assert!(edit.contains(&Action::ProblemsChanged {
        cue: id(6),
        problems: vec![CueProblem::TargetWrongKind { target: id(4) }],
    }));
}

// ----- review round 1 -----

fn voice_times(actions: &[Action]) -> Vec<(&'static str, SampleTime)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::PauseVoice { at, .. } => Some(("pause", *at)),
            Action::ResumeVoice { at, .. } => Some(("resume", *at)),
            Action::StopVoice { at, .. } => Some(("stop", *at)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_resume_is_never_dated_before_a_committed_pause() {
    // Review M1: the Pause cue's PauseVoice is committed for 1050.
    let mut rig = Rig::with_lookahead(vec![sound(1), timed(pause_cue(2, 1), 0.05, 0.0, DNC)], 100);
    rig.start(0, 1);
    assert_eq!(voice_times(&rig.start(1000, 2)), vec![("pause", 1050)]);
    let resume = rig.at(1010, Input::Resume { cue: id(1) });
    assert_eq!(voice_times(&resume), vec![("resume", 1050)]);
    assert!(
        resume
            .iter()
            .any(|a| matches!(a, Action::ResumeVoice { offset: 1050, .. }))
    );
    assert_eq!(rig.state(1), CueState::Running);
}

#[test]
fn a_stop_is_never_dated_before_a_committed_pause() {
    let mut rig = Rig::with_lookahead(vec![sound(1), timed(pause_cue(2, 1), 0.05, 0.0, DNC)], 100);
    rig.start(0, 1);
    rig.start(1000, 2);
    assert_eq!(voice_times(&rig.stop(1010, 1)), vec![("stop", 1050)]);
}

fn scheduled(rig: &mut Rig, now: SampleTime, run: RunId, at: SampleTime) -> Vec<Action> {
    rig.at(
        now,
        Input::DevampScheduled {
            cue: id(1),
            run,
            at,
        },
    )
}

#[test]
fn a_pending_devamp_is_sent_again_for_the_resumed_voice() {
    // Review M2: the answer for the voice before the pause is stale after the resume.
    let mut rig = devamp_rig(true, false, 0);
    rig.go(100);
    let old = rig.run(1);
    rig.pause(110, 1);
    let resume = rig.at(120, Input::Resume { cue: id(1) });
    let fresh = rig.run(1);
    assert_ne!(old, fresh);
    assert!(resume.iter().any(|a| matches!(
        a,
        Action::DevampVoice { run, at: 120, stop: false, .. } if *run == fresh
    )));
    assert!(scheduled(&mut rig, 130, old, 5000).is_empty());
    assert!(rig.engine.next_deadline().is_none());
    scheduled(&mut rig, 140, fresh, 6000);
    assert_eq!(starts(&rig.tick(6000)), vec![(id(3), 6000)]);
}

#[test]
fn a_devamp_answered_before_a_pause_is_sent_again_on_resume() {
    let mut rig = devamp_rig(true, true, 0);
    rig.go(100);
    let old = rig.run(1);
    scheduled(&mut rig, 110, old, 5000);
    rig.pause(1000, 1);
    let resume = rig.at(2000, Input::Resume { cue: id(1) });
    let fresh = rig.run(1);
    assert_eq!(devamps(&resume), vec![(id(1), 2000, true)]);
    // The old answer no longer counts: nothing is due until the new one comes.
    assert!(rig.engine.next_deadline().is_none());
    scheduled(&mut rig, 2010, fresh, 7000);
    assert_eq!(starts(&rig.tick(7000)), vec![(id(3), 7000)]);
}

#[test]
fn a_devamp_of_a_paused_target_takes_effect_after_the_resume() {
    // User decision 2026-10-25: a Devamp fired at a paused cue is kept, neither ignored nor a
    // resume. Core holds it and sends it once, for the resumed segment on the resume sample, so
    // the cue finishes the loop pass it was paused in and then leaves the loop.
    let mut rig = devamp_rig(true, false, 0);
    let paused = rig.pause(100, 1);
    let old = rig.run(1);
    assert!(devamps(&paused).is_empty());
    let go = rig.go(200);
    assert!(warnings(&go).is_empty(), "{:?}", warnings(&go));
    assert!(
        devamps(&go).is_empty(),
        "nothing is sent to the paused segment"
    );
    assert_eq!(rig.state(1), CueState::Paused, "a devamp is not a resume");
    assert!(rig.engine.next_deadline().is_none());
    // A second Devamp while it is pending is ignored, as for a playing target.
    rig.set_playhead(250, 2);
    assert_eq!(
        warnings(&rig.go(250)),
        vec![EngineWarning::DevampIgnored { cue: id(1) }]
    );
    let resume = rig.at(1000, Input::Resume { cue: id(1) });
    let fresh = rig.run(1);
    assert_ne!(old, fresh);
    // The resume, then the devamp for the resumed segment, both on the resume sample.
    let voice: Vec<&Action> = resume
        .iter()
        .filter(|a| matches!(a, Action::ResumeVoice { .. } | Action::DevampVoice { .. }))
        .collect();
    assert!(
        matches!(
            voice.as_slice(),
            [
                Action::ResumeVoice { run: r, paused: p, at: 1000, .. },
                Action::DevampVoice { run: d, at: 1000, stop: false, .. },
            ] if *r == fresh && *p == old && *d == fresh
        ),
        "{voice:?}"
    );
    // The answer (the loop is left at the end of the current pass) starts the next cue there.
    scheduled(&mut rig, 1010, fresh, 4000);
    assert_eq!(starts(&rig.tick(4000)), vec![(id(3), 4000)]);
}

#[test]
fn a_devamp_of_a_target_whose_pause_is_committed_is_never_dated_before_it() {
    // With a lookahead the pause is committed ahead of `now`; the devamp that comes meanwhile
    // goes out after the resume, so nothing for the voice goes back in time.
    let mut rig = devamp_rig(false, true, 64);
    rig.tick(1000);
    let pause = rig.pause(1000, 1);
    let paused_at = pause
        .iter()
        .find_map(|a| match a {
            Action::PauseVoice { at, .. } => Some(*at),
            _ => None,
        })
        .unwrap();
    let go = rig.go(1010);
    assert!(devamps(&go).is_empty());
    let resume = rig.at(2000, Input::Resume { cue: id(1) });
    let sent = devamps(&resume);
    assert_eq!(sent, vec![(id(1), 2000, true)]);
    assert!(sent[0].1 >= paused_at);
}

#[test]
fn a_devamp_kept_for_a_paused_target_is_dropped_by_a_stop() {
    let mut rig = devamp_rig(true, false, 0);
    rig.pause(100, 1);
    rig.go(200);
    rig.stop(300, 1);
    rig.start(400, 1);
    // A fresh run: nothing pending from before the stop.
    assert!(rig.engine.next_deadline().is_none());
    let tick = rig.tick(10_000);
    assert!(devamps(&tick).is_empty());
    assert!(starts(&tick).is_empty());
}

#[test]
fn a_devamped_target_stopped_while_paused_starts_nothing() {
    let mut rig = devamp_rig(true, false, 0);
    rig.go(100);
    let old = rig.run(1);
    scheduled(&mut rig, 110, old, 5000);
    rig.pause(1000, 1);
    rig.stop(2000, 1);
    assert!(rig.engine.next_deadline().is_none());
    assert!(starts(&rig.tick(10_000)).is_empty());
}

#[test]
fn go_on_a_devamp_with_start_next_stands_on_the_cue_it_will_start() {
    // M2.24 (QLab check D5, superseding review M3): the playhead stands on the cue the Devamp
    // cue will start when the loop is left.
    let mut rig = devamp_rig(true, false, 0);
    rig.go(100);
    assert_eq!(rig.playhead(), Some(id(3)));
    // Without "start next cue" the next cue stands by as usual.
    let mut rig = devamp_rig(false, false, 0);
    rig.go(100);
    assert_eq!(rig.playhead(), Some(id(3)));
}

#[test]
fn start_next_supersedes_the_devamp_cues_own_continue() {
    // Review M4: the next cue starts once, on the devamp sample, not also at the action start.
    let mut rig = Rig::new(vec![
        sound(1),
        timed(devamp_cue(2, 1, true, false), 0.0, 0.0, AC),
        memo(3),
        memo(4),
    ]);
    rig.start(0, 1);
    rig.set_playhead(0, 2);
    let go = rig.go(100);
    assert_eq!(starts(&go), vec![(id(2), 100)]);
    let run = rig.run(1);
    scheduled(&mut rig, 150, run, 5000);
    assert_eq!(starts(&rig.tick(5000)), vec![(id(3), 5000)]);
    // Without "start next cue" its own continue applies.
    let mut rig = Rig::new(vec![
        sound(1),
        timed(devamp_cue(2, 1, false, false), 0.0, 0.0, AF),
        memo(3),
    ]);
    rig.start(0, 1);
    rig.set_playhead(0, 2);
    assert_eq!(starts(&rig.go(100)), vec![(id(2), 100), (id(3), 100)]);
}

#[test]
fn an_operator_edit_after_a_disarm_counts_on_the_next_call() {
    // Review L2: the binary applies SetArmed before the next call, so the override ends there;
    // a user edit flipping `armed` back right after is honoured.
    let mut rig = Rig::new(vec![arm_cue(1, 2, false), sound(2)]);
    assert_eq!(set_armed(&rig.go(0)), vec![(id(2), false)]);
    rig.find_mut(2).armed = false; // the binary applies it
    rig.find_mut(2).armed = true; // the operator arms it again before the next call
    assert_eq!(voice_starts(&rig.start(100, 2)), vec![(id(2), 100)]);
}
