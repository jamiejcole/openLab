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
    stream(media(), info.frames, probed.seeking.clone())
        .map_err(|e| format!("probe {info:?} but streaming: {e}"))
}

fn damage(warnings: &[MediaWarning]) -> Vec<MediaWarning> {
    warnings
        .iter()
        .filter(|w| w.kind != WarningKind::SlowSeeks)
        .copied()
        .collect()
}

fn stream(media: Media, frames: u64, seeking: Seeking) -> Result<(), MediaError> {
    let mut pipe = Pipeline::new(open_source(media, seeking)?, frames, DEVICE_RATE)?;
    let ch = pipe.channels();
    let mut buf = vec![0.0; 1024 * ch];
    while pipe.read(&mut buf)? > 0 {}
    let total = pipe.position();
    for at in [total / 2, 0, total.saturating_sub(1), total / 3] {
        pipe.seek(at)?;
        pipe.read(&mut buf)?;
    }
    Ok(())
}
