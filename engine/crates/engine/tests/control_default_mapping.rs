// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Default patch mapping (M4.4c, WS-07): a workspace with no mapping on this machine plays on
//! the device's first outputs without a mapping step, as in QLab; an existing mapping is never
//! touched. Offline driver, virtual wall clock, settings in a temp folder.

mod common;
mod show;

use common::{Env, env};
use openlab_core::protocol::{
    ClientMessage, LogLevel, Outcome, Request, ServerMessage, Snapshot, TransportAction,
};
use openlab_core::{
    Command, CueKind, CueState, DeviceInfo, MachineSettings, PatchMapping, Position, Workspace,
    WorkspaceId,
};
use openlab_engine::control::ControlConfig;
use openlab_engine::control::offline::OfflineShow;
use openlab_engine::persist::{load_machine_settings, load_machine_settings_checked};
use show::*;

fn device(name: &str, outputs: u32) -> DeviceInfo {
    DeviceInfo {
        name: name.into(),
        output_channels: outputs,
    }
}

/// A config with no mapping for anybody, the given devices and settings stored in `env`.
fn bare(env: &Env, devices: Vec<DeviceInfo>) -> ControlConfig {
    let mut c = ControlConfig::new(RATE, BLOCK);
    c.seed = 7;
    c.devices = devices;
    c.mixer_patches = 2;
    c.app_dirs = Some(env.dirs.clone());
    c
}

fn new_workspace(s: &mut OfflineShow) -> Snapshot {
    let id = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id,
        request: Request::New {
            discard_changes: false,
        },
    });
    replies
        .iter()
        .find_map(|m| match m {
            ServerMessage::Snapshot { snapshot } => Some((**snapshot).clone()),
            _ => None,
        })
        .expect("no snapshot")
}

fn infos(msgs: &[ServerMessage]) -> Vec<String> {
    msgs.iter()
        .filter_map(|m| match m {
            ServerMessage::Log {
                level: LogLevel::Info,
                message,
            } if message.starts_with("mapped patch") => Some(message.clone()),
            _ => None,
        })
        .collect()
}

fn warns(msgs: &[ServerMessage]) -> Vec<String> {
    msgs.iter()
        .filter_map(|m| match m {
            ServerMessage::Log {
                level: LogLevel::Warn,
                message,
            } => Some(message.clone()),
            _ => None,
        })
        .collect()
}

fn mapping(ws: &Workspace, machine: &MachineSettings) -> Option<PatchMapping> {
    machine.patch_mapping(ws.id, ws.patches[0].id).cloned()
}

/// Adds an audio cue (output 0 only) on the workspace's default patch and list.
fn add_audio(s: &mut OfflineShow, snap: &Snapshot, path: &std::path::Path) {
    let mut cue = audio(50, path, 0);
    if let CueKind::Audio { patch, .. } = &mut cue.kind {
        *patch = snap.workspace.patches[0].id;
    }
    let id = s.request_id();
    let replies = s.send(ClientMessage::Command {
        id,
        coalesce: None,
        command: Box::new(Command::AddCue {
            at: Position {
                list: snap.workspace.cue_lists[0].id,
                parent: None,
                index: 0,
            },
            cue,
            auto_number: false,
        }),
    });
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
    s.settle_media();
}

#[test]
fn a_new_workspace_plays_without_a_mapping_step() {
    let env = env();
    let c = bare(&env, vec![device("Offline", 2)]);
    let mut s = show_with(workspace(vec![]), c);
    s.take_outbound();
    let snap = new_workspace(&mut s);
    let msgs = s.take_outbound();
    assert_eq!(
        infos(&msgs),
        vec!["mapped patch 'Main Outputs' to Offline outputs 1\u{2013}2".to_owned()]
    );
    let m = mapping(&snap.workspace, s.control().machine()).unwrap();
    assert_eq!(
        (m.device.as_str(), m.channels.clone()),
        ("Offline", vec![0, 1])
    );

    let path = wav(&dc(400, 0.5));
    add_audio(&mut s, &snap, &path);
    assert_eq!(s.control().engine().state(id(50)), CueState::Idle);
    s.transport(TransportAction::Start { cue: id(50) });
    s.run(1000);
    assert!(
        first_nonzero(&s.channel(0)).is_some(),
        "cue 50 made no sound"
    );
    assert!(s.channel(1).iter().all(|&x| x == 0.0), "routed to output 0");
    drop(s);
    // Persisted for the next launch.
    let loaded = load_machine_settings(&env.dirs);
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    assert_eq!(mapping(&snap.workspace, &loaded.settings), Some(m));
}

#[test]
fn an_existing_mapping_is_kept() {
    let env = env();
    let ws = workspace(vec![]);
    let mut c = bare(&env, vec![device("Offline", 4), device("Other", 2)]);
    let mut machine = MachineSettings::default();
    let kept = PatchMapping {
        device: "Other".into(),
        channels: vec![1, 0],
    };
    machine
        .set_patch_mapping(ws.id, patch_id(), kept.clone())
        .unwrap();
    c.machine = machine;
    let mut s = show_with(ws.clone(), c);
    let msgs = s.take_outbound();
    assert!(infos(&msgs).is_empty(), "{msgs:?}");
    assert_eq!(mapping(&ws, s.control().machine()), Some(kept));
    drop(s);
    // Nothing was written: the engine had nothing to change.
    assert!(!env.dirs.settings_path().exists());
}

#[test]
fn a_show_with_no_mapping_on_this_machine_is_mapped() {
    let env = env();
    let ws = workspace(vec![]);
    let mut c = bare(&env, vec![device("Offline", 8)]);
    // Another workspace has a mapping; this one has none.
    let mut other = workspace(vec![]);
    other.id = WorkspaceId(uuid::Uuid::from_u128(99));
    let mut machine = MachineSettings::default();
    machine
        .set_patch_mapping(
            other.id,
            patch_id(),
            PatchMapping {
                device: "Elsewhere".into(),
                channels: vec![6, 7],
            },
        )
        .unwrap();
    c.machine = machine;
    let mut s = show_with(ws.clone(), c);
    let msgs = s.take_outbound();
    assert_eq!(
        infos(&msgs),
        vec!["mapped patch 'Main' to Offline outputs 1\u{2013}2".to_owned()]
    );
    let m = mapping(&ws, s.control().machine()).unwrap();
    assert_eq!(m.channels, vec![0, 1]);
    // The other workspace's mapping is untouched, in memory and on disk.
    assert_eq!(
        mapping(&other, s.control().machine()).unwrap().device,
        "Elsewhere"
    );
    drop(s);
    let loaded = load_machine_settings(&env.dirs).settings;
    assert_eq!(mapping(&ws, &loaded), Some(m));
    assert_eq!(mapping(&other, &loaded).unwrap().channels, vec![6, 7]);
}

#[test]
fn a_device_with_too_few_outputs_leaves_the_patch_unmapped_with_one_warning() {
    let env = env();
    let path = wav(&dc(400, 0.5));
    let ws = workspace(vec![audio(1, &path, 0)]);
    let c = bare(&env, vec![device("Mono", 1)]);
    let mut s = show_with(ws.clone(), c);
    let msgs = s.take_outbound();
    assert!(infos(&msgs).is_empty());
    let w = warns(&msgs);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(
        w[0].contains("device Mono has 1 outputs") && w[0].contains("needs 2"),
        "{w:?}"
    );
    assert_eq!(mapping(&ws, s.control().machine()), None);
    assert_eq!(s.control().engine().state(id(1)), CueState::Broken);
    // Not logged again on later syncs of the same workspace.
    s.take_outbound();
    let _ = new_workspace(&mut s);
    let again = warns(&s.take_outbound());
    assert_eq!(again.len(), 1, "one warning, for the new workspace's patch");
    drop(s);
    assert!(!env.dirs.settings_path().exists());
}

#[test]
fn only_the_unmapped_patch_of_a_partly_mapped_workspace_is_mapped() {
    let env = env();
    let mut ws = workspace(vec![]);
    let mut second = ws.patches[0].clone();
    second.id = openlab_core::PatchId(uuid::Uuid::from_u128(0x77));
    second.name = "Foldback".into();
    ws.patches.push(second.clone());
    let mut c = bare(&env, vec![device("Offline", 4)]);
    let mut machine = MachineSettings::default();
    let kept = PatchMapping {
        device: "Offline".into(),
        channels: vec![3, 2],
    };
    machine
        .set_patch_mapping(ws.id, patch_id(), kept.clone())
        .unwrap();
    c.machine = machine;
    let mut s = show_with(ws.clone(), c);
    assert_eq!(
        infos(&s.take_outbound()),
        vec!["mapped patch 'Foldback' to Offline outputs 1\u{2013}2".to_owned()]
    );
    let machine = s.control().machine();
    assert_eq!(machine.patch_mapping(ws.id, patch_id()), Some(&kept));
    assert_eq!(
        machine.patch_mapping(ws.id, second.id).unwrap().channels,
        vec![0, 1]
    );
}

#[test]
fn no_device_leaves_the_patch_unmapped_and_writes_nothing() {
    let env = env();
    let path = wav(&dc(400, 0.5));
    let ws = workspace(vec![audio(1, &path, 0)]);
    let c = bare(&env, vec![]);
    let s = show_with(ws.clone(), c);
    assert_eq!(mapping(&ws, s.control().machine()), None);
    assert_eq!(s.control().engine().state(id(1)), CueState::Broken);
    drop(s);
    assert!(!env.dirs.settings_path().exists());
}

/// A config whose machine settings come from the file in `env`, as the host loads them.
fn from_disk(env: &Env, devices: Vec<DeviceInfo>) -> ControlConfig {
    let mut c = bare(env, devices);
    let (loaded, writable) = load_machine_settings_checked(&env.dirs);
    c.machine = loaded.settings;
    c.machine_writable = writable;
    c
}

#[test]
fn unreadable_settings_are_never_overwritten_by_a_default_mapping() {
    let env = env();
    // Not a regular file: reading fails with something other than "not found".
    let path = env.dirs.settings_path();
    std::fs::create_dir_all(&path).unwrap();
    let c = from_disk(&env, vec![device("Offline", 2)]);
    assert!(!c.machine_writable);
    let ws = workspace(vec![]);
    let mut s = show_with(ws.clone(), c);
    // Mapped in memory, so the show plays...
    let msgs = s.take_outbound();
    assert_eq!(infos(&msgs).len(), 1);
    assert!(mapping(&ws, s.control().machine()).is_some());
    // ...with one warning that the file is left alone, however many patches map later.
    let w = warns(&msgs);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("left as it is"), "{w:?}");
    let _ = new_workspace(&mut s);
    assert!(warns(&s.take_outbound()).is_empty());
    drop(s);
    assert!(path.is_dir(), "the settings path was replaced");
}

#[test]
fn keys_this_version_does_not_know_survive_a_default_mapping() {
    let env = env();
    let path = env.dirs.settings_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        r#"{"settings_version":1,"future_thing":{"a":[1,2]},"workspaces":{}}"#,
    )
    .unwrap();
    let c = from_disk(&env, vec![device("Offline", 2)]);
    assert!(c.machine_writable);
    let ws = workspace(vec![]);
    let s = show_with(ws.clone(), c);
    drop(s);
    let text = std::fs::read_to_string(&path).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["future_thing"]["a"][1], 2, "{text}");
    assert!(mapping(&ws, &load_machine_settings(&env.dirs).settings).is_some());
}
