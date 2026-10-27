// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The OSC workspace copy across `revert` and `open`, in front of the real control loop: a
//! replacement is announced by the broadcast `snapshot` alone (no `model_changed`, no `document`,
//! the same path after a revert), so the OSC side has to install it. Otherwise `/cue/5/start`
//! after a `revert` that discarded a renumber would start the cue that used to be 5.

mod show;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::thread;
use std::time::Duration;

use openlab_audio::Mixer;
use openlab_core::protocol::{ClientMessage, Outcome, Request, RequestId, ServerMessage};
use openlab_core::{Command, CueKind, GroupMode, NumberAssignment, Workspace, save_show};
use openlab_engine::control::{
    Backend, Control, Inbox, OUTBOUND_CAPACITY, outbound_channel, reply_channel, runner,
};
use openlab_engine::net::{OscConfig, ServerConfig, WsServer};
use openlab_engine::osc::{decode_packet, encode_message};
use rosc::{OscMessage, OscType};
use serde_json::Value;
use show::{config, cue, id, workspace};
use tokio::net::UdpSocket;

const GUARD: Duration = Duration::from_secs(20);

async fn guard<F: std::future::Future>(what: &str, f: F) -> F::Output {
    match tokio::time::timeout(GUARD, f).await {
        Ok(out) => out,
        Err(_) => panic!("hang: {what} took longer than {GUARD:?}"),
    }
}

fn group(n: u128, number: &str, name: &str) -> openlab_core::Cue {
    let mut c = cue(
        n,
        CueKind::Group {
            mode: GroupMode::Timeline,
            children: vec![],
        },
    );
    c.number = Some(number.into());
    c.name = name.into();
    c
}

fn write_show(path: &Path, ws: &Workspace) {
    std::fs::write(path, save_show(ws).unwrap()).unwrap();
}

fn msg(addr: &str, args: Vec<OscType>) -> OscMessage {
    OscMessage {
        addr: addr.into(),
        args,
    }
}

struct Osc {
    socket: UdpSocket,
    target: SocketAddr,
}

impl Osc {
    async fn send(&self, m: &OscMessage) {
        self.socket
            .send_to(&encode_message(m).unwrap(), self.target)
            .await
            .unwrap();
    }

    async fn recv(&self) -> OscMessage {
        let mut buf = vec![0u8; 70_000];
        let (n, _) = guard("a UDP message", self.socket.recv_from(&mut buf))
            .await
            .unwrap();
        decode_packet(&buf[..n]).unwrap().remove(0)
    }

    /// What `address` answers with (update messages in between are skipped).
    async fn read(&self, address: &str) -> Value {
        self.send(&msg(address, vec![])).await;
        loop {
            let m = self.recv().await;
            if m.addr.starts_with("/reply/") {
                let [OscType::String(s)] = m.args.as_slice() else {
                    panic!("{m:?}")
                };
                return serde_json::from_str(s).unwrap();
            }
        }
    }

    /// Asks `address` until its `data` is `want` (the replacement reaches the OSC side through
    /// the broadcast, a moment after the request's `result`).
    async fn until_name(&self, address: &str, want: &str) {
        guard(&format!("{address} to name {want}"), async {
            loop {
                let reply = self.read(address).await;
                if reply["status"] == "ok" && reply["data"] == want {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
    }
}

fn request(inbox: &crossbeam_channel::Sender<Inbox>, n: u64, request: Request) -> Outcome {
    let (tx, mut rx) = reply_channel();
    inbox
        .send(Inbox::Client {
            message: ClientMessage::Request {
                id: RequestId(n),
                request,
            },
            reply: tx,
        })
        .unwrap();
    loop {
        if let ServerMessage::Result { outcome, .. } =
            rx.blocking_recv().expect("the control loop answered")
        {
            return outcome;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn osc_follows_a_revert_and_an_open() {
    let dir = tempfile::tempdir().unwrap();
    // Cue A is number 5, cue B number 6.
    let mut ws = workspace(vec![group(1, "5", "A"), group(2, "6", "B")]);
    ws.name = "first".into();
    let first = dir.path().join("first.openlab");
    write_show(&first, &ws);
    // Another show: its cue 5 is named differently and has other ids.
    let mut other = workspace(vec![group(11, "5", "Q5")]);
    other.id = openlab_core::WorkspaceId(uuid::Uuid::from_u128(77));
    other.name = "other".into();
    let other_path = dir.path().join("other.openlab");
    write_show(&other_path, &other);

    let (ctl, _mixer) = Mixer::new(show::mixer_config()).unwrap();
    let (inbox_tx, inbox) = crossbeam_channel::unbounded();
    let (out_tx, outbound) = outbound_channel(OUTBOUND_CAPACITY);
    let mut c = config(&ws);
    c.app_dirs = Some(openlab_engine::persist::AppDirs::new(
        dir.path().join("support"),
    ));
    c.show_path = Some(first.clone());
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
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let server = WsServer::start(
        inbox_tx.clone(),
        outbound,
        ServerConfig {
            osc: Some(OscConfig {
                bind: Ipv4Addr::LOCALHOST.into(),
                port: 0,
                reply_port: socket.local_addr().unwrap().port(),
                ..OscConfig::default()
            }),
            ..ServerConfig::default()
        },
    )
    .unwrap();
    let osc = Osc {
        socket,
        target: SocketAddr::from((Ipv4Addr::LOCALHOST, server.osc_ports().unwrap().udp)),
    };

    osc.until_name("/cue/5/name", "A").await;

    // Renumber: A becomes 7 and B becomes 5. The OSC copy follows the `model_changed`.
    let (tx, mut rx) = reply_channel();
    inbox_tx
        .send(Inbox::Client {
            message: ClientMessage::Command {
                id: RequestId(1),
                command: Box::new(Command::SetCueNumbers {
                    numbers: vec![
                        NumberAssignment {
                            id: id(1),
                            number: Some("7".into()),
                        },
                        NumberAssignment {
                            id: id(2),
                            number: Some("5".into()),
                        },
                    ],
                }),
            },
            reply: tx,
        })
        .unwrap();
    assert!(matches!(
        rx.recv().await,
        Some(ServerMessage::ModelChanged { .. } | ServerMessage::Document { .. })
            | Some(ServerMessage::Result { .. })
    ));
    osc.until_name("/cue/5/name", "B").await;

    // Revert drops the renumber. The cue ids and the path are the same, no `model_changed` is
    // sent: only the snapshot says the numbers went back, and `/cue/5` is A again.
    let outcome = tokio::task::block_in_place(|| {
        request(
            &inbox_tx,
            2,
            Request::Revert {
                discard_changes: true,
            },
        )
    });
    assert!(matches!(outcome, Outcome::Ok(_)), "{outcome:?}");
    osc.until_name("/cue/5/name", "A").await;
    // And it starts A, not B.
    osc.send(&msg("/updates", vec![OscType::Int(1)])).await;
    osc.send(&msg("/cue/5/start", vec![])).await;
    let ws_id = ws.id.to_string();
    let first_update = guard("a cue state update", async {
        loop {
            let m = osc.recv().await;
            if m.addr
                .starts_with(&format!("/update/workspace/{ws_id}/cue_id/"))
            {
                return m.addr;
            }
        }
    })
    .await;
    assert_eq!(
        first_update,
        format!("/update/workspace/{ws_id}/cue_id/{}", id(1)),
        "the cue that is number 5 after the revert"
    );

    // Open another show: OSC cue commands resolve against it, and the old ids are gone.
    let outcome = tokio::task::block_in_place(|| {
        request(
            &inbox_tx,
            3,
            Request::Open {
                path: other_path.to_string_lossy().into_owned(),
                discard_changes: true,
                recovery: None,
            },
        )
    });
    assert!(matches!(outcome, Outcome::Ok(_)), "{outcome:?}");
    osc.until_name("/cue/5/name", "Q5").await;
    let old = osc.read(&format!("/cue_id/{}/name", id(1))).await;
    assert_eq!(old["status"], "error", "{old}");

    let _ = inbox_tx.send(Inbox::Shutdown);
    worker.join().unwrap();
    server.stop();
}
