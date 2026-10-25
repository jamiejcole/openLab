// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded pre-check of an MP4 file's `stts` (time-to-sample) tables, run before symphonia
//! parses the file. symphonia 0.6.1 adds the entries' sample counts in `u32` and their
//! count x delta products in `u64` without checks, so a hostile table panics under overflow
//! checks (cargo-fuzz aborts on it). Such tables are rejected as corrupt; everything else is left
//! to symphonia.

use std::io::{Read, Seek, SeekFrom};

use crate::decode::MediaError;

/// Boxes that hold the path moov > trak > mdia > minf > stbl > stts.
const CONTAINERS: [&[u8; 4]; 5] = [b"moov", b"trak", b"mdia", b"minf", b"stbl"];
/// Deepest nesting followed (`stts` sits at depth 5).
const MAX_DEPTH: usize = 6;
/// Boxes looked at in total; real files have a few dozen.
const MAX_BOXES: usize = 4096;
/// `stts` entries looked at in total; real tables are runs of equal durations, a handful long.
const MAX_ENTRIES: u64 = 1 << 22;
/// symphonia's `find_timing_for_sample` computes `sample_num + sample_count`, so the sum of all
/// counts must leave room for that in `u32`.
const MAX_SAMPLES: u64 = (u32::MAX / 2) as u64;
/// Likewise for the running duration sum in `u64`.
const MAX_DURATION: u128 = (u64::MAX / 2) as u128;

fn corrupt(what: &str) -> MediaError {
    MediaError::Corrupt(format!("header: mp4 {what}"))
}

/// Rejects MP4 files whose `stts` tables would overflow symphonia's arithmetic. Leaves `r` at the
/// start. Files that are not MP4, or whose boxes are malformed, pass: symphonia reports those.
pub(crate) fn check_mp4_stts<R: Read + Seek + ?Sized>(r: &mut R) -> Result<(), MediaError> {
    let io = |e: std::io::Error| MediaError::Io(e.to_string());
    let len = r.seek(SeekFrom::End(0)).map_err(io)?;
    r.seek(SeekFrom::Start(0)).map_err(io)?;
    let mut walk = Walk {
        boxes: 0,
        entries: 0,
    };
    let result = walk.level(r, 0, len, 0);
    r.seek(SeekFrom::Start(0)).map_err(io)?;
    result
}

struct Walk {
    boxes: usize,
    entries: u64,
}

impl Walk {
    /// Walks the boxes in `start..end`. Read errors end the level quietly.
    fn level<R: Read + Seek + ?Sized>(
        &mut self,
        r: &mut R,
        start: u64,
        end: u64,
        depth: usize,
    ) -> Result<(), MediaError> {
        let mut pos = start;
        while pos.saturating_add(8) <= end && self.boxes < MAX_BOXES {
            self.boxes += 1;
            let mut head = [0u8; 16];
            if r.seek(SeekFrom::Start(pos)).is_err() || r.read_exact(&mut head[..8]).is_err() {
                return Ok(());
            }
            let kind = [head[4], head[5], head[6], head[7]];
            // Only MP4 files start with `ftyp`; anything else is not looked at.
            if depth == 0 && pos == 0 && &kind != b"ftyp" {
                return Ok(());
            }
            let size32 = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
            let (header, size) = match size32 {
                0 => (8, end - pos),
                1 => {
                    if r.read_exact(&mut head[8..16]).is_err() {
                        return Ok(());
                    }
                    let mut b = [0u8; 8];
                    b.copy_from_slice(&head[8..16]);
                    (16, u64::from_be_bytes(b))
                }
                n => (8, u64::from(n)),
            };
            if size < header {
                return Ok(());
            }
            // A box running past its parent is cut short, as a truncated file is.
            let box_end = pos.saturating_add(size).min(end);
            let body = pos + header;
            if &kind == b"stts" {
                self.stts(r, body, box_end)?;
            } else if depth < MAX_DEPTH && CONTAINERS.iter().any(|c| **c == kind) {
                self.level(r, body, box_end, depth + 1)?;
            }
            pos = box_end;
        }
        Ok(())
    }

    /// Checks one `stts` body: version and flags, entry count, then (count, delta) pairs.
    fn stts<R: Read + Seek + ?Sized>(
        &mut self,
        r: &mut R,
        body: u64,
        end: u64,
    ) -> Result<(), MediaError> {
        if body.saturating_add(8) > end || r.seek(SeekFrom::Start(body + 4)).is_err() {
            return Ok(());
        }
        let mut count = [0u8; 4];
        if r.read_exact(&mut count).is_err() {
            return Ok(());
        }
        // Entries beyond the box are symphonia's read error, not ours.
        let mut left = u64::from(u32::from_be_bytes(count)).min((end - body - 8) / 8);
        let mut samples: u64 = 0;
        let mut duration: u128 = 0;
        let mut buf = [[0u8; 8]; 512];
        while left > 0 {
            let n = left.min(buf.len() as u64);
            self.entries += n;
            if self.entries > MAX_ENTRIES {
                return Err(corrupt("stts has too many entries"));
            }
            let Some(chunk) = buf.get_mut(..n as usize) else {
                return Ok(());
            };
            if r.read_exact(chunk.as_flattened_mut()).is_err() {
                return Ok(());
            }
            for [c0, c1, c2, c3, d0, d1, d2, d3] in chunk.iter().copied() {
                let c = u32::from_be_bytes([c0, c1, c2, c3]);
                let d = u32::from_be_bytes([d0, d1, d2, d3]);
                samples += u64::from(c);
                duration += u128::from(c) * u128::from(d);
                if samples > MAX_SAMPLES || duration > MAX_DURATION {
                    return Err(corrupt("stts sample counts or durations overflow"));
                }
            }
            left -= n;
        }
        Ok(())
    }
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

    fn stts(entries: &[(u32, u32)]) -> Vec<u8> {
        let mut b = vec![0, 0, 0, 0];
        b.extend((entries.len() as u32).to_be_bytes());
        for (c, d) in entries {
            b.extend(c.to_be_bytes());
            b.extend(d.to_be_bytes());
        }
        boxed(b"stts", &b)
    }

    fn file(entries: &[(u32, u32)]) -> Vec<u8> {
        let mut stbl = boxed(b"stbl", &stts(entries));
        for k in [b"minf", b"mdia", b"trak", b"moov"] {
            stbl = boxed(k, &stbl);
        }
        let mut f = boxed(b"ftyp", b"M4A \0\0\0\0");
        f.extend(stbl);
        f
    }

    fn check(bytes: Vec<u8>) -> Result<(), MediaError> {
        check_mp4_stts(&mut Cursor::new(bytes))
    }

    #[test]
    fn sane_tables_pass() {
        assert!(check(file(&[(1000, 1024), (1, 512)])).is_ok());
        assert!(check(file(&[])).is_ok());
    }

    #[test]
    fn overflowing_counts_and_durations_are_corrupt() {
        // Sum of counts overflows u32 (the fuzz finding's shape).
        assert!(check(file(&[(u32::MAX, 1), (u32::MAX, 1)])).is_err());
        // Each fits, the sum leaves no room for symphonia's `sample_num + count`.
        assert!(check(file(&[(u32::MAX - 5, 0)])).is_err());
        // Duration sum overflows u64.
        assert!(
            check(file(&[
                (1 << 30, u32::MAX),
                (1 << 30, u32::MAX),
                (1 << 30, u32::MAX)
            ]))
            .is_err()
        );
    }

    #[test]
    fn non_mp4_and_malformed_boxes_pass_without_looping() {
        assert!(check(b"RIFF....WAVE".to_vec()).is_ok());
        assert!(check(Vec::new()).is_ok());
        // A box claiming more entries than it holds, and a zero-sized-header loop.
        let mut f = file(&[(1, 1)]);
        let n = f.len();
        f[n - 16..n - 12].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(check(f).is_ok());
        let mut f = boxed(b"ftyp", b"M4A ");
        f.extend([0, 0, 0, 4, b'm', b'o', b'o', b'v']);
        assert!(check(f).is_ok());
    }
}
