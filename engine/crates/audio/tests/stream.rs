// SPDX-License-Identifier: GPL-3.0-or-later
// Helpers outside #[test] fns are test code too; clippy's in-test allowances miss them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! Disk streaming end to end through the offline renderer (AUD-04, AUD-05, AUD-11, AUD-12,
//! CT-16): decode → resample → ring → mixer voice. Tests wait on stream signals (primed, frames
//! written, devamp cut), never on timers; the timeouts are only failsafes, and every loop is
//! bounded. Every render runs with allocation forbidden.

mod common;

use std::time::Duration;

use common::media::*;
use common::*;
use openlab_audio::{
    CommandKind, DiskStreamer, Event, FinishReason, Media, MediaError, MediaInfo, MediaWarning,
    MixerController, OfflineRenderer, Plays, Seeking, Slice, StreamHandle, StreamRequest,
    StreamerConfig, VoiceId, WarningKind, plan, preload, probe, probe_media,
};

const SR: u32 = 48_000;
const BLOCK: usize = 256;
const FAILSAFE: Duration = Duration::from_secs(10);

/// Renders `frames` (interleaved over the mixer's outputs) with allocation forbidden.
fn render(r: &mut OfflineRenderer, frames: usize) -> Vec<f32> {
    let ch = r.mixer_mut().output_channels();
    let mut out = vec![0.0; frames * ch];
    for chunk in out.chunks_mut(BLOCK * ch) {
        no_alloc(|| r.render_into(chunk, &[chunk.len() / ch]));
    }
    out
}

fn streamer(buffer_ms: u32) -> DiskStreamer {
    let mut c = StreamerConfig::new(SR);
    c.buffer_ms = buffer_ms;
    DiskStreamer::new(c).unwrap()
}

/// A mono 48 kHz float WAV whose frame `i` holds `marker(i)`.
fn marker_file(name: &str, frames: u64) -> (Media, MediaInfo) {
    let path = temp_file(name, &wav(&markers(frames), 1, SR, Pcm::F32));
    let info = probe(&path).unwrap().info;
    assert_eq!(info.frames, frames);
    (Media::Path(path), info)
}

/// Prepares a stream, waits until it is primed and schedules it as voice 1 at `at`, routed to
/// output 0.
fn start_stream(
    d: &mut DiskStreamer,
    ctl: &mut MixerController,
    media: Media,
    info: MediaInfo,
    slices: Vec<Slice>,
    at: u64,
) -> StreamHandle {
    start_warned(d, ctl, media, (info, Seeking::Direct), slices, at)
}

/// As `start_stream`, seeking as the probe found.
fn start_warned(
    d: &mut DiskStreamer,
    ctl: &mut MixerController,
    media: Media,
    (info, seeking): (MediaInfo, Seeking),
    slices: Vec<Slice>,
    at: u64,
) -> StreamHandle {
    let p = d
        .prepare(StreamRequest {
            media,
            info,
            seeking,
            slices,
        })
        .unwrap();
    assert!(p.handle.wait_primed(FAILSAFE).unwrap());
    let (mut v, h) = p.into_voice(VoiceId(1), ctl.cue_outputs());
    for i in 0..usize::from(v.channels) {
        v.set_crosspoint(i, i.min(ctl.cue_outputs() - 1), 1.0);
    }
    send_start(ctl, at, v);
    h
}

/// Renders `frames` in blocks, first waiting each time until the stream has written `lead`
/// frames beyond what the voice will have consumed (so a test never races the disk thread).
fn render_synced(
    r: &mut OfflineRenderer,
    ctl: &mut MixerController,
    h: &StreamHandle,
    frames: usize,
    lead: u64,
) -> Vec<f32> {
    let ch = r.mixer_mut().output_channels();
    let mut out = vec![0.0; frames * ch];
    let mut done = 0;
    while done < frames {
        let n = BLOCK.min(frames - done);
        let need = ctl.samples_rendered() + n as u64 + lead;
        assert!(h.wait_written(need, FAILSAFE).unwrap(), "stream stalled");
        no_alloc(|| r.render_into(&mut out[done * ch..(done + n) * ch], &[n]));
        ctl.pump();
        done += n;
    }
    out
}

/// Unrolls a finite slice table independently of the code under test.
fn unrolled(slices: &[Slice]) -> Vec<f32> {
    let mut v = Vec::new();
    for s in slices {
        let Plays::Times(n) = s.plays else {
            panic!("finite tables only")
        };
        for _ in 0..n {
            v.extend((s.start..s.start + s.frames).map(marker));
        }
    }
    v
}

fn assert_plays(out: &[f32], at: usize, want: &[f32]) {
    assert!(
        out[..at].iter().all(|&s| s == 0.0),
        "audio before the start"
    );
    for (i, (g, w)) in out[at..at + want.len()].iter().zip(want).enumerate() {
        assert_eq!(g, w, "frame {i} after the start");
    }
    assert!(
        out[at + want.len()..].iter().all(|&s| s == 0.0),
        "audio after the end"
    );
}

fn slice_ended(at: u64, slice: u32) -> Event {
    Event::SliceEnded {
        voice: VoiceId(1),
        at,
        slice,
    }
}

fn looped(at: u64, slice: u32, count: u64) -> Event {
    Event::LoopReached {
        voice: VoiceId(1),
        at,
        slice,
        count,
    }
}

fn finished(at: u64, reason: FinishReason) -> Event {
    Event::VoiceFinished {
        voice: VoiceId(1),
        at,
        reason,
    }
}

#[test]
fn trimmed_start_and_end_are_sample_exact() {
    let (media, info) = marker_file("trim.wav", 20_000);
    let (mut ctl, mut r) = setup(config(1));
    // A ring smaller than the trimmed region, so the worker refills while playing.
    let mut d = streamer(50);
    let slices = plan(1000..15_000, &[], &[], 1000).unwrap();
    let h = start_stream(&mut d, &mut ctl, media, info, slices.clone(), 100);
    let out = render_synced(&mut r, &mut ctl, &h, 15_000, 0);
    assert_plays(&out, 100, &unrolled(&slices));
    assert_eq!(
        events(&mut ctl),
        vec![
            slice_ended(14_100, 0),
            finished(14_100, FinishReason::OutPoint)
        ]
    );
    assert_eq!(ctl.underrun_frames(), 0);
}

#[test]
fn start_offset_plays_from_the_offset() {
    let (media, info) = marker_file("offset.wav", 20_000);
    let (mut ctl, mut r) = setup(config(1));
    let mut d = streamer(50);
    let slices = plan(1000..15_000, &[], &[], 6_789).unwrap();
    let h = start_stream(&mut d, &mut ctl, media, info, slices, 0);
    let out = render_synced(&mut r, &mut ctl, &h, 9_000, 0);
    let want: Vec<f32> = (6_789..15_000).map(marker).collect();
    assert_plays(&out, 0, &want);
}

#[test]
fn slice_loops_n_times_then_continues() {
    let (media, info) = marker_file("loops.wav", 4_000);
    let (mut ctl, mut r) = setup(config(1));
    let mut d = streamer(50);
    let plays = [Plays::Times(1), Plays::Times(3), Plays::Times(1)];
    let slices = plan(0..3_000, &[1_000, 2_000], &plays, 0).unwrap();
    let h = start_stream(&mut d, &mut ctl, media, info, slices.clone(), 10);
    let out = render_synced(&mut r, &mut ctl, &h, 6_000, 0);
    assert_plays(&out, 10, &unrolled(&slices));
    assert_eq!(
        events(&mut ctl),
        vec![
            slice_ended(1_010, 0),
            looped(2_010, 1, 1),
            looped(3_010, 1, 2),
            slice_ended(4_010, 1),
            slice_ended(5_010, 2),
            finished(5_010, FinishReason::OutPoint),
        ]
    );
}

#[test]
fn loop_from_a_start_offset_inside_the_loop() {
    let (media, info) = marker_file("offset-loop.wav", 4_000);
    let (mut ctl, mut r) = setup(config(1));
    let mut d = streamer(50);
    let plays = [Plays::Times(1), Plays::Times(2), Plays::Times(1)];
    let slices = plan(0..3_000, &[1_000, 2_000], &plays, 1_500).unwrap();
    let h = start_stream(&mut d, &mut ctl, media, info, slices, 0);
    let out = render_synced(&mut r, &mut ctl, &h, 4_000, 0);
    let want: Vec<f32> = (1_500..2_000).chain(1_000..3_000).map(marker).collect();
    assert_plays(&out, 0, &want);
    assert_eq!(
        events(&mut ctl),
        vec![
            looped(500, 1, 1),
            slice_ended(1_500, 1),
            slice_ended(2_500, 2),
            finished(2_500, FinishReason::OutPoint),
        ]
    );
}

fn infinite_loop_without_drift(frames: usize) {
    let (media, info) = marker_file("vamp.wav", 1_000);
    let (mut ctl, mut r) = setup(config(1));
    let mut d = streamer(100);
    let plays = [Plays::Times(1), Plays::Infinite, Plays::Times(1)];
    let slices = plan(0..1_000, &[200, 300], &plays, 0).unwrap();
    let h = start_stream(&mut d, &mut ctl, media, info, slices, 0);
    let mut out = vec![0.0; BLOCK];
    let mut loops = 0u64;
    let mut t = 0usize;
    while t < frames {
        assert!(h.wait_written(t as u64 + BLOCK as u64, FAILSAFE).unwrap());
        no_alloc(|| r.render_into(&mut out, &[BLOCK]));
        for (j, &s) in out.iter().enumerate() {
            let p = t + j;
            let want = if p < 200 { p } else { 200 + (p - 200) % 100 };
            assert_eq!(s, marker(want as u64), "frame {p}");
        }
        for e in events(&mut ctl) {
            match e {
                Event::SliceEnded {
                    at: 200, slice: 0, ..
                } => {}
                Event::LoopReached {
                    at,
                    slice: 1,
                    count,
                    ..
                } => {
                    loops += 1;
                    assert_eq!((count, at), (loops, 200 + 100 * loops), "loop drifted");
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        t += BLOCK;
        ctl.pump();
    }
    assert_eq!(loops, (t as u64 - 200) / 100);
    // Every play after the first came from the loop cache, not a seek.
    assert!(h.cached_frames() >= t as u64 - 300);
    assert_eq!(ctl.underrun_frames(), 0);
    assert_eq!(ctl.dropped_events(), 0);
    // The ring is fixed: the stream never runs further ahead than its buffer.
    assert!(h.frames_written() <= frames as u64 + 4_800 + 4_096);
}

#[test]
fn infinite_loop_runs_many_iterations_without_drift() {
    infinite_loop_without_drift(1_000_000);
}

/// One simulated hour; run in release with `--ignored`.
#[test]
#[ignore = "long soak"]
fn infinite_loop_runs_for_an_hour_without_drift() {
    infinite_loop_without_drift(3600 * SR as usize);
}

/// Devamp tests: slice 1 (1000..2000) loops forever between an intro and an outro. With the
/// default 2 s ring, many stale loop plays are already buffered when the devamp arrives.
fn vamp(stop: bool, devamp_at: u64) -> (Vec<f32>, Vec<Event>) {
    let (media, info) = marker_file("devamp.wav", 4_000);
    let (mut ctl, mut r) = setup(config(1));
    let mut d = streamer(2_000);
    let plays = [Plays::Times(1), Plays::Infinite, Plays::Times(1)];
    let slices = plan(0..3_000, &[1_000, 2_000], &plays, 0).unwrap();
    let h = start_stream(&mut d, &mut ctl, media, info, slices, 0);
    // Let the ring fill, so the cut has to drop stale plays.
    assert!(h.wait_written(90_000, FAILSAFE).unwrap());
    let mut out = render_synced(&mut r, &mut ctl, &h, devamp_at as usize, 0);
    ctl.send(
        devamp_at,
        CommandKind::Devamp {
            voice: VoiceId(1),
            stop,
        },
    );
    // The mixer takes the command in the next block and publishes the cut; wait for the disk.
    out.extend(render_synced(&mut r, &mut ctl, &h, BLOCK, 0));
    if !stop {
        assert!(h.wait_cuts(1, FAILSAFE).unwrap());
        assert!(
            h.wait_written(u64::MAX, FAILSAFE).unwrap(),
            "continuation not written"
        );
    }
    let rest = 5_000 - out.len();
    out.extend(render(&mut r, rest));
    (out, events(&mut ctl))
}

#[test]
fn devamp_continues_exactly_at_the_end_of_the_current_play() {
    let (out, ev) = vamp(false, 1_250);
    let want: Vec<f32> = (0..3_000).map(marker).collect();
    assert_plays(&out, 0, &want);
    assert_eq!(
        ev,
        vec![
            slice_ended(1_000, 0),
            Event::DevampScheduled {
                voice: VoiceId(1),
                at: 2_000
            },
            slice_ended(2_000, 1),
            slice_ended(3_000, 2),
            finished(3_000, FinishReason::OutPoint),
        ]
    );
}

#[test]
fn devamp_in_a_later_play_exits_at_that_play_end() {
    let (out, ev) = vamp(false, 2_600);
    let want: Vec<f32> = (0..2_000).chain(1_000..3_000).map(marker).collect();
    assert_plays(&out, 0, &want);
    assert!(ev.contains(&looped(2_000, 1, 1)));
    assert!(ev.contains(&slice_ended(3_000, 1)));
    assert!(ev.contains(&finished(4_000, FinishReason::OutPoint)));
}

#[test]
fn devamp_stop_ends_the_voice_at_the_end_of_the_current_play() {
    let (out, ev) = vamp(true, 1_250);
    let want: Vec<f32> = (0..2_000).map(marker).collect();
    assert_plays(&out, 0, &want);
    assert_eq!(
        ev,
        vec![
            slice_ended(1_000, 0),
            Event::DevampScheduled {
                voice: VoiceId(1),
                at: 2_000
            },
            slice_ended(2_000, 1),
            finished(2_000, FinishReason::OutPoint),
        ]
    );
}

#[test]
fn devamp_cuts_the_remaining_plays_of_a_finite_loop() {
    let (media, info) = marker_file("devamp-n.wav", 4_000);
    let (mut ctl, mut r) = setup(config(1));
    let mut d = streamer(2_000);
    let plays = [Plays::Times(1), Plays::Times(5), Plays::Times(1)];
    let slices = plan(0..3_000, &[1_000, 2_000], &plays, 0).unwrap();
    let h = start_stream(&mut d, &mut ctl, media, info, slices, 0);
    assert!(h.wait_written(u64::MAX, FAILSAFE).unwrap());
    let mut out = render(&mut r, 1_500);
    ctl.send(
        1_500,
        CommandKind::Devamp {
            voice: VoiceId(1),
            stop: false,
        },
    );
    out.extend(render(&mut r, BLOCK));
    assert!(h.wait_cuts(1, FAILSAFE).unwrap());
    assert!(h.wait_written(u64::MAX, FAILSAFE).unwrap());
    out.extend(render(&mut r, 5_000 - out.len()));
    let want: Vec<f32> = (0..3_000).map(marker).collect();
    assert_plays(&out, 0, &want);
    assert!(events(&mut ctl).contains(&finished(3_000, FinishReason::OutPoint)));
}

#[test]
fn devamp_before_the_ring_reaches_the_boundary_needs_no_skip() {
    let (media, info) = marker_file("devamp-long.wav", 10_000);
    let (mut ctl, mut r) = setup(config(1));
    // 4096-frame ring: the 8000-frame loop end is not buffered yet when the devamp arrives.
    let mut d = streamer(50);
    let plays = [Plays::Times(1), Plays::Infinite, Plays::Times(1)];
    let slices = plan(0..10_000, &[1_000, 9_000], &plays, 0).unwrap();
    let h = start_stream(&mut d, &mut ctl, media, info, slices, 0);
    let mut out = render_synced(&mut r, &mut ctl, &h, 1_280, 0);
    ctl.send(
        1_280,
        CommandKind::Devamp {
            voice: VoiceId(1),
            stop: false,
        },
    );
    out.extend(render_synced(&mut r, &mut ctl, &h, BLOCK, 0));
    out.extend(render_synced(&mut r, &mut ctl, &h, 12_000 - out.len(), 0));
    assert!(h.cuts() == 1);
    let want: Vec<f32> = (0..10_000).map(marker).collect();
    assert_plays(&out, 0, &want);
    assert_eq!(ctl.underrun_frames(), 0);
}

#[test]
fn slow_disk_underrun_recovers_where_it_left_off() {
    let frames = 40_000u64;
    let bytes = wav(&markers(frames), 1, SR, Pcm::F32);
    let len = bytes.len() as u64;
    let gate = Gate::default();
    // Everything after the first ~10000 frames is held back, except the tail (metadata probes).
    let reader = GatedReader::new(bytes, 44 + 10_000 * 4, len - 1024, gate.clone());
    let info = MediaInfo {
        channels: 1,
        sample_rate: SR,
        frames,
        duration: Duration::from_secs_f64(frames as f64 / f64::from(SR)),
    };
    let (mut ctl, mut r) = setup(config(1));
    let mut d = streamer(2_000);
    let slices = plan(0..frames, &[], &[], 0).unwrap();
    let h = start_stream(
        &mut d,
        &mut ctl,
        Media::Reader(Box::new(reader), Some("wav".into())),
        info,
        slices,
        0,
    );
    // The worker writes whole 4096-frame chunks; the third needs data behind the gate.
    assert!(h.wait_written(8_192, FAILSAFE).unwrap());
    assert_eq!(h.frames_written(), 8_192);
    // Play past what the stalled disk could deliver.
    let mut out = render(&mut r, 20_000);
    let ev = events(&mut ctl);
    let gap_at = match ev.as_slice() {
        [Event::Underrun { at, .. }] => *at,
        other => panic!("expected one underrun, got {other:?}"),
    };
    assert_eq!(gap_at, 8_192);
    gate.open();
    assert!(h.wait_written(frames, FAILSAFE).unwrap());
    out.extend(render(&mut r, 40_000));
    // Remove the silent gap: what is left is the whole file, in order, nothing skipped.
    let gap = (20_000 - gap_at) as usize;
    let played: Vec<f32> = out[..gap_at as usize]
        .iter()
        .chain(&out[gap_at as usize + gap..])
        .copied()
        .take(frames as usize)
        .collect();
    assert_eq!(played, markers(frames));
    assert_eq!(ctl.underrun_frames(), gap as u64);
    assert!(events(&mut ctl).contains(&finished(frames + gap as u64, FinishReason::OutPoint)));
}

#[test]
fn preload_plays_the_same_as_streaming() {
    // 44.1 kHz source, so both paths resample; loops make both seek.
    let path = temp_file(
        "preload.wav",
        &wav(&tone(44_100, 44_100), 2, 44_100, Pcm::I16),
    );
    let info = probe(&path).unwrap().info;
    assert_eq!(info.frames_at(SR), 48_000);
    let plays = [Plays::Times(1), Plays::Times(3), Plays::Times(1)];
    let slices = plan(4_000..40_000, &[10_000, 20_000], &plays, 4_000).unwrap();
    let run = |media: Media| {
        let (mut ctl, mut r) = setup(config(2));
        let mut d = streamer(100);
        let h = start_stream(&mut d, &mut ctl, media, info, slices.clone(), 0);
        render_synced(&mut r, &mut ctl, &h, 60_000, 0)
    };
    let streamed = run(Media::Path(path.clone()));
    let pre = preload(Media::Path(path)).unwrap();
    assert_eq!(pre.bytes(), 44_100 * 2 * 4);
    let preloaded = run(Media::Preloaded(pre));
    assert_eq!(streamed.len(), preloaded.len());
    assert!(streamed == preloaded, "preload and streaming differ");
    // 6000 + 3 × 10000 + 20000 frames of tone, then silence.
    assert!(streamed[..56_000 * 2].iter().any(|&s| s != 0.0));
    assert!(streamed[56_000 * 2..].iter().all(|&s| s == 0.0));
}

#[test]
fn resampled_stream_has_exact_length_and_frequency() {
    let path = temp_file(
        "resample.wav",
        &wav(&sine(44_100, 44_100, 1000.0, 0.5), 1, 44_100, Pcm::F32),
    );
    let info = probe(&path).unwrap().info;
    let total = info.frames_at(SR);
    assert_eq!(total, 48_000);
    let (mut ctl, mut r) = setup(config(1));
    let mut d = streamer(100);
    let h = start_stream(
        &mut d,
        &mut ctl,
        Media::Path(path),
        info,
        plan(0..total, &[], &[], 0).unwrap(),
        0,
    );
    let out = render_synced(&mut r, &mut ctl, &h, 50_000, 0);
    assert!(events(&mut ctl).contains(&finished(48_000, FinishReason::OutPoint)));
    let f = frequency(&out[2_000..46_000], SR);
    assert!((f - 1000.0).abs() < 0.05, "{f} Hz");
}

/// Every format streams from a seek point exactly as a full decode has it there.
#[test]
fn every_format_streams_from_a_seek_point_like_a_full_decode() {
    for name in [
        "tone.flac",
        "tone-alac.m4a",
        "tone.mp3",
        "tone.m4a",
        "tone.ogg",
    ] {
        let info = probe(&fixture(name)).unwrap().info;
        let full = preload(Media::Path(fixture(name))).unwrap();
        let mut c = StreamerConfig::new(44_100);
        c.buffer_ms = 100;
        let mut d = DiskStreamer::new(c).unwrap();
        let cfg = openlab_audio::MixerConfig {
            sample_rate: 44_100,
            ..config(2)
        };
        let (mut ctl, mut r) = setup(cfg);
        let slices = plan(7_000..15_000, &[], &[], 7_000).unwrap();
        let h = start_stream(
            &mut d,
            &mut ctl,
            Media::Path(fixture(name)),
            info,
            slices,
            0,
        );
        let out = render_synced(&mut r, &mut ctl, &h, 8_000, 0);
        let want = &full.samples()[7_000 * 2..15_000 * 2];
        let err = out
            .iter()
            .zip(want)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        // Bit-exact, except AAC: ffmpeg's encoder uses noise substitution, which the decoder
        // fills from a pseudo-random sequence that depends on everything decoded before (see
        // `decode::tests`). An off-by-one frame would be over 0.03.
        let tol = if name == "tone.m4a" { 1e-3 } else { 0.0 };
        assert!(err <= tol, "{name}: max error {err}");
    }
}

#[test]
fn unplayable_streams_fail_with_a_typed_error() {
    let mut d = streamer(100);
    let one = |frames| plan(0..frames, &[], &[], 0).unwrap();
    let info = MediaInfo {
        channels: 1,
        sample_rate: SR,
        frames: 1000,
        duration: Duration::from_millis(20),
    };
    let p = d
        .prepare(StreamRequest {
            media: Media::Path("/nonexistent/openlab.wav".into()),
            info,
            seeking: Seeking::Direct,
            slices: one(1000),
        })
        .unwrap();
    assert!(matches!(
        p.handle.wait_primed(FAILSAFE),
        Err(MediaError::NotFound(_))
    ));

    // The file no longer matches its probe.
    let (media, mut info) = marker_file("changed.wav", 1000);
    info.channels = 2;
    let p = d
        .prepare(StreamRequest {
            media,
            info,
            seeking: Seeking::Direct,
            slices: one(1000),
        })
        .unwrap();
    assert_eq!(p.handle.wait_primed(FAILSAFE), Err(MediaError::Changed));
}

/// Streams `media` (probed as `info`) as voice 1 from sample 0 until the stream fails. Then
/// checks the voice played exactly what was written, ended on the next sample with
/// `EndOfStream`, and that this took a bounded number of blocks. Returns the error.
fn play_until_failure(media: Media, info: MediaInfo, device_rate: u32) -> MediaError {
    let mut c = StreamerConfig::new(device_rate);
    c.buffer_ms = 100;
    let mut d = DiskStreamer::new(c).unwrap();
    let cfg = openlab_audio::MixerConfig {
        sample_rate: device_rate,
        ..config(1)
    };
    let (mut ctl, mut r) = setup(cfg);
    let total = info.frames_at(device_rate);
    let h = start_stream(
        &mut d,
        &mut ctl,
        media,
        info,
        plan(0..total, &[], &[], 0).unwrap(),
        0,
    );
    let mut out = Vec::new();
    let mut failed = None;
    for _ in 0..total / BLOCK as u64 + 2 {
        match h.wait_written(ctl.samples_rendered() + BLOCK as u64, FAILSAFE) {
            Ok(true) => out.extend(render(&mut r, BLOCK)),
            Ok(false) => panic!("stream stalled"),
            Err(e) => {
                failed = Some(e);
                break;
            }
        }
    }
    let failed = failed.expect("the stream failed");
    assert_eq!(h.error(), Some(failed.clone()));
    // The stream is gone: the voice plays out what is in the ring, then ends.
    let written = h.frames_written();
    assert!(written < total);
    let left = (written - ctl.samples_rendered()) as usize;
    out.extend(render(&mut r, left + BLOCK));
    let ev = events(&mut ctl);
    assert!(
        ev.contains(&finished(written, FinishReason::EndOfStream)),
        "{ev:?}"
    );
    assert!(out[written as usize..].iter().all(|&s| s == 0.0));
    assert_eq!(ctl.underrun_frames(), 0);
    failed
}

#[test]
fn read_error_mid_stream_ends_the_voice() {
    let frames = 200_000;
    let bytes = wav(&markers(frames), 1, SR, Pcm::F32);
    let info = probe(&temp_file("read-error.wav", &bytes)).unwrap().info;
    let at = 44 + 100_000 * 4;
    let reader = FailingReader::new(bytes, at, Failure::Error);
    let e = play_until_failure(
        Media::Reader(Box::new(reader), Some("wav".into())),
        info,
        SR,
    );
    assert!(matches!(e, MediaError::Io(_)), "{e:?}");
}

#[test]
fn decoder_panic_mid_stream_is_contained() {
    let frames = 200_000;
    let bytes = wav(&markers(frames), 1, SR, Pcm::F32);
    let info = probe(&temp_file("panic.wav", &bytes)).unwrap().info;
    let reader = FailingReader::new(bytes, 44 + 100_000 * 4, Failure::Panic);
    let e = play_until_failure(
        Media::Reader(Box::new(reader), Some("wav".into())),
        info,
        SR,
    );
    assert!(matches!(e, MediaError::Corrupt(_)), "{e:?}");
    // The worker survived: it still streams.
    let mut d = streamer(100);
    let (media, info) = marker_file("after-panic.wav", 1000);
    let p = d
        .prepare(StreamRequest {
            media,
            info,
            seeking: Seeking::Direct,
            slices: plan(0..1000, &[], &[], 0).unwrap(),
        })
        .unwrap();
    assert!(p.handle.wait_written(u64::MAX, FAILSAFE).unwrap());
}

#[test]
fn file_truncated_after_probe_ends_the_voice() {
    for (rate, device_rate) in [(SR, SR), (44_100, SR)] {
        let frames = 200_000;
        let bytes = wav(&markers(frames), 1, rate, Pcm::F32);
        let path = temp_file("shrinks.wav", &bytes);
        let info = probe(&path).unwrap().info;
        // The header still claims every frame; the data stops half way.
        std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        let e = play_until_failure(Media::Path(path), info, device_rate);
        assert!(
            matches!(e, MediaError::Truncated { expected: 200_000, actual } if actual < 110_000),
            "{rate}: {e:?}"
        );
    }
}

/// The FLAC fixture with one whole frame (8192..12288) spliced out.
fn spliced_flac() -> Vec<u8> {
    let bytes = std::fs::read(fixture("tone.flac")).unwrap();
    let syncs: Vec<usize> = bytes
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w == &[0xFF, 0xF8])
        .map(|(i, _)| i)
        .collect();
    bytes[..syncs[2]]
        .iter()
        .chain(&bytes[syncs[3]..])
        .copied()
        .collect()
}

fn missing(at: u64, frames: u64) -> MediaWarning {
    MediaWarning {
        kind: WarningKind::Missing,
        at,
        frames,
    }
}

/// Renders `frames` at 44.1 kHz from a stream started at 0, keeping a lead on the disk.
fn play_44k(
    media: Media,
    probed: (MediaInfo, Seeking),
    slices: Vec<Slice>,
    frames: usize,
) -> (Vec<f32>, Vec<Event>, StreamHandle) {
    let mut c = StreamerConfig::new(44_100);
    c.buffer_ms = 100;
    let mut d = DiskStreamer::new(c).unwrap();
    let cfg = openlab_audio::MixerConfig {
        sample_rate: 44_100,
        ..config(2)
    };
    let (mut ctl, mut r) = setup(cfg);
    let h = start_warned(&mut d, &mut ctl, media, probed, slices, 0);
    let out = render_synced(&mut r, &mut ctl, &h, frames, 0);
    (out, events(&mut ctl), h)
}

/// Damage first met mid-stream (the file changed after its probe): the decoder treats it as the
/// probe would, silence for the missing frames, and reports it on the handle. The voice plays
/// on to its out point: never silent and unreported, never a hang.
#[test]
fn damage_first_found_mid_stream_plays_as_silence_and_is_reported() {
    let path = temp_file(
        "damaged.flac",
        &std::fs::read(fixture("tone.flac")).unwrap(),
    );
    let info = probe(&path).unwrap().info;
    std::fs::write(&path, spliced_flac()).unwrap();
    let clean = preload(Media::Path(fixture("tone.flac"))).unwrap();
    let slices = plan(0..info.frames, &[], &[], 0).unwrap();
    let (out, ev, h) = play_44k(Media::Path(path), (info, Seeking::Direct), slices, 23_000);
    assert_eq!(h.error(), None);
    assert_eq!(h.warnings(), vec![missing(8_192, 4_096)]);
    let c = clean.samples();
    assert!(out[..8_192 * 2] == c[..8_192 * 2], "before the damage");
    assert!(
        out[8_192 * 2..12_288 * 2].iter().all(|&s| s == 0.0),
        "silence"
    );
    assert!(
        out[12_288 * 2..22_050 * 2] == c[12_288 * 2..],
        "after the damage"
    );
    assert!(
        ev.contains(&finished(22_050, FinishReason::OutPoint)),
        "{ev:?}"
    );
}

/// A file probed with damage streams on the probe's timeline, also through loops that seek
/// into, across and out of the damaged region: sample for sample its preload.
#[test]
fn damaged_files_stream_and_loop_on_the_probe_timeline() {
    let mut ogg = std::fs::read(fixture("tone-4s.ogg")).unwrap();
    for x in &mut ogg[10_888..10_904] {
        *x ^= 0x5a;
    }
    for (ext, bytes, marks) in [
        ("flac", spliced_flac(), [6_000u64, 10_000, 14_000]),
        ("ogg", ogg, [40_000, 100_000, 140_000]),
    ] {
        let media = || {
            Media::Reader(
                Box::new(std::io::Cursor::new(bytes.clone())),
                Some(ext.into()),
            )
        };
        let probed = probe_media(media()).unwrap();
        assert_eq!(probed.warnings.len(), 1, "{ext}");
        let pre = preload(media()).unwrap();
        let total = probed.info.frames;
        // Intro, a twice-played slice that starts before the damage and ends inside it, a
        // twice-played slice from inside it to after it, then the rest.
        let plays = [
            Plays::Times(1),
            Plays::Times(2),
            Plays::Times(2),
            Plays::Times(1),
        ];
        let slices = plan(0..total, &marks, &plays, 0).unwrap();
        let mut want = Vec::new();
        for s in &slices {
            let Plays::Times(n) = s.plays else {
                panic!("finite tables only")
            };
            for _ in 0..n {
                want.extend_from_slice(
                    &pre.samples()[s.start as usize * 2..(s.start + s.frames) as usize * 2],
                );
            }
        }
        let frames = want.len() / 2 + 1_000;
        let (out, _, h) = play_44k(
            media(),
            (probed.info, probed.seeking.clone()),
            slices,
            frames,
        );
        assert_eq!(h.error(), None, "{ext}");
        if let Some(i) = out.iter().zip(&want).position(|(a, b)| a != b) {
            panic!("{ext}: sample {i} is {} not {}", out[i], want[i]);
        }
        assert!(out[want.len()..].iter().all(|&s| s == 0.0));
    }
}

/// a devamp that leaves less audio than the integrated fade-out restarts the fade
/// from the gain the voice had, so the gain never jumps, and it still ends exactly at silence on
/// the last frame.
#[test]
fn fade_out_stays_continuous_when_a_devamp_shortens_the_end() {
    // Constant 0.5: the output is the envelope. Intro 0..1000, loop 1000..2000, outro 2000..4000.
    let path = temp_file("level.wav", &wav(&[0.5; 4_000], 1, SR, Pcm::F32));
    let info = probe(&path).unwrap().info;
    for (plays, devamp_at, fade) in [
        // Out of an infinite loop (no fade before) into a 2000-frame outro, fade 4000.
        (Plays::Infinite, 1_250, 4_000),
        // Out of the 2nd of 5 plays while the fade-out (6000) is already running.
        (Plays::Times(5), 1_250, 6_000),
    ] {
        let (mut ctl, mut r) = setup(config(1));
        let mut d = streamer(2_000);
        let slices = plan(
            0..4_000,
            &[1_000, 2_000],
            &[Plays::Times(1), plays, Plays::Times(1)],
            0,
        )
        .unwrap();
        let p = d
            .prepare(StreamRequest {
                media: Media::Path(path.clone()),
                info,
                seeking: Seeking::Direct,
                slices,
            })
            .unwrap();
        assert!(p.handle.wait_primed(FAILSAFE).unwrap());
        let (mut v, h) = p.into_voice(VoiceId(1), ctl.cue_outputs());
        v.set_crosspoint(0, 0, 1.0);
        v.fade_out = openlab_audio::Fade::new(fade, openlab_audio::Curve::Linear);
        send_start(&mut ctl, 0, v);
        assert!(h.wait_written(20_000, FAILSAFE).unwrap());
        let mut out = render(&mut r, devamp_at as usize);
        ctl.send(
            devamp_at,
            CommandKind::Devamp {
                voice: VoiceId(1),
                stop: false,
            },
        );
        out.extend(render(&mut r, BLOCK));
        assert!(h.wait_cuts(1, FAILSAFE).unwrap());
        assert!(h.wait_written(u64::MAX, FAILSAFE).unwrap());
        out.extend(render(&mut r, 5_000 - out.len()));
        // The outro is the last 2000 frames played: 2000..4000. Its last frame is exactly 0.
        assert_eq!(out[3_999], 0.0, "{plays:?}: last frame");
        assert!(out[4_000..].iter().all(|&s| s == 0.0));
        // Never a step bigger than a linear fade from 0.5 over the 2000 frames left could take.
        let max_step = 0.5 / 2_000.0 * 1.01;
        for i in 0..3_999 {
            let step = (out[i + 1] - out[i]).abs();
            assert!(
                step <= max_step,
                "{plays:?}: step {step} at frame {i} ({} -> {})",
                out[i],
                out[i + 1]
            );
        }
    }
}

/// Review H1: a devamp out of an infinite loop whose plays fill the whole ring. The ring is
/// FIFO and full of stale plays, so the continuation cannot be in it at the boundary; the voice
/// plays the continuation's start from the loop exit's buffer while the stale frames are
/// skipped and the worker refills. No waiting on the disk until the exit buffer is nearly used
/// up: then only for the frames the next block needs.
#[test]
fn devamp_from_a_full_ring_never_starves_at_the_exit() {
    const END: u64 = 10_000; // loop end
    let exit = u64::from(SR) * u64::from(openlab_audio::DEFAULT_EXIT_MS) / 1000;
    for devamp_at in [END - 1, END - 4_080, END - 4_096] {
        let (media, info) = marker_file("full-ring.wav", 60_000);
        let (mut ctl, mut r) = setup(config(1));
        let mut d = streamer(2_000);
        let plays = [Plays::Times(1), Plays::Infinite, Plays::Times(1)];
        let slices = plan(0..60_000, &[1_000, END], &plays, 0).unwrap();
        let h = start_stream(&mut d, &mut ctl, media, info, slices, 0);
        // A full ring: 2 s of stale loop plays.
        // As full as the worker fills it (it writes whole 4096-frame chunks).
        assert!(h.wait_written(96_000 - 4_096, FAILSAFE).unwrap());
        let mut out = render(&mut r, devamp_at as usize);
        ctl.send(
            devamp_at,
            CommandKind::Devamp {
                voice: VoiceId(1),
                stop: false,
            },
        );
        // Through the boundary and a block into the exit buffer, with no help from the disk.
        out.extend(render(&mut r, (END - devamp_at) as usize + BLOCK));
        // The ring is full of stale plays and the voice drops them only while rendering, so the
        // worker can write nothing until the voice has rendered after the cut is published.
        // Waiting for continuation frames right now would deadlock whenever the worker is slower
        // than the render (it was, under load): wait only for the cut, which the exit buffer
        // covers however long the worker takes.
        assert!(h.wait_cuts(1, FAILSAFE).unwrap());
        let resume = h.last_resume().unwrap();
        // From here the ring has to supply the continuation: wait only for what each block
        // needs. The first renders drop the stale plays; the exit buffer plays meanwhile.
        while out.len() < 60_000 + 2 * BLOCK {
            let t = out.len() as u64 + BLOCK as u64;
            let need = resume + t.saturating_sub(END + exit);
            assert!(
                h.wait_written(need.min(resume + 50_000 - exit), FAILSAFE)
                    .unwrap()
            );
            out.extend(render(&mut r, BLOCK));
        }
        let want: Vec<f32> = (0..END).chain(END..60_000).map(marker).collect();
        assert_plays(&out, 0, &want);
        assert_eq!(ctl.underrun_frames(), 0, "devamp at {devamp_at}");
        let ev = events(&mut ctl);
        assert!(
            !ev.iter().any(|e| matches!(e, Event::Underrun { .. })),
            "{ev:?}"
        );
        assert!(
            ev.contains(&Event::DevampScheduled {
                voice: VoiceId(1),
                at: END
            }),
            "{ev:?}"
        );
        assert!(ev.contains(&slice_ended(END, 1)));
        assert!(ev.contains(&finished(60_000, FinishReason::OutPoint)));
    }
}
