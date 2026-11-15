// SPDX-License-Identifier: GPL-3.0-or-later

//! An Audio cue's playback plan: its trims, slice markers and play counts as the slice table the
//! disk stream and the mixer voice share, started
//! `offset` device frames into the cue's action. Pure: the probe's `MediaInfo` comes in.

use std::num::NonZeroU32;
use std::ops::Range;

use openlab_audio::{MediaInfo, PlanError, Plays, Slice, plan_at_rate};
use openlab_core::{SliceMarker, seconds_to_samples};

/// The parts of an Audio cue the plan depends on.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanSpec<'a> {
    pub start_time: f64,
    pub end_time: Option<f64>,
    pub slices: &'a [SliceMarker],
    pub last_slice_play_count: u32,
}

/// Why a cue has no playable plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanFailure {
    /// The trims, markers or counts do not make a table (`LoopTooShort` included: a looping
    /// slice shorter than `MIN_LOOP_MS`).
    Plan(PlanError),
}

/// The slice table for `spec` on a file probed as `info`, at `rate`, `offset` frames into the
/// action (loops unrolled: an offset inside the third play of a loop starts in that play). An
/// offset at or past the end of a finite action gives an empty table: nothing to play.
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
/// `n` times plays its `n - 1` other passes), else nothing (an empty table). Also returns how far into the action the point
/// is when the cue plays from the top, every slice before it played its count (one looping for
/// ever once): the elapsed time it shows.
pub fn cue_slices_from(
    spec: &PlanSpec<'_>,
    info: &MediaInfo,
    rate: NonZeroU32,
    position: u64,
) -> Result<(Vec<Slice>, u64), PlanFailure> {
    let parts = Parts::of(spec, info, rate);
    let top = parts.plan(parts.range.start, rate)?;
    if position >= parts.range.end {
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
    }
    let position = position.max(parts.range.start);
    let table = parts.plan(position, rate)?;
    Ok((table, elapsed_at(&top, position)))
}

/// A cue's trim range and slice cuts in device frames.
struct Parts {
    range: Range<u64>,
    marks: Vec<u64>,
    plays: Vec<Plays>,
}

impl Parts {
    fn of(spec: &PlanSpec<'_>, info: &MediaInfo, rate: NonZeroU32) -> Self {
        let total = info.frames_at(rate.get());
        let start = seconds_to_samples(spec.start_time, rate).min(total);
        let end = spec
            .end_time
            .map_or(total, |t| seconds_to_samples(t, rate))
            .min(total);
        let mut markers: Vec<(u64, u32)> = spec
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
        plan_at_rate(
            self.range.clone(),
            &self.marks,
            &self.plays,
            start,
            rate.get(),
        )
        .map_err(PlanFailure::Plan)
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

fn to_plays(count: u32) -> Plays {
    if count == 0 {
        Plays::Infinite
    } else {
        Plays::Times(count)
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
            last_slice_play_count: last,
        }
    }

    fn marker(time: f64, play_count: u32) -> SliceMarker {
        SliceMarker { time, play_count }
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
            last_slice_play_count: 1,
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
                last_slice_play_count: 0,
                ..trimmed.clone()
            };
            let (t, _) = cue_slices_from(&looped, &info(1000), rate(), pos).unwrap();
            assert_eq!(t, vec![slice(0, 100, 500, Plays::Infinite)], "{pos}");
            // Played three times: two passes are left.
            let thrice = PlanSpec {
                last_slice_play_count: 3,
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
                last_slice_play_count: 0,
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
}
