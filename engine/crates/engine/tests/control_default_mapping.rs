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
use openlab_engine::persist::load_machine_settings;
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
fn a_device_with_one_output_gets_a_partial_mapping_and_a_silent_patch() {
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
        w[0].contains("needs 2 outputs") && w[0].contains("Mono has 1"),
        "{w:?}"
    );
    let m = mapping(&ws, s.control().machine()).unwrap();
    assert_eq!((m.device.as_str(), m.channels), ("Mono", vec![0]));
    // Resolution is OutputCountMismatch: the cue is broken and nothing plays.
    assert_eq!(s.control().engine().state(id(1)), CueState::Broken);
    s.transport(TransportAction::Start { cue: id(1) });
    s.run(500);
    assert!(s.audio().iter().all(|&x| x == 0.0));
    // Not retried (and not logged again) on later syncs.
    s.take_outbound();
    let snap = new_workspace(&mut s);
    assert_eq!(snap.workspace.patches.len(), 1);
    assert_eq!(
        warns(&s.take_outbound()).len(),
        1,
        "one warning, for the new workspace"
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
