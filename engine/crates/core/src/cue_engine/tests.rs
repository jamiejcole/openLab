// SPDX-License-Identifier: GPL-3.0-or-later

//! Paths only reachable with out-of-date static problems (a model change the binary did not
//! signal, in a release build): debug builds correct such a change at the start of `handle`, so
//! these tests carry out the deadlines directly on the edited workspace.

use uuid::Uuid;

use super::*;
use crate::cue::{CueColor, FadeCurve, FadeLevels, Triggers};
use crate::model::{CueList, WorkspaceId, WorkspaceSettings};

fn id(n: u128) -> CueId {
    CueId(Uuid::from_u128(n))
}

fn cue(n: u128, kind: CueKind, pre_wait: f64) -> Cue {
    Cue {
        id: id(n),
        number: None,
        name: String::new(),
        notes: String::new(),
        color: CueColor::None,
        flagged: false,
        armed: true,
        pre_wait,
        post_wait: 0.0,
        continue_mode: ContinueMode::DoNotContinue,
        triggers: Triggers {
            hotkey: None,
            osc: None,
        },
        kind,
    }
}

fn workspace(cues: Vec<Cue>) -> Workspace {
    Workspace {
        id: WorkspaceId(Uuid::from_u128(1)),
        name: String::new(),
        settings: WorkspaceSettings {
            min_go_interval: 0.0,
            panic_duration: 1.0,
            autosave_interval: 30.0,
            osc_passcode: None,
            master_db: 0.0,
            hotkeys: crate::model::TransportHotkeys::default(),
            auto_number_start: 1.0,
            auto_number_increment: 1.0,
        },
        patches: vec![],
        network_patches: vec![],
        cue_lists: vec![CueList {
            id: CueListId(Uuid::from_u128(1)),
            name: String::new(),
            cues,
        }],
    }
}

/// Starts cue 1 (pre-wait 1 s) on `ws`, then carries out its action on `stale` without the start
/// of a call (so the static problems still describe `ws`). Returns the warnings.
fn act_on_stale(ws: &Workspace, stale: &Workspace) -> Vec<EngineWarning> {
    let mut engine = CueEngine::new(EngineConfig::new(1000, 0, 1).unwrap());
    engine.handle(ws, Input::Start { cue: id(1) }, 0);
    let limit = CueEngine::chain_limit(stale);
    let mut cx = Ctx {
        ws: stale,
        now: 1000,
        horizon: 1000,
        budget: limit,
        limit,
        out: Vec::new(),
        vacated: Vec::new(),
    };
    engine.advance(&mut cx);
    cx.out
        .into_iter()
        .filter_map(|a| match a {
            Action::Warning(w) => Some(w),
            _ => None,
        })
        .collect()
}

#[test]
fn a_control_cue_whose_target_went_away_unsignalled_only_warns() {
    let ws = workspace(vec![
        cue(
            1,
            CueKind::Start {
                target: Some(id(2)),
            },
            1.0,
        ),
        cue(2, CueKind::Memo, 0.0),
    ]);
    let mut gone = ws.clone();
    gone.cue_lists[0].cues.truncate(1);
    assert_eq!(
        act_on_stale(&ws, &gone),
        vec![EngineWarning::TargetMissing {
            cue: id(1),
            target: id(2)
        }]
    );
    let mut unset = ws.clone();
    unset.cue_lists[0].cues[0].kind.set_target(None);
    assert_eq!(
        act_on_stale(&ws, &unset),
        vec![EngineWarning::NoTarget { cue: id(1) }]
    );
}

#[test]
fn a_fade_cue_whose_target_changed_unsignalled_only_warns() {
    let fade = CueKind::Fade {
        target: Some(id(2)),
        duration: 1.0,
        curve: FadeCurve::Linear,
        levels: FadeLevels {
            master_db: Some(0.0),
            output_db: vec![],
            crosspoints: vec![],
        },
        stop_target_when_done: false,
    };
    let ws = workspace(vec![
        cue(1, fade, 1.0),
        cue(
            2,
            CueKind::Group {
                mode: crate::cue::GroupMode::Timeline,
                children: vec![],
            },
            0.0,
        ),
    ]);
    let mut memo = ws.clone();
    memo.cue_lists[0].cues[1].kind = CueKind::Memo;
    assert_eq!(
        act_on_stale(&ws, &memo),
        vec![EngineWarning::TargetWrongKind {
            cue: id(1),
            target: id(2)
        }]
    );
    let mut gone = ws.clone();
    gone.cue_lists[0].cues.truncate(1);
    assert_eq!(
        act_on_stale(&ws, &gone),
        vec![EngineWarning::TargetMissing {
            cue: id(1),
            target: id(2)
        }]
    );
}

/// Carries out `input` on `ws` without the start of a call, as `handle` does after a model
/// change the binary did not signal in a release build (debug builds would correct it at the
/// start of `handle`).
fn input_unsignalled(
    engine: &mut CueEngine,
    ws: &Workspace,
    input: Input,
    now: SampleTime,
) -> Vec<Action> {
    let limit = CueEngine::chain_limit(ws);
    let mut cx = Ctx {
        ws,
        now,
        horizon: now,
        budget: limit,
        limit,
        out: Vec::new(),
        vacated: Vec::new(),
    };
    engine.apply(input, &mut cx);
    engine.drain(&mut cx);
    engine.advance(&mut cx);
    cx.out
}

#[test]
fn a_cue_fixed_by_an_unsignalled_edit_is_not_refused() {
    // Regression (re-review M1'): the binary missed `model_changed` after retargeting a broken
    // Start cue; GO then refused it for its old problem, with nothing showing why.
    let ws = workspace(vec![
        cue(
            1,
            CueKind::Start {
                target: Some(id(99)),
            },
            0.0,
        ),
        cue(2, CueKind::Memo, 0.0),
    ]);
    let mut engine = CueEngine::new(EngineConfig::new(1000, 0, 1).unwrap());
    engine.handle(&ws, Input::Tick, 0);
    assert_eq!(engine.state(id(1)), CueState::Broken);
    let mut fixed = ws.clone();
    fixed.cue_lists[0].cues[0].kind.set_target(Some(id(2)));
    let go = input_unsignalled(
        &mut engine,
        &fixed,
        Input::Go {
            list: CueListId(Uuid::from_u128(1)),
        },
        10,
    );
    assert!(go.contains(&Action::ProblemsChanged {
        cue: id(1),
        problems: vec![]
    }));
    assert!(
        !go.iter()
            .any(|a| matches!(a, Action::Warning(EngineWarning::CueBroken { .. })))
    );
    // It ran: its action started its target.
    assert!(go.contains(&Action::StateChanged {
        cue: id(2),
        state: CueState::PreWaiting,
        at: 10
    }));
    assert_eq!(engine.unsignalled_model_changes(), 1);
    assert!(engine.problems(id(1)).is_empty());
}

#[test]
fn a_cue_still_broken_after_an_unsignalled_edit_is_still_refused() {
    let ws = workspace(vec![
        cue(
            1,
            CueKind::Start {
                target: Some(id(99)),
            },
            0.0,
        ),
        cue(2, CueKind::Memo, 0.0),
    ]);
    let mut engine = CueEngine::new(EngineConfig::new(1000, 0, 1).unwrap());
    engine.handle(&ws, Input::Tick, 0);
    let mut renamed = ws.clone();
    renamed.cue_lists[0].cues[1].name = "Renamed".into();
    let start = input_unsignalled(&mut engine, &renamed, Input::Start { cue: id(1) }, 10);
    assert!(start.contains(&Action::Warning(EngineWarning::CueBroken { cue: id(1) })));
    assert_eq!(engine.unsignalled_model_changes(), 0);
    // Retargeted to another missing cue: still refused, with the new reason.
    let mut other = ws.clone();
    other.cue_lists[0].cues[0].kind.set_target(Some(id(98)));
    let load = input_unsignalled(
        &mut engine,
        &other,
        Input::Load {
            cue: id(1),
            time: 0.0,
        },
        20,
    );
    assert!(load.contains(&Action::Warning(EngineWarning::CueBroken { cue: id(1) })));
    assert_eq!(
        engine.problems(id(1)),
        &[CueProblem::TargetMissing { target: id(98) }]
    );
    assert_eq!(engine.unsignalled_model_changes(), 1);
}
