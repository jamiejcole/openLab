// SPDX-License-Identifier: GPL-3.0-or-later

//! The socket-free OSC layer (M4.5 part 1): codec, SLIP, dispatch, sessions, replies.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use openlab_core::protocol::{ClientMessage, RequestId, TransportAction};
use openlab_core::{Command, CueId, CueListId, CueProperty, NumberAssignment, WorkspaceId};
use openlab_engine::osc::{
    CueField, DecodeError, GLOBAL_MAX_FAILURES, GLOBAL_WINDOW, LOCKOUT, MAX_ARRAY_DEPTH,
    MAX_BAD_PASSCODES, MAX_BUNDLE_DEPTH, MAX_MESSAGES_PER_PACKET, MAX_PACKET_BYTES, MAX_PEERS,
    MAX_REPLIES_PER_CALL, MAX_REPLY_JSON_BYTES, Osc, OscInput, OscShow, Outcome, Peer, SlipDecoder,
    SlipEvent, UDP_IDLE, decode_packet, encode_message, parse_address, slip_encode,
};
use proptest::prelude::*;
use rosc::{OscMessage, OscPacket, OscTime, OscType};
use serde_json::{Value, json};
use uuid::Uuid;

const WS: u128 = 0xAB;
const LIST: CueListId = CueListId(Uuid::from_u128(0x11));
const CUE_1: CueId = CueId(Uuid::from_u128(0x101));
const CUE_25: CueId = CueId(Uuid::from_u128(0x102));
const TRIGGERED: CueId = CueId(Uuid::from_u128(0x103));

struct Show {
    passcode: Option<String>,
    list: Option<CueListId>,
    props: HashMap<(CueId, &'static str), Value>,
}

impl Show {
    fn open() -> Self {
        let mut props = HashMap::new();
        props.insert((CUE_1, "name"), json!("Opening"));
        props.insert((CUE_1, "preWait"), json!(1.5));
        props.insert((CUE_1, "armed"), json!(true));
        Self {
            passcode: None,
            list: Some(LIST),
            props,
        }
    }

    fn with_passcode(code: &str) -> Self {
        Self {
            passcode: Some(code.to_owned()),
            ..Self::open()
        }
    }
}

fn workspace() -> WorkspaceId {
    WorkspaceId(Uuid::from_u128(WS))
}

impl OscShow for Show {
    fn workspace_id(&self) -> WorkspaceId {
        workspace()
    }
    fn passcode(&self) -> Option<&str> {
        self.passcode.as_deref()
    }
    fn active_list(&self) -> Option<CueListId> {
        self.list
    }
    fn cue_by_number(&self, number: &str) -> Option<CueId> {
        match number {
            "1" => Some(CUE_1),
            "2.5" => Some(CUE_25),
            _ => None,
        }
    }
    fn cue_exists(&self, id: CueId) -> bool {
        [CUE_1, CUE_25, TRIGGERED].contains(&id)
    }
    fn cue_property(&self, id: CueId, field: CueField) -> Option<Value> {
        let key = match field {
            CueField::Name => "name",
            CueField::Number => "number",
            CueField::Notes => "notes",
            CueField::PreWait => "preWait",
            CueField::PostWait => "postWait",
            CueField::Armed => "armed",
            CueField::Flagged => "flagged",
        };
        self.props.get(&(id, key)).cloned()
    }
    fn cue_by_osc_trigger(&self, address: &str) -> Option<CueId> {
        matches!(address, "/lights/blackout" | "/fx/all*").then_some(TRIGGERED)
    }
}

fn tcp_at(conn: u64, last_octet: u8) -> Peer {
    Peer::Tcp {
        conn,
        ip: IpAddr::V4(Ipv4Addr::new(10, 0, 9, last_octet)),
    }
}

fn tcp_peer(conn: u64) -> Peer {
    tcp_at(conn, 1)
}

fn ip_n(i: usize) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(
        10,
        1,
        u8::try_from(i / 256).unwrap(),
        u8::try_from(i % 256).unwrap(),
    ))
}

fn msg(addr: &str, args: Vec<OscType>) -> OscMessage {
    OscMessage {
        addr: addr.to_owned(),
        args,
    }
}

fn bytes(m: &OscMessage) -> Vec<u8> {
    encode_message(m).unwrap()
}

fn s(text: &str) -> OscType {
    OscType::String(text.to_owned())
}

fn udp() -> Peer {
    Peer::Udp(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7)))
}

const T0: Duration = Duration::from_secs(100);

/// Sends one message as a datagram and returns what came out.
fn send(osc: &mut Osc, show: &Show, peer: Peer, now: Duration, m: &OscMessage) -> Outcome {
    osc.handle_datagram(peer, now, &bytes(m), show)
}

fn one_input(osc: &mut Osc, show: &Show, addr: &str, args: Vec<OscType>) -> OscInput {
    let out = send(osc, show, udp(), T0, &msg(addr, args));
    assert!(
        out.replies.is_empty(),
        "{addr}: unexpected {:?}",
        out.replies
    );
    assert_eq!(out.inputs.len(), 1, "{addr}: {out:?}");
    out.inputs.into_iter().next().unwrap()
}

fn transport(action: TransportAction) -> OscInput {
    OscInput::Transport(action)
}

fn reply_json(m: &OscMessage) -> Value {
    let [OscType::String(text)] = m.args.as_slice() else {
        panic!("reply must have one string argument: {m:?}");
    };
    serde_json::from_str(text).unwrap()
}

// ---- addresses -> inputs

#[test]
fn each_supported_address_becomes_its_protocol_input() {
    let show = Show::open();
    let mut osc = Osc::new();
    let cases: Vec<(&str, Vec<OscType>, OscInput)> = vec![
        ("/go", vec![], transport(TransportAction::Go { list: LIST })),
        ("/panic", vec![], transport(TransportAction::Panic)),
        ("/pause", vec![], transport(TransportAction::PauseAll)),
        ("/resume", vec![], transport(TransportAction::ResumeAll)),
        (
            "/playhead/next",
            vec![],
            transport(TransportAction::PlayheadNext { list: LIST }),
        ),
        (
            "/playhead/previous",
            vec![],
            transport(TransportAction::PlayheadPrevious { list: LIST }),
        ),
        (
            "/playhead/2.5",
            vec![],
            transport(TransportAction::SetPlayheadByNumber {
                list: LIST,
                number: "2.5".into(),
            }),
        ),
        (
            "/select/7",
            vec![],
            transport(TransportAction::SetPlayheadByNumber {
                list: LIST,
                number: "7".into(),
            }),
        ),
        (
            "/select/next",
            vec![],
            transport(TransportAction::PlayheadNext { list: LIST }),
        ),
        (
            "/cue/1/start",
            vec![],
            transport(TransportAction::Start { cue: CUE_1 }),
        ),
        (
            "/cue/2.5/stop",
            vec![],
            transport(TransportAction::Stop {
                cue: CUE_25,
                fade_out: None,
            }),
        ),
        (
            "/cue/1/hardStop",
            vec![],
            transport(TransportAction::HardStop { cue: CUE_1 }),
        ),
        (
            "/cue/1/pause",
            vec![],
            transport(TransportAction::Pause { cue: CUE_1 }),
        ),
        (
            "/cue/1/resume",
            vec![],
            transport(TransportAction::Resume { cue: CUE_1 }),
        ),
        (
            "/cue/1/load",
            vec![],
            transport(TransportAction::Load {
                cue: CUE_1,
                at: 0.0,
            }),
        ),
        (
            "/cue/1/load",
            vec![OscType::Float(2.5)],
            transport(TransportAction::Load {
                cue: CUE_1,
                at: 2.5,
            }),
        ),
        (
            "/cue/1/reset",
            vec![],
            transport(TransportAction::Reset { cue: CUE_1 }),
        ),
        (
            "/cue/1/preview",
            vec![],
            transport(TransportAction::Preview { cue: CUE_1 }),
        ),
        (
            "/cue_id/00000000-0000-0000-0000-000000000102/start",
            vec![],
            transport(TransportAction::Start { cue: CUE_25 }),
        ),
        (
            "/lights/blackout",
            vec![],
            transport(TransportAction::Start { cue: TRIGGERED }),
        ),
    ];
    for (addr, args, expected) in cases {
        assert_eq!(one_input(&mut osc, &show, addr, args), expected, "{addr}");
    }
    assert_eq!(osc.stats().rejected, 0);
}

#[test]
fn inputs_become_the_messages_the_ui_sends() {
    let input = transport(TransportAction::Panic);
    assert_eq!(
        input.into_client_message(RequestId(9)),
        ClientMessage::Transport {
            id: RequestId(9),
            action: TransportAction::Panic
        }
    );
}

#[test]
fn cue_properties_are_set_with_commands() {
    let show = Show::open();
    let mut osc = Osc::new();
    let set = |property| {
        OscInput::Command(Box::new(Command::SetCueProperty {
            id: CUE_1,
            property,
        }))
    };
    let cases: Vec<(&str, OscType, OscInput)> = vec![
        (
            "name",
            s("Intro"),
            set(CueProperty::Name {
                value: "Intro".into(),
            }),
        ),
        (
            "notes",
            s("watch the lift"),
            set(CueProperty::Notes {
                value: "watch the lift".into(),
            }),
        ),
        (
            "preWait",
            OscType::Int(3),
            set(CueProperty::PreWait { value: 3.0 }),
        ),
        (
            "postWait",
            OscType::Double(0.25),
            set(CueProperty::PostWait { value: 0.25 }),
        ),
        (
            "armed",
            OscType::Int(0),
            set(CueProperty::Armed { value: false }),
        ),
        (
            "flagged",
            OscType::Bool(true),
            set(CueProperty::Flagged { value: true }),
        ),
        (
            "number",
            s("12"),
            OscInput::Command(Box::new(Command::SetCueNumbers {
                numbers: vec![NumberAssignment {
                    id: CUE_1,
                    number: Some("12".into()),
                }],
            })),
        ),
        (
            "number",
            OscType::Float(3.0),
            OscInput::Command(Box::new(Command::SetCueNumbers {
                numbers: vec![NumberAssignment {
                    id: CUE_1,
                    number: Some("3".into()),
                }],
            })),
        ),
        (
            "number",
            s(""),
            OscInput::Command(Box::new(Command::SetCueNumbers {
                numbers: vec![NumberAssignment {
                    id: CUE_1,
                    number: None,
                }],
            })),
        ),
    ];
    for (field, arg, expected) in cases {
        let addr = format!("/cue/1/{field}");
        assert_eq!(
            one_input(&mut osc, &show, &addr, vec![arg]),
            expected,
            "{addr}"
        );
    }
}

#[test]
fn bad_property_arguments_get_an_error_reply_and_no_input() {
    let show = Show::open();
    let mut osc = Osc::new();
    let cases: Vec<(&str, Vec<OscType>)> = vec![
        ("/cue/1/name", vec![OscType::Int(3)]),
        ("/cue/1/name", vec![s("a"), s("b")]),
        ("/cue/1/preWait", vec![OscType::Float(-1.0)]),
        ("/cue/1/preWait", vec![OscType::Float(f32::NAN)]),
        ("/cue/1/preWait", vec![OscType::Double(f64::INFINITY)]),
        ("/cue/1/armed", vec![OscType::Int(2)]),
        ("/cue/1/number", vec![OscType::Nil]),
        ("/cue/1/load", vec![OscType::Float(-0.5)]),
        ("/cue/1/load", vec![s("x")]),
        ("/cue/1/start", vec![OscType::Int(1)]),
    ];
    for (addr, args) in cases {
        let out = send(&mut osc, &show, udp(), T0, &msg(addr, args.clone()));
        assert!(out.inputs.is_empty(), "{addr} {args:?}");
        assert_eq!(out.replies.len(), 1, "{addr} {args:?}");
        assert_eq!(reply_json(&out.replies[0])["status"], "error");
    }
    assert_eq!(osc.stats().rejected, 10);
}

#[test]
fn property_get_replies_with_the_value() {
    let show = Show::open();
    let mut osc = Osc::new();
    let out = send(&mut osc, &show, udp(), T0, &msg("/cue/1/name", vec![]));
    assert!(out.inputs.is_empty());
    let [reply] = out.replies.as_slice() else {
        panic!("{out:?}")
    };
    assert_eq!(reply.addr, "/reply/cue/1/name");
    assert_eq!(
        reply_json(reply),
        json!({
            "workspace_id": workspace().to_string(),
            "address": "/cue/1/name",
            "status": "ok",
            "data": "Opening",
        })
    );
    let out = send(&mut osc, &show, udp(), T0, &msg("/cue/1/preWait", vec![]));
    assert_eq!(reply_json(&out.replies[0])["data"], json!(1.5));
    let out = send(&mut osc, &show, udp(), T0, &msg("/cue/1/armed", vec![]));
    assert_eq!(reply_json(&out.replies[0])["data"], json!(true));
    // A cue without that property.
    let out = send(&mut osc, &show, udp(), T0, &msg("/cue/2.5/name", vec![]));
    assert_eq!(reply_json(&out.replies[0])["status"], "error");
}

#[test]
fn unknown_cues_and_missing_list_are_error_replies() {
    let mut show = Show::open();
    let mut osc = Osc::new();
    for addr in [
        "/cue/99/start",
        "/cue_id/00000000-0000-0000-0000-0000000fffff/start",
        "/cue_id/not-a-uuid/start",
        "/cue/*/start",
        "/cue/{1,2}/start",
        "/stop",
    ] {
        let out = send(&mut osc, &show, udp(), T0, &msg(addr, vec![]));
        assert!(out.inputs.is_empty(), "{addr}");
        let [reply] = out.replies.as_slice() else {
            panic!("{addr}: {out:?}")
        };
        assert_eq!(reply_json(reply)["status"], "error", "{addr}");
        assert_eq!(reply_json(reply)["address"], addr);
    }
    show.list = None;
    for addr in ["/go", "/playhead/next", "/select/1"] {
        let out = send(&mut osc, &show, udp(), T0, &msg(addr, vec![]));
        assert!(out.inputs.is_empty(), "{addr}");
        assert_eq!(reply_json(&out.replies[0])["status"], "error", "{addr}");
    }
}

#[test]
fn unknown_and_unusable_addresses_are_dropped_and_counted() {
    let show = Show::open();
    let mut osc = Osc::new();
    for addr in ["/nothing/here", "/cue/1/unknown", "/cue", "/workspace/x"] {
        let out = send(&mut osc, &show, udp(), T0, &msg(addr, vec![]));
        assert_eq!(out, Outcome::default(), "{addr}");
    }
    assert_eq!(osc.stats().unknown_address, 4);
    // Not addresses at all: empty parts, too many parts, too long. (rosc refuses to encode
    // none of them, so they reach the dispatcher.)
    let long = format!("/{}", "a".repeat(300));
    for addr in ["/cue//start", "//go", "/a/b/c/d/e/f/g/h/i", long.as_str()] {
        let out = send(&mut osc, &show, udp(), T0, &msg(addr, vec![]));
        assert_eq!(out, Outcome::default(), "{addr}");
    }
    assert_eq!(osc.stats().bad_address, 4);
    // A trailing slash is tolerated.
    assert_eq!(
        one_input(&mut osc, &show, "/go/", vec![]),
        transport(TransportAction::Go { list: LIST })
    );
}

// ---- /workspace/{id}/ prefix

#[test]
fn workspace_prefix_with_the_right_id_works_for_every_form() {
    let show = Show::open();
    let mut osc = Osc::new();
    let id = workspace().to_string();
    assert_eq!(
        one_input(&mut osc, &show, &format!("/workspace/{id}/go"), vec![]),
        transport(TransportAction::Go { list: LIST })
    );
    assert_eq!(
        one_input(
            &mut osc,
            &show,
            &format!("/workspace/{}/cue/1/start", id.to_uppercase()),
            vec![]
        ),
        transport(TransportAction::Start { cue: CUE_1 })
    );
    assert_eq!(
        one_input(
            &mut osc,
            &show,
            &format!("/workspace/{id}/playhead/2.5"),
            vec![]
        ),
        transport(TransportAction::SetPlayheadByNumber {
            list: LIST,
            number: "2.5".into()
        })
    );
    // A reply to a prefixed address keeps the prefix.
    let asked = format!("/workspace/{id}/cue/1/name");
    let out = send(&mut osc, &show, udp(), T0, &msg(&asked, vec![]));
    assert_eq!(out.replies[0].addr, format!("/reply{asked}"));
    assert_eq!(reply_json(&out.replies[0])["address"], asked);
}

#[test]
fn workspace_prefix_with_another_id_is_ignored_without_a_reply() {
    let show = Show::open();
    let mut osc = Osc::new();
    for addr in [
        "/workspace/00000000-0000-0000-0000-000000000001/go",
        "/workspace/00000000-0000-0000-0000-000000000001/connect",
        "/workspace/nonsense/cue/1/start",
    ] {
        let out = send(&mut osc, &show, udp(), T0, &msg(addr, vec![]));
        assert_eq!(out, Outcome::default(), "{addr}");
    }
    assert_eq!(osc.stats().wrong_workspace, 3);
}

// ---- passcode

#[test]
fn without_a_passcode_nothing_needs_connect_and_connect_is_accepted() {
    let show = Show::open();
    let mut osc = Osc::new();
    let out = send(&mut osc, &show, udp(), T0, &msg("/go", vec![]));
    assert_eq!(out.inputs.len(), 1);
    let out = send(
        &mut osc,
        &show,
        udp(),
        T0,
        &msg("/connect", vec![s("anything")]),
    );
    let reply = reply_json(&out.replies[0]);
    assert_eq!(
        (reply["status"].as_str(), reply["data"].as_str()),
        (Some("ok"), Some("ok"))
    );
    assert_eq!(out.replies[0].addr, "/reply/connect");
    // An empty passcode is no passcode.
    let mut show = Show::open();
    show.passcode = Some(String::new());
    assert_eq!(
        send(&mut Osc::new(), &show, udp(), T0, &msg("/go", vec![]))
            .inputs
            .len(),
        1
    );
}

#[test]
fn with_a_passcode_everything_but_connect_is_denied_until_connected() {
    let show = Show::with_passcode("s3cret");
    let mut osc = Osc::new();
    let tcp = tcp_peer(1);
    // TCP peers are told; UDP peers (possibly forged) are not.
    let out = send(&mut osc, &show, tcp, T0, &msg("/go", vec![]));
    assert!(out.inputs.is_empty());
    assert_eq!(reply_json(&out.replies[0])["status"], "denied");
    let out = send(&mut osc, &show, udp(), T0, &msg("/go", vec![]));
    assert_eq!(out, Outcome::default());
    assert_eq!(osc.stats().denied, 2);
    // Denial comes before anything about the show is revealed.
    let out = send(&mut osc, &show, tcp, T0, &msg("/cue/99/start", vec![]));
    assert_eq!(reply_json(&out.replies[0])["status"], "denied");
    let out = send(
        &mut osc,
        &show,
        tcp,
        T0,
        &msg("/updates", vec![OscType::Int(1)]),
    );
    assert_eq!(reply_json(&out.replies[0])["status"], "denied");
    assert!(osc.update_subscribers().is_empty());
}

#[test]
fn the_right_passcode_connects_and_the_wrong_one_does_not() {
    let show = Show::with_passcode("s3cret");
    let mut osc = Osc::new();
    let out = send(
        &mut osc,
        &show,
        udp(),
        T0,
        &msg("/connect", vec![s("nope")]),
    );
    let reply = reply_json(&out.replies[0]);
    assert_eq!(
        (reply["status"].as_str(), reply["data"].as_str()),
        (Some("error"), Some("badpass"))
    );
    assert!(
        send(&mut osc, &show, udp(), T0, &msg("/go", vec![]))
            .inputs
            .is_empty()
    );
    // No argument, or the wrong type, is a wrong passcode.
    for args in [vec![], vec![OscType::Int(0)], vec![s("s3cret"), s("x")]] {
        let out = send(&mut osc, &show, udp(), T0, &msg("/connect", args));
        assert_eq!(reply_json(&out.replies[0])["data"], "badpass");
    }
    let out = send(
        &mut osc,
        &show,
        udp(),
        T0,
        &msg("/connect", vec![s("s3cret")]),
    );
    assert_eq!(reply_json(&out.replies[0])["status"], "ok");
    assert_eq!(
        send(&mut osc, &show, udp(), T0, &msg("/go", vec![]))
            .inputs
            .len(),
        1
    );
    // Another peer is not connected by it.
    let other = Peer::Udp(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 8)));
    assert!(
        send(&mut osc, &show, other, T0, &msg("/go", vec![]))
            .inputs
            .is_empty()
    );
    // The prefixed form connects too.
    let asked = format!("/workspace/{}/connect", workspace());
    let out = send(&mut osc, &show, other, T0, &msg(&asked, vec![s("s3cret")]));
    assert_eq!(out.replies[0].addr, format!("/reply{asked}"));
    assert_eq!(
        send(&mut osc, &show, other, T0, &msg("/go", vec![]))
            .inputs
            .len(),
        1
    );
}

#[test]
fn repeated_wrong_passcodes_lock_the_peer_out_for_a_while() {
    let show = Show::with_passcode("s3cret");
    let mut osc = Osc::new();
    for _ in 0..MAX_BAD_PASSCODES {
        let out = send(&mut osc, &show, udp(), T0, &msg("/connect", vec![s("x")]));
        assert_eq!(reply_json(&out.replies[0])["data"], "badpass");
    }
    // Even the right passcode is refused during the lockout.
    let during = T0 + LOCKOUT - Duration::from_secs(1);
    let out = send(
        &mut osc,
        &show,
        udp(),
        during,
        &msg("/connect", vec![s("s3cret")]),
    );
    assert_eq!(reply_json(&out.replies[0])["status"], "denied");
    assert!(
        send(&mut osc, &show, udp(), during, &msg("/go", vec![]))
            .inputs
            .is_empty()
    );
    let after = T0 + LOCKOUT;
    let out = send(
        &mut osc,
        &show,
        udp(),
        after,
        &msg("/connect", vec![s("s3cret")]),
    );
    assert_eq!(reply_json(&out.replies[0])["status"], "ok");
}

#[test]
fn a_wrong_passcode_never_drops_a_connected_peer_and_reset_forgets_everyone() {
    let show = Show::with_passcode("s3cret");
    let mut osc = Osc::new();
    let console = udp();
    send(
        &mut osc,
        &show,
        console,
        T0,
        &msg("/connect", vec![s("s3cret")]),
    );
    send(
        &mut osc,
        &show,
        console,
        T0,
        &msg("/updates", vec![OscType::Int(1)]),
    );
    // Many bad /connects from the same address (a forger, or someone behind the same NAT):
    // the console stays connected and subscribed, and is not locked out.
    for _ in 0..MAX_BAD_PASSCODES * 3 {
        let out = send(
            &mut osc,
            &show,
            console,
            T0,
            &msg("/connect", vec![s("bad")]),
        );
        assert_eq!(reply_json(&out.replies[0])["data"], "badpass");
        let out = send(&mut osc, &show, console, T0, &msg("/go", vec![]));
        assert_eq!(out.inputs.len(), 1);
    }
    assert_eq!(osc.update_subscribers(), vec![console]);
    // And it can still re-send the right passcode.
    let out = send(
        &mut osc,
        &show,
        console,
        T0,
        &msg("/connect", vec![s("s3cret")]),
    );
    assert_eq!(reply_json(&out.replies[0])["status"], "ok");
    osc.reset_sessions();
    assert!(
        send(&mut osc, &show, udp(), T0, &msg("/go", vec![]))
            .inputs
            .is_empty()
    );
}

// ---- updates

#[test]
fn updates_toggle_the_subscription() {
    let show = Show::open();
    let mut osc = Osc::new();
    let tcp = tcp_peer(5);
    let out = send(
        &mut osc,
        &show,
        tcp,
        T0,
        &msg("/updates", vec![OscType::Int(1)]),
    );
    assert_eq!(reply_json(&out.replies[0])["status"], "ok");
    assert_eq!(osc.update_subscribers(), vec![tcp]);
    send(
        &mut osc,
        &show,
        udp(),
        T0,
        &msg("/updates", vec![OscType::Bool(true)]),
    );
    let mut subs = osc.update_subscribers();
    subs.sort_by_key(|p| matches!(p, Peer::Tcp { .. }));
    assert_eq!(subs, vec![udp(), tcp]);
    send(
        &mut osc,
        &show,
        tcp,
        T0,
        &msg("/updates", vec![OscType::Int(0)]),
    );
    assert_eq!(osc.update_subscribers(), vec![udp()]);
    // Unsubscribing a peer that never subscribed creates nothing.
    let stranger = tcp_peer(77);
    let out = send(
        &mut osc,
        &show,
        stranger,
        T0,
        &msg("/updates", vec![OscType::Int(0)]),
    );
    assert_eq!(reply_json(&out.replies[0])["status"], "ok");
    assert_eq!(osc.update_subscribers(), vec![udp()]);
    // Closing a connection ends its subscription.
    send(
        &mut osc,
        &show,
        tcp,
        T0,
        &msg("/updates", vec![OscType::Int(1)]),
    );
    osc.disconnect(tcp);
    assert_eq!(osc.update_subscribers(), vec![udp()]);
}

#[test]
fn bad_updates_arguments_are_errors() {
    let show = Show::open();
    let mut osc = Osc::new();
    for args in [
        vec![],
        vec![OscType::Int(2)],
        vec![s("1")],
        vec![OscType::Int(1), OscType::Int(1)],
    ] {
        let out = send(&mut osc, &show, udp(), T0, &msg("/updates", args));
        assert_eq!(reply_json(&out.replies[0])["status"], "error");
    }
    assert!(osc.update_subscribers().is_empty());
}

#[test]
fn a_silent_udp_peer_stops_getting_updates_but_a_tcp_one_does_not() {
    let show = Show::open();
    let mut osc = Osc::new();
    let tcp = tcp_peer(1);
    send(
        &mut osc,
        &show,
        udp(),
        T0,
        &msg("/updates", vec![OscType::Int(1)]),
    );
    send(
        &mut osc,
        &show,
        tcp,
        T0,
        &msg("/updates", vec![OscType::Int(1)]),
    );
    // Some other peer's traffic, long after, triggers the sweep.
    let later = T0 + UDP_IDLE + Duration::from_secs(11);
    let other = Peer::Udp(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 9)));
    send(&mut osc, &show, other, later, &msg("/go", vec![]));
    assert_eq!(osc.update_subscribers(), vec![tcp]);
}

#[test]
fn a_udp_peer_that_keeps_talking_is_not_expired() {
    let show = Show::open();
    let mut osc = Osc::new();
    send(
        &mut osc,
        &show,
        udp(),
        T0,
        &msg("/updates", vec![OscType::Int(1)]),
    );
    let mut now = T0;
    for _ in 0..5 {
        now += UDP_IDLE / 2;
        send(&mut osc, &show, udp(), now, &msg("/go", vec![]));
    }
    assert_eq!(osc.update_subscribers(), vec![udp()]);
}

#[test]
fn a_flood_of_spoofed_subscribers_cannot_evict_a_subscribed_console() {
    let show = Show::open();
    let mut osc = Osc::new();
    let sub = [OscType::Int(1)];
    // The console subscribes first, so it is the longest silent peer.
    let console = Peer::Udp(ip_n(0));
    send(&mut osc, &show, console, T0, &msg("/updates", sub.to_vec()));
    for i in 1..MAX_PEERS {
        let now = T0 + Duration::from_millis(u64::try_from(i).unwrap());
        send(
            &mut osc,
            &show,
            Peer::Udp(ip_n(i)),
            now,
            &msg("/updates", sub.to_vec()),
        );
    }
    assert_eq!(osc.update_subscribers().len(), MAX_PEERS);
    // More spoofed sources are refused, not let in at the console's expense.
    let later = T0 + Duration::from_secs(5);
    for i in MAX_PEERS..MAX_PEERS + 50 {
        let out = send(
            &mut osc,
            &show,
            Peer::Udp(ip_n(i)),
            later,
            &msg("/updates", sub.to_vec()),
        );
        assert_eq!(reply_json(&out.replies[0])["status"], "error");
    }
    let subs = osc.update_subscribers();
    assert_eq!(subs.len(), MAX_PEERS);
    assert!(subs.contains(&console));
    // The console keeps talking, so it outlives the silent flood; then there is room again.
    let mut now = later;
    for _ in 0..3 {
        now += UDP_IDLE / 2;
        send(&mut osc, &show, console, now, &msg("/go", vec![]));
    }
    let out = send(
        &mut osc,
        &show,
        Peer::Udp(ip_n(MAX_PEERS + 100)),
        now,
        &msg("/updates", sub.to_vec()),
    );
    assert_eq!(reply_json(&out.replies[0])["status"], "ok");
    assert!(osc.update_subscribers().contains(&console));
}

#[test]
fn wrong_passcodes_from_one_ip_lock_it_out_even_over_new_tcp_connections() {
    let show = Show::with_passcode("s3cret");
    let mut osc = Osc::new();
    for conn in 0..u64::from(MAX_BAD_PASSCODES) {
        let out = send(
            &mut osc,
            &show,
            tcp_at(conn, 5),
            T0,
            &msg("/connect", vec![s("x")]),
        );
        assert_eq!(reply_json(&out.replies[0])["data"], "badpass");
    }
    // A fresh connection from the same machine is still locked out, even with the right code.
    let out = send(
        &mut osc,
        &show,
        tcp_at(100, 5),
        T0,
        &msg("/connect", vec![s("s3cret")]),
    );
    assert_eq!(reply_json(&out.replies[0])["status"], "denied");
    // Another machine is not.
    let out = send(
        &mut osc,
        &show,
        tcp_at(101, 6),
        T0,
        &msg("/connect", vec![s("s3cret")]),
    );
    assert_eq!(reply_json(&out.replies[0])["status"], "ok");
    // After the lockout the first machine may try again.
    let out = send(
        &mut osc,
        &show,
        tcp_at(102, 5),
        T0 + LOCKOUT,
        &msg("/connect", vec![s("s3cret")]),
    );
    assert_eq!(reply_json(&out.replies[0])["status"], "ok");
}

#[test]
fn rotating_source_addresses_hits_the_global_failure_budget() {
    let show = Show::with_passcode("s3cret");
    let mut osc = Osc::new();
    let console = udp();
    send(
        &mut osc,
        &show,
        console,
        T0,
        &msg("/connect", vec![s("s3cret")]),
    );
    // One wrong guess from each of many addresses: none is locked out by itself.
    for i in 0..GLOBAL_MAX_FAILURES as usize {
        let out = send(
            &mut osc,
            &show,
            Peer::Udp(ip_n(i + 10)),
            T0,
            &msg("/connect", vec![s("x")]),
        );
        assert_eq!(reply_json(&out.replies[0])["data"], "badpass", "guess {i}");
    }
    // Now every new /connect is refused, even a right one from a fresh address...
    let fresh = Peer::Udp(ip_n(999));
    let out = send(
        &mut osc,
        &show,
        fresh,
        T0,
        &msg("/connect", vec![s("s3cret")]),
    );
    assert_eq!(reply_json(&out.replies[0])["status"], "denied");
    // ...but the connected console is unaffected,
    assert_eq!(
        send(&mut osc, &show, console, T0, &msg("/go", vec![]))
            .inputs
            .len(),
        1
    );
    // and when the window has passed new connections work again.
    let out = send(
        &mut osc,
        &show,
        fresh,
        T0 + GLOBAL_WINDOW,
        &msg("/connect", vec![s("s3cret")]),
    );
    assert_eq!(reply_json(&out.replies[0])["status"], "ok");
}

#[test]
fn passcodes_of_other_lengths_do_not_match() {
    let show = Show::with_passcode("s3cret");
    let mut osc = Osc::new();
    for wrong in ["s3cre", "s3crets", "", "S3CRET"] {
        let out = send(&mut osc, &show, udp(), T0, &msg("/connect", vec![s(wrong)]));
        assert_eq!(reply_json(&out.replies[0])["data"], "badpass", "{wrong:?}");
    }
}

#[test]
fn replies_per_call_are_capped() {
    let show = Show::open();
    let mut osc = Osc::new();
    let read = bytes(&msg("/cue/1/name", vec![]));
    let packet = bundle(&vec![read; MAX_REPLIES_PER_CALL + 20]);
    let out = osc.handle_datagram(udp(), T0, &packet, &show);
    assert_eq!(out.replies.len(), MAX_REPLIES_PER_CALL);
    assert_eq!(osc.stats().replies_dropped, 20);
    // The same over a stream read of many small frames (a TCP peer is not byte-budgeted).
    let mut data = Vec::new();
    for _ in 0..MAX_REPLIES_PER_CALL + 5 {
        data.extend(slip_encode(&bytes(&msg("/cue/1/name", vec![]))));
    }
    let mut decoder = SlipDecoder::default();
    let out = osc.handle_stream(tcp_peer(1), T0, &mut decoder, &data, &show);
    assert_eq!(out.replies.len(), MAX_REPLIES_PER_CALL);
    assert_eq!(osc.stats().replies_dropped, 25);
}

#[test]
fn an_oversize_reply_becomes_an_error_reply() {
    let mut show = Show::open();
    show.props
        .insert((CUE_1, "notes"), json!("n".repeat(MAX_REPLY_JSON_BYTES)));
    let mut osc = Osc::new();
    let out = send(&mut osc, &show, udp(), T0, &msg("/cue/1/notes", vec![]));
    let [reply] = out.replies.as_slice() else {
        panic!("{out:?}")
    };
    let body = reply_json(reply);
    assert_eq!(body["status"], "error");
    assert_eq!(body["data"], "reply too large");
    let OscType::String(text) = &reply.args[0] else {
        panic!()
    };
    assert!(text.len() <= MAX_REPLY_JSON_BYTES);
    // A note that fits is sent whole.
    show.props.insert((CUE_1, "notes"), json!("n".repeat(500)));
    let out = send(&mut osc, &show, udp(), T0, &msg("/cue/1/notes", vec![]));
    assert_eq!(reply_json(&out.replies[0])["status"], "ok");
}

#[test]
fn a_udp_source_has_a_reply_byte_budget_that_refills() {
    let mut show = Show::open();
    show.props.insert((CUE_1, "notes"), json!("n".repeat(1000)));
    let mut osc = Osc::new();
    let victim = udp();
    let mut sent = 0usize;
    let mut asks = 0;
    while sent < 200 {
        let out = send(&mut osc, &show, victim, T0, &msg("/cue/1/notes", vec![]));
        sent += out.replies.len();
        asks += 1;
        if out.replies.is_empty() {
            break;
        }
        assert!(asks < 100, "the budget never ran out");
    }
    // About 16 KiB of ~1.1 KB replies, then nothing.
    assert!((10..=16).contains(&sent), "{sent} replies sent");
    assert!(osc.stats().replies_throttled >= 1);
    // Another source has its own budget.
    let other = Peer::Udp(ip_n(77));
    assert_eq!(
        send(&mut osc, &show, other, T0, &msg("/cue/1/notes", vec![]))
            .replies
            .len(),
        1
    );
    // Time refills it.
    let later = T0 + Duration::from_secs(2);
    assert_eq!(
        send(&mut osc, &show, victim, later, &msg("/cue/1/notes", vec![]))
            .replies
            .len(),
        1
    );
}

#[test]
fn a_cue_trigger_may_contain_pattern_characters() {
    let show = Show::open();
    let mut osc = Osc::new();
    assert_eq!(
        one_input(&mut osc, &show, "/fx/all*", vec![]),
        transport(TransportAction::Start { cue: TRIGGERED })
    );
    // An address with pattern characters that is no trigger is still refused.
    let out = send(&mut osc, &show, udp(), T0, &msg("/fx/some*", vec![]));
    assert_eq!(reply_json(&out.replies[0])["status"], "error");
}

#[test]
fn a_table_full_of_tcp_peers_refuses_a_new_peer_politely() {
    let show = Show::open();
    let mut osc = Osc::new();
    for i in 0..MAX_PEERS {
        send(
            &mut osc,
            &show,
            tcp_peer(i as u64),
            T0,
            &msg("/updates", vec![OscType::Int(1)]),
        );
    }
    let out = send(
        &mut osc,
        &show,
        udp(),
        T0,
        &msg("/updates", vec![OscType::Int(1)]),
    );
    assert_eq!(reply_json(&out.replies[0])["status"], "error");
    let guarded = Show::with_passcode("x");
    let out = send(
        &mut osc,
        &guarded,
        udp(),
        T0,
        &msg("/connect", vec![s("x")]),
    );
    assert_eq!(reply_json(&out.replies[0])["status"], "error");
    assert_eq!(osc.update_subscribers().len(), MAX_PEERS);
}

// ---- SLIP

fn feed(decoder: &mut SlipDecoder, data: &[u8]) -> Vec<SlipEvent> {
    let mut events = Vec::new();
    decoder.feed(data, &mut events);
    events
}

#[test]
fn slip_frames_split_at_every_byte_still_decode() {
    let payload = bytes(&msg("/cue/1/start", vec![]));
    let framed = slip_encode(&payload);
    let mut decoder = SlipDecoder::default();
    let mut frames = Vec::new();
    for byte in &framed {
        frames.extend(feed(&mut decoder, std::slice::from_ref(byte)));
    }
    assert_eq!(frames, vec![SlipEvent::Frame(payload)]);
}

#[test]
fn slip_escapes_end_and_esc_bytes_both_ways() {
    let payload = vec![1, 0xC0, 2, 0xDB, 3, 0xC0, 0xDB, 0xDC, 0xDD];
    let framed = slip_encode(&payload);
    assert_eq!(framed.first(), Some(&0xC0));
    assert_eq!(framed.last(), Some(&0xC0));
    assert!(!framed[1..framed.len() - 1].contains(&0xC0));
    let mut decoder = SlipDecoder::default();
    assert_eq!(feed(&mut decoder, &framed), vec![SlipEvent::Frame(payload)]);
}

#[test]
fn slip_handles_several_frames_in_a_read_and_ignores_empty_ones() {
    let mut data = slip_encode(b"abcd");
    data.extend(slip_encode(b"efgh"));
    data.extend([0xC0, 0xC0, 0xC0]);
    let mut decoder = SlipDecoder::default();
    assert_eq!(
        feed(&mut decoder, &data),
        vec![
            SlipEvent::Frame(b"abcd".to_vec()),
            SlipEvent::Frame(b"efgh".to_vec())
        ]
    );
}

#[test]
fn an_oversize_slip_frame_is_rejected_once_and_the_next_frame_decodes() {
    let mut decoder = SlipDecoder::new(16);
    let mut data = vec![0xC0];
    data.extend(std::iter::repeat_n(7u8, 1000));
    data.push(0xC0);
    data.extend(slip_encode(b"fine"));
    let events = feed(&mut decoder, &data);
    assert_eq!(
        events,
        vec![SlipEvent::Oversize, SlipEvent::Frame(b"fine".to_vec())]
    );
    // Exactly at the limit is fine.
    let mut decoder = SlipDecoder::new(16);
    assert_eq!(
        feed(&mut decoder, &slip_encode(&[9u8; 16])),
        vec![SlipEvent::Frame(vec![9u8; 16])]
    );
    // The report comes as soon as the limit is passed, even if the frame never ends.
    let mut decoder = SlipDecoder::new(16);
    assert_eq!(feed(&mut decoder, &[5u8; 17]), vec![SlipEvent::Oversize]);
    assert_eq!(feed(&mut decoder, &[5u8; 100_000]), vec![]);
}

#[test]
fn a_bad_slip_escape_drops_only_its_frame() {
    let mut decoder = SlipDecoder::default();
    let mut data = vec![0xC0, 1, 0xDB, 0x00, 2, 0xC0];
    data.extend([0xDB, 0xDB, 0xC0]);
    data.extend([3, 0xDB, 0xC0]);
    data.extend(slip_encode(b"ok"));
    assert_eq!(
        feed(&mut decoder, &data),
        vec![
            SlipEvent::BadEscape,
            SlipEvent::BadEscape,
            SlipEvent::BadEscape,
            SlipEvent::Frame(b"ok".to_vec())
        ]
    );
}

#[test]
fn a_tcp_stream_dispatches_and_counts_oversize_and_garbage() {
    let show = Show::open();
    let mut osc = Osc::new();
    let mut decoder = SlipDecoder::new(MAX_PACKET_BYTES);
    let tcp = tcp_peer(1);
    let mut data = slip_encode(&bytes(&msg("/go", vec![])));
    data.extend(slip_encode(&bytes(&msg("/cue/1/name", vec![]))));
    data.extend(slip_encode(b"this is not osc"));
    data.push(0xC0);
    data.extend(std::iter::repeat_n(1u8, MAX_PACKET_BYTES + 10));
    data.push(0xC0);
    data.extend(slip_encode(&bytes(&msg("/panic", vec![]))));
    let mut inputs = Vec::new();
    let mut replies = Vec::new();
    for chunk in data.chunks(1000) {
        let out = osc.handle_stream(tcp, T0, &mut decoder, chunk, &show);
        inputs.extend(out.inputs);
        replies.extend(out.replies);
    }
    assert_eq!(
        inputs,
        vec![
            transport(TransportAction::Go { list: LIST }),
            transport(TransportAction::Panic)
        ]
    );
    assert_eq!(replies.len(), 1);
    assert_eq!(osc.stats().malformed, 1);
    assert_eq!(osc.stats().oversize_frames, 1);
    assert_eq!(osc.stats().packets, 4);
}

// ---- bundles, malformed packets

fn bundle(elements: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"#bundle\0".to_vec();
    out.extend([0, 0, 0, 0, 0, 0, 0, 1]);
    for element in elements {
        out.extend(u32::try_from(element.len()).unwrap().to_be_bytes());
        out.extend(element);
    }
    out
}

#[test]
fn a_bundle_is_flattened_in_order_and_time_tags_are_ignored() {
    let show = Show::open();
    let mut osc = Osc::new();
    let inner = bundle(&[bytes(&msg("/pause", vec![]))]);
    let packet = bundle(&[
        bytes(&msg("/go", vec![])),
        inner,
        bytes(&msg("/panic", vec![])),
    ]);
    let out = osc.handle_datagram(udp(), T0, &packet, &show);
    assert_eq!(
        out.inputs,
        vec![
            transport(TransportAction::Go { list: LIST }),
            transport(TransportAction::PauseAll),
            transport(TransportAction::Panic)
        ]
    );
    assert_eq!(osc.stats().messages, 3);
}

#[test]
fn bundles_nest_up_to_the_limit_and_no_further() {
    let mut packet = bytes(&msg("/go", vec![]));
    for depth in 1..=MAX_BUNDLE_DEPTH + 3 {
        packet = bundle(&[packet]);
        let decoded = decode_packet(&packet);
        if depth <= MAX_BUNDLE_DEPTH {
            assert_eq!(decoded.unwrap().len(), 1, "depth {depth}");
        } else {
            assert_eq!(decoded, Err(DecodeError::BundleTooDeep), "depth {depth}");
        }
    }
}

#[test]
fn a_huge_nesting_of_bundles_is_refused_without_overflowing_the_stack() {
    let mut packet = bytes(&msg("/go", vec![]));
    while packet.len() < MAX_PACKET_BYTES - 40 {
        packet = bundle(&[packet]);
    }
    assert_eq!(decode_packet(&packet), Err(DecodeError::BundleTooDeep));
    let mut osc = Osc::new();
    let out = osc.handle_datagram(udp(), T0, &packet, &Show::open());
    assert_eq!(out, Outcome::default());
    assert_eq!(osc.stats().malformed, 1);
}

#[test]
fn too_many_messages_in_one_packet_are_refused_whole() {
    let one = bytes(&msg("/go", vec![]));
    let ok = bundle(&vec![one.clone(); MAX_MESSAGES_PER_PACKET]);
    assert_eq!(decode_packet(&ok).unwrap().len(), MAX_MESSAGES_PER_PACKET);
    let too_many = bundle(&vec![one; MAX_MESSAGES_PER_PACKET + 1]);
    assert_eq!(decode_packet(&too_many), Err(DecodeError::TooManyMessages));
    let mut osc = Osc::new();
    let out = osc.handle_datagram(udp(), T0, &too_many, &Show::open());
    assert!(out.inputs.is_empty());
    assert_eq!(osc.stats().malformed, 1);
}

#[test]
fn malformed_bundles_are_errors() {
    let one = bytes(&msg("/go", vec![]));
    let mut cases: Vec<Vec<u8>> = vec![
        b"#bundle\0".to_vec(),
        b"#bundle\0\0\0\0\0\0\0\0".to_vec(), // header cut short
        bundle(&[vec![0xAB, 0xCD, 0x01, 0x02]]), // an element that is no message
        bundle(&[vec![]]),
    ];
    // An element longer than the bundle.
    let mut long = bundle(std::slice::from_ref(&one));
    long[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
    cases.push(long);
    // A trailing size with no element.
    let mut cut = bundle(&[one]);
    cut.extend([0, 0, 0, 8]);
    cases.push(cut);
    for case in cases {
        assert!(
            matches!(
                decode_packet(&case),
                Err(DecodeError::BadBundle(_) | DecodeError::BadMessage(_))
            ),
            "{case:?}"
        );
    }
}

#[test]
fn malformed_packets_are_counted_and_dropped() {
    let show = Show::open();
    let mut osc = Osc::new();
    let mut cases: Vec<Vec<u8>> = vec![
        vec![],
        vec![0],
        b"hello".to_vec(),
        b"/go".to_vec(),                         // no terminator
        b"/go\0,i\0\0".to_vec(),                 // argument missing
        b"/go\0,s\0\0\xff\xff\xff\xff".to_vec(), // bad string
        vec![0xFF; 300],
        vec![0; MAX_PACKET_BYTES + 1],
    ];
    // A blob claiming 4 GiB.
    cases.push(b"/go\0,b\0\0\xff\xff\xff\xffxxxx".to_vec());
    // Array type tags nested too deep.
    let mut deep = b"/go\0,".to_vec();
    deep.extend(std::iter::repeat_n(b'[', MAX_ARRAY_DEPTH + 1));
    deep.extend(std::iter::repeat_n(b']', MAX_ARRAY_DEPTH + 1));
    deep.extend([0, 0, 0]);
    cases.push(deep);
    let count = cases.len() as u64;
    for case in &cases {
        let out = osc.handle_datagram(udp(), T0, case, &show);
        assert_eq!(out, Outcome::default(), "{case:?}");
    }
    assert_eq!(osc.stats().malformed, count);
    assert_eq!(osc.stats().packets, count);
    assert_eq!(osc.stats().messages, 0);
    assert_eq!(decode_packet(&[]), Err(DecodeError::Empty));
    assert!(matches!(
        decode_packet(&vec![0; MAX_PACKET_BYTES + 1]),
        Err(DecodeError::TooLarge { .. })
    ));
}

#[test]
fn a_very_deep_array_type_tag_is_refused_before_it_is_built() {
    let mut packet = b"/go\0,".to_vec();
    packet.extend(std::iter::repeat_n(b'[', 30_000));
    packet.extend(std::iter::repeat_n(b']', 30_000));
    packet.extend([0, 0, 0]);
    assert_eq!(decode_packet(&packet), Err(DecodeError::TagsTooDeep));
    // Arrays within the limit decode.
    let nested = msg(
        "/x",
        vec![OscType::Array(rosc::OscArray {
            content: vec![OscType::Array(rosc::OscArray {
                content: vec![OscType::Int(1)],
            })],
        })],
    );
    assert_eq!(decode_packet(&bytes(&nested)).unwrap(), vec![nested]);
}

// ---- properties

fn arb_arg() -> impl Strategy<Value = OscType> {
    let leaf = prop_oneof![
        any::<i32>().prop_map(OscType::Int),
        any::<i64>().prop_map(OscType::Long),
        any::<f32>()
            .prop_filter("NaN is not equal to itself", |f| !f.is_nan())
            .prop_map(OscType::Float),
        any::<f64>()
            .prop_filter("NaN is not equal to itself", |f| !f.is_nan())
            .prop_map(OscType::Double),
        "[^\0]{0,40}".prop_map(OscType::String),
        proptest::collection::vec(any::<u8>(), 0..40).prop_map(OscType::Blob),
        any::<bool>().prop_map(OscType::Bool),
        Just(OscType::Nil),
        Just(OscType::Inf),
        (any::<u32>(), any::<u32>()).prop_map(|(seconds, fractional)| OscType::Time(OscTime {
            seconds,
            fractional
        })),
    ];
    leaf.prop_recursive(2, 8, 4, |inner| {
        proptest::collection::vec(inner, 0..4)
            .prop_map(|content| OscType::Array(rosc::OscArray { content }))
    })
}

fn arb_message() -> impl Strategy<Value = OscMessage> {
    (
        "/[a-zA-Z0-9_.]{1,12}(/[a-zA-Z0-9_.]{1,12}){0,4}",
        proptest::collection::vec(arb_arg(), 0..6),
    )
        .prop_map(|(addr, args)| OscMessage { addr, args })
}

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic_as_datagrams(
        data in proptest::collection::vec(any::<u8>(), 0..2048),
        passcode in any::<bool>(),
    ) {
        let show = if passcode { Show::with_passcode("1") } else { Show::open() };
        let mut osc = Osc::new();
        let _ = osc.handle_datagram(udp(), T0, &data, &show);
        let _ = decode_packet(&data);
        prop_assert_eq!(osc.stats().packets, 1);
    }

    #[test]
    fn arbitrary_bytes_never_panic_as_a_stream(
        data in proptest::collection::vec(any::<u8>(), 0..4096),
        chunk in 1usize..64,
    ) {
        let show = Show::open();
        let mut osc = Osc::new();
        let mut decoder = SlipDecoder::new(256);
        for part in data.chunks(chunk) {
            let _ = osc.handle_stream(tcp_peer(1), T0, &mut decoder, part, &show);
        }
    }

    #[test]
    fn arbitrary_bytes_behind_a_bundle_header_never_panic(
        data in proptest::collection::vec(any::<u8>(), 0..512),
    ) {
        let mut packet = b"#bundle\0\0\0\0\0\0\0\0\x01".to_vec();
        packet.extend(data);
        let mut osc = Osc::new();
        let _ = osc.handle_datagram(udp(), T0, &packet, &Show::open());
    }

    #[test]
    fn arbitrary_addresses_never_panic(addr in "\\PC{0,300}") {
        let _ = parse_address(&addr);
    }

    #[test]
    fn arbitrary_messages_never_panic_in_the_dispatcher(
        addr in prop_oneof![
            "/(cue|cue_id|select|playhead|workspace|go|connect|updates)(/[a-zA-Z0-9.*{}?-]{0,12}){0,4}",
            "/[ -~]{0,40}",
        ],
        args in proptest::collection::vec(arb_arg(), 0..4),
    ) {
        let mut osc = Osc::new();
        let m = OscMessage { addr, args };
        if let Ok(data) = encode_message(&m) {
            let out = osc.handle_datagram(udp(), T0, &data, &Show::open());
            for reply in &out.replies {
                let [OscType::String(text)] = reply.args.as_slice() else {
                    return Err(TestCaseError::fail("reply without one string"));
                };
                prop_assert!(serde_json::from_str::<Value>(text).is_ok());
                prop_assert!(reply.addr.starts_with("/reply/"));
            }
        }
    }

    #[test]
    fn decoded_then_encoded_messages_round_trip(m in arb_message()) {
        let data = encode_message(&m).unwrap();
        prop_assert_eq!(decode_packet(&data).unwrap(), vec![m.clone()]);
        // And through SLIP, split anywhere.
        let framed = slip_encode(&data);
        let mut decoder = SlipDecoder::default();
        let mut events = Vec::new();
        let mid = framed.len() / 2;
        decoder.feed(&framed[..mid], &mut events);
        decoder.feed(&framed[mid..], &mut events);
        prop_assert_eq!(events, vec![SlipEvent::Frame(data.clone())]);
        // And as the content of a bundle.
        let b = OscPacket::Bundle(rosc::OscBundle {
            timetag: OscTime { seconds: 0, fractional: 1 },
            content: vec![OscPacket::Message(m.clone())],
        });
        let bundled = rosc::encoder::encode(&b).unwrap();
        prop_assert_eq!(decode_packet(&bundled).unwrap(), vec![m]);
    }

    #[test]
    fn slip_round_trips_any_payload_split_anywhere(
        payload in proptest::collection::vec(any::<u8>(), 1..300),
        cuts in proptest::collection::vec(any::<usize>(), 0..4),
    ) {
        let framed = slip_encode(&payload);
        let mut points: Vec<usize> = cuts.iter().map(|c| c % (framed.len() + 1)).collect();
        points.sort_unstable();
        let mut decoder = SlipDecoder::default();
        let mut events = Vec::new();
        let mut last = 0;
        for p in points {
            decoder.feed(&framed[last..p], &mut events);
            last = p;
        }
        decoder.feed(&framed[last..], &mut events);
        prop_assert_eq!(events, vec![SlipEvent::Frame(payload)]);
    }

    #[test]
    fn the_fuzz_entry_holds_its_invariants(data in proptest::collection::vec(any::<u8>(), 0..600)) {
        prop_assert_eq!(openlab_engine::osc::fuzz::fuzz_osc(&data), Ok(()));
    }
}

#[test]
fn the_fuzz_entry_handles_real_traffic() {
    let mut data = vec![1u8 | 4];
    data.extend(slip_encode(&bytes(&msg("/connect", vec![s("1234")]))));
    data.extend(slip_encode(&bytes(&msg("/cue/1/name", vec![]))));
    assert_eq!(openlab_engine::osc::fuzz::fuzz_osc(&data), Ok(()));
    assert_eq!(openlab_engine::osc::fuzz::fuzz_osc(&[]), Ok(()));
}

#[test]
fn the_fuzz_seeds_pass_the_fuzz_entry() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/osc-seeds");
    let mut count = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let data = std::fs::read(entry.unwrap().path()).unwrap();
        assert_eq!(openlab_engine::osc::fuzz::fuzz_osc(&data), Ok(()));
        count += 1;
    }
    assert!(count >= 7);
}
