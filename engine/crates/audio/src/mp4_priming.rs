// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded scan of an MP4 file for the AAC encoder's priming (the frames of encoder delay at the
//! start of the stream), which symphonia 0.6.1 does not apply: it ignores the edit list (`elst`
//! media time) and iTunes' gapless tag (`iTunSMPB` in moov > udta > meta > ilst). The scan only
//! reads; the decoder decides whether the value is believable (see `Decoder::open`). It never
//! allocates from a file-supplied length and follows the limits of `mp4_check`.

use std::io::{Read, Seek, SeekFrom};

use crate::mp4_check::box_header;

/// Boxes that are descended into (`meta` is a full box, handled in `level`).
const CONTAINERS: [&[u8; 4]; 8] = [
    b"moov", b"trak", b"mdia", b"edts", b"udta", b"meta", b"ilst", b"----",
];
/// Deepest nesting followed (moov > udta > meta > ilst > ---- > data is depth 5).
const MAX_DEPTH: usize = 6;
/// Boxes looked at in total; real files have a few dozen.
const MAX_BOXES: usize = 4096;
/// Tracks recorded; a real file has a handful.
const MAX_TRACKS: usize = 64;
/// Longest `iTunSMPB` text read: real ones are 100 bytes (`" 00000000 00000840 ..."`).
const SMPB_MAX: usize = 256;
/// Media rate 1.0 in 16.16 fixed point.
const RATE_ONE: u32 = 0x0001_0000;

/// What a track's edit list says about the start of its media.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edit {
    /// No `elst` (or an empty one).
    Absent,
    /// One entry at normal speed starting at `media_time` (media timescale units).
    Single(u64),
    /// Several entries, a negative media time (an empty edit), a rate other than 1, a zero
    /// timescale: not understood.
    Odd,
}

#[derive(Clone, Copy, Debug)]
struct TrackEdit {
    id: u32,
    timescale: u32,
    edit: Edit,
}

/// The `iTunSMPB` tag's priming field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Smpb {
    Absent,
    Frames(u64),
    /// Present but unparsable.
    Bad,
}

/// The gapless information found in an MP4 file.
#[derive(Debug)]
pub(crate) struct Mp4Gapless {
    tracks: Vec<TrackEdit>,
    smpb: Smpb,
}

/// The priming of a track, as far as the file says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Priming {
    /// The file says nothing (or says zero): nothing to trim.
    None,
    /// Frames to skip at the stream's start, at the track's rate.
    Frames(u64),
    /// The file has gapless information that cannot be used (damaged or unsupported).
    Ignored,
}

impl Mp4Gapless {
    /// Priming for track `id` whose frames run at `rate`. The edit list wins; the `iTunSMPB`
    /// tag is the fallback, also when the edit list is not understood.
    pub(crate) fn priming(&self, id: u32, rate: u32) -> Priming {
        let track = self.tracks.iter().find(|t| t.id == id);
        let mut edit = track.map_or(Edit::Absent, |t| t.edit);
        // A zero timescale (`mdhd` missing or damaged) cannot place the media time.
        if edit != Edit::Absent && track.is_none_or(|t| t.timescale == 0) {
            edit = Edit::Odd;
        }
        match (edit, track) {
            (Edit::Single(t), Some(track)) => {
                let (num, den) = (
                    u128::from(t) * u128::from(rate),
                    u128::from(track.timescale),
                );
                match u64::try_from((2 * num + den) / (2 * den)) {
                    Ok(0) => Priming::None,
                    Ok(f) => Priming::Frames(f),
                    Err(_) => Priming::Ignored,
                }
            }
            (Edit::Absent, _) | (Edit::Odd, _) | (Edit::Single(_), None) => {
                let from_edit_list = edit == Edit::Odd;
                match self.smpb {
                    Smpb::Frames(0) => Priming::None,
                    Smpb::Frames(f) => Priming::Frames(f),
                    Smpb::Bad => Priming::Ignored,
                    Smpb::Absent if from_edit_list => Priming::Ignored,
                    Smpb::Absent => Priming::None,
                }
            }
        }
    }
}

/// Scans `r` (from the start; left at the start). Anything that is not understood is left out:
/// the result then says nothing, and the file plays as symphonia decodes it.
pub(crate) fn scan_mp4_gapless<R: Read + Seek + ?Sized>(r: &mut R) -> Mp4Gapless {
    let mut scan = Scan {
        boxes: 0,
        tracks: Vec::new(),
        smpb: Smpb::Absent,
        name_is_smpb: false,
    };
    if let Ok(len) = r.seek(SeekFrom::End(0)) {
        scan.level(r, 0, len, 0, *b"    ");
    }
    // Best effort: a failed seek here surfaces as symphonia's own read error.
    let _ = r.seek(SeekFrom::Start(0));
    Mp4Gapless {
        tracks: scan.tracks,
        smpb: scan.smpb,
    }
}

struct Scan {
    boxes: usize,
    tracks: Vec<TrackEdit>,
    smpb: Smpb,
    /// The `----` atom being read named itself `iTunSMPB`.
    name_is_smpb: bool,
}

fn read_at<R: Read + Seek + ?Sized>(r: &mut R, pos: u64, buf: &mut [u8]) -> bool {
    r.seek(SeekFrom::Start(pos)).is_ok() && r.read_exact(buf).is_ok()
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([
        *s.first()?,
        *s.get(1)?,
        *s.get(2)?,
        *s.get(3)?,
    ]))
}

fn be64(b: &[u8], at: usize) -> Option<u64> {
    Some((u64::from(be32(b, at)?) << 32) | u64::from(be32(b, at.checked_add(4)?)?))
}

impl Scan {
    fn level<R: Read + Seek + ?Sized>(
        &mut self,
        r: &mut R,
        start: u64,
        end: u64,
        depth: usize,
        parent: [u8; 4],
    ) {
        let mut pos = start;
        while pos.saturating_add(8) <= end && self.boxes < MAX_BOXES {
            self.boxes += 1;
            let Some((kind, header, box_end)) = box_header(r, pos, end) else {
                return;
            };
            if depth == 0 && pos == 0 && &kind != b"ftyp" {
                return;
            }
            let body = pos + header;
            match (&parent, &kind) {
                (b"trak", b"tkhd") => self.tkhd(r, body, box_end),
                (b"mdia", b"mdhd") => self.mdhd(r, body, box_end),
                (b"edts", b"elst") => self.elst(r, body, box_end),
                (b"----", b"name") => self.name(r, body, box_end),
                (b"----", b"data") => self.data(r, body, box_end),
                _ => {}
            }
            if depth < MAX_DEPTH && CONTAINERS.iter().any(|c| **c == kind) {
                if &kind == b"trak" {
                    if self.tracks.len() >= MAX_TRACKS {
                        pos = box_end;
                        continue;
                    }
                    self.tracks.push(TrackEdit {
                        id: 0,
                        timescale: 0,
                        edit: Edit::Absent,
                    });
                }
                // `meta` is a full box in MP4 and iTunes files (4 bytes of version and flags
                // before its children) but a plain one in old QuickTime files: the first child
                // there is `hdlr` right away.
                let mut inner = body;
                if &kind == b"meta" {
                    let mut t = [0u8; 4];
                    let plain = read_at(r, body + 4, &mut t) && &t == b"hdlr";
                    inner = if plain { body } else { body.saturating_add(4) };
                }
                self.level(r, inner, box_end, depth + 1, kind);
            }
            pos = box_end;
        }
    }

    fn tkhd<R: Read + Seek + ?Sized>(&mut self, r: &mut R, body: u64, end: u64) {
        let mut b = [0u8; 24];
        if body.saturating_add(b.len() as u64) > end || !read_at(r, body, &mut b) {
            return;
        }
        // Version 0: times are 32-bit, version 1: 64-bit; the track id follows them.
        let at = if b.first() == Some(&1) { 20 } else { 12 };
        let (Some(id), Some(t)) = (be32(&b, at), self.tracks.last_mut()) else {
            return;
        };
        t.id = id;
    }

    fn mdhd<R: Read + Seek + ?Sized>(&mut self, r: &mut R, body: u64, end: u64) {
        let mut b = [0u8; 24];
        if body.saturating_add(b.len() as u64) > end || !read_at(r, body, &mut b) {
            return;
        }
        let at = if b.first() == Some(&1) { 20 } else { 12 };
        let (Some(ts), Some(t)) = (be32(&b, at), self.tracks.last_mut()) else {
            return;
        };
        t.timescale = ts;
    }

    fn elst<R: Read + Seek + ?Sized>(&mut self, r: &mut R, body: u64, end: u64) {
        let mut b = [0u8; 28];
        let avail = usize::try_from(end.saturating_sub(body)).unwrap_or(usize::MAX);
        let size = avail.min(28);
        let Some(head) = b.get_mut(..size) else {
            return;
        };
        if head.len() < 8 || !read_at(r, body, head) {
            return;
        }
        let version = b.first().copied().unwrap_or(0);
        let (count, entry) = (be32(&b, 4), 8usize);
        let edit = match count {
            Some(0) => Edit::Absent,
            Some(1) => {
                // Version 1: u64 duration, i64 media time; version 0: 32-bit each. Then the
                // 16.16 media rate.
                let (time, rate) = if version == 1 {
                    (be64(&b, entry + 8), be32(&b, entry + 16))
                } else {
                    (
                        be32(&b, entry + 4).map(|t| i64::from(t as i32) as u64),
                        be32(&b, entry + 8),
                    )
                };
                match (time, rate) {
                    (Some(t), Some(RATE_ONE)) if (t as i64) >= 0 => Edit::Single(t),
                    _ => Edit::Odd,
                }
            }
            _ => Edit::Odd,
        };
        if let Some(t) = self.tracks.last_mut() {
            t.edit = edit;
        }
    }

    fn name<R: Read + Seek + ?Sized>(&mut self, r: &mut R, body: u64, end: u64) {
        // Version and flags, then the name.
        let mut b = [0u8; 12];
        let len = end.saturating_sub(body);
        self.name_is_smpb =
            len == 12 && read_at(r, body, &mut b) && b.get(4..) == Some(b"iTunSMPB".as_slice());
    }

    fn data<R: Read + Seek + ?Sized>(&mut self, r: &mut R, body: u64, end: u64) {
        if !self.name_is_smpb || self.smpb != Smpb::Absent {
            return;
        }
        // Type and locale (8 bytes), then the text.
        let len = end.saturating_sub(body).saturating_sub(8);
        let n = usize::try_from(len).unwrap_or(usize::MAX).min(SMPB_MAX);
        let mut buf = [0u8; SMPB_MAX];
        let ok = buf
            .get_mut(..n)
            .is_some_and(|text| read_at(r, body + 8, text));
        let priming = buf.get(..n).filter(|_| ok).and_then(parse_smpb);
        self.smpb = priming.map_or(Smpb::Bad, Smpb::Frames);
    }
}

/// The second field of `" 00000000 00000840 000001CA 000000000000AC6C ..."`: encoder delay in
/// hex.
fn parse_smpb(text: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(text).ok()?;
    let field = text.split_ascii_whitespace().nth(1)?;
    if field.len() > 8 {
        return None;
    }
    u64::from_str_radix(field, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    /// A `trak` with the given id, media timescale and edit list entries (version 0).
    fn trak(id: u32, timescale: u32, edits: &[(i32, u32)]) -> Vec<u8> {
        let mut tkhd = vec![0u8; 12];
        tkhd.extend(id.to_be_bytes());
        tkhd.extend([0u8; 8]);
        let mut mdhd = vec![0u8; 12];
        mdhd.extend(timescale.to_be_bytes());
        mdhd.extend([0u8; 8]);
        let mut elst = vec![0, 0, 0, 0];
        elst.extend((edits.len() as u32).to_be_bytes());
        for (time, rate) in edits {
            elst.extend(0u32.to_be_bytes());
            elst.extend(time.to_be_bytes());
            elst.extend(rate.to_be_bytes());
        }
        let mut body = boxed(b"tkhd", &tkhd);
        body.extend(boxed(b"edts", &boxed(b"elst", &elst)));
        body.extend(boxed(b"mdia", &boxed(b"mdhd", &mdhd)));
        boxed(b"trak", &body)
    }

    fn file(traks: &[Vec<u8>]) -> Vec<u8> {
        let mut f = boxed(b"ftyp", b"M4A \0\0\0\0");
        f.extend(boxed(b"moov", &traks.concat()));
        f
    }

    fn priming(bytes: Vec<u8>, id: u32, rate: u32) -> Priming {
        scan_mp4_gapless(&mut Cursor::new(bytes)).priming(id, rate)
    }

    const ONE: u32 = 0x0001_0000;

    #[test]
    fn media_time_is_scaled_from_the_media_timescale_to_the_rate() {
        let f = || file(&[trak(1, 44_100, &[(2_112, ONE)])]);
        assert_eq!(priming(f(), 1, 44_100), Priming::Frames(2_112));
        // A track at 48 kHz played back at 44.1 kHz would be odd, but the maths is exact.
        assert_eq!(priming(f(), 1, 22_050), Priming::Frames(1_056));
        let f = file(&[trak(1, 90_000, &[(4_500, ONE)])]);
        assert_eq!(priming(f, 1, 44_100), Priming::Frames(2_205));
    }

    #[test]
    fn the_edit_list_of_the_asked_track_is_used() {
        let f = || {
            file(&[
                trak(1, 44_100, &[(1_024, ONE)]),
                trak(2, 44_100, &[(2_112, ONE)]),
            ])
        };
        assert_eq!(priming(f(), 2, 44_100), Priming::Frames(2_112));
        assert_eq!(priming(f(), 1, 44_100), Priming::Frames(1_024));
        // A track the file does not describe has no priming.
        assert_eq!(priming(f(), 9, 44_100), Priming::None);
    }

    #[test]
    fn unusable_edit_lists_are_ignored() {
        for edits in [
            vec![(-1, ONE)],
            vec![(1_024, 2 * ONE)],
            vec![(1_024, ONE), (0, ONE)],
        ] {
            let f = file(&[trak(1, 44_100, &edits)]);
            assert_eq!(priming(f, 1, 44_100), Priming::Ignored, "{edits:?}");
        }
        // No timescale to place the media time in.
        let f = file(&[trak(1, 0, &[(1_024, ONE)])]);
        assert_eq!(priming(f, 1, 44_100), Priming::Ignored);
        // Nothing, or an empty list, says nothing.
        assert_eq!(
            priming(file(&[trak(1, 44_100, &[])]), 1, 44_100),
            Priming::None
        );
        assert_eq!(priming(file(&[]), 1, 44_100), Priming::None);
        assert_eq!(priming(Vec::new(), 1, 44_100), Priming::None);
        assert_eq!(priming(b"RIFF....WAVE".to_vec(), 1, 44_100), Priming::None);
    }

    #[test]
    fn version_1_boxes_are_read() {
        let mut tkhd = vec![1, 0, 0, 0];
        tkhd.extend([0u8; 16]);
        tkhd.extend(7u32.to_be_bytes());
        let mut mdhd = vec![1, 0, 0, 0];
        mdhd.extend([0u8; 16]);
        mdhd.extend(44_100u32.to_be_bytes());
        let mut elst = vec![1, 0, 0, 0, 0, 0, 0, 1];
        elst.extend(0u64.to_be_bytes());
        elst.extend(1_105u64.to_be_bytes());
        elst.extend(ONE.to_be_bytes());
        let mut body = boxed(b"tkhd", &tkhd);
        body.extend(boxed(b"edts", &boxed(b"elst", &elst)));
        body.extend(boxed(b"mdia", &boxed(b"mdhd", &mdhd)));
        let f = file(&[boxed(b"trak", &body)]);
        assert_eq!(priming(f, 7, 44_100), Priming::Frames(1_105));
    }

    #[test]
    fn smpb_text_is_parsed_strictly() {
        assert_eq!(parse_smpb(b" 00000000 00000840 000001CA 0000"), Some(0x840));
        assert_eq!(parse_smpb(b"00000000 0 1"), Some(0));
        for bad in [
            &b""[..],
            b" 00000000",
            b" 0 xyz",
            b" 0 123456789",
            b" 0 -1",
            &[0xff, 0xfe],
        ] {
            assert_eq!(parse_smpb(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_scan_is_bounded_by_boxes_and_depth() {
        // More boxes than the limit: returns, having read at most MAX_BOXES of them.
        let mut f = boxed(b"ftyp", b"M4A \0\0\0\0");
        let mut moov = Vec::new();
        for _ in 0..(MAX_BOXES * 2) {
            moov.extend(boxed(b"free", &[]));
        }
        moov.extend(trak(1, 44_100, &[(1_024, ONE)]));
        f.extend(boxed(b"moov", &moov));
        assert_eq!(priming(f, 1, 44_100), Priming::None);
        // Deeper than the limit: not followed.
        let mut deep = trak(1, 44_100, &[(1_024, ONE)]);
        for _ in 0..(MAX_DEPTH + 2) {
            deep = boxed(b"moov", &deep);
        }
        let mut f = boxed(b"ftyp", b"M4A \0\0\0\0");
        f.extend(deep);
        assert_eq!(priming(f, 1, 44_100), Priming::None);
    }
}
