// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Whole show sessions over real sockets (M4.10, QA-04): the WebSocket server and the OSC
//! listeners in front of the real `Control`, driven on a virtual clock.
//!
//! `runner::run` never renders in the offline backend, so sample time would stand still. These
//! tests run their own control thread instead: it handles inbox messages as they come and renders
//! blocks only when the test says so (`Show::advance`), so every start, fade, pause and end
//! happens at a sample position the test chose, and nothing depends on the wall clock. The
//! timeouts below are hang guards only.
//!
//! Coverage, against the other net tests: `net_ws` and `net_osc` use a stand-in control thread
//! (framing, limits, resync, refusals); `net_control` and `net_osc::go_over_the_wire...` run the
//! real loop but without time passing. Here: a full session per protocol (edits, undo/redo, GO,
//! runtime and progress, pause, resume, panic, shutdown), and both protocols and several clients
//! at once seeing the same show.

mod show;

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use futures_util::{SinkExt, StreamExt};
use openlab_audio::{Mixer, OfflineRenderer};
use openlab_core::protocol::{
    ClientMessage, Delete, Outcome, PROTOCOL_VERSION, Request, RequestId, RuntimeChange,
    ServerMessage, Snapshot, TransportAction, Upsert, WireCueState, decode_server, encode_client,
};
use openlab_core::{Command, CueId, CueKind, CueProperty, Position, Workspace};
use openlab_engine::control::{
    Backend, Control, Flow, Inbox, OUTBOUND_CAPACITY, outbound_channel, reply_channel,
};
use openlab_engine::net::{OscConfig, ServerConfig, WsServer};
use openlab_engine::osc::{SlipDecoder, SlipEvent, decode_packet, encode_message, slip_encode};
use rosc::{OscMessage, OscType};
use serde_json::Value;
use show::{BLOCK, audio, config, cue, dc, id, list_id, mixer_config, wav, workspace};
use tokio::net::{TcpStream, UdpSocket};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

const GUARD: Duration = Duration::from_secs(30);
const SETTLE: Duration = Duration::from_secs(30);

async fn guard<F: std::future::Future>(what: &str, f: F) -> F::Output {
    match tokio::time::timeout(GUARD, f).await {
        Ok(out) => out,
        Err(_) => panic!("hang: {what} took longer than {GUARD:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// The control thread on a virtual clock

struct Advance {
    frames: u64,
    done: Sender<Advanced>,
}

/// What rendering some frames produced.
struct Advanced {
    peak: f32,
    finite: bool,
}

fn control_loop(
    mut control: Control,
    mut renderer: OfflineRenderer,
    inbox: &Receiver<Inbox>,
    adv: &Receiver<Advance>,
) {
    let mut buf = vec![0.0f32; BLOCK as usize * 2];
    loop {
        crossbeam_channel::select! {
            recv(inbox) -> m => match m {
                Ok(m) => {
                    if control.handle(m) == Flow::Shutdown {
                        break;
                    }
                    control.tick();
                }
                Err(_) => break,
            },
            recv(adv) -> a => match a {
                Ok(a) => {
                    let (out, stop) = advance(&mut control, &mut renderer, inbox, &mut buf, a.frames);
                    let _ = a.done.send(out);
                    if stop {
                        break;
                    }
                }
                Err(_) => break,
            },
        }
    }
    control.shutdown();
}

/// What `OfflineShow` does per step, minus its capture buffer. Returns whether a shutdown came
/// in meanwhile.
fn advance(
    control: &mut Control,
    renderer: &mut OfflineRenderer,
    inbox: &Receiver<Inbox>,
    buf: &mut [f32],
    frames: u64,
) -> (Advanced, bool) {
    let mut out = Advanced {
        peak: 0.0,
        finite: true,
    };
    let deadline = std::time::Instant::now() + SETTLE;
    // Media answers first, so a start does not wait on a probe for want of a wake.
    while control.probes_pending() > 0 || control.preloads_pending() > 0 {
        if std::time::Instant::now() >= deadline {
            panic!("probes never answered");
        }
        match inbox.recv_timeout(Duration::from_millis(20)) {
            Ok(m) => {
                if control.handle(m) == Flow::Shutdown {
                    return (out, true);
                }
            }
            Err(_) => control.tick(),
        }
    }
    control.tick();
    let until = control.mixer().samples_rendered() + frames;
    while control.mixer().samples_rendered() < until {
        while let Ok(m) = inbox.try_recv() {
            if control.handle(m) == Flow::Shutdown {
                return (out, true);
            }
        }
        let end = control.mixer().samples_rendered() + BLOCK;
        for (handle, needed) in control.stream_needs(end) {
            let _ = handle.wait_written(needed, SETTLE);
        }
        for handle in control.prepared_streams() {
            let _ = handle.wait_primed(SETTLE);
        }
        renderer.render_into(buf, &[BLOCK as usize]);
        for s in buf.iter() {
            out.finite &= s.is_finite();
            out.peak = out.peak.max(s.abs());
        }
        control.tick();
    }
    (out, false)
}

struct Show {
    inbox: Sender<Inbox>,
    adv: Sender<Advance>,
    worker: Option<JoinHandle<()>>,
    server: WsServer,
}

impl Show {
    fn start(ws: &Workspace, osc_reply_port: Option<u16>) -> Self {
        let (ctl, mixer_rt) = Mixer::new(mixer_config()).unwrap();
        let (inbox_tx, inbox) = crossbeam_channel::unbounded();
        let (adv, adv_rx) = crossbeam_channel::unbounded();
        let (out_tx, outbound) = outbound_channel(OUTBOUND_CAPACITY);
        let control = Control::new(
            ws.clone(),
            ctl,
            Backend::Offline,
            config(ws),
            inbox_tx.clone(),
            out_tx,
        )
        .unwrap();
        let renderer = OfflineRenderer::new(mixer_rt);
        let worker = thread::spawn(move || control_loop(control, renderer, &inbox, &adv_rx));
        let server = WsServer::start(
            inbox_tx.clone(),
            outbound,
            ServerConfig {
                osc: osc_reply_port.map(|reply_port| OscConfig {
                    bind: Ipv4Addr::LOCALHOST.into(),
                    port: 0,
                    reply_port,
                    ..OscConfig::default()
                }),
                ..ServerConfig::default()
            },
        )
        .unwrap();
        Self {
            inbox: inbox_tx,
            adv,
            worker: Some(worker),
            server,
        }
    }

    /// Settles media, then renders `frames` (whole blocks). Blocks the caller until done.
    fn advance(&self, frames: u64) -> Advanced {
        let (done, rx) = crossbeam_channel::bounded(1);
        self.adv.send(Advance { frames, done }).unwrap();
        let out = rx
            .recv_timeout(Duration::from_secs(120))
            .expect("hang: advance");
        assert!(out.finite, "the output has a NaN or infinity");
        out
    }

    /// The engine's own state, asked as a client would: a snapshot request through the inbox.
    async fn engine_snapshot(&self) -> Snapshot {
        let (reply, mut rx) = reply_channel();
        self.inbox
            .send(Inbox::Client {
                message: ClientMessage::Request {
                    id: RequestId(1),
                    request: Request::Snapshot,
                },
                reply,
            })
            .unwrap();
        guard("a snapshot", async {
            loop {
                if let ServerMessage::Snapshot { snapshot } = rx.recv().await.unwrap() {
                    return *snapshot;
                }
            }
        })
        .await
    }

    fn finish(mut self) {
        let _ = self.inbox.send(Inbox::Shutdown);
        self.worker.take().unwrap().join().unwrap();
        self.server.stop();
    }
}

// ---------------------------------------------------------------------------------------------
// A WebSocket client that checks the stream's invariants as it reads

struct Ws {
    c: WebSocketStream<MaybeTlsStream<TcpStream>>,
    revision: u64,
    seq: u64,
    next_id: u64,
    document: openlab_core::protocol::DocumentState,
    /// Non-idle cue states and the playhead, as a client folds them from `runtime`.
    states: HashMap<CueId, WireCueState>,
    playhead: Option<CueId>,
    progress: Vec<(CueId, f64, Option<f64>)>,
    changed: Vec<(Vec<Upsert>, Vec<Delete>)>,
}

impl Ws {
    async fn connect(show: &Show) -> (Self, Snapshot) {
        let (mut c, _) = connect_async(format!("ws://127.0.0.1:{}", show.server.port()))
            .await
            .unwrap();
        let hello = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            token: show.server.token().to_owned(),
            client_name: "session test".into(),
        };
        c.send(Message::text(encode_client(&hello).unwrap()))
            .await
            .unwrap();
        let mut me = Self {
            c,
            revision: 0,
            seq: 0,
            next_id: 0,
            document: openlab_core::protocol::DocumentState {
                path: None,
                dirty: false,
                can_undo: false,
                can_redo: false,
            },
            states: HashMap::new(),
            playhead: None,
            progress: Vec::new(),
            changed: Vec::new(),
        };
        let ServerMessage::Welcome { snapshot, .. } = me.raw().await else {
            panic!("the first message was not a welcome")
        };
        me.adopt(&snapshot);
        (me, *snapshot)
    }

    fn adopt(&mut self, s: &Snapshot) {
        self.revision = s.revision;
        self.seq = s.runtime_seq;
        self.document = s.document.clone();
        self.states = s.cue_states.iter().map(|e| (e.cue, e.state)).collect();
        self.playhead = s.playheads.iter().find_map(|p| p.cue);
    }

    /// The next message, skipping pings.
    async fn raw(&mut self) -> ServerMessage {
        guard("a WebSocket message", async {
            loop {
                match self.c.next().await {
                    Some(Ok(Message::Text(t))) => return decode_server(t.as_str()).unwrap(),
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    other => panic!("the connection ended: {other:?}"),
                }
            }
        })
        .await
    }

    /// The next message, folded into this client's copy of the show; checks that revisions and
    /// runtime sequence numbers rise by exactly one.
    async fn next(&mut self) -> ServerMessage {
        let m = self.raw().await;
        match &m {
            ServerMessage::Snapshot { snapshot } => self.adopt(snapshot),
            ServerMessage::ModelChanged {
                revision,
                upserts,
                deletes,
            } => {
                assert_eq!(*revision, self.revision + 1, "a model_changed was skipped");
                self.revision = *revision;
                self.changed.push((upserts.clone(), deletes.clone()));
            }
            ServerMessage::Document { document } => self.document = document.clone(),
            ServerMessage::Runtime { seq, changes } => {
                assert_eq!(*seq, self.seq + 1, "a runtime message was skipped");
                self.seq = *seq;
                for c in changes {
                    match c {
                        RuntimeChange::CueState { cue, state, .. } => {
                            if *state == WireCueState::Idle {
                                self.states.remove(cue);
                            } else {
                                self.states.insert(*cue, *state);
                            }
                        }
                        RuntimeChange::Playhead { cue, .. } => self.playhead = *cue,
                        _ => {}
                    }
                }
            }
            ServerMessage::Progress { items } => {
                self.progress = items
                    .iter()
                    .map(|p| (p.cue, p.elapsed, p.remaining))
                    .collect();
            }
            ServerMessage::Error { .. } => panic!("{m:?}"),
            _ => {}
        }
        m
    }

    /// Reads until `pred` holds for this client's folded state.
    async fn wait(&mut self, what: &str, pred: impl Fn(&Ws) -> bool) {
        let mut reads = 0;
        while !pred(self) {
            self.next().await;
            reads += 1;
            assert!(reads < 10_000, "{what}: never happened");
        }
    }

    async fn call(&mut self, make: impl FnOnce(RequestId) -> ClientMessage) -> Outcome {
        self.next_id += 1;
        let want = RequestId(self.next_id);
        let message = make(want);
        self.c
            .send(Message::text(encode_client(&message).unwrap()))
            .await
            .unwrap();
        loop {
            if let ServerMessage::Result { id, outcome } = self.next().await
                && id == want
            {
                if let Outcome::Ok(ok) = &outcome
                    && let Some(r) = ok.revision
                {
                    assert_eq!(r, self.revision, "the result came before its model_changed");
                }
                return outcome;
            }
        }
    }

    async fn command(&mut self, command: Command) -> Outcome {
        self.call(|id| ClientMessage::Command {
            id,
            command: Box::new(command),
        })
        .await
    }

    async fn transport(&mut self, action: TransportAction) {
        let o = self
            .call(|id| ClientMessage::Transport { id, action })
            .await;
        assert!(matches!(o, Outcome::Ok(_)), "{o:?}");
    }

    async fn undo(&mut self) {
        let o = self.call(|id| ClientMessage::Undo { id }).await;
        assert!(matches!(o, Outcome::Ok(_)), "{o:?}");
    }

    async fn redo(&mut self) {
        let o = self.call(|id| ClientMessage::Redo { id }).await;
        assert!(matches!(o, Outcome::Ok(_)), "{o:?}");
    }

    fn state(&self, n: u128) -> Option<WireCueState> {
        self.states.get(&id(n)).copied()
    }
}

fn rename(n: u128, name: &str) -> Command {
    Command::SetCueProperty {
        id: id(n),
        property: CueProperty::Name { value: name.into() },
    }
}

fn go() -> TransportAction {
    TransportAction::Go { list: list_id() }
}

/// Three cues: a 0.375 s clip, a 2 s clip, a memo. 8 kHz, so 3,000 and 16,000 frames.
fn session_workspace() -> Workspace {
    let a = wav(&dc(3000, 0.25));
    let b = wav(&dc(16000, 0.25));
    workspace(vec![
        audio(1, &a, 0),
        audio(2, &b, 1),
        cue(3, CueKind::Memo),
    ])
}

/// What a client folded must be what the engine says.
fn assert_consistent(ws: &Ws, engine: &Snapshot) {
    let engine_states: HashMap<CueId, WireCueState> =
        engine.cue_states.iter().map(|e| (e.cue, e.state)).collect();
    assert_eq!(ws.states, engine_states);
    assert_eq!(ws.playhead, engine.playheads.iter().find_map(|p| p.cue));
    assert_eq!(ws.revision, engine.revision);
    assert_eq!(ws.seq, engine.runtime_seq);
    assert_eq!(ws.document, engine.document);
}

#[tokio::test]
async fn a_full_show_session_over_the_websocket() {
    let ws = session_workspace();
    let show = Show::start(&ws, None);
    let (mut c, welcome) = Ws::connect(&show).await;
    assert_eq!(welcome.workspace, ws);
    assert!(welcome.cue_states.is_empty());
    assert_eq!(c.playhead, Some(id(1)));
    assert!(!welcome.document.can_undo);

    // Edits: each is one model_changed (checked by the client) before its result.
    let revision = c.revision;
    assert!(matches!(
        c.command(rename(3, "Interval")).await,
        Outcome::Ok(_)
    ));
    let extra = wav(&dc(800, 0.5));
    let add = Command::AddCue {
        at: Position {
            list: list_id(),
            parent: None,
            index: 3,
        },
        cue: audio(4, &extra, 0),
        auto_number: false,
    };
    assert!(matches!(c.command(add).await, Outcome::Ok(_)));
    assert_eq!(c.revision, revision + 2);
    assert!(
        c.changed
            .last()
            .unwrap()
            .0
            .iter()
            .any(|u| matches!(u, Upsert::Cue { cue, .. } if cue.id == id(4)))
    );
    // A refused edit changes nothing and is answered with an error.
    let refused = c.command(Command::RemoveCue { id: id(999) }).await;
    assert!(matches!(refused, Outcome::Error(_)), "{refused:?}");
    assert_eq!(c.revision, revision + 2);
    c.undo().await;
    assert!(
        c.changed
            .last()
            .unwrap()
            .1
            .iter()
            .any(|d| matches!(d, Delete::Cue { id: gone } if *gone == id(4)))
    );
    c.redo().await;
    c.wait("the document state", |c| {
        c.document.can_undo && !c.document.can_redo && c.document.dirty
    })
    .await;
    assert_eq!(c.revision, revision + 4);

    // GO starts cue 1 on the virtual clock; progress comes while it runs.
    show.advance(0);
    c.transport(go()).await;
    let out = show.advance(1000);
    assert!(out.peak > 0.2, "cue 1 should sound: {}", out.peak);
    c.wait("cue 1 running", |c| {
        c.state(1) == Some(WireCueState::Running)
    })
    .await;
    c.wait("progress for cue 1", |c| {
        c.progress.iter().any(|&(cue, elapsed, left)| {
            cue == id(1) && elapsed > 0.0 && left.is_some_and(|l| l > 0.0)
        })
    })
    .await;
    // It ends by itself; the playhead is on cue 2.
    show.advance(4000);
    c.wait("cue 1 idle", |c| c.state(1).is_none()).await;
    c.wait("the playhead on cue 2", |c| c.playhead == Some(id(2)))
        .await;
    assert!(show.advance(256).peak == 0.0, "cue 1 should be silent");

    // Cue 2: GO, pause everything, resume, panic.
    c.transport(go()).await;
    assert!(show.advance(2000).peak > 0.2);
    c.wait("cue 2 running", |c| {
        c.state(2) == Some(WireCueState::Running)
    })
    .await;
    c.transport(TransportAction::PauseAll).await;
    show.advance(256);
    c.wait("cue 2 paused", |c| c.state(2) == Some(WireCueState::Paused))
        .await;
    assert!(show.advance(1000).peak == 0.0, "a paused cue is silent");
    c.transport(TransportAction::ResumeAll).await;
    assert!(show.advance(1000).peak > 0.2);
    c.wait("cue 2 running again", |c| {
        c.state(2) == Some(WireCueState::Running)
    })
    .await;
    c.transport(TransportAction::Panic).await;
    show.advance(2000);
    c.wait("every cue idle", |c| c.states.is_empty()).await;
    assert!(show.advance(256).peak == 0.0, "panic leaves silence");

    // What the client folded is what the engine holds, and what a snapshot request returns.
    let engine = show.engine_snapshot().await;
    assert_consistent(&c, &engine);
    assert_eq!(engine.workspace.cue_lists[0].cues.len(), 4);
    assert_eq!(engine.workspace.cue_lists[0].cues[2].name, "Interval");
    let before = (c.states.clone(), c.playhead, c.revision, c.seq);
    let o = c
        .call(|id| ClientMessage::Request {
            id,
            request: Request::Snapshot,
        })
        .await;
    assert!(matches!(o, Outcome::Ok(_)));
    assert_eq!((c.states.clone(), c.playhead, c.revision, c.seq), before);

    // goodbye { shutdown } ends the control loop.
    c.c.send(Message::text(
        encode_client(&ClientMessage::Goodbye { shutdown: true }).unwrap(),
    ))
    .await
    .unwrap();
    show.finish();
}

// ---------------------------------------------------------------------------------------------
// OSC clients

enum Link {
    Udp {
        sock: UdpSocket,
        target: SocketAddr,
    },
    Tcp {
        stream: TcpStream,
        decoder: SlipDecoder,
        frames: std::collections::VecDeque<Vec<u8>>,
    },
}

struct Osc {
    link: Link,
    ws_id: String,
    /// Update messages seen per cue id, and the last playhead update.
    updates: HashMap<CueId, u64>,
    playhead: Option<String>,
}

fn text(s: &str) -> OscType {
    OscType::String(s.into())
}

impl Osc {
    fn new(link: Link, ws: &Workspace) -> Self {
        Self {
            link,
            ws_id: ws.id.to_string(),
            updates: HashMap::new(),
            playhead: None,
        }
    }

    fn udp(sock: UdpSocket, show: &Show, ws: &Workspace) -> Self {
        let port = show.server.osc_ports().unwrap().udp;
        let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        Self::new(Link::Udp { sock, target }, ws)
    }

    async fn tcp(show: &Show, ws: &Workspace) -> Self {
        let port = show.server.osc_ports().unwrap().tcp;
        let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        Self::new(
            Link::Tcp {
                stream,
                decoder: SlipDecoder::new(70_000),
                frames: std::collections::VecDeque::new(),
            },
            ws,
        )
    }

    async fn send(&mut self, addr: &str, args: Vec<OscType>) {
        let bytes = encode_message(&OscMessage {
            addr: addr.into(),
            args,
        })
        .unwrap();
        match &mut self.link {
            Link::Udp { sock, target } => {
                sock.send_to(&bytes, *target).await.unwrap();
            }
            Link::Tcp { stream, .. } => {
                let mut rest = slip_encode(&bytes);
                while !rest.is_empty() {
                    stream.writable().await.unwrap();
                    match stream.try_write(&rest) {
                        Ok(n) => {
                            rest.drain(..n);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(e) => panic!("{e}"),
                    }
                }
            }
        }
    }

    /// The next message from the server, folded into the update counters.
    async fn recv(&mut self) -> OscMessage {
        let m = guard("an OSC message", async {
            match &mut self.link {
                Link::Udp { sock, .. } => {
                    let mut buf = vec![0u8; 70_000];
                    let (n, _) = sock.recv_from(&mut buf).await.unwrap();
                    decode_packet(&buf[..n]).unwrap().remove(0)
                }
                Link::Tcp {
                    stream,
                    decoder,
                    frames,
                } => loop {
                    if let Some(frame) = frames.pop_front() {
                        return decode_packet(&frame).unwrap().remove(0);
                    }
                    stream.readable().await.unwrap();
                    let mut buf = [0u8; 4096];
                    match stream.try_read(&mut buf) {
                        Ok(0) => panic!("the OSC connection was closed"),
                        Ok(n) => {
                            let mut events = Vec::new();
                            decoder.feed(&buf[..n], &mut events);
                            for e in events {
                                match e {
                                    SlipEvent::Frame(f) => frames.push_back(f),
                                    other => panic!("{other:?}"),
                                }
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(e) => panic!("{e}"),
                    }
                },
            }
        })
        .await;
        let prefix = format!("/update/workspace/{}", self.ws_id);
        if let Some(rest) = m.addr.strip_prefix(&format!("{prefix}/cue_id/")) {
            let cue: uuid::Uuid = rest.parse().unwrap();
            *self.updates.entry(CueId(cue)).or_default() += 1;
        } else if m.addr.starts_with(&format!("{prefix}/cueList/"))
            && m.addr.ends_with("/playbackPosition")
        {
            let [OscType::String(s)] = m.args.as_slice() else {
                panic!("{m:?}")
            };
            self.playhead = Some(s.clone());
        }
        m
    }

    /// Sends a message and reads until its reply, returning the reply's JSON.
    async fn ask(&mut self, addr: &str, args: Vec<OscType>) -> Value {
        self.send(addr, args).await;
        let want = format!("/reply{addr}");
        for _ in 0..10_000 {
            let m = self.recv().await;
            if m.addr == want {
                let [OscType::String(s)] = m.args.as_slice() else {
                    panic!("{m:?}")
                };
                return serde_json::from_str(s).unwrap();
            }
        }
        panic!("no reply to {addr}");
    }

    /// A write, followed by a read as a barrier: the actor handles one peer's messages in
    /// order and forwards inputs before it replies, so when the reply is back the write has
    /// reached the control loop's inbox.
    async fn command(&mut self, addr: &str, args: Vec<OscType>) {
        self.send(addr, args).await;
        let v = self.ask("/cue/1/name", vec![]).await;
        assert_eq!(v["status"], "ok");
    }

    async fn name_of(&mut self, n: u128) -> String {
        let v = self.ask(&format!("/cue/{n}/name"), vec![]).await;
        assert_eq!(v["status"], "ok", "{v}");
        v["data"].as_str().unwrap().to_owned()
    }

    async fn wait(&mut self, what: &str, pred: impl Fn(&Osc) -> bool) {
        let mut reads = 0;
        while !pred(self) {
            self.recv().await;
            reads += 1;
            assert!(reads < 10_000, "{what}: never happened");
        }
    }

    async fn subscribe(&mut self) {
        let v = self.ask("/updates", vec![OscType::Int(1)]).await;
        assert_eq!(v["status"], "ok", "{v}");
    }

    fn updated(&self, n: u128) -> bool {
        self.updates.get(&id(n)).is_some_and(|&c| c >= 1)
    }

    fn playhead_on(&self, n: u128) -> bool {
        self.playhead.as_deref() == Some(&id(n).to_string())
    }
}

#[tokio::test]
async fn a_full_show_session_over_osc() {
    let ws = session_workspace();
    let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let show = Show::start(&ws, Some(sock.local_addr().unwrap().port()));
    let mut osc = Osc::udp(sock, &show, &ws);
    osc.subscribe().await;
    assert_eq!(osc.name_of(1).await, "cue 1");

    // An edit: the engine has it, a read returns it, and an update said so.
    osc.command("/cue/3/name", vec![text("Interval")]).await;
    osc.wait("an update for cue 3", |o| o.updated(3)).await;
    assert_eq!(osc.name_of(3).await, "Interval");
    let engine = show.engine_snapshot().await;
    assert_eq!(engine.workspace.cue_lists[0].cues[2].name, "Interval");
    // An edit the engine refuses (a negative pre-wait) is answered and changes nothing.
    let v = osc.ask("/cue/1/preWait", vec![OscType::Float(-1.0)]).await;
    assert_eq!(v["status"], "error");

    // GO: cue 1 plays and ends, the playhead moves on.
    show.advance(0);
    osc.command("/go", vec![]).await;
    let out = show.advance(1000);
    assert!(out.peak > 0.2);
    assert_eq!(
        show.engine_snapshot().await.cue_states[0].state,
        WireCueState::Running
    );
    osc.wait("the playhead on cue 2", |o| o.playhead_on(2))
        .await;
    show.advance(4000);
    assert!(show.advance(256).peak == 0.0);
    assert!(show.engine_snapshot().await.cue_states.is_empty());
    assert!(osc.updated(1));

    // Start by number, pause, resume, panic.
    osc.command("/cue/2/start", vec![]).await;
    assert!(show.advance(2000).peak > 0.2);
    osc.command("/pause", vec![]).await;
    show.advance(256);
    assert!(show.advance(1000).peak == 0.0, "paused");
    assert_eq!(
        show.engine_snapshot().await.cue_states[0].state,
        WireCueState::Paused
    );
    osc.command("/resume", vec![]).await;
    assert!(show.advance(1000).peak > 0.2);
    osc.command("/panic", vec![]).await;
    show.advance(2000);
    assert!(show.advance(256).peak == 0.0);
    assert!(show.engine_snapshot().await.cue_states.is_empty());
    osc.wait("an update for cue 2", |o| o.updated(2)).await;
    show.finish();
}

#[tokio::test]
async fn websocket_and_osc_clients_see_one_consistent_show() {
    let ws = session_workspace();
    let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let show = Show::start(&ws, Some(sock.local_addr().unwrap().port()));
    let (mut a, _) = Ws::connect(&show).await;
    let mut udp = Osc::udp(sock, &show, &ws);
    let mut tcp = Osc::tcp(&show, &ws).await;
    udp.subscribe().await;
    tcp.subscribe().await;

    // A WebSocket edit reaches both OSC peers.
    assert!(matches!(
        a.command(rename(3, "From the desk")).await,
        Outcome::Ok(_)
    ));
    for osc in [&mut udp, &mut tcp] {
        osc.wait("an update for cue 3", |o| o.updated(3)).await;
        assert_eq!(osc.name_of(3).await, "From the desk");
    }

    // An OSC edit reaches the WebSocket client as an ordinary model_changed.
    tcp.command("/cue/2/name", vec![text("From OSC")]).await;
    let revision = a.revision;
    a.wait("the OSC edit", |a| a.revision > revision).await;
    let (upserts, _) = a.changed.last().unwrap();
    assert!(
        upserts
            .iter()
            .any(|u| matches!(u, Upsert::Cue { cue, .. } if cue.name == "From OSC"))
    );

    // GO from OSC: the WebSocket client sees the cue run and the playhead move, and a client
    // that joins now is told the same in its welcome.
    show.advance(0);
    udp.command("/go", vec![]).await;
    show.advance(1000);
    a.wait("cue 1 running", |a| {
        a.state(1) == Some(WireCueState::Running)
    })
    .await;
    a.wait("the playhead on cue 2", |a| a.playhead == Some(id(2)))
        .await;
    let (mut b, welcome) = Ws::connect(&show).await;
    assert_eq!(b.states, a.states);
    assert_eq!(b.playhead, a.playhead);
    assert_eq!(welcome.workspace.cue_lists[0].cues[1].name, "From OSC");
    for osc in [&mut udp, &mut tcp] {
        osc.wait("the playhead", |o| o.playhead_on(2)).await;
    }

    // GO from a WebSocket client starts cue 2 (cue 1 may still be running: its clip is 3,000
    // frames); both clients agree it is running.
    a.transport(go()).await;
    show.advance(1000);
    for ws in [&mut a, &mut b] {
        ws.wait("cue 2 running", |c| {
            c.state(2) == Some(WireCueState::Running)
        })
        .await;
    }
    // Pause from OSC, resume from the second client, panic from the first.
    tcp.command("/pause", vec![]).await;
    show.advance(256);
    for ws in [&mut a, &mut b] {
        ws.wait("cue 2 paused", |c| c.state(2) == Some(WireCueState::Paused))
            .await;
    }
    assert!(show.advance(1000).peak == 0.0);
    b.transport(TransportAction::ResumeAll).await;
    assert!(show.advance(1000).peak > 0.2);
    a.transport(TransportAction::Panic).await;
    show.advance(2000);
    for ws in [&mut a, &mut b] {
        ws.wait("everything idle", |c| c.states.is_empty()).await;
    }
    assert!(show.advance(256).peak == 0.0);

    // Final state: equal everywhere.
    let engine = show.engine_snapshot().await;
    assert_consistent(&a, &engine);
    assert_consistent(&b, &engine);
    for osc in [&mut udp, &mut tcp] {
        for (n, cue) in engine.workspace.cue_lists[0].cues.iter().enumerate() {
            assert_eq!(osc.name_of(n as u128 + 1).await, cue.name);
        }
        assert!(osc.updated(2));
    }
    show.finish();
}
