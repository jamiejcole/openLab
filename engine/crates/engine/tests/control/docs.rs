// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! Documents through the control loop on the offline driver (NFR-04, NFR-05, NFR-06,
//! WS-02, WS-03, WS-06, WS-07, WS-08): new, open, open-recovery, close, save, save-as, autosave
//! and recovery, with a virtual wall clock and the persistence layer on a temp folder.
//!
//! Most tests follow one workspace through its life: `request` closes the docs open before one
//! that `new`, `open` or `open_recovery` opened, so the accessors of `control()` (the first
//! doc's) show it, and hands its snapshot back in front of its result as a connection bound to
//! it would get it.

use crate::show;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::common::{Env, env, names, set_mtime};
use crate::show::*;
use openlab_core::protocol::{
    ClientMessage, ErrorCode, OkBody, Outcome, RecoveryChoice, Request, RequestId, ResultData,
    ServerMessage, Snapshot, WireCueState, WireProblem,
};
use openlab_core::{Command, CueKind, CueProperty, MediaRef, Workspace, load_show, save_show};
use openlab_engine::control::offline::OfflineShow;
use openlab_engine::control::{DocHook, Inbox, ManualClock, SaveHook, reply_channel};
use openlab_engine::persist::JobKind;

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
    let mut replies = s.send(ClientMessage::Request { id, request });
    assert_eq!(s.stalls(), 0, "a worker did not answer in time");
    let opened = replies.iter().find_map(|m| match m {
        ServerMessage::Result {
            outcome:
                Outcome::Ok(OkBody {
                    data: Some(ResultData::Opened { doc }),
                    ..
                }),
            ..
        } => Some(*doc),
        _ => None,
    });
    if let Some(doc) = opened {
        for other in s.control().doc_ids() {
            if other != doc {
                s.close_doc(other, true).unwrap();
            }
        }
        let snapshot = Box::new(s.control().doc(doc).unwrap().snapshot());
        replies.insert(0, ServerMessage::Snapshot { snapshot });
    }
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
        coalesce: None,
    });
    ok_result(&replies);
}

fn write_show(path: &Path, ws: &Workspace) {
    fs::write(path, save_show(ws).unwrap()).unwrap();
}

/// A workspace of memos, with an id of its name's: shows of other names can be open together
/// (one with the id of an open one opens as a copy, with a fresh id).
fn memo_ws(name: &str, cues: &[u128]) -> Workspace {
    let mut ws = workspace(cues.iter().map(|&n| cue(n, CueKind::Memo)).collect());
    ws.name = name.into();
    let hash = name.bytes().fold(0xcbf2_9ce4_8422_2325_u128, |h, b| {
        (h ^ u128::from(b)).wrapping_mul(0x1b3)
    });
    ws.id = openlab_core::WorkspaceId(uuid::Uuid::from_u128(hash));
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
        recovery: None,
    }
}

#[test]
fn new_save_as_and_open_round_trip_through_the_protocol() {
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

    // New: an empty untitled workspace, never saved (Save has no path).
    let replies = request(&mut r.s, Request::New {});
    ok_result(&replies);
    let snap = snapshot(&replies);
    assert_eq!(snap.document.path, None);
    assert!(!snap.document.dirty);
    assert_eq!(snap.workspace.cue_lists.len(), 1);
    assert!(snap.workspace.cue_lists[0].cues.is_empty());
    assert_ne!(snap.workspace.id, saved.id);
    // QLab's default OSC access: nothing without a passcode, one 4-digit passcode with
    // all rights (the digits are random).
    let access = &snap.workspace.settings.osc_access;
    assert_eq!(access.no_passcode, openlab_core::OscRights::NONE);
    assert_eq!(access.passcodes.len(), 1);
    assert_eq!(access.passcodes[0].rights, openlab_core::OscRights::ALL);
    assert!(
        access.passcodes[0].passcode.len() == 4
            && access.passcodes[0]
                .passcode
                .bytes()
                .all(|b| b.is_ascii_digit())
    );
    assert_eq!(
        error_code(&request(&mut r.s, Request::Save)),
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
fn discarding_edits_removes_their_autosave_so_it_is_not_offered_later() {
    let mut r = saved_rig(memo_ws("one", &[1]));
    let path = r.env.show_path();
    rename(&mut r.s, 1, "edited");
    r.s.tick();
    r.clock.advance(31_000);
    r.s.settle_docs();
    let recovery = r.env.dirs.recovery_dir();
    assert_eq!(names(&recovery).len(), 1, "autosaved");
    // Closing it refuses while it has unsaved edits, then discards them on request.
    let close = |r: &mut Rig, discard_changes| {
        let id = r.s.request_id();
        r.s.send(ClientMessage::Close {
            id,
            doc: None,
            discard_changes,
        })
    };
    assert_eq!(error_code(&close(&mut r, false)), ErrorCode::UnsavedChanges);
    assert_eq!(names(&recovery).len(), 1, "kept while refused");
    ok_result(&close(&mut r, true));
    assert!(r.s.control().doc_ids().is_empty());
    r.s.settle_docs();
    // The worker removes it asynchronously: wait for it through the inbox-free path.
    crate::show::spin::spin_until("the recovery file to be removed", || {
        names(&recovery).is_empty()
    });
    assert!(names(&recovery).is_empty(), "{:?}", names(&recovery));
    // Opening the show now offers nothing.
    let replies = request(&mut r.s, open(&path));
    ok_result(&replies);
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
            coalesce: None,
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
    ws.settings.osc_access = openlab_core::OscAccess::new_workspace(77);
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
    // Closed first: an open show cannot be opened again.
    let first = r.s.control().doc_ids()[0];
    r.s.close_doc(first, false).unwrap();
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

/// The name of cue 1 in the recovery file of `workspace` at `show`.
fn recovered_at(r: &Rig, workspace: openlab_core::WorkspaceId, show: &Path) -> String {
    let file = r.env.dirs.recovery_path(workspace, Some(show));
    load_show(&fs::read_to_string(file).unwrap())
        .unwrap()
        .workspace
        .find_cue(id(1))
        .unwrap()
        .name
        .clone()
}

/// Save As from P1 to P2 with no edit since it began: P1's autosave holds nothing P2 does not,
/// so it goes, and is not offered again beside P2 (#383).
#[test]
fn save_as_removes_the_autosave_left_at_the_old_path() {
    let mut ws = memo_ws("one", &[1]);
    ws.settings.autosave_interval = 30.0;
    let mut r = saved_rig(ws);
    let workspace = r.s.control().workspace().id;
    let old = r.env.show_path();
    rename(&mut r.s, 1, "edited");
    r.s.tick();
    r.clock.advance(30_000);
    r.s.settle_docs();
    assert_eq!(recovered_at(&r, workspace, &old), "edited");
    let new = r.env.show_dir().join("moved.openlab");
    ok_result(&request(
        &mut r.s,
        Request::SaveAs {
            path: new.to_string_lossy().into_owned(),
        },
    ));
    r.s.settle_docs();
    assert!(names(&r.env.dirs.recovery_dir()).is_empty());
    assert!(matches!(
        data(&request(&mut r.s, Request::ScanRecovery)),
        ResultData::RecoveryEntries { entries } if entries.is_empty()
    ));
}

/// An edit made while the Save As to P2 is written is in P1's autosave and not in P2: that
/// autosave stays until an autosave at P2 holds the edit, and while another workspace has P1
/// open it stays, since it may be that one's (#383).
#[test]
fn an_autosave_left_at_the_old_path_stays_while_it_holds_edits_or_another_workspace_has_it() {
    let hold = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (entered_tx, entered) = mpsc::channel::<()>();
    let (release_tx, release) = mpsc::channel::<()>();
    let release = Arc::new(Mutex::new(release));
    let entered_tx = Mutex::new(entered_tx);
    let held = Arc::clone(&hold);
    let hook = SaveHook(Arc::new(move |kind: JobKind| {
        if kind == JobKind::Save && held.load(std::sync::atomic::Ordering::SeqCst) {
            let _ = entered_tx.lock().unwrap().send(());
            let _ = release.lock().unwrap().recv();
        }
    }));
    let mut ws = memo_ws("one", &[1]);
    ws.settings.autosave_interval = 30.0;
    let mut r = rig_with(ws, None, Some(hook));
    let _unblock = Unblock(release_tx.clone());
    let old = r.env.show_path();
    ok_result(&request(
        &mut r.s,
        Request::SaveAs {
            path: old.to_string_lossy().into_owned(),
        },
    ));
    let workspace = r.s.control().workspace().id;
    let recovery = r.env.dirs.recovery_dir();
    // Autosaved long ago, so the next edit is autosaved at once (no clock moves while the save
    // is held: the clock also times a stalled write).
    rename(&mut r.s, 1, "before");
    r.s.tick();
    r.clock.advance(30_000);
    r.s.settle_docs();
    r.clock.advance(60_000);
    r.s.settle_docs();
    assert_eq!(recovered_at(&r, workspace, &old), "before");

    // Save As to P2, held on the writer; an edit meanwhile is autosaved at P1 once it is done.
    hold.store(true, std::sync::atomic::Ordering::SeqCst);
    let new = r.env.show_dir().join("moved.openlab");
    let (tx, mut answer) = reply_channel();
    let request_id = r.s.request_id();
    r.s.deliver(Inbox::Client {
        message: ClientMessage::Request {
            id: request_id,
            request: Request::SaveAs {
                path: new.to_string_lossy().into_owned(),
            },
        },
        reply: tx,
    });
    r.s.tick();
    entered.recv().unwrap();
    // Delivered, not sent: sending waits for the writer.
    let (tx, _edited) = reply_channel();
    let edit_id = r.s.request_id();
    r.s.deliver(Inbox::Client {
        message: ClientMessage::Command {
            id: edit_id,
            command: Box::new(Command::SetCueProperty {
                id: id(1),
                property: CueProperty::Name {
                    value: "during".into(),
                },
            }),
            coalesce: None,
        },
        reply: tx,
    });
    r.s.tick();
    hold.store(false, std::sync::atomic::Ordering::SeqCst);
    release_tx.send(()).unwrap();
    r.s.settle_docs();
    let got = answer.try_recv();
    assert!(
        matches!(
            got,
            Ok(ServerMessage::Result {
                outcome: Outcome::Ok(_),
                ..
            })
        ),
        "{got:?}"
    );
    assert!(!fs::read_to_string(&new).unwrap().contains("during"));
    assert_eq!(
        recovered_at(&r, workspace, &old),
        "during",
        "kept: P2 lacks it"
    );

    // Another workspace opens P1: P1's autosave may be its, so an autosave at P2 leaves it.
    let other =
        r.s.open_doc(openlab_engine::control::DocSource::Open {
            path: old.clone(),
            recovery: Some(RecoveryChoice::Ignore),
        })
        .unwrap();
    rename(&mut r.s, 1, "after");
    r.clock.advance(30_000);
    r.s.settle_docs();
    assert_eq!(recovered_at(&r, workspace, &new), "after");
    assert_eq!(
        recovered_at(&r, workspace, &old),
        "during",
        "P1 is open elsewhere"
    );

    // Closed again (a copy with a fresh id, so unsaved): the next autosave at P2 removes it.
    r.s.close_doc(other, true).unwrap();
    rename(&mut r.s, 1, "later");
    r.clock.advance(30_000);
    r.s.settle_docs();
    assert_eq!(recovered_at(&r, workspace, &new), "later");
    assert_eq!(
        names(&recovery),
        vec![
            r.env
                .dirs
                .recovery_path(workspace, Some(&new))
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        ]
    );
}

/// A recovery file the operator kept when opening P1 (`ignore`: offered again next time) holds
/// edits that are nowhere else: a Save As to P2 without an edit leaves it alone, since this doc
/// never wrote it (#383).
#[test]
fn a_recovery_file_kept_at_open_survives_a_save_as_elsewhere() {
    let mut ws = memo_ws("one", &[1]);
    ws.settings.autosave_interval = 30.0;
    let mut r = saved_rig(ws);
    let workspace = r.s.control().workspace().id;
    let old = r.env.show_path();
    // Unsaved edits autosaved at P1 (as a crashed run leaves them).
    rename(&mut r.s, 1, "crash");
    r.s.tick();
    r.clock.advance(30_000);
    r.s.settle_docs();
    assert_eq!(recovered_at(&r, workspace, &old), "crash");
    let file = r.env.dirs.recovery_path(workspace, Some(&old));
    let bytes = fs::read(&file).unwrap();
    let doc = r.s.control().doc_ids()[0];
    r.s.close_doc(doc, true).unwrap();
    r.s.settle_docs();
    fs::write(&file, &bytes).unwrap();
    // The operator opens P1 and ignores the offered recovery ("offered again next time").
    r.s.open_doc(openlab_engine::control::DocSource::Open {
        path: old.clone(),
        recovery: Some(RecoveryChoice::Ignore),
    })
    .unwrap();
    assert_eq!(r.s.control().workspace().id, workspace);
    assert!(file.exists(), "ignored recovery kept after open");
    let new = r.env.show_dir().join("copy.openlab");
    ok_result(&request(
        &mut r.s,
        Request::SaveAs {
            path: new.to_string_lossy().into_owned(),
        },
    ));
    r.s.settle_docs();
    r.s.tick();
    r.s.settle_docs();
    assert!(
        !fs::read_to_string(&new).unwrap().contains("crash"),
        "P2 lacks the crash edits"
    );
    assert!(
        file.exists(),
        "the kept recovery of P1, whose edits are nowhere else, was deleted"
    );
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

/// The app tells another copy's autosave from the open doc's own by the `path_hash` in
/// `workspaces`, so it must be the hash the autosave is named and listed by.
#[test]
fn the_workspace_list_carries_the_hash_its_autosave_is_named_by() {
    let mut r = saved_rig(memo_ws("one", &[1]));
    rename(&mut r.s, 1, "edited");
    r.s.tick();
    r.clock.advance(31_000);
    r.s.settle_docs();
    let entries = match data(&request(&mut r.s, Request::ScanRecovery)) {
        ResultData::RecoveryEntries { entries } => entries,
        other => panic!("{other:?}"),
    };
    assert_eq!(entries.len(), 1);
    let listed = r.s.control().workspace_list();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].path_hash.is_some());
    assert_eq!(listed[0].path_hash, entries[0].path_hash);
    let id = r.s.control().workspace().id;
    assert_eq!(
        names(&r.env.dirs.recovery_dir()),
        vec![format!(
            "{id}-{}.openlab",
            entries[0].path_hash.as_deref().unwrap()
        )]
    );
    // A doc never saved lists no hash, as its autosave is named by its id alone.
    let untitled = rig(memo_ws("two", &[2]));
    assert_eq!(untitled.s.control().workspace_list()[0].path_hash, None);
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
    assert!(
        matches!(data(&replies), ResultData::Opened { .. }),
        "opened, no offer"
    );
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
    // The first completed normally: another doc is open beside the first.
    let kinds: Vec<_> = std::iter::from_fn(|| rx1.try_recv().ok()).collect();
    assert!(matches!(
        kinds[..],
        [ServerMessage::Result {
            outcome: Outcome::Ok(_),
            ..
        }]
    ));
    let docs = r.s.control().doc_ids();
    assert_eq!(docs.len(), 2);
    assert_eq!(
        r.s.control().doc(docs[1]).unwrap().workspace().name,
        "other"
    );
}

/// The doc open keeps playing and editing while another is read (the file opens beside
/// it; the edit is not thrown away, and the open is not refused for it).
#[test]
fn an_edit_made_while_a_file_is_read_stays_in_its_doc() {
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
            coalesce: None,
        },
        reply: etx,
    });
    r.s.settle_docs();
    assert!(matches!(
        rx.try_recv().unwrap(),
        ServerMessage::Result {
            outcome: Outcome::Ok(_),
            ..
        }
    ));
    assert_eq!(r.s.control().workspace().name, "one");
    assert_eq!(cue_name(&r.s, 1), "edited");
    assert!(r.s.control().is_unsaved());
    assert_eq!(r.s.control().doc_ids().len(), 2);
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
    // The file is still found from the new folder, and plays: an empty catalog would show no
    // problem either.
    r.s.settle_media();
    assert!(r.s.control().snapshot().problems.is_empty());
    let from = r.s.audio().len();
    start(&mut r.s, 1);
    r.s.run(BLOCK * 20);
    assert!(
        r.s.audio()[from..].iter().any(|x| *x != 0.0),
        "the cue plays after the folder change"
    );
    // The original file is untouched.
    let original = load_show(&fs::read_to_string(a.path().join("show.openlab")).unwrap())
        .unwrap()
        .workspace;
    assert_eq!(media_of(&original), "sfx/hit.wav");
}

/// Two cues with relative media in folder A, shown as a saved show; Save As into folder B
/// must re-spell both references.
fn two_relative_cues() -> (tempfile::TempDir, tempfile::TempDir, Rig, PathBuf) {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    fs::create_dir_all(a.path().join("sfx")).unwrap();
    let sound = a.path().join("sfx").join("hit.wav");
    fs::copy(wav(&dc(800, 0.5)), &sound).unwrap();
    let mut ws = workspace(vec![audio(1, &sound, 0), audio(2, &sound, 0)]);
    for c in &mut ws.cue_lists[0].cues {
        if let CueKind::Audio { media, .. } = &mut c.kind {
            *media = Some(MediaRef::new("sfx/hit.wav"));
        }
    }
    let a_show = a.path().join("show.openlab");
    write_show(&a_show, &ws);
    let mut r = rig_with(ws, Some(a_show), None);
    r.s.settle_media();
    r.s.take_outbound();
    let b_show = b.path().join("copy.openlab");
    (a, b, r, b_show)
}

fn media_of(ws: &Workspace, n: u128) -> String {
    match &ws.find_cue(id(n)).unwrap().kind {
        CueKind::Audio { media, .. } => media.clone().unwrap().as_str().to_owned(),
        other => panic!("not an audio cue: {other:?}"),
    }
}

fn error_logs(out: &[ServerMessage]) -> Vec<&str> {
    out.iter()
        .filter_map(|m| match m {
            ServerMessage::Log {
                level: openlab_core::protocol::LogLevel::Error,
                message,
            } => Some(message.as_str()),
            _ => None,
        })
        .collect()
}

/// A save-as to another folder re-spells relative references in the live workspace: that edit
/// reaches the cue engine with no signal by hand (the workspace's write guard tells it).
#[test]
fn a_save_as_that_respells_references_reaches_the_cue_engine_without_a_signal() {
    let (_a, _b, mut r, b_show) = two_relative_cues();
    assert_engine_follows(&mut r.s, |s| {
        ok_result(&request(
            s,
            Request::SaveAs {
                path: b_show.to_string_lossy().into_owned(),
            },
        ));
    });
    assert_ne!(media_of(r.s.control().workspace(), 1), "sfx/hit.wav");
}

#[test]
fn a_relink_of_the_save_copy_that_cannot_roll_back_saves_the_workspace_as_it_is() {
    let (_a, _b, mut r, b_show) = two_relative_cues();
    // The copy's relink fails at its second cue and cannot be undone; the live relink after the
    // save is fine.
    openlab_core::fault::inject(0, 1, true);
    ok_result(&request(
        &mut r.s,
        Request::SaveAs {
            path: b_show.to_string_lossy().into_owned(),
        },
    ));
    let out = r.s.take_outbound();
    assert!(
        error_logs(&out)
            .iter()
            .any(|m| m.contains("partly changed")),
        "{out:?}"
    );
    // The file holds neither half-relinked reference: it is the workspace as it was.
    let saved = load_show(&fs::read_to_string(&b_show).unwrap())
        .unwrap()
        .workspace;
    assert_eq!(media_of(&saved, 1), "sfx/hit.wav");
    assert_eq!(media_of(&saved, 2), "sfx/hit.wav");
    // Not rebased, as for any refused relink: the live workspace matches the file.
    assert_eq!(r.s.control().workspace(), &saved);
}

#[test]
fn a_relink_after_a_folder_change_that_cannot_roll_back_is_an_unundoable_edit() {
    let (_a, _b, mut r, b_show) = two_relative_cues();
    // The save copy's relink is fine; the live workspace's fails at its second cue.
    openlab_core::fault::inject(1, 1, true);
    ok_result(&request(
        &mut r.s,
        Request::SaveAs {
            path: b_show.to_string_lossy().into_owned(),
        },
    ));
    let out = r.s.take_outbound();
    assert!(
        error_logs(&out)
            .iter()
            .any(|m| m.contains("partly changed")),
        "{out:?}"
    );
    // The saved copy is fully rebased; the live one has only the first cue re-spelled, and
    // every client is sent that whole model.
    let saved = load_show(&fs::read_to_string(&b_show).unwrap())
        .unwrap()
        .workspace;
    assert_ne!(media_of(&saved, 2), "sfx/hit.wav");
    let live = r.s.control().workspace();
    assert_eq!(media_of(live, 1), media_of(&saved, 1));
    assert_eq!(media_of(live, 2), "sfx/hit.wav");
    let snap = out
        .iter()
        .find_map(|m| match m {
            ServerMessage::Snapshot { snapshot } => Some(snapshot),
            _ => None,
        })
        .expect("snapshot");
    assert_eq!(&snap.workspace, live);
    assert!(!snap.document.can_undo);
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
    ok_result(&request(&mut r.s, Request::New {}));
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
    ok_result(&request(&mut r.s, Request::New {}));
    let replies = request(&mut r.s, open(&other));
    ok_result(&replies);
    assert_eq!(snapshot(&replies).workspace.name, "other");
    // The stuck read comes back much later: its answer is stale and changes nothing, and the
    // worker leaves.
    release.send(()).unwrap();
    crate::show::spin::spin_until("the stuck worker to leave", || {
        r.s.control().stuck_doc_workers() == 0
    });
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
                coalesce: None,
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

/// A show with a missing file on cue 1 and a present one on cue 2.
fn media_ws() -> Workspace {
    let present = wav(&dc(2000, 0.5));
    let missing = fresh_path("wav");
    assert!(!missing.exists());
    workspace(vec![audio(1, &missing, 0), audio(2, &present, 0)])
}

/// The media catalog of the replaced workspace is live: the missing file is a problem of its
/// cue, the present file plays (a catalog that was reset and never refilled has neither).
fn assert_media_is_live(s: &mut OfflineShow, what: &str) {
    s.settle_media();
    let problems = s.control().snapshot().problems;
    assert!(
        problems.iter().any(|p| p.cue == id(1)
            && p.problems
                .iter()
                .any(|x| matches!(x, WireProblem::MediaMissing { .. }))),
        "{what}: no MediaMissing for the missing file: {problems:?}"
    );
    assert!(
        !problems.iter().any(|p| p.cue == id(2)),
        "{what}: {problems:?}"
    );
    let from = s.audio().len();
    start(s, 2);
    s.run(BLOCK * 20);
    assert!(
        s.audio()[from..].iter().any(|x| *x != 0.0),
        "{what}: the present file's cue is silent"
    );
    assert_eq!(s.stalls(), 0);
}

#[test]
fn media_is_probed_again_after_open() {
    let mut r = rig(memo_ws("blank", &[5]));
    let path = r.env.show_dir().join("media.openlab");
    write_show(&path, &media_ws());
    ok_result(&request(&mut r.s, open(&path)));
    assert_media_is_live(&mut r.s, "open");
}

#[test]
fn media_is_probed_again_after_open_recovery() {
    let mut r = rig(media_ws());
    let path = r.env.show_path();
    ok_result(&request(
        &mut r.s,
        Request::SaveAs {
            path: path.to_string_lossy().into_owned(),
        },
    ));
    let (env, path, id) = crash_with_autosave(r);
    // The show file is damaged: only the autosave is left.
    fs::write(&path, "{ corrupt").unwrap();
    let mut r = restart(env, memo_ws("blank", &[5]));
    ok_result(&request(
        &mut r.s,
        Request::OpenRecovery {
            workspace_id: id,
            path_hash: None,
            modified_ms: None,
        },
    ));
    assert_media_is_live(&mut r.s, "open_recovery");
}

#[test]
fn an_answer_already_in_the_inbox_when_the_timeout_fires_is_not_reported_as_io() {
    let mut r = rig_tweak(memo_ws("one", &[1]), |c| {
        c.doc_timeout = std::time::Duration::from_secs(20);
    });
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
    // The worker has read the file and posted its answer, which nobody has applied yet.
    crate::show::spin::spin_until("the worker to post its answer", || {
        r.s.control().doc_answers_posted() != 0
    });
    assert_eq!(r.s.control().doc_answers_posted(), 1);
    r.clock.advance(25_000);
    // A tick that does not apply the inbox first: the timeout check runs with the answer unread.
    r.s.control_mut().tick();
    assert!(rx.try_recv().is_err(), "not failed with io");
    assert_eq!(r.s.control().stuck_doc_workers(), 0);
    r.s.tick();
    assert!(matches!(
        rx.try_recv().unwrap(),
        ServerMessage::Result {
            outcome: Outcome::Ok(OkBody {
                data: Some(ResultData::Opened { .. }),
                ..
            }),
            ..
        }
    ));
    let docs = r.s.control().doc_ids();
    assert_eq!(
        r.s.control().doc(docs[1]).unwrap().workspace().name,
        "other"
    );
}

/// `new` and `open` open another doc beside the one open, which keeps playing; the
/// requester gets `opened { doc }` and no snapshot (a connection bound to the doc gets that),
/// nothing is broadcast as a snapshot, and the app
/// connections get the list with both. Closing the first then stops its cue.
#[test]
fn another_doc_opens_beside_a_playing_one_and_closing_it_stops_its_cues() {
    let path = wav(&dc(16_000, 0.5));
    let mut r = saved_rig(workspace(vec![audio(1, &path, 0), cue(2, CueKind::Memo)]));
    let other = r.env.show_dir().join("other.openlab");
    write_show(&other, &memo_ws("other", &[9]));
    start(&mut r.s, 1);
    r.s.run(BLOCK * 8);
    let first = r.s.control().doc_ids()[0];
    for (n, opened) in [Request::New {}, open(&other)].into_iter().enumerate() {
        r.s.take_outbound();
        let rid = r.s.request_id();
        let replies = r.s.send(ClientMessage::Request {
            id: rid,
            request: opened,
        });
        assert!(
            matches!(
                &replies[..],
                [ServerMessage::Result {
                    outcome: Outcome::Ok(OkBody {
                        data: Some(ResultData::Opened { .. }),
                        ..
                    }),
                    ..
                }]
            ),
            "{replies:?}"
        );
        r.s.run(BLOCK * 4);
        let out = r.s.take_outbound();
        assert!(
            !out.iter()
                .any(|m| matches!(m, ServerMessage::Snapshot { .. })),
            "no snapshot broadcast"
        );
        assert!(out.iter().any(|m| matches!(
            m,
            ServerMessage::Workspaces { workspaces } if workspaces.len() == n + 2
        )));
        assert_eq!(
            r.s.control().engine().state(id(1)),
            openlab_core::CueState::Running,
            "the first doc plays on"
        );
    }
    r.s.close_doc(first, false).unwrap();
    let out = r.s.take_outbound();
    assert!(
        states(&out, id(1))
            .iter()
            .any(|(s, _)| *s == WireCueState::Idle),
        "its clients are told the cue stopped"
    );
    assert!(
        out.iter()
            .any(|m| matches!(m, ServerMessage::Closed { doc } if *doc == first))
    );
    let from = r.s.audio().len() + usize::try_from(BLOCK).unwrap() * 2 * 2;
    r.s.run(BLOCK * 40);
    assert!(r.s.audio()[from..].iter().all(|x| *x == 0.0));
}

/// A show that is open answers `already_open`, also under another spelling of its path.
#[test]
fn opening_an_open_show_again_is_already_open() {
    let mut r = saved_rig(memo_ws("one", &[1]));
    let path = r.env.show_path();
    let spelled = format!("{}/./My Show.openlab", r.env.show_dir().display());
    assert_ne!(spelled, path.to_string_lossy());
    for again in [path.to_string_lossy().into_owned(), spelled] {
        let replies = request(
            &mut r.s,
            Request::Open {
                path: again,
                recovery: None,
            },
        );
        assert_eq!(error_code(&replies), ErrorCode::AlreadyOpen);
    }
    assert_eq!(r.s.control().doc_ids().len(), 1);
}

/// Closing a doc over its unsaved edits removes their autosave even while that autosave is
/// being written: the removal runs after the write, so nothing is left to offer.
#[test]
fn an_autosave_in_flight_cannot_bring_back_edits_the_operator_discarded() {
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
    r.s.deliver(Inbox::Client {
        message: ClientMessage::Close {
            id,
            doc: None,
            discard_changes: true,
        },
        reply: tx,
    });
    assert!(matches!(
        rx.try_recv().unwrap(),
        ServerMessage::Result {
            outcome: Outcome::Ok(_),
            ..
        }
    ));
    release_tx.send(()).unwrap();
    r.s.settle_docs();
    assert!(
        names(&r.env.dirs.recovery_dir()).is_empty(),
        "{:?}",
        names(&r.env.dirs.recovery_dir())
    );
    let replies = request(&mut r.s, open(&path));
    assert!(
        matches!(data(&replies), ResultData::Opened { .. }),
        "no offer"
    );
}

/// Another doc opened beside the first (not closing it, unlike `request`).
fn open_beside(s: &mut OfflineShow, request: Request) -> Vec<ServerMessage> {
    let id = s.request_id();
    s.send(ClientMessage::Request { id, request })
}

fn opened_doc(msgs: &[ServerMessage]) -> openlab_engine::control::DocId {
    match data(msgs) {
        ResultData::Opened { doc } => doc,
        other => panic!("{other:?}"),
    }
}

/// A Save As onto the show file another open doc has is refused with
/// `already_open` and writes nothing, also when the path reaches it through a symlinked folder
/// (the writer compares the files themselves); else the later save of either doc would silently
/// overwrite the other's.
#[test]
fn save_as_onto_another_open_docs_show_is_already_open() {
    let mut r = saved_rig(memo_ws("one", &[1]));
    let path = r.env.show_path();
    let before = fs::read_to_string(&path).unwrap();
    let doc = opened_doc(&open_beside(&mut r.s, Request::New {}));
    let save_as = |s: &mut OfflineShow, p: &Path| {
        let id = s.request_id();
        let replies = s.send_doc(
            doc,
            ClientMessage::Request {
                id,
                request: Request::SaveAs {
                    path: p.to_string_lossy().into_owned(),
                },
            },
        );
        assert_eq!(s.stalls(), 0);
        replies
    };
    assert_eq!(
        error_code(&save_as(&mut r.s, &path)),
        ErrorCode::AlreadyOpen
    );
    #[cfg(unix)]
    {
        let link = r.env.show_dir().with_file_name("linked");
        std::os::unix::fs::symlink(r.env.show_dir(), &link).unwrap();
        let via_link = link.join(path.file_name().unwrap());
        assert_eq!(
            error_code(&save_as(&mut r.s, &via_link)),
            ErrorCode::AlreadyOpen
        );
    }
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        before,
        "nothing written"
    );
    let view = r.s.control().doc(doc).unwrap();
    assert_eq!(view.show_path(), None, "the new doc is still untitled");
}

/// An open through a symlinked folder of a show that is open is `already_open`
/// (the document worker compares the files), not a second doc on the same file.
#[cfg(unix)]
#[test]
fn opening_an_open_show_through_a_symlink_is_already_open() {
    let mut r = saved_rig(memo_ws("one", &[1]));
    let path = r.env.show_path();
    let link = r.env.show_dir().with_file_name("linked");
    std::os::unix::fs::symlink(r.env.show_dir(), &link).unwrap();
    let replies = open_beside(
        &mut r.s,
        Request::Open {
            path: link
                .join(path.file_name().unwrap())
                .to_string_lossy()
                .into_owned(),
            recovery: None,
        },
    );
    assert_eq!(error_code(&replies), ErrorCode::AlreadyOpen);
    assert_eq!(r.s.control().doc_ids().len(), 1);
}

/// A show file a Save As is still writing is claimed: opening it meanwhile is
/// `already_open` (its doc only takes the path when the write is done).
#[test]
fn opening_a_show_a_save_as_is_writing_is_already_open() {
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
    // An older file at the path, so `open` would find one.
    write_show(&path, &memo_ws("older", &[2]));
    let (tx, mut rx) = reply_channel();
    r.s.deliver(Inbox::Client {
        message: ClientMessage::Request {
            id: RequestId(1),
            request: Request::SaveAs {
                path: path.to_string_lossy().into_owned(),
            },
        },
        reply: tx,
    });
    entered.recv().unwrap(); // the save is mid-write
    // Answered at once, on the control thread (the writer is still held).
    let (otx, mut orx) = reply_channel();
    r.s.deliver(Inbox::Client {
        message: ClientMessage::Request {
            id: RequestId(2),
            request: open(&path),
        },
        reply: otx,
    });
    let replies: Vec<_> = std::iter::from_fn(|| orx.try_recv().ok()).collect();
    assert_eq!(error_code(&replies), ErrorCode::AlreadyOpen);
    release_tx.send(()).unwrap();
    r.s.settle_docs();
    assert!(matches!(
        rx.try_recv().unwrap(),
        ServerMessage::Result {
            outcome: Outcome::Ok(_),
            ..
        }
    ));
    assert_eq!(r.s.control().doc_ids().len(), 1);
}

/// A save stuck on a volume that stopped answering is answered `io` after the document
/// timeout, on the engine's clock, instead of leaving the operator (and every later save)
/// waiting.
#[test]
fn a_stalled_save_is_answered_io_after_the_document_timeout() {
    let (entered_tx, entered) = mpsc::channel::<()>();
    let (release_tx, release) = mpsc::channel::<()>();
    let entered_tx = Mutex::new(entered_tx);
    let release = Mutex::new(release);
    let first = std::sync::atomic::AtomicBool::new(true);
    let hook = SaveHook(Arc::new(move |kind: JobKind| {
        if kind == JobKind::Save && first.swap(false, std::sync::atomic::Ordering::SeqCst) {
            let _ = entered_tx.lock().unwrap().send(());
            let _ = release.lock().unwrap().recv();
        }
    }));
    let time = ProbeTime::new();
    let mut r = rig_tweak(memo_ws("one", &[1]), |c| {
        c.save_hook = Some(hook);
        c.doc_timeout = std::time::Duration::from_secs(20);
        c.stall_clock = time.clock();
    });
    let _unblock = Unblock(release_tx);
    let (tx, mut rx) = reply_channel();
    r.s.deliver(Inbox::Client {
        message: ClientMessage::Request {
            id: RequestId(1),
            request: Request::SaveAs {
                path: r.env.show_path().to_string_lossy().into_owned(),
            },
        },
        reply: tx,
    });
    entered.recv().unwrap();
    r.s.tick();
    // A wall-clock step does not count: only the monotonic stall clock does.
    r.clock.advance(3_600_000);
    time.advance(std::time::Duration::from_millis(19_999));
    r.s.tick();
    r.s.tick();
    assert!(rx.try_recv().is_err(), "still waiting short of the timeout");
    time.advance(std::time::Duration::from_millis(1));
    r.s.tick();
    r.s.tick();
    assert!(matches!(
        rx.try_recv().unwrap(),
        ServerMessage::Result {
            outcome: Outcome::Error(ref e),
            ..
        } if e.code == ErrorCode::Io && e.message.contains("did not finish within 20 s")
    ));
}

fn portable(p: &Path) -> String {
    openlab_core::to_portable(p.to_str().unwrap(), openlab_core::PathStyle::host())
}

fn media_cue(n: u128, media: &str) -> openlab_core::Cue {
    let mut c = audio(n, Path::new("x.wav"), 0);
    if let CueKind::Audio { media: m, .. } = &mut c.kind {
        *m = Some(MediaRef::new(media));
    }
    c
}

fn saved_media(path: &Path, n: u128) -> String {
    let saved = load_show(&fs::read_to_string(path).unwrap())
        .unwrap()
        .workspace;
    media_of(&saved, n)
}

fn set_media(s: &mut OfflineShow, n: u128, media: &str) {
    let id1 = s.request_id();
    ok_result(&s.send(ClientMessage::Command {
        id: id1,
        command: Box::new(Command::SetCueProperty {
            id: id(n),
            property: CueProperty::Media {
                value: Some(MediaRef::new(media)),
            },
        }),
        coalesce: None,
    }));
}

fn save_as(s: &mut OfflineShow, path: &Path) {
    ok_result(&request(
        s,
        Request::SaveAs {
            path: path.to_string_lossy().into_owned(),
        },
    ));
}

#[test]
fn the_first_save_makes_media_inside_the_folder_relative_and_leaves_the_rest_absolute() {
    let dir = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let inside = portable(&dir.path().join("media").join("thunder.wav"));
    let outside = portable(&other.path().join("rain.wav"));
    // Not literally inside: `..` and a different case stay as the user gave them.
    let dotted = format!("{}/media/../up.wav", portable(dir.path()));
    let cased = portable(dir.path()).to_uppercase() + "/Cased.wav";
    let ws = workspace(vec![
        media_cue(1, &inside),
        media_cue(2, &outside),
        media_cue(3, &dotted),
        media_cue(4, &cased),
        media_cue(5, "already/relative.wav"),
    ]);
    let mut r = rig(ws);
    rename(&mut r.s, 2, "renamed");
    let show = dir.path().join("first.openlab");
    save_as(&mut r.s, &show);
    let out = r.s.take_outbound();
    let live = r.s.control().workspace().clone();
    assert_eq!(media_of(&live, 1), "media/thunder.wav");
    assert_eq!(media_of(&live, 2), outside);
    assert_eq!(media_of(&live, 3), dotted);
    assert_eq!(media_of(&live, 4), cased);
    assert_eq!(media_of(&live, 5), "already/relative.wav");
    // The file on disk holds the same.
    assert_eq!(saved_media(&show, 1), "media/thunder.wav");
    assert_eq!(saved_media(&show, 2), outside);
    assert_eq!(saved_media(&show, 3), dotted);
    assert!(!r.s.control().is_unsaved());
    // Clients are told, since cues changed without an edit of theirs.
    let snap = out
        .iter()
        .find_map(|m| match m {
            ServerMessage::Snapshot { snapshot } => Some(snapshot),
            _ => None,
        })
        .expect("snapshot");
    assert_eq!(media_of(&snap.workspace, 1), "media/thunder.wav");

    // Undo after the save still works: the earlier edit goes and the paths stay as saved.
    let undo_id = r.s.request_id();
    ok_result(&r.s.send(ClientMessage::Undo { id: undo_id }));
    assert_ne!(cue_name(&r.s, 2), "renamed");
    assert_eq!(media_of(r.s.control().workspace(), 1), "media/thunder.wav");
    assert_eq!(media_of(r.s.control().workspace(), 2), outside);
    assert!(r.s.control().is_unsaved(), "the undo is an edit");
}

#[test]
fn undo_and_redo_of_a_media_edit_after_the_first_save_name_the_same_files() {
    let dir = tempfile::tempdir().unwrap();
    let inside = portable(&dir.path().join("a.wav"));
    let mut r = rig(workspace(vec![media_cue(1, "/Old/old.wav")]));
    set_media(&mut r.s, 1, &inside);
    save_as(&mut r.s, &dir.path().join("s.openlab"));
    assert_eq!(media_of(r.s.control().workspace(), 1), "a.wav");
    let undo_id = r.s.request_id();
    ok_result(&r.s.send(ClientMessage::Undo { id: undo_id }));
    assert_eq!(media_of(r.s.control().workspace(), 1), "/Old/old.wav");
    let redo_id = r.s.request_id();
    ok_result(&r.s.send(ClientMessage::Redo { id: redo_id }));
    // Redo re-applies the value the undo replaced, which was the relative spelling by then.
    assert_eq!(media_of(r.s.control().workspace(), 1), "a.wav");
}

#[test]
fn save_as_to_another_folder_relativizes_only_what_lies_inside_the_new_one() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let in_b = portable(&b.path().join("sfx").join("b.wav"));
    let in_a = portable(&a.path().join("a.wav"));
    let ws = workspace(vec![
        media_cue(1, &in_b),
        media_cue(2, &in_a),
        media_cue(3, "rel/c.wav"),
    ]);
    let a_show = a.path().join("show.openlab");
    write_show(&a_show, &ws);
    let mut r = rig_with(ws, Some(a_show), None);
    let b_show = b.path().join("copy.openlab");
    save_as(&mut r.s, &b_show);
    for ws in [
        r.s.control().workspace().clone(),
        load_show(&fs::read_to_string(&b_show).unwrap())
            .unwrap()
            .workspace,
    ] {
        assert_eq!(media_of(&ws, 1), "sfx/b.wav");
        assert_eq!(media_of(&ws, 2), in_a, "outside the new folder: untouched");
        assert_eq!(
            media_of(&ws, 3),
            portable(&a.path().join("rel").join("c.wav")),
            "a relative path is re-spelled to the same file"
        );
    }
    assert!(!r.s.control().is_unsaved());
}

#[test]
fn an_odd_file_name_is_made_relative_composed_and_a_backslash_is_a_name_on_posix() {
    let dir = tempfile::tempdir().unwrap();
    let base = portable(dir.path());
    // NFD "e" + acute in a sub-folder; a backslash in a name only where it is one.
    let nfd = format!("{base}/Cafe\u{301}/a b.wav");
    let mut cues = vec![media_cue(1, &nfd)];
    if cfg!(unix) {
        cues.push(media_cue(2, &format!("{base}/Door\\Slam.wav")));
    }
    let mut r = rig(workspace(cues));
    save_as(&mut r.s, &dir.path().join("odd.openlab"));
    assert_eq!(media_of(r.s.control().workspace(), 1), "Caf\u{e9}/a b.wav");
    if cfg!(unix) {
        assert_eq!(media_of(r.s.control().workspace(), 2), "Door\\Slam.wav");
    }
}

#[test]
fn save_as_from_one_folder_to_another_drops_history_that_names_relative_paths_of_the_old_one() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let in_b = portable(&b.path().join("b.wav"));
    // Saved in A with a relative file; the edit replaces it with a file that lies inside B, so
    // the Save As to B only turns an absolute path relative, yet the undo step holds "old.wav".
    let ws = workspace(vec![media_cue(1, "old.wav")]);
    let a_show = a.path().join("show.openlab");
    write_show(&a_show, &ws);
    let mut r = rig_with(ws, Some(a_show), None);
    set_media(&mut r.s, 1, &in_b);
    save_as(&mut r.s, &b.path().join("copy.openlab"));
    assert_eq!(media_of(r.s.control().workspace(), 1), "b.wav");
    let snap =
        r.s.take_outbound()
            .into_iter()
            .find_map(|m| match m {
                ServerMessage::Snapshot { snapshot } => Some(snapshot),
                _ => None,
            })
            .expect("snapshot");
    assert!(!snap.document.can_undo, "no step may bring back old.wav");
    let undo_id = r.s.request_id();
    let _ = r.s.send(ClientMessage::Undo { id: undo_id });
    assert_eq!(media_of(r.s.control().workspace(), 1), "b.wav");
}
