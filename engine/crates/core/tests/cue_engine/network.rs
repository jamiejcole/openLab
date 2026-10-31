// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! Network cues (CT-30) and their problems (NFR-07), per docs/cue-engine.md "Network cues".
//! Virtual sample clock at 1 kHz: one second is 1000 samples.

use crate::common::*;
use openlab_core::*;

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

fn sends(actions: &[Action]) -> Vec<&Action> {
    actions
        .iter()
        .filter(|a| matches!(a, Action::SendOsc { .. }))
        .collect()
}

fn set_patch(rig: &mut Rig, f: impl FnOnce(&mut NetworkPatch)) {
    f(&mut rig.ws.network_patches[0]);
}

#[test]
fn go_sends_the_message_once_and_the_cue_completes_at_once() {
    let mut rig = Rig::checked(vec![network(1), memo(2)]);
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
    let mut rig = Rig::checked(vec![c]);
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
    let mut rig = Rig::checked(vec![c, network_to(2, 1, "/two", vec![])]);
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
    let mut rig = Rig::checked(vec![
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
    let mut rig = Rig::checked(vec![c, network_to(2, 1, "/two", vec![])]);
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
    let mut rig = Rig::checked(vec![network_to(1, 1, "/all", args.clone())]);
    let actions = rig.go(0);
    assert!(matches!(
        sends(&actions)[0],
        Action::SendOsc { args: sent, .. } if *sent == args
    ));
}

#[test]
fn a_missing_patch_breaks_the_cue_and_nothing_is_sent() {
    let mut rig = Rig::checked(vec![network_to(1, 9, "/x", vec![])]);
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
        let mut rig = Rig::on(ws, EngineConfig::new(RATE, 0, 1).unwrap());
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
        let mut rig = Rig::checked(vec![c.clone()]);
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
    let rig = Rig::on(ws, EngineConfig::new(RATE, 0, 1).unwrap());
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
fn a_host_name_is_never_checked_before_go_and_go_sends() {
    // QLab 5 does not flag a destination it cannot find (manual pp. 371-372 list only a missing
    // patch, an incomplete patch and a malformed message), so a name is not a problem: the
    // cue stays playable and GO emits the send for the worker to resolve and warn about.
    let mut ws = workspace(vec![network(1), memo(2)]);
    ws.network_patches[0].host = "no-such-host.invalid".into();
    let mut rig = Rig::on(ws, EngineConfig::new(RATE, 0, 1).unwrap());
    assert!(rig.problems(1).is_empty());
    assert_eq!(rig.engine.state(id(1)), CueState::Idle);
    assert!(rig.engine.broken_cues().is_empty());
    let actions = rig.go(10);
    assert_eq!(sends(&actions).len(), 1);
}

#[test]
fn a_broken_network_cue_with_auto_follow_is_skipped_like_other_broken_cues() {
    let mut a = network_to(1, 1, "bad", vec![]);
    a.continue_mode = ContinueMode::AutoFollow;
    let mut rig = Rig::checked(vec![a, network_to(2, 1, "/two", vec![])]);
    let actions = rig.go(0);
    // Skipped for its problems, so the next cue starts (QLab: GO on a broken auto-follow cue).
    let sent = sends(&actions);
    assert_eq!(sent.len(), 1);
    assert!(matches!(sent[0], Action::SendOsc { cue, .. } if *cue == id(2)));
}

#[test]
fn deleting_a_network_cue_leaves_the_problem_cache_consistent() {
    let mut ws = workspace(vec![network_to(1, 1, "bad", vec![])]);
    ws.network_patches[0].host = "lights.local".into();
    let mut rig = Rig::on(ws, EngineConfig::new(RATE, 0, 1).unwrap());
    assert!(!rig.problems(1).is_empty());
    rig.ws.cue_lists[0].cues.clear();
    rig.engine.model_changed();
    rig.last = Some(rig.ws.clone());
    rig.engine.handle(&rig.ws, Input::Tick, 1);
    rig.engine.verify_problem_cache(&rig.ws).unwrap();
    assert!(rig.engine.broken_cues().is_empty());
}

#[test]
fn editing_the_patch_host_rebuilds_the_problems() {
    let mut rig = Rig::checked(vec![network(1)]);
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
    let mut rig = Rig::checked(vec![network(1), memo(2)]);
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

/// The commands that edit a Network cue reach the engine; GO sends the edited message.
#[test]
fn an_edited_network_cue_sends_the_edited_message() {
    let mut rig = Rig::checked(vec![network(1), memo(2)]);
    rig.ws.network_patches.push(NetworkPatch {
        id: net_id(2),
        name: "Second".into(),
        host: "10.0.0.2".into(),
        port: 9000,
        protocol: NetworkProtocol::Udp,
    });
    for property in [
        CueProperty::Destination { value: net_id(2) },
        CueProperty::Address {
            value: "/lx/[1-2]/go".into(),
        },
        CueProperty::Args {
            value: vec![
                OscArg::String { value: "q".into() },
                OscArg::Float { value: 0.5 },
            ],
        },
    ] {
        rig.ws
            .apply(Command::SetCueProperty {
                id: id(1),
                property,
            })
            .unwrap();
        rig.at(1, Input::Tick);
    }
    assert!(rig.problems(1).is_empty());
    assert_eq!(
        sends(&rig.go(5)),
        vec![&Action::SendOsc {
            cue: id(1),
            at: 5,
            destination: net_id(2),
            address: "/lx/[1-2]/go".into(),
            args: vec![
                OscArg::String { value: "q".into() },
                OscArg::Float { value: 0.5 }
            ],
        }]
    );
}

/// QLab parity (manual pp. 371-372): a malformed address typed into the cue is accepted, the cue
/// shows broken (`network_message_invalid`) and sends nothing; fixing it clears the problem.
#[test]
fn a_malformed_address_edit_breaks_the_cue_until_it_is_fixed() {
    let mut rig = Rig::checked(vec![network(1), memo(2)]);
    let edit = |rig: &mut Rig, value: &str| {
        rig.ws
            .apply(Command::SetCueProperty {
                id: id(1),
                property: CueProperty::Address {
                    value: value.into(),
                },
            })
            .unwrap();
        rig.at(1, Input::Tick);
    };
    edit(&mut rig, "lx go");
    assert!(matches!(
        rig.problems(1).as_slice(),
        [CueProblem::NetworkMessageInvalid { .. }]
    ));
    assert_eq!(rig.engine.state(id(1)), CueState::Broken);
    assert!(sends(&rig.go(2)).is_empty());
    edit(&mut rig, "/lx/go");
    assert!(rig.problems(1).is_empty());
    assert_ne!(rig.engine.state(id(1)), CueState::Broken);
}

/// A destination naming no patch is accepted by the edit; the cue shows broken and sends nothing.
#[test]
fn an_unknown_destination_edit_breaks_the_cue() {
    let mut rig = Rig::checked(vec![network(1)]);
    rig.ws
        .apply(Command::SetCueProperty {
            id: id(1),
            property: CueProperty::Destination { value: net_id(9) },
        })
        .unwrap();
    rig.at(1, Input::Tick);
    assert_eq!(
        rig.problems(1),
        vec![CueProblem::NetworkPatchMissing { patch: net_id(9) }]
    );
    assert!(sends(&rig.go(2)).is_empty());
}
