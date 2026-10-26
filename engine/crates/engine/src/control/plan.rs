// SPDX-License-Identifier: GPL-3.0-or-later

//! An Audio cue's playback plan: its trims, slice markers and play counts as the slice table the
//! disk stream and the mixer voice share (docs/audio.md "Slices, loops and devamp"), started
//! `offset` device frames into the cue's action. Pure: the probe's `MediaInfo` comes in.

use std::num::NonZeroU32;

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
    /// slice shorter than `MIN_LOOP_MS`, M3.17).
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
    let table =
        plan_at_rate(start..end, &marks, &plays, start, rate.get()).map_err(PlanFailure::Plan)?;
    Ok(skip(&table, offset))
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
        // 10 ms minimum = 10 frames at 1 kHz.
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
