// SPDX-License-Identifier: GPL-3.0-or-later

//! A 64-channel audio file plays through the control loop (AUD-03, AUD-04): symphonia
//! cannot read such WAVs, so this is the in-house PCM reader behind a real cue, with every file
//! channel routed to its own output of a 64-output offline device.

use crate::show::*;
use openlab_audio::MixerConfig;
use openlab_core::{Crosspoint, CueKind, DeviceInfo, MachineSettings, PatchMapping, PatchOutput};
use openlab_engine::control::ControlConfig;

const CHANNELS: usize = 64;
const FRAMES: usize = 400;

/// Channel `c` of the file holds the constant `(c + 1) / 128` (exact in `f32`).
fn level(c: usize) -> f32 {
    (c as f32 + 1.0) / 128.0
}

/// A 64-channel WAVE_FORMAT_EXTENSIBLE float file at the show's rate.
fn wide_wav() -> std::path::PathBuf {
    let ch = CHANNELS as u16;
    let data_len = FRAMES * CHANNELS * 4;
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
    for _ in 0..FRAMES {
        for c in 0..CHANNELS {
            v.extend_from_slice(&level(c).to_le_bytes());
        }
    }
    file("wav", &v)
}

#[test]
fn a_64_channel_cue_plays_each_channel_on_its_own_output() {
    let path = wide_wav();
    let mut cue = audio(1, &path, 0);
    if let CueKind::Audio { levels, .. } = &mut cue.kind {
        levels.outputs = vec![
            openlab_core::CueOutput {
                db: 0.0,
                mute: false,
            };
            CHANNELS
        ];
        levels.crosspoints = (0..CHANNELS as u32)
            .map(|i| Crosspoint {
                input: i,
                output: i,
                db: 0.0,
                mute: false,
            })
            .collect();
    }
    let mut ws = workspace(vec![cue]);
    ws.patches[0].outputs = vec![
        PatchOutput {
            level_db: 0.0,
            mute: false,
            delay_ms: 0.0,
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
    let mut s = openlab_engine::control::offline::OfflineShow::new(ws, mixer, config).unwrap();
    s.settle_media();
    s.take_outbound();
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
