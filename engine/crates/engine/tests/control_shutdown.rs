// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The engine starts with no doc and ends with a final recovery write of every doc with unsaved
//! changes (M4.15g, M4.4d, M4.4e; ARCHITECTURE §8, NFR-05), on the offline driver with a
//! virtual wall clock and the persistence layer on a temp folder.

mod common;
mod show;

use std::fs;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Env, env, names};
use openlab_core::protocol::{
    ClientMessage, ErrorCode, LogLevel, OkBody, Outcome, Request, ResultData, ServerMessage,
};
use openlab_core::{Command, CueKind, CueProperty, Workspace, save_show};
use openlab_engine::control::offline::OfflineShow;
use openlab_engine::control::{ControlConfig, DocId, FinalWrites, ManualClock, SaveHook};
use openlab_engine::persist::JobKind;
use show::*;

const T0: i64 = 1_700_000_000_000;

fn launch(env: &Env, tweak: impl FnOnce(&mut ControlConfig)) -> (OfflineShow, ManualClock) {
    let clock = ManualClock::new(T0);
    let mut c = config(&workspace(Vec::new()));
    c.app_dirs = Some(env.dirs.clone());
    c.clock = clock.clock();
    tweak(&mut c);
    (OfflineShow::empty(mixer_config(), c).unwrap(), clock)
}

fn result(msgs: &[ServerMessage]) -> &Outcome {
    msgs.iter()
        .find_map(|m| match m {
            ServerMessage::Result { outcome, .. } => Some(outcome),
            _ => None,
        })
        .expect("no result")
}

fn data(msgs: &[ServerMessage]) -> ResultData {
    match result(msgs) {
        Outcome::Ok(OkBody { data: Some(d), .. }) => d.clone(),
        other => panic!("{other:?}"),
    }
}

fn opened(msgs: &[ServerMessage]) -> DocId {
    match data(msgs) {
        ResultData::Opened { doc } => doc,
        other => panic!("{other:?}"),
    }
}

fn app_request(s: &mut OfflineShow, request: Request) -> Vec<ServerMessage> {
    let id = s.request_id();
    s.send_app(ClientMessage::Request { id, request })
}

fn edit(s: &mut OfflineShow, doc: DocId, command: Command) {
    let id = s.request_id();
    let replies = s.send_doc(
        doc,
        ClientMessage::Command {
            id,
            command: Box::new(command),
            coalesce: None,
        },
    );
    assert!(matches!(result(&replies), Outcome::Ok(_)), "{replies:?}");
}

fn memo_show(name: &str) -> Workspace {
    let mut ws = workspace(vec![cue(1, CueKind::Memo)]);
    ws.name = name.into();
    ws.id = openlab_core::WorkspaceId(uuid::Uuid::from_u128(0xabc));
    ws
}

fn errors(s: &mut OfflineShow) -> Vec<String> {
    match data(&app_request(s, Request::LogTail { max: None })) {
        ResultData::LogTail { entries, .. } => entries
            .into_iter()
            .filter(|e| e.level == LogLevel::Error)
            .map(|e| e.message)
            .collect(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_engine_starts_with_no_doc_and_the_client_opens_one() {
    let env = env();
    let (mut s, _clock) = launch(&env, |_| {});
    assert!(s.control().doc_ids().is_empty());
    let id = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id,
        request: Request::Save,
    });
    match result(&replies) {
        Outcome::Error(e) => assert_eq!(e.code, ErrorCode::NoWorkspace),
        other => panic!("{other:?}"),
    }
    let doc = opened(&app_request(&mut s, Request::New {}));
    assert_eq!(s.control().doc_ids(), vec![doc]);
    // Nothing unsaved: shutdown writes nothing.
    assert_eq!(s.shutdown(), FinalWrites::default());
    assert!(names(&env.dirs.recovery_dir()).is_empty());
}

/// M4.15g: shutdown with two dirty docs writes two recovery files; the next launch lists both
/// (`scan_recovery`) and opens both (`open_recovery`) with their edits.
#[test]
fn shutdown_writes_a_recovery_file_per_dirty_doc_and_the_next_launch_restores_both() {
    let env = env();
    let path = env.show_path();
    fs::write(&path, save_show(&memo_show("Saved show")).unwrap()).unwrap();
    let (mut s, _clock) = launch(&env, |_| {});
    let saved = opened(&app_request(
        &mut s,
        Request::Open {
            path: path.to_string_lossy().into_owned(),
            recovery: None,
        },
    ));
    let untitled = opened(&app_request(&mut s, Request::New {}));
    let clean = opened(&app_request(&mut s, Request::New {}));
    edit(
        &mut s,
        saved,
        Command::SetCueProperty {
            id: id(1),
            property: CueProperty::Name {
                value: "unsaved cue".into(),
            },
        },
    );
    edit(
        &mut s,
        untitled,
        Command::RenameWorkspace {
            name: "Never saved".into(),
        },
    );
    s.tick();
    // The (virtual) clock never reached an autosave: only shutdown writes them.
    assert!(names(&env.dirs.recovery_dir()).is_empty());
    assert_eq!(
        s.shutdown(),
        FinalWrites {
            written: 2,
            ..FinalWrites::default()
        }
    );
    assert_eq!(names(&env.dirs.recovery_dir()).len(), 2);
    let untitled_id = s.control().doc(untitled).unwrap().workspace().id;
    assert!(s.control().doc(clean).is_some());
    drop(s);

    let (mut s, _clock) = launch(&env, |_| {});
    let mut entries = match data(&app_request(&mut s, Request::ScanRecovery)) {
        ResultData::RecoveryEntries { entries } => entries,
        other => panic!("{other:?}"),
    };
    assert_eq!(entries.len(), 2, "{entries:?}");
    entries.sort_by_key(|e| e.path_hash.is_none());
    let mut docs = Vec::new();
    for entry in &entries {
        docs.push(opened(&app_request(
            &mut s,
            Request::OpenRecovery {
                workspace_id: entry.workspace_id,
                path_hash: entry.path_hash.clone(),
                modified_ms: entry.modified_ms,
            },
        )));
    }
    let [from_saved, from_untitled] = docs.as_slice() else {
        panic!("{docs:?}")
    };
    let a = s.control().doc(*from_saved).unwrap();
    assert_eq!(a.workspace().find_cue(id(1)).unwrap().name, "unsaved cue");
    assert!(a.is_unsaved());
    let b = s.control().doc(*from_untitled).unwrap();
    assert_eq!(b.workspace().name, "Never saved");
    assert_eq!(b.workspace().id, untitled_id);
    assert!(b.is_unsaved());
}

/// M4.4d: the writer discards autosaves still queued when it is dropped, so shutdown queues and
/// waits; on a hung disk the wait is bounded and the doc that did not make it is logged.
#[test]
fn a_final_write_on_a_hung_disk_is_bounded_and_logged() {
    let env = env();
    let (release, gate) = mpsc::channel::<()>();
    let gate = Arc::new(Mutex::new(gate));
    let hook = SaveHook(Arc::new(move |kind: JobKind| {
        if kind == JobKind::Autosave {
            let _ = gate.lock().unwrap().recv_timeout(Duration::from_secs(30));
        }
    }));
    let (mut s, _clock) = launch(&env, |c| {
        c.save_hook = Some(hook);
        c.shutdown_wait = Duration::ZERO;
    });
    let doc = opened(&app_request(&mut s, Request::New {}));
    edit(
        &mut s,
        doc,
        Command::RenameWorkspace {
            name: "Stuck".into(),
        },
    );
    assert_eq!(
        s.shutdown(),
        FinalWrites {
            unfinished: 1,
            ..FinalWrites::default()
        }
    );
    let errors = errors(&mut s);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("\"Stuck\"") && e.contains("still being written")),
        "{errors:?}"
    );
    // The disk answers: the writer, still running, finishes the write as the engine goes.
    release.send(()).unwrap();
    drop(s);
    assert_eq!(names(&env.dirs.recovery_dir()).len(), 1);
}

/// An autosave of the last edit already on disk is not written again.
#[test]
fn a_doc_whose_last_edit_is_autosaved_is_not_written_again() {
    let env = env();
    let writes = Arc::new(Mutex::new(0));
    let counted = Arc::clone(&writes);
    let hook = SaveHook(Arc::new(move |kind: JobKind| {
        if kind == JobKind::Autosave {
            *counted.lock().unwrap() += 1;
        }
    }));
    let (mut s, clock) = launch(&env, |c| c.save_hook = Some(hook));
    let doc = opened(&app_request(&mut s, Request::New {}));
    edit(
        &mut s,
        doc,
        Command::RenameWorkspace {
            name: "Autosaved".into(),
        },
    );
    s.tick();
    clock.advance(31_000);
    s.settle_docs();
    assert_eq!(*writes.lock().unwrap(), 1);
    assert_eq!(
        s.shutdown(),
        FinalWrites {
            written: 1,
            ..FinalWrites::default()
        }
    );
    assert_eq!(*writes.lock().unwrap(), 1);
}

/// SM-07, SM-01: Show Mode is the engine's (per doc, in memory, in `document` and the
/// snapshot), and the loop asks to keep the computer awake while a doc is in it or a cue runs.
#[test]
fn show_mode_and_running_cues_keep_the_computer_awake() {
    let env = env();
    let (awake_tx, awake) = crossbeam_channel::unbounded();
    let ws = workspace(vec![cue(1, CueKind::Wait { duration: 1.0 })]);
    let mut c = config(&ws);
    c.app_dirs = Some(env.dirs.clone());
    c.awake = Some(awake_tx);
    let mut s = OfflineShow::new(ws, mixer_config(), c).unwrap();
    let doc = s.control().doc_ids()[0];
    let said = |rx: &crossbeam_channel::Receiver<bool>| rx.try_iter().collect::<Vec<_>>();

    let id = s.request_id();
    let replies = s.send_doc(
        doc,
        ClientMessage::Request {
            id,
            request: Request::SetShowMode { on: true },
        },
    );
    assert!(matches!(result(&replies), Outcome::Ok(_)), "{replies:?}");
    let documents: Vec<bool> = s
        .take_outbound()
        .into_iter()
        .filter_map(|m| match m {
            ServerMessage::Document { document } => Some(document.show_mode),
            _ => None,
        })
        .collect();
    assert_eq!(documents, [true]);
    assert!(s.control().doc(doc).unwrap().snapshot().document.show_mode);
    s.tick();
    assert!(s.control().keeps_awake());
    assert_eq!(said(&awake), [true]);
    let id = s.request_id();
    s.send_doc(
        doc,
        ClientMessage::Request {
            id,
            request: Request::SetShowMode { on: false },
        },
    );
    s.tick();
    assert_eq!(said(&awake), [false]);

    // A running cue keeps it awake too, until it ends.
    start(&mut s, 1);
    s.tick();
    assert_eq!(said(&awake), [true]);
    s.run(u64::from(RATE) * 2);
    assert_eq!(said(&awake), [false]);

    // Workspace-bound: an app connection has none.
    let replies = app_request(&mut s, Request::SetShowMode { on: true });
    match result(&replies) {
        Outcome::Error(e) => assert_eq!(e.code, ErrorCode::NoWorkspace),
        other => panic!("{other:?}"),
    }
    // Shutdown lets go.
    let id = s.request_id();
    s.send_doc(
        doc,
        ClientMessage::Request {
            id,
            request: Request::SetShowMode { on: true },
        },
    );
    s.tick();
    assert_eq!(said(&awake), [true]);
    s.shutdown();
    assert_eq!(said(&awake), [false]);
}

/// Without an app-support folder there is nowhere to write: counted and logged, not silent.
#[test]
fn unsaved_changes_with_nowhere_to_write_are_logged_at_shutdown() {
    let env = env();
    let (mut s, _clock) = launch(&env, |c| c.app_dirs = None);
    let doc = opened(&app_request(&mut s, Request::New {}));
    edit(
        &mut s,
        doc,
        Command::RenameWorkspace {
            name: "Nowhere".into(),
        },
    );
    assert_eq!(
        s.shutdown(),
        FinalWrites {
            failed: 1,
            ..FinalWrites::default()
        }
    );
    let errors = errors(&mut s);
    assert!(
        errors.iter().any(|e| e.contains("\"Nowhere\"")),
        "{errors:?}"
    );
}
