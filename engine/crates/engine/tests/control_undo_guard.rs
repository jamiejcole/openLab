// SPDX-License-Identifier: GPL-3.0-or-later

//! An undo or redo that would stop an active cue is refused (M5.10d, NFR-01, ED-07): the
//! workspace, the revision and both stacks stay, the cue plays on, and the same step goes
//! through once the cue is idle. Offline render, virtual clock.

mod show;

use openlab_core::protocol::{
    ClientMessage, ErrorCode, Outcome, ServerMessage, TransportAction, WireWarning,
};
use openlab_core::{
    Command, ContinueMode, Cue, CueKind, CueState, FadeCurve, FadeLevels, GroupMode, Position,
    Workspace,
};
use openlab_engine::control::offline::OfflineShow;
use show::*;

#[allow(clippy::panic)]
fn outcome(replies: &[ServerMessage]) -> Outcome {
    match replies.last() {
        Some(ServerMessage::Result { outcome, .. }) => outcome.clone(),
        other => panic!("no result: {other:?}"),
    }
}

fn add(s: &mut OfflineShow, cue: Cue) {
    let id = s.request_id();
    let r = s.send(ClientMessage::Command {
        id,
        command: Box::new(Command::AddCue {
            at: Position {
                list: list_id(),
                parent: None,
                index: 0,
            },
            cue,
            auto_number: false,
        }),
        coalesce: None,
    });
    assert!(matches!(outcome(&r), Outcome::Ok(_)), "{r:?}");
    s.settle_media();
}

fn undo(s: &mut OfflineShow) -> Outcome {
    let id = s.request_id();
    outcome(&s.send(ClientMessage::Undo { id }))
}

fn redo(s: &mut OfflineShow) -> Outcome {
    let id = s.request_id();
    outcome(&s.send(ClientMessage::Redo { id }))
}

/// `step` is refused as `command_refused` with a message naming the cue and what it is doing;
/// nothing changes: workspace, revision, undo/redo availability, no `model_changed` and no
/// `cue_removed`.
#[allow(clippy::panic)]
fn assert_refused(s: &mut OfflineShow, step: fn(&mut OfflineShow) -> Outcome, doing: &str) {
    let before: Workspace = s.control().workspace().clone();
    let revision = s.control().revision();
    let document = s.control().snapshot().document;
    s.take_outbound();
    match step(s) {
        Outcome::Error(e) => {
            assert_eq!(e.code, ErrorCode::CommandRefused);
            assert!(e.message.contains(doing), "{}", e.message);
            assert!(e.message.contains("cue 1 \"cue 1\""), "{}", e.message);
        }
        other => panic!("not refused: {other:?}"),
    }
    assert_eq!(s.control().workspace(), &before);
    assert_eq!(s.control().revision(), revision);
    assert_eq!(s.control().snapshot().document, document);
    let out = s.take_outbound();
    assert!(
        !out.iter()
            .any(|m| matches!(m, ServerMessage::ModelChanged { .. })),
        "{out:?}"
    );
    assert!(
        !warnings(&out)
            .iter()
            .any(|w| matches!(w, WireWarning::CueRemoved { .. }))
    );
    assert_eq!(s.control().engine().unsignalled_model_changes(), 0);
}

/// The cue still renders after the refusal.
fn still_plays(s: &mut OfflineShow) {
    let from = s.now() as usize;
    s.run(BLOCK * 4);
    assert!(s.channel(0)[from..].iter().any(|&v| v != 0.0));
}

fn hard_stop(s: &mut OfflineShow, n: u128) {
    s.transport(TransportAction::HardStop { cue: id(n) });
    s.run(BLOCK * 2);
    assert!(!s.control().engine().state(id(n)).is_active());
}

#[test]
fn undo_of_adding_a_running_cue_is_refused_until_it_stops() {
    let path = wav(&dc(40_000, 0.5));
    let mut s = show(workspace(vec![]));
    add(&mut s, audio(1, &path, 0));
    start(&mut s, 1);
    s.run(500);
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    assert_refused(&mut s, undo, "Can't undo");
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    still_plays(&mut s);
    hard_stop(&mut s, 1);
    // The step stayed on the stack: it goes through now.
    assert!(matches!(undo(&mut s), Outcome::Ok(ok) if ok.revision.is_some()));
    assert!(s.control().workspace().find_cue(id(1)).is_none());
    // And redo still has it.
    assert!(matches!(redo(&mut s), Outcome::Ok(ok) if ok.revision.is_some()));
    assert!(s.control().workspace().find_cue(id(1)).is_some());
}

#[test]
fn redo_of_deleting_a_running_cue_is_refused_until_it_stops() {
    let path = wav(&dc(40_000, 0.5));
    let mut s = show(workspace(vec![audio(1, &path, 0)]));
    let rid = s.request_id();
    s.send(ClientMessage::Command {
        id: rid,
        command: Box::new(Command::RemoveCue { id: id(1) }),
        coalesce: None,
    });
    assert!(matches!(undo(&mut s), Outcome::Ok(_)));
    s.settle_media();
    start(&mut s, 1);
    s.run(500);
    assert_refused(&mut s, redo, "Can't redo: cue 1 \"cue 1\" is running");
    still_plays(&mut s);
    hard_stop(&mut s, 1);
    assert!(matches!(redo(&mut s), Outcome::Ok(ok) if ok.revision.is_some()));
    assert!(s.control().workspace().find_cue(id(1)).is_none());
}

#[test]
fn a_pre_waiting_cue_blocks_the_undo() {
    let path = wav(&dc(40_000, 0.5));
    let mut s = show(workspace(vec![]));
    let mut c = audio(1, &path, 0);
    c.pre_wait = secs(4000);
    add(&mut s, c);
    start(&mut s, 1);
    s.run(BLOCK * 2);
    assert_eq!(s.control().engine().state(id(1)), CueState::PreWaiting);
    assert_refused(&mut s, undo, "is pre-waiting");
    // Its pre-wait still ends on time and it plays.
    s.run(4000);
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
}

#[test]
fn a_paused_cue_blocks_the_undo() {
    let path = wav(&dc(40_000, 0.5));
    let mut s = show(workspace(vec![]));
    add(&mut s, audio(1, &path, 0));
    start(&mut s, 1);
    s.run(500);
    s.transport(TransportAction::Pause { cue: id(1) });
    s.run(BLOCK * 2);
    assert_eq!(s.control().engine().state(id(1)), CueState::Paused);
    assert_refused(&mut s, undo, "is paused");
    s.transport(TransportAction::Resume { cue: id(1) });
    still_plays(&mut s);
}

#[test]
fn a_fading_cue_blocks_the_undo() {
    let path = wav(&dc(40_000, 0.5));
    let fade = cue(
        2,
        CueKind::Fade {
            target: Some(id(1)),
            duration: secs(8000),
            curve: FadeCurve::Linear,
            levels: FadeLevels {
                master_db: Some(-6.0),
                output_db: vec![],
                crosspoints: vec![],
            },
            stop_target_when_done: false,
        },
    );
    let mut s = show(workspace(vec![fade]));
    add(&mut s, audio(1, &path, 0));
    start(&mut s, 1);
    s.run(500);
    start(&mut s, 2);
    s.run(BLOCK * 2);
    assert_eq!(s.control().engine().state(id(1)), CueState::Fading);
    assert_refused(&mut s, undo, "is fading");
    still_plays(&mut s);
}

#[test]
fn a_post_waiting_cue_blocks_the_undo() {
    // The post-wait counts from the start and outlasts the 300-frame action.
    let path = wav(&dc(300, 0.5));
    let mut s = show(workspace(vec![cue(3, CueKind::Memo)]));
    let mut c = audio(1, &path, 0);
    c.continue_mode = ContinueMode::AutoContinue;
    c.post_wait = secs(8000);
    add(&mut s, c);
    start(&mut s, 1);
    s.run(1000);
    assert_eq!(s.control().engine().state(id(1)), CueState::PostWaiting);
    assert_refused(&mut s, undo, "is post-waiting");
    assert_eq!(s.control().engine().state(id(1)), CueState::PostWaiting);
}

#[test]
fn a_previewed_cue_blocks_the_undo() {
    let path = wav(&dc(40_000, 0.5));
    let mut s = show(workspace(vec![]));
    add(&mut s, audio(1, &path, 0));
    s.transport(TransportAction::Preview { cue: id(1) });
    s.run(500);
    assert!(s.control().engine().is_previewing(id(1)));
    assert_refused(&mut s, undo, "is being previewed");
    still_plays(&mut s);
    s.transport(TransportAction::StopPreview { cue: Some(id(1)) });
    s.run(8000);
    assert!(!s.control().engine().is_previewing(id(1)));
    assert!(matches!(undo(&mut s), Outcome::Ok(ok) if ok.revision.is_some()));
}

#[test]
fn a_group_whose_child_runs_blocks_the_undo_of_adding_the_group() {
    let path = wav(&dc(40_000, 0.5));
    let group = cue(
        10,
        CueKind::Group {
            mode: GroupMode::Timeline,
            children: vec![audio(1, &path, 0)],
        },
    );
    let mut s = show(workspace(vec![]));
    add(&mut s, group);
    // The child alone: its group does not run.
    start(&mut s, 1);
    s.run(500);
    assert!(!s.control().engine().state(id(10)).is_active());
    assert_refused(&mut s, undo, "Can't undo: cue 1 \"cue 1\" is running");
    still_plays(&mut s);
    hard_stop(&mut s, 1);
    assert!(matches!(undo(&mut s), Outcome::Ok(ok) if ok.revision.is_some()));
    assert!(s.control().workspace().find_cue(id(10)).is_none());
}

#[test]
fn an_undo_that_leaves_the_active_cue_in_place_goes_through() {
    // A cue plays while another, idle one is added and its add undone: nothing is stopped.
    let path = wav(&dc(40_000, 0.5));
    let mut s = show(workspace(vec![audio(1, &path, 0)]));
    start(&mut s, 1);
    s.run(500);
    add(&mut s, audio(2, &path, 1));
    assert!(matches!(undo(&mut s), Outcome::Ok(ok) if ok.revision.is_some()));
    assert!(s.control().workspace().find_cue(id(2)).is_none());
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    still_plays(&mut s);
}
