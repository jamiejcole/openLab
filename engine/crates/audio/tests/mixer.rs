// SPDX-License-Identifier: GPL-3.0-or-later

//! Sample-level behaviour of the mixer through the offline renderer: timing, fades, trims,
//! routing, levels and block-size invariance.

mod common;

use common::*;
use openlab_audio::{
    CommandKind, Curve, Event, Fade, FinishReason, GainTarget, OutputSettings, SILENCE_DB, VoiceId,
    VoiceStart, db_to_linear, ramp_value,
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

fn mono_dc(frames: usize) -> rtrb::Consumer<f32> {
    source(&vec![1.0; frames])
}

#[test]
fn simultaneous_starts_land_on_the_same_frame() {
    for &blocks in PATTERNS {
        for t in [0u64, 1000, 1001] {
            let (mut ctl, mut r) = setup(config(2));
            start(&mut ctl, t, 1, mono_dc(4000), 1);
            start(&mut ctl, t, 2, mono_dc(4000), 1);
            // Voice 2 to output 1 only; at its start sample this applies with no de-zipper.
            let xp = |output| GainTarget::Crosspoint { input: 0, output };
            ctl.send(t, ramp(2, xp(0), 0.0, Fade::NONE)).unwrap();
            ctl.send(t, ramp(2, xp(1), 1.0, Fade::NONE)).unwrap();
            let out = r.render(3000, blocks);
            let (a, b) = (channel(&out, 2, 0), channel(&out, 2, 1));
            assert_eq!(first_nonzero(&a), Some(t as usize), "{blocks:?} t={t}");
            assert_eq!(first_nonzero(&b), Some(t as usize), "{blocks:?} t={t}");
            assert_eq!(a[t as usize], 1.0);
            assert_eq!(b[t as usize], 1.0);
        }
    }
}

#[test]
fn immediate_stop_cuts_on_the_exact_sample() {
    for &blocks in PATTERNS {
        let (mut ctl, mut r) = setup(config(1));
        start(&mut ctl, 10, 1, mono_dc(4000), 1);
        ctl.send(
            777,
            CommandKind::StopVoice {
                voice: VoiceId(1),
                fade: Fade::NONE,
            },
        )
        .unwrap();
        let out = r.render(1000, blocks);
        assert!(out[10..777].iter().all(|s| *s == 1.0), "{blocks:?}");
        assert!(out[777..].iter().all(|s| *s == 0.0), "{blocks:?}");
        let ev = events(&mut ctl);
        assert_eq!(
            ev,
            [Event::VoiceFinished {
                voice: VoiceId(1),
                at: 777,
                reason: FinishReason::Stopped
            }]
        );
    }
}

#[test]
fn stop_with_fade_ends_at_zero_and_finishes() {
    let (mut ctl, mut r) = setup(config(1));
    start(&mut ctl, 0, 1, mono_dc(4000), 1);
    ctl.send(
        500,
        CommandKind::StopVoice {
            voice: VoiceId(1),
            fade: Fade::new(100, Curve::Linear),
        },
    )
    .unwrap();
    let out = r.render(1000, &[64]);
    for k in 0..=100 {
        assert_eq!(
            out[500 + k],
            ramp_value(1.0, 0.0, k as u64, 100, Curve::Linear),
            "k={k}"
        );
    }
    assert!(out[600..].iter().all(|s| *s == 0.0));
    let ev = events(&mut ctl);
    assert_eq!(
        ev,
        [Event::VoiceFinished {
            voice: VoiceId(1),
            at: 600,
            reason: FinishReason::Stopped
        }]
    );
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
        )
        .unwrap();
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
        let mut v = VoiceStart::new(VoiceId(1), mono_dc(4000), 1);
        v.gain = 0.0;
        ctl.send(0, CommandKind::StartVoice(v)).unwrap();
        ctl.send(
            1000,
            ramp(1, GainTarget::Master, 1.0, Fade::new(1000, curve)),
        )
        .unwrap();
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
            let mut v = VoiceStart::new(VoiceId(1), mono_dc(4000), 1);
            v.fade_in = Fade::new(100, curve);
            ctl.send(300, CommandKind::StartVoice(v)).unwrap();
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

#[test]
fn integrated_fade_out_ends_at_exact_zero_on_last_frame() {
    for curve in CURVES {
        for &blocks in PATTERNS {
            let (mut ctl, mut r) = setup(config(1));
            let mut v = VoiceStart::new(VoiceId(1), mono_dc(4000), 1);
            v.play_frames = Some(1000);
            v.fade_out = Fade::new(100, curve);
            ctl.send(300, CommandKind::StartVoice(v)).unwrap();
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
            let ev = events(&mut ctl);
            assert_eq!(
                ev,
                [Event::VoiceFinished {
                    voice: VoiceId(1),
                    at: 1300,
                    reason: FinishReason::OutPoint
                }]
            );
        }
    }
}

#[test]
fn trims_play_exact_in_and_out_points() {
    let ramp_src: Vec<f32> = (0..1000).map(|i| i as f32).collect();
    for &blocks in PATTERNS {
        let (mut ctl, mut r) = setup(config(1));
        let mut v = VoiceStart::new(VoiceId(1), source(&ramp_src), 1);
        v.skip_frames = 100;
        v.play_frames = Some(50);
        ctl.send(20, CommandKind::StartVoice(v)).unwrap();
        let out = r.render(200, blocks);
        assert!(out[..20].iter().all(|s| *s == 0.0));
        for k in 0..50 {
            assert_eq!(out[20 + k], (100 + k) as f32, "{blocks:?}");
        }
        assert!(out[70..].iter().all(|s| *s == 0.0));
        let ev = events(&mut ctl);
        assert_eq!(
            ev,
            [Event::VoiceFinished {
                voice: VoiceId(1),
                at: 70,
                reason: FinishReason::OutPoint
            }]
        );
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
        let ev = events(&mut ctl);
        assert_eq!(
            ev,
            [Event::VoiceFinished {
                voice: VoiceId(1),
                at: 505,
                reason: FinishReason::EndOfStream
            }]
        );
        assert_eq!(ctl.collect_returns(), 1);
    }
}

#[test]
fn crosspoints_cue_outputs_and_voice_master() {
    let (mut ctl, mut r) = setup(config(3));
    let stereo: Vec<f32> = (0..2000).flat_map(|_| [1.0, 0.5]).collect();
    let mut v = VoiceStart::new(VoiceId(1), source(&stereo), 2);
    v.gain = 0.8;
    ctl.send(0, CommandKind::StartVoice(v)).unwrap();
    let xp = |input, output| GainTarget::Crosspoint { input, output };
    ctl.send(0, ramp(1, xp(0, 0), 0.5, Fade::NONE)).unwrap();
    ctl.send(0, ramp(1, xp(0, 1), 0.25, Fade::NONE)).unwrap();
    ctl.send(0, ramp(1, xp(1, 2), 1.0, Fade::NONE)).unwrap();
    ctl.send(0, ramp(1, GainTarget::CueOutput(2), 0.5, Fade::NONE))
        .unwrap();
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
}

#[test]
fn patch_routing_maps_cue_outputs_to_device_outputs() {
    let (mut ctl, mut r) = setup(config(2));
    ctl.send(
        0,
        CommandKind::SetPatchRouting {
            cue_output: 0,
            device_output: 0,
            gain: 0.0,
        },
    )
    .unwrap();
    ctl.send(
        0,
        CommandKind::SetPatchRouting {
            cue_output: 0,
            device_output: 1,
            gain: 0.5,
        },
    )
    .unwrap();
    start(&mut ctl, 1000, 1, mono_dc(2000), 1);
    let out = r.render(1100, &[64]);
    assert_eq!(&out[1050 * 2..1051 * 2], &[0.0, 0.5]);
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
    ctl.send(
        0,
        CommandKind::SetOutput {
            output: 0,
            settings: set(0.5, false, false),
        },
    )
    .unwrap();
    ctl.send(
        0,
        CommandKind::SetOutput {
            output: 1,
            settings: set(1.0, false, true),
        },
    )
    .unwrap();
    ctl.send(
        0,
        CommandKind::SetOutput {
            output: 2,
            settings: set(1.0, true, false),
        },
    )
    .unwrap();
    ctl.send(
        0,
        CommandKind::SetOutput {
            output: 3,
            settings: set(db_to_linear(-6.0), false, true),
        },
    )
    .unwrap();
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
        ctl.send(
            0,
            CommandKind::SetOutput {
                output: 1,
                settings,
            },
        )
        .unwrap();
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
    ctl.send(
        0,
        CommandKind::SetOutput {
            output: 0,
            settings,
        },
    )
    .unwrap();
    let mut impulse = vec![0.0; 100];
    impulse[0] = 1.0;
    start(&mut ctl, 1000, 1, source(&impulse), 1);
    let out = r.render(1000 + 48_000 + 10, &[512]);
    assert_eq!(first_nonzero(&out), Some(1000 + 48_000));
}

#[test]
fn global_master_and_per_output_master_levels() {
    let (mut ctl, mut r) = setup(config(2));
    ctl.send(0, CommandKind::SetMaster { gain: 0.25 }).unwrap();
    let half = OutputSettings {
        level: 0.5,
        ..OutputSettings::default()
    };
    ctl.send(
        0,
        CommandKind::SetOutput {
            output: 1,
            settings: half,
        },
    )
    .unwrap();
    start(&mut ctl, 1000, 1, source(&vec![1.0; 2 * 2000]), 2);
    let out = r.render(1100, &[64]);
    assert_eq!(&out[1050 * 2..1051 * 2], &[0.25, 0.125]);
}

#[test]
fn gang_moves_members_by_the_same_db_on_the_same_sample() {
    let (mut ctl, mut r) = setup(config(3));
    let mut v = VoiceStart::new(VoiceId(1), source(&vec![1.0; 3 * 4000]), 3);
    v.gain = 1.0;
    ctl.send(0, CommandKind::StartVoice(v)).unwrap();
    ctl.send(
        0,
        ramp(1, GainTarget::CueOutput(1), db_to_linear(-6.0), Fade::NONE),
    )
    .unwrap();
    ctl.send(0, ramp(1, GainTarget::CueOutput(2), 0.0, Fade::NONE))
        .unwrap();
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
fn gang_is_all_or_nothing_when_the_queue_is_nearly_full() {
    let (mut ctl, _r) = setup(config(2));
    for _ in 0..openlab_audio::COMMAND_CAPACITY - 1 {
        ctl.send(0, CommandKind::SetMaster { gain: 1.0 }).unwrap();
    }
    let members = [
        (GainTarget::CueOutput(0), 0.0),
        (GainTarget::CueOutput(1), 0.0),
    ];
    assert!(
        ctl.ramp_gang(0, VoiceId(1), &members, -3.0, Fade::NONE)
            .is_err()
    );
    // The gang took no slot: exactly one is still free.
    ctl.send(0, CommandKind::SetMaster { gain: 1.0 }).unwrap();
    assert!(ctl.send(0, CommandKind::SetMaster { gain: 1.0 }).is_err());
}

#[test]
fn instant_changes_are_dezippered() {
    let (mut ctl, mut r) = setup(config(1));
    start(&mut ctl, 0, 1, mono_dc(12_000), 1);
    let out0 = |s: OutputSettings| CommandKind::SetOutput {
        output: 0,
        settings: s,
    };
    let d = OutputSettings::default();
    ctl.send(1000, ramp(1, GainTarget::Master, 0.0, Fade::NONE))
        .unwrap();
    ctl.send(2000, ramp(1, GainTarget::Master, 1.0, Fade::NONE))
        .unwrap();
    ctl.send(3000, out0(OutputSettings { invert: true, ..d }))
        .unwrap();
    ctl.send(4000, out0(d)).unwrap();
    ctl.send(
        4500,
        out0(OutputSettings {
            delay_frames: 100,
            ..d
        }),
    )
    .unwrap();
    ctl.send(
        5000,
        out0(OutputSettings {
            mute: true,
            delay_frames: 100,
            ..d
        }),
    )
    .unwrap();
    ctl.send(
        6000,
        out0(OutputSettings {
            delay_frames: 100,
            ..d
        }),
    )
    .unwrap();
    ctl.send(7000, CommandKind::SetMaster { gain: 0.1 })
        .unwrap();
    ctl.send(8000, CommandKind::SetMaster { gain: 1.0 })
        .unwrap();
    ctl.send(
        9000,
        CommandKind::SetPatchRouting {
            cue_output: 0,
            device_output: 0,
            gain: 0.0,
        },
    )
    .unwrap();
    let out = r.render(10_000, &[100, 37]);
    // 5 ms at 48 kHz = 240 samples; a polarity flip covers 2.0 in that time.
    let max_step = out
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    assert!(max_step <= 2.0 / 240.0 + 1e-6, "step {max_step}");
    assert_eq!(out[1240], 0.0);
    assert_eq!(out[2240], 1.0);
    assert_eq!(out[3240], -1.0);
    assert_eq!(out[5340], 0.0, "mute is applied before the 100-frame delay");
    assert!((out[7240] - 0.1).abs() < 1e-6);
    assert_eq!(out[9340], 0.0);
}

#[test]
fn late_and_out_of_order_commands() {
    let (mut ctl, mut r) = setup(config(1));
    // A future ramp queued before the start it applies to.
    ctl.send(
        2000,
        ramp(1, GainTarget::Master, 0.5, Fade::new(1, Curve::Linear)),
    )
    .unwrap();
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
fn non_finite_gains_are_silence() {
    let (mut ctl, mut r) = setup(config(1));
    start(&mut ctl, 0, 1, mono_dc(4000), 1);
    ctl.send(0, ramp(1, GainTarget::Master, f32::NAN, Fade::NONE))
        .unwrap();
    ctl.send(
        0,
        CommandKind::SetMaster {
            gain: f32::INFINITY,
        },
    )
    .unwrap();
    let out = r.render(1000, &[64]);
    assert!(out.iter().all(|s| *s == 0.0));
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

/// A scripted show: voices with fades/trims, ramps, stops and output changes.
#[derive(Clone, Debug)]
struct Script {
    starts: Vec<(u64, u64, u64, Option<u64>, u64, usize)>,
    ramps: Vec<(u64, u64, u8, f32, u64, usize)>,
    stops: Vec<(u64, u64, u64, usize)>,
    outputs: Vec<(u64, u16, f32, bool, bool, u32)>,
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
    (
        prop::collection::vec(start, 1..5),
        prop::collection::vec(ramp, 0..12),
        prop::collection::vec(stop, 0..3),
        prop::collection::vec(output, 0..4),
        prop::collection::vec((0u64..4000, 0f32..1.5), 0..3),
    )
        .prop_map(|(starts, ramps, stops, outputs, masters)| Script {
            starts,
            ramps,
            stops,
            outputs,
            masters,
        })
}

#[allow(clippy::unwrap_used)] // test helper outside #[test]
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
        let mut v = VoiceStart::new(VoiceId(id as u64), source(&data), 2);
        v.skip_frames = skip;
        v.fade_in = Fade::new(fade_in, CURVES[curve]);
        v.play_frames = play;
        v.fade_out = Fade::new(fade_out, CURVES[(curve + 1) % 5]);
        ctl.send(at, CommandKind::StartVoice(v)).unwrap();
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
        )
        .unwrap();
    }
    for &(at, voice, frames, curve) in &s.stops {
        ctl.send(
            at,
            CommandKind::StopVoice {
                voice: VoiceId(voice),
                fade: Fade::new(frames, CURVES[curve]),
            },
        )
        .unwrap();
    }
    for &(at, output, level, mute, invert, delay_frames) in &s.outputs {
        ctl.send(
            at,
            CommandKind::SetOutput {
                output,
                settings: OutputSettings {
                    level,
                    mute,
                    invert,
                    delay_frames,
                },
            },
        )
        .unwrap();
    }
    for &(at, gain) in &s.masters {
        ctl.send(at, CommandKind::SetMaster { gain }).unwrap();
    }
    let out = r.render(4500, blocks);
    // Events must also match: append them as a fingerprint.
    // Events must also match. Within one block they come in voice-slot order, so sort by time.
    let mut finished: Vec<(u64, u64)> = events(&mut ctl)
        .into_iter()
        .filter_map(|e| match e {
            Event::VoiceFinished { voice, at, .. } => Some((at, voice.0)),
            _ => None,
        })
        .collect();
    finished.sort_unstable();
    let mut out = out;
    out.extend(
        finished
            .into_iter()
            .flat_map(|(at, v)| [at as f32, v as f32]),
    );
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
