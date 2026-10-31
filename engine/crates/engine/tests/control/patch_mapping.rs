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
    CueKind, CueState, DeviceInfo, MachineSettings, PatchId, PatchMapping, Workspace,
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

#[test]
fn a_mapping_with_the_wrong_count_is_reported_and_setting_it_right_fixes_it() {
    let env = env();
    let path = wav(&dc(400, 0.5));
    let ws = workspace(vec![audio(1, &path, 0)]);
    // This machine's file maps the two-output patch to one channel (a patch resized since).
    let mut machine = MachineSettings::default();
    machine
        .set_patch_mapping(ws.id, patch_id(), mapping("Offline", &[0]))
        .unwrap();
    let mut c = from_disk(&env, vec![device("Offline", 4)]);
    c.machine = machine;
    let mut s = start_show(ws.clone(), c);
    assert_eq!(s.control().engine().state(id(1)), CueState::Broken);
    let report = preshow(&mut s);
    let p = report
        .patches
        .iter()
        .find(|p| p.patch == patch_id())
        .unwrap();
    assert!(
        matches!(
            p.resolution,
            Some(WireResolution::OutputCountMismatch {
                mapped: 1,
                outputs: 2
            })
        ),
        "{:?}",
        p.resolution
    );

    assert_eq!(
        set(&mut s, patch_id(), Some(mapping("Offline", &[0, 1]))),
        Ok(())
    );
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    assert!(preshow(&mut s).patches.is_empty());
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

/// A device the host lists but the mixer does not play on would leave the patch silent
/// (`device_missing`) with no word why: the request is refused and the mapping stays.
#[test]
fn a_mapping_to_a_listed_device_the_mixer_is_not_playing_on_is_refused() {
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
    let refusal = set_on_device(&mut s, mapping("Fake 2", &[0, 1])).unwrap_err();
    assert_eq!(refusal.code, ErrorCode::BadRequest);
    assert!(refusal.message.contains("Fake 1"), "{}", refusal.message);
    assert_eq!(
        s.control().machine().patch_mapping(ws.id, patch_id()),
        Some(&mapping("Fake 1", &[0, 1]))
    );
    // The device in use is accepted.
    assert_eq!(set_on_device(&mut s, mapping("Fake 1", &[1, 0])), Ok(()));
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
