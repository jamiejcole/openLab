// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Several workspaces (docs) in one engine (WS-02, REQUIREMENTS §21: at most 8;
//! ARCHITECTURE §1, §3.1, §3.5): the docs share the mixer, the device, the probe pool and the
//! media watch, and nothing one doc does reaches another. Driven through the engine's test API
//! (`OfflineShow::open_doc`, `close_doc`, `transport_doc`, `panic_all`, `hotkey`); the
//! protocol's bound connections are in net_docs.rs.

use std::time::Duration;

use crate::show::device::{DeviceShow, listed};
use crate::show::*;
use openlab_core::protocol::{
    ClientMessage, ErrorCode, Outcome, Request, ServerMessage, TransportAction, WireProblem,
    WireWarning,
};
use openlab_core::{
    Command, ContinueMode, CueState, Hotkey, MachineSettings, Modifiers, NetworkPatch,
    NetworkPatchId, NetworkProtocol, PatchMapping, TransportKey, Workspace, WorkspaceId,
    WorkspaceSetting,
};
use openlab_engine::control::offline::OfflineShow;
use openlab_engine::control::{ControlConfig, DocId, DocSource, MAX_DOCS, PROBE_WORKERS};
use uuid::Uuid;

/// The workspace of doc number `n` (n = 0 is the first doc): its own id, the usual patch.
fn ws_n(n: u128, cues: Vec<openlab_core::Cue>) -> Workspace {
    let mut ws = workspace(cues);
    ws.id = WorkspaceId(Uuid::from_u128(100 + n));
    ws.name = format!("doc {n}");
    ws
}

/// Maps the usual patch of every workspace in `wss` to the offline device's two outputs.
fn map_all(c: &mut ControlConfig, wss: &[&Workspace]) {
    let mut machine = MachineSettings::default();
    for ws in wss {
        machine
            .set_patch_mapping(
                ws.id,
                patch_id(),
                PatchMapping {
                    device: "Offline".into(),
                    channels: vec![0, 1],
                },
            )
            .unwrap();
    }
    c.machine = machine;
}

/// A show whose first doc is `a`, then `others` opened beside it, every probe answered.
fn docs_show(a: Workspace, others: Vec<Workspace>) -> (OfflineShow, Vec<DocId>) {
    docs_show_with(a, others, |_| {})
}

fn docs_show_with(
    a: Workspace,
    others: Vec<Workspace>,
    tweak: impl FnOnce(&mut ControlConfig),
) -> (OfflineShow, Vec<DocId>) {
    let mut c = config(&a);
    let all: Vec<&Workspace> = std::iter::once(&a).chain(others.iter()).collect();
    map_all(&mut c, &all);
    tweak(&mut c);
    let mut s = show_with(a, c);
    let mut ids = s.control().doc_ids();
    for ws in others {
        ids.push(s.open_doc(DocSource::Workspace(Box::new(ws))).unwrap());
    }
    s.settle_media();
    s.take_outbound();
    (s, ids)
}

fn start_in(s: &mut OfflineShow, doc: DocId, n: u128) {
    s.transport_doc(doc, TransportAction::Start { cue: id(n) });
}

fn state(s: &OfflineShow, doc: DocId, n: u128) -> CueState {
    s.control().doc(doc).unwrap().engine().state(id(n))
}

fn result_error(msgs: &[ServerMessage]) -> Option<ErrorCode> {
    msgs.iter().find_map(|m| match m {
        ServerMessage::Result {
            outcome: Outcome::Error(e),
            ..
        } => Some(e.code),
        _ => None,
    })
}

fn notice_codes(warnings: &[WireWarning]) -> Vec<String> {
    warnings
        .iter()
        .filter_map(|w| match w {
            WireWarning::Notice { code, .. } => Some(code.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn two_docs_play_side_by_side_sample_exact() {
    let a = wav(&ramp(3000));
    let b = wav(&dc(2000, 0.5));
    let (mut s, ids) = docs_show(
        workspace(vec![audio(1, &a, 0)]),
        vec![ws_n(1, vec![audio(1, &b, 1)])],
    );
    let (da, db) = (ids[0], ids[1]);
    assert_ne!(da, db);
    s.run(BLOCK * 2);
    let at = s.now() as usize;
    start_in(&mut s, da, 1);
    start_in(&mut s, db, 1);
    s.run(4000);
    let (ch0, ch1) = (s.channel(0), s.channel(1));
    // Each doc plays through its own mixer slot: A's ramp on output 1, B's level on output 2,
    // both from the sample one block after the requests, exactly.
    assert_eq!(first_nonzero(&ch0), Some(at));
    assert_eq!(&ch0[at..at + 3000], ramp(3000).as_slice());
    assert_eq!(last_nonzero(&ch0), Some(at + 2999));
    assert_eq!(first_nonzero(&ch1), Some(at));
    assert!(ch1[at..at + 2000].iter().all(|&v| v == 0.5));
    assert_eq!(last_nonzero(&ch1), Some(at + 1999));
    assert_eq!(s.stalls(), 0);
    assert!(s.control().is_idle());
}

/// Panic, Stop All and Pause All are a doc's own (QLab 5: they act on one workspace; the
/// front window's, which is the UI's choice): the other doc plays on, sample for sample.
#[test]
fn panic_stop_all_and_pause_all_in_one_doc_leave_the_other_alone() {
    for action in [
        TransportAction::Panic,
        TransportAction::StopAll,
        TransportAction::PauseAll,
    ] {
        let a = wav(&dc(20_000, 0.25));
        let b = wav(&ramp(6000));
        let (mut s, ids) = docs_show(
            workspace(vec![audio(1, &a, 0)]),
            vec![ws_n(1, vec![audio(1, &b, 1)])],
        );
        let (da, db) = (ids[0], ids[1]);
        let at = s.now() as usize;
        start_in(&mut s, da, 1);
        start_in(&mut s, db, 1);
        s.run(BLOCK * 10);
        s.transport_doc(da, action.clone());
        s.run(2000);
        assert_eq!(state(&s, db, 1), CueState::Running, "{action:?}");
        assert_ne!(state(&s, da, 1), CueState::Running, "{action:?}");
        s.run(5000);
        let ch1 = s.channel(1);
        assert_eq!(&ch1[at..at + 6000], ramp(6000).as_slice(), "{action:?}");
        // A is silent by now (the panic fade is 0.1 s; a pause holds the voice silent).
        let ch0 = s.channel(0);
        let end = ch0.len();
        assert!(ch0[end - 1000..].iter().all(|&v| v == 0.0), "{action:?}");
    }
}

/// Esc is app-scope (QLab 5 check: it panics every workspace): both docs stop; twice within
/// the fade is a hard stop in each.
#[test]
fn an_app_scope_panic_stops_every_doc() {
    let a = wav(&dc(20_000, 0.25));
    let b = wav(&dc(20_000, 0.5));
    let (mut s, ids) = docs_show(
        workspace(vec![audio(1, &a, 0)]),
        vec![ws_n(1, vec![audio(1, &b, 1)])],
    );
    start_in(&mut s, ids[0], 1);
    start_in(&mut s, ids[1], 1);
    s.run(BLOCK * 4);
    s.panic_all();
    for doc in &ids {
        assert!(
            matches!(state(&s, *doc, 1), CueState::Fading | CueState::Stopping),
            "doc {doc}"
        );
    }
    s.panic_all();
    let cut = s.now() as usize;
    s.run(BLOCK * 4);
    for doc in &ids {
        assert_eq!(state(&s, *doc, 1), CueState::Idle);
    }
    assert!(s.channel(0)[cut..].iter().all(|&v| v == 0.0));
    assert!(s.channel(1)[cut..].iter().all(|&v| v == 0.0));
    assert!(s.control().is_idle());
}

/// QLab 5 check: a hotkey fires in every open workspace, in front or not.
#[test]
fn a_hotkey_fires_in_every_doc() {
    let key = Hotkey {
        key: "a".into(),
        modifiers: Modifiers::default(),
    };
    let a = wav(&dc(500, 0.25));
    let b = wav(&dc(500, 0.5));
    let mut ca = audio(1, &a, 0);
    ca.triggers.hotkey = Some(key.clone());
    let mut cb = audio(1, &b, 1);
    cb.triggers.hotkey = Some(key.clone());
    let (mut s, _) = docs_show(workspace(vec![ca]), vec![ws_n(1, vec![cb])]);
    s.run(BLOCK);
    let at = s.now() as usize;
    s.hotkey(&key);
    s.run(1000);
    assert_eq!(first_nonzero(&s.channel(0)), Some(at));
    assert_eq!(first_nonzero(&s.channel(1)), Some(at));
    // Another key starts nothing.
    s.hotkey(&Hotkey {
        key: "b".into(),
        modifiers: Modifiers::default(),
    });
    let later = s.now() as usize;
    s.run(1000);
    assert!(s.channel(0)[later..].iter().all(|&v| v == 0.0));
}

/// (TRG-01): the app-scope hotkey follows each doc's edited key map. A key edit that
/// takes a cue's hotkey is refused over the protocol, naming the cue; the key GO moves to starts
/// no cue, the key it left starts the cue that has it, and undo gives it back to GO. An edit in
/// one doc does not change what a key does in another.
#[test]
fn a_hotkey_follows_the_edited_key_map_of_each_doc() {
    let plain = |k: &str| Hotkey {
        key: k.into(),
        modifiers: Modifiers::default(),
    };
    let f = wav(&dc(20_000, 0.25));
    let keyed = |n, key: &str, patch| {
        let mut c = audio(n, &f, patch);
        c.triggers.hotkey = Some(plain(key));
        c
    };
    // Doc A: cue 1 on F2, cue 2 on Space (GO's key: it never fires, K1b). Doc B: cue 1 on F2.
    let (mut s, ids) = docs_show(
        workspace(vec![keyed(1, "f2", 0), keyed(2, "space", 0)]),
        vec![ws_n(1, vec![keyed(1, "f2", 1)])],
    );
    s.settle_media();
    let (a, b) = (ids[0], ids[1]);
    let set = |s: &mut OfflineShow, doc, key, value| {
        let rid = s.request_id();
        s.send_doc(
            doc,
            ClientMessage::Command {
                id: rid,
                command: Box::new(Command::SetWorkspaceSetting {
                    setting: WorkspaceSetting::Hotkey { key, value },
                }),
                coalesce: None,
            },
        )
    };
    // F2 is cue 1's in doc A: the key map refuses it, naming the cue.
    let replies = set(&mut s, a, TransportKey::Go, Some(plain("f2")));
    let refusal = replies
        .iter()
        .find_map(|m| match m {
            ServerMessage::Result {
                outcome: Outcome::Error(e),
                ..
            } => Some(e.clone()),
            _ => None,
        })
        .expect("refused");
    assert_eq!(refusal.code, ErrorCode::CommandRefused);
    assert!(
        refusal.message.contains("settings.hotkeys.go"),
        "{}",
        refusal.message
    );
    assert!(
        refusal.message.contains("hotkey of cue"),
        "{}",
        refusal.message
    );
    // GO moves to F9 in doc A: Space is now cue 2's, and F9 starts nothing.
    set(&mut s, a, TransportKey::Go, Some(plain("f9")));
    s.run(BLOCK);
    s.hotkey(&plain("f9"));
    s.run(BLOCK * 4);
    assert_eq!(state(&s, a, 1), CueState::Idle);
    assert_eq!(state(&s, a, 2), CueState::Idle);
    s.hotkey(&plain("space"));
    s.run(BLOCK * 4);
    assert_eq!(state(&s, a, 2), CueState::Running);
    // Stop Selected takes F3 in doc B; F2 still fires in both docs.
    set(&mut s, b, TransportKey::StopSelected, Some(plain("f3")));
    s.hotkey(&plain("f2"));
    s.run(BLOCK * 4);
    assert_eq!(state(&s, a, 1), CueState::Running);
    assert_eq!(state(&s, b, 1), CueState::Running);
    // Undo in doc A gives Space back to GO: it no longer starts cue 2 (already running, so it
    // is checked on a fresh press after a stop).
    s.transport_doc(a, TransportAction::StopAll);
    s.run(BLOCK * 8);
    let rid = s.request_id();
    s.send_doc(a, ClientMessage::Undo { id: rid });
    s.hotkey(&plain("space"));
    s.run(BLOCK * 4);
    assert_eq!(state(&s, a, 2), CueState::Idle);
}

/// The hotkey's release (the `hotkey` message with `phase: up`) reaches every doc too: a cue
/// whose second trigger is on release stops in each.
#[test]
fn a_hotkey_release_reaches_every_doc() {
    let key = Hotkey {
        key: "q".into(),
        modifiers: Modifiers::default(),
    };
    let f = wav(&dc(20_000, 0.25));
    let held = |patch| {
        let mut c = audio(1, &f, patch);
        c.triggers.hotkey = Some(key.clone());
        c.second_trigger = openlab_core::SecondTrigger::HardStop;
        c.second_trigger_on_release = true;
        c
    };
    let (mut s, ids) = docs_show(workspace(vec![held(0)]), vec![ws_n(1, vec![held(1)])]);
    s.settle_media();
    s.run(BLOCK);
    s.hotkey(&key);
    s.run(BLOCK * 4);
    for doc in &ids {
        assert_eq!(state(&s, *doc, 1), CueState::Running, "doc {doc}");
    }
    s.hotkey_up(&key);
    s.run(BLOCK * 4);
    for doc in &ids {
        assert_eq!(state(&s, *doc, 1), CueState::Idle, "doc {doc}");
    }
}

#[test]
fn voice_ids_are_unique_across_docs_and_their_owners_are_forgotten() {
    let f = wav(&dc(800, 0.25));
    let cues = || {
        vec![
            audio(1, &f, 0),
            audio(2, &f, 0),
            audio(3, &f, 1),
            audio(4, &f, 1),
        ]
    };
    let (mut s, ids) = docs_show(workspace(cues()), vec![ws_n(1, cues())]);
    for doc in &ids {
        for n in 1..=4 {
            start_in(&mut s, *doc, n);
        }
    }
    s.run(BLOCK * 2);
    // Eight voices of two docs in one mixer: an id taken twice would be refused
    // (`voice_rejected`).
    assert_eq!(s.control_mut().mixer_voice_count(), 8);
    assert_eq!(s.control().voice_owners(), 8);
    let msgs = s.take_outbound();
    assert!(
        !states_all(&msgs).contains(&"broken"),
        "a start was refused"
    );
    s.run(2000);
    assert!(s.control().is_idle());
    // The owner map follows the voices alive.
    assert_eq!(s.control().voice_owners(), 0);
}

fn states_all(msgs: &[ServerMessage]) -> Vec<&'static str> {
    changes(msgs)
        .into_iter()
        .filter_map(|c| match c {
            openlab_core::protocol::RuntimeChange::CueState { state, .. } => Some(match state {
                openlab_core::protocol::WireCueState::Broken => "broken",
                _ => "other",
            }),
            _ => None,
        })
        .collect()
}

/// Mixer events go to the doc that owns the voice and to no other; a doc closed while the
/// mixer still holds events of its voices (the control loop was late) has them dropped, and
/// the other doc is untouched.
#[test]
fn a_late_event_of_a_closed_doc_is_ignored() {
    let short = wav(&dc(300, 0.25));
    let long = wav(&ramp(6000));
    let (mut s, ids) = docs_show(
        workspace(vec![audio(1, &short, 0)]),
        vec![ws_n(1, vec![audio(1, &long, 1)])],
    );
    let (da, db) = (ids[0], ids[1]);
    let at = s.now() as usize;
    start_in(&mut s, da, 1);
    start_in(&mut s, db, 1);
    s.run(BLOCK * 8);
    // A's voice ended: its event went to A alone (B's long voice has had none).
    assert_eq!(state(&s, da, 1), CueState::Idle);
    assert!(s.control().doc(da).unwrap().events_routed() >= 1);
    assert_eq!(s.control().doc(db).unwrap().events_routed(), 0);
    // Again, and this time A's voice ends in the mixer while its event waits, unread.
    start_in(&mut s, da, 1);
    s.render_only(BLOCK * 8);
    s.close_doc(da, false).unwrap();
    assert!(s.control().doc(da).is_none());
    // A's waiting events are drained now: none of them reaches B.
    s.run(BLOCK);
    assert_eq!(s.control().doc(db).unwrap().events_routed(), 0);
    s.run(7000);
    assert_eq!(state(&s, db, 1), CueState::Idle);
    assert_eq!(&s.channel(1)[at..at + 6000], ramp(6000).as_slice());
    assert_eq!(s.control().voice_owners(), 0);
    assert!(s.control().is_idle());
}

/// Closing one show must never glitch another: B's ramp has no missing or repeated sample.
#[test]
fn closing_a_doc_while_another_plays_leaves_no_gap() {
    let a = wav(&dc(20_000, 0.25));
    let b = wav(&ramp(8000));
    let (mut s, ids) = docs_show(
        workspace(vec![audio(1, &a, 0)]),
        vec![ws_n(1, vec![audio(1, &b, 1)])],
    );
    let (da, db) = (ids[0], ids[1]);
    let at = s.now() as usize;
    start_in(&mut s, da, 1);
    start_in(&mut s, db, 1);
    s.run(3000);
    let cut = s.now() as usize;
    s.close_doc(da, false).unwrap();
    s.run(6000);
    assert_eq!(&s.channel(1)[at..at + 8000], ramp(8000).as_slice());
    assert_eq!(last_nonzero(&s.channel(1)), Some(at + 7999));
    // A stopped at once (Panic twice: a hard stop), from the next sample the mixer renders.
    assert!(s.channel(0)[cut..].iter().all(|&v| v == 0.0));
    assert_eq!(s.control().doc_ids(), vec![db]);
}

#[test]
fn at_most_eight_docs_are_open_and_ids_are_never_reused() {
    let (mut s, mut ids) = docs_show(workspace(vec![]), vec![]);
    for n in 1..MAX_DOCS as u128 {
        ids.push(
            s.open_doc(DocSource::Workspace(Box::new(ws_n(n, vec![]))))
                .unwrap(),
        );
    }
    assert_eq!(s.control().doc_ids().len(), MAX_DOCS);
    let refused = s.open_doc(DocSource::New).unwrap_err();
    assert_eq!(refused.code, ErrorCode::CommandRefused);
    assert_eq!(s.control().doc_ids(), ids);
    // Ids rise in open order and a closed doc's id is not handed out again.
    assert!(ids.windows(2).all(|w| w[0] < w[1]));
    let gone = ids[3];
    s.close_doc(gone, false).unwrap();
    let fresh = s.open_doc(DocSource::New).unwrap();
    assert!(ids.iter().all(|&d| d < fresh));
    // The closed id names nothing: a message for it answers `no_workspace`.
    let rid = s.request_id();
    let replies = s.send_doc(
        gone,
        ClientMessage::Request {
            id: rid,
            request: Request::Snapshot,
        },
    );
    assert_eq!(result_error(&replies), Some(ErrorCode::NoWorkspace));
    assert_eq!(
        s.close_doc(gone, true).unwrap_err().code,
        ErrorCode::NoWorkspace
    );
}

/// Eight docs share one probe pool of two workers (ARCHITECTURE §3.1).
#[test]
fn eight_docs_share_one_pool_of_two_probe_workers() {
    let files: Vec<_> = (0..MAX_DOCS).map(|_| wav(&dc(100, 0.5))).collect();
    let others = (1..MAX_DOCS)
        .map(|n| ws_n(n as u128, vec![audio(1, &files[n], 0)]))
        .collect();
    let (s, ids) = docs_show(workspace(vec![audio(1, &files[0], 0)]), others);
    assert_eq!(ids.len(), MAX_DOCS);
    assert_eq!(s.control().probe_workers(), PROBE_WORKERS);
    assert_eq!(PROBE_WORKERS, 2);
    // Every doc's file was probed by the shared pool and answered to its own doc (docs past
    // the mixer's two slots are broken for want of a slot, never for their media).
    for doc in ids {
        let view = s.control().doc(doc).unwrap();
        assert_eq!(view.probes_pending(), 0);
        let problems: Vec<_> = view
            .snapshot()
            .problems
            .into_iter()
            .flat_map(|p| p.problems)
            .collect();
        assert!(
            problems.iter().all(|p| !matches!(
                p,
                WireProblem::NoMedia
                    | WireProblem::MediaMissing { .. }
                    | WireProblem::MediaUnsupported { .. }
            )),
            "doc {doc}: {problems:?}"
        );
    }
}

/// A file two docs use is watched once; one doc letting go (closing) leaves the other's watch;
/// a change is reported to the doc that still watches it.
#[test]
fn the_media_watch_is_shared_with_counts() {
    let shared = wav(&dc(400, 0.5));
    let own = wav(&dc(400, 0.5));
    let (mut s, ids) = docs_show(
        workspace(vec![audio(1, &shared, 0), audio(2, &own, 0)]),
        vec![ws_n(1, vec![audio(1, &shared, 1)])],
    );
    let (da, db) = (ids[0], ids[1]);
    assert_eq!(s.control().watched_files(), 2);
    s.watch_pass();
    s.close_doc(da, false).unwrap();
    s.tick();
    assert_eq!(
        s.control().watched_files(),
        1,
        "B still watches the shared file"
    );
    std::fs::remove_file(&shared).unwrap();
    s.watch_pass();
    assert_eq!(state(&s, db, 1), CueState::Broken);
    s.close_doc(db, false).unwrap();
    s.tick();
    assert_eq!(s.control().watched_files(), 0);
}

/// PB-10 is one flag for the engine: set through any doc, every doc's Network cues send
/// nothing; and it is the same flag whichever doc asks to turn it off.
#[test]
fn the_network_output_override_applies_to_every_doc() {
    let net = NetworkPatch {
        id: NetworkPatchId(Uuid::from_u128(1)),
        name: "Desk".into(),
        host: "127.0.0.1".into(),
        port: 9,
        protocol: NetworkProtocol::Udp,
    };
    let osc = || {
        cue(
            1,
            openlab_core::CueKind::Network {
                destination: NetworkPatchId(Uuid::from_u128(1)),
                address: "/go".into(),
                args: vec![],
            },
        )
    };
    let mut a = workspace(vec![osc()]);
    a.network_patches = vec![net.clone()];
    let mut b = ws_n(1, vec![osc()]);
    b.network_patches = vec![net];
    let (mut s, ids) = docs_show(a, vec![b]);
    let (da, db) = (ids[0], ids[1]);
    let rid = s.request_id();
    s.send_doc(
        db,
        ClientMessage::Request {
            id: rid,
            request: Request::SetOutputOverrides {
                network_disabled: true,
            },
        },
    );
    assert!(s.control().network_output_disabled());
    for doc in [da, db] {
        start_in(&mut s, doc, 1);
    }
    s.run(BLOCK * 4);
    s.settle_net();
    let logs: Vec<String> = s
        .control()
        .show_log()
        .tail_seq(200)
        .0
        .into_iter()
        .map(|e| e.message)
        .filter(|m| m.contains("network output is disabled"))
        .collect();
    assert_eq!(logs.len(), 2, "{logs:?}");
    let rid = s.request_id();
    s.send_doc(
        da,
        ClientMessage::Request {
            id: rid,
            request: Request::SetOutputOverrides {
                network_disabled: false,
            },
        },
    );
    assert!(!s.control().network_output_disabled());
}

/// Device loss stops every doc (one device for the engine; the policy is in
/// `control_devices.rs`), and the device's return makes every doc playable again.
#[test]
fn device_loss_stops_every_doc() {
    const DEV: &str = "fake:1";
    const DEV_NAME: &str = "Fake 1";
    let a_ws = workspace(vec![audio(1, &wav(&dc(40_000, 0.25)), 0)]);
    let b_ws = ws_n(1, vec![audio(1, &wav(&dc(40_000, 0.5)), 1)]);
    let mut c = ControlConfig::new(RATE, BLOCK);
    c.seed = 7;
    c.watch.mode = openlab_engine::control::watch::WatchMode::Manual;
    c.mixer_patches = 2;
    let mut machine = MachineSettings::default();
    for ws in [&a_ws, &b_ws] {
        machine
            .set_patch_mapping(
                ws.id,
                patch_id(),
                PatchMapping {
                    device: DEV_NAME.into(),
                    channels: vec![0, 1],
                },
            )
            .unwrap();
    }
    c.machine = machine;
    let mut s = DeviceShow::new(a_ws, vec![listed(DEV, DEV_NAME, 2)], c);
    let da = s.control().doc_ids()[0];
    let db = s.open_doc(b_ws);
    s.transport_doc(da, TransportAction::Start { cue: id(1) });
    s.transport_doc(db, TransportAction::Start { cue: id(1) });
    s.run(BLOCK * 4);
    assert_eq!(s.control_mut().mixer_voice_count(), 2);
    s.take_outbound();
    s.fake.unplug(DEV, true);
    s.settle_devices();
    assert!(s.control().device_down());
    for doc in [da, db] {
        let engine = s.control().doc(doc).unwrap().engine();
        assert_eq!(
            engine.broken_reason(id(1)),
            Some(openlab_core::BrokenReason::DeviceLost),
            "doc {doc}"
        );
    }
    let notices: Vec<String> = warnings(&s.take_outbound())
        .into_iter()
        .filter_map(|w| match w {
            WireWarning::Notice { code, message, .. } if code == "device_lost" => Some(message),
            _ => None,
        })
        .collect();
    // One notice per doc, each with its own count.
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert!(
        notices.iter().all(|m| m.contains("1 cue(s)")),
        "{notices:?}"
    );
    s.fake.plug(listed(DEV, DEV_NAME, 2));
    s.wait_wall(Duration::from_secs(1), 2);
    assert!(!s.control().device_down());
    s.run(BLOCK * 2);
    assert_eq!(s.control().live_voices(), 0);
    assert_eq!(s.control_mut().mixer_voice_count(), 0);
    for doc in [da, db] {
        assert_eq!(
            s.control().doc(doc).unwrap().engine().state(id(1)),
            CueState::Idle
        );
    }
}

#[test]
fn closing_a_doc_frees_its_mixer_slots_for_one_waiting() {
    let f = wav(&dc(20_000, 0.25));
    // Two mixer slots: A and B take them, C waits.
    let (mut s, ids) = docs_show(
        workspace(vec![audio(1, &f, 0)]),
        vec![
            ws_n(1, vec![audio(1, &f, 1)]),
            ws_n(2, vec![audio(1, &f, 0)]),
        ],
    );
    let (da, db, dc_) = (ids[0], ids[1], ids[2]);
    assert_eq!(s.control().free_mixer_patches(), 0);
    let view = s.control().doc(dc_).unwrap();
    assert!(notice_codes(view.warnings()).contains(&"mixer_patches_exhausted".to_owned()));
    assert_eq!(view.engine().state(id(1)), CueState::Broken);
    start_in(&mut s, db, 1);
    s.run(BLOCK * 4);
    assert_eq!(s.control_mut().mixer_voice_count(), 1);
    s.close_doc(db, false).unwrap();
    s.run(BLOCK * 2);
    // B's voice was stopped (Panic twice) and its slot handed to C.
    assert_eq!(s.control_mut().mixer_voice_count(), 0);
    assert_eq!(s.control().free_mixer_patches(), 0);
    let view = s.control().doc(dc_).unwrap();
    assert!(!notice_codes(view.warnings()).contains(&"mixer_patches_exhausted".to_owned()));
    assert_ne!(view.engine().state(id(1)), CueState::Broken);
    let at = s.now() as usize;
    start_in(&mut s, dc_, 1);
    s.run(BLOCK * 4);
    assert_eq!(first_nonzero(&s.channel(0)[at..]), Some(0));
    // A, untouched.
    assert_eq!(state(&s, da, 1), CueState::Idle);
}

/// Opening a big show beside one that plays: the install is cheap on the control thread and
/// the playing doc's auto-follow chain keeps its exact joins, with no late start.
#[test]
fn opening_two_thousand_cues_during_an_auto_follow_chain_keeps_exact_joins() {
    let parts: Vec<_> = (0..4)
        .map(|k| wav(&dc(600, 0.1 * (k + 1) as f32)))
        .collect();
    let chain: Vec<_> = parts
        .iter()
        .enumerate()
        .map(|(k, p)| {
            let mut c = audio(k as u128 + 1, p, 1);
            if k + 1 < parts.len() {
                c.continue_mode = ContinueMode::AutoFollow;
            }
            c
        })
        .collect();
    let (mut s, ids) = docs_show(workspace(chain), vec![]);
    let at = s.now() as usize;
    start_in(&mut s, ids[0], 1);
    s.run(800);
    let big_media = wav(&dc(100, 0.5));
    let big: Vec<_> = (0..2000)
        .map(|n| audio(1000 + n as u128, &big_media, 0))
        .collect();
    let mut big_ws = ws_n(1, big);
    big_ws.cue_lists[0].name = "big".into();
    let db = s.open_doc(DocSource::Workspace(Box::new(big_ws))).unwrap();
    s.run(3000);
    let ch1 = s.channel(1);
    for k in 0..4 {
        let from = at + 600 * k;
        let want = 0.1 * (k + 1) as f32;
        assert!(
            ch1[from..from + 600].iter().all(|&v| v == want),
            "part {k} is not exact"
        );
    }
    assert_eq!(last_nonzero(&ch1), Some(at + 2399));
    let msgs = s.take_outbound();
    assert!(
        !warnings(&msgs)
            .iter()
            .any(|w| matches!(w, WireWarning::LateStart { .. }))
    );
    assert_eq!(
        s.control().doc(db).unwrap().workspace().cue_lists[0]
            .cues
            .len(),
        2000
    );
}

/// A seeded random operator over two docs (and a third opened and closed): any mix of
/// transport actions, opens and closes keeps the engine sound, and an app-scope panic leaves
/// nothing running, no voice, owner or stream behind.
#[test]
fn a_random_operator_over_several_docs_leaves_nothing_behind() {
    for seed in [1u64, 7, 42, 1234] {
        let f = wav(&dc(900, 0.25));
        let g = wav(&ramp(1500));
        let cues = || {
            let mut c1 = audio(1, &f, 0);
            c1.continue_mode = ContinueMode::AutoFollow;
            vec![c1, audio(2, &g, 1), audio(3, &f, 0)]
        };
        let (mut s, mut ids) = docs_show(workspace(cues()), vec![ws_n(1, cues())]);
        let mut rng = seed;
        let mut next = move |n: u64| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng % n
        };
        for _ in 0..120 {
            let doc = ids[next(ids.len() as u64) as usize];
            let n = u128::from(next(3)) + 1;
            match next(10) {
                0 => s.transport_doc(doc, TransportAction::Panic),
                1 => s.transport_doc(doc, TransportAction::StopAll),
                2 => s.transport_doc(doc, TransportAction::PauseAll),
                3 => s.transport_doc(doc, TransportAction::ResumeAll),
                4 => s.transport_doc(doc, TransportAction::Go { list: list_id() }),
                5 if ids.len() < 3 => {
                    let ws = ws_n(10 + ids.len() as u128, cues());
                    ids.push(s.open_doc(DocSource::Workspace(Box::new(ws))).unwrap());
                    s.settle_media();
                    Vec::new()
                }
                6 if ids.len() > 1 => {
                    s.close_doc(doc, true).unwrap();
                    ids.retain(|d| *d != doc);
                    Vec::new()
                }
                _ => s.transport_doc(doc, TransportAction::Start { cue: id(n) }),
            };
            s.run(BLOCK * (1 + next(6)));
        }
        s.panic_all();
        s.panic_all();
        // Past a meter read (30 Hz), which forgets the owners of voices gone.
        s.run(BLOCK * 8);
        assert!(s.control().is_idle(), "seed {seed}");
        assert_eq!(s.control().voice_owners(), 0, "seed {seed}");
        assert_eq!(s.control_mut().mixer_voice_count(), 0, "seed {seed}");
        assert_eq!(s.control().held_streams(), primed(&s, &ids), "seed {seed}");
        assert_eq!(s.stalls(), 0, "seed {seed}");
        for doc in &ids {
            for n in 1..=3 {
                assert!(
                    matches!(state(&s, *doc, n), CueState::Idle | CueState::Loaded),
                    "seed {seed} doc {doc} cue {n}"
                );
            }
        }
    }
}

/// Streams held while idle are the primes only.
fn primed(s: &OfflineShow, ids: &[DocId]) -> usize {
    ids.iter()
        .map(|d| s.control().doc(*d).unwrap().primed().len())
        .sum()
}

/// A doc built off the control thread walked its slice-plan checks there, before any file was
/// probed: the checks follow its files' answers as the first doc's do.
#[test]
fn a_doc_built_off_the_control_thread_checks_its_slices_as_its_files_answer() {
    let path = wav(&dc(300, 0.5));
    let mut bad = audio(2, &path, 0);
    if let openlab_core::CueKind::Audio { start_time, .. } = &mut bad.kind {
        // Past the 300-frame file's end: a plan that cannot play.
        *start_time = 1.0;
    }
    let (mut s, ids) = docs_show(
        workspace(vec![]),
        vec![ws_n(1, vec![audio(1, &path, 0), bad])],
    );
    s.run(BLOCK);
    let doc = s.control().doc(ids[1]).unwrap();
    assert!(doc.engine().problems(id(1)).is_empty());
    assert!(matches!(
        doc.engine().problems(id(2)),
        [openlab_core::CueProblem::SlicesUnplayable { .. }]
    ));
}

/// Answers of the shared workers for a doc closed since (its probes, here) are dropped: they
/// never reach another doc, whose own answers still come.
#[test]
fn a_closed_docs_late_probe_answers_are_dropped() {
    let a = wav(&dc(400, 0.25));
    let (mut s, ids) = docs_show(workspace(vec![audio(1, &a, 0)]), vec![]);
    let files: Vec<_> = (0..20).map(|_| wav(&dc(100, 0.5))).collect();
    let cues = files
        .iter()
        .enumerate()
        .map(|(n, f)| audio(n as u128 + 1, f, 0))
        .collect();
    let b = s
        .open_doc(DocSource::Workspace(Box::new(ws_n(1, cues))))
        .unwrap();
    // Closed while its twenty probes are still on the workers.
    s.close_doc(b, false).unwrap();
    let late = wav(&dc(100, 0.5));
    let c = s
        .open_doc(DocSource::Workspace(Box::new(ws_n(
            2,
            vec![audio(1, &late, 1)],
        ))))
        .unwrap();
    s.settle_media();
    s.run(BLOCK * 2);
    assert_eq!(s.control().doc_ids(), vec![ids[0], c]);
    assert_eq!(s.control().probes_pending(), 0);
    assert_ne!(state(&s, c, 1), CueState::Broken);
    let at = s.now() as usize;
    start_in(&mut s, c, 1);
    s.run(BLOCK * 4);
    assert_eq!(first_nonzero(&s.channel(1)[at..]), Some(0));
}

/// A file named with `marker`, holding `frames` frames of `value` (gated probes pick files out
/// by name).
fn marked_wav(marker: &str, frames: usize, value: f32) -> std::path::PathBuf {
    let p = wav(&dc(frames, value));
    let q = p.with_file_name(format!(
        "{marker}-{}",
        p.file_name().unwrap().to_string_lossy()
    ));
    std::fs::rename(&p, &q).unwrap();
    q
}

/// A closed doc's jobs still waiting in the shared probe queues are dropped, never run: closing
/// a big show opened by mistake does not keep both workers busy on its files.
#[test]
fn a_closed_docs_queued_probes_are_never_run() {
    let files: Vec<_> = (0..20).map(|_| marked_wav("m1gate", 100, 0.5)).collect();
    gate(&files[0]);
    gate(&files[1]);
    let (mut s, _) = docs_show_with(workspace(vec![]), vec![], |c| {
        c.probe = gated_probe;
        c.probe_clock = ProbeTime::new().clock();
    });
    let cues = files
        .iter()
        .enumerate()
        .map(|(n, f)| audio(n as u128 + 1, f, 0))
        .collect();
    let b = s
        .open_doc(DocSource::Workspace(Box::new(ws_n(1, cues))))
        .unwrap();
    // Both workers hold one of B's files; the other 18 wait in the queues.
    wait_probed(&files, 2);
    s.close_doc(b, false).unwrap();
    release(&files[0]);
    release(&files[1]);
    // A doc opened after them is probed once the queues ahead of it have been taken.
    let late = wav(&dc(100, 0.5));
    let c = s
        .open_doc(DocSource::Workspace(Box::new(ws_n(
            2,
            vec![audio(1, &late, 1)],
        ))))
        .unwrap();
    s.settle_media();
    assert_eq!(s.control().doc(c).unwrap().probes_pending(), 0);
    assert_eq!(
        probe_order(&files).len(),
        2,
        "a closed doc's queued probes ran"
    );
}

/// Which volume a path is on, for the two-doc stall test: files named `m2dead` are on a share.
fn m2_volume(path: &std::path::Path) -> String {
    if path.to_string_lossy().contains("m2dead") {
        "/Volumes/M2Dead".to_owned()
    } else {
        "/".to_owned()
    }
}

/// One doc's show volume hanging cannot take the shared workers from another doc:
/// with several docs open the show's own volume is held back like any other once two of its
/// files are stuck, so the other doc's probes still run.
#[test]
fn a_hung_show_volume_of_one_doc_leaves_workers_for_the_others() {
    // The share is a folder of its own, the show file in it too.
    let share = fresh_path("m2dead");
    std::fs::create_dir_all(&share).unwrap();
    let dead: Vec<_> = (0..6)
        .map(|n| {
            let to = share.join(format!("{n}.wav"));
            std::fs::rename(wav(&dc(100, 0.5)), &to).unwrap();
            to
        })
        .collect();
    for f in &dead {
        gate(f);
    }
    let time = ProbeTime::new();
    let cues = dead
        .iter()
        .enumerate()
        .map(|(n, f)| audio(n as u128 + 1, f, 0))
        .collect();
    let a = workspace(cues);
    let mut c = config(&a);
    c.probe = gated_probe;
    c.probe_clock = time.clock();
    c.probe_volume = Some(m2_volume);
    // Doc A's show file is on the hung share too.
    c.show_path = Some(share.join("show.openlab"));
    let mut s = OfflineShow::new(a, mixer_config(), c).unwrap();
    wait_probed(&dead, 2);
    // Doc B: one local file, far from its playhead (no urgent copy of its probe).
    let local = wav(&dc(100, 0.5));
    let mut cues: Vec<_> = (1..=19)
        .map(|n| cue(n, openlab_core::CueKind::Memo))
        .collect();
    cues.push(audio(20, &local, 1));
    let b = s
        .open_doc(DocSource::Workspace(Box::new(ws_n(1, cues))))
        .unwrap();
    // A's two stuck probes are given up on: two files stuck on one volume hold it back, so the
    // fresh workers try one of A's files and leave a worker for B.
    time.advance(Duration::from_secs(31));
    s.tick();
    crate::show::spin::spin_until(
        "doc B's probe to run: A's hung show volume took every worker",
        || {
            s.tick();
            s.control().doc(b).unwrap().probes_pending() == 0
        },
    );
    assert_ne!(state(&s, b, 20), CueState::Broken);
    for f in &dead {
        release(f);
    }
}

/// Primes are a head start only: the docs' primes together never take the stream permits a
/// start needs. With 4 permits, two docs prime one cue each and a start of a cue
/// nobody primed still plays.
#[test]
fn other_docs_primes_never_refuse_a_start() {
    let f = wav(&dc(400, 0.25));
    let cues = || vec![audio(1, &f, 0), audio(2, &f, 0), audio(3, &f, 0)];
    let (mut s, ids) = docs_show_with(workspace(cues()), vec![ws_n(1, cues())], |c| {
        c.streamer.max_streams = 4;
        c.prime_cues = 3;
    });
    s.run(BLOCK * 2);
    for doc in &ids {
        assert!(s.control().doc(*doc).unwrap().primed().len() <= 1);
    }
    let at = s.now() as usize;
    start_in(&mut s, ids[0], 3);
    s.run(BLOCK * 4);
    assert_ne!(state(&s, ids[0], 3), CueState::Broken);
    assert_eq!(first_nonzero(&s.channel(0)[at..]), Some(0));
    let msgs = s.take_outbound();
    assert!(!format!("{msgs:?}").contains("stream_failed"));
}

/// The preload budget (AUD-05) is the engine's, not each doc's: a second doc's
/// preload that does not fit what the first holds streams, and a closed doc gives its share
/// back.
#[test]
fn the_preload_budget_is_shared_by_every_doc() {
    let preloaded = |n: u128, path: &std::path::Path| {
        let mut c = audio(n, path, 0);
        if let openlab_core::CueKind::Audio { preload, .. } = &mut c.kind {
            *preload = true;
        }
        c
    };
    // 1,000 mono frames are 4,000 bytes preloaded: the budget holds one such file.
    let fa = wav(&dc(1000, 0.25));
    let fb = wav(&dc(1000, 0.25));
    let fc = wav(&dc(1000, 0.25));
    let (mut s, ids) = docs_show_with(
        workspace(vec![preloaded(1, &fa)]),
        vec![ws_n(1, vec![preloaded(1, &fb)])],
        |c| c.preload_budget = 4000,
    );
    s.run(BLOCK);
    let over = |s: &OfflineShow, doc| {
        notice_codes(s.control().doc(doc).unwrap().warnings())
            .contains(&"preload_over_budget".to_owned())
    };
    assert!(!over(&s, ids[0]));
    assert!(
        over(&s, ids[1]),
        "the second doc's preload fitted a budget already taken"
    );
    s.close_doc(ids[0], false).unwrap();
    let c = s
        .open_doc(DocSource::Workspace(Box::new(ws_n(
            2,
            vec![preloaded(1, &fc)],
        ))))
        .unwrap();
    s.settle_media();
    s.run(BLOCK);
    assert!(!over(&s, c), "the closed doc's share did not come back");
}

/// A doc's mixer slot takes its gains at once when the doc opens: a GO on the sample it is
/// installed is heard at its level, not faded in over the de-zipper.
#[test]
fn a_cue_started_as_its_doc_opens_is_not_faded_in() {
    let f = wav(&dc(800, 0.25));
    let (mut s, _) = docs_show(workspace(vec![]), vec![]);
    s.run(BLOCK * 4);
    let b = s
        .open_doc(DocSource::Workspace(Box::new(ws_n(
            1,
            vec![audio(1, &f, 1)],
        ))))
        .unwrap();
    s.settle_media();
    let at = s.now() as usize;
    start_in(&mut s, b, 1);
    s.run(900);
    let ch1 = s.channel(1);
    assert_eq!(first_nonzero(&ch1), Some(at));
    assert!(
        ch1[at..at + 400].iter().all(|&v| v == 0.25),
        "{:?}",
        &ch1[at..at + 8]
    );
}

/// With several docs open, a doc's show-log lines name its workspace; alone, they read as
/// before.
#[test]
fn log_lines_name_their_doc_while_several_are_open() {
    let f = wav(&dc(200, 0.25));
    let (mut s, ids) = docs_show(workspace(vec![audio(1, &f, 0)]), vec![]);
    start_in(&mut s, ids[0], 1);
    let lines = |s: &OfflineShow| -> Vec<String> {
        s.control()
            .show_log()
            .tail_seq(500)
            .0
            .into_iter()
            .map(|e| e.message)
            .filter(|m| m.contains("cue 1 \"cue 1\" started"))
            .collect()
    };
    assert_eq!(lines(&s), vec!["cue 1 \"cue 1\" started".to_owned()]);
    let b = s
        .open_doc(DocSource::Workspace(Box::new(ws_n(
            1,
            vec![audio(1, &f, 1)],
        ))))
        .unwrap();
    s.settle_media();
    start_in(&mut s, b, 1);
    assert_eq!(
        lines(&s).last().map(String::as_str),
        Some("[doc 1] cue 1 \"cue 1\" started")
    );
}

/// The PB-10 override is app-scope: it is set even with no doc open.
#[test]
fn the_output_override_needs_no_open_doc() {
    let (mut s, ids) = docs_show(workspace(vec![]), vec![]);
    s.close_doc(ids[0], false).unwrap();
    let rid = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id: rid,
        request: Request::SetOutputOverrides {
            network_disabled: true,
        },
    });
    assert_eq!(result_error(&replies), None, "{replies:?}");
    assert!(s.control().network_output_disabled());
}

/// The prime pool is shared by demand (review M-C): an idle second doc opening beside a busy
/// show takes none of its primes, and nothing re-primes on the open or the close.
#[test]
fn an_idle_second_doc_takes_no_primes_from_a_busy_show() {
    let key = Hotkey {
        key: "f".into(),
        modifiers: Modifiers::default(),
    };
    let f = wav(&dc(400, 0.25));
    // 60 hotkey cues: all primed (ARCHITECTURE §3.5), within the 64 of the pool.
    let cues = (1..=60)
        .map(|n| {
            let mut c = audio(n, &f, 0);
            c.triggers.hotkey = Some(key.clone());
            c
        })
        .collect();
    let (mut s, ids) = docs_show(workspace(cues), vec![]);
    s.run(BLOCK * 2);
    let primed = |s: &OfflineShow| s.control().doc(ids[0]).unwrap().primed().len();
    assert_eq!(primed(&s), 60);
    let opened_before = s.control().open_streams();
    let b = s
        .open_doc(DocSource::Workspace(Box::new(ws_n(1, vec![]))))
        .unwrap();
    s.settle_media();
    s.run(BLOCK * 8);
    assert_eq!(primed(&s), 60, "an empty doc halved the show's primes");
    s.close_doc(b, false).unwrap();
    s.run(BLOCK * 8);
    assert_eq!(primed(&s), 60);
    assert_eq!(s.control().open_streams(), opened_before, "streams churned");
}

/// Under real contention each doc keeps at least its fair share of the prime pool (half the
/// stream permits), and a start of a cue nobody primed still finds a permit (review M-C).
#[test]
fn contended_primes_are_shared_fairly_and_never_refuse_a_start() {
    let f = wav(&dc(400, 0.25));
    let cues = || (1..=8).map(|n| audio(n, &f, 0)).collect::<Vec<_>>();
    let (mut s, ids) = docs_show_with(workspace(cues()), vec![], |c| {
        c.streamer.max_streams = 8;
        c.prime_cues = 8;
    });
    s.run(BLOCK * 2);
    let primed = |s: &OfflineShow, d| s.control().doc(d).unwrap().primed().len();
    // Alone it has the whole pool: half of 8 permits.
    assert_eq!(primed(&s, ids[0]), 4);
    let b = s
        .open_doc(DocSource::Workspace(Box::new(ws_n(1, cues()))))
        .unwrap();
    s.settle_media();
    // Each fills a few rings per pass; give the shares time to settle.
    s.run(BLOCK * 16);
    assert_eq!(primed(&s, ids[0]), 2);
    assert_eq!(primed(&s, b), 2);
    // Both docs start a cue nobody primed: each finds a permit.
    let at = s.now() as usize;
    start_in(&mut s, ids[0], 8);
    start_in(&mut s, b, 8);
    s.run(BLOCK * 4);
    assert_ne!(state(&s, ids[0], 8), CueState::Broken);
    assert_ne!(state(&s, b, 8), CueState::Broken);
    assert_eq!(first_nonzero(&s.channel(0)[at..]), Some(0));
    assert!(!format!("{:?}", s.take_outbound()).contains("stream_failed"));
}

/// A copy of an open show (its workspace id) opens with a fresh id, and the doc's cue engine is
/// told: no model change goes unsignalled (debug builds' edit cross-check would count one).
#[test]
fn a_copy_of_an_open_show_gets_a_fresh_id_signalled_to_its_engine() {
    let a = ws_n(0, vec![cue(1, openlab_core::CueKind::Memo)]);
    let (mut s, ids) = docs_show(a.clone(), vec![]);
    let copy = s
        .open_doc(DocSource::Workspace(Box::new(a.clone())))
        .unwrap();
    s.settle_media();
    start_in(&mut s, copy, 1);
    s.run(100);
    let doc = s.control().doc(copy).unwrap();
    assert_ne!(doc.workspace().id, a.id);
    assert_eq!(doc.engine().unsignalled_model_changes(), 0);
    // Two signalled follow-ups, in every build: the opened workspace's (on the document
    // worker) and the new id's.
    assert_eq!(doc.engine().model_syncs(), 2);
    let first = s.control().doc(ids[0]).unwrap();
    assert_eq!(first.engine().unsignalled_model_changes(), 0);
}
