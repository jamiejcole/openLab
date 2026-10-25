// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Property tests for the cue engine: random lists with nested groups of every mode, control and
//! Fade cues with random targets (so Start/GoTo cycles, self-targets and missing targets), and
//! duplicate ids (more trigger cycles), and random input sequences, with edits (deletes, moves
//! out of groups, reorders, mode changes), Arm/Disarm changes applied to the model as the binary
//! does, and stale feedback (including devamp answers) mixed in. `handle` never
//! panics, every cue has exactly the state last reported for it, playheads stay valid, a running
//! group always has a child that is not idle, the chain bound holds and every due deadline is
//! carried out. The full suite is M2.9.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::*;
use openlab_core::*;
use proptest::prelude::*;

#[derive(Clone, Debug)]
struct Spec {
    kind: u8,
    pre: u8,
    post: u8,
    mode: u8,
    armed: bool,
    /// Reuse the previous cue's id.
    dup: bool,
    /// Nesting level wanted (clamped to the groups open at this point).
    level: u8,
    /// Group mode, for groups.
    group_mode: u8,
    /// Control and Fade cues: which cue they target (by position; past the end = missing).
    target: u8,
    flag: bool,
}

#[derive(Clone, Debug)]
struct Step {
    dt: u64,
    op: u8,
    a: u8,
    b: u8,
}

fn spec() -> impl Strategy<Value = Spec> {
    (
        (0u8..15, 0u8..3, 0u8..3, 0u8..3, any::<bool>()),
        (
            prop::bool::weighted(0.1),
            0u8..4,
            0u8..7,
            any::<u8>(),
            any::<bool>(),
        ),
    )
        .prop_map(
            |((kind, pre, post, mode, armed), (dup, level, group_mode, target, flag))| Spec {
                kind,
                pre,
                post,
                mode,
                armed,
                dup,
                level,
                group_mode,
                target,
                flag,
            },
        )
}

fn step() -> impl Strategy<Value = Step> {
    (0u64..1500, 0u8..20, any::<u8>(), any::<u8>()).prop_map(|(dt, op, a, b)| Step { dt, op, a, b })
}

fn mode(n: u8) -> ContinueMode {
    match n % 3 {
        0 => ContinueMode::DoNotContinue,
        1 => ContinueMode::AutoContinue,
        _ => ContinueMode::AutoFollow,
    }
}

fn group_mode(n: u8) -> GroupMode {
    match n % 7 {
        0 => GroupMode::Timeline,
        1 => GroupMode::StartFirstAndEnter,
        2 => GroupMode::StartFirstAndGoToNext,
        3 => GroupMode::StartRandom,
        4 => GroupMode::Playlist {
            loop_: false,
            shuffle: false,
        },
        5 => GroupMode::Playlist {
            loop_: true,
            shuffle: false,
        },
        _ => GroupMode::Playlist {
            loop_: true,
            shuffle: true,
        },
    }
}

fn attach(stack: &mut [Cue], top: &mut Vec<Cue>, c: Cue) {
    match stack.last_mut().map(|g| &mut g.kind) {
        Some(CueKind::Group { children, .. }) => children.push(c),
        _ => top.push(c),
    }
}

fn build(specs: &[Spec]) -> Vec<Cue> {
    let mut top: Vec<Cue> = Vec::new();
    // Groups still open, outermost first.
    let mut open: Vec<Cue> = Vec::new();
    let mut last_id: Option<u128> = None;
    for (i, s) in specs.iter().enumerate() {
        let n = match last_id {
            Some(last) if s.dup => last,
            _ => i as u128 + 1,
        };
        last_id = Some(n);
        // Targets name cues by position, so cycles and self-targets come up; past the end is a
        // missing target.
        let target = Some(cue_id(u128::from(s.target) % (specs.len() as u128 + 2) + 1));
        let kind = match s.kind {
            0 => CueKind::Memo,
            1 => CueKind::Wait {
                duration: f64::from(s.post) * 0.7,
            },
            2 | 3 => audio(),
            4 => CueKind::Group {
                mode: group_mode(s.group_mode),
                children: vec![],
            },
            5 => CueKind::Start { target },
            6 => CueKind::Stop {
                target,
                fade_out: FadeSpec {
                    duration: f64::from(s.pre) * 0.3,
                    curve: FadeCurve::Linear,
                },
            },
            7 => CueKind::Pause { target },
            8 => CueKind::Load {
                target,
                load_to_time: f64::from(s.post) * 0.4,
            },
            9 => CueKind::Reset { target },
            10 => CueKind::Devamp {
                target,
                start_next_cue: s.flag,
                stop_target_when_slice_ends: s.armed,
            },
            11 => CueKind::GoTo {
                target,
                then_go: s.flag,
            },
            12 if s.flag => CueKind::Arm { target },
            12 => CueKind::Disarm { target },
            13 => CueKind::Fade {
                target,
                duration: f64::from(s.post) * 0.6,
                curve: FadeCurve::SCurve,
                levels: FadeLevels {
                    master_db: Some(f64::from(s.group_mode) * -10.0),
                    output_db: vec![Some(-3.0), None, Some(-6.0)],
                    crosspoints: vec![FadeCrosspoint {
                        input: u32::from(s.level),
                        output: u32::from(s.pre),
                        db: -12.0,
                    }],
                },
                stop_target_when_done: s.flag,
            },
            _ => CueKind::Group {
                mode: group_mode(s.group_mode),
                children: vec![],
            },
        };
        let mut c = cue(n, kind);
        c.number = Some(format!("{i}"));
        c.pre_wait = f64::from(s.pre) * 0.5;
        c.post_wait = f64::from(s.post) * 0.5;
        c.continue_mode = mode(s.mode);
        c.armed = s.armed;
        let level = usize::from(s.level).min(open.len());
        while open.len() > level {
            let g = open.pop().unwrap();
            attach(&mut open, &mut top, g);
        }
        if matches!(c.kind, CueKind::Group { .. }) {
            open.push(c);
        } else {
            attach(&mut open, &mut top, c);
        }
    }
    while let Some(g) = open.pop() {
        attach(&mut open, &mut top, g);
    }
    top
}

/// Every cue, depth-first.
fn flatten(cues: &[Cue], out: &mut Vec<CueId>) {
    for c in cues {
        out.push(c.id);
        flatten(c.children(), out);
    }
}

fn all_ids(ws: &Workspace) -> Vec<CueId> {
    let mut out = Vec::new();
    flatten(&ws.cue_lists[0].cues, &mut out);
    out
}

/// Removes the first cue with this id, anywhere.
fn take(cues: &mut Vec<Cue>, id: CueId) -> Option<Cue> {
    if let Some(i) = cues.iter().position(|c| c.id == id) {
        return Some(cues.remove(i));
    }
    cues.iter_mut().find_map(|c| match &mut c.kind {
        CueKind::Group { children, .. } => take(children, id),
        _ => None,
    })
}

fn find_mut(cues: &mut [Cue], id: CueId) -> Option<&mut Cue> {
    for c in cues {
        if c.id == id {
            return Some(c);
        }
        if let CueKind::Group { children, .. } = &mut c.kind
            && let Some(found) = find_mut(children, id)
        {
            return Some(found);
        }
    }
    None
}

fn pick(ws: &Workspace, a: u8) -> CueId {
    let ids = all_ids(ws);
    if ids.is_empty() || a == u8::MAX {
        cue_id(999)
    } else {
        ids[usize::from(a) % ids.len()]
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn engine_invariants(
        specs in prop::collection::vec(spec(), 0..12),
        steps in prop::collection::vec(step(), 1..40),
        lookahead in 0u64..32,
        min_go in 0u8..3,
    ) {
        let mut ws = workspace(build(&specs));
        ws.settings.min_go_interval = f64::from(min_go) * 0.3;
        let mut engine = CueEngine::new(EngineConfig::new(1000, lookahead, 7).unwrap());
        let list = list_id(1);
        let mut now: SampleTime = 0;
        // The state of every cue as the reported changes tell it.
        let mut reported: BTreeMap<CueId, CueState> = BTreeMap::new();
        let mut seen: BTreeSet<CueId> = all_ids(&ws).into_iter().collect();

        for s in &steps {
            now += s.dt;
            let target = pick(&ws, s.a);
            let input = match s.op {
                0 => Input::Tick,
                1 => Input::Go { list },
                2 => Input::Start { cue: target },
                3 => Input::Stop {
                    cue: target,
                    fade_out: (s.b % 2 == 1).then_some(FadeSpec {
                        duration: f64::from(s.b) / 100.0,
                        curve: FadeCurve::Linear,
                    }),
                },
                4 => Input::Pause { cue: target },
                5 => Input::Resume { cue: target },
                6 => Input::Load { cue: target, time: f64::from(s.b) / 50.0 },
                7 => Input::Reset { cue: target },
                8 => Input::PlayheadNext { list },
                9 => Input::PlayheadPrevious { list },
                10 => Input::SetPlayhead {
                    list,
                    cue: (s.b % 4 != 0).then_some(target),
                },
                11 => {
                    // Model edit: delete a cue anywhere.
                    take(&mut ws.cue_lists[0].cues, target);
                    Input::Tick
                }
                15 => {
                    // Model edit: move a cue to the end of the top level.
                    if let Some(c) = take(&mut ws.cue_lists[0].cues, target) {
                        ws.cue_lists[0].cues.push(c);
                    }
                    Input::Tick
                }
                16 => {
                    // Model edit: reverse a group's children, or change its mode.
                    if let Some(CueKind::Group { mode, children }) =
                        find_mut(&mut ws.cue_lists[0].cues, target).map(|c| &mut c.kind)
                    {
                        if s.b % 2 == 0 {
                            children.reverse();
                        } else {
                            *mode = group_mode(s.b / 2);
                        }
                    }
                    Input::Tick
                }
                17 => Input::SetPlayheadByNumber {
                    list,
                    number: format!("{}", s.b % 12),
                },
                12 => Input::ActionEnded {
                    cue: target,
                    run: if s.b % 3 == 0 {
                        RunId(u64::from(s.b))
                    } else {
                        engine.run_of(target).unwrap_or(RunId(0))
                    },
                    at: now.saturating_sub(u64::from(s.b)),
                },
                13 => Input::ActionFailed {
                    cue: target,
                    run: engine.run_of(target).unwrap_or(RunId(0)),
                    reason: BrokenReason::StreamFailed,
                },
                18 => Input::DevampScheduled {
                    cue: target,
                    run: engine.run_of(target).unwrap_or(RunId(0)),
                    // Sometimes in the past (a late answer), sometimes ahead.
                    at: (now + u64::from(s.b) * 4).saturating_sub(300),
                },
                19 => Input::DevampIgnored {
                    cue: target,
                    run: engine.run_of(target).unwrap_or(RunId(0)),
                },
                _ => {
                    // Model edit: change a cue's timing and continue mode.
                    if let Some(c) = find_mut(&mut ws.cue_lists[0].cues, target) {
                        c.post_wait = f64::from(s.b % 4) * 0.25;
                        c.continue_mode = mode(s.b);
                    }
                    Input::Tick
                }
            };
            let actions = engine.handle(&ws, input, now);
            // Deadlines in `now .. now + lookahead` are committed (end excluded).
            let horizon = now + lookahead.saturating_sub(1);

            let mut starts = 0usize;
            let mut arm = Vec::new();
            for action in &actions {
                match action {
                    Action::StateChanged { cue, state, at } => {
                        let previous = reported.get(cue).copied().unwrap_or(CueState::Idle);
                        prop_assert_ne!(previous, *state, "repeated report");
                        if *state == CueState::PreWaiting && previous != CueState::Paused {
                            starts += 1;
                        }
                        prop_assert!(*at <= horizon);
                        reported.insert(*cue, *state);
                        seen.insert(*cue);
                    }
                    Action::StartVoice { at, .. }
                    | Action::StopVoice { at, .. }
                    | Action::PauseVoice { at, .. }
                    | Action::ResumeVoice { at, .. }
                    | Action::Ramp { at, .. }
                    | Action::DevampVoice { at, .. } => {
                        prop_assert!(*at >= now && *at <= horizon, "voice action at {at}");
                    }
                    Action::SetArmed { cue, armed } => arm.push((*cue, *armed)),
                    _ => {}
                }
            }
            prop_assert!(starts <= CueEngine::chain_limit(&ws));
            // Only audio cues have levels to fade.
            for (id, state) in engine.cue_states() {
                if state == CueState::Fading {
                    let audio = matches!(
                        ws.find_cue(id).map(|c| &c.kind),
                        Some(CueKind::Audio { .. })
                    );
                    prop_assert!(audio, "cue {} fading", id);
                }
            }
            // The binary applies Arm/Disarm changes to the model before the next call (M4.2).
            for (cue, armed) in arm {
                if let Some(c) = find_mut(&mut ws.cue_lists[0].cues, cue) {
                    c.armed = armed;
                }
            }

            // Exactly one state per cue, and it is the one last reported.
            for id in &seen {
                let reported = reported.get(id).copied().unwrap_or(CueState::Idle);
                prop_assert_eq!(engine.state(*id), reported, "cue {}", id);
            }
            // Cues no longer in the workspace are idle.
            for (id, _) in engine.cue_states() {
                prop_assert!(ws.find_cue(id).is_some());
            }
            // The playhead is past the end or on a position of its list (a top-level cue or one
            // inside "start first and enter" groups).
            if let Some(p) = engine.playhead(list) {
                prop_assert!(CueEngine::playhead_positions(&ws, list).contains(&p));
                prop_assert_eq!(engine.standing_by(&ws, list).map(|c| c.id), Some(p));
            } else {
                prop_assert!(engine.standing_by(&ws, list).is_none());
            }
            // No stranded group: a running group has a child that is not idle or loaded. Only
            // meaningful when ids are unique (a duplicate shares its runtime).
            let ids = all_ids(&ws);
            let unique: BTreeSet<CueId> = ids.iter().copied().collect();
            if unique.len() == ids.len() {
                for id in &ids {
                    let Some(c) = ws.find_cue(*id) else { continue };
                    if matches!(c.kind, CueKind::Group { .. })
                        && engine.state(*id) == CueState::Running
                    {
                        prop_assert!(
                            c.children().iter().any(|k| !matches!(
                                engine.state(k.id),
                                CueState::Idle | CueState::Loaded
                            )),
                            "group {} runs with no busy child", id
                        );
                    }
                }
            }
            // Everything due has been done.
            prop_assert!(engine.next_deadline().is_none_or(|d| d > horizon));
            prop_assert!(!engine.is_due(now));
        }
    }
}
