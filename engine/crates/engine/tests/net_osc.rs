// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

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
use openlab_core::{CueKind, GroupMode, OscAccess, OscPasscode, OscRights, Workspace};
use openlab_engine::control::{
    Backend, Control, Inbox, OUTBOUND_CAPACITY, OutboundTx, Outgoing, ReplyTx, outbound_channel,
    runner,
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
    /// `Inbox::Trigger` (source, detail) pairs, in arrival order.
    triggers: Mutex<Vec<(String, String)>>,
    /// While set, transports and commands are not answered.
    mute: std::sync::atomic::AtomicBool,
    /// While set, snapshot answers wait for [`Stand::release`].
    hold: std::sync::atomic::AtomicBool,
    held: Mutex<Vec<(ReplyTx, Vec<ServerMessage>)>>,
    /// A second open doc ([`DOC2`]), for the multi-doc tests; `None` while it is closed.
    second: Mutex<Option<Workspace>>,
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
    /// Snapshot requests so far.
    fn snapshot_requests(&self) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
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
    }

    /// Answers the held snapshot requests, and the ones after them at once.
    fn release(&self) {
        self.hold.store(false, std::sync::atomic::Ordering::SeqCst);
        for (reply, answer) in self.held.lock().unwrap().drain(..) {
            for m in answer {
                let _ = reply.send(m);
            }
        }
    }

    fn set_name(&self, name: &str) {
        self.workspace.lock().unwrap().as_mut().unwrap().cue_lists[0].cues[0].name = name.into();
    }

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
            show_mode: false,
        },
        workspace: ws.clone(),
        cue_states: vec![],
        problems: vec![],
        group_warnings: vec![],
        playheads: vec![],
        previewing: vec![],
        warnings: vec![],
        media_durations: vec![],
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
            // The OSC side asks which docs are open as an app connection does (M4.15h): one,
            // [`DOC`]. Not recorded: `snapshot_requests` counts the doc's.
            Inbox::Conn {
                doc: None,
                message:
                    ClientMessage::Request {
                        id,
                        request: Request::Snapshot,
                    },
                reply,
            } => {
                let ws = c.workspace.lock().unwrap().clone().unwrap();
                let info = |doc, ws: &Workspace| openlab_core::protocol::WorkspaceInfo {
                    doc,
                    workspace_id: ws.id,
                    name: ws.name.clone(),
                    path: None,
                    dirty: false,
                };
                let mut workspaces = vec![info(DOC, &ws)];
                if let Some(second) = c.second.lock().unwrap().as_ref() {
                    workspaces.push(info(DOC2, second));
                }
                let _ = reply.send(ServerMessage::Workspaces { workspaces });
                let _ = reply.send(ok(id));
                c.changed.notify_one();
            }
            // The second doc's snapshot: its workspace, or `no_workspace` once it is closed.
            Inbox::Conn {
                doc: Some(DOC2),
                message:
                    ClientMessage::Request {
                        id,
                        request: Request::Snapshot,
                    },
                reply,
            } => {
                c.seen.lock().unwrap().push(ClientMessage::Request {
                    id,
                    request: Request::Snapshot,
                });
                let second = c.second.lock().unwrap().clone();
                let answer = match second {
                    Some(ws) => vec![
                        ServerMessage::Snapshot {
                            snapshot: Box::new(snapshot(&ws, 0)),
                        },
                        ok(id),
                    ],
                    None => vec![ServerMessage::Result {
                        id,
                        outcome: Outcome::Error(openlab_core::protocol::ErrorBody::new(
                            openlab_core::protocol::ErrorCode::NoWorkspace,
                            "closed",
                        )),
                    }],
                };
                for m in answer {
                    let _ = reply.send(m);
                }
                c.changed.notify_one();
            }
            Inbox::Client { message, reply } | Inbox::Conn { message, reply, .. } => {
                c.seen.lock().unwrap().push(message.clone());
                match message {
                    ClientMessage::Request {
                        id,
                        request: Request::Snapshot,
                    } => {
                        let ws = c.workspace.lock().unwrap().clone().unwrap();
                        let answer = vec![
                            ServerMessage::Snapshot {
                                snapshot: Box::new(snapshot(&ws, 0)),
                            },
                            ok(id),
                        ];
                        if c.hold.load(std::sync::atomic::Ordering::SeqCst) {
                            c.held.lock().unwrap().push((reply, answer));
                        } else {
                            for m in answer {
                                let _ = reply.send(m);
                            }
                        }
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
            Inbox::Trigger { source, detail } => {
                c.triggers.lock().unwrap().push((source, detail));
                c.changed.notify_one();
            }
            _ => break,
        }
    }
}

/// The one doc the stand-in plays.
const DOC: openlab_core::protocol::DocId = openlab_core::protocol::DocId(0);
/// The second doc some tests open beside it.
const DOC2: openlab_core::protocol::DocId = openlab_core::protocol::DocId(1);

/// The control loop's outbound channel as the stand-in uses it: every message is about [`DOC`].
struct Out(OutboundTx);

impl Out {
    /// Opens the channel as the engine does: the list of open docs comes first.
    fn new(tx: OutboundTx, ws: &Workspace) -> Self {
        let list = ServerMessage::Workspaces {
            workspaces: vec![openlab_core::protocol::WorkspaceInfo {
                doc: DOC,
                workspace_id: ws.id,
                name: ws.name.clone(),
                path: None,
                dirty: false,
            }],
        };
        tx.try_send(Outgoing::all(list)).unwrap();
        Self(tx)
    }

    async fn send(
        &self,
        m: ServerMessage,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<Outgoing>> {
        self.0.send(Outgoing::doc(DOC, m)).await
    }
}

struct Fix {
    server: WsServer,
    out: Out,
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

/// No Passcode has no rights; one passcode has all (what a v3 show with a passcode migrates to).
fn single_passcode(code: &str) -> openlab_core::OscAccess {
    openlab_core::OscAccess {
        no_passcode: openlab_core::OscRights::NONE,
        passcodes: vec![openlab_core::OscPasscode {
            passcode: code.into(),
            rights: openlab_core::OscRights::ALL,
        }],
    }
}

fn with_passcode(code: &str) -> Workspace {
    let mut ws = test_workspace();
    ws.settings.osc_access = single_passcode(code);
    ws
}

/// A loopback UDP socket whose receive buffer holds the largest datagram: Windows truncates one
/// bigger than the buffer, which is smaller than 64 KiB by default.
fn big_udp() -> UdpSocket {
    let socket = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None).unwrap();
    socket.set_recv_buffer_size(1 << 20).unwrap();
    socket
        .bind(&SocketAddr::from((Ipv4Addr::LOCALHOST, 0)).into())
        .unwrap();
    socket.set_nonblocking(true).unwrap();
    UdpSocket::from_std(socket.into()).unwrap()
}

async fn fixture(ws: Workspace) -> Fix {
    fixture_with(ws, |_| {}).await
}

async fn fixture_with(ws: Workspace, tune: impl FnOnce(&mut OscConfig)) -> Fix {
    fixture_held(ws, false, tune).await
}

/// `hold`: the first snapshot request is not answered until [`Stand::release`].
async fn fixture_held(ws: Workspace, hold: bool, tune: impl FnOnce(&mut OscConfig)) -> Fix {
    let udp = big_udp();
    let mut osc = OscConfig {
        bind: Ipv4Addr::LOCALHOST.into(),
        port: 0,
        reply_port: udp.local_addr().unwrap().port(),
        // Most tests send the same message again and again; the one about repeats turns it on.
        duplicate_window: Duration::ZERO,
        ..OscConfig::default()
    };
    tune(&mut osc);
    let (inbox, rx) = crossbeam_channel::unbounded();
    let (out, outbound) = outbound_channel(OUTBOUND_CAPACITY);
    let out = Out::new(out, &ws);
    let control = Arc::new(Stand::default());
    *control.workspace.lock().unwrap() = Some(ws);
    control
        .hold
        .store(hold, std::sync::atomic::Ordering::SeqCst);
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

    /// Sends `m` then `/version` (answered in turn) and returns the JSON replies to `m`.
    async fn udp_ask(&self, m: &OscMessage) -> Vec<Value> {
        self.udp_send(m).await;
        self.udp_send(&msg("/version", vec![])).await;
        let mut replies = Vec::new();
        loop {
            let r = self.udp_recv().await;
            if r.addr == "/reply/version" {
                return replies;
            }
            replies.push(body(&r));
        }
    }

    /// A round trip that proves what was sent before it was handled: the server answers
    /// `/version` in turn with everything else (QLab sends no reply to `/updates` or to actions).
    async fn udp_sync(&self) {
        self.udp_send(&msg("/version", vec![])).await;
        assert_eq!(self.udp_recv().await.addr, "/reply/version");
    }

    async fn udp_subscribe(&self) {
        self.udp_send(&msg("/updates", vec![OscType::Int(1)])).await;
        self.udp_sync().await;
    }

    /// Waits until the listener's counters satisfy `pred` (no sleeping: yields to the server's
    /// threads until it does, within the guard).
    async fn wait_stats(&self, what: &str, pred: impl Fn(&OscListenerStats) -> bool) {
        let done = tokio::time::timeout(GUARD, async {
            while !pred(&self.stats()) {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(done.is_ok(), "hang: {what}: {:?}", self.stats());
    }
}

struct Tcp {
    stream: TcpStream,
    decoder: SlipDecoder,
    frames: Vec<Vec<u8>>,
}

impl Tcp {
    /// As [`Fix::udp_sync`].
    async fn sync(&mut self) {
        self.send(&msg("/version", vec![])).await;
        assert_eq!(self.recv().await.unwrap().addr, "/reply/version");
    }

    async fn subscribe(&mut self) {
        self.send(&msg("/updates", vec![OscType::Int(1)])).await;
        self.sync().await;
    }

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

    /// Like `write`, but `false` (instead of a panic) once the server has closed the connection.
    async fn try_write_all(&self, bytes: &[u8]) -> bool {
        let mut rest = bytes;
        while !rest.is_empty() {
            if self.stream.writable().await.is_err() {
                return false;
            }
            match self.stream.try_write(rest) {
                Ok(n) => rest = &rest[n..],
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return false,
            }
        }
        true
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

/// The workspace id as QLab (and so the OSC server) writes it: upper case.
fn ws_id() -> String {
    workspace(vec![]).id.to_string().to_uppercase()
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
                coalesce: None,
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
async fn an_osc_input_is_announced_to_control_with_its_source_before_the_input() {
    let f = fixture(test_workspace()).await;
    f.udp_send(&msg("/go", vec![])).await;
    f.control
        .wait_seen("/go", |seen| seen.iter().any(is_input))
        .await;
    // Sent just before the input on the same channel: already there when the input is.
    assert_eq!(
        *f.control.triggers.lock().unwrap(),
        vec![("osc 127.0.0.1".to_owned(), "transport go".to_owned())]
    );
}

#[tokio::test]
async fn an_osc_edit_is_not_announced_as_a_trigger() {
    let f = fixture(test_workspace()).await;
    f.udp_send(&msg("/cue/1/name", vec![text("Renamed")])).await;
    f.udp_send(&msg("/go", vec![])).await;
    f.control
        .wait_seen("both inputs", |seen| {
            seen.iter().filter(|m| is_input(m)).count() == 2
        })
        .await;
    // Only the transport is a trigger; the edit would otherwise fill the log (DX-02).
    assert_eq!(
        *f.control.triggers.lock().unwrap(),
        vec![("osc 127.0.0.1".to_owned(), "transport go".to_owned())]
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
async fn passcode_refuses_udp_and_tcp_with_denied() {
    let f = fixture(with_passcode("sesame")).await;
    // UDP: denied, as QLab answers, and nothing reaches control.
    f.udp_send(&msg("/go", vec![])).await;
    let reply = f.udp_recv().await;
    assert_eq!(reply.addr, "/reply/go");
    assert_eq!(
        body(&reply),
        serde_json::json!({
            "address": format!("/workspace/{}/go", ws_id()),
            "status": "denied",
            "workspace_id": ws_id(),
        })
    );
    f.udp_send(&msg("/connect", vec![text("wrong")])).await;
    let reply = f.udp_recv().await;
    assert_eq!(reply.addr, "/reply/connect");
    assert_eq!(body(&reply)["status"], "ok");
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
    assert_eq!(f.stats().osc.denied, 2);

    // After the right passcode both work.
    t.send(&msg("/connect", vec![text("sesame")])).await;
    assert_eq!(
        body(&t.recv().await.unwrap())["data"],
        "ok:view|edit|control"
    );
    t.send(&msg("/go", vec![])).await;
    f.udp_send(&msg("/connect", vec![text("sesame")])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "ok:view|edit|control");
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
    a.subscribe().await;
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

/// The next update message: its address, and the string argument if it has one.
async fn next_update(f: &Fix) -> (String, Vec<OscType>) {
    let m = f.udp_recv().await;
    assert!(m.addr.starts_with("/update/"), "{}", m.addr);
    (m.addr, m.args)
}

#[tokio::test]
async fn updates_report_cue_edits_and_playhead_moves() {
    let f = fixture(test_workspace()).await;
    // No reply to `/updates`: QLab sends none.
    f.udp_subscribe().await;
    let ws = ws_id();
    let base = format!("/update/workspace/{ws}");

    // A cue edit: that cue, and nothing else.
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
    let (addr, args) = next_update(&f).await;
    assert_eq!(addr, format!("{base}/cue_id/{}", id(1)));
    assert!(args.is_empty());
    // The copy follows: a property read now answers with the new name (and, in order, nothing
    // else came before it).
    f.udp_send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "Renamed");

    // A playhead move: the position (the cue's id as a string), then the dashboard.
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
    let (addr, args) = next_update(&f).await;
    assert_eq!(
        addr,
        format!("{base}/cueList/{}/playbackPosition", list_id())
    );
    assert_eq!(args, vec![text(&id(2).to_string())]);
    assert_eq!(next_update(&f).await, (format!("{base}/dashboard"), vec![]));

    // Past the end: no argument at all. The same playhead again says nothing.
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
    let (addr, args) = next_update(&f).await;
    assert!(addr.ends_with("/playbackPosition"));
    assert!(args.is_empty());
    assert_eq!(next_update(&f).await.0, format!("{base}/dashboard"));

    // A cue starting: the cue, its list and the root, then the dashboard (a start).
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
    let got: Vec<String> = vec![
        next_update(&f).await.0,
        next_update(&f).await.0,
        next_update(&f).await.0,
        next_update(&f).await.0,
    ];
    assert_eq!(
        got,
        vec![
            format!("{base}/cue_id/{}", id(2)),
            format!("{base}/cue_id/{}", list_id()),
            format!("{base}/cue_id/__root__"),
            format!("{base}/dashboard"),
        ]
    );
    // Stopping it: the same three, and no dashboard.
    f.out
        .send(ServerMessage::Runtime {
            seq: 5,
            changes: vec![RuntimeChange::CueState {
                cue: id(2),
                state: WireCueState::Idle,
                reason: None,
            }],
        })
        .await
        .unwrap();
    for expected in [
        format!("{base}/cue_id/{}", id(2)),
        format!("{base}/cue_id/{}", list_id()),
        format!("{base}/cue_id/__root__"),
    ] {
        assert_eq!(next_update(&f).await.0, expected);
    }

    // /updates 0 stops them: the next thing to arrive is the reply to a read.
    f.udp_send(&msg("/updates", vec![OscType::Int(0)])).await;
    f.udp_sync().await;
    f.out
        .send(ServerMessage::Runtime {
            seq: 6,
            changes: vec![RuntimeChange::CueState {
                cue: id(1),
                state: WireCueState::Running,
                reason: None,
            }],
        })
        .await
        .unwrap();
    f.udp_send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(f.udp_recv().await.addr, "/reply/cue/1/name");
}

#[tokio::test]
async fn updates_go_to_the_ip_on_the_reply_port_whatever_port_the_subscriber_sent_from() {
    let f = fixture(test_workspace()).await;
    // A second socket, on another port, subscribes: the updates still arrive on the reply port
    // (`f.udp`), keyed on the IP, as QLab's do.
    let other = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    assert_ne!(
        other.local_addr().unwrap().port(),
        f.udp.local_addr().unwrap().port()
    );
    other
        .send_to(
            &encode_message(&msg("/updates", vec![OscType::Int(1)])).unwrap(),
            f.udp_target(),
        )
        .await
        .unwrap();
    f.udp_sync().await;
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
    assert!(next_update(&f).await.0.ends_with("/playbackPosition"));
}

#[tokio::test]
async fn updates_over_tcp_arrive_on_that_connection_only() {
    let f = fixture(test_workspace()).await;
    let mut t = f.tcp().await;
    t.subscribe().await;
    let mut quiet = f.tcp().await;
    f.udp_subscribe().await;
    // A UDP subscriber is not a TCP one, and the other connection gets nothing.
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
    let on_tcp = t.recv().await.unwrap();
    assert!(on_tcp.addr.ends_with("/playbackPosition"));
    assert_eq!(on_tcp.args, vec![text(&id(2).to_string())]);
    assert!(next_update(&f).await.0.ends_with("/playbackPosition"));
    quiet.sync().await;
    drop(quiet);
}

#[tokio::test]
async fn updates_over_tcp_and_added_cues_update_the_list_and_the_root() {
    let f = fixture(test_workspace()).await;
    let mut t = f.tcp().await;
    t.subscribe().await;
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
    // The cue, its list, the root and the dashboard: no workspace-level update.
    for expected in [
        format!("/update/workspace/{ws}/cue_id/{}", id(3)),
        format!("/update/workspace/{ws}/cue_id/{}", list_id()),
        format!("/update/workspace/{ws}/cue_id/__root__"),
        format!("/update/workspace/{ws}/dashboard"),
    ] {
        assert_eq!(t.recv().await.unwrap().addr, expected);
    }
    // The new cue is addressable.
    t.send(&msg("/cue/3/name", vec![])).await;
    assert_eq!(body(&t.recv().await.unwrap())["status"], "ok");
}

#[tokio::test]
async fn a_gap_in_the_numbering_rebuilds_the_copy_from_a_snapshot() {
    let f = fixture(test_workspace()).await;
    f.udp_subscribe().await;
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
    // The rebuilt copy differs from the old one: the root changed, and the dashboard.
    assert_eq!(
        next_update(&f).await.0,
        format!("/update/workspace/{}/cue_id/__root__", ws_id())
    );
    assert_eq!(
        next_update(&f).await.0,
        format!("/update/workspace/{}/dashboard", ws_id())
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
        guard.as_mut().unwrap().settings.osc_access = single_passcode("hunter2");
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
async fn updates_to_a_subscriber_are_not_held_to_the_byte_budget_of_a_stranger() {
    let f = fixture(test_workspace()).await;
    f.udp_subscribe().await;
    // 60 cue changes: three messages each (the cue, its list, the root), about 24 KiB with
    // headers, more than the 16 KiB burst a peer that may not ask is held to, and every one
    // arrives (few enough for the socket's own receive buffer).
    for seq in 1..=60u64 {
        f.out
            .send(ServerMessage::Runtime {
                seq,
                changes: vec![RuntimeChange::CueState {
                    cue: id(1),
                    state: if seq % 2 == 0 {
                        WireCueState::Paused
                    } else {
                        WireCueState::Running
                    },
                    reason: None,
                }],
            })
            .await
            .unwrap();
    }
    let mut received = 0u64;
    guard("the updates to arrive", async {
        let mut buf = vec![0u8; 70_000];
        // 60 changes x (cue, list, root), and the dashboard of the first one (a start).
        while received < 181 {
            f.udp.recv_from(&mut buf).await.unwrap();
            received += 1;
        }
    })
    .await;
    assert_eq!(f.stats().osc.replies_throttled, 0);
    assert_eq!(f.stats().osc.updates_dropped, 0);
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
async fn one_host_past_its_connection_cap_is_refused_while_the_table_has_room() {
    let f = fixture_with(test_workspace(), |c| {
        c.max_tcp_connections = 8;
        c.max_tcp_connections_per_host = 2;
    })
    .await;
    let mut a = f.tcp().await;
    let mut b = f.tcp().await;
    for t in [&mut a, &mut b] {
        t.send(&msg("/cue/1/name", vec![])).await;
        assert_eq!(body(&t.recv().await.unwrap())["status"], "ok");
    }
    let mut c = f.tcp().await;
    assert!(
        c.recv().await.is_none(),
        "a third connection from one host was served"
    );
    assert_eq!(f.stats().tcp_refused, 1);
    f.control
        .wait_for("the refusal log", |c| {
            c.logs.lock().unwrap().iter().any(|l| {
                l == "OSC server refused 1 connection(s): per-host connection limit (2) reached by 127.0.0.1"
            })
        })
        .await;
    // The host's own connections are still served (another host's slots are in the unit test
    // of `HostSlots`: every test peer is 127.0.0.1).
    a.send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&a.recv().await.unwrap())["status"], "ok");
    drop(b);
}

/// A console on flaky Wi-Fi leaves dead connections behind each time it reconnects. At its
/// cap its next connection closes its own longest-idle one instead of being refused; with all
/// of them subscribed it is refused.
#[tokio::test]
async fn a_host_at_its_cap_with_idle_connections_still_gets_a_new_one_served() {
    let f = fixture_with(test_workspace(), |c| {
        c.max_tcp_connections = 8;
        c.max_tcp_connections_per_host = 2;
        c.tcp_idle_evict = Duration::ZERO;
    })
    .await;
    let mut a = f.tcp().await;
    a.send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&a.recv().await.unwrap())["status"], "ok");
    let mut b = f.tcp().await;
    b.send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&b.recv().await.unwrap())["status"], "ok");
    let mut c = f.tcp().await;
    c.send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&c.recv().await.unwrap())["status"], "ok");
    assert!(
        a.recv().await.is_none(),
        "the host's oldest idle connection stayed"
    );
    assert_eq!(f.stats().tcp_refused, 0);
    b.send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&b.recv().await.unwrap())["status"], "ok");
    // All of the host's connections subscribed: none may be closed, the next is refused.
    b.subscribe().await;
    c.subscribe().await;
    let mut d = f.tcp().await;
    assert!(
        d.recv().await.is_none(),
        "a connection past the cap was served"
    );
    assert_eq!(f.stats().tcp_refused, 1);
    f.control
        .wait_for("the refusal log", |s| {
            s.logs.lock().unwrap().iter().any(|l| {
                l == "OSC server refused 1 connection(s): per-host connection limit (2) reached by 127.0.0.1"
            })
        })
        .await;
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
    send(msg("/version", vec![])).await;
    assert_eq!(recv().await.addr, "/reply/version");
    send(msg("/go", vec![])).await;
    // The cue ran (a state update for it) and the playhead moved to the next cue.
    let (mut started, mut moved) = (false, false);
    let ws_id = ws.id.to_string().to_uppercase();
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
    let f = fixture_with(test_workspace(), |c| c.max_inflight = 256).await;
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
            show_mode: false,
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

fn read_notes() -> OscMessage {
    msg("/cue/1/notes", vec![])
}

#[tokio::test]
async fn tcp_flooders_cannot_starve_udp() {
    // An unauthenticated TCP peer that streams requests as fast as the server reads them (a
    // runaway script) used to fill the one shared queue and make the console's datagrams find
    // it full.
    let f = fixture(with_passcode("sesame")).await;
    let frame = slip_encode(&encode_message(&msg("/cue/1/name", vec![])).unwrap());
    let burst: Vec<u8> = frame
        .iter()
        .copied()
        .cycle()
        .take(frame.len() * 50)
        .collect();
    let mut flooders = Vec::new();
    for _ in 0..4 {
        let t = f.tcp().await;
        let burst = burst.clone();
        flooders.push(tokio::spawn(async move {
            loop {
                let mut rest = burst.as_slice();
                while !rest.is_empty() {
                    if t.stream.writable().await.is_err() {
                        return;
                    }
                    match t.stream.try_write(rest) {
                        Ok(n) => rest = &rest[n..],
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(_) => return,
                    }
                }
            }
        }));
    }
    f.udp_send(&msg("/connect", vec![text("sesame")])).await;
    assert_eq!(body(&f.udp_recv().await)["status"], "ok");
    for _ in 0..100 {
        f.udp_send(&msg("/go", vec![])).await;
        for _ in 0..5 {
            tokio::task::yield_now().await;
        }
    }
    let all = tokio::time::timeout(
        GUARD / 2,
        f.control.wait_seen("every /go", |seen| {
            seen.iter().filter(|m| is_input(m)).count() == 100
        }),
    )
    .await;
    assert!(all.is_ok(), "{:?}", f.stats());
    // None dropped at the queue and none aged out waiting behind the flood. The second half
    // depends on `max_input_age` (2 s): a datagram that waits that long behind the flood is
    // dropped as stale and counted in `inputs_dropped`. If this fails in CI, the machine was
    // too slow to serve a datagram within 2 s (or datagrams are taken after the TCP events
    // again: see `next_input`), not a lost datagram in the kernel.
    let stats = f.stats();
    assert_eq!(
        (stats.udp_dropped, stats.inputs_dropped),
        (0, 0),
        "{stats:?}"
    );
    for flooder in flooders {
        flooder.abort();
    }
}

#[tokio::test]
async fn input_waits_while_a_resync_snapshot_is_out() {
    let f = fixture(test_workspace()).await;
    f.udp_send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "Opening");
    // The engine's workspace moves on; the OSC side finds out by a gap and asks, and the
    // answer is held.
    f.control
        .hold
        .store(true, std::sync::atomic::Ordering::SeqCst);
    f.control.set_name("Changed");
    f.out
        .send(ServerMessage::ModelChanged {
            revision: 5,
            upserts: vec![],
            deletes: vec![],
        })
        .await
        .unwrap();
    f.control
        .wait_for("the resync request", |c| c.snapshot_requests() == 2)
        .await;
    // Resolved against the old copy this would answer "Opening"; it must wait for the new one.
    f.udp_send(&msg("/cue/1/name", vec![])).await;
    // Time for the server's threads to take the datagram, if they were going to.
    for _ in 0..1000 {
        tokio::task::yield_now().await;
    }
    f.control.release();
    assert_eq!(body(&f.udp_recv().await)["data"], "Changed");
}

#[tokio::test]
async fn a_tap_overflow_rebuilds_the_copy_after_the_held_snapshot() {
    let f = fixture(test_workspace()).await;
    f.udp_send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "Opening");
    f.control
        .hold
        .store(true, std::sync::atomic::Ordering::SeqCst);
    f.control.set_name("Second");
    f.out
        .send(ServerMessage::ModelChanged {
            revision: 5,
            upserts: vec![],
            deletes: vec![],
        })
        .await
        .unwrap();
    f.control
        .wait_for("the resync request", |c| c.snapshot_requests() == 2)
        .await;
    // With the snapshot out the actor reads no broadcasts: more than the tap holds fill it.
    let drained = async |extra: u64| {
        for seq in 1..=extra {
            f.out
                .send(ServerMessage::Runtime {
                    seq,
                    changes: vec![],
                })
                .await
                .unwrap();
        }
        // The fan-out task has taken every message (and, as it takes the next only after
        // offering the one before, offered all but possibly the last).
        while f.out.0.capacity() < f.out.0.max_capacity() {
            tokio::task::yield_now().await;
        }
    };
    guard("the broadcasts to be fanned out", async {
        drained(1100).await;
        drained(1).await;
    })
    .await;
    f.control.set_name("Third");
    f.control.release();
    // The held answer ("Second") is installed, then the lag is noticed and a third snapshot
    // taken ("Third").
    f.control
        .wait_for("the rebuild request", |c| c.snapshot_requests() == 3)
        .await;
    guard("the rebuilt copy", async {
        loop {
            f.udp_send(&msg("/cue/1/name", vec![])).await;
            if body(&f.udp_recv().await)["data"] == "Third" {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
}

/// #183: a doc closes while the tap overflows, so its `closed` is lost. The rebuild's own list
/// answer must drop its copy: it answers nothing, takes no input and is not listed.
#[tokio::test]
async fn a_lag_rebuild_drops_a_doc_whose_closed_was_lost() {
    let f = fixture(test_workspace()).await;
    let mut other = workspace(vec![cue(
        21,
        CueKind::Group {
            mode: GroupMode::Timeline,
            children: vec![],
        },
    )]);
    other.id = openlab_core::WorkspaceId(uuid::Uuid::from_u128(2));
    other.name = "other".into();
    other.cue_lists[0].cues[0].number = Some("1".into());
    other.cue_lists[0].cues[0].name = "Other".into();
    *f.control.second.lock().unwrap() = Some(other.clone());
    // The second doc opens: the tap's list names it and the actor takes its snapshot.
    let both = ServerMessage::Workspaces {
        workspaces: vec![
            openlab_core::protocol::WorkspaceInfo {
                doc: DOC,
                workspace_id: workspace(vec![]).id,
                name: "test".into(),
                path: None,
                dirty: false,
            },
            openlab_core::protocol::WorkspaceInfo {
                doc: DOC2,
                workspace_id: other.id,
                name: "other".into(),
                path: None,
                dirty: false,
            },
        ],
    };
    f.out.0.send(Outgoing::all(both)).await.unwrap();
    // One reply per doc while both are open.
    guard("both docs to answer", async {
        loop {
            let replies = f.udp_ask(&msg("/cue/1/name", vec![])).await;
            if replies.len() == 2 {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;

    // A resync is out (its answer held), so the actor reads no broadcasts: more than the tap
    // holds overflow it, and the second doc's `closed` is among those lost.
    f.control
        .hold
        .store(true, std::sync::atomic::Ordering::SeqCst);
    f.control.set_name("Changed");
    let before = f.control.snapshot_requests();
    f.out
        .send(ServerMessage::ModelChanged {
            revision: 5,
            upserts: vec![],
            deletes: vec![],
        })
        .await
        .unwrap();
    f.control
        .wait_for("the resync request", |c| c.snapshot_requests() > before)
        .await;
    let flood = async |extra: u64| {
        for seq in 1..=extra {
            f.out
                .send(ServerMessage::Runtime {
                    seq,
                    changes: vec![],
                })
                .await
                .unwrap();
        }
        while f.out.0.capacity() < f.out.0.max_capacity() {
            tokio::task::yield_now().await;
        }
    };
    guard("the broadcasts to be fanned out", async {
        flood(1100).await;
        flood(1).await;
    })
    .await;
    *f.control.second.lock().unwrap() = None;
    f.out
        .0
        .send(Outgoing::doc(DOC2, ServerMessage::Closed { doc: DOC2 }))
        .await
        .unwrap();
    guard("the broadcasts to be fanned out", flood(1)).await;
    f.control.release();

    // The rebuild's list names one doc: the other's copy is gone (nothing answers for it, the
    // list omits it, and an un-prefixed GO reaches one doc only).
    guard("the rebuilt copies", async {
        loop {
            let replies = f.udp_ask(&msg("/cue/1/name", vec![])).await;
            if replies.len() == 1 {
                assert_eq!(replies[0]["data"], "Changed");
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    let listed = f.udp_ask(&msg("/workspaces", vec![])).await;
    let names: Vec<&str> = listed[0]["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["displayName"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["test"]);
    let inputs_before = f.control.inputs().len();
    f.udp_send(&msg("/go", vec![])).await;
    f.udp_sync().await;
    f.control
        .wait_seen("the GO", |seen| {
            seen.iter().filter(|m| is_input(m)).count() > inputs_before
        })
        .await;
    assert_eq!(f.control.inputs().len(), inputs_before + 1);
    // The closed doc is not addressable any more.
    let replies = f.udp_ask(&msg("/workspace/other/cue/1/name", vec![])).await;
    assert_eq!(replies[0]["status"], "error");
}

#[tokio::test]
async fn a_tcp_peer_that_never_reads_is_closed_and_stalls_nobody() {
    let mut ws = test_workspace();
    // Large replies, so that the kernel's buffers and our pending cap fill after a few hundred
    // reads even on a busy machine.
    ws.cue_lists[0].cues[0].notes = "x".repeat(60_000);
    let f = fixture_with(ws, |c| c.write_timeout = Duration::from_millis(200)).await;
    // A small receive buffer, so that a few MB of replies are more than the two sockets hold
    // (the server drops replies it cannot queue: the queue is what it can hold, not the flood).
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    let a = Tcp {
        stream: socket
            .connect((Ipv4Addr::LOCALHOST, f.tcp_port).into())
            .await
            .unwrap(),
        decoder: SlipDecoder::default(),
        frames: Vec::new(),
    };
    let frame = slip_encode(&encode_message(&read_notes()).unwrap());
    let burst: Vec<u8> = frame
        .iter()
        .copied()
        .cycle()
        .take(frame.len() * 100)
        .collect();
    // The actor answers far faster than the writer drains, so most replies are dropped at the
    // queue (at most 16 per read, and 256 queued); enough get through only if the flood is
    // large: the server's send buffer alone can take a few MB.
    f.wait_stats("the connection to be registered", |s| {
        s.tcp_connections == 1
    })
    .await;
    // Ask until the server's buffers and queue are full (the writer is then stuck for
    // `write_timeout` and closes the connection). Each reply is built whether or
    // not it can be queued, so a burst is sent only once the server has handled the one before
    // (a backlog in the kernel's buffers would only be more work for it). How many bursts fill
    // the buffers depends on the machine, so keep going until the server has closed the
    // connection, under the guard, instead of for a fixed count.
    let per_burst = 100;
    let mut sent = 0u64;
    guard("the stalled connection to be closed", async {
        while f.stats().tcp_connections != 0 {
            if !a.try_write_all(&burst).await {
                break;
            }
            sent += per_burst;
            while f.stats().tcp_connections != 0 && f.stats().osc.messages < sent {
                tokio::task::yield_now().await;
            }
        }
    })
    .await;
    // Others are served meanwhile.
    f.udp_send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "Opening");
    f.wait_stats("the stalled connection to be closed", |s| {
        s.tcp_connections == 0
    })
    .await;
    drop(a);
    let mut b = f.tcp().await;
    b.send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&b.recv().await.unwrap())["data"], "Opening");
}

#[tokio::test]
async fn an_idle_connection_is_closed_to_make_room_and_a_subscribed_one_is_not() {
    let f = fixture_with(test_workspace(), |c| {
        c.max_tcp_connections = 2;
        c.tcp_idle_evict = Duration::ZERO;
    })
    .await;
    let mut a = f.tcp().await;
    a.send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&a.recv().await.unwrap())["status"], "ok");
    let mut b = f.tcp().await;
    b.send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&b.recv().await.unwrap())["status"], "ok");
    // Full: a, the one silent longest, is closed for c.
    let mut c = f.tcp().await;
    c.send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&c.recv().await.unwrap())["status"], "ok");
    assert!(
        a.recv().await.is_none(),
        "the oldest idle connection stayed"
    );
    assert_eq!(f.stats().tcp_refused, 0);
    f.control
        .wait_for("the eviction log", |s| {
            s.logs
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.starts_with("OSC server closed an idle TCP connection"))
        })
        .await;
    // b stayed.
    b.send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&b.recv().await.unwrap())["status"], "ok");

    // Subscribed connections are never closed for this: with the table full of them the next
    // is refused.
    b.subscribe().await;
    c.subscribe().await;
    let mut d = f.tcp().await;
    assert!(d.recv().await.is_none());
    assert_eq!(f.stats().tcp_refused, 1);
}

#[tokio::test]
async fn input_that_waited_for_the_first_snapshot_too_long_is_dropped() {
    let f = fixture_held(test_workspace(), true, |c| c.max_input_age = Duration::ZERO).await;
    for _ in 0..3 {
        f.udp_send(&msg("/go", vec![])).await;
    }
    f.control
        .wait_for("the first snapshot request", |c| c.snapshot_requests() == 1)
        .await;
    f.control.release();
    f.wait_stats("the three GOs to be dropped", |s| s.inputs_dropped == 3)
        .await;
    assert!(f.control.inputs().is_empty());
}

#[tokio::test]
async fn a_passcode_changed_by_an_edit_ends_every_session() {
    let f = fixture(with_passcode("sesame")).await;
    let mut t = f.tcp().await;
    t.send(&msg("/connect", vec![text("sesame")])).await;
    assert_eq!(body(&t.recv().await.unwrap())["status"], "ok");
    let mut settings = with_passcode("hunter2").settings;
    settings.osc_access = single_passcode("hunter2");
    f.out
        .send(ServerMessage::ModelChanged {
            revision: 1,
            upserts: vec![Upsert::Settings {
                name: "test".into(),
                settings: Box::new(settings),
            }],
            deletes: vec![],
        })
        .await
        .unwrap();
    guard("the session to end", async {
        loop {
            t.send(&msg("/cue/1/name", vec![])).await;
            if body(&t.recv().await.unwrap())["status"] == "denied" {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    t.send(&msg("/connect", vec![text("hunter2")])).await;
    assert_eq!(body(&t.recv().await.unwrap())["status"], "ok");
}

#[tokio::test]
async fn with_a_passcode_an_authenticated_console_is_never_closed_for_room() {
    let f = fixture_with(with_passcode("sesame"), |c| {
        c.max_tcp_connections = 3;
        c.tcp_idle_evict = Duration::ZERO;
    })
    .await;
    // The console connected first, so it has been silent longest.
    let mut console = f.tcp().await;
    console.send(&msg("/connect", vec![text("sesame")])).await;
    assert_eq!(body(&console.recv().await.unwrap())["status"], "ok");
    let mut strangers = Vec::new();
    for _ in 0..2 {
        let mut t = f.tcp().await;
        t.send(&msg("/go", vec![])).await;
        assert_eq!(body(&t.recv().await.unwrap())["status"], "denied");
        strangers.push(t);
    }
    // Full: a newcomer takes the place of the stranger silent longest, never the console.
    let mut newcomer = f.tcp().await;
    newcomer.send(&msg("/go", vec![])).await;
    assert_eq!(body(&newcomer.recv().await.unwrap())["status"], "denied");
    assert!(strangers[0].recv().await.is_none());
    console.send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&console.recv().await.unwrap())["data"], "Opening");
    assert_eq!(f.stats().tcp_refused, 0);
}

#[tokio::test]
async fn with_a_passcode_and_only_authenticated_connections_the_newcomer_is_refused() {
    let f = fixture_with(with_passcode("sesame"), |c| {
        c.max_tcp_connections = 2;
        c.tcp_idle_evict = Duration::ZERO;
    })
    .await;
    let mut consoles = Vec::new();
    for _ in 0..2 {
        let mut t = f.tcp().await;
        t.send(&msg("/connect", vec![text("sesame")])).await;
        assert_eq!(body(&t.recv().await.unwrap())["status"], "ok");
        consoles.push(t);
    }
    let mut newcomer = f.tcp().await;
    assert!(newcomer.recv().await.is_none());
    assert_eq!(f.stats().tcp_refused, 1);
    for t in &mut consoles {
        t.send(&msg("/cue/1/name", vec![])).await;
        assert_eq!(body(&t.recv().await.unwrap())["status"], "ok");
    }
}

#[tokio::test]
async fn a_passcode_edit_that_fails_to_apply_still_ends_every_session() {
    let f = fixture(with_passcode("sesame")).await;
    let mut t = f.tcp().await;
    t.send(&msg("/connect", vec![text("sesame")])).await;
    assert_eq!(body(&t.recv().await.unwrap())["status"], "ok");
    let mut settings = with_passcode("hunter2").settings;
    settings.osc_access = single_passcode("hunter2");
    // The settings apply, then the cue upsert into a list that is not there fails.
    f.out
        .send(ServerMessage::ModelChanged {
            revision: 1,
            upserts: vec![
                Upsert::Settings {
                    name: "test".into(),
                    settings: Box::new(settings),
                },
                Upsert::Cue {
                    list: openlab_core::CueListId(uuid::Uuid::from_u128(4242)),
                    parent: None,
                    index: 0,
                    cue: Box::new(test_workspace().cue_lists[0].cues[0].clone()),
                },
            ],
            deletes: vec![],
        })
        .await
        .unwrap();
    guard("the session to end", async {
        loop {
            t.send(&msg("/cue/1/name", vec![])).await;
            if body(&t.recv().await.unwrap())["status"] == "denied" {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
}

// ---- QLab 5 behaviour (M4.5b) over real sockets

#[tokio::test]
async fn version_and_workspaces_report_the_bound_ports() {
    let f = fixture(test_workspace()).await;
    f.udp_send(&msg("/version", vec![])).await;
    let reply = f.udp_recv().await;
    assert_eq!(
        body(&reply),
        serde_json::json!({
            "address": "/version",
            "status": "ok",
            "data": env!("CARGO_PKG_VERSION"),
        })
    );
    f.udp_send(&msg("/workspaces", vec![])).await;
    let reply = f.udp_recv().await;
    let json = body(&reply);
    assert_eq!(json["address"], "/workspaces");
    assert!(json.get("workspace_id").is_none());
    let entry = &json["data"][0];
    assert_eq!(entry["uniqueID"], ws_id());
    assert_eq!(entry["port"], f.udp_port);
    assert_eq!(entry["udpReplyPort"], f.udp.local_addr().unwrap().port());
    assert_eq!(entry["version"], env!("CARGO_PKG_VERSION"));
    assert!(entry["displayName"].is_string());
    // Before a passcode: still answered.
    let guarded = fixture(with_passcode("sesame")).await;
    let mut t = guarded.tcp().await;
    t.send(&msg("/workspaces", vec![])).await;
    assert_eq!(body(&t.recv().await.unwrap())["status"], "ok");
}

#[tokio::test]
async fn an_identical_message_within_the_window_is_dropped_over_the_wire() {
    // A window an hour long: the second identical message is always a repeat, with no sleeping.
    let f = fixture_with(test_workspace(), |c| {
        c.duplicate_window = Duration::from_secs(3600);
    })
    .await;
    f.udp_send(&msg("/go", vec![])).await;
    f.udp_send(&msg("/go", vec![])).await;
    f.udp_send(&msg("/go", vec![])).await;
    // A different message is handled; its reply proves the repeats before it were seen.
    f.udp_send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "Opening");
    f.udp_send(&msg("/cue/1/name", vec![])).await;
    f.udp_send(&msg("/cue/2/name", vec![])).await;
    assert_eq!(body(&f.udp_recv().await)["status"], "ok");
    f.control
        .wait_seen("the one GO", |seen| {
            seen.iter().filter(|m| is_input(m)).count() == 1
        })
        .await;
    assert_eq!(f.stats().osc.duplicates_dropped, 3);
    assert_eq!(blank_ids(f.control.inputs()), vec![go()]);
    // Another peer (a TCP connection) is not affected by what UDP sent.
    let t = f.tcp().await;
    t.send(&msg("/go", vec![])).await;
    f.control
        .wait_seen("a GO over TCP", |seen| {
            seen.iter().filter(|m| is_input(m)).count() == 2
        })
        .await;
}

#[tokio::test]
async fn large_replies_go_over_udp_up_to_a_datagram_and_over_tcp_beyond() {
    let mut ws = test_workspace();
    ws.cue_lists[0].cues[0].notes = "m".repeat(100_000);
    ws.cue_lists[0].cues[1].notes = "n".repeat(60_000);
    let f = fixture(ws).await;
    // 60 KB fits a datagram, and is sent whole (QLab does).
    f.udp_send(&msg("/cue/2/notes", vec![])).await;
    let reply = f.udp_recv().await;
    assert_eq!(body(&reply)["data"].as_str().unwrap().len(), 60_000);
    // 100 KB cannot: an error reply, with no data.
    f.udp_send(&msg("/cue/1/notes", vec![])).await;
    let json = body(&f.udp_recv().await);
    assert_eq!(json["status"], "error");
    assert!(json.get("data").is_none());
    // Over TCP the same read is answered whole.
    let mut t = f.tcp().await;
    t.send(&msg("/cue/1/notes", vec![])).await;
    let reply = t.recv().await.unwrap();
    assert_eq!(body(&reply)["data"].as_str().unwrap().len(), 100_000);
}

#[tokio::test]
async fn a_udp_reply_the_system_cannot_send_is_counted_and_nothing_hangs() {
    // The first send fails, as a datagram too big for the send buffer does on macOS (the seam
    // is used because no destination fails on every system).
    let f = fixture_with(test_workspace(), |c| c.fail_udp_sends = 1).await;
    f.udp_send(&msg("/cue/1/name", vec![])).await;
    f.wait_stats("the failed send to be counted", |s| s.send_failed == 1)
        .await;
    // The server goes on serving: the peer names a port and is answered.
    let target = f.udp.local_addr().unwrap().port();
    f.udp_send(&msg("/udpReplyPort", vec![OscType::Int(i32::from(target))]))
        .await;
    f.udp_send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "Opening");
    assert_eq!(f.stats().send_failed, 1);
}

#[tokio::test]
async fn a_tcp_message_may_be_nearly_a_megabyte() {
    let f = fixture(test_workspace()).await;
    let t = f.tcp().await;
    let notes = "x".repeat(900 * 1024);
    t.send(&msg("/cue/1/notes", vec![text(&notes)])).await;
    f.control
        .wait_seen("the note", |seen| seen.iter().any(is_input))
        .await;
    let inputs = f.control.inputs();
    let [ClientMessage::Command { command, .. }] = inputs.as_slice() else {
        panic!("{inputs:?}")
    };
    let openlab_core::Command::SetCueProperty {
        property: openlab_core::CueProperty::Notes { value },
        ..
    } = command.as_ref()
    else {
        panic!("{command:?}")
    };
    assert_eq!(value.len(), notes.len());
    assert_eq!(f.stats().osc.oversize_frames, 0);
}

#[tokio::test]
async fn a_peer_that_asks_and_does_not_read_cannot_make_the_server_hold_gigabytes() {
    let mut ws = test_workspace();
    ws.cue_lists[0].cues[0].notes = "m".repeat(900 * 1024);
    // The peer never reads, so the writer is blocked for the whole test: the write timeout (10 s
    // by default, which a loaded machine can pass) must not close the connection under it.
    let f = fixture_with(ws, |c| c.write_timeout = Duration::from_secs(3600)).await;
    let t = f.tcp().await;
    // 30 rounds of two reads of 900 KB (a read of the socket may hold both, and the server stops
    // at 2 MiB of replies per read) that the peer never reads: 54 MB offered. The writer holds
    // at most 4 MiB per connection that the socket has not taken, so the rest is dropped (and
    // counted) instead of queued.
    for round in 1..=30u64 {
        t.send(&msg("/cue/1/notes", vec![])).await;
        t.send(&msg("/cue/1/notes", vec![])).await;
        f.wait_stats("the requests to be handled", |s| {
            s.osc.messages == 2 * round
        })
        .await;
    }
    assert!(f.stats().send_failed > 0, "{:?}", f.stats());
    assert!(f.stats().tcp_pending_bytes <= 4 * 1024 * 1024);
    // When the peer goes, what was queued for it is released (the counters must not leak: past
    // 64 MiB no connection would be sent anything).
    drop(t);
    f.wait_stats("the queued bytes to be released", |s| {
        s.tcp_pending_bytes == 0 && s.tcp_connections == 0
    })
    .await;
}

#[tokio::test]
async fn the_tcp_connection_cap_defaults_to_256() {
    assert_eq!(OscConfig::default().max_tcp_connections, 256);
    assert_eq!(OscConfig::default().max_tcp_connections_per_host, 16);
    // Every test peer is one host: lift its own cap to see the table's.
    let f = fixture_with(test_workspace(), |c| {
        c.max_tcp_connections = OscConfig::default().max_tcp_connections;
        c.max_tcp_connections_per_host = usize::MAX;
    })
    .await;
    // Many connections at once are served.
    let mut conns = Vec::new();
    for _ in 0..40 {
        let mut t = f.tcp().await;
        t.send(&msg("/version", vec![])).await;
        assert_eq!(t.recv().await.unwrap().addr, "/reply/version");
        conns.push(t);
    }
    assert_eq!(f.stats().tcp_refused, 0);
}

#[tokio::test]
async fn the_active_list_survives_a_resync_and_a_snapshot_so_go_fires_the_same_list() {
    let mut ws = test_workspace();
    let second = openlab_core::CueListId(uuid::Uuid::from_u128(0x22));
    let mut c = ws.cue_lists[0].cues[0].clone();
    c.id = id(20);
    c.number = Some("20".into());
    ws.cue_lists.push(openlab_core::CueList {
        id: second,
        name: "Second".into(),
        cues: vec![c],
    });
    let f = fixture(ws).await;
    let go_list = |f: &Fix| -> Option<openlab_core::CueListId> {
        f.control.inputs().iter().rev().find_map(|m| match m {
            ClientMessage::Transport {
                action: TransportAction::Go { list },
                ..
            } => Some(*list),
            _ => None,
        })
    };
    // The operator works in the second list: its playhead moves.
    f.out
        .send(ServerMessage::Runtime {
            seq: 1,
            changes: vec![RuntimeChange::Playhead {
                list: second,
                cue: Some(id(20)),
            }],
        })
        .await
        .unwrap();
    f.udp_sync().await;
    f.udp_send(&msg("/playheadID", vec![])).await;
    assert_eq!(
        body(&f.udp_recv().await)["data"],
        id(20).to_string().to_uppercase()
    );
    // A Save As (another document path) and a gap both make the copy start over from a snapshot.
    let asked = f.control.snapshot_requests();
    f.out
        .send(ServerMessage::Document {
            document: DocumentState {
                path: Some("/shows/saved.openlab".into()),
                dirty: false,
                can_undo: false,
                can_redo: false,
                show_mode: false,
            },
        })
        .await
        .unwrap();
    f.control
        .wait_for("the resync", |c| c.snapshot_requests() > asked)
        .await;
    f.udp_sync().await;
    f.udp_send(&msg("/go", vec![])).await;
    f.control
        .wait_seen("the GO", |seen| seen.iter().any(is_input))
        .await;
    assert_eq!(go_list(&f), Some(second), "list 2 is still the active one");
    let asked = f.control.snapshot_requests();
    f.out
        .send(ServerMessage::ModelChanged {
            revision: 9,
            upserts: vec![],
            deletes: vec![],
        })
        .await
        .unwrap();
    f.control
        .wait_for("the gap resync", |c| c.snapshot_requests() > asked)
        .await;
    f.udp_sync().await;
    f.udp_send(&msg("/go", vec![])).await;
    f.control
        .wait_seen("the second GO", |seen| {
            seen.iter().filter(|m| is_input(m)).count() >= 2
        })
        .await;
    assert_eq!(go_list(&f), Some(second));
}

/// The next datagram on `socket`, which is not the fixture's reply socket.
async fn recv_on(socket: &UdpSocket) -> OscMessage {
    let mut buf = vec![0u8; 70_000];
    let (n, _) = guard("waiting for a UDP datagram", socket.recv_from(&mut buf))
        .await
        .unwrap();
    decode_packet(&buf[..n]).unwrap().remove(0)
}

/// Points the fixture's peer (127.0.0.1) at `socket`'s port.
async fn redirect(f: &Fix, socket: &UdpSocket) {
    let port = i32::from(socket.local_addr().unwrap().port());
    f.udp_send(&msg("/udpReplyPort", vec![OscType::Int(port)]))
        .await;
}

#[tokio::test]
async fn udp_reply_port_moves_replies_to_the_new_port_and_sends_no_reply_itself() {
    let f = fixture(test_workspace()).await;
    let alt = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    redirect(&f, &alt).await;
    f.udp_send(&msg("/cue/1/name", vec![])).await;
    // The first datagram on the new port is the read's reply, so `/udpReplyPort` had none.
    let reply = recv_on(&alt).await;
    assert_eq!(reply.addr, "/reply/cue/1/name");
    assert_eq!(body(&reply)["data"], "Opening");
    // Nothing went to the old port: point back and the first thing there is the next reply.
    redirect(&f, &f.udp).await;
    f.udp_send(&msg("/version", vec![])).await;
    assert_eq!(f.udp_recv().await.addr, "/reply/version");
}

#[tokio::test]
async fn updates_follow_the_reply_port() {
    let f = fixture(test_workspace()).await;
    let alt = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    redirect(&f, &alt).await;
    f.udp_send(&msg("/updates", vec![OscType::Int(1)])).await;
    f.udp_send(&msg("/version", vec![])).await;
    assert_eq!(recv_on(&alt).await.addr, "/reply/version");
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
    assert!(recv_on(&alt).await.addr.ends_with("/playbackPosition"));
    // None on the default port: point back and the next datagram there is a reply.
    redirect(&f, &f.udp).await;
    f.udp_send(&msg("/version", vec![])).await;
    assert_eq!(f.udp_recv().await.addr, "/reply/version");
}

#[tokio::test]
async fn udp_reply_port_over_tcp_is_ignored_and_invalid_ports_do_not_move_replies() {
    let f = fixture(test_workspace()).await;
    let alt = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let mut t = f.tcp().await;
    let port = i32::from(alt.local_addr().unwrap().port());
    t.send(&msg("/udpReplyPort", vec![OscType::Int(port)]))
        .await;
    t.send(&msg("/version", vec![])).await;
    // The first frame is `/version`'s: the port message got no reply.
    assert_eq!(t.recv().await.unwrap().addr, "/reply/version");
    // The UDP peer at the same IP still gets its replies on the default port.
    f.udp_send(&msg("/udpReplyPort", vec![OscType::Int(70000)]))
        .await;
    assert_eq!(body(&f.udp_recv().await)["status"], "error");
    f.udp_send(&msg("/version", vec![])).await;
    assert_eq!(f.udp_recv().await.addr, "/reply/version");
}

// ---- passcode rights over real sockets (M4.5c)

fn rights(view: bool, edit: bool, control: bool) -> OscRights {
    OscRights {
        view,
        edit,
        control,
    }
}

fn row(passcode: &str, rights: OscRights) -> OscPasscode {
    OscPasscode {
        passcode: passcode.into(),
        rights,
    }
}

/// Nothing without a passcode; 1111 View; 2222 View + Edit; 3333 View + Control; 4444 all.
fn rights_table(none: OscRights) -> OscAccess {
    OscAccess {
        no_passcode: none,
        passcodes: vec![
            row("1111", rights(true, false, false)),
            row("2222", rights(true, true, false)),
            row("3333", rights(true, false, true)),
            row("4444", OscRights::ALL),
        ],
    }
}

fn with_table(access: OscAccess) -> Workspace {
    let mut ws = test_workspace();
    ws.settings.osc_access = access;
    ws
}

fn status(m: &OscMessage) -> String {
    body(m)["status"].as_str().unwrap_or_default().to_owned()
}

/// What the control loop was sent so far that is not a snapshot request.
fn input_count(f: &Fix) -> usize {
    f.control.inputs().len()
}

#[tokio::test]
async fn rights_are_enforced_over_tcp() {
    let f = fixture(with_table(rights_table(OscRights::NONE))).await;
    let mut t = f.tcp().await;
    t.send(&msg("/connect", vec![text("1111")])).await;
    assert_eq!(body(&t.recv().await.unwrap())["data"], "ok:view");
    // View: reads, and notes.
    t.send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&t.recv().await.unwrap())["data"], "Opening");
    t.send(&msg("/cue/1/notes", vec![text("a note")])).await;
    f.control
        .wait_seen("the notes edit", |seen| {
            seen.iter()
                .any(|m| matches!(m, ClientMessage::Command { .. }))
        })
        .await;
    assert_eq!(input_count(&f), 1);
    // Not Edit, not Control: denied, and nothing reaches the control loop.
    for m in [
        msg("/cue/1/name", vec![text("x")]),
        msg("/cue/1/preWait", vec![OscType::Float(2.0)]),
        msg("/select/1", vec![]),
        msg("/go", vec![]),
        msg("/playhead/1", vec![]),
        msg("/cue/1/start", vec![]),
        msg("/lights/blackout", vec![]),
    ] {
        t.send(&m).await;
        assert_eq!(status(&t.recv().await.unwrap()), "denied", "{}", m.addr);
    }
    assert_eq!(input_count(&f), 1);
    assert_eq!(f.stats().osc.denied, 7);
    // Another connection with the rights for each.
    let mut c = f.tcp().await;
    c.send(&msg("/connect", vec![text("3333")])).await;
    assert_eq!(body(&c.recv().await.unwrap())["data"], "ok:view|control");
    c.send(&msg("/go", vec![])).await;
    c.send(&msg("/cue/1/name", vec![text("x")])).await;
    assert_eq!(status(&c.recv().await.unwrap()), "denied");
    let mut e = f.tcp().await;
    e.send(&msg("/connect", vec![text("2222")])).await;
    assert_eq!(body(&e.recv().await.unwrap())["data"], "ok:view|edit");
    e.send(&msg("/cue/1/name", vec![text("New")])).await;
    e.send(&msg("/go", vec![])).await;
    assert_eq!(status(&e.recv().await.unwrap()), "denied");
    f.control
        .wait_seen("go and the rename", |seen| {
            seen.iter().filter(|m| is_input(m)).count() == 3
        })
        .await;
    assert_eq!(f.stats().osc.denied, 9);
}

#[tokio::test]
async fn rights_are_enforced_over_udp() {
    let f = fixture(with_table(rights_table(OscRights::NONE))).await;
    // Not connected: the No Passcode row has nothing.
    f.udp_send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(status(&f.udp_recv().await), "denied");
    f.udp_send(&msg("/connect", vec![text("2222")])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "ok:view|edit");
    f.udp_send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "Opening");
    f.udp_send(&msg("/go", vec![])).await;
    let denied = f.udp_recv().await;
    assert_eq!(denied.addr, "/reply/go");
    assert_eq!(status(&denied), "denied");
    f.udp_send(&msg("/select/1", vec![])).await;
    f.control
        .wait_seen("the select", |seen| seen.iter().any(is_input))
        .await;
    assert_eq!(input_count(&f), 1);
    // The same address with another passcode raises the rights.
    f.udp_send(&msg("/connect", vec![text("4444")])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "ok:view|edit|control");
    f.udp_send(&msg("/go", vec![])).await;
    f.control
        .wait_seen("the GO", |seen| {
            seen.iter().filter(|m| is_input(m)).count() == 2
        })
        .await;
}

#[tokio::test]
async fn no_passcode_rights_apply_without_connecting_and_connect_without_an_argument_gives_them() {
    let f = fixture(with_table(rights_table(rights(true, false, false)))).await;
    // View without connecting, over both transports.
    f.udp_send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "Opening");
    let mut t = f.tcp().await;
    t.send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&t.recv().await.unwrap())["data"], "Opening");
    t.send(&msg("/go", vec![])).await;
    assert_eq!(status(&t.recv().await.unwrap()), "denied");
    // `/connect` with no argument says what the peer has.
    t.send(&msg("/connect", vec![])).await;
    let reply = t.recv().await.unwrap();
    assert_eq!(body(&reply)["status"], "ok");
    assert_eq!(body(&reply)["data"], "ok:view");
    f.udp_send(&msg("/connect", vec![])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "ok:view");
    t.send(&msg("/go", vec![])).await;
    assert_eq!(status(&t.recv().await.unwrap()), "denied");
    assert!(f.control.inputs().is_empty());
    // And the table's other rows are still reachable.
    t.send(&msg("/connect", vec![text("4444")])).await;
    assert_eq!(
        body(&t.recv().await.unwrap())["data"],
        "ok:view|edit|control"
    );
    t.send(&msg("/go", vec![])).await;
    f.control
        .wait_seen("the GO", |seen| seen.iter().any(is_input))
        .await;
}

#[tokio::test]
async fn a_new_workspaces_table_asks_for_its_passcode() {
    // What `Request::New` makes: nothing without the passcode, all rights with it.
    let f = fixture(with_table(OscAccess::new_workspace(731))).await;
    f.udp_send(&msg("/connect", vec![])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "ok:");
    f.udp_send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(status(&f.udp_recv().await), "denied");
    f.udp_send(&msg("/connect", vec![text("1731")])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "ok:view|edit|control");
    f.udp_send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "Opening");
}

fn settings_upsert(access: OscAccess) -> Upsert {
    Upsert::Settings {
        name: "test".into(),
        settings: Box::new(with_table(access).settings),
    }
}

#[tokio::test]
async fn adding_a_passcode_keeps_sessions_but_narrowing_the_table_ends_them() {
    let f = fixture(with_table(rights_table(OscRights::NONE))).await;
    let mut console = f.tcp().await;
    console.send(&msg("/connect", vec![text("4444")])).await;
    assert_eq!(body(&console.recv().await.unwrap())["status"], "ok");
    // A row is added mid-show: nobody has to connect again.
    let mut grown = rights_table(OscRights::NONE);
    grown
        .passcodes
        .push(row("5555", rights(true, false, false)));
    f.out
        .send(ServerMessage::ModelChanged {
            revision: 1,
            upserts: vec![settings_upsert(grown.clone())],
            deletes: vec![],
        })
        .await
        .unwrap();
    // The new row works, and the console's session survived it.
    let mut viewer = f.tcp().await;
    guard("the new row to work", async {
        loop {
            viewer.send(&msg("/connect", vec![text("5555")])).await;
            if body(&viewer.recv().await.unwrap())["data"] == "ok:view" {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    console.send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&console.recv().await.unwrap())["data"], "Opening");
    // The console's row loses Control: its session ends, and it must connect again.
    let mut narrowed = grown;
    narrowed.passcodes[3].rights = rights(true, true, false);
    f.out
        .send(ServerMessage::ModelChanged {
            revision: 2,
            upserts: vec![settings_upsert(narrowed)],
            deletes: vec![],
        })
        .await
        .unwrap();
    guard("the session to end", async {
        loop {
            console.send(&msg("/cue/1/name", vec![])).await;
            if status(&console.recv().await.unwrap()) == "denied" {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    console.send(&msg("/connect", vec![text("4444")])).await;
    assert_eq!(body(&console.recv().await.unwrap())["data"], "ok:view|edit");
    // A change of the No Passcode rights ends sessions too, and applies to peers that never
    // connected.
    let mut stranger = f.tcp().await;
    stranger.send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(status(&stranger.recv().await.unwrap()), "denied");
    let mut opened = rights_table(OscRights::NONE);
    opened.no_passcode = rights(true, false, false);
    f.out
        .send(ServerMessage::ModelChanged {
            revision: 3,
            upserts: vec![settings_upsert(opened)],
            deletes: vec![],
        })
        .await
        .unwrap();
    guard("the No Passcode rights to apply", async {
        loop {
            stranger.send(&msg("/cue/1/name", vec![])).await;
            if status(&stranger.recv().await.unwrap()) == "ok" {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
}

#[tokio::test]
async fn updates_go_only_to_subscribers_that_may_view() {
    let f = fixture(with_table(rights_table(OscRights::NONE))).await;
    // `/updates` is accepted at any access level (no reply) ...
    f.udp_subscribe().await;
    let rename = |name: &str, revision| {
        let mut edited = test_workspace().cue_lists[0].cues[0].clone();
        edited.name = name.into();
        ServerMessage::ModelChanged {
            revision,
            upserts: vec![Upsert::Cue {
                list: list_id(),
                parent: None,
                index: 0,
                cue: Box::new(edited),
            }],
            deletes: vec![],
        }
    };
    // ... but this peer may not view, so the cue data stays away. The round trip answers
    // before anything else arrives only if no update was sent.
    f.out.send(rename("Hidden", 1)).await.unwrap();
    f.udp_sync().await;
    // Connecting with a View passcode starts the updates (the subscription was kept).
    f.udp_send(&msg("/connect", vec![text("1111")])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "ok:view");
    f.out.send(rename("Shown", 2)).await.unwrap();
    let (addr, _) = next_update(&f).await;
    assert_eq!(
        addr,
        format!("/update/workspace/{}/cue_id/{}", ws_id(), id(1))
    );
}

/// A UDP socket on another loopback address that receives the replies the server sends to that
/// address (the reply port is the fixture's own).
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn udp_from(f: &Fix, last_octet: u8) -> std::net::UdpSocket {
    let reply_port = f.udp.local_addr().unwrap().port();
    let socket =
        std::net::UdpSocket::bind((Ipv4Addr::new(127, 0, 0, last_octet), reply_port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    socket
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn send_from(f: &Fix, socket: &std::net::UdpSocket, m: &OscMessage) {
    socket
        .send_to(&encode_message(m).unwrap(), f.udp_target())
        .unwrap();
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn recv_from(socket: &std::net::UdpSocket) -> OscMessage {
    let mut buf = vec![0u8; 70_000];
    let (n, _) = socket.recv_from(&mut buf).unwrap();
    decode_packet(&buf[..n]).unwrap().remove(0)
}

/// Wrong passcodes from other addresses until the global budget is used up, proved by a
/// stranger's correct passcode being refused.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn exhaust_the_global_budget(f: &Fix) {
    let other = udp_from(f, 2);
    for _ in 0..60 {
        send_from(f, &other, &msg("/connect", vec![text("0000")]));
    }
    let stranger = udp_from(f, 3);
    send_from(f, &stranger, &msg("/connect", vec![text("4444")]));
    assert_eq!(status(&recv_from(&stranger)), "denied");
}

/// Polls until the console on the fixture's own address has lost its session.
#[cfg_attr(target_os = "macos", allow(dead_code))]
async fn wait_until_the_console_is_disconnected(f: &Fix) {
    guard("the session to end", async {
        loop {
            f.udp_send(&msg("/cue/1/name", vec![])).await;
            if status(&f.udp_recv().await) == "denied" {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
}

// macOS has no 127.0.0.2 by default.
#[cfg(not(target_os = "macos"))]
#[tokio::test]
async fn a_table_edit_ends_sessions_but_the_known_address_still_passes_the_global_budget() {
    let f = fixture(with_table(rights_table(OscRights::NONE))).await;
    f.udp_send(&msg("/connect", vec![text("4444")])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "ok:view|edit|control");
    let mut narrowed = rights_table(OscRights::NONE);
    narrowed.passcodes[3].rights = rights(true, true, false);
    f.out
        .send(ServerMessage::ModelChanged {
            revision: 1,
            upserts: vec![settings_upsert(narrowed)],
            deletes: vec![],
        })
        .await
        .unwrap();
    wait_until_the_console_is_disconnected(&f).await;
    exhaust_the_global_budget(&f);
    // The address connected before the edit, so its correct passcode still gets in.
    f.udp_send(&msg("/connect", vec![text("4444")])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "ok:view|edit");
}

#[cfg(not(target_os = "macos"))]
#[tokio::test]
async fn opening_another_workspace_forgets_the_known_addresses() {
    let f = fixture(with_table(rights_table(OscRights::NONE))).await;
    f.udp_send(&msg("/connect", vec![text("4444")])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "ok:view|edit|control");
    // Another workspace (another id, the same table) replaces the copy through a snapshot.
    {
        let mut guard = f.control.workspace.lock().unwrap();
        guard.as_mut().unwrap().id = openlab_core::WorkspaceId(uuid::Uuid::from_u128(77));
    }
    f.out
        .send(ServerMessage::ModelChanged {
            revision: 9,
            upserts: vec![],
            deletes: vec![],
        })
        .await
        .unwrap();
    wait_until_the_console_is_disconnected(&f).await;
    exhaust_the_global_budget(&f);
    f.udp_send(&msg("/connect", vec![text("4444")])).await;
    assert_eq!(status(&f.udp_recv().await), "denied");
}

/// M4.15h, issue #152: a doc's `closed` drops its copy at once, so reads are not answered from a
/// show that is no longer open; a list from the tap that leaves a doc out (it may be older than
/// what the actor knows) does not drop it.
#[tokio::test]
async fn a_closed_doc_is_not_answered_from_and_a_list_never_drops_a_doc() {
    let f = fixture(test_workspace()).await;
    f.udp_send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "Opening");
    // An empty list on the tap: the doc stays.
    f.out
        .0
        .send(Outgoing::all(ServerMessage::Workspaces {
            workspaces: vec![],
        }))
        .await
        .unwrap();
    f.udp_send(&msg("/cue/1/name", vec![])).await;
    assert_eq!(body(&f.udp_recv().await)["data"], "Opening");
    // Its `closed`: gone, and with no doc open the read is an error naming no workspace.
    f.out
        .send(ServerMessage::Closed { doc: DOC })
        .await
        .unwrap();
    guard("the copy to go", async {
        loop {
            f.udp_send(&msg("/cue/1/name", vec![])).await;
            let reply = body(&f.udp_recv().await);
            if reply["status"] == "error" {
                assert_eq!(
                    reply,
                    serde_json::json!({"status": "error", "address": "/cue/1/name"})
                );
                return;
            }
        }
    })
    .await;
}
