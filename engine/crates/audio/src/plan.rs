// SPDX-License-Identifier: GPL-3.0-or-later

//! Playback plans: trims, slices and loops (AUD-11, AUD-12) as a slice table in device frames.
//! The same table drives the disk stream (which unrolls loops into the ring) and the mixer voice
//! (which counts frames per slice to find loop and slice boundaries), so both agree exactly.

use std::fmt;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

/// Longest slice table a voice accepts. Bounds the per-boundary work on the audio thread.
pub const MAX_SLICES: usize = 256;

/// How many times a slice plays in a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plays {
    /// 1 = play once (no loop). 0 is invalid.
    Times(u32),
    /// Loop until devamped.
    Infinite,
}

/// One entry of a slice table. Every play of the entry is file frames `start..start + frames` at
/// the device rate. `index` is the user's slice number: a slice that starts part way through
/// (a start offset) is split into a one-play entry and an entry for its remaining plays, both
/// with the same index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slice {
    pub index: u32,
    pub start: u64,
    pub frames: u64,
    pub plays: Plays,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanError {
    /// Empty or inverted trim range.
    Range,
    /// Slice marks not strictly increasing inside the trim range.
    Marks,
    /// `plays` has the wrong length or a `Times(0)`.
    Plays,
    /// The start offset is outside the trim range.
    Start,
    /// More than [`MAX_SLICES`] entries.
    TooManySlices,
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PlanError::Range => "trim range is empty",
            PlanError::Marks => "slice marks must increase strictly inside the trims",
            PlanError::Plays => "one play count per slice, each at least 1",
            PlanError::Start => "start offset outside the trims",
            PlanError::TooManySlices => "too many slices",
        })
    }
}

impl std::error::Error for PlanError {}

/// Builds the slice table for playing `range` (in/out points, device frames) cut at `marks`, with
/// `plays[i]` for slice `i` (`marks.len() + 1` entries; empty = every slice once), starting at
/// frame `start` inside `range`.
pub fn plan(
    range: Range<u64>,
    marks: &[u64],
    plays: &[Plays],
    start: u64,
) -> Result<Vec<Slice>, PlanError> {
    if range.start >= range.end {
        return Err(PlanError::Range);
    }
    let mut prev = range.start;
    for &m in marks {
        if m <= prev || m >= range.end {
            return Err(PlanError::Marks);
        }
        prev = m;
    }
    if !(plays.is_empty() || plays.len() == marks.len() + 1)
        || plays.contains(&Plays::Times(0))
    {
        return Err(PlanError::Plays);
    }
    if !range.contains(&start) {
        return Err(PlanError::Start);
    }
    let bounds: Vec<u64> = std::iter::once(range.start)
        .chain(marks.iter().copied())
        .chain(std::iter::once(range.end))
        .collect();
    let mut out = Vec::new();
    for (i, w) in bounds.windows(2).enumerate() {
        let (&[s, e], Ok(index)) = (w, u32::try_from(i)) else {
            return Err(PlanError::TooManySlices);
        };
        if e <= start {
            continue;
        }
        let p = plays.get(i).copied().unwrap_or(Plays::Times(1));
        if s < start {
            out.push(Slice {
                index,
                start,
                frames: e - start,
                plays: Plays::Times(1),
            });
            let rest = match p {
                Plays::Times(n) if n > 1 => Some(Plays::Times(n - 1)),
                Plays::Times(_) => None,
                Plays::Infinite => Some(Plays::Infinite),
            };
            if let Some(plays) = rest {
                out.push(Slice {
                    index,
                    start: s,
                    frames: e - s,
                    plays,
                });
            }
        } else {
            out.push(Slice {
                index,
                start: s,
                frames: e - s,
                plays: p,
            });
        }
    }
    if out.len() > MAX_SLICES {
        return Err(PlanError::TooManySlices);
    }
    Ok(out)
}

/// Total frames a table plays; `None` if any entry loops forever.
pub fn total_frames(slices: &[Slice]) -> Option<u64> {
    slices.iter().try_fold(0u64, |acc, s| match s.plays {
        Plays::Times(n) => Some(acc.saturating_add(s.frames.saturating_mul(u64::from(n)))),
        Plays::Infinite => None,
    })
}

const NONE: u64 = u64::MAX;

/// Shared by a mixer voice and the disk stream that feeds it, for devamp (CT-16). Positions are
/// ring frame indices: frames written to (or consumed from) the voice's ring since it started.
///
/// The voice publishes the boundary where it will leave the loop (`request_cut`). The disk stream
/// stops unrolling there, writes the continuation, and publishes where in the ring the
/// continuation starts (`resume`). The voice skips the stale loop frames in between.
#[derive(Debug)]
pub struct StreamLink {
    cut_at: AtomicU64,
    resume_at: AtomicU64,
}

impl Default for StreamLink {
    fn default() -> Self {
        StreamLink {
            cut_at: AtomicU64::new(NONE),
            resume_at: AtomicU64::new(NONE),
        }
    }
}

impl StreamLink {
    pub(crate) fn request_cut(&self, at: u64) {
        self.cut_at.store(at, Ordering::Release);
    }

    pub(crate) fn cut_requested(&self) -> Option<u64> {
        Some(self.cut_at.load(Ordering::Acquire)).filter(|&v| v != NONE)
    }

    /// Published after every stale frame before `at` is in the ring.
    pub(crate) fn resume(&self, at: u64) {
        self.resume_at.store(at, Ordering::Release);
    }

    pub(crate) fn resumed_at(&self) -> Option<u64> {
        Some(self.resume_at.load(Ordering::Acquire)).filter(|&v| v != NONE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(index: u32, start: u64, frames: u64, plays: Plays) -> Slice {
        Slice {
            index,
            start,
            frames,
            plays,
        }
    }

    #[test]
    fn trims_only_is_one_slice() {
        assert_eq!(
            plan(100..400, &[], &[], 100),
            Ok(vec![s(0, 100, 300, Plays::Times(1))])
        );
    }

    #[test]
    fn slices_with_loops() {
        let p = plan(
            0..1000,
            &[200, 500],
            &[Plays::Times(1), Plays::Times(3), Plays::Infinite],
            0,
        )
        .unwrap();
        assert_eq!(
            p,
            vec![
                s(0, 0, 200, Plays::Times(1)),
                s(1, 200, 300, Plays::Times(3)),
                s(2, 500, 500, Plays::Infinite),
            ]
        );
        assert_eq!(total_frames(&p), None);
        assert_eq!(total_frames(&p[..2]), Some(1100));
    }

    #[test]
    fn start_inside_a_looping_slice_splits_it() {
        let p = plan(0..1000, &[200, 500], &[Plays::Times(1), Plays::Times(3), Plays::Times(1)], 300)
            .unwrap();
        assert_eq!(
            p,
            vec![
                s(1, 300, 200, Plays::Times(1)),
                s(1, 200, 300, Plays::Times(2)),
                s(2, 500, 500, Plays::Times(1)),
            ]
        );
        let p = plan(0..1000, &[500], &[Plays::Infinite, Plays::Times(1)], 100).unwrap();
        assert_eq!(p[1], s(0, 0, 500, Plays::Infinite));
        let p = plan(0..1000, &[500], &[Plays::Times(1), Plays::Times(1)], 100).unwrap();
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(plan(5..5, &[], &[], 5), Err(PlanError::Range));
        assert_eq!(plan(0..10, &[0], &[], 0), Err(PlanError::Marks));
        assert_eq!(plan(0..10, &[5, 5], &[], 0), Err(PlanError::Marks));
        assert_eq!(plan(0..10, &[10], &[], 0), Err(PlanError::Marks));
        assert_eq!(plan(0..10, &[5], &[Plays::Times(1)], 0), Err(PlanError::Plays));
        assert_eq!(
            plan(0..10, &[5], &[Plays::Times(0), Plays::Times(1)], 0),
            Err(PlanError::Plays)
        );
        assert_eq!(plan(0..10, &[], &[], 10), Err(PlanError::Start));
        let marks: Vec<u64> = (1..=MAX_SLICES as u64).collect();
        assert_eq!(
            plan(0..10_000, &marks, &[], 0),
            Err(PlanError::TooManySlices)
        );
    }

    #[test]
    fn link_starts_empty() {
        let l = StreamLink::default();
        assert_eq!((l.cut_requested(), l.resumed_at()), (None, None));
        l.request_cut(10);
        l.resume(12);
        assert_eq!((l.cut_requested(), l.resumed_at()), (Some(10), Some(12)));
    }
}
