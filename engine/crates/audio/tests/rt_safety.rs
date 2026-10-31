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

/// Pause and resume on every path (CUE-05): declick tails split across blocks, ramps on paused
/// voices, re-keying, an id collision, stops while paused, a devamp while paused, a starving
/// voice paused mid-underrun, and pause plus resume on the start sample.
#[test]
fn no_allocation_across_pause_and_resume() {
    let cfg = MixerConfig {
        output_channels: 4,
        max_block_frames: 256,
        max_voices: 40,
        ..MixerConfig::default()
    };
    let (mut ctl, mut r) = setup(cfg);
    let stereo: Vec<f32> = (0..6000)
        .flat_map(|i| [i as f32 * 1e-4, -(i as f32) * 1e-4])
        .collect();
    let pause = |voice: u64| CommandKind::PauseVoice {
        voice: VoiceId(voice),
    };
    let resume = |voice: u64, new_id: u64| CommandKind::ResumeVoice {
        voice: VoiceId(voice),
        new_id: VoiceId(new_id),
    };
    for id in 0..32u64 {
        let mut v = voice(&ctl, id, source(&stereo), 2);
        v.fade_in = Fade::new(100, Curve::EqualPower);
        if id % 2 == 0 {
            v.play_frames = Some(5000);
            v.fade_out = Fade::new(400, Curve::SCurve);
        }
        send_start(&mut ctl, id * 3, v);
        let fade = Fade::new(700, Curve::Exponential);
        ctl.send(
            200 + id,
            CommandKind::Ramp {
                voice: VoiceId(id),
                target: GainTarget::Master,
                gain: 0.3,
                fade,
            },
        );
        ctl.send(300 + id * 11, pause(id));
        ctl.send(
            900,
            CommandKind::Ramp {
                voice: VoiceId(id),
                target: GainTarget::CueOutput(1),
                gain: 0.5,
                fade: Fade::NONE,
            },
        );
        match id % 4 {
            0 => ctl.send(1200 + id, resume(id, 100 + id)),
            // Collides with voice 0's new id, then resumes properly.
            1 => {
                ctl.send(1300, resume(id, 100));
                ctl.send(1400 + id, resume(id, 200 + id));
            }
            2 => ctl.send(
                1000,
                CommandKind::StopVoice {
                    voice: VoiceId(id),
                    fade: Fade::new(300, Curve::Linear),
                },
            ),
            // Resumes inside its own declick tail.
            _ => ctl.send(300 + id * 11 + 50, resume(id, 300 + id)),
        }
    }
    let (_keep_open, starving) = open_source(1000, &[0.1; 200]);
    start(&mut ctl, 0, 50, starving, 1);
    ctl.send(400, pause(50));
    ctl.send(800, resume(50, 51));
    let (_keep_open2, rx) = open_source(1000, &[0.1; 200]);
    start(&mut ctl, 10, 60, rx, 1);
    ctl.send(10, pause(60));
    ctl.send(10, resume(60, 61));

    let mut out = vec![0.0; 4 * 8000];
    no_alloc(|| r.render_into(&mut out, &[1, 64, 333, 7, 256, 128]));

    assert!(out.iter().all(|s| s.is_finite()));
    let ev = events(&mut ctl);
    let finished = ev
        .iter()
        .filter(|e| matches!(e, Event::VoiceFinished { .. }))
        .count();
    assert_eq!(finished, 32);
    assert!(ev.contains(&Event::VoiceRejected {
        voice: VoiceId(100),
        at: 1300,
        reason: RejectReason::DuplicateId,
    }));
    assert_eq!(ctl.collect_returns(), 32);
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

/// a full burst of 1024 equal-time commands lands in one block without allocating,
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

/// Meters (AUD-19) on 64 voices × 2 cue outputs and 64 device outputs: publishing them in every
/// block, voice ends, slot reuse, pause and re-keying resume, xrun notes, and reading them into
/// caller buffers all run without allocating.
#[test]
fn no_allocation_with_meters_active_on_64_voices() {
    let cfg = MixerConfig {
        output_channels: 64,
        max_block_frames: 256,
        max_voices: 64,
        ..MixerConfig::default()
    };
    let (mut ctl, mut r) = setup(cfg);
    let stereo: Vec<f32> = (0..4000)
        .flat_map(|i| [i as f32 * 1e-4, -(i as f32) * 1e-4])
        .collect();
    for id in 0..64u64 {
        let mut v = voice(&ctl, id, source(&stereo), 2);
        // Spread the voices over the device outputs through the patch.
        v.set_crosspoint(0, (id % 64) as usize, 1.0);
        if id % 4 == 0 {
            v.play_frames = Some(500 + id * 10);
        }
        send_start(&mut ctl, id, v);
    }
    // Reuse the slots of the voices that end early.
    for id in 0..16u64 {
        start(&mut ctl, 2000 + id, 100 + id, source(&stereo), 2);
    }
    for id in (1..64u64).step_by(4) {
        ctl.send(1000, CommandKind::PauseVoice { voice: VoiceId(id) });
        ctl.send(
            1500,
            CommandKind::ResumeVoice {
                voice: VoiceId(id),
                new_id: VoiceId(1000 + id),
            },
        );
    }
    let cap = ctl.voice_meter_capacity();
    let (mut ids, mut peaks, mut outs) = (vec![VoiceId(0); cap], vec![0.0; cap * 64], [0.0; 64]);
    let mut out = vec![0.0; 64 * 300];
    let mut entries = 0;
    for _ in 0..12 {
        ctl.pump();
        no_alloc(|| {
            r.render_into(&mut out, &[1, 64, 7, 228]);
            r.inject_xruns(1);
            assert_eq!(ctl.read_output_meters_into(&mut outs), 64);
            entries += ctl.read_voice_meters_into(&mut ids, &mut peaks);
        });
        assert!(outs.iter().all(|p| p.is_finite()));
    }
    assert!(entries >= 64 * 12 - 64);
    assert_eq!(ctl.xruns(), 12);
    assert_eq!(ctl.faults(), 0);
    assert_eq!(ctl.leaked_starts(), 0);
}

#[test]
fn a_fader_drag_of_retargeted_short_ramps_allocates_nothing_and_never_jumps() {
    // a level edit on a playing cue is a 10 ms ramp from wherever the gain is; a drag
    // retargets one every few milliseconds, on the master, a cue output and a crosspoint.
    let (mut ctl, mut r) = setup(config(1));
    start(&mut ctl, 0, 1, source(&[1.0; 60_000]), 1);
    let ramp = |target, gain| CommandKind::Ramp {
        voice: VoiceId(1),
        target,
        gain,
        fade: Fade::new(480, Curve::Linear),
    };
    let xp = GainTarget::Crosspoint {
        input: 0,
        output: 0,
    };
    let mut out = Vec::new();
    let mut block = vec![0.0; 300];
    for k in 0..160u64 {
        let down = 1.0 - k as f32 / 200.0;
        let target = [GainTarget::Master, GainTarget::CueOutput(0), xp][(k % 3) as usize];
        ctl.send(k * 300, ramp(target, down));
        no_alloc(|| r.render_into(&mut block, &[64, 300]));
        out.extend_from_slice(&block);
    }
    assert!(out.iter().all(|s| s.is_finite()));
    let jump = out
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max);
    assert!(jump < 1e-3, "{jump}");
    // Every ramp was carried out: the gains got down to where the drag left them.
    assert!(out[out.len() - 1] < 0.5, "{}", out[out.len() - 1]);
    assert_eq!(ctl.faults(), 0);
}
