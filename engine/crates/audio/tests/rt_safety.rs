// SPDX-License-Identifier: GPL-3.0-or-later

//! Real-time safety (NFR-02): no allocation in `process` on any path, and faults (underrun, pool
//! exhaustion, bad commands) degrade to silence plus an event, never a panic.

mod common;

use common::*;
use openlab_audio::{
    CommandKind, Curve, Event, Fade, FinishReason, GainTarget, MixerConfig, OutputSettings,
    RejectReason, VoiceId, VoiceStart,
};

#[test]
fn harness_detects_allocation() {
    let before = assert_no_alloc::violation_count();
    let v = assert_no_alloc::assert_no_alloc(|| std::hint::black_box(vec![1u8; 16]));
    assert!(assert_no_alloc::violation_count() > before);
    drop(v);
}

#[test]
fn no_allocation_across_a_busy_voice_lifecycle() {
    let cfg = MixerConfig {
        output_channels: 8,
        max_block_frames: 256,
        max_voices: 80,
        ..MixerConfig::default()
    };
    let (mut ctl, mut r) = setup(cfg);
    let stereo: Vec<f32> = (0..3000)
        .flat_map(|i| [i as f32 * 1e-4, -(i as f32) * 1e-4])
        .collect();
    let curves = [
        Curve::Linear,
        Curve::EqualPower,
        Curve::SCurve,
        Curve::Logarithmic,
        Curve::Exponential,
    ];
    for id in 0..64u64 {
        let mut v = VoiceStart::new(VoiceId(id), source(&stereo), 2);
        v.fade_in = Fade::new(200, curves[id as usize % 5]);
        if id % 3 == 0 {
            v.play_frames = Some(2000);
            v.fade_out = Fade::new(300, curves[(id as usize + 2) % 5]);
        }
        ctl.send(id * 7, CommandKind::StartVoice(v)).unwrap();
        let target = GainTarget::Crosspoint {
            input: 1,
            output: (id % 8) as u16,
        };
        ctl.send(
            500 + id,
            CommandKind::Ramp {
                voice: VoiceId(id),
                target,
                gain: 0.5,
                fade: Fade::new(400, curves[id as usize % 5]),
            },
        )
        .unwrap();
        ctl.send(
            600 + id,
            CommandKind::Ramp {
                voice: VoiceId(id),
                target: GainTarget::Master,
                gain: 0.7,
                fade: Fade::NONE,
            },
        )
        .unwrap();
        let fade = if id % 2 == 0 {
            Fade::new(250, Curve::SCurve)
        } else {
            Fade::NONE
        };
        if id % 4 == 1 {
            ctl.send(
                1500 + id,
                CommandKind::StopVoice {
                    voice: VoiceId(id),
                    fade,
                },
            )
            .unwrap();
        }
    }
    // An underrunning voice, and one more than the remaining pool can take after the burst.
    let (_keep_open, starving) = open_source(1000, &[0.1; 200]);
    start(&mut ctl, 0, 100, starving, 1);
    ctl.send(
        800,
        CommandKind::SetOutput {
            output: 3,
            settings: OutputSettings {
                level: 0.5,
                mute: false,
                invert: true,
                delay_frames: 333,
            },
        },
    )
    .unwrap();
    ctl.send(
        900,
        CommandKind::SetPatchRouting {
            cue_output: 1,
            device_output: 4,
            gain: 0.5,
        },
    )
    .unwrap();
    ctl.send(950, CommandKind::SetMaster { gain: 0.8 }).unwrap();

    let mut out = vec![0.0; 8 * 4000];
    no_alloc(|| r.render_into(&mut out, &[1, 64, 333, 7, 1024, 128]));

    assert!(out.iter().all(|s| s.is_finite()));
    assert!(out.iter().any(|s| *s != 0.0));
    let ev = events(&mut ctl);
    let finished = ev
        .iter()
        .filter(|e| matches!(e, Event::VoiceFinished { .. }))
        .count();
    assert_eq!(
        finished, 64,
        "every streamed voice ends by stop, out point or end of stream"
    );
    assert!(ev.contains(&Event::Underrun {
        voice: VoiceId(100),
        at: 200
    }));
    assert_eq!(ctl.collect_returns(), 64);
    assert_eq!(ctl.faults(), 0);
    assert_eq!(ctl.leaked_sources(), 0);
}

#[test]
fn underrun_outputs_silence_reports_and_recovers() {
    let (mut ctl, mut r) = setup(config(1));
    let (mut tx, rx) = open_source(1000, &[1.0; 100]);
    start(&mut ctl, 0, 1, rx, 1);
    let mut out = vec![0.0; 300];
    no_alloc(|| r.render_into(&mut out, &[64]));
    assert!(out[..100].iter().all(|s| *s == 1.0));
    assert!(out[100..].iter().all(|s| *s == 0.0));
    assert_eq!(
        events(&mut ctl),
        [Event::Underrun {
            voice: VoiceId(1),
            at: 100
        }]
    );
    assert_eq!(ctl.underrun_frames(), 200);

    // The disk thread catches up: playback resumes, then a new episode is reported.
    tx.push_entire_slice(&[1.0; 100]).unwrap();
    no_alloc(|| r.render_into(&mut out, &[64]));
    assert!(out[..100].iter().all(|s| *s == 1.0));
    assert!(out[100..].iter().all(|s| *s == 0.0));
    assert_eq!(
        events(&mut ctl),
        [Event::Underrun {
            voice: VoiceId(1),
            at: 400
        }]
    );

    // End of stream: the voice finishes cleanly and its source comes back.
    drop(tx);
    no_alloc(|| r.render_into(&mut out, &[64]));
    assert_eq!(
        events(&mut ctl),
        [Event::VoiceFinished {
            voice: VoiceId(1),
            at: 600,
            reason: FinishReason::EndOfStream
        }]
    );
    assert_eq!(ctl.collect_returns(), 1);
}

#[test]
fn rejected_starts_report_and_return_their_source() {
    let cfg = MixerConfig {
        max_voices: 2,
        max_voice_channels: 2,
        ..config(2)
    };
    let (mut ctl, mut r) = setup(cfg);
    start(&mut ctl, 0, 1, source(&[1.0; 2000]), 1);
    start(&mut ctl, 0, 1, source(&[1.0; 2000]), 1);
    start(&mut ctl, 0, 2, source(&[1.0; 2000]), 1);
    start(&mut ctl, 0, 3, source(&[1.0; 2000]), 1);
    start(&mut ctl, 10, 4, source(&[1.0; 2000]), 0);
    start(&mut ctl, 10, 5, source(&[1.0; 2000]), 3);
    let mut out = vec![0.0; 2 * 100];
    no_alloc(|| r.render_into(&mut out, &[32]));
    let rejected = |voice, at, reason| Event::VoiceRejected {
        voice: VoiceId(voice),
        at,
        reason,
    };
    assert_eq!(
        events(&mut ctl),
        [
            rejected(1, 0, RejectReason::DuplicateId),
            rejected(3, 0, RejectReason::PoolFull),
            rejected(4, 10, RejectReason::BadChannelCount),
            rejected(5, 10, RejectReason::BadChannelCount),
        ]
    );
    assert_eq!(ctl.collect_returns(), 4);
    // The two accepted voices play.
    assert_eq!(out[50 * 2], 2.0);
}

#[test]
fn commands_for_unknown_voices_or_indices_are_ignored() {
    let (mut ctl, mut r) = setup(config(2));
    start(&mut ctl, 0, 1, source(&[1.0; 2000]), 1);
    let bad_xp = GainTarget::Crosspoint {
        input: 5,
        output: 0,
    };
    let cmds = [
        CommandKind::StopVoice {
            voice: VoiceId(9),
            fade: Fade::NONE,
        },
        CommandKind::Ramp {
            voice: VoiceId(9),
            target: GainTarget::Master,
            gain: 0.0,
            fade: Fade::NONE,
        },
        CommandKind::Ramp {
            voice: VoiceId(1),
            target: bad_xp,
            gain: 1.0,
            fade: Fade::NONE,
        },
        CommandKind::Ramp {
            voice: VoiceId(1),
            target: GainTarget::CueOutput(99),
            gain: 0.0,
            fade: Fade::NONE,
        },
        CommandKind::SetPatchRouting {
            cue_output: 99,
            device_output: 0,
            gain: 1.0,
        },
        CommandKind::SetOutput {
            output: 99,
            settings: OutputSettings::default(),
        },
    ];
    for c in cmds {
        ctl.send(0, c).unwrap();
    }
    let mut out = vec![0.0; 2 * 100];
    no_alloc(|| r.render_into(&mut out, &[16]));
    assert!(channel(&out, 2, 0).iter().all(|s| *s == 1.0));
    assert!(channel(&out, 2, 1).iter().all(|s| *s == 0.0));
    assert!(events(&mut ctl).is_empty());
}
