// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Editing a Network cue: `Destination`, `Address` and `Args` (M4.6a, CT-30, ED-07).

mod common;

use common::*;
use openlab_core::*;

fn net(n: u128) -> NetworkPatch {
    NetworkPatch {
        id: net_id(n),
        name: format!("Net {n}"),
        host: "10.0.0.1".into(),
        port: 8000,
        protocol: NetworkProtocol::Udp,
    }
}

fn network_cue(n: u128) -> Cue {
    let mut c = cue(n, CueKind::Memo);
    c.kind = CueKind::Network {
        destination: net_id(1),
        address: "/go".into(),
        args: vec![OscArg::Int { value: 1 }],
    };
    c
}

fn ws() -> Workspace {
    let mut ws = workspace(vec![network_cue(1), cue(2, CueKind::Memo)]);
    ws.network_patches.push(net(2));
    ws
}

fn set(id: u128, property: CueProperty) -> Command {
    Command::SetCueProperty {
        id: cue_id(id),
        property,
    }
}

fn network(ws: &Workspace) -> (NetworkPatchId, String, Vec<OscArg>) {
    match &ws.find_cue(cue_id(1)).unwrap().kind {
        CueKind::Network {
            destination,
            address,
            args,
        } => (*destination, address.clone(), args.clone()),
        other => panic!("{other:?}"),
    }
}

fn address(v: &str) -> CueProperty {
    CueProperty::Address { value: v.into() }
}

fn args(v: Vec<OscArg>) -> CueProperty {
    CueProperty::Args { value: v }
}

#[test]
fn each_property_edits_the_cue_and_undoes() {
    let mut w = ws();
    let before = w.clone();
    let mut history = History::new();
    for p in [
        CueProperty::Destination { value: net_id(2) },
        address("/cue/[1-3]/*"),
        args(vec![
            OscArg::Float { value: 0.5 },
            OscArg::String { value: "x".into() },
            OscArg::Bool { value: true },
        ]),
    ] {
        assert!(history.execute(&mut w, set(1, p)).unwrap());
    }
    let (d, a, r) = network(&w);
    assert_eq!((d, a.as_str(), r.len()), (net_id(2), "/cue/[1-3]/*", 3));
    let edited = w.clone();
    for _ in 0..3 {
        history.undo(&mut w).unwrap();
    }
    assert_eq!(w, before);
    for _ in 0..3 {
        history.redo(&mut w).unwrap();
    }
    assert_eq!(w, edited);
}

#[test]
fn the_same_value_is_not_an_edit() {
    let mut w = ws();
    let mut history = History::new();
    assert!(!history.execute(&mut w, set(1, address("/go"))).unwrap());
    assert_eq!(history.undo_len(), 0);
}

#[test]
fn other_kinds_do_not_have_the_properties() {
    let mut w = ws();
    for p in [
        CueProperty::Destination { value: net_id(1) },
        address("/x"),
        args(vec![]),
    ] {
        assert_eq!(
            w.apply(set(2, p)),
            Err(CommandError::PropertyNotApplicable(cue_id(2)))
        );
    }
}

#[test]
fn hostile_values_are_refused_and_change_nothing() {
    let mut w = ws();
    let before = w.clone();
    let many = vec![OscArg::Int { value: 0 }; 33];
    let long = "/".to_string() + &"a".repeat(512);
    let cases: Vec<(CueProperty, CommandError)> = vec![
        (
            CueProperty::Destination { value: net_id(9) },
            CommandError::NetworkPatchNotFound(net_id(9)),
        ),
        (
            address("go"),
            CommandError::InvalidMessage(MessageIssue::NotAnAddress),
        ),
        (
            address(""),
            CommandError::InvalidMessage(MessageIssue::NotAnAddress),
        ),
        (
            address(&long),
            CommandError::InvalidMessage(MessageIssue::AddressTooLong),
        ),
        (
            address("/a b"),
            CommandError::InvalidMessage(MessageIssue::AddressCharacter),
        ),
        (
            address("/a\0b"),
            CommandError::InvalidMessage(MessageIssue::AddressCharacter),
        ),
        (
            args(many),
            CommandError::InvalidMessage(MessageIssue::TooManyArgs),
        ),
        (
            args(vec![OscArg::Float { value: f32::NAN }]),
            CommandError::InvalidMessage(MessageIssue::FloatNotFinite),
        ),
        (
            args(vec![OscArg::Float {
                value: f32::INFINITY,
            }]),
            CommandError::InvalidMessage(MessageIssue::FloatNotFinite),
        ),
        (
            args(vec![OscArg::String {
                value: "a\0".into(),
            }]),
            CommandError::InvalidMessage(MessageIssue::StringNul),
        ),
        (
            args(vec![OscArg::String {
                value: "x".repeat(4097),
            }]),
            CommandError::InvalidMessage(MessageIssue::StringTooLong),
        ),
        // Eight strings of 1100 bytes each fit the per-string limit but not the message.
        (
            args(vec![
                OscArg::String {
                    value: "x".repeat(1100)
                };
                8
            ]),
            CommandError::InvalidMessage(MessageIssue::TooLarge),
        ),
    ];
    for (property, error) in cases {
        assert_eq!(
            w.apply(set(1, property.clone())),
            Err(error),
            "{property:?}"
        );
        assert_eq!(w, before);
    }
}

#[test]
fn the_size_limit_holds_for_the_pair() {
    let mut w = ws();
    let big = vec![
        OscArg::String {
            value: "x".repeat(4000)
        };
        2
    ];
    w.apply(set(1, args(big))).unwrap();
    let before = w.clone();
    assert!(matches!(
        w.apply(set(1, address(&format!("/{}", "a".repeat(300))))),
        Err(CommandError::InvalidMessage(MessageIssue::TooLarge))
    ));
    assert_eq!(w, before);
}

#[test]
fn a_batch_refused_halfway_rolls_back() {
    let mut w = ws();
    let before = w.clone();
    let batch = Command::BatchSet {
        ids: vec![cue_id(1), cue_id(2)],
        property: address("/x"),
    };
    assert!(matches!(
        w.apply(batch),
        Err(CommandError::InTransaction { index: 1, .. })
    ));
    assert_eq!(w, before);
}

#[test]
fn undo_restores_a_value_that_load_tolerates() {
    // A destination that no longer exists (the patch was removed on purpose) is the usual
    // broken-cue state; undoing back to it must not be refused.
    let mut w = ws();
    let mut history = History::new();
    history
        .execute(
            &mut w,
            Command::RemoveNetworkPatch {
                id: net_id(1),
                even_if_used: true,
            },
        )
        .unwrap();
    history
        .execute(
            &mut w,
            set(1, CueProperty::Destination { value: net_id(2) }),
        )
        .unwrap();
    history.undo(&mut w).unwrap();
    assert_eq!(network(&w).0, net_id(1));
    history.redo(&mut w).unwrap();
    assert_eq!(network(&w).0, net_id(2));
}

#[test]
fn rapid_edits_of_one_field_are_one_undo_step() {
    let mut w = ws();
    let before = w.clone();
    let mut history = History::new();
    for n in 0..20 {
        history
            .execute_coalescing(&mut w, set(1, address(&format!("/fade/{n}"))), Some("addr"))
            .unwrap();
    }
    assert_eq!(network(&w).1, "/fade/19");
    history.undo(&mut w).unwrap();
    assert_eq!(w, before);
    assert_eq!(history.undo_len(), 0);
}

#[test]
fn edits_of_different_fields_in_one_gesture_all_undo() {
    let mut w = ws();
    let before = w.clone();
    let mut history = History::new();
    for p in [address("/a"), args(vec![]), address("/b")] {
        history
            .execute_coalescing(&mut w, set(1, p), Some("k"))
            .unwrap();
    }
    history.undo(&mut w).unwrap();
    assert_eq!(w, before);
}

#[test]
fn the_footprint_is_the_cue() {
    let c = set(1, address("/x"));
    assert!(c.footprint().edited.contains(&cue_id(1)));
}

/// A cue that a show file carries with a bad address and too many arguments (load repairs only
/// non-finite floats). Each half can be mended on its own, and both in one transaction.
fn loaded_invalid() -> Workspace {
    let mut w = ws();
    if let CueKind::Network { address, args, .. } = &mut w.cue_lists[0].cues[0].kind {
        *address = "bad".into();
        *args = vec![OscArg::Int { value: 0 }; 40];
    }
    let text = save_show(&w).unwrap();
    let loaded = load_show(&text).unwrap().workspace;
    assert_eq!(network(&loaded).1, "bad");
    assert_eq!(network(&loaded).2.len(), 40);
    loaded
}

#[test]
fn a_cue_loaded_invalid_is_mended_one_half_at_a_time() {
    let mut w = loaded_invalid();
    w.apply(set(1, address("/ok"))).unwrap();
    w.apply(set(1, args(vec![OscArg::Int { value: 1 }])))
        .unwrap();
    assert_eq!(network(&w).1, "/ok");
    let mut w = loaded_invalid();
    w.apply(set(1, args(vec![]))).unwrap();
    w.apply(set(1, address("/ok"))).unwrap();
}

#[test]
fn a_cue_loaded_invalid_is_mended_by_a_transaction_of_both() {
    let mut w = loaded_invalid();
    let mut history = History::new();
    history
        .execute(
            &mut w,
            Command::Transaction {
                commands: vec![set(1, address("/ok")), set(1, args(vec![]))],
            },
        )
        .unwrap();
    assert_eq!((network(&w).1.as_str(), network(&w).2.len()), ("/ok", 0));
    history.undo(&mut w).unwrap();
    assert_eq!(network(&w).1, "bad");
}

#[test]
fn a_value_still_invalid_by_itself_is_refused_on_a_cue_loaded_invalid() {
    let mut w = loaded_invalid();
    assert!(w.apply(set(1, address("still bad"))).is_err());
    assert!(
        w.apply(set(1, args(vec![OscArg::Int { value: 0 }; 33])))
            .is_err()
    );
}

#[test]
fn setting_the_current_value_of_a_loaded_invalid_cue_is_a_no_op() {
    let mut w = loaded_invalid();
    let before = w.clone();
    let inverse = w.apply(set(1, address("bad"))).unwrap();
    assert_eq!(inverse, Command::Transaction { commands: vec![] });
    assert_eq!(w, before);
}
