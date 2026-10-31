// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! Whole show sessions over real sockets (QA-04): the WebSocket server and the OSC
//! listeners in front of the real `Control`, driven on a virtual clock.
//!
//! `runner::run` never renders in the offline backend, so sample time would stand still. These
//! tests run their own control thread instead: it handles inbox messages as they come and renders
//! blocks only when the test says so (`Show::advance`), so every start, fade, pause and end
//! happens at a sample position the test chose, and nothing depends on the wall clock. The
//! timeouts below are hang guards only.
//!
//! Coverage, against the other net tests: `net_ws` and `net_osc` use a stand-in control thread
//! (framing, limits, resync, refusals); `net_osc::go_over_the_wire...` runs the
//! real loop but without time passing. Here: a full session per protocol (edits, undo/redo, GO,
//! runtime and progress, pause, resume, panic, save as, save and an app connection opening and
//! closing another doc, shutdown), and both protocols and several clients
//! at once seeing the same show.

use crate::common::{ENGINE, Engine, text};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::show::{BLOCK, audio, config, cue, dc, id, list_id, mixer_config, wav, workspace};
use crossbeam_channel::{Receiver, Sender};
use futures_util::{SinkExt, StreamExt};
use openlab_audio::{Mixer, OfflineRenderer};
use openlab_core::protocol::{
    ClientMessage, Delete, DocId, ErrorCode, LogLevel, Outcome, PROTOCOL_VERSION, Request,
    RequestId, ResultData, RuntimeChange, ServerMessage, Snapshot, TransportAction, Upsert,
    WireBrokenReason, WireCueState, decode_server, encode_client,
};
use openlab_core::{Command, CueId, CueKind, CueProperty, Position, Workspace};
use openlab_engine::control::{
    Backend, Control, Flow, Inbox, OUTBOUND_CAPACITY, outbound_channel, reply_channel,
};
use openlab_engine::net::{OscConfig, ServerConfig, WsServer};
use openlab_engine::osc::{SlipDecoder, SlipEvent, decode_packet, encode_message, slip_encode};
use rosc::{OscMessage, OscType};
use serde_json::Value;
use tokio::net::{TcpStream, UdpSocket};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

const GUARD: Duration = Duration::from_secs(30);
const SETTLE: Duration = Duration::from_secs(30);
/// 20 ms waits that make up [`SETTLE`] when counted instead of timed.
const SETTLE_WAITS: u32 = 1_500;

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
            // A save is answered when a tick finds it written: tick while idle too. Ticks do not
            // move sample time (the offline backend renders only in `advance`).
            default(Duration::from_millis(5)) => control.tick(),
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
    let mut timeouts = 0;
    // Media answers first, so a start does not wait on a probe for want of a wake.
    while control.probes_pending() > 0 || control.preloads_pending() > 0 {
        match inbox.recv_timeout(Duration::from_millis(20)) {
            Ok(m) => {
                if control.handle(m) == Flow::Shutdown {
                    return (out, true);
                }
            }
            Err(_) => {
                // Only a wait that ran its 20 ms counts: SETTLE_WAITS of them are at least SETTLE.
                timeouts += 1;
                if timeouts > SETTLE_WAITS {
                    panic!("probes never answered");
                }
                control.tick();
            }
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
    /// The application folder (autosaves) and a place for show files.
    dir: tempfile::TempDir,
}

impl Show {
    fn start(ws: &Workspace, osc_reply_port: Option<u16>) -> Self {
        let (ctl, mixer_rt) = Mixer::new(mixer_config()).unwrap();
        let (inbox_tx, inbox) = crossbeam_channel::unbounded();
        let (adv, adv_rx) = crossbeam_channel::unbounded();
        let (out_tx, outbound) = outbound_channel(OUTBOUND_CAPACITY);
        let dir = tempfile::tempdir().unwrap();
        let mut c = config(ws);
        c.app_dirs = Some(openlab_engine::persist::AppDirs::new(
            dir.path().join("support"),
        ));
        let control = Control::new(
            ws.clone(),
            ctl,
            Backend::Offline,
            c,
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
                    // The sessions repeat the same read as a barrier.
                    duplicate_window: Duration::ZERO,
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
            dir,
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
    /// Every message read, in order.
    seen: Vec<ServerMessage>,
}

impl Ws {
    async fn connect(show: &Show) -> (Self, Snapshot) {
        Self::connect_to(show.server.port(), show.server.token(), DocId(0)).await
    }

    /// A connection bound to doc `doc` of the engine at `port`.
    async fn connect_to(port: u16, token: &str, doc: DocId) -> (Self, Snapshot) {
        let (mut c, _) = connect_async(format!("ws://127.0.0.1:{port}"))
            .await
            .unwrap();
        let hello = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            token: token.to_owned(),
            client_name: "session test".into(),
            workspace: Some(doc),
            pid: None,
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
                show_mode: false,
            },
            states: HashMap::new(),
            playhead: None,
            progress: Vec::new(),
            changed: Vec::new(),
            seen: Vec::new(),
        };
        let ServerMessage::Welcome {
            snapshot: Some(snapshot),
            ..
        } = me.raw().await
        else {
            panic!("the first message was not a welcome with a snapshot")
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
        let m = guard("a WebSocket message", async {
            loop {
                match self.c.next().await {
                    Some(Ok(Message::Text(t))) => return decode_server(t.as_str()).unwrap(),
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    other => panic!("the connection ended: {other:?}"),
                }
            }
        })
        .await;
        self.seen.push(m.clone());
        m
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
            ServerMessage::Progress { items, .. } => {
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
            coalesce: None,
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

/// An app connection (`hello` without a workspace).
struct App {
    c: WebSocketStream<MaybeTlsStream<TcpStream>>,
    /// Every message read, in order.
    seen: Vec<ServerMessage>,
}

impl App {
    async fn connect(show: &Show) -> Self {
        let (me, open) = Self::connect_to(show.server.port(), show.server.token()).await;
        assert_eq!(open, 1);
        me
    }

    /// An app connection to the engine at `port`, and how many workspaces its welcome lists.
    async fn connect_to(port: u16, token: &str) -> (Self, usize) {
        let (mut c, _) = connect_async(format!("ws://127.0.0.1:{port}"))
            .await
            .unwrap();
        let hello = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            token: token.to_owned(),
            client_name: "app".into(),
            workspace: None,
            pid: None,
        };
        c.send(Message::text(encode_client(&hello).unwrap()))
            .await
            .unwrap();
        let mut me = Self {
            c,
            seen: Vec::new(),
        };
        match me.next().await {
            ServerMessage::Welcome {
                workspaces: Some(list),
                ..
            } => {
                let open = list.len();
                (me, open)
            }
            other => panic!("{other:?}"),
        }
    }

    async fn next(&mut self) -> ServerMessage {
        let m = guard("an app connection message", async {
            loop {
                match self.c.next().await {
                    Some(Ok(Message::Text(t))) => return decode_server(t.as_str()).unwrap(),
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    other => panic!("the connection ended: {other:?}"),
                }
            }
        })
        .await;
        self.seen.push(m.clone());
        m
    }

    async fn call(&mut self, id: u64, message: ClientMessage) -> Outcome {
        self.c
            .send(Message::text(encode_client(&message).unwrap()))
            .await
            .unwrap();
        loop {
            if let ServerMessage::Result { id: rid, outcome } = self.next().await
                && rid == RequestId(id)
            {
                return outcome;
            }
        }
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

/// What a client folded must be what the engine says, once the client has read up to the
/// snapshot: runtime changes a control-loop tick makes (`media_duration`) are not held
/// behind a command's result, so the snapshot can be ahead of what the client has read.
async fn assert_consistent(ws: &mut Ws, engine: &Snapshot) {
    ws.wait("the engine's revision and runtime seq", |c| {
        c.revision >= engine.revision && c.seq >= engine.runtime_seq
    })
    .await;
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
    assert_consistent(&mut c, &engine).await;
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

    // Documents: save as, an edit, save; the bound client follows the file
    // state. An app connection then finds the show open already, opens another doc beside it
    // and closes that one; the session's doc is not touched.
    let path = show.dir.path().join("Session.openlab");
    let o = c
        .call(|id| ClientMessage::Request {
            id,
            request: Request::SaveAs {
                path: path.to_string_lossy().into_owned(),
            },
        })
        .await;
    assert!(matches!(o, Outcome::Ok(_)), "{o:?}");
    let saved = path.to_string_lossy().into_owned();
    c.wait("saved", |c| {
        c.document.path.as_deref() == Some(saved.as_str()) && !c.document.dirty
    })
    .await;
    assert!(matches!(
        c.command(rename(3, "Saved")).await,
        Outcome::Ok(_)
    ));
    c.wait("dirty", |c| c.document.dirty).await;
    let o = c
        .call(|id| ClientMessage::Request {
            id,
            request: Request::Save,
        })
        .await;
    assert!(matches!(o, Outcome::Ok(_)), "{o:?}");
    c.wait("clean", |c| !c.document.dirty).await;
    let on_disk = openlab_core::load_show(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(on_disk.workspace.cue_lists[0].cues[2].name, "Saved");

    let mut app = App::connect(&show).await;
    let open = Request::Open {
        path: saved.clone(),
        recovery: None,
    };
    match app
        .call(
            1,
            ClientMessage::Request {
                id: RequestId(1),
                request: open,
            },
        )
        .await
    {
        Outcome::Error(e) => {
            assert_eq!(e.code, openlab_core::protocol::ErrorCode::AlreadyOpen)
        }
        other => panic!("{other:?}"),
    }
    let new = ClientMessage::Request {
        id: RequestId(2),
        request: Request::New {},
    };
    let doc = match app.call(2, new).await {
        Outcome::Ok(openlab_core::protocol::OkBody {
            data: Some(openlab_core::protocol::ResultData::Opened { doc }),
            ..
        }) => doc,
        other => panic!("{other:?}"),
    };
    let close = ClientMessage::Close {
        id: RequestId(3),
        doc: Some(doc),
        discard_changes: false,
    };
    assert!(matches!(app.call(3, close).await, Outcome::Ok(_)));
    let engine = show.engine_snapshot().await;
    assert_consistent(&mut c, &engine).await;

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

impl Osc {
    fn new(link: Link, ws: &Workspace) -> Self {
        Self {
            link,
            ws_id: ws.id.to_string().to_uppercase(),
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
            // A cue, or the list or `__root__` that changes with it.
            if let Ok(cue) = rest.parse::<uuid::Uuid>() {
                *self.updates.entry(CueId(cue)).or_default() += 1;
            }
        } else if m.addr.starts_with(&format!("{prefix}/cueList/"))
            && m.addr.ends_with("/playbackPosition")
        {
            // No argument: the playhead is past the end.
            self.playhead = match m.args.as_slice() {
                [OscType::String(s)] => Some(s.to_lowercase()),
                [] => None,
                _ => panic!("{m:?}"),
            };
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
        // QLab sends no reply to `/updates`: a read afterwards is the barrier.
        self.send("/updates", vec![OscType::Int(1)]).await;
        let v = self.ask("/cue/1/name", vec![]).await;
        assert_eq!(v["status"], "ok", "{v}");
    }

    fn updated(&self, n: u128) -> bool {
        self.updates.get(&id(n)).is_some_and(|&c| c >= 1)
    }

    fn playhead_on(&self, n: u128) -> bool {
        self.playhead.as_deref() == Some(&id(n).to_string())
    }
}

/// Over the wire: undoing the add of a cue that is playing is performed (as QLab 5 does) and
/// stops the cue as a delete does.
#[tokio::test]
async fn an_undo_that_removes_a_playing_cue_stops_it_over_the_websocket() {
    let ws = session_workspace();
    let show = Show::start(&ws, None);
    let (mut c, _) = Ws::connect(&show).await;
    let long = wav(&dc(16000, 0.5));
    let add = Command::AddCue {
        at: Position {
            list: list_id(),
            parent: None,
            index: 3,
        },
        cue: audio(4, &long, 0),
        auto_number: false,
    };
    assert!(matches!(c.command(add).await, Outcome::Ok(_)));
    show.advance(0);
    c.transport(TransportAction::Start { cue: id(4) }).await;
    assert!(show.advance(1000).peak > 0.2);
    c.wait("cue 4 running", |c| {
        c.state(4) == Some(WireCueState::Running)
    })
    .await;
    let revision = c.revision;

    c.undo().await;
    assert_eq!(c.revision, revision + 1);
    show.advance(1000);
    c.wait("cue 4 idle", |c| c.state(4).is_none()).await;
    assert!(show.advance(1000).peak == 0.0, "cue 4 stopped");
    let engine = show.engine_snapshot().await;
    assert!(engine.workspace.find_cue(id(4)).is_none());
    assert_consistent(&mut c, &engine).await;
    show.finish();
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
    assert_consistent(&mut a, &engine).await;
    assert_consistent(&mut b, &engine).await;
    for osc in [&mut udp, &mut tcp] {
        for (n, cue) in engine.workspace.cue_lists[0].cues.iter().enumerate() {
            assert_eq!(osc.name_of(n as u128 + 1).await, cue.name);
        }
        assert!(osc.updated(2));
    }
    show.finish();
}

// ---------------------------------------------------------------------------------------------
// The engine binary on its virtual device (QA-01): no audio hardware, the real process and socket

/// A virtual-device engine (`openlab-engine --virtual-device <mode>`) with a fresh
/// application-support folder, once it listens: the process, the folder, its port and token.
fn spawn_virtual(mode: &str) -> (Engine, tempfile::TempDir, u16, String) {
    let support = tempfile::tempdir().unwrap();
    let mut engine = Engine::spawn(
        std::process::Command::new(ENGINE)
            .args(["--virtual-device", mode, "--watch-stdin"])
            .args(["--app-support".as_ref(), support.path().as_os_str()]),
    );
    assert!(engine.wait_for("listening on"), "{:?}", engine.seen);
    let file = openlab_engine::persist::AppDirs::new(support.path()).engine_file_path();
    let info = openlab_engine::host::engine_file::read(&file).expect("engine.json");
    (engine, support, info.port, info.token)
}

/// Ends a spawned engine as its parent going away does (stdin closes); it must exit cleanly.
fn end(mut engine: Engine) {
    drop(engine.child.stdin.take());
    let status = engine.exit_status();
    assert!(status.success(), "{status}: {:?}", engine.seen);
}

fn request(id: RequestId, request: Request) -> ClientMessage {
    ClientMessage::Request { id, request }
}

fn advance_by(frames: u64) -> impl FnOnce(RequestId) -> ClientMessage {
    move |id| request(id, Request::Advance { frames })
}

/// The virtual device's rate.
const VRATE: u64 = 48_000;

/// Three audio cues on 2 s files, saved to `dir`.
fn three_cue_show(dir: &std::path::Path) -> std::path::PathBuf {
    let files: Vec<_> = (0..3).map(|_| wav(&dc(16_000, 0.25))).collect();
    let ws = workspace(vec![
        audio(1, &files[0], 0),
        audio(2, &files[1], 1),
        audio(3, &files[2], 0),
    ]);
    let path = dir.join("Three cues.openlab");
    std::fs::write(&path, openlab_core::save_show(&ws).unwrap()).unwrap();
    path
}

/// Opens `show` on the app connection; the doc it opened as.
async fn open_show(app: &mut App, id: u64, show: &std::path::Path) -> DocId {
    let opened = app
        .call(
            id,
            request(
                RequestId(id),
                Request::Open {
                    path: show.to_string_lossy().into_owned(),
                    recovery: None,
                },
            ),
        )
        .await;
    match opened {
        Outcome::Ok(ok) => match ok.data {
            Some(ResultData::Opened { doc }) => doc,
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
}

/// The progress `elapsed` of cue `n`, as last reported.
fn elapsed(ws: &Ws, n: u128) -> f64 {
    ws.progress
        .iter()
        .find(|p| p.0 == id(n))
        .map(|p| p.1)
        .unwrap_or_else(|| panic!("no progress for cue {n}: {:?}", ws.progress))
}

async fn ok_call(ws: &mut Ws, make: impl FnOnce(RequestId) -> ClientMessage) {
    let o = ws.call(make).await;
    assert!(matches!(o, Outcome::Ok(_)), "{o:?}");
}

/// What a session told its two connections: results, and every other message, apart (the two
/// kinds may pass each other on a connection, docs/network.md "Ordering and gaps").
#[derive(Debug, PartialEq)]
struct Told {
    app_results: Vec<ServerMessage>,
    results: Vec<ServerMessage>,
    broadcasts: Vec<ServerMessage>,
}

fn split(seen: &[ServerMessage]) -> (Vec<ServerMessage>, Vec<ServerMessage>) {
    seen.iter()
        .cloned()
        .partition(|m| matches!(m, ServerMessage::Result { .. }))
}

/// One scripted session against a stepped engine: open the show, GO, let time pass, panic.
async fn stepped_session(show: &std::path::Path) -> Told {
    let (engine, _support, port, token) = spawn_virtual("stepped");
    let (mut app, open) = App::connect_to(port, &token).await;
    assert_eq!(open, 0, "the engine starts with no workspace open");
    let doc = open_show(&mut app, 1, show).await;
    // The media probes answer before device time moves: the doc is fully known after this.
    let settled = app.call(2, advance_by(0)(RequestId(2))).await;
    assert!(matches!(settled, Outcome::Ok(_)), "{settled:?}");

    let (mut ws, snapshot) = Ws::connect_to(port, &token, doc).await;
    assert!(snapshot.cue_states.is_empty(), "{:?}", snapshot.cue_states);
    assert_eq!(ws.playhead, Some(id(1)));
    // Ids from 100 on this connection, so the two connections' ids never meet.
    ws.next_id = 100;

    ws.transport(go()).await;
    // A tenth of a second: cue 1 runs, the playhead is on cue 2.
    ok_call(&mut ws, advance_by(VRATE / 10)).await;
    assert_eq!(ws.state(1), Some(WireCueState::Running));
    assert_eq!(ws.playhead, Some(id(2)));
    let first = elapsed(&ws, 1);
    assert!(first > 0.0 && first <= 0.1, "{first}");
    ok_call(&mut ws, advance_by(VRATE / 10)).await;
    let second = elapsed(&ws, 1);
    // Progress is sent at most 30 times a second of device time.
    assert!(
        second > first && second <= first + 0.1 + 1.0 / 30.0,
        "{first} then {second}"
    );
    assert_eq!(ws.state(1), Some(WireCueState::Running));

    ws.transport(TransportAction::Panic).await;
    // Past the show's 0.1 s panic fade.
    ok_call(&mut ws, advance_by(VRATE / 5)).await;
    assert_eq!(ws.state(1), None, "cue 1 is idle after the panic");
    assert_eq!(ws.playhead, Some(id(2)), "the panic left the playhead");
    // A barrier: a command's result follows its model_changed, so every broadcast before it
    // has been read by now.
    let o = ws.command(rename(3, "barrier")).await;
    assert!(matches!(o, Outcome::Ok(_)), "{o:?}");
    let o = app.call(3, advance_by(0)(RequestId(3))).await;
    assert!(matches!(o, Outcome::Ok(_)), "{o:?}");
    // The debug engine counts allocations on its audio thread and logs them as errors.
    for m in ws.seen.iter().chain(&app.seen) {
        if let ServerMessage::LogEntry { entry, .. } = m {
            assert_ne!(entry.level, LogLevel::Error, "{entry:?}");
        }
    }
    end(engine);
    let (results, mut broadcasts) = split(&ws.seen);
    // Up to the barrier's model_changed: what the edit sends after it (`document`) may come
    // before or after its result.
    let barrier = broadcasts
        .iter()
        .position(|m| matches!(m, ServerMessage::ModelChanged { .. }))
        .expect("the barrier's model_changed");
    broadcasts.truncate(barrier + 1);
    Told {
        app_results: split(&app.seen).0,
        results,
        broadcasts,
    }
}

/// QA-01 (#132): the real engine binary on its stepped virtual device, over its real socket.
/// A show of three audio cues opens; GO starts cue 1 and moves the playhead to cue 2; progress
/// counts device time, which moves only on `advance`; a panic followed by time past its fade
/// leaves cue 1 idle and the playhead where it was. The same script twice tells the
/// connections exactly the same things.
#[tokio::test]
async fn the_engine_on_its_stepped_virtual_device_plays_a_show_the_same_every_time() {
    let dir = tempfile::tempdir().unwrap();
    let show = three_cue_show(dir.path());
    let once = stepped_session(&show).await;
    let again = stepped_session(&show).await;
    assert!(once.broadcasts.len() > 5, "{:?}", once.broadcasts);
    assert_eq!(once, again);
}

fn device_names(o: Outcome) -> Vec<(String, u32)> {
    match o {
        Outcome::Ok(ok) => match ok.data {
            Some(ResultData::Devices { devices, .. }) => devices
                .into_iter()
                .map(|d| (d.name, d.output_channels))
                .collect(),
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
}

/// The device faults a test can ask a virtual engine for: `lose_device` takes the device away
/// (the running cue breaks with `device_lost`, the device list empties), `restore_device`
/// brings it back and the engine reopens it once a list read sees it.
#[tokio::test]
async fn a_stepped_engine_loses_and_gets_back_its_virtual_device_on_request() {
    let dir = tempfile::tempdir().unwrap();
    let show = three_cue_show(dir.path());
    let (engine, _support, port, token) = spawn_virtual("stepped");
    let (mut app, _) = App::connect_to(port, &token).await;
    let doc = open_show(&mut app, 1, &show).await;
    let (mut ws, _) = Ws::connect_to(port, &token, doc).await;
    ws.next_id = 100;
    let virtual_output = [("openLab Virtual Output".to_owned(), 2)];
    let listed = device_names(ws.call(|id| request(id, Request::ListDevices)).await);
    assert_eq!(listed, virtual_output);

    ws.transport(go()).await;
    ok_call(&mut ws, advance_by(VRATE / 10)).await;
    assert_eq!(ws.state(1), Some(WireCueState::Running));

    ok_call(&mut ws, |id| request(id, Request::LoseDevice)).await;
    // The loss is handled before the answer: the cue that played on the device broke.
    assert_eq!(ws.state(1), Some(WireCueState::Broken));
    assert!(ws.seen.iter().any(|m| matches!(
        m,
        ServerMessage::Runtime { changes, .. } if changes.iter().any(|c| matches!(
            c,
            RuntimeChange::CueState {
                state: WireCueState::Broken,
                reason: Some(WireBrokenReason::DeviceLost),
                ..
            }
        ))
    )));
    // Time passes with the device gone: the list read finds it missing.
    ok_call(&mut ws, advance_by(VRATE)).await;
    assert!(device_names(ws.call(|id| request(id, Request::ListDevices)).await).is_empty());

    ok_call(&mut ws, |id| request(id, Request::RestoreDevice)).await;
    // The next list read sees it back and the stream is reopened at once.
    ok_call(&mut ws, advance_by(VRATE * 2)).await;
    let listed = device_names(ws.call(|id| request(id, Request::ListDevices)).await);
    assert_eq!(listed, virtual_output);
    // Playing again: GO starts the next cue and it runs.
    ws.transport(go()).await;
    ok_call(&mut ws, advance_by(VRATE / 10)).await;
    assert_eq!(ws.state(2), Some(WireCueState::Running));
    end(engine);
}

/// A realtime virtual engine plays on its own: `advance` is refused.
#[tokio::test]
async fn a_realtime_virtual_engine_refuses_advance() {
    let (engine, _support, port, token) = spawn_virtual("realtime");
    let (mut app, _) = App::connect_to(port, &token).await;
    match app.call(1, advance_by(1)(RequestId(1))).await {
        Outcome::Error(e) => assert_eq!(e.code, ErrorCode::Unsupported, "{e:?}"),
        other => panic!("{other:?}"),
    }
    end(engine);
}
