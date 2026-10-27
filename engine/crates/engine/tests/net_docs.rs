// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Document requests over real sockets in front of the real control loop (M4.4a): every connected
//! client, not just the requester, gets the new `snapshot` after `new`, `open` and `revert`, and
//! keeps receiving `model_changed` afterwards (the connection's per-client filter drops anything
//! at or below the last revision it saw, so a client that missed the snapshot would be lost).

mod common;
mod show;

use std::thread;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use openlab_audio::Mixer;
use openlab_core::protocol::{
    ClientMessage, Outcome, PROTOCOL_VERSION, Request, RequestId, ServerMessage, Snapshot,
    decode_server, encode_client,
};
use openlab_core::{Command, CueKind, CueProperty, Position, save_show};
use openlab_engine::control::{Backend, Control, OUTBOUND_CAPACITY, outbound_channel, runner};
use openlab_engine::net::{ServerConfig, WsServer};
use show::{config, cue, id, mixer_config, workspace};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

const GUARD: Duration = Duration::from_secs(20);

type Client = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(server: &WsServer) -> (Client, Snapshot) {
    let (mut c, _) = connect_async(format!("ws://127.0.0.1:{}", server.port()))
        .await
        .unwrap();
    let hello = ClientMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        token: server.token().to_owned(),
        client_name: "test".into(),
    };
    c.send(Message::text(encode_client(&hello).unwrap()))
        .await
        .unwrap();
    match next(&mut c).await {
        ServerMessage::Welcome { snapshot, .. } => (c, *snapshot),
        other => panic!("{other:?}"),
    }
}

async fn next(c: &mut Client) -> ServerMessage {
    loop {
        match tokio::time::timeout(GUARD, c.next()).await.expect("hang") {
            Some(Ok(Message::Text(t))) => return decode_server(t.as_str()).unwrap(),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            other => panic!("connection ended: {other:?}"),
        }
    }
}

/// `goodbye { shutdown }`, then waits for the server to close the connection: dropping a client
/// that still has unread messages would reset the socket and could lose the goodbye.
async fn shut_down(c: &mut Client) {
    send(c, ClientMessage::Goodbye { shutdown: true }).await;
    while let Ok(Some(Ok(_))) = tokio::time::timeout(GUARD, c.next()).await {}
}

async fn send(c: &mut Client, message: ClientMessage) {
    c.send(Message::text(encode_client(&message).unwrap()))
        .await
        .unwrap();
}

/// Reads until a snapshot arrives (the next messages of other kinds are skipped).
async fn snapshot_next(c: &mut Client) -> Snapshot {
    loop {
        if let ServerMessage::Snapshot { snapshot } = next(c).await {
            return *snapshot;
        }
    }
}

async fn result_of(c: &mut Client, id: u64) -> Outcome {
    loop {
        if let ServerMessage::Result { id: rid, outcome } = next(c).await
            && rid.0 == id
        {
            return outcome;
        }
    }
}

#[tokio::test]
async fn a_second_client_gets_the_new_snapshot_and_keeps_receiving_changes() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace(vec![cue(1, CueKind::Memo)]);
    // A show on disk for `open`, with another name and cue.
    let mut other = workspace(vec![cue(5, CueKind::Memo), cue(6, CueKind::Memo)]);
    other.name = "other".into();
    let other_path = dir.path().join("other.openlab");
    std::fs::write(&other_path, save_show(&other).unwrap()).unwrap();

    let (ctl, _mixer) = Mixer::new(mixer_config()).unwrap();
    let (inbox_tx, inbox) = crossbeam_channel::unbounded();
    let (out_tx, outbound) = outbound_channel(OUTBOUND_CAPACITY);
    let mut c = config(&ws);
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
    let worker = thread::spawn(move || runner::run(control, &inbox));
    let server = WsServer::start(inbox_tx, outbound, ServerConfig::default()).unwrap();

    tokio::time::timeout(Duration::from_secs(120), async {
        let (mut a, first) = connect(&server).await;
        let (mut b, _) = connect(&server).await;
        let mut last_revision = first.revision;

        // A opens another show: A gets its snapshot then its result, B gets the broadcast.
        send(
            &mut a,
            ClientMessage::Request {
                id: RequestId(1),
                request: Request::Open {
                    path: other_path.to_string_lossy().into_owned(),
                    discard_changes: false,
                    recovery: None,
                },
            },
        )
        .await;
        let a_snapshot = snapshot_next(&mut a).await;
        assert!(matches!(result_of(&mut a, 1).await, Outcome::Ok(_)));
        let b_snapshot = snapshot_next(&mut b).await;
        assert_eq!(a_snapshot, b_snapshot);
        assert_eq!(b_snapshot.workspace.name, "other");
        assert!(
            b_snapshot.revision > last_revision,
            "numbering keeps rising"
        );
        last_revision = b_snapshot.revision;

        // Edits afterwards reach both, one revision at a time.
        for n in 1..=3u64 {
            send(
                &mut a,
                ClientMessage::Command {
                    id: RequestId(10 + n),
                    command: Box::new(Command::SetCueProperty {
                        id: id(5),
                        property: CueProperty::Name {
                            value: format!("name {n}"),
                        },
                    }),
                },
            )
            .await;
            for client in [&mut a, &mut b] {
                loop {
                    match next(client).await {
                        ServerMessage::ModelChanged { revision, .. } => {
                            assert_eq!(revision, last_revision + n);
                            break;
                        }
                        // The requester has its snapshot as the reply; it is not sent twice.
                        ServerMessage::Snapshot { .. } => panic!("a second snapshot"),
                        _ => {}
                    }
                }
            }
        }
        last_revision += 3;
        assert!(matches!(result_of(&mut a, 13).await, Outcome::Ok(_)));

        // B creates a new workspace over the unsaved edits (discarding them): A gets it too.
        send(
            &mut b,
            ClientMessage::Request {
                id: RequestId(20),
                request: Request::New {
                    discard_changes: true,
                },
            },
        )
        .await;
        let b_new = snapshot_next(&mut b).await;
        let a_new = snapshot_next(&mut a).await;
        assert_eq!(a_new, b_new);
        assert!(a_new.revision > last_revision);
        assert!(a_new.workspace.cue_lists[0].cues.is_empty());
        assert_eq!(a_new.document.path, None);
        assert!(matches!(result_of(&mut b, 20).await, Outcome::Ok(_)));
        last_revision = a_new.revision;

        // And after that, an edit by A reaches B at revision + 1 (B's filter was reset by the
        // snapshot, not left at the old numbering).
        let list = a_new.workspace.cue_lists[0].id;
        send(
            &mut a,
            ClientMessage::Command {
                id: RequestId(30),
                command: Box::new(Command::AddCue {
                    at: Position {
                        list,
                        parent: None,
                        index: 0,
                    },
                    cue: cue(77, CueKind::Memo),
                    auto_number: false,
                }),
            },
        )
        .await;
        for client in [&mut a, &mut b] {
            loop {
                if let ServerMessage::ModelChanged {
                    revision, upserts, ..
                } = next(client).await
                {
                    assert_eq!(revision, last_revision + 1);
                    assert_eq!(upserts.len(), 1);
                    break;
                }
            }
        }

        shut_down(&mut a).await;
    })
    .await
    .expect("hang: the session over the wire did not finish");
    worker.join().unwrap();
    server.stop();
}

/// A real control loop and server whose first document read blocks until `release` is sent.
fn stalled_reads(
    doc_timeout: Duration,
    snapshot_wait: Duration,
) -> (
    WsServer,
    thread::JoinHandle<()>,
    std::sync::mpsc::Sender<()>,
    std::path::PathBuf,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace(vec![cue(1, CueKind::Memo)]);
    let mut other = workspace(vec![cue(5, CueKind::Memo)]);
    other.name = "other".into();
    let other_path = dir.path().join("other.openlab");
    std::fs::write(&other_path, save_show(&other).unwrap()).unwrap();

    let (release_tx, release) = std::sync::mpsc::channel::<()>();
    let release = std::sync::Mutex::new(release);
    let first = std::sync::atomic::AtomicBool::new(true);
    let hook = openlab_engine::control::DocHook(std::sync::Arc::new(move || {
        if first.swap(false, std::sync::atomic::Ordering::SeqCst) {
            let _ = release.lock().unwrap().recv();
        }
    }));
    let (ctl, _mixer) = Mixer::new(mixer_config()).unwrap();
    let (inbox_tx, inbox) = crossbeam_channel::unbounded();
    let (out_tx, outbound) = outbound_channel(OUTBOUND_CAPACITY);
    let mut c = config(&ws);
    c.app_dirs = Some(openlab_engine::persist::AppDirs::new(
        dir.path().join("support"),
    ));
    c.doc_hook = Some(hook);
    c.doc_timeout = doc_timeout;
    let control = Control::new(ws, ctl, Backend::Offline, c, inbox_tx.clone(), out_tx).unwrap();
    let worker = thread::spawn(move || runner::run(control, &inbox));
    let server_config = ServerConfig {
        snapshot_wait,
        ..ServerConfig::default()
    };
    let server = WsServer::start(inbox_tx, outbound, server_config).unwrap();
    (server, worker, release_tx, other_path, dir)
}

fn open_request(id: u64, path: &std::path::Path) -> ClientMessage {
    ClientMessage::Request {
        id: RequestId(id),
        request: Request::Open {
            path: path.to_string_lossy().into_owned(),
            discard_changes: false,
            recovery: None,
        },
    }
}

fn rename_command(id: u64, name: &str) -> ClientMessage {
    ClientMessage::Command {
        id: RequestId(id),
        command: Box::new(Command::SetCueProperty {
            id: self::id(1),
            property: CueProperty::Name { value: name.into() },
        }),
    }
}

#[tokio::test]
async fn a_stalled_open_fails_with_io_and_the_requester_keeps_receiving_broadcasts() {
    let (server, worker, release, other, _dir) =
        stalled_reads(Duration::from_millis(300), Duration::from_secs(60));
    tokio::time::timeout(Duration::from_secs(120), async {
        let (mut a, first) = connect(&server).await;
        let (mut b, _) = connect(&server).await;
        send(&mut a, open_request(1, &other)).await;
        // Once A's snapshot request is answered, its open has been handled (same connection,
        // same order), so B's edit below cannot get in front of it.
        send(
            &mut a,
            ClientMessage::Request {
                id: RequestId(9),
                request: Request::Snapshot,
            },
        )
        .await;
        assert!(matches!(result_of(&mut a, 9).await, Outcome::Ok(_)));
        // Another client edits while A's open is stuck in its read.
        send(&mut b, rename_command(2, "while stuck")).await;
        assert!(matches!(result_of(&mut b, 2).await, Outcome::Ok(_)));
        // A's open fails with io once the engine gives up on the read ...
        match result_of(&mut a, 1).await {
            Outcome::Error(e) => assert_eq!(e.code, openlab_core::protocol::ErrorCode::Io),
            other => panic!("{other:?}"),
        }
        // ... and A, which was holding broadcasts for it, now gets them.
        loop {
            if let ServerMessage::ModelChanged { revision, .. } = next(&mut a).await {
                assert_eq!(revision, first.revision + 1);
                break;
            }
        }
        // The next document request works (B's edit is unsaved, so it discards).
        send(
            &mut a,
            ClientMessage::Request {
                id: RequestId(3),
                request: Request::New {
                    discard_changes: true,
                },
            },
        )
        .await;
        assert!(matches!(result_of(&mut a, 3).await, Outcome::Ok(_)));
        let _ = snapshot_next(&mut b).await;
        shut_down(&mut a).await;
    })
    .await
    .expect("hang");
    let _ = release.send(());
    worker.join().unwrap();
    server.stop();
}

#[tokio::test]
async fn a_lost_snapshot_answer_does_not_blind_a_connection_for_good() {
    // The engine never gives up on the read here; the connection's own bound has to.
    let (server, worker, release, other, _dir) =
        stalled_reads(Duration::from_secs(3600), Duration::from_millis(300));
    tokio::time::timeout(Duration::from_secs(120), async {
        let (mut a, _) = connect(&server).await;
        let (mut b, _) = connect(&server).await;
        send(&mut a, open_request(1, &other)).await;
        // Once A's snapshot request is answered, its open has been handled (same connection,
        // same order), so B's edit below cannot get in front of it.
        send(
            &mut a,
            ClientMessage::Request {
                id: RequestId(9),
                request: Request::Snapshot,
            },
        )
        .await;
        assert!(matches!(result_of(&mut a, 9).await, Outcome::Ok(_)));
        send(&mut b, rename_command(2, "while stuck")).await;
        assert!(matches!(result_of(&mut b, 2).await, Outcome::Ok(_)));
        // A stops holding broadcasts and resyncs. It learns of B's edit either in the snapshot
        // or, if the edit came after the resync, as an ordinary change.
        let mut revision = 0;
        loop {
            match next(&mut a).await {
                ServerMessage::Snapshot { snapshot } => {
                    revision = snapshot.revision;
                    if snapshot.workspace.find_cue(id(1)).unwrap().name == "while stuck" {
                        break;
                    }
                }
                ServerMessage::ModelChanged {
                    revision: r,
                    upserts,
                    ..
                } => {
                    assert_eq!(r, revision + 1, "no gap after the resync");
                    revision = r;
                    let renamed = upserts.iter().any(|u| {
                        matches!(u, openlab_core::protocol::Upsert::Cue { cue, .. }
                            if cue.id == id(1) && cue.name == "while stuck")
                    });
                    if renamed {
                        break;
                    }
                }
                _ => {}
            }
        }
        // ... and then B's later edits as ordinary changes.
        send(&mut b, rename_command(3, "later")).await;
        loop {
            if let ServerMessage::ModelChanged { revision: r, .. } = next(&mut a).await {
                assert_eq!(r, revision + 1);
                break;
            }
        }
        shut_down(&mut b).await;
    })
    .await
    .expect("hang");
    let _ = release.send(());
    worker.join().unwrap();
    server.stop();
}
