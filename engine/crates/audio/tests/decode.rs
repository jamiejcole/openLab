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
    let damage: Vec<MediaWarning> = probed
        .warnings
        .iter()
        .filter(|w| w.kind != WarningKind::SlowSeeks)
        .copied()
        .collect();
    assert_eq!(damage, want, "{ext}");
    // A file whose seeks would be slow says so, and is advised to be preloaded.
    let slow = probed
        .warnings
        .iter()
        .any(|w| w.kind == WarningKind::SlowSeeks);
    assert_eq!(slow, probed.preload_advised(), "{ext}");
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
            16_384,
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
        "tone-onset.m4a",
        "tone-onset-noedit.m4a",
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
/// `aac-damaged-tail.m4a`: damaged AAC packets decode or fail depending on decoder history, so
/// a seek and a straight read ended at different lengths (a packet now counts its container
/// duration either way).
/// `mp3-frames-after-end.mp3`: damage left frames after the packets the MP3 reader trims as end
/// padding; their timestamps were taken for missing audio, extending the timeline past the end
/// where seeks could not follow (the stream now ends at the container's end trim).
/// `flac-readable-only-from-start.flac`: only the first frame is readable, and symphonia's FLAC
/// reader gives it back when reading from the start but not after a seek.
/// `alac-empty-packets*.m4a`: damaged packets decode to nothing; during a seek those empty runs
/// were taken as covering their position, so a target inside the damage could not be found.
/// `mp3-damage-reads-differently-after-seek.mp3`: after damage symphonia's MP3 reader counts
/// and trims frames differently after a seek; a damaged file now seeks by reading from the
/// start.
/// `f32-non-finite.wav`: NaN and infinite float samples made the resampler's FFT panic.
/// `wav-channel-overflow.wav`: symphonia 0.6.1's WAV reader overflows `u16` on a huge channel
/// count (a panic with overflow checks on, as here); contained as a typed error.
/// `mp4-stts-overflow.m4a`: symphonia 0.6.1's `stts` reader sums sample counts in `u32`; the table
/// is rejected before symphonia parses it (see `pre_checked_mp4_stts_finding_is_rejected`).
/// `wav-1hz-resampler-oom.wav`: a 1 Hz sample rate made the resampler size an FFT chunk of tens
/// of millions of frames per channel (out of memory); rates below 8 kHz are now unplayable.
/// `flac-damaged-no-checkpoints.flac`: damage in a file too short for a seek checkpoint asked
/// for checkpoint 0 and underflowed (only with `cfg(fuzzing)`, where symphonia skips FLAC CRCs).
/// `id3-wav-extensible-mask-shift.wav`: an ID3 tag hid a 64-channel extensible WAV from the header
/// check, and symphonia 0.6.1 overflowed a shift fixing its channel mask; the embedded header is
/// now found and rejected first.
/// `non-wave-riff-start-wav-mask-shift.wav`: a RIFF whose form type is not `WAVE` came first, so
/// the scan for embedded headers was skipped, yet the probe went on to the 64-channel extensible
/// WAV behind it.
/// `junk-then-aiff-channel-overflow.aiff`: two junk bytes, then an AIFF with 32769 channels of 24
/// bits; the scan for embedded headers looked only for WAV markers, and symphonia 0.6.1's AIFF
/// reader overflowed `u16` computing the frame size.
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

/// Sets a canonical WAV's rate field (and byte rate, which nothing checks).
fn with_rate(mut w: Vec<u8>, rate: u32) -> Vec<u8> {
    w[24..28].copy_from_slice(&rate.to_le_bytes());
    w
}

fn reader(b: Vec<u8>) -> Media {
    Media::Reader(Box::new(Cursor::new(b)), Some("wav".into()))
}

/// The fuzz finding probes as unplayable (it used to probe fine and then exhaust memory when the
/// pipeline built a resampler for 1 Hz to 48 kHz), so nothing is ever sized from its rate.
#[test]
fn one_hz_wav_finding_is_unplayable() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fuzz-findings/wav-1hz-resampler-oom.wav");
    let r = probe(&path);
    assert!(
        matches!(&r, Err(MediaError::Unsupported(m)) if m.contains("sample rate 1 Hz")),
        "{r:?}"
    );
    assert!(matches!(
        preload(Media::Path(path)),
        Err(MediaError::Unsupported(_))
    ));
}

/// Sample rates outside 8 kHz..=384 kHz are unplayable; the bounds themselves play.
#[test]
fn sample_rates_outside_the_supported_range_are_unplayable() {
    for rate in [0, 1, 2, 100, 7_999, 384_001, u32::MAX / 4] {
        let w = with_rate(wav(&tone(64, 8_000), 2, 8_000, Pcm::I16), rate);
        let r = probe_media(reader(w));
        assert!(
            matches!(r, Err(MediaError::Unsupported(_))),
            "{rate}: {r:?}"
        );
    }
    for rate in [8_000, 8_001, 44_100, 384_000] {
        let w = with_rate(wav(&tone(64, 8_000), 2, 8_000, Pcm::I16), rate);
        let p = probe_media(reader(w)).unwrap();
        assert_eq!((p.info.sample_rate, p.info.frames), (rate, 64));
    }
}

/// The largest channel count at the lowest rate (biggest resampler buffers) and a data chunk that
/// claims 4 GiB in a tiny file both probe, preload and stream without large allocations.
#[test]
fn extreme_header_sizes_stay_bounded() {
    let w = wav(&tone(2_000, 8_000).repeat(64), 64, 8_000, Pcm::I16);
    assert_eq!(openlab_audio::fuzz_decode(&w), Ok(()));
    let mut w = wav(&tone(100, RATE), 2, RATE, Pcm::I16);
    let data_len = w.len() - 8;
    w[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    w[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(w.len() - 44, data_len - 36);
    assert_eq!(openlab_audio::fuzz_decode(&w), Ok(()));
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
    // Growing past the budget while decoding: this damaged Ogg file has no declared length
    // (the damage defeats symphonia's length scan), so only the decoded length can be checked.
    let mut ogg = std::fs::read(fixture("tone-4s.ogg")).unwrap();
    for x in &mut ogg[14_517..14_533] {
        *x ^= 0x5a;
    }
    let media = || Media::Reader(Box::new(Cursor::new(ogg.clone())), Some("ogg".into()));
    let frames = probe_media(media()).unwrap().info.frames;
    let need = frames * 2 * 4;
    let r = preload_within(media(), need - 1);
    assert!(
        matches!(r, Err(MediaError::TooLarge { limit, bytes }) if limit == need - 1 && bytes > limit),
        "{:?}",
        r.map(|p| p.info())
    );
    // At the budget it fits, with no slack beyond the samples.
    let p = preload_within(media(), need).unwrap();
    assert_eq!(p.bytes() as u64, need);
    assert_eq!(openlab_audio::DEFAULT_PRELOAD_BUDGET, 512 << 20);
}

/// The MP4 `stts` finding is refused by the pre-check, so symphonia's overflowing arithmetic is
/// never reached (with overflow checks on, as here, it would panic).
#[test]
fn pre_checked_mp4_stts_finding_is_rejected() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fuzz-findings/mp4-stts-overflow.m4a");
    let r = probe(&path);
    assert!(
        matches!(r, Err(MediaError::Corrupt(ref m)) if m.starts_with("header: mp4")),
        "{r:?}"
    );
}

/// An ID3v2 tag header (no frames) of `size` padding bytes, as MP3 taggers write before audio.
fn id3_tag(size: usize) -> Vec<u8> {
    let mut v = b"ID3\x04\x00\x00".to_vec();
    v.extend_from_slice(&[0, 0, (size >> 7) as u8 & 0x7f, size as u8 & 0x7f]);
    v.resize(10 + size, 0);
    v
}

/// `fuzz-findings/id3-wav-extensible-mask-shift.wav`: symphonia 0.6.1's `fix_wave_channel_mask`
/// shifts by the number of channels missing from the mask (`1 << 62` for 64 channels) or by the
/// mask's bit length (32 when bit 31 is set), a panic with overflow checks on. The probe skips a
/// leading ID3 tag by scanning for the RIFF marker, so the check finds the header past it too.
#[test]
fn extensible_channel_masks_symphonia_would_shift_out_of_range_are_rejected_first() {
    use common::wide::{self, Fmt, Header};
    let mask_error = |b: Vec<u8>| {
        let r = probe_media(reader(b));
        let ok =
            matches!(r, Err(MediaError::Corrupt(ref m)) if m.starts_with("header: channel mask"));
        if !ok {
            eprintln!("{r:?}");
        }
        ok
    };
    let ext = |channels: u16, mask: u32| {
        let samples = vec![0.0; usize::from(channels) * 4];
        wide::wav(&samples, channels, RATE, Fmt::I16, Header::Extensible(mask))
    };
    let after_id3 = |w: Vec<u8>| [id3_tag(20), w].concat();

    // The crash shape: an ID3 tag, then 64 channels with a short mask (a shift by 62).
    assert!(mask_error(after_id3(ext(64, 0x3))));
    // Also a 33-channel file (the smallest shift by 32), and a 2-channel file whose mask has
    // bit 31 set (a shift by the mask's bit length).
    assert!(mask_error(after_id3(ext(33, 0))));
    assert!(mask_error(after_id3(ext(2, 0x8000_0000))));
    // The same headers with no tag in front.
    assert!(mask_error(ext(2, 0x8000_0000)));
    assert!(mask_error(ext(8, 0x8000_00F0)));
    // The finding itself, as committed.
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fuzz-findings/id3-wav-extensible-mask-shift.wav");
    assert!(mask_error(std::fs::read(path).unwrap()));

    // Masks symphonia handles stay playable, with or without a tag.
    for w in [ext(2, 0x3), ext(2, 0), ext(2, 0x7), ext(4, 0x8000_000F)] {
        assert!(probe_media(reader(w.clone())).is_ok(), "{w:?}");
        assert!(probe_media(reader(after_id3(w))).is_ok());
    }
    // A mask with too many ones is trimmed from the top without a shift.
    assert!(probe_media(reader(ext(2, 0xFFFF_FFFF))).is_ok());
    // Ambisonic sub-formats never look at the mask.
    let mut amb = ext(4, 0x8000_0000);
    amb[48..50].copy_from_slice(&0x0721u16.to_le_bytes());
    assert!(!mask_error(amb));
}

/// A leading ID3 tag hid the header from the old check, which only looked at byte 0, so the
/// `u16` channel overflow it guards against was reachable again.
#[test]
fn pcm_header_checks_find_a_wav_after_a_leading_id3_tag() {
    let mut w = wav(&tone(10, RATE), 2, RATE, Pcm::I16);
    w[22..24].copy_from_slice(&u16::MAX.to_le_bytes());
    let r = probe_media(reader([id3_tag(40), w].concat()));
    assert!(
        matches!(r, Err(MediaError::Corrupt(ref m)) if m.starts_with("header:")),
        "{r:?}"
    );
    // A tag in front of a good file changes nothing.
    let good = [id3_tag(40), wav(&tone(10, RATE), 2, RATE, Pcm::I16)].concat();
    assert!(probe_media(reader(good)).is_ok());
}

/// The crash input of `fuzz-findings/non-wave-riff-start-wav-mask-shift.wav`, rebuilt: a start
/// that fails the probe's scoring (so it scans on), junk, then an extensible WAV with 64
/// channels and a short mask.
fn bad_start_then_wav(start: &[u8; 12]) -> Vec<u8> {
    use common::wide::{self, Fmt, Header};
    let samples = vec![0.0; 64 * 4];
    let w = wide::wav(&samples, 64, RATE, Fmt::I16, Header::Extensible(0x3));
    let junk = [0xFF, 0xFB, 0x90, 0x00, 0, 0, 0, 0, 7, 7, 7, 7, 7, 7];
    [&start[..], &junk[..], &w[..]].concat()
}

/// The scan for embedded headers is skipped only when the start is itself a complete header the
/// probe takes; a RIFF/FORM with another form type, or no `fmt `/`COMM` chunk, is scanned past.
#[test]
fn a_non_wave_container_start_does_not_hide_an_overflowing_wav_header() {
    for start in [
        b"RIFF\x24\x01\0\0WAV\t",
        b"RIFF\x24\x01\0\0AVI ",
        b"FORM\0\0\x01\x24AIFX",
    ] {
        let r = probe_media(reader(bad_start_then_wav(start)));
        assert!(
            matches!(r, Err(MediaError::Corrupt(ref m)) if m.starts_with("header: channel mask")),
            "{start:?}: {r:?}"
        );
    }
    // A RIFF/WAVE start with no `fmt ` chunk is not taken by the probe either.
    let mut no_fmt = b"RIFF\x24\x01\0\0WAVEjunk\0\0\0\0".to_vec();
    no_fmt.extend_from_slice(&bad_start_then_wav(b"RIFF\0\0\0\0WAV\t"));
    let r = probe_media(reader(no_fmt));
    assert!(matches!(r, Err(MediaError::Corrupt(_))), "{r:?}");
}

fn header_rejected<T: std::fmt::Debug>(r: &Result<T, MediaError>) -> bool {
    matches!(r, Err(MediaError::Corrupt(m)) if m.starts_with("header:"))
}

/// `fuzz-findings/junk-then-aiff-channel-overflow.aiff`: the embedded-header scan finds AIFF and
/// AIFC containers behind leading junk, not only WAV, and rejects a channel count symphonia's
/// AIFF reader would overflow on (probe, preload and the fuzz pipeline all fail typed).
#[test]
fn an_aiff_behind_leading_junk_is_header_checked() {
    let finding = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fuzz-findings/junk-then-aiff-channel-overflow.aiff");
    let r = probe(&finding);
    assert!(header_rejected(&r), "{r:?}");
    assert!(header_rejected(&preload(Media::Path(finding.clone()))));
    assert!(openlab_audio::fuzz_decode(&std::fs::read(&finding).unwrap()).is_ok());
    for form in [b"AIFF", b"AIFC"] {
        let mut a = aiff(&tone(10, RATE), 2, RATE, Pcm::I24);
        a[8..12].copy_from_slice(form);
        a[20..22].copy_from_slice(&0x8001u16.to_be_bytes());
        for junk in [&[0xFF, 0xF9][..], &id3_tag(40)[..], &[0u8; 1001][..]] {
            let r = probe_media(reader([junk, &a[..]].concat()));
            assert!(
                header_rejected(&r),
                "{form:?} after {} bytes: {r:?}",
                junk.len()
            );
        }
    }
    // A good AIFF behind junk is not refused by the check.
    let good = [&[0xFF, 0xF9][..], &aiff(&tone(10, RATE), 2, RATE, Pcm::I24)].concat();
    let r = probe_media(reader(good));
    assert!(!header_rejected(&r), "{r:?}");
}
