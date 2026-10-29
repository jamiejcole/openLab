// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `model_changed` as the control loop sends it: a client mirror that starts from the snapshot
//! and applies every message holds the engine's workspace after each edit, undo, redo and
//! runtime change, and revisions rise by one (NFR-01, ED-07; docs/protocol.md, docs/control.md).

use crate::show::*;
use openlab_core::protocol::{ClientMessage, ServerMessage};
use openlab_core::{Command, CueKind, CueProperty, Position, Workspace};
use openlab_engine::control::offline::OfflineShow;

/// A client: the snapshot it started from, then every `model_changed`.
struct Mirror {
    workspace: Workspace,
    revision: u64,
    entities: usize,
}

impl Mirror {
    fn new(s: &OfflineShow) -> Self {
        let snapshot = s.control().snapshot();
        Self {
            workspace: snapshot.workspace,
            revision: snapshot.revision,
            entities: 0,
        }
    }

    /// Applies what went out since the last call. Returns how many `model_changed` there were.
    fn catch_up(&mut self, s: &mut OfflineShow) -> usize {
        let mut messages = 0;
        for message in s.take_outbound() {
            if let ServerMessage::ModelChanged {
                revision,
                upserts,
                deletes,
            } = message
            {
                assert_eq!(revision, self.revision + 1, "revision gap");
                self.revision = revision;
                self.entities += upserts.len() + deletes.len();
                self.workspace.apply_changes(&upserts, &deletes).unwrap();
                messages += 1;
            }
        }
        assert_eq!(&self.workspace, s.control().workspace());
        assert_eq!(self.revision, s.control().revision());
        messages
    }
}

fn command(s: &mut OfflineShow, command: Command) {
    let id = s.request_id();
    s.send(ClientMessage::Command {
        id,
        command: Box::new(command),
        coalesce: None,
    });
}

fn rename(n: u128, name: &str) -> Command {
    Command::SetCueProperty {
        id: id(n),
        property: CueProperty::Name { value: name.into() },
    }
}

#[test]
fn every_edit_undo_and_redo_sends_exactly_one_model_changed() {
    let path = wav(&dc(300, 0.5));
    let mut s = show(workspace(vec![
        audio(1, &path, 0),
        audio(2, &path, 0),
        cue(3, CueKind::Memo),
    ]));
    let mut mirror = Mirror::new(&s);
    let at = |index| Position {
        list: list_id(),
        parent: None,
        index,
    };
    command(&mut s, rename(1, "one"));
    assert_eq!(mirror.catch_up(&mut s), 1);
    command(
        &mut s,
        Command::AddCue {
            at: at(1),
            cue: cue(
                10,
                CueKind::Group {
                    mode: openlab_core::GroupMode::Timeline,
                    children: vec![cue(11, CueKind::Memo)],
                },
            ),
            auto_number: false,
        },
    );
    assert_eq!(mirror.catch_up(&mut s), 1);
    command(
        &mut s,
        Command::ReparentCue {
            id: id(2),
            to: Position {
                list: list_id(),
                parent: Some(id(10)),
                index: 0,
            },
        },
    );
    assert_eq!(mirror.catch_up(&mut s), 1);
    command(&mut s, Command::RemoveCue { id: id(10) });
    assert_eq!(mirror.catch_up(&mut s), 1);
    for undo in [true, true, true, true, false, false, false, false] {
        let rid = s.request_id();
        s.send(if undo {
            ClientMessage::Undo { id: rid }
        } else {
            ClientMessage::Redo { id: rid }
        });
        assert_eq!(mirror.catch_up(&mut s), 1);
    }
    // Nothing to undo or redo past the ends: no message, no revision.
    let rid = s.request_id();
    s.send(ClientMessage::Redo { id: rid });
    s.send(ClientMessage::Redo { id: rid });
    assert_eq!(mirror.catch_up(&mut s), 0);
}

#[test]
fn a_refused_command_sends_no_model_changed() {
    let mut s = show(workspace(vec![cue(1, CueKind::Memo)]));
    let mut mirror = Mirror::new(&s);
    command(&mut s, Command::RemoveCue { id: id(42) });
    assert_eq!(mirror.catch_up(&mut s), 0);
}

#[test]
fn an_empty_transaction_changes_nothing() {
    let mut s = show(workspace(vec![cue(1, CueKind::Memo)]));
    let mut mirror = Mirror::new(&s);
    let (revision, dirty) = (
        s.control().revision(),
        s.control().snapshot().document.dirty,
    );
    command(&mut s, Command::Transaction { commands: vec![] });
    let out = s.take_outbound();
    assert!(
        !out.iter().any(|m| matches!(
            m,
            ServerMessage::ModelChanged { .. } | ServerMessage::Document { .. }
        )),
        "sent {out:?}"
    );
    assert_eq!(s.control().revision(), revision);
    assert_eq!(s.control().snapshot().document.dirty, dirty);
    assert!(!s.control().snapshot().document.can_undo);
    assert_eq!(mirror.catch_up(&mut s), 0);
    // The next real edit is the very next revision.
    command(&mut s, rename(1, "one"));
    assert_eq!(mirror.catch_up(&mut s), 1);
    assert_eq!(mirror.revision, revision + 1);
}

#[test]
fn a_runtime_arm_change_is_sent_as_an_upsert_of_that_cue() {
    let path = wav(&dc(300, 0.5));
    let disarm = cue(
        2,
        CueKind::Disarm {
            target: Some(id(3)),
        },
    );
    let mut s = show(workspace(vec![disarm, audio(3, &path, 0)]));
    let mut mirror = Mirror::new(&s);
    go(&mut s);
    s.run(BLOCK * 2);
    assert!(!s.control().workspace().find_cue(id(3)).unwrap().armed);
    assert_eq!(mirror.catch_up(&mut s), 1);
    assert_eq!(mirror.entities, 1);
    assert!(!mirror.workspace.find_cue(id(3)).unwrap().armed);
}

/// A deterministic random walk of edits, undos and redos over groups, moves and renumbering.
#[test]
fn a_random_walk_keeps_the_mirror_identical() {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move |n: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) % n
    };
    let mut s = show(workspace((1..=6).map(|n| cue(n, CueKind::Memo)).collect()));
    let mut mirror = Mirror::new(&s);
    let mut fresh = 100u128;
    let mut applied = 0;
    for _ in 0..300 {
        let ids: Vec<_> = {
            fn walk(cues: &[openlab_core::Cue], out: &mut Vec<u128>) {
                for c in cues {
                    out.push(c.id.0.as_u128());
                    walk(c.children(), out);
                }
            }
            let mut out = Vec::new();
            walk(&s.control().workspace().cue_lists[0].cues, &mut out);
            out
        };
        let pick = |n: u64| ids.get(n as usize % ids.len().max(1)).copied().unwrap_or(1);
        let before = s.control().revision();
        match next(7) {
            0 => {
                fresh += 1;
                let kind = if next(3) == 0 {
                    fresh += 1;
                    CueKind::Group {
                        mode: openlab_core::GroupMode::Timeline,
                        children: vec![cue(fresh, CueKind::Memo)],
                    }
                } else {
                    CueKind::Memo
                };
                let parent = (next(2) == 0).then(|| id(pick(next(50))));
                command(
                    &mut s,
                    Command::AddCue {
                        at: Position {
                            list: list_id(),
                            parent,
                            index: next(3) as usize,
                        },
                        cue: cue(
                            fresh - u128::from(matches!(kind, CueKind::Group { .. })),
                            kind,
                        ),
                        auto_number: next(2) == 0,
                    },
                );
            }
            1 => command(
                &mut s,
                Command::RemoveCue {
                    id: id(pick(next(50))),
                },
            ),
            2 => command(
                &mut s,
                Command::MoveCue {
                    id: id(pick(next(50))),
                    index: next(4) as usize,
                },
            ),
            3 => command(
                &mut s,
                Command::ReparentCue {
                    id: id(pick(next(50))),
                    to: Position {
                        list: list_id(),
                        parent: (next(2) == 0).then(|| id(pick(next(50)))),
                        index: next(3) as usize,
                    },
                },
            ),
            4 => command(&mut s, rename(pick(next(50)), "r")),
            5 => command(
                &mut s,
                Command::Renumber {
                    ids: vec![id(pick(next(50))), id(pick(next(50)))],
                    start: next(20) as f64,
                    increment: 1.0,
                },
            ),
            _ => {
                let rid = s.request_id();
                s.send(if next(2) == 0 {
                    ClientMessage::Undo { id: rid }
                } else {
                    ClientMessage::Redo { id: rid }
                });
            }
        }
        let sent = mirror.catch_up(&mut s);
        assert_eq!(sent as u64, s.control().revision() - before);
        applied += sent;
    }
    assert!(applied > 100, "only {applied} edits applied");
}

#[test]
fn an_edit_in_a_2000_cue_show_sends_only_what_it_changed() {
    let path = wav(&dc(300, 0.5));
    let cues: Vec<_> = (1..=2000).map(|n| audio(n, &path, 0)).collect();
    let mut s = show(workspace(cues));
    let mut mirror = Mirror::new(&s);
    command(&mut s, rename(1000, "x"));
    mirror.catch_up(&mut s);
    assert_eq!(mirror.entities, 1);
    command(&mut s, Command::RemoveCue { id: id(7) });
    mirror.catch_up(&mut s);
    assert_eq!(mirror.entities, 2);
    let rid = s.request_id();
    s.send(ClientMessage::Undo { id: rid });
    mirror.catch_up(&mut s);
    assert_eq!(mirror.entities, 3);
    command(
        &mut s,
        Command::MoveCue {
            id: id(1),
            index: 1999,
        },
    );
    mirror.catch_up(&mut s);
    // A move is a delete and an upsert.
    assert_eq!(mirror.entities, 5);
}
