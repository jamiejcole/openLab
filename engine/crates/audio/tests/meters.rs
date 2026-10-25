// SPDX-License-Identifier: GPL-3.0-or-later
// Helpers outside #[test] fns are test code too; clippy's in-test allowances miss them.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Peak meters (AUD-19) and the xrun counter (NFR-27): exact peaks at the device outputs and per
//! voice cue output, read-and-reset semantics across arbitrary block splits, voice attribution
//! through slot reuse, pause and re-keying, and non-finite input. Offline renders only.

mod common;

use common::*;
use openlab_audio::{
    CommandKind, Curve, Fade, GainTarget, MixerConfig, MixerController, OfflineRenderer,
    OutputSettings, VoiceId,
};

/// Past every de-zipper ramp (5 ms at 48 kHz = 240 frames).
const SETTLE: usize = 1000;

fn render(r: &mut OfflineRenderer, frames: usize, blocks: &[usize]) -> Vec<f32> {
    let ch = r.mixer_mut().output_channels();
    let mut out = vec![0.0; frames * ch];
    no_alloc(|| r.render_into(&mut out, blocks));
    out
}

fn dc(value: f32, frames: usize) -> Vec<f32> {
    vec![value; frames]
}

/// Interleaves equal-length channels.
fn interleave(chs: &[Vec<f32>]) -> Vec<f32> {
    (0..chs[0].len())
        .flat_map(|f| chs.iter().map(move |c| c[f]))
        .collect()
}

fn voice_peaks(ctl: &mut MixerController) -> Vec<(u64, Vec<f32>)> {
    let mut v: Vec<_> = ctl
        .voice_meters()
        .into_iter()
        .map(|(id, p)| (id.0, p))
        .collect();
    v.sort_by_key(|e| e.0);
    v
}

fn max_abs(xs: &[f32]) -> f32 {
    xs.iter().fold(0.0, |m, x| m.max(x.abs()))
}

#[test]
fn output_meters_show_exact_post_master_peaks() {
    let (mut ctl, mut r) = setup(config(4));
    // A: 0.5 on cue output 0 (identity → device 0, plus cue 0 → device 3, which is muted).
    // B: -0.25 on cue output 1, patched to device 2 at 0.5 instead of device 1.
    ctl.send(
        0,
        CommandKind::SetPatchRouting {
            patch: 0,
            cue_output: 1,
            device_output: 1,
            gain: 0.0,
        },
    );
    ctl.send(
        0,
        CommandKind::SetPatchRouting {
            patch: 0,
            cue_output: 1,
            device_output: 2,
            gain: 0.5,
        },
    );
    ctl.send(
        0,
        CommandKind::SetPatchRouting {
            patch: 0,
            cue_output: 0,
            device_output: 3,
            gain: 1.0,
        },
    );
    let out = |level, mute, invert| OutputSettings {
        level,
        mute,
        invert,
        delay_frames: 0,
    };
    ctl.send(
        0,
        CommandKind::SetOutput {
            output: 2,
            settings: out(0.5, false, true),
        },
    );
    ctl.send(
        0,
        CommandKind::SetOutput {
            output: 3,
            settings: out(1.0, true, false),
        },
    );
    ctl.send(0, CommandKind::SetMaster { gain: 0.5 });
    render(&mut r, SETTLE, &[]);
    assert_eq!(ctl.output_meters(), vec![0.0; 4], "nothing playing yet");

    let n = SETTLE as u64;
    start(&mut ctl, n, 1, source(&dc(0.5, 20_000)), 1);
    let mut b = voice(&ctl, 2, source(&dc(-0.25, 20_000)), 1);
    b.set_crosspoint(0, 0, 0.0);
    b.set_crosspoint(0, 1, 1.0);
    send_start(&mut ctl, n, b);
    let rendered = render(&mut r, 2000, &[64, 1, 333]);
    assert_eq!(ctl.output_meters(), vec![0.25, 0.0, 0.0625 * 0.5, 0.0]);
    // The meter shows what went to the device.
    for d in 0..4 {
        let peak = max_abs(&channel(&rendered, 4, d));
        assert_eq!(peak, [0.25, 0.0, 0.03125, 0.0][d]);
    }
    // A read resets; reads with no block in between see nothing.
    assert_eq!(ctl.output_meters(), vec![0.0; 4]);
    let mut buf = [9.0; 4];
    assert_eq!(ctl.read_output_meters_into(&mut buf), 4);
    assert_eq!(buf, [0.0; 4]);
    // A short buffer reads only what fits; the others keep their peaks.
    render(&mut r, 10, &[]);
    let mut two = [0.0; 2];
    assert_eq!(ctl.read_output_meters_into(&mut two), 2);
    assert_eq!(two, [0.25, 0.0]);
    assert_eq!(ctl.output_meters(), vec![0.0, 0.0, 0.03125, 0.0]);
}

#[test]
fn a_peak_between_reads_is_never_lost_whatever_the_block_split() {
    let spike_at = 777;
    let mut data = dc(0.125, 3000);
    data[spike_at] = -0.875;
    for blocks in [
        &[1usize][..],
        &[7, 64, 13],
        &[256],
        &[1000, 3],
        &[333, 1, 2],
    ] {
        for read_every in [1usize, 5, 50, 400] {
            let (mut ctl, mut r) = setup(config(2));
            start(&mut ctl, 0, 1, source(&data), 1);
            let (mut reads, mut voice_reads) = (Vec::new(), Vec::new());
            let mut done = 0;
            while done < 3000 {
                let n = read_every.min(3000 - done);
                render(&mut r, n, blocks);
                done += n;
                reads.push(ctl.output_meters()[0]);
                voice_reads.push(ctl.voice_meters().first().map_or(0.0, |e| e.1[0]));
            }
            for got in [&reads, &voice_reads] {
                let spikes: Vec<_> = got.iter().filter(|&&p| p == 0.875).collect();
                assert_eq!(spikes.len(), 1, "blocks {blocks:?} every {read_every}");
                assert!(got.iter().all(|&p| p == 0.875 || p == 0.125));
            }
        }
    }
}

#[test]
fn voice_meters_show_each_cue_output_post_fader_pre_patch() {
    let (mut ctl, mut r) = setup(config(2));
    // The patch, output level and master do not reach the voice meter.
    ctl.send(
        0,
        CommandKind::SetPatchRouting {
            patch: 0,
            cue_output: 0,
            device_output: 0,
            gain: 0.25,
        },
    );
    ctl.send(0, CommandKind::SetMaster { gain: 0.5 });
    render(&mut r, SETTLE, &[]);
    let n = SETTLE as u64;
    let stereo = interleave(&[dc(0.5, 20_000), dc(-0.25, 20_000)]);
    let mut v = voice(&ctl, 7, source(&stereo), 2);
    v.gain = 0.5;
    v.cue_outputs[1] = 0.5;
    v.set_crosspoint(1, 1, 0.5);
    // Input 0 also feeds cue output 1, at 0.25: cue output 1 sums both inputs.
    v.set_crosspoint(0, 1, 0.25);
    send_start(&mut ctl, n, v);
    start(&mut ctl, n, 8, source(&dc(0.75, 20_000)), 1);
    render(&mut r, 500, &[100, 3]);
    // Voice 7: out 0 = 0.5 × 1 × 1 × 0.5; out 1 = (0.5 × 0.25 − 0.25 × 0.5) × 0.5 × 0.5 = 0.
    // Voice 8: out 0 = 0.75.
    assert_eq!(
        voice_peaks(&mut ctl),
        vec![(7, vec![0.25, 0.0]), (8, vec![0.75, 0.0])]
    );
    // Cue output 1 of voice 7 now only carries input 1: −0.25 × 0.5 × 0.5 × 0.5.
    ctl.send(
        r.mixer_mut().now(),
        CommandKind::Ramp {
            voice: VoiceId(7),
            target: GainTarget::Crosspoint {
                input: 0,
                output: 1,
            },
            gain: 0.0,
            fade: Fade::new(10, Curve::Linear),
        },
    );
    render(&mut r, 500, &[64]);
    ctl.voice_meters();
    render(&mut r, 500, &[64]);
    assert_eq!(
        voice_peaks(&mut ctl),
        vec![(7, vec![0.25, 0.03125]), (8, vec![0.75, 0.0])]
    );
    // Device 0 carries both voices through the patch (0.25) and master (0.5).
    assert_eq!(ctl.output_meters()[0], (0.25 + 0.75) * 0.25 * 0.5);
}

#[test]
fn voice_meters_follow_fades_and_ramps_sample_exactly() {
    let (mut ctl, mut r) = setup(config(1));
    let data: Vec<f32> = (0..6000).map(|i| ((i % 17) as f32 - 8.0) / 9.0).collect();
    let mut v = voice(&ctl, 3, source(&data), 1);
    v.fade_in = Fade::new(1500, Curve::EqualPower);
    v.play_frames = Some(5000);
    v.fade_out = Fade::new(1200, Curve::SCurve);
    send_start(&mut ctl, 0, v);
    ctl.send(
        2000,
        CommandKind::Ramp {
            voice: VoiceId(3),
            target: GainTarget::Master,
            gain: 0.3,
            fade: Fade::new(900, Curve::Exponential),
        },
    );
    let mut done = 0;
    for chunk in [37usize, 400, 1, 999, 64, 1500, 2000, 999] {
        let out = render(&mut r, chunk, &[128, 7]);
        done += chunk;
        let want = max_abs(&out);
        let got = voice_peaks(&mut ctl);
        if want > 0.0 {
            // One voice, unity patch and master: the device output is the voice's contribution.
            assert_eq!(got, vec![(3, vec![want])], "chunk ending at {done}");
        } else {
            assert!(got.iter().all(|e| e.1 == vec![0.0]));
        }
    }
}

#[test]
fn an_ended_voice_reports_its_last_peak_once() {
    let (mut ctl, mut r) = setup(config(1));
    let mut data = dc(0.25, 300);
    data[250] = 0.5;
    start(&mut ctl, 0, 4, source(&data), 1);
    render(&mut r, 100, &[]);
    assert_eq!(voice_peaks(&mut ctl), vec![(4, vec![0.25])]);
    render(&mut r, 400, &[]);
    assert_eq!(
        voice_peaks(&mut ctl),
        vec![(4, vec![0.5])],
        "its final peak"
    );
    assert_eq!(voice_peaks(&mut ctl), vec![], "and only once");
    render(&mut r, 100, &[]);
    assert_eq!(voice_peaks(&mut ctl), vec![]);
}

#[test]
fn a_reused_slot_never_shows_the_old_voice_peak_under_the_new_id() {
    let cfg = MixerConfig {
        max_voices: 1,
        ..config(1)
    };
    // Old voice seen by the reader, then ended, slot reused before the next read.
    let (mut ctl, mut r) = setup(cfg);
    start(&mut ctl, 0, 1, source(&dc(0.875, 200)), 1);
    render(&mut r, 100, &[]);
    assert_eq!(voice_peaks(&mut ctl), vec![(1, vec![0.875])]);
    start(&mut ctl, 300, 2, source(&dc(0.125, 1000)), 1);
    render(&mut r, 400, &[64, 3]);
    // Each voice's peak under its own id; the old one's final peak once.
    assert_eq!(
        voice_peaks(&mut ctl),
        vec![(1, vec![0.875]), (2, vec![0.125])]
    );
    assert_eq!(voice_peaks(&mut ctl), vec![(2, vec![0.0])]);

    // Old voice never seen by the reader: it lives and ends between two reads.
    let (mut ctl, mut r) = setup(cfg);
    start(&mut ctl, 0, 1, source(&dc(0.875, 50)), 1);
    start(&mut ctl, 60, 2, source(&dc(0.125, 1000)), 1);
    render(&mut r, 200, &[16]);
    assert_eq!(voice_peaks(&mut ctl), vec![(2, vec![0.125])]);

    // Old voice ended and was read before the reuse: the new voice starts from zero.
    let (mut ctl, mut r) = setup(cfg);
    start(&mut ctl, 0, 1, source(&dc(0.875, 50)), 1);
    render(&mut r, 100, &[]);
    assert_eq!(voice_peaks(&mut ctl), vec![(1, vec![0.875])]);
    start(&mut ctl, 100, 2, source(&dc(0.125, 1000)), 1);
    render(&mut r, 1, &[]);
    assert_eq!(voice_peaks(&mut ctl), vec![(2, vec![0.125])]);

    // Same slot ended and reused within one block: each voice keeps its own peak.
    let (mut ctl, mut r) = setup(cfg);
    start(&mut ctl, 0, 1, source(&dc(0.875, 10)), 1);
    render(&mut r, 5, &[]);
    assert_eq!(voice_peaks(&mut ctl), vec![(1, vec![0.875])]);
    start(&mut ctl, 20, 2, source(&dc(0.125, 1000)), 1);
    render(&mut r, 100, &[]);
    assert_eq!(
        voice_peaks(&mut ctl),
        vec![(1, vec![0.875]), (2, vec![0.125])]
    );

    // Two reuses between reads: the voice in between was never seen, so its peak and the one
    // it displaced are dropped; nothing shows under the wrong id.
    let (mut ctl, mut r) = setup(cfg);
    start(&mut ctl, 0, 1, source(&dc(0.875, 10)), 1);
    render(&mut r, 5, &[]);
    assert_eq!(voice_peaks(&mut ctl), vec![(1, vec![0.875])]);
    start(&mut ctl, 20, 2, source(&dc(0.75, 10)), 1);
    start(&mut ctl, 40, 3, source(&dc(0.125, 1000)), 1);
    render(&mut r, 100, &[7]);
    assert_eq!(voice_peaks(&mut ctl), vec![(3, vec![0.125])]);
}

#[test]
fn a_paused_voice_meters_silence_and_a_resumed_one_reports_under_its_new_id() {
    let (mut ctl, mut r) = setup(config(1));
    start(&mut ctl, 0, 10, source(&dc(0.5, 20_000)), 1);
    render(&mut r, 1000, &[]);
    assert_eq!(voice_peaks(&mut ctl), vec![(10, vec![0.5])]);
    ctl.send(1000, CommandKind::PauseVoice { voice: VoiceId(10) });
    // The declick tail (240 frames) is still heard, then nothing.
    render(&mut r, 500, &[100]);
    let tail = voice_peaks(&mut ctl);
    assert_eq!(tail.len(), 1);
    assert!(tail[0].1[0] > 0.0 && tail[0].1[0] <= 0.5);
    render(&mut r, 500, &[100]);
    assert_eq!(
        voice_peaks(&mut ctl),
        vec![(10, vec![0.0])],
        "paused: present, silent"
    );

    ctl.send(
        2000,
        CommandKind::ResumeVoice {
            voice: VoiceId(10),
            new_id: VoiceId(11),
        },
    );
    render(&mut r, 1000, &[64]);
    assert_eq!(
        voice_peaks(&mut ctl),
        vec![(11, vec![0.5])],
        "only the new id"
    );
    assert_eq!(voice_peaks(&mut ctl), vec![(11, vec![0.0])]);

    // Re-keyed while the reader still has peaks under the old id: both are reported once.
    ctl.send(3500, CommandKind::PauseVoice { voice: VoiceId(11) });
    ctl.send(
        3600,
        CommandKind::ResumeVoice {
            voice: VoiceId(11),
            new_id: VoiceId(12),
        },
    );
    render(&mut r, 1000, &[64]);
    assert_eq!(
        voice_peaks(&mut ctl),
        vec![(11, vec![0.5]), (12, vec![0.5])]
    );
    render(&mut r, 10, &[]);
    assert_eq!(voice_peaks(&mut ctl), vec![(12, vec![0.5])]);
}

#[test]
fn non_finite_input_never_reaches_a_meter() {
    let (mut ctl, mut r) = setup(config(2));
    let bad = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.5, f32::NAN];
    let data: Vec<f32> = bad.iter().copied().cycle().take(2000).collect();
    let mut v = voice(&ctl, 1, source(&data), 1);
    v.set_crosspoint(0, 1, 1.0);
    send_start(&mut ctl, 0, v);
    start(
        &mut ctl,
        0,
        2,
        source(&interleave(&[dc(f32::NAN, 1000), dc(f32::NAN, 1000)])),
        2,
    );
    for _ in 0..4 {
        r.render(500, &[64, 7]);
        for p in ctl.output_meters() {
            assert!(p.is_finite() && p >= 0.0, "{p}");
        }
        for (_, ps) in ctl.voice_meters() {
            assert!(ps.iter().all(|p| p.is_finite() && *p >= 0.0), "{ps:?}");
        }
    }
}

#[test]
fn xruns_are_counted_exactly_and_never_from_voice_underruns() {
    let (mut ctl, mut r) = setup(config(2));
    assert_eq!(ctl.xruns(), 0);
    let (_keep_open, starving) = open_source(1000, &[0.1; 100]);
    start(&mut ctl, 0, 1, starving, 1);
    render(&mut r, 1000, &[64]);
    assert!(ctl.underrun_frames() > 0);
    assert_eq!(ctl.xruns(), 0, "a voice underrun is not an xrun");

    let mut seen = 0;
    for n in [1u64, 0, 3, 2] {
        r.inject_xruns(n);
        render(&mut r, 64, &[]);
        let now = ctl.xruns();
        assert!(now >= seen);
        assert_eq!(now - seen, n);
        seen = now;
    }
    // The notifier counts from another thread (the device's error callback in M3.8).
    let notifier = r.mixer_mut().xrun_notifier();
    std::thread::spawn(move || {
        for _ in 0..10 {
            notifier.note(1);
        }
    })
    .join()
    .unwrap();
    no_alloc(|| r.mixer_mut().note_xruns(4));
    assert_eq!(ctl.xruns(), 6 + 10 + 4);
}

/// Reads racing the audio thread never attribute a peak to the wrong voice: every voice plays a
/// level unique to its id, through a small pool reused constantly.
#[test]
fn concurrent_reads_never_misattribute_a_peak() {
    let level = |id: u64| ((id % 251) + 1) as f32 / 512.0;
    let cfg = MixerConfig {
        max_voices: 3,
        ..config(2)
    };
    let (mut ctl, mut r) = setup(cfg);
    for id in 0..2000u64 {
        let v = voice(&ctl, id, source(&dc(level(id), 30)), 1);
        send_start(&mut ctl, id * 20, v);
    }
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&done);
    let audio = std::thread::spawn(move || {
        let mut buf = vec![0.0; 2 * 16];
        for _ in 0..3000 {
            no_alloc(|| r.render_into(&mut buf, &[16]));
        }
        flag.store(true, std::sync::atomic::Ordering::Release);
    });
    let cap = ctl.voice_meter_capacity();
    let (mut ids, mut peaks) = (vec![VoiceId(0); cap], vec![0.0; cap * 2]);
    let mut reported = 0;
    loop {
        let last = done.load(std::sync::atomic::Ordering::Acquire);
        ctl.pump();
        let n = ctl.read_voice_meters_into(&mut ids, &mut peaks);
        for (k, id) in ids.iter().take(n).enumerate() {
            let p = peaks[2 * k];
            assert!(p == 0.0 || p == level(id.0), "voice {id:?} peak {p}");
            assert_eq!(peaks[2 * k + 1], 0.0);
            reported += usize::from(p > 0.0);
        }
        if last {
            break;
        }
    }
    audio.join().unwrap();
    assert!(reported > 0);
}
