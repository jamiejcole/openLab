// SPDX-License-Identifier: GPL-3.0-or-later
// Helpers outside #[test] fns are test code too; clippy's in-test allowances miss them.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! AAC encoder priming in MP4 (M3.14, AUD-04): the edit list (preferred) or the iTunSMPB tag
//! (fallback) says how many frames of encoder delay the file starts with; probe, preload and
//! streaming all start at the first real frame, and damaged values are ignored with a warning.
//!
//! `tone-onset*.m4a` (see fixtures/generate.sh): 0.5 s, 44.1 kHz stereo, silence for 1000
//! frames, then a tone starting at phase 0. ffmpeg's encoder delays it by 1024 frames. The
//! first file says so in its edit list; the second has no edit list.

mod common;

use std::f32::consts::TAU;
use std::io::Cursor;
use std::path::Path;

use common::media::*;
use openlab_audio::{Media, MediaError, WarningKind, preload, probe, probe_media};

const FRAMES: u64 = 22_050;
/// The encoder's delay in the fixtures.
const PRIMING: usize = 1_024;
const ONSET: usize = 1_000;

fn read(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name))
        .unwrap()
}

fn media(bytes: &[u8]) -> Media {
    Media::Reader(Box::new(Cursor::new(bytes.to_vec())), Some("m4a".into()))
}

/// The signal the fixtures encode, interleaved stereo.
fn reference(frames: usize) -> Vec<f32> {
    (0..frames)
        .flat_map(|i| {
            let t = i.saturating_sub(ONSET) as f32;
            let on = if i < ONSET { 0.0 } else { 1.0 };
            [
                on * 0.5 * (TAU * 440.0 * t / 44_100.0).sin(),
                on * 0.25 * (TAU * 1000.0 * t / 44_100.0).sin(),
            ]
        })
        .collect()
}

/// How many frames later than `reference` the decoded left channel is, over its first 6000
/// frames (the lag with the smallest squared error).
fn lag(got: &[f32]) -> i32 {
    let want = reference(6_000);
    let err = |lag: i32| -> f64 {
        (2_000..5_000i32)
            .map(|i| {
                let g = got.get((i + lag) as usize * 2).copied().unwrap_or(0.0);
                let w = want[i as usize * 2];
                f64::from(g - w).powi(2)
            })
            .sum()
    };
    (-1_500..=1_500).min_by(|&a, &b| err(a).total_cmp(&err(b))).unwrap()
}

fn max_err(got: &[f32], want: &[f32]) -> f32 {
    got.iter().zip(want).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max)
}

fn decoded(bytes: &[u8]) -> Vec<f32> {
    preload(media(bytes)).unwrap().samples().to_vec()
}

fn position(bytes: &[u8], needle: &[u8]) -> usize {
    bytes.windows(needle.len()).position(|w| w == needle).unwrap()
}

/// Overwrites the `elst` entry fields: version-0 layout, `[count][duration][media_time][rate]`.
fn edit_list(bytes: &[u8], count: u32, media_time: i32, rate: u32) -> Vec<u8> {
    let mut b = bytes.to_vec();
    let at = position(&b, b"elst") + 4 + 4; // type, version and flags
    b[at..at + 4].copy_from_slice(&count.to_be_bytes());
    b[at + 8..at + 12].copy_from_slice(&media_time.to_be_bytes());
    b[at + 12..at + 16].copy_from_slice(&rate.to_be_bytes());
    b
}

fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(kind);
    v.extend_from_slice(body);
    v
}

/// Replaces the one-entry edit list by one with two entries (a leading empty edit and the
/// priming), growing its parents; `moov` is the last box, so no offsets move.
fn two_entry_edit_list(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let at = position(bytes, b"elst") - 4;
    let mut body = vec![0, 0, 0, 0, 0, 0, 0, 2];
    for (dur, time) in [(1_000u32, -1i32), (22_050, 1_024)] {
        body.extend(dur.to_be_bytes());
        body.extend(time.to_be_bytes());
        body.extend(0x0001_0000u32.to_be_bytes());
    }
    let new = boxed(b"elst", &body);
    out.splice(at..at + 28, new);
    for parent in [&b"edts"[..], b"trak", b"moov"] {
        let p = position(bytes, parent) - 4;
        let size = u32::from_be_bytes(bytes[p..p + 4].try_into().unwrap()) + 12;
        out[p..p + 4].copy_from_slice(&size.to_be_bytes());
    }
    out
}

/// Appends iTunes' `udta > meta > ilst > ---- (mean, name, data)` gapless tag to the `moov`
/// box, which ffmpeg writes last.
fn with_smpb(bytes: &[u8], text: &str) -> Vec<u8> {
    let moov = position(bytes, b"moov") - 4;
    let size = u32::from_be_bytes(bytes[moov..moov + 4].try_into().unwrap()) as usize;
    assert_eq!(moov + size, bytes.len(), "moov is the last box");
    let mean = boxed(b"mean", &[&[0u8; 4][..], b"com.apple.iTunes"].concat());
    let name = boxed(b"name", &[&[0u8; 4][..], b"iTunSMPB"].concat());
    let data = boxed(b"data", &[&[0, 0, 0, 1, 0, 0, 0, 0][..], text.as_bytes()].concat());
    let item = boxed(b"----", &[mean, name, data].concat());
    let meta = boxed(b"meta", &[&[0u8; 4][..], &boxed(b"hdlr", &[0u8; 24]), &boxed(b"ilst", &item)].concat());
    let udta = boxed(b"udta", &meta);
    let mut out = bytes.to_vec();
    out.extend_from_slice(&udta);
    let new = (size + udta.len()) as u32;
    out[moov..moov + 4].copy_from_slice(&new.to_be_bytes());
    out
}

fn smpb(priming: u32) -> String {
    format!(" 00000000 {priming:08X} 000001CA 0000000000005622 00000000 00000000 00000000")
}

fn ignored(bytes: &[u8]) -> bool {
    probe_media(media(bytes))
        .unwrap()
        .warnings
        .iter()
        .any(|w| w.kind == WarningKind::PrimingIgnored)
}

#[test]
fn the_fixtures_really_start_with_the_encoders_delay() {
    // Without gapless information the decode includes the priming (what symphonia gives).
    let plain = read("tone-onset-noedit.m4a");
    assert!(!ignored(&plain));
    assert_eq!(probe_media(media(&plain)).unwrap().info.frames, FRAMES + PRIMING as u64);
    assert_eq!(lag(&decoded(&plain)), PRIMING as i32);
}

#[test]
fn edit_list_priming_is_trimmed_from_probe_and_preload() {
    let bytes = read("tone-onset.m4a");
    let probed = probe_media(media(&bytes)).unwrap();
    assert_eq!(probed.info.frames, FRAMES);
    assert!(probed.warnings.is_empty(), "{:?}", probed.warnings);
    let pre = preload(media(&bytes)).unwrap();
    assert_eq!(pre.info(), probed.info);
    assert!(pre.warnings().is_empty());
    let got = pre.samples();
    assert_eq!(got.len() as u64, FRAMES * 2);
    // The onset is on frame 1000 to the sample: no lag against the reference.
    assert_eq!(lag(got), 0);
    // AAC tolerance (the encoder smears the onset; the lag above is the exact check).
    let want = reference(FRAMES as usize);
    let err = max_err(&got[ONSET * 2..], &want[ONSET * 2..]);
    assert!(err < 0.06, "max error {err}");
    // Before the onset: silence up to the encoder's pre-echo.
    assert!(max_err(&got[..(ONSET - 100) * 2], &want[..(ONSET - 100) * 2]) < 0.06);
    // From a path too.
    let path = temp_file("tone-onset-path.m4a", &bytes);
    assert_eq!(probe(&path).unwrap().info.frames, FRAMES);
}

#[test]
fn itunsmpb_is_the_fallback_and_agrees_with_the_edit_list() {
    let plain = read("tone-onset-noedit.m4a");
    let tagged = with_smpb(&plain, &smpb(PRIMING as u32));
    assert_eq!(probe_media(media(&tagged)).unwrap().info.frames, FRAMES);
    let got = decoded(&tagged);
    assert_eq!(lag(&got), 0);
    // Same decoder, same frames: the tag gives exactly what the edit list gives.
    assert_eq!(got, decoded(&read("tone-onset.m4a")));
}

#[test]
fn priming_longer_than_a_packet_is_trimmed_across_packets() {
    let plain = read("tone-onset-noedit.m4a");
    let long = decoded(&with_smpb(&plain, &smpb(2_112)));
    let one = decoded(&read("tone-onset.m4a"));
    assert_eq!(long.len() as u64, (FRAMES - 1_088) * 2);
    assert_eq!(long[..], one[1_088 * 2..]);
    assert_eq!(
        probe_media(media(&with_smpb(&plain, &smpb(2_112)))).unwrap().info.frames,
        FRAMES - 1_088
    );
}

#[test]
fn the_edit_list_wins_over_the_tag() {
    let tagged = with_smpb(&read("tone-onset.m4a"), &smpb(2_112));
    assert_eq!(decoded(&tagged), decoded(&read("tone-onset.m4a")));
}

#[test]
fn an_edit_list_of_zero_trims_nothing_and_is_not_a_problem() {
    let bytes = edit_list(&read("tone-onset.m4a"), 1, 0, 0x0001_0000);
    let probed = probe_media(media(&bytes)).unwrap();
    assert!(probed.warnings.is_empty());
    assert_eq!(probed.info.frames, FRAMES + PRIMING as u64);
}

#[test]
fn damaged_or_absurd_edit_lists_are_ignored_with_a_warning() {
    let good = read("tone-onset.m4a");
    let cases: [(&str, Vec<u8>); 7] = [
        ("past the file", edit_list(&good, 1, 1_000_000, 0x0001_0000)),
        ("whole file", edit_list(&good, 1, FRAMES as i32 + PRIMING as i32, 0x0001_0000)),
        ("absurd", edit_list(&good, 1, 1 << 20, 0x0001_0000)),
        ("negative", edit_list(&good, 1, -1, 0x0001_0000)),
        ("min", edit_list(&good, 1, i32::MIN, 0x0001_0000)),
        ("speed", edit_list(&good, 1, 1_024, 0x0002_0000)),
        ("several", two_entry_edit_list(&good)),
    ];
    for (what, bytes) in cases {
        let probed = probe_media(media(&bytes)).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(probed.info.frames, FRAMES + PRIMING as u64, "{what}");
        assert!(
            probed.warnings.iter().any(|w| w.kind == WarningKind::PrimingIgnored),
            "{what}: {:?}",
            probed.warnings
        );
        // Preload agrees with the probe and plays the file as decoded.
        let pre = preload(media(&bytes)).unwrap();
        assert_eq!(pre.info(), probed.info, "{what}");
        assert_eq!(lag(pre.samples()), PRIMING as i32, "{what}");
    }
}

#[test]
fn a_damaged_edit_list_falls_back_to_the_tag() {
    let odd = two_entry_edit_list(&read("tone-onset.m4a"));
    let tagged = with_smpb(&odd, &smpb(PRIMING as u32));
    let probed = probe_media(media(&tagged)).unwrap();
    assert_eq!(probed.info.frames, FRAMES);
    assert!(probed.warnings.is_empty());
}

#[test]
fn damaged_tags_are_ignored_with_a_warning() {
    let plain = read("tone-onset-noedit.m4a");
    for text in [
        "",
        "not hex at all",
        " 00000000",
        " 00000000 ZZZZ 000001CA",
        " 00000000 FFFFFFFF 000001CA 0000000000005622",
        " 00000000 123456789 000001CA",
        &smpb(1 << 20),
        &smpb(PRIMING as u32 + FRAMES as u32 + 1),
        &"x".repeat(5_000),
    ] {
        let bytes = with_smpb(&plain, text);
        let probed = probe_media(media(&bytes)).unwrap();
        assert_eq!(probed.info.frames, FRAMES + PRIMING as u64, "{text:?}");
        assert!(
            probed.warnings.iter().any(|w| w.kind == WarningKind::PrimingIgnored),
            "{text:?}"
        );
        assert_eq!(preload(media(&bytes)).unwrap().info(), probed.info);
    }
}

#[test]
fn a_tag_that_is_not_itunsmpb_is_not_read() {
    let tagged = with_smpb(&read("tone-onset-noedit.m4a"), &smpb(PRIMING as u32));
    let mut other = tagged.clone();
    let at = position(&other, b"iTunSMPB");
    other[at..at + 8].copy_from_slice(b"iTunNORM");
    let probed = probe_media(media(&other)).unwrap();
    assert_eq!(probed.info.frames, FRAMES + PRIMING as u64);
    assert!(probed.warnings.is_empty());
}

#[test]
fn only_aac_is_trimmed() {
    // ALAC has no encoder delay: an edit list or a tag on it changes nothing.
    let alac = read("tone-alac.m4a");
    let base = decoded(&alac);
    let mut bytes = alac.clone();
    let at = position(&bytes, b"elst");
    let patched = at + 4 + 4 + 4 + 4 + 4; // type, version, count, duration, media_time at +4
    bytes[patched - 4..patched].copy_from_slice(&1_000i32.to_be_bytes());
    let tagged = with_smpb(&bytes, &smpb(2_112));
    assert_eq!(decoded(&tagged), base);
    let probed = probe_media(media(&tagged)).unwrap();
    assert_eq!(probed.info.frames, FRAMES);
    assert!(probed.warnings.is_empty());
}

#[test]
fn other_formats_decode_as_before() {
    for name in ["tone.flac", "tone.mp3", "tone.ogg", "tone-alac.m4a"] {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
        let probed = probe(&p).unwrap();
        assert!(probed.warnings.is_empty(), "{name}");
        if name != "tone.ogg" {
            assert_eq!(probed.info.frames, FRAMES, "{name}");
        }
        assert_eq!(preload(Media::Path(p)).unwrap().info(), probed.info, "{name}");
    }
}

#[test]
fn hostile_boxes_are_bounded() {
    // A flood of tiny boxes, deep nesting and sizes past the file: no hang, no panic, and the
    // file plays as symphonia reads it (or fails with a typed error).
    let good = read("tone-onset-noedit.m4a");
    let mut flood = good.clone();
    let moov = position(&flood, b"moov") - 4;
    flood.truncate(moov);
    flood.extend_from_slice(&[0, 0, 0, 8, b'f', b'r', b'e', b'e'].repeat(200_000));
    let mut deep = good.clone();
    deep.truncate(moov);
    let mut inner = boxed(b"elst", &[0u8; 24]);
    for _ in 0..200 {
        inner = boxed(b"moov", &inner);
    }
    deep.extend_from_slice(&inner);
    let mut huge = good.clone();
    huge[moov..moov + 4].copy_from_slice(&u32::MAX.to_be_bytes());
    let mut wide = good;
    wide.truncate(moov);
    wide.extend_from_slice(&[0, 0, 0, 1, b'm', b'o', b'o', b'v', 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
    for bytes in [flood, deep, huge, wide] {
        match probe_media(media(&bytes)) {
            Ok(_) | Err(MediaError::Unsupported(_) | MediaError::Corrupt(_)) => {}
            Err(e) => panic!("{e}"),
        }
        openlab_audio::fuzz_decode(&bytes).unwrap();
    }
}
