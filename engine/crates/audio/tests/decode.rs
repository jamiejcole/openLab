// SPDX-License-Identifier: GPL-3.0-or-later
// Helpers outside #[test] fns are test code too; clippy's in-test allowances miss them.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Decoding (AUD-04): every supported format decodes to the known signal with an exact length,
//! and bad files give typed errors, never panics.

mod common;

use std::io::Cursor;
use std::path::Path;

use common::media::*;
use common::*;
use openlab_audio::{
    Media, MediaError, MediaWarning, Probed, WarningKind, preload, probe, probe_media,
};

const RATE: u32 = 44_100;
/// Length of the committed fixtures (0.5 s).
const FIXTURE_FRAMES: u64 = 22_050;

#[test]
fn wav_formats_decode_to_the_known_signal() {
    let reference = tone(11_025, RATE);
    for fmt in [Pcm::I16, Pcm::I24, Pcm::I32, Pcm::F32] {
        let bytes = wav(&reference, 2, RATE, fmt);
        let path = temp_file(&format!("tone-{fmt:?}.wav"), &bytes);
        let info = probe(&path).unwrap().info;
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
        let info = probe_media(Media::Reader(Box::new(Cursor::new(bytes.clone())), None))
            .unwrap()
            .info;
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
        let info = probe(&fixture(name)).unwrap().info;
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
        let info = probe(&fixture(name)).unwrap().info;
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
    assert_eq!(
        probe(&fixture("tone.mp3")).unwrap().info.frames,
        FIXTURE_FRAMES
    );
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

fn warning(kind: WarningKind, at: u64, frames: u64) -> MediaWarning {
    MediaWarning { kind, at, frames }
}

/// Damage plays as described by its warning: the probe, preload and playback agree on one
/// timeline. Checks the preload against the probe and returns it.
fn load_damaged(bytes: Vec<u8>, ext: &str, want: &[MediaWarning]) -> (Probed, Vec<f32>) {
    let media = || Media::Reader(Box::new(Cursor::new(bytes.clone())), Some(ext.into()));
    let probed = probe_media(media()).unwrap();
    assert_eq!(probed.warnings, want, "{ext}");
    let pre = preload(media()).unwrap();
    assert_eq!((pre.info(), pre.warnings()), (probed.info, want), "{ext}");
    // Whatever probes streams and seeks too.
    openlab_audio::fuzz_decode(&bytes).unwrap();
    (probed, pre.samples().to_vec())
}

fn frames(x: &[f32], ch: usize, r: std::ops::Range<usize>) -> &[f32] {
    &x[r.start * ch..r.end * ch]
}

/// Equal sample for sample; on failure names the first differing interleaved sample.
fn assert_same(got: &[f32], want: &[f32], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: length");
    if let Some(i) = got.iter().zip(want).position(|(a, b)| a != b) {
        panic!("{what}: sample {i} is {} not {}", got[i], want[i]);
    }
}

#[test]
fn truncated_wav_plays_to_its_real_end_with_a_warning() {
    let reference = tone(10_000, RATE);
    let bytes = wav(&reference, 2, RATE, Pcm::I16);
    // 8000 bytes = 2000 stereo 16-bit frames cut off the end; the header still says 10000.
    let cut = bytes[..bytes.len() - 8_000].to_vec();
    let (p, got) = load_damaged(cut, "wav", &[warning(WarningKind::Truncated, 8_000, 2_000)]);
    assert_eq!(p.info.frames, 8_000);
    assert!(max_diff(&got, &reference[..16_000]) <= Pcm::I16.lsb() * 1.01);
}

#[test]
fn truncated_flac_plays_to_its_real_end_with_a_warning() {
    let bytes = std::fs::read(fixture("tone.flac")).unwrap();
    let clean = decode_all(Media::Path(fixture("tone.flac")));
    // Cut inside the fourth frame: three whole frames are left.
    let half = bytes[..bytes.len() * 3 / 4].to_vec();
    let probed = probe_media(Media::Reader(Box::new(Cursor::new(half.clone())), None)).unwrap();
    let end = probed.info.frames;
    assert!(end > 0 && end < FIXTURE_FRAMES);
    let (_, got) = load_damaged(
        half,
        "flac",
        &[warning(WarningKind::Truncated, end, FIXTURE_FRAMES - end)],
    );
    assert_same(&got, frames(&clean, 2, 0..end as usize), "truncated flac");
}

/// Damage in the first frame: the frames before the first decodable one are silence, so
/// everything after keeps its position.
#[test]
fn damaged_first_frame_plays_as_silence_at_the_start() {
    let clean = decode_all(Media::Path(fixture("tone.flac")));
    let mut bytes = std::fs::read(fixture("tone.flac")).unwrap();
    let mid = bytes.len() / 2;
    for b in &mut bytes[mid..mid + 16] {
        *b ^= 0x5a;
    }
    let (p, got) = load_damaged(bytes, "flac", &[warning(WarningKind::Missing, 0, 4_096)]);
    assert_eq!(p.info.frames, FIXTURE_FRAMES);
    assert!(frames(&got, 2, 0..4_096).iter().all(|&s| s == 0.0));
    let end = FIXTURE_FRAMES as usize;
    assert_same(
        frames(&got, 2, 4_096..end),
        frames(&clean, 2, 4_096..end),
        "after the damage",
    );
}

/// Damage the container skips (a FLAC frame that fails its CRC, a frame spliced out, an Ogg
/// page dropped, an AAC packet that does not decode) plays as silence of its length, so every
/// later frame keeps its position.
#[test]
fn damaged_media_plays_with_silence_where_frames_are_missing() {
    let damage = |name: &str, at: usize, len: usize| {
        let mut b = std::fs::read(fixture(name)).unwrap();
        for x in &mut b[at..at + len] {
            *x ^= 0x5a;
        }
        b
    };
    let flac = std::fs::read(fixture("tone.flac")).unwrap();
    let syncs: Vec<usize> = flac
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w == &[0xFF, 0xF8])
        .map(|(i, _)| i)
        .collect();
    let spliced: Vec<u8> = flac[..syncs[2]]
        .iter()
        .chain(&flac[syncs[3]..])
        .copied()
        .collect();
    let cases = [
        (
            "tone.flac",
            "flac",
            damage("tone.flac", 13_832, 16),
            8_192,
            4_096,
        ),
        ("tone.flac", "flac", spliced, 8_192, 4_096),
        (
            "tone-4s.ogg",
            "ogg",
            damage("tone-4s.ogg", 10_888, 16),
            89_088,
            45_056,
        ),
        (
            "tone.m4a",
            "m4a",
            damage("tone.m4a", 5_964, 16),
            17_408,
            1_024,
        ),
    ];
    for (name, ext, bytes, at, len) in cases {
        let clean = decode_all(Media::Path(fixture(name)));
        let (p, got) = load_damaged(bytes, ext, &[warning(WarningKind::Missing, at, len)]);
        let total = clean.len() / 2;
        assert_eq!(p.info.frames as usize, total, "{name}: length unchanged");
        let (at, end) = (at as usize, (at + len) as usize);
        assert!(
            frames(&got, 2, at..end).iter().all(|&s| s == 0.0),
            "{name}: silence"
        );
        // Before the damage: exact. Vorbis differs earlier: symphonia's Ogg reader plays the
        // page after a lost one in the lost page's place (here from frame 44032), and only the
        // page after that is placed by its own timestamp, which is where the gap shows. The
        // warning reports that gap; playback and seeks agree on it (see decode.rs tests).
        let before = match ext {
            "flac" => at,
            "ogg" => 44_032,
            _ => at - 2_048,
        };
        assert_same(
            frames(&got, 2, 0..before),
            frames(&clean, 2, 0..before),
            &format!("{name}: before"),
        );
        // After the gap a lossy decoder restarts without its predecessor's state; a little
        // later it matches the clean decode exactly (for FLAC at once).
        let settle = if ext == "flac" { 0 } else { 4_096 };
        let (g, c) = (
            frames(&got, 2, end + settle..total),
            frames(&clean, 2, end + settle..total),
        );
        if ext == "m4a" {
            // AAC noise substitution draws from a sequence the failed packet put out of step.
            assert!(max_diff(g, c) < 2.5e-3, "{name}: after");
        } else {
            assert_same(g, c, &format!("{name}: after"));
        }
    }
}

/// Lossy damage the container cannot place: the file plays shorter than its header says, and
/// the shortfall is reported.
#[test]
fn lossy_frames_lost_without_a_position_are_reported() {
    let mut b = std::fs::read(fixture("tone.mp3")).unwrap();
    for x in &mut b[4_619..4_919] {
        *x ^= 0x5a;
    }
    let (p, _) = load_damaged(b, "mp3", &[warning(WarningKind::Truncated, 21_935, 115)]);
    assert_eq!(p.info.frames, 21_935);
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

/// Inputs the decode fuzz target once failed on (tests/fuzz-findings/, one file per finding; kept out of the
/// seed corpus so the fuzzer does not stop on findings that are contained but not fixed upstream).
/// `alac-short-index.m4a`: the MP4 index declares fewer frames than decode; seeking past the
/// declared length was refused by the container and taken for end of file.
/// `f32-non-finite.wav`: NaN and infinite float samples made the resampler's FFT panic.
/// `wav-channel-overflow.wav`: symphonia 0.6.1's WAV reader overflows `u16` on a huge channel
/// count (a panic with overflow checks on, as here); contained as a typed error.
#[test]
fn fuzz_findings_stay_fixed() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fuzz-findings");
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
    let samples = [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        1e30,
        -1e30,
        0.5,
        -2.0,
    ];
    let bytes = wav(&samples, 1, RATE, Pcm::F32);
    let got = decode_all(Media::Reader(
        Box::new(Cursor::new(bytes)),
        Some("wav".into()),
    ));
    assert_eq!(got, [0.0, 0.0, 0.0, 16.0, -16.0, 0.5, -2.0]);
}

/// Headers that would overflow symphonia 0.6.1's `u16` frame-size arithmetic are rejected before
/// symphonia parses them (with overflow checks on, as in tests and fuzzing, it would panic).
#[test]
fn pcm_headers_symphonia_would_overflow_on_are_rejected_first() {
    let header_error = |r: Result<openlab_audio::Probed, MediaError>| matches!(r, Err(MediaError::Corrupt(ref m)) if m.starts_with("header:"));
    let finding =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fuzz-findings/wav-channel-overflow.wav");
    assert!(header_error(probe(&finding)), "{:?}", probe(&finding));
    let reader = |b: Vec<u8>| Media::Reader(Box::new(Cursor::new(b)), None);
    // WAV: 65535 channels; 16 channels of 65535 bits.
    let mut w = wav(&tone(10, RATE), 2, RATE, Pcm::I16);
    w[22..24].copy_from_slice(&u16::MAX.to_le_bytes());
    assert!(header_error(probe_media(reader(w.clone()))));
    w[22..24].copy_from_slice(&16u16.to_le_bytes());
    w[34..36].copy_from_slice(&u16::MAX.to_le_bytes());
    assert!(header_error(probe_media(reader(w))));
    // AIFF COMM: 65535 channels.
    let mut a = aiff(&tone(10, RATE), 2, RATE, Pcm::I16);
    a[20..22].copy_from_slice(&u16::MAX.to_be_bytes());
    assert!(header_error(probe_media(reader(a))));
    // Valid headers pass, and the reader is rewound for symphonia.
    assert!(probe_media(reader(aiff(&tone(10, RATE), 2, RATE, Pcm::I24))).is_ok());
}

/// Preload stays within its memory budget: over it is a typed error, never an allocation abort.
#[test]
fn preload_is_bounded_by_its_budget() {
    use openlab_audio::preload_within;
    // Declared size: the header claims 100 M stereo frames (800 MB of f32) but holds 100. The
    // budget check refuses it before decoding (a decode would end in `Truncated`).
    let mut w = wav(&tone(100, RATE), 2, RATE, Pcm::I16);
    w[4..8].copy_from_slice(&(36 + 400_000_000u32).to_le_bytes());
    w[40..44].copy_from_slice(&400_000_000u32.to_le_bytes());
    let r = preload_within(
        Media::Reader(Box::new(Cursor::new(w)), Some("wav".into())),
        1 << 20,
    );
    assert!(
        matches!(
            r,
            Err(MediaError::TooLarge {
                bytes: 800_000_000,
                limit: 1_048_576
            })
        ),
        "{:?}",
        r.map(|p| p.info())
    );
    // Expanding stream: this MP4 declares 22050 frames (176400 bytes) but decodes 24576
    // (196608 bytes); the decoded length is checked as it grows.
    let finding =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fuzz-findings/alac-short-index.m4a");
    let r = preload_within(Media::Path(finding.clone()), 180_000);
    assert!(
        matches!(r, Err(MediaError::TooLarge { limit: 180_000, bytes }) if bytes > 180_000),
        "{:?}",
        r.map(|p| p.info())
    );
    // At the budget it fits, with no slack beyond the samples.
    let p = preload_within(Media::Path(finding), 196_608).unwrap();
    assert_eq!(p.bytes(), 196_608);
    assert_eq!(openlab_audio::DEFAULT_PRELOAD_BUDGET, 512 << 20);
}
