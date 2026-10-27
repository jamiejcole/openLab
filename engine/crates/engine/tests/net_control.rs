// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The WebSocket server in front of the real control loop (`runner::run`, no audio device):
//! the wire order of a command's `model_changed` and `result`, the snapshot in `welcome`, and
//! `goodbye { shutdown }` ending the loop.

mod show;

use std::thread;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use openlab_audio::Mixer;
use openlab_core::protocol::{
    ClientMessage, Outcome, PROTOCOL_VERSION, RequestId, ServerMessage, TransportAction,
    decode_server, encode_client,
};
use openlab_core::{Command, CueKind, CueProperty, GroupMode};
use openlab_engine::control::{Backend, Control, OUTBOUND_CAPACITY, outbound_channel, runner};
use openlab_engine::net::{ServerConfig, WsServer};
use show::{config, cue, id, list_id, mixer_config, workspace};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

const GUARD: Duration = Duration::from_secs(20);

#[tokio::test]
async fn commands_and_transport_over_the_wire() {
    let ws = workspace(vec![cue(
        1,
        CueKind::Group {
            mode: GroupMode::Timeline,
            children: vec![],
        },
    )]);
    let (ctl, _mixer) = Mixer::new(mixer_config()).unwrap();
    let (inbox_tx, inbox) = crossbeam_channel::unbounded();
    let (out_tx, outbound) = outbound_channel(OUTBOUND_CAPACITY);
    let control = Control::new(
        ws.clone(),
        ctl,
        Backend::Offline,
        config(&ws),
        inbox_tx.clone(),
        out_tx,
    )
    .unwrap();
    let worker = thread::spawn(move || runner::run(control, &inbox));
    let server = WsServer::start(inbox_tx, outbound, ServerConfig::default()).unwrap();

    // One guard over the whole exchange: the loops below skip broadcasts and pings, so a
    // per-message timeout would not end a wait for a message that never comes.
    tokio::time::timeout(Duration::from_secs(120), async {
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
        let next = async |c: &mut tokio_tungstenite::WebSocketStream<_>| loop {
            match tokio::time::timeout(GUARD, c.next()).await.expect("hang") {
                Some(Ok(Message::Text(t))) => return Some(decode_server(t.as_str()).unwrap()),
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                _ => return None,
            }
        };
        let revision = match next(&mut c).await {
            Some(ServerMessage::Welcome { snapshot, .. }) => {
                assert_eq!(snapshot.workspace, ws);
                snapshot.revision
            }
            other => panic!("{other:?}"),
        };

        for n in 1..=20u64 {
            let rename = ClientMessage::Command {
                id: RequestId(n),
                command: Box::new(Command::SetCueProperty {
                    id: id(1),
                    property: CueProperty::Name {
                        value: format!("name {n}"),
                    },
                }),
            };
            c.send(Message::text(encode_client(&rename).unwrap()))
                .await
                .unwrap();
            // model_changed (revision + n), maybe a document message, then the result.
            let mut changed = false;
            loop {
                match next(&mut c).await.expect("closed") {
                    ServerMessage::ModelChanged { revision: r, .. } => {
                        assert_eq!(r, revision + n);
                        changed = true;
                    }
                    ServerMessage::Result {
                        id: rid, outcome, ..
                    } if rid.0 == n => {
                        assert!(changed, "result {n} before its model_changed");
                        assert!(
                            matches!(outcome, Outcome::Ok(ok) if ok.revision == Some(revision + n))
                        );
                        break;
                    }
                    _ => {}
                }
            }
        }

        let go = ClientMessage::Transport {
            id: RequestId(100),
            action: TransportAction::Go { list: list_id() },
        };
        c.send(Message::text(encode_client(&go).unwrap()))
            .await
            .unwrap();
        loop {
            if let ServerMessage::Result { id: rid, .. } = next(&mut c).await.expect("closed")
                && rid.0 == 100
            {
                break;
            }
        }

        // goodbye { shutdown } reaches the control loop, which ends.
        c.send(Message::text(
            encode_client(&ClientMessage::Goodbye { shutdown: true }).unwrap(),
        ))
        .await
        .unwrap();
        while next(&mut c).await.is_some() {}
    })
    .await
    .expect("hang: the session over the wire did not finish");
    worker.join().unwrap();
    server.stop();
}
