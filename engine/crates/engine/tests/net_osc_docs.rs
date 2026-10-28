// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! OSC over several open docs, in front of the real control loop (M4.15h, ARCHITECTURE section
//! 7): one listener, a copy per doc, an un-prefixed message to every doc with one reply each,
//! `/workspace/{id|name}` to one, `/connect` per doc, updates per doc, a closed doc's sessions
//! gone, and with no doc open nothing answered from a closed doc's copy (issue #152).

mod show;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::thread;
use std::time::Duration;

use openlab_audio::Mixer;
use openlab_core::protocol::{ClientMessage, DocId, Outcome, Request, RequestId, ServerMessage};
use openlab_core::{
    Command, CueKind, CueProperty, GroupMode, OscAccess, OscPasscode, OscRights, Workspace,
    save_show,
};
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
}

fn request(inbox: &crossbeam_channel::Sender<Inbox>, n: u64, request: Request) -> Outcome {
    let (tx, mut rx) = reply_channel();
    inbox
        .send(Inbox::Conn {
            doc: None,
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

/// Sends `message` to doc `doc` as a connection bound to it would, and waits for its result.
async fn to_doc(inbox: &crossbeam_channel::Sender<Inbox>, doc: DocId, message: ClientMessage) {
    let (tx, mut rx) = reply_channel();
    inbox
        .send(Inbox::Conn {
            doc: Some(doc),
            message,
            reply: tx,
        })
        .unwrap();
    loop {
        match guard("a result", rx.recv()).await {
            Some(ServerMessage::Result { outcome, .. }) => {
                assert!(matches!(outcome, Outcome::Ok(_)), "{outcome:?}");
                return;
            }
            Some(_) => {}
            None => panic!("the control loop hung up"),
        }
    }
}

fn rename(id: openlab_core::CueId, name: &str) -> ClientMessage {
    ClientMessage::Command {
        id: RequestId(50),
        command: Box::new(Command::SetCueProperty {
            id,
            property: CueProperty::Name { value: name.into() },
        }),
        coalesce: None,
    }
}

impl Osc {
    /// The replies `m` gets, and the update messages that came in before them: `/version` is
    /// sent after it and the actor handles datagrams in order, so its reply marks the end.
    async fn ask(&self, m: &OscMessage) -> (Vec<Value>, Vec<String>) {
        self.send(m).await;
        self.send(&msg("/version", vec![])).await;
        let mut replies = Vec::new();
        let mut updates = Vec::new();
        loop {
            let m = self.recv().await;
            if m.addr == "/reply/version" {
                return (replies, updates);
            }
            if m.addr.starts_with("/reply/") {
                let [OscType::String(s)] = m.args.as_slice() else {
                    panic!("{m:?}")
                };
                replies.push(serde_json::from_str(s).unwrap());
            } else {
                updates.push(m.addr);
            }
        }
    }

    async fn replies(&self, addr: &str, args: Vec<OscType>) -> Vec<Value> {
        self.ask(&msg(addr, args)).await.0
    }

    /// Asks `addr` until the replies, as (status, data) pairs, are `want`: what the docs say
    /// reaches the OSC side through broadcasts, a moment after the request's `result`.
    async fn until(&self, addr: &str, want: &[(&str, Value)]) {
        guard(&format!("{addr} to answer {want:?}"), async {
            loop {
                let got: Vec<(String, Value)> = self
                    .replies(addr, vec![])
                    .await
                    .into_iter()
                    .map(|r| {
                        (
                            r["status"].as_str().unwrap_or_default().to_owned(),
                            r.get("data").cloned().unwrap_or(Value::Null),
                        )
                    })
                    .collect();
                let matches = got.len() == want.len()
                    && got.iter().zip(want).all(|(g, w)| g.0 == w.0 && g.1 == w.1);
                if matches {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
    }
}

fn upper_ws(ws: &Workspace) -> String {
    ws.id.0.to_string().to_uppercase()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn osc_serves_every_open_doc() {
    let dir = tempfile::tempdir().unwrap();
    // The first show is open to everyone: cue 5 "A" then cue 6 "B".
    let mut ws = workspace(vec![group(1, "5", "A"), group(2, "6", "B")]);
    ws.name = "first".into();
    let first = dir.path().join("first.openlab");
    write_show(&first, &ws);
    // The other has a passcode, and its own cue 5.
    let mut other = workspace(vec![group(11, "5", "Q5")]);
    other.id = openlab_core::WorkspaceId(uuid::Uuid::from_u128(77));
    other.name = "other".into();
    other.settings.osc_access = OscAccess {
        no_passcode: OscRights::NONE,
        passcodes: vec![OscPasscode {
            passcode: "2222".into(),
            rights: OscRights::ALL,
        }],
    };
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
                // The test polls with the same read again and again.
                duplicate_window: Duration::ZERO,
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
    let doc_a = DocId(0);
    osc.until("/cue/5/name", &[("ok", "A".into())]).await;

    // Open the other show beside it: an un-prefixed read is answered by both, the other one
    // refusing (it has a passcode).
    let outcome = tokio::task::block_in_place(|| {
        request(
            &inbox_tx,
            3,
            Request::Open {
                path: other_path.to_string_lossy().into_owned(),
                recovery: None,
            },
        )
    });
    assert!(matches!(outcome, Outcome::Ok(_)), "{outcome:?}");
    let doc_b = DocId(1);
    osc.until(
        "/cue/5/name",
        &[("ok", "A".into()), ("denied", Value::Null)],
    )
    .await;
    let replies = osc.replies("/cue/5/name", vec![]).await;
    assert_eq!(replies[0]["workspace_id"], upper_ws(&ws));
    assert_eq!(replies[1]["workspace_id"], upper_ws(&other));

    // `/workspaces` lists both, in open order.
    let list = osc.replies("/workspaces", vec![]).await;
    let names: Vec<&str> = list[0]["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["displayName"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["first", "other"]);

    // Subscribed before connecting to the other: its edits send nothing (no View there), the
    // first's do.
    assert!(
        osc.replies("/updates", vec![OscType::Int(1)])
            .await
            .is_empty()
    );
    to_doc(&inbox_tx, doc_b, rename(id(11), "Q5 renamed")).await;
    to_doc(&inbox_tx, doc_a, rename(id(1), "A renamed")).await;
    let a_update = format!("/update/workspace/{}/cue_id/{}", upper_ws(&ws), upper_id(1));
    guard("the first doc's update", async {
        let mut buf = vec![0u8; 70_000];
        loop {
            let (n, _) = osc.socket.recv_from(&mut buf).await.unwrap();
            let m = decode_packet(&buf[..n]).unwrap().remove(0);
            assert!(
                !m.addr.contains(&upper_ws(&other)),
                "an update of a doc the peer may not view: {}",
                m.addr
            );
            if m.addr == a_update {
                return;
            }
        }
    })
    .await;
    // A prefixed `/updates` is an error.
    let prefixed = format!("/workspace/{}/updates", upper_ws(&ws));
    let replies = osc.replies(&prefixed, vec![OscType::Int(1)]).await;
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["status"], "error");

    // `/connect` to the other, by its id: only it answers, and the first is untouched.
    let replies = osc
        .replies(
            &format!("/workspace/{}/connect", upper_ws(&other)),
            vec![OscType::String("2222".into())],
        )
        .await;
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["data"], "ok:view|edit|control");
    osc.until(
        "/cue/5/name",
        &[("ok", "A renamed".into()), ("ok", "Q5 renamed".into())],
    )
    .await;

    // The prefix by name picks one: GO in the first only.
    let ids = |r: &[Value]| -> Vec<Value> { r.iter().map(|r| r["data"].clone()).collect() };
    osc.until(
        "/playheadID",
        &[("ok", upper_id(1).into()), ("ok", upper_id(11).into())],
    )
    .await;
    assert!(osc.replies("/workspace/first/go", vec![]).await.is_empty());
    osc.until(
        "/playheadID",
        &[("ok", upper_id(2).into()), ("ok", upper_id(11).into())],
    )
    .await;
    // Un-prefixed, GO fires both.
    assert!(osc.replies("/go", vec![]).await.is_empty());
    osc.until(
        "/playheadID",
        &[("ok", "none".into()), ("ok", "none".into())],
    )
    .await;
    assert_eq!(ids(&osc.replies("/playheadID", vec![]).await).len(), 2);

    // An id no doc has: one error that says nothing else.
    let replies = osc.replies("/workspace/NOPE/go", vec![]).await;
    assert_eq!(
        replies,
        vec![serde_json::json!({"status": "error", "address": "/workspace/NOPE/go"})]
    );

    // Closing the other ends its sessions: opened again it is locked once more.
    let close = |doc| ClientMessage::Close {
        id: RequestId(60),
        doc: Some(doc),
        discard_changes: true,
    };
    to_doc(&inbox_tx, doc_b, close(doc_b)).await;
    osc.until("/cue/5/name", &[("ok", "A renamed".into())])
        .await;
    let outcome = tokio::task::block_in_place(|| {
        request(
            &inbox_tx,
            4,
            Request::Open {
                path: other_path.to_string_lossy().into_owned(),
                recovery: None,
            },
        )
    });
    assert!(matches!(outcome, Outcome::Ok(_)), "{outcome:?}");
    osc.until(
        "/cue/5/name",
        &[("ok", "A renamed".into()), ("denied", Value::Null)],
    )
    .await;

    // With no doc open nothing is answered from a closed doc's copy (#152): a workspace
    // message errors as for an unknown id, and the list is empty.
    to_doc(&inbox_tx, doc_a, close(doc_a)).await;
    to_doc(&inbox_tx, DocId(2), close(DocId(2))).await;
    guard("no doc to answer", async {
        loop {
            let replies = osc.replies("/cue/5/name", vec![]).await;
            if replies == vec![serde_json::json!({"status": "error", "address": "/cue/5/name"})] {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    let list = osc.replies("/workspaces", vec![]).await;
    assert_eq!(list[0]["data"], serde_json::json!([]));
    assert_eq!(
        osc.replies(&format!("/cue_id/{}/name", upper_id(1)), vec![])
            .await[0]["status"],
        "error"
    );

    let _ = inbox_tx.send(Inbox::Shutdown);
    worker.join().unwrap();
    server.stop();
}

fn upper_id(n: u128) -> String {
    id(n).0.to_string().to_uppercase()
}
