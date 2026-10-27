// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Network cues (CT-30) and their problems (NFR-07), per docs/cue-engine.md "Network cues".
//! Virtual sample clock at 1 kHz: one second is 1000 samples.

mod common;

use common::*;
use openlab_core::*;

const RATE: u32 = 1000;

fn id(n: u128) -> CueId {
    cue_id(n)
}

fn network_to(n: u128, patch: u128, address: &str, args: Vec<OscArg>) -> Cue {
    cue(
        n,
        CueKind::Network {
            destination: net_id(patch),
            address: address.into(),
            args,
        },
    )
}

fn network(n: u128) -> Cue {
    network_to(n, 1, "/lx/go", vec![OscArg::Int { value: 7 }])
}

fn memo(n: u128) -> Cue {
    cue(n, CueKind::Memo)
}

struct Rig {
    ws: Workspace,
    engine: CueEngine,
    last: Option<Workspace>,
}

impl Rig {
    fn new(cues: Vec<Cue>) -> Self {
        Self::on(workspace(cues))
    }

    fn on(ws: Workspace) -> Self {
        let mut rig = Self {
            ws,
            engine: CueEngine::new(EngineConfig::new(RATE, 0, 1).unwrap()),
            last: None,
        };
        rig.at(0, Input::Tick);
        rig
    }

    fn at(&mut self, now: SampleTime, input: Input) -> Vec<Action> {
        let out = handle_as_binary(&mut self.engine, &self.ws, &mut self.last, input, now);
        self.engine.verify_problem_cache(&self.ws).unwrap();
        out
    }

    fn go(&mut self, now: SampleTime) -> Vec<Action> {
        self.at(now, Input::Go { list: list_id(1) })
    }

    fn reach(&mut self, now: SampleTime, patch: u128, r: NetworkReachability) -> Vec<Action> {
        self.at(
            now,
            Input::NetworkAvailability {
                patches: vec![(net_id(patch), r)],
            },
        )
    }

    fn problems(&self, n: u128) -> Vec<CueProblem> {
        self.engine.problems(id(n)).to_vec()
    }
}

fn sends(actions: &[Action]) -> Vec<&Action> {
    actions
        .iter()
        .filter(|a| matches!(a, Action::SendOsc { .. }))
        .collect()
}

fn set_patch(rig: &mut Rig, f: impl FnOnce(&mut NetworkPatch)) {
    f(&mut rig.ws.network_patches[0]);
}

fn unresolved(reason: &str) -> NetworkReachability {
    NetworkReachability::Unresolved {
        reason: reason.into(),
    }
}

#[test]
fn go_sends_the_message_once_and_the_cue_completes_at_once() {
    let mut rig = Rig::new(vec![network(1), memo(2)]);
    let actions = rig.go(5);
    assert_eq!(
        sends(&actions),
        vec![&Action::SendOsc {
            cue: id(1),
            at: 5,
            destination: net_id(1),
            address: "/lx/go".into(),
            args: vec![OscArg::Int { value: 7 }],
        }]
    );
    assert_eq!(rig.engine.state(id(1)), CueState::Idle);
    // The playhead moved on: the next GO is the memo, and sends nothing.
    assert!(sends(&rig.go(10)).is_empty());
    assert!(rig.engine.next_deadline().is_none());
}

#[test]
fn a_pre_wait_delays_the_send_to_its_end() {
    let mut c = network(1);
    c.pre_wait = 2.0;
    let mut rig = Rig::new(vec![c]);
    assert!(sends(&rig.go(0)).is_empty());
    assert_eq!(rig.engine.state(id(1)), CueState::PreWaiting);
    assert!(sends(&rig.at(1999, Input::Tick)).is_empty());
    let actions = rig.at(2000, Input::Tick);
    let sent = sends(&actions);
    assert_eq!(sent.len(), 1);
    assert!(matches!(sent[0], Action::SendOsc { at: 2000, .. }));
    assert_eq!(rig.engine.state(id(1)), CueState::Idle);
}

#[test]
fn auto_follow_continues_to_the_next_cue_straight_away() {
    let mut c = network(1);
    c.continue_mode = ContinueMode::AutoFollow;
    let mut rig = Rig::new(vec![c, network_to(2, 1, "/two", vec![])]);
    let actions = rig.go(0);
    let addresses: Vec<&str> = sends(&actions)
        .into_iter()
        .filter_map(|a| match a {
            Action::SendOsc { address, .. } => Some(address.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(addresses, ["/lx/go", "/two"]);
}

#[test]
fn a_start_cue_sends_its_target_s_message() {
    let mut rig = Rig::new(vec![
        network(1),
        cue(
            2,
            CueKind::Start {
                target: Some(id(1)),
            },
        ),
    ]);
    // Go on the first sends; a Start cue then fires it again.
    assert_eq!(sends(&rig.go(0)).len(), 1);
    assert_eq!(sends(&rig.go(10)).len(), 1);
}

#[test]
fn a_disarmed_cue_sends_nothing_but_still_completes() {
    let mut c = network(1);
    c.armed = false;
    let mut rig = Rig::new(vec![c, network_to(2, 1, "/two", vec![])]);
    assert!(sends(&rig.go(0)).is_empty());
    // The playhead passed it.
    assert_eq!(sends(&rig.go(10)).len(), 1);
}

#[test]
fn all_argument_types_are_sent() {
    let args = vec![
        OscArg::Int { value: -3 },
        OscArg::Float { value: 0.25 },
        OscArg::String { value: "hi".into() },
        OscArg::Bool { value: false },
    ];
    let mut rig = Rig::new(vec![network_to(1, 1, "/all", args.clone())]);
    let actions = rig.go(0);
    assert!(matches!(
        sends(&actions)[0],
        Action::SendOsc { args: sent, .. } if *sent == args
    ));
}

#[test]
fn a_missing_patch_breaks_the_cue_and_nothing_is_sent() {
    let mut rig = Rig::new(vec![network_to(1, 9, "/x", vec![])]);
    assert_eq!(
        rig.problems(1),
        vec![CueProblem::NetworkPatchMissing { patch: net_id(9) }]
    );
    assert_eq!(rig.engine.state(id(1)), CueState::Broken);
    let actions = rig.go(0);
    assert!(sends(&actions).is_empty());
    assert!(actions.contains(&Action::Warning(EngineWarning::CueBroken { cue: id(1) })));
}

#[test]
fn an_address_that_cannot_be_used_breaks_the_cue_with_the_reason() {
    for (host, port, reason) in [
        ("", 53000, "the host is empty"),
        (
            "my host",
            53000,
            "the host is not a valid IP address or host name",
        ),
        (
            "300.1.1.1",
            53000,
            "the host is not a valid IP address or host name",
        ),
        ("127.0.0.1", 0, "the port must be 1 to 65535"),
    ] {
        let mut ws = workspace(vec![network(1)]);
        ws.network_patches[0].host = host.into();
        ws.network_patches[0].port = port;
        let mut rig = Rig::on(ws);
        assert_eq!(
            rig.problems(1),
            vec![CueProblem::NetworkHostInvalid {
                patch: net_id(1),
                reason: reason.into()
            }],
            "{host:?}:{port}"
        );
        assert!(sends(&rig.go(0)).is_empty());
    }
}

#[test]
fn a_malformed_message_breaks_the_cue() {
    let long = "x".repeat(MAX_OSC_STRING_BYTES + 1);
    let cases = [
        network_to(1, 1, "", vec![]),
        network_to(1, 1, "lx/go", vec![]),
        network_to(1, 1, "/lx go", vec![]),
        network_to(1, 1, "/x", vec![OscArg::String { value: long }]),
        network_to(1, 1, "/x", vec![OscArg::Float { value: f32::NAN }]),
        network_to(1, 1, "/x", vec![OscArg::Int { value: 0 }; MAX_OSC_ARGS + 1]),
    ];
    for c in cases {
        let mut rig = Rig::new(vec![c.clone()]);
        let problems = rig.problems(1);
        assert!(
            matches!(
                problems.as_slice(),
                [CueProblem::NetworkMessageInvalid { .. }]
            ),
            "{c:?}: {problems:?}"
        );
        assert_eq!(rig.engine.state(id(1)), CueState::Broken);
        assert!(sends(&rig.go(0)).is_empty());
    }
}

#[test]
fn problems_add_up_in_order() {
    let mut ws = workspace(vec![network_to(1, 1, "nope", vec![])]);
    ws.network_patches[0].port = 0;
    let rig = Rig::on(ws);
    let problems = rig.problems(1);
    assert!(matches!(
        problems.as_slice(),
        [
            CueProblem::NetworkHostInvalid { .. },
            CueProblem::NetworkMessageInvalid { .. }
        ]
    ));
}

#[test]
fn an_unresolvable_host_name_breaks_the_cues_that_use_it_until_it_resolves() {
    let mut ws = workspace(vec![network(1), network(2), memo(3)]);
    ws.network_patches[0].host = "lights.local".into();
    let mut rig = Rig::on(ws);
    // Taken to be fine until the binary says otherwise.
    assert!(rig.problems(1).is_empty());

    let actions = rig.reach(10, 1, unresolved("no such host"));
    let want = vec![CueProblem::NetworkHostUnresolved {
        patch: net_id(1),
        reason: "no such host".into(),
    }];
    assert_eq!(rig.problems(1), want);
    assert_eq!(rig.problems(2), want);
    assert!(actions.contains(&Action::ProblemsChanged {
        cue: id(1),
        problems: want.clone()
    }));
    assert_eq!(rig.engine.state(id(2)), CueState::Broken);
    assert!(rig.engine.broken_cues().iter().any(|(c, _)| *c == id(1)));

    // GO on a broken cue sends nothing.
    assert!(sends(&rig.go(20)).is_empty());

    // The same news again changes nothing; a new reason is shown.
    assert!(
        !rig.reach(30, 1, unresolved("no such host"))
            .iter()
            .any(|a| matches!(a, Action::ProblemsChanged { .. }))
    );
    rig.reach(40, 1, unresolved("timed out"));
    assert!(matches!(
        rig.problems(1).as_slice(),
        [CueProblem::NetworkHostUnresolved { reason, .. }] if reason == "timed out"
    ));

    // Resolved: fixed, and the cue sends again.
    let actions = rig.reach(50, 1, NetworkReachability::Resolved);
    assert!(rig.problems(1).is_empty());
    assert!(actions.contains(&Action::ProblemsChanged {
        cue: id(1),
        problems: vec![]
    }));
    assert_eq!(rig.engine.state(id(2)), CueState::Idle);
    let mut rig2 = rig;
    rig2.at(
        60,
        Input::SetPlayhead {
            list: list_id(1),
            cue: Some(id(1)),
        },
    );
    assert_eq!(sends(&rig2.go(70)).len(), 1);
}

#[test]
fn unknown_clears_a_failed_look_up() {
    let mut ws = workspace(vec![network(1)]);
    ws.network_patches[0].host = "lights.local".into();
    let mut rig = Rig::on(ws);
    rig.reach(0, 1, unresolved("x"));
    assert!(!rig.problems(1).is_empty());
    rig.reach(1, 1, NetworkReachability::Unknown);
    assert!(rig.problems(1).is_empty());
}

#[test]
fn a_report_about_an_ip_address_or_another_patch_changes_nothing() {
    // The default patch is 127.0.0.1: there is nothing to look up, so a failure report (a
    // stale one, say) cannot break its cues.
    let mut rig = Rig::new(vec![network(1)]);
    assert!(
        !rig.reach(0, 1, unresolved("stale"))
            .iter()
            .any(|a| matches!(a, Action::ProblemsChanged { .. }))
    );
    assert!(rig.problems(1).is_empty());
    rig.reach(1, 77, unresolved("unknown patch"));
    assert!(rig.problems(1).is_empty());
}

#[test]
fn a_broken_network_cue_with_auto_follow_is_skipped_like_other_broken_cues() {
    let mut a = network_to(1, 1, "bad", vec![]);
    a.continue_mode = ContinueMode::AutoFollow;
    let mut rig = Rig::new(vec![a, network_to(2, 1, "/two", vec![])]);
    let actions = rig.go(0);
    // Skipped for its problems, so the next cue starts (QLab: GO on a broken auto-follow cue).
    let sent = sends(&actions);
    assert_eq!(sent.len(), 1);
    assert!(matches!(sent[0], Action::SendOsc { cue, .. } if *cue == id(2)));
}

#[test]
fn deleting_the_cue_forgets_its_look_up_bookkeeping() {
    let mut ws = workspace(vec![network(1)]);
    ws.network_patches[0].host = "lights.local".into();
    let mut rig = Rig::on(ws);
    rig.reach(0, 1, unresolved("x"));
    rig.ws.cue_lists[0].cues.clear();
    rig.engine.model_changed();
    rig.last = Some(rig.ws.clone());
    rig.engine.handle(&rig.ws, Input::Tick, 1);
    rig.engine.verify_problem_cache(&rig.ws).unwrap();
    assert!(rig.engine.broken_cues().is_empty());
}

#[test]
fn editing_the_patch_host_rebuilds_the_problems() {
    let mut rig = Rig::new(vec![network(1)]);
    assert!(rig.problems(1).is_empty());
    set_patch(&mut rig, |p| p.port = 0);
    assert!(rig.at(1, Input::Tick).iter().any(|a| matches!(
        a,
        Action::ProblemsChanged { problems, .. } if !problems.is_empty()
    )));
    assert!(matches!(
        rig.problems(1).as_slice(),
        [CueProblem::NetworkHostInvalid { .. }]
    ));
    set_patch(&mut rig, |p| p.port = 9000);
    rig.at(2, Input::Tick);
    assert!(rig.problems(1).is_empty());
}

/// L1: an incremental update (`model_changed_cues`) notices a network patch whose host or port
/// changed although its id did not.
#[test]
fn an_incremental_edit_notices_a_changed_network_patch() {
    let mut rig = Rig::new(vec![network(1), memo(2)]);
    assert!(rig.problems(1).is_empty());
    rig.ws.network_patches[0].port = 0;
    rig.engine.model_changed_cues(&Footprint::edited([id(2)]));
    rig.last = Some(rig.ws.clone());
    rig.engine.handle(&rig.ws, Input::Tick, 1);
    rig.engine.verify_problem_cache(&rig.ws).unwrap();
    assert!(matches!(
        rig.problems(1).as_slice(),
        [CueProblem::NetworkHostInvalid { .. }]
    ));
}
