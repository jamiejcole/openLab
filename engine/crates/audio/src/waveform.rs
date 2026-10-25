// SPDX-License-Identifier: GPL-3.0-or-later

//! Waveform overview data (AUD-14): a min/max peak pyramid per channel, at the file's own rate.
//! Level 0 holds one min/max pair per [`BASE_FRAMES`] frames; each further level halves the
//! resolution until one bucket covers the whole file. The builder is pure; [`build_waveform`]
//! feeds it from the existing decoder and runs on a worker thread, never the audio or control
//! thread.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::Seeking;
use crate::decode::{Media, MediaError, MediaInfo, MediaWarning, WarningKind, open_source};

/// Source frames per level-0 bucket (5.3 ms at 48 kHz).
pub const BASE_FRAMES: u64 = 256;
/// Most output buckets one [`Waveform::range`] call returns; more are never useful on screen.
pub const MAX_RANGE_BUCKETS: usize = 1 << 16;
/// Peaks are stored as `round(x · PEAK_SCALE)`, `x` clamped to ±1.
pub const PEAK_SCALE: f32 = 32767.0;
/// Frames the decoder is asked for at a time (cancellation latency).
const CHUNK_FRAMES: usize = 4096;

/// Minimum and maximum of a stretch of one channel, quantised to `i16` (full scale = ±32767).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Peak {
    pub min: i16,
    pub max: i16,
}

impl Peak {
    pub fn min_f32(self) -> f32 {
        f32::from(self.min) / PEAK_SCALE
    }

    pub fn max_f32(self) -> f32 {
        f32::from(self.max) / PEAK_SCALE
    }

    fn merge(self, o: Peak) -> Peak {
        Peak {
            min: self.min.min(o.min),
            max: self.max.max(o.max),
        }
    }
}

/// Sanitised sample as a quantised value: NaN and ±∞ are 0, the rest is clamped to ±1.
fn quantise(x: f32) -> i16 {
    if !x.is_finite() {
        return 0;
    }
    // In ±32767 after the clamp, so the cast cannot truncate.
    (x.clamp(-1.0, 1.0) * PEAK_SCALE).round() as i16
}

/// One resolution of the pyramid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Level {
    /// Source frames per bucket (the last bucket may cover fewer).
    pub frames_per_bucket: u64,
    /// `channels[c][i]` covers frames `i · frames_per_bucket ..` of channel `c`.
    pub channels: Vec<Vec<Peak>>,
}

impl Level {
    pub fn buckets(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }
}

/// The finished pyramid. `levels[0]` is the finest; the last has a single bucket. Empty (no
/// levels) for a file of no frames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Waveform {
    pub sample_rate: u32,
    pub frames: u64,
    pub channels: u16,
    pub levels: Vec<Level>,
}

impl Waveform {
    /// Bytes of peak data held (4 per bucket per channel, all levels).
    pub fn bytes(&self) -> usize {
        self.levels
            .iter()
            .map(|l| l.channels.iter().map(Vec::len).sum::<usize>() * std::mem::size_of::<Peak>())
            .sum()
    }

    /// Min/max of `channel` over file frames `start..end`, in `buckets` equal parts, from the
    /// coarsest level that still has at least one source bucket per output bucket (the finest
    /// level if none does, so output buckets then repeat source buckets). `end` is clamped to the
    /// file. Returns fewer buckets than asked when the range has fewer frames than buckets or
    /// `buckets` is over [`MAX_RANGE_BUCKETS`]; empty for an unknown channel, `buckets == 0`, or
    /// an empty or inverted range. A bucket covers every source bucket it overlaps, so edges
    /// round outwards by up to one source bucket.
    pub fn range(&self, channel: usize, start: u64, end: u64, buckets: usize) -> Vec<Peak> {
        let end = end.min(self.frames);
        if buckets == 0 || start >= end {
            return Vec::new();
        }
        let span = end - start;
        let n = u64::try_from(buckets.min(MAX_RANGE_BUCKETS))
            .unwrap_or(u64::MAX)
            .min(span);
        let per_out = span / n;
        // Levels get coarser, so the last one that fits is the coarsest that fits.
        let level = self
            .levels
            .iter()
            .rev()
            .find(|l| l.frames_per_bucket <= per_out)
            .or_else(|| self.levels.first());
        let Some(peaks) = level.and_then(|l| l.channels.get(channel).map(|p| (l, p))) else {
            return Vec::new();
        };
        let (level, peaks) = peaks;
        let fpb = level.frames_per_bucket.max(1);
        let mut out = Vec::with_capacity(usize::try_from(n).unwrap_or(0));
        for j in 0..n {
            // u128: span · j can pass u64 for absurd inputs; spans here are bounded by frames.
            let a = start + (u128::from(span) * u128::from(j) / u128::from(n)) as u64;
            let b = start + (u128::from(span) * u128::from(j + 1) / u128::from(n)) as u64;
            let first = usize::try_from(a / fpb).unwrap_or(usize::MAX);
            let last = usize::try_from(b.div_ceil(fpb)).unwrap_or(usize::MAX);
            let last = last.max(first.saturating_add(1)).min(peaks.len());
            let mut acc: Option<Peak> = None;
            for p in peaks.get(first..last).unwrap_or_default() {
                acc = Some(acc.map_or(*p, |x| x.merge(*p)));
            }
            out.push(acc.unwrap_or_default());
        }
        out
    }
}

/// Why no waveform came out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WaveformError {
    /// The file cannot be decoded (or has no frames).
    Media(MediaError),
    /// The cancel flag was set.
    Cancelled,
    /// Zero channels, or memory for the peaks could not be had.
    TooLarge,
}

impl fmt::Display for WaveformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WaveformError::Media(e) => e.fmt(f),
            WaveformError::Cancelled => f.write_str("waveform cancelled"),
            WaveformError::TooLarge => f.write_str("waveform too large"),
        }
    }
}

impl std::error::Error for WaveformError {}

impl From<MediaError> for WaveformError {
    fn from(e: MediaError) -> Self {
        WaveformError::Media(e)
    }
}

/// Builds a [`Waveform`] from interleaved frames fed in chunks of any size. Pure: no I/O.
/// Memory is reserved up front from the expected length and never grows past it: frames beyond
/// it are ignored.
pub struct WaveformBuilder {
    sample_rate: u32,
    channels: usize,
    capacity_frames: u64,
    frames: u64,
    level0: Vec<Vec<Peak>>,
    /// Running min/max of the open bucket, per channel (unused while `filled == 0`).
    open: Vec<Peak>,
    filled: u64,
}

impl WaveformBuilder {
    /// `expected_frames` (from the probe) bounds the frames kept and sizes the allocation.
    pub fn new(
        sample_rate: u32,
        channels: u16,
        expected_frames: u64,
    ) -> Result<Self, WaveformError> {
        let ch = usize::from(channels);
        if ch == 0 {
            return Err(WaveformError::TooLarge);
        }
        let buckets = usize::try_from(expected_frames.div_ceil(BASE_FRAMES))
            .map_err(|_| WaveformError::TooLarge)?;
        let mut level0 = Vec::new();
        level0
            .try_reserve_exact(ch)
            .map_err(|_| WaveformError::TooLarge)?;
        for _ in 0..ch {
            let mut v: Vec<Peak> = Vec::new();
            v.try_reserve_exact(buckets)
                .map_err(|_| WaveformError::TooLarge)?;
            level0.push(v);
        }
        let mut open = Vec::new();
        open.try_reserve_exact(ch)
            .map_err(|_| WaveformError::TooLarge)?;
        open.resize(ch, Peak::default());
        Ok(WaveformBuilder {
            sample_rate,
            channels: ch,
            capacity_frames: expected_frames,
            frames: 0,
            level0,
            open,
            filled: 0,
        })
    }

    /// Frames accepted so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Frames still accepted before the expected length is reached.
    pub fn remaining(&self) -> u64 {
        self.capacity_frames - self.frames
    }

    /// Adds interleaved samples (a trailing partial frame is ignored). Returns the frames taken,
    /// fewer than offered once the expected length is reached.
    pub fn push(&mut self, interleaved: &[f32]) -> u64 {
        let mut taken = 0;
        for frame in interleaved.chunks_exact(self.channels) {
            if self.frames >= self.capacity_frames {
                break;
            }
            for (acc, &x) in self.open.iter_mut().zip(frame) {
                let q = quantise(x);
                if self.filled == 0 {
                    *acc = Peak { min: q, max: q };
                } else {
                    acc.min = acc.min.min(q);
                    acc.max = acc.max.max(q);
                }
            }
            self.filled += 1;
            self.frames += 1;
            taken += 1;
            if self.filled == BASE_FRAMES {
                self.close_bucket();
            }
        }
        taken
    }

    fn close_bucket(&mut self) {
        for (dst, acc) in self.level0.iter_mut().zip(&self.open) {
            // Capacity was reserved for every bucket, so this never reallocates.
            dst.push(*acc);
        }
        self.filled = 0;
    }

    /// Closes the last partial bucket and derives the coarser levels. The waveform covers the
    /// frames actually received.
    pub fn finish(mut self) -> Result<Waveform, WaveformError> {
        if self.filled > 0 {
            self.close_bucket();
        }
        let mut levels = Vec::new();
        if self.frames > 0 {
            let mut cur = Level {
                frames_per_bucket: BASE_FRAMES,
                channels: std::mem::take(&mut self.level0),
            };
            loop {
                let done = cur.buckets() <= 1;
                let next = if done { None } else { Some(halve(&cur)?) };
                levels.push(cur);
                match next {
                    Some(n) => cur = n,
                    None => break,
                }
            }
        }
        Ok(Waveform {
            sample_rate: self.sample_rate,
            frames: self.frames,
            channels: u16::try_from(self.channels).unwrap_or(u16::MAX),
            levels,
        })
    }
}

/// The next coarser level: neighbouring buckets merged (an odd last bucket carries over).
fn halve(l: &Level) -> Result<Level, WaveformError> {
    let mut channels = Vec::new();
    channels
        .try_reserve_exact(l.channels.len())
        .map_err(|_| WaveformError::TooLarge)?;
    for src in &l.channels {
        let mut v: Vec<Peak> = Vec::new();
        v.try_reserve_exact(src.len().div_ceil(2))
            .map_err(|_| WaveformError::TooLarge)?;
        v.extend(
            src.chunks(2)
                .map(|pair| pair.iter().copied().reduce(Peak::merge).unwrap_or_default()),
        );
        channels.push(v);
    }
    Ok(Level {
        frames_per_bucket: l.frames_per_bucket.saturating_mul(2),
        channels,
    })
}

/// A finished waveform and the damage the decoder met while reading (as [`crate::probe`] reports
/// it; damaged media plays, so it still has a waveform).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaveformResult {
    pub waveform: Waveform,
    pub warnings: Vec<MediaWarning>,
}

/// Decodes `media` once with the regular decoder and builds its waveform. `info` is the file's
/// probe ([`crate::Probed::info`]); it sizes the allocation. Blocking: call from a worker
/// thread. `cancel` is checked before every ~4096-frame read; setting it gives
/// [`WaveformError::Cancelled`].
///
/// A file longer than probed is read only up to `info.frames` (as a stream does). A shorter one
/// gives a waveform of the frames it has and a `Truncated` warning. Channels or rate differing
/// from `info` is [`MediaError::Changed`].
pub fn build_waveform(
    media: Media,
    info: &MediaInfo,
    cancel: &AtomicBool,
) -> Result<WaveformResult, WaveformError> {
    // A panic inside symphonia (malformed data) is a corrupt file, not a dead worker.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build(media, info, cancel)))
        .unwrap_or_else(|_| Err(MediaError::Corrupt("decoder failed".into()).into()))
}

fn build(
    media: Media,
    info: &MediaInfo,
    cancel: &AtomicBool,
) -> Result<WaveformResult, WaveformError> {
    let mut src = open_source(media, Seeking::Direct)?;
    if src.channels() != usize::from(info.channels) || src.sample_rate() != info.sample_rate {
        return Err(MediaError::Changed.into());
    }
    let ch = src.channels();
    let mut builder = WaveformBuilder::new(info.sample_rate, info.channels, info.frames)?;
    let mut buf = vec![0.0f32; CHUNK_FRAMES * ch];
    while builder.remaining() > 0 {
        if cancel.load(Ordering::Relaxed) {
            return Err(WaveformError::Cancelled);
        }
        let want = usize::try_from(builder.remaining())
            .unwrap_or(CHUNK_FRAMES)
            .min(CHUNK_FRAMES);
        let n = src.read(buf.get_mut(..want * ch).unwrap_or_default())?;
        if n == 0 {
            break;
        }
        builder.push(buf.get(..n * ch).unwrap_or_default());
    }
    let mut warnings = src.take_warnings();
    let got = builder.frames();
    if got == 0 {
        return Err(MediaError::Corrupt("no audio frames".into()).into());
    }
    if got < info.frames {
        warnings.push(MediaWarning {
            kind: WarningKind::Truncated,
            at: got,
            frames: info.frames - got,
        });
    }
    Ok(WaveformResult {
        waveform: builder.finish()?,
        warnings,
    })
}
