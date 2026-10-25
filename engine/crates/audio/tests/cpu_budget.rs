// SPDX-License-Identifier: GPL-3.0-or-later
// Helpers outside #[test] fns are test code too; clippy's in-test allowances miss them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr
)]

//! CPU budget gate (NFR-23, QA-06, M3.10): 64 stereo voices with fades must render at least
//! 10x faster than real time. Wall-clock dependent, so it is `#[ignore]`d and never part of
//! `cargo test`; CI runs it on the Intel runner with
//!
//! `cargo test --release -p openlab-audio --test cpu_budget -- --ignored --nocapture`
//!
//! One test function runs every measurement in sequence: parallel tests would compete for the
//! cores and skew each other's timings. `Instant` is used here and nowhere else.
//!
//! What is timed: only `OfflineRenderer::render_into` for one block at a time, i.e. the mixer
//! callback on the rendering thread (voice reads, fades, de-zipper, crosspoints, patch, output
//! stage, meters publishing). Not timed: filling the rings, sending commands, reading meters and
//! polling events (control-thread work between groups of blocks), and in the streamed variant
//! the disk workers' decoding and resampling, which run on their own threads.

mod common;

use std::io::Cursor;
use std::time::{Duration, Instant};

use common::media::*;
use common::*;
use openlab_audio::{
    CommandKind, Curve, DiskStreamer, Event, Fade, GainTarget, Media, MixerConfig, MixerController,
    Plays, StreamHandle, StreamRequest, StreamerConfig, VoiceId, plan, preload, probe_media,
};
use rtrb::Producer;

/// NFR-23 / QA-06: audio rendered per wall-clock second on the minimum-spec target class
/// (Intel CI runner) must be at least this.
const MIN_REALTIME_RATIO: f64 = 10.0;

const RATE: u32 = 48_000;
const VOICES: u64 = 64;
const DEVICE_OUTPUTS: usize = 16;
/// Frames per group of blocks between control-thread work; divisible by 64, 256 and 512.
const GROUP: usize = 12_288;
const WARMUP_GROUPS: usize = 8;
/// 250 groups = 64.0 s of audio measured (NFR-23 asks for 60 s at least).
const MEASURED_GROUPS: usize = 250;
/// Voices play longer than the run, so all 64 stay active throughout.
const VOICE_FRAMES: u64 = 70 * RATE as u64;
const FAILSAFE: Duration = Duration::from_secs(60);

struct Stats {
    ratio: f64,
    p50_us: f64,
    p99_us: f64,
    max_us: f64,
    underrun_frames: u64,
    wall_ratio: f64,
}

#[derive(Clone, Copy, PartialEq)]
enum Feed {
    /// The test thread tops up each voice's ring between groups (rings only: deterministic).
    Rings,
    /// 44.1 kHz in-memory files through `DiskStreamer`, resampled to 48 kHz by its workers.
    Resampled,
}

/// Fills `tx` from the looping `src` (interleaved stereo) up to its free space.
fn top_up(tx: &mut Producer<f32>, src: &[f32], pos: &mut usize) {
    let mut free = tx.slots();
    while free > 0 {
        let n = free.min(src.len() - *pos);
        if tx.push_entire_slice(&src[*pos..*pos + n]).is_err() {
            break;
        }
        *pos = (*pos + n) % src.len();
        free -= n;
    }
}

/// A voice's ring producer, its looping source and the read position in it.
type Feeder = (Producer<f32>, Vec<f32>, usize);

fn start_voices(
    ctl: &mut MixerController,
    feed: Feed,
) -> (Vec<Feeder>, Vec<StreamHandle>, Option<DiskStreamer>) {
    let curves = [
        Curve::Linear,
        Curve::EqualPower,
        Curve::SCurve,
        Curve::Logarithmic,
        Curve::Exponential,
    ];
    let mut rings = Vec::new();
    let mut handles = Vec::new();
    let mut streamer = None;
    let shared = match feed {
        Feed::Resampled => {
            let s = DiskStreamer::new(StreamerConfig::new(RATE)).unwrap();
            streamer = Some(s);
            let src = tone(72 * 44_100, 44_100);
            let bytes = wav(&src, 2, 44_100, Pcm::F32);
            Some(
                preload(Media::Reader(
                    Box::new(Cursor::new(bytes)),
                    Some("wav".into()),
                ))
                .unwrap(),
            )
        }
        Feed::Rings => None,
    };
    for id in 0..VOICES {
        let mut v = match (&shared, streamer.as_mut()) {
            (Some(p), Some(d)) => {
                let info = probe_media(Media::Preloaded(p.clone())).unwrap().info;
                let slices = plan(
                    0..VOICE_FRAMES.min(info.frames_at(RATE)),
                    &[],
                    &[Plays::Times(1)],
                    0,
                )
                .unwrap();
                let prepared = d
                    .prepare(StreamRequest {
                        media: Media::Preloaded(p.clone()),
                        info,
                        seeking: openlab_audio::Seeking::Direct,
                        slices,
                    })
                    .unwrap();
                let (v, h) = prepared.into_voice(VoiceId(id), ctl.cue_outputs());
                handles.push(h);
                v
            }
            _ => {
                let (tx, rx) = rtrb::RingBuffer::new(RATE as usize * 2);
                let freq = 110.0 + 7.0 * id as f64;
                let src: Vec<f32> = sine(RATE as usize, RATE, freq, 0.2)
                    .into_iter()
                    .flat_map(|s| [s, -s])
                    .collect();
                rings.push((tx, src, 0));
                openlab_audio::VoiceStart::new(VoiceId(id), rx, 2, ctl.cue_outputs())
            }
        };
        // Two cue outputs (0 and 1), onto device outputs through the voice's patch.
        v.set_crosspoint(0, 0, 1.0);
        v.set_crosspoint(1, 1, 1.0);
        v.patch = (id % 4) as u16;
        v.gain = 0.5;
        v.fade_in = Fade::new(RATE as u64 * 3 / 2, curves[id as usize % 5]);
        // A slice table replaces `play_frames`.
        if v.slices.is_empty() {
            v.play_frames = Some(VOICE_FRAMES);
        }
        v.fade_out = Fade::new(RATE as u64 * 2, curves[(id as usize + 2) % 5]);
        send_start(ctl, 0, v);
    }
    (rings, handles, streamer)
}

fn run(block: usize, feed: Feed) -> Stats {
    let cfg = MixerConfig {
        sample_rate: RATE,
        output_channels: DEVICE_OUTPUTS,
        max_block_frames: 512,
        max_voices: VOICES as usize,
        ..MixerConfig::default()
    };
    let (mut ctl, mut r) = setup(cfg);
    for p in 0..4u16 {
        for c in 0..2u16 {
            ctl.send(
                0,
                CommandKind::SetPatchRouting {
                    patch: p,
                    cue_output: c,
                    device_output: 2 * p + c,
                    gain: 1.0,
                },
            );
        }
    }
    ctl.send(0, CommandKind::SetMaster { gain: 0.9 });
    let (mut rings, handles, _streamer) = start_voices(&mut ctl, feed);

    let mut out = vec![0.0f32; GROUP * DEVICE_OUTPUTS];
    let mut times = Vec::with_capacity(MEASURED_GROUPS * (GROUP / block));
    let (mut ids, mut peaks) = (
        vec![VoiceId(0); ctl.voice_meter_capacity()],
        vec![0.0; ctl.voice_meter_capacity() * DEVICE_OUTPUTS],
    );
    let mut outs = [0.0f32; DEVICE_OUTPUTS];
    let mut measured = Duration::ZERO;
    let mut wall = Duration::ZERO;
    let mut finished = 0;
    for g in 0..WARMUP_GROUPS + MEASURED_GROUPS {
        let wall_start = Instant::now();
        // Control-thread work, untimed: feed the rings, ramps in flight, meters, events.
        for (tx, src, pos) in &mut rings {
            top_up(tx, src, pos);
        }
        let now = ctl.samples_rendered();
        for h in &handles {
            let need = now + 2 * GROUP as u64;
            assert!(
                h.wait_written(need, FAILSAFE).unwrap(),
                "stream stalled at group {g}: written {} need {need} err {:?}",
                h.frames_written(),
                h.error()
            );
        }
        // Once a second, 600 ms gain and crosspoint ramps on every voice: fades in flight for
        // 60 % of the run, with the other half of the time on the de-zipper's settled path.
        if g % 4 == 0 {
            let up = (g / 4) % 2 == 0;
            for id in 0..VOICES {
                let fade = Fade::new(RATE as u64 * 6 / 10, Curve::SCurve);
                let (a, b) = if up { (1.0, 0.6) } else { (0.5, 1.0) };
                ctl.send(
                    now,
                    CommandKind::Ramp {
                        voice: VoiceId(id),
                        target: GainTarget::CueOutput(0),
                        gain: a,
                        fade,
                    },
                );
                ctl.send(
                    now,
                    CommandKind::Ramp {
                        voice: VoiceId(id),
                        target: GainTarget::Crosspoint {
                            input: 1,
                            output: 1,
                        },
                        gain: b,
                        fade,
                    },
                );
            }
        }
        ctl.pump();
        wall += wall_start.elapsed();
        for chunk in out.chunks_mut(block * DEVICE_OUTPUTS) {
            let t = Instant::now();
            r.render_into(chunk, &[block]);
            let d = t.elapsed();
            if g >= WARMUP_GROUPS {
                measured += d;
                times.push(d.as_nanos() as f64 / 1000.0);
            }
        }
        // Meters are read at the UI's 30 Hz at most; here once a group (about 4 Hz).
        ctl.read_output_meters_into(&mut outs);
        ctl.read_voice_meters_into(&mut ids, &mut peaks);
        while let Some(e) = ctl.poll_event() {
            if matches!(e, Event::VoiceFinished { .. }) {
                finished += 1;
            }
        }
    }
    assert_eq!(finished, 0, "voices must stay active for the whole run");
    assert_eq!(ctl.faults(), 0);
    assert!(out.iter().any(|s| *s != 0.0), "the mix is not silent");
    assert!(out.iter().all(|s| s.is_finite()));
    let audio_s = (MEASURED_GROUPS * GROUP) as f64 / f64::from(RATE);
    times.sort_by(|a, b| a.total_cmp(b));
    let pct = |p: f64| times[((times.len() - 1) as f64 * p) as usize];
    Stats {
        ratio: audio_s / measured.as_secs_f64(),
        p50_us: pct(0.5),
        p99_us: pct(0.99),
        max_us: times[times.len() - 1],
        underrun_frames: ctl.underrun_frames(),
        wall_ratio: audio_s / (measured + wall).as_secs_f64(),
    }
}

fn report(name: &str, block: usize, s: &Stats) {
    println!(
        "{name} block={block:>3}: {:.1}x real time (render only), p50 {:.1} us  p99 {:.1} us  max {:.1} us  (budget per block {:.0} us), underrun frames {}, incl. control/feed work {:.1}x",
        s.ratio,
        s.p50_us,
        s.p99_us,
        s.max_us,
        block as f64 / f64::from(RATE) * 1e6,
        s.underrun_frames,
        s.wall_ratio
    );
}

#[test]
#[ignore = "wall-clock CPU budget gate (NFR-23, QA-06); run with --release -- --ignored --nocapture"]
fn sixty_four_stereo_voices_with_fades_render_at_least_ten_times_real_time() {
    if cfg!(debug_assertions) {
        eprintln!("cpu_budget: skipped in a debug build; use `cargo test --release`");
        return;
    }
    let mut gated = 0.0;
    for block in [512, 256, 64] {
        let s = run(block, Feed::Rings);
        report("rings    ", block, &s);
        assert_eq!(
            s.underrun_frames, 0,
            "a starved voice would flatter the ratio"
        );
        if block == 512 {
            gated = s.ratio;
        }
    }
    // Resampling runs on the disk workers, not the rendering thread: reported, not gated.
    let s = run(512, Feed::Resampled);
    report("resampled", 512, &s);
    assert!(
        gated >= MIN_REALTIME_RATIO,
        "mixer render cost regressed: {gated:.1}x real time, budget {MIN_REALTIME_RATIO}x (NFR-23, QA-06)"
    );
}
