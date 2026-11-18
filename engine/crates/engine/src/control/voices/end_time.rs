// SPDX-License-Identifier: GPL-3.0-or-later

//! An end time edited while its cue plays (AUD-11, VID-07): the cue's voices end where the new
//! end is, at once if they are past it, and the cue engine is told the action's new length.
//! Only the end follows the edit: a voice keeps the start time, slice markers and play counts
//! it was planned with until its cue starts again, and an end that falls in a slice that loops
//! (or past a marker the voice's table does not have) is heard from the next start too, as its
//! disk stream has written those loops ahead.

use openlab_core::{PlayCount, SliceMarker};

use super::*;

/// What a slice table was planned from: its cue's time and loops then and its file's probe,
/// so an end time edited since can be planned against what the table plays.
#[derive(Clone, Debug, PartialEq)]
pub struct Planned {
    start_time: f64,
    end_time: Option<f64>,
    slices: Vec<SliceMarker>,
    last: PlayCount,
    info: openlab_audio::MediaInfo,
    /// An end edited since dropped entries a stream may already have written: the last entry
    /// can no longer grow.
    sealed: bool,
}

impl Planned {
    pub fn of(spec: &PlanSpec<'_>, info: openlab_audio::MediaInfo) -> Self {
        Planned {
            start_time: spec.start_time,
            end_time: spec.end_time,
            slices: spec.slices.to_vec(),
            last: spec.last_slice_play_count,
            info,
            sealed: false,
        }
    }

    pub fn end_time(&self) -> Option<f64> {
        self.end_time
    }

    /// The cue's last slice once its end time is `end`, the rest as planned: the last entry of
    /// its plan from the top at `rate`. `None` when that cannot be planned (a slice the new end
    /// leaves too short to loop) or nothing plays.
    pub fn last_slice(&self, end: Option<f64>, rate: NonZeroU32) -> Option<Slice> {
        let spec = PlanSpec {
            start_time: self.start_time,
            end_time: end,
            slices: &self.slices,
            last_slice_play_count: self.last,
        };
        cue_slices(&spec, &self.info, rate, 0).ok()?.last().copied()
    }

    /// `last` (planned at `from`) in frames at `to`, inside the file, as a table planned at
    /// `from` is rescaled for a voice at `to`.
    pub fn at_rate(&self, last: Slice, from: u32, to: u32) -> Option<Slice> {
        openlab_audio::rescale(&[last], from, to, self.info.frames_at(to))
            .first()
            .copied()
    }

    /// Whether a table planned from this may grow its last entry.
    pub fn grows(&self) -> bool {
        !self.sealed
    }

    /// A new end time `end` was carried out on a table planned from this, as `cut`.
    pub fn ended(&mut self, end: Option<f64>, cut: NewEnd, entries: usize) {
        self.end_time = end;
        self.sealed |= cut.seals(entries);
    }
}

/// A new end for a slice table: entry `entry` becomes its last, `frames` long; `frames` 0 means
/// nothing more of it plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NewEnd {
    pub entry: usize,
    pub frames: u64,
}

impl NewEnd {
    /// It drops entries of a table `entries` long (or all of it): what a stream wrote of them
    /// can never be played, so the last entry must not grow again.
    pub fn seals(self, entries: usize) -> bool {
        self.frames == 0 || self.entry + 1 < entries
    }

    /// Cuts `table` (the table it was worked out for) to the new end.
    pub fn cut(self, table: &mut Vec<Slice>) {
        if self.frames == 0 {
            table.clear();
            return;
        }
        table.truncate(self.entry + 1);
        if let Some(e) = table.get_mut(self.entry) {
            e.frames = self.frames;
        }
    }
}

/// How `table`, a slice table planned from a cue's time and loops (from any point of its
/// action), ends once the cue's last slice is `last` (its plan from the top with the new end
/// time; both in frames at one rate). `None` when it ends there already, or cannot follow: the
/// slice the new end falls in loops (the table's passes of it are written ahead), the end moved
/// past a marker (the slices it adds are not in the table), or the table would grow and `grow`
/// is false. A table wholly past the new end gets `frames` 0.
pub fn new_end(table: &[Slice], last: Slice, grow: bool) -> Option<NewEnd> {
    if last.plays != Plays::Times(1) || table.is_empty() {
        return None;
    }
    let end = last.start.saturating_add(last.frames);
    let Some(k) = table.iter().position(|s| s.index == last.index) else {
        // Started past the new end's slice: nothing more of it plays.
        let past = table.iter().all(|s| s.index > last.index);
        return past.then_some(NewEnd {
            entry: 0,
            frames: 0,
        });
    };
    let s = table.get(k)?;
    if s.plays != Plays::Times(1) || s.start < last.start {
        return None;
    }
    let frames = end.saturating_sub(s.start);
    if frames == 0 {
        // Only a table that starts part way into the slice (its first entry) can start at or
        // after the new end: nothing of it plays.
        return (k == 0).then_some(NewEnd {
            entry: 0,
            frames: 0,
        });
    }
    let final_entry = k + 1 == table.len();
    if frames > s.frames && !(final_entry && grow) {
        return None;
    }
    if final_entry && frames == s.frames {
        return None;
    }
    Some(NewEnd { entry: k, frames })
}

impl VoiceRec {
    /// What its table was planned from, for a voice not started yet its prepared stream's.
    pub(super) fn planned_now(&self) -> Option<&Planned> {
        match &self.phase {
            Phase::Waiting {
                prepared: Some(p), ..
            } => p.planned.as_deref().or(self.planned.as_deref()),
            _ => self.planned.as_deref(),
        }
    }

    /// The action's length, in the core's count, once the voice plays its table from sample
    /// `t` on: where it is then plus what is left of the table (after its devamp cuts).
    /// `None` while what is left loops for ever.
    fn length_from(&self, t: SampleTime) -> Option<u64> {
        let pos = self.position(t.max(self.start_at))?;
        let played = pos.saturating_sub(self.offset);
        let left = self.scale.local(played);
        let rest = match rest_of_table(&self.table, &self.cuts, self.devamp_stop, left) {
            Some(rest) => total_frames(&rest)?,
            None => 0,
        };
        let rest = self
            .scale
            .master(left.saturating_add(rest))
            .saturating_sub(self.scale.master(left));
        Some(pos.saturating_add(rest).saturating_add(self.delay))
    }
}

impl Voices {
    /// `cue`'s content was edited: each of its voices planned with another end time than the
    /// cue has now ends at the new one, if its table can follow ([`new_end`]); the others keep
    /// their plan until the cue starts again.
    pub fn follow_end_time(&mut self, cue: CueId, env: &mut Env<'_>, fx: &mut Effects) {
        let mut ids: Vec<VoiceId> = (self.recs.iter())
            .filter(|(_, r)| r.cue == cue)
            .map(|(id, _)| *id)
            .collect();
        if ids.is_empty() {
            return;
        }
        ids.sort_by_key(|id| id.0);
        let Some(end) = cue_of(env, cue)
            .and_then(|c| plan_spec(&c.kind))
            .map(|s| s.end_time)
        else {
            return;
        };
        for id in ids {
            self.follow_end(id, end, env, fx);
        }
    }

    fn follow_end(&mut self, id: VoiceId, end: Option<f64>, env: &Env<'_>, fx: &mut Effects) {
        let master = env.rate;
        let Some(r) = self.recs.get_mut(&id) else {
            return;
        };
        let Some(planned) = r.planned_now().filter(|p| p.end_time != end).cloned() else {
            return;
        };
        let Some(last) = planned.last_slice(end, master) else {
            return;
        };
        let at_rate = |rate: u32| planned.at_rate(last, master.get(), rate);
        let grow = planned.grows();
        match &mut r.phase {
            Phase::Playing => {
                let Some(cut) =
                    at_rate(r.scale.local_rate()).and_then(|last| new_end(&r.table, last, grow))
                else {
                    return;
                };
                // Not before its start or its latest resume: the mixer knows it by this id
                // from then.
                let at = env.now.max(r.start_at).max(r.resumed_at.unwrap_or(0));
                fx.commands.push((
                    at,
                    CommandKind::EndAt {
                        voice: id,
                        entry: cut.entry,
                        frames: cut.frames,
                    },
                ));
                if let Some(h) = &r.stream {
                    h.set_end(cut.entry, cut.frames);
                }
                if let Some(p) = r.planned.as_mut() {
                    p.ended(end, cut, r.table.len());
                }
                cut.cut(&mut r.table);
                r.total = total_frames(&r.table).map(|t| r.scale.master(t));
                if !r.preview
                    && !r.retired
                    && let Some(frames) = r.length_from(at)
                {
                    fx.feedback.push(Input::ActionLength {
                        cue: r.cue,
                        run: r.run,
                        frames,
                    });
                }
            }
            Phase::Waiting {
                prepared, paused, ..
            } => {
                if let Some(p) = prepared.as_mut()
                    && let Some(cut) = at_rate(p.scale.local_rate())
                        .and_then(|last| new_end(&p.stream.slices, last, grow))
                {
                    if cut.frames == 0 {
                        // Nothing of it plays: its start finds nothing to play.
                        *prepared = None;
                    } else {
                        p.stream.handle.set_end(cut.entry, cut.frames);
                        // A stream made from a table handed on keeps its record's plan.
                        let of = match p.planned.as_deref_mut() {
                            Some(pl) => Some(pl),
                            None => r.planned.as_deref_mut(),
                        };
                        if let Some(pl) = of {
                            pl.ended(end, cut, p.stream.slices.len());
                        }
                        cut.cut(&mut p.stream.slices);
                        p.total = total_frames(&p.stream.slices).map(|t| p.scale.master(t));
                    }
                }
                // A table handed on (a move, a Video cue's seek) is planned again from it.
                if let Some((table, rate)) = r.rest.as_mut()
                    && let Some(cut) = at_rate(*rate).and_then(|last| new_end(table, last, true))
                {
                    if let Some(p) = r.planned.as_mut() {
                        p.ended(end, cut, table.len());
                    }
                    cut.cut(table);
                    // A move to another device told the core its length when it was made.
                    if r.relocated
                        && r.replaces.is_some()
                        && !*paused
                        && !r.preview
                        && let Some(rest) = total_frames(table)
                    {
                        let rest = Scale::new(master.get(), *rate).master(rest);
                        fx.feedback.push(Input::ActionLength {
                            cue: r.cue,
                            run: r.run,
                            frames: r.offset.saturating_add(r.delay).saturating_add(rest),
                        });
                    }
                }
            }
            Phase::Ended => {}
        }
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

    const ONCE: Plays = Plays::Times(1);

    /// Slice 0 (0..100) once, slice 1 (100..200) twice, slice 2 (200..300) once.
    fn table() -> Vec<Slice> {
        vec![
            s(0, 0, 100, ONCE),
            s(1, 100, 100, Plays::Times(2)),
            s(2, 200, 100, ONCE),
        ]
    }

    #[test]
    fn a_new_end_in_the_last_slice_moves_its_end() {
        let t = table();
        // Earlier, later, unchanged.
        let cut = new_end(&t, s(2, 200, 50, ONCE), true);
        assert_eq!(
            cut,
            Some(NewEnd {
                entry: 2,
                frames: 50
            })
        );
        assert_eq!(
            new_end(&t, s(2, 200, 180, ONCE), true),
            Some(NewEnd {
                entry: 2,
                frames: 180
            })
        );
        assert_eq!(new_end(&t, s(2, 200, 100, ONCE), true), None);
        // A table that may not grow keeps its end.
        assert_eq!(new_end(&t, s(2, 200, 180, ONCE), false), None);
        // A voice that started part way into the last slice.
        let late = [s(2, 260, 40, ONCE)];
        assert_eq!(
            new_end(&late, s(2, 200, 80, ONCE), true),
            Some(NewEnd {
                entry: 0,
                frames: 20
            })
        );
        // Moved behind where it started: nothing more plays.
        assert_eq!(
            new_end(&late, s(2, 200, 50, ONCE), true),
            Some(NewEnd {
                entry: 0,
                frames: 0
            })
        );
    }

    #[test]
    fn a_new_end_before_a_marker_cuts_the_table_there() {
        let t = table();
        // Into slice 0, played once: slices 1 and 2 go.
        let cut = new_end(&t, s(0, 0, 60, ONCE), true).unwrap();
        assert_eq!(
            cut,
            NewEnd {
                entry: 0,
                frames: 60
            }
        );
        assert!(cut.seals(t.len()));
        let mut cut_t = t.clone();
        cut.cut(&mut cut_t);
        assert_eq!(cut_t, [s(0, 0, 60, ONCE)]);
        // Into a slice the table loops: left for the next start.
        let mut t2 = vec![s(0, 0, 100, Plays::Times(2)), s(1, 100, 100, ONCE)];
        let cut = new_end(&t2, s(0, 0, 60, ONCE), true);
        assert_eq!(cut, None, "slice 0 loops in the table");
        // A voice that started in slice 2, which the new end (in slice 0) drops.
        let late = [s(2, 210, 90, ONCE)];
        assert_eq!(
            new_end(&late, s(0, 0, 60, ONCE), true),
            Some(NewEnd {
                entry: 0,
                frames: 0
            })
        );
        t2.clear();
        assert_eq!(new_end(&t2, s(0, 0, 60, ONCE), true), None);
    }

    #[test]
    fn an_end_in_a_loop_or_past_a_marker_the_table_lacks_is_left_for_the_next_start() {
        let t = table();
        // The new end's slice loops.
        assert_eq!(new_end(&t, s(1, 100, 50, Plays::Times(2)), true), None);
        assert_eq!(new_end(&t, s(1, 100, 50, ONCE), true), None);
        // The end moved past a marker: slice 3 is not in the table.
        assert_eq!(new_end(&t, s(3, 300, 50, ONCE), true), None);
        // A table that ends in a loop of the last slice.
        let looping = [s(0, 0, 100, ONCE), s(1, 100, 100, Plays::Infinite)];
        assert_eq!(
            new_end(&looping, s(1, 100, 50, Plays::Infinite), true),
            None
        );
    }
}
