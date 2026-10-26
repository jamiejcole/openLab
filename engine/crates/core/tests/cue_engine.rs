// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Cue engine behaviour (docs/cue-engine.md), driven by a virtual sample clock. The rate is
//! 1 kHz, so one second is 1000 samples.

mod common;

use common::*;
use openlab_core::*;

const RATE: u32 = 1000;

fn config(lookahead: SampleTime) -> EngineConfig {
    EngineConfig::new(RATE, lookahead, 1).unwrap()
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

/// Sets pre-wait, post-wait and continue mode.
fn timed(mut c: Cue, pre: f64, post: f64, mode: ContinueMode) -> Cue {
    c.pre_wait = pre;
    c.post_wait = post;
    c.continue_mode = mode;
    c
}

use ContinueMode::{AutoContinue as AC, AutoFollow as AF, DoNotContinue as DNC};

fn id(n: u128) -> CueId {
    cue_id(n)
}

fn list() -> CueListId {
    list_id(1)
}

struct Rig {
    ws: Workspace,
    engine: CueEngine,
    /// The workspace as the engine last saw it (see `handle_as_binary`).
    last: Option<Workspace>,
}

impl Rig {
    fn new(cues: Vec<Cue>) -> Self {
        Self::with_config(cues, config(0))
    }

    fn with_config(cues: Vec<Cue>, config: EngineConfig) -> Self {
        let mut rig = Self {
            ws: workspace(cues),
            engine: CueEngine::new(config),
            last: None,
        };
        rig.at(0, Input::Tick);
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

    fn ended(&mut self, now: SampleTime, n: u128, at: SampleTime) -> Vec<Action> {
        let run = self.engine.run_of(id(n)).unwrap();
        self.at(
            now,
            Input::ActionEnded {
                cue: id(n),
                run,
                at,
            },
        )
    }

    fn state(&self, n: u128) -> CueState {
        self.engine.state(id(n))
    }

    fn playhead(&self) -> Option<CueId> {
        self.engine.playhead(list())
    }

    fn cues_mut(&mut self) -> &mut Vec<Cue> {
        &mut self.ws.cue_lists[0].cues
    }
}

/// `(cue, at)` of every StartVoice.
fn voice_starts(actions: &[Action]) -> Vec<(CueId, SampleTime)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::StartVoice { cue, at, .. } => Some((*cue, *at)),
            _ => None,
        })
        .collect()
}

/// `(state, at)` of every StateChanged for cue `n`.
fn changes(actions: &[Action], n: u128) -> Vec<(CueState, SampleTime)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::StateChanged { cue, state, at } if *cue == id(n) => Some((*state, *at)),
            _ => None,
        })
        .collect()
}

/// The sample cue `n`'s action started on, from its StateChanged to Running.
fn running_at(actions: &[Action], n: u128) -> Option<SampleTime> {
    changes(actions, n)
        .into_iter()
        .find(|(s, _)| *s == CueState::Running)
        .map(|(_, at)| at)
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

fn is_voice_action(a: &Action) -> bool {
    matches!(
        a,
        Action::LoadVoice { .. }
            | Action::UnloadVoice { .. }
            | Action::StartVoice { .. }
            | Action::StopVoice { .. }
            | Action::PauseVoice { .. }
            | Action::ResumeVoice { .. }
    )
}

// ----- timing (CUE-02) -----

#[test]
fn action_starts_on_the_exact_sample_the_pre_wait_ends() {
    let mut rig = Rig::new(vec![timed(sound(1), 1.5, 0.0, DNC)]);
    let go = rig.go(100);
    assert_eq!(changes(&go, 1), vec![(CueState::PreWaiting, 100)]);
    assert!(voice_starts(&go).is_empty());
    assert_eq!(rig.engine.next_deadline(), Some(1600));

    assert!(rig.tick(1599).is_empty());
    assert_eq!(rig.state(1), CueState::PreWaiting);

    let tick = rig.tick(1600);
    assert_eq!(voice_starts(&tick), vec![(id(1), 1600)]);
    assert_eq!(changes(&tick, 1), vec![(CueState::Running, 1600)]);
    assert!(warnings(&tick).is_empty());
    // Audio has no known duration: nothing more until feedback.
    assert_eq!(rig.engine.next_deadline(), None);
}

#[test]
fn lookahead_commits_a_deadline_inside_the_window_on_its_own_sample() {
    let mut rig = Rig::with_config(vec![timed(sound(1), 1.5, 0.0, DNC)], config(16));
    rig.go(100);
    assert!(rig.tick(1583).is_empty());
    let tick = rig.tick(1590);
    assert_eq!(voice_starts(&tick), vec![(id(1), 1600)]);
    assert!(warnings(&tick).is_empty());
}

#[test]
fn a_changed_lookahead_applies_from_the_next_call() {
    let mut rig = Rig::with_config(vec![timed(sound(1), 1.5, 0.0, DNC)], config(16));
    rig.go(100);
    // The window grows to 64: a tick at 1540 now commits the start on its sample.
    rig.engine.set_lookahead(64).unwrap();
    assert_eq!(rig.engine.config().lookahead(), 64);
    assert!(rig.engine.is_due(1540));
    let tick = rig.tick(1540);
    assert_eq!(voice_starts(&tick), vec![(id(1), 1600)]);
    assert!(warnings(&tick).is_empty());
    // Over one second: refused, nothing changes.
    assert!(rig.engine.set_lookahead(RATE as u64 + 1).is_err());
    assert_eq!(rig.engine.config().lookahead(), 64);
}

#[test]
fn late_tick_starts_the_voice_now_and_reports_how_late() {
    let mut rig = Rig::new(vec![timed(sound(1), 1.0, 0.0, DNC)]);
    rig.go(0);
    let tick = rig.tick(1250);
    assert_eq!(voice_starts(&tick), vec![(id(1), 1250)]);
    // The state change belongs to the deadline, not to the tick.
    assert_eq!(changes(&tick, 1), vec![(CueState::Running, 1000)]);
    assert_eq!(
        warnings(&tick),
        vec![EngineWarning::LateStart {
            cue: id(1),
            late_by: 250
        }]
    );
}

#[test]
fn late_tick_does_not_drift_later_cues() {
    let cues = || {
        vec![
            timed(wait(1, 1.0), 1.0, 2.0, AC),
            timed(wait(2, 0.25), 0.5, 1.0, AF),
            sound(3),
        ]
    };
    // Ticked on every deadline.
    let mut exact = Rig::new(cues());
    let mut log = exact.go(0);
    while let Some(d) = exact.engine.next_deadline() {
        log.extend(exact.tick(d));
    }
    // One tick far too late.
    let mut late = Rig::new(cues());
    let mut late_log = late.go(0);
    late_log.extend(late.tick(10_000));

    for n in 1..=3 {
        assert_eq!(changes(&log, n), changes(&late_log, n), "cue {n}");
    }
    // A starts acting at 1000; B is triggered at 3000, acts at 3500 for 250; C follows at 3750.
    assert_eq!(running_at(&late_log, 2), Some(3500));
    assert_eq!(
        changes(&late_log, 3),
        vec![(CueState::PreWaiting, 3750), (CueState::Running, 3750)]
    );
    assert_eq!(voice_starts(&log), vec![(id(3), 3750)]);
    assert_eq!(voice_starts(&late_log), vec![(id(3), 10_000)]);
}

#[test]
fn wait_cue_runs_for_its_duration() {
    let mut rig = Rig::new(vec![timed(wait(1, 2.0), 0.5, 0.0, DNC)]);
    rig.go(0);
    assert_eq!(running_at(&rig.tick(500), 1), Some(500));
    assert_eq!(rig.engine.next_deadline(), Some(2500));
    assert_eq!(changes(&rig.tick(2500), 1), vec![(CueState::Idle, 2500)]);
}

#[test]
fn memo_completes_at_once() {
    let mut rig = Rig::new(vec![memo(1)]);
    let go = rig.go(10);
    assert_eq!(
        changes(&go, 1),
        vec![
            (CueState::PreWaiting, 10),
            (CueState::Running, 10),
            (CueState::Idle, 10)
        ]
    );
    assert!(go.iter().all(|a| !is_voice_action(a)));
}

// ----- continue modes (CUE-03) -----

#[test]
fn auto_continue_triggers_the_next_cue_when_the_post_wait_elapses() {
    let mut rig = Rig::new(vec![timed(sound(1), 1.0, 2.0, AC), sound(2)]);
    rig.go(0);
    rig.tick(1000);
    assert_eq!(rig.engine.next_deadline(), Some(3000));
    assert!(rig.tick(2999).is_empty());
    let tick = rig.tick(3000);
    assert_eq!(voice_starts(&tick), vec![(id(2), 3000)]);
    // A is still playing; its post-wait is done.
    assert_eq!(rig.state(1), CueState::Running);
}

#[test]
fn auto_continue_without_post_wait_triggers_at_action_start() {
    let mut rig = Rig::new(vec![
        timed(sound(1), 1.0, 0.0, AC),
        timed(sound(2), 0.5, 0.0, DNC),
    ]);
    rig.go(0);
    let tick = rig.tick(1000);
    assert_eq!(voice_starts(&tick), vec![(id(1), 1000)]);
    assert_eq!(changes(&tick, 2), vec![(CueState::PreWaiting, 1000)]);
    assert_eq!(voice_starts(&rig.tick(1500)), vec![(id(2), 1500)]);
}

#[test]
fn post_wait_longer_than_the_action_reports_post_waiting() {
    let mut rig = Rig::new(vec![timed(wait(1, 1.0), 0.0, 3.0, AC), memo(2)]);
    rig.go(0);
    assert_eq!(
        changes(&rig.tick(1000), 1),
        vec![(CueState::PostWaiting, 1000)]
    );
    let tick = rig.tick(3000);
    assert_eq!(changes(&tick, 1), vec![(CueState::Idle, 3000)]);
    assert_eq!(running_at(&tick, 2), Some(3000));
}

#[test]
fn auto_follow_triggers_the_next_cue_when_the_action_ends() {
    let mut rig = Rig::new(vec![
        timed(sound(1), 0.0, 5.0, AF),
        timed(sound(2), 0.5, 0.0, DNC),
    ]);
    rig.go(0);
    // The post-wait is ignored in auto-follow.
    assert!(rig.tick(6000).is_empty());
    assert_eq!(rig.state(2), CueState::Idle);
    // Feedback arrives after the voice ended at 6500.
    let ended = rig.ended(6600, 1, 6500);
    assert_eq!(changes(&ended, 1), vec![(CueState::Idle, 6500)]);
    assert_eq!(changes(&ended, 2), vec![(CueState::PreWaiting, 6500)]);
    assert_eq!(voice_starts(&rig.tick(7000)), vec![(id(2), 7000)]);
}

#[test]
fn auto_follow_on_a_wait_cue_follows_its_duration() {
    let mut rig = Rig::new(vec![timed(wait(1, 2.0), 1.0, 0.0, AF), memo(2)]);
    rig.go(0);
    rig.tick(1000);
    assert_eq!(running_at(&rig.tick(3000), 2), Some(3000));
}

#[test]
fn do_not_continue_triggers_nothing() {
    let mut rig = Rig::new(vec![timed(wait(1, 1.0), 0.0, 0.5, DNC), memo(2)]);
    rig.go(0);
    rig.tick(500);
    rig.tick(1000);
    assert_eq!(rig.engine.next_deadline(), None);
    assert!(rig.tick(100_000).is_empty());
    assert_eq!(rig.state(2), CueState::Idle);
}

#[test]
fn zero_wait_auto_continue_chain_starts_on_one_sample() {
    let mut rig = Rig::new(vec![
        timed(sound(1), 0.0, 0.0, AC),
        timed(sound(2), 0.0, 0.0, AC),
        timed(sound(3), 0.0, 0.0, DNC),
        sound(4),
    ]);
    let go = rig.go(500);
    assert_eq!(
        voice_starts(&go),
        vec![(id(1), 500), (id(2), 500), (id(3), 500)]
    );
    assert_eq!(rig.playhead(), Some(id(4)));
}

#[test]
fn self_triggering_cycle_stops_at_the_chain_limit() {
    // Two cues with one id (invalid, but the engine must survive it): each time the first
    // completes it follows on to the "next" one, which is itself again.
    let a = timed(memo(1), 0.0, 0.0, AF);
    let mut rig = Rig::new(vec![a.clone(), a]);
    let limit = CueEngine::chain_limit(&rig.ws);
    let go = rig.go(0);
    let starts = changes(&go, 1)
        .iter()
        .filter(|(s, _)| *s == CueState::PreWaiting)
        .count();
    assert_eq!(starts, limit);
    assert_eq!(
        warnings(&go),
        vec![EngineWarning::ChainLimit { cue: id(1), limit }]
    );
    // The chain is cut, not deferred: nothing is left to do.
    assert_eq!(rig.engine.next_deadline(), None);
    assert!(rig.tick(1).is_empty());
}

#[test]
fn two_cue_cycle_stops_at_the_chain_limit() {
    // A → B → A (a duplicate of A's id) → B → …
    let a = timed(memo(1), 0.0, 0.0, AC);
    let b = timed(memo(2), 0.0, 0.0, AC);
    let mut rig = Rig::new(vec![a.clone(), b, a]);
    let limit = CueEngine::chain_limit(&rig.ws);
    let go = rig.go(0);
    let starts = go
        .iter()
        .filter(|a| {
            matches!(
                a,
                Action::StateChanged {
                    state: CueState::PreWaiting,
                    ..
                }
            )
        })
        .count();
    assert_eq!(starts, limit);
    assert!(matches!(
        warnings(&go).as_slice(),
        [EngineWarning::ChainLimit { .. }]
    ));
    assert_eq!(rig.engine.next_deadline(), None);
}

#[test]
fn cycle_with_post_waits_is_bounded_per_call() {
    // A cycle with time in it never hangs a call: each call only reaches its own deadlines.
    let a = timed(memo(1), 0.0, 1.0, AC);
    let mut rig = Rig::new(vec![a.clone(), a]);
    rig.go(0);
    for t in 1..=5 {
        let tick = rig.tick(t * 1000);
        assert_eq!(running_at(&tick, 1), Some(t * 1000));
        assert!(warnings(&tick).is_empty());
    }
}

#[test]
fn disarmed_cue_skips_its_action_but_keeps_timing_and_continue() {
    let mut a = timed(sound(1), 1.0, 0.0, AC);
    a.armed = false;
    let mut rig = Rig::new(vec![a, sound(2)]);
    rig.go(0);
    let tick = rig.tick(1000);
    assert_eq!(voice_starts(&tick), vec![(id(2), 1000)]);
    assert_eq!(rig.state(1), CueState::Idle);
}

#[test]
fn unsupported_kind_warns_completes_at_once_and_continues() {
    // Network cues come with M4.6.
    let send = timed(
        cue(
            1,
            CueKind::Network {
                destination: net_id(1),
                address: "/go".into(),
                args: vec![],
            },
        ),
        0.0,
        0.0,
        AF,
    );
    let mut rig = Rig::new(vec![send, memo(2), sound(10)]);
    let go = rig.go(0);
    assert_eq!(
        warnings(&go),
        vec![EngineWarning::Unsupported { cue: id(1) }]
    );
    assert_eq!(rig.state(10), CueState::Idle);
    assert_eq!(running_at(&go, 2), Some(0));
    assert!(go.iter().all(|a| !is_voice_action(a)));
}

// ----- GO (PB-01) and double-GO (PB-02) -----

#[test]
fn go_starts_the_playhead_cue_and_skips_its_continue_chain() {
    let mut rig = Rig::new(vec![
        timed(sound(1), 1.0, 0.0, AC),
        timed(sound(2), 0.0, 0.0, AF),
        timed(sound(3), 0.0, 0.0, DNC),
        sound(4),
    ]);
    assert_eq!(rig.playhead(), Some(id(1)));
    let go = rig.go(0);
    assert!(go.contains(&Action::PlayheadChanged {
        list: list(),
        cue: Some(id(4))
    }));
    assert_eq!(rig.state(1), CueState::PreWaiting);
    assert_eq!(rig.state(4), CueState::Idle);
    rig.go(100);
    assert_eq!(rig.playhead(), None);
}

#[test]
fn go_on_an_auto_continue_last_cue_leaves_the_playhead_past_the_end() {
    let mut rig = Rig::new(vec![memo(1), timed(memo(2), 0.0, 0.0, AC)]);
    rig.at(0, Input::PlayheadNext { list: list() });
    rig.go(0);
    assert_eq!(rig.playhead(), None);
}

#[test]
fn go_on_empty_list_and_past_the_end_is_ignored() {
    let mut empty = Rig::new(vec![]);
    assert_eq!(empty.playhead(), None);
    assert_eq!(
        empty.go(0),
        vec![Action::GoIgnored {
            list: list(),
            reason: GoIgnoredReason::NoCue
        }]
    );

    let mut rig = Rig::new(vec![memo(1)]);
    rig.go(0);
    assert_eq!(rig.playhead(), None);
    assert_eq!(
        rig.go(1000),
        vec![Action::GoIgnored {
            list: list(),
            reason: GoIgnoredReason::NoCue
        }]
    );
}

#[test]
fn go_on_unknown_list_is_ignored() {
    let mut rig = Rig::new(vec![memo(1)]);
    let other = list_id(99);
    assert_eq!(
        rig.at(0, Input::Go { list: other }),
        vec![Action::GoIgnored {
            list: other,
            reason: GoIgnoredReason::UnknownList
        }]
    );
}

#[test]
fn go_on_an_active_cue_is_ignored_but_moves_the_playhead() {
    let mut rig = Rig::new(vec![sound(1), sound(2)]);
    rig.go(0);
    let run = rig.engine.run_of(id(1)).unwrap();
    rig.at(10, Input::PlayheadPrevious { list: list() });
    let go = rig.go(500);
    assert!(go.iter().all(|a| !is_voice_action(a)));
    assert!(changes(&go, 1).is_empty());
    assert_eq!(
        warnings(&go),
        vec![EngineWarning::TriggerIgnored {
            cue: id(1),
            state: CueState::Running
        }]
    );
    assert_eq!(rig.engine.run_of(id(1)), Some(run));
    assert_eq!(rig.playhead(), Some(id(2)));
}

#[test]
fn continue_into_an_active_cue_is_ignored() {
    // [A, A] (duplicate ids): A's auto-continue targets itself while it is still running.
    let a = timed(memo(1), 0.0, 0.0, AC);
    let mut rig = Rig::new(vec![a.clone(), a]);
    let go = rig.go(0);
    assert_eq!(
        warnings(&go),
        vec![EngineWarning::TriggerIgnored {
            cue: id(1),
            state: CueState::Running
        }]
    );
    assert_eq!(rig.state(1), CueState::Idle);
}

#[test]
fn go_on_a_broken_cue_warns_and_moves_on() {
    let mut rig = Rig::new(vec![sound(1), sound(2)]);
    rig.start(0, 1);
    let run = rig.engine.run_of(id(1)).unwrap();
    rig.at(
        100,
        Input::ActionFailed {
            cue: id(1),
            run,
            reason: BrokenReason::StreamFailed,
        },
    );
    let go = rig.go(200);
    assert_eq!(warnings(&go), vec![EngineWarning::CueBroken { cue: id(1) }]);
    assert_eq!(rig.playhead(), Some(id(2)));
}

fn double_go_rig() -> Rig {
    let mut rig = Rig::new(vec![memo(1), memo(2), memo(3)]);
    rig.ws.settings.min_go_interval = 0.5;
    rig
}

fn double_go(actions: &[Action]) -> bool {
    actions.contains(&Action::GoIgnored {
        list: list(),
        reason: GoIgnoredReason::DoubleGo,
    })
}

#[test]
fn double_go_inside_the_interval_is_ignored() {
    let mut rig = double_go_rig();
    rig.go(1000);
    let second = rig.go(1499);
    assert!(double_go(&second));
    assert_eq!(second.len(), 1);
    assert_eq!(rig.playhead(), Some(id(2)));
    // The interval runs from the accepted GO, not the ignored one.
    assert!(!double_go(&rig.go(1500)));
}

#[test]
fn go_exactly_at_the_interval_is_accepted() {
    let mut rig = double_go_rig();
    rig.go(1000);
    assert!(!double_go(&rig.go(1500)));
    assert_eq!(rig.playhead(), Some(id(3)));
}

#[test]
fn go_just_past_the_interval_is_accepted() {
    let mut rig = double_go_rig();
    rig.go(1000);
    assert!(!double_go(&rig.go(1501)));
    assert_eq!(rig.playhead(), Some(id(3)));
}

#[test]
fn zero_interval_never_ignores_a_go() {
    let mut rig = Rig::new(vec![memo(1), memo(2)]);
    rig.go(1000);
    assert!(!double_go(&rig.go(1000)));
    assert_eq!(rig.playhead(), None);
}

#[test]
fn clock_going_backwards_does_not_block_go() {
    let mut rig = double_go_rig();
    rig.go(1_000_000);
    assert!(!double_go(&rig.go(10)));
}

// ----- playhead navigation (PB-05, CL-06) -----

fn numbered_rig() -> Rig {
    Rig::new(vec![
        numbered(1, "1", CueKind::Memo),
        numbered(2, "2", CueKind::Memo),
        numbered(3, "3", group(vec![numbered(30, "3.1", CueKind::Memo)])),
    ])
}

fn moved(actions: &[Action]) -> Option<Option<CueId>> {
    actions.iter().find_map(|a| match a {
        Action::PlayheadChanged { cue, .. } => Some(*cue),
        _ => None,
    })
}

#[test]
fn first_handle_reports_the_initial_playhead() {
    let mut engine = CueEngine::new(config(0));
    let ws = workspace(vec![memo(1)]);
    let actions = engine.handle(&ws, Input::Tick, 0);
    assert_eq!(
        actions,
        vec![Action::PlayheadChanged {
            list: list(),
            cue: Some(id(1))
        }]
    );
    assert_eq!(engine.standing_by(&ws, list()).map(|c| c.id), Some(id(1)));
}

#[test]
fn next_and_previous_move_without_firing() {
    let mut rig = numbered_rig();
    let next = |rig: &mut Rig| rig.at(0, Input::PlayheadNext { list: list() });
    let previous = |rig: &mut Rig| rig.at(0, Input::PlayheadPrevious { list: list() });

    assert_eq!(moved(&next(&mut rig)), Some(Some(id(2))));
    assert_eq!(moved(&next(&mut rig)), Some(Some(id(3))));
    // Over the timeline group as a unit (QLab 5 enters only "start first and enter" groups;
    // docs/cue-engine.md, Playhead).
    assert_eq!(moved(&next(&mut rig)), Some(None));
    assert!(next(&mut rig).is_empty());
    assert_eq!(moved(&previous(&mut rig)), Some(Some(id(3))));
    previous(&mut rig);
    previous(&mut rig);
    assert_eq!(rig.playhead(), Some(id(1)));
    assert!(previous(&mut rig).is_empty());
    assert!(rig.engine.cue_states().next().is_none());
}

#[test]
fn jump_by_id_and_number() {
    let mut rig = numbered_rig();
    let to = |cue| Input::SetPlayhead {
        list: list(),
        cue: Some(cue),
    };
    assert_eq!(moved(&rig.at(0, to(id(3)))), Some(Some(id(3))));
    let by_number = |n: &str| Input::SetPlayheadByNumber {
        list: list(),
        number: n.into(),
    };
    assert_eq!(moved(&rig.at(0, by_number("2"))), Some(Some(id(2))));
    assert_eq!(
        rig.at(
            0,
            Input::SetPlayhead {
                list: list(),
                cue: None
            }
        ),
        vec![Action::PlayheadChanged {
            list: list(),
            cue: None
        }]
    );
    assert!(rig.engine.cue_states().next().is_none());
}

#[test]
fn bad_jumps_warn_and_leave_the_playhead() {
    let mut rig = numbered_rig();
    // A cue that is not in the list.
    assert_eq!(
        rig.at(
            0,
            Input::SetPlayhead {
                list: list(),
                cue: Some(id(999))
            }
        ),
        vec![Action::Warning(EngineWarning::NotInList {
            list: list(),
            cue: id(999)
        })]
    );
    assert_eq!(
        rig.at(
            0,
            Input::SetPlayheadByNumber {
                list: list(),
                number: "nope".into()
            }
        ),
        vec![Action::Warning(EngineWarning::NumberNotFound {
            list: list(),
            number: "nope".into()
        })]
    );
    let other = list_id(9);
    assert_eq!(
        rig.at(0, Input::PlayheadNext { list: other }),
        vec![Action::Warning(EngineWarning::UnknownList { list: other })]
    );
    assert_eq!(rig.playhead(), Some(id(1)));
}

#[test]
fn deleting_the_playhead_cue_moves_to_the_cue_after_it() {
    let mut rig = Rig::new(vec![memo(1), memo(2), memo(3), memo(4)]);
    rig.at(0, Input::PlayheadNext { list: list() });
    // Delete 2 and 3 at once: the first survivor after 2 is 4.
    rig.cues_mut().retain(|c| c.id != id(2) && c.id != id(3));
    assert_eq!(moved(&rig.tick(1)), Some(Some(id(4))));
    // Delete the last cue under the playhead: past the end.
    rig.cues_mut().retain(|c| c.id != id(4));
    assert_eq!(moved(&rig.tick(2)), Some(None));
}

#[test]
fn reordering_keeps_the_playhead_on_its_cue() {
    let mut rig = Rig::new(vec![memo(1), memo(2), memo(3)]);
    rig.at(0, Input::PlayheadNext { list: list() });
    rig.cues_mut().reverse();
    assert!(rig.tick(1).is_empty());
    assert_eq!(rig.playhead(), Some(id(2)));
}

#[test]
fn empty_list_gaining_cues_stands_on_the_first() {
    let mut rig = Rig::new(vec![]);
    rig.cues_mut().push(memo(5));
    assert_eq!(moved(&rig.tick(1)), Some(Some(id(5))));
}

#[test]
fn new_and_removed_lists_get_and_lose_playheads() {
    let mut rig = Rig::new(vec![memo(1)]);
    rig.ws.cue_lists.push(CueList {
        id: list_id(2),
        name: "Second".into(),
        cues: vec![memo(7)],
    });
    assert_eq!(
        rig.tick(1),
        vec![Action::PlayheadChanged {
            list: list_id(2),
            cue: Some(id(7))
        }]
    );
    rig.ws.cue_lists.remove(1);
    rig.tick(2);
    assert_eq!(rig.engine.playhead(list_id(2)), None);
}

// ----- pause and resume -----

#[test]
fn pause_and_resume_keep_the_remaining_pre_wait_exactly() {
    let mut rig = Rig::new(vec![timed(sound(1), 2.0, 0.0, DNC)]);
    rig.go(0);
    assert_eq!(
        changes(&rig.at(500, Input::Pause { cue: id(1) }), 1),
        vec![(CueState::Paused, 500)]
    );
    assert_eq!(rig.engine.next_deadline(), None);
    assert!(rig.tick(50_000).is_empty());
    let resume = rig.at(60_500, Input::Resume { cue: id(1) });
    assert_eq!(changes(&resume, 1), vec![(CueState::PreWaiting, 60_500)]);
    assert!(resume.iter().all(|a| !is_voice_action(a)));
    assert_eq!(rig.engine.next_deadline(), Some(62_000));
    assert_eq!(voice_starts(&rig.tick(62_000)), vec![(id(1), 62_000)]);
}

#[test]
fn pause_and_resume_keep_the_remaining_post_wait_and_voice_position() {
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 2.0, AC), memo(2)]);
    rig.go(0);
    let run = rig.engine.run_of(id(1)).unwrap();
    let pause = rig.at(500, Input::Pause { cue: id(1) });
    assert_eq!(
        pause,
        vec![
            Action::PauseVoice {
                cue: id(1),
                run,
                at: 500,
                offset: 500
            },
            Action::StateChanged {
                cue: id(1),
                state: CueState::Paused,
                at: 500
            }
        ]
    );
    let resume = rig.at(5500, Input::Resume { cue: id(1) });
    assert_eq!(resumed(&resume), [(5500, 500)]);
    assert_eq!(rig.engine.next_deadline(), Some(7000));
    assert_eq!(running_at(&rig.tick(7000), 2), Some(7000));
    // A second pause measures from where the first left off.
    rig.at(8000, Input::Pause { cue: id(1) });
    assert_eq!(
        resumed(&rig.at(9000, Input::Resume { cue: id(1) })),
        [(9000, 3000)]
    );
}

#[test]
fn pause_freezes_a_wait_cue_action() {
    let mut rig = Rig::new(vec![wait(1, 2.0)]);
    rig.go(0);
    rig.at(1500, Input::Pause { cue: id(1) });
    rig.at(4000, Input::Resume { cue: id(1) });
    assert_eq!(rig.engine.next_deadline(), Some(4500));
}

// ----- stop, load, reset -----

#[test]
fn stop_during_pre_wait_cancels_the_action_and_its_continue() {
    let mut rig = Rig::new(vec![timed(sound(1), 1.0, 0.0, AC), sound(2)]);
    rig.go(0);
    let stop = rig.at(
        500,
        Input::Stop {
            cue: id(1),
            fade_out: None,
        },
    );
    assert_eq!(changes(&stop, 1), vec![(CueState::Idle, 500)]);
    assert!(stop.iter().all(|a| !is_voice_action(a)));
    assert!(rig.tick(100_000).is_empty());
    assert_eq!(rig.state(2), CueState::Idle);
}

#[test]
fn stopping_a_cue_cancels_its_auto_follow() {
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 0.0, AF), memo(2)]);
    rig.go(0);
    let run = rig.engine.run_of(id(1)).unwrap();
    rig.at(
        100,
        Input::Stop {
            cue: id(1),
            fade_out: None,
        },
    );
    // The voice's own end report (reason Stopped) arrives afterwards and is stale.
    let late = rig.at(
        200,
        Input::ActionEnded {
            cue: id(1),
            run,
            at: 100,
        },
    );
    assert!(late.is_empty());
    assert_eq!(rig.state(2), CueState::Idle);
}

fn fade(seconds: f64) -> Option<FadeSpec> {
    Some(FadeSpec {
        duration: seconds,
        curve: FadeCurve::EqualPower,
    })
}

#[test]
fn stop_with_fade_ends_when_the_fade_does() {
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 10.0, AC), memo(2)]);
    rig.go(0);
    let run = rig.engine.run_of(id(1)).unwrap();
    let stop = rig.at(
        1000,
        Input::Stop {
            cue: id(1),
            fade_out: fade(0.5),
        },
    );
    assert_eq!(
        stop,
        vec![
            Action::StopVoice {
                cue: id(1),
                run,
                at: 1000,
                fade: Some(StopFade {
                    frames: 500,
                    curve: FadeCurve::EqualPower
                })
            },
            Action::StateChanged {
                cue: id(1),
                state: CueState::Stopping,
                at: 1000
            }
        ]
    );
    assert_eq!(rig.engine.next_deadline(), Some(1500));
    // Without waiting for feedback, which may be lost.
    assert_eq!(changes(&rig.tick(1500), 1), vec![(CueState::Idle, 1500)]);
    // The post-wait was cancelled.
    assert!(rig.tick(20_000).is_empty());
}

#[test]
fn stop_with_fade_also_ends_on_feedback() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.go(0);
    rig.at(
        1000,
        Input::Stop {
            cue: id(1),
            fade_out: fade(0.5),
        },
    );
    assert_eq!(
        changes(&rig.ended(1400, 1, 1400), 1),
        vec![(CueState::Idle, 1400)]
    );
}

#[test]
fn stop_with_fade_on_a_silent_action_stops_at_once() {
    let mut rig = Rig::new(vec![wait(1, 5.0)]);
    rig.go(0);
    let stop = rig.at(
        1000,
        Input::Stop {
            cue: id(1),
            fade_out: fade(1.0),
        },
    );
    assert_eq!(changes(&stop, 1), vec![(CueState::Idle, 1000)]);
}

#[test]
fn stop_never_lands_before_the_voice_start() {
    // With lookahead a start can be committed on a sample after `now`; a stop at `now` must not
    // overtake it in the mixer.
    let mut rig = Rig::with_config(vec![timed(sound(1), 1.0, 0.0, DNC)], config(20));
    rig.go(0);
    rig.tick(990);
    let run = rig.engine.run_of(id(1)).unwrap();
    let stop = rig.at(
        990,
        Input::Stop {
            cue: id(1),
            fade_out: None,
        },
    );
    assert!(stop.contains(&Action::StopVoice {
        cue: id(1),
        run,
        at: 1000,
        fade: None
    }));
}

#[test]
fn load_to_time_inside_the_action() {
    let mut rig = Rig::new(vec![timed(sound(1), 1.0, 0.0, DNC)]);
    let load = rig.at(
        0,
        Input::Load {
            cue: id(1),
            time: 1.5,
        },
    );
    let run = rig.engine.run_of(id(1)).unwrap();
    assert_eq!(
        load,
        vec![
            Action::LoadVoice {
                cue: id(1),
                run,
                offset: 500
            },
            Action::StateChanged {
                cue: id(1),
                state: CueState::Loaded,
                at: 0
            }
        ]
    );
    // Starting skips the pre-wait and plays from 0.5 s into the file, with the same run.
    let start = rig.start(2000, 1);
    assert!(start.contains(&Action::StartVoice {
        cue: id(1),
        run,
        at: 2000,
        offset: 500
    }));
}

#[test]
fn load_to_time_inside_the_pre_wait() {
    let mut rig = Rig::new(vec![timed(sound(1), 1.0, 0.0, DNC)]);
    rig.at(
        0,
        Input::Load {
            cue: id(1),
            time: 0.4,
        },
    );
    rig.start(2000, 1);
    assert_eq!(rig.engine.next_deadline(), Some(2600));
    assert!(rig.tick(2600).iter().any(|a| matches!(
        a,
        Action::StartVoice {
            at: 2600,
            offset: 0,
            ..
        }
    )));
}

#[test]
fn load_to_time_shortens_known_durations_and_post_waits() {
    let mut rig = Rig::new(vec![timed(wait(1, 3.0), 1.0, 2.0, AC), memo(2)]);
    rig.at(
        0,
        Input::Load {
            cue: id(1),
            time: 2.5,
        },
    );
    let start = rig.start(10_000, 1);
    // 1.5 s into the action: the 2 s post-wait has 0.5 s left, the 3 s wait 1.5 s.
    assert_eq!(running_at(&start, 1), Some(10_000));
    assert_eq!(running_at(&rig.tick(10_500), 2), Some(10_500));
    assert_eq!(
        changes(&rig.tick(11_500), 1),
        vec![(CueState::Idle, 11_500)]
    );
}

#[test]
fn reset_clears_a_broken_cue() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.start(0, 1);
    let run = rig.engine.run_of(id(1)).unwrap();
    rig.at(
        10,
        Input::ActionFailed {
            cue: id(1),
            run,
            reason: BrokenReason::VoiceRejected,
        },
    );
    assert_eq!(
        rig.engine.broken_reason(id(1)),
        Some(BrokenReason::VoiceRejected)
    );
    assert_eq!(
        changes(&rig.at(20, Input::Reset { cue: id(1) }), 1),
        vec![(CueState::Idle, 20)]
    );
    assert_eq!(rig.engine.broken_reason(id(1)), None);
    assert_eq!(voice_starts(&rig.start(30, 1)), vec![(id(1), 30)]);
}

#[test]
fn start_on_a_paused_cue_resumes_it() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.start(0, 1);
    let run = rig.engine.run_of(id(1)).unwrap();
    rig.at(100, Input::Pause { cue: id(1) });
    let start = rig.start(300, 1);
    assert_eq!(resumed(&start), [(300, 100)]);
    // The resumed segment has its own id and names the one it continues.
    let fresh = rig.engine.run_of(id(1)).unwrap();
    assert_ne!(fresh, run);
    assert_eq!(resume_ids(&start), [(fresh, run)]);
}

// ----- feedback -----

#[test]
fn stale_and_unknown_feedback_is_ignored() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.start(0, 1);
    let old = rig.engine.run_of(id(1)).unwrap();
    rig.at(
        90,
        Input::Stop {
            cue: id(1),
            fade_out: None,
        },
    );
    rig.start(100, 1);
    let new = rig.engine.run_of(id(1)).unwrap();
    let ended = |cue, run| Input::ActionEnded { cue, run, at: 150 };
    let failed = |cue, run| Input::ActionFailed {
        cue,
        run,
        reason: BrokenReason::StreamFailed,
    };
    assert!(rig.at(200, ended(id(1), old)).is_empty());
    assert!(rig.at(200, failed(id(1), old)).is_empty());
    assert!(rig.at(200, ended(id(77), new)).is_empty());
    assert!(rig.at(200, failed(id(77), new)).is_empty());
    assert!(rig.at(200, ended(id(1), RunId(u64::MAX))).is_empty());
    assert_eq!(rig.state(1), CueState::Running);
    // Feedback for an idle cue.
    let mut idle = Rig::new(vec![sound(1)]);
    assert!(idle.at(0, ended(id(1), RunId(1))).is_empty());
}

#[test]
fn feedback_before_the_action_start_is_clamped_to_it() {
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 0.0, AF), memo(2)]);
    rig.start(1000, 1);
    let ended = rig.ended(1200, 1, 3);
    assert_eq!(changes(&ended, 1), vec![(CueState::Idle, 1000)]);
    assert_eq!(running_at(&ended, 2), Some(1000));
}

#[test]
fn operations_on_unknown_cues_warn() {
    let mut rig = Rig::new(vec![memo(1)]);
    for input in [
        Input::Start { cue: id(9) },
        Input::Stop {
            cue: id(9),
            fade_out: None,
        },
        Input::Pause { cue: id(9) },
        Input::Resume { cue: id(9) },
        Input::Load {
            cue: id(9),
            time: 0.0,
        },
        Input::Reset { cue: id(9) },
    ] {
        assert_eq!(
            rig.at(0, input),
            vec![Action::Warning(EngineWarning::UnknownCue { cue: id(9) })]
        );
    }
}

// ----- model edits -----

#[test]
fn deleting_a_running_cue_stops_it() {
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 5.0, AC), memo(2)]);
    rig.go(0);
    let run = rig.engine.run_of(id(1)).unwrap();
    rig.cues_mut().remove(0);
    let tick = rig.tick(100);
    assert!(tick.contains(&Action::StopVoice {
        cue: id(1),
        run,
        at: 100,
        fade: None
    }));
    assert!(tick.contains(&Action::Warning(EngineWarning::CueRemoved { cue: id(1) })));
    assert_eq!(changes(&tick, 1), vec![(CueState::Idle, 100)]);
    assert!(rig.tick(10_000).is_empty());
}

#[test]
fn deleting_a_loaded_cue_unloads_it() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.at(
        0,
        Input::Load {
            cue: id(1),
            time: 0.0,
        },
    );
    let run = rig.engine.run_of(id(1)).unwrap();
    rig.cues_mut().clear();
    assert!(
        rig.tick(1)
            .contains(&Action::UnloadVoice { cue: id(1), run })
    );
    assert_eq!(rig.state(1), CueState::Idle);
}

#[test]
fn edited_waits_apply_to_timers_not_yet_started() {
    let mut rig = Rig::new(vec![timed(wait(1, 1.0), 1.0, 3.0, AC), memo(2)]);
    rig.go(0);
    // The pre-wait is already counting; the post-wait starts with the action and takes the edit.
    rig.cues_mut()[0].pre_wait = 5.0;
    rig.cues_mut()[0].post_wait = 0.5;
    rig.tick(1000);
    assert_eq!(rig.engine.next_deadline(), Some(1500));
    // Once the post-wait is counting, edits leave it alone.
    let mut rig = Rig::new(vec![timed(wait(1, 1.0), 0.0, 3.0, AC), memo(2)]);
    rig.go(0);
    rig.cues_mut()[0].post_wait = 0.5;
    rig.tick(500);
    assert_eq!(rig.state(2), CueState::Idle);
    assert_eq!(running_at(&rig.tick(3000), 2), Some(3000));
}

#[test]
fn continue_goes_to_the_cue_that_is_next_when_it_fires() {
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 1.0, AC), memo(2), memo(3)]);
    rig.go(0);
    rig.cues_mut().swap(1, 2);
    let tick = rig.tick(1000);
    assert_eq!(running_at(&tick, 3), Some(1000));
    assert_eq!(rig.state(2), CueState::Idle);
}

// ----- transition table (docs/cue-engine.md) -----

#[derive(Clone, Copy, Debug)]
enum Setup {
    Idle,
    Loaded,
    PreWaiting,
    Running,
    Paused,
    PostWaiting,
    Stopping,
    Broken,
}

#[derive(Clone, Copy, Debug)]
enum Op {
    Start,
    Stop,
    StopFade,
    Pause,
    Resume,
    Load,
    Reset,
}

/// Cue 1 (audio, pre-wait 1 s, auto-continue after 5 s) brought into `from` by sample 2000; the
/// operation comes at 2200.
fn table_rig(from: Setup) -> Rig {
    let x = timed(sound(1), 1.0, 5.0, AC);
    let mut rig = Rig::new(vec![x, memo(2)]);
    let run_x = |rig: &mut Rig| {
        rig.start(0, 1);
        rig.tick(1000);
    };
    match from {
        Setup::Idle => {}
        Setup::Loaded => {
            rig.at(
                0,
                Input::Load {
                    cue: id(1),
                    time: 0.0,
                },
            );
        }
        Setup::PreWaiting => {
            rig.start(1500, 1);
        }
        Setup::Running => run_x(&mut rig),
        Setup::Paused => {
            run_x(&mut rig);
            rig.at(1500, Input::Pause { cue: id(1) });
        }
        Setup::PostWaiting => {
            run_x(&mut rig);
            rig.ended(1900, 1, 1800);
        }
        Setup::Stopping => {
            run_x(&mut rig);
            rig.at(
                1500,
                Input::Stop {
                    cue: id(1),
                    fade_out: fade(1.0),
                },
            );
        }
        Setup::Broken => {
            run_x(&mut rig);
            let run = rig.engine.run_of(id(1)).unwrap();
            rig.at(
                1500,
                Input::ActionFailed {
                    cue: id(1),
                    run,
                    reason: BrokenReason::StreamFailed,
                },
            );
        }
    }
    rig.tick(2000);
    rig
}

fn op_input(op: Op) -> Input {
    let cue = id(1);
    match op {
        Op::Start => Input::Start { cue },
        Op::Stop => Input::Stop {
            cue,
            fade_out: None,
        },
        Op::StopFade => Input::Stop {
            cue,
            fade_out: fade(1.0),
        },
        Op::Pause => Input::Pause { cue },
        Op::Resume => Input::Resume { cue },
        Op::Load => Input::Load { cue, time: 0.0 },
        Op::Reset => Input::Reset { cue },
    }
}

/// What the voice side of the actions must be.
#[derive(Clone, Copy, Debug, PartialEq)]
enum V {
    /// No voice action.
    Nothing,
    Load,
    Unload,
    UnloadThenLoad,
    Stop,
    StopFade,
    Pause,
    Resume,
}

fn voice_kinds(actions: &[Action]) -> Vec<V> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::LoadVoice { .. } => Some(V::Load),
            Action::UnloadVoice { .. } => Some(V::Unload),
            Action::StopVoice { fade: None, .. } => Some(V::Stop),
            Action::StopVoice { fade: Some(_), .. } => Some(V::StopFade),
            Action::PauseVoice { .. } => Some(V::Pause),
            Action::ResumeVoice { .. } => Some(V::Resume),
            _ => None,
        })
        .collect()
}

fn expected_voice(v: V) -> Vec<V> {
    match v {
        V::Nothing => vec![],
        V::UnloadThenLoad => vec![V::Unload, V::Load],
        other => vec![other],
    }
}

#[test]
fn every_transition_table_row() {
    use CueState as S;
    use Op as O;
    use Setup as F;
    #[rustfmt::skip]
    let table: &[(F, O, S, V, Option<EngineWarning>)] = &[
        (F::Idle, O::Start, S::PreWaiting, V::Nothing, None),
        (F::Idle, O::Stop, S::Idle, V::Nothing, None),
        (F::Idle, O::StopFade, S::Idle, V::Nothing, None),
        (F::Idle, O::Pause, S::Idle, V::Nothing, None),
        (F::Idle, O::Resume, S::Idle, V::Nothing, None),
        (F::Idle, O::Load, S::Loaded, V::Load, None),
        (F::Idle, O::Reset, S::Idle, V::Nothing, None),

        (F::Loaded, O::Start, S::PreWaiting, V::Nothing, None),
        (F::Loaded, O::Stop, S::Idle, V::Unload, None),
        (F::Loaded, O::StopFade, S::Idle, V::Unload, None),
        (F::Loaded, O::Pause, S::Loaded, V::Nothing, None),
        (F::Loaded, O::Resume, S::Loaded, V::Nothing, None),
        (F::Loaded, O::Load, S::Loaded, V::UnloadThenLoad, None),
        (F::Loaded, O::Reset, S::Idle, V::Unload, None),

        (F::PreWaiting, O::Start, S::PreWaiting, V::Nothing, Some(EngineWarning::TriggerIgnored { cue: id(1), state: S::PreWaiting })),
        (F::PreWaiting, O::Stop, S::Idle, V::Nothing, None),
        (F::PreWaiting, O::StopFade, S::Idle, V::Nothing, None),
        (F::PreWaiting, O::Pause, S::Paused, V::Nothing, None),
        (F::PreWaiting, O::Resume, S::PreWaiting, V::Nothing, None),
        (F::PreWaiting, O::Load, S::PreWaiting, V::Nothing, Some(EngineWarning::LoadIgnored { cue: id(1) })),
        (F::PreWaiting, O::Reset, S::Idle, V::Nothing, None),

        (F::Running, O::Start, S::Running, V::Nothing, Some(EngineWarning::TriggerIgnored { cue: id(1), state: S::Running })),
        (F::Running, O::Stop, S::Idle, V::Stop, None),
        (F::Running, O::StopFade, S::Stopping, V::StopFade, None),
        (F::Running, O::Pause, S::Paused, V::Pause, None),
        (F::Running, O::Resume, S::Running, V::Nothing, None),
        (F::Running, O::Load, S::Running, V::Nothing, Some(EngineWarning::LoadIgnored { cue: id(1) })),
        (F::Running, O::Reset, S::Idle, V::Stop, None),

        (F::Paused, O::Start, S::Running, V::Resume, None),
        (F::Paused, O::Stop, S::Idle, V::Stop, None),
        (F::Paused, O::StopFade, S::Idle, V::Stop, None),
        (F::Paused, O::Pause, S::Paused, V::Nothing, None),
        (F::Paused, O::Resume, S::Running, V::Resume, None),
        (F::Paused, O::Load, S::Paused, V::Nothing, Some(EngineWarning::LoadIgnored { cue: id(1) })),
        (F::Paused, O::Reset, S::Idle, V::Stop, None),

        (F::PostWaiting, O::Start, S::PostWaiting, V::Nothing, Some(EngineWarning::TriggerIgnored { cue: id(1), state: S::PostWaiting })),
        (F::PostWaiting, O::Stop, S::Idle, V::Nothing, None),
        (F::PostWaiting, O::StopFade, S::Idle, V::Nothing, None),
        (F::PostWaiting, O::Pause, S::Paused, V::Nothing, None),
        (F::PostWaiting, O::Resume, S::PostWaiting, V::Nothing, None),
        (F::PostWaiting, O::Load, S::PostWaiting, V::Nothing, Some(EngineWarning::LoadIgnored { cue: id(1) })),
        (F::PostWaiting, O::Reset, S::Idle, V::Nothing, None),

        (F::Stopping, O::Start, S::Stopping, V::Nothing, Some(EngineWarning::TriggerIgnored { cue: id(1), state: S::Stopping })),
        (F::Stopping, O::Stop, S::Idle, V::Stop, None),
        (F::Stopping, O::StopFade, S::Idle, V::Stop, None),
        (F::Stopping, O::Pause, S::Stopping, V::Nothing, None),
        (F::Stopping, O::Resume, S::Stopping, V::Nothing, None),
        (F::Stopping, O::Load, S::Stopping, V::Nothing, Some(EngineWarning::LoadIgnored { cue: id(1) })),
        (F::Stopping, O::Reset, S::Idle, V::Stop, None),

        (F::Broken, O::Start, S::Broken, V::Nothing, Some(EngineWarning::CueBroken { cue: id(1) })),
        (F::Broken, O::Stop, S::Broken, V::Nothing, None),
        (F::Broken, O::StopFade, S::Broken, V::Nothing, None),
        (F::Broken, O::Pause, S::Broken, V::Nothing, None),
        (F::Broken, O::Resume, S::Broken, V::Nothing, None),
        (F::Broken, O::Load, S::Broken, V::Nothing, Some(EngineWarning::CueBroken { cue: id(1) })),
        (F::Broken, O::Reset, S::Idle, V::Nothing, None),
    ];
    assert_eq!(table.len(), 8 * 7);
    for &(from, op, expect, voice, ref warning) in table {
        let mut rig = table_rig(from);
        let before = rig.state(1);
        let actions = rig.at(2200, op_input(op));
        let row = format!("{from:?} --{op:?}-->");
        assert_eq!(rig.state(1), expect, "{row} state");
        assert_eq!(voice_kinds(&actions), expected_voice(voice), "{row} voice");
        assert_eq!(
            warnings(&actions),
            warning.iter().cloned().collect::<Vec<_>>(),
            "{row}"
        );
        // The reported state change matches, and nothing is reported for a no-op.
        let last = changes(&actions, 1).last().map(|(s, _)| *s);
        if before == expect && !matches!(op, Op::Start | Op::Load) {
            assert_eq!(last, None, "{row} reported a change");
        } else if before != expect {
            assert_eq!(last, Some(expect), "{row} report");
        }
        // A cue left idle has nothing pending, and its continue was cancelled.
        if expect == S::Idle {
            assert!(rig.tick(100_000).is_empty(), "{row} left work behind");
            assert_eq!(rig.state(2), S::Idle, "{row} continued");
        }
    }
}

#[test]
fn table_start_from_loaded_keeps_the_run() {
    let mut rig = table_rig(Setup::Loaded);
    let run = rig.engine.run_of(id(1)).unwrap();
    rig.start(2200, 1);
    assert!(rig.tick(3200).contains(&Action::StartVoice {
        cue: id(1),
        run,
        at: 3200,
        offset: 0
    }));
}

// Event rows of the table.

#[test]
fn event_post_wait_elapses_while_running() {
    let mut rig = table_rig(Setup::Running);
    let tick = rig.tick(6000);
    assert_eq!(rig.state(1), CueState::Running);
    assert!(changes(&tick, 1).is_empty());
    assert_eq!(running_at(&tick, 2), Some(6000));
}

#[test]
fn event_post_wait_elapses_while_post_waiting() {
    let mut rig = table_rig(Setup::PostWaiting);
    let tick = rig.tick(6000);
    assert_eq!(changes(&tick, 1), vec![(CueState::Idle, 6000)]);
    assert_eq!(running_at(&tick, 2), Some(6000));
}

#[test]
fn event_action_ends_with_post_wait_pending() {
    let mut rig = table_rig(Setup::Running);
    assert_eq!(
        changes(&rig.ended(2500, 1, 2400), 1),
        vec![(CueState::PostWaiting, 2400)]
    );
}

#[test]
fn event_fade_out_completes() {
    let mut rig = table_rig(Setup::Stopping);
    assert_eq!(changes(&rig.tick(2500), 1), vec![(CueState::Idle, 2500)]);
}

#[test]
fn event_failure_breaks_every_running_state() {
    // The failed run's voice is released at once, so nothing can keep sounding (review H1).
    // Cue 1 auto-continues 5 s after its action start: a failure keeps that schedule, except
    // where it was already cancelled (stopping) or is held by a pause.
    for (from, voice, continues_at) in [
        (Setup::Loaded, V::Unload, None),
        (Setup::PreWaiting, V::Nothing, Some(7500)),
        (Setup::Running, V::Stop, Some(6000)),
        (Setup::Paused, V::Stop, None),
        (Setup::PostWaiting, V::Nothing, Some(6000)),
        (Setup::Stopping, V::Stop, None),
    ] {
        let mut rig = table_rig(from);
        let run = rig.engine.run_of(id(1)).unwrap();
        let failed = rig.at(
            2200,
            Input::ActionFailed {
                cue: id(1),
                run,
                reason: BrokenReason::DeviceLost,
            },
        );
        assert_eq!(
            changes(&failed, 1),
            vec![(CueState::Broken, 2200)],
            "{from:?}"
        );
        assert_eq!(voice_kinds(&failed), expected_voice(voice), "{from:?}");
        let later = rig.tick(100_000);
        assert!(later.iter().all(|a| !is_voice_action(a)), "{from:?}");
        assert!(changes(&later, 1).is_empty(), "{from:?}");
        assert_eq!(running_at(&later, 2), continues_at, "{from:?}");
        assert_eq!(rig.state(1), CueState::Broken, "{from:?}");
        assert_eq!(
            rig.engine.broken_reason(id(1)),
            Some(BrokenReason::DeviceLost),
            "{from:?}"
        );
    }
}

fn break_cue(rig: &mut Rig, now: SampleTime) -> Vec<Action> {
    let run = rig.engine.run_of(id(1)).unwrap();
    rig.at(
        now,
        Input::ActionFailed {
            cue: id(1),
            run,
            reason: BrokenReason::StreamFailed,
        },
    )
}

#[test]
fn broken_cue_keeps_its_auto_continue_on_the_exact_sample() {
    // Break during the pre-wait: the action is skipped, the timing is kept.
    let mut rig = Rig::new(vec![timed(sound(1), 1.0, 2.0, AC), memo(2)]);
    rig.at(
        0,
        Input::Load {
            cue: id(1),
            time: 0.0,
        },
    );
    rig.go(0);
    break_cue(&mut rig, 500);
    assert_eq!(rig.engine.next_deadline(), Some(1000));
    let tick = rig.tick(1000);
    assert!(tick.iter().all(|a| !is_voice_action(a)));
    assert_eq!(rig.state(1), CueState::Broken);
    assert_eq!(rig.engine.next_deadline(), Some(3000));
    assert!(rig.tick(2999).is_empty());
    assert_eq!(running_at(&rig.tick(3000), 2), Some(3000));
    assert_eq!(rig.state(1), CueState::Broken);

    // Break while running.
    let mut rig = Rig::new(vec![timed(sound(1), 1.0, 2.0, AC), memo(2)]);
    rig.go(0);
    rig.tick(1000);
    break_cue(&mut rig, 1500);
    assert_eq!(running_at(&rig.tick(3000), 2), Some(3000));
}

#[test]
fn broken_paused_cue_keeps_its_continue_held_until_resumed() {
    // A failure does not change when the continue fires: the pause still holds it.
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 2.0, AC), memo(2)]);
    rig.go(0);
    rig.at(500, Input::Pause { cue: id(1) });
    break_cue(&mut rig, 600);
    assert!(rig.tick(10_000).is_empty());
    let resume = rig.at(10_500, Input::Resume { cue: id(1) });
    assert!(resume.iter().all(|a| !is_voice_action(a)));
    assert_eq!(rig.state(1), CueState::Broken);
    // 1.5 s of post-wait were left at the pause.
    assert_eq!(rig.engine.next_deadline(), Some(12_000));
    assert_eq!(running_at(&rig.tick(12_000), 2), Some(12_000));
}

#[test]
fn broken_cue_never_auto_follows() {
    // Break during pre-wait, while running, and while paused.
    for setup in 0..3 {
        let mut rig = Rig::new(vec![timed(sound(1), 1.0, 0.0, AF), memo(2)]);
        rig.go(0);
        match setup {
            0 => {}
            1 => {
                rig.tick(1000);
            }
            _ => {
                rig.tick(1000);
                rig.at(1200, Input::Pause { cue: id(1) });
            }
        }
        break_cue(&mut rig, 1500);
        rig.tick(100_000);
        rig.at(100_000, Input::Resume { cue: id(1) });
        rig.tick(200_000);
        assert_eq!(rig.state(2), CueState::Idle, "setup {setup}");
        assert_eq!(rig.state(1), CueState::Broken, "setup {setup}");
        assert_eq!(rig.engine.next_deadline(), None, "setup {setup}");
    }
}

#[test]
fn stop_or_reset_on_a_broken_cue_cancels_its_pending_continue() {
    for stop in [true, false] {
        let mut rig = Rig::new(vec![timed(sound(1), 0.0, 2.0, AC), memo(2)]);
        rig.go(0);
        break_cue(&mut rig, 500);
        let input = if stop {
            Input::Stop {
                cue: id(1),
                fade_out: None,
            }
        } else {
            Input::Reset { cue: id(1) }
        };
        let actions = rig.at(1000, input);
        assert!(actions.iter().all(|a| !is_voice_action(a)));
        assert_eq!(
            rig.state(1),
            if stop {
                CueState::Broken
            } else {
                CueState::Idle
            }
        );
        assert_eq!(rig.engine.next_deadline(), None);
        assert!(rig.tick(100_000).is_empty());
        assert_eq!(rig.state(2), CueState::Idle);
    }
}

#[test]
fn event_paused_timers_do_not_run() {
    let mut rig = table_rig(Setup::Paused);
    assert!(rig.tick(1_000_000).is_empty());
    assert_eq!(rig.state(1), CueState::Paused);
}

#[test]
fn resume_with_a_clock_that_went_back_resumes_where_it_paused() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.start(0, 1);
    let run = rig.engine.run_of(id(1)).unwrap();
    rig.at(5000, Input::Pause { cue: id(1) });
    let resume = rig.at(100, Input::Resume { cue: id(1) });
    assert_eq!(resumed(&resume), [(100, 5000)]);
    assert_eq!(resume_ids(&resume)[0].1, run);
}

#[test]
fn failed_running_voice_is_stopped_and_stop_reset_stay_safe() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.start(0, 1);
    let run = rig.engine.run_of(id(1)).unwrap();
    let failed = rig.at(
        100,
        Input::ActionFailed {
            cue: id(1),
            run,
            reason: BrokenReason::StreamFailed,
        },
    );
    assert_eq!(voice_kinds(&failed), vec![V::Stop]);
    assert!(failed.contains(&Action::StopVoice {
        cue: id(1),
        run,
        at: 100,
        fade: None
    }));
    // The voice is gone; Stop and Reset have nothing left to silence and do not panic.
    assert!(
        rig.at(
            110,
            Input::Stop {
                cue: id(1),
                fade_out: None
            }
        )
        .is_empty()
    );
    assert_eq!(
        changes(&rig.at(120, Input::Reset { cue: id(1) }), 1),
        vec![(CueState::Idle, 120)]
    );
}

#[test]
fn failed_paused_voice_is_stopped() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.start(0, 1);
    rig.at(100, Input::Pause { cue: id(1) });
    let run = rig.engine.run_of(id(1)).unwrap();
    let failed = rig.at(
        110,
        Input::ActionFailed {
            cue: id(1),
            run,
            reason: BrokenReason::StreamFailed,
        },
    );
    assert!(failed.contains(&Action::StopVoice {
        cue: id(1),
        run,
        at: 110,
        fade: None
    }));
    assert_eq!(rig.state(1), CueState::Broken);
}

#[test]
fn end_report_dated_during_a_pause_is_ignored() {
    // A pause built as stop + restart reports the stop as the voice ending (review M1).
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 0.0, AF), memo(2)]);
    rig.start(0, 1);
    let run = rig.engine.run_of(id(1)).unwrap();
    rig.at(500, Input::Pause { cue: id(1) });
    assert!(rig.ended(520, 1, 500).is_empty());
    assert_eq!(rig.state(1), CueState::Paused);
    assert_eq!(rig.state(2), CueState::Idle);
    let resume = rig.at(1000, Input::Resume { cue: id(1) });
    assert_eq!(resumed(&resume), [(1000, 500)]);
    assert_eq!(resume_ids(&resume)[0].1, run);
}

#[test]
fn end_before_the_pause_still_completes_and_follows() {
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 0.0, AF), memo(2)]);
    rig.start(0, 1);
    rig.at(500, Input::Pause { cue: id(1) });
    let ended = rig.ended(520, 1, 480);
    assert_eq!(changes(&ended, 1), vec![(CueState::Idle, 480)]);
    assert_eq!(running_at(&ended, 2), Some(480));
}

fn pause_offset(actions: &[Action]) -> Option<SampleTime> {
    actions.iter().find_map(|a| match a {
        Action::PauseVoice { offset, .. } => Some(*offset),
        _ => None,
    })
}

#[test]
fn voice_position_counts_from_a_start_committed_in_the_lookahead() {
    // Review L1: the voice starts at 100 although the pause came at 95.
    let mut rig = Rig::with_config(vec![timed(sound(1), 0.1, 0.0, DNC)], config(10));
    rig.start(0, 1);
    assert_eq!(voice_starts(&rig.tick(95)), vec![(id(1), 100)]);
    assert_eq!(
        pause_offset(&rig.at(95, Input::Pause { cue: id(1) })),
        Some(0)
    );
    rig.at(200, Input::Resume { cue: id(1) });
    assert_eq!(
        pause_offset(&rig.at(300, Input::Pause { cue: id(1) })),
        Some(100)
    );
}

#[test]
fn voice_position_counts_from_a_late_start() {
    let mut rig = Rig::new(vec![timed(sound(1), 1.0, 0.0, DNC)]);
    rig.start(0, 1);
    assert_eq!(voice_starts(&rig.tick(1250)), vec![(id(1), 1250)]);
    assert_eq!(
        pause_offset(&rig.at(1500, Input::Pause { cue: id(1) })),
        Some(250)
    );
}

#[test]
fn lookahead_window_excludes_its_end() {
    // With lookahead one block, `now .. now + block` is the next block; its end is the first
    // sample of the block after, which is not committed yet (review L2).
    let mut rig = Rig::with_config(vec![timed(sound(1), 1.6, 0.0, DNC)], config(10));
    rig.start(0, 1);
    assert!(rig.tick(1590).is_empty());
    assert_eq!(voice_starts(&rig.tick(1591)), vec![(id(1), 1600)]);
}

#[test]
fn config_is_checked() {
    assert_eq!(
        EngineConfig::new(0, 0, 0),
        Err(EngineConfigError::SampleRate(0))
    );
    assert_eq!(
        EngineConfig::new(MAX_SAMPLE_RATE + 1, 0, 0),
        Err(EngineConfigError::SampleRate(MAX_SAMPLE_RATE + 1))
    );
    assert_eq!(
        EngineConfig::new(48_000, 48_001, 0),
        Err(EngineConfigError::Lookahead {
            lookahead: 48_001,
            max: 48_000
        })
    );
    let ok = EngineConfig::new(48_000, 48_000, 0).unwrap();
    assert_eq!((ok.sample_rate().get(), ok.lookahead()), (48_000, 48_000));
    assert!(EngineConfig::new(MAX_SAMPLE_RATE, 0, 0).is_ok());
}

#[test]
fn end_report_of_a_pause_arriving_after_resume_is_ignored() {
    // Review M1': a pause carried out as a stop reports its end late, after a quick resume.
    let mut rig = Rig::new(vec![timed(sound(1), 0.0, 0.0, AF), memo(2)]);
    rig.start(0, 1);
    let paused_run = rig.engine.run_of(id(1)).unwrap();
    rig.at(500, Input::Pause { cue: id(1) });
    rig.at(600, Input::Resume { cue: id(1) });
    let late = rig.at(
        610,
        Input::ActionEnded {
            cue: id(1),
            run: paused_run,
            at: 500,
        },
    );
    assert!(late.is_empty());
    assert_eq!(rig.state(1), CueState::Running);
    assert_eq!(rig.state(2), CueState::Idle);
    // The resumed voice's own end still completes the cue and follows.
    let ended = rig.ended(1000, 1, 900);
    assert_eq!(changes(&ended, 1), vec![(CueState::Idle, 900)]);
    assert_eq!(running_at(&ended, 2), Some(900));
}

/// `(at, offset)` of every ResumeVoice.
fn resumed(actions: &[Action]) -> Vec<(SampleTime, SampleTime)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::ResumeVoice { at, offset, .. } => Some((*at, *offset)),
            _ => None,
        })
        .collect()
}

/// `(run, paused)` of every ResumeVoice.
fn resume_ids(actions: &[Action]) -> Vec<(RunId, RunId)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::ResumeVoice { run, paused, .. } => Some((*run, *paused)),
            _ => None,
        })
        .collect()
}

#[test]
fn wake_rule_ticks_a_deadline_at_now_on_time() {
    // Review L6: with lookahead 0 a deadline at `now` must be due at `now`, not a wake later.
    let mut rig = Rig::new(vec![timed(sound(1), 1.6, 0.0, DNC)]);
    rig.start(0, 1);
    assert!(!rig.engine.is_due(1599));
    assert!(rig.engine.is_due(1600));
    let tick = rig.tick(1600);
    assert_eq!(voice_starts(&tick), vec![(id(1), 1600)]);
    assert!(warnings(&tick).is_empty());
    assert!(!rig.engine.is_due(u64::MAX));

    // With lookahead the window's end is excluded.
    let mut rig = Rig::with_config(vec![timed(sound(1), 1.6, 0.0, DNC)], config(10));
    rig.start(0, 1);
    assert!(!rig.engine.is_due(1590));
    assert!(rig.engine.is_due(1591));
}
