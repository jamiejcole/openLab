// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The WebSocket server over real sockets on 127.0.0.1, against a stand-in control thread
//! (docs/network.md, docs/protocol.md section 2). Timeouts here are hang guards only; the
//! server's own timers are shortened through `ServerConfig`.

mod show;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use futures_util::{SinkExt, StreamExt};
use openlab_core::protocol::DocId;
use openlab_core::protocol::{
    ClientMessage, Delete, DocumentState, ErrorCode, LogLevel, OkBody, Outcome, PROTOCOL_VERSION,
    Request, RequestId, ServerMessage, Snapshot, TransportAction, Upsert, decode_server,
    encode_client,
};
use openlab_core::{Command, CueKind, CueProperty, GroupMode};
use openlab_engine::control::{
    Inbox, OUTBOUND_CAPACITY, OutboundTx, Outgoing, ReplyTx, outbound_channel,
};
use openlab_engine::net::{ServerConfig, WsServer};
use show::{cue, id, list_id, workspace};
use tokio::net::TcpStream;
use tokio::sync::mpsc::error::TrySendError;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Hang guard for one wait on the network. It bounds the whole wait: the server's pings must
/// not keep a wait for a message that never comes alive (that hung CI on Windows).
const GUARD: Duration = Duration::from_secs(20);

/// Runs `f` within [`GUARD`], or fails the test naming `what`.
async fn guard<F: std::future::Future>(what: &str, f: F) -> F::Output {
    match tokio::time::timeout(GUARD, f).await {
        Ok(out) => out,
        Err(_) => panic!("hang: {what} took longer than {GUARD:?}"),
    }
}

/// The doc the stand-in control thread plays and the test clients bind to.
const DOC: DocId = DocId(0);

/// The control loop's outbound channel, as the stand-in uses it: every message is about [`DOC`].
#[derive(Clone)]
struct Out(OutboundTx);

impl Out {
    fn try_send(&self, m: ServerMessage) -> Result<(), TrySendError<ServerMessage>> {
        self.0.try_send(Outgoing::doc(DOC, m)).map_err(|e| match e {
            TrySendError::Full(o) => TrySendError::Full(o.message),
            TrySendError::Closed(o) => TrySendError::Closed(o.message),
        })
    }

    fn blocking_send(&self, m: ServerMessage) -> Result<(), ()> {
        self.0.blocking_send(Outgoing::doc(DOC, m)).map_err(|_| ())
    }
}

#[derive(Default)]
struct Control {
    revision: AtomicU64,
    seen: Mutex<Vec<ClientMessage>>,
    /// `Inbox::Log` messages from the server.
    logs: Mutex<Vec<String>>,
    /// `Inbox::Trigger` (source, detail) pairs, in arrival order.
    triggers: Mutex<Vec<(String, String)>>,
    /// While set, the client's own snapshot requests are answered (at the revision of the
    /// moment they arrive) only when the test calls [`Control::release_replies`].
    hold_snapshots: AtomicBool,
    held_replies: Mutex<Vec<(ReplyTx, Vec<ServerMessage>)>>,
    shutdown: AtomicBool,
}

struct Fix {
    server: WsServer,
    out: Out,
    inbox: Sender<Inbox>,
    control: Arc<Control>,
    thread: Option<thread::JoinHandle<()>>,
}

fn snapshot(revision: u64) -> Snapshot {
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
        workspace: workspace(vec![]),
        cue_states: vec![],
        problems: vec![],
        group_warnings: vec![],
        playheads: vec![],
        previewing: vec![],
        warnings: vec![],
    }
}

fn ok(id: RequestId, revision: Option<u64>) -> ServerMessage {
    ServerMessage::Result {
        id,
        outcome: Outcome::Ok(OkBody {
            revision,
            data: None,
        }),
    }
}

/// Answers like the real control loop: a command bumps the revision, broadcasts
/// `model_changed`, then replies.
/// Replies go to a connection that may already be gone: not an error here.
fn control_thread(rx: &Receiver<Inbox>, out: &Out, c: &Control) {
    while let Ok(msg) = rx.recv() {
        let (message, reply) = match msg {
            Inbox::Client { message, reply } | Inbox::Conn { message, reply, .. } => {
                (message, reply)
            }
            Inbox::Log { message, .. } => {
                c.logs.lock().unwrap().push(message);
                continue;
            }
            Inbox::Trigger { source, detail } => {
                c.triggers.lock().unwrap().push((source, detail));
                continue;
            }
            _ => break,
        };
        c.seen.lock().unwrap().push(message.clone());
        match message {
            ClientMessage::Command { id, .. } => {
                let revision = c.revision.fetch_add(1, Ordering::SeqCst) + 1;
                out.blocking_send(ServerMessage::ModelChanged {
                    revision,
                    upserts: vec![],
                    deletes: vec![],
                })
                .unwrap();
                let _ = reply.send(ok(id, Some(revision)));
            }
            ClientMessage::Request {
                id,
                request: Request::Snapshot,
            } => {
                let revision = c.revision.load(Ordering::SeqCst);
                let answer = vec![
                    ServerMessage::Snapshot {
                        snapshot: Box::new(snapshot(revision)),
                    },
                    ok(id, None),
                ];
                if id != RequestId(u64::MAX) && c.hold_snapshots.load(Ordering::SeqCst) {
                    c.held_replies.lock().unwrap().push((reply, answer));
                } else {
                    for m in answer {
                        let _ = reply.send(m);
                    }
                }
            }
            // A snapshot older than what was broadcast (as a reordered reply would be).
            ClientMessage::Request {
                id,
                request: Request::Relink { .. },
            } => {
                let _ = reply.send(ServerMessage::Snapshot {
                    snapshot: Box::new(snapshot(0)),
                });
                let _ = reply.send(ok(id, None));
            }
            // A revision bump whose `model_changed` is "lost".
            ClientMessage::Undo { id } => {
                let revision = c.revision.fetch_add(1, Ordering::SeqCst) + 1;
                let _ = reply.send(ok(id, Some(revision)));
            }
            ClientMessage::Transport { id, .. }
            | ClientMessage::Redo { id }
            | ClientMessage::Request { id, .. } => {
                let _ = reply.send(ok(id, None));
            }
            ClientMessage::Goodbye { shutdown } => {
                if shutdown {
                    c.shutdown.store(true, Ordering::SeqCst);
                }
            }
            ClientMessage::Close { id, .. }
            | ClientMessage::Panic { id }
            | ClientMessage::Hotkey { id, .. } => {
                let _ = reply.send(ok(id, None));
            }
            ClientMessage::Hello { .. } => {}
        }
    }
}

impl Control {
    fn release_replies(&self) {
        for (reply, answer) in self.held_replies.lock().unwrap().drain(..) {
            for m in answer {
                let _ = reply.send(m);
            }
        }
    }
}

fn fixture_with(config: ServerConfig) -> Fix {
    let (inbox, rx) = crossbeam_channel::unbounded();
    let (out, outbound) = outbound_channel(OUTBOUND_CAPACITY);
    let out = Out(out);
    let control = Arc::new(Control::default());
    let server = WsServer::start(inbox.clone(), outbound, config).unwrap();
    let thread = {
        let (out, control) = (out.clone(), control.clone());
        thread::spawn(move || control_thread(&rx, &out, &control))
    };
    Fix {
        server,
        out,
        inbox,
        control,
        thread: Some(thread),
    }
}

fn fixture() -> Fix {
    fixture_with(ServerConfig::default())
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

impl Fix {
    async fn raw(&self) -> Client {
        let url = format!("ws://127.0.0.1:{}", self.server.port());
        tokio::time::timeout(GUARD, connect_async(url))
            .await
            .unwrap()
            .unwrap()
            .0
    }

    /// A client that has said hello and been welcomed.
    async fn client(&self) -> Client {
        let mut c = self.raw().await;
        hello(&mut c, self.server.token(), PROTOCOL_VERSION).await;
        match next(&mut c).await {
            Some(ServerMessage::Welcome { .. }) => c,
            other => panic!("no welcome: {other:?}"),
        }
    }
}

fn hello_text(token: &str, version: u32, name: &str) -> String {
    encode_client(&ClientMessage::Hello {
        protocol_version: version,
        token: token.into(),
        client_name: name.into(),
        workspace: Some(DOC),
    })
    .unwrap()
}

async fn hello(c: &mut Client, token: &str, version: u32) {
    send_text(c, hello_text(token, version, "test")).await;
}

async fn send_text(c: &mut Client, text: String) {
    // A refused client may be closed by the time it writes: not an error for the test.
    let _ = guard("sending a frame", c.send(Message::text(text))).await;
}

async fn send(c: &mut Client, m: &ClientMessage) {
    send_text(c, encode_client(m).unwrap()).await;
}

/// The next engine message; `None` when the connection is closed. Pings do not extend the
/// guard.
async fn next(c: &mut Client) -> Option<ServerMessage> {
    guard("waiting for the next message", async {
        loop {
            match c.next().await {
                Some(Ok(Message::Text(t))) => return Some(decode_server(t.as_str()).unwrap()),
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return None,
                Some(Ok(other)) => panic!("unexpected frame {other:?}"),
            }
        }
    })
    .await
}

async fn expect_closed(c: &mut Client) {
    guard("waiting for the server to close", async {
        while next(c).await.is_some() {}
    })
    .await;
}

async fn connect_tcp(port: u16) -> TcpStream {
    guard("connecting", TcpStream::connect(("127.0.0.1", port)))
        .await
        .unwrap()
}

fn rename(n: u128) -> ClientMessage {
    ClientMessage::Command {
        id: RequestId(n as u64),
        command: Box::new(Command::SetCueProperty {
            id: id(1),
            property: CueProperty::Name { value: "x".into() },
        }),
        coalesce: None,
    }
}

fn go(n: u64) -> ClientMessage {
    ClientMessage::Transport {
        id: RequestId(n),
        action: TransportAction::Go { list: list_id() },
    }
}

fn error_code(m: Option<ServerMessage>) -> ErrorCode {
    match m {
        Some(ServerMessage::Error {
            code,
            protocol_version,
            ..
        }) => {
            assert_eq!(protocol_version, PROTOCOL_VERSION);
            code
        }
        other => panic!("not an error: {other:?}"),
    }
}

async fn wait_result(c: &mut Client, id: u64) -> Vec<ServerMessage> {
    guard("waiting for a result", async {
        let mut seen = vec![];
        loop {
            let m = next(c).await.expect("closed before the result");
            let done = matches!(&m, ServerMessage::Result { id: i, .. } if i.0 == id);
            seen.push(m);
            if done {
                return seen;
            }
        }
    })
    .await
}

// --- authentication -----------------------------------------------------------------------

#[tokio::test]
async fn a_valid_hello_is_welcomed_with_a_snapshot() {
    let f = fixture();
    f.control.revision.store(7, Ordering::SeqCst);
    let mut c = f.raw().await;
    hello(&mut c, f.server.token(), PROTOCOL_VERSION).await;
    match next(&mut c).await {
        Some(ServerMessage::Welcome {
            protocol_version,
            doc,
            snapshot: Some(snapshot),
            ..
        }) => {
            assert_eq!(protocol_version, PROTOCOL_VERSION);
            assert_eq!(doc, Some(DOC));
            assert_eq!(snapshot.revision, 7);
        }
        other => panic!("{other:?}"),
    }
    // The server's own snapshot request must not leak a result to the client.
    send(&mut c, &go(1)).await;
    let seen = wait_result(&mut c, 1).await;
    assert_eq!(seen.len(), 1);
}

#[tokio::test]
async fn a_transport_is_announced_to_control_as_a_ui_trigger_and_a_request_is_not() {
    let f = fixture();
    let mut c = f.raw().await;
    hello(&mut c, f.server.token(), PROTOCOL_VERSION).await;
    assert!(matches!(
        next(&mut c).await,
        Some(ServerMessage::Welcome { .. })
    ));
    send(&mut c, &go(1)).await;
    wait_result(&mut c, 1).await;
    send(
        &mut c,
        &ClientMessage::Request {
            id: RequestId(2),
            request: Request::Snapshot,
        },
    )
    .await;
    wait_result(&mut c, 2).await;
    assert_eq!(
        *f.control.triggers.lock().unwrap(),
        vec![("ui".to_owned(), "transport go".to_owned())]
    );
}

#[tokio::test]
async fn the_token_is_random_per_server_and_never_in_debug_output() {
    let a = fixture();
    let b = fixture();
    assert_ne!(a.server.token(), b.server.token());
    assert!(!format!("{:?}", a.server).contains(a.server.token()));
    assert_ne!(a.server.port(), 0);
}

#[tokio::test]
async fn a_wrong_token_is_refused_and_closed() {
    let f = fixture();
    let mut c = f.raw().await;
    hello(&mut c, "nope", PROTOCOL_VERSION).await;
    assert_eq!(error_code(next(&mut c).await), ErrorCode::BadToken);
    assert!(next(&mut c).await.is_none());
    // A token that is a prefix of the real one is wrong too.
    let mut c = f.raw().await;
    let prefix = &f.server.token()[..10];
    hello(&mut c, prefix, PROTOCOL_VERSION).await;
    assert_eq!(error_code(next(&mut c).await), ErrorCode::BadToken);
    assert!(f.control.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn another_protocol_version_is_refused_with_the_engines_version() {
    let f = fixture();
    let mut c = f.raw().await;
    hello(&mut c, f.server.token(), PROTOCOL_VERSION + 1).await;
    assert_eq!(
        error_code(next(&mut c).await),
        ErrorCode::ProtocolVersionMismatch
    );
    assert!(next(&mut c).await.is_none());
}

#[tokio::test]
async fn any_other_message_first_is_a_bad_token() {
    let f = fixture();
    let mut c = f.raw().await;
    send(&mut c, &go(1)).await;
    assert_eq!(error_code(next(&mut c).await), ErrorCode::BadToken);
    assert!(next(&mut c).await.is_none());
    assert!(f.control.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_unreadable_first_frame_is_a_bad_request() {
    let f = fixture();
    let mut c = f.raw().await;
    send_text(&mut c, "{not json".into()).await;
    assert_eq!(error_code(next(&mut c).await), ErrorCode::BadRequest);
    assert!(next(&mut c).await.is_none());
}

#[tokio::test]
async fn a_hello_over_4_kib_is_refused_even_with_the_right_token() {
    let f = fixture();
    let mut c = f.raw().await;
    let text = hello_text(f.server.token(), PROTOCOL_VERSION, &"x".repeat(5000));
    send_text(&mut c, text).await;
    assert_eq!(error_code(next(&mut c).await), ErrorCode::BadRequest);
    assert!(next(&mut c).await.is_none());
}

#[tokio::test]
async fn a_huge_first_frame_is_cut_off_before_it_is_buffered() {
    let f = fixture();
    let mut c = f.raw().await;
    send_text(&mut c, "x".repeat(900 * 1024)).await;
    // Closed without a welcome (an error message may or may not get through first).
    while let Some(m) = next(&mut c).await {
        assert!(!matches!(m, ServerMessage::Welcome { .. }));
    }
    assert!(f.control.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn silence_before_hello_times_out() {
    let f = fixture_with(ServerConfig {
        hello_timeout: Duration::from_millis(150),
        ..ServerConfig::default()
    });
    let mut c = f.raw().await;
    expect_closed(&mut c).await;
    // A raw TCP connection that never even upgrades is dropped too.
    let mut tcp = connect_tcp(f.server.port()).await;
    let mut buf = [0u8; 16];
    let n = tokio::time::timeout(GUARD, tokio::io::AsyncReadExt::read(&mut tcp, &mut buf))
        .await
        .expect("hang");
    assert!(matches!(n, Ok(0) | Err(_)));
}

#[tokio::test]
async fn too_many_unauthenticated_connections_are_dropped_not_queued() {
    let f = fixture_with(ServerConfig {
        max_handshaking: 2,
        ..ServerConfig::default()
    });
    // Two silent raw connections use up the handshake slots.
    let _a = connect_tcp(f.server.port()).await;
    let _b = connect_tcp(f.server.port()).await;
    // The third is accepted by the OS but closed by the server without an upgrade; wait for
    // the slots to be taken by retrying until one is refused.
    let mut refused = false;
    for _ in 0..50 {
        let mut c = connect_tcp(f.server.port()).await;
        let mut buf = [0u8; 1];
        if let Ok(Ok(0) | Err(_)) = tokio::time::timeout(
            Duration::from_millis(200),
            tokio::io::AsyncReadExt::read(&mut c, &mut buf),
        )
        .await
        {
            refused = true;
            break;
        }
    }
    assert!(refused);
}

// --- relaying -------------------------------------------------------------------------------

#[tokio::test]
async fn model_changed_comes_before_the_result_of_its_command() {
    let f = fixture();
    let mut c = f.client().await;
    for n in 1..=200u64 {
        send(&mut c, &rename(u128::from(n))).await;
        let seen = wait_result(&mut c, n).await;
        let changed = seen
            .iter()
            .position(
                |m| matches!(m, ServerMessage::ModelChanged { revision, .. } if *revision == n),
            )
            .unwrap_or_else(|| panic!("no model_changed {n} before its result: {seen:?}"));
        assert_eq!(changed, seen.len() - 2, "{seen:?}");
        assert!(matches!(
            seen.last(),
            Some(ServerMessage::Result { outcome: Outcome::Ok(OkBody { revision: Some(r), .. }), .. }) if *r == n
        ));
    }
}

#[tokio::test]
async fn a_result_goes_out_even_if_its_model_changed_was_lost() {
    let f = fixture_with(ServerConfig {
        revision_wait: Duration::from_millis(100),
        ..ServerConfig::default()
    });
    let mut c = f.client().await;
    // The stand-in answers `undo` with a new revision but no `model_changed` (as if the control
    // loop's outbound channel had been full): the result must not wait for ever.
    send(&mut c, &ClientMessage::Undo { id: RequestId(5) }).await;
    let seen = wait_result(&mut c, 5).await;
    assert_eq!(seen.len(), 1);
    assert!(matches!(
        &seen[0],
        ServerMessage::Result {
            outcome: Outcome::Ok(OkBody {
                revision: Some(1),
                ..
            }),
            ..
        }
    ));
}

#[tokio::test]
async fn every_client_gets_the_broadcasts() {
    let f = fixture();
    let mut a = f.client().await;
    let mut b = f.client().await;
    f.out
        .try_send(ServerMessage::Runtime {
            seq: 1,
            changes: vec![],
        })
        .unwrap();
    f.out
        .try_send(ServerMessage::Log {
            level: LogLevel::Info,
            message: "hi".into(),
        })
        .unwrap();
    for c in [&mut a, &mut b] {
        assert!(matches!(
            next(c).await,
            Some(ServerMessage::Runtime { seq: 1, .. })
        ));
        assert!(matches!(next(c).await, Some(ServerMessage::Log { .. })));
    }
    // Replies go only to the asking client.
    send(&mut a, &go(3)).await;
    wait_result(&mut a, 3).await;
    f.out
        .try_send(ServerMessage::Runtime {
            seq: 2,
            changes: vec![],
        })
        .unwrap();
    assert!(matches!(
        next(&mut b).await,
        Some(ServerMessage::Runtime { seq: 2, .. })
    ));
}

#[tokio::test]
async fn broadcasts_the_welcome_snapshot_already_contains_are_not_repeated() {
    let f = fixture();
    f.control.revision.store(5, Ordering::SeqCst);
    let mut c = f.client().await;
    for revision in [4, 5, 6] {
        f.out
            .try_send(ServerMessage::ModelChanged {
                revision,
                upserts: vec![],
                deletes: vec![],
            })
            .unwrap();
    }
    assert!(matches!(
        next(&mut c).await,
        Some(ServerMessage::ModelChanged { revision: 6, .. })
    ));
}

#[tokio::test]
async fn a_gap_is_recovered_with_a_snapshot_request() {
    let f = fixture();
    let mut c = f.client().await;
    f.out
        .try_send(ServerMessage::ModelChanged {
            revision: 1,
            upserts: vec![],
            deletes: vec![],
        })
        .unwrap();
    // Revision 2 is lost on the way (the control loop's outbound channel was full).
    f.control.revision.store(3, Ordering::SeqCst);
    f.out
        .try_send(ServerMessage::ModelChanged {
            revision: 3,
            upserts: vec![],
            deletes: vec![],
        })
        .unwrap();
    assert!(matches!(
        next(&mut c).await,
        Some(ServerMessage::ModelChanged { revision: 1, .. })
    ));
    // The server forwards the gap as it is; the client sees it and asks.
    assert!(matches!(
        next(&mut c).await,
        Some(ServerMessage::ModelChanged { revision: 3, .. })
    ));
    send(
        &mut c,
        &ClientMessage::Request {
            id: RequestId(8),
            request: Request::Snapshot,
        },
    )
    .await;
    let seen = wait_result(&mut c, 8).await;
    assert!(matches!(
        &seen[0],
        ServerMessage::Snapshot { snapshot } if snapshot.revision == 3
    ));
    // Later broadcasts continue from the snapshot.
    f.out
        .try_send(ServerMessage::ModelChanged {
            revision: 4,
            upserts: vec![],
            deletes: vec![],
        })
        .unwrap();
    assert!(matches!(
        next(&mut c).await,
        Some(ServerMessage::ModelChanged { revision: 4, .. })
    ));
}

#[tokio::test]
async fn a_big_subtree_upsert_goes_through() {
    let f = fixture();
    let mut c = f.client().await;
    let children: Vec<_> = (2..2002u128)
        .map(|n| {
            cue(
                n,
                CueKind::Group {
                    mode: GroupMode::Timeline,
                    children: vec![],
                },
            )
        })
        .collect();
    let group = cue(
        1,
        CueKind::Group {
            mode: GroupMode::Timeline,
            children,
        },
    );
    f.out
        .try_send(ServerMessage::ModelChanged {
            revision: 1,
            upserts: vec![Upsert::Cue {
                list: list_id(),
                parent: None,
                index: 0,
                cue: Box::new(group),
            }],
            deletes: vec![Delete::Cue { id: id(9999) }],
        })
        .unwrap();
    match next(&mut c).await {
        Some(ServerMessage::ModelChanged { upserts, .. }) => match &upserts[..] {
            [Upsert::Cue { cue, .. }] => assert_eq!(cue.children().len(), 2000),
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_message_over_the_frame_cap_makes_the_client_resnapshot() {
    let f = fixture();
    f.control.revision.store(2, Ordering::SeqCst);
    let mut a = f.client().await;
    let mut b = f.client().await;
    // 33 MiB of log text cannot be sent (MAX_SERVER_FRAME_BYTES is 32 MiB).
    f.out
        .try_send(ServerMessage::Log {
            level: LogLevel::Info,
            message: "x".repeat(33 * 1024 * 1024),
        })
        .unwrap();
    f.control.revision.store(3, Ordering::SeqCst);
    for c in [&mut a, &mut b] {
        match next(c).await {
            Some(ServerMessage::Snapshot { snapshot }) => assert!(snapshot.revision >= 2),
            other => panic!("{other:?}"),
        }
    }
    // And the connection works on afterwards, without the swallowed result showing.
    send(&mut a, &go(4)).await;
    assert_eq!(wait_result(&mut a, 4).await.len(), 1);
}

#[tokio::test]
async fn meters_and_progress_are_relayed() {
    let f = fixture();
    let mut c = f.client().await;
    f.out
        .try_send(ServerMessage::Meters {
            outputs: vec![],
            cues: vec![],
        })
        .unwrap();
    f.out
        .try_send(ServerMessage::Progress { items: vec![] })
        .unwrap();
    assert!(matches!(
        next(&mut c).await,
        Some(ServerMessage::Meters { .. })
    ));
    assert!(matches!(
        next(&mut c).await,
        Some(ServerMessage::Progress { .. })
    ));
}

// --- slow and bad clients ------------------------------------------------------------------

#[tokio::test]
async fn a_client_that_does_not_read_blocks_neither_the_control_loop_nor_other_clients() {
    let f = fixture_with(ServerConfig {
        write_timeout: Duration::from_millis(500),
        ..ServerConfig::default()
    });
    let mut stalled = f.client().await;
    let mut live = f.client().await;
    let total = 1500usize;
    let producer = {
        let out = f.out.clone();
        thread::spawn(move || {
            for n in 0..total {
                // 16 KiB each: far more than the socket buffers and the client queue hold.
                let message = format!("{n:08}{}", "x".repeat(16 * 1024));
                // The control loop's own channel never stays full because the fan-out never
                // waits for a client; one that does not drain within the guard means it blocked.
                let mut m = ServerMessage::Log {
                    level: LogLevel::Info,
                    message,
                };
                let deadline = std::time::Instant::now() + GUARD;
                loop {
                    match out.try_send(m) {
                        Ok(()) => break,
                        Err(TrySendError::Full(back)) => {
                            assert!(std::time::Instant::now() < deadline, "fan-out blocked");
                            m = back;
                            thread::yield_now();
                        }
                        Err(TrySendError::Closed(_)) => panic!("fan-out gone"),
                    }
                }
            }
        })
    };
    for n in 0..total {
        match next(&mut live).await {
            Some(ServerMessage::Log { message, .. }) => {
                assert!(message.starts_with(&format!("{n:08}")));
            }
            other => panic!("{other:?}"),
        }
    }
    producer.join().unwrap();
    // The live client still gets replies.
    send(&mut live, &go(1)).await;
    wait_result(&mut live, 1).await;
    // The stalled one is closed by the write timeout; reading it now drains what was buffered
    // and must reach the end of the connection.
    expect_closed(&mut stalled).await;
}

#[tokio::test]
async fn malformed_frames_are_answered_and_the_connection_stays_up() {
    let f = fixture();
    let mut c = f.client().await;
    send_text(&mut c, "not json".into()).await;
    assert_eq!(error_code(next(&mut c).await), ErrorCode::BadRequest);
    send_text(&mut c, r#"{"type":"frobnicate"}"#.into()).await;
    assert_eq!(error_code(next(&mut c).await), ErrorCode::UnknownMessage);
    send_text(&mut c, r#"{"id":1}"#.into()).await;
    assert_eq!(error_code(next(&mut c).await), ErrorCode::BadRequest);
    send_text(
        &mut c,
        r#"{"type":"transport","id":1,"action":{"type":"nope"}}"#.into(),
    )
    .await;
    assert_eq!(error_code(next(&mut c).await), ErrorCode::BadRequest);
    guard("sending a frame", c.send(Message::binary(vec![1u8, 2, 3])))
        .await
        .unwrap();
    assert_eq!(error_code(next(&mut c).await), ErrorCode::BadRequest);
    // A second hello is not a way to re-authenticate.
    send_text(&mut c, hello_text("x", PROTOCOL_VERSION, "again")).await;
    assert_eq!(error_code(next(&mut c).await), ErrorCode::BadRequest);
    // The reserved id.
    send(&mut c, &go(u64::MAX)).await;
    assert_eq!(error_code(next(&mut c).await), ErrorCode::BadRequest);
    // Still working.
    send(&mut c, &go(2)).await;
    wait_result(&mut c, 2).await;
    // None of the bad frames reached the control loop.
    let seen = f.control.seen.lock().unwrap();
    assert!(
        seen.iter()
            .all(|m| !matches!(m, ClientMessage::Hello { .. }))
    );
}

#[tokio::test]
async fn an_endless_run_of_bad_frames_closes_the_connection() {
    let f = fixture();
    let mut c = f.client().await;
    for _ in 0..40 {
        send_text(&mut c, "junk".into()).await;
    }
    expect_closed(&mut c).await;
}

#[tokio::test]
async fn a_frame_over_the_client_limit_closes_the_connection() {
    let f = fixture();
    let mut c = f.client().await;
    send_text(&mut c, "x".repeat(1024 * 1024 + 1)).await;
    expect_closed(&mut c).await;
}

#[tokio::test]
async fn goodbye_ends_the_session_and_only_shutdown_reaches_the_control_loop() {
    let f = fixture();
    let mut c = f.client().await;
    send(&mut c, &ClientMessage::Goodbye { shutdown: false }).await;
    expect_closed(&mut c).await;
    // The session was closed; give the control thread a message to prove it is still alive.
    let mut d = f.client().await;
    assert!(!f.control.shutdown.load(Ordering::SeqCst));
    send(&mut d, &ClientMessage::Goodbye { shutdown: true }).await;
    expect_closed(&mut d).await;
    // The stand-in records the goodbye on its thread; a round trip orders after it.
    let mut e = f.client().await;
    send(&mut e, &go(1)).await;
    wait_result(&mut e, 1).await;
    assert!(f.control.shutdown.load(Ordering::SeqCst));
}

// --- keepalive ------------------------------------------------------------------------------

#[tokio::test]
async fn the_server_pings_and_a_reading_client_stays_connected() {
    let f = fixture_with(ServerConfig {
        ping_interval: Duration::from_millis(40),
        dead_after: Duration::from_millis(400),
        ..ServerConfig::default()
    });
    let mut c = f.client().await;
    guard("waiting for 15 pings", async {
        let mut pings = 0;
        while pings < 15 {
            match c.next().await {
                Some(Ok(Message::Ping(_))) => pings += 1,
                other => panic!("{other:?}"),
            }
        }
    })
    .await;
    // Still alive: it answers requests.
    send(&mut c, &go(1)).await;
    wait_result(&mut c, 1).await;
}

#[tokio::test]
async fn a_client_that_answers_nothing_is_dropped() {
    let f = fixture_with(ServerConfig {
        ping_interval: Duration::from_millis(30),
        dead_after: Duration::from_millis(120),
        ..ServerConfig::default()
    });
    let mut c = f.client().await;
    // Read the raw socket, bypassing the WebSocket layer, so no pong is ever sent; the server
    // must close the connection after `dead_after`.
    let MaybeTlsStream::Plain(tcp) = c.get_mut() else {
        panic!("not a plain socket");
    };
    // One guard for the whole wait: the server's pings keep arriving until it gives up.
    let mut buf = [0u8; 4096];
    guard("the server giving up on a silent client", async {
        loop {
            let n = tokio::io::AsyncReadExt::read(tcp, &mut buf).await;
            if matches!(n, Ok(0) | Err(_)) {
                break;
            }
        }
    })
    .await;
}

#[tokio::test]
async fn stopping_the_server_closes_every_connection() {
    let f = fixture();
    let mut a = f.client().await;
    let mut b = f.client().await;
    f.server.stop();
    expect_closed(&mut a).await;
    expect_closed(&mut b).await;
}

// --- regression: snapshot ordering -----------------------------------------------------------

/// Blocks until the stand-in has seen a message matching `pred` (hang-guarded). Nothing on the
/// test's own runtime needs to run meanwhile: the server has its own.
fn wait_seen(c: &Control, pred: impl Fn(&ClientMessage) -> bool) {
    let deadline = std::time::Instant::now() + GUARD;
    loop {
        if c.seen.lock().unwrap().iter().any(&pred) {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "hang");
        thread::yield_now();
    }
}

fn has(seen: &[ServerMessage], pred: impl Fn(&ServerMessage) -> bool) -> Option<usize> {
    seen.iter().position(pred)
}

#[tokio::test]
async fn a_requested_snapshot_never_reaches_the_client_after_a_newer_model_changed() {
    let f = fixture();
    let mut a = f.client().await;
    let mut b = f.client().await;
    // A asks for a snapshot; the stand-in takes it at revision 0 but holds the answer back.
    f.control.hold_snapshots.store(true, Ordering::SeqCst);
    send(
        &mut a,
        &ClientMessage::Request {
            id: RequestId(8),
            request: Request::Snapshot,
        },
    )
    .await;
    wait_seen(&f.control, |m| {
        matches!(
            m,
            ClientMessage::Request {
                id: RequestId(8),
                ..
            }
        )
    });
    // B edits: revision 1, newer than A's snapshot. Once B has its model_changed, the fan-out
    // has put it in A's queue too (it offers each message to every client in one go).
    send(&mut b, &rename(1)).await;
    wait_result(&mut b, 1).await;
    // Only now does A's (older) snapshot reach A's connection: both are ready at once.
    f.control.release_replies();
    let result8 = |m: &ServerMessage| matches!(m, ServerMessage::Result { id, .. } if id.0 == 8);
    let changed1 = |m: &ServerMessage| matches!(m, ServerMessage::ModelChanged { revision: 1, .. });
    let mut seen = vec![];
    while has(&seen, result8).is_none() || has(&seen, changed1).is_none() {
        seen.push(next(&mut a).await.expect("closed"));
    }
    let snap = has(
        &seen,
        |m| matches!(m, ServerMessage::Snapshot { snapshot } if snapshot.revision == 0),
    )
    .unwrap_or_else(|| panic!("no snapshot: {seen:?}"));
    assert!(
        snap < has(&seen, changed1).unwrap(),
        "snapshot 0 after model_changed 1: {seen:?}"
    );
    assert!(snap < has(&seen, result8).unwrap(), "{seen:?}");
}

#[tokio::test]
async fn a_snapshot_older_than_what_the_client_has_is_followed_by_a_fresh_one() {
    let f = fixture();
    let mut c = f.client().await;
    send(&mut c, &rename(1)).await;
    wait_result(&mut c, 1).await;
    // The stand-in answers with a snapshot at revision 0, behind the client's 1.
    send(
        &mut c,
        &ClientMessage::Request {
            id: RequestId(2),
            request: Request::Relink {
                folder: "/x".into(),
            },
        },
    )
    .await;
    let mut seen = wait_result(&mut c, 2).await;
    let fresh = |m: &ServerMessage| matches!(m, ServerMessage::Snapshot { snapshot } if snapshot.revision == 1);
    while has(&seen, fresh).is_none() {
        seen.push(next(&mut c).await.expect("closed"));
    }
    let stale = has(
        &seen,
        |m| matches!(m, ServerMessage::Snapshot { snapshot } if snapshot.revision == 0),
    )
    .unwrap_or_else(|| panic!("{seen:?}"));
    assert!(stale < has(&seen, fresh).unwrap(), "{seen:?}");
    // And the connection goes on from the fresh one.
    f.out.try_send(changed(1)).unwrap();
    f.out.try_send(changed(2)).unwrap();
    assert!(matches!(
        next(&mut c).await,
        Some(ServerMessage::ModelChanged { revision: 2, .. })
    ));
}

// --- slow-client policy and limits -----------------------------------------------------------

/// Hands `m` to the fan-out from a plain thread, failing if it does not drain within the guard.
fn push(out: &Out, mut m: ServerMessage) {
    let deadline = std::time::Instant::now() + GUARD;
    loop {
        match out.try_send(m) {
            Ok(()) => return,
            Err(TrySendError::Full(back)) => {
                assert!(std::time::Instant::now() < deadline, "fan-out blocked");
                m = back;
                thread::yield_now();
            }
            Err(TrySendError::Closed(_)) => panic!("fan-out gone"),
        }
    }
}

fn changed(revision: u64) -> ServerMessage {
    ServerMessage::ModelChanged {
        revision,
        upserts: vec![],
        deletes: vec![],
    }
}

#[tokio::test]
async fn a_client_whose_queue_overflows_gets_a_snapshot_and_then_only_newer_broadcasts() {
    let f = fixture_with(ServerConfig {
        // The stalled client must not be closed while the queue fills.
        write_timeout: Duration::from_secs(120),
        ..ServerConfig::default()
    });
    f.control.revision.store(5, Ordering::SeqCst);
    let mut stalled = f.client().await;
    let mut live = f.client().await;
    // Far more than the socket buffers and the 1024-message queue hold (16 KiB each).
    let total = 4000usize;
    let producer = {
        let out = f.out.clone();
        thread::spawn(move || {
            for n in 0..total {
                let message = format!("{n:08}{}", "x".repeat(16 * 1024));
                push(
                    &out,
                    ServerMessage::Log {
                        level: LogLevel::Info,
                        message,
                    },
                );
            }
        })
    };
    // The fan-out offers each message to every client in turn: once the live client has the
    // last one, the stalled client's queue has overflowed.
    for n in 0..total {
        match next(&mut live).await {
            Some(ServerMessage::Log { message, .. }) => {
                assert!(message.starts_with(&format!("{n:08}")));
            }
            other => panic!("{other:?}"),
        }
    }
    producer.join().unwrap();
    // It reads again: some logs (what was already on the way), then a fresh snapshot instead
    // of the lost rest. Whether the connection's writes stall at once depends on the OS's
    // socket buffers: where they keep making progress for a while (Windows loopback), the
    // connection resyncs while the producer is still sending, so logs made after that resync
    // (and, if the queue overflows again, further snapshots) follow the first snapshot. All
    // of that is correct; every snapshot is at revision 5.
    let mut logs = 0;
    let mut snapshots = 0;
    loop {
        // Each read is guarded by `next`; the loop ends at the first snapshot, and at most
        // `total` logs can come before it.
        match next(&mut stalled).await.expect("closed") {
            ServerMessage::Log { .. } => logs += 1,
            ServerMessage::Snapshot { snapshot } => {
                assert_eq!(snapshot.revision, 5);
                snapshots += 1;
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    // Later broadcasts are filtered against the snapshot: 5 is in it, 6 is new. As in the real
    // control loop, the revision moves before its model_changed goes out: if the connection
    // lagged again and the hub skipped model_changed 6, the next snapshot (asked after that)
    // is at revision 6 and covers it.
    f.control.revision.store(6, Ordering::SeqCst);
    f.out.try_send(changed(5)).unwrap();
    f.out.try_send(changed(6)).unwrap();
    guard("revision 6 reaching the client", async {
        loop {
            match next(&mut stalled).await.expect("closed") {
                ServerMessage::Log { .. } => logs += 1,
                ServerMessage::Snapshot { snapshot } => {
                    assert!(snapshot.revision >= 5);
                    snapshots += 1;
                    if snapshot.revision == 6 {
                        break;
                    }
                }
                ServerMessage::ModelChanged { revision, .. } => {
                    assert_eq!(revision, 6, "model_changed 5 was not filtered");
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
    })
    .await;
    assert!(
        logs < total,
        "nothing was dropped: the queue never overflowed ({snapshots} snapshots)"
    );
    // The server's own snapshot requests leak no result.
    send(&mut stalled, &go(1)).await;
    let seen = wait_result(&mut stalled, 1).await;
    assert!(
        seen.iter()
            .all(|m| !matches!(m, ServerMessage::Result { id, .. } if id.0 != 1)),
        "{seen:?}"
    );
}

/// Waits until the server closes a new connection without upgrading it.
async fn expect_refused(port: u16) {
    let mut tcp = connect_tcp(port).await;
    let mut buf = [0u8; 1];
    let n = tokio::time::timeout(GUARD, tokio::io::AsyncReadExt::read(&mut tcp, &mut buf))
        .await
        .expect("not refused");
    assert!(matches!(n, Ok(0) | Err(_)), "{n:?}");
}

#[tokio::test]
async fn the_connection_after_the_limit_is_refused_and_logged_once() {
    let config = ServerConfig {
        // No second log entry within the test, however slow the machine.
        refusal_log_interval: Duration::from_secs(24 * 3600),
        ..ServerConfig::default()
    };
    let limit = config.max_connections;
    let f = fixture_with(config);
    let mut clients = vec![];
    for _ in 0..limit {
        clients.push(f.client().await);
    }
    // The 17th (and 18th) are closed at accept, before any upgrade.
    expect_refused(f.server.port()).await;
    expect_refused(f.server.port()).await;
    // Each refusal is sent to the inbox before its socket closes; a round trip through the
    // stand-in orders after both.
    send(&mut clients[0], &go(1)).await;
    wait_result(&mut clients[0], 1).await;
    let logs = f.control.logs.lock().unwrap().clone();
    assert_eq!(logs.len(), 1, "{logs:?}");
    assert!(logs[0].contains("connection limit"), "{logs:?}");
    // Every admitted client still works.
    for (n, c) in clients.iter_mut().enumerate() {
        let n = n as u64 + 10;
        send(c, &go(n)).await;
        wait_result(c, n).await;
    }
}

#[tokio::test]
async fn frames_are_read_while_a_result_waits_for_its_model_changed() {
    let f = fixture_with(ServerConfig {
        // Longer than the guard: the held result can only go out through its model_changed.
        revision_wait: Duration::from_secs(600),
        ..ServerConfig::default()
    });
    let mut c = f.client().await;
    // The stand-in "loses" the undo's model_changed: its result (revision 1) is held.
    send(&mut c, &ClientMessage::Undo { id: RequestId(5) }).await;
    // A GO meanwhile is read, forwarded and answered at once.
    send(&mut c, &go(6)).await;
    assert!(matches!(
        next(&mut c).await,
        Some(ServerMessage::Result {
            id: RequestId(6),
            ..
        })
    ));
    // The model_changed arrives late: then, and only then, the held result.
    f.out.try_send(changed(1)).unwrap();
    assert!(matches!(
        next(&mut c).await,
        Some(ServerMessage::ModelChanged { revision: 1, .. })
    ));
    assert!(matches!(
        next(&mut c).await,
        Some(ServerMessage::Result {
            id: RequestId(5),
            ..
        })
    ));
}

#[tokio::test]
async fn refusals_after_the_first_log_are_logged_once_the_interval_has_passed() {
    let f = fixture_with(ServerConfig {
        max_handshaking: 1,
        refusal_log_interval: Duration::from_millis(50),
        ..ServerConfig::default()
    });
    // A silent raw connection holds the only handshake slot.
    let _held = connect_tcp(f.server.port()).await;
    // Every further connection is refused: the first refusal is logged at once, the others
    // when the interval has passed, even though no connection follows them.
    for _ in 0..3 {
        expect_refused(f.server.port()).await;
    }
    let total = |logs: &[String]| -> u64 {
        logs.iter()
            .map(|l| {
                l.trim_start_matches("UI server refused ")
                    .split(' ')
                    .next()
                    .unwrap()
                    .parse::<u64>()
                    .unwrap()
            })
            .sum()
    };
    // Hang guard only: the flush is the server's own timer.
    let deadline = std::time::Instant::now() + GUARD;
    loop {
        let logs = f.control.logs.lock().unwrap().clone();
        if total(&logs) >= 3 {
            assert_eq!(total(&logs), 3, "{logs:?}");
            assert!(logs.iter().all(|l| l.contains("handshaking")), "{logs:?}");
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "not flushed: {logs:?}"
        );
        tokio::task::yield_now().await;
    }
}
