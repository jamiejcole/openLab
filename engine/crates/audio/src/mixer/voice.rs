// SPDX-License-Identifier: GPL-3.0-or-later

//! One playing voice: its play plan (slices, loops, devamps), fades, pause and resume, patch
//! moves, and reading its source into the mixer's scratch buffers.

use crate::gain::{Curve, CurveRamp, Fade, Param, ramp_value, sanitize_gain};
use crate::meter::{Meters, next_epoch};
use crate::plan::{Plays, Slice, StreamLink};

use super::render::deinterleave;
use super::{Event, FaultLog, FinishReason, GainTarget, Outbox, SampleTime, VoiceId, VoiceStart};

pub(super) enum ReadOutcome {
    Full,
    Underrun,
    Ended(FinishReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Devamp {
    Stop,
    /// `cut`: the disk stream was asked to stop unrolling the loop, so stale frames must be
    /// skipped once it says where the continuation starts.
    Continue {
        cut: bool,
        /// The voice plays the loop exit's buffer at the boundary (it existed when the devamp
        /// was accepted, and the disk stream was told so with the cut).
        exit: bool,
    },
}

/// A devamp cut accepted while the voice still waited for the disk stream's answer to the
/// previous cut: published once that answer is in (the link carries one cut at a time).
#[derive(Clone, Copy, Debug)]
struct Deferred {
    /// Voice frame of the boundary.
    at: u64,
    exit: bool,
    /// The voice has reached the boundary already.
    passed: bool,
}

/// What happened at the end of a play.
enum Step {
    /// Next play or slice; the frames-remaining count keeps decreasing by one per frame.
    Next,
    /// Devamped into a later slice: the frames-remaining count jumps.
    Jumped,
    End,
}

/// Frames left after each played frame of one read, for the integrated fade-out: `first` for the
/// first frame, decreasing by one per frame, until frame `split`, which has `after`. `None` = no
/// known end (an infinite loop ahead).
pub(super) struct Remaining {
    pub(super) first: Option<u64>,
    pub(super) split: usize,
    pub(super) after: Option<u64>,
    /// The voice's re-started fade-out (see `Voice::refade`) before and after the split.
    pub(super) refade_first: Option<Refade>,
    pub(super) refade_after: Option<Refade>,
}

/// A fade-out restarted by a devamp that left less audio than the fade's length: from gain
/// `from` (what the last frame before the jump had) to silence on the last frame, over `len`
/// frames, so the gain never jumps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Refade {
    from: f32,
    len: u64,
}

/// A voice moving to another patch ([`CommandKind::MoveVoice`]): it plays into `from` and into
/// its new patch for `len` frames, `done` of them so far, crossfading from the one to the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Move {
    pub(super) from: usize,
    done: u64,
    len: u64,
}

impl Move {
    /// The new patch's share on frame `j` of the current segment (the old one's is `1 - g`).
    pub(super) fn gain(&self, j: usize) -> f32 {
        let k = self.done.saturating_add(j as u64);
        if k >= self.len {
            1.0
        } else {
            k as f32 / self.len as f32
        }
    }
}

/// A paused voice (CUE-05). The fade-out tail plays the frames that follow the pause point
/// without consuming them, so the voice resumes from exactly where it paused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Paused {
    /// Sample the pause took effect on.
    at: SampleTime,
    /// Declick fade-out frames rendered so far, of `len`: the de-zipper length, or fewer if
    /// fewer frames could be peeked (0 for a voice paused before it was heard).
    pub(super) done: u64,
    pub(super) len: u64,
    /// Something of the voice had been rendered when it paused.
    heard: bool,
}

/// The rest of a declick tail still fading out after a resume cut it short, crossfaded with the
/// resumed audio so the waveform never jumps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Xfade {
    pub(super) tail: Paused,
    /// The voice's `pos` on the resume: the tail plays `tail.done − (pos − pos_at)` frames ahead
    /// of the voice.
    pub(super) pos_at: u64,
    /// The declick gain the tail was heard at.
    pub(super) scale: f32,
}

impl Paused {
    /// Declick gain of the tail's frame `j`: falls from 1 and is exactly 0 on frame `len - 1`.
    pub(super) fn gain(&self, j: u64) -> f32 {
        ramp_value(1.0, 0.0, j + 1, self.len, Curve::Linear)
    }
}

/// Where [`Voice::peek`] copies to: `input` (stride: the block size), `xbuf`, or nowhere (only
/// count the frames).
#[derive(Clone, Copy, Debug)]
pub(super) enum Target {
    Input(usize),
    Xfade,
    Count,
}

/// Integrated fade-out gain for a frame with `r` frames after it, evaluated by `ramp`.
fn fade_out_gain(ramp: &mut CurveRamp, fout: Fade, r: Option<u64>, refade: Option<Refade>) -> f32 {
    match (r, refade) {
        (Some(r), Some(f)) => {
            ramp.value_of(f.from, 0.0, f.len, fout.curve, f.len.saturating_sub(r))
        }
        (Some(r), None) if fout.frames > 0 && r <= fout.frames => {
            ramp.value_of(1.0, 0.0, fout.frames, fout.curve, fout.frames - r)
        }
        _ => 1.0,
    }
}

/// Multiplies [`fade_out_gain`] into `gain`, whose frame `j` has `r0 − j` frames after it
/// (`None`: no known end, gain 1).
fn apply_fade_out(
    ramp: &mut CurveRamp,
    fout: Fade,
    gain: &mut [f32],
    r0: Option<u64>,
    refade: Option<Refade>,
) {
    let Some(r0) = r0 else { return };
    // Frames past the end have no `r` and gain 1.
    let n = usize::try_from(r0.saturating_add(1))
        .unwrap_or(usize::MAX)
        .min(gain.len());
    let Some(gain) = gain.get_mut(..n) else {
        return;
    };
    // The ramp's length; frames with more than that after them sit before its position 0.
    let len = match refade {
        Some(f) => f.len,
        None => fout.frames,
    };
    let lead = usize::try_from(r0.saturating_sub(len))
        .unwrap_or(usize::MAX)
        .min(n);
    let (early, ramped) = gain.split_at_mut(lead);
    let pos = len.saturating_sub(r0 - lead as u64);
    match refade {
        // `f.len − r` saturates to position 0, exactly `f.from`.
        Some(f) => {
            early.iter_mut().for_each(|g| *g *= f.from);
            ramp.apply_of((f.from, 0.0, f.len, fout.curve), pos, ramped, |g, v| {
                *g *= v
            });
        }
        // Not yet in the fade-out: gain 1.
        None => ramp.apply_of((1.0, 0.0, fout.frames, fout.curve), pos, ramped, |g, v| {
            *g *= v
        }),
    }
}

impl Remaining {
    fn at(&self, j: usize) -> Option<u64> {
        if j < self.split {
            self.first.and_then(|r| r.checked_sub(j as u64))
        } else {
            self.after
                .and_then(|r| r.checked_sub((j - self.split) as u64))
        }
    }
}

pub(super) struct Voice {
    pub(super) id: VoiceId,
    /// `Some` while the voice is active; holds the source and goes back whole when it ends.
    pub(super) start: Option<Box<VoiceStart>>,
    pub(super) channels: usize,
    pub(super) patch: usize,
    skip: u64,
    /// Frames consumed after the skip.
    pub(super) pos: u64,
    /// Frames taken from the ring so far, skips included (the devamp link's position unit).
    ring_pos: u64,
    /// Current slice-table entry, frames left in its current play, and plays after this one
    /// (`None` = infinite).
    entry: usize,
    left: u64,
    plays_left: Option<u64>,
    /// Plays of the current user slice completed so far.
    count: u64,
    /// Frames after the current play until the end; `None` = unbounded.
    tail: Option<u64>,
    devamp: Option<Devamp>,
    /// Devamped at this voice frame (`pos`); waiting for the disk stream to say where the
    /// continuation starts in the ring.
    awaiting: Option<u64>,
    /// See [`Deferred`].
    deferred: Option<Deferred>,
    /// Set when a devamp left less audio than the fade-out's length.
    pub(super) refade: Option<Refade>,
    /// Playing the start of a loop exit's continuation from its exit buffer (see
    /// [`StreamLink`]): entry it was made for, and frames of it played.
    exit: Option<(usize, usize)>,
    fade_in: Fade,
    fade_out: Fade,
    /// Evaluators of the integrated fade-in and fade-out (or refade) curves.
    env_in: CurveRamp,
    env_out: CurveRamp,
    pub(super) master: Param,
    pub(super) stop: Param,
    pub(super) stop_end: Option<SampleTime>,
    pub(super) cue_out: Vec<Param>,
    /// Row-major `[input][cue output]`.
    pub(super) xp: Vec<Param>,
    /// Planar `[channel][frame]`, `max_block_frames` per channel.
    pub(super) input: Vec<f32>,
    /// Not yet rendered: gain sets apply without de-zipper.
    pub(super) fresh: bool,
    pub(super) underrunning: bool,
    /// `Some` while paused; then nothing is read and no gain advances.
    pub(super) paused: Option<Paused>,
    /// Declick gain: ramps up from the tail's level on resume, 1.0 otherwise.
    pub(super) declick: Param,
    /// See [`Xfade`].
    pub(super) xfade: Option<Xfade>,
    /// See [`Move`].
    pub(super) moving: Option<Move>,
    /// A move asked for during `moving`'s crossfade: made when it ends.
    next_patch: Option<usize>,
    /// Devamp requests arrived while paused; the resume answers them.
    devamp_due: bool,
    /// Planar `[channel][frame]`, the de-zipper length per channel: the crossfaded tail.
    pub(super) xbuf: Vec<f32>,
    pub(super) xstride: usize,
    /// Pool slot index: where this voice's meters live.
    pub(super) slot: usize,
    /// Meter epoch of the current id (see `meter.rs`).
    pub(super) epoch: u32,
    /// Peak per cue output since the last meter flush.
    pub(super) meter: Vec<f32>,
    /// Recent faults blamed on this voice (see [`FAULT_LIMIT`]).
    pub(super) faults: FaultLog,
}

impl Voice {
    pub(super) fn new(
        slot: usize,
        outputs: usize,
        max_channels: usize,
        block: usize,
        dezip: usize,
    ) -> Self {
        Voice {
            id: VoiceId(0),
            start: None,
            channels: 0,
            patch: 0,
            skip: 0,
            pos: 0,
            ring_pos: 0,
            entry: 0,
            left: 0,
            plays_left: None,
            count: 0,
            tail: None,
            devamp: None,
            awaiting: None,
            deferred: None,
            refade: None,
            exit: None,
            fade_in: Fade::NONE,
            fade_out: Fade::NONE,
            env_in: CurveRamp::new(0.0, 1.0, 0, Curve::Linear),
            env_out: CurveRamp::new(1.0, 0.0, 0, Curve::Linear),
            master: Param::new(1.0),
            stop: Param::new(1.0),
            stop_end: None,
            cue_out: vec![Param::new(1.0); outputs],
            xp: vec![Param::new(0.0); max_channels * outputs],
            input: vec![0.0; max_channels * block],
            fresh: false,
            underrunning: false,
            paused: None,
            declick: Param::new(1.0),
            xfade: None,
            moving: None,
            next_patch: None,
            devamp_due: false,
            xbuf: vec![0.0; max_channels * dezip],
            xstride: dezip,
            slot,
            epoch: 0,
            meter: vec![0.0; outputs],
            faults: FaultLog::EMPTY,
        }
    }

    pub(super) fn is_active(&self) -> bool {
        self.start.is_some()
    }

    fn has_slices(&self) -> bool {
        self.start.as_ref().is_some_and(|s| !s.slices.is_empty())
    }

    /// Slice-table entry `k`; a voice without a table is one entry of `play_frames` (unbounded
    /// when `None`).
    fn slice(&self, k: usize) -> Option<Slice> {
        let s = self.start.as_ref()?;
        if s.slices.is_empty() {
            (k == 0).then_some(Slice {
                index: 0,
                start: 0,
                frames: s.play_frames.unwrap_or(u64::MAX),
                plays: Plays::Times(1),
            })
        } else {
            s.slices.get(k).copied()
        }
    }

    /// The first entry after `k` that belongs to a different user slice.
    fn next_slice_after(&self, k: usize) -> Option<usize> {
        let index = self.slice(k)?.index;
        let mut i = k + 1;
        while let Some(s) = self.slice(i) {
            if s.index != index {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// Starts entry `k`. Sums the later entries for the fade-out tail: O(`MAX_SLICES`) once per
    /// entry, never per loop.
    fn enter(&mut self, k: usize) {
        let Some(s) = self.slice(k) else { return };
        self.entry = k;
        self.left = s.frames;
        self.plays_left = match s.plays {
            Plays::Times(n) => Some(u64::from(n.saturating_sub(1))),
            Plays::Infinite => None,
        };
        let mut tail = self.plays_left.map(|n| n.saturating_mul(s.frames));
        let mut i = k + 1;
        while let (Some(t), Some(next)) = (tail, self.slice(i)) {
            tail = match next.plays {
                Plays::Times(n) => Some(t.saturating_add(next.frames.saturating_mul(u64::from(n)))),
                Plays::Infinite => None,
            };
            i += 1;
        }
        self.tail = tail;
    }

    /// Frames left after the next frame to be played.
    pub(super) fn remaining(&self) -> Option<u64> {
        self.tail
            .map(|t| t.saturating_add(self.left.saturating_sub(1)))
    }

    /// Takes over a validated start, copying its routing into the voice's gains.
    pub(super) fn begin(&mut self, s: Box<VoiceStart>, meters: &Meters) {
        // A new epoch: peaks the previous voice left unread stay its own.
        meters.flush_voice(self.slot, self.epoch, &mut self.meter);
        meters.retire(self.slot);
        self.epoch = next_epoch(self.epoch);
        meters.assign(self.slot, s.voice, self.epoch, true);
        self.id = s.voice;
        self.channels = usize::from(s.channels);
        self.patch = usize::from(s.patch);
        self.skip = s.skip_frames;
        self.pos = 0;
        self.ring_pos = 0;
        self.count = 0;
        self.devamp = None;
        self.awaiting = None;
        self.deferred = None;
        self.refade = None;
        self.exit = None;
        self.fade_in = s.fade_in;
        self.fade_out = s.fade_out;
        self.master.set(sanitize_gain(s.gain));
        self.stop.set(1.0);
        self.stop_end = None;
        for (p, g) in self.cue_out.iter_mut().zip(&s.cue_outputs) {
            p.set(sanitize_gain(*g));
        }
        for (p, g) in self.xp.iter_mut().zip(&s.crosspoints) {
            p.set(sanitize_gain(*g));
        }
        self.fresh = true;
        self.underrunning = false;
        self.paused = None;
        self.declick.set(1.0);
        self.xfade = None;
        self.moving = None;
        self.next_patch = None;
        self.devamp_due = false;
        self.start = Some(s);
        self.enter(0);
    }

    pub(super) fn param_mut(&mut self, target: GainTarget) -> Option<&mut Param> {
        let outs = self.cue_out.len();
        match target {
            GainTarget::Master => Some(&mut self.master),
            GainTarget::CueOutput(o) => self.cue_out.get_mut(usize::from(o)),
            GainTarget::Crosspoint { input, output } => {
                let (i, o) = (usize::from(input), usize::from(output));
                if i < self.channels && o < outs {
                    self.xp.get_mut(i * outs + o)
                } else {
                    None
                }
            }
        }
    }

    fn link(&self) -> Option<&StreamLink> {
        self.start.as_ref().and_then(|s| s.link.as_deref())
    }

    /// Arms a devamp at the end of the current play (CT-16), also while an earlier devamp's
    /// loop exit is still playing. Ignored without a slice table, while a different devamp is
    /// pending, or when continuing out of a loop has no link.
    pub(super) fn request_devamp(&mut self, stop: bool, t: SampleTime, outbox: &mut Outbox) {
        let ignored = Event::DevampIgnored {
            voice: self.id,
            at: t,
        };
        if !self.has_slices() {
            outbox.emit(ignored);
            return;
        }
        // The same devamp again (the cue engine re-sends a pending one after a resume) is
        // answered again; a different one cannot be carried out as well.
        if let Some(pending) = self.devamp {
            if (pending == Devamp::Stop) == stop {
                self.announce_devamp(t, outbox);
            } else {
                outbox.emit(ignored);
            }
            return;
        }
        let more_plays = self.plays_left != Some(0)
            || self
                .slice(self.entry + 1)
                .zip(self.slice(self.entry))
                .is_some_and(|(n, c)| n.index == c.index);
        let mode = if stop {
            Devamp::Stop
        } else if more_plays {
            // The ring already holds more plays of this loop; the disk stream must cut them.
            let at = self.pos.saturating_add(self.left);
            let target = self.next_slice_after(self.entry);
            let Some(link) = self.link() else {
                outbox.emit(ignored);
                return;
            };
            let exit = target
                .and_then(|k| link.exit(k))
                .is_some_and(|e| e.frames > 0);
            if self.awaiting.is_some() {
                // The link still carries the previous cut: send this one once it is answered.
                // Only one waits: a cut must never be dropped, or the disk stream would map
                // the next one from the wrong place.
                if self.deferred.is_some() {
                    outbox.emit(ignored);
                    return;
                }
                self.deferred = Some(Deferred {
                    at,
                    exit,
                    passed: false,
                });
            } else {
                link.request_cut(at, exit);
            }
            Devamp::Continue { cut: true, exit }
        } else {
            Devamp::Continue {
                cut: false,
                exit: false,
            }
        };
        self.devamp = Some(mode);
        self.announce_devamp(t, outbox);
    }

    /// Answers a devamp request applied on `t` with where the voice leaves the loop. A paused
    /// voice does not know that yet: its resume answers, once however many requests came in.
    fn announce_devamp(&mut self, t: SampleTime, outbox: &mut Outbox) {
        if self.paused.is_some() {
            self.devamp_due = true;
            return;
        }
        outbox.emit(Event::DevampScheduled {
            voice: self.id,
            at: t.saturating_add(self.left),
        });
    }

    /// Pauses on sample `t` with a declick tail of up to `dezip` frames: as many as can be
    /// peeked now, so the fade always reaches silence (none if nothing has been heard yet).
    /// Gains, fades and the stop fade freeze where they are.
    pub(super) fn pause(&mut self, t: SampleTime, dezip: usize) {
        if self.paused.is_some() {
            return;
        }
        // A crossfade still under way is cut: it is at most one de-zipper length old.
        self.xfade = None;
        let len = if self.fresh {
            0
        } else {
            self.peek(0, dezip, Target::Count) as u64
        };
        self.paused = Some(Paused {
            at: t,
            done: 0,
            len,
            heard: !self.fresh,
        });
    }

    /// Moves to patch `to`, crossfading over `len` frames unless nothing of the voice is heard
    /// now (see [`CommandKind::MoveVoice`]).
    pub(super) fn move_to(&mut self, to: usize, len: u64) {
        if self.moving.is_some() {
            self.next_patch = (to != self.patch).then_some(to);
            return;
        }
        if to == self.patch {
            return;
        }
        let silent = self.fresh || self.paused.is_some_and(|p| p.done >= p.len);
        if !silent && len > 0 {
            self.moving = Some(Move {
                from: self.patch,
                done: 0,
                len,
            });
        }
        self.patch = to;
    }

    /// The move's crossfade has played `n` more frames: once it is over, the move that waited
    /// for it starts.
    pub(super) fn advance_move(&mut self, n: usize) {
        let Some(m) = self.moving.as_mut() else {
            return;
        };
        m.done = m.done.saturating_add(n as u64);
        if m.done < m.len {
            return;
        }
        let len = m.len;
        self.moving = None;
        if let Some(next) = self.next_patch.take() {
            self.move_to(next, len);
        }
    }

    /// Resumes on sample `t` as `id` (nothing if not paused). Everything timed in samples (the
    /// stop fade's end) moves by the time spent paused.
    pub(super) fn resume(&mut self, t: SampleTime, id: VoiceId, dezip: u64, outbox: &mut Outbox) {
        let Some(p) = self.paused.take() else {
            return;
        };
        self.stop_end = self
            .stop_end
            .map(|e| e.saturating_add(t.saturating_sub(p.at)));
        // Nothing heard yet, or resumed on the pause sample before any of the tail played: the
        // voice simply carries on. Otherwise fade in, crossfading with what is left of the tail.
        if p.heard && (p.done > 0 || p.len == 0) {
            if p.done < p.len {
                self.xfade = Some(Xfade {
                    tail: p,
                    pos_at: self.pos,
                    scale: self.declick.current(),
                });
            }
            self.declick.set(0.0);
            self.declick.ramp_to(1.0, dezip, Curve::Linear);
        }
        if id != self.id {
            // Peaks so far belong to the old id; from here on they are the new id's.
            let meters = &outbox.shared.meters;
            meters.flush_voice(self.slot, self.epoch, &mut self.meter);
            meters.retire(self.slot);
            self.epoch = next_epoch(self.epoch);
            meters.assign(self.slot, id, self.epoch, true);
        }
        self.id = id;
        if let Some(s) = self.start.as_mut() {
            s.voice = id;
        }
        // Answers requests made while paused. A devamp answered before the pause is not
        // announced again: the cue engine re-sends it for the resumed id and gets its answer.
        if std::mem::take(&mut self.devamp_due) && self.devamp.is_some() {
            outbox.emit(Event::DevampScheduled {
                voice: id,
                at: t.saturating_add(self.left),
            });
        }
    }

    /// Copies up to `n` frames that will play next, starting `from` frames ahead, into `to`
    /// without consuming them (an `rtrb` read chunk dropped uncommitted leaves the ring as it
    /// was). Stops at the voice's end, at a devamp boundary and where the ring runs out; the rest
    /// up to `n` is silence. Returns the frames copied.
    pub(super) fn peek(&mut self, from: u64, n: usize, to: Target) -> usize {
        let ch = self.channels;
        // A pending devamp jumps at the end of this play; beyond it only the exit could be peeked.
        let limit = if self.devamp.is_some() || self.exit.is_some() {
            self.left
        } else {
            self.remaining().map_or(u64::MAX, |r| r.saturating_add(1))
        };
        let want = usize::try_from(limit.saturating_sub(from))
            .unwrap_or(usize::MAX)
            .min(n);
        let from = usize::try_from(from).unwrap_or(usize::MAX);
        let Voice {
            start,
            input,
            xbuf,
            xstride,
            exit,
            awaiting,
            skip,
            ..
        } = self;
        let (mut input, block) = match to {
            Target::Input(block) => (Some(input), block),
            Target::Xfade => (Some(xbuf), *xstride),
            Target::Count => (None, 0),
        };
        let mut got = 0;
        if let Some((target, done)) = *exit {
            if let Some(e) = start
                .as_ref()
                .and_then(|s| s.link.as_deref())
                .and_then(|l| l.exit(target))
            {
                let first = done.saturating_add(from);
                got = e.frames.saturating_sub(first).min(want);
                if let Some(src) = e.samples.get(first * ch..(first + got) * ch) {
                    if let Some(input) = input.as_mut() {
                        deinterleave(src, &[], ch, input, block, 0);
                    }
                } else {
                    got = 0;
                }
            }
        } else if awaiting.is_none()
            && let Some(src) = start.as_mut().map(|s| &mut s.source)
        {
            // Frames still to be skipped come first in the ring.
            let first = usize::try_from(*skip)
                .unwrap_or(usize::MAX)
                .saturating_add(from);
            let avail = src.slots() / ch;
            got = avail.saturating_sub(first).min(want);
            if input.is_none() {
                // Counting only.
            } else if got > 0
                && let Ok(chunk) = src.read_chunk((first + got) * ch)
            {
                let (a, b) = chunk.as_slices();
                let skip = first * ch;
                if let Some(input) = input.as_mut() {
                    let b = b.get(skip.saturating_sub(a.len())..).unwrap_or_default();
                    let a = a.get(skip..).unwrap_or_default();
                    deinterleave(a, b, ch, input, block, 0);
                }
                // Dropped without a commit: nothing is consumed.
            } else {
                got = 0;
            }
        }
        if let Some(input) = input {
            for c in 0..ch {
                if let Some(rest) = input.get_mut(c * block + got..c * block + n) {
                    rest.fill(0.0);
                }
            }
        }
        got
    }

    /// The current play ended just before sample `at`.
    fn boundary(&mut self, at: SampleTime, outbox: &mut Outbox) -> Step {
        let Some(cur) = self.slice(self.entry) else {
            return Step::End;
        };
        let voice = self.id;
        // Voices without a slice table report only VoiceFinished.
        let events = self.has_slices();
        let mut emit = |e: Event| {
            if events {
                outbox.emit(e);
            }
        };
        let slice_ended = Event::SliceEnded {
            voice,
            at,
            slice: cur.index,
        };
        match self.devamp.take() {
            Some(Devamp::Stop) => {
                emit(slice_ended);
                return Step::End;
            }
            Some(Devamp::Continue { cut, exit }) => {
                emit(slice_ended);
                let Some(target) = self.next_slice_after(self.entry) else {
                    return Step::End;
                };
                if cut {
                    match self.deferred.as_mut() {
                        Some(d) => d.passed = true,
                        None => self.awaiting = Some(self.pos),
                    }
                    // Play the continuation's start from its exit buffer while the stale
                    // frames are skipped and the stream refills the ring behind them. Without
                    // one (not built), the ring waits for the continuation.
                    self.exit = exit.then_some((target, 0));
                }
                self.count = 0;
                self.enter(target);
                return Step::Jumped;
            }
            None => {}
        }
        let looped = Event::LoopReached {
            voice,
            at,
            slice: cur.index,
            count: self.count + 1,
        };
        if self.plays_left != Some(0) {
            self.plays_left = self.plays_left.map(|n| n - 1);
            self.left = cur.frames;
            self.tail = self.tail.map(|t| t.saturating_sub(cur.frames));
            self.count += 1;
            emit(looped);
            return Step::Next;
        }
        let Some(next) = self.slice(self.entry + 1) else {
            emit(slice_ended);
            return Step::End;
        };
        if next.index == cur.index {
            self.count += 1;
            emit(looped);
        } else {
            self.count = 0;
            emit(slice_ended);
        }
        self.enter(self.entry + 1);
        Step::Next
    }

    /// Reads up to `live` frames into `input`, crossing loop and slice boundaries (their events
    /// at `t0` + offset), and zero-fills it up to `len`.
    pub(super) fn read(
        &mut self,
        live: usize,
        len: usize,
        block: usize,
        t0: SampleTime,
        outbox: &mut Outbox,
    ) -> (usize, ReadOutcome, Remaining) {
        let ch = self.channels;
        let mut got = 0;
        let mut rem = Remaining {
            first: self.remaining(),
            split: usize::MAX,
            after: None,
            refade_first: self.refade,
            refade_after: None,
        };
        let outcome = loop {
            if let Some(c) = self.awaiting
                && let Some(r) = self.link().and_then(|l| l.resumed(c))
            {
                // Every ring frame before the continuation is stale.
                self.skip = self.skip.max(r.saturating_sub(self.ring_pos));
                self.awaiting = None;
            }
            if self.awaiting.is_none()
                && let Some(d) = self.deferred.take()
            {
                if let Some(link) = self.link() {
                    link.request_cut(d.at, d.exit);
                }
                if d.passed {
                    self.awaiting = Some(d.at);
                }
            }
            // The exit buffer covers the wait.
            if self.awaiting.is_some() && self.exit.is_none() {
                // Without its producer the continuation will never come.
                let abandoned = self.start.as_ref().is_none_or(|s| s.source.is_abandoned());
                break if abandoned {
                    ReadOutcome::Ended(FinishReason::EndOfStream)
                } else {
                    ReadOutcome::Underrun
                };
            }
            let Some(src) = self.start.as_mut().map(|s| &mut s.source) else {
                break ReadOutcome::Ended(FinishReason::Stopped);
            };
            let mut avail = src.slots() / ch;
            if self.skip > 0 {
                let s = avail.min(usize::try_from(self.skip).unwrap_or(usize::MAX));
                if let Ok(chunk) = src.read_chunk(s * ch) {
                    chunk.commit_all();
                    self.skip -= s as u64;
                    self.ring_pos += s as u64;
                    avail -= s;
                }
            }
            if got == live {
                break ReadOutcome::Full;
            }
            if let Some((target, done)) = self.exit {
                let n = self.read_exit(target, done, got, live, block);
                got += n;
                self.pos += n as u64;
                self.left -= n as u64;
                match self.exit.as_mut() {
                    Some(e) if n > 0 => e.1 += n,
                    // Exhausted (or missing): the ring has the rest.
                    _ => self.exit = None,
                }
                if self.left == 0 {
                    match self.boundary(t0 + got as u64, outbox) {
                        Step::Next => {}
                        Step::Jumped => self.jumped(got, &mut rem),
                        Step::End => break ReadOutcome::Ended(FinishReason::OutPoint),
                    }
                }
                continue;
            }
            let want = (live - got)
                .min(avail)
                .min(usize::try_from(self.left).unwrap_or(usize::MAX));
            if want > 0
                && let Ok(chunk) = src.read_chunk(want * ch)
            {
                let (a, b) = chunk.as_slices();
                deinterleave(a, b, ch, &mut self.input, block, got);
                chunk.commit_all();
                got += want;
                self.pos += want as u64;
                self.ring_pos += want as u64;
                self.left -= want as u64;
            }
            if self.left == 0 {
                match self.boundary(t0 + got as u64, outbox) {
                    Step::Next => {}
                    Step::Jumped => self.jumped(got, &mut rem),
                    Step::End => break ReadOutcome::Ended(FinishReason::OutPoint),
                }
                continue;
            }
            if got == live {
                break ReadOutcome::Full;
            }
            // Check abandonment before re-reading slots, so frames pushed just before the
            // producer was dropped are never mistaken for end of stream.
            let Some(src) = self.start.as_ref().map(|s| &s.source) else {
                break ReadOutcome::Ended(FinishReason::Stopped);
            };
            break if src.is_abandoned() && src.slots() / ch == 0 {
                ReadOutcome::Ended(FinishReason::EndOfStream)
            } else {
                ReadOutcome::Underrun
            };
        };
        for c in 0..ch {
            if let Some(rest) = self.input.get_mut(c * block + got..c * block + len) {
                rest.fill(0.0);
            }
        }
        (got, outcome, rem)
    }

    /// After a devamp jump at frame `got` of a read: the frames-remaining count changes there,
    /// and a fade-out the jump shortened restarts from the gain the last frame before it had.
    fn jumped(&mut self, got: usize, rem: &mut Remaining) {
        // The gain the last frame before the jump had (the frame before this read's first, if
        // the jump comes first).
        let last = match got.checked_sub(1) {
            Some(j) => rem.at(j),
            None => rem.first.map(|r| r.saturating_add(1)),
        };
        let from = fade_out_gain(&mut self.env_out, self.fade_out, last, self.refade);
        rem.split = got;
        rem.after = self.remaining();
        self.refade = match rem.after {
            Some(r) if self.fade_out.frames > 0 && r < self.fade_out.frames => {
                Some(Refade { from, len: r + 1 })
            }
            _ => None,
        };
        rem.refade_after = self.refade;
    }

    /// Copies up to `live - got` frames (and at most the current play's `left`) of the exit
    /// buffer for entry `target`, from its frame `done`, into `input` at frame `got`. Returns the
    /// frames copied; 0 when the buffer is exhausted.
    fn read_exit(
        &mut self,
        target: usize,
        done: usize,
        got: usize,
        live: usize,
        block: usize,
    ) -> usize {
        let ch = self.channels;
        let left = usize::try_from(self.left).unwrap_or(usize::MAX);
        let Voice { start, input, .. } = self;
        let Some(exit) = start
            .as_ref()
            .and_then(|s| s.link.as_deref())
            .and_then(|l| l.exit(target))
        else {
            return 0;
        };
        let n = exit.frames.saturating_sub(done).min(live - got).min(left);
        let Some(src) = exit.samples.get(done * ch..(done + n) * ch) else {
            return 0;
        };
        deinterleave(src, &[], ch, input, block, got);
        n
    }

    /// Multiplies the integrated fade envelope into `gain` for the `got` frames read from `pos0`.
    pub(super) fn apply_envelope(
        &mut self,
        gain: &mut [f32],
        pos0: u64,
        got: usize,
        rem: &Remaining,
    ) {
        let (fin, fout) = (self.fade_in, self.fade_out);
        let in_active = fin.frames > 0 && pos0 < fin.frames;
        // The fade-out covers a frame with at most `fout.frames` frames after it.
        let out_active = fout.frames > 0
            && (rem.refade_first.is_some()
                || rem.refade_after.is_some()
                || [rem.first, rem.after]
                    .iter()
                    .flatten()
                    .any(|&r| r <= fout.frames.saturating_add(got as u64)));
        if !in_active && !out_active {
            return;
        }
        let n = got.min(gain.len());
        let Some(gain) = gain.get_mut(..n) else {
            return;
        };
        // Frame by frame this is `g *= env_in(p)`, then `g *= fade_out_gain(…)`; each envelope
        // is applied over the stretch it covers, in that order, so the bits are the same.
        if in_active {
            let k = usize::try_from(fin.frames - pos0)
                .unwrap_or(usize::MAX)
                .min(n);
            if let Some(head) = gain.get_mut(..k) {
                let ramp = (0.0, 1.0, fin.frames, fin.curve);
                self.env_in.apply_of(ramp, pos0, head, |g, v| *g *= v);
            }
        }
        if out_active {
            let (before, after) = gain.split_at_mut(rem.split.min(n));
            apply_fade_out(&mut self.env_out, fout, before, rem.first, rem.refade_first);
            apply_fade_out(&mut self.env_out, fout, after, rem.after, rem.refade_after);
        }
    }

    pub(super) fn finish(&mut self, at: SampleTime, reason: FinishReason, outbox: &mut Outbox) {
        self.paused = None;
        if let Some(start) = self.start.take() {
            // Its last peaks are read once more under its own id (same epoch, now inactive).
            let meters = &outbox.shared.meters;
            meters.flush_voice(self.slot, self.epoch, &mut self.meter);
            meters.assign(self.slot, self.id, self.epoch, false);
            outbox.give_back(start);
            outbox.emit(Event::VoiceFinished {
                voice: self.id,
                at,
                reason,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The per-frame envelope as it was before it was applied by stretches (M3.19): the
    /// reference the mixer's must match bit for bit.
    fn envelope_per_frame(v: &mut Voice, gain: &mut [f32], pos0: u64, got: usize, rem: &Remaining) {
        let (fin, fout) = (v.fade_in, v.fade_out);
        let in_active = fin.frames > 0 && pos0 < fin.frames;
        let out_active = fout.frames > 0
            && (rem.refade_first.is_some()
                || rem.refade_after.is_some()
                || [rem.first, rem.after]
                    .iter()
                    .flatten()
                    .any(|&r| r <= fout.frames.saturating_add(got as u64)));
        if !in_active && !out_active {
            return;
        }
        for (j, g) in gain.iter_mut().take(got).enumerate() {
            let p = pos0 + j as u64;
            if in_active && p < fin.frames {
                *g *= v.env_in.value_of(0.0, 1.0, fin.frames, fin.curve, p);
            }
            if out_active {
                let refade = if j < rem.split {
                    rem.refade_first
                } else {
                    rem.refade_after
                };
                *g *= fade_out_gain(&mut v.env_out, fout, rem.at(j), refade);
            }
        }
    }

    const CURVES: [Curve; 5] = [
        Curve::Linear,
        Curve::EqualPower,
        Curve::SCurve,
        Curve::Logarithmic,
        Curve::Exponential,
    ];

    proptest::proptest! {
        /// Fade-in, fade-out and refades (either side of a devamp jump), from any position:
        /// the stretch-wise envelope gives the per-frame one's bits.
        #[test]
        fn envelope_by_stretches_matches_the_per_frame_reference(
            fin in 0u64..300,
            fout in 0u64..300,
            curves in (0usize..5, 0usize..5),
            pos0 in 0u64..400,
            got in 0usize..=256,
            first in proptest::option::of(0u64..700),
            split in proptest::option::of(0usize..300),
            after in proptest::option::of(0u64..700),
            refades in (
                proptest::option::of((0.0f32..2.0, 1u64..300)),
                proptest::option::of((0.0f32..2.0, 1u64..300)),
            ),
            reps in 1usize..4,
        ) {
            let refade = |r: Option<(f32, u64)>| r.map(|(from, len)| Refade { from, len });
            let rem = Remaining {
                first,
                split: split.unwrap_or(usize::MAX),
                after,
                refade_first: refade(refades.0),
                refade_after: refade(refades.1),
            };
            let (mut a, mut b) = (Voice::new(0, 2, 2, 256, 16), Voice::new(0, 2, 2, 256, 16));
            for v in [&mut a, &mut b] {
                v.fade_in = Fade::new(fin, CURVES[curves.0]);
                v.fade_out = Fade::new(fout, CURVES[curves.1]);
            }
            // Repeated calls reuse (or replace) the evaluators' state.
            for k in 0..reps {
                let mut ga: Vec<f32> = (0..256).map(|j| 0.5 + j as f32 * 1e-3).collect();
                let mut gb = ga.clone();
                let p = pos0 + (k * got) as u64;
                a.apply_envelope(&mut ga, p, got, &rem);
                envelope_per_frame(&mut b, &mut gb, p, got, &rem);
                let bits = |g: &[f32]| g.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
                proptest::prop_assert_eq!(bits(&ga), bits(&gb));
            }
        }
    }
}
