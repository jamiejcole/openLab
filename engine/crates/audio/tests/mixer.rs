// SPDX-License-Identifier: GPL-3.0-or-later

//! Sample-level behaviour of the mixer through the offline renderer: timing, fades, trims,
//! routing, levels, scheduling and block-size invariance.

mod common;

use common::*;
use openlab_audio::{
    CommandKind, Curve, Event, Fade, FinishReason, GainTarget, MAX_GAIN, MixerConfig,
    OutputSettings, RAMP_TOLERANCE, RejectReason, SILENCE_DB, VoiceId, VoiceStart, db_to_linear,
    ramp_value,
};
use proptest::prelude::*;

const CURVES: [Curve; 5] = [
    Curve::Linear,
    Curve::EqualPower,
    Curve::SCurve,
    Curve::Logarithmic,
    Curve::Exponential,
];

const PATTERNS: &[&[usize]] = &[
    &[1],
    &[2],
    &[3],
    &[7],
    &[13],
    &[64],
    &[127],
    &[256],
    &[257],
    &[1031],
    &[5, 1, 300, 17],
    &[64, 1, 2, 500, 3],
    &[511, 1, 1],
];

fn ramp(voice: u64, target: GainTarget, gain: f32, fade: Fade) -> CommandKind {
    CommandKind::Ramp {
        voice: VoiceId(voice),
        target,
        gain,
        fade,
    }
}

fn stop(voice: u64, fade: Fade) -> CommandKind {
    CommandKind::StopVoice {
        voice: VoiceId(voice),
        fade,
    }
}

fn set_output(output: u16, settings: OutputSettings) -> CommandKind {
    CommandKind::SetOutput { output, settings }
}

fn patch(patch: u16, cue_output: u16, device_output: u16, gain: f32) -> CommandKind {
    CommandKind::SetPatchRouting {
        patch,
        cue_output,
        device_output,
        gain,
    }
}

fn finished(voice: u64, at: u64, reason: FinishReason) -> Event {
    Event::VoiceFinished {
        voice: VoiceId(voice),
        at,
        reason,
    }
}

fn mono_dc(frames: usize) -> rtrb::Consumer<f32> {
    source(&vec![1.0; frames])
}

fn max_step(samples: &[f32]) -> f32 {
    samples
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max)
}

#[test]
fn simultaneous_starts_land_on_the_same_frame() {
    for &blocks in PATTERNS {
        for t in [0u64, 1000, 1001] {
            let (mut ctl, mut r) = setup(config(2));
            start(&mut ctl, t, 1, mono_dc(4000), 1);
            let mut b = VoiceStart::new(VoiceId(2), mono_dc(4000), 1, 2);
            b.set_crosspoint(0, 1, 1.0);
            send_start(&mut ctl, t, b);
            let out = r.render(3000, blocks);
            let (a, b) = (channel(&out, 2, 0), channel(&out, 2, 1));
            assert_eq!(first_nonzero(&a), Some(t as usize), "{blocks:?} t={t}");
            assert_eq!(first_nonzero(&b), Some(t as usize), "{blocks:?} t={t}");
            assert_eq!(a[t as usize], 1.0);
            assert_eq!(b[t as usize], 1.0);
        }
    }
}

/// Review H1 regression: the start and its routing travel together, so a voice is never heard
/// with default routing even if the mixer runs a block between the control thread's sends.
#[test]
fn routing_arrives_with_the_start_so_there_is_no_default_blip() {
    let (mut ctl, mut r) = setup(config(2));
    let mut v = VoiceStart::new(VoiceId(1), mono_dc(4000), 1, 2);
    v.set_crosspoint(0, 1, 0.01);
    send_start(&mut ctl, 256, v);
    let first = r.render(256, &[256]);
    let second = r.render(256, &[256]);
    assert!(first.iter().all(|s| *s == 0.0));
    assert!(channel(&second, 2, 0).iter().all(|s| *s == 0.0));
    assert!(channel(&second, 2, 1).iter().all(|s| *s == 0.01));
}

#[test]
fn a_voice_with_default_routing_is_silent() {
    let (mut ctl, mut r) = setup(config(2));
    send_start(
        &mut ctl,
        0,
        VoiceStart::new(VoiceId(1), mono_dc(4000), 1, 2),
    );
    assert!(r.render(1000, &[64]).iter().all(|s| *s == 0.0));
}

#[test]
fn immediate_stop_cuts_on_the_exact_sample() {
    for &blocks in PATTERNS {
        let (mut ctl, mut r) = setup(config(1));
        start(&mut ctl, 10, 1, mono_dc(4000), 1);
        ctl.send(777, stop(1, Fade::NONE));
        let out = r.render(1000, blocks);
        assert!(out[10..777].iter().all(|s| *s == 1.0), "{blocks:?}");
        assert!(out[777..].iter().all(|s| *s == 0.0), "{blocks:?}");
        assert_eq!(events(&mut ctl), [finished(1, 777, FinishReason::Stopped)]);
    }
}

#[test]
fn stop_with_fade_ends_at_zero_and_finishes() {
    let (mut ctl, mut r) = setup(config(1));
    start(&mut ctl, 0, 1, mono_dc(4000), 1);
    ctl.send(500, stop(1, Fade::new(100, Curve::Linear)));
    let out = r.render(1000, &[64]);
    for k in 0..=100 {
        assert_eq!(
            out[500 + k],
            ramp_value(1.0, 0.0, k as u64, 100, Curve::Linear),
            "k={k}"
        );
    }
    assert!(out[600..].iter().all(|s| *s == 0.0));
    assert_eq!(events(&mut ctl), [finished(1, 600, FinishReason::Stopped)]);
}

/// Review L1 regression: audio after a stop fade ends is not read or counted as underrun.
#[test]
fn no_underrun_is_reported_after_a_stop_fade_ends() {
    for &blocks in PATTERNS {
        let (mut ctl, mut r) = setup(config(1));
        let (_tx, rx) = open_source(1000, &[1.0; 150]);
        start(&mut ctl, 0, 1, rx, 1);
        ctl.send(0, stop(1, Fade::new(100, Curve::Linear)));
        r.render(256, blocks);
        assert_eq!(
            events(&mut ctl),
            [finished(1, 100, FinishReason::Stopped)],
            "{blocks:?}"
        );
        assert_eq!(ctl.underrun_frames(), 0);
    }
}

#[test]
fn ramp_mid_block_starts_on_the_exact_sample() {
    for curve in CURVES {
        let (mut ctl, mut r) = setup(config(1));
        start(&mut ctl, 100, 1, mono_dc(4000), 1);
        let at = 100 + 64 * 3 + 37;
        ctl.send(
            at as u64,
            ramp(1, GainTarget::Master, 0.0, Fade::new(200, curve)),
        );
        let out = r.render(1000, &[64]);
        assert!(out[100..at].iter().all(|s| *s == 1.0));
        for k in 0..=200 {
            assert_eq!(
                out[at + k],
                ramp_value(1.0, 0.0, k as u64, 200, curve),
                "{curve:?} k={k}"
            );
        }
        assert!(out[at + 1] < 1.0, "{curve:?}");
        assert!(out[at + 200..].iter().all(|s| *s == 0.0), "{curve:?}");
    }
}

#[test]
fn ramp_reaches_midpoint_gain_from_curve_formula() {
    for curve in CURVES {
        let (mut ctl, mut r) = setup(config(1));
        let mut v = voice(&ctl, 1, mono_dc(4000), 1);
        v.gain = 0.0;
        send_start(&mut ctl, 0, v);
        ctl.send(
            1000,
            ramp(1, GainTarget::Master, 1.0, Fade::new(1000, curve)),
        );
        let out = r.render(2500, &[100]);
        assert!(
            (f64::from(out[1500]) - curve.shape(0.5)).abs() < 1e-6,
            "{curve:?}"
        );
        assert_eq!(out[1000], 0.0);
        assert_eq!(out[2000], 1.0);
    }
}

#[test]
fn integrated_fade_in_starts_at_exact_zero() {
    for curve in CURVES {
        for &blocks in PATTERNS {
            let (mut ctl, mut r) = setup(config(1));
            let mut v = voice(&ctl, 1, mono_dc(4000), 1);
            v.fade_in = Fade::new(100, curve);
            send_start(&mut ctl, 300, v);
            let out = r.render(600, blocks);
            assert!(out[..=300].iter().all(|s| *s == 0.0));
            for k in 0..=100 {
                assert_eq!(
                    out[300 + k],
                    ramp_value(0.0, 1.0, k as u64, 100, curve),
                    "{curve:?} k={k}"
                );
            }
            assert!(out[301] > 0.0);
            assert!(out[400..].iter().all(|s| *s == 1.0));
        }
    }
}

/// The mixer evaluates curves by recurrence between exact knots (docs/audio.md): long ramps,
/// fade-ins and fade-outs stay within `RAMP_TOLERANCE` of `ramp_value`, land exactly, and do
/// not depend on the block size.
#[test]
fn long_curved_ramps_and_fades_are_within_tolerance_and_block_size_independent() {
    const FADE: u64 = 3001;
    for curve in CURVES {
        let mut first: Option<Vec<f32>> = None;
        for &blocks in PATTERNS {
            let (mut ctl, mut r) = setup(config(1));
            let mut v = voice(&ctl, 1, mono_dc(20_000), 1);
            v.gain = 4.0;
            v.fade_in = Fade::new(FADE, curve);
            v.fade_out = Fade::new(FADE, curve);
            v.play_frames = Some(15_000);
            send_start(&mut ctl, 0, v);
            ctl.send(
                5000,
                ramp(1, GainTarget::Master, 0.25, Fade::new(FADE, curve)),
            );
            let out = r.render(16_000, blocks);
            let near = |k: usize, want: f32, scale: f32| {
                let err = (f64::from(out[k]) - f64::from(want)).abs();
                assert!(
                    err <= RAMP_TOLERANCE * f64::from(scale),
                    "{curve:?} {blocks:?} frame {k}: {} vs {want}",
                    out[k]
                );
            };
            for k in 0..=FADE as usize {
                near(k, 4.0 * ramp_value(0.0, 1.0, k as u64, FADE, curve), 4.0);
                near(5000 + k, ramp_value(4.0, 0.25, k as u64, FADE, curve), 4.0);
                near(
                    11_998 + k,
                    0.25 * ramp_value(1.0, 0.0, k as u64, FADE, curve),
                    0.25,
                );
            }
            assert_eq!(out[0], 0.0);
            assert_eq!(
                out[5000 + FADE as usize],
                0.25,
                "{curve:?} lands on the target"
            );
            assert_eq!(out[14_999], 0.0, "{curve:?} last frame is silent");
            match &first {
                None => first = Some(out),
                Some(f) => assert!(
                    f.iter().zip(&out).all(|(a, b)| a.to_bits() == b.to_bits()),
                    "{curve:?} {blocks:?} differs from {:?}",
                    PATTERNS[0]
                ),
            }
        }
    }
}

#[test]
fn integrated_fade_out_ends_at_exact_zero_on_last_frame() {
    for curve in CURVES {
        for &blocks in PATTERNS {
            let (mut ctl, mut r) = setup(config(1));
            let mut v = voice(&ctl, 1, mono_dc(4000), 1);
            v.play_frames = Some(1000);
            v.fade_out = Fade::new(100, curve);
            send_start(&mut ctl, 300, v);
            let out = r.render(1500, blocks);
            assert!(out[300..=1199].iter().all(|s| *s == 1.0), "{curve:?}");
            for k in 0..=100 {
                assert_eq!(
                    out[1199 + k],
                    ramp_value(1.0, 0.0, k as u64, 100, curve),
                    "{curve:?} k={k}"
                );
            }
            assert_eq!(out[1299], 0.0, "last played frame");
            assert!(out[1298] > 0.0);
            assert_eq!(
                events(&mut ctl),
                [finished(1, 1300, FinishReason::OutPoint)]
            );
        }
    }
}

/// Review L6: a fade-out with no out point is an engine bug; make it visible.
#[test]
fn fade_out_without_out_point_is_rejected() {
    let (mut ctl, mut r) = setup(config(1));
    let mut v = voice(&ctl, 1, mono_dc(4000), 1);
    v.fade_out = Fade::new(100, Curve::Linear);
    send_start(&mut ctl, 0, v);
    assert!(r.render(100, &[64]).iter().all(|s| *s == 0.0));
    assert_eq!(
        events(&mut ctl),
        [Event::VoiceRejected {
            voice: VoiceId(1),
            at: 0,
            reason: RejectReason::BadFade
        }]
    );
}

#[test]
fn trims_play_exact_in_and_out_points() {
    let ramp_src: Vec<f32> = (0..1000).map(|i| i as f32).collect();
    for &blocks in PATTERNS {
        let (mut ctl, mut r) = setup(config(1));
        let mut v = voice(&ctl, 1, source(&ramp_src), 1);
        v.skip_frames = 100;
        v.play_frames = Some(50);
        send_start(&mut ctl, 20, v);
        let out = r.render(200, blocks);
        assert!(out[..20].iter().all(|s| *s == 0.0));
        for k in 0..50 {
            assert_eq!(out[20 + k], (100 + k) as f32, "{blocks:?}");
        }
        assert!(out[70..].iter().all(|s| *s == 0.0));
        assert_eq!(events(&mut ctl), [finished(1, 70, FinishReason::OutPoint)]);
    }
}

#[test]
fn end_of_stream_finishes_voice_on_the_next_sample() {
    for &blocks in PATTERNS {
        let (mut ctl, mut r) = setup(config(1));
        start(&mut ctl, 5, 1, mono_dc(500), 1);
        let out = r.render(1000, blocks);
        assert!(out[5..505].iter().all(|s| *s == 1.0));
        assert!(out[505..].iter().all(|s| *s == 0.0));
        assert_eq!(
            events(&mut ctl),
            [finished(1, 505, FinishReason::EndOfStream)]
        );
        assert_eq!(ctl.collect_returns(), 1);
    }
}

#[test]
fn crosspoints_cue_outputs_and_voice_master() {
    let (mut ctl, mut r) = setup(config(3));
    let stereo: Vec<f32> = (0..2000).flat_map(|_| [1.0, 0.5]).collect();
    let mut v = VoiceStart::new(VoiceId(1), source(&stereo), 2, 3);
    v.gain = 0.8;
    v.set_crosspoint(0, 0, 0.5);
    v.set_crosspoint(0, 1, 0.25);
    v.set_crosspoint(1, 1, 1.0);
    v.set_crosspoint(1, 2, 1.0);
    v.cue_outputs[2] = 0.5;
    send_start(&mut ctl, 0, v);
    let out = r.render(100, &[64]);
    let frame = &out[50 * 3..51 * 3];
    let expect = [
        1.0 * 0.5 * 0.8,
        (1.0 * 0.25 + 0.5 * 1.0) * 0.8,
        0.5 * 0.5 * 0.8,
    ];
    for (got, want) in frame.iter().zip(expect) {
        assert!((got - want).abs() < 1e-6, "{frame:?} vs {expect:?}");
    }
    // The same gains remain rampable after the start.
    ctl.send(
        200,
        ramp(
            1,
            GainTarget::Crosspoint {
                input: 0,
                output: 0,
            },
            1.0,
            Fade::new(10, Curve::Linear),
        ),
    );
    let out = r.render(200, &[64]);
    assert!((out[150 * 3] - 0.8).abs() < 1e-6);
}

#[test]
fn patch_routing_maps_cue_outputs_to_device_outputs() {
    let (mut ctl, mut r) = setup(config(2));
    ctl.send(0, patch(0, 0, 0, 0.0));
    ctl.send(0, patch(0, 0, 1, 0.5));
    start(&mut ctl, 1000, 1, mono_dc(2000), 1);
    let out = r.render(1100, &[64]);
    assert_eq!(&out[1050 * 2..1051 * 2], &[0.0, 0.5]);
}

/// M3.11: each voice goes through its own patch.
#[test]
fn voices_route_through_their_own_patch() {
    let (mut ctl, mut r) = setup(config(2));
    ctl.send(0, patch(1, 0, 0, 0.0));
    ctl.send(0, patch(1, 0, 1, 0.25));
    start(&mut ctl, 1000, 1, mono_dc(2000), 1);
    let mut b = voice(&ctl, 2, mono_dc(2000), 1);
    b.patch = 1;
    send_start(&mut ctl, 1000, b);
    let mut bad = voice(&ctl, 3, mono_dc(2000), 1);
    bad.patch = 99;
    send_start(&mut ctl, 1000, bad);
    let out = r.render(1100, &[64]);
    assert_eq!(&out[1050 * 2..1051 * 2], &[1.0, 0.25]);
    assert_eq!(
        events(&mut ctl),
        [Event::VoiceRejected {
            voice: VoiceId(3),
            at: 1000,
            reason: RejectReason::BadRouting
        }]
    );
}

#[test]
fn output_level_mute_and_polarity() {
    let (mut ctl, mut r) = setup(config(4));
    let set = |level, mute, invert| OutputSettings {
        level,
        mute,
        invert,
        delay_frames: 0,
    };
    ctl.send(0, set_output(0, set(0.5, false, false)));
    ctl.send(0, set_output(1, set(1.0, false, true)));
    ctl.send(0, set_output(2, set(1.0, true, false)));
    ctl.send(0, set_output(3, set(db_to_linear(-6.0), false, true)));
    start(&mut ctl, 1000, 1, source(&vec![1.0; 4 * 2000]), 4);
    let out = r.render(1100, &[64]);
    assert_eq!(
        &out[1050 * 4..1051 * 4],
        &[0.5, -1.0, 0.0, -db_to_linear(-6.0)]
    );
}

#[test]
fn output_delay_is_sample_exact() {
    for &blocks in PATTERNS {
        let (mut ctl, mut r) = setup(config(2));
        let settings = OutputSettings {
            delay_frames: 480,
            ..OutputSettings::default()
        };
        ctl.send(0, set_output(1, settings));
        let mut impulse = vec![0.0; 2 * 1000];
        impulse[0] = 1.0;
        impulse[1] = 1.0;
        start(&mut ctl, 1000, 1, source(&impulse), 2);
        let out = r.render(2000, blocks);
        let (a, b) = (channel(&out, 2, 0), channel(&out, 2, 1));
        assert_eq!(first_nonzero(&a), Some(1000));
        assert_eq!(a.iter().filter(|s| **s != 0.0).count(), 1);
        assert_eq!(first_nonzero(&b), Some(1480), "{blocks:?}");
        assert_eq!(b[1480], 1.0);
        assert_eq!(b.iter().filter(|s| **s != 0.0).count(), 1);
    }
}

#[test]
fn delay_is_capped_at_one_second() {
    let (mut ctl, mut r) = setup(config(1));
    let settings = OutputSettings {
        delay_frames: u32::MAX,
        ..OutputSettings::default()
    };
    ctl.send(0, set_output(0, settings));
    let mut impulse = vec![0.0; 100];
    impulse[0] = 1.0;
    start(&mut ctl, 1000, 1, source(&impulse), 1);
    let out = r.render(1000 + 48_000 + 10, &[512]);
    assert_eq!(first_nonzero(&out), Some(1000 + 48_000));
}

/// Review M3 regression: a second delay change during a crossfade must not jump.
#[test]
fn delay_retarget_mid_crossfade_is_continuous() {
    let (mut ctl, mut r) = setup(config(1));
    let sig: Vec<f32> = (0..20_000).map(|i| (i as f32 * 0.001).sin()).collect();
    start(&mut ctl, 0, 1, source(&sig), 1);
    let delay = |d| OutputSettings {
        delay_frames: d,
        ..OutputSettings::default()
    };
    ctl.send(2000, set_output(0, delay(4000)));
    ctl.send(2120, set_output(0, delay(8000)));
    ctl.send(2200, set_output(0, delay(0)));
    let out = r.render(10_000, &[256]);
    // Crossfading two full-scale taps over 240 frames moves at most 2/240 per frame, plus the
    // signal's own slope (0.001).
    assert!(
        max_step(&out) < 2.0 / 240.0 + 0.002,
        "step {}",
        max_step(&out)
    );
    // The last request wins once the crossfades finish.
    assert!((out[9000] - sig[9000]).abs() < 1e-6);
}

#[test]
fn global_master_and_per_output_master_levels() {
    let (mut ctl, mut r) = setup(config(2));
    ctl.send(0, CommandKind::SetMaster { gain: 0.25 });
    let half = OutputSettings {
        level: 0.5,
        ..OutputSettings::default()
    };
    ctl.send(0, set_output(1, half));
    start(&mut ctl, 1000, 1, source(&vec![1.0; 2 * 2000]), 2);
    let out = r.render(1100, &[64]);
    assert_eq!(&out[1050 * 2..1051 * 2], &[0.25, 0.125]);
}

#[test]
fn gang_moves_members_by_the_same_db_on_the_same_sample() {
    let (mut ctl, mut r) = setup(config(3));
    let mut v = voice(&ctl, 1, source(&vec![1.0; 3 * 4000]), 3);
    v.cue_outputs[1] = db_to_linear(-6.0);
    v.cue_outputs[2] = 0.0;
    send_start(&mut ctl, 0, v);
    let members = [
        (GainTarget::CueOutput(0), 0.0),
        (GainTarget::CueOutput(1), -6.0),
        (GainTarget::CueOutput(2), SILENCE_DB),
    ];
    ctl.ramp_gang(
        1000,
        VoiceId(1),
        &members,
        -6.0,
        Fade::new(100, Curve::Linear),
    )
    .unwrap();
    let out = r.render(1200, &[64]);
    // Both audible members start moving on sample 1001 (ramp step 1).
    assert_eq!(out[1000 * 3], 1.0);
    assert!(out[1001 * 3] < 1.0 && out[1001 * 3 + 1] < db_to_linear(-6.0));
    let end = &out[1150 * 3..1151 * 3];
    assert!((end[0] - db_to_linear(-6.0)).abs() < 1e-6);
    assert!((end[1] - db_to_linear(-12.0)).abs() < 1e-6);
    assert_eq!(end[2], 0.0);
}

#[test]
fn gang_is_released_whole_when_the_queue_is_nearly_full() {
    let (mut ctl, mut r) = setup(config(2));
    start(&mut ctl, 0, 1, source(&vec![1.0; 2 * 4000]), 2);
    for _ in 0..openlab_audio::COMMAND_CAPACITY - 2 {
        ctl.send(0, CommandKind::SetMaster { gain: 1.0 });
    }
    let members = [
        (GainTarget::CueOutput(0), 0.0),
        (GainTarget::CueOutput(1), 0.0),
    ];
    // One slot free: the two-command gang waits whole on the control thread.
    ctl.ramp_gang(0, VoiceId(1), &members, -6.0, Fade::new(64, Curve::Linear))
        .unwrap();
    assert_eq!(ctl.held(), 2);
    r.render(64, &[64]);
    assert_eq!(ctl.pump(), 2);
    let out = r.render(200, &[64]);
    // Released late (at the next block), but both members move together on the same sample.
    assert_eq!(&out[..2], &[1.0, 1.0]);
    assert_eq!(out[2], out[3]);
    assert!(out[2] < 1.0);
    assert!((out[150 * 2] - db_to_linear(-6.0)).abs() < 1e-6);
    let too_big: Vec<_> = (0..openlab_audio::COMMAND_CAPACITY + 1)
        .map(|_| (0, CommandKind::SetMaster { gain: 1.0 }))
        .collect();
    assert!(ctl.send_batch(too_big).is_err());
}

#[test]
fn far_future_commands_wait_on_the_control_thread_and_never_delay_a_stop() {
    let cfg = MixerConfig {
        lookahead_ms: 10,
        ..config(1)
    };
    let (mut ctl, mut r) = setup(cfg);
    start(&mut ctl, 0, 1, mono_dc(20_000), 1);
    for i in 0..5000u64 {
        ctl.send(10_000_000 + i, CommandKind::SetMaster { gain: 1.0 });
    }
    ctl.send(
        5000,
        ramp(1, GainTarget::Master, 0.5, Fade::new(1, Curve::Linear)),
    );
    assert_eq!(ctl.held(), 5001, "beyond the 480-frame lookahead");
    ctl.send(256, stop(1, Fade::NONE));
    // Released as the clock advances; the ramp lands on its exact sample.
    let mut out = Vec::new();
    for _ in 0..40 {
        ctl.pump();
        out.extend(r.render(128, &[128]));
    }
    assert!(out[..256].iter().all(|s| *s == 1.0));
    assert!(out[256..].iter().all(|s| *s == 0.0));
    assert_eq!(events(&mut ctl), [finished(1, 256, FinishReason::Stopped)]);
    assert_eq!(ctl.held(), 5000);
}

#[test]
fn held_commands_land_on_their_exact_sample_when_pumped() {
    let cfg = MixerConfig {
        lookahead_ms: 10,
        ..config(1)
    };
    let (mut ctl, mut r) = setup(cfg);
    start(&mut ctl, 0, 1, mono_dc(20_000), 1);
    ctl.send(
        5000,
        ramp(1, GainTarget::Master, 0.5, Fade::new(1, Curve::Linear)),
    );
    let mut out = Vec::new();
    for _ in 0..50 {
        ctl.pump();
        out.extend(r.render(128, &[128]));
    }
    assert_eq!(out[5000], 1.0);
    assert!(out[5001..].iter().all(|s| *s == 0.5));
}

/// Review 2 M4 (probe `order`): equal-`at` commands apply in send order even when one was held
/// and a later one travelled in an earlier-keyed batch.
#[test]
fn equal_time_commands_apply_in_send_order_across_batches_and_held_commands() {
    let cfg = MixerConfig {
        lookahead_ms: 10,
        ..config(1)
    };
    let (mut ctl, mut r) = setup(cfg);
    start(&mut ctl, 0, 1, mono_dc(20_000), 1);
    ctl.send(5000, CommandKind::SetMaster { gain: 0.5 });
    ctl.send_batch(vec![
        (0, CommandKind::SetMaster { gain: 1.0 }),
        (5000, CommandKind::SetMaster { gain: 0.2 }),
    ])
    .unwrap();
    let mut out = Vec::new();
    for _ in 0..40 {
        ctl.pump();
        out.extend(r.render(256, &[256]));
    }
    assert!((out[9000] - 0.2).abs() < 1e-6, "{}", out[9000]);
}

#[test]
fn instant_changes_are_dezippered() {
    let (mut ctl, mut r) = setup(config(1));
    start(&mut ctl, 0, 1, mono_dc(12_000), 1);
    let d = OutputSettings::default();
    let delayed = |o: OutputSettings| OutputSettings {
        delay_frames: 100,
        ..o
    };
    ctl.send(1000, ramp(1, GainTarget::Master, 0.0, Fade::NONE));
    ctl.send(2000, ramp(1, GainTarget::Master, 1.0, Fade::NONE));
    ctl.send(3000, set_output(0, OutputSettings { invert: true, ..d }));
    ctl.send(4000, set_output(0, d));
    ctl.send(4500, set_output(0, delayed(d)));
    ctl.send(
        5000,
        set_output(0, delayed(OutputSettings { mute: true, ..d })),
    );
    ctl.send(6000, set_output(0, delayed(d)));
    ctl.send(7000, CommandKind::SetMaster { gain: 0.1 });
    ctl.send(8000, CommandKind::SetMaster { gain: 1.0 });
    ctl.send(9000, patch(0, 0, 0, 0.0));
    let out = r.render(10_000, &[100, 37]);
    // 5 ms at 48 kHz = 240 samples; a polarity flip covers 2.0 in that time.
    assert!(
        max_step(&out) <= 2.0 / 240.0 + 1e-6,
        "step {}",
        max_step(&out)
    );
    assert_eq!(out[1240], 0.0);
    assert_eq!(out[2240], 1.0);
    assert_eq!(out[3240], -1.0);
    assert_eq!(out[5240], 0.0, "mute acts after the delay, so immediately");
    assert!((out[7240] - 0.1).abs() < 1e-6);
    assert_eq!(
        out[9340], 0.0,
        "patch routing is before the 100-frame delay"
    );
}

#[test]
fn late_and_out_of_order_commands() {
    let (mut ctl, mut r) = setup(config(1));
    // A future ramp queued before the start it applies to.
    ctl.send(
        2000,
        ramp(1, GainTarget::Master, 0.5, Fade::new(1, Curve::Linear)),
    );
    start(&mut ctl, 1000, 1, mono_dc(4000), 1);
    let out = r.render(2100, &[256]);
    assert_eq!(first_nonzero(&out), Some(1000));
    assert_eq!(out[2001], 0.5);

    // A command for a time already rendered applies at the start of the next block.
    start(&mut ctl, 0, 2, mono_dc(4000), 1);
    let out = r.render(100, &[50]);
    assert_eq!(out[0], 0.5 + 1.0);
}

#[test]
fn nan_gain_is_silence() {
    let (mut ctl, mut r) = setup(config(1));
    start(&mut ctl, 0, 1, mono_dc(4000), 1);
    ctl.send(0, ramp(1, GainTarget::Master, f32::NAN, Fade::NONE));
    assert!(r.render(1000, &[64]).iter().all(|s| *s == 0.0));
}

/// Review M4: every gain is capped at +24 dB, at both ends of the range.
#[test]
fn gains_are_capped_at_the_ceiling() {
    let (mut ctl, mut r) = setup(config(3));
    let mut v = voice(&ctl, 1, source(&vec![0.01; 3 * 4000]), 3);
    v.set_crosspoint(2, 2, -1e10);
    send_start(&mut ctl, 0, v);
    ctl.ramp_gang(
        0,
        VoiceId(1),
        &[(GainTarget::CueOutput(0), 0.0)],
        200.0,
        Fade::NONE,
    )
    .unwrap();
    ctl.send(
        0,
        ramp(1, GainTarget::CueOutput(1), f32::INFINITY, Fade::NONE),
    );
    let out = r.render(100, &[64]);
    let frame = &out[50 * 3..51 * 3];
    for (got, want) in frame
        .iter()
        .zip([0.01 * MAX_GAIN, 0.01 * MAX_GAIN, -0.01 * MAX_GAIN])
    {
        assert!((got - want).abs() < 1e-6, "{frame:?}");
    }

    // Review 2 M1: a NaN level or offset in a gang move is silence, not the ceiling.
    let (mut ctl, mut r) = setup(config(2));
    let v = voice(&ctl, 1, source(&vec![0.01; 2 * 4000]), 2);
    send_start(&mut ctl, 0, v);
    ctl.ramp_gang(
        0,
        VoiceId(1),
        &[(GainTarget::CueOutput(0), f64::NAN)],
        0.0,
        Fade::NONE,
    )
    .unwrap();
    ctl.ramp_gang(
        0,
        VoiceId(1),
        &[(GainTarget::CueOutput(1), 0.0)],
        f64::NAN,
        Fade::NONE,
    )
    .unwrap();
    let out = r.render(100, &[64]);
    assert!(out.iter().all(|s| *s == 0.0), "{:?}", &out[..4]);

    let (mut ctl, mut r) = setup(config(1));
    start(&mut ctl, 0, 1, source(&vec![0.01; 4000]), 1);
    ctl.send(0, CommandKind::SetMaster { gain: 1e10 });
    let out = r.render(1000, &[64]);
    assert!((out[900] - 0.01 * MAX_GAIN).abs() < 1e-6);
}

#[test]
fn clock_and_short_buffers() {
    let (ctl, mut r) = setup(config(2));
    let mut buf = vec![0.0; 20];
    r.mixer_mut().process(&mut buf, 100);
    assert_eq!(ctl.samples_rendered(), 10);
    r.render(1000, &[3, 0]);
    assert_eq!(ctl.samples_rendered(), 1010);
    assert_eq!(r.mixer_mut().now(), 1010);
}

/// A scripted show: voices with fades/trims/patches, ramps, stops and output/patch changes.
#[derive(Clone, Debug)]
struct Script {
    starts: Vec<(u64, u64, u64, Option<u64>, u64, usize)>,
    ramps: Vec<(u64, u64, u8, f32, u64, usize)>,
    stops: Vec<(u64, u64, u64, usize)>,
    outputs: Vec<(u64, u16, f32, bool, bool, u32)>,
    patches: Vec<(u64, u16, u16, u16, f32)>,
    masters: Vec<(u64, f32)>,
}

fn script() -> impl Strategy<Value = Script> {
    let start = (
        0u64..3000,
        0u64..300,
        0u64..200,
        prop::option::of(1u64..2500),
        0u64..400,
        0usize..5,
    );
    let ramp = (0u64..4000, 0u64..4, 0u8..5, 0f32..1.5, 0u64..800, 0usize..5);
    let stop = (0u64..4000, 0u64..4, 0u64..500, 0usize..5);
    let output = (
        0u64..4000,
        0u16..2,
        0f32..1.5,
        any::<bool>(),
        any::<bool>(),
        0u32..300,
    );
    let patch = (0u64..4000, 0u16..2, 0u16..2, 0u16..2, 0f32..1.5);
    (
        prop::collection::vec(start, 1..5),
        prop::collection::vec(ramp, 0..12),
        prop::collection::vec(stop, 0..3),
        prop::collection::vec(output, 0..4),
        prop::collection::vec(patch, 0..4),
        prop::collection::vec((0u64..4000, 0f32..1.5), 0..3),
    )
        .prop_map(|(starts, ramps, stops, outputs, patches, masters)| Script {
            starts,
            ramps,
            stops,
            outputs,
            patches,
            masters,
        })
}

fn render_script(s: &Script, blocks: &[usize]) -> Vec<f32> {
    let (mut ctl, mut r) = setup(config(2));
    for (id, &(at, skip, fade_in, play, fade_out, curve)) in s.starts.iter().enumerate() {
        let data: Vec<f32> = (0..3000u32)
            .flat_map(|i| {
                [
                    (i as f32 * 0.01).sin(),
                    (i as f32 * 0.003 + id as f32).cos(),
                ]
            })
            .collect();
        let mut v = voice(&ctl, id as u64, source(&data), 2);
        v.patch = (id % 2) as u16;
        v.skip_frames = skip;
        v.fade_in = Fade::new(fade_in, CURVES[curve]);
        v.play_frames = play;
        if play.is_some() {
            v.fade_out = Fade::new(fade_out, CURVES[(curve + 1) % 5]);
        }
        send_start(&mut ctl, at, v);
    }
    for &(at, voice, target, gain, frames, curve) in &s.ramps {
        let target = match target {
            0 => GainTarget::Master,
            1 => GainTarget::CueOutput(1),
            2 => GainTarget::Crosspoint {
                input: 0,
                output: 1,
            },
            3 => GainTarget::Crosspoint {
                input: 1,
                output: 1,
            },
            _ => GainTarget::CueOutput(0),
        };
        ctl.send(
            at,
            ramp(voice, target, gain, Fade::new(frames, CURVES[curve])),
        );
    }
    for &(at, voice, frames, curve) in &s.stops {
        ctl.send(at, stop(voice, Fade::new(frames, CURVES[curve])));
    }
    for &(at, output, level, mute, invert, delay_frames) in &s.outputs {
        let settings = OutputSettings {
            level,
            mute,
            invert,
            delay_frames,
        };
        ctl.send(at, set_output(output, settings));
    }
    for &(at, p, o, d, gain) in &s.patches {
        ctl.send(at, patch(p, o, d, gain));
    }
    for &(at, gain) in &s.masters {
        ctl.send(at, CommandKind::SetMaster { gain });
    }
    let mut out = r.render(4500, blocks);
    // Events must also match. Within one block they come in voice-slot order, so sort by time.
    let mut done: Vec<(u64, u64)> = events(&mut ctl)
        .into_iter()
        .filter_map(|e| match e {
            Event::VoiceFinished { voice, at, .. } => Some((at, voice.0)),
            _ => None,
        })
        .collect();
    done.sort_unstable();
    out.extend(done.into_iter().flat_map(|(at, v)| [at as f32, v as f32]));
    out
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn output_is_independent_of_block_sizes(s in script(), blocks in prop::collection::vec(1usize..700, 1..8)) {
        let reference = render_script(&s, &[4500]);
        let varied = render_script(&s, &blocks);
        prop_assert!(reference == varied, "block pattern {:?} changed the output", blocks);
    }
}

/// Ring wraps that split a frame (a capacity that is not a whole number of frames) and ones that
/// do not both deliver every channel's samples in order.
#[test]
fn rings_wrapping_mid_frame_deliver_every_channel_in_order() {
    const CH: usize = 3;
    const FRAMES: usize = 600;
    let data: Vec<f32> = (0..FRAMES * CH)
        .map(|i| (i / CH) as f32 * 1e-3 + (i % CH) as f32 * 0.25)
        .collect();
    for capacity in [7, 8, 10, 12, 31, 3000] {
        let (mut ctl, mut r) = setup(config(CH));
        let (mut tx, rx) = rtrb::RingBuffer::new(capacity);
        start(&mut ctl, 0, 1, rx, CH as u16);
        let mut fed = 0;
        let mut out = Vec::new();
        while out.len() < FRAMES * CH {
            let n = tx.slots().min(data.len() - fed);
            tx.push_entire_slice(&data[fed..fed + n]).unwrap();
            fed += n;
            // Fewer frames than the ring holds, so reads start at every offset in it.
            let frames = (capacity / CH).clamp(1, 3);
            out.extend(r.render(frames, &[frames]));
        }
        assert_eq!(ctl.underrun_frames(), 0, "capacity {capacity}");
        for (i, (got, want)) in out.iter().zip(&data).enumerate() {
            assert_eq!(
                got.to_bits(),
                want.to_bits(),
                "capacity {capacity} sample {i}"
            );
        }
    }
}
