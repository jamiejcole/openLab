// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Settings, patch, network patch and cue list commands through the control loop (M1.12,
//! ED-07): what clients are sent, what the engine and mixer do with them, the refusal to move
//! a patch under a playing cue (M4.2f), undo coalescing and edits that change nothing.

mod show;

use openlab_core::protocol::{
    ClientMessage, ErrorCode, Outcome, ServerMessage, TransportAction, WireProblem,
};
use openlab_core::{
    Command, CueKind, CueState, NetworkPatch, NetworkPatchProperty, NetworkProtocol, OutputPatch,
    PatchId, PatchOutput, PatchProperty, TransportKey, Workspace, WorkspaceSetting,
};
use openlab_engine::control::offline::OfflineShow;
use show::*;

struct Mirror {
    workspace: Workspace,
    revision: u64,
}

impl Mirror {
    fn new(s: &OfflineShow) -> Self {
        let snapshot = s.control().snapshot();
        Self {
            workspace: snapshot.workspace,
            revision: snapshot.revision,
        }
    }

    /// Applies what went out; returns the number of `model_changed`.
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
                self.workspace.apply_changes(&upserts, &deletes).unwrap();
                messages += 1;
            }
        }
        assert_eq!(&self.workspace, s.control().workspace());
        assert_eq!(
            s.control().engine().unsignalled_model_changes(),
            0,
            "the engine was not told about an edit"
        );
        messages
    }
}

fn send(s: &mut OfflineShow, command: Command, coalesce: Option<&str>) -> Vec<ServerMessage> {
    let id = s.request_id();
    s.send(ClientMessage::Command {
        id,
        command: Box::new(command),
        coalesce: coalesce.map(str::to_owned),
    })
}

fn command(s: &mut OfflineShow, command: Command) {
    let replies = send(s, command, None);
    assert!(
        replies.iter().all(|m| !matches!(
            m,
            ServerMessage::Result {
                outcome: Outcome::Error(_),
                ..
            }
        )),
        "{replies:?}"
    );
}

fn undo(s: &mut OfflineShow) -> Vec<ServerMessage> {
    let id = s.request_id();
    s.send(ClientMessage::Undo { id })
}

/// The refusal message of a reply, if the command was refused.
fn refusal(replies: &[ServerMessage]) -> Option<String> {
    replies.iter().find_map(|m| match m {
        ServerMessage::Result {
            outcome: Outcome::Error(e),
            ..
        } => {
            assert_eq!(e.code, ErrorCode::CommandRefused, "{e:?}");
            Some(e.message.clone())
        }
        _ => None,
    })
}

fn patch(n: u128) -> OutputPatch {
    OutputPatch {
        id: PatchId(uuid::Uuid::from_u128(n)),
        name: format!("Patch {n}"),
        master_db: 0.0,
        outputs: vec![
            PatchOutput {
                level_db: 0.0,
                mute: false,
                delay_ms: 0.0,
                polarity_inverted: false,
            };
            2
        ],
    }
}

fn master(db: f64) -> Command {
    Command::SetWorkspaceSetting {
        setting: WorkspaceSetting::MasterDb { value: db },
    }
}

/// A show with a long audio cue 1 on the patch every `show` workspace has (index 0), and a
/// second patch (index 1) nobody uses.
fn playing_show() -> OfflineShow {
    let path = wav(&dc(RATE as usize * 20, 0.5));
    let mut ws = workspace(vec![audio(1, &path, 0)]);
    ws.patches.push(patch(2));
    show(ws)
}

#[test]
fn settings_patches_and_lists_reach_clients_as_model_changed() {
    let mut s = playing_show();
    let mut mirror = Mirror::new(&s);
    let revision = s.control().revision();
    let steps = [
        master(-3.0),
        Command::AddOscPasscode {
            index: 0,
            passcode: "4821".into(),
            rights: openlab_core::OscRights::ALL,
        },
        Command::SetOscRights {
            target: openlab_core::OscRightsTarget::NoPasscode,
            rights: openlab_core::OscRights::NONE,
        },
        Command::RemoveOscPasscode {
            passcode: "4821".into(),
        },
        Command::AddPatch {
            index: 2,
            patch: patch(3),
        },
        Command::SetPatchProperty {
            id: patch(3).id,
            property: PatchProperty::OutputLevel {
                output: 1,
                value: -6.0,
            },
        },
        Command::MovePatch {
            id: patch(3).id,
            index: 0,
        },
        Command::AddNetworkPatch {
            index: 0,
            patch: NetworkPatch {
                id: openlab_core::NetworkPatchId(uuid::Uuid::from_u128(5)),
                name: "Lights".into(),
                host: "127.0.0.1".into(),
                port: 9000,
                protocol: NetworkProtocol::Udp,
            },
        },
        Command::AddCueList {
            index: 0,
            id: openlab_core::CueListId(uuid::Uuid::from_u128(7)),
            name: "B".into(),
        },
        Command::RenameCueList {
            id: list_id(),
            name: "A".into(),
        },
        Command::RemovePatch {
            id: patch(3).id,
            even_if_used: false,
        },
    ];
    let count = steps.len();
    for step in steps {
        command(&mut s, step);
        assert_eq!(mirror.catch_up(&mut s), 1);
    }
    assert_eq!(s.control().revision(), revision + count as u64);
    for _ in 0..count {
        undo(&mut s);
        assert_eq!(mirror.catch_up(&mut s), 1);
    }
    assert_eq!(s.control().workspace().patches.len(), 2);
    assert!(s.control().workspace().network_patches.is_empty());
    assert_eq!(s.control().workspace().settings.master_db, 0.0);
    assert_eq!(
        s.control().workspace().settings.osc_access,
        openlab_core::OscAccess::default()
    );
    assert!(s.control().snapshot().document.dirty);
}

#[test]
fn adding_the_missing_patch_mends_a_broken_cue_and_undo_breaks_it_again() {
    let path = wav(&dc(800, 0.5));
    let mut cue1 = audio(1, &path, 0);
    if let CueKind::Audio { patch, .. } = &mut cue1.kind {
        *patch = PatchId(uuid::Uuid::from_u128(9));
    }
    let mut s = show(workspace(vec![cue1]));
    let broken = |s: &OfflineShow| {
        s.control().snapshot().problems.iter().any(|p| {
            p.cue == id(1) && matches!(p.problems.as_slice(), [WireProblem::PatchMissing { .. }])
        })
    };
    assert!(broken(&s));
    command(
        &mut s,
        Command::AddPatch {
            index: 1,
            patch: patch(9),
        },
    );
    assert!(!broken(&s), "the new patch should fix the cue");
    undo(&mut s);
    assert!(broken(&s), "undo should break it again");
}

#[test]
fn removing_a_patch_a_cue_uses_is_refused_with_the_cue_named() {
    let mut s = playing_show();
    let mut mirror = Mirror::new(&s);
    let revision = s.control().revision();
    let replies = send(
        &mut s,
        Command::RemovePatch {
            id: patch_id(),
            even_if_used: false,
        },
        None,
    );
    let message = refusal(&replies).expect("refused");
    assert!(message.contains(&id(1).to_string()), "{message}");
    assert_eq!(mirror.catch_up(&mut s), 0);
    assert_eq!(s.control().revision(), revision);
}

#[test]
fn patch_layout_is_frozen_while_any_voice_plays_and_free_when_none_does() {
    let mut s = playing_show();
    let mut mirror = Mirror::new(&s);
    // Nothing plays: every layout change is fine.
    command(
        &mut s,
        Command::MovePatch {
            id: patch_id(),
            index: 1,
        },
    );
    command(
        &mut s,
        Command::MovePatch {
            id: patch_id(),
            index: 0,
        },
    );
    assert_eq!(mirror.catch_up(&mut s), 2);

    start(&mut s, 1);
    s.run(BLOCK * 8);
    assert!(s.control().engine().state(id(1)).is_active());
    let revision = s.control().revision();
    let blocked = [
        Command::AddPatch {
            index: 0,
            patch: patch(3),
        },
        Command::MovePatch {
            id: patch_id(),
            index: 1,
        },
        Command::MovePatch {
            id: patch(2).id,
            index: 0,
        },
        // Even a patch no cue names: a voice may still sit on its slot.
        Command::RemovePatch {
            id: patch(2).id,
            even_if_used: false,
        },
        Command::RemovePatch {
            id: patch_id(),
            even_if_used: true,
        },
        // An append followed by a move to the front shifts every slot.
        Command::Transaction {
            commands: vec![
                Command::AddPatch {
                    index: 2,
                    patch: patch(3),
                },
                Command::MovePatch {
                    id: patch(3).id,
                    index: 0,
                },
            ],
        },
        Command::Transaction {
            commands: vec![
                master(-1.0),
                Command::MovePatch {
                    id: patch_id(),
                    index: 1,
                },
            ],
        },
        // A bigger patch no longer fits its device mapping: the running cue would go silent.
        Command::SetPatchProperty {
            id: patch_id(),
            property: PatchProperty::ChannelCount { value: 4 },
        },
    ];
    for command in blocked {
        let replies = send(&mut s, command.clone(), None);
        let message = refusal(&replies).unwrap_or_else(|| panic!("{command:?} was allowed"));
        assert!(message.contains("playing"), "{message}");
    }
    assert_eq!(mirror.catch_up(&mut s), 0);
    assert_eq!(s.control().revision(), revision);
    assert_eq!(s.control().workspace().settings.master_db, 0.0);

    // What keeps the layout is fine: edits in place and an append.
    command(
        &mut s,
        Command::SetPatchProperty {
            id: patch_id(),
            property: PatchProperty::MasterDb { value: -3.0 },
        },
    );
    command(
        &mut s,
        Command::AddPatch {
            index: 2,
            patch: patch(3),
        },
    );
    assert_eq!(mirror.catch_up(&mut s), 2);

    // Stopped, the same edits go through.
    s.transport(TransportAction::HardStop { cue: id(1) });
    run_until_idle(&mut s, id(1), 8000);
    // The stopped voice leaves the mixer a moment after the cue is idle.
    s.run(BLOCK * 16);
    command(
        &mut s,
        Command::AddPatch {
            index: 0,
            patch: patch(4),
        },
    );
    command(
        &mut s,
        Command::SetPatchProperty {
            id: patch_id(),
            property: PatchProperty::ChannelCount { value: 4 },
        },
    );
    assert_eq!(mirror.catch_up(&mut s), 2);
    assert_eq!(s.control().workspace().patches[0].id, patch(4).id);
}

#[test]
fn undo_and_redo_of_a_patch_move_are_held_to_the_same_rule() {
    let mut s = playing_show();
    command(
        &mut s,
        Command::MovePatch {
            id: patch_id(),
            index: 1,
        },
    );
    start(&mut s, 1);
    s.run(BLOCK * 8);
    let revision = s.control().revision();
    let replies = undo(&mut s);
    assert!(refusal(&replies).unwrap().contains("playing"));
    assert_eq!(s.control().revision(), revision);
    // The step stayed: after the cue stops, undo works.
    s.transport(TransportAction::HardStop { cue: id(1) });
    run_until_idle(&mut s, id(1), 8000);
    // The stopped voice leaves the mixer a moment after the cue is idle.
    s.run(BLOCK * 16);
    let replies = undo(&mut s);
    assert!(refusal(&replies).is_none());
    assert_eq!(s.control().workspace().patches[0].id, patch_id());
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
}

#[test]
fn the_master_level_setting_changes_what_is_heard() {
    let path = wav(&dc(RATE as usize * 4, 0.5));
    let mut s = show(workspace(vec![audio(1, &path, 0)]));
    start(&mut s, 1);
    s.run(BLOCK * 8);
    let loud = s.channel(0).iter().fold(0.0f32, |m, x| m.max(x.abs()));
    assert!(loud > 0.4, "{loud}");
    command(&mut s, master(-20.0));
    s.run(BLOCK * 8);
    let tail: Vec<f32> = s.channel(0);
    let quiet = tail[tail.len() - BLOCK as usize..]
        .iter()
        .fold(0.0f32, |m, x| m.max(x.abs()));
    assert!(quiet < 0.1 && quiet > 0.01, "{quiet}");
}

#[test]
fn edits_that_change_nothing_make_no_revision_and_no_undo_step() {
    let mut s = playing_show();
    let mut mirror = Mirror::new(&s);
    let before = s.control().snapshot();
    for same in [
        master(0.0),
        Command::SetWorkspaceSetting {
            setting: WorkspaceSetting::Hotkey {
                key: TransportKey::Go,
                value: before.workspace.settings.hotkeys.go.clone(),
            },
        },
        Command::SetPatchProperty {
            id: patch_id(),
            property: PatchProperty::ChannelCount { value: 2 },
        },
        Command::SetNetworkPatchProperty {
            id: openlab_core::NetworkPatchId(uuid::Uuid::from_u128(77)),
            property: NetworkPatchProperty::Port { value: 1 },
        },
        Command::SetCueProperty {
            id: id(1),
            property: openlab_core::CueProperty::Armed { value: true },
        },
    ] {
        let replies = send(&mut s, same.clone(), None);
        // The unknown network patch is a refusal; every other one is accepted and ignored.
        let refused = refusal(&replies).is_some();
        assert_eq!(
            refused,
            matches!(same, Command::SetNetworkPatchProperty { .. })
        );
    }
    assert_eq!(mirror.catch_up(&mut s), 0);
    let after = s.control().snapshot();
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.document.dirty, before.document.dirty);
    assert!(!after.document.can_undo);
}

#[test]
fn a_fader_drag_with_one_key_is_one_undo_step_but_every_value_reaches_clients() {
    let mut s = playing_show();
    let mut mirror = Mirror::new(&s);
    let revision = s.control().revision();
    for db in [-1.0, -2.0, -3.0, -4.0] {
        let replies = send(&mut s, master(db), Some("master:1"));
        assert!(refusal(&replies).is_none());
        assert_eq!(mirror.catch_up(&mut s), 1, "each value is shown at once");
    }
    assert_eq!(s.control().revision(), revision + 4);
    assert_eq!(s.control().workspace().settings.master_db, -4.0);
    assert!(s.control().snapshot().document.can_undo);

    // A second gesture is its own step.
    send(&mut s, master(-9.0), Some("master:2"));
    mirror.catch_up(&mut s);
    undo(&mut s);
    mirror.catch_up(&mut s);
    assert_eq!(s.control().workspace().settings.master_db, -4.0);
    undo(&mut s);
    mirror.catch_up(&mut s);
    assert_eq!(s.control().workspace().settings.master_db, 0.0);
    assert!(!s.control().snapshot().document.can_undo);
}

#[test]
fn a_command_without_a_key_never_merges_and_a_long_key_is_refused() {
    let mut s = playing_show();
    send(&mut s, master(-1.0), None);
    send(&mut s, master(-2.0), None);
    undo(&mut s);
    assert_eq!(s.control().workspace().settings.master_db, -1.0);

    let revision = s.control().revision();
    let replies = send(&mut s, master(-5.0), Some(&"k".repeat(129)));
    let error = replies
        .iter()
        .find_map(|m| match m {
            ServerMessage::Result {
                outcome: Outcome::Error(e),
                ..
            } => Some(e.clone()),
            _ => None,
        })
        .expect("refused");
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert_eq!(s.control().revision(), revision);
    assert_eq!(s.control().workspace().settings.master_db, -1.0);
    // The longest allowed key works.
    let replies = send(&mut s, master(-5.0), Some(&"k".repeat(128)));
    assert!(refusal(&replies).is_none());
    assert_eq!(s.control().workspace().settings.master_db, -5.0);
}

#[test]
fn a_new_cue_list_can_be_played_and_removed() {
    let mut s = playing_show();
    let mut mirror = Mirror::new(&s);
    let new = openlab_core::CueListId(uuid::Uuid::from_u128(7));
    command(
        &mut s,
        Command::AddCueList {
            index: 1,
            id: new,
            name: "B".into(),
        },
    );
    assert_eq!(mirror.catch_up(&mut s), 1);
    // Its playhead exists: GO on an empty list does not error or disturb the other list.
    let replies = s.transport(TransportAction::Go { list: new });
    assert!(replies.iter().all(|m| !matches!(
        m,
        ServerMessage::Result {
            outcome: Outcome::Error(_),
            ..
        }
    )));
    assert!(
        s.control()
            .snapshot()
            .playheads
            .iter()
            .any(|p| p.list == new)
    );
    let refused = send(&mut s, Command::RemoveCueList { id: list_id() }, None);
    assert!(refusal(&refused).unwrap().contains("still has cues"));
    command(&mut s, Command::RemoveCueList { id: new });
    assert_eq!(mirror.catch_up(&mut s), 1);
    assert!(
        !s.control()
            .snapshot()
            .playheads
            .iter()
            .any(|p| p.list == new)
    );
}

#[test]
fn undo_and_redo_of_a_merged_multi_slot_gesture_reach_a_mirror() {
    let mut s = playing_show();
    let mut mirror = Mirror::new(&s);
    let name = |v: &str| Command::SetCueProperty {
        id: id(1),
        property: openlab_core::CueProperty::Name { value: v.into() },
    };
    for step in [
        name("a"),
        master(-2.0),
        Command::SetPatchProperty {
            id: patch_id(),
            property: PatchProperty::MasterDb { value: -1.0 },
        },
        name("b"),
        master(-4.0),
    ] {
        send(&mut s, step, Some("g"));
        assert_eq!(mirror.catch_up(&mut s), 1);
    }
    let after = s.control().workspace().clone();
    undo(&mut s);
    assert_eq!(mirror.catch_up(&mut s), 1);
    assert_eq!(s.control().workspace().settings.master_db, 0.0);
    assert!(!s.control().snapshot().document.can_undo);
    let rid = s.request_id();
    s.send(ClientMessage::Redo { id: rid });
    assert_eq!(mirror.catch_up(&mut s), 1);
    assert_eq!(s.control().workspace(), &after);
}

#[test]
fn a_refusal_for_another_reason_wins_over_the_playing_guard() {
    let mut s = playing_show();
    start(&mut s, 1);
    s.run(BLOCK * 8);
    let replies = send(
        &mut s,
        Command::RemovePatch {
            id: patch_id(),
            even_if_used: false,
        },
        None,
    );
    let message = refusal(&replies).expect("refused");
    assert!(message.contains("is used by cue"), "{message}");
}
