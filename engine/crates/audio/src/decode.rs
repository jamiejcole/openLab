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

/// Most warnings kept per file; damage beyond that still plays as silence.
const MAX_WARNINGS: usize = 256;
/// Consecutive container errors after which the rest of a file counts as unreadable.
const MAX_READER_ERRORS: u32 = 16;

/// What is wrong with a playable file. Positions are frames on the playback timeline (the file's
/// own rate), the same timeline seeks, loops and slices use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarningKind {
    /// `frames` frames from `at` are missing or undecodable (damage the container skipped, or a
    /// packet that failed to decode): they play as silence, so later positions do not move.
    Missing,
    /// `frames` frames at `at` repeat positions already played: they are dropped.
    Repeated,
    /// The file ends at `at`, `frames` frames before its header says: it plays to its real end.
    Truncated,
    /// Nothing can be read past `at` (the container keeps failing, or its timestamps jump by
    /// more than a minute): the file plays to there.
    Unreadable,
    /// Seeks in this damaged file decode forward from the start (see [`Seeking::FromStart`]):
    /// preload it if it fits, or expect slow loops and start offsets deep in the file.
    SlowSeeks,
}

/// How a seek reaches a frame of a file on its probe's timeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seeking {
    /// The container's own seek lands on the probe's timeline (no damage).
    Direct,
    /// Damaged: a seek goes to the nearest checkpoint before its target that still decodes as
    /// the probe decoded it, then decodes forward (at most `INDEX_SPACING` frames).
    Indexed(Arc<SeekIndex>),
    /// Damaged, and seeks do not reproduce the probe's timeline (symphonia's MP3 reader counts
    /// damaged frames differently after a seek; AAC noise substitution is never bit-identical
    /// after one): seeks decode forward from the start, in bounded steps on a disk worker.
    FromStart,
}

/// Checkpoints recorded during a probe's full decode: a hash of the `CHECK_LEN` timeline
/// frames from every `INDEX_SPACING`-th frame.
#[derive(Debug, PartialEq, Eq)]
pub struct SeekIndex {
    /// Checkpoint `j` (from 1) is at frame `j * INDEX_SPACING`.
    hashes: Vec<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaWarning {
    pub kind: WarningKind,
    pub at: u64,
    pub frames: u64,
}

/// A playable file's probe: its format and exact length, any damage found on the way, and how
/// seeks reach its frames (pass it on in [`StreamRequest`](crate::StreamRequest)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probed {
    pub info: MediaInfo,
    pub warnings: Vec<MediaWarning>,
    pub seeking: Seeking,
}

impl Probed {
    /// Seeks in this file are slow ([`Seeking::FromStart`]): preload it if it fits the budget.
    pub fn preload_advised(&self) -> bool {
        self.seeking == Seeking::FromStart
    }
}

/// Frames between seek-index checkpoints.
pub const INDEX_SPACING: u64 = 8192;
/// Frames hashed at each checkpoint.
const CHECK_LEN: u64 = 256;
/// Decoding a seek to a checkpoint may take before the checkpoint counts as unusable.
const VERIFY_BUDGET: u64 = 8 * INDEX_SPACING;
/// Checkpoints tried before a seek falls back to decoding from the start.
const CHECKPOINT_TRIES: usize = 3;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv(mut h: u64, samples: &[f32]) -> u64 {
    for x in samples {
        for b in x.to_bits().to_le_bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(FNV_PRIME);
        }
    }
    h
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
    /// Preloading needs more memory than its budget (`bytes` is at least what it would take),
    /// or the memory could not be had. Stream the file instead, or report the cue.
    TooLarge {
        bytes: u64,
        limit: u64,
    },
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
            MediaError::TooLarge { bytes, limit } => {
                write!(
                    f,
                    "preload needs {bytes} bytes, over its {limit}-byte budget"
                )
            }
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
    /// Interleaved, `info.frames × info.channels`. Kept as the decode buffer (no final copy).
    samples: Vec<f32>,
    warnings: Vec<MediaWarning>,
}

impl Preloaded {
    #[cfg(test)]
    pub(crate) fn from_samples(channels: u16, rate: u32, samples: Vec<f32>) -> Arc<Self> {
        let frames = (samples.len() / usize::from(channels)) as u64;
        Arc::new(Preloaded {
            info: MediaInfo::new(channels, rate, frames),
            samples,
            warnings: Vec::new(),
        })
    }

    pub fn info(&self) -> MediaInfo {
        self.info
    }

    /// Damage found while decoding (the same as [`probe`] reports).
    pub fn warnings(&self) -> &[MediaWarning] {
        &self.warnings
    }

    /// Interleaved samples at the file's own rate.
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// Memory held by the decoded samples.
    pub fn bytes(&self) -> usize {
        self.samples.capacity() * std::mem::size_of::<f32>()
    }
}

/// Runs third-party decoding code so that a panic in it (a decoder bug hit by malformed input)
/// is a [`MediaError::Corrupt`] for this file instead of taking the calling thread down.
pub(crate) fn contain<T>(f: impl FnOnce() -> Result<T, MediaError>) -> Result<T, MediaError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(MediaError::Corrupt("decoder failed".into())))
}

/// Probes `path` and returns its exact format and length. Blocking: call from an I/O thread.
/// Damage does not fail the probe: it is listed in [`Probed::warnings`] and plays as described
/// there. Only a file with nothing playable is an error.
pub fn probe(path: &Path) -> Result<Probed, MediaError> {
    probe_media(Media::Path(path.to_path_buf()))
}

/// As [`probe`], for any [`Media`].
pub fn probe_media(media: Media) -> Result<Probed, MediaError> {
    match media {
        Media::Preloaded(p) => Ok(Probed {
            info: p.info,
            warnings: p.warnings.clone(),
            seeking: Seeking::Direct,
        }),
        other => contain(|| {
            let mut d = Decoder::open(other)?;
            let info = d.exact_info()?;
            let seeking = d.choose_seeking(info.frames)?;
            if seeking == Seeking::FromStart {
                d.warn(MediaWarning {
                    kind: WarningKind::SlowSeeks,
                    at: 0,
                    frames: 0,
                });
            }
            Ok(Probed {
                info,
                warnings: std::mem::take(&mut d.warnings),
                seeking,
            })
        }),
    }
}

/// Memory one preload may use by default (512 MiB: about 46 minutes of 48 kHz stereo).
pub const DEFAULT_PRELOAD_BUDGET: u64 = 512 << 20;

/// Decodes the whole of `media` into memory, within [`DEFAULT_PRELOAD_BUDGET`]. Blocking: call
/// from an I/O thread.
pub fn preload(media: Media) -> Result<Arc<Preloaded>, MediaError> {
    preload_within(media, DEFAULT_PRELOAD_BUDGET)
}

/// As [`preload`], using at most `max_bytes` for the samples. The container's declared length is
/// checked before decoding, and the decoded length while decoding (a small compressed file can
/// expand to far more than it declares), so going over is a [`MediaError::TooLarge`], never an
/// allocation failure that aborts the process.
pub fn preload_within(media: Media, max_bytes: u64) -> Result<Arc<Preloaded>, MediaError> {
    if let Media::Preloaded(p) = media {
        return Ok(p);
    }
    contain(|| decode_all(media, max_bytes))
}

const SAMPLE_BYTES: u64 = std::mem::size_of::<f32>() as u64;

fn decode_all(media: Media, max_bytes: u64) -> Result<Arc<Preloaded>, MediaError> {
    let mut d = Decoder::open(media)?;
    let ch = d.channels;
    let limit = max_bytes;
    let too_large = |samples: u64| MediaError::TooLarge {
        bytes: samples.saturating_mul(SAMPLE_BYTES),
        limit,
    };
    let max_samples = usize::try_from(max_bytes / SAMPLE_BYTES).unwrap_or(usize::MAX);
    let mut samples: Vec<f32> = Vec::new();
    if let Some(declared) = d.declared {
        let want = declared.saturating_mul(ch as u64);
        if want > max_samples as u64 {
            return Err(too_large(want));
        }
        let want = usize::try_from(want).unwrap_or(usize::MAX);
        samples
            .try_reserve_exact(want)
            .map_err(|_| too_large(want as u64))?;
    }
    let mut chunk = vec![0.0; 4096 * ch];
    loop {
        let n = d.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        let need = samples.len().saturating_add(n * ch);
        if need > max_samples {
            return Err(too_large(need as u64));
        }
        if need > samples.capacity() {
            // Grow geometrically, but never past the budget.
            let cap = samples
                .capacity()
                .saturating_mul(2)
                .max(need)
                .min(max_samples);
            samples
                .try_reserve_exact(cap - samples.len())
                .map_err(|_| too_large(cap as u64))?;
        }
        samples.extend_from_slice(chunk.get(..n * ch).unwrap_or_default());
    }
    samples.shrink_to_fit();
    let frames = (samples.len() / ch) as u64;
    if let Some(expected) = d.declared.filter(|&e| e > frames) {
        d.warn(MediaWarning {
            kind: WarningKind::Truncated,
            at: frames,
            frames: expected - frames,
        });
    }
    if frames == 0 {
        return Err(MediaError::Corrupt("no audio frames".into()));
    }
    let channels = u16::try_from(ch).unwrap_or(0);
    Ok(Arc::new(Preloaded {
        info: MediaInfo::new(channels, d.rate, frames),
        samples,
        warnings: std::mem::take(&mut d.warnings),
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
    /// Damage met since the last call (played as silence or skipped, see [`MediaWarning`]).
    fn take_warnings(&mut self) -> Vec<MediaWarning> {
        Vec::new()
    }
    /// Does at most about `budget` frames of the decoding a seek still needs; true once the
    /// next `read` is cheap. Lets a disk worker split a long seek across steps.
    fn prepare(&mut self, _budget: u64) -> Result<bool, MediaError> {
        Ok(true)
    }
}

/// Opens `media` as a frame source, seeking as its probe found (`Probed::seeking`). Blocking.
pub(crate) fn open_source(
    media: Media,
    seeking: Seeking,
) -> Result<Box<dyn FrameSource>, MediaError> {
    match media {
        Media::Preloaded(p) => Ok(Box::new(MemorySource { data: p, pos: 0 })),
        other => {
            let mut d = Decoder::open(other)?;
            d.seeking = seeking;
            Ok(Box::new(d))
        }
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
    /// The current run: `silence` frames of silence (damaged or missing audio), then the
    /// current packet's playable frames `buf[buf_pos..]`, interleaved. The run starts at `next`.
    silence: u64,
    buf: Vec<f32>,
    buf_pos: usize,
    /// Position of the next frame `read` returns.
    next: u64,
    /// Position just after the last packet decoded: where the next packet must start.
    cursor: u64,
    /// Set by a seek until the packet holding the target frame is decoded.
    resync: Option<Resync>,
    eof: bool,
    /// Damage found so far, in position order of discovery; `reported` of them were handed out
    /// by `take_warnings`.
    warnings: Vec<MediaWarning>,
    reported: usize,
    /// How seeks reach the probe's timeline (see [`Seeking`]).
    seeking: Seeking,
    /// Set while the probe's full decode records the seek index.
    index: Option<IndexBuilder>,
    /// Frames decoded since opening (what a seek costs).
    decoded: u64,
}

/// The seek index being recorded: checkpoint hashes so far, and the hash of a window in progress.
#[derive(Debug, Default)]
struct IndexBuilder {
    hashes: Vec<u64>,
    open: Option<u64>,
}

/// What a bounded refill achieved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fill {
    Ready,
    End,
    /// The budget ran out while a seek was still decoding towards its target.
    Pending,
}

#[derive(Clone, Copy, Debug)]
struct Resync {
    /// End position of the last packet decoded since the seek: a later packet starting after
    /// the target with this before it means the target lies in a gap.
    prev_end: Option<i64>,
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
                        crate::mp4_check::check_mp4_stts(&mut f)?;
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
                crate::mp4_check::check_mp4_stts(&mut inner)?;
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
            silence: 0,
            warnings: Vec::new(),
            reported: 0,
            seeking: Seeking::Direct,
            index: None,
            decoded: 0,
        };
        d.find_origin()?;
        Ok(d)
    }

    /// Finds position 0 and decodes up to the first playable frame, keeping its packet
    /// buffered. Position 0 is the container's start (encoder delay excluded: playable frames
    /// start at timestamp 0 or later), or the first decodable frame if that comes before it. If
    /// the first packets are damaged, the frames before the first decodable one are silence.
    fn find_origin(&mut self) -> Result<(), MediaError> {
        while let Some(abs) = self.decode_packet()? {
            let run = self.run_len();
            if run == 0 {
                continue;
            }
            if self.ts_per_frame.is_some() {
                let start = self.ts_to_frames(self.start_ts).max(0);
                let origin = if abs > 0 { start.min(abs) } else { abs };
                let lead = u64::try_from(abs - origin).unwrap_or(0);
                // A start far after the container's: take the stream as it is.
                let origin = if lead > self.max_gap() { abs } else { origin };
                self.origin = origin;
                // Anything noted before the origin was known is relative to 0.
                let shift = u64::try_from(origin).unwrap_or(0);
                for w in &mut self.warnings {
                    w.at = w.at.saturating_sub(shift);
                }
                let lead = u64::try_from(abs - origin).unwrap_or(0);
                if lead > 0 {
                    self.warn(MediaWarning {
                        kind: WarningKind::Missing,
                        at: 0,
                        frames: lead,
                    });
                    self.silence = self.silence.saturating_add(lead);
                }
            }
            self.cursor = self.run_len();
            return Ok(());
        }
        Ok(())
    }

    /// Exact length on the playback timeline: a PCM header's length is checked by decoding the
    /// tail; anything else (compressed formats, where frames can be missing without the
    /// container noticing) is counted by decoding the whole file, which also finds any damage.
    /// Damage is a warning, not an error: the file is playable as long as any audio decodes.
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
                        self.count_to_end()?.min(declared)
                    }
                }
            }
            None => {
                self.index = Some(IndexBuilder::default());
                self.count_to_end()?
            }
        };
        // Fewer frames than the container declares: the file was cut short (or, for lossy
        // formats, frames were lost where the container could not tell).
        if let Some(d) = self.declared.filter(|&d| frames < d) {
            self.warn(MediaWarning {
                kind: WarningKind::Truncated,
                at: frames,
                frames: d - frames,
            });
        }
        if frames == 0 {
            return Err(MediaError::Corrupt("no audio frames".into()));
        }
        Ok(MediaInfo::new(channels, self.rate, frames))
    }

    /// Decodes to the end and returns the frame position reached.
    fn count_to_end(&mut self) -> Result<u64, MediaError> {
        loop {
            self.index_feed();
            self.next += self.run_len();
            self.silence = 0;
            self.buf_pos = self.buf.len();
            if !self.refill()? {
                return Ok(self.next);
            }
        }
    }

    /// Records damage once (the same damage is met again on every pass and every seek).
    fn warn(&mut self, w: MediaWarning) {
        if self.warnings.len() < MAX_WARNINGS && !self.warnings.contains(&w) {
            self.warnings.push(w);
        }
    }

    /// Frames in the current run: silence, then buffered frames.
    fn run_len(&self) -> u64 {
        self.silence.saturating_add(self.kept() as u64)
    }

    /// Drops `n` frames from the front of the current run.
    fn drop_front(&mut self, n: u64) {
        let s = n.min(self.silence);
        self.silence -= s;
        let rest = usize::try_from(n - s)
            .unwrap_or(usize::MAX)
            .min(self.kept());
        self.buf_pos += rest * self.channels;
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

    /// Decodes the next packet of our track into the current run, with the packet's encoder
    /// delay and padding trimmed; a packet that fails to decode becomes silence of its length
    /// (and a warning). Returns the run's position, or `None` at the end of the stream.
    fn decode_packet(&mut self) -> Result<Option<i64>, MediaError> {
        self.buf.clear();
        self.buf_pos = 0;
        self.silence = 0;
        let mut reader_errors = 0;
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
                // A real read error is not damage in the file: the caller fails.
                Err(SymError::IoError(e)) => return Err(MediaError::Io(e.to_string())),
                // Damaged container data: the reader resynchronises on the next call. One that
                // keeps failing ends the playable audio here.
                Err(_) => {
                    reader_errors += 1;
                    if reader_errors >= MAX_READER_ERRORS {
                        self.unreadable();
                        break;
                    }
                    continue;
                }
            };
            reader_errors = 0;
            if packet.track_id != self.track {
                continue;
            }
            let trim = |d: u64| -> usize {
                let f = self.ts_to_frames(i64::try_from(d).unwrap_or(i64::MAX));
                usize::try_from(f).unwrap_or(0)
            };
            let (trim_start, trim_end, dur) = (
                trim(packet.trim_start.get()),
                trim(packet.trim_end.get()),
                trim(packet.dur.get()),
            );
            // Position of the packet's frame `front`.
            let base = match self.ts_per_frame {
                Some(_) => self
                    .ts_to_frames(packet.pts.get())
                    .saturating_sub(self.origin),
                None => i64::try_from(self.cursor).unwrap_or(i64::MAX),
            };
            let pos_of = |front: usize| match self.ts_per_frame {
                Some(_) => base.saturating_add(front as i64),
                None => base,
            };
            let decoded = match self.decoder.decode(&packet) {
                Ok(d) => d,
                Err(SymError::ResetRequired) => {
                    return Err(MediaError::Unsupported(
                        "stream parameters change mid-file".into(),
                    ));
                }
                // The decoder does no I/O: any other failure (including its bit reader running
                // off the end of a damaged packet) is damage in this packet.
                Err(_) => {
                    // The packet a lossy seek drops anyway may not decode without its predecessor.
                    if let Some(r) = self.resync.as_mut().filter(|r| r.discard_first) {
                        r.discard_first = false;
                        continue;
                    }
                    // Damaged audio data: silence of the packet's length keeps the timeline.
                    let pos = pos_of(trim_start);
                    let frames = dur as u64;
                    if frames > self.max_gap() {
                        self.unreadable();
                        break;
                    }
                    if frames > 0 {
                        self.warn(MediaWarning {
                            kind: WarningKind::Missing,
                            at: u64::try_from(pos).unwrap_or(0),
                            frames,
                        });
                    }
                    self.silence = frames;
                    return Ok(Some(pos));
                }
            };
            if decoded.spec().channels().count() != self.channels {
                return Err(MediaError::Unsupported(
                    "channel count changes mid-file".into(),
                ));
            }
            let n = decoded.frames();
            self.decoded = self.decoded.saturating_add(n as u64);
            decoded.copy_to_vec_interleaved(&mut self.buf);
            for x in &mut self.buf {
                *x = sanitize_sample(*x);
            }
            let front = trim_start.min(n);
            let mut back = trim_end.min(n - front);
            // A packet is at most its container duration, whether or not it decodes (a failed
            // one is silence of `dur`), so the timeline never depends on decoder state. This
            // also drops an AAC stream's padding past its container length.
            if dur > 0 && n - front - back > dur {
                back = n - front - dur;
            }
            self.buf.truncate((n - back) * self.channels);
            self.buf_pos = front * self.channels;
            // The container trims encoder padding only off the stream's last packet: whatever
            // follows (data left after damage) is not part of the audio.
            if trim_end > 0 {
                self.eof = true;
            }
            return Ok(Some(pos_of(front)));
        }
        Ok(None)
    }

    /// Longest gap filled with silence (60 s at the file's rate); a longer jump in the
    /// timestamps is not missing audio but a broken container.
    fn max_gap(&self) -> u64 {
        u64::from(self.rate) * 60
    }

    /// The rest of the file cannot be read: the playable audio ends at the cursor.
    fn unreadable(&mut self) {
        self.eof = true;
        self.warn(MediaWarning {
            kind: WarningKind::Unreadable,
            at: self.cursor,
            frames: 0,
        });
    }

    /// Decodes the next run that holds frame `next`, aligned so the run starts at that frame.
    /// Missing frames (a gap in the positions) become silence and a warning; repeated ones are
    /// dropped. Returns false at the end of the stream.
    fn refill(&mut self) -> Result<bool, MediaError> {
        Ok(self.fill(None)? == Fill::Ready)
    }

    /// As `refill`; with a `budget`, a seek still decoding towards its target stops after about
    /// that many frames (`Fill::Pending`) and continues on the next call.
    fn fill(&mut self, budget: Option<u64>) -> Result<Fill, MediaError> {
        let mut spent = 0u64;
        loop {
            if self.resync.is_some() && budget.is_some_and(|b| spent >= b) {
                return Ok(Fill::Pending);
            }
            let Some(mut pos) = self.decode_packet()? else {
                // A seek that runs into the end before its target may have met a container that
                // reads differently after a seek (symphonia's FLAC reader can stop short of
                // damage it reads past from the start): decode forward from the start once.
                if let Some(r) = self.resync
                    && r.tries + 1 < SEEK_TRIES
                {
                    self.seek_with(self.next, SEEK_TRIES - 1)?;
                    continue;
                }
                return Ok(Fill::End);
            };
            let len = i64::try_from(self.run_len()).unwrap_or(i64::MAX);
            let end = pos.saturating_add(len);
            // Every packet costs a decode, even one that yields nothing.
            spent = spent.saturating_add(self.run_len().max(64));
            if let Some(r) = self.resync {
                // Counted positions need every packet, even the ones dropped here.
                self.cursor = u64::try_from(end).unwrap_or(0);
                if r.discard_first {
                    self.resync = Some(Resync {
                        discard_first: false,
                        prev_end: if len > 0 { Some(end) } else { r.prev_end },
                        ..r
                    });
                    continue;
                }
                let want = i64::try_from(self.next).unwrap_or(i64::MAX);
                // An empty run (a packet that decoded to nothing) covers no position.
                if len == 0 {
                    continue;
                }
                if end <= want {
                    self.resync = Some(Resync {
                        prev_end: Some(end),
                        ..r
                    });
                    continue;
                }
                if pos > want {
                    match r.prev_end {
                        // The target lies in a gap the decode just stepped over.
                        Some(prev) if prev <= want => {
                            let gap = u64::try_from(pos - want).unwrap_or(0);
                            self.warn(MediaWarning {
                                kind: WarningKind::Missing,
                                at: u64::try_from(prev).unwrap_or(0),
                                frames: u64::try_from(pos - prev).unwrap_or(0),
                            });
                            self.silence = self.silence.saturating_add(gap);
                            pos = want;
                        }
                        // The container landed after the target: retry from further back.
                        _ => {
                            if r.tries + 1 >= SEEK_TRIES {
                                return Err(MediaError::Corrupt("cannot seek to frame".into()));
                            }
                            self.seek_with(self.next, r.tries + 1)?;
                            continue;
                        }
                    }
                }
                self.drop_front(u64::try_from(want - pos).unwrap_or(0));
                self.resync = None;
                return Ok(Fill::Ready);
            }
            if len == 0 {
                continue;
            }
            let cursor = i64::try_from(self.cursor).unwrap_or(i64::MAX);
            if pos > cursor {
                // Frames are missing (damage the container skipped): play silence for them.
                let gap = u64::try_from(pos - cursor).unwrap_or(0);
                if gap > self.max_gap() {
                    self.unreadable();
                    return Ok(Fill::End);
                }
                self.warn(MediaWarning {
                    kind: WarningKind::Missing,
                    at: self.cursor,
                    frames: gap,
                });
                self.silence = self.silence.saturating_add(gap);
            } else if pos < cursor {
                // Frames repeat positions already played: drop them.
                let dup = u64::try_from(cursor - pos).unwrap_or(0);
                self.warn(MediaWarning {
                    kind: WarningKind::Repeated,
                    at: u64::try_from(pos).unwrap_or(0),
                    frames: dup.min(u64::try_from(len).unwrap_or(0)),
                });
                if end <= cursor {
                    continue;
                }
                self.drop_front(dup);
            }
            self.cursor = u64::try_from(end).unwrap_or(0);
            return Ok(Fill::Ready);
        }
    }

    /// Seeks the container so that decoding reaches `frame` after a pre-roll.
    fn seek_with(&mut self, frame: u64, tries: u32) -> Result<(), MediaError> {
        self.buf.clear();
        self.buf_pos = 0;
        self.eof = false;
        self.silence = 0;
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
            // Decoding from the start: anything before the first packet is a leading gap.
            prev_end: at_start.then_some(0),
            discard_first: self.lossy && !at_start,
            tries,
        });
        Ok(())
    }

    /// Feeds the current run (frames `next..next + run_len()`) to the seek index being recorded.
    fn index_feed(&mut self) {
        let (start, len, silence, ch) = (self.next, self.run_len(), self.silence, self.channels);
        let Some(ib) = self.index.as_mut() else {
            return;
        };
        loop {
            let c = (ib.hashes.len() as u64 + 1) * INDEX_SPACING;
            let w_end = c + CHECK_LEN;
            if c >= start + len {
                return;
            }
            let from = c.max(start);
            let to = w_end.min(start + len);
            let mut h = ib.open.unwrap_or(FNV_OFFSET);
            if from < to {
                // Silence first, then the buffered frames.
                let zeros = (start + silence).saturating_sub(from).min(to - from);
                for _ in 0..zeros * ch as u64 {
                    h = fnv(h, &[0.0]);
                }
                let b0 = (from + zeros).saturating_sub(start + silence);
                let b1 = to.saturating_sub(start + silence);
                let at = |f: u64| self.buf_pos + usize::try_from(f).unwrap_or(0) * ch;
                if b1 > b0
                    && let Some(x) = self.buf.get(at(b0)..at(b1))
                {
                    h = fnv(h, x);
                }
            }
            if w_end <= start + len {
                ib.hashes.push(h);
                ib.open = None;
            } else {
                ib.open = Some(h);
                return;
            }
        }
    }

    /// After the probe's full decode: how seeks in this file can reach its timeline. A damaged
    /// file's checkpoints after each piece of damage, and its last, must decode after a seek as
    /// the probe decoded them; otherwise seeks decode from the start.
    fn choose_seeking(&mut self, frames: u64) -> Result<Seeking, MediaError> {
        let hashes = self.index.take().map(|b| b.hashes).unwrap_or_default();
        if self.warnings.is_empty() || self.pcm {
            return Ok(Seeking::Direct);
        }
        let index = Arc::new(SeekIndex { hashes });
        let n = index.hashes.len() as u64;
        let mut picks: Vec<u64> = self
            .warnings
            .iter()
            .map(|w| (w.at + w.frames).div_ceil(INDEX_SPACING).max(1))
            .chain(std::iter::once(n))
            .filter(|&j| j >= 1 && j <= n && j * INDEX_SPACING + CHECK_LEN <= frames)
            .collect();
        picks.sort_unstable();
        picks.dedup();
        // Only the straight read's damage counts: a seek that misplaces it (the reason it
        // fails verification) would record it at the wrong positions.
        let warnings = self.warnings.clone();
        let mut ok = true;
        for j in picks {
            if !self.verify_checkpoint(&index, j)? {
                ok = false;
                break;
            }
        }
        self.warnings = warnings;
        Ok(if ok {
            Seeking::Indexed(index)
        } else {
            Seeking::FromStart
        })
    }

    /// Seeks (the container's way) to checkpoint `j` and checks the frames there decode exactly
    /// as the probe decoded them. On true the decoder is positioned just after the window.
    fn verify_checkpoint(&mut self, index: &SeekIndex, j: u64) -> Result<bool, MediaError> {
        let Some(&want) = usize::try_from(j - 1)
            .ok()
            .and_then(|i| index.hashes.get(i))
        else {
            return Ok(false);
        };
        self.seek_with(j * INDEX_SPACING, 0)?;
        if self.fill_prepared(VERIFY_BUDGET)? != Fill::Ready {
            return Ok(false);
        }
        let mut buf = vec![0.0; usize::try_from(CHECK_LEN).unwrap_or(0) * self.channels];
        let mut got = 0;
        while got < buf.len() / self.channels {
            let n = self.read(buf.get_mut(got * self.channels..).unwrap_or_default())?;
            if n == 0 {
                return Ok(false);
            }
            got += n;
        }
        Ok(fnv(FNV_OFFSET, &buf) == want)
    }

    /// Runs a pending seek's decoding within `budget`; `Ready` also when nothing is pending.
    fn fill_prepared(&mut self, budget: u64) -> Result<Fill, MediaError> {
        if self.resync.is_none() {
            return Ok(Fill::Ready);
        }
        self.fill(Some(budget))
    }

    /// A seek in an indexed file: from the nearest checkpoint before `frame` that still
    /// decodes as probed, decode forward; if none of the nearest few does, from the start.
    fn seek_indexed(&mut self, index: &SeekIndex, frame: u64) -> Result<(), MediaError> {
        let top = (frame.saturating_sub(CHECK_LEN) / INDEX_SPACING).min(index.hashes.len() as u64);
        for j in (1..=top).rev().take(CHECKPOINT_TRIES) {
            let seen = self.warnings.len();
            if self.verify_checkpoint(index, j)? {
                return self.forward_to(frame);
            }
            // What a misplacing seek met is not on the probe's timeline.
            self.warnings.truncate(seen.max(self.reported));
        }
        self.seek_with(frame, SEEK_TRIES - 1)
    }

    /// From a position reached by reading through (just after a verified checkpoint), moves on
    /// to `frame` by decoding forward, as part of the next reads or `prepare` calls.
    fn forward_to(&mut self, frame: u64) -> Result<(), MediaError> {
        let ahead = frame.saturating_sub(self.next);
        if ahead < self.run_len() {
            self.drop_front(ahead);
            self.next = frame;
            return Ok(());
        }
        let cursor = i64::try_from(self.cursor).unwrap_or(i64::MAX);
        self.buf.clear();
        self.buf_pos = 0;
        self.silence = 0;
        self.next = frame;
        self.resync = Some(Resync {
            prev_end: Some(cursor),
            discard_first: false,
            tries: SEEK_TRIES - 1,
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
        match self.seeking.clone() {
            Seeking::Direct => self.seek_with(frame, 0),
            Seeking::Indexed(index) => self.seek_indexed(&index, frame),
            Seeking::FromStart => self.seek_with(frame, SEEK_TRIES - 1),
        }
    }

    fn prepare(&mut self, budget: u64) -> Result<bool, MediaError> {
        Ok(self.fill_prepared(budget)? != Fill::Pending)
    }

    fn read(&mut self, out: &mut [f32]) -> Result<usize, MediaError> {
        let ch = self.channels;
        let want = out.len() / ch;
        let mut got = 0;
        while got < want {
            if self.run_len() == 0 && !self.refill()? {
                break;
            }
            if self.silence > 0 {
                let n = usize::try_from(self.silence)
                    .unwrap_or(usize::MAX)
                    .min(want - got);
                if let Some(dst) = out.get_mut(got * ch..(got + n) * ch) {
                    dst.fill(0.0);
                }
                self.silence -= n as u64;
                self.next += n as u64;
                got += n;
                continue;
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

    fn take_warnings(&mut self) -> Vec<MediaWarning> {
        let new = self
            .warnings
            .get(self.reported..)
            .unwrap_or_default()
            .to_vec();
        self.reported = self.warnings.len();
        new
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
            let mut src = open_source(Media::Path(fixture(name)), Seeking::Direct).unwrap();
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

    /// A damaged file seeks onto the same timeline its probe, preload and a straight read give,
    /// bit for bit, from offsets all over the file and around and inside the damage, whichever
    /// way the probe chose to seek it. Also pins which formats get which way.
    #[test]
    fn damaged_files_seek_onto_the_probe_timeline() {
        let damage = |name: &str, at: usize, len: usize| {
            let mut b = std::fs::read(fixture(name)).unwrap();
            for x in b.get_mut(at..at + len).unwrap() {
                *x ^= 0x5a;
            }
            b
        };
        let cut = |name: &str, keep: usize| {
            let b = std::fs::read(fixture(name)).unwrap();
            b.get(..b.len() * keep / 4).unwrap().to_vec()
        };
        let finding = |name: &str| {
            std::fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fuzz-findings")
                    .join(name),
            )
            .unwrap()
        };
        let cases = [
            ("flac", damage("tone.flac", 13_832, 16), "indexed"),
            ("flac", damage("tone.flac", 10_374, 16), "indexed"),
            ("flac", cut("tone.flac", 3), "indexed"),
            ("ogg", damage("tone-4s.ogg", 10_888, 16), "indexed"),
            // AAC noise substitution never decodes identically after a seek.
            ("m4a", damage("tone.m4a", 5_964, 16), "from start"),
            ("mp3", damage("tone.mp3", 4_619, 300), "indexed"),
            ("mp3", damage("tone.mp3", 6_158, 300), "indexed"),
            // symphonia's MP3 reader places frames after this damage differently after a seek.
            (
                "mp3",
                finding("mp3-damage-reads-differently-after-seek.mp3"),
                "from start",
            ),
        ];
        for (ext, bytes, want) in cases {
            let media =
                || Media::Reader(Box::new(io::Cursor::new(bytes.clone())), Some(ext.into()));
            let probed = probe_media(media()).unwrap();
            assert!(!probed.warnings.is_empty(), "{ext}: damage found");
            let got = match probed.seeking {
                Seeking::Indexed(_) => "indexed",
                Seeking::FromStart => "from start",
                Seeking::Direct => "direct",
            };
            assert_eq!(got, want, "{ext}: seeking");
            let full = preload(media()).unwrap();
            let all = full.samples();
            let frames = full.info().frames;
            let mut src = open_source(media(), probed.seeking.clone()).unwrap();
            let mut targets: Vec<u64> = (0..frames).step_by(997).collect();
            targets.push(frames - 1);
            for w in &probed.warnings {
                for t in [
                    w.at.saturating_sub(700),
                    w.at.saturating_sub(1),
                    w.at,
                    w.at + 1,
                ] {
                    targets.push(t);
                }
                let end = w.at + w.frames;
                targets.extend([w.at + w.frames / 2, end.saturating_sub(1), end, end + 300]);
            }
            for at in targets.into_iter().filter(|&t| t < frames) {
                src.seek(at).unwrap();
                let mut buf = vec![0.0; 2 * 500];
                let mut got = 0;
                while got < 500 {
                    let n = src.read(&mut buf[got * 2..]).unwrap();
                    if n == 0 {
                        break;
                    }
                    got += n;
                }
                let n = (frames - at).min(500) as usize;
                assert_eq!(got, n, "{ext} at {at}: frames");
                let start = at as usize * 2;
                let want = &all[start..start + n * 2];
                if let Some(i) = buf[..n * 2].iter().zip(want).position(|(a, b)| a != b) {
                    panic!("{ext} at {at}: sample {i} is {} not {}", buf[i], want[i]);
                }
            }
        }
    }

    /// Review H-R1: a seek deep into an indexed damaged file decodes from a checkpoint near the
    /// target, not from the start.
    #[test]
    fn indexed_seeks_decode_a_bounded_amount() {
        let mut b = std::fs::read(fixture("tone-4s.ogg")).unwrap();
        for x in b.get_mut(10_888..10_904).unwrap() {
            *x ^= 0x5a;
        }
        let media = || Media::Reader(Box::new(io::Cursor::new(b.clone())), Some("ogg".into()));
        let probed = probe_media(media()).unwrap();
        assert!(matches!(probed.seeking, Seeking::Indexed(_)));
        let mut d = Decoder::open(media()).unwrap();
        d.seeking = probed.seeking.clone();
        for at in [170_000u64, 140_000, 100_000] {
            let before = d.decoded;
            d.seek(at).unwrap();
            let mut buf = vec![0.0; 2 * 256];
            assert_eq!(d.read(&mut buf).unwrap(), 256);
            let cost = d.decoded - before;
            // Pre-roll and the checkpoint window, then at most one spacing forward (plus packet
            // rounding): far less than the `at` frames a decode from the start takes.
            assert!(
                cost < 2 * SEEK_PREROLL + 2 * INDEX_SPACING,
                "seek to {at} decoded {cost}"
            );
        }
    }

    /// Review L-R1: damage met inside a seek (here a file that probed clean but is now damaged:
    /// the packet holding the target no longer decodes) is reported, not just silenced.
    #[test]
    fn damage_met_during_a_seek_is_reported() {
        let mut b = std::fs::read(fixture("tone.m4a")).unwrap();
        for x in b.get_mut(5_964..5_980).unwrap() {
            *x ^= 0x5a;
        }
        let mut src = open_source(
            Media::Reader(Box::new(io::Cursor::new(b)), None),
            Seeking::Direct,
        )
        .unwrap();
        src.seek(17_500).unwrap();
        let mut buf = vec![0.0; 2 * 100];
        assert_eq!(src.read(&mut buf).unwrap(), 100);
        assert!(
            buf.iter().all(|&x| x == 0.0),
            "the damaged packet plays as silence"
        );
        let w = src.take_warnings();
        assert!(
            w.contains(&MediaWarning {
                kind: WarningKind::Missing,
                at: 17_408,
                frames: 1_024
            }),
            "{w:?}"
        );
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
