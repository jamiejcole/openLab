// SPDX-License-Identifier: GPL-3.0-or-later
// Helpers outside #[test] fns are test code too; clippy's in-test allowances miss them.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Decoding (AUD-04): every supported format decodes to the known signal with an exact length,
//! and bad files give typed errors, never panics.

mod common;

use std::io::Cursor;

use common::media::*;
use common::*;
use openlab_audio::{Media, MediaError, preload, probe, probe_media};

const RATE: u32 = 44_100;
/// Length of the committed fixtures (0.5 s).
const FIXTURE_FRAMES: u64 = 22_050;

#[test]
fn wav_formats_decode_to_the_known_signal() {
    let reference = tone(11_025, RATE);
    for fmt in [Pcm::I16, Pcm::I24, Pcm::I32, Pcm::F32] {
        let bytes = wav(&reference, 2, RATE, fmt);
        let path = temp_file(&format!("tone-{fmt:?}.wav"), &bytes);
        let info = probe(&path).unwrap();
        assert_eq!(
            (info.channels, info.sample_rate, info.frames),
            (2, RATE, 11_025)
        );
        let got = decode_all(Media::Path(path));
        assert_eq!(got.len(), reference.len(), "{fmt:?}");
        let err = max_diff(&got, &reference);
        assert!(err <= fmt.lsb() * 1.01, "{fmt:?}: max error {err}");
    }
}

#[test]
fn aiff_decodes_to_the_known_signal() {
    let reference = tone(11_025, RATE);
    for fmt in [Pcm::I16, Pcm::I24] {
        let bytes = aiff(&reference, 2, RATE, fmt);
        let info = probe_media(Media::Reader(Box::new(Cursor::new(bytes.clone())), None)).unwrap();
        assert_eq!(
            (info.channels, info.sample_rate, info.frames),
            (2, RATE, 11_025)
        );
        let got = decode_all(Media::Reader(
            Box::new(Cursor::new(bytes)),
            Some("aiff".into()),
        ));
        assert!(max_diff(&got, &reference) <= fmt.lsb() * 1.01, "{fmt:?}");
    }
}

#[test]
fn lossless_fixtures_decode_exactly() {
    let reference = tone(FIXTURE_FRAMES as usize, RATE);
    for name in ["tone.flac", "tone-alac.m4a"] {
        let info = probe(&fixture(name)).unwrap();
        assert_eq!(
            (info.channels, info.sample_rate, info.frames),
            (2, RATE, FIXTURE_FRAMES),
            "{name}"
        );
        let got = decode_all(Media::Path(fixture(name)));
        assert_eq!(got.len(), reference.len(), "{name}");
        // ffmpeg's tone generator is fixed point; allow a few 16-bit steps against the float tone.
        assert!(max_diff(&got, &reference) < 1e-3, "{name}");
        check_tone(name, &got);
    }
}

#[test]
fn lossy_fixtures_have_exact_counted_length_and_the_right_content() {
    for name in ["tone.mp3", "tone.m4a", "tone.ogg"] {
        let info = probe(&fixture(name)).unwrap();
        assert_eq!((info.channels, info.sample_rate), (2, RATE), "{name}");
        let got = decode_all(Media::Path(fixture(name)));
        // The probed length is exactly what decoding yields, so fades and slices land right.
        assert_eq!(got.len() as u64, info.frames * 2, "{name}");
        // Encoders add priming/padding that not every container lets the decoder trim.
        assert!(
            info.frames.abs_diff(FIXTURE_FRAMES) <= 3 * 1152,
            "{name}: {}",
            info.frames
        );
        check_tone(name, &got);
    }
}

#[test]
fn mp3_gapless_trim_gives_the_original_length() {
    // LAME writes encoder delay and padding; the decoder trims them.
    assert_eq!(probe(&fixture("tone.mp3")).unwrap().frames, FIXTURE_FRAMES);
}

#[test]
fn missing_file_is_a_typed_error() {
    let p = std::path::PathBuf::from("/nonexistent/openlab/missing.wav");
    assert_eq!(probe(&p), Err(MediaError::NotFound(p.clone())));
    assert!(matches!(
        preload(Media::Path(p)),
        Err(MediaError::NotFound(_))
    ));
}

#[test]
fn garbage_is_a_typed_error() {
    let junk: Vec<u8> = (0..10_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    let path = temp_file("junk.wav", &junk);
    assert!(matches!(
        probe(&path),
        Err(MediaError::Unsupported(_) | MediaError::Corrupt(_))
    ));
    assert!(probe_media(Media::Reader(Box::new(Cursor::new(Vec::new())), None)).is_err());
}

#[test]
fn truncated_wav_is_a_typed_error() {
    let bytes = wav(&tone(10_000, RATE), 2, RATE, Pcm::I16);
    let cut = &bytes[..bytes.len() - 8_000];
    let path = temp_file("truncated.wav", cut);
    assert!(
        matches!(
            probe(&path),
            Err(MediaError::Truncated {
                expected: 10_000,
                ..
            })
        ),
        "{:?}",
        probe(&path)
    );
    assert!(matches!(
        preload(Media::Path(path)),
        Err(MediaError::Truncated { .. })
    ));
}

#[test]
fn truncated_flac_is_a_typed_error() {
    let bytes = std::fs::read(fixture("tone.flac")).unwrap();
    let path = temp_file("truncated.flac", &bytes[..bytes.len() / 2]);
    let r = probe(&path);
    assert!(
        matches!(
            r,
            Err(MediaError::Truncated {
                expected: FIXTURE_FRAMES,
                ..
            })
        ),
        "{r:?}"
    );
}

#[test]
fn corrupt_flac_is_a_typed_error() {
    let mut bytes = std::fs::read(fixture("tone.flac")).unwrap();
    let mid = bytes.len() / 2;
    for b in &mut bytes[mid..mid + 2000] {
        *b ^= 0x5a;
    }
    let path = temp_file("corrupt.flac", &bytes);
    // symphonia's FLAC reader stops at the damaged frame; probe decodes compressed files fully,
    // so the damage is found when the cue loads, not during the show.
    let r = probe(&path);
    assert!(
        matches!(
            r,
            Err(MediaError::Corrupt(_) | MediaError::Truncated { .. })
        ),
        "{r:?}"
    );
    let r = preload(Media::Path(path));
    assert!(
        matches!(
            r,
            Err(MediaError::Corrupt(_) | MediaError::Truncated { .. })
        ),
        "{r:?}"
    );
}

#[test]
fn missing_frames_are_a_typed_error() {
    // Splice one whole FLAC frame out: every remaining frame is valid, but the next frame header
    // carries a later sample number. Decoding on would shift everything after it.
    let bytes = std::fs::read(fixture("tone.flac")).unwrap();
    let syncs: Vec<usize> = bytes
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w == &[0xFF, 0xF8])
        .map(|(i, _)| i)
        .collect();
    let spliced: Vec<u8> = bytes[..syncs[2]]
        .iter()
        .chain(&bytes[syncs[3]..])
        .copied()
        .collect();
    let path = temp_file("spliced.flac", &spliced);
    let r = probe(&path);
    assert!(
        matches!(r, Err(MediaError::Corrupt(ref m)) if m.contains("discontinuity")),
        "{r:?}"
    );
    assert!(matches!(
        preload(Media::Path(path)),
        Err(MediaError::Corrupt(_))
    ));
}

#[test]
fn unsupported_channel_count_is_a_typed_error() {
    let bytes = wav(&vec![0.0; 65 * 10], 65, RATE, Pcm::I16);
    let r = probe_media(Media::Reader(
        Box::new(Cursor::new(bytes)),
        Some("wav".into()),
    ));
    assert!(
        matches!(r, Err(MediaError::Unsupported(_) | MediaError::Corrupt(_))),
        "{r:?}"
    );
}

#[test]
fn preload_reports_its_memory() {
    let p = preload(Media::Path(fixture("tone.flac"))).unwrap();
    assert_eq!(p.bytes(), FIXTURE_FRAMES as usize * 2 * 4);
    assert_eq!(p.info().frames, FIXTURE_FRAMES);
}

fn max_diff(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

/// Left channel is 440 Hz at 0.5, right 1000 Hz at 0.25.
fn check_tone(name: &str, x: &[f32]) {
    // The middle half: lossy codecs add near-silent priming at the start.
    let mid = |c: Vec<f32>| c[c.len() / 4..c.len() * 3 / 4].to_vec();
    let (l, r) = (mid(channel(x, 2, 0)), mid(channel(x, 2, 1)));
    // ffmpeg's built-in Vorbis encoder (the only one available to make the fixture) distorts the
    // quieter channel; ffmpeg's own decoder measures the same ~993 Hz. OGG is optional (§21).
    let (tol_hz, tol_rms) = if name.ends_with(".ogg") {
        (12.0, 0.05)
    } else {
        (0.5, 0.02)
    };
    let (fl, fr) = (frequency(&l, RATE), frequency(&r, RATE));
    assert!((fl - 440.0).abs() < tol_hz, "{name}: left {fl} Hz");
    assert!((fr - 1000.0).abs() < tol_hz, "{name}: right {fr} Hz");
    let (al, ar) = (rms(&l), rms(&r));
    assert!(
        (al - 0.5 / 2f64.sqrt()).abs() < tol_rms,
        "{name}: left rms {al}"
    );
    assert!(
        (ar - 0.25 / 2f64.sqrt()).abs() < tol_rms,
        "{name}: right rms {ar}"
    );
}

fn decode_all(media: Media) -> Vec<f32> {
    preload(media).unwrap().samples().to_vec()
}

/// The decode fuzz target's checks on every fixture and on deterministic damage to each: cuts,
/// flipped bytes, zeroed runs. Anything that probes must preload and stream exactly as probed;
/// anything else must fail with a typed error (a panic fails the test).
#[test]
fn fuzz_invariants_hold_on_fixtures_and_damaged_copies() {
    let mut inputs: Vec<Vec<u8>> = [
        "tone.flac",
        "tone-alac.m4a",
        "tone.mp3",
        "tone.m4a",
        "tone.ogg",
    ]
    .iter()
    .map(|n| std::fs::read(fixture(n)).unwrap())
    .collect();
    inputs.push(wav(&tone(2_000, RATE), 2, RATE, Pcm::I24));
    inputs.push(aiff(&tone(2_000, RATE), 2, RATE, Pcm::I16));
    for (i, bytes) in inputs.iter().enumerate() {
        let n = bytes.len();
        let mut variants = vec![bytes.clone()];
        for cut in [12, 64, n / 3, n - 1] {
            variants.push(bytes[..cut].to_vec());
        }
        for (at, len) in [(n / 2, 1), (n / 5, 16), (40, 8), (n * 3 / 4, 300)] {
            let mut v = bytes.clone();
            for b in &mut v[at..(at + len).min(n)] {
                *b ^= 0xA5;
            }
            variants.push(v);
            let mut z = bytes.clone();
            z[at..(at + len).min(n)].fill(0);
            variants.push(z);
        }
        for (j, v) in variants.iter().enumerate() {
            if let Err(e) = openlab_audio::fuzz_decode(v) {
                panic!("input {i} variant {j}: {e}");
            }
        }
    }
}

/// Inputs the decode fuzz target once failed on (tests/fixtures/fuzz/, one file per finding).
/// `alac-short-index.m4a`: the MP4 index declares fewer frames than decode; seeking past the
/// declared length was refused by the container and taken for end of file.
/// `f32-non-finite.wav`: NaN and infinite float samples made the resampler's FFT panic.
#[test]
fn fuzz_findings_stay_fixed() {
    let dir = fixture("fuzz");
    let mut n = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let bytes = std::fs::read(&path).unwrap();
        if let Err(e) = openlab_audio::fuzz_decode(&bytes) {
            panic!("{}: {e}", path.display());
        }
        n += 1;
    }
    assert!(n > 0);
}

#[test]
fn non_finite_and_absurd_float_samples_are_made_safe() {
    let samples = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e30, -1e30, 0.5, -2.0];
    let bytes = wav(&samples, 1, RATE, Pcm::F32);
    let got = decode_all(Media::Reader(Box::new(Cursor::new(bytes)), Some("wav".into())));
    assert_eq!(got, [0.0, 0.0, 0.0, 16.0, -16.0, 0.5, -2.0]);
}
