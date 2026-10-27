// SPDX-License-Identifier: GPL-3.0-or-later

//! The socket-free OSC layer (M4.5 part 1): codec, SLIP, dispatch, sessions, replies.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::ops::ControlFlow;
use std::time::Duration;

use openlab_core::protocol::{ClientMessage, RequestId, TransportAction};
use openlab_core::{Command, CueId, CueListId, CueProperty, NumberAssignment, WorkspaceId};
use openlab_engine::osc::{
    CueField, CuePattern, DecodeError, GLOBAL_MAX_FAILURES, GLOBAL_WINDOW, LOCKOUT, MATCH_BUDGET,
    MAX_ARRAY_DEPTH, MAX_BAD_PASSCODES, MAX_BUNDLE_DEPTH, MAX_FRAME_BYTES, MAX_KNOWN_IPS,
    MAX_MATCH_CHARS, MAX_MATCHES, MAX_MESSAGES_PER_PACKET, MAX_PACKET_BYTES, MAX_PEERS,
    MAX_REPLIES_PER_CALL, MAX_REPLY_BYTES_PER_CALL, MAX_REPLY_JSON_BYTES, MAX_UDP_DATAGRAM_BYTES,
    Osc, OscInput, OscShow, Outcome, Peer, Reply, SlipDecoder, SlipEvent, Target, UDP_IDLE,
    decode_packet, encode_message, parse_address, reply_message, slip_encode, upper,
};
use proptest::prelude::*;
use rosc::{OscMessage, OscPacket, OscTime, OscType};
use serde_json::{Value, json};
use uuid::Uuid;

const WS: u128 = 0xAB;
const LIST: CueListId = CueListId(Uuid::from_u128(0x11));
const LIST_B: CueListId = CueListId(Uuid::from_u128(0x12));
const CUE_1: CueId = CueId(Uuid::from_u128(0x101));
const CUE_25: CueId = CueId(Uuid::from_u128(0x102));
const TRIGGERED: CueId = CueId(Uuid::from_u128(0x103));
const CUE_7: CueId = CueId(Uuid::from_u128(0x107));
const GROUP: CueId = CueId(Uuid::from_u128(0x1A0));
const CHILD: CueId = CueId(Uuid::from_u128(0x111));
const CUE_20: CueId = CueId(Uuid::from_u128(0x120));

/// Every list and cue in workspace order, with its number text: list `Main` holds cues 1, 2.5, one
/// without a number, group 10 (with child 11) and 7; list `Second` holds cue 20.
fn tree() -> Vec<(Target, &'static str)> {
    vec![
        (Target::List(LIST), ""),
        (Target::Cue(CUE_1), "1"),
        (Target::Cue(CUE_25), "2.5"),
        (Target::Cue(TRIGGERED), ""),
        (Target::Cue(GROUP), "10"),
        (Target::Cue(CHILD), "11"),
        (Target::Cue(CUE_7), "7"),
        (Target::List(LIST_B), ""),
        (Target::Cue(CUE_20), "20"),
    ]
}

struct Show {
    passcode: Option<String>,
    list: Option<CueListId>,
    props: HashMap<(CueId, &'static str), Value>,
    running: Vec<CueId>,
    paused: Vec<CueId>,
    playheads: HashMap<CueListId, CueId>,
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
            running: Vec::new(),
            paused: Vec::new(),
            playheads: HashMap::new(),
        }
    }

    fn with_passcode(code: &str) -> Self {
        Self {
            passcode: Some(code.to_owned()),
            ..Self::open()
        }
    }

    fn number_of(id: CueId) -> Option<&'static str> {
        tree()
            .into_iter()
            .find_map(|(t, n)| (t == Target::Cue(id) && !n.is_empty()).then_some(n))
    }
}

fn workspace() -> WorkspaceId {
    WorkspaceId(Uuid::from_u128(WS))
}

impl OscShow for Show {
    fn workspace_id(&self) -> WorkspaceId {
        workspace()
    }
    fn workspace_name(&self) -> &str {
        "Hamlet"
    }
    fn passcode(&self) -> Option<&str> {
        self.passcode.as_deref()
    }
    fn active_list(&self) -> Option<CueListId> {
        self.list
    }
    fn cue_by_number(&self, number: &str) -> Option<CueId> {
        tree().into_iter().find_map(|(t, n)| match t {
            Target::Cue(id) if n == number && !n.is_empty() => Some(id),
            _ => None,
        })
    }
    fn cue_exists(&self, id: CueId) -> bool {
        tree().iter().any(|(t, _)| *t == Target::Cue(id))
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
            CueField::UniqueId => "uniqueID",
            CueField::IsLoaded => "isLoaded",
            CueField::IsRunning => "isRunning",
            CueField::IsPaused => "isPaused",
            CueField::IsBroken => "isBroken",
        };
        if let Some(value) = self.props.get(&(id, key)) {
            return Some(value.clone());
        }
        match field {
            CueField::Name => Show::number_of(id).map(|n| json!(format!("Cue {n}"))),
            CueField::Number => Some(json!(Show::number_of(id).unwrap_or(""))),
            CueField::UniqueId => Some(json!(upper(id))),
            CueField::IsRunning => Some(json!(self.running.contains(&id))),
            CueField::IsPaused => Some(json!(self.paused.contains(&id))),
            _ => None,
        }
    }
    fn cue_by_osc_trigger(&self, address: &str) -> Option<CueId> {
        matches!(address, "/lights/blackout" | "/fx/all*").then_some(TRIGGERED)
    }
    fn top_level_list(&self, cue: CueId) -> Option<CueListId> {
        match cue {
            CUE_20 => Some(LIST_B),
            CHILD => None,
            id if self.cue_exists(id) => Some(LIST),
            _ => None,
        }
    }
    fn playhead(&self, list: CueListId) -> Option<CueId> {
        self.playheads.get(&list).copied()
    }
    fn list_name(&self, list: CueListId) -> Option<String> {
        match list {
            LIST => Some("Main".into()),
            LIST_B => Some("Second".into()),
            _ => None,
        }
    }
    fn running_cues(&self, include_paused: bool) -> Vec<CueId> {
        tree()
            .into_iter()
            .filter_map(|(t, _)| match t {
                Target::Cue(id)
                    if self.running.contains(&id)
                        || (include_paused && self.paused.contains(&id)) =>
                {
                    Some(id)
                }
                _ => None,
            })
            .collect()
    }
    fn visit_targets(&self, visit: &mut dyn FnMut(Target, &str) -> ControlFlow<()>) {
        for (target, number) in tree() {
            if visit(target, number).is_break() {
                return;
            }
        }
    }
    fn cue_object(&self, id: CueId) -> Option<Value> {
        self.cue_exists(id)
            .then(|| json!({"uniqueID": upper(id), "number": Show::number_of(id).unwrap_or("")}))
    }
    fn cue_lists_object(&self) -> Value {
        json!([{"uniqueID": upper(LIST.0), "type": "Cue List"}])
    }
}

/// An endpoint that does not drop repeats: most tests send the same message again and again.
fn quiet_osc() -> Osc {
    let mut osc = Osc::new();
    osc.set_duplicate_window(Duration::ZERO);
    osc
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
    let mut osc = quiet_osc();
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
    let mut osc = quiet_osc();
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
    let mut osc = quiet_osc();
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
    let mut osc = quiet_osc();
    let out = send(&mut osc, &show, udp(), T0, &msg("/cue/1/name", vec![]));
    assert!(out.inputs.is_empty());
    let [reply] = out.replies.as_slice() else {
        panic!("{out:?}")
    };
    assert_eq!(reply.addr, "/reply/cue/1/name");
    // As QLab writes it: the workspace id in upper case, the address in its full form.
    assert_eq!(
        reply_json(reply),
        json!({
            "workspace_id": upper(workspace()),
            "address": format!("/workspace/{}/cue/1/name", upper(workspace())),
            "status": "ok",
            "data": "Opening",
        })
    );
    let out = send(&mut osc, &show, udp(), T0, &msg("/cue/1/preWait", vec![]));
    assert_eq!(reply_json(&out.replies[0])["data"], json!(1.5));
    let out = send(&mut osc, &show, udp(), T0, &msg("/cue/1/armed", vec![]));
    assert_eq!(reply_json(&out.replies[0])["data"], json!(true));
    // A cue without that property: an error with no data.
    let out = send(&mut osc, &show, udp(), T0, &msg("/cue/2.5/notes", vec![]));
    assert_eq!(
        reply_json(&out.replies[0]),
        json!({
            "workspace_id": upper(workspace()),
            "address": format!("/workspace/{}/cue/2.5/notes", upper(workspace())),
            "status": "error",
        })
    );
}

#[test]
fn unknown_cues_and_missing_list_are_error_replies() {
    let mut show = Show::open();
    let mut osc = quiet_osc();
    for addr in [
        "/cue/99/start",
        "/cue_id/00000000-0000-0000-0000-0000000fffff/start",
        "/cue_id/not-a-uuid/start",
        // Lower case is not a cue id; QLab's are upper case.
        "/cue_id/00000000-0000-0000-0000-0000000001a0/start",
        // Patterns that match nothing.
        "/cue/zzz*/start",
        "/cue/[a-c]/start",
        "/cue/{x,y}/start",
        // No such property on the pseudo-cues, and no action on them.
        "/cue/active/name",
        "/cue/selected/name",
        "/cue/playhead/start",
    ] {
        let out = send(&mut osc, &show, udp(), T0, &msg(addr, vec![]));
        assert!(out.inputs.is_empty(), "{addr}");
        let [reply] = out.replies.as_slice() else {
            panic!("{addr}: {out:?}")
        };
        assert_eq!(reply_json(reply)["status"], "error", "{addr}");
        assert_eq!(
            reply_json(reply)["address"],
            format!("/workspace/{}{addr}", upper(workspace()))
        );
        assert!(reply_json(reply).get("data").is_none(), "{addr}");
    }
    show.list = None;
    for addr in ["/go", "/playhead/next", "/select/previous"] {
        let out = send(&mut osc, &show, udp(), T0, &msg(addr, vec![]));
        assert!(out.inputs.is_empty(), "{addr}");
        assert_eq!(reply_json(&out.replies[0])["status"], "error", "{addr}");
    }
}

#[test]
fn unknown_addresses_get_an_error_reply_and_unusable_ones_are_dropped_and_counted() {
    let show = Show::open();
    let mut osc = quiet_osc();
    // Wrongly cased ones too: addresses are case-sensitive.
    for addr in [
        "/nothing/here",
        "/cue/1/unknown",
        "/cue",
        "/workspace/x",
        "/GO",
        "/Cue/1/start",
        "/cue/1/NAME",
        "/cue/1/prewait",
        "/VERSION",
        "/hardPanic",
    ] {
        let out = send(&mut osc, &show, udp(), T0, &msg(addr, vec![]));
        assert!(out.inputs.is_empty(), "{addr}");
        let [reply] = out.replies.as_slice() else {
            panic!("{addr}: {out:?}")
        };
        assert_eq!(
            reply_json(reply),
            json!({
                "workspace_id": upper(workspace()),
                "address": format!("/workspace/{}{addr}", upper(workspace())),
                "status": "error",
            }),
            "{addr}"
        );
        assert_eq!(reply.addr, format!("/reply{addr}"));
    }
    assert_eq!(osc.stats().unknown_address, 10);
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
    let mut osc = quiet_osc();
    let id = upper(workspace());
    assert_eq!(
        one_input(&mut osc, &show, &format!("/workspace/{id}/go"), vec![]),
        transport(TransportAction::Go { list: LIST })
    );
    assert_eq!(
        one_input(
            &mut osc,
            &show,
            &format!("/workspace/{id}/cue/1/start"),
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
fn a_wrong_or_lower_case_workspace_id_is_an_error_with_only_address_and_status() {
    let show = Show::open();
    let mut osc = quiet_osc();
    let lower = format!("/workspace/{}/cue/1/name", workspace());
    for addr in [
        "/workspace/00000000-0000-0000-0000-000000000001/go",
        "/workspace/00000000-0000-0000-0000-000000000001/connect",
        "/workspace/nonsense/cue/1/start",
        // The right id in lower case is not the right id.
        lower.as_str(),
    ] {
        let out = send(&mut osc, &show, udp(), T0, &msg(addr, vec![]));
        assert!(out.inputs.is_empty(), "{addr}");
        let [reply] = out.replies.as_slice() else {
            panic!("{addr}: {out:?}")
        };
        assert_eq!(reply.addr, format!("/reply{addr}"));
        assert_eq!(
            reply_json(reply),
            json!({"address": addr, "status": "error"}),
            "{addr}"
        );
    }
    assert_eq!(osc.stats().wrong_workspace, 4);
    // The same before a passcode is given, and over TCP.
    let guarded = Show::with_passcode("x");
    let out = send(
        &mut osc,
        &guarded,
        tcp_peer(1),
        T0,
        &msg("/workspace/nonsense/go", vec![]),
    );
    assert_eq!(reply_json(&out.replies[0])["status"], "error");
    // Application-level queries are not available under the prefix.
    let under = format!("/workspace/{}/version", upper(workspace()));
    let out = send(&mut osc, &show, udp(), T0, &msg(&under, vec![]));
    assert_eq!(reply_json(&out.replies[0])["status"], "error");
    assert_eq!(
        reply_json(&out.replies[0])["workspace_id"],
        upper(workspace())
    );
}

// ---- passcode

#[test]
fn without_a_passcode_nothing_needs_connect_and_connect_is_accepted() {
    let show = Show::open();
    let mut osc = quiet_osc();
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
        (Some("ok"), Some("ok:view|edit|control"))
    );
    assert_eq!(out.replies[0].addr, "/reply/connect");
    // An empty passcode is no passcode.
    let mut show = Show::open();
    show.passcode = Some(String::new());
    assert_eq!(
        send(&mut quiet_osc(), &show, udp(), T0, &msg("/go", vec![]))
            .inputs
            .len(),
        1
    );
}

#[test]
fn with_a_passcode_everything_but_connect_is_denied_until_connected() {
    let show = Show::with_passcode("s3cret");
    let mut osc = quiet_osc();
    let tcp = tcp_peer(1);
    // TCP and UDP peers are both told, as QLab does (a UDP reply is held to the byte budget of
    // peers that may not ask yet).
    let out = send(&mut osc, &show, tcp, T0, &msg("/go", vec![]));
    assert!(out.inputs.is_empty());
    assert_eq!(
        reply_json(&out.replies[0]),
        json!({
            "address": format!("/workspace/{}/go", upper(workspace())),
            "status": "denied",
            "workspace_id": upper(workspace()),
        })
    );
    let out = send(&mut osc, &show, udp(), T0, &msg("/go", vec![]));
    assert!(out.inputs.is_empty());
    assert_eq!(out.replies[0].addr, "/reply/go");
    assert_eq!(reply_json(&out.replies[0])["status"], "denied");
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
    // An application-level message: no workspace id in the reply.
    assert_eq!(
        reply_json(&out.replies[0]),
        json!({"address": "/updates", "status": "denied"})
    );
    assert!(osc.update_subscribers(T0).is_empty());
}

#[test]
fn the_right_passcode_connects_and_the_wrong_one_does_not() {
    let show = Show::with_passcode("s3cret");
    let mut osc = quiet_osc();
    let out = send(
        &mut osc,
        &show,
        udp(),
        T0,
        &msg("/connect", vec![s("nope")]),
    );
    let reply = reply_json(&out.replies[0]);
    // QLab: a wrong passcode is an `ok` status with the data `badpass`.
    assert_eq!(
        (reply["status"].as_str(), reply["data"].as_str()),
        (Some("ok"), Some("badpass"))
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
    assert_eq!(reply_json(&out.replies[0])["data"], "ok:view|edit|control");
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
    let asked = format!("/workspace/{}/connect", upper(workspace()));
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
    let mut osc = quiet_osc();
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
    let mut osc = quiet_osc();
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
    assert_eq!(osc.update_subscribers(T0), vec![console]);
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
    let mut osc = quiet_osc();
    let tcp = tcp_peer(5);
    // QLab sends no reply when `/updates` is allowed.
    let out = send(
        &mut osc,
        &show,
        tcp,
        T0,
        &msg("/updates", vec![OscType::Int(1)]),
    );
    assert!(out.replies.is_empty());
    assert_eq!(osc.update_subscribers(T0), vec![tcp]);
    send(
        &mut osc,
        &show,
        udp(),
        T0,
        &msg("/updates", vec![OscType::Bool(true)]),
    );
    let mut subs = osc.update_subscribers(T0);
    subs.sort_by_key(|p| matches!(p, Peer::Tcp { .. }));
    assert_eq!(subs, vec![udp(), tcp]);
    send(
        &mut osc,
        &show,
        tcp,
        T0,
        &msg("/updates", vec![OscType::Int(0)]),
    );
    assert_eq!(osc.update_subscribers(T0), vec![udp()]);
    // Unsubscribing a peer that never subscribed creates nothing.
    let stranger = tcp_peer(77);
    let out = send(
        &mut osc,
        &show,
        stranger,
        T0,
        &msg("/updates", vec![OscType::Int(0)]),
    );
    assert!(out.replies.is_empty());
    assert_eq!(osc.update_subscribers(T0), vec![udp()]);
    // Closing a connection ends its subscription.
    send(
        &mut osc,
        &show,
        tcp,
        T0,
        &msg("/updates", vec![OscType::Int(1)]),
    );
    osc.disconnect(tcp);
    assert_eq!(osc.update_subscribers(T0), vec![udp()]);
}

#[test]
fn bad_updates_arguments_are_errors() {
    let show = Show::open();
    let mut osc = quiet_osc();
    for args in [
        vec![],
        vec![OscType::Int(2)],
        vec![s("1")],
        vec![OscType::Int(1), OscType::Int(1)],
    ] {
        let out = send(&mut osc, &show, udp(), T0, &msg("/updates", args));
        assert_eq!(
            reply_json(&out.replies[0]),
            json!({"address": "/updates", "status": "error"})
        );
    }
    assert!(osc.update_subscribers(T0).is_empty());
}

#[test]
fn a_silent_udp_peer_stops_getting_updates_but_a_tcp_one_does_not() {
    let show = Show::open();
    let mut osc = quiet_osc();
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
    assert_eq!(osc.update_subscribers(T0), vec![tcp]);
}

#[test]
fn a_udp_peer_that_keeps_talking_is_not_expired() {
    let show = Show::open();
    let mut osc = quiet_osc();
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
    assert_eq!(osc.update_subscribers(T0), vec![udp()]);
}

#[test]
fn a_flood_of_spoofed_subscribers_cannot_evict_a_subscribed_console() {
    let show = Show::open();
    let mut osc = quiet_osc();
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
    assert_eq!(osc.update_subscribers(T0).len(), MAX_PEERS);
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
    let subs = osc.update_subscribers(T0);
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
    assert!(out.replies.is_empty());
    assert!(osc.update_subscribers(now).contains(&console));
}

#[test]
fn wrong_passcodes_from_one_ip_lock_it_out_even_over_new_tcp_connections() {
    let show = Show::with_passcode("s3cret");
    let mut osc = quiet_osc();
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
    let mut osc = quiet_osc();
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

/// What `/connect` said: `denied`, `badpass` or `ok` (the rights granted are checked elsewhere).
fn connect_status(osc: &mut Osc, show: &Show, peer: Peer, now: Duration, code: &str) -> String {
    let out = send(osc, show, peer, now, &msg("/connect", vec![s(code)]));
    let body = reply_json(&out.replies[0]);
    match (body["status"].as_str().unwrap(), body["data"].as_str()) {
        ("denied", _) => "denied".to_owned(),
        ("ok", Some("badpass")) => "badpass".to_owned(),
        ("ok", Some(data)) if data.starts_with("ok:") => "ok".to_owned(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_known_address_reconnects_with_the_right_passcode_through_both_lockouts() {
    let show = Show::with_passcode("s3cret");
    let mut osc = quiet_osc();
    let operator = Peer::Udp(ip_n(1));
    assert_eq!(
        connect_status(&mut osc, &show, operator, T0, "s3cret"),
        "ok"
    );
    // The operator's session expires.
    let later = T0 + UDP_IDLE + Duration::from_secs(30);
    send(
        &mut osc,
        &show,
        Peer::Udp(ip_n(2)),
        later,
        &msg("/go", vec![]),
    );
    assert!(
        send(&mut osc, &show, operator, later, &msg("/go", vec![]))
            .inputs
            .is_empty()
    );
    // Forged wrong passcodes from the operator's own address lock that address out...
    for _ in 0..MAX_BAD_PASSCODES {
        assert_eq!(
            connect_status(&mut osc, &show, operator, later, "x"),
            "badpass"
        );
    }
    // ...and, spread over many addresses, exhaust the global budget.
    for i in 0..GLOBAL_MAX_FAILURES as usize {
        connect_status(&mut osc, &show, Peer::Udp(ip_n(i + 100)), later, "x");
    }
    // A wrong passcode from the known address is still refused (and not counted further).
    assert_eq!(
        connect_status(&mut osc, &show, operator, later, "x"),
        "denied"
    );
    // The right one gets through both.
    assert_eq!(
        connect_status(&mut osc, &show, operator, later, "s3cret"),
        "ok"
    );
    assert_eq!(
        send(&mut osc, &show, operator, later, &msg("/go", vec![]))
            .inputs
            .len(),
        1
    );
    // An address that never connected is still refused with the right passcode.
    let stranger = Peer::Udp(ip_n(9));
    assert_eq!(
        connect_status(&mut osc, &show, stranger, later, "s3cret"),
        "denied"
    );
    // Known-ness belongs to the address, so a TCP reconnect from it passes too.
    let tcp = Peer::Tcp {
        conn: 5,
        ip: operator.ip(),
    };
    assert_eq!(connect_status(&mut osc, &show, tcp, later, "s3cret"), "ok");
}

#[test]
fn a_reset_forgets_known_addresses() {
    let show = Show::with_passcode("s3cret");
    let mut osc = quiet_osc();
    let operator = Peer::Udp(ip_n(1));
    connect_status(&mut osc, &show, operator, T0, "s3cret");
    osc.disconnect(operator);
    osc.reset_sessions();
    // Forgotten by the reset: the address is a stranger again.
    assert_eq!(osc.known_ips(), 0);
    for _ in 0..MAX_BAD_PASSCODES {
        connect_status(&mut osc, &show, operator, T0, "x");
    }
    assert_eq!(
        connect_status(&mut osc, &show, operator, T0, "s3cret"),
        "denied"
    );
}

#[test]
fn the_known_address_set_is_bounded() {
    let show = Show::open();
    let mut osc = quiet_osc();
    for i in 0..MAX_KNOWN_IPS + 20 {
        connect_status(&mut osc, &show, Peer::Udp(ip_n(i)), T0, "any");
    }
    assert_eq!(osc.known_ips(), MAX_KNOWN_IPS);
}

#[test]
fn passcodes_of_other_lengths_do_not_match() {
    let show = Show::with_passcode("s3cret");
    let mut osc = quiet_osc();
    for wrong in ["s3cre", "s3crets", "", "S3CRET"] {
        let out = send(&mut osc, &show, udp(), T0, &msg("/connect", vec![s(wrong)]));
        assert_eq!(reply_json(&out.replies[0])["data"], "badpass", "{wrong:?}");
    }
}

#[test]
fn replies_per_call_are_capped() {
    let show = Show::open();
    let mut osc = quiet_osc();
    // `/cue/*/name` answers once per list and cue (nine in the stand-in show), so a bundle of
    // them goes past the cap.
    let read = bytes(&msg("/cue/*/name", vec![]));
    let packet = bundle(&vec![read.clone(); MAX_MESSAGES_PER_PACKET]);
    let asked = MAX_MESSAGES_PER_PACKET * 9;
    let out = osc.handle_datagram(udp(), T0, &packet, &show);
    assert_eq!(out.replies.len(), MAX_REPLIES_PER_CALL);
    assert_eq!(
        osc.stats().replies_dropped,
        (asked - MAX_REPLIES_PER_CALL) as u64
    );
    // The same over a stream read of many frames.
    let mut data = Vec::new();
    for _ in 0..MAX_MESSAGES_PER_PACKET {
        data.extend(slip_encode(&read));
    }
    let mut decoder = SlipDecoder::default();
    let out = osc.handle_stream(tcp_peer(1), T0, &mut decoder, &data, &show);
    assert_eq!(out.replies.len(), MAX_REPLIES_PER_CALL);
    assert_eq!(
        osc.stats().replies_dropped,
        2 * (asked - MAX_REPLIES_PER_CALL) as u64
    );
}

#[test]
fn a_reply_is_as_large_as_the_peer_may_be_sent() {
    let mut show = Show::with_passcode("s3cret");
    let notes = |len: usize| json!("n".repeat(len));
    let ask = msg("/cue/1/notes", vec![]);
    let mut osc = quiet_osc();
    let console = udp();
    let tcp = tcp_peer(2);
    for peer in [console, tcp] {
        connect_status(&mut osc, &show, peer, T0, "s3cret");
    }
    // A note over the old 1400-byte cap is sent whole to a peer that connected.
    show.props
        .insert((CUE_1, "notes"), notes(MAX_REPLY_JSON_BYTES + 100));
    let out = send(&mut osc, &show, console, T0, &ask);
    assert_eq!(reply_json(&out.replies[0])["status"], "ok");
    // Up to what a datagram can carry (QLab sent a 60 KB reply over UDP).
    show.props.insert((CUE_1, "notes"), notes(60_000));
    let out = send(&mut osc, &show, console, T0, &ask);
    assert_eq!(
        reply_json(&out.replies[0])["data"].as_str().unwrap().len(),
        60_000
    );
    assert!(encode_message(&out.replies[0]).unwrap().len() <= MAX_UDP_DATAGRAM_BYTES);
    // More than that cannot go in one datagram: an error reply, with no data.
    show.props.insert((CUE_1, "notes"), notes(70_000));
    let out = send(&mut osc, &show, console, T0, &ask);
    let body = reply_json(&out.replies[0]);
    assert_eq!(body["status"], "error");
    assert!(body.get("data").is_none());
    assert!(encode_message(&out.replies[0]).unwrap().len() <= MAX_UDP_DATAGRAM_BYTES);
    // TCP carries it (QLab answered a 100 KB name over TCP).
    show.props.insert((CUE_1, "notes"), notes(100_000));
    let mut decoder = SlipDecoder::default();
    let frame = slip_encode(&bytes(&ask));
    let out = osc.handle_stream(tcp, T0, &mut decoder, &frame, &show);
    assert_eq!(
        reply_json(&out.replies[0])["data"].as_str().unwrap().len(),
        100_000
    );
    // Without a passcode nobody has to connect: everyone may be sent large replies.
    let open = {
        let mut open = Show::open();
        open.props.insert((CUE_1, "notes"), notes(60_000));
        open
    };
    let out = send(&mut osc, &open, Peer::Udp(ip_n(5)), T0, &ask);
    assert_eq!(reply_json(&out.replies[0])["status"], "ok");
}

#[test]
fn a_peer_that_may_not_ask_is_sent_nothing_large() {
    // The cap for such a peer is applied by `reply_message`, whatever the reply holds.
    let body = Reply::ok(json!("n".repeat(MAX_REPLY_JSON_BYTES + 1)));
    let big = reply_message(workspace(), "/x", "/x", &body, MAX_REPLY_JSON_BYTES);
    let small = Reply::ok(json!("n".repeat(500)));
    let kept = reply_message(workspace(), "/x", "/x", &small, MAX_REPLY_JSON_BYTES);
    assert_eq!(reply_json(&big)["status"], "error");
    assert!(reply_json(&big).get("data").is_none());
    assert_eq!(reply_json(&kept)["status"], "ok");
}

#[test]
fn replies_to_one_call_stop_at_a_byte_cap() {
    let mut show = Show::with_passcode("s3cret");
    show.props
        .insert((CUE_1, "notes"), json!("n".repeat(60_000)));
    let mut osc = quiet_osc();
    // A peer that connected with the passcode: not held to a byte budget, so only the call's cap applies.
    connect_status(&mut osc, &show, udp(), T0, "s3cret");
    let read = bytes(&msg("/cue/1/notes", vec![]));
    let packet = bundle(&vec![read; 60]);
    let out = osc.handle_datagram(udp(), T0, &packet, &show);
    let total: usize = out.replies.len() * 60_000;
    assert!(total <= MAX_REPLY_BYTES_PER_CALL, "{total}");
    assert!(out.replies.len() >= 30, "{}", out.replies.len());
    assert!(osc.stats().replies_dropped >= 1);
}

#[test]
fn only_a_peer_that_may_not_ask_has_a_reply_byte_budget_that_refills() {
    let show = Show::with_passcode("s3cret");
    let mut osc = quiet_osc();
    let stranger = udp();
    let mut sent = 0usize;
    let mut asks = 0;
    loop {
        let out = send(&mut osc, &show, stranger, T0, &msg("/go", vec![]));
        if out.replies.is_empty() {
            break;
        }
        sent += out.replies.len();
        asks += 1;
        assert!(asks < 500, "the budget never ran out");
    }
    // About 16 KiB of ~190-byte `denied` replies, then nothing.
    assert!((60..=120).contains(&sent), "{sent} replies sent");
    assert!(osc.stats().replies_throttled >= 1);
    // Another source has its own budget.
    let other = Peer::Udp(ip_n(77));
    assert_eq!(
        send(&mut osc, &show, other, T0, &msg("/go", vec![]))
            .replies
            .len(),
        1
    );
    // Time refills it.
    let later = T0 + Duration::from_secs(2);
    assert_eq!(
        send(&mut osc, &show, stranger, later, &msg("/go", vec![]))
            .replies
            .len(),
        1
    );
    // A peer that has connected is not held to it.
    connect_status(&mut osc, &show, stranger, later, "s3cret");
    let mut open = Show::with_passcode("s3cret");
    open.props.insert((CUE_1, "notes"), json!("n".repeat(1000)));
    for _ in 0..300 {
        let out = send(
            &mut osc,
            &open,
            stranger,
            later,
            &msg("/cue/1/notes", vec![]),
        );
        assert_eq!(reply_json(&out.replies[0])["status"], "ok");
    }
}

#[test]
fn with_no_passcode_a_source_that_has_not_connected_has_a_large_budget_and_one_that_has_none() {
    use openlab_engine::osc::{OPEN_REPLY_BURST_BYTES, OPEN_REPLY_BYTES_PER_SEC};
    assert_eq!(OPEN_REPLY_BURST_BYTES, 256 * 1024);
    assert_eq!(OPEN_REPLY_BYTES_PER_SEC, 64 * 1024);
    let mut show = Show::open();
    show.props
        .insert((CUE_1, "notes"), json!("n".repeat(60_000)));
    let mut osc = quiet_osc();
    let victim = Peer::Udp(ip_n(40));
    let ask = msg("/cue/1/notes", vec![]);
    // A forged source: a 60 KB notes read works (four of them fit the burst), then the budget
    // is spent however many datagrams come: the reflection is bounded.
    let mut sent = 0usize;
    for _ in 0..200 {
        sent += send(&mut osc, &show, victim, T0, &ask).replies.len();
    }
    assert_eq!(sent, 4, "256 KiB of 60 KB replies");
    assert!(osc.stats().replies_throttled >= 196);
    // 64 KiB a second refills: one more such reply after a second.
    let later = T0 + Duration::from_secs(1);
    assert_eq!(send(&mut osc, &show, victim, later, &ask).replies.len(), 1);
    // Updates to a forged subscriber share the budget.
    send(
        &mut osc,
        &show,
        victim,
        later,
        &msg("/updates", vec![OscType::Int(1)]),
    );
    let mut allowed = 0;
    for _ in 0..2000 {
        if osc.update_allowed(victim, later, 120, false) {
            allowed += 1;
        }
    }
    assert!(allowed < 2000 && osc.stats().updates_dropped > 0);
    // Another source has its own; a TCP peer none.
    assert_eq!(
        send(&mut osc, &show, Peer::Udp(ip_n(41)), T0, &ask)
            .replies
            .len(),
        1
    );
    assert!(osc.update_allowed(tcp_peer(9), T0, 100_000, false));
    // With no passcode a `/connect` proves nothing about a datagram's source address: a forged
    // `/connect` followed by reads (in one bundle, or one datagram after another) leaves the
    // victim's budget in place, and so do its updates.
    let forged = Peer::Udp(ip_n(42));
    let packet = bundle(&[
        bytes(&msg("/connect", vec![s("x")])),
        bytes(&ask),
        bytes(&ask),
    ]);
    let mut replies = 0usize;
    for _ in 0..200 {
        replies += osc
            .handle_datagram(forged, T0, &packet, &show)
            .replies
            .iter()
            .filter(|r| r.addr == "/reply/cue/1/notes")
            .count();
    }
    assert!(osc.is_connected(forged));
    assert_eq!(replies, 4, "256 KiB of 60 KB replies and no more");
    let mut allowed = 0;
    for _ in 0..2000 {
        allowed += usize::from(osc.update_allowed(forged, T0, 120, false));
    }
    assert!(allowed < 2000);
}

#[test]
fn with_a_passcode_a_peer_that_connected_is_not_held_to_a_budget() {
    let mut show = Show::with_passcode("s3cret");
    show.props.insert((CUE_1, "notes"), json!("n".repeat(1000)));
    let mut osc = quiet_osc();
    let console = Peer::Udp(ip_n(43));
    connect_status(&mut osc, &show, console, T0, "s3cret");
    for _ in 0..300 {
        let out = send(&mut osc, &show, console, T0, &msg("/cue/1/notes", vec![]));
        assert_eq!(reply_json(&out.replies[0])["status"], "ok");
    }
    assert!(osc.update_allowed(console, T0, 100_000, true));
    // The passcode, not the session, is what proves the address: with none set it does not.
    assert!(!osc.update_allowed(console, T0, 10_000_000, false));
}

#[test]
fn empty_cue_numbers_and_trailing_stars_are_charged_and_bounded() {
    // Unnumbered cues and lists have an empty number; every alternative still has to be set up.
    let big = Numbered(vec![String::new(); 5000]);
    let mut osc = quiet_osc();
    let stars = "*".repeat(100);
    let elements: Vec<Vec<u8>> = (0..120)
        .map(|i| {
            bytes(&msg(
                &format!("/cue/{{*,**}}{{*,**}}{{*,**}}{{*,**}}{{*,**}}{{*,**}}{stars}Z{i}/name"),
                vec![],
            ))
        })
        .collect();
    let started = std::time::Instant::now();
    let out = osc.handle_datagram(udp(), T0, &bundle(&elements), &big);
    assert!(started.elapsed() < Duration::from_millis(1500));
    assert_eq!(out.replies.len(), 120);
    assert!(osc.stats().matches_capped >= 1);
    // Runs of stars are one star.
    let mut budget = MATCH_BUDGET;
    let p = CuePattern::compile("a***b").unwrap();
    assert_eq!(p.is_match("axxb", &mut budget), Ok(true));
}

#[test]
fn a_cue_trigger_may_contain_pattern_characters() {
    let show = Show::open();
    let mut osc = quiet_osc();
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
    let mut osc = quiet_osc();
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
    assert_eq!(osc.update_subscribers(T0).len(), MAX_PEERS);
}

// ---- QLab 5 behaviour (M4.5b): each point of the brief, as measured in the capture

fn replies_of(out: &Outcome) -> Vec<Value> {
    out.replies.iter().map(reply_json).collect()
}

fn ws() -> String {
    upper(workspace())
}

/// The cue numbers an `/cue/{pattern}/name` read answered for, in order (a cue list's reply has
/// a `cue_id` address, and its data is its name).
fn answered_for(osc: &mut Osc, show: &Show, pattern: &str) -> Vec<String> {
    let out = send(
        osc,
        show,
        udp(),
        T0,
        &msg(&format!("/cue/{pattern}/name"), vec![]),
    );
    replies_of(&out)
        .iter()
        .filter(|r| r["status"] == "ok")
        .map(|r| {
            let address = r["address"].as_str().unwrap();
            let tail = address
                .strip_prefix(&format!("/workspace/{}", ws()))
                .unwrap();
            let parts: Vec<&str> = tail.split('/').collect();
            if parts[1] == "cue_id" {
                "list".to_owned()
            } else {
                parts[2].to_owned()
            }
        })
        .collect()
}

// 1. /connect replies

#[test]
fn connect_says_ok_and_the_rights_granted_and_a_wrong_passcode_is_ok_badpass() {
    let show = Show::with_passcode("1234");
    let mut osc = quiet_osc();
    let out = send(
        &mut osc,
        &show,
        udp(),
        T0,
        &msg("/connect", vec![s("1234")]),
    );
    assert_eq!(
        reply_json(&out.replies[0]),
        json!({
            "status": "ok",
            "data": "ok:view|edit|control",
            "workspace_id": ws(),
            "address": format!("/workspace/{}/connect", ws()),
        })
    );
    let out = send(
        &mut osc,
        &show,
        tcp_peer(1),
        T0,
        &msg("/connect", vec![s("wrong")]),
    );
    assert_eq!(
        reply_json(&out.replies[0]),
        json!({
            "status": "ok",
            "data": "badpass",
            "workspace_id": ws(),
            "address": format!("/workspace/{}/connect", ws()),
        })
    );
    // 1234 sent as an integer is accepted like the string.
    let by_int = send(
        &mut osc,
        &show,
        Peer::Udp(ip_n(3)),
        T0,
        &msg("/connect", vec![OscType::Int(1234)]),
    );
    assert_eq!(
        reply_json(&by_int.replies[0])["data"],
        "ok:view|edit|control"
    );
    // With no passcode set the grant is the same.
    let open = Show::open();
    let out = send(&mut osc, &open, udp(), T0, &msg("/connect", vec![]));
    assert_eq!(reply_json(&out.replies[0])["data"], "ok:view|edit|control");
    // A lockout is `denied`, not `ok`.
    for i in 0..MAX_BAD_PASSCODES {
        connect_status(&mut osc, &show, tcp_at(10 + u64::from(i), 99), T0, "bad");
    }
    let out = send(
        &mut osc,
        &show,
        tcp_at(50, 99),
        T0,
        &msg("/connect", vec![s("1234")]),
    );
    assert_eq!(reply_json(&out.replies[0])["status"], "denied");
}

// 2. Messages before /connect are `denied` over UDP and TCP

#[test]
fn before_connect_every_workspace_message_is_denied_over_udp_and_tcp_but_the_application_ones_answer()
 {
    let show = Show::with_passcode("1234");
    let mut osc = quiet_osc();
    for peer in [udp(), tcp_peer(1)] {
        for addr in ["/go", "/cue/1/name", "/cueLists", "/stop", "/bogus/address"] {
            let out = send(&mut osc, &show, peer, T0, &msg(addr, vec![]));
            assert!(out.inputs.is_empty(), "{addr}");
            assert_eq!(
                reply_json(&out.replies[0]),
                json!({
                    "address": format!("/workspace/{}{addr}", ws()),
                    "status": "denied",
                    "workspace_id": ws(),
                }),
                "{addr}"
            );
            assert_eq!(out.replies[0].addr, format!("/reply{addr}"));
        }
        // `/version` and `/workspaces` need no passcode.
        let out = send(&mut osc, &show, peer, T0, &msg("/version", vec![]));
        assert_eq!(reply_json(&out.replies[0])["status"], "ok");
        let out = send(&mut osc, &show, peer, T0, &msg("/workspaces", vec![]));
        assert_eq!(reply_json(&out.replies[0])["status"], "ok");
    }
    // After the passcode the same messages are served.
    connect_status(&mut osc, &show, udp(), T0, "1234");
    let out = send(&mut osc, &show, udp(), T0, &msg("/cue/1/name", vec![]));
    assert_eq!(reply_json(&out.replies[0])["status"], "ok");
}

// 3 is in `a_wrong_or_lower_case_workspace_id_is_an_error_with_only_address_and_status`.

// 4. Patterns

#[test]
fn patterns_match_cue_numbers_in_every_list_and_inside_groups() {
    let show = Show::open();
    let mut osc = quiet_osc();
    // `*` also reaches the lists and the cue without a number; the others only numbered cues.
    assert_eq!(
        answered_for(&mut osc, &show, "*"),
        vec!["list", "1", "2.5", "10", "11", "7", "list", "20"]
    );
    assert_eq!(answered_for(&mut osc, &show, "?"), vec!["1", "7"]);
    assert_eq!(answered_for(&mut osc, &show, "??"), vec!["10", "11", "20"]);
    assert_eq!(answered_for(&mut osc, &show, "[12]"), vec!["1"]);
    assert_eq!(answered_for(&mut osc, &show, "[0-9]"), vec!["1", "7"]);
    assert_eq!(answered_for(&mut osc, &show, "[!1]"), vec!["7"]);
    assert_eq!(answered_for(&mut osc, &show, "{1,2.5}"), vec!["1", "2.5"]);
    assert_eq!(answered_for(&mut osc, &show, "1*"), vec!["1", "10", "11"]);
    assert_eq!(answered_for(&mut osc, &show, "{1,20}"), vec!["1", "20"]);
    assert_eq!(answered_for(&mut osc, &show, "2?"), vec!["20"]);
    assert_eq!(
        answered_for(&mut osc, &show, "nothing*"),
        Vec::<String>::new()
    );
}

#[test]
fn a_pattern_read_answers_once_per_match_with_the_cues_own_address() {
    let show = Show::open();
    let mut osc = quiet_osc();
    let out = send(&mut osc, &show, udp(), T0, &msg("/cue/[12]/name", vec![]));
    assert_eq!(out.replies.len(), 1);
    assert_eq!(out.replies[0].addr, "/reply/cue/[12]/name");
    assert_eq!(
        reply_json(&out.replies[0]),
        json!({
            "status": "ok",
            "data": "Opening",
            "workspace_id": ws(),
            "address": format!("/workspace/{}/cue/1/name", ws()),
        })
    );
    // A cue list answers with its name, at its `cue_id` address.
    let out = send(&mut osc, &show, udp(), T0, &msg("/cue/*/name", vec![]));
    let first = reply_json(&out.replies[0]);
    assert_eq!(first["data"], "Main");
    assert_eq!(
        first["address"],
        format!("/workspace/{}/cue_id/{}/name", ws(), upper(LIST.0))
    );
    // A reply to one cue that has no such property is an error for that cue alone.
    let out = send(&mut osc, &show, udp(), T0, &msg("/cue/*/notes", vec![]));
    assert!(replies_of(&out).iter().all(|r| r["status"] == "error"));
    assert_eq!(out.replies.len(), 7);
}

#[test]
fn an_action_through_a_pattern_applies_to_every_match() {
    let show = Show::open();
    let mut osc = quiet_osc();
    let out = send(
        &mut osc,
        &show,
        udp(),
        T0,
        &msg("/cue/{1,20}/start", vec![]),
    );
    assert!(out.replies.is_empty());
    assert_eq!(
        out.inputs,
        vec![
            transport(TransportAction::Start { cue: CUE_1 }),
            transport(TransportAction::Start { cue: CUE_20 }),
        ]
    );
    // `*` acts on the cues in every list and group, not on the cue lists.
    let out = send(&mut osc, &show, udp(), T0, &msg("/cue/*/stop", vec![]));
    assert!(out.replies.is_empty());
    let stopped: Vec<CueId> = out
        .inputs
        .iter()
        .map(|i| match i {
            OscInput::Transport(TransportAction::Stop { cue, .. }) => *cue,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        stopped,
        vec![CUE_1, CUE_25, TRIGGERED, GROUP, CHILD, CUE_7, CUE_20]
    );
    // A write reaches every match.
    let out = send(
        &mut osc,
        &show,
        udp(),
        T0,
        &msg("/cue/[12]*/armed", vec![OscType::Int(0)]),
    );
    assert_eq!(out.inputs.len(), 5);
    // An argument that is wrong for the verb is an error for each match.
    let out = send(
        &mut osc,
        &show,
        udp(),
        T0,
        &msg("/cue/{1,7}/start", vec![OscType::Int(1)]),
    );
    assert!(out.inputs.is_empty());
    assert_eq!(out.replies.len(), 2);
}

#[test]
fn a_pattern_that_is_not_valid_or_matches_nothing_is_an_error() {
    let show = Show::open();
    let mut osc = quiet_osc();
    for pattern in ["[", "[]", "{", "{a", "a}", "]", "{a{b}}", "[a-"] {
        let out = send(
            &mut osc,
            &show,
            udp(),
            T0,
            &msg(&format!("/cue/{pattern}/start"), vec![]),
        );
        assert!(out.inputs.is_empty(), "{pattern}");
        assert_eq!(out.replies.len(), 1, "{pattern}");
        assert_eq!(reply_json(&out.replies[0])["status"], "error", "{pattern}");
    }
}

/// A show that is nothing but cues with the given numbers (all in one list).
struct Numbered(Vec<String>);

impl OscShow for Numbered {
    fn workspace_id(&self) -> WorkspaceId {
        workspace()
    }
    fn passcode(&self) -> Option<&str> {
        None
    }
    fn active_list(&self) -> Option<CueListId> {
        Some(LIST)
    }
    fn cue_by_number(&self, _: &str) -> Option<CueId> {
        None
    }
    fn cue_exists(&self, _: CueId) -> bool {
        true
    }
    fn cue_property(&self, _: CueId, field: CueField) -> Option<Value> {
        (field == CueField::Armed).then_some(json!(true))
    }
    fn cue_by_osc_trigger(&self, _: &str) -> Option<CueId> {
        None
    }
    fn visit_targets(&self, visit: &mut dyn FnMut(Target, &str) -> ControlFlow<()>) {
        for (i, number) in self.0.iter().enumerate() {
            let id = CueId(Uuid::from_u128(i as u128));
            if visit(Target::Cue(id), number).is_break() {
                return;
            }
        }
    }
}

#[test]
fn a_pathological_pattern_is_bounded() {
    let show = Show::open();
    let mut osc = quiet_osc();
    // More alternatives than are allowed: an error, not 2^n work.
    let many = "{a,b}".repeat(40);
    let out = send(
        &mut osc,
        &show,
        udp(),
        T0,
        &msg(&format!("/cue/{many}/name"), vec![]),
    );
    assert_eq!(out.replies.len(), 1);
    assert_eq!(reply_json(&out.replies[0])["status"], "error");
    // Stars that backtrack on long numbers: the matcher is linear in pattern times text, and the
    // work of one message has a budget, so this ends quickly and says it was cut short.
    let numbers: Vec<String> = (0..10_000).map(|_| "a".repeat(200)).collect();
    let stars = format!("{}b", "*a".repeat(50));
    let out = osc.handle_datagram(
        udp(),
        T0,
        &bytes(&msg(&format!("/cue/{stars}/name"), vec![])),
        &Numbered(numbers.clone()),
    );
    assert_eq!(osc.stats().matches_capped, 1);
    assert_eq!(out.replies.len(), 1);
    assert_eq!(reply_json(&out.replies[0])["status"], "error");
    // A number longer than a pattern is tried against is not matched, however it is spelt.
    let long = vec!["1".repeat(MAX_MATCH_CHARS + 1)];
    let out = osc.handle_datagram(
        udp(),
        T0,
        &bytes(&msg("/cue/*/armed", vec![])),
        &Numbered(long),
    );
    assert_eq!(reply_json(&out.replies[0])["status"], "error");
    // The longest address allowed, all pattern.
    let long = "*".repeat(240);
    let out = send(
        &mut osc,
        &show,
        udp(),
        T0,
        &msg(&format!("/cue/{long}/name"), vec![]),
    );
    assert!(!out.replies.is_empty());
}

#[test]
fn a_pattern_matching_too_many_cues_is_cut_at_the_cap_and_counted() {
    let numbers: Vec<String> = (0..5000).map(|i| i.to_string()).collect();
    let big = Numbered(numbers);
    let mut osc = quiet_osc();
    let out = osc.handle_datagram(udp(), T0, &bytes(&msg("/cue/*/start", vec![])), &big);
    assert_eq!(out.inputs.len(), MAX_MATCHES);
    assert_eq!(osc.stats().matches_capped, 1);
    // Replies stop at the cap too.
    let out = osc.handle_datagram(udp(), T0, &bytes(&msg("/cue/*/armed", vec![])), &big);
    assert_eq!(out.replies.len(), MAX_MATCHES);
    assert_eq!(osc.stats().matches_capped, 2);
}

// 5. New transport actions

#[test]
fn stop_hard_stop_and_per_cue_panic_and_hard_stop_are_transport_actions() {
    let show = Show::open();
    let mut osc = quiet_osc();
    assert_eq!(
        one_input(&mut osc, &show, "/stop", vec![]),
        transport(TransportAction::StopAll)
    );
    assert_eq!(
        one_input(&mut osc, &show, "/hardStop", vec![]),
        transport(TransportAction::HardStopAll)
    );
    assert_eq!(
        one_input(&mut osc, &show, "/panic", vec![]),
        transport(TransportAction::Panic)
    );
    assert_eq!(
        one_input(&mut osc, &show, "/cue/1/panic", vec![]),
        transport(TransportAction::PanicCue { cue: CUE_1 })
    );
    assert_eq!(
        one_input(&mut osc, &show, "/cue/1/hardStop", vec![]),
        transport(TransportAction::HardStop { cue: CUE_1 })
    );
    // `/cue_id`, the workspace prefix and a pattern reach them too.
    assert_eq!(
        one_input(
            &mut osc,
            &show,
            &format!("/workspace/{}/cue_id/{}/panic", ws(), upper(GROUP)),
            vec![]
        ),
        transport(TransportAction::PanicCue { cue: GROUP })
    );
    let out = send(
        &mut osc,
        &show,
        udp(),
        T0,
        &msg("/cue/{1,2.5}/panic", vec![]),
    );
    assert_eq!(out.inputs.len(), 2);
    // `/hardPanic` does not exist; an argument on `/cue/1/panic` is wrong.
    let out = send(&mut osc, &show, udp(), T0, &msg("/hardPanic", vec![]));
    assert_eq!(reply_json(&out.replies[0])["status"], "error");
    let out = send(
        &mut osc,
        &show,
        udp(),
        T0,
        &msg("/cue/1/panic", vec![OscType::Int(1)]),
    );
    assert_eq!(reply_json(&out.replies[0])["status"], "error");
    // None of the successful ones was answered.
}

// 6. /go with a string argument

#[test]
fn go_with_a_string_goes_from_that_cue_and_numbers_are_ignored() {
    let show = Show::open();
    let mut osc = quiet_osc();
    let out = send(&mut osc, &show, udp(), T0, &msg("/go", vec![s("7")]));
    assert!(out.replies.is_empty());
    assert_eq!(
        out.inputs,
        vec![transport(TransportAction::GoFrom {
            list: LIST,
            cue: CUE_7
        }),]
    );
    // A cue in the other list goes from that list's playhead.
    let out = send(&mut osc, &show, udp(), T0, &msg("/go", vec![s("20")]));
    assert_eq!(
        out.inputs,
        vec![transport(TransportAction::GoFrom {
            list: LIST_B,
            cue: CUE_20
        }),]
    );
    // Integer and float arguments are ignored: a plain GO.
    for arg in [OscType::Int(7), OscType::Float(7.0), OscType::Double(7.0)] {
        assert_eq!(
            one_input(&mut osc, &show, "/go", vec![arg]),
            transport(TransportAction::Go { list: LIST })
        );
    }
    // No such cue, no number, or a cue no playhead can rest on (inside a group): an error, and
    // nothing is started.
    for arg in ["999", "", "11"] {
        let out = send(&mut osc, &show, udp(), T0, &msg("/go", vec![s(arg)]));
        assert!(out.inputs.is_empty(), "{arg:?}");
        assert_eq!(reply_json(&out.replies[0])["status"], "error", "{arg:?}");
    }
}

// 7. UDP sessions forgotten after 60 s of silence

#[test]
fn a_udp_session_and_subscription_are_forgotten_after_sixty_seconds_of_silence() {
    assert_eq!(UDP_IDLE, Duration::from_secs(60));
    let show = Show::with_passcode("1234");
    let mut osc = quiet_osc();
    let console = udp();
    connect_status(&mut osc, &show, console, T0, "1234");
    send(
        &mut osc,
        &show,
        console,
        T0,
        &msg("/updates", vec![OscType::Int(1)]),
    );
    // Any message within the window keeps it (it is renewed from the last one).
    let t30 = T0 + Duration::from_secs(30);
    let out = send(&mut osc, &show, console, t30, &msg("/cue/1/name", vec![]));
    assert_eq!(reply_json(&out.replies[0])["status"], "ok");
    let just_before = t30 + UDP_IDLE - Duration::from_millis(1);
    assert_eq!(osc.update_subscribers(just_before), vec![console]);
    let out = send(
        &mut osc,
        &show,
        console,
        just_before,
        &msg("/cue/2.5/name", vec![]),
    );
    assert_eq!(reply_json(&out.replies[0])["status"], "ok");
    // Sixty seconds of silence: no more updates for it, and its next message is `denied`.
    let silent_until = just_before + UDP_IDLE;
    assert!(osc.update_subscribers(silent_until).is_empty());
    let out = send(
        &mut osc,
        &show,
        console,
        silent_until,
        &msg("/cue/1/name", vec![]),
    );
    assert_eq!(reply_json(&out.replies[0])["status"], "denied");
    assert!(osc.update_subscribers(silent_until).is_empty());
    // A TCP peer is not forgotten for silence.
    let tcp = tcp_peer(4);
    connect_status(&mut osc, &show, tcp, T0, "1234");
    send(
        &mut osc,
        &show,
        tcp,
        T0,
        &msg("/updates", vec![OscType::Int(1)]),
    );
    let much_later = T0 + Duration::from_secs(3600);
    assert_eq!(osc.update_subscribers(much_later), vec![tcp]);
    assert!(osc.is_connected(tcp));
}

// 8. /cue/{n} searches all lists; /go acts on the active list

#[test]
fn a_cue_number_is_found_in_any_list_and_go_acts_on_the_active_one() {
    let mut show = Show::open();
    let mut osc = quiet_osc();
    // The active list is the main one: cue 20 of the second list is still reachable.
    assert_eq!(
        one_input(&mut osc, &show, "/cue/20/start", vec![]),
        transport(TransportAction::Start { cue: CUE_20 })
    );
    let out = send(&mut osc, &show, udp(), T0, &msg("/cue/20/name", vec![]));
    assert_eq!(reply_json(&out.replies[0])["data"], "Cue 20");
    assert_eq!(
        one_input(&mut osc, &show, "/go", vec![]),
        transport(TransportAction::Go { list: LIST })
    );
    // With the second list active `/go` is its GO, and cue 1 is still reachable.
    show.list = Some(LIST_B);
    assert_eq!(
        one_input(&mut osc, &show, "/go", vec![]),
        transport(TransportAction::Go { list: LIST_B })
    );
    assert_eq!(
        one_input(&mut osc, &show, "/cue/1/start", vec![]),
        transport(TransportAction::Start { cue: CUE_1 })
    );
    // A playhead set to a cue in another list goes to that list, whichever is active.
    assert_eq!(
        one_input(&mut osc, &show, "/playhead/7", vec![]),
        transport(TransportAction::SetPlayheadByNumber {
            list: LIST,
            number: "7".into()
        })
    );
    show.list = Some(LIST);
    assert_eq!(
        one_input(&mut osc, &show, "/select/20", vec![]),
        transport(TransportAction::SetPlayheadByNumber {
            list: LIST_B,
            number: "20".into()
        })
    );
    // A cue that does not exist, or that cannot hold a playhead, is an error.
    for addr in ["/playhead/999", "/select/11"] {
        let out = send(&mut osc, &show, udp(), T0, &msg(addr, vec![]));
        assert!(out.inputs.is_empty(), "{addr}");
        assert_eq!(reply_json(&out.replies[0])["status"], "error", "{addr}");
    }
}

// 10. Queries

#[test]
fn selected_and_playhead_are_the_cue_at_the_active_lists_playhead() {
    let mut show = Show::open();
    show.playheads.insert(LIST, CUE_25);
    let mut osc = quiet_osc();
    for who in ["selected", "playhead"] {
        let out = send(
            &mut osc,
            &show,
            udp(),
            T0,
            &msg(&format!("/cue/{who}/number"), vec![]),
        );
        assert_eq!(out.replies.len(), 1);
        assert_eq!(out.replies[0].addr, format!("/reply/cue/{who}/number"));
        assert_eq!(
            reply_json(&out.replies[0]),
            json!({
                "status": "ok",
                "data": "2.5",
                "workspace_id": ws(),
                "address": format!("/workspace/{}/cue/2.5/number", ws()),
            })
        );
    }
    // A property can be written through them.
    let out = send(
        &mut osc,
        &show,
        udp(),
        T0,
        &msg("/cue/selected/name", vec![s("New")]),
    );
    assert_eq!(out.inputs.len(), 1);
    // A cue with no number is answered at its `cue_id` address.
    show.playheads.insert(LIST, TRIGGERED);
    let out = send(
        &mut osc,
        &show,
        udp(),
        T0,
        &msg("/cue/playhead/number", vec![]),
    );
    assert_eq!(
        reply_json(&out.replies[0])["address"],
        format!("/workspace/{}/cue_id/{}/number", ws(), upper(TRIGGERED))
    );
    // Nothing at the playhead, or the other list: an error.
    show.playheads.clear();
    let out = send(
        &mut osc,
        &show,
        udp(),
        T0,
        &msg("/cue/playhead/number", vec![]),
    );
    assert_eq!(reply_json(&out.replies[0])["status"], "error");
    show.playheads.insert(LIST, CUE_1);
    show.playheads.insert(LIST_B, CUE_20);
    show.list = Some(LIST_B);
    let out = send(
        &mut osc,
        &show,
        udp(),
        T0,
        &msg("/cue/selected/number", vec![]),
    );
    assert_eq!(reply_json(&out.replies[0])["data"], "20");
}

#[test]
fn active_answers_once_per_running_cue_and_errors_when_nothing_runs() {
    let mut show = Show::open();
    let mut osc = quiet_osc();
    let out = send(&mut osc, &show, udp(), T0, &msg("/cue/active/name", vec![]));
    assert_eq!(out.replies.len(), 1);
    assert_eq!(reply_json(&out.replies[0])["status"], "error");
    show.running = vec![CUE_1, CUE_20];
    show.paused = vec![CUE_7];
    let out = send(
        &mut osc,
        &show,
        udp(),
        T0,
        &msg("/cue/active/number", vec![]),
    );
    let replies = replies_of(&out);
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0]["data"], "1");
    assert_eq!(
        replies[0]["address"],
        format!("/workspace/{}/cue/1/number", ws())
    );
    assert_eq!(replies[1]["data"], "20");
    assert!(
        out.replies
            .iter()
            .all(|r| r.addr == "/reply/cue/active/number")
    );
    // The bare forms are errors.
    for addr in ["/cue/active", "/cue/selected", "/cue/playhead"] {
        let out = send(&mut osc, &show, udp(), T0, &msg(addr, vec![]));
        assert_eq!(reply_json(&out.replies[0])["status"], "error", "{addr}");
    }
    // And the properties `isRunning` and friends read.
    let out = send(&mut osc, &show, udp(), T0, &msg("/cue/1/isRunning", vec![]));
    assert_eq!(reply_json(&out.replies[0])["data"], true);
    let out = send(&mut osc, &show, udp(), T0, &msg("/cue/7/isPaused", vec![]));
    assert_eq!(reply_json(&out.replies[0])["data"], true);
}

#[test]
fn the_workspace_queries_answer_with_the_captured_shapes() {
    let mut show = Show::open();
    show.running = vec![CUE_1];
    show.paused = vec![CUE_7];
    show.playheads.insert(LIST, CUE_25);
    let mut osc = quiet_osc();
    let ask = |osc: &mut Osc, show: &Show, addr: &str| {
        let out = send(osc, show, udp(), T0, &msg(addr, vec![]));
        assert_eq!(out.replies.len(), 1, "{addr}");
        assert_eq!(out.replies[0].addr, format!("/reply{addr}"));
        reply_json(&out.replies[0])
    };
    let running = ask(&mut osc, &show, "/runningCues");
    assert_eq!(running["status"], "ok");
    assert_eq!(running["workspace_id"], ws());
    assert_eq!(
        running["address"],
        format!("/workspace/{}/runningCues", ws())
    );
    let numbers = |v: &Value| -> Vec<String> {
        v["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["number"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(numbers(&running), vec!["1"]);
    assert_eq!(
        numbers(&ask(&mut osc, &show, "/runningOrPausedCues")),
        vec!["1", "7"]
    );
    assert_eq!(numbers(&ask(&mut osc, &show, "/selectedCues")), vec!["2.5"]);
    let lists = ask(&mut osc, &show, "/cueLists");
    assert_eq!(lists["data"][0]["type"], "Cue List");
    // Nothing running is an empty list, not an error.
    show.running.clear();
    show.paused.clear();
    assert_eq!(ask(&mut osc, &show, "/runningCues")["data"], json!([]));
    // `/playheadID`: the playhead's id, or `none`.
    let id = ask(&mut osc, &show, "/playheadID");
    assert_eq!(
        id,
        json!({
            "status": "ok",
            "data": upper(CUE_25),
            "workspace_id": ws(),
            "address": format!("/workspace/{}/playheadID", ws()),
        })
    );
    show.playheads.clear();
    assert_eq!(ask(&mut osc, &show, "/playheadID")["data"], "none");
    assert_eq!(ask(&mut osc, &show, "/selectedCues")["data"], json!([]));
    // `/playheadId` is deprecated: answered, with a warning, at the `/playheadID` address.
    show.playheads.insert(LIST, CUE_1);
    let old = ask(&mut osc, &show, "/playheadId");
    assert_eq!(old["status"], "ok");
    assert_eq!(old["data"], upper(CUE_1));
    assert_eq!(old["deprecated"], true);
    assert!(old["warning"].as_str().unwrap().contains("playheadID"));
    assert_eq!(old["address"], format!("/workspace/{}/playheadID", ws()));
}

#[test]
fn version_and_workspaces_are_application_level_replies() {
    let show = Show::open();
    let mut osc = Osc::with_info(openlab_engine::osc::ServerInfo {
        version: "0.1.2".into(),
        port: 53000,
        reply_port: 53001,
    });
    let out = send(&mut osc, &show, udp(), T0, &msg("/version", vec![]));
    assert_eq!(out.replies[0].addr, "/reply/version");
    assert_eq!(
        reply_json(&out.replies[0]),
        json!({"address": "/version", "status": "ok", "data": "0.1.2"})
    );
    let out = send(&mut osc, &show, udp(), T0, &msg("/workspaces", vec![]));
    assert_eq!(
        reply_json(&out.replies[0]),
        json!({
            "address": "/workspaces",
            "status": "ok",
            "data": [{
                "version": "0.1.2",
                "displayName": "Hamlet",
                "udpReplyPort": 53001,
                "uniqueID": ws(),
                "port": 53000,
            }],
        })
    );
}

// 11. Identical messages within 100 ms

#[test]
fn an_identical_message_within_a_hundred_milliseconds_is_dropped_go_included() {
    let show = Show::open();
    let mut osc = Osc::new();
    let go = msg("/go", vec![]);
    let at = |ms: u64| T0 + Duration::from_millis(ms);
    // 0 ms: handled. 50 ms: dropped, no input and no reply. 100 ms: past the window of the
    // message that was handled (a dropped one does not renew it): handled. 150 ms: dropped.
    assert_eq!(send(&mut osc, &show, udp(), at(0), &go).inputs.len(), 1);
    assert_eq!(
        send(&mut osc, &show, udp(), at(50), &go),
        Outcome::default()
    );
    assert_eq!(send(&mut osc, &show, udp(), at(100), &go).inputs.len(), 1);
    assert_eq!(
        send(&mut osc, &show, udp(), at(150), &go),
        Outcome::default()
    );
    assert_eq!(osc.stats().duplicates_dropped, 2);
    // The same read three times in a burst is answered once.
    let read = msg("/cue/1/name", vec![]);
    let out = send(&mut osc, &show, udp(), at(1000), &read);
    assert_eq!(out.replies.len(), 1);
    assert_eq!(
        send(&mut osc, &show, udp(), at(1000), &read),
        Outcome::default()
    );
    assert_eq!(
        send(&mut osc, &show, udp(), at(1000), &read),
        Outcome::default()
    );
    // 120 ms apart they are all handled.
    for i in 0..5 {
        let out = send(&mut osc, &show, udp(), at(2000 + 120 * i), &read);
        assert_eq!(out.replies.len(), 1);
    }
}

#[test]
fn messages_that_differ_or_come_from_another_peer_are_not_dropped() {
    let show = Show::open();
    let mut osc = Osc::new();
    let at = T0;
    // Different addresses, quickly: all handled.
    for n in ["1", "2.5", "7", "20"] {
        let out = send(
            &mut osc,
            &show,
            udp(),
            at,
            &msg(&format!("/cue/{n}/name"), vec![]),
        );
        assert_eq!(out.replies.len(), 1);
    }
    // The same address with different arguments.
    for value in 0..3 {
        let out = send(
            &mut osc,
            &show,
            udp(),
            at,
            &msg("/cue/1/preWait", vec![OscType::Int(value)]),
        );
        assert_eq!(out.inputs.len(), 1);
    }
    // The same message from another peer, and over TCP.
    let go = msg("/go", vec![]);
    for peer in [udp(), Peer::Udp(ip_n(2)), tcp_peer(1), tcp_peer(2)] {
        assert_eq!(send(&mut osc, &show, peer, at, &go).inputs.len(), 1);
    }
    // A closed TCP connection's memory goes with it: its number may be used again at once.
    osc.disconnect(tcp_peer(1));
    assert_eq!(send(&mut osc, &show, tcp_peer(1), at, &go).inputs.len(), 1);
}

#[test]
fn the_repeat_memory_is_bounded() {
    use openlab_engine::osc::Dedupe;
    let mut dedupe = Dedupe::default();
    let now = T0;
    let cap = openlab_engine::osc::MAX_DEDUPE_PEERS;
    for i in 0..cap + 500 {
        dedupe.is_duplicate(Peer::Udp(ip_n(i)), now, b"x");
    }
    assert_eq!(dedupe.len(), cap);
    // A peer past the cap is just not remembered (never an error), and the ones that are keep
    // working.
    assert!(!dedupe.is_duplicate(Peer::Udp(ip_n(cap + 100)), now, b"x"));
    assert!(!dedupe.is_duplicate(Peer::Udp(ip_n(cap + 100)), now, b"x"));
    assert!(dedupe.is_duplicate(Peer::Udp(ip_n(0)), now, b"x"));
    // Entries out of the window make room.
    let later = now + Duration::from_millis(200);
    assert!(!dedupe.is_duplicate(Peer::Udp(ip_n(cap + 100)), later, b"x"));
    assert!(dedupe.is_duplicate(Peer::Udp(ip_n(cap + 100)), later, b"x"));
    assert!(dedupe.len() < cap);
    dedupe.expire(later + Duration::from_secs(1));
    assert!(dedupe.is_empty());
}

// 12. Limits

#[test]
fn an_unauthenticated_tcp_peer_may_send_only_small_frames_and_a_connected_one_large_ones() {
    use openlab_engine::osc::MAX_UNAUTH_FRAME_BYTES;
    assert_eq!(MAX_FRAME_BYTES, 1024 * 1024);
    let show = Show::with_passcode("1234");
    let mut osc = quiet_osc();
    let tcp = tcp_peer(1);
    let mut decoder = SlipDecoder::default();
    let big = |len: usize| slip_encode(&bytes(&msg("/cue/1/notes", vec![s(&"x".repeat(len))])));
    // Before `/connect`: a frame over 4 KiB is skipped and counted.
    let out = osc.handle_stream(tcp, T0, &mut decoder, &big(MAX_UNAUTH_FRAME_BYTES), &show);
    assert_eq!(out, Outcome::default());
    assert_eq!(osc.stats().oversize_frames, 1);
    let connect = slip_encode(&bytes(&msg("/connect", vec![s("1234")])));
    let out = osc.handle_stream(tcp, T0, &mut decoder, &connect, &show);
    assert_eq!(reply_json(&out.replies[0])["data"], "ok:view|edit|control");
    // After it a frame of 900 KB is handled (a note: a write, so an input).
    let out = osc.handle_stream(tcp, T0, &mut decoder, &big(900 * 1024), &show);
    assert_eq!(out.inputs.len(), 1);
    assert_eq!(osc.stats().oversize_frames, 1);
    assert_eq!(osc.stats().malformed, 0);
    // And one over 1 MiB is not.
    let out = osc.handle_stream(tcp, T0, &mut decoder, &big(MAX_FRAME_BYTES), &show);
    assert_eq!(out, Outcome::default());
    assert_eq!(osc.stats().oversize_frames, 2);
}

#[test]
fn the_wire_limits_are_the_decided_ones() {
    assert_eq!(MAX_PACKET_BYTES, 64 * 1024);
    assert_eq!(MAX_UDP_DATAGRAM_BYTES, 65_507);
    assert_eq!(MAX_REPLIES_PER_CALL, 1024);
}

// ---- fix round 1

#[test]
fn identical_messages_are_judged_by_arrival_not_by_when_they_are_handled() {
    use openlab_engine::osc::Osc as O;
    let show = Show::open();
    let mut osc: O = Osc::new();
    let go = bytes(&msg("/go", vec![]));
    // Two GOs 250 ms apart, handled back to back (the task was busy): both fire.
    let handled = T0 + Duration::from_secs(1);
    let first = osc.handle_datagram_arrived(udp(), handled, T0, &go, &show);
    let second =
        osc.handle_datagram_arrived(udp(), handled, T0 + Duration::from_millis(250), &go, &show);
    assert_eq!((first.inputs.len(), second.inputs.len()), (1, 1));
    // Two that arrived 50 ms apart are a repeat, however late they are handled.
    let third = osc.handle_datagram_arrived(
        udp(),
        handled + Duration::from_secs(5),
        T0 + Duration::from_millis(300),
        &go,
        &show,
    );
    assert!(third.inputs.is_empty());
    // And over a stream.
    let mut decoder = SlipDecoder::default();
    let frame = slip_encode(&go);
    let a = osc.handle_stream_arrived(tcp_peer(1), handled, T0, &mut decoder, &frame, &show);
    let b = osc.handle_stream_arrived(
        tcp_peer(1),
        handled,
        T0 + Duration::from_millis(250),
        &mut decoder,
        &frame,
        &show,
    );
    assert_eq!((a.inputs.len(), b.inputs.len()), (1, 1));
}

#[test]
fn the_match_budget_is_one_per_read_and_a_go_behind_a_hostile_bundle_still_fires() {
    let numbers: Vec<String> = (0..10_000).map(|_| "a".repeat(200)).collect();
    let big = Numbered(numbers);
    let mut osc = quiet_osc();
    // 100 distinct patterns with long classes, each non-matching: unbounded they would take
    // seconds; the one budget of the read stops them all.
    let class: String = ('a'..='z')
        .chain('A'..='Z')
        .map(|c| format!("{c}-{c}"))
        .collect();
    let elements: Vec<Vec<u8>> = (0..100)
        .map(|i| bytes(&msg(&format!("/cue/*a*[{class}]Z{i}/name"), vec![])))
        .collect();
    let packet = bundle(&elements);
    let started = std::time::Instant::now();
    let out = osc.handle_datagram(udp(), T0, &packet, &big);
    let took = started.elapsed();
    assert!(took < Duration::from_millis(1500), "{took:?}");
    assert_eq!(out.replies.len(), 100, "each is an error reply");
    assert!(osc.stats().matches_capped >= 1);
    // The next read has its own budget, and a GO behind the bundle is not held up or dropped.
    let show = Show::open();
    let go = osc.handle_datagram(udp(), T0, &bytes(&msg("/go", vec![])), &show);
    assert_eq!(go.inputs.len(), 1);
    let read = osc.handle_datagram(udp(), T0, &bytes(&msg("/cue/*/armed", vec![])), &show);
    assert!(!read.replies.is_empty());
}

#[test]
fn a_class_costs_by_its_ranges() {
    let compiled = CuePattern::compile("*[a-ab-bc-cd-de-e]Z").unwrap();
    let mut few = MATCH_BUDGET;
    let plain = CuePattern::compile("*aZ").unwrap();
    let mut plain_budget = MATCH_BUDGET;
    let _ = compiled.is_match("xxxxxxxxxx", &mut few);
    let _ = plain.is_match("xxxxxxxxxx", &mut plain_budget);
    assert!(MATCH_BUDGET - few > 2 * (MATCH_BUDGET - plain_budget));
}

// ---- pattern language

fn matches(pattern: &str, text: &str) -> bool {
    let mut budget = MATCH_BUDGET;
    CuePattern::compile(pattern)
        .unwrap()
        .is_match(text, &mut budget)
        .unwrap()
}

#[test]
fn the_pattern_language_is_osc_s() {
    assert!(matches("1", "1") && !matches("1", "10"));
    assert!(matches("*", "") && matches("*", "anything"));
    assert!(matches("1*", "1") && matches("1*", "1.5") && !matches("1*", "21"));
    assert!(matches("*5", "2.5") && !matches("*5", "2.51"));
    assert!(matches("a*b*c", "aXbYc") && !matches("a*b*c", "aXbY"));
    assert!(matches("?", "x") && !matches("?", "") && !matches("?", "xy"));
    assert!(matches("1?5", "1.5") && !matches("1?5", "15"));
    assert!(matches("[ab]", "a") && !matches("[ab]", "c") && !matches("[ab]", "ab"));
    assert!(matches("[a-c]", "b") && !matches("[a-c]", "d"));
    assert!(
        matches("[c-a]", "b"),
        "a reversed range is read the right way round"
    );
    assert!(matches("[!a-c]", "d") && !matches("[!a-c]", "b"));
    assert!(matches("[a-]", "-") && matches("[-a]", "-"));
    assert!(matches("{1,2}", "2") && !matches("{1,2}", "3"));
    assert!(matches("{a,}", "") && matches("{a,}", "a"));
    assert!(matches("x{1,2}y{3,4}", "x2y3") && !matches("x{1,2}y{3,4}", "x2y5"));
    assert!(matches("{a*,b?}", "abc") && matches("{a*,b?}", "bc"));
    assert!(matches("[0-9]*{.5,}", "12.5"));
    // Unicode: a character is a character, not a byte.
    assert!(matches("?", "é") && matches("[é]", "é"));
    for bad in ["[", "[]", "[a", "{", "{a", "}", "]", "{a{b}}", "a]"] {
        assert!(CuePattern::compile(bad).is_err(), "{bad}");
    }
    assert!(CuePattern::compile(&"{a,b}".repeat(7)).is_err());
    assert!(CuePattern::compile(&"{a,b}".repeat(6)).is_ok());
    // A budget of nothing is spent at once.
    let compiled = CuePattern::compile("*a").unwrap();
    let mut none = 0;
    assert!(compiled.is_match("aaaa", &mut none).is_err());
}

// ---- address parsing

#[test]
fn addresses_parse_to_the_documented_forms() {
    use openlab_engine::osc::{Address, CueRef, Query, Verb};
    let parse = |a: &str| parse_address(a).unwrap().address;
    assert_eq!(parse("/stop"), Address::Stop);
    assert_eq!(parse("/hardStop"), Address::HardStop);
    assert_eq!(parse("/version"), Address::Version);
    assert_eq!(parse("/workspaces"), Address::Workspaces);
    assert_eq!(parse("/updates"), Address::Updates);
    assert_eq!(
        parse("/playheadId"),
        Address::Query(Query::PlayheadId { deprecated: true })
    );
    assert_eq!(
        parse("/playheadID"),
        Address::Query(Query::PlayheadId { deprecated: false })
    );
    assert_eq!(
        parse("/cue/{1,2}/start"),
        Address::Cue {
            cue: CueRef::Pattern("{1,2}".into()),
            verb: Verb::Start
        }
    );
    assert_eq!(
        parse("/cue/selected/name"),
        Address::Cue {
            cue: CueRef::Selected,
            verb: Verb::Field(CueField::Name)
        }
    );
    // Application-level addresses are not available under a workspace prefix.
    let p = parse_address("/workspace/X/version").unwrap();
    assert_eq!(p.workspace.as_deref(), Some("X"));
    assert_eq!(p.address, Address::Other("/version".into()));
    // A pattern anywhere but the cue number is not expanded.
    assert_eq!(parse("/g*"), Address::Pattern("/g*".into()));
    assert_eq!(parse("/cue/1/st*"), Address::Pattern("/cue/1/st*".into()));
    // Cue ids: upper case only.
    assert!(matches!(
        parse("/cue_id/0000000A-0000-0000-0000-000000000000/start"),
        Address::Cue {
            cue: CueRef::Id(_),
            ..
        }
    ));
    assert_eq!(
        parse("/cue_id/0000000a-0000-0000-0000-000000000000/start"),
        Address::Unsupported("not a cue id")
    );
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
    let mut osc = quiet_osc();
    let mut decoder = SlipDecoder::default();
    let tcp = tcp_peer(1);
    let mut data = slip_encode(&bytes(&msg("/go", vec![])));
    data.extend(slip_encode(&bytes(&msg("/cue/1/name", vec![]))));
    data.extend(slip_encode(b"this is not osc"));
    data.push(0xC0);
    data.extend(std::iter::repeat_n(1u8, MAX_FRAME_BYTES + 10));
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
    let mut osc = quiet_osc();
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
    let mut osc = quiet_osc();
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
    let mut osc = quiet_osc();
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
    let mut osc = quiet_osc();
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
        decode_packet(&vec![0; MAX_FRAME_BYTES + 1]),
        Err(DecodeError::TooLarge { .. })
    ));
    // A datagram over what UDP can carry is malformed; a frame of that size over TCP is not.
    let before = osc.stats().malformed;
    let out = osc.handle_datagram(udp(), T0, &vec![0; MAX_PACKET_BYTES + 1], &show);
    assert_eq!(out, Outcome::default());
    assert_eq!(osc.stats().malformed, before + 1);
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
        let mut osc = quiet_osc();
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
        let mut osc = quiet_osc();
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
        let mut osc = quiet_osc();
        let _ = osc.handle_datagram(udp(), T0, &packet, &Show::open());
    }

    #[test]
    fn arbitrary_patterns_and_texts_never_panic_and_stay_in_budget(
        pattern in "[a-c*?\\[\\]{},!-]{0,40}",
        text in "[a-c.]{0,30}",
    ) {
        if let Ok(compiled) = CuePattern::compile(&pattern) {
            let mut budget = MATCH_BUDGET;
            let _ = compiled.is_match(&text, &mut budget);
            prop_assert!(budget <= MATCH_BUDGET);
        }
    }

    #[test]
    fn a_plain_text_matches_itself_and_star_but_not_something_longer(text in "[a-z0-9.]{1,20}") {
        let mut budget = MATCH_BUDGET;
        let exact = CuePattern::compile(&text).unwrap();
        prop_assert_eq!(exact.is_match(&text, &mut budget), Ok(true));
        prop_assert_eq!(exact.is_match(&format!("{text}x"), &mut budget), Ok(false));
        let star = CuePattern::compile("*").unwrap();
        prop_assert_eq!(star.is_match(&text, &mut budget), Ok(true));
        let prefixed = CuePattern::compile(&format!("{}*", &text[..1])).unwrap();
        prop_assert_eq!(prefixed.is_match(&text, &mut budget), Ok(true));
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
        let mut osc = quiet_osc();
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
