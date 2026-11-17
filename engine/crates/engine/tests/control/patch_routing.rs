// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! A patch's routing matrix (AUD-07; QLab 5 manual p. 190): any cue output to any device
//! outputs, each crosspoint at its own level, and a level and mute per device output, through the
//! control loop and rendered offline on a four-output device. Edits on a playing cue move over the
//! mixer's de-zipper, never in one step.

use crate::show::*;
use openlab_core::protocol::{ClientMessage, Outcome, ServerMessage, WireWarning};
use openlab_core::{
    Command, DeviceInfo, DeviceOutputLevel, OutputPatch, PatchCrosspoint, PatchProperty,
    SILENCE_DB, Workspace, db_to_gain, load_show, save_show,
};
use openlab_engine::control::offline::OfflineShow;

const LEVEL: f32 = 0.25;
/// The mixer's de-zipper at [`RATE`] (5 ms).
const DEZIP: usize = 40;

/// A show of `cues` (each 10 s of a steady `LEVEL`) on a device with `outputs` outputs, the
/// patch mapped to its first two as a new workspace's is.
fn show_on(ws: Workspace, outputs: u32) -> OfflineShow {
    let mut c = config(&ws);
    c.devices = vec![DeviceInfo {
        name: "Offline".into(),
        output_channels: outputs,
    }];
    let mut m = mixer_config();
    m.output_channels = outputs as usize;
    let mut s = OfflineShow::new(ws, m, c).unwrap();
    s.settle_media();
    s
}

/// Cue 1 on cue output 0, and with `two` cue 2 on cue output 1.
fn steady(two: bool) -> Workspace {
    let a = wav(&dc(RATE as usize * 10, LEVEL));
    let mut cues = vec![audio(1, &a, 0)];
    if two {
        cues.push(audio(2, &a, 1));
    }
    workspace(cues)
}

fn edit(s: &mut OfflineShow, command: Command) -> usize {
    let at = s.now() as usize;
    let id = s.request_id();
    let replies = s.send(ClientMessage::Command {
        id,
        command: Box::new(command),
        coalesce: None,
    });
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
    at
}

fn set(s: &mut OfflineShow, property: PatchProperty) -> usize {
    edit(
        s,
        Command::SetPatchProperty {
            id: patch_id(),
            property,
        },
    )
}

fn crosspoint(output: u32, device_output: u32, db: Option<f64>) -> PatchProperty {
    PatchProperty::Crosspoint {
        output,
        device_output,
        value: db,
    }
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-5
}

fn held(x: &[f32], want: f32, what: &str) {
    assert!(
        x.iter().all(|&v| near(v, want)),
        "{what}: want {want}, got {:?}",
        &x[..x.len().min(8)]
    );
}

/// The largest step between neighbouring samples.
fn max_jump(x: &[f32]) -> f32 {
    x.windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max)
}

/// Channel `ch` holds `from` up to the edit at `at`, moves to `to` over the de-zipper with no
/// sample stepping more than a linear ramp does, then holds `to` up to `until`.
fn heard_edit(s: &OfflineShow, ch: usize, at: usize, from: f32, to: f32, until: usize) {
    let x = s.channel(ch);
    held(&x[at - 100..at], from, "before");
    held(&x[at + DEZIP..until], to, "after");
    let jump = max_jump(&x[at - 100..until]);
    assert!(
        jump <= (to - from).abs() / DEZIP as f32 * 1.5 + 1e-6,
        "jump {jump} from {from} to {to}"
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

fn patch(s: &OfflineShow) -> OutputPatch {
    s.control().workspace().patches[0].clone()
}

/// Checklist: in the patch editor's routing, a cue output is sent to two device outputs at
/// different levels; a cue on that patch is heard on both at those levels.
#[test]
fn a_cue_output_sent_to_two_device_outputs_is_heard_on_both_at_their_levels() {
    let mut s = show_on(steady(false), 4);
    set(&mut s, crosspoint(0, 0, Some(-6.0)));
    set(&mut s, crosspoint(0, 3, Some(-12.0)));
    s.run(200);
    let start = go(&mut s) as usize;
    s.run(800);
    let end = s.channel(0).len();
    held(
        &s.channel(0)[start + 10..end],
        LEVEL * db_to_gain(-6.0),
        "device output 1",
    );
    held(
        &s.channel(3)[start + 10..end],
        LEVEL * db_to_gain(-12.0),
        "device output 4",
    );
    // Neither of the others: cue output 1 is not fed and cue output 0 goes nowhere else.
    for ch in [1, 2] {
        held(&s.channel(ch)[start..end], 0.0, "unrouted");
    }
    // A patch with none plays as its mapping says: removing both brings back output 1 only.
    set(&mut s, crosspoint(0, 0, None));
    set(&mut s, crosspoint(0, 3, None));
    let at = s.now() as usize;
    s.run(400);
    let end = s.channel(0).len();
    held(&s.channel(0)[at + DEZIP..end], LEVEL, "mapped output");
    held(&s.channel(3)[at + DEZIP..end], 0.0, "crosspoint gone");
    assert!(patch(&s).crosspoints.is_empty());
}

/// Checklist: a crosspoint's level is typed while a cue plays; the change is heard at once,
/// with no click: up, down, to silence and from it, the voice playing on throughout.
#[test]
fn a_crosspoint_level_typed_while_a_cue_plays_is_heard_at_once_with_no_click() {
    let mut s = show_on(steady(false), 4);
    go(&mut s);
    s.run(800);
    s.take_outbound();
    let g6 = LEVEL * db_to_gain(-6.0);
    let at = set(&mut s, crosspoint(0, 0, Some(-6.0)));
    s.run(400);
    heard_edit(&s, 0, at, LEVEL, g6, at + 300);
    // A new route from silence.
    let at = set(&mut s, crosspoint(0, 2, Some(0.0)));
    s.run(400);
    heard_edit(&s, 2, at, 0.0, LEVEL, at + 300);
    // The mapped route taken away (typed empty: -inf).
    let at = set(&mut s, crosspoint(0, 0, Some(SILENCE_DB)));
    s.run(400);
    heard_edit(&s, 0, at, g6, 0.0, at + 300);
    // Undone: back at -6 dB, as smoothly.
    let rid = s.request_id();
    s.send(ClientMessage::Undo { id: rid });
    let at = s.now() as usize;
    s.run(400);
    heard_edit(&s, 0, at, 0.0, g6, at + 300);
    assert_eq!(s.control().live_voices(), 1);
    assert!(states(&s.take_outbound(), id(1)).is_empty());
}

/// Checklist: each device output has its own level and mute; lowering one lowers everything
/// sent to it, and nothing sent elsewhere.
#[test]
fn each_device_output_has_its_own_level_and_mute_and_lowering_one_lowers_all_it_hears() {
    let mut s = show_on(steady(true), 4);
    // Cue output 1 goes to device output 1 as well as its own (2).
    set(&mut s, crosspoint(1, 0, Some(0.0)));
    go(&mut s);
    go(&mut s);
    s.run(800);
    let both = 2.0 * LEVEL;
    let at = set(
        &mut s,
        PatchProperty::DeviceOutputLevel {
            device_output: 0,
            value: -6.0,
        },
    );
    s.run(400);
    let lowered = both * db_to_gain(-6.0);
    heard_edit(&s, 0, at, both, lowered, at + 300);
    held(&s.channel(1)[at - 100..at + 300], LEVEL, "device output 2 untouched");

    let at = set(
        &mut s,
        PatchProperty::DeviceOutputMute {
            device_output: 0,
            value: true,
        },
    );
    s.run(400);
    heard_edit(&s, 0, at, lowered, 0.0, at + 300);
    held(&s.channel(1)[at - 100..at + 300], LEVEL, "device output 2 untouched");
    // Unmuted, it comes back at its level, which the mute kept.
    let at = set(
        &mut s,
        PatchProperty::DeviceOutputMute {
            device_output: 0,
            value: false,
        },
    );
    s.run(400);
    heard_edit(&s, 0, at, 0.0, lowered, at + 300);
    assert_eq!(patch(&s).device_output(0).level_db, -6.0);
}

/// Checklist (engine part): what the editor's buttons send, one step each. Routing set to the
/// defaults (no crosspoints, device outputs at 0 dB) plays as the mapping says; mutes cleared
/// unmute every device output and cue output; one undo brings back the routing before.
#[test]
fn routing_set_to_defaults_and_mutes_cleared_are_one_step_each() {
    let mut s = show_on(steady(false), 4);
    set(&mut s, crosspoint(0, 0, Some(SILENCE_DB)));
    set(&mut s, crosspoint(0, 3, Some(-6.0)));
    set(
        &mut s,
        PatchProperty::DeviceOutputMute {
            device_output: 3,
            value: true,
        },
    );
    go(&mut s);
    s.run(800);
    held(&s.channel(0)[700..800], 0.0, "mapped route taken away");
    held(&s.channel(3)[700..800], 0.0, "device output 4 muted");

    // Clear mutes: one step, heard at once.
    let cleared: Vec<_> = patch(&s)
        .device_outputs
        .into_iter()
        .map(|d| DeviceOutputLevel { mute: false, ..d })
        .filter(|d| !d.is_default())
        .collect();
    let at = edit(
        &mut s,
        Command::Transaction {
            commands: vec![Command::SetPatchProperty {
                id: patch_id(),
                property: PatchProperty::DeviceOutputs { value: cleared },
            }],
        },
    );
    s.run(400);
    heard_edit(&s, 3, at, 0.0, LEVEL * db_to_gain(-6.0), at + 300);

    // Defaults: the mapping's routing, as for a new patch.
    let to_defaults = Command::Transaction {
        commands: vec![
            Command::SetPatchProperty {
                id: patch_id(),
                property: PatchProperty::Crosspoints { value: vec![] },
            },
            Command::SetPatchProperty {
                id: patch_id(),
                property: PatchProperty::DeviceOutputs { value: vec![] },
            },
        ],
    };
    let at = edit(&mut s, to_defaults);
    s.run(400);
    heard_edit(&s, 0, at, 0.0, LEVEL, at + 300);
    heard_edit(&s, 3, at, LEVEL * db_to_gain(-6.0), 0.0, at + 300);
    assert!(patch(&s).crosspoints.is_empty());

    let rid = s.request_id();
    s.send(ClientMessage::Undo { id: rid });
    assert_eq!(
        patch(&s).crosspoints,
        vec![
            PatchCrosspoint {
                output: 0,
                device_output: 0,
                db: SILENCE_DB
            },
            PatchCrosspoint {
                output: 0,
                device_output: 3,
                db: -6.0
            },
        ]
    );
}

/// Checklist: a show saved with such routing reopens with it; on a machine with fewer device
/// outputs the missing ones are warned about, not lost: the rest plays, and saving again writes
/// the same file. Back on a machine with the outputs, it is heard there.
#[test]
fn a_show_saved_with_routing_reopens_with_it_and_fewer_outputs_warn_and_keep_it() {
    let mut s = show_on(steady(false), 4);
    set(&mut s, crosspoint(0, 3, Some(-6.0)));
    set(
        &mut s,
        PatchProperty::DeviceOutputLevel {
            device_output: 3,
            value: -3.0,
        },
    );
    assert!(!notice_codes(&s).contains(&"patch_routing_beyond_device".to_owned()));
    let text = save_show(s.control().workspace()).unwrap();
    let loaded = load_show(&text).unwrap();
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);

    // A two-output machine: warned, kept, and the mapped route still plays.
    let mut small = show_on(loaded.workspace.clone(), 2);
    small.run(64);
    let warnings = small.control().snapshot().warnings;
    let notice = warnings
        .iter()
        .find_map(|w| match w {
            WireWarning::Notice {
                code,
                message,
                device,
                ..
            } if code == "patch_routing_beyond_device" => Some((message.clone(), device.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no warning: {warnings:?}"));
    assert!(notice.0.contains("device output 4"), "{}", notice.0);
    assert_eq!(notice.1.as_deref(), Some("Offline"));
    let start = go(&mut small) as usize;
    small.run(400);
    held(&small.channel(0)[start + 10..start + 300], LEVEL, "mapped route");
    assert_eq!(save_show(small.control().workspace()).unwrap(), text);

    // The four-output machine again: no warning, heard on device output 4 at -9 dB.
    let mut big = show_on(loaded.workspace, 4);
    assert!(!notice_codes(&big).contains(&"patch_routing_beyond_device".to_owned()));
    let start = go(&mut big) as usize;
    big.run(400);
    held(
        &big.channel(3)[start + 10..start + 300],
        LEVEL * db_to_gain(-9.0),
        "crosspoint × device output",
    );
}

/// A crosspoint typed to a device output this device lacks is kept and warned about at once;
/// taken away, the warning goes. A silenced one, or one for a cue output the patch lacks, routes
/// nothing and is not warned about.
#[test]
fn routing_to_a_device_output_the_device_lacks_warns_while_it_is_there() {
    let mut s = show_on(steady(false), 2);
    let code = "patch_routing_beyond_device".to_owned();
    set(&mut s, crosspoint(0, 5, Some(SILENCE_DB)));
    set(&mut s, crosspoint(7, 5, Some(0.0)));
    s.run(64);
    assert!(!notice_codes(&s).contains(&code));
    set(&mut s, crosspoint(0, 5, Some(-6.0)));
    s.run(64);
    assert!(notice_codes(&s).contains(&code));
    set(&mut s, crosspoint(0, 5, None));
    s.run(64);
    assert!(!notice_codes(&s).contains(&code));
    set(
        &mut s,
        PatchProperty::DeviceOutputMute {
            device_output: 2,
            value: true,
        },
    );
    s.run(64);
    assert!(notice_codes(&s).contains(&code));
}

/// The show model's device-output limit is the audio engine's.
#[test]
fn the_device_output_limit_is_the_mixers() {
    assert_eq!(
        openlab_core::MAX_DEVICE_OUTPUTS as usize,
        openlab_audio::MAX_OUTPUT_CHANNELS
    );
}
