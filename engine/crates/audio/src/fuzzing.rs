// SPDX-License-Identifier: GPL-3.0-or-later

//! Entry point for the decode fuzz target (`engine/fuzz/fuzz_targets/decode.rs`). Not API.

use std::io::Cursor;

use crate::decode::{Media, MediaError, open_source, preload, probe_media};
use crate::resample::Pipeline;

/// Device rate the pipeline resamples to: most inputs then go through the resampler.
const DEVICE_RATE: u32 = 48_000;

/// Probes, preloads and streams `data` with seeks. Bad input must give a typed [`MediaError`];
/// a panic in a decoder is a bug the fuzzer reports (its panic hook aborts before
/// [`contain`](crate::decode::contain) can catch it). Returns an `Err` describing a broken
/// invariant: whatever probes must preload to the same format and length, and must stream and
/// seek without error (a cue that loads must not fail during the show).
pub fn fuzz_decode(data: &[u8]) -> Result<(), String> {
    let media = || Media::Reader(Box::new(Cursor::new(data.to_vec())), None);
    let Ok(info) = probe_media(media()) else {
        return Ok(());
    };
    if info.channels == 0 || info.sample_rate == 0 || info.frames == 0 {
        return Err(format!("probe accepted {info:?}"));
    }
    match preload(media()) {
        Ok(p) if p.info() == info => {}
        other => {
            return Err(format!(
                "probe {info:?} but preload {:?}",
                other.map(|p| p.info())
            ));
        }
    }
    stream(media(), info.frames).map_err(|e| format!("probe {info:?} but streaming: {e}"))
}

fn stream(media: Media, frames: u64) -> Result<(), MediaError> {
    let mut pipe = Pipeline::new(open_source(media)?, frames, DEVICE_RATE)?;
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
