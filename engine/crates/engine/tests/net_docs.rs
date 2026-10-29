// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Several workspaces over real sockets in front of the real control loop (docs/protocol.md
//! section 2): an app connection (`hello` without a workspace) sees the `workspaces` list and
//! opens and closes docs; a connection bound to a doc (`hello { workspace }`) gets that doc's
//! snapshot and changes only, numbered per doc; closing a doc sends `closed` to its connections
//! and closes them.

mod show;

use std::thread;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use openlab_audio::Mixer;
use openlab_core::protocol::{
    ClientMessage, DocId, ErrorCode, Outcome, PROTOCOL_VERSION, Request, RequestId, ResultData,
    ServerMessage, Snapshot, WorkspaceInfo, decode_server, encode_client,
};
use openlab_core::{Command, CueKind, CueProperty, Workspace, save_show};
use openlab_engine::control::{Backend, Control, OUTBOUND_CAPACITY, outbound_channel, runner};
use openlab_engine::net::{ServerConfig, WsServer};
use show::{config, cue, id, mixer_config, workspace};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

const GUARD: Duration = Duration::from_secs(20);

type Client = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// The real control loop on doc 0 (`ws`) with an application folder in `dir`, and the server.
fn engine(ws: Workspace, dir: &std::path::Path) -> (WsServer, thread::JoinHandle<()>) {
    engine_with(ws, dir, |_| {})
}

fn engine_with(
    ws: Workspace,
    dir: &std::path::Path,
    tweak: impl FnOnce(&mut openlab_engine::control::ControlConfig),
) -> (WsServer, thread::JoinHandle<()>) {
    let (ctl, _mixer) = Mixer::new(mixer_config()).unwrap();
    let (inbox_tx, inbox) = crossbeam_channel::unbounded();
    let (out_tx, outbound) = outbound_channel(OUTBOUND_CAPACITY);
    let mut c = config(&ws);
    c.app_dirs = Some(openlab_engine::persist::AppDirs::new(dir.join("support")));
    tweak(&mut c);
    let control = Control::new(ws, ctl, Backend::Offline, c, inbox_tx.clone(), out_tx).unwrap();
    let worker = thread::spawn(move || runner::run(control, &inbox));
    let server = WsServer::start(inbox_tx, outbound, ServerConfig::default()).unwrap();
    (server, worker)
}

async fn hello(server: &WsServer, workspace: Option<DocId>) -> Client {
    let (mut c, _) = connect_async(format!("ws://127.0.0.1:{}", server.port()))
        .await
        .unwrap();
    let hello = ClientMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        token: server.token().to_owned(),
        client_name: "test".into(),
        workspace,
        pid: None,
    };
    send(&mut c, hello).await;
    c
}

/// A connection bound to `doc`, and the snapshot its welcome carried.
async fn bind(server: &WsServer, doc: DocId) -> (Client, Snapshot) {
    let mut c = hello(server, Some(doc)).await;
    match next(&mut c).await {
        ServerMessage::Welcome {
            doc: Some(d),
            snapshot: Some(snapshot),
            workspaces: None,
            ..
        } => {
            assert_eq!(d, doc);
            (c, *snapshot)
        }
        other => panic!("{other:?}"),
    }
}

/// An app connection, and the workspace list its welcome carried.
async fn app(server: &WsServer) -> (Client, Vec<WorkspaceInfo>) {
    let mut c = hello(server, None).await;
    match next(&mut c).await {
        ServerMessage::Welcome {
            doc: None,
            snapshot: None,
            workspaces: Some(list),
            ..
        } => (c, list),
        other => panic!("{other:?}"),
    }
}

async fn next(c: &mut Client) -> ServerMessage {
    next_or_end(c)
        .await
        .unwrap_or_else(|| panic!("the connection ended"))
}

/// The next message, or `None` once the server has closed the connection.
async fn next_or_end(c: &mut Client) -> Option<ServerMessage> {
    loop {
        match tokio::time::timeout(GUARD, c.next()).await.expect("hang") {
            Some(Ok(Message::Text(t))) => return Some(decode_server(t.as_str()).unwrap()),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            _ => return None,
        }
    }
}

async fn send(c: &mut Client, message: ClientMessage) {
    c.send(Message::text(encode_client(&message).unwrap()))
        .await
        .unwrap();
}

/// `goodbye { shutdown }`, then waits for the server to close the connection: dropping a client
/// that still has unread messages would reset the socket and could lose the goodbye.
async fn shut_down(c: &mut Client) {
    send(c, ClientMessage::Goodbye { shutdown: true }).await;
    while next_or_end(c).await.is_some() {}
}

/// Sends `message` and reads until its result, collecting what came before it.
async fn call(c: &mut Client, message: ClientMessage) -> (Outcome, Vec<ServerMessage>) {
    let want = match &message {
        ClientMessage::Command { id, .. }
        | ClientMessage::Request { id, .. }
        | ClientMessage::Close { id, .. }
        | ClientMessage::Panic { id }
        | ClientMessage::Transport { id, .. } => *id,
        other => panic!("{other:?} has no result"),
    };
    send(c, message).await;
    let mut before = Vec::new();
    loop {
        match next(c).await {
            ServerMessage::Result { id, outcome } if id == want => return (outcome, before),
            m => before.push(m),
        }
    }
}

fn request(id: u64, request: Request) -> ClientMessage {
    ClientMessage::Request {
        id: RequestId(id),
        request,
    }
}

fn rename(id: u64, cue: u128, name: &str) -> ClientMessage {
    ClientMessage::Command {
        id: RequestId(id),
        command: Box::new(Command::SetCueProperty {
            id: self::id(cue),
            property: CueProperty::Name { value: name.into() },
        }),
        coalesce: None,
    }
}

fn open(id: u64, path: &std::path::Path) -> ClientMessage {
    request(
        id,
        Request::Open {
            path: path.to_string_lossy().into_owned(),
            recovery: None,
        },
    )
}

fn opened(outcome: &Outcome) -> DocId {
    match outcome {
        Outcome::Ok(ok) => match ok.data {
            Some(ResultData::Opened { doc }) => doc,
            ref other => panic!("{other:?}"),
        },
        Outcome::Error(e) => panic!("{e:?}"),
    }
}

fn error_code(outcome: &Outcome) -> ErrorCode {
    match outcome {
        Outcome::Error(e) => e.code,
        Outcome::Ok(ok) => panic!("ok: {ok:?}"),
    }
}

/// The first `workspaces` list that satisfies `pred`: in `seen` (what came before a result: a
/// broadcast is written before a reply), else read from `c`.
async fn list_until(
    seen: &[ServerMessage],
    c: &mut Client,
    pred: impl Fn(&[WorkspaceInfo]) -> bool,
) -> Vec<WorkspaceInfo> {
    for m in seen {
        if let ServerMessage::Workspaces { workspaces } = m
            && pred(workspaces)
        {
            return workspaces.clone();
        }
    }
    loop {
        if let ServerMessage::Workspaces { workspaces } = next(c).await
            && pred(&workspaces)
        {
            return workspaces;
        }
    }
}

fn run(f: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(120), f)
                .await
                .expect("hang: the session over the wire did not finish");
        });
}

/// Two docs over the wire: the app connection lists both and follows one's unsaved state; a
/// connection bound to one sees only its doc's changes, numbered per doc; workspace messages on
/// the app connection answer `no_workspace`; the app-scope panic works on any connection.
#[test]
fn two_docs_are_isolated_and_the_app_connection_follows_the_list() {
    let dir = tempfile::tempdir().unwrap();
    let (server, worker) = engine(workspace(vec![cue(1, CueKind::Memo)]), dir.path());
    run(async {
        let (mut app_c, list) = app(&server).await;
        assert_eq!(list.len(), 1);
        let first = list[0].doc;
        assert!(!list[0].dirty);

        // `new` on the app connection opens another doc beside the first.
        let (outcome, before) = call(&mut app_c, request(1, Request::New {})).await;
        let second = opened(&outcome);
        assert_ne!(second, first);
        assert!(
            !before
                .iter()
                .any(|m| matches!(m, ServerMessage::Snapshot { .. })),
            "no snapshot on the app connection"
        );
        list_until(&before, &mut app_c, |l| l.len() == 2 && l[1].doc == second).await;

        let (mut a, snap_a) = bind(&server, first).await;
        let (mut b, snap_b) = bind(&server, second).await;
        assert!(snap_b.workspace.cue_lists[0].cues.is_empty());
        assert_eq!(
            snap_b.revision, 1,
            "each doc is numbered from its own start"
        );

        // An edit of A reaches A and makes A dirty in the list; B sees nothing of it.
        let (outcome, before) = call(&mut a, rename(2, 1, "A's")).await;
        assert!(matches!(outcome, Outcome::Ok(_)));
        assert!(before.iter().any(|m| matches!(
            m,
            ServerMessage::ModelChanged { revision, .. } if *revision == snap_a.revision + 1
        )));
        list_until(&[], &mut app_c, |l| l.first().is_some_and(|w| w.dirty)).await;
        // B's own request comes back with nothing of A's in front of it.
        let (outcome, before) = call(&mut b, request(3, Request::Snapshot)).await;
        assert!(matches!(outcome, Outcome::Ok(_)));
        for m in &before {
            match m {
                ServerMessage::Snapshot { snapshot } => {
                    assert!(snapshot.workspace.find_cue(id(1)).is_none());
                    assert_eq!(snapshot.revision, 1);
                }
                ServerMessage::ModelChanged { .. } | ServerMessage::Runtime { .. } => {
                    panic!("B got A's {m:?}")
                }
                _ => {}
            }
        }

        // Workspace messages need a bound connection.
        let (outcome, _) = call(&mut app_c, rename(4, 1, "nope")).await;
        assert_eq!(error_code(&outcome), ErrorCode::NoWorkspace);
        // The app-scope panic works on any connection.
        let (outcome, _) = call(&mut app_c, ClientMessage::Panic { id: RequestId(5) }).await;
        assert!(matches!(outcome, Outcome::Ok(_)));
        let (outcome, _) = call(&mut b, ClientMessage::Panic { id: RequestId(6) }).await;
        assert!(matches!(outcome, Outcome::Ok(_)));
        // So does the PB-10 override (engine-wide).
        let overrides = request(
            7,
            Request::SetOutputOverrides {
                network_disabled: true,
            },
        );
        let (outcome, _) = call(&mut app_c, overrides).await;
        assert!(matches!(outcome, Outcome::Ok(_)), "{outcome:?}");

        shut_down(&mut a).await;
    });
    worker.join().unwrap();
    server.stop();
}

/// `close` sends `closed` to the doc's connections (after the requester's result) and the server
/// closes them; the app connection's list loses the doc; binding to it again is refused.
#[test]
fn closing_a_doc_sends_closed_and_closes_its_connections() {
    let dir = tempfile::tempdir().unwrap();
    let (server, worker) = engine(workspace(vec![cue(1, CueKind::Memo)]), dir.path());
    run(async {
        let (mut app_c, _) = app(&server).await;
        let (outcome, _) = call(&mut app_c, request(1, Request::New {})).await;
        let doc = opened(&outcome);
        let (mut a, _) = bind(&server, doc).await;
        let (mut watcher, _) = bind(&server, doc).await;

        // Unsaved edits refuse a close without `discard_changes`.
        let edit = ClientMessage::Command {
            id: RequestId(3),
            command: Box::new(Command::RenameWorkspace {
                name: "edited".into(),
            }),
            coalesce: None,
        };
        let (outcome, _) = call(&mut a, edit).await;
        assert!(matches!(outcome, Outcome::Ok(_)));
        let close = |id, discard_changes| ClientMessage::Close {
            id: RequestId(id),
            doc: None,
            discard_changes,
        };
        let (outcome, _) = call(&mut a, close(4, false)).await;
        assert_eq!(error_code(&outcome), ErrorCode::UnsavedChanges);

        // The bound connection closes its own doc: its result first, then `closed`, then the end.
        let (outcome, _) = call(&mut a, close(5, true)).await;
        assert!(matches!(outcome, Outcome::Ok(_)), "{outcome:?}");
        for c in [&mut a, &mut watcher] {
            loop {
                match next_or_end(c).await {
                    Some(ServerMessage::Closed { doc: d }) => {
                        assert_eq!(d, doc);
                        break;
                    }
                    Some(_) => {}
                    None => panic!("closed without `closed`"),
                }
            }
            assert!(next_or_end(c).await.is_none(), "the server closes it");
        }
        list_until(&[], &mut app_c, |l| l.iter().all(|w| w.doc != doc)).await;

        // A closed doc is gone for good: binding to it is refused.
        let mut late = hello(&server, Some(doc)).await;
        match next(&mut late).await {
            ServerMessage::Error { code, .. } => assert_eq!(code, ErrorCode::NoWorkspace),
            other => panic!("{other:?}"),
        }
        assert!(next_or_end(&mut late).await.is_none());
        // Closing it again names a doc that is not open.
        let closed_again = ClientMessage::Close {
            id: RequestId(6),
            doc: Some(doc),
            discard_changes: true,
        };
        let (outcome, _) = call(&mut app_c, closed_again).await;
        assert_eq!(error_code(&outcome), ErrorCode::NoWorkspace);

        shut_down(&mut app_c).await;
    });
    worker.join().unwrap();
    server.stop();
}

/// Opening a show that is open already answers `already_open`; a copy of it (same workspace id,
/// another file) opens with a fresh id and unsaved, as QLab 5 does for a Finder duplicate.
#[test]
fn an_open_show_answers_already_open_and_a_copy_gets_a_fresh_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut show = workspace(vec![cue(5, CueKind::Memo)]);
    show.name = "on disk".into();
    let path = dir.path().join("show.openlab");
    std::fs::write(&path, save_show(&show).unwrap()).unwrap();
    let copy = dir.path().join("show copy.openlab");
    std::fs::copy(&path, &copy).unwrap();
    // Doc 0 is another workspace (`workspace()` gives every test show the same id).
    let mut first = workspace(vec![]);
    first.id = openlab_core::WorkspaceId(uuid::Uuid::from_u128(0xf1));
    let (server, worker) = engine(first, dir.path());
    run(async {
        let (mut c, _) = app(&server).await;
        let (outcome, _) = call(&mut c, open(1, &path)).await;
        let doc = opened(&outcome);
        let (outcome, _) = call(&mut c, open(2, &path)).await;
        assert_eq!(error_code(&outcome), ErrorCode::AlreadyOpen);

        let (outcome, before) = call(&mut c, open(3, &copy)).await;
        let dup = opened(&outcome);
        let list = list_until(&before, &mut c, |l| l.iter().any(|w| w.doc == dup)).await;
        let of = |d: DocId| list.iter().find(|w| w.doc == d).unwrap().clone();
        assert_eq!(of(doc).workspace_id, show.id);
        assert!(!of(doc).dirty);
        assert_ne!(of(dup).workspace_id, show.id, "a fresh id for the copy");
        assert!(of(dup).dirty, "the copy is unsaved");
        shut_down(&mut c).await;
    });
    worker.join().unwrap();
    server.stop();
}
