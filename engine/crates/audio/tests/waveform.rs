// SPDX-License-Identifier: GPL-3.0-or-later
// Helpers outside #[test] fns are test code too; clippy's in-test allowances miss them.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Waveform overview (AUD-14): exact peaks at every level, chunking independence, range queries,
//! and building from real files with damage, length disagreement and cancellation.

mod common;

use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};

use common::media::*;
use openlab_audio::{
    BASE_FRAMES, Media, MediaError, Peak, WarningKind, Waveform, WaveformBuilder, WaveformError,
    build_waveform, probe, probe_media,
};

const RATE: u32 = 44_100;

fn q(x: f32) -> i16 {
    (x.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

/// Deterministic pseudo-random samples in ±1 (an LCG), interleaved.
fn noise(frames: usize, channels: usize, seed: u32) -> Vec<f32> {
    let mut s = seed;
    (0..frames * channels)
        .map(|_| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (s >> 8) as f32 / (1u32 << 23) as f32 - 1.0
        })
        .collect()
}

fn build(samples: &[f32], channels: u16, chunk_frames: usize) -> Waveform {
    let frames = (samples.len() / usize::from(channels)) as u64;
    let mut b = WaveformBuilder::new(RATE, channels, frames).unwrap();
    for c in samples.chunks(chunk_frames * usize::from(channels)) {
        b.push(c);
    }
    b.finish().unwrap()
}

/// Reference: min/max of channel `c` per `size` frames.
fn naive(samples: &[f32], channels: usize, c: usize, size: usize) -> Vec<Peak> {
    let ch: Vec<i16> = samples
        .iter()
        .skip(c)
        .step_by(channels)
        .map(|&x| q(x))
        .collect();
    ch.chunks(size)
        .map(|w| Peak {
            min: *w.iter().min().unwrap(),
            max: *w.iter().max().unwrap(),
        })
        .collect()
}

fn assert_exact(samples: &[f32], channels: usize, w: &Waveform) {
    let frames = samples.len() / channels;
    assert_eq!(w.frames, frames as u64);
    assert_eq!(w.channels as usize, channels);
    let mut size = BASE_FRAMES as usize;
    for (i, level) in w.levels.iter().enumerate() {
        assert_eq!(level.frames_per_bucket, size as u64, "level {i}");
        assert_eq!(level.channels.len(), channels);
        for c in 0..channels {
            assert_eq!(
                level.channels[c],
                naive(samples, channels, c, size),
                "level {i} ch {c}"
            );
        }
        size *= 2;
    }
    // Ends with a single bucket covering the whole file.
    assert_eq!(w.levels.last().unwrap().buckets(), 1);
    assert!(w.levels.iter().rev().skip(1).all(|l| l.buckets() > 1));
}

#[test]
fn peaks_are_exact_at_every_level_with_a_partial_last_bucket() {
    // 3 full buckets + 232 frames: 4, 2 and 1 buckets.
    let s = noise(1000, 2, 1);
    let w = build(&s, 2, 1000);
    assert_eq!(w.levels.len(), 3);
    assert_eq!(w.levels[0].buckets(), 4);
    assert_exact(&s, 2, &w);
    // Several sizes, odd and even bucket counts, up to many levels.
    for frames in [257, 512, 513, 1025, 4097, 70_001] {
        let s = noise(frames, 2, frames as u32);
        assert_exact(&s, 2, &build(&s, 2, 4096));
    }
}

#[test]
fn files_shorter_than_one_bucket_have_one_level_of_one_bucket() {
    for frames in [1, 100, 255, 256] {
        let s = noise(frames, 1, 9);
        let w = build(&s, 1, 64);
        assert_eq!(w.levels.len(), 1, "{frames}");
        assert_exact(&s, 1, &w);
    }
    let empty = WaveformBuilder::new(RATE, 2, 0).unwrap().finish().unwrap();
    assert!(empty.levels.is_empty());
    assert_eq!(empty.frames, 0);
    assert!(empty.range(0, 0, 10, 4).is_empty());
}

#[test]
fn channels_are_independent() {
    // Left silent, right a spike at frame 300.
    let mut s = vec![0.0; 1000 * 2];
    s[300 * 2 + 1] = 0.5;
    s[301 * 2 + 1] = -0.25;
    let w = build(&s, 2, 128);
    for l in &w.levels {
        assert!(l.channels[0].iter().all(|p| *p == Peak::default()));
    }
    assert_eq!(
        w.levels[0].channels[1][1],
        Peak {
            min: q(-0.25),
            max: q(0.5)
        }
    );
    assert_eq!(w.levels[0].channels[1][0], Peak::default());
    assert_exact(&s, 2, &w);
}

#[test]
fn chunk_size_does_not_change_the_result() {
    let s = noise(9_000, 3, 5);
    let whole = build(&s, 3, 9_000);
    for chunk in [1, 2, 7, 255, 256, 257, 1000, 4096] {
        assert_eq!(build(&s, 3, chunk), whole, "chunk {chunk}");
    }
    assert_exact(&s, 3, &whole);
}

#[test]
fn a_partial_trailing_frame_and_surplus_frames_are_ignored() {
    let s = noise(600, 2, 3);
    let mut b = WaveformBuilder::new(RATE, 2, 500).unwrap();
    assert_eq!(b.push(&s[..401]), 200); // 200 whole frames + 1 stray sample
    assert_eq!(b.remaining(), 300);
    assert_eq!(b.push(&s[400..]), 300); // only up to the expected length
    assert_eq!(b.push(&s), 0);
    let w = b.finish().unwrap();
    assert_eq!(w.frames, 500);
    assert_exact(&s[..1000], 2, &w);
    assert!(WaveformBuilder::new(RATE, 0, 10).is_err());
}

#[test]
fn non_finite_and_out_of_range_samples_never_reach_the_output() {
    let s = [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        5.0,
        -5.0,
        0.5,
        0.0,
        f32::NAN,
    ];
    let w = build(&s, 1, 8);
    assert_eq!(
        w.levels[0].channels[0][0],
        Peak {
            min: -32767,
            max: 32767
        }
    );
    let only_bad = build(&[f32::NAN, f32::INFINITY, f32::NEG_INFINITY], 1, 3);
    assert_eq!(only_bad.levels[0].channels[0][0], Peak::default());
    assert_eq!(
        Peak {
            min: -32767,
            max: 32767
        }
        .max_f32(),
        1.0
    );
}

/// 4096 frames of low noise with a spike of 1.0 at `spike` and a dip of -1.0 at `dip`.
fn spiky(spike: usize, dip: usize) -> Waveform {
    let mut s: Vec<f32> = noise(4096, 1, 2).iter().map(|x| x * 0.1).collect();
    s[spike] = 1.0;
    s[dip] = -1.0;
    build(&s, 1, 4096)
}

#[test]
fn range_matches_a_brute_force_scan_on_aligned_buckets() {
    let s = noise(8192, 2, 11);
    let w = build(&s, 2, 1000);
    for (start, end, buckets, size) in [
        (0, 8192, 32, 256),
        (0, 8192, 8, 1024),
        (0, 8192, 1, 8192),
        (1024, 5120, 4, 1024),
        (512, 2560, 4, 512),
    ] {
        for c in 0..2 {
            let want: Vec<Peak> = naive(&s[..], 2, c, 1)
                .chunks(size)
                .skip(start / size)
                .take((end - start) / size)
                .map(|w| Peak {
                    min: w.iter().map(|p| p.min).min().unwrap(),
                    max: w.iter().map(|p| p.max).max().unwrap(),
                })
                .collect();
            assert_eq!(
                w.range(c, start as u64, end as u64, buckets),
                want,
                "{start}..{end}/{buckets}"
            );
        }
    }
}

#[test]
fn range_picks_the_coarsest_level_with_a_source_bucket_per_output_bucket() {
    let w = spiky(800, 3000);
    // 4 buckets over 100..2148: 512 frames each, so level 1 (512). Output bucket 0 is 100..612
    // and rounds out to source buckets 0..1024, which holds the spike at 800. Level 0 would
    // stop at 768 and miss it.
    let r = w.range(0, 100, 2148, 4);
    assert_eq!(r.len(), 4);
    assert_eq!(r[0].max, 32767);
    // 8 buckets over the same span: 256 frames each, level 0; bucket 0 is 100..356 -> 0..512.
    let r = w.range(0, 100, 2148, 8);
    assert_eq!(r.len(), 8);
    assert!(r[0].max < 32767);
    // One bucket over everything: the top level, the file's extremes.
    assert_eq!(
        w.range(0, 0, 4096, 1),
        vec![Peak {
            min: -32767,
            max: 32767
        }]
    );
    // Finer than level 0 is asked for: level 0 buckets repeat, still within the data.
    let r = w.range(0, 768, 1024, 256);
    assert_eq!(r.len(), 256);
    assert!(r.iter().all(|p| p.max == 32767));
}

#[test]
fn range_edges_never_panic() {
    let w = spiky(10, 4000);
    // Beyond the end is clamped.
    let r = w.range(0, 3900, u64::MAX, 2);
    assert_eq!(r.len(), 2);
    assert_eq!(r[1].min, -32767);
    assert_eq!(w.range(0, 3900, 100_000, 2), r);
    // Mid-bucket start and end.
    let r = w.range(0, 5, 17, 3);
    assert_eq!(r.len(), 3);
    assert_eq!(r[0].max, 32767);
    // Empty, inverted, out of range, no buckets, unknown channel.
    assert!(w.range(0, 100, 100, 4).is_empty());
    assert!(w.range(0, 200, 100, 4).is_empty());
    assert!(w.range(0, 4096, 5000, 4).is_empty());
    assert!(w.range(0, u64::MAX, u64::MAX, 4).is_empty());
    assert!(w.range(0, 0, 4096, 0).is_empty());
    assert!(w.range(1, 0, 4096, 4).is_empty());
    assert!(w.range(usize::MAX, 0, 4096, 4).is_empty());
    // More buckets than frames: one per frame. Absurd counts are capped.
    assert_eq!(w.range(0, 0, 3, usize::MAX).len(), 3);
    let r = w.range(0, 0, 4096, usize::MAX);
    assert_eq!(r.len(), 4096);
    assert_eq!(r[10].max, 32767);
    let long = build(&vec![0.25; 200_000], 1, 4096);
    let r = long.range(0, 0, 200_000, usize::MAX);
    assert_eq!(r.len(), openlab_audio::MAX_RANGE_BUCKETS);
    assert!(r.iter().all(|p| p.min == q(0.25) && p.max == q(0.25)));
}

#[test]
fn pyramid_size_is_about_eight_bytes_per_base_bucket_of_stereo() {
    let w = build(&noise(100 * 256, 2, 4), 2, 4096);
    // 2 channels x 4 bytes x (100 + 50 + 25 + 13 + 7 + 4 + 2 + 1 = 202 buckets).
    assert_eq!(w.bytes(), 2 * 4 * 202);
}

#[test]
fn real_files_give_a_waveform_matching_their_probe() {
    let never = AtomicBool::new(false);
    let wav = temp_file("wf-tone.wav", &wav(&tone(30_000, RATE), 2, RATE, Pcm::I16));
    for path in [
        wav,
        fixture("tone.flac"),
        fixture("tone.mp3"),
        fixture("tone.m4a"),
    ] {
        let probed = probe(&path).unwrap();
        let r = build_waveform(Media::Path(path.clone()), &probed.info, &never).unwrap();
        let w = r.waveform;
        assert_eq!(w.frames, probed.info.frames, "{path:?}");
        assert_eq!(w.sample_rate, probed.info.sample_rate);
        assert_eq!(w.channels, probed.info.channels);
        assert_eq!(w.levels[0].buckets() as u64, w.frames.div_ceil(BASE_FRAMES));
        assert_eq!(w.levels.last().unwrap().buckets(), 1);
        // A tone is audible, and no peak is NaN-derived garbage.
        let top = w.range(0, 0, w.frames, 1)[0];
        assert!(top.max > 1000 && top.min < -1000, "{path:?} {top:?}");
        let warnings: Vec<_> = r.warnings.iter().map(|x| x.kind).collect();
        let probed_kinds: Vec<_> = probed.warnings.iter().map(|x| x.kind).collect();
        assert!(
            warnings.iter().all(|k| probed_kinds.contains(k)),
            "{path:?} {warnings:?}"
        );
    }
}

#[test]
fn wav_peaks_match_the_decoded_samples() {
    let reference = tone(5_000, RATE);
    let bytes = wav(&reference, 2, RATE, Pcm::F32);
    let info = probe_media(Media::Reader(
        Box::new(Cursor::new(bytes.clone())),
        Some("wav".into()),
    ))
    .unwrap()
    .info;
    let r = build_waveform(
        Media::Reader(Box::new(Cursor::new(bytes)), Some("wav".into())),
        &info,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(r.warnings.is_empty());
    assert_exact(&reference, 2, &r.waveform);
}

fn reader(bytes: &[u8]) -> Media {
    Media::Reader(Box::new(Cursor::new(bytes.to_vec())), Some("wav".into()))
}

#[test]
fn a_file_that_disagrees_with_its_probe_is_bounded() {
    let bytes = wav(&tone(10_000, RATE), 2, RATE, Pcm::I16);
    let info = probe_media(reader(&bytes)).unwrap().info;
    let never = AtomicBool::new(false);
    // Longer than probed: stops at the probed length.
    let fewer = openlab_audio::MediaInfo {
        frames: 4_000,
        ..info
    };
    let r = build_waveform(reader(&bytes), &fewer, &never).unwrap();
    assert_eq!(r.waveform.frames, 4_000);
    assert!(r.warnings.is_empty());
    // Shorter than probed: the frames there are, and a Truncated warning.
    let more = openlab_audio::MediaInfo {
        frames: 12_000,
        ..info
    };
    let r = build_waveform(reader(&bytes), &more, &never).unwrap();
    assert_eq!(r.waveform.frames, 10_000);
    assert_eq!(
        r.warnings.last().map(|w| (w.kind, w.at, w.frames)),
        Some((WarningKind::Truncated, 10_000, 2_000))
    );
    // Channels or rate that changed.
    for bad in [
        openlab_audio::MediaInfo {
            channels: 1,
            ..info
        },
        openlab_audio::MediaInfo {
            sample_rate: 48_000,
            ..info
        },
    ] {
        assert_eq!(
            build_waveform(reader(&bytes), &bad, &never),
            Err(WaveformError::Media(MediaError::Changed))
        );
    }
}

#[test]
fn cancelling_stops_the_build() {
    let bytes = wav(&tone(10_000, RATE), 2, RATE, Pcm::I16);
    let info = probe_media(reader(&bytes)).unwrap().info;
    let cancel = AtomicBool::new(true);
    assert_eq!(
        build_waveform(reader(&bytes), &info, &cancel),
        Err(WaveformError::Cancelled)
    );
    // Cancelled part way: the flag flips when the reader is first read past the start.
    let flag = std::sync::Arc::new(AtomicBool::new(false));
    struct Flip(Cursor<Vec<u8>>, std::sync::Arc<AtomicBool>, u64);
    impl std::io::Read for Flip {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            let n = self.0.read(b)?;
            if self.0.position() > self.2 {
                self.1.store(true, Ordering::Relaxed);
            }
            Ok(n)
        }
    }
    impl std::io::Seek for Flip {
        fn seek(&mut self, p: std::io::SeekFrom) -> std::io::Result<u64> {
            self.0.seek(p)
        }
    }
    let long = wav(&tone(200_000, RATE), 2, RATE, Pcm::I16);
    let info = probe_media(reader(&long)).unwrap().info;
    let media = Media::Reader(
        Box::new(Flip(Cursor::new(long), flag.clone(), 100_000)),
        Some("wav".into()),
    );
    assert_eq!(
        build_waveform(media, &info, &flag),
        Err(WaveformError::Cancelled)
    );
}

#[test]
fn damaged_media_gives_a_waveform_and_warnings() {
    let bytes = wav(&tone(10_000, RATE), 2, RATE, Pcm::I16);
    let cut = &bytes[..bytes.len() - 8_000];
    let probed = probe_media(reader(cut)).unwrap();
    assert_eq!(probed.info.frames, 8_000);
    let r = build_waveform(reader(cut), &probed.info, &AtomicBool::new(false)).unwrap();
    assert_eq!(r.waveform.frames, 8_000);

    // A FLAC with a corrupted frame: silence where the frame is missing, Missing warning.
    let mut flac = std::fs::read(fixture("tone.flac")).unwrap();
    let mid = flac.len() / 2;
    for b in &mut flac[mid..mid + 16] {
        *b ^= 0x5a;
    }
    let media = || Media::Reader(Box::new(Cursor::new(flac.clone())), Some("flac".into()));
    let probed = probe_media(media()).unwrap();
    let r = build_waveform(media(), &probed.info, &AtomicBool::new(false)).unwrap();
    assert_eq!(r.waveform.frames, probed.info.frames);
    assert!(r.warnings.iter().any(|w| w.kind == WarningKind::Missing));
    assert_eq!(r.waveform.range(0, 0, 4_096, 1)[0], Peak::default());
}

#[test]
fn unplayable_files_are_errors() {
    let info = probe(&fixture("tone.flac")).unwrap().info;
    let never = AtomicBool::new(false);
    let junk = Media::Reader(Box::new(Cursor::new(vec![7u8; 5_000])), None);
    assert!(matches!(
        build_waveform(junk, &info, &never),
        Err(WaveformError::Media(_))
    ));
    let empty = Media::Reader(Box::new(Cursor::new(Vec::new())), None);
    assert!(matches!(
        build_waveform(empty, &info, &never),
        Err(WaveformError::Media(_))
    ));
    let missing = Media::Path("/nonexistent/openlab/none.wav".into());
    assert!(matches!(
        build_waveform(missing, &info, &never),
        Err(WaveformError::Media(MediaError::NotFound(_)))
    ));
}
