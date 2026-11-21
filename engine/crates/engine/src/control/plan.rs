// SPDX-License-Identifier: GPL-3.0-or-later

//! An Audio cue's playback plan: its trims, slice markers and play counts as the slice table the
//! disk stream and the mixer voice share, started
//! `offset` device frames into the cue's action. Pure: the probe's `MediaInfo` comes in.

use std::num::NonZeroU32;
use std::ops::Range;

use openlab_audio::{MediaInfo, PlanError, Plays, Slice, plan_at_rate};
use openlab_core::{PlayCount, SliceMarker, SliceProblem, seconds_to_samples};

/// The parts of an Audio cue the plan depends on.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanSpec<'a> {
    pub start_time: f64,
    pub end_time: Option<f64>,
    pub slices: &'a [SliceMarker],
    pub last_slice_play_count: PlayCount,
}

/// A movie of `seconds` (a Video cue's file, from its probe) as media at `rate`, so its trims,
/// slices and loops are planned exactly as an Audio cue's on the engine's clock.
pub fn picture_info(seconds: f64, rate: NonZeroU32) -> MediaInfo {
    let frames = seconds_to_samples(seconds, rate);
    MediaInfo {
        channels: 1,
        sample_rate: rate.get(),
        frames,
        duration: std::time::Duration::try_from_secs_f64(seconds).unwrap_or_default(),
    }
}

/// Why a cue has no playable plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanFailure {
    /// The trims, markers or counts do not make a table (`LoopTooShort` included: a looping
    /// slice shorter than `MIN_LOOP_MS`).
    Plan(PlanError),
    /// Every slice inside the trims is skipped: no part of the file plays.
    NothingPlays,
}

impl PlanFailure {
    /// What the cue engine is told about a cue whose plan fails so.
    pub fn problem(self) -> SliceProblem {
        match self {
            Self::Plan(e) => SliceProblem::Unplayable {
                reason: e.to_string(),
            },
            Self::NothingPlays => SliceProblem::NothingPlays,
        }
    }
}

impl std::fmt::Display for PlanFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plan(e) => e.fmt(f),
            Self::NothingPlays => f.write_str("every slice is skipped"),
        }
    }
}

/// The slice table for `spec` on a file probed as `info`, at `rate`, `offset` frames into the
/// action (loops unrolled: an offset inside the third play of a loop starts in that play). An
/// offset at or past the end of a finite action gives an empty table: nothing to play. A skipped
/// slice has no entry: the entry before it is followed by the next slice that plays, and the
/// action (so `offset`) counts only what plays.
pub fn cue_slices(
    spec: &PlanSpec<'_>,
    info: &MediaInfo,
    rate: NonZeroU32,
    offset: u64,
) -> Result<Vec<Slice>, PlanFailure> {
    let parts = Parts::of(spec, info, rate);
    let table = parts.plan(parts.range.start, rate)?;
    Ok(skip(&table, offset))
}

/// The slice table for `spec` starting at `position`, a frame of the file at `rate` (a seek):
/// the slices as they play from there, so the rest of a looping slice still loops, the rest of
/// a slice played `n` times plays once and then its `n - 1` plays, and a point past a slice that
/// loops for ever is reached. A point before the in point starts there. One at or past the out
/// point stands at the out point, the end of a pass of the last slice: what follows is that
/// slice's next pass if it has one (one looping for ever loops on from its start, one played
/// `n` times plays its `n - 1` other passes), else nothing (an empty table). A point inside a
/// skipped slice starts at the next slice that plays; with none after it, it stands at the end
/// of the last slice that plays, which is the out point of such a cue. Also returns how far into
/// the action the point is when the cue plays from the top, every slice before it played its
/// count (one looping for ever once, a skipped one not at all): the elapsed time it shows.
pub fn cue_slices_from(
    spec: &PlanSpec<'_>,
    info: &MediaInfo,
    rate: NonZeroU32,
    position: u64,
) -> Result<(Vec<Slice>, u64), PlanFailure> {
    let parts = Parts::of(spec, info, rate);
    let top = parts.plan(parts.range.start, rate)?;
    // From the top no slice is split, so `top` is the slices that play, in file order: the
    // first that ends after the point is the one the point is in, or the next after a skipped
    // one.
    let from = top
        .iter()
        .find(|s| s.start.saturating_add(s.frames) > position)
        .map(|s| position.max(s.start));
    let Some(position) = from else {
        let rest = top.last().and_then(|last| {
            let plays = match last.plays {
                Plays::Infinite => Plays::Infinite,
                Plays::Times(n @ 2..) => Plays::Times(n - 1),
                Plays::Times(_) => return None,
            };
            Some(Slice { plays, ..*last })
        });
        // It stands at the end of the last slice's first pass, as a point inside a slice is in
        // its first pass: the passes left are still to come.
        let shown = top.last().map_or(0, |last| {
            total_once(&top)
                .saturating_sub(once(last))
                .saturating_add(last.frames)
        });
        return Ok((rest.into_iter().collect(), shown));
    };
    let table = parts.plan(position, rate)?;
    Ok((table, elapsed_at(&top, position)))
}

/// A cue's trim range and slice cuts in device frames.
struct Parts {
    range: Range<u64>,
    marks: Vec<u64>,
    /// One per slice: how it plays, `None` for a skipped slice.
    plays: Vec<Option<Plays>>,
}

impl Parts {
    fn of(spec: &PlanSpec<'_>, info: &MediaInfo, rate: NonZeroU32) -> Self {
        let total = info.frames_at(rate.get());
        let start = seconds_to_samples(spec.start_time, rate).min(total);
        let end = spec
            .end_time
            .map_or(total, |t| seconds_to_samples(t, rate))
            .min(total);
        let mut markers: Vec<(u64, PlayCount)> = spec
            .slices
            .iter()
            .map(|m| (seconds_to_samples(m.time, rate), m.play_count))
            .collect();
        markers.sort_by_key(|&(at, _)| at);
        let mut marks = Vec::new();
        let mut plays = Vec::new();
        for &(at, count) in &markers {
            // Markers outside the trims cut nothing; a repeated marker would make an empty slice.
            if at <= start || at >= end || marks.last() == Some(&at) {
                continue;
            }
            marks.push(at);
            plays.push(to_plays(count));
        }
        // The slice the out point falls in plays as the marker that ends it says.
        let last = markers
            .iter()
            .find(|&&(at, _)| at >= end && at > start)
            .map_or(spec.last_slice_play_count, |&(_, count)| count);
        plays.push(to_plays(last));
        Parts {
            range: start..end,
            marks,
            plays,
        }
    }

    fn plan(&self, start: u64, rate: NonZeroU32) -> Result<Vec<Slice>, PlanFailure> {
        if self.plays.iter().all(Option::is_none) {
            return Err(PlanFailure::NothingPlays);
        }
        // A skipped slice is planned as one played once (any length may be, so the checks
        // judge only what plays) and its entries are then taken out: the entries around it
        // keep their own frames and slice numbers, and the stream seeks from one to the next.
        let once: Vec<Plays> = (self.plays.iter())
            .map(|p| p.unwrap_or(Plays::Times(1)))
            .collect();
        let mut table = plan_at_rate(self.range.clone(), &self.marks, &once, start, rate.get())
            .map_err(PlanFailure::Plan)?;
        table.retain(|s| {
            usize::try_from(s.index)
                .ok()
                .and_then(|i| self.plays.get(i))
                .is_some_and(Option::is_some)
        });
        Ok(table)
    }
}

/// Frames of `table` (from the top) before it first reaches file frame `position`, every entry
/// before it played its count and one looping for ever played once.
fn elapsed_at(table: &[Slice], position: u64) -> u64 {
    let mut acc = 0u64;
    for s in table {
        if (s.start..s.start.saturating_add(s.frames)).contains(&position) {
            return acc.saturating_add(position - s.start);
        }
        acc = acc.saturating_add(once(s));
    }
    acc
}

/// Frames `table` plays with each entry looping for ever played once.
fn total_once(table: &[Slice]) -> u64 {
    table
        .iter()
        .fold(0u64, |acc, s| acc.saturating_add(once(s)))
}

fn once(s: &Slice) -> u64 {
    match s.plays {
        Plays::Times(n) => s.frames.saturating_mul(u64::from(n)),
        Plays::Infinite => s.frames,
    }
}

/// How a slice with `count` plays; `None` for a skipped one.
fn to_plays(count: PlayCount) -> Option<Plays> {
    match count {
        PlayCount::Times(n) => Some(Plays::Times(n.get())),
        PlayCount::Endless => Some(Plays::Infinite),
        PlayCount::Skip => None,
    }
}

/// The table after its first `offset` frames have played, loops unrolled.
pub fn skip(table: &[Slice], offset: u64) -> Vec<Slice> {
    let mut left = offset;
    let mut out = Vec::with_capacity(table.len() + 1);
    for s in table {
        if left == 0 || s.frames == 0 {
            out.push(*s);
            continue;
        }
        if let Plays::Times(n) = s.plays {
            let whole = s.frames.saturating_mul(u64::from(n));
            if left >= whole {
                left -= whole;
                continue;
            }
        }
        let done = left / s.frames;
        let within = left % s.frames;
        left = 0;
        let mut used = done;
        if within > 0 {
            out.push(Slice {
                index: s.index,
                start: s.start + within,
                frames: s.frames - within,
                plays: Plays::Times(1),
            });
            used += 1;
        }
        match s.plays {
            Plays::Infinite => out.push(Slice {
                plays: Plays::Infinite,
                ..*s
            }),
            Plays::Times(n) => {
                let rest = u64::from(n).saturating_sub(used);
                if let Ok(rest @ 1..) = u32::try_from(rest) {
                    out.push(Slice {
                        plays: Plays::Times(rest),
                        ..*s
                    });
                }
            }
        }
    }
    if left > 0 {
        // Past the end of a finite action.
        out.clear();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlab_audio::total_frames;
    use std::time::Duration;

    const RATE: u32 = 1000;

    fn rate() -> NonZeroU32 {
        NonZeroU32::new(RATE).unwrap()
    }

    fn info(frames: u64) -> MediaInfo {
        MediaInfo {
            channels: 1,
            sample_rate: RATE,
            frames,
            duration: Duration::from_millis(frames),
        }
    }

    fn spec(markers: &[SliceMarker], last: u32) -> PlanSpec<'_> {
        PlanSpec {
            start_time: 0.0,
            end_time: None,
            slices: markers,
            last_slice_play_count: last.into(),
        }
    }

    fn marker(time: f64, play_count: u32) -> SliceMarker {
        SliceMarker {
            time,
            play_count: play_count.into(),
        }
    }

    /// Unrolls a finite table into file frames, for comparing positions.
    fn unroll(table: &[Slice]) -> Vec<u64> {
        let mut v = Vec::new();
        for s in table {
            let Plays::Times(n) = s.plays else {
                panic!("infinite")
            };
            for _ in 0..n {
                v.extend(s.start..s.start + s.frames);
            }
        }
        v
    }

    #[test]
    fn plain_cue_plays_its_trims_once() {
        let s = PlanSpec {
            start_time: 0.1,
            end_time: Some(0.5),
            slices: &[],
            last_slice_play_count: 1.into(),
        };
        let t = cue_slices(&s, &info(1000), rate(), 0).unwrap();
        assert_eq!(
            t,
            vec![Slice {
                index: 0,
                start: 100,
                frames: 400,
                plays: Plays::Times(1)
            }]
        );
    }

    #[test]
    fn end_past_the_file_is_clamped() {
        let s = PlanSpec {
            end_time: Some(9.0),
            ..spec(&[], 1)
        };
        assert_eq!(
            total_frames(&cue_slices(&s, &info(300), rate(), 0).unwrap()),
            Some(300)
        );
    }

    #[test]
    fn markers_cut_slices_and_count_plays() {
        let m = [marker(0.2, 2), marker(0.5, 0)];
        let t = cue_slices(&spec(&m, 1), &info(1000), rate(), 0).unwrap();
        assert_eq!(t.len(), 3);
        assert_eq!(t[0].plays, Plays::Times(2));
        assert_eq!(t[1].plays, Plays::Infinite);
        assert_eq!(t[2].plays, Plays::Times(1));
        assert_eq!(total_frames(&t), None);
    }

    #[test]
    fn markers_outside_the_trims_are_ignored_and_the_out_slice_keeps_its_count() {
        let m = [marker(0.05, 3), marker(0.6, 4)];
        let s = PlanSpec {
            start_time: 0.1,
            end_time: Some(0.5),
            ..spec(&m, 1)
        };
        let t = cue_slices(&s, &info(1000), rate(), 0).unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].plays, Plays::Times(4));
        assert_eq!((t[0].start, t[0].frames), (100, 400));
    }

    #[test]
    fn a_too_short_loop_is_refused() {
        // The 50 ms minimum is 49 frames at 1 kHz (one frame of slack).
        let m = [marker(0.005, 2)];
        let err = cue_slices(&spec(&m, 1), &info(1000), rate(), 0).unwrap_err();
        assert!(matches!(
            err,
            PlanFailure::Plan(PlanError::LoopTooShort { .. })
        ));
    }

    #[test]
    fn empty_trims_are_refused() {
        let s = PlanSpec {
            start_time: 0.5,
            end_time: Some(0.5),
            ..spec(&[], 1)
        };
        assert_eq!(
            cue_slices(&s, &info(1000), rate(), 0),
            Err(PlanFailure::Plan(PlanError::Range))
        );
    }

    #[test]
    fn skip_matches_unrolled_positions() {
        let table = vec![
            Slice {
                index: 0,
                start: 0,
                frames: 10,
                plays: Plays::Times(1),
            },
            Slice {
                index: 1,
                start: 10,
                frames: 7,
                plays: Plays::Times(3),
            },
            Slice {
                index: 2,
                start: 17,
                frames: 5,
                plays: Plays::Times(1),
            },
        ];
        let all = unroll(&table);
        for offset in 0..=all.len() as u64 + 3 {
            let skipped = skip(&table, offset);
            let want: Vec<u64> = all.iter().copied().skip(offset as usize).collect();
            assert_eq!(unroll(&skipped), want, "offset {offset}");
        }
    }

    fn slice(index: u32, start: u64, frames: u64, plays: Plays) -> Slice {
        Slice {
            index,
            start,
            frames,
            plays,
        }
    }

    /// A seek's table: the rest of the slice it lands in (looping on if it loops, its other
    /// plays after this one), then the slices after it; past a loop too; and how far into the
    /// action the point is from the top.
    #[test]
    fn a_table_from_a_point_of_the_file_plays_the_slices_from_there() {
        // 0..200 twice, 200..500 until devamped, 500..1000 once.
        let m = [marker(0.2, 2), marker(0.5, 0)];
        let s = spec(&m, 1);
        let from = |pos| cue_slices_from(&s, &info(1000), rate(), pos).unwrap();
        // Inside the slice played twice: the rest of this pass, then the other pass.
        let (t, shown) = from(50);
        assert_eq!(t[0], slice(0, 50, 150, Plays::Times(1)));
        assert_eq!(t[1], slice(0, 0, 200, Plays::Times(1)));
        assert_eq!(t[2].plays, Plays::Infinite);
        assert_eq!(shown, 50);
        // Wherever the point is in a finite cue, the time shown there and what is left make
        // the cue's length: 0..200 three times, then 200..1000 twice is 2200.
        let m3 = [marker(0.2, 3)];
        let finite = spec(&m3, 2);
        for pos in [0, 50, 199, 200, 599, 999, 1000, 5000] {
            let (t, shown) = cue_slices_from(&finite, &info(1000), rate(), pos).unwrap();
            assert_eq!(shown + total_frames(&t).unwrap(), 2200, "{pos}");
        }
        // Inside the loop: from the point, then the loop again for ever.
        let (t, shown) = from(300);
        assert_eq!(t[0], slice(1, 300, 200, Plays::Times(1)));
        assert_eq!(t[1], slice(1, 200, 300, Plays::Infinite));
        assert_eq!(shown, 400 + 100);
        // Past the loop: reached, the loop never heard.
        let (t, shown) = from(700);
        assert_eq!(t, vec![slice(2, 700, 300, Plays::Times(1))]);
        assert_eq!(total_frames(&t), Some(300));
        assert_eq!(shown, 400 + 300 + 200);
        // Before the in point: the in point. At or past the out point: the end of a pass of
        // the last slice, so what follows is its next pass, if any.
        let trimmed = PlanSpec {
            start_time: 0.1,
            end_time: Some(0.6),
            ..spec(&[], 1)
        };
        let (t, shown) = cue_slices_from(&trimmed, &info(1000), rate(), 20).unwrap();
        assert_eq!(t, vec![slice(0, 100, 500, Plays::Times(1))]);
        assert_eq!(shown, 0);
        for pos in [600, 601, u64::MAX] {
            let (t, shown) = cue_slices_from(&trimmed, &info(1000), rate(), pos).unwrap();
            assert!(t.is_empty(), "{pos}");
            assert_eq!(shown, 500);
            // Looping until devamped: it loops on from the last slice's start.
            let looped = PlanSpec {
                last_slice_play_count: 0.into(),
                ..trimmed.clone()
            };
            let (t, _) = cue_slices_from(&looped, &info(1000), rate(), pos).unwrap();
            assert_eq!(t, vec![slice(0, 100, 500, Plays::Infinite)], "{pos}");
            // Played three times: two passes are left.
            let thrice = PlanSpec {
                last_slice_play_count: 3.into(),
                ..trimmed.clone()
            };
            let (t, shown) = cue_slices_from(&thrice, &info(1000), rate(), pos).unwrap();
            assert_eq!(t, vec![slice(0, 100, 500, Plays::Times(2))], "{pos}");
            // At the end of the first pass: shown and what is left make the cue's length.
            assert_eq!(shown, 500, "{pos}");
            assert_eq!(shown + total_frames(&t).unwrap(), 1500);
            // Just before it, the same sum.
            let (t, shown) = cue_slices_from(&thrice, &info(1000), rate(), 599).unwrap();
            assert_eq!(shown + total_frames(&t).unwrap(), 1500);
            // With markers, only the last slice plays on.
            let m = [marker(0.3, 2)];
            let sliced = PlanSpec {
                slices: &m,
                last_slice_play_count: 0.into(),
                ..trimmed.clone()
            };
            let (t, _) = cue_slices_from(&sliced, &info(1000), rate(), pos).unwrap();
            assert_eq!(t, vec![slice(1, 300, 300, Plays::Infinite)], "{pos}");
        }
        // A plan the cue cannot play is refused as from the top.
        let short = [marker(0.005, 2)];
        assert!(cue_slices_from(&spec(&short, 1), &info(1000), rate(), 500).is_err());
    }

    #[test]
    fn skip_into_an_infinite_loop_stays_in_it() {
        let table = vec![
            Slice {
                index: 0,
                start: 0,
                frames: 10,
                plays: Plays::Times(1),
            },
            Slice {
                index: 1,
                start: 10,
                frames: 20,
                plays: Plays::Infinite,
            },
        ];
        let t = skip(&table, 10 + 45);
        assert_eq!(
            t,
            vec![
                Slice {
                    index: 1,
                    start: 15,
                    frames: 15,
                    plays: Plays::Times(1)
                },
                Slice {
                    index: 1,
                    start: 10,
                    frames: 20,
                    plays: Plays::Infinite
                },
            ]
        );
    }

    /// A spec over a 1000-frame file cut at 200, 500 and 800, the four slices played as `counts`.
    fn counted(counts: [PlayCount; 4]) -> Vec<SliceMarker> {
        [0.2, 0.5, 0.8]
            .into_iter()
            .zip(counts)
            .map(|(time, play_count)| SliceMarker { time, play_count })
            .collect()
    }

    fn with_last(markers: &[SliceMarker], last: PlayCount) -> PlanSpec<'_> {
        PlanSpec {
            last_slice_play_count: last,
            ..spec(markers, 1)
        }
    }

    const ONCE: PlayCount = PlayCount::ONCE;
    const SKIP: PlayCount = PlayCount::Skip;

    #[test]
    fn a_skipped_slice_has_no_entry_and_its_neighbours_keep_their_frames_and_numbers() {
        // The second and third slices skipped (two in a row), the others once.
        let m = counted([ONCE, SKIP, SKIP, ONCE]);
        let s = with_last(&m, ONCE);
        let t = cue_slices(&s, &info(1000), rate(), 0).unwrap();
        assert_eq!(
            t,
            vec![
                slice(0, 0, 200, Plays::Times(1)),
                slice(3, 800, 200, Plays::Times(1))
            ]
        );
        assert_eq!(total_frames(&t), Some(400));
        // An offset counts only what plays: 250 frames in is 50 into the last slice.
        let t = cue_slices(&s, &info(1000), rate(), 250).unwrap();
        assert_eq!(t, vec![slice(3, 850, 150, Plays::Times(1))]);
        assert!(cue_slices(&s, &info(1000), rate(), 400).unwrap().is_empty());
    }

    #[test]
    fn the_first_and_the_last_slice_can_be_skipped() {
        let m = counted([SKIP, PlayCount::from_stored(2), ONCE, ONCE]);
        let t = cue_slices(&with_last(&m, SKIP), &info(1000), rate(), 0).unwrap();
        assert_eq!(
            t,
            vec![
                slice(1, 200, 300, Plays::Times(2)),
                slice(2, 500, 300, Plays::Times(1))
            ]
        );
        // A skipped slice may be shorter than a loop may be: it is not played at all.
        let short = [
            SliceMarker {
                time: 0.5,
                play_count: PlayCount::from_stored(2),
            },
            SliceMarker {
                time: 0.51,
                play_count: SKIP,
            },
        ];
        let t = cue_slices(&with_last(&short, ONCE), &info(1000), rate(), 0).unwrap();
        assert_eq!(
            t,
            vec![
                slice(0, 0, 500, Plays::Times(2)),
                slice(2, 510, 490, Plays::Times(1))
            ]
        );
    }

    #[test]
    fn every_slice_skipped_is_its_own_failure() {
        let m = counted([SKIP, SKIP, SKIP, SKIP]);
        let all = with_last(&m, SKIP);
        assert_eq!(
            cue_slices(&all, &info(1000), rate(), 0),
            Err(PlanFailure::NothingPlays)
        );
        assert_eq!(
            cue_slices_from(&all, &info(1000), rate(), 300),
            Err(PlanFailure::NothingPlays)
        );
        assert_eq!(
            PlanFailure::NothingPlays.problem(),
            SliceProblem::NothingPlays
        );
        // A cue without markers whose one slice is skipped, too.
        assert_eq!(
            cue_slices(&with_last(&[], SKIP), &info(1000), rate(), 0),
            Err(PlanFailure::NothingPlays)
        );
        // A count on a slice the trims leave out does not make the cue play: only the slices
        // inside the trims count.
        let m = counted([ONCE, SKIP, SKIP, SKIP]);
        let trimmed = PlanSpec {
            start_time: 0.2,
            ..with_last(&m, SKIP)
        };
        assert_eq!(
            cue_slices(&trimmed, &info(1000), rate(), 0),
            Err(PlanFailure::NothingPlays)
        );
        // One slice that plays is enough.
        let m = counted([SKIP, SKIP, SKIP, SKIP]);
        let t = cue_slices(&with_last(&m, ONCE), &info(1000), rate(), 0).unwrap();
        assert_eq!(t, vec![slice(3, 800, 200, Plays::Times(1))]);
    }

    #[test]
    fn a_seek_into_a_skipped_slice_starts_at_the_next_slice_that_plays() {
        // 0..200 once, 200..500 skipped, 500..800 until devamped, 800..1000 skipped.
        let m = counted([ONCE, SKIP, PlayCount::Endless, SKIP]);
        let s = with_last(&m, SKIP);
        for pos in [200, 350, 499, 500] {
            let (t, shown) = cue_slices_from(&s, &info(1000), rate(), pos).unwrap();
            assert_eq!(t, vec![slice(2, 500, 300, Plays::Infinite)], "{pos}");
            assert_eq!(shown, 200, "{pos}: the skipped slice takes no time");
        }
        // Inside the loop: the rest of the pass, then the loop.
        let (t, shown) = cue_slices_from(&s, &info(1000), rate(), 600).unwrap();
        assert_eq!(
            t,
            vec![
                slice(2, 600, 200, Plays::Times(1)),
                slice(2, 500, 300, Plays::Infinite)
            ]
        );
        assert_eq!(shown, 300);
        // In the skipped slice at the end: the end of the last slice that plays, which loops on.
        for pos in [800, 900, 1000, 5000] {
            let (t, shown) = cue_slices_from(&s, &info(1000), rate(), pos).unwrap();
            assert_eq!(t, vec![slice(2, 500, 300, Plays::Infinite)], "{pos}");
            assert_eq!(shown, 500, "{pos}");
        }
        // Before a skipped first slice: the first slice that plays; after a last slice that
        // played its count: nothing.
        let m = counted([SKIP, ONCE, SKIP, SKIP]);
        let s = with_last(&m, SKIP);
        let (t, shown) = cue_slices_from(&s, &info(1000), rate(), 0).unwrap();
        assert_eq!((t, shown), (vec![slice(1, 200, 300, Plays::Times(1))], 0));
        let (t, shown) = cue_slices_from(&s, &info(1000), rate(), 700).unwrap();
        assert_eq!((t, shown), (vec![], 300));
    }
}
