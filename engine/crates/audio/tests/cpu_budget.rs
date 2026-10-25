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

const CURVES: [Curve; 5] = [
    Curve::Linear,
    Curve::EqualPower,
    Curve::SCurve,
    Curve::Logarithmic,
    Curve::Exponential,
];

/// NFR-23 / QA-06: audio rendered per wall-clock second on the minimum-spec target class
/// (Intel CI runner) must be at least this.
const MIN_REALTIME_RATIO: f64 = 10.0;

/// Independent timed runs at block 512; the gate passes as soon as one reaches the budget.
const GATE_RUNS: usize = 3;

const RATE: u32 = 48_000;
const VOICES: u64 = 64;
const DEVICE_OUTPUTS: usize = 16;
/// Frames per group of blocks between control-thread work; divisible by 64, 256 and 512.
const GROUP: usize = 12_288;
const WARMUP_GROUPS: usize = 8;
/// Voices are replaced as they near their end, so about 64 stay audible while fades keep
/// running; the overlap of an old and a new voice needs a little headroom over 64.
const MAX_VOICES: usize = 96;
/// Steady-state life of a voice: 1.5 s fade-in, 2 s fade-out inside it (44 % of a voice's life
/// is spent in a curved fade, before the gain ramps are counted).
const LIFE: u64 = 8 * RATE as u64;
/// A voice's successor starts this long before its end (crossfade: fade-out meets fade-in).
const OVERLAP: u64 = RATE as u64 / 2;
/// 250 groups = 64.0 s of audio measured (NFR-23 asks for 60 s at least).
const MEASURED_GROUPS: usize = 250;
const FAILSAFE: Duration = Duration::from_secs(60);

struct Stats {
    ratio: f64,
    p50_us: f64,
    p99_us: f64,
    max_us: f64,
    underrun_frames: u64,
    wall_ratio: f64,
    voices_started: u64,
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

/// One playing voice instance: its feed and the frame it started and ends at.
struct Live {
    start: u64,
    end: u64,
    life: u64,
    ring: Option<(Producer<f32>, Vec<f32>, usize)>,
    handle: Option<StreamHandle>,
}

/// Starts voice instances and keeps 64 slots occupied by replacing each voice as it ends.
struct Workload {
    streamer: Option<DiskStreamer>,
    shared: Option<std::sync::Arc<openlab_audio::Preloaded>>,
    next_id: u64,
    /// The index into `live` is not stable; a slot's current voice is found by `slot_voice`.
    slot_voice: Vec<VoiceId>,
    slot_end: Vec<u64>,
    live: Vec<Live>,
    started: u64,
}

impl Workload {
    fn new(feed: Feed) -> Result<Self, String> {
        let (streamer, shared) = match feed {
            Feed::Resampled => {
                let s = DiskStreamer::new(StreamerConfig::new(RATE))
                    .map_err(|e| format!("disk streamer: {e:?}"))?;
                let src = tone(72 * 44_100, 44_100);
                let bytes = wav(&src, 2, 44_100, Pcm::F32);
                let media = Media::Reader(Box::new(Cursor::new(bytes)), Some("wav".into()));
                (
                    Some(s),
                    Some(preload(media).map_err(|e| format!("preload: {e:?}"))?),
                )
            }
            Feed::Rings => (None, None),
        };
        Ok(Self {
            streamer,
            shared,
            next_id: 0,
            slot_voice: vec![VoiceId(0); VOICES as usize],
            slot_end: vec![0; VOICES as usize],
            live: Vec::new(),
            started: 0,
        })
    }

    /// Starts a voice of `life` frames in `slot` at frame `at`. The curves rotate with the
    /// voice's id, so every curve runs in both directions throughout the window.
    fn spawn(
        &mut self,
        ctl: &mut MixerController,
        slot: usize,
        at: u64,
        life: u64,
    ) -> Result<(), String> {
        let id = self.next_id;
        self.next_id += 1;
        self.started += 1;
        let vid = VoiceId(id);
        let (mut v, ring, handle) = match (&self.shared, self.streamer.as_mut()) {
            (Some(p), Some(d)) => {
                let info = probe_media(Media::Preloaded(p.clone()))
                    .map_err(|e| format!("probe: {e:?}"))?
                    .info;
                let slices = plan(
                    0..life.min(info.frames_at(RATE)),
                    &[],
                    &[Plays::Times(1)],
                    0,
                )
                .map_err(|e| format!("plan: {e:?}"))?;
                let prepared = d
                    .prepare(StreamRequest {
                        media: Media::Preloaded(p.clone()),
                        info,
                        seeking: openlab_audio::Seeking::Direct,
                        slices,
                    })
                    .map_err(|e| format!("prepare: {e:?}"))?;
                let (v, h) = prepared.into_voice(vid, ctl.cue_outputs());
                (v, None, Some(h))
            }
            _ => {
                let (tx, rx) = rtrb::RingBuffer::new(RATE as usize * 2);
                let freq = 110.0 + 7.0 * (id % VOICES) as f64;
                let src: Vec<f32> = sine(RATE as usize, RATE, freq, 0.2)
                    .into_iter()
                    .flat_map(|s| [s, -s])
                    .collect();
                let v = openlab_audio::VoiceStart::new(vid, rx, 2, ctl.cue_outputs());
                (v, Some((tx, src, 0)), None)
            }
        };
        // Two cue outputs (0 and 1), onto device outputs through the voice's patch.
        v.set_crosspoint(0, 0, 1.0);
        v.set_crosspoint(1, 1, 1.0);
        v.patch = (slot % 4) as u16;
        v.gain = 0.5;
        v.fade_in = Fade::new(RATE as u64 * 3 / 2, CURVES[id as usize % 5]);
        // A slice table replaces `play_frames`.
        if v.slices.is_empty() {
            v.play_frames = Some(life);
        }
        v.fade_out = Fade::new(RATE as u64 * 2, CURVES[(id as usize / 5 + 2) % 5]);
        send_start(ctl, at, v);
        self.slot_voice[slot] = vid;
        self.slot_end[slot] = at + life;
        self.live.push(Live {
            start: at,
            end: at + life,
            life,
            ring,
            handle,
        });
        Ok(())
    }

    /// Initial lives are staggered from 4 s to 8 s, so starts, fade-ins and fade-outs spread
    /// evenly over the window from the first group on.
    fn start_all(&mut self, ctl: &mut MixerController) -> Result<(), String> {
        for slot in 0..VOICES as usize {
            let life = LIFE / 2 + LIFE / 2 * slot as u64 / VOICES;
            self.spawn(ctl, slot, 0, life)?;
        }
        Ok(())
    }

    /// Control-thread work for the group starting at `now`: replace ending voices, retire ended
    /// ones, feed the rings. Err when a streamed voice stalls.
    fn service(&mut self, ctl: &mut MixerController, now: u64) -> Result<(), String> {
        for slot in 0..VOICES as usize {
            if now + OVERLAP >= self.slot_end[slot] {
                self.spawn(ctl, slot, now, LIFE)?;
            }
        }
        self.live.retain(|l| now <= l.end + GROUP as u64);
        for l in &mut self.live {
            if let Some((tx, src, pos)) = &mut l.ring {
                top_up(tx, src, pos);
            }
            if let Some(h) = &l.handle {
                let need = (now.saturating_sub(l.start) + 2 * GROUP as u64).min(l.life);
                match h.wait_written(need, FAILSAFE) {
                    Ok(true) => {}
                    other => {
                        return Err(format!(
                            "stream stalled: written {} need {need} result {other:?} err {:?}",
                            h.frames_written(),
                            h.error()
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

fn run(block: usize, feed: Feed) -> Result<Stats, String> {
    let cfg = MixerConfig {
        sample_rate: RATE,
        output_channels: DEVICE_OUTPUTS,
        max_block_frames: 512,
        max_voices: MAX_VOICES,
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
    let mut w = Workload::new(feed).map_err(|e| format!("setup: {e}"))?;
    w.start_all(&mut ctl).map_err(|e| format!("setup: {e}"))?;

    let mut out = vec![0.0f32; GROUP * DEVICE_OUTPUTS];
    let mut times = Vec::with_capacity(MEASURED_GROUPS * (GROUP / block));
    let (mut ids, mut peaks) = (
        vec![VoiceId(0); ctl.voice_meter_capacity()],
        vec![0.0; ctl.voice_meter_capacity() * DEVICE_OUTPUTS],
    );
    let mut outs = [0.0f32; DEVICE_OUTPUTS];
    let mut measured = Duration::ZERO;
    let mut wall = Duration::ZERO;
    let mut finished = Vec::new();
    for g in 0..WARMUP_GROUPS + MEASURED_GROUPS {
        let wall_start = Instant::now();
        let now = ctl.samples_rendered();
        // Control-thread work, untimed: replace and feed voices, ramps, meters, events.
        w.service(&mut ctl, now)
            .map_err(|e| format!("group {g}: {e}"))?;
        // Every second, 600 ms gain and crosspoint ramps on every voice: ramps in flight for
        // 60 % of the run, with the rest on the de-zipper's settled path. They cycle through
        // the curves by slot and run on top of the per-voice fade-in/out.
        if g % 4 == 0 {
            let up = (g / 4) % 2 == 0;
            for slot in 0..VOICES as usize {
                let voice = w.slot_voice[slot];
                let fade = Fade::new(RATE as u64 * 6 / 10, CURVES[(slot + g / 4) % 5]);
                let (a, b) = if up { (1.0, 0.6) } else { (0.5, 1.0) };
                ctl.send(
                    now,
                    CommandKind::Ramp {
                        voice,
                        target: GainTarget::CueOutput(0),
                        gain: a,
                        fade,
                    },
                );
                ctl.send(
                    now,
                    CommandKind::Ramp {
                        voice,
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
            if let Event::VoiceFinished { voice, .. } = e {
                finished.push(voice);
            }
        }
        if let Some(v) = finished.iter().find(|v| w.slot_voice.contains(v)) {
            return Err(format!("voice {} ended before its replacement", v.0));
        }
    }
    if ctl.faults() != 0 {
        return Err(format!("{} faults", ctl.faults()));
    }
    if !out.iter().any(|s| *s != 0.0) || !out.iter().all(|s| s.is_finite()) {
        return Err("the mix is silent or not finite".into());
    }
    let audio_s = (MEASURED_GROUPS * GROUP) as f64 / f64::from(RATE);
    times.sort_by(|a, b| a.total_cmp(b));
    let pct = |p: f64| times[((times.len() - 1) as f64 * p) as usize];
    Ok(Stats {
        ratio: audio_s / measured.as_secs_f64(),
        p50_us: pct(0.5),
        p99_us: pct(0.99),
        max_us: times[times.len() - 1],
        underrun_frames: ctl.underrun_frames(),
        wall_ratio: audio_s / (measured + wall).as_secs_f64(),
        voices_started: w.started,
    })
}

fn report(name: &str, block: usize, s: &Stats) {
    println!(
        "{name} block={block:>3}: {:.1}x real time (render only), p50 {:.1} us  p99 {:.1} us  max {:.1} us  (budget per block {:.0} us), underrun frames {}, voices started {}, incl. control/feed work {:.1}x",
        s.ratio,
        s.p50_us,
        s.p99_us,
        s.max_us,
        block as f64 / f64::from(RATE) * 1e6,
        s.underrun_frames,
        s.voices_started,
        s.wall_ratio
    );
}

/// Report-only measurement: a stall or a failed check is printed, never asserted.
fn report_only(name: &str, block: usize, feed: Feed) {
    match run(block, feed) {
        Ok(s) => report(name, block, &s),
        Err(e) => println!("{name} block={block:>3}: not measured: {e}"),
    }
}

#[test]
#[ignore = "wall-clock CPU budget gate (NFR-23, QA-06); run with --release -- --ignored --nocapture"]
fn sixty_four_stereo_voices_with_fades_render_at_least_ten_times_real_time() {
    if cfg!(debug_assertions) {
        eprintln!("cpu_budget: skipped in a debug build; use `cargo test --release`");
        return;
    }
    // The gate first, so nothing else can pre-empt it. Best of three runs at block 512: a
    // noisy neighbour rarely slows all three, a real regression slows every one.
    let mut best: f64 = 0.0;
    for attempt in 1..=GATE_RUNS {
        let s = run(512, Feed::Rings).expect("the gated workload must run cleanly");
        report(&format!("rings  #{attempt}"), 512, &s);
        assert_eq!(
            s.underrun_frames, 0,
            "a starved voice would flatter the ratio"
        );
        best = best.max(s.ratio);
        if best >= MIN_REALTIME_RATIO {
            break;
        }
    }
    println!("gate: best {best:.1}x, budget {MIN_REALTIME_RATIO}x");
    // Reported only, after the gate: smaller blocks cost more per second of audio, and
    // resampling runs on the disk workers, not the rendering thread.
    report_only("rings    ", 256, Feed::Rings);
    report_only("rings    ", 64, Feed::Rings);
    report_only("resampled", 512, Feed::Resampled);
    assert!(
        best >= MIN_REALTIME_RATIO,
        "mixer render cost regressed: best of {GATE_RUNS} runs {best:.1}x real time, budget {MIN_REALTIME_RATIO}x (NFR-23, QA-06)"
    );
}
