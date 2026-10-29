// SPDX-License-Identifier: GPL-3.0-or-later

//! Playback plans: trims, slices and loops (AUD-11, AUD-12) as a slice table in device frames.
//! The same table drives the disk stream (which unrolls loops into the ring) and the mixer voice
//! (which counts frames per slice to find loop and slice boundaries), so both agree exactly.

use std::fmt;
use std::ops::Range;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::stream::Reservation;

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
    /// A slice that plays more than once is shorter than [`min_loop_frames`] (M3.17; only
    /// [`plan_at_rate`] and [`check_loop_lengths`] check this).
    LoopTooShort { slice: u32, frames: u64, min: u64 },
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PlanError::Range => "trim range is empty",
            PlanError::Marks => "slice marks must increase strictly inside the trims",
            PlanError::Plays => "one play count per slice, each at least 1",
            PlanError::Start => "start offset outside the trims",
            PlanError::TooManySlices => "too many slices",
            PlanError::LoopTooShort { .. } => "a looping slice is too short",
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
    if !(plays.is_empty() || plays.len() == marks.len() + 1) || plays.contains(&Plays::Times(0)) {
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

/// Shortest slice that may play more than once, in milliseconds of device time (M3.17). Every
/// play of a loop is one `LoopReached` event and one boundary for the voice, so a loop of a few
/// frames would flood the event queue (1024 entries) and keep the mixer at its boundaries.
/// At 50 ms, 128 looping voices raise at most about 13 loop events per 5 ms control tick. The
/// same floor as core's `MIN_SLICE_SECONDS`: QLab 5 accepts no region under 0.05 s.
pub const MIN_LOOP_MS: u32 = 50;

/// [`MIN_LOOP_MS`] in device frames at `device_rate`, less one frame of slack, at least 1. The
/// slack is because a slice's length is the difference of two times each rounded to a frame, so
/// a slice of exactly 50 ms may come out one frame short (2204 frames at 44.1 kHz) and must
/// still play.
pub fn min_loop_frames(device_rate: u32) -> u64 {
    (u64::from(device_rate) * u64::from(MIN_LOOP_MS) / 1000)
        .saturating_sub(1)
        .max(1)
}

/// Checks that every entry that plays a slice again (more than one play, or the rest of a slice
/// split by a start offset) is at least [`min_loop_frames`] long.
pub fn check_loop_lengths(slices: &[Slice], device_rate: u32) -> Result<(), PlanError> {
    let min = min_loop_frames(device_rate);
    let mut prev: Option<u32> = None;
    for s in slices {
        let repeats = s.plays != Plays::Times(1) || prev == Some(s.index);
        if repeats && s.frames < min {
            return Err(PlanError::LoopTooShort {
                slice: s.index,
                frames: s.frames,
                min,
            });
        }
        prev = Some(s.index);
    }
    Ok(())
}

/// [`plan`] for a voice at `device_rate`, also refusing loops shorter than [`MIN_LOOP_MS`]
/// ([`PlanError::LoopTooShort`]). The engine builds its tables with this.
pub fn plan_at_rate(
    range: Range<u64>,
    marks: &[u64],
    plays: &[Plays],
    start: u64,
    device_rate: u32,
) -> Result<Vec<Slice>, PlanError> {
    let slices = plan(range, marks, plays, start)?;
    check_loop_lengths(&slices, device_rate)?;
    Ok(slices)
}

/// Total frames a table plays; `None` if any entry loops forever.
pub fn total_frames(slices: &[Slice]) -> Option<u64> {
    slices.iter().try_fold(0u64, |acc, s| match s.plays {
        Plays::Times(n) => Some(acc.saturating_add(s.frames.saturating_mul(u64::from(n)))),
        Plays::Infinite => None,
    })
}

const NONE: u64 = u64::MAX;
/// Set in a published cut when the voice plays the target's exit buffer at the boundary.
const USES_EXIT: u64 = 1 << 63;

/// Shared by a mixer voice and the disk stream that feeds it, for devamp (CT-16).
///
/// Cuts are dated in voice frames: frames of the plan played since the voice started (exit
/// buffer frames included, underruns and skipped frames not). Only the disk stream maps them to
/// ring positions (frames written to the voice's ring), so a cut can be made while the voice is
/// still playing an earlier loop exit's buffer.
///
/// The voice publishes the boundary where it will leave the loop (`request_cut`), and whether
/// it will play that loop exit's buffer (`exit`) there. The disk stream decodes each exit buffer
/// before it writes any of that loop to the ring (see docs/audio.md, Loop exits), so normally
/// nothing has to arrive in time. Meanwhile the disk stream stops unrolling the loop, writes the
/// continuation from where the exit buffer ends, and publishes where in the ring that starts
/// (`resume`); the voice skips the stale loop frames in between, which frees the ring for it.
/// The voice publishes a new cut only after it has read the resume for the previous one.
#[derive(Debug)]
pub struct StreamLink {
    cut_at: AtomicU64,
    resume_at: AtomicU64,
    /// The cut `resume_at` answers; written after it.
    resumed_for: AtomicU64,
    /// One slot per slice-table entry: the exit buffer for leaving a loop into that entry.
    exits: Box<[OnceLock<Exit>]>,
}

/// The first frames played after leaving a loop into slice-table entry `target`: the table
/// unrolled from there, interleaved. Its memory stays reserved in the streamer's cache budget
/// until it is freed (with the link, on the control thread).
#[derive(Debug)]
pub(crate) struct Exit {
    pub(crate) frames: usize,
    pub(crate) samples: Box<[f32]>,
    pub(crate) _budget: Reservation,
}

impl Default for StreamLink {
    fn default() -> Self {
        Self::with_entries(0)
    }
}

impl StreamLink {
    /// A link with exit slots for a table of `entries` entries.
    pub(crate) fn with_entries(entries: usize) -> Self {
        StreamLink {
            cut_at: AtomicU64::new(NONE),
            resume_at: AtomicU64::new(NONE),
            resumed_for: AtomicU64::new(NONE),
            exits: (0..entries).map(|_| OnceLock::new()).collect(),
        }
    }

    /// Asks the disk stream to leave the loop at voice frame `at`.
    pub(crate) fn request_cut(&self, at: u64, uses_exit: bool) {
        let v = at.min(USES_EXIT - 2) | if uses_exit { USES_EXIT } else { 0 };
        self.cut_at.store(v, Ordering::Release);
    }

    /// The latest cut: voice frame, and whether the voice plays the exit buffer there.
    pub(crate) fn cut_requested(&self) -> Option<(u64, bool)> {
        let v = self.cut_at.load(Ordering::Acquire);
        (v != NONE).then_some((v & !USES_EXIT, v & USES_EXIT != 0))
    }

    /// Published for the cut at voice frame `cut` after every stale frame before ring position
    /// `at` is in the ring.
    pub(crate) fn resume(&self, cut: u64, at: u64) {
        self.resume_at.store(at, Ordering::Release);
        self.resumed_for.store(cut, Ordering::Release);
    }

    /// Where the continuation after the cut at voice frame `cut` starts in the ring, once known.
    pub(crate) fn resumed(&self, cut: u64) -> Option<u64> {
        (self.resumed_for.load(Ordering::Acquire) == cut)
            .then(|| self.resume_at.load(Ordering::Acquire))
    }

    /// Publishes the exit buffer for leaving into entry `target`, once.
    pub(crate) fn set_exit(&self, target: usize, exit: Exit) {
        if let Some(slot) = self.exits.get(target) {
            let _ = slot.set(exit);
        }
    }

    /// The exit buffer for leaving into entry `target`. Lock- and allocation-free (an index and
    /// an acquire load): safe on the audio thread.
    pub(crate) fn exit(&self, target: usize) -> Option<&Exit> {
        self.exits.get(target)?.get()
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
        let p = plan(
            0..1000,
            &[200, 500],
            &[Plays::Times(1), Plays::Times(3), Plays::Times(1)],
            300,
        )
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
        assert_eq!(
            plan(0..10, &[5], &[Plays::Times(1)], 0),
            Err(PlanError::Plays)
        );
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
        assert_eq!((l.cut_requested(), l.resumed(10)), (None, None));
        assert!(l.exit(0).is_none());
        l.request_cut(10, true);
        assert_eq!(l.cut_requested(), Some((10, true)));
        l.request_cut(11, false);
        assert_eq!(l.cut_requested(), Some((11, false)));
        l.resume(11, 12);
        assert_eq!((l.resumed(10), l.resumed(11)), (None, Some(12)));
        // Absurd positions saturate below the flag instead of reading back as "none".
        l.request_cut(u64::MAX, true);
        assert_eq!(l.cut_requested(), Some((USES_EXIT - 2, true)));
    }

    #[test]
    fn minimum_loop_length_is_enforced_exactly_at_the_edge() {
        assert_eq!(min_loop_frames(48_000), 2399);
        assert_eq!(min_loop_frames(44_100), 2204);
        assert_eq!(min_loop_frames(22_050), 1101); // 1102.5 rounds down
        assert_eq!(min_loop_frames(1), 1);
        let plays = [Plays::Times(1), Plays::Infinite, Plays::Times(1)];
        // Exactly the minimum loops; one frame less is refused, as a typed error.
        assert!(plan_at_rate(0..6_000, &[100, 2_499], &plays, 0, 48_000).is_ok());
        assert_eq!(
            plan_at_rate(0..6_000, &[100, 2_498], &plays, 0, 48_000),
            Err(PlanError::LoopTooShort {
                slice: 1,
                frames: 2_398,
                min: 2_399
            })
        );
        // A 50 ms loop whose two ends rounded to frames came out a frame short still plays.
        assert!(plan_at_rate(0..6_000, &[100, 2_304], &plays, 0, 44_100).is_ok());
        // Slices that play once may be as short as one frame.
        let once = [Plays::Times(1), Plays::Times(1), Plays::Times(1)];
        assert!(plan_at_rate(0..2_000, &[1, 2], &once, 0, 48_000).is_ok());
        assert!(
            plan_at_rate(
                0..2_000,
                &[1, 2],
                &[Plays::Times(1), Plays::Times(2), Plays::Times(1)],
                0,
                48_000
            )
            .is_err()
        );
        // A start offset inside a short slice played twice: its second play is a loop.
        let twice = [Plays::Times(2), Plays::Times(1)];
        assert_eq!(
            plan_at_rate(0..6_000, &[100], &twice, 50, 48_000),
            Err(PlanError::LoopTooShort {
                slice: 0,
                frames: 100,
                min: 2_399
            })
        );
        // plan() itself is unchanged: it accepts a one-frame loop.
        assert!(
            plan(
                0..10,
                &[1, 2],
                &[Plays::Times(1), Plays::Infinite, Plays::Times(1)],
                0
            )
            .is_ok()
        );
        assert!(check_loop_lengths(&[], 48_000).is_ok());
    }
}
