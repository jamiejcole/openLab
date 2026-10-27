// SPDX-License-Identifier: GPL-3.0-or-later
// Helpers outside #[test] fns are test code too; clippy's in-test allowances miss them.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Many-channel PCM (M3.23, AUD-03, AUD-04): uncompressed WAV and AIFF of up to 64 channels decode
//! sample-exactly, seek anywhere, stream into a 64-output mixer and give a waveform. symphonia
//! cannot read these files; the in-house reader does. Bad headers are `Corrupt`, never a panic.

mod common;

use std::io::Cursor;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use common::media::temp_file;
use common::wide::{self, Fmt, Header};
use common::*;
use openlab_audio::{
    DiskStreamer, Media, MediaError, Plays, Seeking, StreamRequest, StreamerConfig, VoiceId,
    WarningKind, build_waveform, fuzz_decode, plan, preload, preload_within, probe, probe_media,
};

const RATE: u32 = 48_000;
const FAILSAFE: Duration = Duration::from_secs(10);
/// Frame the streamed voice starts at (a start needs a little lead).
const START: u64 = 100;

fn reader(bytes: Vec<u8>) -> Media {
    Media::Reader(Box::new(Cursor::new(bytes)), None)
}

fn as_f32(x: &[f64]) -> Vec<f32> {
    x.iter().map(|&v| v as f32).collect()
}

/// Probes and preloads `bytes` (as a path and as a reader) and checks the exact samples.
fn assert_decodes(bytes: &[u8], channels: u16, frames: u64, want: &[f64], what: &str) {
    let want = as_f32(want);
    let path = temp_file("wide.bin", bytes);
    let media = |i: usize| {
        if i == 0 {
            Media::Path(path.clone())
        } else {
            reader(bytes.to_vec())
        }
    };
    for i in 0..2 {
        let probed = probe_media(media(i)).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(probed.info.channels, channels, "{what}");
        assert_eq!(probed.info.sample_rate, RATE, "{what}");
        assert_eq!(probed.info.frames, frames, "{what}");
        assert!(probed.warnings.is_empty(), "{what}: {:?}", probed.warnings);
        let p = preload(media(i)).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(p.info(), probed.info, "{what}");
        assert!(p.samples() == want.as_slice(), "{what}: samples differ");
    }
}

#[test]
fn wav_of_64_channels_decodes_sample_exactly_in_every_format() {
    for fmt in Fmt::ALL {
        for header in [Header::Plain, Header::Extensible(0)] {
            let want = fmt.samples(300, 64);
            let bytes = wide::wav(&want, 64, RATE, fmt, header);
            assert_decodes(&bytes, 64, 300, &want, &format!("{fmt:?} {header:?}"));
        }
    }
}

#[test]
fn aiff_of_64_channels_decodes_sample_exactly_in_every_format() {
    let cases: [(Fmt, Option<&[u8; 4]>); 11] = [
        (Fmt::I8, None),
        (Fmt::I16, None),
        (Fmt::I24, None),
        (Fmt::I32, None),
        (Fmt::I16, Some(b"NONE")),
        (Fmt::I16, Some(b"sowt")),
        (Fmt::I16, Some(b"twos")),
        (Fmt::I24, Some(b"in24")),
        (Fmt::I32, Some(b"in32")),
        (Fmt::F32, Some(b"fl32")),
        (Fmt::F64, Some(b"fl64")),
    ];
    for (fmt, comp) in cases {
        let want = fmt.samples(300, 64);
        let bytes = wide::aiff(&want, 64, RATE, fmt, comp);
        assert_decodes(&bytes, 64, 300, &want, &format!("{fmt:?} {comp:?}"));
    }
}

#[test]
fn every_channel_count_above_symphonias_native_range_decodes() {
    for ch in [9u16, 16, 18, 19, 24, 26, 27, 33, 48, 63, 64] {
        let fmt = Fmt::I24;
        let want = fmt.samples(50, usize::from(ch));
        let w = wide::wav(&want, ch, RATE, fmt, Header::Plain);
        assert_decodes(&w, ch, 50, &want, &format!("wav {ch}"));
        let a = wide::aiff(&want, ch, RATE, fmt, None);
        assert_decodes(&a, ch, 50, &want, &format!("aiff {ch}"));
    }
}

/// The channel mask names speaker positions; openLab routes by index, so any mask plays, even
/// one that names more or fewer speakers than the file has channels.
#[test]
fn channel_layout_masks_beyond_18_channels_are_ignored() {
    for ch in [19u16, 24, 32, 64] {
        for mask in [0, 0x0003_FFFF, 0x0000_0003, u32::MAX, 0x8000_0001] {
            let fmt = Fmt::I24;
            let want = fmt.samples(40, usize::from(ch));
            let bytes = wide::wav(&want, ch, RATE, fmt, Header::Extensible(mask));
            assert_decodes(
                &bytes,
                ch,
                40,
                &want,
                &format!("{ch} channels, mask {mask:#x}"),
            );
        }
    }
}

#[test]
fn narrow_files_still_decode_through_symphonia() {
    let want = Fmt::I16.samples(100, 2);
    let bytes = wide::wav(&want, 2, RATE, Fmt::I16, Header::Plain);
    assert_decodes(&bytes, 2, 100, &want, "stereo");
}

/// A DiskStreamer-fed 64-output mixer: the voice carries every channel of the file to its own
/// output, for trims that seek to arbitrary frames and a loop that seeks back.
#[test]
fn a_64_channel_file_streams_into_a_64_output_mixer_with_exact_seeks() {
    const FRAMES: u64 = 12_000;
    for (name, bytes) in [
        (
            "wav",
            wide::wav(
                &Fmt::I24.samples(FRAMES, 64),
                64,
                RATE,
                Fmt::I24,
                Header::Extensible(0x0003_FFFF),
            ),
        ),
        (
            "aiff",
            wide::aiff(
                &Fmt::F32.samples(FRAMES, 64),
                64,
                RATE,
                Fmt::F32,
                Some(b"fl32"),
            ),
        ),
    ] {
        let fmt = if name == "wav" { Fmt::I24 } else { Fmt::F32 };
        let path = temp_file(&format!("wide.{name}"), &bytes);
        let info = probe(&path).unwrap().info;
        assert_eq!((info.channels, info.frames), (64, FRAMES));
        // Seeks to an odd frame, then three plays of a loop (a seek back each time).
        let slices = plan(
            3_217..9_001,
            &[4_000],
            &[Plays::Times(1), Plays::Times(2)],
            3_217,
        )
        .unwrap();
        let mut cfg = config(64);
        cfg.max_voice_channels = 64;
        let (mut ctl, mut r) = setup(cfg);
        let mut c = StreamerConfig::new(RATE);
        c.buffer_ms = 50;
        let mut d = DiskStreamer::new(c).unwrap();
        let p = d
            .prepare(StreamRequest {
                media: Media::Path(path),
                info,
                seeking: Seeking::Direct,
                slices: slices.clone(),
            })
            .unwrap();
        assert!(p.handle.wait_primed(FAILSAFE).unwrap());
        let (mut v, h) = p.into_voice(VoiceId(1), ctl.cue_outputs());
        assert_eq!(v.channels, 64);
        for i in 0..64 {
            v.set_crosspoint(i, i, 1.0);
        }
        send_start(&mut ctl, START, v);
        let want: Vec<(u64, u64)> = slices
            .iter()
            .flat_map(|s| {
                let Plays::Times(n) = s.plays else {
                    panic!("finite")
                };
                (0..n).map(move |_| (s.start, s.frames))
            })
            .collect();
        let total: u64 = want.iter().map(|w| w.1).sum();
        let mut out = vec![0.0f32; 256 * 64];
        let mut done = 0u64;
        let mut plays = want.iter().flat_map(|&(s, n)| s..s + n);
        while done < total + START {
            let n = 256.min((total + START - done) as usize);
            let need = ctl.samples_rendered() + n as u64;
            assert!(h.wait_written(need, FAILSAFE).unwrap(), "stream stalled");
            let block = &mut out[..n * 64];
            no_alloc(|| r.render_into(block, &[n]));
            ctl.pump();
            for frame in block.chunks(64) {
                if done < START {
                    assert!(frame.iter().all(|&x| x == 0.0), "audio before the start");
                    done += 1;
                    continue;
                }
                let at = plays.next().unwrap();
                for (c, &got) in frame.iter().enumerate() {
                    assert_eq!(
                        got,
                        fmt.value(at, c) as f32,
                        "{name}: file frame {at} ch {c}"
                    );
                }
                done += 1;
            }
        }
        assert_eq!(ctl.underrun_frames(), 0);
    }
}

#[test]
fn a_64_channel_waveform_has_every_channels_peaks() {
    // Channel c holds the constant (c + 1) / 65.
    let frames = 10_000u64;
    let samples: Vec<f64> = (0..frames)
        .flat_map(|_| (0..64).map(|c| f64::from(c + 1) / 65.0))
        .collect();
    let bytes = wide::wav(&samples, 64, RATE, Fmt::I16, Header::Plain);
    let info = probe_media(reader(bytes.clone())).unwrap().info;
    let r = build_waveform(reader(bytes), &info, &AtomicBool::new(false)).unwrap();
    assert!(r.warnings.is_empty());
    assert_eq!((r.waveform.channels, r.waveform.frames), (64, frames));
    for c in 0..64usize {
        let peaks = r.waveform.range(c, 0, frames, 1);
        let want = ((c as f32 + 1.0) / 65.0 * 32767.0).round() as i16;
        let (lo, hi) = (i32::from(peaks[0].min), i32::from(peaks[0].max));
        assert!(
            (hi - i32::from(want)).abs() <= 1 && (lo - i32::from(want)).abs() <= 1,
            "channel {c}: {:?} vs {want}",
            peaks[0]
        );
    }
}

#[test]
fn a_64_channel_file_over_the_preload_budget_is_too_large() {
    let want = Fmt::I16.samples(1000, 64);
    let bytes = wide::wav(&want, 64, RATE, Fmt::I16, Header::Plain);
    // 1000 frames x 64 channels x 4 bytes of f32.
    assert!(matches!(
        preload_within(reader(bytes.clone()), 255_999),
        Err(MediaError::TooLarge {
            bytes: 256_000,
            limit: 255_999
        })
    ));
    assert!(preload_within(reader(bytes), 256_000).is_ok());
}

// --- damaged and hostile headers --------------------------------------------------------------

fn corrupt(r: Result<openlab_audio::Probed, MediaError>) -> bool {
    matches!(r, Err(MediaError::Corrupt(_)))
}

fn wav64(header: Header) -> Vec<u8> {
    wide::wav(&Fmt::I24.samples(8, 64), 64, RATE, Fmt::I24, header)
}

fn find(bytes: &[u8], tag: &[u8; 4]) -> usize {
    bytes.windows(4).position(|w| w == tag).unwrap()
}

#[test]
fn every_truncation_of_a_wide_header_is_corrupt_or_a_typed_error() {
    for header in [Header::Plain, Header::Extensible(0)] {
        let full = wav64(header);
        let data = find(&full, b"data") + 8;
        for len in 0..data + 1 {
            let r = probe_media(reader(full[..len].to_vec()));
            // Once the whole `fmt ` chunk is there the file is recognised as wide PCM.
            if len >= 12 + 8 + 16 + 24 {
                assert!(corrupt(r.clone()), "{header:?} cut at {len}: {r:?}");
            } else {
                assert!(r.is_err(), "{header:?} cut at {len}: {r:?}");
            }
        }
    }
    let aiff = wide::aiff(&Fmt::I24.samples(8, 64), 64, RATE, Fmt::I24, None);
    let data = find(&aiff, b"SSND") + 16;
    for len in 0..=data {
        let r = probe_media(reader(aiff[..len].to_vec()));
        assert!(r.is_err(), "aiff cut at {len}: {r:?}");
        if len >= 12 + 8 + 18 {
            assert!(corrupt(r.clone()), "aiff cut at {len}: {r:?}");
        }
    }
}

#[test]
fn cut_audio_data_plays_to_its_real_end_with_a_truncated_warning() {
    let full = wav64(Header::Plain);
    // 8 frames of 192 bytes; losing 100 bytes loses the last frame and part of the one before.
    let cut = full[..full.len() - 100].to_vec();
    let probed = probe_media(reader(cut.clone())).unwrap();
    assert_eq!(probed.info.frames, 7);
    let warning = probed
        .warnings
        .iter()
        .find(|w| w.kind == WarningKind::Truncated);
    assert_eq!(warning.map(|w| (w.at, w.frames)), Some((7, 1)));
    let p = preload(reader(cut)).unwrap();
    assert_eq!(p.info(), probed.info);
    assert_eq!(p.warnings(), probed.warnings.as_slice());
    // No whole frame left.
    let none = full[..find(&full, b"data") + 8 + 100].to_vec();
    assert!(corrupt(probe_media(reader(none))));
}

#[test]
fn bad_wide_wav_headers_are_typed_errors() {
    let base = wav64(Header::Plain);
    let patch = |at: usize, v: &[u8]| {
        let mut b = base.clone();
        b[at..at + v.len()].copy_from_slice(v);
        reader(b)
    };
    // fmt body starts at 20: tag 20, channels 22, rate 24, block align 32, bits 34.
    assert!(
        corrupt(probe_media(patch(32, &1u16.to_le_bytes()))),
        "block align"
    );
    assert!(
        corrupt(probe_media(patch(22, &0u16.to_le_bytes()))),
        "no channels"
    );
    assert!(
        corrupt(probe_media(patch(22, &65u16.to_le_bytes()))),
        "65 channels"
    );
    assert!(matches!(
        probe_media(patch(24, &1u32.to_le_bytes())),
        Err(MediaError::Unsupported(_))
    ));
    assert!(matches!(
        probe_media(patch(34, &12u16.to_le_bytes())),
        Err(MediaError::Unsupported(_))
    ));
    // ADPCM-style tag with 64 channels.
    assert!(probe_media(patch(20, &2u16.to_le_bytes())).is_err());
    // An empty data chunk, and a skipped chunk that claims more than the file holds.
    let data = find(&base, b"data");
    assert!(corrupt(probe_media(patch(data + 4, &0u32.to_le_bytes()))));
    let list = find(&base, b"JUNK");
    assert!(corrupt(probe_media(patch(
        list + 4,
        &u32::MAX.to_le_bytes()
    ))));
    // A data chunk that claims more than the file holds plays what is there.
    let probed = probe_media(patch(data + 4, &0x7fff_fff0u32.to_le_bytes())).unwrap();
    assert_eq!(probed.info.frames, 8);
    assert!(
        probed
            .warnings
            .iter()
            .any(|w| w.kind == WarningKind::Truncated)
    );
    // The streaming convention (0xFFFFFFFF) is "to the end of the file", without a warning.
    let open = probe_media(patch(data + 4, &u32::MAX.to_le_bytes())).unwrap();
    assert_eq!(open.info.frames, 8);
    assert!(open.warnings.is_empty());
    // data before fmt.
    let mut swapped = base.clone();
    swapped[12..16].copy_from_slice(b"data");
    assert!(corrupt(probe_media(reader(swapped))));
    // Extensible: a wrong sub-format GUID and valid bits above the container.
    let ext = wav64(Header::Extensible(0));
    let mut bad_guid = ext.clone();
    bad_guid[50] ^= 0xff;
    assert!(matches!(
        probe_media(reader(bad_guid)),
        Err(MediaError::Unsupported(_))
    ));
    let mut bad_valid = ext;
    bad_valid[38..40].copy_from_slice(&32u16.to_le_bytes());
    assert!(corrupt(probe_media(reader(bad_valid))));
}

#[test]
fn bad_wide_aiff_headers_are_typed_errors() {
    let base = wide::aiff(&Fmt::I24.samples(8, 64), 64, RATE, Fmt::I24, None);
    let comm = find(&base, b"COMM") + 8;
    let patch = |at: usize, v: &[u8]| {
        let mut b = base.clone();
        b[at..at + v.len()].copy_from_slice(v);
        reader(b)
    };
    assert!(
        corrupt(probe_media(patch(comm, &0u16.to_be_bytes()))),
        "no channels"
    );
    assert!(
        corrupt(probe_media(patch(comm, &65u16.to_be_bytes()))),
        "65 channels"
    );
    assert!(matches!(
        probe_media(patch(comm + 6, &0u16.to_be_bytes())),
        Err(MediaError::Unsupported(_))
    ));
    assert!(matches!(
        probe_media(patch(comm + 6, &33u16.to_be_bytes())),
        Err(MediaError::Unsupported(_))
    ));
    // Sample rate: zero mantissa, negative, and a huge exponent.
    assert!(corrupt(probe_media(patch(
        comm + 8,
        &[0x40, 0x0e, 0, 0, 0, 0, 0, 0, 0, 0]
    ))));
    assert!(corrupt(probe_media(patch(
        comm + 8,
        &[0xc0, 0x0e, 0x80, 0, 0, 0, 0, 0, 0, 0]
    ))));
    assert!(corrupt(probe_media(patch(
        comm + 8,
        &[0x7f, 0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0]
    ))));
    // SSND offset past its chunk; SSND before COMM.
    let ssnd = find(&base, b"SSND") + 8;
    assert!(corrupt(probe_media(patch(ssnd, &u32::MAX.to_be_bytes()))));
    let ssnd_at = ssnd - 8;
    let mut early = base[..12].to_vec();
    early.extend_from_slice(&base[ssnd_at..]);
    early.extend_from_slice(&base[12..ssnd_at]);
    assert!(corrupt(probe_media(reader(early))));
    // A frame count above what the file holds plays what is there.
    let probed = probe_media(patch(comm + 2, &1000u32.to_be_bytes())).unwrap();
    assert_eq!(probed.info.frames, 8);
    assert!(
        probed
            .warnings
            .iter()
            .any(|w| w.kind == WarningKind::Truncated)
    );
    // Compressed AIFF-C with 64 channels is not ours to play.
    let mut aifc = wide::aiff(&Fmt::I16.samples(8, 64), 64, RATE, Fmt::I16, Some(b"ulaw"));
    assert!(matches!(
        probe_media(reader(aifc.clone())),
        Err(MediaError::Unsupported(_))
    ));
    aifc[0] = b'X';
    assert!(probe_media(reader(aifc)).is_err());
}

/// The decode fuzz target's invariants on damaged copies of wide files: flips of every header
/// byte, and cuts. Anything that probes must preload and stream as probed.
#[test]
fn damaged_wide_files_hold_the_fuzz_invariants() {
    let mut files = vec![
        wav64(Header::Plain),
        wav64(Header::Extensible(0x3_ffff)),
        wide::aiff(&Fmt::I24.samples(8, 64), 64, RATE, Fmt::I24, None),
        wide::aiff(&Fmt::F64.samples(8, 12), 12, RATE, Fmt::F64, Some(b"fl64")),
    ];
    files.push(wide::wav(
        &Fmt::F32.samples(8, 12),
        12,
        RATE,
        Fmt::F32,
        Header::Plain,
    ));
    for f in &files {
        fuzz_decode(f).unwrap();
        // Every header byte (the audio data after it is only samples).
        let head = f.len().min(100);
        for i in 0..head {
            for v in [0x00, 0x80, 0xff] {
                let mut b = f.clone();
                b[i] = v;
                // A changed rate only adds resampler cost (seconds in a debug build for rates
                // coprime with the device's), which other tests cover.
                if let Ok(p) = probe_media(reader(b.clone()))
                    && p.info.sample_rate != RATE
                {
                    continue;
                }
                fuzz_decode(&b).unwrap();
            }
        }
        for cut in (0..f.len()).step_by(97) {
            fuzz_decode(&f[..cut]).unwrap();
        }
    }
}

/// The fuzz corpus seeds for the wide reader must stay valid files, or they stop seeding it.
#[test]
fn wide_fuzz_seeds_are_playable() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/decode-seeds");
    for (name, channels) in [
        ("wide-s16-24ch.wav", 24),
        ("wide-s16-64ch-extensible.wav", 64),
        ("wide-s24-12ch.aiff", 12),
    ] {
        let bytes = std::fs::read(dir.join(name)).unwrap();
        let probed = probe_media(reader(bytes.clone())).unwrap();
        assert_eq!(probed.info.channels, channels, "{name}");
        assert!(probed.warnings.is_empty(), "{name}");
        fuzz_decode(&bytes).unwrap();
    }
}

/// Ambisonic B-format sub-GUIDs (PCM and float) play like plain PCM and float, at 9 to 18
/// channels (where symphonia used to read them) and beyond.
#[test]
fn ambisonic_b_format_extensible_files_decode() {
    for (fmt, tag) in [(Fmt::I24, 1u8), (Fmt::F32, 3)] {
        for ch in [16u16, 25] {
            let want = fmt.samples(30, usize::from(ch));
            let mut b = wide::wav(&want, ch, RATE, fmt, Header::Extensible(0));
            // fmt body at 20; sub-format GUID at 44: tag, then the ambisonic tail.
            let guid = [
                tag, 0, 0, 0, 0x21, 0x07, 0xd3, 0x11, 0x86, 0x44, 0xc8, 0xc1, 0xca, 0, 0, 0,
            ];
            b[44..60].copy_from_slice(&guid);
            assert_decodes(&b, ch, 30, &want, &format!("ambisonic {fmt:?} {ch}"));
        }
    }
}

/// Non-PCM format tags with 9 or more channels stay on symphonia's path, not our error.
#[test]
fn wide_non_pcm_wav_tags_are_left_to_symphonia() {
    let mut b = wav64(Header::Plain);
    b[20..22].copy_from_slice(&6u16.to_le_bytes());
    let r = probe_media(reader(b));
    assert!(
        !matches!(&r, Err(MediaError::Unsupported(m)) if m.starts_with("WAV format tag")),
        "{r:?}"
    );
}
