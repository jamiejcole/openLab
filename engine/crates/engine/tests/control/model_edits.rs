// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! Settings, patch, network patch and cue list commands through the control loop
//! (ED-07): what clients are sent, what the engine and mixer do with them, patch layout edits
//! under a playing voice (stable mixer slots), the workspace master in the
//! matrices, undo coalescing and edits that change nothing.

use crate::show::*;
use openlab_core::protocol::WireWarning;
use openlab_core::protocol::{
    ClientMessage, ErrorCode, Outcome, ServerMessage, TransportAction, WireProblem,
};
use openlab_core::{
    Command, CueKind, CueProperty, NetworkPatch, NetworkPatchProperty, NetworkProtocol,
    OutputPatch, PatchId, PatchMapping, PatchOutput, PatchProperty, TransportKey, Workspace,
    WorkspaceSetting,
};
use openlab_engine::control::offline::OfflineShow;

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
    let replies = s.send(ClientMessage::Command {
        id,
        command: Box::new(command),
        coalesce: coalesce.map(str::to_owned),
    });
    assert_eq!(s.control().engine().unsignalled_model_changes(), 0);
    replies
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

/// A show like [`playing_show`] with room for four patches in the mixer: patch 2 (unused by
/// any cue) is mapped crossed, to device outputs 1 and 0, so a voice that ended up on its
/// matrix would be heard on output 1 instead of 0.
fn roomy_playing_show(mixer_patches: usize) -> OfflineShow {
    let path = wav(&dc(RATE as usize * 20, 0.5));
    let mut ws = workspace(vec![audio(1, &path, 0)]);
    ws.patches.push(patch(2));
    let mut c = config(&ws);
    for (n, channels) in [(2, vec![1, 0]), (3, vec![1, 1]), (4, vec![1, 0])] {
        c.machine
            .set_patch_mapping(
                ws.id,
                patch(n).id,
                PatchMapping {
                    device: "Offline".into(),
                    channels,
                },
            )
            .unwrap();
    }
    c.mixer_patches = mixer_patches;
    let mut m = mixer_config();
    m.patches = mixer_patches;
    let mut s = OfflineShow::new(ws, m, c).unwrap();
    s.settle_media();
    s
}

/// Every sample since cue 1's voice was first heard is exactly its DC on output 0 and silence
/// on output 1: no edit since made a single sample move.
fn heard_as_routed(s: &OfflineShow) {
    let from = first_nonzero(&s.channel(0)).expect("cue 1 is heard");
    heard_as_routed_since(s, from);
}

/// [`heard_as_routed`] from sample `from` on.
fn heard_as_routed_since(s: &OfflineShow, from: usize) {
    for (ch, want) in [(0, 0.5f32), (1, 0.0)] {
        let x = s.channel(ch);
        assert!(x.len() > from, "nothing rendered since {from}");
        if let Some(i) = x[from..].iter().position(|v| v.to_bits() != want.to_bits()) {
            panic!("output {ch}, sample {}: {}", from + i, x[from + i]);
        }
    }
}

/// The last block of output 0 is exactly `want` (after a level edit's de-zipper ramp).
fn last_block_of_output_0(s: &OfflineShow, want: f32) {
    let x = s.channel(0);
    let tail = &x[x.len() - BLOCK as usize..];
    assert!(
        tail.iter().all(|v| v.to_bits() == want.to_bits()),
        "want {want}: {tail:?}"
    );
}

fn notice_codes(s: &OfflineShow) -> Vec<String> {
    s.control()
        .snapshot()
        .warnings
        .into_iter()
        .filter_map(|w| match w {
            WireWarning::Notice { code, .. } => Some(code),
            _ => None,
        })
        .collect()
}

#[test]
fn patch_layout_edits_succeed_while_a_voice_plays_and_keep_its_routing() {
    let mut s = roomy_playing_show(4);
    let mut mirror = Mirror::new(&s);
    start(&mut s, 1);
    s.run(BLOCK * 8);
    assert!(s.control().engine().state(id(1)).is_active());
    heard_as_routed(&s);
    let edits = [
        // Insert before the playing cue's patch (index 0): every later patch shifts.
        Command::AddPatch {
            index: 0,
            patch: patch(3),
        },
        Command::MovePatch {
            id: patch_id(),
            index: 2,
        },
        Command::MovePatch {
            id: patch(2).id,
            index: 0,
        },
        Command::RemovePatch {
            id: patch(3).id,
            even_if_used: false,
        },
        // An append followed by a move to the front, as one step.
        Command::Transaction {
            commands: vec![
                Command::AddPatch {
                    index: 2,
                    patch: patch(4),
                },
                Command::MovePatch {
                    id: patch(4).id,
                    index: 0,
                },
            ],
        },
    ];
    let count = edits.len();
    for edit in edits {
        command(&mut s, edit.clone());
        s.run(BLOCK * 4);
        heard_as_routed(&s);
        assert!(s.control().engine().state(id(1)).is_active(), "{edit:?}");
    }
    // Undo and redo of every step are layout edits too.
    for _ in 0..count {
        assert!(refusal(&undo(&mut s)).is_none());
        s.run(BLOCK * 4);
        heard_as_routed(&s);
    }
    assert_eq!(
        s.control()
            .workspace()
            .patches
            .iter()
            .map(|p| p.id)
            .collect::<Vec<_>>(),
        vec![patch_id(), patch(2).id]
    );
    for _ in 0..count {
        let rid = s.request_id();
        let replies = s.send(ClientMessage::Redo { id: rid });
        assert!(refusal(&replies).is_none());
        s.run(BLOCK * 4);
        heard_as_routed(&s);
    }
    assert_eq!(mirror.catch_up(&mut s), 3 * count);
    assert!(s.control().engine().state(id(1)).is_active());
    assert!(!notice_codes(&s).contains(&"mixer_patches_exhausted".to_owned()));
}

#[test]
fn a_resize_under_a_playing_voice_leaves_it_as_it_started_and_applies_to_the_next_start() {
    let mut s = roomy_playing_show(4);
    start(&mut s, 1);
    s.run(BLOCK * 8);
    heard_as_routed(&s);
    // Four outputs: the machine's mapping (two channels) no longer fits, so the patch cannot
    // play new starts, but the voice already sounding keeps the layout it started with.
    command(
        &mut s,
        Command::SetPatchProperty {
            id: patch_id(),
            property: PatchProperty::ChannelCount { value: 4 },
        },
    );
    s.run(BLOCK * 8);
    heard_as_routed(&s);
    assert!(s.control().engine().state(id(1)).is_active());
    // Undo while it still plays: back to two outputs, still as routed.
    assert!(refusal(&undo(&mut s)).is_none());
    s.run(BLOCK * 8);
    heard_as_routed(&s);
    s.transport(TransportAction::HardStop { cue: id(1) });
    run_until_idle(&mut s, id(1), 8000);
    s.run(BLOCK * 16);
    // A start after the voice left plays through the patch as it is now.
    let restart = s.channel(0).len();
    start(&mut s, 1);
    s.run(BLOCK * 8);
    let heard = restart + first_nonzero(&s.channel(0)[restart..]).unwrap();
    heard_as_routed_since(&s, heard);
}

#[test]
fn a_voice_kept_on_its_old_layout_by_a_resize_still_follows_the_patch_settings() {
    let mut s = roomy_playing_show(4);
    start(&mut s, 1);
    s.run(BLOCK * 8);
    command(
        &mut s,
        Command::SetPatchProperty {
            id: patch_id(),
            property: PatchProperty::ChannelCount { value: 3 },
        },
    );
    s.run(BLOCK * 8);
    heard_as_routed(&s);
    let set = |s: &mut OfflineShow, property| {
        command(
            s,
            Command::SetPatchProperty {
                id: patch_id(),
                property,
            },
        );
        s.run(BLOCK * 8);
    };
    // The operator mutes the output the playing cue is on: it goes silent.
    set(
        &mut s,
        PatchProperty::OutputMute {
            output: 0,
            value: true,
        },
    );
    last_block_of_output_0(&s, 0.0);
    set(
        &mut s,
        PatchProperty::OutputMute {
            output: 0,
            value: false,
        },
    );
    last_block_of_output_0(&s, 0.5);
    // The patch master and the output level reach it too, and polarity.
    set(&mut s, PatchProperty::MasterDb { value: -6.0 });
    last_block_of_output_0(&s, 0.5 * openlab_core::db_to_gain(-6.0));
    set(&mut s, PatchProperty::MasterDb { value: 0.0 });
    set(
        &mut s,
        PatchProperty::OutputLevel {
            output: 0,
            value: -12.0,
        },
    );
    last_block_of_output_0(&s, 0.5 * openlab_core::db_to_gain(-12.0));
    set(
        &mut s,
        PatchProperty::OutputPolarity {
            output: 0,
            value: true,
        },
    );
    last_block_of_output_0(&s, -(0.5 * openlab_core::db_to_gain(-12.0)));
    assert!(s.control().engine().state(id(1)).is_active());
}

#[test]
fn a_patch_with_no_free_mixer_slot_waits_for_the_one_a_voice_still_uses() {
    // Two slots: patch 901 (cue 1, slot 0) and patch 2 (slot 1).
    let mut s = roomy_playing_show(2);
    start(&mut s, 1);
    s.run(BLOCK * 8);
    // Patch 901 is removed (with its cue's reference left dangling, as an undo of its add
    // would) and patch 3 added: slot 0 still carries the playing voice, so 3 has none.
    command(
        &mut s,
        Command::Transaction {
            commands: vec![
                Command::RemovePatch {
                    id: patch_id(),
                    even_if_used: true,
                },
                Command::AddPatch {
                    index: 1,
                    patch: patch(3),
                },
            ],
        },
    );
    s.run(BLOCK * 8);
    heard_as_routed(&s);
    assert!(notice_codes(&s).contains(&"mixer_patches_exhausted".to_owned()));
    // The voice ends; its slot is freed and patch 3 gets it (mapped to output 1 twice).
    s.transport(TransportAction::Panic);
    s.run(RATE as u64);
    s.run(BLOCK * 16);
    assert!(!notice_codes(&s).contains(&"mixer_patches_exhausted".to_owned()));
    let x = s.channel(0);
    assert!(x[x.len() - BLOCK as usize..].iter().all(|v| *v == 0.0));
}

#[test]
fn a_paused_voice_keeps_its_slot_and_resumes_as_routed() {
    let mut s = roomy_playing_show(2);
    start(&mut s, 1);
    s.run(BLOCK * 8);
    s.transport(TransportAction::Pause { cue: id(1) });
    s.run(BLOCK * 8);
    command(
        &mut s,
        Command::Transaction {
            commands: vec![
                Command::RemovePatch {
                    id: patch_id(),
                    even_if_used: true,
                },
                Command::AddPatch {
                    index: 1,
                    patch: patch(3),
                },
            ],
        },
    );
    s.run(BLOCK * 8);
    // The paused voice still holds slot 0, so patch 3 must wait for it.
    assert!(notice_codes(&s).contains(&"mixer_patches_exhausted".to_owned()));
    s.transport(TransportAction::Resume { cue: id(1) });
    s.run(BLOCK * 8);
    let resumed = s.channel(0).len() - BLOCK as usize * 2;
    heard_as_routed_since(&s, resumed);
}

#[test]
fn the_workspace_master_scales_each_sample_exactly() {
    let samples = ramp(RATE as usize);
    let path = wav(&samples);
    for db in [0.0, -6.0, -20.0] {
        let mut ws = workspace(vec![audio(1, &path, 0)]);
        ws.settings.master_db = db;
        let mut s = show(ws);
        // Past the matrices' de-zipper ramp from the mixer's identity start.
        s.run(BLOCK * 4);
        start(&mut s, 1);
        s.run(RATE as u64 / 2);
        let out = s.channel(0);
        let first = first_nonzero(&out).unwrap();
        let g = openlab_core::db_to_gain(db);
        let n = RATE as usize / 4;
        for (i, (y, x)) in out[first..first + n].iter().zip(&samples).enumerate() {
            assert_eq!(y.to_bits(), (g * x).to_bits(), "{db} dB, sample {i}");
        }
        if db == 0.0 {
            // Bit for bit the file: what the mixer's own master at unity gave before.
            assert_eq!(&out[first..first + n], &samples[..n]);
        }
    }
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
fn removing_a_patch_a_playing_cue_uses_is_still_refused_as_used() {
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

/// M5.6b: a Wait's duration and a Load cue's time edited through the control loop (as the
/// inspector sends them) reach clients as `model_changed`, a bad value or a cue without the
/// field is refused with nothing sent, undo restores, and a running Wait whose duration is
/// edited takes the new duration (QLab 5).
#[test]
fn duration_and_load_time_edits_go_through_the_engine_and_undo() {
    let wait = cue(1, CueKind::Wait { duration: 1.0 });
    let load = cue(
        2,
        CueKind::Load {
            target: Some(id(1)),
            load_to_time: 0.0,
        },
    );
    let mut s = show(workspace(vec![wait, load]));
    let before = s.control().workspace().clone();
    let mut mirror = Mirror::new(&s);
    let set = |n: u128, property: CueProperty| Command::SetCueProperty {
        id: id(n),
        property,
    };
    command(&mut s, set(1, CueProperty::Duration { value: 2.5 }));
    command(&mut s, set(2, CueProperty::LoadToTime { value: 0.75 }));
    assert_eq!(mirror.catch_up(&mut s), 2);
    for (n, bad, field) in [
        (1, CueProperty::Duration { value: -1.0 }, "duration"),
        (1, CueProperty::Duration { value: 1e9 }, "duration"),
        (2, CueProperty::LoadToTime { value: -0.5 }, "load_to_time"),
    ] {
        let message = refusal(&send(&mut s, set(n, bad), None)).expect("refused");
        assert!(message.contains(field), "{message}");
    }
    assert!(
        refusal(&send(
            &mut s,
            set(2, CueProperty::Duration { value: 1.0 }),
            None
        ))
        .is_some()
    );
    assert_eq!(mirror.catch_up(&mut s), 0);
    undo(&mut s);
    undo(&mut s);
    assert_eq!(mirror.catch_up(&mut s), 2);
    assert_eq!(s.control().workspace(), &before);

    // The Wait (1 s) is running when it is made 3 s long: it ends after 3 s (the idle state is
    // seen at block granularity, so within a block of it).
    let started = go(&mut s);
    s.step();
    assert_eq!(
        s.control().engine().state(id(1)),
        openlab_core::CueState::Running
    );
    command(&mut s, set(1, CueProperty::Duration { value: 3.0 }));
    let idle = run_until_idle(&mut s, id(1), u64::from(RATE) * 5);
    let ran = idle.abs_diff(started);
    assert!(
        ran.abs_diff(u64::from(RATE) * 3) <= u64::from(RATE) / 10,
        "ran {ran} frames"
    );
}
