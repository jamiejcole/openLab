// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Fade cues (M2.6, CT-10 audio levels), per docs/cue-engine.md "Fade cues". Virtual sample
//! clock at 1 kHz: one second is 1000 samples.

mod common;

use common::*;
use openlab_core::*;

const RATE: u32 = 1000;

use ContinueMode::{AutoFollow as AF, DoNotContinue as DNC};

fn id(n: u128) -> CueId {
    cue_id(n)
}

fn memo(n: u128) -> Cue {
    cue(n, CueKind::Memo)
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

/// A Fade of cue `target` over `seconds` to `levels`.
fn fade_to(
    n: u128,
    target: u128,
    seconds: f64,
    curve: FadeCurve,
    levels: FadeLevels,
    stop: bool,
) -> Cue {
    cue(
        n,
        CueKind::Fade {
            target: Some(id(target)),
            duration: seconds,
            curve,
            levels,
            stop_target_when_done: stop,
        },
    )
}

fn master(db: f64) -> FadeLevels {
    FadeLevels {
        master_db: Some(db),
        output_db: vec![],
        crosspoints: vec![],
    }
}

/// A linear master fade of cue 1 over `seconds` to `db`.
fn master_fade(n: u128, seconds: f64, db: f64) -> Cue {
    fade_to(n, 1, seconds, FadeCurve::Linear, master(db), false)
}

struct Rig {
    ws: Workspace,
    engine: CueEngine,
}

impl Rig {
    fn new(cues: Vec<Cue>) -> Self {
        Self::with_lookahead(cues, 0)
    }

    fn with_lookahead(cues: Vec<Cue>, lookahead: SampleTime) -> Self {
        let mut rig = Self {
            ws: workspace(cues),
            engine: CueEngine::new(EngineConfig::new(RATE, lookahead, 1).unwrap()),
        };
        rig.tick(0);
        rig
    }

    fn at(&mut self, now: SampleTime, input: Input) -> Vec<Action> {
        self.engine.handle(&self.ws, input, now)
    }

    fn tick(&mut self, now: SampleTime) -> Vec<Action> {
        self.at(now, Input::Tick)
    }

    fn start(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(now, Input::Start { cue: id(n) })
    }

    fn pause(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(now, Input::Pause { cue: id(n) })
    }

    fn resume(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(now, Input::Resume { cue: id(n) })
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

    fn state(&self, n: u128) -> CueState {
        self.engine.state(id(n))
    }

    fn level(&self, n: u128, param: LevelParam, at: SampleTime) -> Option<f32> {
        self.engine.fade_level(id(n), param, at)
    }
}

#[derive(Debug, PartialEq)]
struct R {
    cue: CueId,
    at: SampleTime,
    param: LevelParam,
    gain: f32,
    frames: SampleTime,
    curve: FadeCurve,
}

fn ramps(actions: &[Action]) -> Vec<R> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Ramp {
                cue,
                at,
                param,
                gain,
                frames,
                curve,
                ..
            } => Some(R {
                cue: *cue,
                at: *at,
                param: *param,
                gain: *gain,
                frames: *frames,
                curve: *curve,
            }),
            _ => None,
        })
        .collect()
}

fn ramp(n: u128, at: SampleTime, param: LevelParam, gain: f32, frames: SampleTime) -> R {
    R {
        cue: id(n),
        at,
        param,
        gain,
        frames,
        curve: FadeCurve::Linear,
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

fn warnings(actions: &[Action]) -> Vec<EngineWarning> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Warning(w) => Some(w.clone()),
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

const M: LevelParam = LevelParam::Master;

#[test]
fn fade_ramps_the_targets_levels_with_its_curve_and_duration() {
    let levels = FadeLevels {
        master_db: Some(SILENCE_DB),
        // Output 2 is beyond the target's two outputs: ignored.
        output_db: vec![Some(-6.0), None, Some(-3.0)],
        crosspoints: vec![
            FadeCrosspoint {
                input: 1,
                output: 0,
                db: -10.0,
            },
            // Output 5 does not exist: ignored.
            FadeCrosspoint {
                input: 0,
                output: 5,
                db: -10.0,
            },
        ],
    };
    let mut rig = Rig::new(vec![
        sound(1),
        fade_to(2, 1, 5.0, FadeCurve::EqualPower, levels, false),
    ]);
    rig.start(0, 1);
    let run = rig.run(1);
    let go = rig.start(1000, 2);
    let eq = |param, gain| R {
        cue: id(1),
        at: 1000,
        param,
        gain,
        frames: 5000,
        curve: FadeCurve::EqualPower,
    };
    assert_eq!(
        ramps(&go),
        vec![
            eq(M, 0.0),
            eq(LevelParam::Output(0), db_to_gain(-6.0)),
            eq(
                LevelParam::Crosspoint {
                    input: 1,
                    output: 0
                },
                db_to_gain(-10.0)
            ),
        ]
    );
    assert!(
        go.iter()
            .all(|a| !matches!(a, Action::Ramp { run: r, .. } if *r != run))
    );
    assert!(warnings(&go).is_empty());
    assert_eq!(rig.state(1), CueState::Fading);
    assert_eq!(rig.state(2), CueState::Running);
    // The unlisted crosspoint starts from silence, the rest from the model's levels.
    let xp = LevelParam::Crosspoint {
        input: 1,
        output: 0,
    };
    assert_eq!(rig.level(1, xp, 1000), Some(0.0));
    assert_eq!(rig.level(1, M, 1000), Some(1.0));
    assert_eq!(
        rig.level(1, M, 3500),
        Some(ramp_gain(1.0, 0.0, 2500, 5000, FadeCurve::EqualPower))
    );
    // The fade's action completes at its end; the target is back to running.
    assert!(changes(&rig.tick(5999), 2).is_empty());
    let end = rig.tick(6000);
    assert_eq!(changes(&end, 2), vec![(CueState::Idle, 6000)]);
    assert_eq!(changes(&end, 1), vec![(CueState::Running, 6000)]);
    assert_eq!(rig.level(1, M, 6000), Some(0.0));
}

#[test]
fn stop_target_when_done_stops_it_on_the_fades_last_sample() {
    let mut rig = Rig::new(vec![
        sound(1),
        timed(
            fade_to(2, 1, 2.0, FadeCurve::Linear, master(SILENCE_DB), true),
            0.0,
            0.0,
            AF,
        ),
        memo(3),
    ]);
    rig.start(0, 1);
    rig.start(500, 2);
    assert!(voice_stops(&rig.tick(2499)).is_empty());
    let end = rig.tick(2500);
    assert_eq!(voice_stops(&end), vec![(id(1), 2500, None)]);
    assert_eq!(changes(&end, 1), vec![(CueState::Idle, 2500)]);
    // Its auto-follow fires on the same sample.
    assert_eq!(changes(&end, 3)[0], (CueState::PreWaiting, 2500));
}

#[test]
fn stop_target_in_the_lookahead_window_lands_on_the_exact_sample() {
    let mut rig = Rig::with_lookahead(
        vec![
            sound(1),
            fade_to(2, 1, 2.0, FadeCurve::Linear, master(SILENCE_DB), true),
        ],
        64,
    );
    rig.start(0, 1);
    rig.start(500, 2);
    let tick = rig.tick(2450);
    assert_eq!(voice_stops(&tick), vec![(id(1), 2500, None)]);
}

#[test]
fn chained_fades_start_from_where_the_last_one_ended() {
    let mut rig = Rig::new(vec![
        sound(1),
        timed(master_fade(2, 2.0, -20.0), 0.0, 0.0, AF),
        master_fade(3, 1.0, 0.0),
    ]);
    rig.start(0, 1);
    rig.start(0, 2);
    let end = rig.tick(2000);
    assert_eq!(ramps(&end), vec![ramp(1, 2000, M, 1.0, 1000)]);
    assert_eq!(rig.level(1, M, 2000), Some(db_to_gain(-20.0)));
    assert_eq!(
        rig.level(1, M, 2500),
        Some(ramp_gain(
            db_to_gain(-20.0),
            1.0,
            500,
            1000,
            FadeCurve::Linear
        ))
    );
    assert_eq!(rig.state(1), CueState::Fading);
    rig.tick(3000);
    assert_eq!(rig.state(1), CueState::Running);
    assert_eq!(rig.level(1, M, 3000), Some(1.0));
}

#[test]
fn a_fade_interrupted_by_another_continues_from_its_current_value() {
    let mut rig = Rig::new(vec![
        sound(1),
        master_fade(2, 4.0, SILENCE_DB),
        master_fade(3, 1.0, 0.0),
    ]);
    rig.start(0, 1);
    rig.start(0, 2);
    let second = rig.start(1000, 3);
    assert_eq!(ramps(&second), vec![ramp(1, 1000, M, 1.0, 1000)]);
    // The mixer starts the new ramp from where the first one stood: 0.75.
    assert_eq!(rig.level(1, M, 1000), Some(0.75));
    assert_eq!(rig.level(1, M, 1500), Some(0.875));
    // The first fade no longer owns the master: pausing or stopping it sends nothing.
    assert!(ramps(&rig.pause(1500, 2)).is_empty());
    assert!(ramps(&rig.stop(1600, 2)).is_empty());
    assert_eq!(rig.state(1), CueState::Fading);
    rig.tick(2000);
    assert_eq!(rig.level(1, M, 2000), Some(1.0));
    assert_eq!(rig.state(1), CueState::Running);
}

#[test]
fn pausing_a_fade_holds_its_ramp_and_resuming_carries_on() {
    let mut rig = Rig::new(vec![sound(1), master_fade(2, 4.0, SILENCE_DB)]);
    rig.start(0, 1);
    rig.start(0, 2);
    let pause = rig.pause(1000, 2);
    assert_eq!(ramps(&pause), vec![ramp(1, 1000, M, 0.75, 0)]);
    assert_eq!(changes(&pause, 1), vec![(CueState::Running, 1000)]);
    assert_eq!(rig.state(2), CueState::Paused);
    assert_eq!(rig.level(1, M, 2500), Some(0.75));
    // Resumed at 3000 with 3000 samples left: it ends on 6000.
    let resume = rig.resume(3000, 2);
    assert_eq!(ramps(&resume), vec![ramp(1, 3000, M, 0.0, 3000)]);
    assert_eq!(changes(&resume, 1), vec![(CueState::Fading, 3000)]);
    assert_eq!(rig.level(1, M, 4500), Some(0.375));
    rig.tick(5999);
    assert_eq!(rig.state(2), CueState::Running);
    rig.tick(6000);
    assert_eq!(rig.state(2), CueState::Idle);
    assert_eq!(rig.level(1, M, 6000), Some(0.0));
}

#[test]
fn stopping_a_fade_leaves_the_levels_where_they_are() {
    let mut rig = Rig::new(vec![
        sound(1),
        fade_to(2, 1, 4.0, FadeCurve::Linear, master(SILENCE_DB), true),
    ]);
    rig.start(0, 1);
    rig.start(0, 2);
    let stop = rig.stop(1000, 2);
    assert_eq!(ramps(&stop), vec![ramp(1, 1000, M, 0.75, 0)]);
    assert_eq!(changes(&stop, 1), vec![(CueState::Running, 1000)]);
    // Stop target when done never fires: the fade did not complete.
    assert!(voice_stops(&rig.tick(4000)).is_empty());
    assert_eq!(rig.state(1), CueState::Running);
    assert_eq!(rig.level(1, M, 4000), Some(0.75));
}

#[test]
fn stopping_a_paused_fade_sends_nothing_more() {
    let mut rig = Rig::new(vec![sound(1), master_fade(2, 4.0, SILENCE_DB)]);
    rig.start(0, 1);
    rig.start(0, 2);
    rig.pause(1000, 2);
    assert!(ramps(&rig.stop(2000, 2)).is_empty());
    assert_eq!(rig.level(1, M, 3000), Some(0.75));
}

#[test]
fn a_fade_on_a_group_fades_its_playing_audio_cues() {
    let mut short = sound(4);
    if let CueKind::Audio { levels, .. } = &mut short.kind {
        levels.outputs.truncate(1);
    }
    let fade = FadeLevels {
        master_db: Some(-6.0),
        output_db: vec![None, Some(-12.0)],
        crosspoints: vec![],
    };
    let mut rig = Rig::new(vec![
        cue(
            1,
            CueKind::Group {
                mode: GroupMode::Timeline,
                children: vec![sound(2), memo(3), short, sound(5)],
            },
        ),
        fade_to(6, 1, 1.0, FadeCurve::Linear, fade, true),
    ]);
    rig.start(0, 1);
    // Cue 5 has stopped: only playing audio cues are faded.
    rig.stop(10, 5);
    let go = rig.start(100, 6);
    let cues: Vec<(CueId, LevelParam)> = ramps(&go).iter().map(|r| (r.cue, r.param)).collect();
    assert_eq!(
        cues,
        vec![
            (id(2), M),
            (id(2), LevelParam::Output(1)),
            // Cue 4 has a single output: its output 1 is ignored.
            (id(4), M),
        ]
    );
    assert_eq!(rig.state(2), CueState::Fading);
    assert_eq!(rig.state(4), CueState::Fading);
    assert_eq!(rig.state(1), CueState::Running);
    // Stop target when done stops the group and everything in it (children last first).
    let end = rig.tick(1100);
    assert_eq!(
        voice_stops(&end),
        vec![(id(4), 1100, None), (id(2), 1100, None)]
    );
    assert_eq!(rig.state(1), CueState::Idle);
}

#[test]
fn a_fade_of_a_target_that_is_not_playing_warns_and_keeps_its_timing() {
    let mut rig = Rig::new(vec![
        sound(1),
        timed(
            fade_to(2, 1, 1.0, FadeCurve::Linear, master(SILENCE_DB), true),
            0.0,
            0.0,
            AF,
        ),
        memo(3),
    ]);
    let go = rig.start(0, 2);
    assert_eq!(
        warnings(&go),
        vec![EngineWarning::TargetNotRunning {
            cue: id(2),
            target: id(1)
        }]
    );
    assert!(ramps(&go).is_empty());
    assert_eq!(rig.state(2), CueState::Running);
    let end = rig.tick(1000);
    assert_eq!(changes(&end, 3)[0], (CueState::PreWaiting, 1000));
    assert!(voice_stops(&end).is_empty());
}

#[test]
fn a_fade_of_a_pre_waiting_or_broken_target_does_nothing() {
    let mut rig = Rig::new(vec![
        timed(sound(1), 5.0, 0.0, DNC),
        master_fade(2, 1.0, SILENCE_DB),
    ]);
    rig.start(0, 1);
    let go = rig.start(100, 2);
    assert!(ramps(&go).is_empty());
    assert_eq!(warnings(&go).len(), 1);
    rig.stop(200, 1);
    rig.ws.cue_lists[0].cues[0].pre_wait = 0.0;
    rig.start(300, 1);
    let run = rig.run(1);
    rig.at(
        310,
        Input::ActionFailed {
            cue: id(1),
            run,
            reason: BrokenReason::StreamFailed,
        },
    );
    rig.tick(2000);
    let go = rig.start(2000, 2);
    assert!(ramps(&go).is_empty());
    assert_eq!(rig.state(1), CueState::Broken);
}

#[test]
fn fade_with_missing_or_wrong_targets_is_broken() {
    let mut rig = Rig::new(vec![
        memo(1),
        master_fade(2, 1.0, 0.0),
        cue(
            3,
            CueKind::Fade {
                target: None,
                duration: 1.0,
                curve: FadeCurve::Linear,
                levels: master(0.0),
                stop_target_when_done: false,
            },
        ),
        fade_to(4, 99, 1.0, FadeCurve::Linear, master(0.0), true),
    ]);
    assert_eq!(
        rig.engine.problems(id(2)),
        &[CueProblem::TargetWrongKind { target: id(1) }]
    );
    assert_eq!(rig.engine.problems(id(3)), &[CueProblem::NoTarget]);
    assert_eq!(
        rig.engine.problems(id(4)),
        &[CueProblem::TargetMissing { target: id(99) }]
    );
    for n in [2, 3, 4] {
        assert_eq!(
            warnings(&rig.start(0, n)),
            vec![EngineWarning::CueBroken { cue: id(n) }]
        );
        assert_eq!(rig.engine.state(id(n)), CueState::Broken);
    }
}

#[test]
fn a_zero_length_fade_sets_the_levels_at_once() {
    let mut rig = Rig::new(vec![
        sound(1),
        fade_to(2, 1, 0.0, FadeCurve::SCurve, master(-6.0), true),
    ]);
    rig.start(0, 1);
    let go = rig.start(100, 2);
    assert_eq!(
        ramps(&go),
        vec![R {
            cue: id(1),
            at: 100,
            param: M,
            gain: db_to_gain(-6.0),
            frames: 0,
            curve: FadeCurve::SCurve
        }]
    );
    assert_eq!(voice_stops(&go), vec![(id(1), 100, None)]);
    assert_eq!(rig.state(2), CueState::Idle);
}

#[test]
fn muted_outputs_and_crosspoints_are_not_faded() {
    let mut target = sound(1);
    if let CueKind::Audio { levels, .. } = &mut target.kind {
        levels.outputs[1].mute = true;
        levels.crosspoints.push(Crosspoint {
            input: 0,
            output: 0,
            db: 0.0,
            mute: true,
        });
    }
    let fade = FadeLevels {
        master_db: None,
        output_db: vec![Some(-6.0), Some(-6.0)],
        crosspoints: vec![FadeCrosspoint {
            input: 0,
            output: 0,
            db: -6.0,
        }],
    };
    let mut rig = Rig::new(vec![
        target,
        fade_to(2, 1, 1.0, FadeCurve::Linear, fade, false),
    ]);
    rig.start(0, 1);
    let go = rig.start(0, 2);
    assert_eq!(
        ramps(&go),
        vec![ramp(1, 0, LevelParam::Output(0), db_to_gain(-6.0), 1000)]
    );
}

#[test]
fn a_fade_never_ramps_before_its_targets_voice_exists() {
    // The target's voice is committed for sample 50 (lookahead) when the fade acts at 0: its
    // ramp starts at 50 and still ends on the fade's end.
    let mut rig = Rig::with_lookahead(
        vec![
            timed(sound(1), 0.05, 0.0, DNC),
            master_fade(2, 1.0, SILENCE_DB),
        ],
        100,
    );
    rig.start(0, 1);
    let go = rig.start(0, 2);
    assert_eq!(ramps(&go), vec![ramp(1, 50, M, 0.0, 950)]);
}

#[test]
fn a_late_fade_keeps_its_end_sample() {
    let mut rig = Rig::new(vec![
        sound(1),
        timed(master_fade(2, 1.0, SILENCE_DB), 1.0, 0.0, DNC),
    ]);
    rig.start(0, 1);
    rig.start(0, 2);
    // The tick comes 200 samples late: the ramp starts now and still ends on 2000.
    let tick = rig.tick(1200);
    assert_eq!(ramps(&tick), vec![ramp(1, 1200, M, 0.0, 800)]);
    assert_eq!(changes(&rig.tick(2000), 2), vec![(CueState::Idle, 2000)]);
}

#[test]
fn pausing_the_target_keeps_the_fade_running() {
    // The fade belongs to the Fade cue; a paused target reports paused while it moves.
    let mut rig = Rig::new(vec![sound(1), master_fade(2, 2.0, SILENCE_DB)]);
    rig.start(0, 1);
    rig.start(0, 2);
    rig.pause(500, 1);
    assert_eq!(rig.state(1), CueState::Paused);
    rig.resume(1000, 1);
    assert_eq!(rig.state(1), CueState::Fading);
    rig.tick(2000);
    assert_eq!(rig.state(1), CueState::Running);
}

#[test]
fn second_trigger_on_a_fading_cue_is_ignored() {
    let mut rig = Rig::new(vec![sound(1), master_fade(2, 2.0, SILENCE_DB)]);
    rig.start(0, 1);
    rig.start(0, 2);
    assert_eq!(
        warnings(&rig.start(100, 1)),
        vec![EngineWarning::TriggerIgnored {
            cue: id(1),
            state: CueState::Fading
        }]
    );
}

#[test]
fn feedback_never_breaks_a_fade_cue() {
    let mut rig = Rig::new(vec![sound(1), master_fade(2, 2.0, SILENCE_DB)]);
    rig.start(0, 1);
    rig.start(0, 2);
    let run = rig.run(2);
    rig.at(
        100,
        Input::ActionFailed {
            cue: id(2),
            run,
            reason: BrokenReason::VoiceRejected,
        },
    );
    assert_eq!(rig.state(2), CueState::Running);
    assert_eq!(rig.state(1), CueState::Fading);
}

#[test]
fn deleting_a_running_fade_holds_its_ramp() {
    let mut rig = Rig::new(vec![sound(1), master_fade(2, 4.0, SILENCE_DB)]);
    rig.start(0, 1);
    rig.start(0, 2);
    rig.ws.cue_lists[0].cues.remove(1);
    let tick = rig.tick(2000);
    assert_eq!(ramps(&tick), vec![ramp(1, 2000, M, 0.5, 0)]);
    assert_eq!(rig.state(1), CueState::Running);
    assert_eq!(rig.state(2), CueState::Idle);
}

#[test]
fn a_restarted_target_is_not_touched_by_the_old_fade() {
    let mut rig = Rig::new(vec![sound(1), master_fade(2, 4.0, SILENCE_DB)]);
    rig.start(0, 1);
    rig.start(0, 2);
    rig.stop(1000, 1);
    rig.start(1100, 1);
    assert_eq!(rig.state(1), CueState::Running);
    assert!(rig.level(1, M, 1100).is_none());
    assert!(ramps(&rig.pause(1200, 2)).is_empty());
}

#[test]
fn a_fade_that_stops_its_own_group_reports_once() {
    // Regression (proptest): the fade's own end stopped the group around it, which stopped the
    // fade too; it then reported idle a second time.
    let mut rig = Rig::new(vec![cue(
        1,
        CueKind::Group {
            mode: GroupMode::Timeline,
            children: vec![
                sound(2),
                fade_to(3, 1, 1.0, FadeCurve::Linear, master(SILENCE_DB), true),
            ],
        },
    )]);
    rig.start(0, 1);
    let end = rig.tick(1000);
    assert_eq!(changes(&end, 3), vec![(CueState::Idle, 1000)]);
    assert_eq!(voice_stops(&end), vec![(id(2), 1000, None)]);
    assert_eq!(rig.state(1), CueState::Idle);
}

// ----- review round 1 -----

#[test]
fn a_fade_taking_over_never_ramps_before_a_committed_ramp() {
    // Review M1: Fade 2's ramp is committed for 1400 (lookahead); an operator Fade at 1100
    // must not be dated before it, or the mixer would carry out the two the wrong way round.
    let mut f2 = master_fade(2, 1.0, SILENCE_DB);
    f2.pre_wait = 0.4;
    let mut rig = Rig::with_lookahead(vec![sound(1), f2, master_fade(3, 1.0, 0.0)], 500);
    rig.start(0, 1);
    assert_eq!(
        ramps(&rig.start(1000, 2)),
        vec![ramp(1, 1400, M, 0.0, 1000)]
    );
    let take = rig.start(1100, 3);
    // Fade 3 ends on 2100; its ramp starts on 1400 (from Fade 2's start value, 1.0).
    assert_eq!(ramps(&take), vec![ramp(1, 1400, M, 1.0, 700)]);
    assert_eq!(rig.level(1, M, 2100), Some(1.0));
    rig.tick(2600);
    assert_eq!(rig.level(1, M, 2600), Some(1.0));
}

#[test]
fn in_progress_ramps_can_be_read_back_for_a_restart() {
    // Review L4: what the binary needs to restart a voice mid-fade.
    let mut rig = Rig::new(vec![sound(1), master_fade(2, 4.0, SILENCE_DB)]);
    rig.start(0, 1);
    rig.start(0, 2);
    assert_eq!(
        rig.engine.fade_ramps(id(1), 1000),
        vec![LevelRamp {
            param: M,
            value: 0.75,
            to: 0.0,
            end: 4000,
            curve: FadeCurve::Linear,
        }]
    );
    rig.pause(1000, 2);
    assert_eq!(
        rig.engine.fade_ramps(id(1), 2000),
        vec![LevelRamp {
            param: M,
            value: 0.75,
            to: 0.75,
            end: 1000,
            curve: FadeCurve::Linear,
        }]
    );
    assert!(rig.engine.fade_ramps(id(2), 2000).is_empty());
}
