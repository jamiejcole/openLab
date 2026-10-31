// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The `relink` request (M1.17; WS-03, NFR-04): the folder is walked on a thread of its own, the
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
    /// A folder that does not exist: the cues' files are missing from it.
    gone: std::path::PathBuf,
    clock: ManualClock,
    _env: common::Env,
}

/// Cues 1..=n each point at `<gone>/<name>`, under the temp folder (absolute on every platform); the library folder starts empty.
fn rig_with(
    names: &[&str],
    tweak: impl FnOnce(&mut openlab_engine::control::ControlConfig),
) -> Rig {
    rig_inner(names, false, tweak)
}

/// `saved`: the show has a file (`<env>/show/My Show.openlab`), so Save works.
fn rig_inner(
    names: &[&str],
    saved: bool,
    tweak: impl FnOnce(&mut openlab_engine::control::ControlConfig),
) -> Rig {
    let env = env();
    let gone = env.root.path().join("gone");
    let ws = workspace(
        names
            .iter()
            .enumerate()
            .map(|(i, n)| audio(i as u128 + 1, &gone.join(n), 0))
            .collect(),
    );
    let clock = ManualClock::new(T0);
    let mut c = config(&ws);
    c.app_dirs = Some(env.dirs.clone());
    c.clock = clock.clock();
    if saved {
        c.show_path = Some(env.show_path());
    }
    tweak(&mut c);
    let lib = env.root.path().join("lib");
    fs::create_dir_all(&lib).unwrap();
    Rig {
        s: show_with(ws, c),
        lib,
        gone,
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

/// A path as the show file spells it: forward slashes on every platform.
fn portable(path: &Path) -> String {
    openlab_core::to_portable(path.to_str().unwrap(), openlab_core::PathStyle::host())
}

/// The cue's media, in portable spelling.
fn media(s: &OfflineShow, n: u128) -> String {
    openlab_core::audio_media(s.control().workspace())
        .into_iter()
        .find(|(c, _)| *c == id(n))
        .map(|(_, m)| openlab_core::to_portable(m.as_str(), openlab_core::PathStyle::host()))
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
    let gone = portable(&r.gone);
    assert!(media(&r.s, 1).ends_with("a.wav") && !media(&r.s, 1).starts_with(&gone));
    assert!(media(&r.s, 2).to_lowercase().ends_with("sub/deep/b.wav"));
    assert_eq!(media(&r.s, 3), format!("{gone}/c.wav"));
    assert!(
        r.s.take_outbound()
            .iter()
            .any(|m| matches!(m, ServerMessage::ModelChanged { .. }))
    );
    // One undo step puts both back.
    let id = r.s.request_id();
    r.s.send(ClientMessage::Undo { id });
    assert_eq!(media(&r.s, 1), format!("{}/a.wav", portable(&r.gone)));
    assert_eq!(media(&r.s, 2), format!("{}/b.wav", portable(&r.gone)));
}

/// The relink's edit reaches the cue engine with no signal by hand: the workspace's write guard
/// tells it (docs/control.md, Model changes).
#[test]
fn a_relink_reaches_the_cue_engine_without_a_signal() {
    let mut r = rig(&["a.wav"]);
    touch(&r.lib.join("a.wav"));
    let folder = r.lib.to_string_lossy().into_owned();
    let replies = assert_engine_follows(&mut r.s, |s| {
        let id = s.request_id();
        s.send(ClientMessage::Request {
            id,
            request: Request::Relink { folder },
        })
    });
    assert_eq!(counts(&replies).0, 1);
}

#[test]
fn duplicates_are_reported_ambiguous_and_not_guessed() {
    let mut r = rig(&["a.wav"]);
    touch(&r.lib.join("x/a.wav"));
    touch(&r.lib.join("y/a.wav"));
    let rev = r.s.control().snapshot().revision;
    let replies = relink(&mut r);
    assert_eq!(counts(&replies), (0, 1, 0, false, 0));
    assert_eq!(media(&r.s, 1), format!("{}/a.wav", portable(&r.gone)));
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
    assert_eq!(media(&r.s, 1), format!("{}/a.wav", portable(&r.gone)));
    // The worker is free again.
    touch(&r.lib.join("a.wav"));
    assert_eq!(counts(&relink(&mut r)).0, 1);
}

/// The walk's thread cannot be started (the process is out of threads): the request answers
/// `internal`, nothing changes, and no walk is left counted as running, so the next relink is
/// not refused as "another relink is still walking".
#[test]
fn a_walk_whose_thread_cannot_start_answers_internal_and_changes_nothing() {
    let mut r = rig_with(&["a.wav"], |c| c.fail_relink_spawn = true);
    touch(&r.lib.join("a.wav"));
    let rev = r.s.control().snapshot().revision;
    for _ in 0..2 {
        assert_eq!(error_code(&relink(&mut r)), ErrorCode::Internal);
    }
    assert_eq!(media(&r.s, 1), format!("{}/a.wav", portable(&r.gone)));
    assert_eq!(r.s.control().snapshot().revision, rev);
    assert_eq!(r.s.control().docs_busy(), 0);
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

/// A hang guard for waits on the document worker, not a timing assertion: generous for a slow
/// host, so a regression that never answers fails instead of hanging the run.
const HANG: std::time::Duration = std::time::Duration::from_secs(30);

/// Ticks the show until `rx` answers; fails the test if no answer ever comes. The answer comes
/// from worker threads, so the bound is a count of polls, not a tick count.
fn tick_until_answer(r: &mut Rig, rx: &mut openlab_engine::control::ReplyRx) -> ServerMessage {
    let mut answer = None;
    show::spin::spin_until("an answer from the document worker", || {
        r.s.tick();
        answer = rx.try_recv().ok();
        answer.is_some()
    });
    answer.unwrap()
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
    entered.recv_timeout(HANG).unwrap();
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
fn a_slow_walk_holds_up_no_open_or_scan_and_applies_when_it_returns() {
    let (hook, entered, release) = hold_first_job();
    let mut r = rig_with(&["a.wav"], |c| c.doc_hook = Some(hook));
    touch(&r.lib.join("a.wav"));
    let folder = r.lib.to_string_lossy().into_owned();
    let (_, mut rx) = deliver(&mut r, Request::Relink { folder });
    entered.recv_timeout(HANG).unwrap();
    // The walk has a slot and a thread of its own: a recovery scan (like an `open`) is not
    // refused or held up behind it; only a second relink waits its turn.
    let (_, mut scan) = deliver(&mut r, Request::ScanRecovery);
    assert!(matches!(
        tick_until_answer(&mut r, &mut scan),
        ServerMessage::Result {
            outcome: Outcome::Ok(OkBody {
                data: Some(ResultData::RecoveryEntries { .. }),
                ..
            }),
            ..
        }
    ));
    let nowhere = r.gone.join("show.openlab").to_string_lossy().into_owned();
    let (_, mut open) = deliver(
        &mut r,
        Request::Open {
            path: nowhere,
            recovery: None,
        },
    );
    match tick_until_answer(&mut r, &mut open) {
        ServerMessage::Result {
            outcome: Outcome::Error(e),
            ..
        } => assert_ne!(e.code, ErrorCode::CommandRefused, "{e:?}"),
        other => panic!("{other:?}"),
    }
    let folder = r.lib.to_string_lossy().into_owned();
    let (_, mut second) = deliver(&mut r, Request::Relink { folder });
    assert!(matches!(
        second.try_recv().unwrap(),
        ServerMessage::Result { outcome: Outcome::Error(ref e), .. } if e.code == ErrorCode::CommandRefused
    ));
    release.send(()).unwrap();
    tick_until_answer(&mut r, &mut rx);
    assert!(!media(&r.s, 1).starts_with(&portable(&r.gone)));
}

#[test]
fn an_edit_made_during_the_walk_is_not_overwritten() {
    let (hook, entered, release) = hold_first_job();
    let mut r = rig_with(&["a.wav", "b.wav"], |c| c.doc_hook = Some(hook));
    touch(&r.lib.join("a.wav"));
    touch(&r.lib.join("b.wav"));
    let folder = r.lib.to_string_lossy().into_owned();
    let (_, mut rx) = deliver(&mut r, Request::Relink { folder });
    entered.recv_timeout(HANG).unwrap();
    // The operator points cue 1 elsewhere while the folder is walked.
    let chosen = portable(&r.lib.join("elsewhere/chose.wav"));
    let cid = r.s.request_id();
    let mut kind =
        r.s.control()
            .workspace()
            .find_cue(id(1))
            .unwrap()
            .kind
            .clone();
    if let openlab_core::CueKind::Audio { media, .. } = &mut kind {
        *media = Some(openlab_core::MediaRef::new(chosen.clone()));
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
    let got = tick_until_answer(&mut r, &mut rx);
    assert_eq!(media(&r.s, 1), chosen);
    assert!(!media(&r.s, 2).starts_with(&portable(&r.gone)));
    assert_eq!(counts(&[got]).0, 1);
}

#[test]
fn a_doc_closed_during_the_walk_answers_no_workspace() {
    let (hook, entered, release) = hold_first_job();
    let mut r = rig_with(&["a.wav"], |c| c.doc_hook = Some(hook));
    let folder = r.lib.to_string_lossy().into_owned();
    let (_, mut rx) = deliver(&mut r, Request::Relink { folder });
    entered.recv_timeout(HANG).unwrap();
    let doc = r.s.control().doc_ids()[0];
    r.s.control_mut().close_doc(doc, true).unwrap();
    release.send(()).unwrap();
    let got = tick_until_answer(&mut r, &mut rx);
    assert_eq!(error_code(&[got]), ErrorCode::NoWorkspace);
}

/// Half a second at [`RATE`] for any file that is there.
fn present_probe(path: &Path) -> Result<openlab_audio::Probed, openlab_audio::MediaError> {
    if !path.exists() {
        return Err(openlab_audio::MediaError::NotFound(path.to_path_buf()));
    }
    Ok(openlab_audio::Probed {
        info: openlab_audio::MediaInfo {
            channels: 1,
            sample_rate: RATE,
            frames: u64::from(RATE) / 2,
            duration: std::time::Duration::from_millis(500),
        },
        warnings: vec![],
        seeking: openlab_audio::Seeking::Direct,
    })
}

#[test]
fn a_relinked_cue_gets_its_new_files_lengths() {
    // M5.4d: a missing file has no lengths; once relinked and probed, the cue has the new file's.
    let mut r = rig_with(&["a.wav"], |c| c.probe = present_probe);
    assert!(r.s.control().snapshot().media_durations.is_empty());
    touch(&r.lib.join("a.wav"));
    r.s.take_outbound();
    relink(&mut r);
    r.s.settle_media();
    let told: Vec<_> = changes(&r.s.take_outbound())
        .into_iter()
        .filter_map(|c| match c {
            openlab_core::protocol::RuntimeChange::MediaDuration(d) => Some(d),
            _ => None,
        })
        .collect();
    let expected = openlab_core::protocol::CueMediaDuration {
        cue: id(1),
        file: Some(0.5),
        trimmed: Some(0.5),
        duration: Some(0.5),
    };
    assert_eq!(told, vec![expected.clone()]);
    assert_eq!(r.s.control().snapshot().media_durations, vec![expected]);
}

#[test]
fn save_works_while_a_relink_walk_is_pending_and_saves_the_model_as_it_is() {
    // #200: Save does not use the single pending document-request slot, so a walk stuck on a
    // slow volume does not stop the operator saving.
    let (hook, entered, release) = hold_first_job();
    let mut r = rig_inner(&["a.wav"], true, |c| c.doc_hook = Some(hook));
    touch(&r.lib.join("a.wav"));
    let folder = r.lib.to_string_lossy().into_owned();
    let (_, mut walk) = deliver(&mut r, Request::Relink { folder });
    entered.recv_timeout(HANG).unwrap();
    let gone_ref = media(&r.s, 1);
    let (_, mut save) = deliver(&mut r, Request::Save);
    let answer = tick_until_answer(&mut r, &mut save);
    assert!(
        matches!(
            answer,
            ServerMessage::Result {
                outcome: Outcome::Ok(_),
                ..
            }
        ),
        "{answer:?}"
    );
    assert!(walk.try_recv().is_err(), "the walk is still pending");
    assert!(!r.s.control().is_unsaved());
    // The file holds the model as it stood: the cue still points at the missing file.
    let saved = openlab_core::load_show(&fs::read_to_string(r._env.show_path()).unwrap())
        .unwrap()
        .workspace;
    let saved_media = openlab_core::audio_media(&saved);
    assert_eq!(
        openlab_core::to_portable(saved_media[0].1.as_str(), openlab_core::PathStyle::host()),
        gone_ref
    );
    // The walk then lands as an edit after the save: relinked, and the show is dirty again.
    release.send(()).unwrap();
    let got = tick_until_answer(&mut r, &mut walk);
    assert_eq!(counts(&[got]).0, 1);
    assert!(r.s.control().is_unsaved());
}

/// Plays a 2 s cue; with `relink`, its file moves into a library folder mid-play and a relink
/// follows. Returns the audible span of the output. Unix only: Windows refuses to move a file
/// a voice has open.
#[cfg(unix)]
fn playing_cue_span(relink_midway: bool) -> (usize, usize) {
    let env = env();
    let lib = env.root.path().join("lib");
    fs::create_dir_all(&lib).unwrap();
    let original = wav(&dc(2 * RATE as usize, 0.5));
    let name = original.file_name().unwrap().to_owned();
    let ws = workspace(vec![audio(1, &original, 0)]);
    let mut c = config(&ws);
    c.app_dirs = Some(env.dirs.clone());
    let mut s = show_with(ws, c);
    start(&mut s, 1);
    s.run(BLOCK * 8);
    let run = s.control().engine().run_of(id(1)).unwrap();
    assert_eq!(
        s.control().engine().state(id(1)),
        openlab_core::CueState::Running
    );
    if relink_midway {
        fs::copy(&original, lib.join(&name)).unwrap();
        fs::remove_file(&original).unwrap();
        let request_id = s.request_id();
        let msgs = s.send(ClientMessage::Request {
            id: request_id,
            request: Request::Relink {
                folder: lib.to_string_lossy().into_owned(),
            },
        });
        assert_eq!(counts(&msgs).0, 1);
        assert_eq!(media(&s, 1), portable(&lib.join(&name)));
        assert_eq!(
            s.control().engine().state(id(1)),
            openlab_core::CueState::Running
        );
        assert_eq!(s.control().engine().run_of(id(1)), Some(run));
    }
    run_until_idle(&mut s, id(1), 4 * u64::from(RATE));
    let out = s.channel(0);
    (first_nonzero(&out).unwrap(), last_nonzero(&out).unwrap())
}

/// #200: relinking the media of a cue that is playing leaves its voice alone: same run, and it
/// sounds exactly as long as the same cue with no relink.
#[cfg(unix)]
#[test]
fn relinking_the_media_of_a_playing_cue_leaves_its_voice_alone() {
    let (first, last) = playing_cue_span(false);
    assert!(last - first >= 2 * RATE as usize - 2 * BLOCK as usize);
    assert_eq!(playing_cue_span(true), (first, last));
}
