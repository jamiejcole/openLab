// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! Settings, patch, network patch and cue list commands through the control loop
//! (ED-07): what clients are sent, what the engine and mixer do with them, patch layout edits
//! under a playing voice (stable mixer slots), the patch levels in the
//! matrices, undo coalescing and edits that change nothing.

use crate::show::*;
use openlab_core::protocol::WireWarning;
use openlab_core::protocol::{
    ClientMessage, ErrorCode, Outcome, ServerMessage, TransportAction, WireProblem,
};
use openlab_core::{
    Command, CueKind, CueProperty, NetworkPatch, NetworkPatchProperty, NetworkProtocol,
    OutputPatch, PatchId, PatchMapping, PatchOutput, PatchProperty, SILENCE_DB, TransportKey,
    Workspace, WorkspaceSetting,
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

fn minimum_limit(db: f64) -> Command {
    Command::SetWorkspaceSetting {
        setting: WorkspaceSetting::OutputMinDb { value: db - 60.0 },
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
        minimum_limit(-3.0),
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
    assert_eq!(s.control().workspace().settings.output_min_db, -60.0);
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

// ---- Patch main and output faders, and the workspace volume limits (PB-11) ----

/// The loudest sample of the last two blocks of output channel `ch`.
fn peak(s: &OfflineShow, ch: usize) -> f32 {
    let out = s.channel(ch);
    out[out.len() - BLOCK as usize * 2..]
        .iter()
        .fold(0.0f32, |m, x| m.max(x.abs()))
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() <= b.abs() * 0.01 + 1e-6
}

/// A show playing a steady 0.2 on both output channels (file channel 1 to cue outputs 1 and 2).
fn steady_show(edit: impl FnOnce(&mut Workspace)) -> OfflineShow {
    let path = wav(&dc(RATE as usize * 20, 0.2));
    let mut cue = audio(1, &path, 0);
    if let CueKind::Audio { levels, .. } = &mut cue.kind {
        levels.crosspoints.push(openlab_core::Crosspoint {
            input: 0,
            output: 1,
            db: 0.0,
            mute: false,
        });
    }
    let mut ws = workspace(vec![cue]);
    edit(&mut ws);
    let mut s = show(ws);
    start(&mut s, 1);
    s.run(BLOCK * 8);
    s
}

fn master_of_patch(db: f64) -> Command {
    Command::SetPatchProperty {
        id: patch_id(),
        property: PatchProperty::MasterDb { value: db },
    }
}

fn output_of_patch(output: usize, db: f64) -> Command {
    Command::SetPatchProperty {
        id: patch_id(),
        property: PatchProperty::OutputLevel { output, value: db },
    }
}

fn cue_master(s: &OfflineShow) -> f64 {
    match &s.control().workspace().find_cue(id(1)).unwrap().kind {
        CueKind::Audio { levels, .. } => levels.master_db,
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_patch_main_level_changes_what_is_heard_and_leaves_the_cue_alone() {
    let mut s = steady_show(|_| {});
    assert!(near(peak(&s, 0), 0.2) && near(peak(&s, 1), 0.2));
    command(&mut s, master_of_patch(-12.0));
    s.run(BLOCK * 8);
    let drop = openlab_core::db_to_gain(-12.0);
    assert!(near(peak(&s, 0), 0.2 * drop), "{}", peak(&s, 0));
    assert!(near(peak(&s, 1), 0.2 * drop), "{}", peak(&s, 1));
    assert_eq!(cue_master(&s), 0.0, "the cue's own levels are untouched");
    // Still playing: the next block has sound in it.
    s.run(BLOCK * 8);
    assert!(peak(&s, 0) > 0.0);
}

#[test]
fn one_patch_output_fader_silences_that_output_only() {
    let mut s = steady_show(|_| {});
    command(&mut s, output_of_patch(1, SILENCE_DB));
    s.run(BLOCK * 8);
    assert_eq!(peak(&s, 1), 0.0, "output 2 is silent");
    assert!(near(peak(&s, 0), 0.2), "output 1 plays at its level");
}

#[test]
fn a_patch_level_above_the_maximum_is_put_at_it_and_no_louder_is_heard() {
    let mut s = steady_show(|_| {});
    command(
        &mut s,
        Command::SetWorkspaceSetting {
            setting: WorkspaceSetting::OutputMaxDb { value: 0.0 },
        },
    );
    command(&mut s, master_of_patch(6.0));
    s.run(BLOCK * 8);
    assert_eq!(s.control().workspace().patches[0].master_db, 0.0);
    assert!(near(peak(&s, 0), 0.2), "{}", peak(&s, 0));
    // A peer's level in a file or command can be nothing but within the limits.
    command(&mut s, output_of_patch(0, 12.0));
    s.run(BLOCK * 8);
    assert_eq!(s.control().workspace().patches[0].outputs[0].level_db, 0.0);
    assert!(near(peak(&s, 0), 0.2), "{}", peak(&s, 0));
}

#[test]
fn lowering_the_maximum_pulls_playing_levels_down_and_is_one_undo_step() {
    let mut s = steady_show(|ws| {
        ws.patches[0].master_db = 6.0;
        if let CueKind::Audio { levels, .. } = &mut ws.cue_lists[0].cues[0].kind {
            levels.master_db = 6.0;
        }
    });
    let loud = 0.2 * openlab_core::db_to_gain(12.0);
    assert!(near(peak(&s, 0), loud), "{}", peak(&s, 0));
    let mut mirror = Mirror::new(&s);
    command(
        &mut s,
        Command::SetWorkspaceSetting {
            setting: WorkspaceSetting::OutputMaxDb { value: 0.0 },
        },
    );
    s.run(BLOCK * 8);
    assert_eq!(
        cue_master(&s),
        0.0,
        "the cue's level reads the clamped value"
    );
    assert_eq!(s.control().workspace().patches[0].master_db, 0.0);
    assert!(near(peak(&s, 0), 0.2), "{}", peak(&s, 0));
    // Clients follow every level the change moved, as ordinary edits of them.
    assert_eq!(mirror.catch_up(&mut s), 1);
    // One step gives it all back.
    let undo = s.request_id();
    let replies = s.send(ClientMessage::Undo { id: undo });
    assert!(refusal(&replies).is_none(), "{replies:?}");
    s.run(BLOCK * 8);
    assert_eq!(cue_master(&s), 6.0);
    assert_eq!(s.control().workspace().settings.output_max_db, 12.0);
    assert!(near(peak(&s, 0), loud), "{}", peak(&s, 0));
    assert_eq!(mirror.catch_up(&mut s), 1);
}

/// A Fade cue `n` of the steady cue's master to `db` over `seconds`.
fn fade_of_steady(
    n: u128,
    db: f64,
    seconds: f64,
    stop_target_when_done: bool,
) -> openlab_core::Cue {
    cue(
        n,
        CueKind::Fade {
            target: Some(id(1)),
            duration: seconds,
            curve: openlab_core::FadeCurve::Linear,
            levels: openlab_core::FadeLevels {
                master_db: Some(db),
                output_db: vec![],
                crosspoints: vec![],
            },
            stop_target_when_done,
            mode: Default::default(),
        },
    )
}

fn set_maximum(s: &mut OfflineShow, db: f64) {
    command(
        s,
        Command::SetWorkspaceSetting {
            setting: WorkspaceSetting::OutputMaxDb { value: db },
        },
    );
}

/// The loudest sample of output channel 1 from sample `from` on.
fn peak_since(s: &OfflineShow, from: usize) -> f32 {
    s.channel(0)[from..]
        .iter()
        .fold(0.0f32, |m, x| m.max(x.abs()))
}

#[test]
fn lowering_the_maximum_pulls_down_a_level_a_fade_has_set() {
    let mut s = steady_show(|ws| {
        ws.cue_lists[0]
            .cues
            .push(fade_of_steady(2, 6.0, 0.0, false))
    });
    start(&mut s, 2);
    s.run(BLOCK * 8);
    assert!(near(peak(&s, 0), 0.2 * openlab_core::db_to_gain(6.0)));
    let before = s.channel(0).len();
    set_maximum(&mut s, 0.0);
    s.run(BLOCK * 8);
    assert!(near(peak(&s, 0), 0.2), "{}", peak(&s, 0));
    // De-zippered, not a step: no sample jumps by more than a 10 ms ramp's share of the change.
    let out = s.channel(0);
    let largest_step = out[before..]
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    let ramp_step = (0.399 - 0.2) / (RATE as f32 * 0.010);
    assert!(largest_step < ramp_step * 1.5, "{largest_step}");
}

#[test]
fn lowering_the_maximum_keeps_a_running_fade_from_passing_it() {
    let mut s = steady_show(|ws| {
        ws.cue_lists[0]
            .cues
            .push(fade_of_steady(2, 12.0, 1.0, false))
    });
    start(&mut s, 2);
    s.run(u64::from(RATE) / 10);
    // On its way up, below the new maximum: it carries on rising, and stops there.
    set_maximum(&mut s, 6.0);
    s.run(u64::from(RATE) * 2);
    let top = 0.2 * openlab_core::db_to_gain(6.0);
    assert!(near(peak(&s, 0), top), "{}", peak(&s, 0));
    assert!(peak_since(&s, 0) <= top * 1.01, "{}", peak_since(&s, 0));
}

#[test]
fn lowering_the_maximum_under_a_running_fade_brings_it_down_at_once() {
    let mut s = steady_show(|ws| {
        ws.cue_lists[0]
            .cues
            .push(fade_of_steady(2, 12.0, 4.0, false))
    });
    start(&mut s, 2);
    s.run(u64::from(RATE) * 2); // half way up the fade, well above 0 dB
    assert!(peak(&s, 0) > 0.25, "{}", peak(&s, 0));
    set_maximum(&mut s, 0.0);
    let settled = s.channel(0).len() + (RATE as usize / 50) + BLOCK as usize;
    s.run(u64::from(RATE) * 4);
    assert!(
        peak_since(&s, settled) <= 0.2 * 1.01,
        "{}",
        peak_since(&s, settled)
    );
    assert!(near(peak(&s, 0), 0.2), "{}", peak(&s, 0));
}

/// A fade-out passing through the region above the new maximum: at once no louder than it, and it
/// still gets where it was going, on time, with its "stop target when done".
#[test]
fn lowering_the_maximum_during_a_fade_out_lets_it_finish_its_way_down() {
    let mut s = steady_show(|ws| {
        ws.cue_lists[0]
            .cues
            .push(fade_of_steady(2, 6.0, 0.0, false));
        ws.cue_lists[0]
            .cues
            .push(fade_of_steady(3, SILENCE_DB, 4.0, true));
    });
    start(&mut s, 2);
    s.run(BLOCK * 8);
    start(&mut s, 3);
    s.run(BLOCK * 8);
    assert!(peak(&s, 0) > 0.3, "{}", peak(&s, 0));
    set_maximum(&mut s, 0.0);
    let settled = s.channel(0).len() + (RATE as usize / 50) + BLOCK as usize;
    s.run(u64::from(RATE));
    assert!(
        peak_since(&s, settled) <= 0.2 * 1.01,
        "{}",
        peak_since(&s, settled)
    );
    assert!(peak(&s, 0) > 0.05, "still fading, not cut: {}", peak(&s, 0));
    s.run(u64::from(RATE) * 4);
    assert_eq!(peak(&s, 0), 0.0, "it reached silence");
    assert_eq!(
        s.control().engine().state(id(1)),
        openlab_core::CueState::Idle,
        "and stopped its target"
    );
}

/// The cue's own level is above the new maximum and a Fade is taking it down: the sweep of the
/// model is the limit's, not the operator taking the level over, so the Fade keeps it and its
/// stop still applies.
#[test]
fn a_maximum_that_moves_the_cues_own_level_does_not_end_a_fade_of_it() {
    let mut s = steady_show(|ws| {
        if let CueKind::Audio { levels, .. } = &mut ws.cue_lists[0].cues[0].kind {
            levels.master_db = 6.0;
        }
        ws.cue_lists[0]
            .cues
            .push(fade_of_steady(2, SILENCE_DB, 2.0, true));
    });
    start(&mut s, 2);
    s.run(u64::from(RATE));
    set_maximum(&mut s, -30.0);
    assert_eq!(cue_master(&s), -30.0);
    s.run(u64::from(RATE) * 2);
    assert_eq!(peak(&s, 0), 0.0, "the fade-out went on");
    assert_eq!(
        s.control().engine().state(id(1)),
        openlab_core::CueState::Idle,
        "and stopped its target"
    );
}

/// A level a Fade has taken below the cue's own: a maximum that moves the cue's level must not
/// bring the sound back up to it.
#[test]
fn a_maximum_that_moves_the_cues_own_level_does_not_raise_a_faded_level() {
    let mut s = steady_show(|ws| {
        if let CueKind::Audio { levels, .. } = &mut ws.cue_lists[0].cues[0].kind {
            levels.master_db = 6.0;
        }
        ws.cue_lists[0]
            .cues
            .push(fade_of_steady(2, -20.0, 0.0, false));
    });
    start(&mut s, 2);
    s.run(BLOCK * 8);
    let faded = 0.2 * openlab_core::db_to_gain(-20.0);
    assert!(near(peak(&s, 0), faded), "{}", peak(&s, 0));
    set_maximum(&mut s, 0.0);
    s.run(BLOCK * 8);
    assert!(near(peak(&s, 0), faded), "{}", peak(&s, 0));
}

/// A paused Fade holds its level; lowered under it, the level comes down now and the resume goes
/// no higher than the maximum.
#[test]
fn a_paused_fade_resumes_within_a_lowered_maximum() {
    let mut s = steady_show(|ws| {
        ws.cue_lists[0]
            .cues
            .push(fade_of_steady(2, 12.0, 1.0, false))
    });
    start(&mut s, 2);
    s.run(u64::from(RATE) * 3 / 4);
    s.transport(TransportAction::Pause { cue: id(2) });
    s.run(BLOCK * 8);
    assert!(peak(&s, 0) > 0.3, "{}", peak(&s, 0));
    set_maximum(&mut s, 0.0);
    s.run(BLOCK * 8);
    assert!(near(peak(&s, 0), 0.2), "{}", peak(&s, 0));
    s.transport(TransportAction::Resume { cue: id(2) });
    s.run(u64::from(RATE));
    assert!(near(peak(&s, 0), 0.2), "{}", peak(&s, 0));
}

#[test]
fn raising_the_minimum_silences_a_level_a_fade_has_set_at_or_below_it() {
    let mut s = steady_show(|ws| {
        ws.settings.output_min_db = -80.0;
        ws.cue_lists[0]
            .cues
            .push(fade_of_steady(2, -50.0, 0.0, false));
    });
    start(&mut s, 2);
    s.run(BLOCK * 8);
    assert!(peak(&s, 0) > 0.0);
    command(
        &mut s,
        Command::SetWorkspaceSetting {
            setting: WorkspaceSetting::OutputMinDb { value: -40.0 },
        },
    );
    s.run(BLOCK * 8);
    assert_eq!(peak(&s, 0), 0.0);
}

/// A paused Fade that was shortened ends where its planned curve is at the new length, which from
/// a level above a lowered maximum is above it too: the resume is limited as well.
#[test]
fn a_shortened_paused_fade_resumes_within_a_lowered_maximum() {
    let mut s = steady_show(|ws| {
        if let CueKind::Audio { levels, .. } = &mut ws.cue_lists[0].cues[0].kind {
            levels.master_db = 6.0;
        }
        ws.cue_lists[0]
            .cues
            .push(fade_of_steady(2, SILENCE_DB, 4.0, false));
    });
    start(&mut s, 2);
    s.run(u64::from(RATE) / 4);
    s.transport(TransportAction::Pause { cue: id(2) });
    s.run(BLOCK * 8);
    command(
        &mut s,
        Command::SetCueProperty {
            id: id(2),
            property: CueProperty::Duration { value: 1.0 },
        },
    );
    set_maximum(&mut s, 0.0);
    s.run(BLOCK * 8);
    s.transport(TransportAction::Resume { cue: id(2) });
    let resumed = s.channel(0).len();
    s.run(u64::from(RATE));
    assert!(
        peak_since(&s, resumed) <= 0.2 * 1.01,
        "{}",
        peak_since(&s, resumed)
    );
}

/// A fade-out above the new maximum goes to it in 10 ms and on down from there; whatever comes in
/// those 10 ms (from a second client or OSC) comes after both steps and is what stays.
fn narrowed_during_a_fade_out(blocks_before: u64) -> OfflineShow {
    let mut s = steady_show(|ws| {
        ws.cue_lists[0]
            .cues
            .push(fade_of_steady(2, 6.0, 0.0, false));
        let mut out = fade_of_steady(3, SILENCE_DB, 4.0, true);
        out.continue_mode = openlab_core::ContinueMode::AutoFollow;
        ws.cue_lists[0].cues.push(out);
        ws.cue_lists[0]
            .cues
            .push(cue(4, CueKind::Wait { duration: 60.0 }));
    });
    start(&mut s, 2);
    s.run(BLOCK * 8);
    start(&mut s, 3);
    s.run(BLOCK * 8);
    set_maximum(&mut s, 0.0);
    s.run(BLOCK * blocks_before);
    s
}

#[test]
fn an_edit_just_after_a_narrowing_wins_over_the_fade_it_interrupted() {
    for blocks in [0, 1] {
        let mut s = narrowed_during_a_fade_out(blocks);
        let mut kind = s
            .control()
            .workspace()
            .find_cue(id(1))
            .unwrap()
            .kind
            .clone();
        if let CueKind::Audio { levels, .. } = &mut kind {
            levels.master_db = -6.0;
        }
        command(
            &mut s,
            Command::SetCueProperty {
                id: id(1),
                property: CueProperty::Kind { value: kind },
            },
        );
        let want = 0.2 * openlab_core::db_to_gain(-6.0);
        s.run(BLOCK * 8);
        assert!(near(peak(&s, 0), want), "soon, {blocks}: {}", peak(&s, 0));
        s.run(u64::from(RATE) * 5);
        assert!(near(peak(&s, 0), want), "late, {blocks}: {}", peak(&s, 0));
        assert_eq!(
            s.control().engine().state(id(1)),
            openlab_core::CueState::Running,
            "the edit took the level over: its stop does not apply"
        );
        // The Fade completed at once, so its auto-follow fired.
        assert_eq!(
            s.control().engine().state(id(4)),
            openlab_core::CueState::Running
        );
    }
}

#[test]
fn stopping_a_fade_just_after_a_narrowing_holds_its_level() {
    for blocks in [0, 1] {
        let mut s = narrowed_during_a_fade_out(blocks);
        s.transport(TransportAction::Stop {
            cue: id(3),
            fade_out: None,
        });
        s.run(BLOCK * 8);
        assert!(near(peak(&s, 0), 0.2), "soon, {blocks}: {}", peak(&s, 0));
        s.run(u64::from(RATE) * 5);
        assert!(near(peak(&s, 0), 0.2), "late, {blocks}: {}", peak(&s, 0));
        assert_eq!(
            s.control().engine().state(id(1)),
            openlab_core::CueState::Running
        );
    }
}

#[test]
fn a_stop_of_the_cue_just_after_a_narrowing_is_not_overtaken() {
    let mut s = narrowed_during_a_fade_out(0);
    s.transport(TransportAction::HardStop { cue: id(1) });
    s.run(BLOCK * 8);
    assert_eq!(peak(&s, 0), 0.0);
}

#[test]
fn a_limit_out_of_range_is_refused_and_the_old_one_stays() {
    let mut s = steady_show(|_| {});
    for bad in [
        WorkspaceSetting::OutputMaxDb { value: -31.0 },
        WorkspaceSetting::OutputMaxDb { value: 13.0 },
        WorkspaceSetting::OutputMinDb { value: -39.0 },
        WorkspaceSetting::OutputMinDb { value: -181.0 },
    ] {
        let replies = send(&mut s, Command::SetWorkspaceSetting { setting: bad }, None);
        assert!(refusal(&replies).is_some(), "{replies:?}");
    }
    let settings = &s.control().workspace().settings;
    assert_eq!(
        (settings.output_max_db, settings.output_min_db),
        (12.0, -60.0)
    );
}

#[test]
fn edits_that_change_nothing_make_no_revision_and_no_undo_step() {
    let mut s = playing_show();
    let mut mirror = Mirror::new(&s);
    let before = s.control().snapshot();
    let passes = s.control().engine().whole_show_passes();
    for same in [
        minimum_limit(0.0),
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
    // Nor a rebuild in the cue engine, though most of them are model-wide (a repeated no-op
    // must not cost a whole-show pass each time).
    s.transport(TransportAction::ResumeAll);
    assert_eq!(s.control().engine().whole_show_passes(), passes);
}

#[test]
fn a_fader_drag_with_one_key_is_one_undo_step_but_every_value_reaches_clients() {
    let mut s = playing_show();
    let mut mirror = Mirror::new(&s);
    let revision = s.control().revision();
    for db in [-1.0, -2.0, -3.0, -4.0] {
        let replies = send(&mut s, minimum_limit(db), Some("limit:1"));
        assert!(refusal(&replies).is_none());
        assert_eq!(mirror.catch_up(&mut s), 1, "each value is shown at once");
    }
    assert_eq!(s.control().revision(), revision + 4);
    assert_eq!(s.control().workspace().settings.output_min_db, -64.0);
    assert!(s.control().snapshot().document.can_undo);

    // A second gesture is its own step.
    send(&mut s, minimum_limit(-9.0), Some("limit:2"));
    mirror.catch_up(&mut s);
    undo(&mut s);
    mirror.catch_up(&mut s);
    assert_eq!(s.control().workspace().settings.output_min_db, -64.0);
    undo(&mut s);
    mirror.catch_up(&mut s);
    assert_eq!(s.control().workspace().settings.output_min_db, -60.0);
    assert!(!s.control().snapshot().document.can_undo);
}

#[test]
fn a_command_without_a_key_never_merges_and_a_long_key_is_refused() {
    let mut s = playing_show();
    send(&mut s, minimum_limit(-1.0), None);
    send(&mut s, minimum_limit(-2.0), None);
    undo(&mut s);
    assert_eq!(s.control().workspace().settings.output_min_db, -61.0);

    let revision = s.control().revision();
    let replies = send(&mut s, minimum_limit(-5.0), Some(&"k".repeat(129)));
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
    assert_eq!(s.control().workspace().settings.output_min_db, -61.0);
    // The longest allowed key works.
    let replies = send(&mut s, minimum_limit(-5.0), Some(&"k".repeat(128)));
    assert!(refusal(&replies).is_none());
    assert_eq!(s.control().workspace().settings.output_min_db, -65.0);
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
        minimum_limit(-2.0),
        Command::SetPatchProperty {
            id: patch_id(),
            property: PatchProperty::MasterDb { value: -1.0 },
        },
        name("b"),
        minimum_limit(-4.0),
    ] {
        send(&mut s, step, Some("g"));
        assert_eq!(mirror.catch_up(&mut s), 1);
    }
    let after = s.control().workspace().clone();
    undo(&mut s);
    assert_eq!(mirror.catch_up(&mut s), 1);
    assert_eq!(s.control().workspace().settings.output_min_db, -60.0);
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

/// A Wait's duration and a Load cue's time edited through the control loop (as the
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

/// Core's marker limit and the mixer's slice-table limit are separate constants (core cannot
/// depend on the audio crate); a cue at the marker limit with a start offset inside its first
/// slice must still fit the table.
#[test]
fn the_cue_slice_marker_limit_fits_the_mixers_slice_table() {
    assert_eq!(
        openlab_core::MAX_SLICE_MARKERS + 2,
        openlab_audio::MAX_SLICES
    );
}

/// Each narrowing under a fade-out above it can date its steps past the one before, so they can
/// lie further ahead than any one call commits. Whatever comes after still comes last, at any
/// block size.
fn narrowed_twice_during_a_fade_out(rate: u32, block: u64) -> OfflineShow {
    let path = wav(&dc(RATE as usize * 20, 0.2));
    let mut steady = audio(1, &path, 0);
    if let CueKind::Audio { levels, .. } = &mut steady.kind {
        levels.master_db = 0.0;
    }
    let mut ws = workspace(vec![steady]);
    ws.cue_lists[0]
        .cues
        .push(fade_of_steady(2, 6.0, 0.0, false));
    ws.cue_lists[0]
        .cues
        .push(fade_of_steady(3, SILENCE_DB, 4.0, true));
    let mut s = show_at(ws, rate, block);
    start(&mut s, 1);
    s.run(block * 8);
    start(&mut s, 2);
    s.run(block * 8);
    start(&mut s, 3);
    s.run(block * 8);
    set_maximum(&mut s, 3.0);
    set_maximum(&mut s, 0.0);
    s
}

const NARROWING_CASES: [(u32, u64); 2] = [(RATE, BLOCK), (48_000, 512)];

#[test]
fn a_stop_after_two_narrowings_holds_the_fades_level() {
    for (rate, block) in NARROWING_CASES {
        let mut s = narrowed_twice_during_a_fade_out(rate, block);
        s.transport(TransportAction::Stop {
            cue: id(3),
            fade_out: None,
        });
        s.run(u64::from(rate) / 2);
        assert!(near(peak(&s, 0), 0.2), "soon, {rate}: {}", peak(&s, 0));
        s.run(u64::from(rate) * 5);
        assert!(near(peak(&s, 0), 0.2), "late, {rate}: {}", peak(&s, 0));
        assert_eq!(
            s.control().engine().state(id(1)),
            openlab_core::CueState::Running
        );
    }
}

#[test]
fn an_edit_after_two_narrowings_wins_over_the_fade() {
    for (rate, block) in NARROWING_CASES {
        let mut s = narrowed_twice_during_a_fade_out(rate, block);
        let mut kind = s
            .control()
            .workspace()
            .find_cue(id(1))
            .unwrap()
            .kind
            .clone();
        if let CueKind::Audio { levels, .. } = &mut kind {
            levels.master_db = -6.0;
        }
        command(
            &mut s,
            Command::SetCueProperty {
                id: id(1),
                property: CueProperty::Kind { value: kind },
            },
        );
        let want = 0.2 * openlab_core::db_to_gain(-6.0);
        s.run(u64::from(rate) / 2);
        assert!(near(peak(&s, 0), want), "soon, {rate}: {}", peak(&s, 0));
        s.run(u64::from(rate) * 5);
        assert!(near(peak(&s, 0), want), "late, {rate}: {}", peak(&s, 0));
    }
}
