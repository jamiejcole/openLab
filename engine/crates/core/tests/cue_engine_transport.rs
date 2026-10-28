// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Panic (PB-03), Pause All and Resume All (PB-04), per docs/cue-engine.md "Panic" and "Pause
//! All and Resume All". Virtual sample clock at 1 kHz: one second is 1000 samples.

mod common;

use common::*;
use openlab_core::*;

const RATE: u32 = 1000;

use ContinueMode::{AutoContinue as AC, DoNotContinue as DNC};

fn id(n: u128) -> CueId {
    cue_id(n)
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

fn master_fade(n: u128, target: u128, seconds: f64, db: f64) -> Cue {
    cue(
        n,
        CueKind::Fade {
            target: Some(id(target)),
            duration: seconds,
            curve: FadeCurve::Linear,
            levels: FadeLevels {
                master_db: Some(db),
                output_db: vec![],
                crosspoints: vec![],
            },
            stop_target_when_done: true,
        },
    )
}

fn devamp_cue(n: u128, target: u128) -> Cue {
    cue(
        n,
        CueKind::Devamp {
            target: Some(id(target)),
            start_next_cue: true,
            stop_target_when_slice_ends: false,
            devamp_mode: DevampMode::CurrentSlice,
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
    /// `lists[0]` is list 1, `lists[1]` list 2, ...; the panic lasts one second.
    fn lists(lists: Vec<Vec<Cue>>, lookahead: SampleTime) -> Self {
        let mut ws = workspace(vec![]);
        ws.settings.panic_duration = 1.0;
        ws.cue_lists = lists
            .into_iter()
            .enumerate()
            .map(|(i, cues)| CueList {
                id: list_id(i as u128 + 1),
                name: format!("List {i}"),
                cues,
            })
            .collect();
        let mut rig = Self {
            ws,
            engine: CueEngine::new(EngineConfig::new(RATE, lookahead, 1).unwrap()),
            last: None,
        };
        rig.tick(0);
        rig
    }

    fn new(cues: Vec<Cue>) -> Self {
        Self::lists(vec![cues], 0)
    }

    fn at(&mut self, now: SampleTime, input: Input) -> Vec<Action> {
        handle_as_binary(&mut self.engine, &self.ws, &mut self.last, input, now)
    }

    fn tick(&mut self, now: SampleTime) -> Vec<Action> {
        self.at(now, Input::Tick)
    }

    fn go(&mut self, now: SampleTime) -> Vec<Action> {
        self.at(now, Input::Go { list: list_id(1) })
    }

    fn start(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(now, Input::Start { cue: id(n) })
    }

    fn panic(&mut self, now: SampleTime) -> Vec<Action> {
        self.at(now, Input::Panic)
    }

    fn state(&self, n: u128) -> CueState {
        self.engine.state(id(n))
    }

    fn run(&self, n: u128) -> RunId {
        self.engine.run_of(id(n)).unwrap()
    }

    /// Nothing runs, waits or is loaded, and no panic is under way.
    fn assert_quiet(&self) {
        assert!(self.engine.next_deadline().is_none());
        assert_eq!(self.engine.cue_states().count(), 0);
        assert_eq!(self.engine.panic_end(), None);
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

fn panic_fade(frames: SampleTime) -> Option<StopFade> {
    Some(StopFade {
        frames,
        curve: PANIC_CURVE,
    })
}

// ----- Panic (PB-03) -----

#[test]
fn panic_fades_every_cue_of_every_list_at_every_depth_and_stops_on_the_end_sample() {
    let mut rig = Rig::lists(
        vec![
            vec![
                sound(1),
                grp(
                    2,
                    GroupMode::Timeline,
                    vec![sound(3), grp(4, GroupMode::Timeline, vec![sound(5)])],
                ),
            ],
            vec![sound(6)],
        ],
        0,
    );
    for n in [1, 2, 6] {
        rig.start(0, n);
    }
    let panic = rig.panic(100);
    assert_eq!(
        voice_stops(&panic),
        vec![
            (id(1), 100, panic_fade(1000)),
            (id(5), 100, panic_fade(1000)),
            (id(3), 100, panic_fade(1000)),
            (id(6), 100, panic_fade(1000)),
        ]
    );
    for n in 1..=6 {
        assert_eq!(rig.state(n), CueState::Stopping, "cue {n}");
    }
    assert_eq!(rig.engine.panic_end(), Some(1100));
    assert_eq!(rig.engine.next_deadline(), Some(1100));
    assert!(rig.tick(1099).is_empty());
    // Every cue goes idle on the panic's end sample, without waiting for the voices' reports.
    let end = rig.tick(1100);
    for n in 1..=6 {
        assert_eq!(changes(&end, n), vec![(CueState::Idle, 1100)], "cue {n}");
    }
    assert!(voice_stops(&end).is_empty());
    rig.assert_quiet();
}

#[test]
fn panic_takes_over_fades_and_stops_in_progress() {
    let mut rig = Rig::new(vec![
        sound(1),
        master_fade(2, 1, 4.0, -60.0),
        sound(3),
        sound(4),
    ]);
    rig.start(0, 1);
    rig.start(0, 2);
    rig.start(0, 3);
    rig.start(0, 4);
    // A long stop fade (ends after the panic) and a short one (ends before it).
    let long = FadeSpec {
        duration: 10.0,
        curve: FadeCurve::SCurve,
    };
    rig.at(
        0,
        Input::Stop {
            cue: id(3),
            fade_out: Some(long),
        },
    );
    let short = FadeSpec {
        duration: 1.2,
        curve: FadeCurve::SCurve,
    };
    rig.at(
        500,
        Input::Stop {
            cue: id(4),
            fade_out: Some(short),
        },
    );
    let level = rig
        .engine
        .fade_level(id(1), LevelParam::Master, 1000)
        .unwrap();
    let panic = rig.panic(1000);
    // The Fade cue stops and holds cue 1's level where it is; the panic fades cue 1 out from
    // there (the stop gain multiplies the held level, so nothing jumps).
    assert!(panic.contains(&Action::Ramp {
        cue: id(1),
        run: rig_run(&panic, 1),
        at: 1000,
        param: LevelParam::Master,
        gain: level,
        frames: 0,
        curve: FadeCurve::Linear,
    }));
    // Cue 3's stop fade would end at 10000: the panic fades it again from where it is, to end
    // with the panic. Cue 4's ends at 1700, before the panic's 2000: it keeps it.
    assert_eq!(
        voice_stops(&panic),
        vec![
            (id(1), 1000, panic_fade(1000)),
            (id(3), 1000, panic_fade(1000)),
        ]
    );
    assert_eq!(rig.state(2), CueState::Idle);
    assert_eq!(changes(&rig.tick(1700), 4), vec![(CueState::Idle, 1700)]);
    let end = rig.tick(2000);
    assert_eq!(changes(&end, 1), vec![(CueState::Idle, 2000)]);
    assert_eq!(changes(&end, 3), vec![(CueState::Idle, 2000)]);
    rig.assert_quiet();
    // The Fade's "stop target when done" never comes.
    assert!(rig.tick(4000).is_empty());
}

/// The run id of cue `n` named in a `StopVoice` of `actions`.
fn rig_run(actions: &[Action], n: u128) -> RunId {
    actions
        .iter()
        .find_map(|a| match a {
            Action::StopVoice { cue, run, .. } if *cue == id(n) => Some(*run),
            _ => None,
        })
        .unwrap()
}

#[test]
fn panic_cancels_pending_pre_waits_continues_and_devamp_next_cues() {
    let mut rig = Rig::new(vec![
        timed(memo(1), 2.0, 0.0, DNC),
        timed(sound(2), 0.0, 0.5, AC),
        memo(3),
        sound(4),
        devamp_cue(5, 4),
        memo(6),
        timed(wait(7, 3.0), 0.0, 0.0, ContinueMode::AutoFollow),
        memo(8),
    ]);
    rig.start(0, 1);
    rig.start(0, 2);
    rig.start(0, 4);
    rig.start(0, 7);
    rig.start(10, 5);
    let run = rig.run(4);
    // The voice will leave its loop on 600, inside the panic fade.
    rig.at(
        20,
        Input::DevampScheduled {
            cue: id(4),
            run,
            at: 600,
        },
    );
    let panic = rig.panic(100);
    assert_eq!(changes(&panic, 1), vec![(CueState::Idle, 100)]);
    assert_eq!(changes(&panic, 7), vec![(CueState::Idle, 100)]);
    let mut later = Vec::new();
    for now in [500, 600, 1100, 2000, 3000] {
        later.extend(rig.tick(now));
    }
    // Nothing new starts: not cue 1's action, cue 2's continue (3), the Devamp cue's next cue
    // (6) or the Wait's follow (8).
    assert!(starts(&later).is_empty());
    assert!(voice_starts(&later).is_empty());
    rig.assert_quiet();
}

/// A Panic with nothing running does nothing (it skips walking the cues, M4.15i: closing an
/// idle 2,000-cue doc panics it twice), yet its window opens as always: a cue started in it is
/// stopped at once by the next Panic, as a second press.
#[test]
fn a_panic_with_nothing_running_does_nothing_but_opens_its_window() {
    let mut rig = Rig::new(vec![sound(1), grp(2, GroupMode::Timeline, vec![sound(3)])]);
    let first = rig.panic(0);
    assert!(voice_stops(&first).is_empty(), "{first:?}");
    for n in 1..=3 {
        assert!(changes(&first, n).is_empty());
        assert_eq!(rig.state(n), CueState::Idle);
    }
    rig.start(100, 1);
    let second = rig.panic(500);
    assert_eq!(voice_stops(&second), vec![(id(1), 500, None)]);
    assert_eq!(changes(&second, 1), vec![(CueState::Idle, 500)]);
    rig.assert_quiet();
}

#[test]
fn a_second_panic_during_the_fade_stops_everything_at_once() {
    let mut rig = Rig::new(vec![sound(1), grp(2, GroupMode::Timeline, vec![sound(3)])]);
    rig.start(0, 1);
    rig.start(0, 2);
    rig.panic(0);
    let second = rig.panic(500);
    assert_eq!(
        voice_stops(&second),
        vec![(id(1), 500, None), (id(3), 500, None)]
    );
    for n in 1..=3 {
        assert_eq!(changes(&second, n), vec![(CueState::Idle, 500)]);
    }
    rig.assert_quiet();
    // A panic after the fade has ended is a new panic, with its fade.
    rig.start(2000, 1);
    let third = rig.panic(2100);
    assert_eq!(voice_stops(&third), vec![(id(1), 2100, panic_fade(1000))]);
    // On its end sample it is over: a panic then fades again.
    rig.start(3100, 3);
    assert_eq!(
        voice_stops(&rig.panic(3100)),
        vec![(id(3), 3100, panic_fade(1000))]
    );
}

#[test]
fn panic_duration_zero_stops_everything_at_once() {
    let mut rig = Rig::new(vec![sound(1), timed(memo(2), 1.0, 0.0, DNC)]);
    rig.ws.settings.panic_duration = 0.0;
    rig.start(0, 1);
    rig.start(0, 2);
    let panic = rig.panic(100);
    assert_eq!(voice_stops(&panic), vec![(id(1), 100, None)]);
    assert_eq!(changes(&panic, 2), vec![(CueState::Idle, 100)]);
    rig.assert_quiet();
}

#[test]
fn panic_stops_paused_loaded_and_broken_cues() {
    let mut rig = Rig::new(vec![
        sound(1),
        timed(memo(2), 1.0, 0.0, DNC),
        sound(3),
        timed(sound(4), 0.0, 2.0, AC),
        memo(5),
    ]);
    rig.start(0, 1);
    rig.start(0, 2);
    rig.at(
        0,
        Input::Load {
            cue: id(3),
            time: 0.0,
        },
    );
    rig.start(0, 4);
    let run4 = rig.run(4);
    // Cue 4 breaks; its auto-continue (5 at 2000) is still pending.
    rig.at(
        50,
        Input::ActionFailed {
            cue: id(4),
            run: run4,
            reason: BrokenReason::StreamFailed,
        },
    );
    rig.at(100, Input::PauseAll);
    let run3 = rig.run(3);
    let panic = rig.panic(200);
    // A paused voice is silent: it stops at once.
    assert_eq!(voice_stops(&panic), vec![(id(1), 200, None)]);
    assert!(panic.contains(&Action::UnloadVoice {
        cue: id(3),
        run: run3
    }));
    for n in [1, 2, 3] {
        assert_eq!(rig.state(n), CueState::Idle);
    }
    assert_eq!(rig.state(4), CueState::Broken);
    assert!(starts(&rig.tick(5000)).is_empty());
    assert!(rig.engine.next_deadline().is_none());
    assert_eq!(
        rig.engine.cue_states().collect::<Vec<_>>(),
        vec![(id(4), CueState::Broken)]
    );
}

#[test]
fn go_during_the_panic_fade_starts_cues_normally() {
    // QLab: Panic does not lock the workspace; a GO during its fade plays as usual.
    let mut rig = Rig::new(vec![sound(1), sound(2)]);
    rig.go(0);
    rig.panic(100);
    let go = rig.go(200);
    assert_eq!(voice_starts(&go), vec![(id(2), 200)]);
    assert_eq!(rig.state(1), CueState::Stopping);
    assert_eq!(rig.state(2), CueState::Running);
    // The panic's end does not touch the new cue.
    rig.tick(1100);
    assert_eq!(rig.state(1), CueState::Idle);
    assert_eq!(rig.state(2), CueState::Running);
    // A panic during the fade is still a second press: everything, new cues too, stops at once.
    let mut rig = Rig::new(vec![sound(1), sound(2)]);
    rig.go(0);
    rig.panic(100);
    rig.go(200);
    let second = rig.panic(300);
    assert_eq!(
        voice_stops(&second),
        vec![(id(1), 300, None), (id(2), 300, None)]
    );
    rig.assert_quiet();
}

#[test]
fn panic_does_not_reset_double_go_protection() {
    let mut rig = Rig::new(vec![sound(1), sound(2)]);
    rig.ws.settings.min_go_interval = 0.5;
    rig.go(0);
    rig.panic(100);
    assert!(rig.go(200).contains(&Action::GoIgnored {
        list: list_id(1),
        reason: GoIgnoredReason::DoubleGo
    }));
    assert_eq!(voice_starts(&rig.go(500)), vec![(id(2), 500)]);
}

#[test]
fn panic_ends_every_voice_on_the_same_sample_with_lookahead() {
    // Lookahead 10: cue 2's start on 105 is committed before the panic at 100 arrives, so its
    // fade starts there and is shorter, ending with the others on 1100.
    let mut rig = Rig::lists(vec![vec![sound(1), timed(sound(2), 0.105, 0.0, DNC)]], 10);
    rig.start(0, 1);
    rig.start(0, 2);
    let panic = rig.panic(100);
    assert_eq!(voice_starts(&panic), vec![(id(2), 105)]);
    assert_eq!(
        voice_stops(&panic),
        vec![
            (id(1), 100, panic_fade(1000)),
            (id(2), 105, panic_fade(995))
        ]
    );
    let end = rig.tick(1091);
    assert_eq!(changes(&end, 1), vec![(CueState::Idle, 1100)]);
    assert_eq!(changes(&end, 2), vec![(CueState::Idle, 1100)]);
}

#[test]
fn panic_of_a_playlist_or_start_first_group_fires_nothing_after() {
    let mut rig = Rig::new(vec![
        timed(
            grp(
                1,
                GroupMode::Playlist {
                    loop_: true,
                    shuffle: false,
                },
                vec![wait(2, 0.5), sound(3)],
            ),
            0.0,
            0.0,
            ContinueMode::AutoFollow,
        ),
        grp(
            4,
            GroupMode::StartFirstAndGoToNext,
            vec![timed(sound(5), 0.0, 0.2, AC), sound(6)],
        ),
        memo(7),
    ]);
    rig.start(0, 1);
    rig.start(0, 4);
    rig.tick(500);
    assert_eq!(rig.state(3), CueState::Running);
    let panic = rig.panic(600);
    assert_eq!(rig.state(1), CueState::Stopping);
    assert_eq!(rig.state(4), CueState::Stopping);
    assert!(starts(&panic).is_empty());
    let mut later = rig.tick(1600);
    later.extend(rig.tick(5000));
    assert!(starts(&later).is_empty());
    assert_eq!(changes(&later, 1), vec![(CueState::Idle, 1600)]);
    rig.assert_quiet();
}

// ----- Pause All / Resume All (PB-04) -----

#[test]
fn pause_all_and_resume_all_reach_every_list_and_depth_and_keep_timing() {
    let mut rig = Rig::lists(
        vec![
            vec![
                sound(1),
                grp(
                    2,
                    GroupMode::Timeline,
                    vec![wait(3, 2.0), timed(memo(4), 1.0, 0.0, DNC)],
                ),
            ],
            vec![sound(5), master_fade(6, 5, 2.0, -40.0)],
        ],
        0,
    );
    for n in [1, 2, 5, 6] {
        rig.start(0, n);
    }
    let (run1, run5) = (rig.run(1), rig.run(5));
    let pause = rig.at(500, Input::PauseAll);
    assert!(pause.contains(&Action::PauseVoice {
        cue: id(1),
        run: run1,
        at: 500,
        offset: 500
    }));
    assert!(pause.contains(&Action::PauseVoice {
        cue: id(5),
        run: run5,
        at: 500,
        offset: 500
    }));
    // The Fade cue holds cue 5's level.
    assert!(pause.iter().any(|a| matches!(
        a,
        Action::Ramp { cue, at: 500, frames: 0, .. } if *cue == id(5)
    )));
    for n in 1..=6 {
        assert_eq!(rig.state(n), CueState::Paused, "cue {n}");
    }
    assert!(rig.engine.next_deadline().is_none());
    assert!(
        rig.tick(5000)
            .iter()
            .all(|a| !matches!(a, Action::StateChanged { .. }))
    );

    let resume = rig.at(6000, Input::ResumeAll);
    let resumed: Vec<_> = resume
        .iter()
        .filter_map(|a| match a {
            Action::ResumeVoice {
                cue,
                run,
                paused,
                at,
                offset,
            } => Some((*cue, *run, *paused, *at, *offset)),
            _ => None,
        })
        .collect();
    assert_eq!(resumed.len(), 2);
    for (cue, run, paused, at, offset) in resumed {
        // A fresh id per resume (feedback for the paused segment is stale).
        assert_ne!(run, paused);
        assert_eq!((at, offset), (6000, 500));
        assert_eq!(rig.engine.run_of(cue), Some(run));
    }
    // The Fade cue carries on to its end, shifted by the pause: 2000 + 5500.
    assert!(resume.iter().any(|a| matches!(
        a,
        Action::Ramp { cue, at: 6000, frames: 1500, .. } if *cue == id(5)
    )));
    // Memo 4's pre-wait had 500 left, the Wait 1500.
    assert_eq!(starts(&rig.tick(6499)), vec![]);
    assert_eq!(
        changes(&rig.tick(6500), 4),
        vec![(CueState::Running, 6500), (CueState::Idle, 6500)]
    );
    assert_eq!(changes(&rig.tick(7500), 3), vec![(CueState::Idle, 7500)]);
    assert_eq!(rig.state(6), CueState::Idle);
    assert_eq!(rig.state(2), CueState::Idle);
}

#[test]
fn resume_all_resends_a_pending_devamp() {
    let mut rig = Rig::new(vec![sound(1), devamp_cue(2, 1), memo(3)]);
    rig.start(0, 1);
    rig.start(10, 2);
    rig.at(100, Input::PauseAll);
    let resume = rig.at(200, Input::ResumeAll);
    let fresh = rig.run(1);
    assert!(resume.contains(&Action::DevampVoice {
        cue: id(1),
        run: fresh,
        at: 200,
        stop: false
    }));
    rig.at(
        210,
        Input::DevampScheduled {
            cue: id(1),
            run: fresh,
            at: 900,
        },
    );
    assert_eq!(starts(&rig.tick(900)), vec![(id(3), 900)]);
}

#[test]
fn resume_all_resumes_every_paused_cue_however_it_was_paused() {
    // QLab: Resume All resumes all paused cues, also those a Pause cue or the operator paused.
    let mut rig = Rig::new(vec![sound(1), sound(2), sound(3)]);
    for n in 1..=3 {
        rig.start(0, n);
    }
    rig.at(100, Input::Pause { cue: id(1) });
    rig.at(200, Input::PauseAll);
    let resume = rig.at(300, Input::ResumeAll);
    let resumed: Vec<CueId> = resume
        .iter()
        .filter_map(|a| match a {
            Action::ResumeVoice { cue, .. } => Some(*cue),
            _ => None,
        })
        .collect();
    assert_eq!(resumed, vec![id(1), id(2), id(3)]);
    // Nothing paused: Resume All does nothing.
    assert!(rig.at(400, Input::ResumeAll).is_empty());
}

#[test]
fn pause_all_leaves_stopping_loaded_and_idle_cues_alone() {
    let mut rig = Rig::new(vec![sound(1), sound(2), memo(3)]);
    rig.start(0, 1);
    rig.at(
        0,
        Input::Stop {
            cue: id(1),
            fade_out: Some(FadeSpec {
                duration: 1.0,
                curve: FadeCurve::Linear,
            }),
        },
    );
    rig.at(
        0,
        Input::Load {
            cue: id(2),
            time: 0.0,
        },
    );
    let pause = rig.at(100, Input::PauseAll);
    assert!(pause.is_empty());
    assert_eq!(rig.state(1), CueState::Stopping);
    assert_eq!(rig.state(2), CueState::Loaded);
    assert_eq!(changes(&rig.tick(1000), 1), vec![(CueState::Idle, 1000)]);
}

#[test]
fn pause_all_holds_an_auto_continue_until_resume_all() {
    let mut rig = Rig::new(vec![timed(memo(1), 0.5, 1.0, AC), memo(2)]);
    rig.go(0);
    rig.at(200, Input::PauseAll);
    assert!(starts(&rig.tick(2000)).is_empty());
    rig.at(3000, Input::ResumeAll);
    // 1300 left of the 1500 to the continue.
    assert_eq!(starts(&rig.tick(4300)), vec![(id(2), 4300)]);
}

#[test]
fn a_stop_with_fade_of_a_devamped_cue_cancels_the_devamp_next_cue() {
    // The devamp's leave sample falls inside the stop's fade-out: a stopped target cancels it.
    let mut rig = Rig::new(vec![sound(1), devamp_cue(2, 1), memo(3)]);
    rig.start(0, 1);
    rig.start(10, 2);
    let run = rig.run(1);
    rig.at(
        20,
        Input::DevampScheduled {
            cue: id(1),
            run,
            at: 500,
        },
    );
    rig.at(
        100,
        Input::Stop {
            cue: id(1),
            fade_out: Some(FadeSpec {
                duration: 1.0,
                curve: FadeCurve::Linear,
            }),
        },
    );
    assert!(starts(&rig.tick(500)).is_empty());
    assert_eq!(rig.state(3), CueState::Idle);
}

#[test]
fn panic_and_pause_all_reach_the_children_of_a_group_with_a_duplicated_id() {
    // Regression (found by the property test): the panic walked groups by id, and the id of
    // this group first names a memo, so its children were never stopped.
    let mut rig = Rig::new(vec![
        memo(4),
        grp(4, GroupMode::Timeline, vec![sound(6), sound(7)]),
    ]);
    rig.start(0, 6);
    rig.at(
        0,
        Input::Load {
            cue: id(7),
            time: 0.0,
        },
    );
    rig.at(10, Input::PauseAll);
    assert_eq!(rig.state(6), CueState::Paused);
    rig.at(20, Input::ResumeAll);
    assert_eq!(rig.state(6), CueState::Running);
    rig.panic(100);
    rig.tick(1100);
    rig.assert_quiet();
}

// ----- triggers during a panic fade (review M1) -----

#[test]
fn go_on_a_cue_fading_out_in_a_panic_starts_it_afresh() {
    // Regression (review M1): the operator panicked by mistake, set the playhead back and
    // pressed GO. The cue was `stopping`, so the GO was ignored and the playhead moved on.
    let mut rig = Rig::new(vec![sound(1), sound(2)]);
    rig.ws.settings.panic_duration = 2.0;
    rig.go(0);
    let old = rig.run(1);
    rig.panic(100);
    rig.at(
        1500,
        Input::SetPlayhead {
            list: list_id(1),
            cue: Some(id(1)),
        },
    );
    let go = rig.go(1600);
    assert!(warnings(&go).is_empty());
    assert_eq!(voice_stops(&go), vec![(id(1), 1600, None)]);
    assert_eq!(voice_starts(&go), vec![(id(1), 1600)]);
    let fresh = rig.run(1);
    assert_ne!(fresh, old);
    assert_eq!(
        changes(&go, 1),
        vec![
            (CueState::Idle, 1600),
            (CueState::PreWaiting, 1600),
            (CueState::Running, 1600)
        ]
    );
    assert_eq!(rig.engine.playhead(list_id(1)), Some(id(2)));
    // The panic's end does not touch the new run.
    assert!(changes(&rig.tick(2100), 1).is_empty());
    assert_eq!(rig.state(1), CueState::Running);
    // An ordinary stop with fade keeps the second-trigger rule.
    rig.at(
        3000,
        Input::Stop {
            cue: id(1),
            fade_out: Some(FadeSpec {
                duration: 1.0,
                curve: FadeCurve::Linear,
            }),
        },
    );
    assert_eq!(
        warnings(&rig.start(3100, 1)),
        vec![EngineWarning::TriggerIgnored {
            cue: id(1),
            state: CueState::Stopping
        }]
    );
}

fn feedback(rig: &mut Rig, now: SampleTime, n: u128) -> Vec<Action> {
    let run = rig.run(n);
    rig.at(
        now,
        Input::ActionEnded {
            cue: id(n),
            run,
            at: now,
        },
    )
}

#[test]
fn a_group_in_a_panic_restarts_on_start_and_a_child_started_on_its_own_ends_it() {
    let mut rig = Rig::new(vec![grp(1, GroupMode::Timeline, vec![sound(2), sound(3)])]);
    rig.start(0, 1);
    rig.panic(100);
    assert_eq!(rig.state(1), CueState::Stopping);
    // GO on a child during the panic: it starts afresh on its own, and its group stops waiting
    // (it would otherwise stay `stopping` until that child ends) and ends at once, as a child
    // started on its own does not make its group run (M2.24, QLab check G4).
    let start = rig.start(200, 3);
    assert_eq!(voice_starts(&start), vec![(id(3), 200)]);
    assert_eq!(changes(&start, 1), vec![(CueState::Idle, 200)]);
    // Its other child finishes its panic fade on its own.
    assert_eq!(rig.state(2), CueState::Stopping);
    let end = rig.tick(1100);
    assert_eq!(changes(&end, 2), vec![(CueState::Idle, 1100)]);
    assert!(changes(&end, 1).is_empty());
    assert_eq!(rig.state(3), CueState::Running);
    let done = feedback(&mut rig, 5000, 3);
    assert_eq!(changes(&done, 3), vec![(CueState::Idle, 5000)]);
    assert!(changes(&done, 1).is_empty());

    // Start on the group during its panic: everything in it stops at once and it starts again.
    let mut rig = Rig::new(vec![grp(1, GroupMode::Timeline, vec![sound(2), sound(3)])]);
    rig.start(0, 1);
    rig.panic(100);
    let restart = rig.start(300, 1);
    assert!(warnings(&restart).is_empty());
    assert_eq!(
        voice_stops(&restart),
        vec![(id(3), 300, None), (id(2), 300, None)]
    );
    assert_eq!(voice_starts(&restart), vec![(id(2), 300), (id(3), 300)]);
    assert_eq!(rig.state(1), CueState::Running);
    rig.tick(1100);
    assert_eq!(rig.state(2), CueState::Running);
}

// ----- feedback during the panic fade (review M3) -----

#[test]
fn feedback_during_the_panic_fade_starts_nothing() {
    let mut rig = Rig::new(vec![
        timed(sound(1), 0.0, 0.0, ContinueMode::AutoFollow),
        memo(2),
        grp(
            3,
            GroupMode::Playlist {
                loop_: false,
                shuffle: false,
            },
            vec![sound(4), sound(5)],
        ),
        sound(6),
        sound(7),
        devamp_cue(8, 7),
        memo(9),
    ]);
    for n in [1, 3, 6, 7] {
        rig.start(0, n);
    }
    rig.start(10, 8);
    let (run6, run7) = (rig.run(6), rig.run(7));
    rig.panic(100);
    let mut during = Vec::new();
    // The auto-follow cue's voice ends during the fade (its fade-out stop finished early).
    during.extend(feedback(&mut rig, 300, 1));
    // The playlist's current child ends.
    during.extend(feedback(&mut rig, 400, 4));
    // A voice fails.
    during.extend(rig.at(
        500,
        Input::ActionFailed {
            cue: id(6),
            run: run6,
            reason: BrokenReason::StreamFailed,
        },
    ));
    // The devamp answer arrives, leaving the loop inside the fade.
    during.extend(rig.at(
        600,
        Input::DevampScheduled {
            cue: id(7),
            run: run7,
            at: 700,
        },
    ));
    during.extend(rig.tick(700));
    assert!(starts(&during).is_empty(), "{during:?}");
    assert!(voice_starts(&during).is_empty());
    assert_eq!(rig.state(1), CueState::Idle);
    assert_eq!(rig.state(5), CueState::Idle);
    assert_eq!(rig.state(6), CueState::Broken);
    rig.tick(1100);
    assert_eq!(
        rig.engine.cue_states().collect::<Vec<_>>(),
        vec![(id(6), CueState::Broken)]
    );
    assert!(rig.engine.next_deadline().is_none());
}

fn fail(rig: &mut Rig, now: SampleTime, n: u128) -> Vec<Action> {
    let run = rig.run(n);
    rig.at(
        now,
        Input::ActionFailed {
            cue: id(n),
            run,
            reason: BrokenReason::StreamFailed,
        },
    )
}

fn playlist(n: u128, children: Vec<Cue>) -> Cue {
    playlist_of(n, true, children)
}

fn playlist_of(n: u128, loop_: bool, children: Vec<Cue>) -> Cue {
    grp(
        n,
        GroupMode::Playlist {
            loop_,
            shuffle: false,
        },
        children,
    )
}

/// Fails the cue's run with a stream failure: while it plays (`ActionFailed`), or before it
/// produced audio (`StartFailed`).
fn fail_as(rig: &mut Rig, now: SampleTime, n: u128, unplayed: bool) -> Vec<Action> {
    let (cue, run, reason) = (id(n), rig.run(n), BrokenReason::StreamFailed);
    rig.at(
        now,
        if unplayed {
            Input::StartFailed { cue, run, reason }
        } else {
            Input::ActionFailed { cue, run, reason }
        },
    )
}

/// M2.31 (found by `cue_engine_prop`): a child of a playlist that is not running (started on its
/// own) fails while it fades out in a panic. It was leaving anyway, so it ends as a stopped cue
/// does: the playlist does not carry on to its next child, looping or not, whether the voice had
/// played (`ActionFailed`) or not (`StartFailed`, review M1).
#[test]
fn a_stream_failure_during_the_panic_fade_does_not_carry_a_playlist_on() {
    for (loop_, unplayed) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut rig = Rig::new(vec![playlist_of(1, loop_, vec![sound(2), sound(3)])]);
        rig.start(0, 2);
        rig.panic(100);
        let failed = fail_as(&mut rig, 200, 2, unplayed);
        let case = format!("loop {loop_}, unplayed {unplayed}: {failed:?}");
        assert!(starts(&failed).is_empty(), "{case}");
        assert!(voice_starts(&failed).is_empty(), "{case}");
        // The failure still shows: the cue is broken, as any cue whose stream failed.
        assert_eq!(rig.state(2), CueState::Broken, "{case}");
        assert_eq!(rig.state(3), CueState::Idle, "{case}");
        let end = rig.tick(1100);
        assert!(starts(&end).is_empty(), "{case}");
        assert!(voice_starts(&end).is_empty(), "{case}");
        assert_eq!(rig.engine.panic_end(), None);
        assert!(rig.engine.next_deadline().is_none());
    }
}

/// The same failure inside a group that runs on its own inside a playlist: the group hears a stop
/// (as from the end of the child's fade), so it stops rather than completes, and the playlist
/// around it does not move on.
#[test]
fn a_stream_failure_during_the_panic_fade_stops_its_group_and_starts_nothing() {
    let mut rig = Rig::new(vec![playlist(
        1,
        vec![grp(2, GroupMode::Timeline, vec![sound(3)]), sound(4)],
    )]);
    rig.start(0, 2);
    rig.panic(100);
    let failed = fail(&mut rig, 200, 3);
    assert!(starts(&failed).is_empty(), "{failed:?}");
    assert!(voice_starts(&failed).is_empty());
    assert_eq!(changes(&failed, 2), vec![(CueState::Idle, 200)]);
    assert_eq!(rig.state(3), CueState::Broken);
    assert_eq!(rig.state(4), CueState::Idle);
    assert!(starts(&rig.tick(1100)).is_empty());
}

/// A cue's own panic (M4.1d) is a panic too: a failure while it fades starts nothing, also from the
/// group around it, which neither panicked nor stops by itself.
#[test]
fn a_stream_failure_during_a_cues_own_panic_starts_nothing() {
    let mut rig = Rig::new(vec![playlist(1, vec![sound(2), sound(3)])]);
    rig.start(0, 2);
    rig.at(100, Input::PanicCue { cue: id(2) });
    let failed = fail(&mut rig, 200, 2);
    assert!(starts(&failed).is_empty(), "{failed:?}");
    assert!(voice_starts(&failed).is_empty());
    assert_eq!(rig.state(2), CueState::Broken);
    assert_eq!(rig.state(3), CueState::Idle);
}

/// A cue's own panic on a child: the group around it did not panic, yet it stops when the failed
/// child leaves.
#[test]
fn a_stream_failure_during_a_cues_own_panic_stops_the_group_around_it() {
    let mut rig = Rig::new(vec![playlist(
        1,
        vec![grp(2, GroupMode::Timeline, vec![sound(3)]), sound(4)],
    )]);
    rig.start(0, 2);
    rig.at(100, Input::PanicCue { cue: id(3) });
    let failed = fail(&mut rig, 200, 3);
    // Its group stops, as at the end of the child's panic fade, rather than completing (which
    // would carry the playlist on to cue 4).
    assert!(starts(&failed).is_empty(), "{failed:?}");
    assert!(voice_starts(&failed).is_empty());
    assert_eq!(changes(&failed, 2), vec![(CueState::Idle, 200)]);
    assert_eq!(rig.state(3), CueState::Broken);
    assert_eq!(rig.state(4), CueState::Idle);
}

/// A stop with a fade is the operator's stop: a failure during the fade ends it early and starts
/// nothing, as the end of the fade would not.
#[test]
fn a_stream_failure_during_a_stop_fade_starts_nothing() {
    let fade = Some(FadeSpec {
        duration: 1.0,
        curve: FadeCurve::Linear,
    });
    let mut rig = Rig::new(vec![playlist(1, vec![sound(2), sound(3)])]);
    rig.start(0, 2);
    rig.at(
        100,
        Input::Stop {
            cue: id(2),
            fade_out: fade,
        },
    );
    let failed = fail(&mut rig, 200, 2);
    assert!(starts(&failed).is_empty(), "{failed:?}");
    assert!(voice_starts(&failed).is_empty());
    assert_eq!(rig.state(2), CueState::Broken);
    assert_eq!(rig.state(3), CueState::Idle);
}

/// The child alone stopped with a fade: its group, running on its own, stops when the child
/// leaves, failed or not, and the playlist around it does not move on.
#[test]
fn a_stream_failure_during_a_childs_stop_fade_stops_its_group_and_starts_nothing() {
    let fade = Some(FadeSpec {
        duration: 1.0,
        curve: FadeCurve::Linear,
    });
    let mut rig = Rig::new(vec![playlist(
        1,
        vec![grp(2, GroupMode::Timeline, vec![sound(3)]), sound(4)],
    )]);
    rig.start(0, 2);
    rig.at(
        100,
        Input::Stop {
            cue: id(3),
            fade_out: fade,
        },
    );
    let failed = fail(&mut rig, 200, 3);
    assert!(starts(&failed).is_empty(), "{failed:?}");
    assert!(voice_starts(&failed).is_empty());
    assert_eq!(changes(&failed, 2), vec![(CueState::Idle, 200)]);
    assert_eq!(rig.state(4), CueState::Idle);
}

/// A lost device's failure during the panic fade takes the same way: nothing moves on, and the
/// cue is idle at once if its patch already shows the device back (as after any device loss).
#[test]
fn a_device_lost_failure_during_the_panic_fade_starts_nothing() {
    for (mapped, unplayed) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut rig = Rig::new(vec![playlist(1, vec![sound(2), sound(3)])]);
        if mapped {
            rig.at(
                0,
                Input::PatchStatus {
                    patch: patch_id(1),
                    resolution: PatchResolution::Mapped {
                        device: "D".into(),
                        channels: vec![0, 1],
                    },
                },
            );
        }
        rig.start(0, 2);
        rig.panic(100);
        let (cue, run, reason) = (id(2), rig.run(2), BrokenReason::DeviceLost);
        let failed = rig.at(
            200,
            if unplayed {
                Input::StartFailed { cue, run, reason }
            } else {
                Input::ActionFailed { cue, run, reason }
            },
        );
        assert!(starts(&failed).is_empty(), "{failed:?}");
        assert!(voice_starts(&failed).is_empty());
        let state = if mapped {
            CueState::Idle
        } else {
            CueState::Broken
        };
        assert_eq!(rig.state(2), state);
        assert_eq!(rig.state(3), CueState::Idle);
    }
}

// ----- a failure while paused (M2.31 review M2) -----

/// Under Pause All, a child that was started on its own in a playlist fails (a read error of its
/// paused stream). A paused show starts nothing: the playlist has no run to hold its step, so the
/// failed child holds it, broken and paused, and the next child starts on the resume sample.
#[test]
fn a_failure_while_paused_holds_a_playlist_step_until_the_resume() {
    for (loop_, unplayed) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut rig = Rig::new(vec![playlist_of(1, loop_, vec![sound(2), sound(3)])]);
        rig.start(0, 2);
        rig.at(100, Input::PauseAll);
        let failed = fail_as(&mut rig, 200, 2, unplayed);
        let case = format!("loop {loop_}, unplayed {unplayed}: {failed:?}");
        assert!(starts(&failed).is_empty(), "{case}");
        assert!(voice_starts(&failed).is_empty(), "{case}");
        assert_eq!(rig.state(2), CueState::Broken, "{case}");
        assert!(rig.engine.is_paused(id(2)), "{case}");
        assert_eq!(rig.state(3), CueState::Idle, "{case}");
        let idle = rig.tick(5000);
        assert!(starts(&idle).is_empty(), "{case}");
        let resumed = rig.at(6000, Input::ResumeAll);
        assert_eq!(starts(&resumed), vec![(id(3), 6000)], "{case}");
        assert_eq!(voice_starts(&resumed), vec![(id(3), 6000)], "{case}");
        assert_eq!(rig.state(2), CueState::Broken, "{case}");
        assert!(!rig.engine.is_paused(id(2)), "{case}");
    }
}

/// A stop or a panic during that hold cancels the step; resuming the held child alone (not Resume
/// All) carries the playlist on, as any resume does.
#[test]
fn a_held_playlist_step_is_cancelled_by_a_stop_or_panic_and_kept_by_a_resume() {
    for cancel in [
        Input::Stop {
            cue: id(2),
            fade_out: None,
        },
        Input::Stop {
            cue: id(1),
            fade_out: None,
        },
        Input::Panic,
        Input::StopAll,
    ] {
        let mut rig = Rig::new(vec![playlist(1, vec![sound(2), sound(3)])]);
        rig.start(0, 2);
        rig.at(100, Input::PauseAll);
        fail(&mut rig, 200, 2);
        let case = format!("{cancel:?}");
        let cancelled = rig.at(300, cancel);
        assert!(starts(&cancelled).is_empty(), "{case}");
        assert_eq!(rig.state(2), CueState::Broken, "{case}");
        assert!(!rig.engine.is_paused(id(2)), "{case}");
        let later = rig.at(400, Input::ResumeAll);
        assert!(starts(&later).is_empty(), "{case}: {later:?}");
        assert!(starts(&rig.tick(10_000)).is_empty(), "{case}");
        assert_eq!(rig.state(3), CueState::Idle, "{case}");
    }

    let mut rig = Rig::new(vec![playlist(1, vec![sound(2), sound(3)])]);
    rig.start(0, 2);
    rig.at(100, Input::Pause { cue: id(2) });
    let failed = fail(&mut rig, 200, 2);
    assert!(starts(&failed).is_empty(), "{failed:?}");
    let resumed = rig.at(300, Input::Resume { cue: id(2) });
    assert_eq!(voice_starts(&resumed), vec![(id(3), 300)]);
}

/// A running playlist holds the step itself (`advance`), as before: on the resume it moves on.
#[test]
fn a_failure_while_paused_in_a_running_playlist_starts_nothing_until_the_resume() {
    let mut rig = Rig::new(vec![playlist(1, vec![sound(2), sound(3)])]);
    rig.start(0, 1);
    rig.at(100, Input::PauseAll);
    let failed = fail_as(&mut rig, 200, 2, true);
    assert!(starts(&failed).is_empty(), "{failed:?}");
    assert!(voice_starts(&failed).is_empty());
    let resumed = rig.at(6000, Input::ResumeAll);
    assert_eq!(voice_starts(&resumed), vec![(id(3), 6000)]);
}

/// Stop All and a second Panic stop at once: the failed run's report comes for a run that is gone
/// and is ignored.
#[test]
fn a_stream_failure_after_stop_all_or_a_hard_panic_is_ignored() {
    for hard in [Input::StopAll, Input::Panic] {
        let mut rig = Rig::new(vec![playlist(1, vec![sound(2), sound(3)])]);
        rig.start(0, 2);
        let run = rig.run(2);
        if matches!(hard, Input::Panic) {
            rig.panic(50);
        }
        rig.at(100, hard);
        let failed = rig.at(
            200,
            Input::ActionFailed {
                cue: id(2),
                run,
                reason: BrokenReason::StreamFailed,
            },
        );
        assert!(failed.is_empty(), "{failed:?}");
        rig.assert_quiet();
    }
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

#[test]
fn a_clock_restart_ends_the_panic_window() {
    // Regression (review L2): after a device restart the old panic's end stayed, so the next
    // Panic was a hard stop and `panic_end()` was stale.
    let mut rig = Rig::new(vec![sound(1)]);
    rig.panic(5000);
    assert_eq!(rig.engine.panic_end(), Some(6000));
    rig.start(50, 1);
    assert_eq!(rig.engine.panic_end(), None);
    let panic = rig.panic(60);
    assert_eq!(voice_stops(&panic), vec![(id(1), 60, panic_fade(1000))]);
    assert_eq!(rig.engine.panic_end(), Some(1060));
}

// ----- Stop All and a cue's own panic (OSC `/stop`, `/hardStop`, `/cue/{n}/panic`) -----

#[test]
fn stop_all_stops_every_cue_of_every_list_and_depth_at_once() {
    let mut rig = Rig::lists(
        vec![
            vec![
                sound(1),
                grp(2, GroupMode::Timeline, vec![sound(3)]),
                timed(memo(7), 1.0, 0.0, DNC),
            ],
            vec![sound(6)],
        ],
        0,
    );
    for n in [1, 2, 6, 7] {
        rig.start(0, n);
    }
    let stop = rig.at(100, Input::StopAll);
    // No fade, children before their groups, on the sample of the input.
    assert_eq!(
        voice_stops(&stop),
        vec![(id(1), 100, None), (id(3), 100, None), (id(6), 100, None)]
    );
    for n in [1, 2, 3, 6, 7] {
        assert_eq!(rig.state(n), CueState::Idle, "cue {n}");
    }
    // The pending pre-wait of cue 7 fires nothing, and no panic window was opened.
    assert!(starts(&rig.tick(5000)).is_empty());
    rig.assert_quiet();
}

#[test]
fn stop_all_ends_a_panic_fade_and_leaves_no_panic_window() {
    let mut rig = Rig::new(vec![sound(1), sound(2)]);
    rig.start(0, 1);
    rig.panic(0);
    assert!(rig.engine.panic_end().is_some());
    let stop = rig.at(200, Input::StopAll);
    assert_eq!(voice_stops(&stop), vec![(id(1), 200, None)]);
    assert_eq!(rig.engine.panic_end(), None);
    // A Panic right after is a new one with its fade, not the "second press" hard stop.
    rig.start(300, 2);
    assert_eq!(
        voice_stops(&rig.panic(400)),
        vec![(id(2), 400, panic_fade(1000))]
    );
}

#[test]
fn stop_all_stops_paused_cues_and_does_nothing_when_idle() {
    let mut rig = Rig::new(vec![sound(1), sound(2)]);
    assert!(rig.at(0, Input::StopAll).is_empty());
    rig.start(0, 1);
    rig.at(50, Input::PauseAll);
    rig.at(100, Input::StopAll);
    assert_eq!(rig.state(1), CueState::Idle);
    rig.assert_quiet();
}

#[test]
fn a_cues_own_panic_fades_it_and_what_is_in_it_and_leaves_the_rest() {
    let mut rig = Rig::new(vec![
        sound(1),
        grp(2, GroupMode::Timeline, vec![sound(3)]),
        sound(4),
    ]);
    for n in [1, 2, 4] {
        rig.start(0, n);
    }
    let panic = rig.at(100, Input::PanicCue { cue: id(2) });
    assert_eq!(voice_stops(&panic), vec![(id(3), 100, panic_fade(1000))]);
    assert_eq!(rig.state(3), CueState::Stopping);
    assert_eq!(rig.state(1), CueState::Running);
    assert_eq!(rig.state(4), CueState::Running);
    // Not a workspace panic: there is no panic window, so a Panic is a first one.
    assert_eq!(rig.engine.panic_end(), None);
    let end = rig.tick(1100);
    assert_eq!(changes(&end, 3), vec![(CueState::Idle, 1100)]);
    assert_eq!(rig.state(1), CueState::Running);
}

#[test]
fn a_second_panic_on_a_cue_fading_out_stops_it_at_once_and_leaves_the_rest() {
    // M4.1d (QLab check Q7): only the selected cues fade, a second press on them stops them at
    // once, other cues keep playing.
    let mut rig = Rig::new(vec![
        sound(1),
        grp(2, GroupMode::Timeline, vec![sound(3)]),
        sound(4),
    ]);
    for n in [1, 2, 4] {
        rig.start(0, n);
    }
    rig.at(100, Input::PanicCue { cue: id(1) });
    rig.at(100, Input::PanicCue { cue: id(2) });
    assert_eq!(rig.state(1), CueState::Stopping);
    assert_eq!(rig.state(3), CueState::Stopping);
    // Before the fades end: cue 1 and the group stop at once; cue 4 plays on.
    let second = rig.at(500, Input::PanicCue { cue: id(1) });
    assert_eq!(voice_stops(&second), vec![(id(1), 500, None)]);
    assert_eq!(changes(&second, 1), vec![(CueState::Idle, 500)]);
    assert_eq!(rig.state(3), CueState::Stopping, "the group's own press");
    let second = rig.at(600, Input::PanicCue { cue: id(2) });
    assert_eq!(voice_stops(&second), vec![(id(3), 600, None)]);
    assert_eq!(rig.state(3), CueState::Idle);
    assert_eq!(rig.state(2), CueState::Idle);
    assert_eq!(rig.state(4), CueState::Running);
    assert_eq!(rig.engine.panic_end(), None);
    // A press on a cue that is playing again is a first press with its fade.
    rig.start(700, 1);
    assert_eq!(
        voice_stops(&rig.at(800, Input::PanicCue { cue: id(1) })),
        vec![(id(1), 800, panic_fade(1000))]
    );
}

#[test]
fn a_first_panic_on_a_group_with_only_some_children_fading_fades_the_rest() {
    // Panic Selected on A, then on its group: B has not been panicked, so it fades (no click).
    let mut rig = Rig::new(vec![grp(1, GroupMode::Timeline, vec![sound(2), sound(3)])]);
    rig.start(0, 1);
    rig.at(100, Input::PanicCue { cue: id(2) });
    let group = rig.at(300, Input::PanicCue { cue: id(1) });
    assert_eq!(voice_stops(&group), vec![(id(3), 300, panic_fade(1000))]);
    assert_eq!(rig.state(3), CueState::Stopping);
    // A press on the group now finds every child fading: a second press.
    let second = rig.at(400, Input::PanicCue { cue: id(1) });
    assert_eq!(
        voice_stops(&second),
        vec![(id(3), 400, None), (id(2), 400, None)]
    );
}

#[test]
fn a_panic_cue_on_a_cue_fading_in_a_workspace_panic_stops_it_at_once() {
    let mut rig = Rig::new(vec![sound(1), sound(2)]);
    rig.start(0, 1);
    rig.start(0, 2);
    rig.panic(100);
    let one = rig.at(300, Input::PanicCue { cue: id(1) });
    assert_eq!(voice_stops(&one), vec![(id(1), 300, None)]);
    assert_eq!(rig.state(1), CueState::Idle);
    assert_eq!(
        rig.state(2),
        CueState::Stopping,
        "the workspace panic goes on"
    );
    assert_eq!(rig.engine.panic_end(), Some(1100));
}

#[test]
fn a_cues_own_panic_with_no_panic_duration_stops_it_at_once() {
    let mut rig = Rig::new(vec![sound(1), sound(2)]);
    rig.ws.settings.panic_duration = 0.0;
    rig.start(0, 1);
    rig.start(0, 2);
    let panic = rig.at(100, Input::PanicCue { cue: id(1) });
    assert_eq!(voice_stops(&panic), vec![(id(1), 100, None)]);
    assert_eq!(rig.state(1), CueState::Idle);
    assert_eq!(rig.state(2), CueState::Running);
}

#[test]
fn a_cues_own_panic_on_an_unknown_or_idle_cue_changes_nothing() {
    let mut rig = Rig::new(vec![sound(1)]);
    let unknown = rig.at(0, Input::PanicCue { cue: id(99) });
    assert!(voice_stops(&unknown).is_empty());
    assert!(rig.at(0, Input::PanicCue { cue: id(1) }).is_empty());
    rig.assert_quiet();
}

#[test]
fn go_from_moves_the_playhead_then_goes_and_does_nothing_for_a_cue_it_cannot_rest_on() {
    let mut rig = Rig::new(vec![
        sound(1),
        sound(2),
        grp(3, GroupMode::Timeline, vec![sound(4)]),
    ]);
    let from = |rig: &mut Rig, n| {
        rig.at(
            0,
            Input::GoFrom {
                list: list_id(1),
                cue: id(n),
            },
        )
    };
    let started = from(&mut rig, 2);
    assert_eq!(voice_starts(&started), vec![(id(2), 0)]);
    assert_eq!(rig.state(1), CueState::Idle);
    assert_eq!(rig.engine.playhead(list_id(1)), Some(id(3)));
    // Not on the list: a warning and nothing started, the playhead stays.
    {
        let n = 99;
        let out = from(&mut rig, n);
        assert!(voice_starts(&out).is_empty());
        assert!(warnings(&out).contains(&EngineWarning::NotInList {
            list: list_id(1),
            cue: id(n)
        }));
    }
    assert_eq!(rig.engine.playhead(list_id(1)), Some(id(3)));
}

#[test]
fn a_refused_go_from_leaves_the_playhead_where_it_was() {
    let mut rig = Rig::new(vec![sound(1), sound(2), sound(3)]);
    rig.ws.settings.min_go_interval = 1.0;
    let from = |rig: &mut Rig, now, n| {
        rig.at(
            now,
            Input::GoFrom {
                list: list_id(1),
                cue: id(n),
            },
        )
    };
    assert_eq!(voice_starts(&from(&mut rig, 0, 2)), vec![(id(2), 0)]);
    assert_eq!(rig.engine.playhead(list_id(1)), Some(id(3)));
    // Within the double-GO interval: refused, and the playhead is not moved to cue 1.
    let out = from(&mut rig, 100, 1);
    assert!(voice_starts(&out).is_empty());
    assert!(out.iter().any(|a| matches!(
        a,
        Action::GoIgnored {
            reason: GoIgnoredReason::DoubleGo,
            ..
        }
    )));
    assert_eq!(rig.engine.playhead(list_id(1)), Some(id(3)));
    // After the interval it works.
    assert_eq!(voice_starts(&from(&mut rig, 2000, 1)), vec![(id(1), 2000)]);
}

// ----- an end dated before a pause, arriving after it (M2.33) -----

/// Reports `cue`'s current run ended on `at`, received at `now`.
fn ended(rig: &mut Rig, now: SampleTime, n: u128, at: SampleTime) -> Vec<Action> {
    let run = rig.run(n);
    rig.at(
        now,
        Input::ActionEnded {
            cue: id(n),
            run,
            at,
        },
    )
}

fn late_starts(actions: &[Action]) -> usize {
    warnings(actions)
        .iter()
        .filter(|w| matches!(w, EngineWarning::LateStart { .. }))
        .count()
}

/// The reviewer's scenario: a child started on its own in a playlist ends on 90, just before Pause
/// All on 100, and the report arrives on 150. A paused show starts nothing: the next child starts
/// on the resume sample, not late at 150 while everything else is paused.
#[test]
fn an_end_dated_before_pause_all_holds_a_playlist_step_until_the_resume() {
    for loop_ in [false, true] {
        let mut rig = Rig::new(vec![playlist_of(1, loop_, vec![sound(2), sound(3)])]);
        rig.start(0, 2);
        rig.at(100, Input::PauseAll);
        let end = ended(&mut rig, 150, 2, 90);
        let case = format!("loop {loop_}: {end:?}");
        assert!(starts(&end).is_empty(), "{case}");
        assert!(voice_starts(&end).is_empty(), "{case}");
        assert_eq!(late_starts(&end), 0, "{case}");
        assert_eq!(rig.state(2), CueState::Paused, "{case}");
        assert_eq!(rig.state(3), CueState::Idle, "{case}");
        assert!(voice_starts(&rig.tick(5000)).is_empty(), "{case}");
        let resumed = rig.at(6000, Input::ResumeAll);
        assert_eq!(starts(&resumed), vec![(id(3), 6000)], "{case}");
        assert_eq!(voice_starts(&resumed), vec![(id(3), 6000)], "{case}");
        assert_eq!(late_starts(&resumed), 0, "{case}");
        // The ended voice is not resumed: there is nothing left of it.
        assert!(
            !resumed
                .iter()
                .any(|a| matches!(a, Action::ResumeVoice { cue, .. } if *cue == id(2))),
            "{case}"
        );
        assert_eq!(
            changes(&resumed, 2).last(),
            Some(&(CueState::Idle, 6000)),
            "{case}"
        );
    }
}

/// A plain auto-follow (continue mode) waits for the resume the same way, also when the cue alone
/// is paused and resumed, or a trigger on it resumes it; the follower's pre-wait counts from the
/// resume sample.
#[test]
fn an_end_dated_before_a_pause_holds_an_auto_follow_until_the_resume() {
    let resumes = [
        Input::ResumeAll,
        Input::Resume { cue: id(2) },
        Input::Start { cue: id(2) },
    ];
    for resume in resumes {
        let mut rig = Rig::new(vec![
            timed(sound(2), 0.0, 0.0, ContinueMode::AutoFollow),
            timed(sound(3), 1.0, 0.0, DNC),
        ]);
        rig.start(0, 2);
        rig.at(100, Input::PauseAll);
        let end = ended(&mut rig, 150, 2, 90);
        let case = format!("{resume:?}: {end:?}");
        assert!(starts(&end).is_empty(), "{case}");
        assert!(voice_starts(&end).is_empty(), "{case}");
        assert_eq!(rig.state(2), CueState::Paused, "{case}");
        assert!(starts(&rig.tick(5000)).is_empty(), "{case}");
        let resumed = rig.at(6000, resume);
        assert_eq!(starts(&resumed), vec![(id(3), 6000)], "{case}");
        assert_eq!(late_starts(&resumed), 0, "{case}");
        assert_eq!(rig.state(2), CueState::Idle, "{case}");
        assert_eq!(voice_starts(&rig.tick(7000)), vec![(id(3), 7000)], "{case}");
    }
}

/// With a lookahead window the follower is still dated on the resume sample, never before it.
#[test]
fn a_held_end_releases_on_the_resume_sample_with_a_lookahead() {
    let mut rig = Rig::lists(
        vec![vec![
            timed(sound(2), 0.0, 0.0, ContinueMode::AutoFollow),
            sound(3),
        ]],
        64,
    );
    rig.start(0, 2);
    rig.at(100, Input::PauseAll);
    let end = ended(&mut rig, 150, 2, 90);
    assert!(voice_starts(&end).is_empty(), "{end:?}");
    let resumed = rig.at(6000, Input::ResumeAll);
    assert_eq!(voice_starts(&resumed), vec![(id(3), 6000)]);
}

/// Stop, Reset, Panic and Stop All during the hold cancel it: nothing starts, then or later.
#[test]
fn a_held_end_is_cancelled_by_a_stop_reset_or_panic() {
    let cancels = [
        Input::Stop {
            cue: id(2),
            fade_out: None,
        },
        Input::Reset { cue: id(2) },
        Input::Panic,
        Input::StopAll,
    ];
    for cancel in cancels {
        for in_playlist in [false, true] {
            let cues = if in_playlist {
                vec![playlist_of(1, false, vec![sound(2), sound(3)])]
            } else {
                vec![
                    timed(sound(2), 0.0, 0.0, ContinueMode::AutoFollow),
                    sound(3),
                ]
            };
            let mut rig = Rig::new(cues);
            rig.start(0, 2);
            rig.at(100, Input::PauseAll);
            ended(&mut rig, 150, 2, 90);
            let case = format!("{cancel:?}, playlist {in_playlist}");
            let cancelled = rig.at(300, cancel.clone());
            assert!(starts(&cancelled).is_empty(), "{case}: {cancelled:?}");
            assert!(voice_starts(&cancelled).is_empty(), "{case}");
            assert_eq!(rig.state(2), CueState::Idle, "{case}");
            let later = rig.at(400, Input::ResumeAll);
            assert!(starts(&later).is_empty(), "{case}: {later:?}");
            assert!(starts(&rig.tick(10_000)).is_empty(), "{case}");
            assert_eq!(rig.state(3), CueState::Idle, "{case}");
        }
    }
}

/// GO during the hold starts the next cue (the operator asked for it); the held follow then finds
/// it active on the resume and starts nothing twice.
#[test]
fn a_go_during_the_hold_and_the_resumed_follow_start_the_next_cue_once() {
    let mut rig = Rig::new(vec![
        timed(sound(2), 0.0, 0.0, ContinueMode::AutoFollow),
        sound(3),
    ]);
    rig.go(0);
    rig.at(100, Input::PauseAll);
    ended(&mut rig, 150, 2, 90);
    // The playhead skipped the auto-followed cue: put it back on it.
    rig.at(
        180,
        Input::SetPlayhead {
            list: list_id(1),
            cue: Some(id(3)),
        },
    );
    let go = rig.go(200);
    assert_eq!(voice_starts(&go), vec![(id(3), 200)]);
    let resumed = rig.at(6000, Input::ResumeAll);
    assert!(voice_starts(&resumed).is_empty(), "{resumed:?}");
    assert_eq!(rig.state(3), CueState::Running);
    assert_eq!(rig.state(2), CueState::Idle);
}

/// An edit during the hold: the follow is looked up on the resume, so a removed next cue is not
/// started (the cue after it is), and a removed held cue starts nothing.
#[test]
fn a_held_end_follows_the_workspace_as_it_is_on_the_resume() {
    let make = || {
        Rig::new(vec![
            timed(sound(2), 0.0, 0.0, ContinueMode::AutoFollow),
            sound(3),
            sound(4),
        ])
    };
    let mut rig = make();
    rig.start(0, 2);
    rig.at(100, Input::PauseAll);
    ended(&mut rig, 150, 2, 90);
    rig.ws.cue_lists[0].cues.retain(|c| c.id != id(3));
    assert!(voice_starts(&rig.tick(200)).is_empty());
    let resumed = rig.at(6000, Input::ResumeAll);
    assert_eq!(voice_starts(&resumed), vec![(id(4), 6000)]);

    let mut rig = make();
    rig.start(0, 2);
    rig.at(100, Input::PauseAll);
    ended(&mut rig, 150, 2, 90);
    rig.ws.cue_lists[0].cues.retain(|c| c.id != id(2));
    assert!(voice_starts(&rig.tick(200)).is_empty());
    let resumed = rig.at(6000, Input::ResumeAll);
    assert!(voice_starts(&resumed).is_empty(), "{resumed:?}");
    assert!(voice_starts(&rig.tick(10_000)).is_empty());
}

/// A devamped voice that left its loop on 80 and ended on 90, both before the pause on 100: the
/// Devamp cue's next cue waits for the resume too.
#[test]
fn an_end_dated_before_a_pause_holds_the_devamp_next_cue_until_the_resume() {
    let mut rig = Rig::new(vec![sound(1), devamp_cue(2, 1), memo(3)]);
    rig.start(0, 1);
    rig.start(10, 2);
    let run = rig.run(1);
    rig.at(
        20,
        Input::DevampScheduled {
            cue: id(1),
            run,
            at: 5000,
        },
    );
    rig.at(100, Input::PauseAll);
    let end = ended(&mut rig, 150, 1, 90);
    assert!(starts(&end).is_empty(), "{end:?}");
    assert_eq!(rig.state(1), CueState::Paused);
    // Late answers for the ended voice change nothing.
    let ignored = rig.at(160, Input::DevampIgnored { cue: id(1), run });
    assert!(warnings(&ignored).is_empty(), "{ignored:?}");
    assert!(starts(&rig.tick(5000)).is_empty());
    let resumed = rig.at(6000, Input::ResumeAll);
    assert_eq!(starts(&resumed), vec![(id(3), 6000)]);
    assert_eq!(late_starts(&resumed), 0);
    assert_eq!(rig.state(1), CueState::Idle);
}

/// A failure report for the voice that already ended (a broken binary sending both) does not
/// break the held cue or cancel its follow.
#[test]
fn a_failure_after_a_held_end_is_ignored() {
    for unplayed in [false, true] {
        let mut rig = Rig::new(vec![playlist_of(1, false, vec![sound(2), sound(3)])]);
        rig.start(0, 2);
        rig.at(100, Input::PauseAll);
        ended(&mut rig, 150, 2, 90);
        let failed = fail_as(&mut rig, 160, 2, unplayed);
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(rig.state(2), CueState::Paused);
        // A second end report for it changes nothing either.
        assert!(ended(&mut rig, 170, 2, 95).is_empty());
        let resumed = rig.at(6000, Input::ResumeAll);
        assert_eq!(voice_starts(&resumed), vec![(id(3), 6000)]);
    }
}

/// With nothing to start, an end dated before the pause still completes the cue at its `at`.
#[test]
fn an_end_dated_before_a_pause_with_nothing_to_start_completes_the_cue() {
    let mut rig = Rig::new(vec![sound(2), sound(3)]);
    rig.start(0, 2);
    rig.at(100, Input::PauseAll);
    let end = ended(&mut rig, 150, 2, 90);
    assert_eq!(changes(&end, 2), vec![(CueState::Idle, 90)]);
    assert!(starts(&end).is_empty());
    assert!(rig.at(6000, Input::ResumeAll).is_empty());
}

// ----- an end on the pause sample (M2.34) -----

/// Pause All on 100; the voice's end report, dated `at`, arrives on 150. The mixer dates an end
/// on the first sample after the voice's last, so an end on 100 left nothing to play when the
/// pause applied: it is an end, as one before it (M2.33). One after it is the pause being carried
/// out. Whatever the tie, a cue left paused either still has its voice (the resume resumes it)
/// or holds an end the resume carries out: never paused with nothing to resume.
#[test]
fn an_end_on_the_pause_sample_ends_the_cue_and_one_after_it_is_the_pause() {
    for at in [99, 100, 101] {
        for follow in [false, true] {
            let mode = if follow {
                ContinueMode::AutoFollow
            } else {
                DNC
            };
            let mut rig = Rig::new(vec![timed(sound(2), 0.0, 0.0, mode), sound(3)]);
            rig.start(0, 2);
            let paused_run = rig.run(2);
            rig.at(100, Input::PauseAll);
            let end = ended(&mut rig, 150, 2, at);
            let case = format!("end on {at}, follow {follow}: {end:?}");
            // A paused show starts nothing, whatever the tie.
            assert!(starts(&end).is_empty(), "{case}");
            assert!(voice_starts(&end).is_empty(), "{case}");
            let resumed = rig.at(6000, Input::ResumeAll);
            let case = format!("{case}; resume {resumed:?}");
            let resumes_voice = resumed.iter().any(
                |a| matches!(a, Action::ResumeVoice { cue, paused, .. } if *cue == id(2) && *paused == paused_run),
            );
            if at > 100 {
                // Still paused with its voice: the resume plays it on, and nothing follows yet.
                assert!(changes(&end, 2).is_empty(), "{case}");
                assert!(resumes_voice, "{case}");
                assert!(starts(&resumed).is_empty(), "{case}");
                assert_eq!(rig.state(2), CueState::Running, "{case}");
            } else if follow {
                // Held (M2.33): paused with no voice until the resume, which carries the end out.
                assert!(changes(&end, 2).is_empty(), "{case}");
                assert!(!resumes_voice, "{case}");
                assert_eq!(starts(&resumed), vec![(id(3), 6000)], "{case}");
                assert_eq!(
                    changes(&resumed, 2).last(),
                    Some(&(CueState::Idle, 6000)),
                    "{case}"
                );
            } else {
                // Completed on its own sample; the resume has nothing to do for it.
                assert_eq!(changes(&end, 2), vec![(CueState::Idle, at)], "{case}");
                assert!(resumed.is_empty(), "{case}");
            }
            assert_ne!(rig.state(2), CueState::Paused, "{case}");
        }
    }
}

/// The same tie in a playlist with no run (the child started on its own): an end on the pause
/// sample holds the next child until the resume.
#[test]
fn an_end_on_the_pause_sample_holds_a_playlist_step_until_the_resume() {
    let mut rig = Rig::new(vec![playlist_of(1, false, vec![sound(2), sound(3)])]);
    rig.start(0, 2);
    rig.at(100, Input::PauseAll);
    let end = ended(&mut rig, 150, 2, 100);
    assert!(voice_starts(&end).is_empty(), "{end:?}");
    assert_eq!(rig.state(2), CueState::Paused);
    let resumed = rig.at(6000, Input::ResumeAll);
    assert_eq!(voice_starts(&resumed), vec![(id(3), 6000)], "{resumed:?}");
    assert_eq!(rig.state(2), CueState::Idle);
}

/// An end the engine knows ahead (`ActionLength`) on the pause sample is carried out before the
/// pause: the rule a reported end on that sample follows too. Its follow starts on that sample,
/// before Pause All pauses it, and the late report of the end changes nothing.
#[test]
fn a_known_end_on_the_pause_sample_comes_before_the_pause() {
    let mut rig = Rig::new(vec![
        timed(sound(2), 0.0, 0.0, ContinueMode::AutoFollow),
        sound(3),
    ]);
    rig.start(0, 2);
    let run = rig.run(2);
    rig.at(
        10,
        Input::ActionLength {
            cue: id(2),
            run,
            frames: 100,
        },
    );
    let paused = rig.at(100, Input::PauseAll);
    assert_eq!(
        changes(&paused, 2),
        vec![(CueState::Idle, 100)],
        "{paused:?}"
    );
    assert_eq!(voice_starts(&paused), vec![(id(3), 100)], "{paused:?}");
    assert_eq!(rig.state(3), CueState::Paused);
    let late = rig.at(
        150,
        Input::ActionEnded {
            cue: id(2),
            run,
            at: 100,
        },
    );
    assert!(late.is_empty(), "{late:?}");
}

/// A preview's voice ending on the Pause All sample (cue_engine_prop seed 886fa41f): the preview
/// ends, as one ending before it; one ending after it is the pause, and the preview stays paused
/// with its voice for Resume All. A paused preview always has a voice to resume.
#[test]
fn a_preview_ending_on_the_pause_sample_ends_and_one_after_it_is_the_pause() {
    for at in [99, 100, 101] {
        let mut rig = Rig::new(vec![sound(1)]);
        rig.at(0, Input::Preview { cue: id(1) });
        let run = rig.engine.preview_run(id(1)).unwrap();
        rig.at(100, Input::PauseAll);
        let end = rig.at(
            150,
            Input::ActionEnded {
                cue: id(1),
                run,
                at,
            },
        );
        let case = format!("end on {at}: {end:?}");
        let resumed = rig.at(6000, Input::ResumeAll);
        let resumes_voice = resumed.iter().any(
            |a| matches!(a, Action::ResumeVoice { cue, paused, .. } if *cue == id(1) && *paused == run),
        );
        if at > 100 {
            assert!(end.is_empty(), "{case}");
            assert!(resumes_voice, "{case}; {resumed:?}");
            assert!(rig.engine.is_previewing(id(1)), "{case}");
        } else {
            assert_eq!(
                end,
                vec![Action::PreviewChanged {
                    cue: id(1),
                    active: false,
                    at
                }],
                "{case}"
            );
            assert!(!rig.engine.is_previewing(id(1)), "{case}");
            assert!(resumed.is_empty(), "{case}; {resumed:?}");
        }
    }
}
