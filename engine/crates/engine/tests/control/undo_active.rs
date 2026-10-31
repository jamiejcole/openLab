// SPDX-License-Identifier: GPL-3.0-or-later
// Helpers outside #[test] fns are test code too; clippy's in-test allowances miss them.
#![allow(clippy::indexing_slicing)]

//! An undo or redo that removes an active cue is performed, as QLab 5 does (ED-07): the cue
//! stops exactly as a deliberate delete of it does (hard stop, `cue_removed` warning).
//! Offline render, virtual clock.

use crate::show::*;
use openlab_core::protocol::{ClientMessage, Outcome, ServerMessage, TransportAction, WireWarning};
use openlab_core::{Command, Cue, CueKind, CueState, GroupMode, Position};
use openlab_engine::control::offline::OfflineShow;

#[allow(clippy::panic)]
fn outcome(replies: &[ServerMessage]) -> Outcome {
    match replies.last() {
        Some(ServerMessage::Result { outcome, .. }) => outcome.clone(),
        other => panic!("no result: {other:?}"),
    }
}

fn command(s: &mut OfflineShow, command: Command) {
    let id = s.request_id();
    let r = s.send(ClientMessage::Command {
        id,
        command: Box::new(command),
        coalesce: None,
    });
    assert!(matches!(outcome(&r), Outcome::Ok(_)), "{r:?}");
    s.settle_media();
    assert_eq!(s.control().engine().unsignalled_model_changes(), 0);
}

fn add(s: &mut OfflineShow, cue: Cue) {
    command(
        s,
        Command::AddCue {
            at: Position {
                list: list_id(),
                parent: None,
                index: 0,
            },
            cue,
            auto_number: false,
        },
    );
}

fn undo(s: &mut OfflineShow) {
    let id = s.request_id();
    let r = s.send(ClientMessage::Undo { id });
    assert!(
        matches!(outcome(&r), Outcome::Ok(ok) if ok.revision.is_some()),
        "{r:?}"
    );
}

fn redo(s: &mut OfflineShow) {
    let id = s.request_id();
    let r = s.send(ClientMessage::Redo { id });
    assert!(
        matches!(outcome(&r), Outcome::Ok(ok) if ok.revision.is_some()),
        "{r:?}"
    );
}

/// After the step: cue `n` is gone and idle, the client was told `cue_removed`, nothing sounds
/// from the next blocks on and every change was signalled.
fn assert_stopped_as_deleted(s: &mut OfflineShow, n: u128) {
    let cut = s.now() as usize;
    s.run(BLOCK * 4);
    assert!(s.control().workspace().find_cue(id(n)).is_none());
    assert_eq!(s.control().engine().state(id(n)), CueState::Idle);
    assert!(!s.control().engine().is_previewing(id(n)));
    assert!(
        warnings(&s.take_outbound())
            .iter()
            .any(|w| matches!(w, WireWarning::CueRemoved { cue } if *cue == id(n)))
    );
    assert!(s.channel(0)[cut..].iter().all(|&v| v == 0.0));
    assert_eq!(s.control().live_voices(), 0);
    assert_eq!(s.control().engine().unsignalled_model_changes(), 0);
}

/// The same running cue, removed once by a delete and once by an undo of its add, at the same
/// sample: the two shows render the same sound and tell the client the same warnings.
#[test]
fn undo_of_adding_a_running_cue_stops_it_as_a_delete_does() {
    let path = wav(&dc(40_000, 0.5));
    let run = |by_undo: bool| {
        let mut s = show(workspace(vec![]));
        add(&mut s, audio(1, &path, 0));
        start(&mut s, 1);
        s.run(500);
        assert_eq!(s.control().engine().state(id(1)), CueState::Running);
        s.take_outbound();
        if by_undo {
            undo(&mut s);
        } else {
            command(&mut s, Command::RemoveCue { id: id(1) });
        }
        let at = s.now() as usize;
        s.run(BLOCK * 4);
        let removed: Vec<WireWarning> = warnings(&s.take_outbound())
            .into_iter()
            .filter(|w| matches!(w, WireWarning::CueRemoved { .. }))
            .collect();
        assert!(s.channel(0)[at..].iter().all(|&v| v == 0.0));
        assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
        (s.channel(0).to_vec(), removed)
    };
    let (deleted, deleted_warnings) = run(false);
    let (undone, undone_warnings) = run(true);
    assert_eq!(undone, deleted);
    assert_eq!(undone_warnings, deleted_warnings);
    assert_eq!(undone_warnings.len(), 1);
}

#[test]
fn redo_of_deleting_a_running_cue_stops_it() {
    let path = wav(&dc(40_000, 0.5));
    let mut s = show(workspace(vec![audio(1, &path, 0)]));
    command(&mut s, Command::RemoveCue { id: id(1) });
    undo(&mut s);
    start(&mut s, 1);
    s.run(500);
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    s.take_outbound();
    redo(&mut s);
    assert_stopped_as_deleted(&mut s, 1);
}

#[test]
fn undo_of_adding_a_paused_cue_stops_it_and_a_resume_does_nothing() {
    let path = wav(&dc(40_000, 0.5));
    let mut s = show(workspace(vec![]));
    add(&mut s, audio(1, &path, 0));
    start(&mut s, 1);
    s.run(500);
    s.transport(TransportAction::Pause { cue: id(1) });
    s.run(BLOCK * 2);
    assert_eq!(s.control().engine().state(id(1)), CueState::Paused);
    s.take_outbound();
    undo(&mut s);
    assert_stopped_as_deleted(&mut s, 1);
    s.transport(TransportAction::ResumeAll);
    let from = s.now() as usize;
    s.run(BLOCK * 4);
    assert!(s.channel(0)[from..].iter().all(|&v| v == 0.0));
}

#[test]
fn undo_of_adding_a_pre_waiting_cue_cancels_its_start() {
    let path = wav(&dc(40_000, 0.5));
    let mut s = show(workspace(vec![]));
    let mut c = audio(1, &path, 0);
    c.pre_wait = secs(4000);
    add(&mut s, c);
    start(&mut s, 1);
    s.run(BLOCK * 2);
    assert_eq!(s.control().engine().state(id(1)), CueState::PreWaiting);
    s.take_outbound();
    undo(&mut s);
    assert_stopped_as_deleted(&mut s, 1);
    // Past where its pre-wait would have ended: it never starts.
    let from = s.now() as usize;
    s.run(6000);
    assert!(s.channel(0)[from..].iter().all(|&v| v == 0.0));
}

#[test]
fn undo_of_adding_a_previewed_cue_stops_the_preview() {
    let path = wav(&dc(40_000, 0.5));
    let mut s = show(workspace(vec![]));
    add(&mut s, audio(1, &path, 0));
    s.transport(TransportAction::Preview { cue: id(1) });
    s.run(500);
    assert!(s.control().engine().is_previewing(id(1)));
    undo(&mut s);
    let cut = s.now() as usize;
    s.run(BLOCK * 4);
    assert!(!s.control().engine().is_previewing(id(1)));
    assert!(s.control().workspace().find_cue(id(1)).is_none());
    assert!(s.channel(0)[cut..].iter().all(|&v| v == 0.0));
    assert_eq!(s.control().live_voices(), 0);
}

#[test]
fn undo_of_adding_a_group_stops_its_running_child() {
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
    start(&mut s, 1);
    s.run(500);
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    s.take_outbound();
    undo(&mut s);
    assert_stopped_as_deleted(&mut s, 1);
    assert!(s.control().workspace().find_cue(id(10)).is_none());
}

#[test]
fn an_undo_that_leaves_the_active_cue_in_place_does_not_stop_it() {
    // A cue plays while another, idle one is added and its add undone: nothing is stopped.
    let path = wav(&dc(40_000, 0.5));
    let mut s = show(workspace(vec![audio(1, &path, 0)]));
    start(&mut s, 1);
    s.run(500);
    add(&mut s, audio(2, &path, 1));
    undo(&mut s);
    assert!(s.control().workspace().find_cue(id(2)).is_none());
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    let from = s.now() as usize;
    s.run(BLOCK * 4);
    assert!(s.channel(0)[from..].iter().any(|&v| v != 0.0));
}
