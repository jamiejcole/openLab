// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Network cues through the control loop (CT-30, PB-10, NFR-07): the exact OSC bytes on a
//! real local socket, the output override, destinations found unusable before GO, and a
//! destination that fails or never answers cannot stall the loop. Sample time is virtual;
//! the only real-time waits are bounded receive timeouts that exist to fail a broken test.

mod show;

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::net::{TcpListener, UdpSocket};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use openlab_core::protocol::{
    ClientMessage, Outcome, Request, RuntimeChange, ServerMessage, SystemStatus, WireProblem,
    WireWarning,
};
use openlab_core::{
    ContinueMode, CueKind, CueProblem, CueState, NetworkPatch, NetworkPatchId, NetworkProtocol,
    OscArg,
};
use openlab_engine::control::offline::OfflineShow;
use openlab_engine::control::{Destination, NetTransport, Transport};
use openlab_engine::diag::StatusConfig;
use openlab_engine::osc::{SlipDecoder, SlipEvent};
use show::*;
use uuid::Uuid;

/// A failure guard for real sockets, never a pause in a passing test.
const GUARD: Duration = Duration::from_secs(10);

fn net_id() -> NetworkPatchId {
    NetworkPatchId(Uuid::from_u128(1))
}

fn patch(host: &str, port: u16, protocol: NetworkProtocol) -> NetworkPatch {
    NetworkPatch {
        id: net_id(),
        name: "Desk".into(),
        host: host.into(),
        port,
        protocol,
    }
}

fn osc(n: u128, address: &str, args: Vec<OscArg>) -> openlab_core::Cue {
    cue(
        n,
        CueKind::Network {
            destination: net_id(),
            address: address.into(),
            args,
        },
    )
}

fn show_of(
    cues: Vec<openlab_core::Cue>,
    patch: NetworkPatch,
    transport: Option<Arc<Fake>>,
) -> OfflineShow {
    let mut ws = workspace(cues);
    ws.network_patches = vec![patch];
    let mut c = config(&ws);
    if let Some(t) = transport {
        c.net = NetTransport(t);
    }
    let mut s = show_with(ws, c);
    s.settle_net();
    s.take_outbound();
    s
}

/// A transport that records what it is asked to send, answers look-ups from a table, and can
/// be made to fail or to hold a send until released.
#[derive(Default)]
struct Fake {
    sent: Mutex<Vec<(Destination, Vec<u8>)>>,
    fail_sends: Mutex<Option<String>>,
    names: Mutex<HashMap<String, Result<(), String>>>,
    lookups: Mutex<Vec<String>>,
    /// While set, `send` waits for a unit on `release` after announcing itself on `entered`.
    gate: Mutex<Option<Gate>>,
}

struct Gate {
    entered: Sender<()>,
    release: Mutex<Receiver<()>>,
}

impl Fake {
    fn sent(&self) -> Vec<(Destination, Vec<u8>)> {
        self.sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    fn name(&self, host: &str, result: Result<(), String>) {
        self.names.lock().unwrap().insert(host.into(), result);
    }
    fn lookups(&self) -> usize {
        self.lookups.lock().unwrap().len()
    }
    fn fail_with(&self, why: Option<&str>) {
        *self.fail_sends.lock().unwrap() = why.map(str::to_owned);
    }
    /// Holds every send; returns the receiver of "a send began" and the sender that lets one go.
    fn hold(&self) -> (Receiver<()>, Sender<()>) {
        let (entered_tx, entered_rx) = channel();
        let (release_tx, release_rx) = channel();
        *self.gate.lock().unwrap() = Some(Gate {
            entered: entered_tx,
            release: Mutex::new(release_rx),
        });
        (entered_rx, release_tx)
    }
}

impl Transport for Fake {
    fn send(&self, to: &Destination, packet: &[u8]) -> Result<(), String> {
        if let Some(gate) = self.gate.lock().unwrap().as_ref() {
            let _ = gate.entered.send(());
            // Failure guard: a test that forgets to release does not hang forever.
            let _ = gate.release.lock().unwrap().recv_timeout(GUARD);
        }
        if let Some(why) = self.fail_sends.lock().unwrap().clone() {
            return Err(why);
        }
        self.sent
            .lock()
            .unwrap()
            .push((to.clone(), packet.to_vec()));
        Ok(())
    }

    fn resolve(&self, host: &str, _: u16) -> Result<(), String> {
        self.lookups.lock().unwrap().push(host.to_owned());
        self.names
            .lock()
            .unwrap()
            .get(host)
            .cloned()
            .unwrap_or(Ok(()))
    }
}

fn notices(msgs: &[ServerMessage]) -> Vec<(String, String)> {
    warnings(msgs)
        .into_iter()
        .filter_map(|w| match w {
            WireWarning::Notice { code, message, .. } => Some((code, message)),
            _ => None,
        })
        .collect()
}

fn statuses(msgs: Vec<ServerMessage>) -> Vec<SystemStatus> {
    msgs.into_iter()
        .filter_map(|m| match m {
            ServerMessage::Status { status } => Some(*status),
            _ => None,
        })
        .collect()
}

fn override_request(s: &mut OfflineShow, disabled: bool) -> Vec<ServerMessage> {
    let id = s.request_id();
    s.send(ClientMessage::Request {
        id,
        request: Request::SetOutputOverrides {
            network_disabled: disabled,
        },
    })
}

/// `/lx/go ,ifsT 7 0.5 "x"`, written out by hand so the encoder is not the oracle.
fn expected_bytes() -> Vec<u8> {
    [
        &b"/lx/go\0\0"[..],
        b",ifsT\0\0\0",
        &7i32.to_be_bytes(),
        &0.5f32.to_be_bytes(),
        b"x\0\0\0",
    ]
    .concat()
}

fn args() -> Vec<OscArg> {
    vec![
        OscArg::Int { value: 7 },
        OscArg::Float { value: 0.5 },
        OscArg::String { value: "x".into() },
        OscArg::Bool { value: true },
    ]
}

#[test]
fn go_sends_the_exact_osc_datagram_over_udp() {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_read_timeout(Some(GUARD)).unwrap();
    let port = socket.local_addr().unwrap().port();
    let mut s = show_of(
        vec![osc(1, "/lx/go", args())],
        patch("127.0.0.1", port, NetworkProtocol::Udp),
        None,
    );
    assert!(s.control().engine().problems(id(1)).is_empty());
    go(&mut s);
    s.run(256);
    let mut buf = [0u8; 2048];
    let (n, _) = socket.recv_from(&mut buf).unwrap();
    assert_eq!(&buf[..n], expected_bytes().as_slice());
    // The cue completed on its own, without waiting for anything.
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    // One message, once.
    socket
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    assert!(socket.recv_from(&mut buf).is_err());
}

#[test]
fn go_sends_a_slip_frame_over_tcp() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut s = show_of(
        vec![osc(1, "/lx/go", args())],
        patch("127.0.0.1", port, NetworkProtocol::Tcp),
        None,
    );
    go(&mut s);
    let began = Instant::now();
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(_) if began.elapsed() < GUARD => {
                s.run(64);
                std::thread::yield_now();
            }
            Err(e) => panic!("nothing connected: {e}"),
        }
    };
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(GUARD)).unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    let mut decoder = SlipDecoder::new(65536);
    let mut events = Vec::new();
    decoder.feed(&bytes, &mut events);
    let frames: Vec<&Vec<u8>> = events
        .iter()
        .filter_map(|e| match e {
            SlipEvent::Frame(f) => Some(f),
            _ => None,
        })
        .collect();
    assert_eq!(frames, [&expected_bytes()]);
}

#[test]
fn the_override_makes_network_cues_run_but_send_nothing() {
    let fake = Arc::new(Fake::default());
    let mut first = osc(1, "/lx/go", args());
    first.continue_mode = ContinueMode::AutoFollow;
    let mut s = show_of(
        vec![first, osc(2, "/second", vec![])],
        patch("127.0.0.1", 9000, NetworkProtocol::Udp),
        Some(Arc::clone(&fake)),
    );
    let revision = s.control().revision();
    let workspace_before = s.control().workspace().clone();

    let replies = override_request(&mut s, true);
    assert!(matches!(
        replies.as_slice(),
        [ServerMessage::Result {
            outcome: Outcome::Ok(_),
            ..
        }]
    ));
    assert!(s.control().network_output_disabled());

    go(&mut s);
    s.run(256);
    s.settle_net();
    let msgs = s.take_outbound();
    // Both cues ran (the first auto-followed into the second) and completed.
    for n in [1, 2] {
        let seen: Vec<_> = states(&msgs, id(n)).into_iter().map(|(st, _)| st).collect();
        assert!(!seen.is_empty(), "cue {n} never ran");
        assert_eq!(s.control().engine().state(id(n)), CueState::Idle);
    }
    assert!(fake.sent().is_empty(), "{:?}", fake.sent());

    // Not part of the show: nothing changed in the workspace, and no revision was spent.
    assert_eq!(s.control().revision(), revision);
    assert_eq!(*s.control().workspace(), workspace_before);

    // Switched back on, the same GO sends.
    override_request(&mut s, false);
    assert!(!s.control().network_output_disabled());
    s.transport(openlab_core::protocol::TransportAction::SetPlayhead {
        list: list_id(),
        cue: Some(id(1)),
    });
    go(&mut s);
    s.run(256);
    s.settle_net();
    let addresses: Vec<Vec<u8>> = fake.sent().into_iter().map(|(_, b)| b).collect();
    assert_eq!(addresses.len(), 2);
    assert!(addresses[0].starts_with(b"/lx/go\0"));
    assert!(addresses[1].starts_with(b"/second\0"));
}

#[test]
fn the_override_is_reported_in_status_promptly_and_survives_no_restart() {
    let ws = {
        let mut ws = workspace(vec![]);
        ws.network_patches = vec![patch("127.0.0.1", 9000, NetworkProtocol::Udp)];
        ws
    };
    let clock = openlab_engine::control::ManualClock::new(1_700_000_000_000);
    let mut c = config(&ws);
    c.clock = clock.clock();
    c.status = Some(StatusConfig::default());
    let mut s = show_with(ws.clone(), c.clone());
    let first = statuses(s.take_outbound());
    assert_eq!(
        first.last().map(|st| st.network_output_disabled),
        Some(false)
    );

    // Flipped between two periodic statuses: sent early (after the minimum gap).
    clock.set(1_700_000_000_300);
    override_request(&mut s, true);
    s.tick();
    let early = statuses(s.take_outbound());
    assert_eq!(early.len(), 1);
    assert!(early[0].network_output_disabled);
    let text = openlab_core::protocol::encode_server(&ServerMessage::Status {
        status: Box::new(early[0].clone()),
    })
    .unwrap();
    assert!(text.contains("\"network_output_disabled\":true"), "{text}");

    // A new engine starts with output on: the override is not in the show file or any setting.
    let fresh = show_with(ws, c);
    assert!(!fresh.control().network_output_disabled());
}

#[test]
fn an_unresolvable_host_breaks_its_cues_before_go_and_clears_when_it_resolves() {
    let fake = Arc::new(Fake::default());
    fake.name("lights.invalid", Err("no such host".into()));
    let mut s = show_of(
        vec![osc(1, "/lx/go", vec![])],
        patch("lights.invalid", 9000, NetworkProtocol::Udp),
        Some(Arc::clone(&fake)),
    );
    // Found at load, off the control thread, by a name look-up alone.
    assert_eq!(fake.lookups(), 1);
    assert!(matches!(
        s.control().engine().problems(id(1)),
        [CueProblem::NetworkHostUnresolved { reason, .. }] if reason == "no such host"
    ));
    assert_eq!(s.control().engine().state(id(1)), CueState::Broken);

    // The operator sees it: a runtime `problems` change was broadcast when it was found.
    // (Taken before the first assertion drained nothing: `show_of` discards startup traffic.)
    go(&mut s);
    s.run(256);
    s.settle_net();
    assert!(fake.sent().is_empty());
    let msgs = s.take_outbound();
    assert!(
        warnings(&msgs)
            .iter()
            .any(|w| matches!(w, WireWarning::CueBroken { cue } if *cue == id(1)))
    );

    // The name starts to resolve (the network came up). Nothing is looked up again before the
    // retry interval of virtual time (10 s) has passed, then the cue is mended.
    fake.name("lights.invalid", Ok(()));
    s.run(u64::from(RATE) * 9);
    s.settle_net();
    assert_eq!(fake.lookups(), 1, "no retry yet");
    assert!(!s.control().engine().problems(id(1)).is_empty());
    s.run(u64::from(RATE) * 2);
    s.settle_net();
    assert_eq!(fake.lookups(), 2);
    assert!(s.control().engine().problems(id(1)).is_empty());
    let msgs = s.take_outbound();
    assert!(changes(&msgs).iter().any(|c| matches!(
        c,
        RuntimeChange::Problems { cue, problems } if *cue == id(1) && problems.is_empty()
    )));

    s.transport(openlab_core::protocol::TransportAction::SetPlayhead {
        list: list_id(),
        cue: Some(id(1)),
    });
    go(&mut s);
    s.run(256);
    s.settle_net();
    assert_eq!(fake.sent().len(), 1);
}

#[test]
fn a_failed_look_up_is_broadcast_as_a_problem() {
    let fake = Arc::new(Fake::default());
    fake.name("lights.invalid", Err("no such host".into()));
    let mut ws = workspace(vec![osc(1, "/lx/go", vec![])]);
    ws.network_patches = vec![patch("lights.invalid", 9000, NetworkProtocol::Udp)];
    let mut c = config(&ws);
    c.net = NetTransport(fake);
    let mut s = show_with(ws, c);
    s.settle_net();
    let msgs = s.take_outbound();
    assert!(changes(&msgs).iter().any(|c| matches!(
        c,
        RuntimeChange::Problems { cue, problems }
            if *cue == id(1)
                && matches!(problems[..], [WireProblem::NetworkHostUnresolved { .. }])
    )));
}

#[test]
fn a_destination_that_cannot_be_addressed_is_broken_without_any_look_up() {
    for (host, port) in [("not a host", 9000), ("", 9000), ("127.0.0.1", 0)] {
        let fake = Arc::new(Fake::default());
        let mut s = show_of(
            vec![osc(1, "/lx/go", vec![])],
            patch(host, port, NetworkProtocol::Udp),
            Some(Arc::clone(&fake)),
        );
        assert!(
            matches!(
                s.control().engine().problems(id(1)),
                [CueProblem::NetworkHostInvalid { .. }]
            ),
            "{host:?}:{port}"
        );
        go(&mut s);
        s.run(256);
        s.settle_net();
        assert!(fake.sent().is_empty());
        assert_eq!(fake.lookups(), 0);
    }
}

#[test]
fn a_message_that_is_not_valid_osc_is_broken_and_never_sent() {
    let fake = Arc::new(Fake::default());
    let mut s = show_of(
        vec![osc(1, "no-slash", vec![]), osc(2, "/ok", vec![])],
        patch("127.0.0.1", 9000, NetworkProtocol::Udp),
        Some(Arc::clone(&fake)),
    );
    assert!(matches!(
        s.control().engine().problems(id(1)),
        [CueProblem::NetworkMessageInvalid { .. }]
    ));
    go(&mut s);
    go(&mut s);
    s.run(256);
    s.settle_net();
    let sent = fake.sent();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].1.starts_with(b"/ok\0"));
}

#[test]
fn a_failed_send_is_a_warning_and_the_show_goes_on() {
    let fake = Arc::new(Fake::default());
    fake.fail_with(Some("network is unreachable"));
    let mut s = show_of(
        vec![osc(1, "/a", vec![]), osc(2, "/b", vec![])],
        patch("127.0.0.1", 9000, NetworkProtocol::Udp),
        Some(Arc::clone(&fake)),
    );
    go(&mut s);
    s.run(256);
    s.settle_net();
    let msgs = s.take_outbound();
    let seen = notices(&msgs);
    assert!(
        seen.iter()
            .any(|(code, message)| code == "network_send_failed"
                && message.contains("network is unreachable")
                && message.contains("udp://127.0.0.1:9000")),
        "{seen:?}"
    );
    // The cue itself finished, and the next GO works once the network is back.
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    fake.fail_with(None);
    go(&mut s);
    s.run(256);
    s.settle_net();
    assert_eq!(fake.sent().len(), 1);
    // The failure is in the show log too.
    assert!(
        s.control()
            .show_log()
            .tail(100)
            .iter()
            .any(|e| e.message.contains("network is unreachable"))
    );
}

#[test]
fn a_destination_that_never_answers_cannot_stall_the_control_loop() {
    let fake = Arc::new(Fake::default());
    let (entered, release) = fake.hold();
    // A Network cue to a stuck destination, then a Wait that must still time out on schedule.
    let mut s = show_of(
        vec![
            osc(1, "/stuck", vec![]),
            cue(2, CueKind::Wait { duration: 0.5 }),
        ],
        patch("127.0.0.1", 9000, NetworkProtocol::Udp),
        Some(Arc::clone(&fake)),
    );
    go(&mut s);
    entered.recv_timeout(GUARD).unwrap();
    // The sender is stuck inside `send` now; the loop carries on regardless.
    go(&mut s);
    let at = s.rendered();
    s.run(u64::from(RATE));
    assert!(s.rendered() >= at + u64::from(RATE));
    assert_eq!(
        s.control().engine().state(id(2)),
        CueState::Idle,
        "the Wait finished"
    );
    assert_eq!(s.stalls(), 0);
    assert_eq!(
        s.control().net_busy(),
        1,
        "still waiting for the destination"
    );
    release.send(()).unwrap();
    s.settle_net();
    assert_eq!(fake.sent().len(), 1);
}

#[test]
fn a_full_queue_refuses_messages_with_a_warning_instead_of_blocking() {
    use openlab_engine::control::LINK_QUEUE;
    let fake = Arc::new(Fake::default());
    let (entered, release) = fake.hold();
    let mut s = show_of(
        vec![osc(1, "/n", vec![])],
        patch("127.0.0.1", 9000, NetworkProtocol::Udp),
        Some(Arc::clone(&fake)),
    );
    start(&mut s, 1);
    entered.recv_timeout(GUARD).unwrap();
    // One message is inside `send`; the queue holds LINK_QUEUE more; the rest are refused.
    let extra = 7;
    for _ in 0..LINK_QUEUE + extra {
        start(&mut s, 1);
    }
    s.run(256);
    let refused = notices(&s.take_outbound())
        .into_iter()
        .filter(|(code, message)| code == "network_send_failed" && message.contains("not sent"))
        .count();
    assert_eq!(refused, extra);
    // Free the sender: everything that was accepted goes out, in order.
    let mut waiting: VecDeque<()> = (0..=LINK_QUEUE).map(|_| ()).collect();
    while waiting.pop_front().is_some() {
        let _ = release.send(());
    }
    s.settle_net();
    assert_eq!(fake.sent().len(), LINK_QUEUE + 1);
}
