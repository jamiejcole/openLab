// SPDX-License-Identifier: GPL-3.0-or-later

//! Real-time safety (NFR-02): no allocation in `process` on any path, and faults (underrun, pool
//! exhaustion, full queues, bad commands) degrade to silence plus an event, never a panic.

mod common;

use common::*;
use openlab_audio::{
    CommandKind, Curve, Event, Fade, FinishReason, GainTarget, MixerConfig, OutputSettings,
    PENDING_CAPACITY, RejectReason, VoiceId,
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
        let mut v = voice(&ctl, id, source(&stereo), 2);
        v.patch = (id % 3) as u16;
        v.fade_in = Fade::new(200, curves[id as usize % 5]);
        if id % 3 == 0 {
            v.play_frames = Some(2000);
            v.fade_out = Fade::new(300, curves[(id as usize + 2) % 5]);
        }
        send_start(&mut ctl, id * 7, v);
        let target = GainTarget::Crosspoint {
            input: 1,
            output: (id % 8) as u16,
        };
        let fade = Fade::new(400, curves[id as usize % 5]);
        ctl.send(
            500 + id,
            CommandKind::Ramp {
                voice: VoiceId(id),
                target,
                gain: 0.5,
                fade,
            },
        );
        ctl.send(
            600 + id,
            CommandKind::Ramp {
                voice: VoiceId(id),
                target: GainTarget::Master,
                gain: 0.7,
                fade: Fade::NONE,
            },
        );
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
            );
        }
    }
    let (_keep_open, starving) = open_source(1000, &[0.1; 200]);
    start(&mut ctl, 0, 100, starving, 1);
    let settings = OutputSettings {
        level: 0.5,
        mute: false,
        invert: true,
        delay_frames: 333,
    };
    ctl.send(
        800,
        CommandKind::SetOutput {
            output: 3,
            settings,
        },
    );
    ctl.send(
        900,
        CommandKind::SetPatchRouting {
            patch: 1,
            cue_output: 1,
            device_output: 4,
            gain: 0.5,
        },
    );
    ctl.send(950, CommandKind::SetMaster { gain: 0.8 });

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
    assert_eq!(ctl.leaked_starts(), 0);
    assert_eq!(ctl.dropped_events(), 0);
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

    // End of stream: the voice finishes cleanly and its start comes back.
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
    let mut short = voice(&ctl, 6, source(&[1.0; 2000]), 1);
    short.crosspoints.pop();
    send_start(&mut ctl, 10, short);
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
            rejected(6, 10, RejectReason::BadRouting),
        ]
    );
    assert_eq!(ctl.collect_returns(), 5);
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
    let ramp = |voice, target| CommandKind::Ramp {
        voice: VoiceId(voice),
        target,
        gain: 1.0,
        fade: Fade::NONE,
    };
    let cmds = [
        CommandKind::StopVoice {
            voice: VoiceId(9),
            fade: Fade::NONE,
        },
        ramp(9, GainTarget::Master),
        ramp(1, bad_xp),
        ramp(1, GainTarget::CueOutput(99)),
        CommandKind::SetPatchRouting {
            patch: 0,
            cue_output: 99,
            device_output: 0,
            gain: 1.0,
        },
        CommandKind::SetPatchRouting {
            patch: 99,
            cue_output: 0,
            device_output: 1,
            gain: 1.0,
        },
        CommandKind::SetOutput {
            output: 99,
            settings: OutputSettings::default(),
        },
    ];
    for c in cmds {
        ctl.send(0, c);
    }
    let mut out = vec![0.0; 2 * 100];
    no_alloc(|| r.render_into(&mut out, &[16]));
    assert!(channel(&out, 2, 0).iter().all(|s| *s == 1.0));
    assert!(channel(&out, 2, 1).iter().all(|s| *s == 0.0));
    assert!(events(&mut ctl).is_empty());
}

#[test]
fn full_event_queue_drops_and_counts_without_allocating() {
    let cfg = MixerConfig {
        max_voices: 1,
        ..config(1)
    };
    let (mut ctl, mut r) = setup(cfg);
    start(&mut ctl, 0, 0, source(&[1.0; 4000]), 1);
    let mut out = vec![0.0; 64];
    for round in 0..2 {
        for i in 0..1000 {
            start(&mut ctl, 0, 1 + round * 1000 + i, source(&[1.0; 4]), 1);
        }
        no_alloc(|| r.render_into(&mut out, &[64]));
        assert!(
            out.iter().all(|s| *s == 1.0),
            "the playing voice is unaffected"
        );
    }
    // 2000 rejections, 1024 fit in the event queue.
    assert_eq!(ctl.dropped_events(), 2000 - 1024);
    assert_eq!(events(&mut ctl).len(), 1024);
    assert_eq!(ctl.leaked_starts(), 0);
}

#[test]
fn full_pending_set_is_reported_and_nothing_is_lost() {
    let cfg = MixerConfig {
        lookahead_ms: 60_000,
        ..config(1)
    };
    let (mut ctl, mut r) = setup(cfg);
    start(&mut ctl, 0, 1, source(&vec![1.0; 40_000]), 1);
    let n = PENDING_CAPACITY + 1500;
    for i in 0..n as u64 {
        // Gains climb to 0.5 at the last one, so the output shows every command arrived in order.
        let gain = if i + 1 == n as u64 { 0.5 } else { 0.9 };
        ctl.send(5000 + i, CommandKind::SetMaster { gain });
    }
    let mut out = vec![0.0; 256];
    let mut seen_full = false;
    for _ in 0..80 {
        ctl.pump();
        no_alloc(|| r.render_into(&mut out, &[256]));
        seen_full |= events(&mut ctl)
            .iter()
            .any(|e| matches!(e, Event::PendingFull { .. }));
    }
    assert!(seen_full);
    assert!(ctl.pending_full_blocks() > 0);
    assert_eq!(ctl.held(), 0);
    assert!(
        (out[255] - 0.5).abs() < 1e-6,
        "the last command was applied"
    );
}

/// Review M1: a full burst of 1024 equal-time commands lands in one block without allocating,
/// and every one of them applies (the last, in send order, sets the final gain).
#[test]
fn a_full_command_burst_applies_in_one_block_without_allocating() {
    let (mut ctl, mut r) = setup(config(1));
    start(&mut ctl, 0, 1, source(&vec![1.0; 4000]), 1);
    let n = openlab_audio::COMMAND_CAPACITY - 1;
    for i in 0..n {
        let gain = if i + 1 == n { 0.25 } else { 0.5 };
        ctl.send(64, CommandKind::SetMaster { gain });
    }
    assert_eq!(ctl.held(), 0, "the whole burst fits in the queue");
    let mut out = vec![0.0; 512];
    no_alloc(|| r.render_into(&mut out, &[512]));
    assert!(out[..64].iter().all(|s| *s == 1.0));
    assert!((out[64 + 240] - 0.25).abs() < 1e-6);
    assert_eq!(ctl.pending_full_blocks(), 0);
}
