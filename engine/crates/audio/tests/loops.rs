// SPDX-License-Identifier: GPL-3.0-or-later
// Helpers outside #[test] fns are test code too; clippy's in-test allowances miss them.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The loop cache (M3.13), bounded loop exit buffers (L-R2) and devamps while a loop exit is
//! still playing (L-R5), end to end through disk streams and the offline renderer (AUD-12,
//! CT-16). Tests wait on stream signals, never on timers; every render runs with allocation
//! forbidden.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::media::*;
use common::*;
use openlab_audio::{
    CommandKind, DiskStreamer, Event, FinishReason, Media, MediaInfo, MixerController,
    OfflineRenderer, Plays, Seeking, Slice, StreamHandle, StreamRequest, StreamerConfig, VoiceId,
    plan, probe,
};

const SR: u32 = 48_000;
const BLOCK: usize = 256;
const FAILSAFE: Duration = Duration::from_secs(20);
const ID: VoiceId = VoiceId(1);

fn render(r: &mut OfflineRenderer, frames: usize) -> Vec<f32> {
    let ch = r.mixer_mut().output_channels();
    let mut out = vec![0.0; frames * ch];
    for chunk in out.chunks_mut(BLOCK * ch) {
        no_alloc(|| r.render_into(chunk, &[chunk.len() / ch]));
    }
    out
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Starts `media` as voice 1 at 0 with identity routing (channels beyond the mixer's outputs
/// are not routed), once it is primed.
fn start(
    d: &mut DiskStreamer,
    ctl: &mut MixerController,
    media: Media,
    info: MediaInfo,
    slices: Vec<Slice>,
) -> StreamHandle {
    let p = d
        .prepare(StreamRequest {
            media,
            info,
            seeking: Seeking::Direct,
            slices,
        })
        .unwrap();
    assert!(p.handle.wait_primed(FAILSAFE).unwrap());
    let (mut v, h) = p.into_voice(ID, ctl.cue_outputs());
    for i in 0..usize::from(v.channels).min(ctl.cue_outputs()) {
        v.set_crosspoint(i, i, 1.0);
    }
    send_start(ctl, 0, v);
    h
}

/// Plays a finite table that fits in the ring (`ring_ms`) from a fully written ring and returns
/// the stereo output and the frames the stream wrote from its loop cache.
fn play_finite(
    media: Media,
    info: MediaInfo,
    slices: Vec<Slice>,
    device_rate: u32,
    tune: impl FnOnce(&mut StreamerConfig),
) -> (Vec<f32>, u64) {
    let total: u64 = slices
        .iter()
        .map(|s| match s.plays {
            Plays::Times(n) => s.frames * u64::from(n),
            Plays::Infinite => panic!("finite tables only"),
        })
        .sum();
    let mut cfg = StreamerConfig::new(device_rate);
    cfg.buffer_ms = 10_000;
    tune(&mut cfg);
    let mut d = DiskStreamer::new(cfg).unwrap();
    let mut mc = config(2);
    mc.sample_rate = device_rate;
    let (mut ctl, mut r) = setup(mc);
    let h = start(&mut d, &mut ctl, media, info, slices);
    assert!(h.wait_written(u64::MAX, FAILSAFE).unwrap());
    let out = render(&mut r, total as usize + 2 * BLOCK);
    assert!(events(&mut ctl).contains(&Event::VoiceFinished {
        voice: ID,
        at: total,
        reason: FinishReason::OutPoint
    }));
    assert_eq!(ctl.underrun_frames(), 0);
    (out, h.cached_frames())
}

/// Deterministic noise, so every frame of a loop differs.
fn noise(samples: usize) -> Vec<f32> {
    let mut x = 0x2545_f491_u32;
    (0..samples)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x as f32 / u32::MAX as f32 - 0.5) * 1.5
        })
        .collect()
}

/// M3.13: for lossless sources at the device rate, later plays from the loop cache are the very
/// frames a seek back would decode, so the output is bit-identical to the uncached path. Covers
/// a loop split by a start offset (its first cached play comes after a seek), a short loop
/// (many plays per worker step) and one longer than a decode chunk (recorded over several
/// steps).
#[test]
fn loop_cache_output_equals_the_uncached_path_bit_for_bit() {
    let wav_path = temp_file("noise.wav", &wav(&noise(2 * 20_000), 2, SR, Pcm::I16));
    let wav_info = probe(&wav_path).unwrap().info;
    let wav_slices = plan(
        0..20_000,
        &[3_000, 4_000, 9_000],
        &[
            Plays::Times(1),
            Plays::Times(30),
            Plays::Times(2),
            Plays::Times(1),
        ],
        3_500,
    )
    .unwrap();
    let flac = fixture("tone.flac");
    let flac_info = probe(&flac).unwrap().info;
    let flac_slices = plan(
        0..flac_info.frames,
        &[2_000, 3_000],
        &[Plays::Times(1), Plays::Times(40), Plays::Times(1)],
        0,
    )
    .unwrap();
    for (path, info, slices, rate, cached) in [
        // 28 plays of the 1000-frame loop after its recorded one, and the second 5000-frame play.
        (wav_path, wav_info, wav_slices, SR, 28_000 + 5_000),
        (flac, flac_info, flac_slices, 44_100, 39_000),
    ] {
        let media = || Media::Path(path.clone());
        let (with, n) = play_finite(media(), info, slices.clone(), rate, |_| {});
        let (without, m) = play_finite(media(), info, slices.clone(), rate, |c| {
            c.loop_cache_bytes = 0;
        });
        assert_eq!((n, m), (cached, 0), "{}", path.display());
        assert!(with.iter().any(|&s| s != 0.0));
        let same = with
            .iter()
            .zip(&without)
            .all(|(a, b)| a.to_bits() == b.to_bits());
        assert!(same, "{}: cached output differs", path.display());
    }
}

/// M3.13: a looped AAC slice plays the same frames every time (a seek back would restart the
/// decoder, whose output after a seek differs slightly).
#[test]
fn looped_aac_plays_are_identical() {
    let path = fixture("tone.m4a");
    let info = probe(&path).unwrap().info;
    assert_eq!(info.sample_rate, 44_100);
    let (start, len, plays) = (4_096u64, 4_096u64, 8u32);
    let slices = plan(
        0..info.frames,
        &[start, start + len],
        &[Plays::Times(1), Plays::Times(plays), Plays::Times(1)],
        0,
    )
    .unwrap();
    let (out, cached) = play_finite(Media::Path(path), info, slices, 44_100, |_| {});
    assert_eq!(cached, u64::from(plays - 1) * len);
    let at = |k: u64| ((start + k * len) * 2) as usize..((start + (k + 1) * len) * 2) as usize;
    let first = &out[at(0)];
    assert!(
        first.iter().any(|&s| s.abs() > 0.1),
        "the loop is not silent"
    );
    for k in 1..u64::from(plays) {
        assert!(
            out[at(k)]
                .iter()
                .zip(first)
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "play {k} differs from the first"
        );
    }
}

/// M3.13 bounds: a loop longer than `loop_cache_bytes`, or one the global budget cannot hold,
/// seeks back for every play as before and plays the same; the memory is given back when the
/// stream ends.
#[test]
fn loops_beyond_the_cache_bounds_seek_as_before() {
    let path = temp_file("bounds.wav", &wav(&markers(10_000), 1, SR, Pcm::F32));
    let info = probe(&path).unwrap().info;
    let plays = [Plays::Times(1), Plays::Times(5), Plays::Times(1)];
    let slices = plan(0..10_000, &[1_000, 2_000], &plays, 0).unwrap();
    let want: Vec<f32> = (0..1_000)
        .chain((0..5).flat_map(|_| 1_000..2_000))
        .chain(2_000..10_000)
        .flat_map(|i| [marker(i), 0.0])
        .collect();
    let media = || Media::Path(path.clone());
    // One play is 1000 frames x 4 bytes.
    type Tune = fn(&mut StreamerConfig);
    let cases: [(&str, Tune, u64); 4] = [
        ("cached", |_| {}, 4_000),
        ("exactly at the cap", |c| c.loop_cache_bytes = 4_000, 4_000),
        ("over the cap", |c| c.loop_cache_bytes = 3_999, 0),
        ("over the budget", |c| c.cache_budget_bytes = 3_999, 0),
    ];
    for (name, tune, cached) in cases {
        let (out, n) = play_finite(media(), info, slices.clone(), SR, tune);
        assert_eq!(n, cached, "{name}");
        assert_eq!(&out[..want.len()], &want[..], "{name}");
    }

    // Memory is counted while the stream holds it and given back afterwards.
    let mut d = DiskStreamer::new(StreamerConfig::new(SR)).unwrap();
    let (mut ctl, mut r) = setup(config(2));
    let inf = [Plays::Times(1), Plays::Infinite, Plays::Times(1)];
    let h = start(
        &mut d,
        &mut ctl,
        media(),
        info,
        plan(0..10_000, &[1_000, 2_000], &inf, 0).unwrap(),
    );
    assert!(h.wait_written(50_000, FAILSAFE).unwrap());
    // The loop cache and the exit buffer into slice 2 (250 ms, 8000 frames of it).
    let exit = 12_000 * 4;
    assert!(h.cached_frames() > 0);
    assert_eq!(h.cache_bytes(), 4_000 + exit);
    assert_eq!(d.cache_bytes(), 4_000 + exit);
    render(&mut r, 3_000);
    ctl.send(
        ctl.samples_rendered(),
        CommandKind::StopVoice {
            voice: ID,
            fade: openlab_audio::Fade::NONE,
        },
    );
    render(&mut r, BLOCK);
    drop(h);
    ctl.pump();
    let deadline = Instant::now() + FAILSAFE;
    while d.cache_bytes() != 0 && Instant::now() < deadline {
        std::thread::yield_now();
        ctl.pump();
    }
    assert_eq!(d.cache_bytes(), 0);
}

/// L-R2: one exit buffer per distinct loop exit, 16 channels (the most a WAV file gives
/// symphonia; 64 channels are checked in the stream's unit tests): 768 KB each, 31 of them
/// before the vamp, 10 within an 8 MiB cap. A devamp out of a loop whose exit was not built
/// still continues exactly (the continuation comes from the ring once the disk has cut).
#[test]
fn many_exits_stay_within_the_cap() {
    const CH: u16 = 16;
    const CAP: usize = 8 << 20;
    const PAIRS: u64 = 40;
    const LEN: u64 = 480;
    const VAMP: u64 = 30; // the loop that plays until devamped
    let frames = PAIRS * 2 * LEN + 2_000;
    let samples: Vec<f32> = (0..frames)
        .flat_map(|i| std::iter::repeat_n(marker(i), usize::from(CH)))
        .collect();
    let path = temp_file("exits-64.wav", &wav(&samples, CH, SR, Pcm::F32));
    drop(samples);
    let info = probe(&path).unwrap().info;
    let marks: Vec<u64> = (1..=2 * PAIRS).map(|k| k * LEN).collect();
    let plays: Vec<Plays> = (0..=2 * PAIRS)
        .map(|k| match k {
            k if k == 2 * VAMP => Plays::Infinite,
            k if k % 2 == 0 && k < 2 * PAIRS => Plays::Times(2),
            _ => Plays::Times(1),
        })
        .collect();
    let slices = plan(0..frames, &marks, &plays, 0).unwrap();
    let mut cfg = StreamerConfig::new(SR);
    cfg.exit_bytes = CAP;
    let mut d = DiskStreamer::new(cfg).unwrap();
    let mut mc = config(1);
    mc.max_voice_channels = usize::from(CH);
    let (mut ctl, mut r) = setup(mc);
    let h = start(&mut d, &mut ctl, Media::Path(path), info, slices);
    assert!(h.wait_written(96_000 - 4_096, FAILSAFE).unwrap());
    let exit = 12_000 * usize::from(CH) * 4;
    let loop_cache = LEN as usize * usize::from(CH) * 4;
    // 31 loops with distinct exits so far; only whole exits within the cap are kept.
    let held = h.cache_bytes();
    assert_eq!(held, CAP / exit * exit + loop_cache);
    assert_eq!(d.cache_bytes(), held);

    // Into the vamp's second play, then leave it: its exit is beyond the cap.
    let vamp_start = VAMP * 3 * LEN; // each pair: the loop twice, then the bridge
    let devamp_at = vamp_start + LEN + 100;
    let boundary = vamp_start + 2 * LEN;
    let mut out = render(&mut r, devamp_at as usize);
    ctl.send(
        devamp_at,
        CommandKind::Devamp {
            voice: ID,
            stop: false,
        },
    );
    out.extend(render(&mut r, (boundary - devamp_at) as usize));
    assert!(h.wait_cuts(1, FAILSAFE).unwrap());
    // Without an exit buffer the ring, full of stale plays, may have nothing to play at the
    // boundary: the voice drops the stale frames and underruns until the disk has refilled
    // behind them (here at most the rest of the block). Then it goes on exactly where it was.
    out.extend(render(&mut r, BLOCK));
    let gap = ctl.underrun_frames() as usize;
    assert!(gap <= BLOCK);
    assert!(h.wait_written(u64::MAX, FAILSAFE).unwrap());
    let rest = (frames * 2) as usize - out.len();
    out.extend(render(&mut r, rest));
    let mut want = Vec::new();
    for k in 0..=2 * PAIRS {
        let s = if k == 2 * PAIRS {
            k * LEN..frames
        } else {
            k * LEN..(k + 1) * LEN
        };
        let n = match plays[k as usize] {
            Plays::Times(n) => u64::from(n),
            Plays::Infinite => 2,
        };
        for _ in 0..n {
            want.extend(s.clone().map(marker));
        }
    }
    let b = boundary as usize;
    assert_eq!(&out[..b], &want[..b]);
    assert!(out[b..b + gap].iter().all(|&s| s == 0.0));
    assert_eq!(&out[b + gap..want.len() + gap], &want[b..]);
    assert!(out[want.len() + gap..].iter().all(|&s| s == 0.0));
    assert_eq!(ctl.underrun_frames(), gap as u64);
    assert!(
        !events(&mut ctl)
            .iter()
            .any(|e| matches!(e, Event::DevampIgnored { .. }))
    );
}

/// L-R5: vamp A, a bridge B shorter than the exit buffer, vamp C, outro D. Devamps that arrive
/// while the voice still plays the exit buffer out of A are carried out: one during the bridge
/// leaves at the bridge's end (the bridge does not loop, so playback goes on into C as without
/// an exit buffer), one in C's third play leaves C at the end of that play. Also with a short
/// exit buffer, so that C's boundary comes after the buffer, from the ring.
#[test]
fn devamps_while_a_loop_exit_plays_are_carried_out() {
    for exit_ms in [250, 31] {
        let (media, info) = {
            let path = temp_file("bridge.wav", &wav(&markers(6_000), 1, SR, Pcm::F32));
            let info = probe(&path).unwrap().info;
            (Media::Path(path), info)
        };
        let mut cfg = StreamerConfig::new(SR);
        cfg.exit_ms = exit_ms;
        let mut d = DiskStreamer::new(cfg).unwrap();
        let (mut ctl, mut r) = setup(config(1));
        let inf = Plays::Infinite;
        let once = Plays::Times(1);
        let slices = plan(
            0..6_000,
            &[1_000, 2_000, 3_000, 4_000],
            &[once, inf, once, inf, once],
            0,
        )
        .unwrap();
        let h = start(&mut d, &mut ctl, media, info, slices);
        assert!(h.wait_written(90_000, FAILSAFE).unwrap());
        let devamp = || CommandKind::Devamp {
            voice: ID,
            stop: false,
        };
        // A's fourth play; it ends at 5000.
        let mut out = render(&mut r, 4_300);
        ctl.send(4_300, devamp());
        out.extend(render(&mut r, BLOCK));
        assert!(h.wait_cuts(1, FAILSAFE).unwrap());
        // In the bridge, which plays from A's exit buffer; the stale plays are dropped, so the
        // disk can refill the ring.
        out.extend(render(&mut r, 5_500 - out.len()));
        let resume = h.last_resume().unwrap();
        assert!(h.wait_written(resume + 20_000, FAILSAFE).unwrap());
        ctl.send(5_500, devamp());
        // C's third play (8000..9000), still inside the 250 ms exit buffer.
        out.extend(render(&mut r, 8_200 - out.len()));
        ctl.send(8_200, devamp());
        out.extend(render(&mut r, BLOCK));
        assert!(h.wait_cuts(2, FAILSAFE).unwrap());
        let resume = h.last_resume().unwrap();
        assert!(h.wait_written(resume + 20_000, FAILSAFE).unwrap());
        out.extend(render(&mut r, 12_000 - out.len()));

        let want: Vec<f32> = (0..1_000)
            .chain((0..4).flat_map(|_| 1_000..2_000))
            .chain(2_000..3_000)
            .chain((0..3).flat_map(|_| 3_000..4_000))
            .chain(4_000..6_000)
            .map(marker)
            .collect();
        assert_eq!(&out[..want.len()], &want[..], "exit {exit_ms} ms");
        assert!(out[want.len()..].iter().all(|&s| s == 0.0));
        let ev = events(&mut ctl);
        for at in [5_000, 6_000, 9_000] {
            assert!(
                ev.contains(&Event::DevampScheduled { voice: ID, at }),
                "exit {exit_ms} ms: {ev:?}"
            );
        }
        assert!(
            !ev.iter().any(|e| matches!(e, Event::DevampIgnored { .. })),
            "{ev:?}"
        );
        assert!(ev.contains(&Event::VoiceFinished {
            voice: ID,
            at: 11_000,
            reason: FinishReason::OutPoint
        }));
        assert_eq!(ctl.underrun_frames(), 0);
    }
}

/// L-R5 with an integrated fade-out: a devamp out of vamp C while A's exit buffer still plays
/// shortens the end below the fade's length, so the fade restarts from the gain the voice had
/// (1.0: no end was known before) and still reaches silence on the last frame, never jumping.
#[test]
fn fade_out_stays_continuous_when_a_devamp_inside_a_loop_exit_shortens_the_end() {
    let path = temp_file("level-exit.wav", &wav(&[0.5; 8_000], 1, SR, Pcm::F32));
    let info = probe(&path).unwrap().info;
    let (once, inf) = (Plays::Times(1), Plays::Infinite);
    let slices = plan(
        0..8_000,
        &[1_000, 2_000, 3_000, 4_000],
        &[once, inf, once, inf, once],
        0,
    )
    .unwrap();
    let mut d = DiskStreamer::new(StreamerConfig::new(SR)).unwrap();
    let (mut ctl, mut r) = setup(config(1));
    let p = d
        .prepare(StreamRequest {
            media: Media::Path(path),
            info,
            seeking: Seeking::Direct,
            slices,
        })
        .unwrap();
    assert!(p.handle.wait_primed(FAILSAFE).unwrap());
    let (mut v, h) = p.into_voice(ID, 1);
    v.set_crosspoint(0, 0, 1.0);
    v.fade_out = openlab_audio::Fade::new(6_000, openlab_audio::Curve::Linear);
    send_start(&mut ctl, 0, v);
    assert!(h.wait_written(90_000, FAILSAFE).unwrap());
    let devamp = || CommandKind::Devamp {
        voice: ID,
        stop: false,
    };
    let mut out = render(&mut r, 1_250);
    ctl.send(1_250, devamp());
    out.extend(render(&mut r, 3_200 - out.len()));
    ctl.send(3_200, devamp());
    // The voice may still be waiting for the first cut's answer (then this one is sent once
    // it is in, on the voice's next block).
    out.extend(render(&mut r, BLOCK));
    assert!(h.wait_cuts(1, FAILSAFE).unwrap());
    out.extend(render(&mut r, BLOCK));
    assert!(h.wait_cuts(2, FAILSAFE).unwrap());
    assert!(h.wait_written(u64::MAX, FAILSAFE).unwrap());
    out.extend(render(&mut r, 9_000 - out.len()));
    // Intro, one play of A, the bridge, one play of C, then the 4000-frame outro.
    assert!(out[..4_000].iter().all(|&s| s == 0.5));
    assert_eq!(out[7_999], 0.0);
    assert!(out[8_000..].iter().all(|&s| s == 0.0));
    let max_step = 0.5 / 4_000.0 * 1.01;
    for i in 0..7_999 {
        let step = (out[i + 1] - out[i]).abs();
        assert!(step <= max_step, "step {step} at frame {i}");
    }
    assert_eq!(ctl.underrun_frames(), 0);
}
