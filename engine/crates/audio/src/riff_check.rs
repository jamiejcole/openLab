// SPDX-License-Identifier: GPL-3.0-or-later

//! Follows a WAV or AIFF container's chunks the way symphonia 0.6.1's readers do, so that every
//! `fmt ` / `COMM` chunk they would parse is header-checked first (see `check_pcm_header`).
//!
//! Symphonia's chunk walk does not always land where the chunk sizes say: a `fmt ` or `COMM`
//! chunk is parsed without skipping its unread tail, a `MARK` or `COMT` chunk can read past its
//! size, and an odd-sized `LIST` gets two padding bytes. A walk by chunk sizes alone can then
//! see different chunks than symphonia, and a hostile file can hide a header from it. This walk
//! tracks both the reader's byte position and its consumed-length counter as symphonia does.
//! Where symphonia's parse of a chunk would fail it stops (symphonia reads nothing further);
//! where that depends on contents it does not model, it assumes success and walks on, which can
//! only check more. Re-check against symphonia's `ChunksReader` and chunk parsers on upgrade.

use std::io::{self, BufReader, Read, Seek, SeekFrom};

use crate::decode::{ChunkBody, MediaError, reject_bad_header};

/// Most chunks walked in one container before the file is refused: symphonia walks them all,
/// and real files have a handful before the audio data.
pub(crate) const MAX_WALK_CHUNKS: usize = 1024;

/// Header-checks every `fmt ` (WAV) or the first `COMM` (AIFF/AIFC) chunk that symphonia's reader
/// would parse in the container starting at byte `start`. Anything that is not such a container
/// is left alone. The reader's position afterwards is unspecified.
pub(crate) fn check_container<R: Read + Seek + ?Sized>(
    r: &mut R,
    start: u64,
) -> Result<(), MediaError> {
    let mut c = Cursor::new(r, start).ok_or_else(|| MediaError::Io("seek failed".into()))?;
    let Some(head) = c.array::<12>() else {
        return Ok(());
    };
    let [m0, m1, m2, m3, l0, l1, l2, l3, f0, f1, f2, f3] = head;
    let len = [l0, l1, l2, l3];
    match (&[m0, m1, m2, m3], &[f0, f1, f2, f3]) {
        (b"RIFF", b"WAVE") => walk_wav(&mut c, u32::from_le_bytes(len)),
        (b"FORM", b"AIFF") => walk_aiff(&mut c, u32::from_be_bytes(len), false),
        (b"FORM", b"AIFC") => walk_aiff(&mut c, u32::from_be_bytes(len), true),
        _ => Ok(()),
    }
}

fn too_many() -> MediaError {
    MediaError::Corrupt(format!("header: more than {MAX_WALK_CHUNKS} chunks"))
}

/// `WavReader::try_new`: chunks up to `data`; every `fmt ` met is parsed and sized (the overflow).
fn walk_wav<R: Read + Seek + ?Sized>(
    c: &mut Cursor<'_, R>,
    riff_len: u32,
) -> Result<(), MediaError> {
    if riff_len < 4 {
        return Ok(());
    }
    // A length of u32::MAX means "unknown" (streamed WAV).
    let len = (riff_len < u32::MAX).then(|| u64::from(riff_len - 4));
    let mut chunks = Chunks::new(len, false);
    while let Some((tag, size)) = chunks.next(c, &[b"fmt ", b"LIST", b"fact", b"data"])? {
        let body = c.pos;
        let used = match &tag {
            b"fmt " => {
                if size < 16 {
                    return Ok(());
                }
                let Some(fmt) = c.body(false, size) else {
                    return Ok(());
                };
                reject_bad_header(&fmt)?;
                match wav_fmt_read(&fmt, size) {
                    Some(n) => n,
                    None => return Ok(()),
                }
            }
            // The list body is skipped (or read as INFO sub-chunks) and padded to even length;
            // the outer walk then pads again, so an odd size moves two bytes.
            b"LIST" if size >= 4 => u64::from(size) + u64::from(size & 1),
            b"fact" if size == 4 => 4,
            // `data` ends the walk; a malformed LIST or fact ends it with an error.
            _ => return Ok(()),
        };
        if !c.goto(body.saturating_add(used)) {
            return Ok(());
        }
    }
    Ok(())
}

/// Bytes `WaveFormatChunk::parse` reads from a `fmt ` body of `size` bytes, or `None` where the
/// parse fails.
fn wav_fmt_read(fmt: &ChunkBody, size: u32) -> Option<u64> {
    let size = u64::from(size);
    let extra = u64::from(fmt.u16_at(16));
    match fmt.u16_at(0) {
        // PCM and IEEE float: base 16 bytes plus the extension each allowed size implies.
        1 | 3 => matches!(size, 16 | 18 | 40).then_some(size),
        // Extensible: exactly 40, whatever the size says.
        0xFFFE => (size >= 40).then_some(40),
        // ADPCM: 18, then as many bytes as the extension size field says.
        2 | 0x11 => (size >= 20).then_some(18 + extra),
        // A-law / mu-law: likewise, from an 18-byte chunk.
        6 | 7 => (size == 18).then_some(18 + extra),
        _ => None,
    }
}

/// `AiffReader::try_new`: every chunk of the form; the first `COMM` is the format.
fn walk_aiff<R: Read + Seek + ?Sized>(
    c: &mut Cursor<'_, R>,
    riff_len: u32,
    aifc: bool,
) -> Result<(), MediaError> {
    if riff_len < 4 {
        return Ok(());
    }
    let mut chunks = Chunks::new(Some(u64::from(riff_len - 4)), true);
    let known: &[&[u8; 4]] = &[
        b"COMM", b"SSND", b"MARK", b"APPL", b"COMT", b"NAME", b"AUTH", b"(c) ", b"ANNO", b"ID3 ",
    ];
    let mut seen = [false; 3];
    while let Some((tag, size)) = chunks.next(c, known)? {
        let body = c.pos;
        // A second COMM, MARK or COMT is an error in symphonia.
        let once = match &tag {
            b"COMM" => Some(0),
            b"MARK" => Some(1),
            b"COMT" => Some(2),
            _ => None,
        };
        if let Some(slot) = once.and_then(|i| seen.get_mut(i)) {
            if *slot {
                return Ok(());
            }
            *slot = true;
        }
        let read = match &tag {
            b"COMM" => {
                // Read whatever the chunk size says: channels, frames, bits, rate (18 bytes).
                let Some(comm) = c.body(true, 18) else {
                    return Ok(());
                };
                reject_bad_header(&comm)?;
                if aifc {
                    // Compression type, then a pascal string padded to even length.
                    let Some([_, _, _, _, n]) = c.array::<5>() else {
                        return Ok(());
                    };
                    23 + u64::from(n | 1)
                } else {
                    18
                }
            }
            b"MARK" => {
                let Some(n) = c.array::<2>().map(u16::from_be_bytes) else {
                    return Ok(());
                };
                for _ in 0..n {
                    // Id, position, then a pascal string padded to even length.
                    let Some([.., l]) = c.array::<7>() else {
                        return Ok(());
                    };
                    if !c.skip(u64::from(l) + u64::from(l & 1 == 0)) {
                        return Ok(());
                    }
                }
                c.pos - body
            }
            b"COMT" => {
                let Some(n) = c.array::<2>().map(u16::from_be_bytes) else {
                    return Ok(());
                };
                for _ in 0..n {
                    let Some([.., a, b]) = c.array::<8>() else {
                        return Ok(());
                    };
                    if !c.skip(u64::from(u16::from_be_bytes([a, b]))) {
                        return Ok(());
                    }
                }
                // Skipped to the chunk's end if read short of it.
                (c.pos - body).max(u64::from(size))
            }
            b"ID3 " => {
                let Some(h) = c.array::<10>() else {
                    return Ok(());
                };
                if h.get(..3) != Some(&b"ID3"[..]) {
                    return Ok(());
                }
                // The tag is read to its own (syncsafe) length, then the chunk to its end.
                let [.., s0, s1, s2, s3] = h;
                let tag_len = [s0, s1, s2, s3]
                    .iter()
                    .fold(0u64, |acc, &b| (acc << 7) | u64::from(b & 0x7F));
                (10 + tag_len).max(u64::from(size))
            }
            // SSND, APPL and text chunks are read or skipped to exactly their size.
            _ => u64::from(size),
        };
        if !c.goto(body.saturating_add(read)) {
            return Ok(());
        }
    }
    Ok(())
}

/// symphonia's `ChunksReader`: its consumed counter (`consumed`, padded to even before each
/// header) advances by the declared sizes, independently of where the parsers leave the reader.
struct Chunks {
    len: Option<u64>,
    consumed: u64,
    big_endian: bool,
    walked: usize,
}

impl Chunks {
    fn new(len: Option<u64>, big_endian: bool) -> Self {
        Chunks {
            len,
            consumed: 0,
            big_endian,
            walked: 0,
        }
    }

    /// The next chunk with one of the `known` tags, the reader at its body; unknown chunks are
    /// skipped. `Ok(None)` where symphonia's walk ends (the parent's end, or an error).
    fn next<R: Read + Seek + ?Sized>(
        &mut self,
        c: &mut Cursor<'_, R>,
        known: &[&[u8; 4]],
    ) -> Result<Option<([u8; 4], u32)>, MediaError> {
        loop {
            self.walked += 1;
            if self.walked > MAX_WALK_CHUNKS {
                return Err(too_many());
            }
            if self.len.is_some_and(|l| self.consumed >= l) {
                return Ok(None);
            }
            if self.consumed & 1 == 1 {
                if c.array::<1>().is_none() {
                    return Ok(None);
                }
                self.consumed += 1;
            }
            if self.len.is_some_and(|l| self.consumed + 8 > l) {
                return Ok(None);
            }
            let Some([t0, t1, t2, t3, s0, s1, s2, s3]) = c.array::<8>() else {
                return Ok(None);
            };
            let size = if self.big_endian {
                u32::from_be_bytes([s0, s1, s2, s3])
            } else {
                u32::from_le_bytes([s0, s1, s2, s3])
            };
            self.consumed += 8;
            if self
                .len
                .is_some_and(|l| l - self.consumed < u64::from(size))
            {
                return Ok(None);
            }
            self.consumed = self.consumed.saturating_add(u64::from(size));
            let tag = [t0, t1, t2, t3];
            if known.contains(&&tag) {
                return Ok(Some((tag, size)));
            }
            if !c.skip(u64::from(size)) {
                return Ok(None);
            }
        }
    }
}

/// A buffered reader that knows its absolute position. Small reads (marker and comment entries)
/// come from the buffer; skips within it do not touch the source.
struct Cursor<'a, R: ?Sized> {
    inner: BufReader<&'a mut R>,
    pos: u64,
}

impl<'a, R: Read + Seek + ?Sized> Cursor<'a, R> {
    fn new(r: &'a mut R, start: u64) -> Option<Self> {
        r.seek(SeekFrom::Start(start)).ok()?;
        Some(Cursor {
            inner: BufReader::with_capacity(4096, r),
            pos: start,
        })
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        let mut b = [0u8; N];
        self.inner.read_exact(&mut b).ok()?;
        self.pos += N as u64;
        Some(b)
    }

    /// The first 16 bytes of a chunk body (needed), then up to 40 of its `size` (as present).
    fn body(&mut self, big_endian: bool, size: u32) -> Option<ChunkBody> {
        let mut body = [0u8; 40];
        let want = usize::try_from(size).unwrap_or(usize::MAX).min(body.len());
        let need = want.min(16);
        self.inner.read_exact(body.get_mut(..need)?).ok()?;
        let mut len = need;
        while len < want {
            match self.inner.read(body.get_mut(len..want)?) {
                Ok(0) => break,
                Ok(n) => len += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return None,
            }
        }
        self.pos += len as u64;
        Some(ChunkBody::new(big_endian, body, len))
    }

    fn skip(&mut self, n: u64) -> bool {
        let Ok(d) = i64::try_from(n) else {
            return false;
        };
        if self.inner.seek_relative(d).is_err() {
            return false;
        }
        self.pos = self.pos.saturating_add(n);
        true
    }

    /// Moves to absolute position `to` (forwards or back).
    fn goto(&mut self, to: u64) -> bool {
        let (Ok(to_i), Ok(at_i)) = (i64::try_from(to), i64::try_from(self.pos)) else {
            return false;
        };
        if self.inner.seek_relative(to_i - at_i).is_err() {
            return false;
        }
        self.pos = to;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn be_chunk(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = tag.to_vec();
        v.extend_from_slice(&(body.len() as u32).to_be_bytes());
        v.extend_from_slice(body);
        v
    }

    fn aiff(chunks: &[u8], form_len: u32) -> Vec<u8> {
        let mut v = b"FORM".to_vec();
        v.extend_from_slice(&form_len.to_be_bytes());
        v.extend_from_slice(b"AIFF");
        v.extend_from_slice(chunks);
        v
    }

    fn bad_comm() -> Vec<u8> {
        let mut body = vec![0x80, 0x01, 0, 0, 0, 0, 0, 24, 0x40, 0x0E, 0xBB, 0x80];
        body.resize(18, 0);
        be_chunk(b"COMM", &body)
    }

    fn check(bytes: Vec<u8>) -> Result<(), MediaError> {
        check_container(&mut io::Cursor::new(bytes), 0)
    }

    /// An `ID3 ` chunk is read to the tag's own length when that is longer than the chunk: the
    /// walk follows it there (where the sizes alone see a chunk past the end of the file).
    #[test]
    fn an_id3_chunk_is_followed_to_its_tags_length() {
        let mut id3 = b"ID3 \0\0\0\x0aID3\x04\0\0\0\0\0\x08".to_vec();
        id3.extend_from_slice(b"JUNK\x7f\xff\xff\xff");
        let chunks = [id3, bad_comm()].concat();
        let len = 4 + 18 + 26;
        let r = check(aiff(&chunks, len));
        assert!(
            matches!(&r, Err(MediaError::Corrupt(m)) if m.starts_with("header:")),
            "{r:?}"
        );
        // A chunk that is not an ID3 tag ends symphonia's walk (it fails to parse).
        let mut not_id3 = b"ID3 \0\0\0\x0aXYZ\x04\0\0\0\0\0\x08".to_vec();
        not_id3.extend_from_slice(b"JUNK\x7f\xff\xff\xff");
        assert_eq!(check(aiff(&[not_id3, bad_comm()].concat(), len)), Ok(()));
    }

    /// Only the first `COMM` is parsed (a second is an error), and chunks past the form's length
    /// are not read.
    #[test]
    fn the_walk_stops_where_symphonias_does() {
        let mut good = vec![0, 2, 0, 0, 0, 0, 0, 16, 0x40, 0x0E, 0xBB, 0x80];
        good.resize(18, 0);
        let chunks = [be_chunk(b"COMM", &good), bad_comm()].concat();
        assert_eq!(check(aiff(&chunks, 4 + 52)), Ok(()));
        let chunks = [be_chunk(b"SSND", &[0; 8]), bad_comm()].concat();
        assert_eq!(check(aiff(&chunks, 4 + 16)), Ok(()));
        assert!(check(aiff(&chunks, 4 + 16 + 26)).is_err());
    }

    /// Hostile sizes and counts end quickly: a `MARK` claiming 65535 markers in a short file, and
    /// chunk sizes of 4 GiB.
    #[test]
    fn hostile_counts_and_sizes_end_quickly() {
        let mark = b"MARK\0\0\0\x02\xff\xff".to_vec();
        assert_eq!(check(aiff(&mark, u32::MAX)), Ok(()));
        let mut wav = b"RIFF\xff\xff\xff\xffWAVE".to_vec();
        wav.extend_from_slice(b"junk\xff\xff\xff\xfffmt \x10\0\0\0");
        assert_eq!(check(wav), Ok(()));
        assert_eq!(check(b"OggS".to_vec()), Ok(()));
        assert_eq!(check(Vec::new()), Ok(()));
    }
}
