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
    CODEC_ID_AAC, CODEC_ID_MP1, CODEC_ID_MP2, CODEC_ID_MP3, CODEC_ID_OPUS, CODEC_ID_PCM_MULAW,
    CODEC_ID_PCM_S32LE, CODEC_ID_VORBIS,
};
use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{TimeBase, Timestamp};

use crate::mixer::{MAX_SAMPLE_RATE, MAX_VOICE_CHANNELS};

/// Frames decoded and thrown away before a seek target, so decoders with inter-frame state (MP3
/// bit reservoir, MDCT overlap) are warm when the wanted frame comes out. Covers the packet a
/// lossy seek drops whole (a Vorbis packet is at most 4096 frames) plus warm-up.
const SEEK_PREROLL: u64 = 8192;
/// Seek attempts for one target: a seek that lands after its target is retried with twice the
/// pre-roll, and the last attempt decodes forward from the start.
const SEEK_TRIES: u32 = 4;

/// Largest sample magnitude passed on (+24 dBFS, the mixer's gain ceiling). Float files can hold
/// NaN, infinities or absurd values; any of them would poison a mix bus (NaN + x = NaN) or break
/// the resampler's FFT. Non-finite samples become silence, the rest are clamped.
pub(crate) const SAMPLE_LIMIT: f32 = 16.0;

fn sanitize_sample(x: f32) -> f32 {
    if x.is_finite() {
        x.clamp(-SAMPLE_LIMIT, SAMPLE_LIMIT)
    } else {
        0.0
    }
}

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

/// Runs third-party decoding code so that a panic in it (a decoder bug hit by malformed input)
/// is a [`MediaError::Corrupt`] for this file instead of taking the calling thread down.
pub(crate) fn contain<T>(f: impl FnOnce() -> Result<T, MediaError>) -> Result<T, MediaError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(MediaError::Corrupt("decoder failed".into())))
}

/// Probes `path` and returns its exact format and length. Blocking: call from an I/O thread.
pub fn probe(path: &Path) -> Result<MediaInfo, MediaError> {
    probe_media(Media::Path(path.to_path_buf()))
}

/// As [`probe`], for any [`Media`].
pub fn probe_media(media: Media) -> Result<MediaInfo, MediaError> {
    match media {
        Media::Preloaded(p) => Ok(p.info),
        other => contain(|| Decoder::open(other)?.exact_info()),
    }
}

/// Decodes the whole of `media` into memory. Blocking: call from an I/O thread.
pub fn preload(media: Media) -> Result<Arc<Preloaded>, MediaError> {
    if let Media::Preloaded(p) = media {
        return Ok(p);
    }
    contain(|| decode_all(media))
}

fn decode_all(media: Media) -> Result<Arc<Preloaded>, MediaError> {
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
        let start = usize::try_from(self.pos)
            .unwrap_or(usize::MAX)
            .saturating_mul(ch);
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

/// Chunks looked at before giving up on finding a WAV `fmt ` / AIFF `COMM` chunk (symphonia then
/// decides what the file is).
const HEADER_CHUNKS: usize = 64;

/// Rejects WAV and AIFF headers whose channel count or sample size is outside what openLab plays,
/// before symphonia sees them: symphonia 0.6.1 computes `(bits / 8) * channels` in `u16`, which
/// overflows on such headers (a panic with overflow checks on). Other data is left to symphonia.
/// Leaves `r` at the start.
fn check_pcm_header<R: Read + Seek + ?Sized>(r: &mut R) -> Result<(), MediaError> {
    let found = find_pcm_format(r);
    r.seek(SeekFrom::Start(0))
        .map_err(|e| MediaError::Io(e.to_string()))?;
    match found {
        Some((channels, bits))
            if !(1..=MAX_VOICE_CHANNELS).contains(&usize::from(channels)) || bits > 64 =>
        {
            Err(MediaError::Corrupt(format!(
                "header: {channels} channels of {bits} bits"
            )))
        }
        _ => Ok(()),
    }
}

/// Channel count and bits per sample from a RIFF/WAVE `fmt ` or AIFF/AIFC `COMM` chunk, if the
/// stream starts with one of those containers. Read errors count as "not found".
fn find_pcm_format<R: Read + Seek + ?Sized>(r: &mut R) -> Option<(u16, u16)> {
    let mut head = [0u8; 12];
    r.read_exact(&mut head).ok()?;
    let (big_endian, want): (bool, &[u8; 4]) = match (head.get(0..4)?, head.get(8..12)?) {
        (b"RIFF", b"WAVE") => (false, b"fmt "),
        (b"FORM", b"AIFF" | b"AIFC") => (true, b"COMM"),
        _ => return None,
    };
    for _ in 0..HEADER_CHUNKS {
        let mut ch = [0u8; 8];
        r.read_exact(&mut ch).ok()?;
        let size = [ch[4], ch[5], ch[6], ch[7]];
        let size = if big_endian {
            u32::from_be_bytes(size)
        } else {
            u32::from_le_bytes(size)
        };
        if &ch[..4] == want {
            let mut body = [0u8; 16];
            let n = usize::try_from(size).unwrap_or(usize::MAX).min(16);
            r.read_exact(body.get_mut(..n)?).ok()?;
            let u16_at = |i: usize| {
                let b = [*body.get(i)?, *body.get(i + 1)?];
                Some(if big_endian {
                    u16::from_be_bytes(b)
                } else {
                    u16::from_le_bytes(b)
                })
            };
            // WAV fmt: tag, channels, rate(4), byte rate(4), block align, bits.
            // AIFF COMM: channels, frames(4), bits.
            return if big_endian {
                Some((u16_at(0)?, u16_at(6)?))
            } else {
                Some((u16_at(2)?, u16_at(14).unwrap_or(0)))
            };
        }
        // Chunks are padded to an even length.
        let skip = i64::from(size) + i64::from(size & 1);
        r.seek(SeekFrom::Current(skip)).ok()?;
    }
    None
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
    /// `None` only if reopening for a seek failed (the error was returned then).
    format: Option<Box<dyn FormatReader>>,
    hint: Hint,
    decoder: Box<dyn AudioDecoder>,
    /// To make a fresh decoder on every seek: symphonia's AAC `reset` leaves state behind.
    params: AudioCodecParameters,
    track: u32,
    time_base: Option<TimeBase>,
    rate: u32,
    channels: usize,
    /// Timestamp units per frame, when every frame starts on a whole timestamp. Positions then
    /// come from packet timestamps, so a seek lands exactly. `None`: positions are counted from
    /// the start, and a seek decodes forward from the start.
    ts_per_frame: Option<i64>,
    /// The track's first timestamp, where a seek to the start goes.
    start_ts: i64,
    /// Timestamp position, in frames, of the first playable frame (position 0).
    origin: i64,
    /// Length from the container; trusted only for PCM (see `exact_info`).
    declared: Option<u64>,
    pcm: bool,
    lossy: bool,
    /// Playable frames of the current packet, interleaved; `buf[buf_pos..]` starts at `next`.
    buf: Vec<f32>,
    buf_pos: usize,
    /// Position of the next frame `read` returns.
    next: u64,
    /// Position just after the last packet decoded: where the next packet must start.
    cursor: u64,
    /// Set by a seek until the packet holding the target frame is decoded.
    resync: Option<Resync>,
    eof: bool,
}

#[derive(Clone, Copy, Debug)]
struct Resync {
    /// A lossy decoder's first packet after a mid-file seek lacks its predecessor's state (MDCT
    /// overlap, MP3 bit reservoir): it is dropped whole.
    discard_first: bool,
    /// Seeks so far for this target; each retry doubles the pre-roll, the last starts at 0.
    tries: u32,
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
                    Ok(mut f) => {
                        check_pcm_header(&mut f)?;
                        Box::new(f)
                    }
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
                check_pcm_header(&mut inner)?;
                Box::new(ReaderSource { inner, len })
            }
            Media::Preloaded(_) => {
                return Err(MediaError::Unsupported(
                    "preloaded media has no decoder".into(),
                ));
            }
        };
        let mss = MediaSourceStream::new(source, Default::default());
        let format = probe_format(&hint, mss)?;
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
        let pcm = (CODEC_ID_PCM_S32LE..=CODEC_ID_PCM_MULAW).contains(&params.codec);
        let params = params.clone();
        let decoder = make_decoder(&params)?;
        let time_base = track.time_base;
        let ts_per_frame = match time_base {
            None => Some(1),
            Some(tb) => {
                let unit = u64::from(tb.numer.get()) * u64::from(rate);
                let denom = u64::from(tb.denom.get());
                (denom % unit == 0)
                    .then(|| i64::try_from(denom / unit).ok())
                    .flatten()
            }
        };
        let (track_id, start_ts, declared) = (track.id, track.start_ts.get(), track.num_frames);
        let mut d = Decoder {
            format: Some(format),
            hint,
            decoder,
            params,
            track: track_id,
            time_base,
            rate,
            channels,
            ts_per_frame,
            start_ts,
            origin: 0,
            declared,
            pcm,
            lossy,
            buf: Vec::new(),
            buf_pos: 0,
            next: 0,
            cursor: 0,
            resync: None,
            eof: false,
        };
        d.find_origin()?;
        Ok(d)
    }

    /// Decodes up to the first playable frame, which becomes position 0, and keeps its packet
    /// buffered.
    fn find_origin(&mut self) -> Result<(), MediaError> {
        while let Some(abs) = self.decode_packet()? {
            let kept = self.kept();
            if kept > 0 {
                if self.ts_per_frame.is_some() {
                    self.origin = abs;
                }
                self.cursor = kept as u64;
                return Ok(());
            }
        }
        Ok(())
    }

    /// Exact length: a PCM header's length is checked by decoding the tail; anything else
    /// (compressed formats, where frames can be missing without the container noticing) is
    /// counted by decoding the whole file, which also checks it has no gaps.
    fn exact_info(&mut self) -> Result<MediaInfo, MediaError> {
        let channels = u16::try_from(self.channels).unwrap_or(0);
        let frames = match self.declared.filter(|_| self.pcm) {
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
            None => {
                let actual = self.count_to_end()?;
                // A lossless container's length is exact: fewer frames means the file was cut.
                match self.declared.filter(|&d| !self.lossy && actual < d) {
                    Some(expected) => return Err(MediaError::Truncated { expected, actual }),
                    None => actual,
                }
            }
        };
        if frames == 0 {
            return Err(MediaError::Corrupt("no audio frames".into()));
        }
        Ok(MediaInfo::new(channels, self.rate, frames))
    }

    /// Decodes to the end and returns the frame position reached.
    fn count_to_end(&mut self) -> Result<u64, MediaError> {
        loop {
            self.next += self.kept() as u64;
            self.buf_pos = self.buf.len();
            if !self.refill()? {
                return Ok(self.next);
            }
        }
    }

    /// Frames in `buf` not yet returned.
    fn kept(&self) -> usize {
        self.buf.len().saturating_sub(self.buf_pos) / self.channels
    }

    /// Timestamp (or duration) `ts` in frames, rounded to the nearest frame.
    fn ts_to_frames(&self, ts: i64) -> i64 {
        let v = match self.time_base {
            Some(tb) => {
                let num = i128::from(ts) * i128::from(tb.numer.get()) * i128::from(self.rate);
                let den = i128::from(tb.denom.get());
                (2 * num + den).div_euclid(2 * den)
            }
            None => i128::from(ts),
        };
        i64::try_from(v).unwrap_or(if v < 0 { i64::MIN } else { i64::MAX })
    }

    /// Timestamp of position `frames`; only used when `ts_per_frame` is known.
    fn frames_to_ts(&self, frames: u64, unit: i64) -> i64 {
        let v = (i128::from(frames) + i128::from(self.origin)) * i128::from(unit);
        i64::try_from(v).unwrap_or(i64::MAX)
    }

    /// Decodes the next packet of our track into `buf`, with the packet's encoder delay and
    /// padding trimmed. Returns the position of its first playable frame (`buf[buf_pos]`), or
    /// `None` at the end of the stream.
    fn decode_packet(&mut self) -> Result<Option<i64>, MediaError> {
        self.buf.clear();
        self.buf_pos = 0;
        while !self.eof {
            let format = self.format.as_mut().ok_or(MediaError::Changed)?;
            let packet = match format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => {
                    self.eof = true;
                    break;
                }
                // A container cut short ends the stream; the caller compares the length.
                Err(SymError::IoError(e)) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    self.eof = true;
                    break;
                }
                Err(e) => return Err(e.into()),
            };
            if packet.track_id != self.track {
                continue;
            }
            let decoded = match self.decoder.decode(&packet) {
                Ok(d) => d,
                // The packet a lossy seek drops anyway may not decode without its predecessor.
                Err(SymError::DecodeError(m)) => match &mut self.resync {
                    Some(r) if r.discard_first => {
                        r.discard_first = false;
                        continue;
                    }
                    _ => return Err(MediaError::Corrupt(m.to_string())),
                },
                Err(e) => return Err(e.into()),
            };
            if decoded.spec().channels().count() != self.channels {
                return Err(MediaError::Unsupported(
                    "channel count changes mid-file".into(),
                ));
            }
            let n = decoded.frames();
            decoded.copy_to_vec_interleaved(&mut self.buf);
            for x in &mut self.buf {
                *x = sanitize_sample(*x);
            }
            let trim = |d: u64| -> usize {
                let f = self.ts_to_frames(i64::try_from(d).unwrap_or(i64::MAX));
                usize::try_from(f).unwrap_or(0)
            };
            let front = trim(packet.trim_start.get()).min(n);
            let back = trim(packet.trim_end.get()).min(n - front);
            self.buf.truncate((n - back) * self.channels);
            self.buf_pos = front * self.channels;
            let pos = match self.ts_per_frame {
                Some(_) => self
                    .ts_to_frames(packet.pts.get())
                    .saturating_add(front as i64)
                    .saturating_sub(self.origin),
                None => i64::try_from(self.cursor).unwrap_or(i64::MAX),
            };
            return Ok(Some(pos));
        }
        Ok(None)
    }

    /// Decodes the next packet that holds frame `next`, aligned so `buf[buf_pos]` is that
    /// frame. Returns false at the end of the stream.
    fn refill(&mut self) -> Result<bool, MediaError> {
        loop {
            let Some(pos) = self.decode_packet()? else {
                return Ok(false);
            };
            let kept = self.kept();
            let end = pos.saturating_add(kept as i64);
            if let Some(r) = self.resync {
                // Counted positions need every packet, even the ones dropped here.
                self.cursor = u64::try_from(end).unwrap_or(0);
                if r.discard_first {
                    self.resync = Some(Resync {
                        discard_first: false,
                        ..r
                    });
                    continue;
                }
                let want = i64::try_from(self.next).unwrap_or(i64::MAX);
                if kept == 0 || end <= want {
                    continue;
                }
                if pos > want {
                    // The container landed after the target: retry from further back.
                    if r.tries + 1 >= SEEK_TRIES {
                        return Err(MediaError::Corrupt("cannot seek to frame".into()));
                    }
                    self.seek_with(self.next, r.tries + 1)?;
                    continue;
                }
                let skip = usize::try_from(want - pos).unwrap_or(usize::MAX);
                self.buf_pos += skip * self.channels;
                self.resync = None;
                return Ok(true);
            }
            if kept == 0 {
                continue;
            }
            let cursor = i64::try_from(self.cursor).unwrap_or(i64::MAX);
            if pos != cursor {
                // Frames are missing (a damaged frame the demuxer skipped) or repeated.
                return Err(MediaError::Corrupt(format!(
                    "discontinuity at frame {cursor} (next packet at {pos})"
                )));
            }
            self.cursor = u64::try_from(end).unwrap_or(0);
            return Ok(true);
        }
    }

    /// Seeks the container so that decoding reaches `frame` after a pre-roll.
    fn seek_with(&mut self, frame: u64, tries: u32) -> Result<(), MediaError> {
        self.buf.clear();
        self.buf_pos = 0;
        self.eof = false;
        self.next = frame;
        self.cursor = 0;
        // Lossless decoders need no warm-up; any decoder gets more pre-roll on a retry.
        let preroll = if self.lossy || tries > 0 {
            SEEK_PREROLL << tries
        } else {
            0
        };
        let from = match self.ts_per_frame {
            Some(_) if tries + 1 < SEEK_TRIES => frame.saturating_sub(preroll),
            _ => 0,
        };
        self.reopen()?;
        let at_start = if from == 0 {
            true
        } else {
            let unit = self.ts_per_frame.unwrap_or(1);
            let to = SeekTo::Timestamp {
                ts: Timestamp::new(self.frames_to_ts(from, unit)),
                track_id: self.track,
            };
            let format = self.format.as_mut().ok_or(MediaError::Changed)?;
            match format.seek(SeekMode::Accurate, to) {
                Ok(s) => s.actual_ts.get() <= self.start_ts,
                // The container cannot seek there (its own length index can be shorter than
                // what decodes): decode forward from the start instead.
                Err(_) => {
                    self.reopen()?;
                    true
                }
            }
        };
        self.decoder = make_decoder(&self.params)?;
        self.resync = Some(Resync {
            discard_first: self.lossy && !at_start,
            tries,
        });
        Ok(())
    }

    /// Replaces the container reader with a fresh one at the start of the same byte stream.
    /// Every seek starts from a fresh reader: symphonia 0.6.1's FLAC reader keeps stale frame
    /// fragments when a seek lands exactly on a frame start (the first frame, or any SEEKTABLE
    /// point), and then returns frames from before the seek.
    fn reopen(&mut self) -> Result<(), MediaError> {
        let old = self.format.take().ok_or(MediaError::Changed)?;
        let mut mss = old.into_inner();
        mss.seek(SeekFrom::Start(0))
            .map_err(|e| MediaError::Io(e.to_string()))?;
        let format = probe_format(&self.hint, mss)?;
        let same = format
            .default_track(TrackType::Audio)
            .is_some_and(|t| t.id == self.track);
        if !same {
            return Err(MediaError::Changed);
        }
        self.format = Some(format);
        Ok(())
    }
}

fn make_decoder(params: &AudioCodecParameters) -> Result<Box<dyn AudioDecoder>, MediaError> {
    // Gapless trimming is applied in `decode_packet`, the same way for every codec: symphonia's
    // decoders differ in whether they apply it.
    let opts = AudioDecoderOptions::default().gapless(false);
    Ok(symphonia::default::get_codecs().make_audio_decoder(params, &opts)?)
}

fn probe_format(
    hint: &Hint,
    mss: MediaSourceStream<'static>,
) -> Result<Box<dyn FormatReader>, MediaError> {
    Ok(symphonia::default::get_probe().probe(
        hint,
        mss,
        FormatOptions::default(),
        MetadataOptions::default(),
    )?)
}

impl FrameSource for Decoder {
    fn channels(&self) -> usize {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn seek(&mut self, frame: u64) -> Result<(), MediaError> {
        self.seek_with(frame, 0)
    }

    fn read(&mut self, out: &mut [f32]) -> Result<usize, MediaError> {
        let ch = self.channels;
        let want = out.len() / ch;
        let mut got = 0;
        while got < want {
            if self.kept() == 0 && !self.refill()? {
                break;
            }
            let n = self.kept().min(want - got);
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

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    /// After any seek, every format yields exactly the frames a decode from the start has there.
    #[test]
    fn seeking_is_sample_accurate_for_every_format() {
        for name in [
            "tone.flac",
            "tone-alac.m4a",
            "tone.mp3",
            "tone.m4a",
            "tone.ogg",
        ] {
            let full = preload(Media::Path(fixture(name))).unwrap();
            let frames = full.info().frames;
            let all = full.samples();
            let mut src = open_source(Media::Path(fixture(name))).unwrap();
            let aac = name == "tone.m4a";
            let mut worst = 0.0f32;
            // Start, inside the pre-roll, packet edges (1024 AAC/Vorbis, 1152 MP3, 4096 FLAC),
            // backwards, near and at the end.
            for at in [
                1,
                0,
                47,
                500,
                1023,
                1024,
                1152,
                4095,
                4096,
                8191,
                8192,
                8193,
                12_000,
                7_000,
                3,
                20_000,
                frames - 300,
                frames - 1,
                frames,
            ] {
                src.seek(at).unwrap();
                let mut buf = vec![0.0; 600];
                let mut got = 0;
                while got < 300 {
                    let n = src.read(&mut buf[got * 2..]).unwrap();
                    if n == 0 {
                        break;
                    }
                    got += n;
                }
                let want = (frames - at).min(300) as usize;
                assert_eq!(got, want, "{name} at {at}: frames");
                let start = at as usize * 2;
                let err = buf[..got * 2]
                    .iter()
                    .zip(&all[start..start + got * 2])
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0, f32::max);
                // Bit-exact, except AAC after a mid-file seek: ffmpeg's AAC encoder uses noise
                // substitution, which the decoder fills from a pseudo-random sequence that depends
                // on everything decoded before.
                if !aac || at < SEEK_PREROLL {
                    assert_eq!(err, 0.0, "{name} at {at}");
                }
                worst = worst.max(err);
            }
            // Substituted noise is tiny; a one-frame shift of this tone would be over 0.03.
            assert!(worst < 2.5e-3, "{name}: max error {worst}");
        }
    }

    /// Without frame-exact timestamps, positions are counted and a seek decodes forward from the
    /// start: slower, equally exact.
    #[test]
    fn counted_positions_seek_exactly() {
        for name in ["tone.flac", "tone.mp3", "tone.ogg"] {
            let full = preload(Media::Path(fixture(name))).unwrap();
            let all = full.samples();
            let mut d = Decoder::open(Media::Path(fixture(name))).unwrap();
            d.ts_per_frame = None;
            for at in [9_000u64, 0, 4_097, 21_000] {
                d.seek(at).unwrap();
                let mut buf = vec![0.0; 2 * 500];
                assert_eq!(d.read(&mut buf).unwrap(), 500, "{name} at {at}");
                let start = at as usize * 2;
                assert_eq!(&buf[..], &all[start..start + 1000], "{name} at {at}");
            }
        }
    }

    #[test]
    fn scale_round_is_exact_and_saturates() {
        assert_eq!(scale_round(44_100, 48_000, 44_100), 48_000);
        assert_eq!(scale_round(1, 48_000, 44_100), 1);
        assert_eq!(scale_round(u64::MAX, 384_000, 1), u64::MAX);
        assert_eq!(scale_round(10, 1, 0), 0);
    }
}
