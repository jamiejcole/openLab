// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Documents through the control loop on the offline driver (M4.4; NFR-04, NFR-05, NFR-06,
//! WS-02, WS-03, WS-06, WS-07, WS-08): new, open, open-recovery, save, save-as, revert, autosave
//! and recovery, with a virtual wall clock and the persistence layer on a temp folder.

mod common;
mod show;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use common::{Env, env, names, set_mtime};
use openlab_core::protocol::{
    ClientMessage, ErrorCode, Outcome, RecoveryChoice, Request, RequestId, ResultData,
    ServerMessage, Snapshot, WireCueState,
};
use openlab_core::{Command, CueKind, CueProperty, MediaRef, Workspace, load_show, save_show};
use openlab_engine::control::offline::OfflineShow;
use openlab_engine::control::{DocHook, Inbox, ManualClock, SaveHook, reply_channel};
use openlab_engine::persist::JobKind;
use show::*;

const T0: i64 = 1_700_000_000_000;

/// Releases a writer held in a test hook when dropped, so a failing assertion does not turn
/// into a hang in the writer's `Drop` (declare it after the rig: it drops first).
struct Unblock(mpsc::Sender<()>);

impl Drop for Unblock {
    fn drop(&mut self) {
        for _ in 0..16 {
            let _ = self.0.send(());
        }
    }
}

struct Rig {
    s: OfflineShow,
    env: Env,
    clock: ManualClock,
}

fn rig_with(ws: Workspace, show_path: Option<PathBuf>, hook: Option<SaveHook>) -> Rig {
    rig_tweak(ws, |c| {
        c.show_path = show_path;
        c.save_hook = hook;
    })
}

fn rig_tweak(
    ws: Workspace,
    tweak: impl FnOnce(&mut openlab_engine::control::ControlConfig),
) -> Rig {
    let env = env();
    let clock = ManualClock::new(T0);
    let mut c = config(&ws);
    c.app_dirs = Some(env.dirs.clone());
    c.clock = clock.clock();
    tweak(&mut c);
    Rig {
        s: show_with(ws, c),
        env,
        clock,
    }
}

fn rig(ws: Workspace) -> Rig {
    rig_with(ws, None, None)
}

/// A rig whose workspace is the show saved at `<env>/show/My Show.openlab`.
fn saved_rig(ws: Workspace) -> Rig {
    let mut r = rig(ws);
    let path = r.env.show_path();
    ok_result(&request(
        &mut r.s,
        Request::SaveAs {
            path: path.to_string_lossy().into_owned(),
        },
    ));
    r
}

fn request(s: &mut OfflineShow, request: Request) -> Vec<ServerMessage> {
    let id = s.request_id();
    let replies = s.send(ClientMessage::Request { id, request });
    assert_eq!(s.stalls(), 0, "a worker did not answer in time");
    replies
}

fn outcome(msgs: &[ServerMessage]) -> &Outcome {
    msgs.iter()
        .find_map(|m| match m {
            ServerMessage::Result { outcome, .. } => Some(outcome),
            _ => None,
        })
        .expect("no result")
}

fn ok_result(msgs: &[ServerMessage]) -> Option<u64> {
    match outcome(msgs) {
        Outcome::Ok(ok) => ok.revision,
        Outcome::Error(e) => panic!("{e:?}"),
    }
}

fn error_code(msgs: &[ServerMessage]) -> ErrorCode {
    match outcome(msgs) {
        Outcome::Error(e) => e.code,
        Outcome::Ok(ok) => panic!("{ok:?}"),
    }
}

fn data(msgs: &[ServerMessage]) -> ResultData {
    match outcome(msgs) {
        Outcome::Ok(ok) => ok.data.clone().expect("no data"),
        Outcome::Error(e) => panic!("{e:?}"),
    }
}

fn snapshot(msgs: &[ServerMessage]) -> &Snapshot {
    msgs.iter()
        .find_map(|m| match m {
            ServerMessage::Snapshot { snapshot } => Some(&**snapshot),
            _ => None,
        })
        .expect("no snapshot")
}

fn rename(s: &mut OfflineShow, n: u128, name: &str) {
    let id = s.request_id();
    let replies = s.send(ClientMessage::Command {
        id,
        command: Box::new(Command::SetCueProperty {
            id: show::id(n),
            property: CueProperty::Name { value: name.into() },
        }),
    });
    ok_result(&replies);
}

fn write_show(path: &Path, ws: &Workspace) {
    fs::write(path, save_show(ws).unwrap()).unwrap();
}

fn memo_ws(name: &str, cues: &[u128]) -> Workspace {
    let mut ws = workspace(cues.iter().map(|&n| cue(n, CueKind::Memo)).collect());
    ws.name = name.into();
    ws
}

fn cue_name(s: &OfflineShow, n: u128) -> String {
    s.control()
        .workspace()
        .find_cue(id(n))
        .unwrap()
        .name
        .clone()
}

fn open(path: &Path) -> Request {
    Request::Open {
        path: path.to_string_lossy().into_owned(),
        discard_changes: false,
        recovery: None,
    }
}

#[test]
fn new_save_as_open_and_revert_round_trip_through_the_protocol() {
    let mut r = rig(memo_ws("one", &[1, 2]));
    let a = r.env.show_dir().join("a.openlab");
    let b = r.env.show_dir().join("b.openlab");

    // Save As: a file on disk, the document message says where and clean, the workspace is
    // bound to the path.
    r.s.take_outbound();
    let replies = request(
        &mut r.s,
        Request::SaveAs {
            path: a.to_string_lossy().into_owned(),
        },
    );
    ok_result(&replies);
    let document =
        r.s.take_outbound()
            .into_iter()
            .find_map(|m| match m {
                ServerMessage::Document { document } => Some(document),
                _ => None,
            })
            .expect("a document message");
    assert_eq!(document.path.as_deref(), Some(a.to_str().unwrap()));
    assert!(!document.dirty);
    assert_eq!(
        load_show(&fs::read_to_string(&a).unwrap())
            .unwrap()
            .workspace
            .name,
        "one"
    );

    // An edit makes it dirty; Save writes it; the file holds it.
    rename(&mut r.s, 1, "edited");
    assert!(r.s.control().is_unsaved());
    ok_result(&request(&mut r.s, Request::Save));
    assert!(!r.s.control().is_unsaved());
    let saved = load_show(&fs::read_to_string(&a).unwrap())
        .unwrap()
        .workspace;
    assert_eq!(saved.find_cue(id(1)).unwrap().name, "edited");

    // Revert drops the next edit, clears undo, and replies snapshot, warnings, result.
    rename(&mut r.s, 2, "dropped");
    let replies = request(
        &mut r.s,
        Request::Revert {
            discard_changes: true,
        },
    );
    assert!(matches!(replies[0], ServerMessage::Snapshot { .. }));
    assert!(matches!(replies[1], ServerMessage::Warnings { .. }));
    assert!(matches!(replies[2], ServerMessage::Result { .. }));
    assert_eq!(cue_name(&r.s, 2), "cue 2");
    assert_eq!(cue_name(&r.s, 1), "edited");
    assert!(!r.s.control().is_unsaved());
    let snap = snapshot(&replies);
    assert!(!snap.document.can_undo, "revert clears undo");
    assert_eq!(snap.workspace.find_cue(id(1)).unwrap().name, "edited");

    // New: an empty untitled workspace, never saved (Save has no path).
    let replies = request(
        &mut r.s,
        Request::New {
            discard_changes: false,
        },
    );
    ok_result(&replies);
    let snap = snapshot(&replies);
    assert_eq!(snap.document.path, None);
    assert!(!snap.document.dirty);
    assert_eq!(snap.workspace.cue_lists.len(), 1);
    assert!(snap.workspace.cue_lists[0].cues.is_empty());
    assert_ne!(snap.workspace.id, saved.id);
    assert_eq!(
        error_code(&request(&mut r.s, Request::Save)),
        ErrorCode::NoWorkspace
    );
    assert_eq!(
        error_code(&request(
            &mut r.s,
            Request::Revert {
                discard_changes: false
            }
        )),
        ErrorCode::NoWorkspace
    );

    // Save As the new one, then Open the first show again.
    ok_result(&request(
        &mut r.s,
        Request::SaveAs {
            path: b.to_string_lossy().into_owned(),
        },
    ));
    let replies = request(&mut r.s, open(&a));
    ok_result(&replies);
    let snap = snapshot(&replies);
    assert_eq!(snap.workspace.id, saved.id);
    assert_eq!(snap.document.path.as_deref(), Some(a.to_str().unwrap()));
    assert_eq!(r.s.control().show_path(), Some(a.as_path()));
}

#[test]
fn open_errors_are_clear_and_change_nothing() {
    let mut r = rig(memo_ws("keep", &[1]));
    let before = r.s.control().revision();
    let missing = r.env.show_dir().join("nope.openlab");
    assert_eq!(
        error_code(&request(&mut r.s, open(&missing))),
        ErrorCode::NotFound
    );
    let garbage = r.env.show_dir().join("bad.openlab");
    fs::write(&garbage, "{ this is not a show").unwrap();
    assert_eq!(
        error_code(&request(&mut r.s, open(&garbage))),
        ErrorCode::BadShowFile
    );
    let binary = r.env.show_dir().join("bin.openlab");
    fs::write(&binary, [0xff, 0xfe, 0x00, 0x80]).unwrap();
    assert_eq!(
        error_code(&request(&mut r.s, open(&binary))),
        ErrorCode::BadShowFile
    );
    assert_eq!(r.s.control().revision(), before);
    assert_eq!(r.s.control().workspace().name, "keep");
}

#[test]
fn unsaved_changes_refuse_new_open_and_revert_unless_discarded() {
    let mut r = saved_rig(memo_ws("one", &[1]));
    let other = r.env.show_dir().join("other.openlab");
    write_show(&other, &memo_ws("other", &[7]));
    rename(&mut r.s, 1, "edited");
    let revision = r.s.control().revision();
    for refused in [
        Request::New {
            discard_changes: false,
        },
        open(&other),
        Request::Revert {
            discard_changes: false,
        },
        Request::OpenRecovery {
            workspace_id: r.s.control().workspace().id,
            path_hash: None,
            modified_ms: None,
            discard_changes: false,
        },
    ] {
        let replies = request(&mut r.s, refused);
        assert_eq!(error_code(&replies), ErrorCode::UnsavedChanges);
        assert!(
            !replies
                .iter()
                .any(|m| matches!(m, ServerMessage::Snapshot { .. }))
        );
    }
    assert_eq!(r.s.control().revision(), revision);
    assert_eq!(cue_name(&r.s, 1), "edited");

    // With the flag, it goes ahead.
    let replies = request(
        &mut r.s,
        Request::Open {
            path: other.to_string_lossy().into_owned(),
            discard_changes: true,
            recovery: None,
        },
    );
    ok_result(&replies);
    assert_eq!(snapshot(&replies).workspace.name, "other");
}

#[test]
fn a_clean_workspace_is_replaced_without_the_flag() {
    let mut r = saved_rig(memo_ws("one", &[1]));
    ok_result(&request(
        &mut r.s,
        Request::New {
            discard_changes: false,
        },
    ));
    assert!(!r.s.control().is_unsaved());
}

#[test]
fn discarding_edits_removes_their_autosave_so_it_is_not_offered_later() {
    let mut r = saved_rig(memo_ws("one", &[1]));
    let path = r.env.show_path();
    rename(&mut r.s, 1, "edited");
    r.s.tick();
    r.clock.advance(31_000);
    r.s.settle_docs();
    let recovery = r.env.dirs.recovery_dir();
    assert_eq!(names(&recovery).len(), 1, "autosaved");
    ok_result(&request(
        &mut r.s,
        Request::New {
            discard_changes: true,
        },
    ));
    r.s.settle_docs();
    // The worker removes it asynchronously: wait for it through the inbox-free path.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !names(&recovery).is_empty() && std::time::Instant::now() < deadline {
        thread::yield_now();
    }
    assert!(names(&recovery).is_empty(), "{:?}", names(&recovery));
    // Opening the show now offers nothing.
    let replies = request(&mut r.s, open(&path));
    ok_result(&replies);
}

#[test]
fn reverting_removes_the_autosave_of_the_dropped_edits() {
    let mut r = saved_rig(memo_ws("one", &[1]));
    rename(&mut r.s, 1, "edited");
    r.s.tick();
    r.clock.advance(31_000);
    r.s.settle_docs();
    assert_eq!(names(&r.env.dirs.recovery_dir()).len(), 1);
    ok_result(&request(
        &mut r.s,
        Request::Revert {
            discard_changes: true,
        },
    ));
    assert!(names(&r.env.dirs.recovery_dir()).is_empty());
}

#[test]
fn running_cues_stop_on_new_open_and_revert() {
    let path = wav(&dc(8000, 0.5));
    for which in 0..3 {
        let mut r = saved_rig(workspace(vec![audio(1, &path, 0), cue(2, CueKind::Memo)]));
        let other = r.env.show_dir().join("other.openlab");
        write_show(&other, &memo_ws("other", &[9]));
        start(&mut r.s, 1);
        r.s.run(BLOCK * 8);
        assert!(r.s.control().live_voices() > 0);
        r.s.take_outbound();
        let replies = match which {
            0 => request(
                &mut r.s,
                Request::New {
                    discard_changes: false,
                },
            ),
            1 => request(&mut r.s, open(&other)),
            _ => request(
                &mut r.s,
                Request::Revert {
                    discard_changes: false,
                },
            ),
        };
        ok_result(&replies);
        assert_eq!(r.s.control().live_voices(), 0, "case {which}");
        let out = r.s.take_outbound();
        assert!(
            states(&out, id(1))
                .iter()
                .any(|(s, _)| *s == WireCueState::Idle),
            "case {which}: the clients are told the cue stopped: {:?}",
            states(&out, id(1))
        );
        // Silence from the block after the request, though the old voice's end is reported
        // late and the revert reuses the cue's id: it is never mistaken for the new cue.
        let from = r.s.audio().len() + usize::try_from(BLOCK).unwrap() * 2 * 2;
        r.s.run(BLOCK * 40);
        assert!(
            r.s.audio()[from..].iter().all(|x| *x == 0.0),
            "case {which}: audio after the stop"
        );
        assert_eq!(
            r.s.control().engine().state(id(1)),
            openlab_core::CueState::Idle,
            "case {which}"
        );
        let later = r.s.take_outbound();
        assert!(
            states(&later, id(1)).is_empty(),
            "case {which}: {:?}",
            states(&later, id(1))
        );
    }
}

#[test]
fn revision_and_runtime_seq_keep_rising_across_replacements() {
    let mut r = saved_rig(memo_ws("one", &[1]));
    let a = r.env.show_path();
    let other = r.env.show_dir().join("other.openlab");
    write_show(&other, &memo_ws("other", &[7]));
    let mut last = (r.s.control().revision(), r.s.control().runtime_seq());
    for request_ in [
        Request::New {
            discard_changes: false,
        },
        open(&other),
        open(&a),
        Request::Revert {
            discard_changes: false,
        },
        Request::New {
            discard_changes: false,
        },
    ] {
        let replies = request(&mut r.s, request_);
        ok_result(&replies);
        let snap = snapshot(&replies);
        assert!(snap.revision > last.0, "{} > {}", snap.revision, last.0);
        assert!(snap.runtime_seq >= last.1);
        assert_eq!(
            ok_result(&replies),
            Some(snap.revision),
            "the result reports the snapshot's revision"
        );
        last = (snap.revision, snap.runtime_seq);
        // And an edit afterwards is exactly one step on.
        let cues: Vec<_> = snap.workspace.cue_lists[0]
            .cues
            .iter()
            .map(|c| c.id)
            .collect();
        if let Some(first) = cues.first() {
            let id = r.s.request_id();
            let replies = r.s.send(ClientMessage::Command {
                id,
                command: Box::new(Command::SetCueProperty {
                    id: *first,
                    property: CueProperty::Name { value: "x".into() },
                }),
            });
            assert_eq!(ok_result(&replies), Some(last.0 + 1));
            last.0 += 1;
            // Leave a clean workspace for the next replacement.
            if r.s.control().show_path().is_some() {
                ok_result(&request(
                    &mut r.s,
                    Request::Revert {
                        discard_changes: true,
                    },
                ));
                last.0 = r.s.control().revision();
            } else {
                ok_result(&request(
                    &mut r.s,
                    Request::New {
                        discard_changes: true,
                    },
                ));
                last.0 = r.s.control().revision();
            }
        }
    }
}

#[test]
fn every_replacement_is_broadcast_as_a_snapshot_to_all_clients() {
    let mut r = saved_rig(memo_ws("one", &[1]));
    r.s.take_outbound();
    let replies = request(
        &mut r.s,
        Request::New {
            discard_changes: false,
        },
    );
    let own = snapshot(&replies).clone();
    let broadcast: Vec<_> =
        r.s.take_outbound()
            .into_iter()
            .filter_map(|m| match m {
                ServerMessage::Snapshot { snapshot } => Some(snapshot),
                _ => None,
            })
            .collect();
    assert_eq!(broadcast.len(), 1);
    assert_eq!(*broadcast[0], own);
}

#[test]
fn the_snapshot_reaches_the_requester_before_its_result() {
    let mut r = saved_rig(memo_ws("one", &[1]));
    let replies = request(
        &mut r.s,
        Request::New {
            discard_changes: false,
        },
    );
    let snapshot_at = replies
        .iter()
        .position(|m| matches!(m, ServerMessage::Snapshot { .. }))
        .unwrap();
    let result_at = replies
        .iter()
        .position(|m| matches!(m, ServerMessage::Result { .. }))
        .unwrap();
    assert!(snapshot_at < result_at);
}

#[test]
fn save_runs_on_the_writer_thread_and_the_control_loop_is_not_held_up() {
    let (entered_tx, entered) = mpsc::channel::<String>();
    let (release_tx, release) = mpsc::channel::<()>();
    let release = Arc::new(Mutex::new(release));
    let entered_tx = Mutex::new(entered_tx);
    let hook = SaveHook(Arc::new(move |kind: JobKind| {
        if kind == JobKind::Save {
            let name = thread::current().name().unwrap_or("").to_owned();
            let _ = entered_tx.lock().unwrap().send(name);
            let _ = release.lock().unwrap().recv();
        }
    }));
    let mut r = rig_with(memo_ws("one", &[1]), None, Some(hook));
    let path = r.env.show_path();
    let (tx, mut rx) = reply_channel();
    let id = r.s.request_id();
    r.s.deliver(Inbox::Client {
        message: ClientMessage::Request {
            id,
            request: Request::SaveAs {
                path: path.to_string_lossy().into_owned(),
            },
        },
        reply: tx,
    });
    // The writer is inside the job, blocked; the control loop answers other messages and
    // reports the save as still in progress.
    let name = entered.recv().unwrap();
    assert_eq!(name, "openlab-writer");
    assert_ne!(Some(name.as_str()), thread::current().name());
    // (Delivered, not sent: `send` would wait for the blocked writer.)
    let (etx, mut erx) = reply_channel();
    let eid = r.s.request_id();
    r.s.deliver(Inbox::Client {
        message: ClientMessage::Command {
            id: eid,
            command: Box::new(Command::SetCueProperty {
                id: show::id(1),
                property: CueProperty::Name {
                    value: "while saving".into(),
                },
            }),
        },
        reply: etx,
    });
    assert!(
        matches!(
            erx.try_recv().unwrap(),
            ServerMessage::Result {
                outcome: Outcome::Ok(_),
                ..
            }
        ),
        "an edit is answered at once"
    );
    r.s.tick();
    assert!(
        rx.try_recv().is_err(),
        "no answer before the file is written"
    );
    assert!(r.s.control().docs_busy() > 0);
    assert!(
        !path.exists(),
        "the file appears only when the save completes"
    );
    release_tx.send(()).unwrap();
    r.s.settle_docs();
    assert!(matches!(
        rx.try_recv().unwrap(),
        ServerMessage::Result {
            outcome: Outcome::Ok(_),
            ..
        }
    ));
    // The edit made while saving is not in the file: still dirty.
    assert!(r.s.control().is_unsaved());
    let on_disk = load_show(&fs::read_to_string(&path).unwrap())
        .unwrap()
        .workspace;
    assert_eq!(on_disk.find_cue(id_of(1)).unwrap().name, "cue 1");
    assert_eq!(names(&r.env.show_dir()), vec!["My Show.openlab".to_owned()]);
}

fn id_of(n: u128) -> openlab_core::CueId {
    id(n)
}

#[test]
fn saves_queued_behind_a_busy_writer_are_all_answered() {
    let (entered_tx, entered) = mpsc::channel::<()>();
    let (release_tx, release) = mpsc::channel::<()>();
    let release = Arc::new(Mutex::new(release));
    let entered_tx = Mutex::new(entered_tx);
    let hook = SaveHook(Arc::new(move |kind: JobKind| {
        if kind == JobKind::Save {
            let _ = entered_tx.lock().unwrap().send(());
            let _ = release.lock().unwrap().recv();
        }
    }));
    let mut r = rig_with(memo_ws("one", &[1]), None, Some(hook));
    let path = r.env.show_path();
    let mut channels = Vec::new();
    // The first save is running (blocked); two more arrive for the same file: the newer
    // replaces the older, and both requests are answered when it is written.
    for n in 0..3 {
        if n == 1 {
            entered.recv().unwrap();
        }
        let (tx, rx) = reply_channel();
        let id = r.s.request_id();
        r.s.deliver(Inbox::Client {
            message: ClientMessage::Request {
                id,
                request: Request::SaveAs {
                    path: path.to_string_lossy().into_owned(),
                },
            },
            reply: tx,
        });
        channels.push(rx);
    }
    release_tx.send(()).unwrap();
    release_tx.send(()).unwrap();
    r.s.settle_docs();
    for mut rx in channels {
        assert!(matches!(
            rx.try_recv().unwrap(),
            ServerMessage::Result {
                outcome: Outcome::Ok(_),
                ..
            }
        ));
    }
}

#[test]
fn a_failed_save_is_an_io_error_and_the_workspace_stays_dirty() {
    let mut r = saved_rig(memo_ws("one", &[1]));
    let good = fs::read_to_string(r.env.show_path()).unwrap();
    rename(&mut r.s, 1, "edited");
    // Save As into a folder that does not exist.
    let bad = r.env.show_dir().join("no such folder").join("x.openlab");
    let replies = request(
        &mut r.s,
        Request::SaveAs {
            path: bad.to_string_lossy().into_owned(),
        },
    );
    assert_eq!(error_code(&replies), ErrorCode::Io);
    assert!(r.s.control().is_unsaved());
    assert_eq!(r.s.control().show_path(), Some(r.env.show_path().as_path()));
    assert_eq!(fs::read_to_string(r.env.show_path()).unwrap(), good);
    // And the workspace still saves where it is.
    ok_result(&request(&mut r.s, Request::Save));
    assert!(!r.s.control().is_unsaved());
}

#[test]
fn saving_keeps_backups_and_leaves_no_temporary_files() {
    let mut r = saved_rig(memo_ws("one", &[1]));
    for n in 0..3 {
        r.clock.advance(1000);
        rename(&mut r.s, 1, &format!("edit {n}"));
        ok_result(&request(&mut r.s, Request::Save));
    }
    assert_eq!(names(&r.env.show_dir()), vec!["My Show.openlab".to_owned()]);
    let backups = r
        .env
        .dirs
        .backups_dir(r.s.control().workspace().id, &r.env.show_path());
    assert_eq!(names(&backups).len(), 3);
}

#[test]
fn the_machine_backup_count_is_honoured_by_saves() {
    for (keep, expect) in [(2u32, 2usize), (0, 0)] {
        let ws = memo_ws("one", &[1]);
        let env = env();
        let clock = ManualClock::new(T0);
        let mut c = config(&ws);
        c.app_dirs = Some(env.dirs.clone());
        c.clock = clock.clock();
        c.machine.backup_keep = keep;
        let mut s = show_with(ws, c);
        let path = env.show_path();
        ok_result(&request(
            &mut s,
            Request::SaveAs {
                path: path.to_string_lossy().into_owned(),
            },
        ));
        for n in 0..4 {
            clock.advance(1000);
            rename(&mut s, 1, &format!("edit {n}"));
            ok_result(&request(&mut s, Request::Save));
        }
        let backups = env.dirs.backups_dir(s.control().workspace().id, &path);
        assert_eq!(names(&backups).len(), expect, "backup_keep {keep}");
    }
}

#[test]
fn workspace_settings_travel_with_the_file_and_machine_settings_do_not() {
    let mut ws = memo_ws("one", &[1]);
    ws.settings.min_go_interval = 0.5;
    ws.settings.panic_duration = 3.0;
    ws.settings.autosave_interval = 45.0;
    ws.settings.osc_passcode = Some("secret".into());
    ws.settings.master_db = -6.0;
    let mut r = rig(ws.clone());
    let path = r.env.show_path();
    ok_result(&request(
        &mut r.s,
        Request::SaveAs {
            path: path.to_string_lossy().into_owned(),
        },
    ));
    let text = fs::read_to_string(&path).unwrap();
    // The machine maps the patch to the device "Offline"; the file knows only logical outputs.
    assert!(
        !text.contains("Offline"),
        "device names stay on the machine"
    );
    let replies = request(&mut r.s, open(&path));
    ok_result(&replies);
    assert_eq!(snapshot(&replies).workspace.settings, ws.settings);
    assert_eq!(snapshot(&replies).workspace.patches, ws.patches);
}

#[test]
fn autosave_fires_on_the_workspace_interval_and_a_save_clears_it() {
    let mut ws = memo_ws("one", &[1]);
    ws.settings.autosave_interval = 30.0;
    let mut r = saved_rig(ws);
    let recovery = r.env.dirs.recovery_dir();
    // Clean: nothing, however long.
    r.clock.advance(3_600_000);
    r.s.settle_docs();
    assert!(names(&recovery).is_empty());
    // Dirty at t: nothing at t + 29.999 s, a file at t + 30 s.
    rename(&mut r.s, 1, "edited");
    r.s.tick();
    r.clock.advance(29_999);
    r.s.settle_docs();
    assert!(names(&recovery).is_empty());
    r.clock.advance(1);
    r.s.settle_docs();
    assert_eq!(names(&recovery).len(), 1);
    // The recovery file holds the edit and the show file does not.
    let recovered = load_show(&fs::read_to_string(recovery.join(&names(&recovery)[0])).unwrap())
        .unwrap()
        .workspace;
    assert_eq!(recovered.find_cue(id(1)).unwrap().name, "edited");
    // No new autosave of the same revision, however long.
    let before = fs::metadata(recovery.join(&names(&recovery)[0]))
        .unwrap()
        .modified()
        .unwrap();
    r.clock.advance(600_000);
    r.s.settle_docs();
    let after = fs::metadata(recovery.join(&names(&recovery)[0]))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(before, after);
    // The autosave is long out of date, so a newer edit is written at the next tick; the one
    // after that waits one interval from it.
    let recovered_name = |r: &Rig| {
        let dir = r.env.dirs.recovery_dir();
        load_show(&fs::read_to_string(dir.join(&names(&dir)[0])).unwrap())
            .unwrap()
            .workspace
            .find_cue(id(1))
            .unwrap()
            .name
            .clone()
    };
    rename(&mut r.s, 1, "edited again");
    r.s.settle_docs();
    assert_eq!(recovered_name(&r), "edited again");
    rename(&mut r.s, 1, "edited a third time");
    r.clock.advance(29_999);
    r.s.settle_docs();
    assert_eq!(
        recovered_name(&r),
        "edited again",
        "not before the interval has passed"
    );
    r.clock.advance(1);
    r.s.settle_docs();
    assert_eq!(recovered_name(&r), "edited a third time");
    // Saving removes it.
    ok_result(&request(&mut r.s, Request::Save));
    assert!(names(&recovery).is_empty());
}

#[test]
fn an_untitled_workspace_autosaves_under_its_id_alone() {
    let mut r = rig(memo_ws("one", &[1]));
    rename(&mut r.s, 1, "edited");
    r.s.tick();
    r.clock.advance(30_000);
    r.s.settle_docs();
    let id = r.s.control().workspace().id;
    assert_eq!(
        names(&r.env.dirs.recovery_dir()),
        vec![format!("{id}.openlab")]
    );
}

/// Simulates an unclean exit: a show saved at `path`, edited and autosaved, then dropped
/// without a save. Returns the show path and the workspace id.
fn crash_with_autosave(r: Rig) -> (Env, PathBuf, openlab_core::WorkspaceId) {
    let mut r = r;
    let path = r.env.show_path();
    rename(&mut r.s, 1, "unsaved work");
    r.s.tick();
    r.clock.advance(31_000);
    r.s.settle_docs();
    let id = r.s.control().workspace().id;
    let recovery = r.env.dirs.recovery_path(id, Some(&path));
    assert!(recovery.exists());
    // File times, not the (virtual) clock, decide: the autosave is newer than the show.
    set_mtime(&path, 1_000_000);
    set_mtime(&recovery, 2_000_000);
    let Rig { env, .. } = r;
    (env, path, id)
}

fn restart(env: Env, ws: Workspace) -> Rig {
    let clock = ManualClock::new(T0);
    let mut c = config(&ws);
    c.app_dirs = Some(env.dirs.clone());
    c.clock = clock.clock();
    Rig {
        s: show_with(ws, c),
        env,
        clock,
    }
}

#[test]
fn recovery_is_offered_on_open_after_an_unclean_exit_and_nothing_changes_until_answered() {
    let (env, path, _) = crash_with_autosave(saved_rig(memo_ws("one", &[1])));
    let mut r = restart(env, memo_ws("blank", &[5]));
    let revision = r.s.control().revision();
    let replies = request(&mut r.s, open(&path));
    match data(&replies) {
        ResultData::RecoveryOffer {
            path: offered,
            workspace_name,
            modified_ms,
        } => {
            assert_eq!(
                offered,
                path.to_string_lossy(),
                "the offer names the open it answers"
            );
            assert_eq!(workspace_name, "one");
            assert_eq!(modified_ms, 2_000_000);
        }
        other => panic!("{other:?}"),
    }
    assert!(
        !replies
            .iter()
            .any(|m| matches!(m, ServerMessage::Snapshot { .. }))
    );
    assert_eq!(r.s.control().revision(), revision);
    assert_eq!(r.s.control().workspace().name, "blank");

    // Restore: the autosave opens, bound to the show path, dirty until saved.
    let replies = request(
        &mut r.s,
        Request::Open {
            path: path.to_string_lossy().into_owned(),
            discard_changes: false,
            recovery: Some(RecoveryChoice::Restore),
        },
    );
    ok_result(&replies);
    let snap = snapshot(&replies);
    assert_eq!(snap.workspace.find_cue(id(1)).unwrap().name, "unsaved work");
    assert!(snap.document.dirty);
    assert_eq!(snap.document.path.as_deref(), Some(path.to_str().unwrap()));
    // Saving makes it the file and removes the autosave.
    ok_result(&request(&mut r.s, Request::Save));
    assert!(!r.s.control().is_unsaved());
    assert!(names(&r.env.dirs.recovery_dir()).is_empty());
    let on_disk = load_show(&fs::read_to_string(&path).unwrap())
        .unwrap()
        .workspace;
    assert_eq!(on_disk.find_cue(id(1)).unwrap().name, "unsaved work");
}

#[test]
fn ignoring_the_recovery_opens_the_saved_file_and_keeps_the_autosave() {
    let (env, path, id) = crash_with_autosave(saved_rig(memo_ws("one", &[1])));
    let mut r = restart(env, memo_ws("blank", &[5]));
    let replies = request(
        &mut r.s,
        Request::Open {
            path: path.to_string_lossy().into_owned(),
            discard_changes: false,
            recovery: Some(RecoveryChoice::Ignore),
        },
    );
    ok_result(&replies);
    let snap = snapshot(&replies);
    assert_eq!(snap.workspace.find_cue(id_of(1)).unwrap().name, "cue 1");
    assert!(!snap.document.dirty);
    assert!(r.env.dirs.recovery_path(id, Some(&path)).exists());
}

#[test]
fn a_stale_autosave_is_not_offered() {
    let (env, path, id) = crash_with_autosave(saved_rig(memo_ws("one", &[1])));
    // The show file was saved after the autosave (a save elsewhere that left it behind).
    let recovery = env.dirs.recovery_path(id, Some(&path));
    set_mtime(&path, 3_000_000);
    set_mtime(&recovery, 2_000_000);
    let mut r = restart(env, memo_ws("blank", &[5]));
    let replies = request(&mut r.s, open(&path));
    ok_result(&replies);
    assert!(matches!(
        outcome(&replies),
        Outcome::Ok(ok) if ok.data.is_none()
    ));
    assert!(!snapshot(&replies).document.dirty);
    // Moved aside, not deleted.
    assert!(
        names(&r.env.dirs.recovery_dir())
            .iter()
            .any(|n| n.contains(".stale."))
    );
}

#[test]
fn an_unusable_autosave_is_a_warning_not_a_failure() {
    let (env, path, id) = crash_with_autosave(saved_rig(memo_ws("one", &[1])));
    let recovery = env.dirs.recovery_path(id, Some(&path));
    fs::write(&recovery, "garbage").unwrap();
    set_mtime(&recovery, 5_000_000);
    let mut r = restart(env, memo_ws("blank", &[5]));
    let replies = request(&mut r.s, open(&path));
    ok_result(&replies);
    let snap = snapshot(&replies);
    assert!(
        snap.warnings.iter().any(|w| matches!(
            w,
            openlab_core::protocol::WireWarning::Notice { code, .. } if code == "recovery_unusable"
        )),
        "{:?}",
        snap.warnings
    );
}

#[test]
fn a_corrupt_show_offers_the_recovery_scan_and_open_recovery_restores_untitled() {
    let (env, path, id) = crash_with_autosave(saved_rig(memo_ws("one", &[1])));
    fs::write(&path, "{ corrupt").unwrap();
    let mut r = restart(env, memo_ws("blank", &[5]));
    assert_eq!(
        error_code(&request(&mut r.s, open(&path))),
        ErrorCode::BadShowFile
    );
    let entries = match data(&request(&mut r.s, Request::ScanRecovery)) {
        ResultData::RecoveryEntries { entries } => entries,
        other => panic!("{other:?}"),
    };
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.workspace_id, id);
    assert_eq!(entry.show_name.as_deref(), Some("one"));
    assert!(!entry.stale);
    assert!(entry.path_hash.is_some());

    // Nothing changed so far; now open the entry.
    assert_eq!(r.s.control().workspace().name, "blank");
    let replies = request(
        &mut r.s,
        Request::OpenRecovery {
            workspace_id: entry.workspace_id,
            path_hash: entry.path_hash.clone(),
            modified_ms: entry.modified_ms,
            discard_changes: false,
        },
    );
    ok_result(&replies);
    let snap = snapshot(&replies);
    assert_eq!(
        snap.workspace.find_cue(id_of(1)).unwrap().name,
        "unsaved work"
    );
    assert_eq!(
        snap.document.path, None,
        "untitled: a save has to choose a path"
    );
    assert!(snap.document.dirty);
    assert_eq!(
        error_code(&request(&mut r.s, Request::Save)),
        ErrorCode::NoWorkspace
    );
    let saved = r.env.show_dir().join("recovered.openlab");
    ok_result(&request(
        &mut r.s,
        Request::SaveAs {
            path: saved.to_string_lossy().into_owned(),
        },
    ));
    assert!(!r.s.control().is_unsaved());
    let on_disk = load_show(&fs::read_to_string(&saved).unwrap())
        .unwrap()
        .workspace;
    assert_eq!(on_disk.find_cue(id_of(1)).unwrap().name, "unsaved work");
}

#[test]
fn open_recovery_errors() {
    let mut r = rig(memo_ws("one", &[1]));
    let unknown = openlab_core::WorkspaceId(uuid::Uuid::from_u128(77));
    let replies = request(
        &mut r.s,
        Request::OpenRecovery {
            workspace_id: unknown,
            path_hash: None,
            modified_ms: None,
            discard_changes: false,
        },
    );
    assert_eq!(error_code(&replies), ErrorCode::NotFound);
    // An empty folder scans to an empty list.
    match data(&request(&mut r.s, Request::ScanRecovery)) {
        ResultData::RecoveryEntries { entries } => assert!(entries.is_empty()),
        other => panic!("{other:?}"),
    }
}

#[test]
fn only_one_document_request_at_a_time() {
    let mut r = rig(memo_ws("one", &[1]));
    let other = r.env.show_dir().join("other.openlab");
    write_show(&other, &memo_ws("other", &[7]));
    let (tx1, mut rx1) = reply_channel();
    let (tx2, mut rx2) = reply_channel();
    for (n, tx) in [(1, tx1), (2, tx2)] {
        r.s.deliver(Inbox::Client {
            message: ClientMessage::Request {
                id: RequestId(n),
                request: open(&other),
            },
            reply: tx,
        });
    }
    r.s.settle_docs();
    let second = rx2.try_recv().unwrap();
    assert!(matches!(
        second,
        ServerMessage::Result {
            outcome: Outcome::Error(ref e),
            ..
        } if e.code == ErrorCode::CommandRefused
    ));
    // The first completed normally.
    let kinds: Vec<_> = std::iter::from_fn(|| rx1.try_recv().ok()).collect();
    assert!(matches!(kinds[0], ServerMessage::Snapshot { .. }));
    assert_eq!(r.s.control().workspace().name, "other");
}

#[test]
fn an_edit_made_while_the_file_is_read_is_not_thrown_away() {
    let mut r = rig(memo_ws("one", &[1]));
    let other = r.env.show_dir().join("other.openlab");
    write_show(&other, &memo_ws("other", &[7]));
    let (tx, mut rx) = reply_channel();
    r.s.deliver(Inbox::Client {
        message: ClientMessage::Request {
            id: RequestId(1),
            request: open(&other),
        },
        reply: tx,
    });
    // The edit is handled before the worker's answer is.
    let id = r.s.request_id();
    let (etx, _erx) = reply_channel();
    r.s.deliver(Inbox::Client {
        message: ClientMessage::Command {
            id,
            command: Box::new(Command::SetCueProperty {
                id: show::id(1),
                property: CueProperty::Name {
                    value: "edited".into(),
                },
            }),
        },
        reply: etx,
    });
    r.s.settle_docs();
    assert!(matches!(
        rx.try_recv().unwrap(),
        ServerMessage::Result {
            outcome: Outcome::Error(ref e),
            ..
        } if e.code == ErrorCode::UnsavedChanges
    ));
    assert_eq!(r.s.control().workspace().name, "one");
    assert_eq!(cue_name(&r.s, 1), "edited");
}

#[test]
fn save_as_in_another_folder_keeps_relative_media_pointing_at_the_same_files() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    fs::create_dir_all(a.path().join("sfx")).unwrap();
    let sound = a.path().join("sfx").join("hit.wav");
    fs::copy(wav(&dc(800, 0.5)), &sound).unwrap();
    let mut ws = workspace(vec![audio(1, &sound, 0)]);
    if let Some(c) = ws.cue_lists[0].cues.first_mut()
        && let CueKind::Audio { media, .. } = &mut c.kind
    {
        *media = Some(MediaRef::new("sfx/hit.wav"));
    }
    let a_show = a.path().join("show.openlab");
    write_show(&a_show, &ws);
    let mut r = rig_with(ws, Some(a_show), None);
    r.s.settle_media();
    r.s.take_outbound();
    let b_show = b.path().join("copy.openlab");
    ok_result(&request(
        &mut r.s,
        Request::SaveAs {
            path: b_show.to_string_lossy().into_owned(),
        },
    ));
    let expected = {
        let mut p = sound.to_string_lossy().into_owned();
        if cfg!(windows) {
            p = p.replace('\\', "/");
        }
        p
    };
    let media_of = |ws: &Workspace| match &ws.find_cue(id(1)).unwrap().kind {
        CueKind::Audio { media, .. } => media.clone().unwrap().as_str().to_owned(),
        other => panic!("not an audio cue: {other:?}"),
    };
    let saved = load_show(&fs::read_to_string(&b_show).unwrap())
        .unwrap()
        .workspace;
    assert_eq!(media_of(&saved), expected, "the saved copy");
    assert_eq!(
        media_of(r.s.control().workspace()),
        expected,
        "the live workspace"
    );
    assert!(!r.s.control().is_unsaved());
    // Every client is told: the cues changed without an edit of theirs.
    let broadcast = r.s.take_outbound();
    let snap = broadcast
        .iter()
        .find_map(|m| match m {
            ServerMessage::Snapshot { snapshot } => Some(snapshot),
            _ => None,
        })
        .expect("snapshot");
    assert_eq!(media_of(&snap.workspace), expected);
    assert!(!snap.document.can_undo);
    // The file is still found from the new folder.
    r.s.settle_media();
    assert!(r.s.control().snapshot().problems.is_empty());
    // The original file is untouched.
    let original = load_show(&fs::read_to_string(a.path().join("show.openlab")).unwrap())
        .unwrap()
        .workspace;
    assert_eq!(media_of(&original), "sfx/hit.wav");
}

#[test]
fn without_an_application_folder_saving_is_an_io_error_not_a_crash() {
    let ws = memo_ws("one", &[1]);
    let c = config(&ws);
    let mut s = show_with(ws, c);
    let path = fresh_path("openlab");
    assert_eq!(
        error_code(&request(
            &mut s,
            Request::SaveAs {
                path: path.to_string_lossy().into_owned()
            }
        )),
        ErrorCode::Io
    );
    // Open and New still work.
    write_show(&path, &memo_ws("other", &[3]));
    ok_result(&request(&mut s, open(&path)));
    assert_eq!(s.control().workspace().name, "other");
}

#[cfg(unix)]
#[test]
fn opening_a_fifo_is_an_io_error_not_a_hang() {
    let mut r = rig(memo_ws("keep", &[1]));
    let fifo = r.env.show_dir().join("pipe.openlab");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success());
    // Answered at once (`request` fails the test if the worker does not answer in time), and
    // the next request is not refused.
    assert_eq!(error_code(&request(&mut r.s, open(&fifo))), ErrorCode::Io);
    ok_result(&request(
        &mut r.s,
        Request::New {
            discard_changes: false,
        },
    ));
}

/// A document hook that blocks its first call until released (a read stuck on a stalled
/// volume); later calls go through.
fn stall_first_read() -> (DocHook, mpsc::Receiver<()>, mpsc::Sender<()>) {
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

#[test]
fn a_stalled_read_times_out_with_io_and_the_next_request_works() {
    let (hook, entered, release) = stall_first_read();
    let mut r = rig_tweak(memo_ws("one", &[1]), |c| {
        c.doc_hook = Some(hook);
        c.doc_timeout = std::time::Duration::from_secs(20);
    });
    let stuck_show = r.env.show_dir().join("stuck.openlab");
    write_show(&stuck_show, &memo_ws("stuck", &[8]));
    let other = r.env.show_dir().join("other.openlab");
    write_show(&other, &memo_ws("other", &[7]));
    let (tx, mut rx) = reply_channel();
    r.s.deliver(Inbox::Client {
        message: ClientMessage::Request {
            id: RequestId(1),
            request: open(&stuck_show),
        },
        reply: tx,
    });
    entered.recv().unwrap();
    // Still waiting just short of the timeout; a second request is refused meanwhile.
    r.clock.advance(19_999);
    r.s.tick();
    assert!(rx.try_recv().is_err());
    let (rtx, mut rrx) = reply_channel();
    r.s.deliver(Inbox::Client {
        message: ClientMessage::Request {
            id: RequestId(2),
            request: Request::ScanRecovery,
        },
        reply: rtx,
    });
    assert!(matches!(
        rrx.try_recv().unwrap(),
        ServerMessage::Result {
            outcome: Outcome::Error(ref e),
            ..
        } if e.code == ErrorCode::CommandRefused
    ));
    r.clock.advance(1);
    r.s.tick();
    assert!(matches!(
        rx.try_recv().unwrap(),
        ServerMessage::Result {
            outcome: Outcome::Error(ref e),
            ..
        } if e.code == ErrorCode::Io
    ));
    assert_eq!(r.s.control().stuck_doc_workers(), 1);
    // A fresh worker serves what comes next (the shared queue), over the stuck one.
    ok_result(&request(
        &mut r.s,
        Request::New {
            discard_changes: false,
        },
    ));
    let replies = request(&mut r.s, open(&other));
    ok_result(&replies);
    assert_eq!(snapshot(&replies).workspace.name, "other");
    // The stuck read comes back much later: its answer is stale and changes nothing, and the
    // worker leaves.
    release.send(()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while r.s.control().stuck_doc_workers() > 0 && std::time::Instant::now() < deadline {
        thread::yield_now();
    }
    assert_eq!(r.s.control().stuck_doc_workers(), 0);
    r.s.tick();
    assert_eq!(r.s.control().workspace().name, "other");
    assert!(
        rx.try_recv().is_err(),
        "no second answer to the timed-out request"
    );
}

#[test]
fn a_superseded_save_is_answered_with_the_save_that_replaced_it_even_when_the_first_finished() {
    let (entered_tx, entered) = mpsc::channel::<()>();
    let (release_tx, release) = mpsc::channel::<()>();
    let release = Arc::new(Mutex::new(release));
    let entered_tx = Mutex::new(entered_tx);
    let hook = SaveHook(Arc::new(move |kind: JobKind| {
        if kind == JobKind::Save {
            let _ = entered_tx.lock().unwrap().send(());
            let _ = release.lock().unwrap().recv();
        }
    }));
    let mut r = rig_with(memo_ws("one", &[1]), None, Some(hook));
    let _unblock = Unblock(release_tx.clone());
    let path = r.env.show_path();
    let save = |r: &mut Rig| {
        let (tx, rx) = reply_channel();
        let id = r.s.request_id();
        r.s.deliver(Inbox::Client {
            message: ClientMessage::Request {
                id,
                request: Request::SaveAs {
                    path: path.to_string_lossy().into_owned(),
                },
            },
            reply: tx,
        });
        rx
    };
    let mut s1 = save(&mut r);
    entered.recv().unwrap(); // s1 is running
    // Each save holds an edit the previous one lacks, so only a later save may answer for an
    // earlier one.
    let edit = |r: &mut Rig, name: &str| {
        let (tx, _rx) = reply_channel();
        let id = r.s.request_id();
        r.s.deliver(Inbox::Client {
            message: ClientMessage::Command {
                id,
                command: Box::new(Command::SetCueProperty {
                    id: show::id(1),
                    property: CueProperty::Name { value: name.into() },
                }),
            },
            reply: tx,
        });
    };
    edit(&mut r, "two");
    let mut s2 = save(&mut r);
    edit(&mut r, "three");
    let mut s3 = save(&mut r); // replaces s2 in the writer's queue
    release_tx.send(()).unwrap(); // s1 finishes
    entered.recv().unwrap(); // s3 is running
    r.s.tick();
    // s1 is answered; s2 waits for s3 (it is not a failure); s3 is still running.
    assert!(matches!(
        s1.try_recv().unwrap(),
        ServerMessage::Result {
            outcome: Outcome::Ok(_),
            ..
        }
    ));
    let got = s2.try_recv();
    assert!(
        got.is_err(),
        "s2 waits for the save that replaced it: {got:?}"
    );
    assert!(s3.try_recv().is_err());
    release_tx.send(()).unwrap();
    r.s.settle_docs();
    for rx in [&mut s2, &mut s3] {
        assert!(matches!(
            rx.try_recv().unwrap(),
            ServerMessage::Result {
                outcome: Outcome::Ok(_),
                ..
            }
        ));
    }
}

#[test]
fn an_autosave_in_flight_cannot_bring_back_edits_the_operator_discarded() {
    for revert in [true, false] {
        let (entered_tx, entered) = mpsc::channel::<()>();
        let (release_tx, release) = mpsc::channel::<()>();
        let release = Arc::new(Mutex::new(release));
        let entered_tx = Mutex::new(entered_tx);
        let hook = SaveHook(Arc::new(move |kind: JobKind| {
            if kind == JobKind::Autosave {
                let _ = entered_tx.lock().unwrap().send(());
                let _ = release.lock().unwrap().recv();
            }
        }));
        let mut r = rig_with(memo_ws("one", &[1]), None, Some(hook));
        let path = r.env.show_path();
        ok_result(&request(
            &mut r.s,
            Request::SaveAs {
                path: path.to_string_lossy().into_owned(),
            },
        ));
        rename(&mut r.s, 1, "edited");
        r.s.tick();
        r.clock.advance(31_000);
        r.s.tick();
        entered.recv().unwrap(); // the autosave of the edit is mid-write
        let (tx, mut rx) = reply_channel();
        let id = r.s.request_id();
        let what = if revert {
            Request::Revert {
                discard_changes: true,
            }
        } else {
            Request::New {
                discard_changes: true,
            }
        };
        r.s.deliver(Inbox::Client {
            message: ClientMessage::Request { id, request: what },
            reply: tx,
        });
        // The read is answered by the worker; apply it (the writer is still blocked).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut replies = Vec::new();
        while !replies
            .iter()
            .any(|m| matches!(m, ServerMessage::Result { .. }))
        {
            r.s.tick();
            replies.extend(std::iter::from_fn(|| rx.try_recv().ok()));
            assert!(std::time::Instant::now() < deadline, "no answer");
            thread::yield_now();
        }
        release_tx.send(()).unwrap();
        r.s.settle_docs();
        // The autosave landed, and the removal ran after it: nothing is left to offer.
        assert!(
            names(&r.env.dirs.recovery_dir()).is_empty(),
            "revert {revert}: {:?}",
            names(&r.env.dirs.recovery_dir())
        );
        if revert {
            let replies = request(&mut r.s, open(&path));
            assert!(matches!(outcome(&replies), Outcome::Ok(ok) if ok.data.is_none()));
        }
    }
}

#[test]
fn reopening_the_same_show_spelled_differently_keeps_the_autosave_just_restored() {
    let (env, path, id) = crash_with_autosave(saved_rig(memo_ws("one", &[1])));
    let mut r = restart(env, memo_ws("blank", &[5]));
    // Restore it, edit on top, then reopen with discard under another spelling of the path.
    let spelled = format!("{}/./My Show.openlab", r.env.show_dir().display());
    assert_ne!(spelled, path.to_string_lossy());
    ok_result(&request(
        &mut r.s,
        Request::Open {
            path: path.to_string_lossy().into_owned(),
            discard_changes: false,
            recovery: Some(RecoveryChoice::Restore),
        },
    ));
    let replies = request(
        &mut r.s,
        Request::Open {
            path: spelled,
            discard_changes: true,
            recovery: Some(RecoveryChoice::Restore),
        },
    );
    ok_result(&replies);
    assert_eq!(
        snapshot(&replies)
            .workspace
            .find_cue(id_of(1))
            .unwrap()
            .name,
        "unsaved work"
    );
    assert!(
        r.env.dirs.recovery_path(id, Some(&path)).exists(),
        "the autosave restored from is not deleted by the discard"
    );
}
