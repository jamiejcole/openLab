// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `set_patch_mapping` (WS-07): an operator sets or clears where a patch plays on this
//! machine. Validated against the device list, stored in the machine settings, applied live.
//! Offline driver, virtual clock, settings in a temp folder.

use crate::common::{Env, env};
use crate::show::device::{DeviceShow, device, listed};
use crate::show::*;
use openlab_core::protocol::{
    ClientMessage, ErrorCode, Outcome, PreshowReport, Request, ResultData, ServerMessage,
    TransportAction, WireResolution,
};
use openlab_core::{
    Command, CueKind, CueState, DeviceInfo, MachineSettings, PatchId, PatchMapping, PatchProperty,
    Workspace,
};
use openlab_engine::control::ControlConfig;
use openlab_engine::control::offline::OfflineShow;
use openlab_engine::persist::load_machine_settings_checked;

fn mixer4() -> openlab_audio::MixerConfig {
    let mut m = mixer_config();
    m.output_channels = 4;
    m
}

/// A config whose machine settings come from the file in `env`, as the host loads them.
fn from_disk(env: &Env, devices: Vec<DeviceInfo>) -> ControlConfig {
    let mut c = ControlConfig::new(RATE, BLOCK);
    c.seed = 7;
    c.devices = devices;
    c.mixer_patches = 2;
    c.app_dirs = Some(env.dirs.clone());
    let (loaded, writable) = load_machine_settings_checked(&env.dirs);
    c.machine = loaded.settings;
    c.machine_writable = writable;
    c
}

fn start_show(ws: Workspace, c: ControlConfig) -> OfflineShow {
    let mut s = OfflineShow::new(ws, mixer4(), c).unwrap();
    s.settle_media();
    s
}

fn mapping(device: &str, channels: &[u32]) -> PatchMapping {
    PatchMapping {
        device: device.into(),
        channels: channels.to_vec(),
    }
}

/// Sends `set_patch_mapping`; the error code if it was refused.
fn set(
    s: &mut OfflineShow,
    patch: PatchId,
    mapping: Option<PatchMapping>,
) -> Result<(), ErrorCode> {
    let id = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id,
        request: Request::SetPatchMapping { patch, mapping },
    });
    let outcome = replies
        .iter()
        .find_map(|m| match m {
            ServerMessage::Result { outcome, .. } => Some(outcome.clone()),
            _ => None,
        })
        .expect("no result");
    match outcome {
        Outcome::Ok(_) => Ok(()),
        Outcome::Error(e) => Err(e.code),
    }
}

fn preshow(s: &mut OfflineShow) -> PreshowReport {
    let id = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id,
        request: Request::PreshowCheck,
    });
    replies
        .iter()
        .find_map(|m| match m {
            ServerMessage::Result {
                outcome:
                    Outcome::Ok(openlab_core::protocol::OkBody {
                        data: Some(ResultData::PreshowCheck(report)),
                        ..
                    }),
                ..
            } => Some((**report).clone()),
            _ => None,
        })
        .expect("no preshow report")
}

fn stored(s: &OfflineShow, ws: &Workspace) -> Option<PatchMapping> {
    s.control()
        .machine()
        .patch_mapping(ws.id, patch_id())
        .cloned()
}

#[test]
fn setting_a_mapping_stores_it_and_moves_a_playing_cue() {
    let env = env();
    let path = wav(&dc(4000, 0.5));
    let ws = workspace(vec![audio(1, &path, 0)]);
    let mut s = start_show(ws.clone(), from_disk(&env, vec![device("Offline", 4)]));
    // The default mapping put the patch on outputs 1-2.
    assert_eq!(stored(&s, &ws), Some(mapping("Offline", &[0, 1])));
    s.transport(TransportAction::Start { cue: id(1) });
    s.run(500);
    assert!(first_nonzero(&s.channel(0)).is_some(), "sounds on output 1");
    assert!(s.channel(2).iter().all(|&x| x == 0.0));

    assert_eq!(
        set(&mut s, patch_id(), Some(mapping("Offline", &[2, 3]))),
        Ok(())
    );
    assert_eq!(stored(&s, &ws), Some(mapping("Offline", &[2, 3])));
    // The slot follows in place (as for a device change): the cue still plays, on the new outputs.
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    s.run(500);
    assert!(
        last_nonzero(&s.channel(2)).is_some_and(|f| f > 400),
        "playing on output 3 after the change"
    );
    drop(s);
    let (loaded, _) = load_machine_settings_checked(&env.dirs);
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    assert_eq!(
        loaded.settings.patch_mapping(ws.id, patch_id()),
        Some(&mapping("Offline", &[2, 3]))
    );
}

/// Clearing the mapping under a playing cue makes the patch `Unmapped` and pauses the cue
/// (QLab 5.5.3: setting the device to none shows the cue paused). Mapping the patch again does
/// not resume it; a Resume plays it on, on the new outputs. A cue on another patch plays on.
#[test]
fn clearing_a_mapping_while_a_cue_plays_pauses_it_and_remapping_does_not_resume_it() {
    let env = env();
    let path = wav(&dc(40_000, 0.5));
    let other = PatchId(uuid::Uuid::from_u128(77));
    let mut ws = workspace(vec![audio(1, &path, 0), audio(2, &path, 0)]);
    let mut second = ws.patches[0].clone();
    second.id = other;
    second.name = "Other".into();
    ws.patches.push(second);
    if let CueKind::Audio { patch, .. } = &mut ws.cue_lists[0].cues[1].kind {
        *patch = other;
    }
    let mut s = start_show(ws.clone(), from_disk(&env, vec![device("Offline", 4)]));
    s.transport(TransportAction::Start { cue: id(1) });
    s.transport(TransportAction::Start { cue: id(2) });
    s.run(500);
    assert!(last_nonzero(&s.channel(0)).is_some_and(|f| f > 400));

    assert_eq!(set(&mut s, patch_id(), None), Ok(()));
    assert_eq!(stored(&s, &ws), None);
    assert_eq!(s.control().engine().state(id(1)), CueState::Paused);
    assert_eq!(s.control().engine().state(id(2)), CueState::Running);
    // Out of the way of the silence checks below.
    s.transport(TransportAction::HardStop { cue: id(2) });
    // Past the pause's declick: silent on every output, still paused.
    s.run(2000);
    let after_ramp = s.channel(0).len();
    s.run(500);
    for ch in 0..4 {
        let out = s.channel(ch);
        assert!(
            out[after_ramp..].iter().all(|&x| x == 0.0),
            "output {ch} still sounds after the pause"
        );
    }
    assert_eq!(s.control().engine().state(id(1)), CueState::Paused);
    let p = preshow(&mut s)
        .patches
        .into_iter()
        .find(|p| p.patch == patch_id())
        .unwrap();
    assert!(!p.mapped);
    assert_eq!(p.resolution, Some(WireResolution::Unmapped));

    // Mapping it again leaves the cue paused and silent.
    assert_eq!(
        set(&mut s, patch_id(), Some(mapping("Offline", &[2, 3]))),
        Ok(())
    );
    let before = s.channel(2).len();
    s.run(1000);
    assert!(s.channel(2)[before..].iter().all(|&x| x == 0.0));
    assert_eq!(s.control().engine().state(id(1)), CueState::Paused);

    // The operator's Resume plays it on, on the new outputs.
    s.transport(TransportAction::Resume { cue: id(1) });
    let before = s.channel(2).len();
    s.run(1000);
    assert!(
        s.channel(2)[before..].iter().any(|&x| x != 0.0),
        "sounds again on output 3"
    );
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
}

#[test]
fn a_cleared_mapping_stays_cleared_across_syncs_and_a_restart() {
    let env = env();
    let path = wav(&dc(400, 0.5));
    let ws = workspace(vec![audio(1, &path, 0)]);
    let mut s = start_show(ws.clone(), from_disk(&env, vec![device("Offline", 4)]));
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);

    assert_eq!(set(&mut s, patch_id(), None), Ok(()));
    assert_eq!(stored(&s, &ws), None);
    assert_eq!(s.control().engine().state(id(1)), CueState::Broken);
    // Another sync (a new workspace opening) does not map it again.
    let id2 = s.request_id();
    s.send(ClientMessage::Request {
        id: id2,
        request: Request::New {},
    });
    assert_eq!(stored(&s, &ws), None);
    assert!(
        s.control()
            .machine()
            .patch_entry(ws.id, patch_id())
            .is_some()
    );
    let report = preshow(&mut s);
    let p = report
        .patches
        .iter()
        .find(|p| p.patch == patch_id())
        .unwrap();
    assert!(!p.mapped);
    assert_eq!(p.resolution, Some(WireResolution::Unmapped));
    drop(s);

    // A restart reads the file: still cleared, and not mapped by default.
    let c = from_disk(&env, vec![device("Offline", 4)]);
    assert!(c.machine.patch_entry(ws.id, patch_id()).is_some());
    let s = start_show(ws.clone(), c);
    assert_eq!(stored(&s, &ws), None);
    assert_eq!(s.control().engine().state(id(1)), CueState::Broken);
}

#[test]
fn a_mapping_survives_an_engine_restart() {
    let env = env();
    let ws = workspace(vec![]);
    let mut s = start_show(ws.clone(), from_disk(&env, vec![device("Offline", 4)]));
    assert_eq!(
        set(&mut s, patch_id(), Some(mapping("Offline", &[3, 1]))),
        Ok(())
    );
    drop(s);
    let s = start_show(ws.clone(), from_disk(&env, vec![device("Offline", 4)]));
    assert_eq!(stored(&s, &ws), Some(mapping("Offline", &[3, 1])));
}

#[test]
fn invalid_mappings_are_refused_and_change_nothing() {
    let env = env();
    let ws = workspace(vec![]);
    // Already mapped, so the engine has nothing to write at start.
    let mut c = from_disk(&env, vec![device("Offline", 4)]);
    c.machine
        .set_patch_mapping(ws.id, patch_id(), mapping("Offline", &[0, 1]))
        .unwrap();
    let mut s = start_show(ws.clone(), c);
    let before = stored(&s, &ws);
    let other_patch = PatchId(uuid::Uuid::from_u128(0xdead));
    let cases = [
        (patch_id(), mapping("Nowhere", &[0, 1]), ErrorCode::NotFound),
        (patch_id(), mapping("", &[0, 1]), ErrorCode::NotFound),
        (
            patch_id(),
            mapping("Offline", &[0, 4]),
            ErrorCode::BadRequest,
        ),
        (
            patch_id(),
            mapping("Offline", &[0, u32::MAX]),
            ErrorCode::BadRequest,
        ),
        (
            patch_id(),
            mapping("Offline", &[0, 1, 2]),
            ErrorCode::BadRequest,
        ),
        (patch_id(), mapping("Offline", &[0]), ErrorCode::BadRequest),
        (patch_id(), mapping("Offline", &[]), ErrorCode::BadRequest),
        (
            other_patch,
            mapping("Offline", &[0, 1]),
            ErrorCode::NotFound,
        ),
    ];
    for (patch, m, code) in cases {
        assert_eq!(set(&mut s, patch, Some(m.clone())), Err(code), "{m:?}");
    }
    assert_eq!(set(&mut s, other_patch, None), Err(ErrorCode::NotFound));
    assert_eq!(stored(&s, &ws), before);
    assert!(
        s.control()
            .machine()
            .patch_entry(ws.id, other_patch)
            .is_none()
    );
    drop(s);
    assert!(
        !env.dirs.settings_path().exists(),
        "a refusal writes nothing"
    );
}

/// Sends a model command; the error code if it was refused.
fn edit(s: &mut OfflineShow, command: Command) -> Result<(), ErrorCode> {
    let id = s.request_id();
    let replies = s.send(ClientMessage::Command {
        id,
        command: Box::new(command),
        coalesce: None,
    });
    match replies.iter().find_map(|m| match m {
        ServerMessage::Result { outcome, .. } => Some(outcome.clone()),
        _ => None,
    }) {
        Some(Outcome::Ok(_)) => Ok(()),
        Some(Outcome::Error(e)) => Err(e.code),
        None => panic!("{replies:?}"),
    }
}

fn channel_count(value: usize) -> Command {
    Command::SetPatchProperty {
        id: patch_id(),
        property: PatchProperty::ChannelCount { value },
    }
}

/// Checklist: a patch's cue-output count is set from 1 to 128, and cues on it show that many
/// outputs; a count outside the range is refused on every path that sets it (QLab 5 manual
/// p. 189). The 128th cue output plays through its row of the patch to the device channel it
/// is mapped to, and the cue's meter has a peak per patch output; undo takes the patch back to
/// its two outputs and the meter follows. 0 and 129 are refused and change nothing, as is a
/// mapping of 129 channels.
#[test]
fn a_patchs_cue_output_count_is_set_from_1_to_128_and_cues_on_it_show_that_many_outputs() {
    let env = env();
    let path = wav(&dc(48_000, 0.5));
    // The cue sends its one file channel to cue output 128 (index 127): silent while the patch
    // has two outputs.
    let mut wide = audio(1, &path, 127);
    if let CueKind::Audio { levels, .. } = &mut wide.kind {
        levels.outputs = vec![
            openlab_core::CueOutput {
                db: 0.0,
                mute: false,
            };
            128
        ];
    }
    let ws = workspace(vec![wide, audio(2, &path, 0)]);
    let mut s = start_show(ws.clone(), from_disk(&env, vec![device("Offline", 4)]));
    let cue_meters = |msgs: &[ServerMessage], n: u128| -> Vec<Vec<f32>> {
        msgs.iter()
            .filter_map(|m| match m {
                ServerMessage::Meters { cues, .. } => cues
                    .iter()
                    .find(|c| c.cue == id(n))
                    .map(|c| c.peaks.clone()),
                _ => None,
            })
            .collect()
    };

    for value in [0, 129, usize::MAX] {
        assert_eq!(
            edit(&mut s, channel_count(value)),
            Err(ErrorCode::CommandRefused),
            "{value}"
        );
    }
    assert_eq!(s.control().workspace().patches[0].outputs.len(), 2);
    assert_eq!(edit(&mut s, channel_count(1)), Ok(()));
    assert_eq!(s.control().workspace().patches[0].outputs.len(), 1);
    assert_eq!(edit(&mut s, channel_count(128)), Ok(()));
    assert_eq!(s.control().workspace().patches[0].outputs.len(), 128);

    // One device channel per cue output: the 128th goes to device output 4.
    let mut channels = vec![0u32; 128];
    channels[127] = 3;
    assert_eq!(
        set(&mut s, patch_id(), Some(mapping("Offline", &[0; 129]))),
        Err(ErrorCode::BadRequest)
    );
    assert_eq!(
        set(&mut s, patch_id(), Some(mapping("Offline", &channels))),
        Ok(())
    );
    s.take_outbound();
    s.transport(TransportAction::Start { cue: id(1) });
    s.transport(TransportAction::Start { cue: id(2) });
    s.run(4000);
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    let out4 = s.channel(3);
    assert!(
        out4[out4.len() - 100..].iter().all(|&x| x == 0.5),
        "cue output 128 heard on device output 4"
    );
    let msgs = s.take_outbound();
    let wide = cue_meters(&msgs, 1);
    let last = wide.last().expect("cue 1 metered");
    assert_eq!(last.len(), 128, "a peak per patch output");
    assert!(last[127] > 0.4 && last[..127].iter().all(|&p| p == 0.0));
    // Cue 2 is on the same patch: 128 peaks too, its own on output 1.
    let narrow = cue_meters(&msgs, 2);
    assert_eq!(narrow.last().map(Vec::len), Some(128));

    // Back to one output (undo), mapped again to one channel: the next start meters one
    // output.
    s.transport(TransportAction::HardStop { cue: id(1) });
    s.transport(TransportAction::HardStop { cue: id(2) });
    s.run(2000);
    let id_undo = s.request_id();
    s.send(ClientMessage::Undo { id: id_undo });
    assert_eq!(s.control().workspace().patches[0].outputs.len(), 1);
    assert_eq!(
        set(&mut s, patch_id(), Some(mapping("Offline", &[0]))),
        Ok(())
    );
    s.take_outbound();
    s.transport(TransportAction::Start { cue: id(2) });
    s.run(2000);
    let msgs = s.take_outbound();
    assert_eq!(cue_meters(&msgs, 2).last().map(Vec::len), Some(1));
}

/// A show whose patch was resized since this machine's mapping was stored opens with the
/// mapping fitted to it (the kept output where it was, the new one on its own number): the
/// patch plays, nothing is reported.
#[test]
fn a_stored_mapping_of_another_length_is_fitted_when_the_show_opens() {
    let env = env();
    let path = wav(&dc(400, 0.5));
    let ws = workspace(vec![audio(1, &path, 0)]);
    let mut machine = MachineSettings::default();
    machine
        .set_patch_mapping(ws.id, patch_id(), mapping("Offline", &[2]))
        .unwrap();
    let mut c = from_disk(&env, vec![device("Offline", 4)]);
    c.machine = machine;
    let mut s = start_show(ws.clone(), c);
    assert_eq!(stored(&s, &ws), Some(mapping("Offline", &[2, 1])));
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    assert!(preshow(&mut s).patches.is_empty());
}

fn ok_reply(replies: &[ServerMessage]) {
    assert!(
        replies.iter().any(|m| matches!(
            m,
            ServerMessage::Result {
                outcome: Outcome::Ok(_),
                ..
            }
        )),
        "{replies:?}"
    );
}

fn undo(s: &mut OfflineShow) {
    let id = s.request_id();
    ok_reply(&s.send(ClientMessage::Undo { id }));
}

fn redo(s: &mut OfflineShow) {
    let id = s.request_id();
    ok_reply(&s.send(ClientMessage::Redo { id }));
}

/// One cue-output count edit keeps the patch mapped on this machine, and so do its undo and
/// redo: outputs kept stay on their device channels, new ones go to the channel of their own
/// number (round the device's outputs), removed ones drop. A cue playing meanwhile plays on,
/// and the next GO of another cue on the patch plays (the patch is never broken).
#[test]
fn a_count_change_its_undo_and_redo_keep_the_patch_mapped_and_cues_playing() {
    let env = env();
    let path = wav(&dc(96_000, 0.5));
    let ws = workspace(vec![audio(1, &path, 0), audio(2, &path, 0)]);
    let mut s = start_show(ws.clone(), from_disk(&env, vec![device("Offline", 4)]));
    assert_eq!(
        set(&mut s, patch_id(), Some(mapping("Offline", &[1, 0]))),
        Ok(())
    );
    s.transport(TransportAction::Start { cue: id(1) });
    s.run(500);
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);

    assert_eq!(edit(&mut s, channel_count(6)), Ok(()));
    assert_eq!(
        stored(&s, &ws),
        Some(mapping("Offline", &[1, 0, 2, 3, 0, 1]))
    );
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    undo(&mut s);
    assert_eq!(stored(&s, &ws), Some(mapping("Offline", &[1, 0])));
    s.run(500);
    assert_eq!(
        s.control().engine().state(id(1)),
        CueState::Running,
        "it plays on"
    );
    s.transport(TransportAction::Start { cue: id(2) });
    s.run(500);
    assert_eq!(
        s.control().engine().state(id(2)),
        CueState::Running,
        "the next GO plays after an undo"
    );
    redo(&mut s);
    assert_eq!(
        stored(&s, &ws),
        Some(mapping("Offline", &[1, 0, 2, 3, 0, 1]))
    );
    assert_eq!(edit(&mut s, channel_count(1)), Ok(()));
    assert_eq!(stored(&s, &ws), Some(mapping("Offline", &[1])));
    s.transport(TransportAction::HardStop { cue: id(2) });
    s.run(500);
    s.transport(TransportAction::Start { cue: id(2) });
    s.run(500);
    assert_eq!(s.control().engine().state(id(2)), CueState::Running);
    assert!(
        preshow(&mut s).patches.is_empty(),
        "no patch problem after count, undo, redo"
    );
}

/// The patch's device is not connected when the count changes: the mapping still follows the
/// count (counted round the outputs it already reaches), so the patch plays as soon as the
/// device is back. An explicitly cleared entry stays cleared.
#[test]
fn a_count_change_while_the_device_is_missing_or_cleared_keeps_the_entry_consistent() {
    let env = env();
    let ws = workspace(vec![]);
    let mut machine = MachineSettings::default();
    machine
        .set_patch_mapping(ws.id, patch_id(), mapping("Gone", &[0, 1]))
        .unwrap();
    let mut c = from_disk(&env, vec![device("Offline", 4)]);
    c.machine = machine;
    let mut s = start_show(ws.clone(), c);
    assert_eq!(edit(&mut s, channel_count(3)), Ok(()));
    assert_eq!(stored(&s, &ws), Some(mapping("Gone", &[0, 1, 0])));

    assert_eq!(set(&mut s, patch_id(), None), Ok(()));
    assert_eq!(edit(&mut s, channel_count(5)), Ok(()));
    assert_eq!(stored(&s, &ws), None, "cleared stays cleared");
    assert_eq!(
        s.control()
            .machine()
            .patch_entry(ws.id, patch_id())
            .map(|m| m.device.is_empty()),
        Some(true)
    );
}

#[test]
fn a_patch_a_too_small_device_left_unmapped_can_be_mapped_by_hand() {
    let env = env();
    let path = wav(&dc(400, 0.5));
    let ws = workspace(vec![audio(1, &path, 0)]);
    let mut s = start_show(ws.clone(), from_disk(&env, vec![device("Mono", 1)]));
    assert_eq!(stored(&s, &ws), None);
    let report = preshow(&mut s);
    assert_eq!(report.patches[0].resolution, Some(WireResolution::Unmapped));
    // Two outputs on a one-channel device cannot be mapped, whatever the channels.
    assert_eq!(
        set(&mut s, patch_id(), Some(mapping("Mono", &[0, 1]))),
        Err(ErrorCode::BadRequest)
    );
    assert_eq!(stored(&s, &ws), None);
}

/// Sends `set_patch_mapping` to a show on the stand-in device; the error if it was refused.
fn set_on_device(
    s: &mut DeviceShow,
    mapping: PatchMapping,
) -> Result<(), openlab_core::protocol::ErrorBody> {
    let id = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id,
        request: Request::SetPatchMapping {
            patch: patch_id(),
            mapping: Some(mapping),
        },
    });
    match replies.iter().find_map(|m| match m {
        ServerMessage::Result { outcome, .. } => Some(outcome.clone()),
        _ => None,
    }) {
        Some(Outcome::Ok(_)) => Ok(()),
        Some(Outcome::Error(e)) => Err(e),
        None => panic!("{replies:?}"),
    }
}

/// AUD-02: a patch can be mapped to any listed device; one the engine does not play on yet
/// opens for it (on its own thread), and the patch plays there once it does.
#[test]
fn a_mapping_to_another_listed_device_opens_that_device() {
    let ws = workspace(vec![]);
    let mut c = ControlConfig::new(RATE, BLOCK);
    c.seed = 7;
    c.mixer_patches = 2;
    c.machine
        .set_patch_mapping(ws.id, patch_id(), mapping("Fake 1", &[0, 1]))
        .unwrap();
    let mut s = DeviceShow::new(ws.clone(), vec![listed("fake:1", "Fake 1", 2)], c);
    s.fake.plug(listed("fake:2", "Fake 2", 8));
    s.wait_wall(std::time::Duration::from_secs(1), 1);
    assert_eq!(set_on_device(&mut s, mapping("Fake 2", &[2, 3])), Ok(()));
    assert_eq!(
        s.control().machine().patch_mapping(ws.id, patch_id()),
        Some(&mapping("Fake 2", &[2, 3]))
    );
    s.wait_output("Fake 2");
    assert!(s.control().output_up("Fake 1") && s.control().output_up("Fake 2"));
    // A device that is not listed is still refused.
    let refusal = set_on_device(&mut s, mapping("Gone", &[0, 1])).unwrap_err();
    assert_eq!(refusal.code, ErrorCode::NotFound);
}

/// With settings that cannot be kept (the file was damaged at start, so the engine leaves it
/// alone) the change would be gone at the next launch: the operator is told, nothing changes.
#[test]
fn a_mapping_that_cannot_be_saved_is_refused_not_kept_in_memory() {
    let env = env();
    let ws = workspace(vec![]);
    let mut c = from_disk(&env, vec![device("Offline", 4)]);
    c.machine
        .set_patch_mapping(ws.id, patch_id(), mapping("Offline", &[0, 1]))
        .unwrap();
    c.machine_writable = false;
    let mut s = start_show(ws.clone(), c);
    assert_eq!(
        set(&mut s, patch_id(), Some(mapping("Offline", &[2, 3]))),
        Err(ErrorCode::Io)
    );
    assert_eq!(set(&mut s, patch_id(), None), Err(ErrorCode::Io));
    assert_eq!(stored(&s, &ws), Some(mapping("Offline", &[0, 1])));
}

/// Sends `set_output_device`; the error code if it was refused.
fn choose(s: &mut OfflineShow, device: Option<&str>) -> Result<(), ErrorCode> {
    let id = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id,
        request: Request::SetOutputDevice {
            device: device.map(str::to_owned),
        },
    });
    match replies.iter().find_map(|m| match m {
        ServerMessage::Result { outcome, .. } => Some(outcome.clone()),
        _ => None,
    }) {
        Some(Outcome::Ok(_)) => Ok(()),
        Some(Outcome::Error(e)) => Err(e.code),
        None => panic!("no result"),
    }
}

/// The operator picks the output device for the next start: a listed name is stored in the
/// machine settings (and reported back by `list_devices`), an unknown one is refused with
/// nothing changed, and null goes back to the system default.
#[test]
fn the_output_device_choice_is_stored_validated_and_cleared() {
    let env = env();
    let ws = workspace(vec![]);
    let mut s = start_show(ws, from_disk(&env, vec![device("Offline", 4)]));
    assert_eq!(choose(&mut s, Some("Nowhere")), Err(ErrorCode::NotFound));
    assert_eq!(s.control().machine().output_device, None);
    assert_eq!(choose(&mut s, Some("Offline")), Ok(()));
    assert_eq!(
        s.control().machine().output_device.as_deref(),
        Some("Offline")
    );
    let id = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id,
        request: Request::ListDevices,
    });
    assert!(replies.iter().any(|m| matches!(
        m,
        ServerMessage::Result { outcome: Outcome::Ok(ok), .. }
            if matches!(&ok.data, Some(ResultData::Devices { selected: Some(n), .. }) if n == "Offline")
    )));
    drop(s);
    let (loaded, _) = load_machine_settings_checked(&env.dirs);
    assert_eq!(loaded.settings.output_device.as_deref(), Some("Offline"));

    let mut s = start_show(
        workspace(vec![]),
        from_disk(&env, vec![device("Offline", 4)]),
    );
    assert_eq!(choose(&mut s, None), Ok(()));
    assert_eq!(s.control().machine().output_device, None);
}

/// Settings that cannot be kept refuse the choice rather than losing it at the next launch.
#[test]
fn an_output_device_choice_that_cannot_be_saved_is_refused() {
    let env = env();
    let mut c = from_disk(&env, vec![device("Offline", 4)]);
    c.machine_writable = false;
    let mut s = start_show(workspace(vec![]), c);
    assert_eq!(choose(&mut s, Some("Offline")), Err(ErrorCode::Io));
    assert_eq!(s.control().machine().output_device, None);
}

/// A patch editor reads where each patch plays: the default mapping, an edit, and a clear.
#[test]
fn list_patch_mappings_reports_the_stored_mapping_of_each_patch() {
    let env = env();
    let ws = workspace(vec![]);
    let mut s = start_show(ws, from_disk(&env, vec![device("Offline", 4)]));
    let listed = |s: &mut OfflineShow| {
        let id = s.request_id();
        let replies = s.send(ClientMessage::Request {
            id,
            request: Request::ListPatchMappings,
        });
        replies
            .iter()
            .find_map(|m| match m {
                ServerMessage::Result {
                    outcome:
                        Outcome::Ok(openlab_core::protocol::OkBody {
                            data: Some(ResultData::PatchMappings { patches }),
                            ..
                        }),
                    ..
                } => Some(patches.clone()),
                _ => None,
            })
            .expect("patch_mappings")
    };
    // Each answer also says where the patch resolves now.
    let entry = |m: Option<PatchMapping>| {
        let resolution = Some(match &m {
            Some(m) => openlab_core::protocol::WireResolution::Mapped {
                device: m.device.clone(),
                channels: m.channels.clone(),
            },
            None => openlab_core::protocol::WireResolution::Unmapped,
        });
        vec![openlab_core::protocol::PatchMappingEntry {
            patch: patch_id(),
            mapping: m,
            resolution,
        }]
    };
    assert_eq!(listed(&mut s), entry(Some(mapping("Offline", &[0, 1]))));
    assert_eq!(
        set(&mut s, patch_id(), Some(mapping("Offline", &[2, 3]))),
        Ok(())
    );
    assert_eq!(listed(&mut s), entry(Some(mapping("Offline", &[2, 3]))));
    assert_eq!(set(&mut s, patch_id(), None), Ok(()));
    assert_eq!(listed(&mut s), entry(None));
}
