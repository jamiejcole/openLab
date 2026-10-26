// SPDX-License-Identifier: GPL-3.0-or-later

//! Entry point for the decode fuzz target (`engine/fuzz/fuzz_targets/decode.rs`). Not API.

use std::io::Cursor;
use std::sync::atomic::AtomicBool;

use crate::decode::{
    Media, MediaError, MediaWarning, Seeking, WarningKind, open_source, preload, probe_media,
};
use crate::resample::Pipeline;
use crate::waveform::build_waveform;

/// Device rate the pipeline resamples to: most inputs then go through the resampler.
const DEVICE_RATE: u32 = 48_000;

/// Probes, preloads and streams `data` with seeks. Bad input must give a typed [`MediaError`];
/// a panic in a decoder is a bug the fuzzer reports (its panic hook aborts before
/// [`contain`](crate::decode::contain) can catch it). Returns an `Err` describing a broken
/// invariant: whatever probes must preload to the same format, length and warnings, and must
/// stream and seek without error (a cue that loads must not fail during the show), damaged or
/// not.
pub fn fuzz_decode(data: &[u8]) -> Result<(), String> {
    let media = || Media::Reader(Box::new(Cursor::new(data.to_vec())), None);
    let Ok(probed) = probe_media(media()) else {
        return Ok(());
    };
    let info = probed.info;
    if info.channels == 0 || info.sample_rate == 0 || info.frames == 0 {
        return Err(format!("probe accepted {info:?}"));
    }
    match preload(media()) {
        // The probe also says when seeks will be slow; damage itself must match.
        Ok(p) if p.info() == info && p.warnings() == damage(&probed.warnings).as_slice() => {}
        // Over the memory budget is a legitimate outcome (the cue streams instead).
        Err(MediaError::TooLarge { .. }) => {}
        other => {
            return Err(format!(
                "probe {info:?} but preload {:?}",
                other.map(|p| p.info())
            ));
        }
    }
    // The overview decodes the same file once more; it may fail but never lie about the length.
    if let Ok(r) = build_waveform(media(), &info, &AtomicBool::new(false))
        && r.waveform.frames > info.frames
    {
        return Err(format!(
            "waveform of {} frames for a probe of {info:?}",
            r.waveform.frames
        ));
    }
    // Seeks in a damaged file verify checkpoints or decode from the start, and mutated inputs
    // are mostly damaged: one seek keeps the target's throughput up.
    let seeks = if damage(&probed.warnings).is_empty() {
        SEEK_POINTS.len()
    } else {
        1
    };
    stream(media(), info.frames, probed.seeking.clone(), seeks)
        .map_err(|e| format!("probe {info:?} but streaming: {e}"))
}

fn damage(warnings: &[MediaWarning]) -> Vec<MediaWarning> {
    warnings
        .iter()
        .filter(|w| w.kind != WarningKind::SlowSeeks)
        .copied()
        .collect()
}

/// Seek targets as fractions of the length (numerator, denominator): the middle first, so a
/// capped run still seeks somewhere.
const SEEK_POINTS: [(u64, u64); 4] = [(1, 2), (0, 1), (1, 1), (1, 3)];

fn stream(media: Media, frames: u64, seeking: Seeking, seeks: usize) -> Result<(), MediaError> {
    let mut pipe = Pipeline::new(open_source(media, seeking)?, frames, DEVICE_RATE)?;
    let ch = pipe.channels();
    let mut buf = vec![0.0; 1024 * ch];
    while pipe.read(&mut buf)? > 0 {}
    let total = pipe.position();
    for &(num, den) in SEEK_POINTS.iter().take(seeks.max(1)) {
        let at = if num == den {
            total.saturating_sub(1)
        } else {
            total / den * num
        };
        pipe.seek(at)?;
        pipe.read(&mut buf)?;
    }
    Ok(())
}
