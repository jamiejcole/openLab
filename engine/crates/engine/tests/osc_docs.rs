// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The socket-free OSC layer over several open docs (M4.15h, ARCHITECTURE section 7): an
//! un-prefixed message goes to every doc and each answers for itself, `/workspace/{id|name}`
//! picks one, `/connect` and its sessions and lockouts are per peer and doc, the reply budgets
//! per IP, and the per-call caps bound the fan-out.

use std::net::{IpAddr, Ipv4Addr};
use std::ops::ControlFlow;
use std::time::Duration;

use openlab_core::protocol::{DocId, TransportAction};
use openlab_core::{CueId, CueListId, OscAccess, OscPasscode, OscRights, WorkspaceId};
use openlab_engine::osc::{
    CueField, CuePattern, DocShow, GLOBAL_MAX_FAILURES, LOCKOUT, MATCH_BUDGET, MAX_BAD_PASSCODES,
    MAX_REPLIES_PER_CALL, Osc, OscInput, OscShow, Outcome, Peer, Target, encode_message, upper,
};
use rosc::{OscMessage, OscType};
use serde_json::{Value, json};
use uuid::Uuid;

const A: DocId = DocId(3);
const B: DocId = DocId(5);
const T0: Duration = Duration::from_secs(100);

/// A doc with `cues` cues numbered 1..=cues in one list, its own id, name and passcode table.
struct Show {
    id: WorkspaceId,
    name: String,
    access: OscAccess,
    list: CueListId,
    cues: u128,
}

impl Show {
    fn new(n: u128, name: &str, cues: u128) -> Self {
        Self {
            id: WorkspaceId(Uuid::from_u128(n)),
            name: name.to_string(),
            access: OscAccess::default(),
            list: CueListId(Uuid::from_u128(n << 16)),
            cues,
        }
    }

    fn locked(mut self, passcode: &str) -> Self {
        self.access = OscAccess {
            no_passcode: OscRights::NONE,
            passcodes: vec![OscPasscode {
                passcode: passcode.into(),
                rights: OscRights::ALL,
            }],
        };
        self
    }

    fn cue(&self, number: u128) -> CueId {
        CueId(Uuid::from_u128((self.id.0.as_u128() << 32) + number))
    }

    fn number(&self, id: CueId) -> Option<u128> {
        let n = id.0.as_u128().checked_sub(self.id.0.as_u128() << 32)?;
        (1..=self.cues).contains(&n).then_some(n)
    }
}

impl OscShow for Show {
    fn workspace_id(&self) -> WorkspaceId {
        self.id
    }
    fn workspace_name(&self) -> &str {
        &self.name
    }
    fn access(&self) -> &OscAccess {
        &self.access
    }
    fn active_list(&self) -> Option<CueListId> {
        Some(self.list)
    }
    fn cue_by_number(&self, number: &str) -> Option<CueId> {
        let n: u128 = number.parse().ok()?;
        (1..=self.cues).contains(&n).then(|| self.cue(n))
    }
    fn cue_exists(&self, id: CueId) -> bool {
        self.number(id).is_some()
    }
    fn cue_property(&self, id: CueId, field: CueField) -> Option<Value> {
        let n = self.number(id)?;
        match field {
            CueField::Name => Some(json!(format!("{} {n}", self.name))),
            CueField::Number => Some(json!(n.to_string())),
            _ => None,
        }
    }
    fn cue_by_osc_trigger(&self, _address: &str) -> Option<CueId> {
        None
    }
    fn top_level_list(&self, cue: CueId) -> Option<CueListId> {
        self.cue_exists(cue).then_some(self.list)
    }
    fn visit_targets(&self, visit: &mut dyn FnMut(Target, &str) -> ControlFlow<()>) {
        for n in 1..=self.cues {
            if visit(Target::Cue(self.cue(n)), &n.to_string()).is_break() {
                return;
            }
        }
    }
}

fn hamlet() -> Show {
    Show::new(0xA1, "Hamlet", 3)
}

fn lear() -> Show {
    Show::new(0xB2, "Lear", 3)
}

fn docs<'a>(a: &'a Show, b: &'a Show) -> [DocShow<'a>; 2] {
    [DocShow { doc: A, show: a }, DocShow { doc: B, show: b }]
}

fn quiet() -> Osc {
    let mut osc = Osc::new();
    osc.set_duplicate_window(Duration::ZERO);
    osc
}

fn udp() -> Peer {
    Peer::Udp(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7)))
}

fn msg(addr: &str, args: Vec<OscType>) -> OscMessage {
    OscMessage {
        addr: addr.into(),
        args,
    }
}

fn s(text: &str) -> OscType {
    OscType::String(text.into())
}

fn send(osc: &mut Osc, docs: &[DocShow<'_>], peer: Peer, m: &OscMessage) -> Outcome {
    osc.handle_datagram_docs(peer, T0, T0, &encode_message(m).unwrap(), docs)
}

fn bodies(out: &Outcome) -> Vec<Value> {
    out.replies
        .iter()
        .map(|r| match r.args.as_slice() {
            [OscType::String(json)] => serde_json::from_str(json).unwrap(),
            other => panic!("{other:?}"),
        })
        .collect()
}

fn ws(show: &Show) -> String {
    upper(show.id)
}

#[test]
fn an_unprefixed_go_fires_every_doc() {
    let (a, b) = (hamlet(), lear());
    let mut osc = quiet();
    let out = send(&mut osc, &docs(&a, &b), udp(), &msg("/go", vec![]));
    assert!(out.replies.is_empty(), "{out:?}");
    let routed: Vec<(DocId, OscInput)> = out.routed().collect();
    assert_eq!(
        routed,
        vec![
            (A, OscInput::Transport(TransportAction::Go { list: a.list })),
            (B, OscInput::Transport(TransportAction::Go { list: b.list })),
        ]
    );
}

#[test]
fn an_unprefixed_query_gets_one_reply_per_doc_each_naming_its_workspace() {
    let (a, b) = (hamlet(), lear());
    let mut osc = quiet();
    let out = send(&mut osc, &docs(&a, &b), udp(), &msg("/cue/2/name", vec![]));
    let replies = bodies(&out);
    assert_eq!(replies.len(), 2, "{replies:?}");
    for (reply, show) in replies.iter().zip([&a, &b]) {
        assert_eq!(reply["status"], "ok");
        assert_eq!(reply["workspace_id"], ws(show));
        assert_eq!(
            reply["address"],
            format!("/workspace/{}/cue/2/name", ws(show))
        );
        assert_eq!(reply["data"], format!("{} 2", show.name));
    }
    // Both on the address asked.
    assert!(out.replies.iter().all(|r| r.addr == "/reply/cue/2/name"));
}

#[test]
fn a_doc_where_the_message_fails_answers_with_an_error_of_its_own() {
    let a = hamlet();
    // Lear has one cue: its cue 3 does not exist.
    let b = Show::new(0xB2, "Lear", 1);
    let mut osc = quiet();
    let replies = bodies(&send(
        &mut osc,
        &docs(&a, &b),
        udp(),
        &msg("/cue/3/name", vec![]),
    ));
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0]["status"], "ok");
    assert_eq!(replies[1]["status"], "error");
    assert_eq!(replies[1]["workspace_id"], ws(&b));
    // An action: the doc that has the cue starts it, the other answers an error.
    let out = send(&mut osc, &docs(&a, &b), udp(), &msg("/cue/3/start", vec![]));
    assert_eq!(out.input_docs, vec![A]);
    let replies = bodies(&out);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["workspace_id"], ws(&b));
}

#[test]
fn the_prefix_picks_one_doc_by_id_or_by_name() {
    let (a, b) = (hamlet(), lear());
    let mut osc = quiet();
    let by_id = send(
        &mut osc,
        &docs(&a, &b),
        udp(),
        &msg(&format!("/workspace/{}/go", ws(&b)), vec![]),
    );
    assert_eq!(by_id.input_docs, vec![B]);
    let by_name = send(
        &mut osc,
        &docs(&a, &b),
        udp(),
        &msg("/workspace/Hamlet/cue/1/name", vec![]),
    );
    let replies = bodies(&by_name);
    assert_eq!(replies.len(), 1);
    // The reply names the workspace by its id, whatever the prefix said.
    assert_eq!(
        replies[0]["address"],
        format!("/workspace/{}/cue/1/name", ws(&a))
    );
    assert_eq!(
        by_name.replies[0].addr,
        "/reply/workspace/Hamlet/cue/1/name"
    );
}

#[test]
fn an_id_another_doc_has_as_its_name_picks_the_doc_with_that_id() {
    let a = hamlet();
    let mut b = lear();
    let id = ws(&a);
    // An owned name, not a leaked one: LeakSanitizer fails the binary on a `Box::leak`.
    b.name = id.clone();
    let mut osc = quiet();
    let out = send(
        &mut osc,
        &docs(&a, &b),
        udp(),
        &msg(&format!("/workspace/{id}/go"), vec![]),
    );
    assert_eq!(out.input_docs, vec![A]);
}

#[test]
fn an_unknown_workspace_is_one_bare_error() {
    let (a, b) = (hamlet(), lear());
    let mut osc = quiet();
    for addr in [
        "/workspace/NOPE/go".to_owned(),
        // The right id in lower case, as measured in QLab.
        format!("/workspace/{}/go", ws(&a).to_lowercase()),
        // A name is matched exactly.
        "/workspace/hamlet/go".to_owned(),
    ] {
        let out = send(&mut osc, &docs(&a, &b), udp(), &msg(&addr, vec![]));
        assert!(out.inputs.is_empty(), "{addr}");
        let replies = bodies(&out);
        assert_eq!(replies, vec![json!({"status": "error", "address": addr})]);
    }
    assert_eq!(osc.stats().wrong_workspace, 3);
}

#[test]
fn with_no_doc_open_a_workspace_message_errors_and_the_list_is_empty() {
    let mut osc = quiet();
    for addr in ["/go", "/cue/1/name", "/connect", "/cueLists"] {
        let out = send(&mut osc, &[], udp(), &msg(addr, vec![]));
        assert!(out.inputs.is_empty());
        assert_eq!(
            bodies(&out),
            vec![json!({"status": "error", "address": addr})],
            "{addr}"
        );
    }
    let out = send(&mut osc, &[], udp(), &msg("/workspaces", vec![]));
    assert_eq!(bodies(&out)[0]["data"], json!([]));
    // `/updates` has nothing to subscribe to.
    let out = send(
        &mut osc,
        &[],
        udp(),
        &msg("/updates", vec![OscType::Int(1)]),
    );
    assert_eq!(
        bodies(&out),
        vec![json!({"status": "error", "address": "/updates"})]
    );
    assert!(osc.update_subscribers(T0).is_empty());
}

#[test]
fn workspaces_lists_every_doc_in_the_order_given() {
    let (a, b) = (hamlet(), lear());
    let mut osc = quiet();
    let out = send(&mut osc, &docs(&a, &b), udp(), &msg("/workspaces", vec![]));
    let replies = bodies(&out);
    assert_eq!(replies.len(), 1);
    assert!(replies[0].get("workspace_id").is_none());
    let names: Vec<&str> = replies[0]["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["displayName"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Hamlet", "Lear"]);
    // `/version` too is answered once, whatever the docs.
    let out = send(&mut osc, &docs(&a, &b), udp(), &msg("/version", vec![]));
    assert_eq!(out.replies.len(), 1);
}

#[test]
fn version_and_workspaces_need_no_passcode_with_every_doc_locked() {
    let (a, b) = (hamlet().locked("1111"), lear().locked("2222"));
    let mut osc = quiet();
    for addr in ["/version", "/workspaces"] {
        let replies = bodies(&send(&mut osc, &docs(&a, &b), udp(), &msg(addr, vec![])));
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0]["status"], "ok", "{addr}");
    }
}

#[test]
fn connect_to_one_doc_leaves_the_other_locked() {
    let (a, b) = (hamlet().locked("1111"), lear().locked("2222"));
    let mut osc = quiet();
    let d = docs(&a, &b);
    let out = send(
        &mut osc,
        &d,
        udp(),
        &msg(&format!("/workspace/{}/connect", ws(&a)), vec![s("1111")]),
    );
    assert_eq!(bodies(&out)[0]["data"], "ok:view|edit|control");
    assert!(osc.is_connected_in(A, udp()));
    assert!(!osc.is_connected_in(B, udp()));
    let out = send(&mut osc, &d, udp(), &msg("/go", vec![]));
    assert_eq!(out.input_docs, vec![A]);
    let replies = bodies(&out);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["status"], "denied");
    assert_eq!(replies[0]["workspace_id"], ws(&b));
}

/// A pattern address over a connected doc A and a locked doc B (#188): A's matching cues start,
/// B is denied once and contributes no input, whichever way the pattern is addressed.
#[test]
fn a_cue_pattern_over_a_connected_and_a_locked_doc_starts_only_the_connected_one() {
    let (a, b) = (hamlet().locked("1111"), lear().locked("2222"));
    let d = docs(&a, &b);
    for (address, addresses_b) in [
        ("/cue/*/start".to_string(), true),
        (format!("/workspace/{}/cue/*/start", ws(&a)), false),
        (format!("/workspace/{}/cue/*/start", ws(&b)), true),
    ] {
        let mut osc = quiet();
        send(
            &mut osc,
            &d,
            udp(),
            &msg(&format!("/workspace/{}/connect", ws(&a)), vec![s("1111")]),
        );
        let out = send(&mut osc, &d, udp(), &msg(&address, vec![]));
        let to_a = !address.contains(&ws(&b));
        assert!(
            out.input_docs.iter().all(|&doc| doc == A),
            "{address}: a locked doc got an input: {:?}",
            out.input_docs
        );
        // Cues 1..=3 of A start when A is addressed.
        assert_eq!(out.input_docs.len(), if to_a { 3 } else { 0 }, "{address}");
        let replies = bodies(&out);
        let denied: Vec<&Value> = replies.iter().filter(|r| r["status"] == "denied").collect();
        if addresses_b {
            assert_eq!(denied.len(), 1, "{address}: {replies:?}");
            assert_eq!(denied[0]["workspace_id"], ws(&b), "{address}");
        } else {
            assert!(denied.is_empty(), "{address}: {replies:?}");
        }
    }
}

#[test]
fn an_unprefixed_connect_is_answered_by_every_doc() {
    let (a, b) = (hamlet().locked("1111"), lear().locked("2222"));
    let mut osc = quiet();
    let out = send(
        &mut osc,
        &docs(&a, &b),
        udp(),
        &msg("/connect", vec![s("2222")]),
    );
    let replies = bodies(&out);
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0]["data"], "badpass");
    assert_eq!(replies[0]["workspace_id"], ws(&a));
    assert_eq!(replies[1]["data"], "ok:view|edit|control");
    assert_eq!(replies[1]["workspace_id"], ws(&b));
    assert!(!osc.is_connected_in(A, udp()));
    assert!(osc.is_connected_in(B, udp()));
}

/// Wrong guesses are counted per address across docs: trying the docs in turn is no fresh budget.
#[test]
fn a_peers_wrong_passcodes_lock_it_out_of_every_doc_together() {
    let (a, b) = (hamlet().locked("1111"), lear().locked("2222"));
    let mut osc = quiet();
    let d = docs(&a, &b);
    let to = |show: &Show, verb: &str| format!("/workspace/{}/{verb}", ws(show));
    for i in 0..MAX_BAD_PASSCODES {
        let show = if i % 2 == 0 { &a } else { &b };
        send(
            &mut osc,
            &d,
            udp(),
            &msg(&to(show, "connect"), vec![s("0000")]),
        );
    }
    // Locked at both, though neither doc saw five guesses.
    for (show, code) in [(&a, "1111"), (&b, "2222")] {
        let out = send(
            &mut osc,
            &d,
            udp(),
            &msg(&to(show, "connect"), vec![s(code)]),
        );
        assert_eq!(bodies(&out)[0]["status"], "denied", "{}", show.name);
    }
    // The lockout ends on its own clock, for both.
    let later = T0 + LOCKOUT;
    let out = osc.handle_datagram_docs(
        udp(),
        later,
        later,
        &encode_message(&msg(&to(&b, "connect"), vec![s("2222")])).unwrap(),
        &d,
    );
    assert_eq!(bodies(&out)[0]["data"], "ok:view|edit|control");
}

/// The engine-wide failure window is not one per doc: guesses from many addresses at several
/// docs add up.
#[test]
fn the_global_failure_budget_is_shared_by_every_doc() {
    let (a, b) = (hamlet().locked("1111"), lear().locked("2222"));
    let mut osc = quiet();
    let d = docs(&a, &b);
    let from = |n: usize| {
        Peer::Udp(IpAddr::V4(Ipv4Addr::new(
            10,
            1,
            (n / 200) as u8,
            (n % 200) as u8,
        )))
    };
    for n in 0..GLOBAL_MAX_FAILURES as usize {
        let show = if n % 2 == 0 { &a } else { &b };
        let addr = format!("/workspace/{}/connect", ws(show));
        send(&mut osc, &d, from(n), &msg(&addr, vec![s("0000")]));
    }
    // A stranger with the right passcode is refused at the doc that saw only half the guesses.
    let addr = format!("/workspace/{}/connect", ws(&a));
    let out = send(&mut osc, &d, from(999), &msg(&addr, vec![s("1111")]));
    assert_eq!(bodies(&out)[0]["status"], "denied", "{out:?}");
}

/// One un-prefixed guess goes to every doc but is one failure.
#[test]
fn an_unprefixed_wrong_guess_counts_once_however_many_docs_answer() {
    let (a, b) = (hamlet().locked("1111"), lear().locked("2222"));
    let mut osc = quiet();
    let d = docs(&a, &b);
    for _ in 0..MAX_BAD_PASSCODES - 1 {
        send(&mut osc, &d, udp(), &msg("/connect", vec![s("0000")]));
    }
    let out = send(
        &mut osc,
        &d,
        udp(),
        &msg(&format!("/workspace/{}/connect", ws(&a)), vec![s("1111")]),
    );
    assert_eq!(bodies(&out)[0]["data"], "ok:view|edit|control", "{out:?}");
}

/// `/updates` and `/udpReplyPort` are the peer's, not a doc's: a doc opened after them, or after
/// the doc that was open then closed, has them too.
#[test]
fn updates_and_the_reply_port_follow_the_peer_to_docs_opened_later() {
    let (a, b) = (hamlet(), lear());
    let mut osc = quiet();
    send(
        &mut osc,
        &[DocShow { doc: A, show: &a }],
        udp(),
        &msg("/updates", vec![OscType::Int(1)]),
    );
    send(
        &mut osc,
        &[DocShow { doc: A, show: &a }],
        udp(),
        &msg("/udpReplyPort", vec![OscType::Int(54329)]),
    );
    // B opens: before any packet reaches it, its updates have the subscriber.
    osc.open_doc(B, T0, b.access());
    assert_eq!(osc.update_subscribers_in(B, T0), vec![udp()]);
    // A closes; the port is still the peer's.
    osc.close_doc(A);
    assert_eq!(osc.reply_port(udp(), T0), Some(54329));
    // A third doc whose first sight is a packet.
    let c = Show::new(0xC3, "Macbeth", 1);
    send(
        &mut osc,
        &[DocShow {
            doc: DocId(7),
            show: &c,
        }],
        udp(),
        &msg("/version", vec![]),
    );
    assert_eq!(osc.update_subscribers_in(DocId(7), T0), vec![udp()]);
    // Unsubscribing reaches docs opened later too.
    send(
        &mut osc,
        &[DocShow { doc: B, show: &b }],
        udp(),
        &msg("/updates", vec![OscType::Int(0)]),
    );
    osc.open_doc(DocId(8), T0, c.access());
    assert!(osc.update_subscribers_in(DocId(8), T0).is_empty());
    assert_eq!(osc.reply_port(udp(), T0), Some(54329));
}

#[test]
fn closing_a_doc_ends_its_sessions_and_a_reopened_one_starts_afresh() {
    let (a, b) = (hamlet().locked("1111"), lear().locked("1111"));
    let mut osc = quiet();
    send(
        &mut osc,
        &docs(&a, &b),
        udp(),
        &msg("/connect", vec![s("1111")]),
    );
    send(
        &mut osc,
        &docs(&a, &b),
        udp(),
        &msg("/updates", vec![OscType::Int(1)]),
    );
    assert!(osc.is_connected_in(A, udp()) && osc.is_connected_in(B, udp()));
    osc.close_doc(B);
    assert!(!osc.is_connected_in(B, udp()));
    assert!(osc.update_subscribers_in(B, T0).is_empty());
    assert_eq!(osc.known_ips_in(B), 0);
    assert!(osc.is_connected_in(A, udp()));
    assert_eq!(osc.update_subscribers_in(A, T0), vec![udp()]);
    // The same show opened again is another doc: it must be connected to again.
    let reopened = [
        DocShow { doc: A, show: &a },
        DocShow {
            doc: DocId(9),
            show: &b,
        },
    ];
    let out = send(&mut osc, &reopened, udp(), &msg("/cue/1/name", vec![]));
    let statuses: Vec<Value> = bodies(&out).iter().map(|r| r["status"].clone()).collect();
    assert_eq!(statuses, vec![json!("ok"), json!("denied")]);
}

#[test]
fn a_doc_left_out_of_a_call_is_forgotten() {
    let (a, b) = (hamlet().locked("1111"), lear().locked("1111"));
    let mut osc = quiet();
    send(
        &mut osc,
        &docs(&a, &b),
        udp(),
        &msg("/connect", vec![s("1111")]),
    );
    send(
        &mut osc,
        &[DocShow { doc: A, show: &a }],
        udp(),
        &msg("/version", vec![]),
    );
    assert!(!osc.is_connected_in(B, udp()));
    assert!(osc.is_connected_in(A, udp()));
}

#[test]
fn updates_subscribe_in_every_doc_and_each_doc_sends_only_where_the_peer_may_view() {
    let a = hamlet();
    let b = lear().locked("2222");
    let mut osc = quiet();
    let out = send(
        &mut osc,
        &docs(&a, &b),
        udp(),
        &msg("/updates", vec![OscType::Int(1)]),
    );
    assert!(out.replies.is_empty(), "no reply: {out:?}");
    assert_eq!(osc.update_subscribers_in(A, T0), vec![udp()]);
    assert_eq!(osc.update_subscribers_in(B, T0), vec![udp()]);
    assert!(osc.may_receive_updates(A, udp(), &a.access));
    assert!(!osc.may_receive_updates(B, udp(), &b.access));
    send(
        &mut osc,
        &docs(&a, &b),
        udp(),
        &msg(&format!("/workspace/{}/connect", ws(&b)), vec![s("2222")]),
    );
    assert!(osc.may_receive_updates(B, udp(), &b.access));
    // `/updates 0` ends both.
    send(
        &mut osc,
        &docs(&a, &b),
        udp(),
        &msg("/updates", vec![OscType::Int(0)]),
    );
    assert!(osc.update_subscribers(T0).is_empty());
}

#[test]
fn a_prefixed_updates_is_an_error_and_subscribes_nothing() {
    let (a, b) = (hamlet(), lear());
    let mut osc = quiet();
    let addr = format!("/workspace/{}/updates", ws(&a));
    let out = send(
        &mut osc,
        &docs(&a, &b),
        udp(),
        &msg(&addr, vec![OscType::Int(1)]),
    );
    let replies = bodies(&out);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["status"], "error");
    assert!(osc.update_subscribers(T0).is_empty());
}

#[test]
fn the_reply_port_is_set_in_every_doc_the_peer_may_view() {
    let a = hamlet();
    let b = lear().locked("2222");
    let mut osc = quiet();
    let out = send(
        &mut osc,
        &docs(&a, &b),
        udp(),
        &msg("/udpReplyPort", vec![OscType::Int(54321)]),
    );
    assert!(out.replies.is_empty());
    assert_eq!(osc.reply_port(udp(), T0), Some(54321));
    // With View nowhere it is denied, once, without a workspace id.
    let (c, d) = (hamlet().locked("1"), lear().locked("2"));
    let mut osc = quiet();
    let out = send(
        &mut osc,
        &docs(&c, &d),
        udp(),
        &msg("/udpReplyPort", vec![OscType::Int(54321)]),
    );
    assert_eq!(
        bodies(&out),
        vec![json!({"status": "denied", "address": "/udpReplyPort"})]
    );
    assert_eq!(osc.reply_port(udp(), T0), None);
}

#[test]
fn the_reply_cap_bounds_a_wildcard_over_every_doc_together() {
    // Each doc alone fits the cap; the two together do not.
    let per_doc = u128::try_from(MAX_REPLIES_PER_CALL).unwrap() * 3 / 4;
    let (a, b) = (
        Show::new(0xA1, "Hamlet", per_doc),
        Show::new(0xB2, "Lear", per_doc),
    );
    let mut osc = quiet();
    let peer = Peer::Tcp {
        conn: 1,
        ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
    };
    let out = send(&mut osc, &docs(&a, &b), peer, &msg("/cue/*/name", vec![]));
    assert_eq!(out.replies.len(), MAX_REPLIES_PER_CALL);
    let dropped = usize::try_from(osc.stats().replies_dropped).unwrap();
    assert_eq!(
        dropped,
        2 * usize::try_from(per_doc).unwrap() - MAX_REPLIES_PER_CALL
    );
    // An action over both is held to the input cap the same way.
    let out = send(&mut osc, &docs(&a, &b), peer, &msg("/cue/*/start", vec![]));
    assert_eq!(out.inputs.len(), openlab_engine::osc::MAX_INPUTS_PER_CALL);
    assert_eq!(out.input_docs.len(), out.inputs.len());
    assert!(osc.stats().inputs_capped > 0);
}

/// The matching steps a pattern costs over cue numbers 1..=n (what one doc's `/cue/{pattern}`
/// spends).
fn cost(pattern: &str, n: u128) -> u64 {
    let compiled = CuePattern::compile(pattern).unwrap();
    let mut budget = MATCH_BUDGET;
    for k in 1..=n {
        let _ = compiled.is_match(&k.to_string(), &mut budget);
    }
    MATCH_BUDGET - budget
}

#[test]
fn the_matching_budget_is_shared_by_every_doc() {
    // 64 alternatives that never match: each number costs a lot. Find a doc size that spends
    // more than half the budget but not all of it, so one doc fits and two do not.
    let pattern = "{*,**}{*,**}{*,**}{*,**}{*,**}{*,**}*Z";
    let (mut lo, mut hi) = (1u128, 1u128 << 20);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if cost(pattern, mid) * 2 > MATCH_BUDGET {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    assert!(
        cost(pattern, lo) < MATCH_BUDGET,
        "the doc size step is too coarse"
    );
    let (a, b) = (Show::new(0xA1, "Hamlet", lo), Show::new(0xB2, "Lear", lo));
    let address = format!("/cue/{pattern}/name");
    let mut osc = quiet();
    send(
        &mut osc,
        &[DocShow { doc: A, show: &a }],
        udp(),
        &msg(&address, vec![]),
    );
    assert_eq!(osc.stats().matches_capped, 0, "one doc fits the budget");
    let out = send(&mut osc, &docs(&a, &b), udp(), &msg(&address, vec![]));
    assert_eq!(
        osc.stats().matches_capped,
        1,
        "the second doc found it spent"
    );
    // Each doc still answers (no match: an error each).
    assert_eq!(out.replies.len(), 2);
}

/// Review M1: a console connecting to two locked docs with their two passcodes, un-prefixed,
/// is not guessing: the passcode one doc accepts must not count as a wrong one in the other,
/// or after five rounds it is locked out of the running show.
#[test]
fn a_passcode_another_doc_accepts_is_no_failed_guess() {
    let (a, b) = (hamlet().locked("1111"), lear().locked("2222"));
    let mut osc = quiet();
    let d = docs(&a, &b);
    for _ in 0..2 * MAX_BAD_PASSCODES {
        for code in ["1111", "2222"] {
            let replies = bodies(&send(&mut osc, &d, udp(), &msg("/connect", vec![s(code)])));
            assert!(replies.iter().all(|r| r["status"] == "ok"), "{replies:?}");
        }
    }
    let out = send(
        &mut osc,
        &d,
        udp(),
        &msg(&format!("/workspace/{}/go", ws(&a)), vec![]),
    );
    assert_eq!(out.input_docs, vec![A], "{out:?}");
    // A passcode no doc has is still a guess in each.
    let other = Peer::Udp(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 8)));
    for _ in 0..MAX_BAD_PASSCODES {
        send(&mut osc, &d, other, &msg("/connect", vec![s("0000")]));
    }
    let replies = bodies(&send(
        &mut osc,
        &d,
        other,
        &msg("/connect", vec![s("1111")]),
    ));
    assert!(
        replies.iter().all(|r| r["status"] == "denied"),
        "{replies:?}"
    );
}

/// Review M2: a name two open docs share (a Finder copy keeps it) picks neither: the bare error
/// of an unknown workspace, and one warning for the show log per peer and name.
#[test]
fn a_name_several_docs_share_is_refused_with_one_warning() {
    let a = hamlet();
    let mut b = lear();
    b.name = "Hamlet".into();
    let mut osc = quiet();
    let out = send(
        &mut osc,
        &docs(&a, &b),
        udp(),
        &msg("/workspace/Hamlet/go", vec![]),
    );
    assert!(out.inputs.is_empty());
    assert_eq!(
        bodies(&out),
        vec![json!({"status": "error", "address": "/workspace/Hamlet/go"})]
    );
    assert_eq!(out.warnings.len(), 1, "{:?}", out.warnings);
    let again = send(
        &mut osc,
        &docs(&a, &b),
        udp(),
        &msg("/workspace/Hamlet/go", vec![]),
    );
    assert!(again.inputs.is_empty() && again.warnings.is_empty());
    // By id each is still reachable.
    let out = send(
        &mut osc,
        &docs(&a, &b),
        udp(),
        &msg(&format!("/workspace/{}/go", ws(&b)), vec![]),
    );
    assert_eq!(out.input_docs, vec![B]);
}
