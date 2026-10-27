// SPDX-License-Identifier: GPL-3.0-or-later

//! In-house reader for many-channel uncompressed WAV and AIFF (M3.23). symphonia 0.6.1 reads WAV
//! only up to a channel layout it knows (about 18 to 26 channels, 16 for ambisonic layouts), but
//! voices carry up to [`MAX_VOICE_CHANNELS`]. Files with more channels than
//! [`NATIVE_MAX_CHANNELS`](crate::decode::NATIVE_MAX_CHANNELS) are read here; fewer channels, and
//! every compressed format, still go through symphonia.
//!
//! Reads integer 8/16/24/32-bit and float 32/64-bit samples, little- or big-endian (WAV,
//! WAVE_FORMAT_EXTENSIBLE, AIFF, AIFF-C `NONE`/`sowt`/`fl32`/`fl64`/`in24`/`in32`). The channel
//! mask of an extensible header is ignored: openLab routes channels by index. Disk and I/O
//! threads only; every failure is a typed [`MediaError`], and no length from the file sizes an
//! allocation.

use std::io::{self, SeekFrom};

use crate::decode::{
    FrameSource, HEADER_CHUNKS, MIN_MEDIA_RATE, MediaError, MediaInfo, MediaRead, MediaWarning,
    WarningKind, sanitize_sample,
};
use crate::mixer::{MAX_SAMPLE_RATE, MAX_VOICE_CHANNELS};

/// Frames converted per read: bounds the byte scratch (2 MiB at 64 channels of 64-bit floats).
const CHUNK_FRAMES: usize = 4096;
/// Bytes of a `fmt ` / `COMM` chunk looked at (WAVE_FORMAT_EXTENSIBLE needs 40, AIFF-C 22).
const FORMAT_BYTES: usize = 40;
/// The tail of the KSDATAFORMAT_SUBTYPE GUIDs after their two-byte format tag.
const GUID_TAIL: [u8; 14] = [
    0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
];
/// The same for the ambisonic B-format sub-formats (`{0000000N-0721-11d3-8644-c8c1ca000000}`):
/// laid out like plain PCM / float, routed by index like every other file.
const AMBISONIC_TAIL: [u8; 14] = [
    0x00, 0x00, 0x21, 0x07, 0xd3, 0x11, 0x86, 0x44, 0xc8, 0xc1, 0xca, 0x00, 0x00, 0x00,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// WAV's 8-bit samples.
    Unsigned8,
    /// Two's complement, left-justified in `bytes`.
    Int,
    Float,
}

#[derive(Clone, Copy, Debug)]
struct Layout {
    kind: Kind,
    bytes: usize,
    big_endian: bool,
}

impl Layout {
    /// Integer samples. WAV's 8-bit samples are unsigned, AIFF's signed.
    fn int(bytes: usize, big_endian: bool, wav: bool) -> Self {
        let kind = if bytes == 1 && wav {
            Kind::Unsigned8
        } else {
            Kind::Int
        };
        Layout {
            kind,
            bytes,
            big_endian,
        }
    }

    fn float(bytes: usize, big_endian: bool) -> Self {
        Layout {
            kind: Kind::Float,
            bytes,
            big_endian,
        }
    }

    /// `v · 2^-(bits-1)` maps an integer sample to ±1.0 exactly.
    fn scale(&self) -> f64 {
        1.0 / (1u64 << (8 * self.bytes - 1)) as f64
    }

    /// One sample from exactly `bytes` bytes. Any other length is silence (callers chunk by
    /// `bytes`, so it cannot happen).
    fn sample(&self, b: &[u8], scale: f64) -> f32 {
        let be = self.big_endian;
        let v: i32 = match (self.kind, b) {
            (Kind::Unsigned8, &[a]) => i32::from(a) - 128,
            (Kind::Int, &[a]) => i32::from(i8::from_ne_bytes([a])),
            (Kind::Int, &[a, b]) if be => i32::from(i16::from_be_bytes([a, b])),
            (Kind::Int, &[a, b]) => i32::from(i16::from_le_bytes([a, b])),
            (Kind::Int, &[a, b, c]) if be => i32::from_be_bytes([a, b, c, 0]) >> 8,
            (Kind::Int, &[a, b, c]) => i32::from_le_bytes([0, a, b, c]) >> 8,
            (Kind::Int, &[a, b, c, d]) if be => i32::from_be_bytes([a, b, c, d]),
            (Kind::Int, &[a, b, c, d]) => i32::from_le_bytes([a, b, c, d]),
            (Kind::Float, &[a, b, c, d]) => {
                let x = if be {
                    f32::from_be_bytes([a, b, c, d])
                } else {
                    f32::from_le_bytes([a, b, c, d])
                };
                return sanitize_sample(x);
            }
            (Kind::Float, &[a, b, c, d, e, f, g, h]) => {
                let x = if be {
                    f64::from_be_bytes([a, b, c, d, e, f, g, h])
                } else {
                    f64::from_le_bytes([a, b, c, d, e, f, g, h])
                };
                return sanitize_sample(x as f32);
            }
            _ => 0,
        };
        (f64::from(v) * scale) as f32
    }
}

/// What a header says, before it is checked against the file's length.
struct Header {
    channels: usize,
    rate: u32,
    layout: Layout,
    /// AIFF's frame count (`None` for WAV, whose length is the data chunk's).
    declared_frames: Option<u64>,
    data_start: u64,
    /// Bytes the data chunk declares; `None` when the writer left the length open.
    data_len: Option<u64>,
}

/// A WAV or AIFF file of many channels as a [`FrameSource`].
pub(crate) struct PcmSource {
    inner: Box<dyn MediaRead>,
    channels: usize,
    rate: u32,
    layout: Layout,
    scale: f64,
    data_start: u64,
    /// Frames that exist in the file (the shorter of its header's length and its bytes).
    frames: u64,
    /// The next frame `read` returns.
    pos: u64,
    /// The reader is not at `pos` (after a seek or a short read): seek before reading.
    dirty: bool,
    scratch: Vec<u8>,
    warnings: Vec<MediaWarning>,
}

fn io_err(e: &io::Error) -> MediaError {
    MediaError::Io(e.to_string())
}

fn corrupt(what: &str) -> MediaError {
    MediaError::Corrupt(format!("header: {what}"))
}

/// Reads `buf.len()` bytes, or fewer at the end of the file; returns how many.
fn read_full(r: &mut dyn MediaRead, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while let Some(rest) = buf.get_mut(n..).filter(|rest| !rest.is_empty()) {
        match r.read(rest) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

fn be16(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]))
}

fn be32(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *b.get(i)?,
        *b.get(i + 1)?,
        *b.get(i + 2)?,
        *b.get(i + 3)?,
    ]))
}

fn le16(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]))
}

fn le32(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *b.get(i)?,
        *b.get(i + 1)?,
        *b.get(i + 2)?,
        *b.get(i + 3)?,
    ]))
}

/// A sample rate from an 80-bit IEEE extended number; `None` if it is not a whole rate in range.
fn extended_rate(b: &[u8]) -> Option<u32> {
    let sign_exp = be16(b, 0)?;
    let mant = u64::from_be_bytes(b.get(2..10)?.try_into().ok()?);
    let exp = i32::from(sign_exp & 0x7fff);
    if sign_exp & 0x8000 != 0 || mant == 0 || !(16383..16383 + 32).contains(&exp) {
        return None;
    }
    let value = mant as f64 * 2f64.powi(exp - 16383 - 63);
    // Rates are whole numbers; a fraction means a corrupt header.
    if value.fract() == 0.0 && value >= 1.0 {
        u32::try_from(value as u64).ok()
    } else {
        None
    }
}

fn check_rate(rate: u32) -> Result<(), MediaError> {
    if (MIN_MEDIA_RATE..=MAX_SAMPLE_RATE).contains(&rate) {
        Ok(())
    } else {
        Err(MediaError::Unsupported(format!(
            "sample rate {rate} Hz outside {MIN_MEDIA_RATE}..={MAX_SAMPLE_RATE} Hz"
        )))
    }
}

fn check_channels(ch: u16) -> Result<usize, MediaError> {
    let ch = usize::from(ch);
    if (1..=MAX_VOICE_CHANNELS).contains(&ch) {
        Ok(ch)
    } else {
        Err(corrupt(&format!("{ch} channels")))
    }
}

/// Walks the chunks after the 12-byte container header, calling `on_chunk(id, body_start, size,
/// body)` with up to [`FORMAT_BYTES`] of each chunk's body, until it returns `Some`.
fn walk<T>(
    r: &mut dyn MediaRead,
    file_len: u64,
    big_endian: bool,
    mut on_chunk: impl FnMut(&[u8; 4], u64, u32, &[u8]) -> Result<Option<T>, MediaError>,
) -> Result<T, MediaError> {
    let mut pos = 12u64;
    for _ in 0..HEADER_CHUNKS {
        r.seek(SeekFrom::Start(pos)).map_err(|e| io_err(&e))?;
        let mut head = [0u8; 8];
        if read_full(r, &mut head).map_err(|e| io_err(&e))? < 8 {
            return Err(corrupt("no audio data chunk"));
        }
        let [i0, i1, i2, i3, ..] = head;
        let id = [i0, i1, i2, i3];
        let size = if big_endian {
            be32(&head, 4)
        } else {
            le32(&head, 4)
        }
        .unwrap_or(0);
        let body_start = pos + 8;
        let mut body = [0u8; FORMAT_BYTES];
        let n = usize::try_from(size)
            .unwrap_or(usize::MAX)
            .min(FORMAT_BYTES);
        let got = read_full(r, body.get_mut(..n).unwrap_or_default()).map_err(|e| io_err(&e))?;
        if let Some(found) = on_chunk(&id, body_start, size, body.get(..got).unwrap_or_default())? {
            return Ok(found);
        }
        // Chunks are padded to an even length.
        pos = body_start + u64::from(size) + u64::from(size & 1);
        if pos > file_len {
            return Err(corrupt("chunk reaches past the end of the file"));
        }
    }
    Err(corrupt("no audio data chunk"))
}

fn parse_wav(r: &mut dyn MediaRead, file_len: u64) -> Result<Header, MediaError> {
    let mut fmt: Option<(usize, u32, Layout)> = None;
    walk(r, file_len, false, |id, body_start, size, body| {
        match id {
            b"fmt " if fmt.is_none() => {
                if size < 16 || body.len() < 16 {
                    return Err(corrupt("fmt chunk too short"));
                }
                let mut tag = le16(body, 0).unwrap_or(0);
                let channels = check_channels(le16(body, 2).unwrap_or(0))?;
                let rate = le32(body, 4).unwrap_or(0);
                let align = usize::from(le16(body, 12).unwrap_or(0));
                let bits = usize::from(le16(body, 14).unwrap_or(0));
                if tag == 0xFFFE {
                    // WAVE_FORMAT_EXTENSIBLE: cbSize, valid bits, channel mask, sub-format GUID.
                    if size < 40 || body.len() < 40 || le16(body, 16).unwrap_or(0) < 22 {
                        return Err(corrupt("extensible fmt chunk too short"));
                    }
                    if usize::from(le16(body, 18).unwrap_or(0)) > bits {
                        return Err(corrupt("extensible fmt chunk"));
                    }
                    let guid = body.get(26..40);
                    if guid != Some(&GUID_TAIL[..]) && guid != Some(&AMBISONIC_TAIL[..]) {
                        return Err(MediaError::Unsupported("WAV extensible sub-format".into()));
                    }
                    tag = le16(body, 24).unwrap_or(0);
                }
                let layout = match (tag, bits) {
                    (1, 8 | 16 | 24 | 32) => Layout::int(bits / 8, false, true),
                    (3, 32 | 64) => Layout::float(bits / 8, false),
                    (1 | 3, _) => {
                        return Err(MediaError::Unsupported(format!(
                            "WAV sample size of {bits} bits"
                        )));
                    }
                    _ => {
                        return Err(MediaError::Unsupported(format!(
                            "WAV format tag {tag:#06x} with {channels} channels"
                        )));
                    }
                };
                if align != channels * layout.bytes {
                    return Err(corrupt(
                        "block align does not match channels and sample size",
                    ));
                }
                check_rate(rate)?;
                fmt = Some((channels, rate, layout));
                Ok(None)
            }
            b"data" => {
                let (channels, rate, layout) = fmt.ok_or_else(|| corrupt("data before fmt"))?;
                // 0xFFFFFFFF is the streaming convention for "until the end of the file".
                let data_len = (size != u32::MAX).then_some(u64::from(size));
                Ok(Some(Header {
                    channels,
                    rate,
                    layout,
                    declared_frames: None,
                    data_start: body_start,
                    data_len,
                }))
            }
            _ => Ok(None),
        }
    })
}

fn parse_aiff(r: &mut dyn MediaRead, file_len: u64, aifc: bool) -> Result<Header, MediaError> {
    let mut comm: Option<(usize, u32, Layout, u64)> = None;
    walk(r, file_len, true, |id, body_start, size, body| match id {
        b"COMM" if comm.is_none() => {
            let need: usize = if aifc { 22 } else { 18 };
            if usize::try_from(size).unwrap_or(0) < need || body.len() < need {
                return Err(corrupt("COMM chunk too short"));
            }
            let channels = check_channels(be16(body, 0).unwrap_or(0))?;
            let frames = u64::from(be32(body, 2).unwrap_or(0));
            let bits = usize::from(be16(body, 6).unwrap_or(0));
            let rate = body
                .get(8..18)
                .and_then(extended_rate)
                .ok_or_else(|| corrupt("sample rate"))?;
            check_rate(rate)?;
            // Integer samples take as many bytes as their bit count needs, left-justified.
            let int = |big_endian: bool| {
                if (1..=32).contains(&bits) {
                    Ok(Layout::int(bits.div_ceil(8), big_endian, false))
                } else {
                    Err(MediaError::Unsupported(format!(
                        "AIFF sample size of {bits} bits"
                    )))
                }
            };
            let comp = if aifc {
                body.get(18..22)
            } else {
                Some(&b"NONE"[..])
            };
            let layout = match comp {
                Some(b"NONE" | b"in24" | b"in32") => int(true)?,
                Some(b"sowt") => int(false)?,
                Some(b"twos") => int(true)?,
                Some(b"fl32" | b"FL32") => Layout::float(4, true),
                Some(b"fl64" | b"FL64") => Layout::float(8, true),
                _ => {
                    return Err(MediaError::Unsupported(format!(
                        "compressed AIFF-C with {channels} channels"
                    )));
                }
            };
            comm = Some((channels, rate, layout, frames));
            Ok(None)
        }
        b"SSND" => {
            let (channels, rate, layout, frames) =
                comm.ok_or_else(|| corrupt("SSND before COMM"))?;
            let offset = u64::from(be32(body, 0).unwrap_or(0));
            let skip = 8 + offset;
            let data_len = u64::from(size)
                .checked_sub(skip)
                .ok_or_else(|| corrupt("SSND offset past its chunk"))?;
            Ok(Some(Header {
                channels,
                rate,
                layout,
                declared_frames: Some(frames),
                data_start: body_start + skip,
                data_len: Some(data_len),
            }))
        }
        _ => Ok(None),
    })
}

impl PcmSource {
    /// Parses the container header of a WAV or AIFF stream (anything else is `Unsupported`).
    pub(crate) fn open(mut inner: Box<dyn MediaRead>) -> Result<Self, MediaError> {
        let file_len = inner.seek(SeekFrom::End(0)).map_err(|e| io_err(&e))?;
        inner.seek(SeekFrom::Start(0)).map_err(|e| io_err(&e))?;
        let mut head = [0u8; 12];
        if read_full(&mut *inner, &mut head).map_err(|e| io_err(&e))? < 12 {
            return Err(corrupt("file ends inside the container header"));
        }
        let h = match (head.get(0..4), head.get(8..12)) {
            (Some(b"RIFF"), Some(b"WAVE")) => parse_wav(&mut *inner, file_len)?,
            (Some(b"FORM"), Some(b"AIFF")) => parse_aiff(&mut *inner, file_len, false)?,
            (Some(b"FORM"), Some(b"AIFC")) => parse_aiff(&mut *inner, file_len, true)?,
            _ => return Err(MediaError::Unsupported("not a WAV or AIFF file".into())),
        };
        let frame_bytes = (h.channels * h.layout.bytes) as u64;
        let avail = file_len.saturating_sub(h.data_start);
        let present = h.data_len.map_or(avail, |d| d.min(avail)) / frame_bytes;
        // What the header promises: AIFF's frame count (0 = unknown), else the data chunk's size.
        let declared = h
            .declared_frames
            .filter(|&f| f > 0)
            .or_else(|| h.data_len.map(|d| d / frame_bytes))
            .unwrap_or(present);
        let frames = present.min(declared);
        if frames == 0 {
            return Err(MediaError::Corrupt("no audio frames".into()));
        }
        let mut warnings = Vec::new();
        if declared > frames {
            warnings.push(MediaWarning {
                kind: WarningKind::Truncated,
                at: frames,
                frames: declared - frames,
            });
        }
        let scale = h.layout.scale();
        Ok(PcmSource {
            inner,
            channels: h.channels,
            rate: h.rate,
            layout: h.layout,
            scale,
            data_start: h.data_start,
            frames,
            pos: 0,
            dirty: true,
            scratch: Vec::new(),
            warnings,
        })
    }

    pub(crate) fn info(&self) -> MediaInfo {
        let channels = u16::try_from(self.channels).unwrap_or(u16::MAX);
        MediaInfo::new(channels, self.rate, self.frames)
    }

    /// Frames the header promises that the file does not hold.
    pub(crate) fn warnings(&self) -> &[MediaWarning] {
        &self.warnings
    }

    fn frame_bytes(&self) -> usize {
        self.channels * self.layout.bytes
    }

    fn sync(&mut self) -> Result<(), MediaError> {
        let at = self
            .pos
            .saturating_mul(self.frame_bytes() as u64)
            .saturating_add(self.data_start);
        self.inner
            .seek(SeekFrom::Start(at))
            .map_err(|e| io_err(&e))?;
        self.dirty = false;
        Ok(())
    }
}

impl FrameSource for PcmSource {
    fn channels(&self) -> usize {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn seek(&mut self, frame: u64) -> Result<(), MediaError> {
        self.pos = frame.min(self.frames);
        self.dirty = true;
        Ok(())
    }

    fn read(&mut self, out: &mut [f32]) -> Result<usize, MediaError> {
        let left = usize::try_from(self.frames - self.pos).unwrap_or(usize::MAX);
        let want = (out.len() / self.channels).min(left).min(CHUNK_FRAMES);
        if want == 0 {
            return Ok(0);
        }
        if self.dirty {
            self.sync()?;
        }
        let fb = self.frame_bytes();
        self.scratch.resize(want * fb, 0);
        let got = match read_full(&mut *self.inner, &mut self.scratch) {
            Ok(got) => got,
            Err(e) => {
                self.dirty = true;
                return Err(io_err(&e));
            }
        };
        let n = got / fb;
        if got % fb != 0 || n == 0 {
            self.dirty = true;
        }
        if n == 0 {
            // The file is shorter than it was probed.
            return Err(MediaError::Changed);
        }
        let bytes = self.layout.bytes;
        let raw = self.scratch.get(..n * fb).unwrap_or_default();
        let dst = out.get_mut(..n * self.channels).unwrap_or_default();
        for (o, b) in dst.iter_mut().zip(raw.chunks_exact(bytes)) {
            *o = self.layout.sample(b, self.scale);
        }
        self.pos += n as u64;
        Ok(n)
    }
}
