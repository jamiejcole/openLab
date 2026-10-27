// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The OSC listeners over real UDP and TCP sockets on 127.0.0.1 (docs/osc.md, docs/network.md),
//! against a stand-in control thread, plus one run in front of the real control loop. Waits are
//! on the sockets or on a notification; the timeouts are hang guards only.

mod show;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use openlab_audio::Mixer;
use openlab_core::protocol::{
    ClientMessage, DocumentState, OkBody, Outcome, Request, RequestId, RuntimeChange,
    ServerMessage, Snapshot, TransportAction, Upsert, WireCueState,
};
use openlab_core::{CueKind, GroupMode, Workspace};
use openlab_engine::control::{
    Backend, Control, Inbox, OUTBOUND_CAPACITY, OutboundTx, outbound_channel, runner,
};
use openlab_engine::net::{OscConfig, OscListenerStats, ServerConfig, WsServer};
use openlab_engine::osc::{SlipDecoder, SlipEvent, decode_packet, encode_message, slip_encode};
use rosc::{OscMessage, OscType};
use serde_json::Value;
use show::{cue, id, list_id, workspace};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::Notify;

const GUARD: Duration = Duration::from_secs(20);

/// Runs `f` within [`GUARD`], or fails the test naming `what`.
async fn guard<F: std::future::Future>(what: &str, f: F) -> F::Output {
    match tokio::time::timeout(GUARD, f).await {
        Ok(out) => out,
        Err(_) => panic!("hang: {what} took longer than {GUARD:?}"),
    }
}

#[derive(Default)]
struct Stand {
    workspace: Mutex<Option<Workspace>>,
    seen: Mutex<Vec<ClientMessage>>,
    logs: Mutex<Vec<String>>,
    /// While set, transports and commands are not answered.
    mute: std::sync::atomic::AtomicBool,
    changed: Notify,
}

impl Stand {
    /// Waits until `pred` holds for the control thread's record.
    async fn wait_for(&self, what: &str, pred: impl Fn(&Stand) -> bool) {
        guard(what, async {
            loop {
                if pred(self) {
                    return;
                }
                self.changed.notified().await;
            }
        })
        .await;
    }

    async fn wait_seen(&self, what: &str, pred: impl Fn(&[ClientMessage]) -> bool) {
        self.wait_for(what, |c| pred(&c.seen.lock().unwrap())).await;
    }

    /// What was sent besides the OSC server's own snapshot requests.
    fn inputs(&self) -> Vec<ClientMessage> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|m| {
                !matches!(
                    m,
                    ClientMessage::Request {
                        request: Request::Snapshot,
                        ..
                    }
                )
            })
            .cloned()
            .collect()
    }
}

fn snapshot(ws: &Workspace, revision: u64) -> Snapshot {
    Snapshot {
        revision,
        runtime_seq: 0,
        document: DocumentState {
            path: None,
            dirty: false,
            can_undo: false,
            can_redo: false,
        },
        workspace: ws.clone(),
        cue_states: vec![],
        problems: vec![],
        group_warnings: vec![],
        playheads: vec![],
        previewing: vec![],
        warnings: vec![],
    }
}

fn ok(id: RequestId) -> ServerMessage {
    ServerMessage::Result {
        id,
        outcome: Outcome::Ok(OkBody {
            revision: None,
            data: None,
        }),
    }
}

/// Answers snapshots from the workspace the test set and everything else with `ok`.
fn control_thread(rx: &Receiver<Inbox>, c: &Stand) {
    while let Ok(msg) = rx.recv() {
        match msg {
            Inbox::Client { message, reply } => {
                c.seen.lock().unwrap().push(message.clone());
                match message {
                    ClientMessage::Request {
                        id,
                        request: Request::Snapshot,
                    } => {
                        let ws = c.workspace.lock().unwrap().clone().unwrap();
                        let _ = reply.send(ServerMessage::Snapshot {
                            snapshot: Box::new(snapshot(&ws, 0)),
                        });
                        let _ = reply.send(ok(id));
                    }
                    ClientMessage::Transport { id, .. } | ClientMessage::Command { id, .. }
                        if !c.mute.load(std::sync::atomic::Ordering::SeqCst) =>
                    {
                        let _ = reply.send(ok(id));
                    }
                    _ => {}
                }
                c.changed.notify_one();
            }
            Inbox::Log { message, .. } => {
                c.logs.lock().unwrap().push(message);
                c.changed.notify_one();
            }
            _ => break,
        }
    }
}

struct Fix {
    server: WsServer,
    out: OutboundTx,
    inbox: Sender<Inbox>,
    control: Arc<Stand>,
    thread: Option<thread::JoinHandle<()>>,
    /// Sends datagrams to the server and receives its replies (the reply port is its own).
    udp: UdpSocket,
    udp_port: u16,
    tcp_port: u16,
}

impl Drop for Fix {
    fn drop(&mut self) {
        self.server.stop();
        let _ = self.inbox.send(Inbox::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn test_workspace() -> Workspace {
    let mut ws = workspace(vec![
        cue(
            1,
            CueKind::Group {
                mode: GroupMode::Timeline,
                children: vec![],
            },
        ),
        cue(
            2,
            CueKind::Group {
                mode: GroupMode::Timeline,
                children: vec![],
            },
        ),
    ]);
    ws.cue_lists[0].cues[0].name = "Opening".into();
    ws.cue_lists[0].cues[1].triggers.osc = Some("/lights/blackout".into());
    ws
}

fn with_passcode(code: &str) -> Workspace {
    let mut ws = test_workspace();
    ws.settings.osc_passcode = Some(code.into());
    ws
}

async fn fixture(ws: Workspace) -> Fix {
    fixture_with(ws, |_| {}).await
}

async fn fixture_with(ws: Workspace, tune: impl FnOnce(&mut OscConfig)) -> Fix {
    let udp = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let mut osc = OscConfig {
        bind: Ipv4Addr::LOCALHOST.into(),
        port: 0,
        reply_port: udp.local_addr().unwrap().port(),
        ..OscConfig::default()
    };
    tune(&mut osc);
    let (inbox, rx) = crossbeam_channel::unbounded();
    let (out, outbound) = outbound_channel(OUTBOUND_CAPACITY);
    let control = Arc::new(Stand::default());
    *control.workspace.lock().unwrap() = Some(ws);
    let server = WsServer::start(
        inbox.clone(),
        outbound,
        ServerConfig {
            osc: Some(osc),
            ..ServerConfig::default()
        },
    )
    .unwrap();
    let ports = server.osc_ports().expect("osc did not bind");
    let thread = {
        let control = control.clone();
        thread::spawn(move || control_thread(&rx, &control))
    };
    Fix {
        server,
        out,
        inbox,
        control,
        thread: Some(thread),
        udp,
        udp_port: ports.udp,
        tcp_port: ports.tcp,
    }
}

fn msg(addr: &str, args: Vec<OscType>) -> OscMessage {
    OscMessage {
        addr: addr.into(),
        args,
    }
}

fn text(s: &str) -> OscType {
    OscType::String(s.into())
}

impl Fix {
    fn udp_target(&self) -> SocketAddr {
        SocketAddr::from((Ipv4Addr::LOCALHOST, self.udp_port))
    }

    async fn udp_send(&self, m: &OscMessage) {
        self.udp
            .send_to(&encode_message(m).unwrap(), self.udp_target())
            .await
            .unwrap();
    }

    async fn udp_send_raw(&self, bytes: &[u8]) {
        self.udp.send_to(bytes, self.udp_target()).await.unwrap();
    }

    /// The next datagram on the reply port.
    async fn udp_recv(&self) -> OscMessage {
        let mut buf = vec![0u8; 70_000];
        let (n, _) = guard("waiting for a UDP reply", self.udp.recv_from(&mut buf))
            .await
            .unwrap();
        let mut messages = decode_packet(&buf[..n]).unwrap();
        assert_eq!(messages.len(), 1);
        messages.remove(0)
    }

    async fn tcp(&self) -> Tcp {
        let stream = guard(
            "connecting",
            TcpStream::connect((Ipv4Addr::LOCALHOST, self.tcp_port)),
        )
        .await
        .unwrap();
        stream.set_nodelay(true).unwrap();
        Tcp {
            stream,
            decoder: SlipDecoder::default(),
            frames: Vec::new(),
        }
    }

    fn stats(&self) -> OscListenerStats {
        self.server.osc_stats().unwrap()
    }

    /// Waits until the listener's counters satisfy `pred` (no sleeping: yields to the server's
    /// threads until it does, within the guard).
    async fn wait_stats(&self, what: &str, pred: impl Fn(&OscListenerStats) -> bool) {
        guard(what, async {
            while !pred(&self.stats()) {
                tokio::task::yield_now().await;
            }
        })
        .await;
    }
}

struct Tcp {
    stream: TcpStream,
    decoder: SlipDecoder,
    frames: Vec<Vec<u8>>,
}

impl Tcp {
    async fn write(&self, bytes: &[u8]) {
        let mut rest = bytes;
        while !rest.is_empty() {
            self.stream.writable().await.unwrap();
            match self.stream.try_write(rest) {
                Ok(n) => rest = &rest[n..],
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("{e}"),
            }
        }
    }

    async fn send(&self, m: &OscMessage) {
        self.write(&slip_encode(&encode_message(m).unwrap())).await;
    }

    /// The next message from the server, or `None` when it closed the connection.
    async fn recv(&mut self) -> Option<OscMessage> {
        guard("waiting for a TCP message", async {
            loop {
                if !self.frames.is_empty() {
                    let frame = self.frames.remove(0);
                    let mut messages = decode_packet(&frame).unwrap();
                    assert_eq!(messages.len(), 1);
                    return Some(messages.remove(0));
                }
                self.stream.readable().await.unwrap();
                let mut buf = [0u8; 4096];
                match self.stream.try_read(&mut buf) {
                    Ok(0) => return None,
                    Ok(n) => {
                        let mut events = Vec::new();
                        self.decoder.feed(&buf[..n], &mut events);
                        for e in events {
                            match e {
                                SlipEvent::Frame(f) => self.frames.push(f),
                                other => panic!("{other:?}"),
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    // A refused connection may be reset instead of closed.
                    Err(_) => return None,
                }
            }
        })
        .await
    }
}

/// The JSON of a `/reply...` message.
fn body(m: &OscMessage) -> Value {
    assert!(m.addr.starts_with("/reply/"), "{}", m.addr);
    let [OscType::String(s)] = m.args.as_slice() else {
        panic!("{m:?}")
    };
    serde_json::from_str(s).unwrap()
}

fn ws_id() -> String {
    workspace(vec![]).id.to_string()
}

fn go() -> ClientMessage {
    ClientMessage::Transport {
        id: RequestId(0),
        action: TransportAction::Go { list: list_id() },
    }
}

/// The inputs as the control thread saw them, with request ids blanked out.
fn blank_ids(inputs: Vec<ClientMessage>) -> Vec<ClientMessage> {
    inputs
        .into_iter()
        .map(|m| match m {
            ClientMessage::Transport { action, .. } => ClientMessage::Transport {
                id: RequestId(0),
                action,
            },
            ClientMessage::Command { command, .. } => ClientMessage::Command {
                id: RequestId(0),
                command,
            },
            other => other,
        })
        .collect()
}

fn is_input(m: &ClientMessage) -> bool {
    !matches!(
        m,
        ClientMessage::Request {
            request: Request::Snapshot,
            ..
        }
    )
}

#[tokio::test]
async fn go_over_udp_reaches_control() {
    let f = fixture(test_workspace()).await;
    f.udp_send(&msg("/go", vec![])).await;
    f.control
        .wait_seen("/go", |seen| seen.iter().any(is_input))
        .await;
    assert_eq!(blank_ids(f.control.inputs()), vec![go()]);
    // The workspace-prefixed form, and the cue's own trigger address.
    f.udp_send(&msg(&format!("/workspace/{}/go", ws_id()), vec![]))
        .await;
    f.udp_send(&msg("/lights/blackout", vec![])).await;
    f.control
        .wait_seen("three inputs", |seen| {
            seen.iter().filter(|m| is_input(m)).count() == 3
        })
        .await;
    let inputs = blank_ids(f.control.inputs());
    assert_eq!(inputs[1], go());
    assert_eq!(
        inputs[2],
        ClientMessage::Transport {
            id: RequestId(0),
            action: TransportAction::Start { cue: id(2) },
        }
    );
}

#[tokio::test]
async fn request_ids_are_reserved() {
    let f = fixture(test_workspace()).await;
    f.udp_send(&msg("/go", vec![])).await;
    f.control
        .wait_seen("/go", |seen| seen.iter().any(is_input))
        .await;
    for m in f.control.seen.lock().unwrap().iter() {
        let (ClientMessage::Transport { id, .. } | ClientMessage::Request { id, .. }) = m else {
            continue;
        };
        assert!(id.0 >= 1 << 63 && id.0 != u64::MAX, "{id:?}");
    }
}

#[tokio::test]
async fn go_over_tcp_reaches_control() {
    let f = fixture(test_workspace()).await;
    let t = f.tcp().await;
    t.send(&msg("/go", vec![])).await;
    f.control
        .wait_seen("/go", |seen| seen.iter().any(is_input))
        .await;
    assert_eq!(blank_ids(f.control.inputs()), vec![go()]);
}

#[tokio::test]
async fn property_read_reply_arrives_at_the_reply_port_over_udp() {
    let f = fixture(test_workspace()).await;
    f.udp_send(&msg("/cue/1/name", vec![])).await;
    let reply = f.udp_recv().await;
    assert_eq!(reply.addr, "/reply/cue/1/name");
    let json = body(&reply);
    assert_eq!(json["status"], "ok");
    assert_eq!(json["data"], "Opening");
    assert_eq!(json["workspace_id"], ws_id());
    // A cue that is not there is an error reply, and nothing reached control.
    f.udp_send(&msg("/cue/99/name", vec![])).await;
    assert_eq!(body(&f.udp_recv().await)["status"], "error");
    assert!(f.control.inputs().is_empty());
}

#[tokio::test]
async fn tcp_replies_are_slip_framed_on_the_same_connection() {
    let f = fixture(test_workspace()).await;
    let mut t = f.tcp().await;
    t.send(&msg(
        "/cue_id/00000000-0000-0000-0000-000000000001/name",
        vec![],
    ))
    .await;
    let reply = t.recv().await.unwrap();
    assert_eq!(body(&reply)["data"], "Opening");
}

#[tokio::test]
async fn slip_frames_split_across_reads_and_joined_in_one_read() {
    let f = fixture(test_workspace()).await;
    let mut t = f.tcp().await;
    let frame = slip_encode(&encode_message(&msg("/cue/1/name", vec![])).unwrap());
    // One byte per write: the server sees the frame in as many pieces as TCP delivers.
    for byte in &frame {
        t.write(std::slice::from_ref(byte)).await;
    }
    assert_eq!(body(&t.recv().await.unwrap())["data"], "Opening");
    // Two frames in one write, the second cut in the middle.
    let mut both = frame.clone();
    both.extend_from_slice(&frame);
    let (first, rest) = both.split_at(frame.len() + 5);
    t.write(first).await;
    assert_eq!(body(&t.recv().await.unwrap())["data"], "Opening");
    t.write(rest).await;
    assert_eq!(body(&t.recv().await.unwrap())["data"], "Opening");
    // A frame with a bad escape is dropped and counted; the connection goes on.
    t.write(&[0xC0, b'/', 0xDB, 0x01, 0xC0]).await;
    t.send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&t.recv().await.unwrap())["data"], "Opening");
    assert_eq!(f.stats().osc.malformed, 1);
}

#[tokio::test]
async fn passcode_refuses_udp_silently_and_tcp_with_denied() {
    let f = fixture(with_passcode("sesame")).await;
    // UDP: no reply, nothing reaches control.
    f.udp_send(&msg("/go", vec![])).await;
    // The next reply that arrives is the one to the wrong passcode, so the refused /go got none.
    f.udp_send(&msg("/connect", vec![text("wrong")])).await;
    let reply = f.udp_recv().await;
    assert_eq!(reply.addr, "/reply/connect");
    assert_eq!(body(&reply)["data"], "badpass");
    assert!(f.control.inputs().is_empty());
    assert_eq!(f.stats().osc.denied, 1);

    // TCP: denied.
    let mut t = f.tcp().await;
    t.send(&msg("/go", vec![])).await;
    let denied = t.recv().await.unwrap();
    assert_eq!(denied.addr, "/reply/go");
    assert_eq!(body(&denied)["status"], "denied");
    assert!(f.control.inputs().is_empty());

    // After the right passcode both work.
    t.send(&msg("/connect", vec![text("sesame")])).await;
    assert_eq!(body(&t.recv().await.unwrap())["status"], "ok");
    t.send(&msg("/go", vec![])).await;
    f.udp_send(&msg("/connect", vec![text("sesame")])).await;
    assert_eq!(body(&f.udp_recv().await)["status"], "ok");
    f.udp_send(&msg("/go", vec![])).await;
    f.control
        .wait_seen("two GOs", |seen| {
            seen.iter().filter(|m| is_input(m)).count() == 2
        })
        .await;
}

#[tokio::test]
async fn disconnect_frees_the_session() {
    let f = fixture(with_passcode("sesame")).await;
    let mut a = f.tcp().await;
    a.send(&msg("/connect", vec![text("sesame")])).await;
    assert_eq!(body(&a.recv().await.unwrap())["status"], "ok");
    a.send(&msg("/updates", vec![OscType::Int(1)])).await;
    assert_eq!(body(&a.recv().await.unwrap())["status"], "ok");
    drop(a);
    f.wait_stats("the connection to be forgotten", |s| s.tcp_connections == 0)
        .await;
    // Updates are no longer aimed at it (that would count as a failed send), and a new
    // connection starts unauthenticated.
    f.out
        .send(ServerMessage::Runtime {
            seq: 1,
            changes: vec![RuntimeChange::CueState {
                cue: id(1),
                state: WireCueState::Running,
                reason: None,
            }],
        })
        .await
        .unwrap();
    let mut b = f.tcp().await;
    b.send(&msg("/go", vec![])).await;
    assert_eq!(body(&b.recv().await.unwrap())["status"], "denied");
    assert_eq!(f.stats().send_failed, 0);
}

#[tokio::test]
async fn updates_report_cue_edits_and_playhead_moves() {
    let f = fixture(test_workspace()).await;
    f.udp_send(&msg("/updates", vec![OscType::Int(1)])).await;
    assert_eq!(body(&f.udp_recv().await)["status"], "ok");
    let ws = ws_id();

    // A cue edit.
    let mut edited = test_workspace().cue_lists[0].cues[0].clone();
    edited.name = "Renamed".into();
    f.out
        .send(ServerMessage::ModelChanged {
            revision: 1,
            upserts: vec![Upsert::Cue {
                list: list_id(),
                parent: None,
                index: 0,
                cue: Box::new(edited),
            }],
            deletes: vec![],
        })
        .await
        .unwrap();
    let update = f.udp_recv().await;
    assert_eq!(
        update.addr,
        format!("/update/workspace/{ws}/cue_id/{}", id(1))
    );
    assert!(update.args.is_empty());
    // The copy follows: a property read now answers with the new name.
    f.udp_send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "Renamed");

    // A playhead move.
    f.out
        .send(ServerMessage::Runtime {
            seq: 1,
            changes: vec![RuntimeChange::Playhead {
                list: list_id(),
                cue: Some(id(2)),
            }],
        })
        .await
        .unwrap();
    let update = f.udp_recv().await;
    assert_eq!(
        update.addr,
        format!(
            "/update/workspace/{ws}/cueList/{}/playbackPosition",
            list_id()
        )
    );
    assert_eq!(update.args, vec![text(&id(2).to_string())]);

    // Past the end: an empty cue id. The same playhead again says nothing.
    f.out
        .send(ServerMessage::Runtime {
            seq: 2,
            changes: vec![RuntimeChange::Playhead {
                list: list_id(),
                cue: Some(id(2)),
            }],
        })
        .await
        .unwrap();
    f.out
        .send(ServerMessage::Runtime {
            seq: 3,
            changes: vec![RuntimeChange::Playhead {
                list: list_id(),
                cue: None,
            }],
        })
        .await
        .unwrap();
    assert_eq!(f.udp_recv().await.args, vec![text("")]);

    // A cue starting.
    f.out
        .send(ServerMessage::Runtime {
            seq: 4,
            changes: vec![RuntimeChange::CueState {
                cue: id(2),
                state: WireCueState::Running,
                reason: None,
            }],
        })
        .await
        .unwrap();
    assert_eq!(
        f.udp_recv().await.addr,
        format!("/update/workspace/{ws}/cue_id/{}", id(2))
    );

    // /updates 0 stops them: the next thing to arrive is the reply to a read.
    f.udp_send(&msg("/updates", vec![OscType::Int(0)])).await;
    assert_eq!(body(&f.udp_recv().await)["status"], "ok");
    f.out
        .send(ServerMessage::Runtime {
            seq: 5,
            changes: vec![RuntimeChange::CueState {
                cue: id(1),
                state: WireCueState::Idle,
                reason: None,
            }],
        })
        .await
        .unwrap();
    f.udp_send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(f.udp_recv().await.addr, "/reply/cue/1/name");
}

#[tokio::test]
async fn updates_over_tcp_and_added_cues_update_the_workspace() {
    let f = fixture(test_workspace()).await;
    let mut t = f.tcp().await;
    t.send(&msg("/updates", vec![OscType::Int(1)])).await;
    assert_eq!(body(&t.recv().await.unwrap())["status"], "ok");
    let mut added = test_workspace().cue_lists[0].cues[0].clone();
    added.id = id(3);
    added.number = Some("3".into());
    f.out
        .send(ServerMessage::ModelChanged {
            revision: 1,
            upserts: vec![Upsert::Cue {
                list: list_id(),
                parent: None,
                index: 2,
                cue: Box::new(added),
            }],
            deletes: vec![],
        })
        .await
        .unwrap();
    let ws = ws_id();
    assert_eq!(
        t.recv().await.unwrap().addr,
        format!("/update/workspace/{ws}")
    );
    assert_eq!(
        t.recv().await.unwrap().addr,
        format!("/update/workspace/{ws}/cue_id/{}", id(3))
    );
    // The new cue is addressable.
    t.send(&msg("/cue/3/name", vec![])).await;
    assert_eq!(body(&t.recv().await.unwrap())["status"], "ok");
}

#[tokio::test]
async fn a_gap_in_the_numbering_rebuilds_the_copy_from_a_snapshot() {
    let f = fixture(test_workspace()).await;
    f.udp_send(&msg("/updates", vec![OscType::Int(1)])).await;
    assert_eq!(body(&f.udp_recv().await)["status"], "ok");
    // The engine's workspace now differs (a cue was renamed in a message the OSC side never
    // saw); the next broadcast skips a revision.
    {
        let mut guard = f.control.workspace.lock().unwrap();
        guard.as_mut().unwrap().cue_lists[0].cues[0].name = "Changed".into();
    }
    f.out
        .send(ServerMessage::ModelChanged {
            revision: 5,
            upserts: vec![],
            deletes: vec![],
        })
        .await
        .unwrap();
    // The rebuilt copy differs from the old one: one workspace update.
    assert_eq!(
        f.udp_recv().await.addr,
        format!("/update/workspace/{}", ws_id())
    );
    f.udp_send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "Changed");
}

#[tokio::test]
async fn a_new_passcode_in_a_snapshot_ends_every_session() {
    let f = fixture(with_passcode("sesame")).await;
    let mut t = f.tcp().await;
    t.send(&msg("/connect", vec![text("sesame")])).await;
    assert_eq!(body(&t.recv().await.unwrap())["status"], "ok");
    t.send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&t.recv().await.unwrap())["data"], "Opening");
    // The engine's workspace gets another passcode; a gap makes the OSC side take it.
    {
        let mut guard = f.control.workspace.lock().unwrap();
        guard.as_mut().unwrap().settings.osc_passcode = Some("hunter2".into());
    }
    f.out
        .send(ServerMessage::ModelChanged {
            revision: 9,
            upserts: vec![],
            deletes: vec![],
        })
        .await
        .unwrap();
    // The connection must connect again, with the new passcode.
    guard("sessions to end", async {
        loop {
            t.send(&msg("/cue/1/name", vec![])).await;
            if body(&t.recv().await.unwrap())["status"] == "denied" {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    t.send(&msg("/connect", vec![text("sesame")])).await;
    assert_eq!(body(&t.recv().await.unwrap())["data"], "badpass");
    t.send(&msg("/connect", vec![text("hunter2")])).await;
    assert_eq!(body(&t.recv().await.unwrap())["status"], "ok");
}

#[tokio::test]
async fn updates_to_a_udp_peer_are_limited_by_its_byte_budget() {
    let f = fixture(test_workspace()).await;
    f.udp_send(&msg("/updates", vec![OscType::Int(1)])).await;
    assert_eq!(body(&f.udp_recv().await)["status"], "ok");
    for seq in 1..=400 {
        f.out
            .send(ServerMessage::Runtime {
                seq,
                changes: vec![RuntimeChange::CueState {
                    cue: id(1),
                    state: WireCueState::Running,
                    reason: None,
                }],
            })
            .await
            .unwrap();
    }
    f.wait_stats("the 400 updates to be handled", |s| {
        s.osc.updates_throttled > 0
    })
    .await;
    // The 16 KiB burst is a few hundred bytes short of 400 updates of about 115 bytes each.
    assert!(f.stats().osc.updates_throttled < 400);
}

#[tokio::test]
async fn malformed_datagrams_are_counted_and_the_server_keeps_running() {
    let f = fixture(test_workspace()).await;
    f.udp_send_raw(&[0xFF, 0x00, 0x13]).await;
    f.udp_send_raw(b"not osc at all").await;
    f.udp_send_raw(&[]).await;
    f.udp_send_raw(b"/go\0,i\0\0").await;
    f.udp_send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "Opening");
    assert_eq!(f.stats().osc.malformed, 4);
    assert!(f.control.inputs().is_empty());
}

#[tokio::test]
async fn the_tcp_connection_cap_refuses_the_next_connection_and_logs_it() {
    let f = fixture_with(test_workspace(), |c| c.max_tcp_connections = 2).await;
    let mut a = f.tcp().await;
    let mut b = f.tcp().await;
    for t in [&mut a, &mut b] {
        t.send(&msg("/cue/1/name", vec![])).await;
        assert_eq!(body(&t.recv().await.unwrap())["status"], "ok");
    }
    let mut c = f.tcp().await;
    assert!(c.recv().await.is_none(), "a third connection was served");
    assert_eq!(f.stats().tcp_refused, 1);
    f.control
        .wait_for("the refusal log", |c| {
            c.logs
                .lock()
                .unwrap()
                .iter()
                .any(|l| l == "OSC server refused 1 connection(s): connection limit reached")
        })
        .await;
    // Closing one connection frees its slot.
    drop(a);
    guard("a slot to be freed", async {
        loop {
            let mut t = f.tcp().await;
            t.send(&msg("/cue/1/name", vec![])).await;
            if t.recv().await.is_some() {
                return;
            }
        }
    })
    .await;
    drop(b);
}

#[tokio::test]
async fn go_over_the_wire_starts_the_cue_in_the_real_control_loop() {
    let ws = test_workspace();
    let (ctl, _mixer) = Mixer::new(show::mixer_config()).unwrap();
    let (inbox_tx, inbox) = crossbeam_channel::unbounded();
    let (out_tx, outbound) = outbound_channel(OUTBOUND_CAPACITY);
    let control = Control::new(
        ws.clone(),
        ctl,
        Backend::Offline,
        show::config(&ws),
        inbox_tx.clone(),
        out_tx,
    )
    .unwrap();
    let worker = thread::spawn(move || runner::run(control, &inbox));
    let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let server = WsServer::start(
        inbox_tx.clone(),
        outbound,
        ServerConfig {
            osc: Some(OscConfig {
                bind: Ipv4Addr::LOCALHOST.into(),
                port: 0,
                reply_port: client.local_addr().unwrap().port(),
                ..OscConfig::default()
            }),
            ..ServerConfig::default()
        },
    )
    .unwrap();
    let target = SocketAddr::from((Ipv4Addr::LOCALHOST, server.osc_ports().unwrap().udp));
    let send = async |m: OscMessage| {
        client
            .send_to(&encode_message(&m).unwrap(), target)
            .await
            .unwrap();
    };
    let recv = async || {
        let mut buf = vec![0u8; 70_000];
        let (n, _) = guard("a UDP message", client.recv_from(&mut buf))
            .await
            .unwrap();
        decode_packet(&buf[..n]).unwrap().remove(0)
    };
    send(msg("/updates", vec![OscType::Int(1)])).await;
    assert_eq!(body(&recv().await)["status"], "ok");
    send(msg("/go", vec![])).await;
    // The cue ran (a state update for it) and the playhead moved to the next cue.
    let (mut started, mut moved) = (false, false);
    let ws_id = ws.id.to_string();
    while !(started && moved) {
        let m = recv().await;
        started |= m.addr == format!("/update/workspace/{ws_id}/cue_id/{}", id(1));
        moved |= m.addr.ends_with("/playbackPosition") && m.args == vec![text(&id(2).to_string())];
    }
    let _ = inbox_tx.send(Inbox::Shutdown);
    worker.join().unwrap();
    server.stop();
}

#[tokio::test]
async fn unanswered_requests_are_capped_and_the_rest_dropped() {
    let f = fixture(test_workspace()).await;
    f.control
        .mute
        .store(true, std::sync::atomic::Ordering::SeqCst);
    // In batches the kernel's receive buffer holds (a burst of 300 would lose datagrams
    // before the server sees them).
    for batch in 1..=3u64 {
        for _ in 0..100 {
            f.udp_send(&msg("/go", vec![])).await;
        }
        f.wait_stats("the datagrams to be handled", |s| {
            s.osc.messages + s.udp_dropped == batch * 100
        })
        .await;
    }
    let stats = f.stats();
    // 256 requests may be outstanding; every datagram is either one of them, dropped at the
    // actor's queue or counted as an input that was not sent.
    let sent = stats.osc.messages - stats.inputs_dropped;
    f.control
        .wait_seen("the requests to arrive", |seen| {
            seen.iter().filter(|m| is_input(m)).count() as u64 == sent
        })
        .await;
    assert!(sent <= 256, "{sent}");
}

#[tokio::test]
async fn a_document_with_another_path_asks_for_a_new_snapshot() {
    let f = fixture(test_workspace()).await;
    let snapshots = |seen: &[ClientMessage]| {
        seen.iter()
            .filter(|m| {
                matches!(
                    m,
                    ClientMessage::Request {
                        request: Request::Snapshot,
                        ..
                    }
                )
            })
            .count()
    };
    f.control
        .wait_seen("the first snapshot request", |s| snapshots(s) == 1)
        .await;
    let document = |path: Option<&str>| ServerMessage::Document {
        document: DocumentState {
            path: path.map(Into::into),
            dirty: false,
            can_undo: false,
            can_redo: false,
        },
    };
    // Same path: nothing to do. Another path: the workspace may be another one.
    f.out.send(document(None)).await.unwrap();
    f.out
        .send(document(Some("/shows/other.openlab")))
        .await
        .unwrap();
    f.control
        .wait_seen("a second snapshot request", |s| snapshots(s) == 2)
        .await;
}

#[tokio::test]
async fn a_port_in_use_turns_osc_off_with_a_log_entry_and_the_ui_server_still_starts() {
    let taken = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let (inbox, rx) = crossbeam_channel::unbounded();
    let (_out, outbound) = outbound_channel(OUTBOUND_CAPACITY);
    let server = WsServer::start(
        inbox,
        outbound,
        ServerConfig {
            osc: Some(OscConfig {
                bind: Ipv4Addr::LOCALHOST.into(),
                port: taken.local_addr().unwrap().port(),
                ..OscConfig::default()
            }),
            ..ServerConfig::default()
        },
    )
    .unwrap();
    assert!(server.osc_ports().is_none());
    assert!(server.osc_stats().is_none());
    assert!(server.port() != 0);
    match rx.try_recv() {
        Ok(Inbox::Log { message, .. }) => {
            assert!(message.starts_with("OSC server is off"), "{message}")
        }
        other => panic!("{other:?}"),
    }
}
