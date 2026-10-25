// SPDX-License-Identifier: GPL-3.0-or-later

//! Media probing and decoding with symphonia (AUD-04). Runs on disk or I/O threads only, never on
//! the audio thread. Every failure is a typed [`MediaError`]; nothing here panics on bad input.
//!
//! Frame positions are exact: a [`FrameSource`] seeks to any frame and then yields exactly the
//! frames a decode from the start would have yielded at that position. Lossy formats (MP3, AAC,
//! Vorbis) are counted by decoding during [`probe`], because their container lengths can be wrong.

use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use symphonia::core::codecs::audio::well_known::{
    CODEC_ID_AAC, CODEC_ID_MP1, CODEC_ID_MP2, CODEC_ID_MP3, CODEC_ID_OPUS, CODEC_ID_VORBIS,
};
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{TimeBase, Timestamp};

use crate::mixer::{MAX_SAMPLE_RATE, MAX_VOICE_CHANNELS};

/// Frames decoded and thrown away before a seek target, so decoders with inter-frame state (MP3
/// bit reservoir, MDCT overlap) are warm when the wanted frame comes out.
const SEEK_PREROLL: u64 = 4096;

/// What [`probe`] learns about a file. `frames` is exact (counted by decoding where the container
/// cannot be trusted).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaInfo {
    pub channels: u16,
    pub sample_rate: u32,
    /// Exact number of frames at `sample_rate`.
    pub frames: u64,
    pub duration: Duration,
}

impl MediaInfo {
    fn new(channels: u16, sample_rate: u32, frames: u64) -> Self {
        let duration = Duration::from_secs_f64(frames as f64 / f64::from(sample_rate));
        MediaInfo {
            channels,
            sample_rate,
            frames,
            duration,
        }
    }

    /// Exact length in frames once resampled to `rate`: `round(frames · rate / sample_rate)`.
    /// Disk streams produce exactly this many frames, so trims and slices can rely on it.
    pub fn frames_at(&self, rate: u32) -> u64 {
        scale_round(self.frames, rate, self.sample_rate)
    }
}

/// `round(x · num / den)`, saturating; `den == 0` gives 0.
pub(crate) fn scale_round(x: u64, num: u32, den: u32) -> u64 {
    if den == 0 {
        return 0;
    }
    let (num, den) = (u128::from(num), u128::from(den));
    let v = (u128::from(x) * num + den / 2) / den;
    u64::try_from(v).unwrap_or(u64::MAX)
}

/// Why a file cannot be played. Shown to the operator as a broken cue; never a panic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaError {
    NotFound(PathBuf),
    Io(String),
    /// Unknown container or codec, or a parameter outside what openLab plays (channels 1–64,
    /// sample rate 1–384000).
    Unsupported(String),
    /// Malformed data.
    Corrupt(String),
    /// The file ends before the length its header declares.
    Truncated {
        expected: u64,
        actual: u64,
    },
    /// The file no longer matches what was probed (channels, rate or length).
    Changed,
    /// A stream's slice table is empty, malformed, or reaches past the end of the file.
    BadSlices,
    /// The disk streamer's limit on open streams is reached.
    TooManyStreams,
    /// A worker thread could not be started or has stopped.
    Worker,
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MediaError::NotFound(p) => write!(f, "file not found: {}", p.display()),
            MediaError::Io(e) => write!(f, "read error: {e}"),
            MediaError::Unsupported(e) => write!(f, "unsupported media: {e}"),
            MediaError::Corrupt(e) => write!(f, "corrupt media: {e}"),
            MediaError::Truncated { expected, actual } => {
                write!(f, "file truncated: {actual} of {expected} frames present")
            }
            MediaError::Changed => f.write_str("file changed since it was probed"),
            MediaError::BadSlices => f.write_str("slice table does not fit the file"),
            MediaError::TooManyStreams => f.write_str("too many open streams"),
            MediaError::Worker => f.write_str("disk worker unavailable"),
        }
    }
}

impl std::error::Error for MediaError {}

impl From<SymError> for MediaError {
    fn from(e: SymError) -> Self {
        match e {
            SymError::IoError(e) => MediaError::Io(e.to_string()),
            SymError::DecodeError(m) => MediaError::Corrupt(m.to_string()),
            SymError::SeekError(_) => MediaError::Corrupt(e.to_string()),
            SymError::Unsupported(m) => MediaError::Unsupported(m.to_string()),
            SymError::LimitError(m) => MediaError::Corrupt(m.to_string()),
            SymError::ResetRequired => {
                MediaError::Unsupported("stream parameters change mid-file".into())
            }
            _ => MediaError::Corrupt(e.to_string()),
        }
    }
}

/// Any seekable byte source (an in-memory buffer, a test reader that throttles reads, …).
pub trait MediaRead: Read + Seek + Send + Sync {}
impl<T: Read + Seek + Send + Sync> MediaRead for T {}

/// Where audio comes from.
pub enum Media {
    Path(PathBuf),
    /// A one-shot reader, with an optional file-extension hint ("wav", "mp3", …).
    Reader(Box<dyn MediaRead>, Option<String>),
    /// Already decoded into memory with [`preload`] (AUD-05).
    Preloaded(Arc<Preloaded>),
}

impl fmt::Debug for Media {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Media::Path(p) => f.debug_tuple("Path").field(p).finish(),
            Media::Reader(_, hint) => f.debug_tuple("Reader").field(hint).finish(),
            Media::Preloaded(p) => f.debug_tuple("Preloaded").field(&p.info).finish(),
        }
    }
}

/// A whole file decoded once at its own rate (AUD-05). Shared by every stream that plays it.
#[derive(Debug)]
pub struct Preloaded {
    info: MediaInfo,
    /// Interleaved, `info.frames × info.channels`.
    samples: Arc<[f32]>,
}

impl Preloaded {
    #[cfg(test)]
    pub(crate) fn from_samples(channels: u16, rate: u32, samples: Vec<f32>) -> Arc<Self> {
        let frames = (samples.len() / usize::from(channels)) as u64;
        Arc::new(Preloaded {
            info: MediaInfo::new(channels, rate, frames),
            samples: samples.into(),
        })
    }

    pub fn info(&self) -> MediaInfo {
        self.info
    }

    /// Interleaved samples at the file's own rate.
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// Memory held by the decoded samples.
    pub fn bytes(&self) -> usize {
        std::mem::size_of_val(&*self.samples)
    }
}

/// Probes `path` and returns its exact format and length. Blocking: call from an I/O thread.
pub fn probe(path: &Path) -> Result<MediaInfo, MediaError> {
    probe_media(Media::Path(path.to_path_buf()))
}

/// As [`probe`], for any [`Media`].
pub fn probe_media(media: Media) -> Result<MediaInfo, MediaError> {
    match media {
        Media::Preloaded(p) => Ok(p.info),
        other => {
            let mut d = Decoder::open(other)?;
            d.exact_info()
        }
    }
}

/// Decodes the whole of `media` into memory. Blocking: call from an I/O thread.
pub fn preload(media: Media) -> Result<Arc<Preloaded>, MediaError> {
    if let Media::Preloaded(p) = media {
        return Ok(p);
    }
    let mut d = Decoder::open(media)?;
    let ch = d.channels;
    let mut samples = Vec::new();
    let mut chunk = vec![0.0; 4096 * ch];
    loop {
        let n = d.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        samples.extend_from_slice(chunk.get(..n * ch).unwrap_or_default());
    }
    let frames = (samples.len() / ch) as u64;
    if let Some(expected) = d.declared.filter(|&e| e > frames && !d.lossy) {
        return Err(MediaError::Truncated {
            expected,
            actual: frames,
        });
    }
    let channels = u16::try_from(ch).unwrap_or(0);
    Ok(Arc::new(Preloaded {
        info: MediaInfo::new(channels, d.rate, frames),
        samples: samples.into(),
    }))
}

/// File-rate interleaved frames with exact seeking. Implemented by the symphonia decoder and by
/// preloaded memory, so streaming and preload run the same pipeline.
pub(crate) trait FrameSource: Send {
    fn channels(&self) -> usize;
    fn sample_rate(&self) -> u32;
    /// Positions the source so the next frame read is `frame`.
    fn seek(&mut self, frame: u64) -> Result<(), MediaError>;
    /// Reads up to `out.len() / channels` frames; `Ok(0)` is end of file.
    fn read(&mut self, out: &mut [f32]) -> Result<usize, MediaError>;
}

/// Opens `media` as a frame source. Blocking.
pub(crate) fn open_source(media: Media) -> Result<Box<dyn FrameSource>, MediaError> {
    match media {
        Media::Preloaded(p) => Ok(Box::new(MemorySource { data: p, pos: 0 })),
        other => Ok(Box::new(Decoder::open(other)?)),
    }
}

struct MemorySource {
    data: Arc<Preloaded>,
    pos: u64,
}

impl FrameSource for MemorySource {
    fn channels(&self) -> usize {
        usize::from(self.data.info.channels)
    }

    fn sample_rate(&self) -> u32 {
        self.data.info.sample_rate
    }

    fn seek(&mut self, frame: u64) -> Result<(), MediaError> {
        self.pos = frame.min(self.data.info.frames);
        Ok(())
    }

    fn read(&mut self, out: &mut [f32]) -> Result<usize, MediaError> {
        let ch = self.channels();
        let left = self.data.info.frames - self.pos;
        let n = (out.len() / ch).min(usize::try_from(left).unwrap_or(usize::MAX));
        let start = usize::try_from(self.pos).unwrap_or(usize::MAX).saturating_mul(ch);
        let src = self
            .data
            .samples
            .get(start..start + n * ch)
            .ok_or(MediaError::Changed)?;
        if let Some(dst) = out.get_mut(..n * ch) {
            dst.copy_from_slice(src);
        }
        self.pos += n as u64;
        Ok(n)
    }
}

/// Adapts a [`MediaRead`] to symphonia's source trait.
struct ReaderSource {
    inner: Box<dyn MediaRead>,
    len: Option<u64>,
}

impl Read for ReaderSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Seek for ReaderSource {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

impl MediaSource for ReaderSource {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        self.len
    }
}

struct Decoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track: u32,
    time_base: Option<TimeBase>,
    start_ts: i64,
    rate: u32,
    channels: usize,
    /// Length from the container; trusted only for lossless codecs.
    declared: Option<u64>,
    lossy: bool,
    /// Decoded interleaved samples not yet returned; `buf[buf_pos..]` starts at frame `next`.
    buf: Vec<f32>,
    buf_pos: usize,
    /// File frame of the next sample returned.
    next: u64,
    /// After a seek: the position of the next decoded packet comes from its timestamp.
    resync: bool,
    eof: bool,
}

impl Decoder {
    fn open(media: Media) -> Result<Self, MediaError> {
        let mut hint = Hint::new();
        let source: Box<dyn MediaSource> = match media {
            Media::Path(path) => {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    hint.with_extension(ext);
                }
                match File::open(&path) {
                    Ok(f) => Box::new(f),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        return Err(MediaError::NotFound(path));
                    }
                    Err(e) => return Err(MediaError::Io(e.to_string())),
                }
            }
            Media::Reader(mut inner, ext) => {
                if let Some(ext) = &ext {
                    hint.with_extension(ext);
                }
                let len = inner.seek(SeekFrom::End(0)).ok();
                inner
                    .seek(SeekFrom::Start(0))
                    .map_err(|e| MediaError::Io(e.to_string()))?;
                Box::new(ReaderSource { inner, len })
            }
            Media::Preloaded(_) => {
                return Err(MediaError::Unsupported("preloaded media has no decoder".into()));
            }
        };
        let mss = MediaSourceStream::new(source, Default::default());
        let format = symphonia::default::get_probe().probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )?;
        let track = format
            .default_track(TrackType::Audio)
            .ok_or_else(|| MediaError::Unsupported("no audio track".into()))?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or_else(|| MediaError::Unsupported("unknown codec".into()))?;
        let rate = params
            .sample_rate
            .filter(|r| (1..=MAX_SAMPLE_RATE).contains(r))
            .ok_or_else(|| MediaError::Unsupported("sample rate".into()))?;
        let channels = params
            .channels
            .as_ref()
            .map(|c| c.count())
            .filter(|c| (1..=MAX_VOICE_CHANNELS).contains(c))
            .ok_or_else(|| MediaError::Unsupported("channel count".into()))?;
        let lossy = [
            CODEC_ID_MP1,
            CODEC_ID_MP2,
            CODEC_ID_MP3,
            CODEC_ID_AAC,
            CODEC_ID_VORBIS,
            CODEC_ID_OPUS,
        ]
        .contains(&params.codec);
        let decoder =
            symphonia::default::get_codecs().make_audio_decoder(params, &AudioDecoderOptions::default())?;
        let (track_id, time_base, start_ts, declared) =
            (track.id, track.time_base, track.start_ts.get(), track.num_frames);
        Ok(Decoder {
            format,
            decoder,
            track: track_id,
            time_base,
            start_ts,
            rate,
            channels,
            declared,
            lossy,
            buf: Vec::new(),
            buf_pos: 0,
            next: 0,
            resync: false,
            eof: false,
        })
    }

    /// Exact length: trusted lossless header lengths are checked by decoding the tail; anything
    /// else is counted by decoding the whole file.
    fn exact_info(&mut self) -> Result<MediaInfo, MediaError> {
        let channels = u16::try_from(self.channels).unwrap_or(0);
        let frames = match self.declared.filter(|_| !self.lossy) {
            Some(declared) => {
                let tail = self
                    .seek(declared.saturating_sub(1))
                    .and_then(|()| self.count_to_end());
                match tail {
                    Ok(end) if end >= declared => declared,
                    // The tail is missing or unreadable: count what is really there.
                    _ => {
                        self.seek(0)?;
                        let actual = self.count_to_end()?;
                        if actual >= declared {
                            return Err(MediaError::Corrupt("unreadable end of file".into()));
                        }
                        return Err(MediaError::Truncated {
                            expected: declared,
                            actual,
                        });
                    }
                }
            }
            None => self.count_to_end()?,
        };
        if frames == 0 {
            return Err(MediaError::Corrupt("no audio frames".into()));
        }
        Ok(MediaInfo::new(channels, self.rate, frames))
    }

    /// Decodes to the end and returns the frame position reached.
    fn count_to_end(&mut self) -> Result<u64, MediaError> {
        loop {
            self.next += ((self.buf.len() - self.buf_pos) / self.channels) as u64;
            self.buf_pos = self.buf.len();
            if !self.refill()? {
                return Ok(self.next);
            }
        }
    }

    fn ts_to_frames(&self, ts: i64) -> i64 {
        let rel = i128::from(ts) - i128::from(self.start_ts);
        let v = match self.time_base {
            Some(tb) => {
                rel * i128::from(tb.numer.get()) * i128::from(self.rate)
                    / i128::from(tb.denom.get())
            }
            None => rel,
        };
        i64::try_from(v).unwrap_or(if v < 0 { i64::MIN } else { i64::MAX })
    }

    fn frames_to_ts(&self, frames: u64) -> i64 {
        let f = i128::from(frames);
        let v = match self.time_base {
            Some(tb) => {
                f * i128::from(tb.denom.get())
                    / (i128::from(tb.numer.get()) * i128::from(self.rate)).max(1)
            }
            None => f,
        } + i128::from(self.start_ts);
        i64::try_from(v).unwrap_or(i64::MAX)
    }

    /// Decodes the next packet of our track into `buf`, aligned so `buf[buf_pos]` is frame
    /// `next`. Returns false at end of file.
    fn refill(&mut self) -> Result<bool, MediaError> {
        self.buf.clear();
        self.buf_pos = 0;
        while !self.eof {
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => {
                    self.eof = true;
                    break;
                }
                // A container cut short ends the stream; probe reports truncation separately.
                Err(SymError::IoError(e)) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    self.eof = true;
                    break;
                }
                Err(e) => return Err(e.into()),
            };
            if packet.track_id != self.track {
                continue;
            }
            let decoded = self.decoder.decode(&packet)?;
            if decoded.spec().channels().count() != self.channels {
                return Err(MediaError::Unsupported("channel count changes mid-file".into()));
            }
            let frames = decoded.frames();
            decoded.copy_to_vec_interleaved(&mut self.buf);
            // A decoder that applied the packet's gapless trim returns only the playable frames.
            let first_ts = if frames as u64 == packet.dur.get() {
                packet.pts.get().saturating_add_unsigned(packet.trim_start.get())
            } else {
                packet.pts.get()
            };
            if self.resync {
                let first = self.ts_to_frames(first_ts);
                let want = i64::try_from(self.next).unwrap_or(i64::MAX);
                if first < want {
                    // Drop frames before the seek target.
                    let drop = usize::try_from(want - first).unwrap_or(usize::MAX);
                    if drop >= frames {
                        self.buf.clear();
                        continue;
                    }
                    self.buf_pos = drop * self.channels;
                } else if first > want {
                    // The reader landed after the target: the gap is silence, keeping time.
                    let gap = usize::try_from(first - want).unwrap_or(0).min(1 << 20);
                    self.buf
                        .splice(0..0, std::iter::repeat_n(0.0, gap * self.channels));
                }
                self.resync = false;
            }
            if self.buf.len() > self.buf_pos {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl FrameSource for Decoder {
    fn channels(&self) -> usize {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn seek(&mut self, frame: u64) -> Result<(), MediaError> {
        let from = frame.saturating_sub(SEEK_PREROLL);
        let to = SeekTo::Timestamp {
            ts: Timestamp::new(self.frames_to_ts(from)),
            track_id: self.track,
        };
        self.buf.clear();
        self.buf_pos = 0;
        self.eof = false;
        self.next = frame;
        match self.format.seek(SeekMode::Accurate, to) {
            Ok(_) => {}
            // Seeking past the end leaves nothing to read.
            Err(SymError::SeekError(_)) if self.declared.is_some_and(|d| from >= d) => {
                self.eof = true;
            }
            Err(e) => return Err(e.into()),
        }
        self.decoder.reset();
        self.resync = true;
        Ok(())
    }

    fn read(&mut self, out: &mut [f32]) -> Result<usize, MediaError> {
        let ch = self.channels;
        let want = out.len() / ch;
        let mut got = 0;
        while got < want {
            if self.buf_pos >= self.buf.len() && !self.refill()? {
                break;
            }
            let avail = (self.buf.len() - self.buf_pos) / ch;
            let n = avail.min(want - got);
            let src = self.buf.get(self.buf_pos..self.buf_pos + n * ch);
            let dst = out.get_mut(got * ch..(got + n) * ch);
            if let (Some(src), Some(dst)) = (src, dst) {
                dst.copy_from_slice(src);
            }
            self.buf_pos += n * ch;
            self.next += n as u64;
            got += n;
        }
        Ok(got)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_round_is_exact_and_saturates() {
        assert_eq!(scale_round(44_100, 48_000, 44_100), 48_000);
        assert_eq!(scale_round(1, 48_000, 44_100), 1);
        assert_eq!(scale_round(u64::MAX, 384_000, 1), u64::MAX);
        assert_eq!(scale_round(10, 1, 0), 0);
    }
}
