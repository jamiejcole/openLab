// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Property tests for the cue engine: random lists (including duplicate ids, which make trigger
//! cycles) and random input sequences, with edits and stale feedback mixed in. `handle` never
//! panics, every cue has exactly the state last reported for it, playheads stay valid, the chain
//! bound holds and every due deadline is carried out. The full suite is M2.9.

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
        0u8..4,
        0u8..3,
        0u8..3,
        0u8..3,
        any::<bool>(),
        prop::bool::weighted(0.1),
    )
        .prop_map(|(kind, pre, post, mode, armed, dup)| Spec {
            kind,
            pre,
            post,
            mode,
            armed,
            dup,
        })
}

fn step() -> impl Strategy<Value = Step> {
    (0u64..1500, 0u8..15, any::<u8>(), any::<u8>()).prop_map(|(dt, op, a, b)| Step { dt, op, a, b })
}

fn mode(n: u8) -> ContinueMode {
    match n % 3 {
        0 => ContinueMode::DoNotContinue,
        1 => ContinueMode::AutoContinue,
        _ => ContinueMode::AutoFollow,
    }
}

fn build(specs: &[Spec]) -> Vec<Cue> {
    let mut cues: Vec<Cue> = Vec::new();
    for (i, s) in specs.iter().enumerate() {
        let n = match cues.last() {
            Some(last) if s.dup => last.id.0.as_u128(),
            _ => i as u128 + 1,
        };
        let kind = match s.kind {
            0 => CueKind::Memo,
            1 => CueKind::Wait {
                duration: f64::from(s.post) * 0.7,
            },
            2 => audio(),
            _ => group(vec![]),
        };
        let mut c = cue(n, kind);
        c.number = Some(format!("{i}"));
        c.pre_wait = f64::from(s.pre) * 0.5;
        c.post_wait = f64::from(s.post) * 0.5;
        c.continue_mode = mode(s.mode);
        c.armed = s.armed;
        cues.push(c);
    }
    cues
}

fn pick(ws: &Workspace, a: u8) -> CueId {
    let cues = &ws.cue_lists[0].cues;
    if cues.is_empty() || a == u8::MAX {
        cue_id(999)
    } else {
        cues[usize::from(a) % cues.len()].id
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn engine_invariants(
        specs in prop::collection::vec(spec(), 0..8),
        steps in prop::collection::vec(step(), 1..40),
        lookahead in 0u64..32,
        min_go in 0u8..3,
    ) {
        let mut ws = workspace(build(&specs));
        ws.settings.min_go_interval = f64::from(min_go) * 0.3;
        let mut engine = CueEngine::new(EngineConfig::new(1000, lookahead).unwrap());
        let list = list_id(1);
        let mut now: SampleTime = 0;
        // The state of every cue as the reported changes tell it.
        let mut reported: BTreeMap<CueId, CueState> = BTreeMap::new();
        let mut seen: BTreeSet<CueId> = ws.cue_lists[0].cues.iter().map(|c| c.id).collect();

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
                    // Model edit: delete a top-level cue.
                    let cues = &mut ws.cue_lists[0].cues;
                    if !cues.is_empty() {
                        let i = usize::from(s.a) % cues.len();
                        cues.remove(i);
                    }
                    Input::Tick
                }
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
                _ => {
                    // Model edit: change a cue's timing and continue mode.
                    if let Some(c) = ws.cue_lists[0].cues.iter_mut().find(|c| c.id == target) {
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
                    | Action::ResumeVoice { at, .. } => {
                        prop_assert!(*at >= now && *at <= horizon, "voice action at {at}");
                    }
                    _ => {}
                }
            }
            prop_assert!(starts <= CueEngine::chain_limit(&ws));

            // Exactly one state per cue, and it is the one last reported.
            for id in &seen {
                let reported = reported.get(id).copied().unwrap_or(CueState::Idle);
                prop_assert_eq!(engine.state(*id), reported, "cue {}", id);
            }
            // Cues no longer in the workspace are idle.
            for (id, _) in engine.cue_states() {
                prop_assert!(ws.find_cue(id).is_some());
            }
            // The playhead is past the end or on a top-level cue of its list.
            if let Some(p) = engine.playhead(list) {
                prop_assert!(ws.cue_lists[0].cues.iter().any(|c| c.id == p));
            } else {
                prop_assert!(engine.standing_by(&ws, list).is_none());
            }
            // Everything due has been done.
            prop_assert!(engine.next_deadline().is_none_or(|d| d > horizon));
            prop_assert!(!engine.is_due(now));
        }
    }
}
