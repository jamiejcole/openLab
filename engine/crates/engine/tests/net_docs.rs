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

        send(&mut a, ClientMessage::Goodbye { shutdown: true }).await;
    })
    .await
    .expect("hang: the session over the wire did not finish");
    worker.join().unwrap();
    server.stop();
}
