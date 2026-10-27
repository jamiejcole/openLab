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
