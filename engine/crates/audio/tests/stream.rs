// SPDX-License-Identifier: GPL-3.0-or-later

//! Disk streaming end to end through the offline renderer (AUD-04, AUD-05, AUD-11, AUD-12,
//! CT-16): decode → resample → ring → mixer voice. Tests wait on stream signals (primed, frames
//! written, devamp cut), never on timers; the timeouts are only failsafes.

mod common;

use std::time::Duration;

use common::media::*;
use common::*;
use openlab_audio::{
    CommandKind, DiskStreamer, Event, FinishReason, Media, MediaError, MediaInfo, MixerController,
    OfflineRenderer, Plays, Slice, StreamHandle, StreamRequest, StreamerConfig, VoiceId, plan,
    preload, probe,
};

const SR: u32 = 48_000;
const BLOCK: usize = 256;
const FAILSAFE: Duration = Duration::from_secs(30);

fn streamer(buffer_ms: u32) -> DiskStreamer {
    let mut c = StreamerConfig::new(SR);
    c.buffer_ms = buffer_ms;
    DiskStreamer::new(c).unwrap()
}

/// A mono 48 kHz float WAV whose frame `i` holds `marker(i)`.
fn marker_file(name: &str, frames: u64) -> (Media, MediaInfo) {
    let path = temp_file(name, &wav(&markers(frames), 1, SR, Pcm::F32));
    let info = probe(&path).unwrap();
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
    let p = d
        .prepare(StreamRequest {
            media,
            info,
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
        r.render_into(&mut out[done * ch..(done + n) * ch], &[n]);
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
    assert!(out[..at].iter().all(|&s| s == 0.0), "audio before the start");
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
        r.render_into(&mut out, &[BLOCK]);
        for (j, &s) in out.iter().enumerate() {
            let p = t + j;
            let want = if p < 200 { p } else { 200 + (p - 200) % 100 };
            assert_eq!(s, marker(want as u64), "frame {p}");
        }
        for e in events(&mut ctl) {
            match e {
                Event::SliceEnded { at: 200, slice: 0, .. } => {}
                Event::LoopReached { at, slice: 1, count, .. } => {
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
    ctl.send(devamp_at, CommandKind::Devamp { voice: VoiceId(1), stop });
    // The mixer takes the command in the next block and publishes the cut; wait for the disk.
    out.extend(render_synced(&mut r, &mut ctl, &h, BLOCK, 0));
    if !stop {
        assert!(h.wait_cuts(1, FAILSAFE).unwrap());
        assert!(h.wait_written(u64::MAX, FAILSAFE).unwrap(), "continuation not written");
    }
    let rest = 5_000 - out.len();
    out.extend(r.render(rest, &[BLOCK]));
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
    let mut out = r.render(1_500, &[BLOCK]);
    ctl.send(1_500, CommandKind::Devamp { voice: VoiceId(1), stop: false });
    out.extend(r.render(BLOCK, &[BLOCK]));
    assert!(h.wait_cuts(1, FAILSAFE).unwrap());
    assert!(h.wait_written(u64::MAX, FAILSAFE).unwrap());
    out.extend(r.render(5_000 - out.len(), &[BLOCK]));
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
    ctl.send(1_280, CommandKind::Devamp { voice: VoiceId(1), stop: false });
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
    // Play past what the stalled disk could deliver.
    let mut out = r.render(20_000, &[BLOCK]);
    let ev = events(&mut ctl);
    let gap_at = match ev.as_slice() {
        [Event::Underrun { at, .. }] => *at,
        other => panic!("expected one underrun, got {other:?}"),
    };
    assert!(gap_at > 0 && gap_at < 20_000);
    gate.open();
    assert!(h.wait_written(frames, FAILSAFE).unwrap());
    out.extend(r.render(40_000, &[BLOCK]));
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
    let path = temp_file("preload.wav", &wav(&tone(44_100, 44_100), 2, 44_100, Pcm::I16));
    let info = probe(&path).unwrap();
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
    let path = temp_file("resample.wav", &wav(&sine(44_100, 44_100, 1000.0, 0.5), 1, 44_100, Pcm::F32));
    let info = probe(&path).unwrap();
    let total = info.frames_at(SR);
    assert_eq!(total, 48_000);
    let (mut ctl, mut r) = setup(config(1));
    let mut d = streamer(100);
    let h = start_stream(&mut d, &mut ctl, Media::Path(path), info, plan(0..total, &[], &[], 0).unwrap(), 0);
    let out = render_synced(&mut r, &mut ctl, &h, 50_000, 0);
    assert!(events(&mut ctl).contains(&finished(48_000, FinishReason::OutPoint)));
    let f = frequency(&out[2_000..46_000], SR);
    assert!((f - 1000.0).abs() < 0.05, "{f} Hz");
}

/// Every format streams from a seek point exactly as a full decode has it there.
#[test]
fn every_format_streams_from_a_seek_point_like_a_full_decode() {
    for name in ["tone.flac", "tone-alac.m4a", "tone.mp3", "tone.m4a", "tone.ogg"] {
        let info = probe(&fixture(name)).unwrap();
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
        let h = start_stream(&mut d, &mut ctl, Media::Path(fixture(name)), info, slices, 0);
        let out = render_synced(&mut r, &mut ctl, &h, 8_000, 0);
        let want = &full.samples()[7_000 * 2..15_000 * 2];
        let err = out
            .iter()
            .zip(want)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        // Lossless decodes are bit-exact after a seek. Lossy decoders restart from a pre-roll,
        // which leaves differences far below audibility (an off-by-one frame would be ~0.05).
        let tol = if name.contains("alac") || name.ends_with(".flac") { 0.0 } else { 1e-3 };
        assert!(err <= tol, "{name}: max error {err}");
    }
}

#[test]
fn stream_errors_are_typed_and_end_the_voice() {
    let (mut ctl, mut r) = setup(config(1));
    let mut d = streamer(100);
    let one = |frames| plan(0..frames, &[], &[], 0).unwrap();

    // Missing file.
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
            slices: one(1000),
        })
        .unwrap();
    assert!(matches!(p.handle.wait_primed(FAILSAFE), Err(MediaError::NotFound(_))));

    // The file no longer matches its probe.
    let (media, mut info) = marker_file("changed.wav", 1000);
    info.channels = 2;
    let p = d
        .prepare(StreamRequest {
            media,
            info,
            slices: one(1000),
        })
        .unwrap();
    assert_eq!(p.handle.wait_primed(FAILSAFE), Err(MediaError::Changed));

    // Corrupt data mid-file: the stream fails and the voice ends instead of hanging.
    let mut bytes = std::fs::read(fixture("tone.flac")).unwrap();
    let mid = bytes.len() / 2;
    for b in &mut bytes[mid..mid + 2000] {
        *b ^= 0x5a;
    }
    let path = temp_file("stream-corrupt.flac", &bytes);
    let info = probe(&path).unwrap();
    let mut c = StreamerConfig::new(44_100);
    c.buffer_ms = 100;
    let mut d44 = DiskStreamer::new(c).unwrap();
    let cfg = openlab_audio::MixerConfig {
        sample_rate: 44_100,
        ..config(2)
    };
    let (mut ctl44, mut r44) = setup(cfg);
    let h = start_stream(&mut d44, &mut ctl44, Media::Path(path), info, one(info.frames), 0);
    let failed = loop {
        match h.wait_written(ctl44.samples_rendered() + BLOCK as u64, FAILSAFE) {
            Ok(true) => r44.render_into(&mut [0.0; BLOCK * 2], &[BLOCK]),
            Ok(false) => panic!("stream stalled"),
            Err(e) => break e,
        }
    };
    assert!(matches!(failed, MediaError::Corrupt(_) | MediaError::Changed), "{failed:?}");
    assert_eq!(h.error(), Some(failed));
    r44.render(30_000, &[BLOCK]);
    assert!(events(&mut ctl44)
        .iter()
        .any(|e| matches!(e, Event::VoiceFinished { reason: FinishReason::EndOfStream, .. })));
    let _ = (&mut ctl, &mut r);
}
