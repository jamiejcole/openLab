// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Level edits on a playing audio cue (M5.9b, AUD-06, AUD-08), per docs/cue-engine.md "Level
//! edits on a playing cue". Virtual sample clock at 1 kHz: the edit ramp is 10 samples.

mod common;

use common::*;
use openlab_core::*;

const RATE: u32 = 1000;
const EDIT: SampleTime = 10;
const M: LevelParam = LevelParam::Master;
const O0: LevelParam = LevelParam::Output(0);

fn id(n: u128) -> CueId {
    cue_id(n)
}

fn sound(n: u128) -> Cue {
    cue(n, audio())
}

fn fade_master(n: u128, target: u128, seconds: f64, db: f64) -> Cue {
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
            stop_target_when_done: false,
        },
    )
}

struct Rig {
    ws: Workspace,
    engine: CueEngine,
    last: Option<Workspace>,
}

impl Rig {
    fn new(cues: Vec<Cue>) -> Self {
        let mut rig = Self {
            ws: workspace(cues),
            engine: CueEngine::new(EngineConfig::new(RATE, 0, 1).unwrap()),
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

    fn start(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(now, Input::Start { cue: id(n) })
    }

    /// Edits cue `n`'s levels (as `SetCueProperty Kind` does) and runs a call at `now`.
    fn edit(&mut self, now: SampleTime, n: u128, f: impl FnOnce(&mut CueLevels)) -> Vec<Action> {
        let cue = find_mut(&mut self.ws.cue_lists[0].cues, id(n)).unwrap();
        let CueKind::Audio { levels, .. } = &mut cue.kind else {
            panic!("not audio");
        };
        f(levels);
        self.tick(now)
    }
}

fn find_mut(cues: &mut [Cue], cue: CueId) -> Option<&mut Cue> {
    for c in cues {
        if c.id == cue {
            return Some(c);
        }
        if let CueKind::Group { children, .. } = &mut c.kind
            && let Some(found) = find_mut(children, cue)
        {
            return Some(found);
        }
    }
    None
}

#[derive(Debug, PartialEq)]
struct R {
    cue: CueId,
    at: SampleTime,
    param: LevelParam,
    gain: f32,
    frames: SampleTime,
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
            } => {
                assert_eq!(*curve, FadeCurve::Linear);
                Some(R {
                    cue: *cue,
                    at: *at,
                    param: *param,
                    gain: *gain,
                    frames: *frames,
                })
            }
            _ => None,
        })
        .collect()
}

fn edit(at: SampleTime, param: LevelParam, db: f64) -> R {
    R {
        cue: id(1),
        at,
        param,
        gain: db_to_gain(db),
        frames: EDIT,
    }
}

#[test]
fn master_output_crosspoint_and_mute_edits_ramp_the_playing_voice() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.start(0, 1);
    let run = rig.engine.run_of(id(1)).unwrap();
    let out = rig.edit(100, 1, |l| l.master_db = -6.0);
    assert_eq!(ramps(&out), vec![edit(100, M, -6.0)]);
    assert!(
        out.iter()
            .all(|a| !matches!(a, Action::Ramp { run: r, .. } if *r != run))
    );
    assert_eq!(
        ramps(&rig.edit(200, 1, |l| l.outputs[1].db = -3.0)),
        vec![edit(200, LevelParam::Output(1), -3.0)]
    );
    // An unlisted crosspoint is silent: listing it brings it up; delisting takes it down.
    let xp = LevelParam::Crosspoint {
        input: 1,
        output: 0,
    };
    let add = |l: &mut CueLevels| {
        l.crosspoints.push(Crosspoint {
            input: 1,
            output: 0,
            db: -10.0,
            mute: false,
        });
    };
    assert_eq!(ramps(&rig.edit(300, 1, add)), vec![edit(300, xp, -10.0)]);
    assert_eq!(
        ramps(&rig.edit(400, 1, |l| l.crosspoints[0].mute = true)),
        vec![edit(400, xp, SILENCE_DB)]
    );
    assert_eq!(
        ramps(&rig.edit(500, 1, |l| l.outputs[0].mute = true)),
        vec![edit(500, O0, SILENCE_DB)]
    );
    // Unmuting restores the kept level.
    assert_eq!(
        ramps(&rig.edit(600, 1, |l| l.outputs[0].mute = false)),
        vec![edit(600, O0, 0.0)]
    );
    assert_eq!(
        ramps(&rig.edit(700, 1, |l| l.crosspoints.clear())),
        vec![edit(700, xp, SILENCE_DB)]
    );
    // Ganging changes no gain; nothing else changed either.
    assert!(ramps(&rig.edit(800, 1, |l| l.gangs = vec![vec![0, 1]])).is_empty());
    assert!(ramps(&rig.tick(900)).is_empty());
    // An edit is not a fade: the cue stays running.
    assert_eq!(rig.engine.state(id(1)), CueState::Running);
}

#[test]
fn an_edit_to_an_idle_or_pre_waiting_cue_sends_nothing() {
    let mut waiting = sound(2);
    waiting.pre_wait = 10.0;
    let mut rig = Rig::new(vec![sound(1), waiting]);
    assert!(ramps(&rig.edit(10, 1, |l| l.master_db = -6.0)).is_empty());
    rig.start(20, 2);
    assert!(ramps(&rig.edit(30, 2, |l| l.master_db = -6.0)).is_empty());
    // It starts with the model's levels; the edit made before is not sent again.
    rig.start(40, 1);
    assert!(ramps(&rig.tick(50)).is_empty());
}

#[test]
fn an_edit_on_the_voices_start_sample_is_immediate() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.start(100, 1);
    let out = rig.edit(100, 1, |l| l.master_db = -6.0);
    assert_eq!(
        ramps(&out),
        vec![R {
            frames: 0,
            ..edit(100, M, -6.0)
        }]
    );
}

#[test]
fn a_drag_sends_one_ramp_per_call_however_many_edits_it_merged() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.start(0, 1);
    // 100 edits over one second, one call each: one ramp each, retargeted by the next.
    for k in 1..=100u32 {
        let db = -f64::from(k) * 0.2;
        let out = rig.edit(u64::from(k) * 10, 1, |l| l.master_db = db);
        assert_eq!(ramps(&out), vec![edit(u64::from(k) * 10, M, db)]);
    }
    // Several edits between two calls are one ramp, to the last.
    for db in [-1.0, -2.0, -3.0] {
        let cue = find_mut(&mut rig.ws.cue_lists[0].cues, id(1)).unwrap();
        if let CueKind::Audio { levels, .. } = &mut cue.kind {
            levels.master_db = db;
        }
    }
    assert_eq!(ramps(&rig.tick(2000)), vec![edit(2000, M, -3.0)]);
}

#[test]
fn a_running_fade_keeps_its_levels_but_a_mute_takes_one_from_it() {
    let mut rig = Rig::new(vec![sound(1), fade_master(2, 1, 1.0, -20.0)]);
    rig.start(0, 1);
    rig.start(100, 2);
    // The fade owns the master: the edit is in the model, not heard while the fade runs.
    assert!(ramps(&rig.edit(200, 1, |l| l.master_db = -6.0)).is_empty());
    // An output no fade touches follows the edit.
    assert_eq!(
        ramps(&rig.edit(300, 1, |l| l.outputs[1].db = -3.0)),
        vec![edit(300, LevelParam::Output(1), -3.0)]
    );
    assert_eq!(rig.engine.state(id(1)), CueState::Fading);
    // The fade ends on its level.
    rig.tick(1100);
    assert_eq!(
        rig.engine.fade_level(id(1), M, 1100),
        Some(db_to_gain(-20.0))
    );
    // A later edit of the master takes it back from the finished fade: the model rules it.
    assert_eq!(
        ramps(&rig.edit(1200, 1, |l| l.master_db = -8.0)),
        vec![edit(1200, M, -8.0)]
    );
    assert_eq!(rig.engine.fade_level(id(1), M, 1200), None);

    // A mute is always heard, also on an output a fade is moving.
    let fade_out = cue(
        3,
        CueKind::Fade {
            target: Some(id(1)),
            duration: 1.0,
            curve: FadeCurve::Linear,
            levels: FadeLevels {
                master_db: None,
                output_db: vec![Some(6.0)],
                crosspoints: vec![],
            },
            stop_target_when_done: false,
        },
    );
    rig.ws.cue_lists[0].cues.push(fade_out);
    rig.start(1300, 3);
    assert!(rig.engine.fade_level(id(1), O0, 1300).is_some());
    assert_eq!(
        ramps(&rig.edit(1400, 1, |l| l.outputs[0].mute = true)),
        vec![edit(1400, O0, SILENCE_DB)]
    );
    assert_eq!(rig.engine.fade_level(id(1), O0, 1400), None);
    // The fade lets go of it: its end changes nothing for that output.
    rig.tick(2300);
    assert_eq!(rig.engine.fade_level(id(1), O0, 2300), None);
}

#[test]
fn a_fade_after_an_edit_starts_from_the_edited_level() {
    let mut rig = Rig::new(vec![sound(1), fade_master(2, 1, 1.0, -20.0)]);
    rig.start(0, 1);
    rig.edit(100, 1, |l| l.master_db = -6.0);
    rig.start(200, 2);
    assert_eq!(rig.engine.fade_level(id(1), M, 200), Some(db_to_gain(-6.0)));
}

#[test]
fn a_relative_group_fade_after_an_edit_moves_the_edited_level() {
    let group = cue(
        1,
        CueKind::Group {
            mode: GroupMode::Timeline,
            children: vec![sound(4)],
        },
    );
    let mut rig = Rig::new(vec![group, fade_master(2, 1, 1.0, -10.0)]);
    rig.start(0, 1);
    let edited = rig.edit(100, 4, |l| l.master_db = -6.0);
    assert_eq!(
        ramps(&edited),
        vec![R {
            cue: id(4),
            ..edit(100, M, -6.0)
        }]
    );
    let go = rig.start(200, 2);
    let r = ramps_any_curve(&go);
    assert_eq!(r.len(), 1);
    // −6 dB moved by −10 dB: −16 dB.
    assert!((r[0] - db_to_gain(-16.0)).abs() < 1e-6, "{}", r[0]);
}

fn ramps_any_curve(actions: &[Action]) -> Vec<f32> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Ramp { gain, .. } => Some(*gain),
            _ => None,
        })
        .collect()
}

#[test]
fn an_edit_of_a_paused_cue_is_sent_and_kept_through_the_resume() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.start(0, 1);
    rig.at(100, Input::Pause { cue: id(1) });
    let paused = rig.engine.run_of(id(1)).unwrap();
    let out = rig.edit(200, 1, |l| l.master_db = -6.0);
    // The paused voice gets it (the mixer advances no gain while paused, so it is heard over
    // the edit ramp once the voice resumes).
    assert!(
        out.iter()
            .any(|a| matches!(a, Action::Ramp { run, param: M, .. } if *run == paused))
    );
    let resumed = rig.at(300, Input::Resume { cue: id(1) });
    // The resume sends no level again; the next edit goes to the resumed run.
    assert!(ramps(&resumed).is_empty());
    let fresh = rig.engine.run_of(id(1)).unwrap();
    assert_ne!(fresh, paused);
    let out = rig.edit(400, 1, |l| l.master_db = -3.0);
    assert!(
        out.iter()
            .any(|a| matches!(a, Action::Ramp { run, param: M, .. } if *run == fresh))
    );
}

#[test]
fn a_restarted_cue_compares_with_the_levels_it_restarted_with() {
    let mut rig = Rig::new(vec![sound(1)]);
    rig.start(0, 1);
    rig.edit(100, 1, |l| l.master_db = -6.0);
    rig.at(
        200,
        Input::Stop {
            cue: id(1),
            fade_out: None,
        },
    );
    rig.start(300, 1);
    assert!(ramps(&rig.tick(400)).is_empty());
    assert_eq!(
        ramps(&rig.edit(500, 1, |l| l.master_db = -6.0)),
        Vec::<R>::new()
    );
    assert_eq!(
        ramps(&rig.edit(600, 1, |l| l.master_db = 0.0)),
        vec![edit(600, M, 0.0)]
    );
}
