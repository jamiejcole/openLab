// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The `relink` request (M1.17; WS-03, NFR-04): the folder is walked on the document worker, the
//! matches are applied as one undoable edit, and a scan limit is reported as `truncated`.

mod common;
mod show;

use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use common::{env, touch};
use openlab_core::protocol::{
    ClientMessage, ErrorCode, OkBody, Outcome, Request, RequestId, ResultData, ServerMessage,
};
use openlab_core::{Command, CueProperty};
use openlab_engine::control::offline::OfflineShow;
use openlab_engine::control::{DocHook, Inbox, ManualClock, reply_channel};
use openlab_engine::persist::ScanLimits;
use show::*;

const T0: i64 = 1_700_000_000_000;

struct Rig {
    s: OfflineShow,
    lib: std::path::PathBuf,
    clock: ManualClock,
    _env: common::Env,
}

/// Cues 1..=n each point at `/gone/<name>`; the library folder starts empty.
fn rig_with(
    names: &[&str],
    tweak: impl FnOnce(&mut openlab_engine::control::ControlConfig),
) -> Rig {
    let env = env();
    let ws = workspace(
        names
            .iter()
            .enumerate()
            .map(|(i, n)| audio(i as u128 + 1, &Path::new("/gone").join(n), 0))
            .collect(),
    );
    let clock = ManualClock::new(T0);
    let mut c = config(&ws);
    c.app_dirs = Some(env.dirs.clone());
    c.clock = clock.clock();
    tweak(&mut c);
    let lib = env.root.path().join("lib");
    fs::create_dir_all(&lib).unwrap();
    Rig {
        s: show_with(ws, c),
        lib,
        clock,
        _env: env,
    }
}

fn rig(names: &[&str]) -> Rig {
    rig_with(names, |_| {})
}

fn relink(r: &mut Rig) -> Vec<ServerMessage> {
    let id = r.s.request_id();
    let folder = r.lib.to_string_lossy().into_owned();
    r.s.send(ClientMessage::Request {
        id,
        request: Request::Relink { folder },
    })
}

fn ok_body(msgs: &[ServerMessage]) -> &OkBody {
    msgs.iter()
        .find_map(|m| match m {
            ServerMessage::Result {
                outcome: Outcome::Ok(ok),
                ..
            } => Some(ok),
            ServerMessage::Result {
                outcome: Outcome::Error(e),
                ..
            } => panic!("{e:?}"),
            _ => None,
        })
        .expect("no result")
}

fn error_code(msgs: &[ServerMessage]) -> ErrorCode {
    msgs.iter()
        .find_map(|m| match m {
            ServerMessage::Result {
                outcome: Outcome::Error(e),
                ..
            } => Some(e.code),
            _ => None,
        })
        .expect("no error")
}

/// (relinked, ambiguous, unmatched, truncated, skipped)
fn counts(msgs: &[ServerMessage]) -> (u64, usize, usize, bool, u64) {
    match ok_body(msgs).data.clone() {
        Some(ResultData::Relink {
            relinked,
            ambiguous,
            unmatched,
            truncated,
            skipped,
        }) => (
            relinked,
            ambiguous.len(),
            unmatched.len(),
            truncated,
            skipped,
        ),
        other => panic!("{other:?}"),
    }
}

fn media(s: &OfflineShow, n: u128) -> String {
    openlab_core::audio_media(s.control().workspace())
        .into_iter()
        .find(|(c, _)| *c == id(n))
        .map(|(_, m)| m.as_str().to_owned())
        .unwrap()
}

#[test]
fn relink_finds_moved_files_in_one_undo_step_and_undo_restores() {
    let mut r = rig(&["a.wav", "b.wav", "c.wav"]);
    touch(&r.lib.join("a.wav"));
    touch(&r.lib.join("sub/deep/B.WAV"));
    let rev = r.s.control().snapshot().revision;
    r.s.take_outbound();
    let replies = relink(&mut r);
    assert_eq!(counts(&replies), (2, 0, 1, false, 0));
    assert_eq!(ok_body(&replies).revision, Some(rev + 1));
    assert!(media(&r.s, 1).ends_with("a.wav") && !media(&r.s, 1).starts_with("/gone"));
    assert!(media(&r.s, 2).to_lowercase().ends_with("sub/deep/b.wav"));
    assert_eq!(media(&r.s, 3), "/gone/c.wav");
    assert!(
        r.s.take_outbound()
            .iter()
            .any(|m| matches!(m, ServerMessage::ModelChanged { .. }))
    );
    // One undo step puts both back.
    let id = r.s.request_id();
    r.s.send(ClientMessage::Undo { id });
    assert_eq!(media(&r.s, 1), "/gone/a.wav");
    assert_eq!(media(&r.s, 2), "/gone/b.wav");
}

#[test]
fn duplicates_are_reported_ambiguous_and_not_guessed() {
    let mut r = rig(&["a.wav"]);
    touch(&r.lib.join("x/a.wav"));
    touch(&r.lib.join("y/a.wav"));
    let rev = r.s.control().snapshot().revision;
    let replies = relink(&mut r);
    assert_eq!(counts(&replies), (0, 1, 0, false, 0));
    assert_eq!(media(&r.s, 1), "/gone/a.wav");
    // Nothing changed: no new revision, no undo step.
    assert_eq!(ok_body(&replies).revision, Some(rev));
    assert!(!r.s.control().snapshot().document.can_undo);
}

#[test]
fn each_scan_limit_is_reported_as_truncated() {
    type Tweak = fn(&mut ScanLimits);
    let cases: [(&str, Tweak); 4] = [
        ("depth", |l| l.max_depth = 1),
        ("files", |l| l.max_files = 2),
        ("folders", |l| l.max_dirs = 2),
        ("time", |l| l.max_duration = std::time::Duration::ZERO),
    ];
    for (name, tweak) in cases {
        let mut r = rig_with(&["a.wav"], |c| tweak(&mut c.relink_limits));
        for f in ["f1", "f2", "f3"] {
            touch(&r.lib.join(f));
        }
        touch(&r.lib.join("s1/s2/s3/s4/a.wav"));
        for d in ["d1", "d2", "d3"] {
            fs::create_dir_all(r.lib.join(d)).unwrap();
        }
        let replies = relink(&mut r);
        let (relinked, _, unmatched, truncated, _) = counts(&replies);
        assert!(truncated, "{name}");
        // The file past the limit was not considered, so it is reported as not found.
        assert_eq!((relinked, unmatched), (0, 1), "{name}");
    }
}

#[test]
fn a_complete_scan_is_not_truncated() {
    let mut r = rig(&["a.wav"]);
    touch(&r.lib.join("a.wav"));
    assert!(!counts(&relink(&mut r)).3);
}

#[test]
fn a_missing_folder_or_a_file_is_refused_without_changes() {
    let mut r = rig(&["a.wav"]);
    let gone = r.lib.join("nope");
    let id1 = r.s.request_id();
    let replies = r.s.send(ClientMessage::Request {
        id: id1,
        request: Request::Relink {
            folder: gone.to_string_lossy().into_owned(),
        },
    });
    assert_eq!(error_code(&replies), ErrorCode::NotFound);
    touch(&r.lib.join("file"));
    let id2 = r.s.request_id();
    let replies = r.s.send(ClientMessage::Request {
        id: id2,
        request: Request::Relink {
            folder: r.lib.join("file").to_string_lossy().into_owned(),
        },
    });
    assert_eq!(error_code(&replies), ErrorCode::BadRequest);
    assert_eq!(media(&r.s, 1), "/gone/a.wav");
    // The worker is free again.
    touch(&r.lib.join("a.wav"));
    assert_eq!(counts(&relink(&mut r)).0, 1);
}

/// A hook that holds the worker's first job until released.
fn hold_first_job() -> (DocHook, mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (entered_tx, entered) = mpsc::channel::<()>();
    let (release_tx, release) = mpsc::channel::<()>();
    let entered_tx = Mutex::new(entered_tx);
    let release = Mutex::new(release);
    let first = std::sync::atomic::AtomicBool::new(true);
    let hook = DocHook(Arc::new(move || {
        if first.swap(false, std::sync::atomic::Ordering::SeqCst) {
            let _ = entered_tx.lock().unwrap().send(());
            let _ = release.lock().unwrap().recv();
        }
    }));
    (hook, entered, release_tx)
}

fn deliver(r: &mut Rig, request: Request) -> (RequestId, openlab_engine::control::ReplyRx) {
    let id = r.s.request_id();
    let (tx, rx) = reply_channel();
    r.s.deliver(Inbox::Client {
        message: ClientMessage::Request { id, request },
        reply: tx,
    });
    (id, rx)
}

#[test]
fn a_slow_folder_does_not_block_the_control_thread_and_a_stalled_walk_times_out() {
    let (hook, entered, release) = hold_first_job();
    let mut r = rig_with(&["a.wav"], |c| {
        c.doc_hook = Some(hook);
        c.doc_timeout = std::time::Duration::from_secs(20);
        c.relink_limits.max_duration = std::time::Duration::from_secs(30);
    });
    touch(&r.lib.join("a.wav"));
    let folder = r.lib.to_string_lossy().into_owned();
    let (_, mut rx) = deliver(&mut r, Request::Relink { folder });
    entered.recv().unwrap();
    // The worker is stuck in the folder; the control thread still edits and plays.
    let cid = r.s.request_id();
    let (tx, mut edit) = reply_channel();
    r.s.deliver(Inbox::Client {
        message: ClientMessage::Command {
            id: cid,
            command: Box::new(Command::SetCueProperty {
                id: id(1),
                property: CueProperty::Name { value: "x".into() },
            }),
            coalesce: None,
        },
        reply: tx,
    });
    assert!(matches!(
        edit.try_recv().unwrap(),
        ServerMessage::Result {
            outcome: Outcome::Ok(_),
            ..
        }
    ));
    r.s.run(BLOCK * 4);
    assert!(rx.try_recv().is_err(), "not answered yet");
    // The walk's own 30 s budget is outwaited by the stall check, not cut at 20 s.
    r.clock.advance(25_000);
    r.s.tick();
    assert!(rx.try_recv().is_err());
    // Past budget plus timeout, a walk that never returns fails with io and frees the queue.
    r.clock.advance(26_000);
    r.s.tick();
    assert!(matches!(
        rx.try_recv().unwrap(),
        ServerMessage::Result { outcome: Outcome::Error(ref e), .. } if e.code == ErrorCode::Io
    ));
    release.send(()).unwrap();
}

#[test]
fn a_walk_that_returns_applies_even_when_slow() {
    let (hook, entered, release) = hold_first_job();
    let mut r = rig_with(&["a.wav"], |c| c.doc_hook = Some(hook));
    touch(&r.lib.join("a.wav"));
    let folder = r.lib.to_string_lossy().into_owned();
    let (_, mut rx) = deliver(&mut r, Request::Relink { folder });
    entered.recv().unwrap();
    // Another document request is refused while the worker is busy.
    let (_, mut other) = deliver(&mut r, Request::ScanRecovery);
    assert!(matches!(
        other.try_recv().unwrap(),
        ServerMessage::Result { outcome: Outcome::Error(ref e), .. } if e.code == ErrorCode::CommandRefused
    ));
    release.send(()).unwrap();
    while rx.try_recv().is_err() {
        r.s.tick();
        std::thread::yield_now();
    }
    assert!(!media(&r.s, 1).starts_with("/gone"));
}

#[test]
fn an_edit_made_during_the_walk_is_not_overwritten() {
    let (hook, entered, release) = hold_first_job();
    let mut r = rig_with(&["a.wav", "b.wav"], |c| c.doc_hook = Some(hook));
    touch(&r.lib.join("a.wav"));
    touch(&r.lib.join("b.wav"));
    let folder = r.lib.to_string_lossy().into_owned();
    let (_, mut rx) = deliver(&mut r, Request::Relink { folder });
    entered.recv().unwrap();
    // The operator points cue 1 elsewhere while the folder is walked.
    let cid = r.s.request_id();
    let mut kind =
        r.s.control()
            .workspace()
            .find_cue(id(1))
            .unwrap()
            .kind
            .clone();
    if let openlab_core::CueKind::Audio { media, .. } = &mut kind {
        *media = Some(openlab_core::MediaRef::new("/operator/chose.wav"));
    }
    let (tx, mut edit) = reply_channel();
    r.s.deliver(Inbox::Client {
        message: ClientMessage::Command {
            id: cid,
            command: Box::new(Command::SetCueProperty {
                id: id(1),
                property: CueProperty::Kind { value: kind },
            }),
            coalesce: None,
        },
        reply: tx,
    });
    assert!(edit.try_recv().is_ok());
    release.send(()).unwrap();
    let mut got = None;
    while got.is_none() {
        r.s.tick();
        got = rx.try_recv().ok();
        std::thread::yield_now();
    }
    assert_eq!(media(&r.s, 1), "/operator/chose.wav");
    assert!(!media(&r.s, 2).starts_with("/gone"));
    assert_eq!(counts(&[got.unwrap()]).0, 1);
}

#[test]
fn a_doc_closed_during_the_walk_answers_no_workspace() {
    let (hook, entered, release) = hold_first_job();
    let mut r = rig_with(&["a.wav"], |c| c.doc_hook = Some(hook));
    let folder = r.lib.to_string_lossy().into_owned();
    let (_, mut rx) = deliver(&mut r, Request::Relink { folder });
    entered.recv().unwrap();
    let doc = r.s.control().doc_ids()[0];
    r.s.control_mut().close_doc(doc, true).unwrap();
    release.send(()).unwrap();
    let mut got = None;
    while got.is_none() {
        r.s.tick();
        got = rx.try_recv().ok();
        std::thread::yield_now();
    }
    assert_eq!(error_code(&[got.unwrap()]), ErrorCode::NoWorkspace);
}
