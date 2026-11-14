// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

//! A 64-channel audio file plays through the control loop (AUD-03, AUD-04): symphonia
//! cannot read such WAVs, so this is the in-house PCM reader behind a real cue, with every file
//! channel routed to its own output of a 64-output offline device.

use crate::show::*;
use openlab_audio::MixerConfig;
use openlab_core::protocol::ClientMessage;
use openlab_core::{
    Command, Crosspoint, CueKind, CueProperty, DeviceInfo, MachineSettings, PatchMapping,
    PatchOutput, db_to_gain,
};
use openlab_engine::control::ControlConfig;
use openlab_engine::control::offline::OfflineShow;

const CHANNELS: usize = 64;
const FRAMES: usize = 400;

/// Channel `c` of the file holds the constant `(c + 1) / 128` (exact in `f32`).
fn level(c: usize) -> f32 {
    (c as f32 + 1.0) / 128.0
}

/// A 64-channel WAVE_FORMAT_EXTENSIBLE float file at the show's rate.
fn wide_wav(frames: usize) -> std::path::PathBuf {
    let ch = CHANNELS as u16;
    let data_len = frames * CHANNELS * 4;
    let mut fmt = Vec::new();
    fmt.extend_from_slice(&0xFFFEu16.to_le_bytes());
    fmt.extend_from_slice(&ch.to_le_bytes());
    fmt.extend_from_slice(&RATE.to_le_bytes());
    fmt.extend_from_slice(&(RATE * u32::from(ch) * 4).to_le_bytes());
    fmt.extend_from_slice(&(ch * 4).to_le_bytes());
    fmt.extend_from_slice(&32u16.to_le_bytes());
    fmt.extend_from_slice(&22u16.to_le_bytes());
    fmt.extend_from_slice(&32u16.to_le_bytes());
    // A mask naming fewer speakers than the file has channels: ignored.
    fmt.extend_from_slice(&0x3u32.to_le_bytes());
    fmt.extend_from_slice(&3u16.to_le_bytes());
    fmt.extend_from_slice(&[
        0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
    ]);
    let mut v = Vec::new();
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&((4 + 8 + fmt.len() + 8 + data_len) as u32).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
    v.extend_from_slice(&fmt);
    v.extend_from_slice(b"data");
    v.extend_from_slice(&(data_len as u32).to_le_bytes());
    for _ in 0..frames {
        for c in 0..CHANNELS {
            v.extend_from_slice(&level(c).to_le_bytes());
        }
    }
    file("wav", &v)
}

/// A show of cue 1, playing `path` with `crosspoints` on a 64-output offline device.
fn wide_show(path: &std::path::Path, crosspoints: Vec<Crosspoint>) -> OfflineShow {
    let mut cue = audio(1, path, 0);
    if let CueKind::Audio { levels, .. } = &mut cue.kind {
        levels.outputs = vec![
            openlab_core::CueOutput {
                db: 0.0,
                mute: false,
            };
            CHANNELS
        ];
        levels.crosspoints = crosspoints;
    }
    let mut ws = workspace(vec![cue]);
    ws.patches[0].outputs = vec![
        PatchOutput {
            name: String::new(),
            level_db: 0.0,
            mute: false,
            polarity_inverted: false,
        };
        CHANNELS
    ];
    let mut config = ControlConfig::new(RATE, BLOCK);
    config.seed = 7;
    config.devices = vec![DeviceInfo {
        name: "Offline".into(),
        output_channels: CHANNELS as u32,
    }];
    let mut machine = MachineSettings::default();
    machine
        .set_patch_mapping(
            ws.id,
            patch_id(),
            PatchMapping {
                device: "Offline".into(),
                channels: (0..CHANNELS as u32).collect(),
            },
        )
        .unwrap();
    config.machine = machine;
    config.mixer_patches = 2;
    let mixer = MixerConfig {
        output_channels: CHANNELS,
        max_voice_channels: CHANNELS,
        ..mixer_config()
    };
    let mut s = OfflineShow::new(ws, mixer, config).unwrap();
    s.settle_media();
    s.take_outbound();
    s
}

#[test]
fn a_64_channel_cue_plays_each_channel_on_its_own_output() {
    let path = wide_wav(FRAMES);
    let crosspoints = (0..CHANNELS as u32)
        .map(|i| Crosspoint {
            input: i,
            output: i,
            db: 0.0,
            mute: false,
        })
        .collect();
    let mut s = wide_show(&path, crosspoints);
    let at = start(&mut s, 1) as usize;
    s.run(FRAMES as u64 + 400);
    for c in 0..CHANNELS {
        let out = s.channel(c);
        let first = first_nonzero(&out).unwrap_or_else(|| panic!("channel {c} is silent"));
        assert_eq!(first, at, "channel {c} starts with the cue");
        assert_eq!(out[at..at + FRAMES], vec![level(c); FRAMES], "channel {c}");
        assert_eq!(
            last_nonzero(&out),
            Some(at + FRAMES - 1),
            "channel {c} ends"
        );
    }
}

/// #377: an edit of every level of a playing 64x64 cue (1 + 64 + 4096 gains) reaches the mixer at once and is heard as one: every output starts the edit ramp on
/// its date and ends it 10 ms later, and no output runs ahead of another.
#[test]
fn a_whole_matrix_edit_of_a_64x64_cue_moves_every_output_together() {
    const EDIT: usize = 80;
    let crosspoint = |input, output, db| Crosspoint {
        input,
        output,
        db,
        mute: false,
    };
    let matrix = |db| -> Vec<Crosspoint> {
        (0..CHANNELS as u32)
            .flat_map(|i| (0..CHANNELS as u32).map(move |o| (i, o)))
            .map(|(i, o)| crosspoint(i, o, db))
            .collect()
    };
    let path = wide_wav(RATE as usize);
    let mut s = wide_show(&path, matrix(-40.0));
    start(&mut s, 1);
    s.run(800);

    let mut kind = s
        .control()
        .workspace()
        .find_cue(id(1))
        .unwrap()
        .kind
        .clone();
    let CueKind::Audio { levels, .. } = &mut kind else {
        panic!("not audio");
    };
    levels.crosspoints = matrix(-30.0);
    levels.master_db = -6.0;
    levels.outputs.iter_mut().for_each(|o| o.db = -6.0);
    let at = s.now() as usize;
    let rid = s.request_id();
    s.send(ClientMessage::Command {
        id: rid,
        command: Box::new(Command::SetCueProperty {
            id: id(1),
            property: CueProperty::Kind { value: kind },
        }),
        coalesce: None,
    });
    assert_eq!(s.control().mixer().held(), 0, "nothing behind it waits");
    s.run(1000);

    let sum: f32 = (0..CHANNELS).map(level).sum();
    let old = sum * db_to_gain(-40.0);
    let new = sum * db_to_gain(-30.0) * db_to_gain(-6.0) * db_to_gain(-6.0);
    // Halfway through the edit ramp each gain is halfway to its new value.
    let halfway = |a: f32, b: f32| (a + b) / 2.0;
    let unity_to_half = halfway(1.0, db_to_gain(-6.0));
    let mid = sum * halfway(db_to_gain(-40.0), db_to_gain(-30.0)) * unity_to_half * unity_to_half;
    let near = |a: f32, b: f32| (a - b).abs() <= 1e-4;
    let outputs: Vec<Vec<f32>> = (0..CHANNELS).map(|c| s.channel(c)).collect();
    for (c, x) in outputs.iter().enumerate() {
        assert!(x[at - 100..at].iter().all(|&v| near(v, old)), "{c} before");
        assert!(near(x[at + EDIT / 2], mid), "{c} mid: {}", x[at + EDIT / 2]);
        assert!(
            x[at + EDIT..at + 800].iter().all(|&v| near(v, new)),
            "{c} after"
        );
        assert_eq!(x[at..at + EDIT], outputs[0][at..at + EDIT], "output {c}");
    }
}
