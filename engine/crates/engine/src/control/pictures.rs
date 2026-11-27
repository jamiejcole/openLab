// SPDX-License-Identifier: GPL-3.0-or-later

//! Video cues' pictures (CT-03, VID-03): the cue engine runs a Video cue's picture as a voice,
//! and this carries its voice actions out on the stage host instead of the mixer. Each run's
//! picture gets an id of the engine's; the stage host is told to load, play, pause and stop it
//! on sample times of the engine's clock, which it is sent as it moves, so the picture follows
//! the audio clock (#641).
//!
//! The run's length is known from the probe (a video's length, a still's typed duration, none
//! for a still shown until stopped) and told to the cue engine when the picture starts
//! (`ActionLength`), so the cue ends on its last frame's sample. When the cue's action is over
//! (it ends, stops, breaks or is reset) its picture goes.
//!
//! A movie's trims, slices and play counts (VID-07) are planned as an Audio cue's are (the same
//! slice table, `cue_slices`) and sent with each play as the passes the stage host follows, so
//! a loop's seam is the stage host's to show without a gap. A devamp is worked out here from
//! that table: the pass under way is the loop's last, the plan is sent again from the same
//! start with that loop cut there, and the cue engine is told where (`DevampScheduled`) and the
//! new length. With hold at end the cue is told no length: it stays active on its last frame
//! until it is stopped. The integrated fades go with each play for the stage host to draw.
//!
//! A GO on a Video cue whose file's probe has not answered yet (just after the show opened, or
//! the file was changed, relinked or came back) waits for it, as an Audio cue's does: nothing
//! is sent to the stage host until the answer, the cue starts late (`cold_start`) when it
//! comes, and breaks only on a probe that fails or on no answer within `PROBE_HOLD_LIMIT`.
//!
//! A start whose picture is not open on the stage host yet (a GO on a cue that was not loaded or
//! standing by, or one the probe held) waits for its open, its first frame decoded, before
//! picture and sound start together on that sample, late by the wait (counted into its length),
//! so its opening frames are shown, never skipped. Only a stage host that is up holds a start;
//! the open's own limit breaks a file that does not open, and `OPEN_WAIT_LIMIT` bounds the wait
//! if the stage host never answers.
//!
//! A stage host that starts (again, after a crash) is sent every picture again, so a picture
//! comes back by itself. A picture the stage host cannot decode breaks only its own cue, with a
//! notice. The control thread never waits on the stage host.

mod primes;
mod sequence;

use std::collections::BTreeMap;

use openlab_core::protocol::LogLevel;
use openlab_core::stage_link::ToStage;
use openlab_core::video_probe::ProbeReport;

use super::media::Probe;
use openlab_audio::{MediaInfo, Plays as Passes, Slice, total_frames};
use openlab_core::{
    Action, BrokenReason, CueId, CueKind, CueState, Input, LevelParam, PREVIEW_RUN_BASE,
    PictureFades, PictureParam, PictureRamp, PlanPass, RampCurve, RunId, SampleTime, StageId,
    VideoGeometry, VideoTiming, plan_point, seconds_to_samples,
};

use super::plan::{PlanSpec, cue_slices, picture_info};

use super::voices::{Effects, Planned, SeekTo, new_end, rest_of_table};
use super::{DocCx, DocId, Engine};

/// How far the engine's clock may move before the stage host is told it again (it moves the
/// picture on by its own clock in between).
const CLOCK_EVERY_MS: u64 = 10;

/// How far a picture's sound may drift from it, on a device whose clock is not the master's,
/// before the picture is moved to follow it (microseconds): far under a frame, and over what the
/// smoothed reading of that device's clock wanders.
const DRIFT_STEP_US: u64 = 500;

/// The most a start waits for its picture's open (sample time) before its cue breaks: past the
/// stage host's own open limit (10 s), which breaks it first with its reason, so this only
/// bounds a wait the stage host never answers.
const OPEN_WAIT_LIMIT_S: f64 = 15.0;

/// A start waiting for its picture's open: the action is `offset` into its time on sample `at`
/// (as the cue engine dates it), and the picture starts `from` into it.
#[derive(Clone, Copy)]
struct Wait {
    at: SampleTime,
    offset: SampleTime,
    from: SampleTime,
    /// When the wait began, for `OPEN_WAIT_LIMIT_S`.
    since: SampleTime,
    paused: bool,
    /// A devamp asked meanwhile (its stop), carried out once the picture plays.
    devamp: Option<bool>,
    /// The cues started with it (one GO's sequence), which start together once every picture
    /// among them is open (`sequence.rs`).
    group: Option<u64>,
}

/// One run's picture.
struct Live {
    doc: DocId,
    cue: CueId,
    run: RunId,
    /// The `load` it was given, to send again to a new stage host.
    load: ToStage,
    /// Its first `play`, likewise: it dates when the picture is first drawn and its place in
    /// the stacking, so a picture paused (or stopping) when the stage host starts again comes
    /// back where it was.
    first: Option<ToStage>,
    /// Its last `play` or `pause`, likewise.
    now: Option<ToStage>,
    /// Its `stop`, likewise, sent after `now`: a stop never takes the place of the play or
    /// pause before it, so a stopping picture comes back where it was (a pause counted).
    stop: Option<ToStage>,
    /// The last ramp of each geometry value Fade cues move (VID-05), likewise: each starts from
    /// where the one before it is on its sample, as the mixer starts a gain's.
    ramps: BTreeMap<PictureParam, PictureRamp>,
    /// It is stopping: gone from the stage host after this sample.
    ends: Option<SampleTime>,
    /// The stage host said its file is open (since the load was last sent).
    opened: bool,
    /// Where it is, for progress: playing from sample `.0` (`Some`), or held (`None`), at `.1`
    /// seconds into its action then; `None` until it first plays.
    motion: Option<(Option<SampleTime>, f64)>,
    /// Its action's length in seconds (a video's, a still's typed duration), if it ends.
    length: Option<f64>,
    still: bool,
    /// A still's start delay (held by its probe or its open): how far its own time runs behind
    /// its action's, as a movie's plan keeps `late`, so a resume tells the cue its whole
    /// duration and progress counts from where it is.
    still_late: SampleTime,
    /// A movie's plan as its last play sent it, for its position, pause and devamp.
    plan: Option<RunPlan>,
    /// How many stage host crashes it was decoding at (or still opening) within
    /// `SUSPECT_WINDOW_S`, and when the first of them was.
    suspected: u32,
    suspected_at: Option<SampleTime>,
    /// Kept off the stage host until it is tried alone (`try_held_back_pictures`).
    held_back: bool,
    /// Sent again alone from the hold-back on this sample: a crash before
    /// `PROBATION_S` has passed is its doing.
    trial: Option<SampleTime>,
    /// Its start, waiting for its open.
    wait: Option<Wait>,
}

/// A movie run's plan (engine samples, from the top of its action). `top` is the plan the run
/// started with, cut by each devamp whose loop it has left since: a resume plays on with it,
/// as a paused Audio voice keeps the plan it was given, whatever was edited since. `table` is
/// what its last play sent (`top`, or `top` cut by the devamp in `devamp`),
/// from sample `at`, `into` samples into the table then; `late` is how far the picture's own
/// time runs behind the action's: a start held by its probe, and its sound late (starved by
/// the disk) or early (on a device whose clock runs fast), which the picture followed (negative:
/// ahead). `followed` is how much of its sound's device drift that includes.
struct RunPlan {
    top: Vec<Slice>,
    table: Vec<Slice>,
    at: SampleTime,
    into: SampleTime,
    late: i64,
    followed: i64,
    passes: Vec<PlanPass>,
    /// The fades its last play was sent with.
    fades: PictureFades,
    /// The last devamp of it: its stop, and the sample it leaves the loop on. Pending until
    /// the picture is there, carried out from then on.
    devamp: Option<(bool, SampleTime)>,
    /// The time and loops `top` was planned from, for an end time edited while it plays.
    planned: Option<Planned>,
}

impl Live {
    /// How far its own time runs behind its action's (negative: ahead).
    fn late(&self) -> i64 {
        match &self.plan {
            Some(p) => p.late,
            None => i64::try_from(self.still_late).unwrap_or(i64::MAX),
        }
    }

    /// What a new stage host is sent for it: its load, first play, last play or pause, and
    /// stop.
    fn messages(&self) -> Vec<ToStage> {
        let now = self
            .now
            .as_ref()
            .filter(|n| Some(*n) != self.first.as_ref());
        let picture = match &self.load {
            ToStage::Load { picture, .. } => *picture,
            _ => 0,
        };
        let ramps = self.ramps.iter().map(|(&param, ramp)| ToStage::Ramp {
            picture,
            param,
            ramp: ramp.clone(),
        });
        std::iter::once(self.load.clone())
            .chain(ramps)
            .chain(self.first.clone())
            .chain(now.cloned())
            .chain(self.stop.clone())
            .collect()
    }
}

impl RunPlan {
    /// Moves the plan `frames` later from sample `at` (earlier when negative) and returns where
    /// it stands then, in the table. `at` may come before the sample the plan was last moved on
    /// (an underrun ended in the block after a step dated a lookahead ahead): it is counted
    /// back from there, so the picture never runs on by the difference.
    fn shift(&mut self, at: SampleTime, frames: i64) -> SampleTime {
        let since = if at >= self.at {
            i64::try_from(at - self.at).unwrap_or(i64::MAX)
        } else {
            i64::try_from(self.at - at).map_or(i64::MIN, |back| -back)
        };
        // Where the plan stands at `at`, less what the sound missed.
        let into = less_late(plus_late(self.into, since), frames);
        self.at = at;
        self.into = into;
        self.late = self.late.saturating_add(frames);
        // A devamp already carried out leaves its loop that much later.
        self.devamp = self
            .devamp
            .map(|(stop, leaves)| (stop, plus_late(leaves, frames)));
        into
    }

    /// The run's plan with no devamp pending, `elapsed` samples into the table: a devamp whose
    /// loop the picture has left by then stays cut (taken at the boundary, as an Audio voice
    /// takes its own); one it has not reached is dropped.
    fn settled(&self, elapsed: SampleTime) -> &[Slice] {
        let left = self.devamp.is_some_and(|(_, leaves)| {
            elapsed >= self.into.saturating_add(leaves.saturating_sub(self.at))
        });
        if left { &self.table } else { &self.top }
    }
}

/// Crashes this far apart count as one run of crashes for the pictures decoding at them.
const SUSPECT_WINDOW_S: f64 = 60.0;
/// How long a picture tried alone after a hold-back must stay up without a crash to be cleared.
const PROBATION_S: f64 = 5.0;

/// A run whose picture waits for its file's probe: nothing is on the stage host yet.
struct Held {
    doc: DocId,
    cue: CueId,
    run: RunId,
    /// Where in its action it loads (or holds while paused).
    offset: SampleTime,
    /// Its start (sample, offset into the action), once it has one: a load has none.
    start: Option<(SampleTime, SampleTime)>,
    /// When its start began to wait: the hold limit counts from here.
    since: Option<SampleTime>,
    paused: bool,
}

/// Every open doc's pictures, and the clock the stage host was last told.
#[derive(Default)]
pub(super) struct Pictures {
    next: u64,
    live: BTreeMap<u64, Live>,
    held: Vec<Held>,
    clock_sent: Option<SampleTime>,
    /// Pictures opened ahead for the cues standing by.
    primes: primes::Primes,
    /// Cues started together that wait for a picture among them to open.
    sequence: sequence::Sequences,
    /// Pictures were being tried alone after crashes at the last look (`trials_ended`).
    on_trial: bool,
    /// Pictures the stage host reported failed, for their doc's next tick, and whether their
    /// start was still waiting for the open (nothing seen or heard: the cue was only skipped).
    failed: Vec<(DocId, CueId, RunId, String, bool)>,
}

impl Pictures {
    fn find(&self, doc: DocId, run: RunId) -> Option<u64> {
        self.live
            .iter()
            .find(|(_, l)| l.doc == doc && l.run == run)
            .map(|(id, _)| *id)
    }

    fn held_mut(&mut self, doc: DocId, run: RunId) -> Option<&mut Held> {
        self.held.iter_mut().find(|h| h.doc == doc && h.run == run)
    }

    fn take_held(&mut self, doc: DocId, run: RunId) -> Option<Held> {
        let at = self
            .held
            .iter()
            .position(|h| h.doc == doc && h.run == run)?;
        Some(self.held.swap_remove(at))
    }
}

/// Why a Video cue cannot be loaded now.
enum Refused {
    /// Its file's probe has not answered yet: the run waits for it.
    Pending,
    /// It cannot play: the cue breaks.
    Failed(&'static str),
}

/// What a Video cue plays: its file's probe and its stage.
struct Plays {
    path: String,
    still: bool,
    stage: StageId,
    geometry: VideoGeometry,
    /// The action's length in seconds, if it has one (a still's typed duration; a movie's is
    /// its plan's).
    length: Option<f64>,
    /// What a movie's plan is built on (its length as media on the engine's clock, or the
    /// probe of the audio track its sound plays), and its time and loops.
    movie: Option<(MediaInfo, VideoTiming)>,
}

/// A movie's plan at `rate` from the top of its action, and the action's length (`None` if it
/// loops for ever).
fn movie_table(
    file: &MediaInfo,
    timing: &VideoTiming,
    rate: std::num::NonZeroU32,
) -> Result<(Vec<Slice>, Option<SampleTime>), &'static str> {
    let top = cue_slices(&movie_spec(timing), file, rate, 0)
        .map_err(|_| "its trims and loops cannot be played")?;
    let total = total_frames(&top);
    Ok((top, total))
}

/// What a movie's plan is planned from.
fn movie_spec(timing: &VideoTiming) -> PlanSpec<'_> {
    PlanSpec {
        start_time: timing.start_time,
        end_time: timing.end_time,
        slices: &timing.slices,
        last_slice_play_count: timing.last_slice_play_count,
    }
}

/// The passes the stage host follows for `table`.
fn passes(table: &[Slice], rate: u32) -> Vec<PlanPass> {
    table
        .iter()
        .map(|s| PlanPass {
            from: seconds(s.start, rate),
            length: seconds(s.frames, rate),
            plays: match s.plays {
                Passes::Times(n) => n,
                Passes::Infinite => 0,
            },
        })
        .collect()
}

/// Where in the file a movie following `passes` is `into` seconds into them: past their end,
/// just before the end of the last pass (the frame it holds), never further on.
fn position_in(passes: &[PlanPass], into: f64) -> f64 {
    plan_point(passes, into).map_or(0.0, |p| p.position)
}

/// Seconds into `passes` at which a movie following them first stands at file second `at`: the
/// start of the pass that holds it (a point in a pass that repeats is its first play), the start
/// of the next pass for a point in a gap or before the first (the nearest trim), and for a point
/// at or past the end trim the end of the last pass's first play (as an Audio cue's seek: its
/// other plays follow). A pass that loops until devamped is left after one play.
fn action_at(passes: &[PlanPass], at: f64) -> f64 {
    let mut elapsed = 0.0;
    let mut end_trim = 0.0;
    for p in passes {
        if at < p.from {
            return elapsed;
        }
        if at < p.from + p.length || p.plays == 0 {
            return elapsed + (at - p.from).clamp(0.0, p.length);
        }
        end_trim = elapsed + p.length;
        elapsed += p.length * f64::from(p.plays);
    }
    end_trim
}

/// Where a devamp at `elapsed` samples into `table` leaves its loop: the table with the pass
/// under way the last of its slice (and the slice's later entries dropped, or everything after
/// with `stop`), and the samples into the table it leaves on. `None` when the table has ended
/// there or a play count would not fit.
fn devamped(table: &[Slice], elapsed: SampleTime, stop: bool) -> Option<(Vec<Slice>, SampleTime)> {
    let mut start = 0u64;
    for (i, s) in table.iter().enumerate() {
        let whole = match s.plays {
            Passes::Times(n) => s.frames.saturating_mul(u64::from(n)),
            Passes::Infinite => u64::MAX,
        };
        let into = elapsed.saturating_sub(start);
        if into < whole && s.frames > 0 {
            let pass = into / s.frames;
            let leaves = start.saturating_add((pass + 1).saturating_mul(s.frames));
            let mut out: Vec<Slice> = table.get(..i).unwrap_or_default().to_vec();
            out.push(Slice {
                plays: Passes::Times(u32::try_from(pass + 1).ok()?),
                ..*s
            });
            if !stop {
                // The rest of this slice's plays (a loop split by a start part way in) are
                // left; the slices after it play as planned.
                out.extend(table.iter().skip(i + 1).skip_while(|n| n.index == s.index));
            }
            return Some((out, leaves));
        }
        start = start.saturating_add(whole);
    }
    None
}

impl Engine {
    /// Sends a picture message to the stage host, if one runs.
    fn picture(&self, message: ToStage) {
        if let Some(host) = self.stages_host() {
            host.picture(message);
        }
    }

    /// Tells the stage host the clock now, when it moved far enough since it was last told
    /// (or `force`) and any picture is up.
    pub(super) fn picture_clock(&mut self, force: bool) {
        if self.pictures.live.is_empty() {
            return;
        }
        let now = self.now();
        let every = u64::from(self.rate.get()) * CLOCK_EVERY_MS / 1000;
        if !force
            && self
                .pictures
                .clock_sent
                .is_some_and(|sent| now.saturating_sub(sent) < every.max(1))
        {
            return;
        }
        self.pictures.clock_sent = Some(now);
        let clock = ToStage::Clock {
            sample: now,
            rate: self.rate.get(),
        };
        match self.stages_host() {
            // Before a play or a probe the clock must arrive; between them, only the newest
            // matters.
            Some(host) if force => host.picture(clock),
            Some(host) => host.routine_clock(clock),
            None => {}
        }
    }

    /// A stage host started: it is sent the clock and every picture again.
    pub(super) fn resend_pictures(&mut self) {
        if self.pictures.live.is_empty() {
            return;
        }
        for l in self.pictures.live.values_mut() {
            l.opened = false;
        }
        self.pictures.clock_sent = None;
        self.picture_clock(true);
        let messages: Vec<ToStage> = self
            .pictures
            .live
            .values()
            .filter(|l| !l.held_back)
            .flat_map(Live::messages)
            .collect();
        for m in messages {
            self.picture(m);
        }
    }

    /// The stage host has `picture`'s file open; `true` if it was opened ahead (the docs then
    /// open their next).
    pub(super) fn picture_opened(&mut self, picture: u64) -> bool {
        if self.prime_opened(picture) {
            return true;
        }
        if let Some(l) = self.pictures.live.get_mut(&picture) {
            l.opened = true;
        }
        false
    }

    /// The stage host stopped. Only a crash of its own (`crashed`: not a kill by the
    /// supervisor or from outside) can have been caused by a picture's file, and only by one
    /// it was opening or decoding then: one not yet open, or a movie playing (a still once open,
    /// and a movie paused or not yet played, have no decoder running). Those are suspects.
    /// - One tried alone after a hold-back (`trial`) when the crash came is the culprit, its
    ///   cue breaks, if nothing else was opening or decoding then (a GO meanwhile, a picture
    ///   opened ahead); else it is a suspect with the others.
    /// - A suspect at a second crash within `SUSPECT_WINDOW_S`, if it is the only one, is the
    ///   culprit too. If several are, they are held back (kept off the stage host, their cues
    ///   running unseen) and tried again one at a time, the earliest suspected last
    ///   (`try_held_back_pictures`), so the bad file ends up alone and only its cue breaks.
    /// - Everything else is sent to the next stage host.
    /// - A picture being opened ahead at the crash is a suspect too, counted by its file (a
    ///   run's picture of that file later takes the count over). A file opened ahead that is
    ///   the only repeat suspect is not opened ahead again; with others, the running ones are
    ///   tried alone and nothing is opened ahead meanwhile, so it ends up alone too.
    pub(super) fn pictures_host_stopped(&mut self, crashed: bool) {
        let ahead = self.primes_host_stopped(crashed);
        for l in self.pictures.live.values_mut() {
            l.trial = l.trial.filter(|_| crashed);
        }
        if !crashed {
            return;
        }
        let now = self.now();
        let window = seconds_to_samples(SUSPECT_WINDOW_S, self.rate);
        let probation = seconds_to_samples(PROBATION_S, self.rate);
        let on_trial: Vec<u64> = self
            .pictures
            .live
            .iter()
            .filter(|(_, l)| l.trial.is_some_and(|t| now.saturating_sub(t) < probation))
            .map(|(id, _)| *id)
            .collect();
        // The pictures on trial are the culprit only if nothing else was opening or decoding
        // (a GO meanwhile, or a picture opened ahead): else they are suspects with the rest.
        let others = ahead.is_some()
            || (self.pictures.live.iter())
                .any(|(id, l)| !on_trial.contains(id) && !l.held_back && decoding(l));
        if !on_trial.is_empty() && !others {
            for picture in on_trial {
                self.picture_failed(
                    picture,
                    "the video output crashed while it played this file".to_owned(),
                );
            }
            return;
        }
        let mut repeat = Vec::new();
        for (id, l) in &mut self.pictures.live {
            l.trial = None;
            if !decoding(l) || l.held_back {
                continue;
            }
            if l.suspected_at
                .is_none_or(|t| now.saturating_sub(t) >= window)
            {
                l.suspected = 0;
                l.suspected_at = Some(now);
            }
            l.suspected += 1;
            if l.suspected >= 2 {
                repeat.push(*id);
            }
        }
        let ahead = ahead.filter(|file| self.pictures.primes.suspect(file, now, window) >= 2);
        match (repeat.as_slice(), ahead) {
            ([], None) => {}
            ([], Some(file)) => self.bar_prime_file(&file),
            ([culprit], None) => self.picture_failed(
                *culprit,
                "the video output crashed twice while it played this file".to_owned(),
            ),
            (several, _) => {
                for id in several {
                    if let Some(l) = self.pictures.live.get_mut(id) {
                        l.held_back = true;
                    }
                }
            }
        }
    }

    /// Each tick: a picture held back after crashes is sent again alone once the stage host
    /// is up and no other is on trial, the latest suspected first; one whose trial passed
    /// without a crash is cleared.
    pub(super) fn try_held_back_pictures(&mut self, now: SampleTime) {
        let probation = seconds_to_samples(PROBATION_S, self.rate);
        let mut trying = false;
        for l in self.pictures.live.values_mut() {
            match l.trial {
                Some(t) if now.saturating_sub(t) >= probation => {
                    l.trial = None;
                    l.suspected = 0;
                    l.suspected_at = None;
                }
                Some(_) => trying = true,
                None => {}
            }
        }
        if trying || !self.stages.up {
            return;
        }
        let next = self
            .pictures
            .live
            .iter()
            .filter(|(_, l)| l.held_back)
            .max_by_key(|(id, l)| (l.suspected_at, **id))
            .map(|(id, _)| *id);
        let Some(id) = next else { return };
        let Some(l) = self.pictures.live.get_mut(&id) else {
            return;
        };
        l.held_back = false;
        l.trial = Some(now);
        l.opened = false;
        let messages = l.messages();
        self.picture_clock(true);
        for m in messages {
            self.picture(m);
        }
    }

    /// Whether the pictures tried alone after crashes are all done since the last call (passed
    /// or broken): pictures may be opened ahead again.
    pub(super) fn trials_ended(&mut self) -> bool {
        let on = self.pictures_on_trial();
        let ended = self.pictures.on_trial && !on;
        self.pictures.on_trial = on;
        ended
    }

    /// Pictures are being tried alone after crashes (held back, or on trial): nothing is
    /// opened ahead meanwhile, so a crash names the picture on trial.
    fn pictures_on_trial(&self) -> bool {
        (self.pictures.live.values()).any(|l| l.held_back || l.trial.is_some())
    }

    /// The stage host could not decode or draw `picture`: its cue breaks at its doc's next
    /// tick.
    pub(super) fn picture_failed(&mut self, picture: u64, reason: String) {
        if let Some(l) = self.pictures.live.remove(&picture) {
            let waited = l.wait.is_some();
            self.pictures
                .failed
                .push((l.doc, l.cue, l.run, reason, waited));
        }
    }

    /// Pictures whose stop is over at `now` are forgotten.
    pub(super) fn prune_pictures(&mut self, now: SampleTime) {
        let before = self.pictures.live.len();
        self.pictures
            .live
            .retain(|_, l| l.ends.is_none_or(|end| end >= now));
        // The clock past their ends, so the stage host drops them now: with no picture left up
        // no more clocks go, and one held short of an end would keep its last frame drawn.
        if self.pictures.live.len() < before {
            self.picture(ToStage::Clock {
                sample: now,
                rate: self.rate.get(),
            });
            self.pictures.clock_sent = Some(now);
        }
    }

    /// A doc closed: its pictures go.
    pub(super) fn drop_doc_pictures(&mut self, doc: DocId) {
        self.drop_doc_primes(doc);
        let gone: Vec<u64> = self
            .pictures
            .live
            .iter()
            .filter(|(_, l)| l.doc == doc)
            .map(|(id, _)| *id)
            .collect();
        for picture in gone {
            self.pictures.live.remove(&picture);
            self.picture(ToStage::Unload { picture });
        }
        self.pictures.held.retain(|h| h.doc != doc);
    }

    /// Test support: the stage host is told the clock now, before it draws for a probe.
    #[cfg(feature = "test-support")]
    pub(super) fn picture_clock_now(&mut self) {
        let now = self.now();
        self.pictures.clock_sent = Some(now);
        self.picture(ToStage::Clock {
            sample: now,
            rate: self.rate.get(),
        });
    }

    /// The cue of one of `doc`'s pictures up (or stopping) on `stage`: the stage its load
    /// put it on, whatever stage the cue has been aimed at since.
    pub(super) fn cue_with_picture_on(&self, doc: DocId, stage: StageId) -> Option<CueId> {
        self.pictures
            .live
            .values()
            .find(|l| {
                l.doc == doc && matches!(&l.load, ToStage::Load { stage: s, .. } if *s == stage)
            })
            .map(|l| l.cue)
    }

    /// Pictures up, every doc's (tests).
    pub(super) fn pictures_live(&self) -> usize {
        self.pictures.live.len()
    }

    /// Progress of this doc's Video cues whose picture has played: (cue, elapsed seconds into
    /// the action, remaining seconds if it has an end, seconds into a video's file). Elapsed
    /// follows the engine clock from the picture's play (its load-to-time offset included)
    /// and holds while paused; a still with no duration has no end, as any cue without one.
    pub(super) fn picture_progress(
        &self,
        doc: DocId,
        rendered: SampleTime,
    ) -> Vec<(CueId, f64, Option<f64>, Option<f64>)> {
        let rate = self.rate.get();
        self.pictures
            .live
            .values()
            .filter(|l| l.doc == doc)
            .filter_map(|l| {
                let (from, offset) = l.motion?;
                let elapsed = match from {
                    Some(at) => offset + seconds(rendered.saturating_sub(at), rate),
                    None => offset,
                };
                let elapsed = match l.length {
                    Some(length) => elapsed.min(length),
                    None => elapsed,
                };
                let remaining = l.length.map(|length| (length - elapsed).max(0.0));
                // A movie's place in its file: where its plan is (inside a loop, the loop's
                // pass), held on its last frame once the plan is over.
                let position = match &l.plan {
                    Some(p) => Some(position_in(&p.passes, elapsed)),
                    None => (!l.still).then_some(elapsed),
                };
                Some((l.cue, elapsed, remaining, position))
            })
            .collect()
    }

    /// Where in its file each Video cue's picture that is loaded and not yet played stands (the
    /// frame it opens on, seconds), for a loaded cue's position line. A still has none.
    pub(super) fn loaded_pictures(&self, doc: DocId) -> Vec<(CueId, f64)> {
        self.pictures
            .live
            .values()
            .filter(|l| l.doc == doc && l.motion.is_none() && !l.still && l.wait.is_none())
            .filter_map(|l| match &l.load {
                ToStage::Load { offset, .. } => Some((l.cue, *offset)),
                _ => None,
            })
            .collect()
    }

    /// Pictures up whose file the stage host has open (tests).
    pub(super) fn pictures_opened(&self) -> usize {
        self.pictures.live.values().filter(|l| l.opened).count()
    }

    /// Pictures being tried alone after crashes (tests).
    pub(super) fn picture_trials(&self) -> usize {
        self.pictures
            .live
            .values()
            .filter(|l| l.trial.is_some())
            .count()
    }

    /// Starts waiting for their picture's open, not paused (tests).
    pub(super) fn pictures_waiting(&self) -> usize {
        (self.pictures.live.values())
            .filter(|l| l.wait.is_some_and(|w| !w.paused))
            .count()
    }

    /// Whether a start may wait for its picture's open: a stage host is up that draws
    /// pictures. Otherwise nothing would answer, and the cue plays unseen as it would anyway.
    fn opens_pictures(&self) -> bool {
        self.stages_host().is_some() && self.stages.up && self.stages.video == Some(true)
    }
}

/// Whether the stage host has a decoder running for `l`: opening it, or a movie playing (a
/// still once open, and a movie paused or not yet played, have none).
fn decoding(l: &Live) -> bool {
    !l.opened || (!l.still && matches!(l.motion, Some((Some(_), _))))
}

/// `x` less a lateness of `late` frames (more for a negative one), never below 0.
fn less_late(x: SampleTime, late: i64) -> SampleTime {
    if late >= 0 {
        x.saturating_sub(late.unsigned_abs())
    } else {
        x.saturating_add(late.unsigned_abs())
    }
}

/// `x` plus a lateness of `late` frames (less for a negative one), never below 0.
fn plus_late(x: SampleTime, late: i64) -> SampleTime {
    if late >= 0 {
        x.saturating_add(late.unsigned_abs())
    } else {
        x.saturating_sub(late.unsigned_abs())
    }
}

fn seconds(samples: SampleTime, rate: u32) -> f64 {
    samples as f64 / f64::from(rate.max(1))
}

impl DocCx<'_> {
    /// What `cue` plays, if it is a Video cue whose file has been probed and plays.
    fn plays(&self, cue: CueId) -> Result<Plays, Refused> {
        let Some(CueKind::Video {
            media: Some(media),
            stage: Some(stage),
            duration,
            geometry,
            timing,
            ..
        }) = self.cue(cue).map(|c| &c.kind)
        else {
            return Err(Refused::Failed("the cue has no file or stage"));
        };
        let key = media.as_str();
        let Some(ready) = self.doc.media.picture(key) else {
            // Not probed yet, or being probed again after it failed (the file came back, or
            // was changed): the answer decides.
            return Err(match self.doc.media.get(key) {
                None | Some(Probe::Pending) => Refused::Pending,
                Some(_) if self.doc.media.probing(key) => Refused::Pending,
                Some(_) => Refused::Failed("its file cannot be played"),
            });
        };
        let path = ready
            .path
            .to_str()
            .ok_or(Refused::Failed("its file's path is not valid Unicode"))?
            .to_owned();
        Ok(match ready.report {
            ProbeReport::Video { duration: d, .. } => Plays {
                path,
                still: false,
                stage: *stage,
                geometry: *geometry,
                length: None,
                movie: Some((
                    self.sound_info(cue)
                        .unwrap_or_else(|| picture_info(d, self.eng.rate)),
                    (**timing).clone(),
                )),
            },
            ProbeReport::Still { .. } => Plays {
                path,
                still: true,
                stage: *stage,
                geometry: *geometry,
                length: *duration,
                movie: None,
            },
            ProbeReport::Missing | ProbeReport::Unsupported { .. } => {
                return Err(Refused::Failed("its file cannot be played"));
            }
        })
    }

    /// Whether `action` is a voice action of a Video cue's run (carried out here, not by the
    /// mixer's voices).
    pub(super) fn is_picture_action(&self, action: &Action) -> bool {
        let cue = match action {
            Action::LoadVoice { cue, .. }
            | Action::UnloadVoice { cue, .. }
            | Action::StartVoice { cue, .. }
            | Action::SeekVoice { cue, .. }
            | Action::StopVoice { cue, .. }
            | Action::PreviewStart { cue, .. }
            | Action::PreviewStop { cue, .. }
            | Action::PauseVoice { cue, .. }
            | Action::ResumeVoice { cue, .. }
            | Action::Ramp { cue, .. }
            | Action::DuckVoice { cue, .. }
            | Action::DevampVoice { cue, .. } => *cue,
            _ => return false,
        };
        // A run whose picture is up (or waits) stays one even if its cue was edited into
        // another kind.
        self.picture_of_cue(cue).is_some()
            || self
                .eng
                .pictures
                .held
                .iter()
                .any(|h| h.doc == self.doc.id && h.cue == cue)
            || matches!(self.cue(cue).map(|c| &c.kind), Some(CueKind::Video { .. }))
    }

    /// `cue` in the workspace, found through the media catalog's cue index (not a walk of
    /// every cue: this runs for every voice action).
    fn cue(&self, cue: CueId) -> Option<&openlab_core::Cue> {
        let ws = self.doc.model.ws();
        let index = self.doc.media.index();
        if index.contains(cue) {
            index.locate(ws, cue)
        } else {
            None
        }
    }

    /// After an edit: each of this doc's pictures whose cue's geometry changed is sent its new
    /// geometry, so the stage follows at once without restarting the picture (VID-04). Its
    /// `load` is kept up to date too, so a stage host that starts again draws it so. Costs
    /// one index lookup per picture up, whatever the show's size.
    pub(super) fn follow_picture_geometry(&mut self) {
        let doc = self.doc.id;
        let changed: Vec<(u64, VideoGeometry)> = self
            .eng
            .pictures
            .live
            .iter()
            .filter(|(_, l)| l.doc == doc)
            .filter_map(|(id, l)| {
                let ToStage::Load { geometry: sent, .. } = &l.load else {
                    return None;
                };
                match self.cue(l.cue).map(|c| &c.kind) {
                    Some(CueKind::Video { geometry, .. }) if geometry != sent => {
                        Some((*id, *geometry))
                    }
                    _ => None,
                }
            })
            .collect();
        for (picture, geometry) in changed {
            let Some(live) = self.eng.pictures.live.get_mut(&picture) else {
                continue;
            };
            if let ToStage::Load { geometry: sent, .. } = &mut live.load {
                *sent = geometry;
            }
            if !live.held_back {
                self.eng.picture(ToStage::Geometry { picture, geometry });
            }
        }
    }

    /// Whether a picture of `cue` is up.
    pub(super) fn has_picture(&self, cue: CueId) -> bool {
        self.picture_of_cue(cue).is_some()
    }

    fn picture_of_cue(&self, cue: CueId) -> Option<u64> {
        let doc = self.doc.id;
        self.eng
            .pictures
            .live
            .iter()
            .find(|(_, l)| l.doc == doc && l.cue == cue)
            .map(|(id, _)| *id)
    }

    /// Carries out a Video cue run's voice action on the stage host.
    pub(super) fn picture_action(&mut self, action: &Action, fx: &mut Effects) {
        let rate = self.eng.rate.get();
        let doc = self.doc.id;
        match *action {
            // A Video cue's preview plays through the stage as its run does, from its start;
            // its run id is the preview's own, so it is no show run and ends on its own length
            // (the cue engine's timed preview).
            Action::PreviewStart { cue, run, at } => {
                let start = Action::StartVoice {
                    cue,
                    run,
                    at,
                    offset: 0,
                };
                self.picture_action(&start, fx)
            }
            Action::PreviewStop { cue, run, at, fade } => {
                let stop = Action::StopVoice { cue, run, at, fade };
                self.picture_action(&stop, fx)
            }
            Action::LoadVoice { cue, run, offset } => match self.load_picture(cue, run, offset) {
                Ok(()) => self.sound_action(action, fx),
                Err(Refused::Pending) => self.eng.pictures.held.push(Held {
                    doc,
                    cue,
                    run,
                    offset,
                    start: None,
                    since: None,
                    paused: false,
                }),
                Err(Refused::Failed(why)) => self.picture_refused(cue, run, why, false, fx),
            },
            Action::StartVoice {
                cue,
                run,
                at,
                offset,
            } => {
                if let Some(h) = self.eng.pictures.held_mut(doc, run) {
                    h.start = Some((at, offset));
                    return self.hold_start(cue, run, fx);
                }
                // A picture loaded on a file that has changed since (relinked, replaced, or
                // probed again) is let go and loaded afresh, waiting for the probe if need be.
                let loaded = self.eng.pictures.find(doc, run);
                if let Some(id) = loaded
                    && !self.still_plays(id, cue)
                {
                    self.eng.pictures.live.remove(&id);
                    self.eng.picture(ToStage::Unload { picture: id });
                    self.sound_action(&Action::UnloadVoice { cue, run }, fx);
                }
                let loaded = self.eng.pictures.find(doc, run).is_some();
                if !loaded {
                    match self.load_picture(cue, run, offset) {
                        Ok(()) => {}
                        Err(Refused::Pending) => {
                            self.eng.pictures.held.push(Held {
                                doc,
                                cue,
                                run,
                                offset,
                                start: Some((at, offset)),
                                since: None,
                                paused: false,
                            });
                            return self.hold_start(cue, run, fx);
                        }
                        Err(Refused::Failed(why)) => {
                            return self.picture_refused(cue, run, why, true, fx);
                        }
                    }
                }
                // A cue loaded beforehand loaded its sound with its picture.
                self.start_or_wait(cue, run, (at, offset), offset, !loaded, fx);
            }
            Action::ResumeVoice {
                cue,
                run,
                paused,
                at,
                offset,
            } => {
                if let Some(h) = self.eng.pictures.held_mut(doc, paused) {
                    h.run = run;
                    h.paused = false;
                    h.start = Some((at, offset));
                    return;
                }
                let Some(id) = self.eng.pictures.find(doc, paused) else {
                    return;
                };
                let now = self.now();
                let mut waits = false;
                if let Some(l) = self.eng.pictures.live.get_mut(&id) {
                    l.run = run;
                    // Paused before its picture opened: it waits again, from where its action
                    // is now, its sound loaded under the new run.
                    if let Some(w) = l.wait.as_mut() {
                        *w = Wait {
                            at,
                            offset,
                            since: now,
                            paused: false,
                            ..*w
                        };
                        waits = true;
                    }
                }
                if waits {
                    self.sound_action(&Action::UnloadVoice { cue, run: paused }, fx);
                    let from = self.picture_wait(id).map_or(offset, |w| w.from);
                    self.load_sound_for_wait(cue, run, from, fx);
                    self.start_if_open(id, fx);
                    return;
                }
                self.play_picture(cue, run, at, offset, 0, false, fx);
                // Its sound resumes on the same sample (a picture that could not play on has
                // failed or waits, and its cue's end takes the sound).
                if self.eng.pictures.find(doc, run).is_some() {
                    self.sound_action(action, fx);
                }
            }
            Action::PauseVoice {
                run, at, offset, ..
            } => {
                if let Some(h) = self.eng.pictures.held_mut(doc, run) {
                    h.paused = true;
                    h.offset = offset;
                    h.start = None;
                    return;
                }
                if let Some(id) = self.eng.pictures.find(doc, run) {
                    // A start still waiting for its open has shown and sounded nothing: it
                    // waits on, paused.
                    if let Some(w) =
                        (self.eng.pictures.live.get_mut(&id)).and_then(|l| l.wait.as_mut())
                    {
                        w.paused = true;
                        return;
                    }
                    // A movie holds where its plan is then; a still has no position.
                    let late = self.eng.pictures.live.get(&id).map_or(0, Live::late);
                    // The picture's own time: the action's less its lateness.
                    let own = seconds(less_late(offset, late), rate);
                    let at_file = self.eng.pictures.live.get(&id).map_or(own, |l| {
                        l.plan.as_ref().map_or(own, |p| position_in(&p.passes, own))
                    });
                    let message = ToStage::Pause {
                        picture: id,
                        at,
                        offset: at_file.min(openlab_core::MAX_TIME),
                        action: own.min(openlab_core::MAX_TIME),
                    };
                    if let Some(l) = self.eng.pictures.live.get_mut(&id) {
                        l.now = Some(message.clone());
                        // Held at the frame it pauses on.
                        l.motion = Some((None, own));
                    }
                    self.eng.picture(message);
                    self.sound_action(action, fx);
                }
            }
            Action::SeekVoice {
                cue,
                run,
                from,
                at,
                position,
            } => self.seek_picture(cue, run, from, at, position, fx),
            Action::StopVoice {
                cue, run, at, fade, ..
            } => {
                let frames = fade.map_or(0, |f| f.frames);
                self.sound_action(action, fx);
                self.eng.pictures.take_held(doc, run);
                // The stop (its fade, if any) is over on this sample, as a mixer voice's is; an
                // action that ends sooner (its range's end, or a pending devamp's stop) ends
                // picture, sound and cue there.
                let mut end = at.saturating_add(frames);
                if let Some(id) = self.eng.pictures.find(doc, run) {
                    end = self.stop_picture(id, at, frames);
                }
                let ended = Input::ActionEnded { cue, run, at: end };
                fx.pictured.push((Some(end), ended));
            }
            Action::UnloadVoice { run, .. } => {
                self.sound_action(action, fx);
                self.eng.pictures.take_held(doc, run);
                if let Some(id) = self.eng.pictures.find(doc, run) {
                    self.eng.pictures.live.remove(&id);
                    self.eng.picture(ToStage::Unload { picture: id });
                }
            }
            Action::DevampVoice { cue, run, at, stop } => {
                // A start waiting for its open is devamped once it plays.
                let waiting = (self.eng.pictures.find(doc, run))
                    .and_then(|id| self.eng.pictures.live.get_mut(&id))
                    .and_then(|l| l.wait.as_mut());
                if let Some(w) = waiting {
                    w.devamp = Some(stop);
                    return;
                }
                self.devamp_picture(cue, run, at, stop, fx);
                // Its sound leaves the loop with it: only a devamp the picture carried out.
                let carried = (self.eng.pictures.find(doc, run))
                    .and_then(|id| self.eng.pictures.live.get(&id))
                    .and_then(|l| l.plan.as_ref())
                    .is_some_and(|p| matches!(p.devamp, Some((s, _)) if s == stop));
                if carried {
                    self.sound_action(action, fx);
                }
            }
            Action::Ramp {
                run,
                at,
                param: LevelParam::Picture(param),
                gain,
                frames,
                ref curve,
                ..
            } => self.ramp_picture(run, param, at, f64::from(gain), frames, curve.clone()),
            // Levels and ducks act on its sound.
            _ => self.sound_action(action, fx),
        }
    }

    /// Where in its action (seconds) a Video cue's plan first stands at `file` seconds into its
    /// movie, for a seek: `None` for a cue that is not a probed movie (a still has no timeline;
    /// a file not probed has no plan) or a point that is not a time.
    pub(super) fn video_seek_point(&self, cue: CueId, file: f64) -> Option<f64> {
        if !file.is_finite() {
            return None;
        }
        let (info, timing) = self.plays(cue).ok()?.movie?;
        let (top, _) = movie_table(&info, &timing, self.eng.rate).ok()?;
        Some(action_at(&passes(&top, self.eng.rate.get()), file.max(0.0)))
    }

    /// A Video cue's run moved to `position` samples into its action (`Action::SeekVoice`) and
    /// goes on as `run`. A picture waiting for its probe takes the point; a paused one is held
    /// on the frame there; a playing one plays on from it on sample `at`, its plan as it was.
    /// From the point on the cue engine counts the action as the picture's own time, so a late
    /// start before the seek is behind it. Its sound goes on from the same point of the same
    /// plan, as a voice of its own if the old one has ended (hold at end); a point past the
    /// plan's end leaves it nothing to play.
    fn seek_picture(
        &mut self,
        cue: CueId,
        run: RunId,
        from: RunId,
        at: SampleTime,
        position: SampleTime,
        fx: &mut Effects,
    ) {
        let doc = self.doc.id;
        let rate = self.eng.rate.get();
        if let Some(h) = self.eng.pictures.held_mut(doc, from) {
            h.run = run;
            h.offset = position;
            if h.start.is_some() {
                h.start = Some((at, position));
            }
            return;
        }
        let Some(id) = self.eng.pictures.find(doc, from) else {
            return;
        };
        if let Some(w) = self.picture_wait(id) {
            return self.seek_waiting_picture(id, cue, (run, from), at, position, w, fx);
        }
        let Some(live) = self.eng.pictures.live.get_mut(&id) else {
            return;
        };
        live.run = run;
        let paused = matches!(live.motion, Some((None, _)));
        if let Some(p) = live.plan.as_mut() {
            p.late = 0;
            p.followed = 0;
        }
        live.still_late = 0;
        let own = seconds(position, rate);
        if paused {
            let file = live
                .plan
                .as_ref()
                .map_or(own, |p| position_in(&p.passes, own));
            let message = ToStage::Pause {
                picture: id,
                at,
                offset: file.min(openlab_core::MAX_TIME),
                action: own.min(openlab_core::MAX_TIME),
            };
            live.now = Some(message.clone());
            live.motion = Some((None, own));
            self.eng.picture(message);
        } else {
            self.play_picture(cue, run, at, position, 0, false, fx);
        }
        // What the picture plays from the point (a paused one, from its resume): nothing if it
        // could not play on (it failed, or waits for its file again and starts its sound then).
        let rest = (self.eng.pictures.find(doc, run))
            .and_then(|id| self.eng.pictures.live.get(&id))
            .and_then(|l| l.plan.as_ref())
            .and_then(|p| {
                let (table, into) = if paused {
                    (p.settled(position), position)
                } else {
                    (p.table.as_slice(), p.into)
                };
                rest_of_table(table, &[], false, into)
            })
            .unwrap_or_default();
        let to = SeekTo::Plan {
            table: rest,
            rate,
            offset: position,
            paused,
        };
        self.sound_seek(cue, (run, from), at, to, fx);
    }

    /// A start waits for its file's probe: said once, and the hold limit starts.
    fn hold_start(&mut self, cue: CueId, run: RunId, fx: &mut Effects) {
        let now = self.now();
        let Some(h) = self.eng.pictures.held_mut(self.doc.id, run) else {
            return;
        };
        if h.since.is_some() {
            return;
        }
        h.since = Some(now);
        fx.notices.push((
            "probe_wait",
            "a cue waits for its video file to be read; it starts once it is".to_owned(),
            Some(cue),
        ));
    }

    /// Starts the run's picture and its sound, dated `at` with the action `offset` into its time
    /// then, from `from` into it: at once if the stage host has its file open (or nothing would
    /// open it), else once it has (`start_waiting_pictures`), its sound loaded meanwhile when
    /// `load_sound` (a cue loaded beforehand loaded it with its picture).
    fn start_or_wait(
        &mut self,
        cue: CueId,
        run: RunId,
        (at, offset): (SampleTime, SampleTime),
        from: SampleTime,
        load_sound: bool,
        fx: &mut Effects,
    ) {
        let Some(id) = self.eng.pictures.find(self.doc.id, run) else {
            return;
        };
        let since = self.now();
        // Started with others (one GO's sequence): they start together once every picture
        // among them is open, which the batch's end decides (`end_sequence`).
        let group = self.eng.pictures.sequence.batch();
        if let Some(l) = self.eng.pictures.live.get_mut(&id) {
            l.wait = Some(Wait {
                at,
                offset,
                from,
                since,
                paused: false,
                devamp: None,
                group,
            });
        }
        let started = group.is_none() && self.start_if_open(id, fx);
        if !started && load_sound {
            self.load_sound_for_wait(cue, run, from, fx);
        }
    }

    /// Plays the run's picture and sound from `from` into its action on sample `on` (not before
    /// `at`): the action was `offset` into its time on `at`, so what it has run past `from` by
    /// then is late, counted into its length. A devamp asked while it waited follows.
    #[allow(clippy::too_many_arguments)]
    fn play_from(
        &mut self,
        cue: CueId,
        run: RunId,
        (at, offset): (SampleTime, SampleTime),
        from: SampleTime,
        on: SampleTime,
        devamp: Option<bool>,
        fx: &mut Effects,
    ) {
        let on = on.max(at);
        let late = offset.saturating_add(on - at).saturating_sub(from);
        self.play_picture(cue, run, on, from, late, true, fx);
        if late > 0 {
            fx.notices.push((
                "cold_start",
                "a cue started late: its video file had to be read first".to_owned(),
                Some(cue),
            ));
        }
        if let Some(stop) = devamp
            && self.eng.pictures.find(self.doc.id, run).is_some()
        {
            let devamp = Action::DevampVoice {
                cue,
                run,
                at: on,
                stop,
            };
            self.picture_action(&devamp, fx);
        }
    }

    /// Loads the sound of a start that waits for its picture's open, so its stream fills
    /// meanwhile: on its primed stream for a start from the top, if it has one.
    fn load_sound_for_wait(&mut self, cue: CueId, run: RunId, from: SampleTime, fx: &mut Effects) {
        if from == 0 && self.load_primed_sound(cue, run, fx) {
            return;
        }
        self.sound_action(
            &Action::LoadVoice {
                cue,
                run,
                offset: from,
            },
            fx,
        );
    }

    fn picture_wait(&self, id: u64) -> Option<Wait> {
        self.eng.pictures.live.get(&id).and_then(|l| l.wait)
    }

    /// A seek of a start still waiting for its picture's open: the picture is loaded again at
    /// the point and the run waits for that open from there (or stays paused), its sound
    /// loaded there; what it waited before is behind it.
    #[allow(clippy::too_many_arguments)]
    fn seek_waiting_picture(
        &mut self,
        id: u64,
        cue: CueId,
        (run, from): (RunId, RunId),
        at: SampleTime,
        position: SampleTime,
        wait: Wait,
        fx: &mut Effects,
    ) {
        let doc = self.doc.id;
        self.eng.pictures.live.remove(&id);
        self.eng.picture(ToStage::Unload { picture: id });
        self.sound_action(&Action::UnloadVoice { cue, run: from }, fx);
        match self.load_picture(cue, run, position) {
            Ok(()) => {}
            Err(Refused::Pending) => {
                self.eng.pictures.held.push(Held {
                    doc,
                    cue,
                    run,
                    offset: position,
                    start: (!wait.paused).then_some((at, position)),
                    since: None,
                    paused: wait.paused,
                });
                if !wait.paused {
                    self.hold_start(cue, run, fx);
                }
                return;
            }
            Err(Refused::Failed(why)) => return self.picture_refused(cue, run, why, true, fx),
        }
        if wait.paused {
            let since = self.now();
            if let Some(l) = (self.eng.pictures.find(doc, run))
                .and_then(|id| self.eng.pictures.live.get_mut(&id))
            {
                l.wait = Some(Wait {
                    at,
                    offset: position,
                    from: position,
                    since,
                    ..wait
                });
            }
            return;
        }
        self.start_or_wait(cue, run, (at, position), position, true, fx);
        let Some(stop) = wait.devamp else { return };
        let waiting = (self.eng.pictures.find(doc, run))
            .and_then(|id| self.eng.pictures.live.get_mut(&id))
            .and_then(|l| l.wait.as_mut());
        match waiting {
            Some(w) => w.devamp = Some(stop),
            None => self.picture_action(&Action::DevampVoice { cue, run, at, stop }, fx),
        }
    }

    /// Each tick: a start waiting for its picture's open plays once the stage host has the file
    /// open, or at once when the stage host is down or the picture is held back after crashes
    /// (it plays unseen, as those do); one still waiting `OPEN_WAIT_LIMIT_S` after it began
    /// breaks its cue (the stage host never answered), as a start that failed: nothing was seen
    /// or heard.
    pub(super) fn start_waiting_pictures(&mut self, now: SampleTime, fx: &mut Effects) {
        self.start_lone_sounds(fx);
        let doc = self.doc.id;
        let limit = seconds_to_samples(OPEN_WAIT_LIMIT_S, self.eng.rate);
        let waiting: Vec<(u64, SampleTime)> = (self.eng.pictures.live.iter())
            .filter(|(_, l)| l.doc == doc)
            .filter_map(|(id, l)| Some((*id, l.wait.filter(|w| !w.paused)?.since)))
            .collect();
        for (id, since) in waiting {
            // Gone, or started with its sequence by another's open since the list was made.
            if self.picture_wait(id).is_none_or(|w| w.paused) {
                continue;
            }
            if self.start_if_open(id, fx) || now < since.saturating_add(limit) {
                continue;
            }
            let Some(l) = self.eng.pictures.live.remove(&id) else {
                continue;
            };
            let (cue, run) = (l.cue, l.run);
            self.eng.picture(ToStage::Unload { picture: id });
            let why = format!("its file took longer than {OPEN_WAIT_LIMIT_S} s to open");
            self.log(
                LogLevel::Warn,
                format!("a Video cue's picture failed: {why}"),
            );
            fx.notices.push((
                "video_decode_failed",
                format!("the Video cue's picture failed: {why}"),
                Some(cue),
            ));
            fx.feedback.push(Input::StartFailed {
                cue,
                run,
                reason: BrokenReason::DecodeFailed,
            });
        }
    }

    /// Picture `id`'s start, if it waits (not paused) and its file is open, or nothing would
    /// open it (the stage host down, the picture held back after crashes: it plays unseen, as
    /// those do), plays now. `false` while it still waits.
    fn start_if_open(&mut self, id: u64, fx: &mut Effects) -> bool {
        let opens = self.eng.opens_pictures();
        let Some(l) = self.eng.pictures.live.get_mut(&id) else {
            return false;
        };
        let go = l.opened || l.held_back || !opens;
        let Some(w) = l.wait.filter(|w| !w.paused) else {
            return false;
        };
        if let Some(group) = w.group {
            return self.start_sequence_if_open(group, fx);
        }
        if !go {
            return false;
        }
        l.wait = None;
        let (cue, run) = (l.cue, l.run);
        let on = w.at.max(self.now());
        self.play_from(cue, run, (w.at, w.offset), w.from, on, w.devamp, fx);
        true
    }

    /// The probe of `key` answered: the runs of this doc that waited for it load, and start if
    /// they were started (late, from their head, as a held audio start does); a file that
    /// cannot play breaks them.
    pub(super) fn release_pictures(&mut self, key: &str, now: SampleTime) {
        let doc = self.doc.id;
        let waiting: Vec<RunId> = self
            .eng
            .pictures
            .held
            .iter()
            .filter(|h| h.doc == doc)
            .filter(|h| {
                matches!(
                    self.cue(h.cue).map(|c| &c.kind),
                    Some(CueKind::Video { media: Some(m), .. }) if m.as_str() == key
                )
            })
            .map(|h| h.run)
            .collect();
        let mut fx = Effects::default();
        for run in waiting {
            let Some(h) = self.eng.pictures.take_held(doc, run) else {
                continue;
            };
            match self.load_picture(h.cue, h.run, h.offset) {
                Ok(()) => {}
                Err(Refused::Pending) => {
                    self.eng.pictures.held.push(h);
                    continue;
                }
                Err(Refused::Failed(why)) => {
                    self.picture_refused(h.cue, h.run, why, h.start.is_some(), &mut fx);
                    continue;
                }
            }
            if let (Some((at, offset)), false) = (h.start, h.paused) {
                // From its head, as a held audio start: what it waited is late.
                let from = offset;
                self.start_or_wait(h.cue, h.run, (at, offset), from, true, &mut fx);
            } else if h.paused && h.since.is_some() {
                // A start paused before its file was known waits, paused, for its open: the
                // resume plays it from where its action stood once the file is open.
                let live = (self.eng.pictures.find(doc, h.run))
                    .and_then(|id| self.eng.pictures.live.get_mut(&id));
                if let Some(l) = live {
                    l.wait = Some(Wait {
                        at: now,
                        offset: h.offset,
                        from: h.offset,
                        since: now,
                        paused: true,
                        devamp: None,
                        group: None,
                    });
                }
            }
        }
        self.follow(fx, now);
    }

    /// Starts that have waited for their file's probe since before `now - limit` fail: the cue
    /// goes broken instead of showing as running with nothing on stage.
    pub(super) fn expire_held_pictures(
        &mut self,
        now: SampleTime,
        limit: SampleTime,
        fx: &mut Effects,
    ) {
        let doc = self.doc.id;
        let expired: Vec<RunId> = self
            .eng
            .pictures
            .held
            .iter()
            .filter(|h| h.doc == doc)
            .filter(|h| h.since.is_some_and(|t| now >= t.saturating_add(limit)))
            .map(|h| h.run)
            .collect();
        for run in expired {
            let Some(h) = self.eng.pictures.take_held(doc, run) else {
                continue;
            };
            fx.feedback.push(Input::StartFailed {
                cue: h.cue,
                run: h.run,
                reason: BrokenReason::DecodeFailed,
            });
            fx.notices.push((
                "probe_timeout",
                "a cue's video file could not be read in time; the cue did not play".to_owned(),
                Some(h.cue),
            ));
        }
    }

    /// What `cue`'s picture loads `offset` into its action (picture id 0), its action's length
    /// (a still's duration) and whether it is a still.
    fn picture_load(
        &self,
        cue: CueId,
        offset: SampleTime,
    ) -> Result<(ToStage, Option<f64>, bool), Refused> {
        let plays = self.plays(cue)?;
        let rate = self.eng.rate.get();
        // A movie opens on the frame its plan is at there (inside its trims and loops).
        let at_file = match &plays.movie {
            Some((file, timing)) => {
                let (top, _) = movie_table(file, timing, self.eng.rate).map_err(Refused::Failed)?;
                position_in(&passes(&top, rate), seconds(offset, rate))
            }
            None => seconds(offset, rate),
        };
        let load = ToStage::Load {
            picture: 0,
            stage: plays.stage,
            file: plays.path,
            still: plays.still,
            offset: at_file.min(openlab_core::MAX_TIME),
            geometry: plays.geometry,
        };
        Ok((load, plays.length, plays.still))
    }

    /// Loads the run's picture `offset` into its action: the cue's picture opened ahead if it
    /// opens the same point, else a new one.
    fn load_picture(&mut self, cue: CueId, run: RunId, offset: SampleTime) -> Result<(), Refused> {
        let (mut load, length, still) = self.picture_load(cue, offset)?;
        let primed = self.take_picture_prime(cue, &load);
        let (id, opened) = match &primed {
            Some((id, opened, _)) => (*id, *opened),
            None => {
                self.eng.pictures.next += 1;
                (self.eng.pictures.next, false)
            }
        };
        if let ToStage::Load { picture, .. } = &mut load {
            *picture = id;
        }
        self.eng.pictures.live.insert(
            id,
            Live {
                doc: self.doc.id,
                cue,
                run,
                load: load.clone(),
                first: None,
                now: None,
                stop: None,
                ramps: BTreeMap::new(),
                ends: None,
                opened,
                suspected: 0,
                suspected_at: None,
                held_back: false,
                trial: None,
                wait: None,
                motion: None,
                length,
                still,
                still_late: 0,
                plan: None,
            },
        );
        // A file suspected of a crash while it was opened ahead stays suspected in the run.
        let file = match &load {
            ToStage::Load { file, .. } => file.as_str(),
            _ => "",
        };
        if let Some((count, since)) = self.eng.pictures.primes.take_suspicion(file)
            && let Some(l) = self.eng.pictures.live.get_mut(&id)
        {
            l.suspected = count;
            l.suspected_at = Some(since);
        }
        match primed {
            // Already on the stage host: only a geometry edited since it was opened goes.
            Some((_, _, ToStage::Load { geometry: was, .. })) => {
                if let ToStage::Load { geometry, .. } = load
                    && geometry != was
                {
                    self.eng.picture(ToStage::Geometry {
                        picture: id,
                        geometry,
                    });
                }
            }
            _ => self.eng.picture(load),
        }
        self.seed_picture_ramps(id, cue);
        Ok(())
    }

    /// A Fade cue moves one of the run's picture values (VID-05): from where it is on `at` (the
    /// last ramp's value then, else the cue's own) to `to` over `frames` with `curve`. A run
    /// whose picture waits for its probe has none yet: it is given the Fade's values when it
    /// loads (`seed_picture_ramps`).
    fn ramp_picture(
        &mut self,
        run: RunId,
        param: PictureParam,
        at: SampleTime,
        to: f64,
        frames: SampleTime,
        curve: RampCurve,
    ) {
        let Some(id) = self.eng.pictures.find(self.doc.id, run) else {
            return;
        };
        let Some(live) = self.eng.pictures.live.get_mut(&id) else {
            return;
        };
        let own = match &live.load {
            ToStage::Load { geometry, .. } => param.of(geometry),
            _ => param.of(&VideoGeometry::default()),
        };
        let from = live.ramps.get(&param).map_or(own, |r| r.value(at));
        // Within range on the link whatever the arithmetic: a value out of it is refused there.
        let ramp = PictureRamp {
            from: param.clamp(from),
            to: param.clamp(to),
            at,
            frames,
            curve,
        };
        live.ramps.insert(param, ramp.clone());
        if !live.held_back {
            self.eng.picture(ToStage::Ramp {
                picture: id,
                param,
                ramp,
            });
        }
    }

    /// A picture loaded for a run that Fade cues have already moved (one that waited for its
    /// probe, or loaded again on a changed file) starts with the values they gave it, each still
    /// moving to where it is going by its end.
    fn seed_picture_ramps(&mut self, id: u64, cue: CueId) {
        let now = self.now();
        let ramps: Vec<(PictureParam, PictureRamp)> = (self.doc.model.engine())
            .fade_ramps(cue, now)
            .into_iter()
            .filter_map(|l| {
                let param = l.param.picture()?;
                let moving = l.end > now;
                Some((
                    param,
                    PictureRamp {
                        from: param.clamp(f64::from(l.value)),
                        to: param.clamp(f64::from(l.to)),
                        at: now,
                        frames: if moving { l.end - now } else { 0 },
                        curve: l.curve,
                    },
                ))
            })
            .collect();
        let Some(live) = self.eng.pictures.live.get_mut(&id) else {
            return;
        };
        live.ramps.extend(ramps.iter().cloned());
        if live.held_back {
            return;
        }
        for (param, ramp) in ramps {
            self.eng.picture(ToStage::Ramp {
                picture: id,
                param,
                ramp,
            });
        }
    }

    /// Whether picture `id` is still of what `cue` plays now: the same file, probed and
    /// playable (a file changed since it was loaded is not).
    fn still_plays(&self, id: u64, cue: CueId) -> bool {
        let loaded = match self.eng.pictures.live.get(&id).map(|l| &l.load) {
            Some(ToStage::Load { file, .. }) => file.as_str(),
            _ => return false,
        };
        matches!(self.plays(cue), Ok(p) if p.path == loaded)
    }

    /// Plays the run's picture from sample `at`, `offset` into its action; `late` is how much
    /// later than the engine dated it the start is (a start held by its probe), counted into
    /// the action's length. A movie follows its plan (trims, slices and loops) from `offset`
    /// with its integrated fades: a `fresh` start plans it from the model, a resume plays on
    /// with the run's plan as its last devamp left it. With hold at end the cue is told no length. Never
    /// silent: a cue whose file is no longer playable fails, one whose file is being probed
    /// again waits for the answer as a start does.
    #[allow(clippy::too_many_arguments)]
    fn play_picture(
        &mut self,
        cue: CueId,
        run: RunId,
        at: SampleTime,
        offset: SampleTime,
        late: SampleTime,
        fresh: bool,
        fx: &mut Effects,
    ) {
        let doc = self.doc.id;
        let rate = self.eng.rate.get();
        let Some(id) = self.eng.pictures.find(doc, run) else {
            return;
        };
        let plays = match self.plays(cue) {
            Ok(p) => p,
            Err(Refused::Pending) => {
                // Probed again (its file changed or came back): it waits, unseen, and starts
                // from its head when the answer comes, as a start held by its probe does.
                self.eng.pictures.live.remove(&id);
                self.eng.picture(ToStage::Unload { picture: id });
                self.sound_action(&Action::UnloadVoice { cue, run }, fx);
                self.eng.pictures.held.push(Held {
                    doc,
                    cue,
                    run,
                    offset,
                    start: Some((at, offset)),
                    since: None,
                    paused: false,
                });
                return self.hold_start(cue, run, fx);
            }
            Err(Refused::Failed(why)) => return self.picture_refused(cue, run, why, fresh, fx),
        };
        let action = seconds(offset, rate).min(openlab_core::MAX_TIME);
        // A still keeps its start's delay for a resume, as a movie's plan does: its own time
        // runs that far behind the action's.
        let still_late = if fresh {
            late
        } else {
            self.eng.pictures.live.get(&id).map_or(0, |l| l.still_late)
        };
        let still_own = if fresh {
            action
        } else {
            seconds(offset.saturating_sub(still_late), rate).min(openlab_core::MAX_TIME)
        };
        // (the file position then, the plan, the fades, the action's end in the picture's time
        // if it ends, and whether the cue is told that end)
        let (position, plan, fades, length, ends) = match &plays.movie {
            Some((file, timing)) => {
                let kept = if fresh {
                    None
                } else {
                    self.eng
                        .pictures
                        .live
                        .get(&id)
                        .and_then(|l| l.plan.as_ref())
                };
                // A resume keeps a devamp already carried out; one still pending is dropped,
                // for the cue engine to ask again.
                let (top, late, followed, planned) = match kept {
                    Some(p) => (
                        p.settled(less_late(offset, p.late)).to_vec(),
                        p.late,
                        p.followed,
                        p.planned.clone(),
                    ),
                    None => match movie_table(file, timing, self.eng.rate) {
                        Ok((top, _)) => (
                            top,
                            i64::try_from(late).unwrap_or(i64::MAX),
                            0,
                            Some(Planned::of(&movie_spec(timing), *file)),
                        ),
                        Err(why) => return self.picture_refused(cue, run, why, fresh, fx),
                    },
                };
                let total = total_frames(&top);
                // A fresh start plays from its head at `offset`; a resume is dated in the
                // action's time, which runs `late` ahead of the picture's.
                let into = if fresh {
                    offset
                } else {
                    less_late(offset, late)
                };
                let sent = passes(&top, rate);
                let position = position_in(&sent, seconds(into, rate));
                let end = total.map(|t| seconds(t, rate));
                let fades = PictureFades {
                    fade_in: timing.fade_in,
                    fade_out: timing.fade_out,
                    end: end.map(|e| e.min(openlab_core::MAX_TIME)),
                };
                let plan = RunPlan {
                    table: top.clone(),
                    top,
                    at,
                    into,
                    late,
                    followed,
                    passes: sent,
                    fades,
                    devamp: None,
                    planned,
                };
                let ends = !timing.hold_at_end;
                (position, Some(plan), fades, end, ends)
            }
            None => (still_own, None, PictureFades::NONE, plays.length, true),
        };
        // The picture's own time at `at` (a still has no plan: its action time less its delay).
        let into = plan.as_ref().map_or(still_own, |p| seconds(p.into, rate));
        let message = ToStage::Play {
            picture: id,
            at,
            offset: position.min(openlab_core::MAX_TIME),
            action: into.min(openlab_core::MAX_TIME),
            plan: plan.as_ref().map(|p| p.passes.clone()).unwrap_or_default(),
            into: into.min(openlab_core::MAX_TIME),
            fades,
        };
        let late = plan
            .as_ref()
            .map_or(i64::try_from(still_late).unwrap_or(i64::MAX), |p| p.late);
        let plan_start = plan.as_ref().map(|p| p.into);
        let mut first = false;
        if let Some(l) = self.eng.pictures.live.get_mut(&id) {
            l.now = Some(message.clone());
            l.motion = Some((Some(at), into));
            l.length = length.filter(|_| ends);
            l.still_late = if plan.is_none() { still_late } else { 0 };
            l.plan = plan;
            if l.first.is_none() {
                l.first = Some(message.clone());
                first = true;
            }
        }
        // A picture starting on a stage arms its black for after it (if it keeps black).
        if first {
            self.eng.arm_stage(plays.stage);
        }
        self.eng.picture_clock(true);
        self.eng.picture(message);
        // Its sound starts on the same sample, as far into the same plan.
        if let (true, Some(p)) = (fresh, plan_start) {
            let start = Action::StartVoice {
                cue,
                run,
                at,
                offset: p,
            };
            self.sound_action(&start, fx);
        }
        // The action's length from its start: a movie's plan, a still's duration; none while
        // it loops for ever or holds its last frame. A late start is counted in.
        if let (Some(length), true) = (length, ends) {
            let frames = plus_late(seconds_to_samples(length, self.eng.rate), late);
            fx.picture_says(Input::ActionLength { cue, run, frames });
        }
    }

    /// A devamp of a movie run's picture (CT-16, VID-07): the loop under way at `at` is left at
    /// the end of its pass, as an Audio cue's voice leaves it. The plan is sent again from the
    /// same start with that pass the loop's last (and, with `stop`, nothing after it: the
    /// picture stops there), so what plays before the seam is unchanged. Said again for the
    /// same devamp while it is pending; once the picture has left that loop the devamp is done
    /// and a later one is of the loop under way then. A different one while one is pending, a
    /// still, a paused picture, or a plan already over is ignored, as the mixer ignores it.
    fn devamp_picture(
        &mut self,
        cue: CueId,
        run: RunId,
        at: SampleTime,
        stop: bool,
        fx: &mut Effects,
    ) {
        let rate = self.eng.rate.get();
        let ignored = Input::DevampIgnored { cue, run };
        let Some(id) = self.eng.pictures.find(self.doc.id, run) else {
            return fx.picture_says(ignored);
        };
        let hold =
            matches!(&self.plays(cue), Ok(Plays { movie: Some((_, t)), .. }) if t.hold_at_end);
        let Some(l) = self.eng.pictures.live.get_mut(&id) else {
            return fx.picture_says(ignored);
        };
        // A stopping picture is going whatever its plan.
        let playing = matches!(l.motion, Some((Some(_), _))) && l.ends.is_none();
        let Some(plan) = l.plan.as_mut().filter(|_| playing) else {
            return fx.picture_says(ignored);
        };
        // Samples into the table: where the plan was at its play, and since.
        let elapsed = plan.into.saturating_add(at.saturating_sub(plan.at));
        match plan.devamp {
            // Its loop is left: that devamp is done, and this one is of what plays now.
            Some((_, leaves)) if at >= leaves => {
                plan.top = plan.table.clone();
                plan.devamp = None;
            }
            Some((same, leaves)) if same == stop => {
                return fx.picture_says(Input::DevampScheduled {
                    cue,
                    run,
                    at: leaves,
                });
            }
            Some(_) => return fx.picture_says(ignored),
            None => {}
        }
        let Some((table, leaves)) = devamped(&plan.table, elapsed, stop) else {
            return fx.picture_says(ignored);
        };
        let leaves_at = plan.at.saturating_add(leaves.saturating_sub(plan.into));
        plan.devamp = Some((stop, leaves_at));
        plan.passes = passes(&table, rate);
        // The picture's end, in its own time and in the action's (a late start counted in).
        let end = total_frames(&table);
        let total = end.map(|t| plus_late(t, plan.late));
        plan.table = table;
        let Some(ToStage::Play {
            offset,
            action,
            fades,
            into,
            ..
        }) = l.now.clone()
        else {
            return fx.picture_says(ignored);
        };
        let fades = PictureFades {
            end: end.map(|t| seconds(t, rate).min(openlab_core::MAX_TIME)),
            ..fades
        };
        plan.fades = fades;
        let message = ToStage::Play {
            picture: id,
            at: plan.at,
            offset,
            action,
            plan: plan.passes.clone(),
            into,
            fades,
        };
        let ends = stop || !hold;
        l.length = total.map(|t| seconds(t, rate)).filter(|_| ends);
        l.now = Some(message.clone());
        if l.first
            .as_ref()
            .is_some_and(|f| matches!(f, ToStage::Play { at, .. } if *at == plan.at))
        {
            l.first = Some(message.clone());
        }
        // With `stop` the plan sent ends where the loop is left, and so does the action: the
        // cue ends there and its end takes the picture (`end_pictures`). No stop is dated
        // ahead for it, so a pause before then keeps the picture and the resume moves the end.
        let held_back = l.held_back;
        if !held_back {
            self.eng.picture(message);
        }
        fx.picture_says(Input::DevampScheduled {
            cue,
            run,
            at: leaves_at,
        });
        if let (Some(frames), true) = (total, ends) {
            fx.picture_says(Input::ActionLength { cue, run, frames });
        }
    }

    /// `cue`'s content was edited: a movie run's picture planned with another end time than the
    /// cue has now ends at the new one, as its sound does (`Voices::follow_end_time`): its plan
    /// is cut there and sent again from the same play sample, so nothing before the new end
    /// changes, and the cue is told its new length (at once if the picture is past the new end:
    /// it holds there until its cue's end takes it). A paused picture keeps the cut plan for its
    /// resume, which tells the length. A plan that cannot follow (the new end falls in a slice
    /// that loops, or past a marker) is kept until the cue starts again; a stopping picture is
    /// left to its stop.
    pub(super) fn follow_picture_end(&mut self, cue: CueId, now: SampleTime, fx: &mut Effects) {
        let rate = self.eng.rate;
        let doc = self.doc.id;
        // A picture whose action the cue engine has ended (dated within the lookahead) is left
        // to that end.
        let engine = self.doc.model.engine();
        let ids: Vec<u64> = (self.eng.pictures.live.iter())
            .filter(|(_, l)| l.doc == doc && l.cue == cue && l.ends.is_none() && l.plan.is_some())
            .filter(|(_, l)| engine.runs_action(cue, l.run))
            .map(|(id, _)| *id)
            .collect();
        if ids.is_empty() {
            return;
        }
        let Ok(Plays {
            movie: Some((_, timing)),
            ..
        }) = self.plays(cue)
        else {
            return;
        };
        let (end, hold) = (timing.end_time, timing.hold_at_end);
        for id in ids {
            let Some(l) = self.eng.pictures.live.get_mut(&id) else {
                continue;
            };
            let Some(plan) = l.plan.as_mut() else {
                continue;
            };
            let Some(last) = (plan.planned.as_ref())
                .filter(|p| p.end_time() != end)
                .and_then(|p| p.last_slice(end, rate))
            else {
                continue;
            };
            let grow = plan.planned.as_ref().is_some_and(Planned::grows);
            let Some(cut) = new_end(&plan.table, last, grow) else {
                continue;
            };
            if let Some(p) = plan.planned.as_mut() {
                p.ended(end, cut, plan.table.len());
            }
            cut.cut(&mut plan.table);
            if let Some(top_cut) = new_end(&plan.top, last, grow) {
                top_cut.cut(&mut plan.top);
            }
            plan.passes = passes(&plan.table, rate.get());
            let total = total_frames(&plan.table);
            plan.fades.end = total.map(|t| seconds(t, rate.get()).min(openlab_core::MAX_TIME));
            let playing = matches!(l.motion, Some((Some(_), _)));
            // Where the picture is in its own time now, and in the action's.
            let elapsed = plan.into.saturating_add(now.saturating_sub(plan.at));
            let length = total.map(|t| plus_late(t, plan.late));
            let reached = plus_late(elapsed, plan.late);
            l.length = length.map(|t| seconds(t, rate.get())).filter(|_| !hold);
            if !playing {
                continue;
            }
            let Some(ToStage::Play {
                offset,
                action,
                into,
                ..
            }) = l.now.clone()
            else {
                continue;
            };
            let message = ToStage::Play {
                picture: id,
                at: plan.at,
                offset,
                action,
                plan: plan.passes.clone(),
                into,
                fades: plan.fades,
            };
            l.now = Some(message.clone());
            if l.first
                .as_ref()
                .is_some_and(|f| matches!(f, ToStage::Play { at, .. } if *at == plan.at))
            {
                l.first = Some(message.clone());
            }
            let run = l.run;
            if !l.held_back {
                self.eng.picture(message);
            }
            if let (Some(length), false) = (length, hold) {
                fx.picture_says(Input::ActionLength {
                    cue,
                    run,
                    frames: length.max(reached),
                });
            }
        }
    }

    /// The run's sound plays on from sample `at`, `frames` later than planned (starved by the
    /// disk, or on a device whose clock runs slow; negative: earlier, on one whose clock runs
    /// fast): its picture is put back (or on) by as much from there (the sound is the clock),
    /// and the run's end moves with them. A devamp's leave sample moves too, so the pass under
    /// way is played whole (with a stop, the cue is told the later end). A paused picture (the
    /// pause came before this report was read) is held that much earlier and its plan counts
    /// the lateness, so the resume plays both on together and tells the later end. A stopping
    /// picture is left as it is: it ends with its sound on the stop's sample whatever the plan.
    pub(super) fn picture_late(
        &mut self,
        run: RunId,
        at: SampleTime,
        frames: i64,
        feedback: &mut Vec<Input>,
    ) {
        let rate = self.eng.rate.get();
        let Some(id) = self.eng.pictures.find(self.doc.id, run) else {
            return;
        };
        let Some(l) = self.eng.pictures.live.get_mut(&id) else {
            return;
        };
        let cue = l.cue;
        let (Some(plan), Some((moving, held))) = (l.plan.as_mut(), l.motion) else {
            return;
        };
        if moving.is_none() {
            if l.ends.is_some() {
                return;
            }
            // Paused: the resume plays from the action's place less `late`.
            plan.late = plan.late.saturating_add(frames);
            let own = seconds(
                less_late(seconds_to_samples(held, self.eng.rate), frames),
                rate,
            )
            .min(openlab_core::MAX_TIME);
            let Some(ToStage::Pause { at: since, .. }) = l.now else {
                return;
            };
            let message = ToStage::Pause {
                picture: id,
                at: since,
                offset: position_in(&plan.passes, own).min(openlab_core::MAX_TIME),
                action: own,
            };
            l.now = Some(message.clone());
            l.motion = Some((None, own));
            if !l.held_back {
                self.eng.picture(message);
            }
            return;
        }
        if l.ends.is_some() {
            return;
        }
        let fades = plan.fades;
        let into = plan.shift(at, frames);
        let own = seconds(into, rate).min(openlab_core::MAX_TIME);
        let message = ToStage::Play {
            picture: id,
            at,
            offset: position_in(&plan.passes, own).min(openlab_core::MAX_TIME),
            action: own,
            plan: plan.passes.clone(),
            into: own,
            fades,
        };
        let total = total_frames(&plan.table).map(|t| plus_late(t, plan.late));
        let ends = l.length.is_some();
        l.now = Some(message.clone());
        l.motion = Some((Some(at), own));
        let held_back = l.held_back;
        if !held_back {
            self.eng.picture_clock(true);
            self.eng.picture(message);
        }
        if let (Some(frames), true) = (total, ends) {
            feedback.push(Input::ActionLength { cue, run, frames });
        }
    }

    /// Each of this doc's playing pictures follows its sound's clock (the sound is the clock): a
    /// voice on a device other than the primary plays as much more (or less) than the master
    /// clock counts as that device's clock drifts from the master's, so once the drift its
    /// picture has not followed reaches [`DRIFT_STEP_US`] the picture is put on (or back) by it
    /// from `now`, as for a sound the disk made late. A picture whose sound moved to another
    /// device since follows that one from where the first one's drift had taken it (the moved
    /// sound goes on from there). Dated
    /// `now` (a lookahead ahead): an underrun reported in the block after is counted back from it.
    pub(super) fn follow_sound_clocks(&mut self, now: SampleTime, fx: &mut Effects) {
        let step = (u64::from(self.eng.rate.get()) * DRIFT_STEP_US / 1_000_000).max(1);
        let doc = self.doc.id;
        for l in self.eng.pictures.live.values_mut() {
            if l.doc != doc || l.ends.is_some() || !matches!(l.motion, Some((Some(_), _))) {
                continue;
            }
            let (Some(plan), Some(voice)) = (l.plan.as_mut(), self.doc.voices.playing(l.run))
            else {
                continue;
            };
            // Saturating: no clock drifts this far, and the step stays far below the limit.
            let drift = (self.eng.outputs.drift(voice).round() as i64)
                .saturating_add(self.doc.voices.carried_drift(voice));
            let ahead = drift.saturating_sub(plan.followed);
            if ahead.unsigned_abs() < step {
                continue;
            }
            plan.followed = drift;
            fx.late.push((l.run, now, ahead.saturating_neg()));
        }
    }

    /// The run `from` of a Video cue goes on as `run`: its sound moved to the device its patch
    /// plays on now (`Action::RelocateVoice`). The picture plays on untouched under the new
    /// id, so what the cue engine asks of that run next finds it. A playing one tells its
    /// length again: the cue engine dropped it for the moved voice to tell, and a Video cue's
    /// length is its picture's to tell.
    pub(super) fn relocate_picture(&mut self, run: RunId, from: RunId, fx: &mut Effects) {
        let doc = self.doc.id;
        if let Some(h) = self.eng.pictures.held_mut(doc, from) {
            h.run = run;
        }
        let Some(id) = self.eng.pictures.find(doc, from) else {
            return;
        };
        let Some(l) = self.eng.pictures.live.get_mut(&id) else {
            return;
        };
        l.run = run;
        let playing = matches!(l.motion, Some((Some(_), _)));
        let total =
            (l.plan.as_ref()).and_then(|p| total_frames(&p.table).map(|t| plus_late(t, p.late)));
        if let (true, Some(_), Some(frames)) = (playing, l.length, total) {
            let cue = l.cue;
            fx.picture_says(Input::ActionLength { cue, run, frames });
        }
    }

    /// Stops picture `id` from sample `at`, fading out over `frames`, and returns the sample it
    /// is gone on. A playing picture whose action ends before that fade is over (its range's
    /// end, or the end of the pass a devamp stops it at) fades with its sound until then and
    /// is cut there, as its sound is, and its cue ends there too: a picture held on its last
    /// frame would be the action's end drawn on.
    fn stop_picture(&mut self, id: u64, at: SampleTime, frames: SampleTime) -> SampleTime {
        let mut end = at.saturating_add(frames);
        let mut cut = None;
        if let Some(l) = self.eng.pictures.live.get_mut(&id) {
            let playing = matches!(l.motion, Some((Some(_), _)));
            // Its plan's end on the engine's clock; none while it loops for ever or holds its
            // last frame (hold at end with no devamp to stop it).
            let plan_end = (l.plan.as_ref())
                .filter(|_| playing && l.length.is_some())
                .and_then(|p| {
                    let total = total_frames(&p.table)?;
                    Some(p.at.saturating_add(total.saturating_sub(p.into)))
                });
            if let Some(plan_end) = plan_end
                && at < plan_end
                && plan_end < end
            {
                (end, cut) = (plan_end, Some(plan_end));
            }
            l.wait = None;
            if l.ends.is_some_and(|e| e <= end) {
                return end;
            }
            l.ends = Some(end);
            l.stop = Some(ToStage::Stop {
                picture: id,
                at,
                fade: frames,
                cut,
            });
        }
        self.eng.picture(ToStage::Stop {
            picture: id,
            at,
            fade: frames,
            cut,
        });
        end
    }

    /// A picture that cannot be loaded: the cue breaks, as a stream that cannot be opened does
    /// (`started`: at its start, so it was only skipped).
    fn picture_refused(
        &mut self,
        cue: CueId,
        run: RunId,
        why: &str,
        started: bool,
        fx: &mut Effects,
    ) {
        fx.notices.push((
            "video_unplayable",
            format!("the Video cue's picture cannot be shown: {why}"),
            Some(cue),
        ));
        let reason = BrokenReason::DecodeFailed;
        fx.feedback.push(if started {
            Input::StartFailed { cue, run, reason }
        } else {
            Input::ActionFailed { cue, run, reason }
        });
    }

    /// The pictures of cues whose action is over (`StateChanged` to idle, loaded, post-wait or
    /// broken) go on that sample, unless a stop with a fade already takes them.
    pub(super) fn end_pictures(&mut self, actions: &[Action]) {
        for action in actions {
            let Action::StateChanged { cue, state, at } = *action else {
                continue;
            };
            // A cue its sound's device left (shown paused, owning nothing) loses its picture
            // too, as an Audio cue on that device stops.
            let lost = state == CueState::Paused && self.doc.model.engine().is_lost(cue);
            if !lost
                && !matches!(
                    state,
                    CueState::Idle | CueState::PostWaiting | CueState::Broken
                )
            {
                continue;
            }
            let doc = self.doc.id;
            self.eng
                .pictures
                .held
                .retain(|h| !(lost && h.doc == doc && h.cue == cue));
            // The run the cue has now, if its action is under way again.
            let engine = self.doc.model.engine();
            let over = matches!(
                engine.state(cue),
                CueState::Idle | CueState::PostWaiting | CueState::Broken
            );
            let current = engine.run_of(cue).filter(|_| !over);
            let ids: Vec<u64> = self
                .eng
                .pictures
                .live
                .iter()
                .filter(|(_, l)| l.doc == doc && l.cue == cue && l.ends.is_none())
                // Not the picture of a run the cue started since (a GO in its panic fade).
                .filter(|(_, l)| lost || current != Some(l.run))
                // A preview's picture is no part of the show run that ended: it ends on its own
                // length or its Stop Preview.
                .filter(|(_, l)| l.run.0 < PREVIEW_RUN_BASE)
                .map(|(id, _)| *id)
                .collect();
            for id in ids {
                let _ = self.stop_picture(id, at, 0);
            }
        }
    }

    /// Pictures of this doc the stage host reported failed: each breaks its cue, with a
    /// notice, and nothing else (#643: a decode failure mid-show stops only that cue).
    pub(super) fn picture_failures(&mut self, now: SampleTime) {
        let doc = self.doc.id;
        let (mine, others): (Vec<_>, Vec<_>) = std::mem::take(&mut self.eng.pictures.failed)
            .into_iter()
            .partition(|(d, ..)| *d == doc);
        self.eng.pictures.failed = others;
        for (_, cue, run, reason, waited) in mine {
            self.log(
                LogLevel::Warn,
                format!("a Video cue's picture failed: {reason}"),
            );
            let mut fx = Effects::default();
            fx.notices.push((
                "video_decode_failed",
                format!("the Video cue's picture failed: {reason}"),
                Some(cue),
            ));
            // A start that waited for its open showed and sounded nothing: only skipped.
            let reason = BrokenReason::DecodeFailed;
            fx.feedback.push(if waited {
                Input::StartFailed { cue, run, reason }
            } else {
                Input::ActionFailed { cue, run, reason }
            });
            self.follow(fx, now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slice(index: u32, start: u64, frames: u64, plays: Passes) -> Slice {
        Slice {
            index,
            start,
            frames,
            plays,
        }
    }

    fn plan_at(at: SampleTime, into: SampleTime) -> RunPlan {
        RunPlan {
            top: Vec::new(),
            table: Vec::new(),
            at,
            into,
            late: 0,
            followed: 0,
            passes: Vec::new(),
            fades: PictureFades::NONE,
            devamp: None,
            planned: None,
        }
    }

    /// A move of the plan dated before the sample it was last moved on (an underrun ending in
    /// the block after a drift step dated a lookahead ahead) is counted back from there: the
    /// plan stands where it was at that sample less what the sound missed, never on by the
    /// difference. Moving it the other way (a sound early) puts it on.
    #[test]
    fn a_shift_dated_before_the_last_one_counts_back_from_it() {
        // Moved on sample 1 000, 500 into the table (a drift step dated `now`).
        let mut plan = plan_at(1_000, 500);
        // The sound missed 10 frames, reported on sample 936 (rendered, a block before).
        assert_eq!(plan.shift(936, 10), 426);
        assert_eq!((plan.at, plan.late), (936, 10));
        // From there on as the clock goes; a sound 4 frames early puts it on by 4.
        assert_eq!(plan.shift(1_036, -4), 530);
        assert_eq!((plan.at, plan.late), (1_036, 6));
        // Never before the table's top.
        assert_eq!(plan.shift(0, 0), 0);
    }

    /// A seek's file time is counted through the plan: into a pass, the first play of a pass that
    /// repeats, the nearest trim for a point before the plan or in a gap, the end of the last
    /// pass's first play past it, and one play into a loop that never ends.
    #[test]
    fn a_file_time_is_placed_in_the_plan_at_its_first_play() {
        let pass = |from, length, plays| PlanPass {
            from,
            length,
            plays,
        };
        // 1..3 twice, a gap, 5..6 once.
        let plan = [pass(1.0, 2.0, 2), pass(5.0, 1.0, 1)];
        assert_eq!(
            action_at(&plan, 0.2),
            0.0,
            "before the first pass: its start"
        );
        assert_eq!(action_at(&plan, 1.5), 0.5);
        assert_eq!(
            action_at(&plan, 2.9),
            1.9,
            "the first play of a repeated pass"
        );
        assert_eq!(
            action_at(&plan, 4.0),
            4.0,
            "in the gap: the next pass's start"
        );
        assert_eq!(action_at(&plan, 5.25), 4.25);
        assert_eq!(
            action_at(&plan, 9.0),
            5.0,
            "past the end: the last pass played once"
        );
        let twice = [pass(1.0, 2.0, 2)];
        assert_eq!(
            action_at(&twice, 9.0),
            2.0,
            "past the end: the end of the first play, the other to come"
        );
        let endless = [pass(0.0, 2.0, 0), pass(2.0, 1.0, 1)];
        assert_eq!(action_at(&endless, 1.0), 1.0);
        assert_eq!(
            action_at(&endless, 2.5),
            2.0,
            "past a loop that never ends: after one play"
        );
        assert_eq!(action_at(&[], 3.0), 0.0, "no plan");
    }

    /// A devamp leaves the loop at the end of the pass under way: the loop's entry keeps the
    /// passes played so far plus this one, the slices after it follow; with `stop` nothing
    /// follows. A loop split by a start part way in (its rest of a pass, then its passes) is
    /// left at the end of whichever play is under way, its later entry dropped.
    #[test]
    fn a_devamp_cuts_the_loop_after_the_pass_under_way() {
        let table = [
            slice(0, 0, 10, Passes::Times(1)),
            slice(1, 10, 20, Passes::Infinite),
            slice(2, 30, 5, Passes::Times(1)),
        ];
        // 10 + 2 passes + 5 into the third: leaves after the third pass, at 70.
        let (t, leaves) = devamped(&table, 55, false).unwrap();
        assert_eq!(leaves, 70);
        assert_eq!(t[1].plays, Passes::Times(3));
        assert_eq!(t[2], table[2]);
        let (t, leaves) = devamped(&table, 55, true).unwrap();
        assert_eq!((t.len(), leaves), (2, 70));
        // In the bridge before the loop: it ends there, the loop still to come.
        let (t, leaves) = devamped(&table, 3, false).unwrap();
        assert_eq!(leaves, 10);
        assert_eq!(t[0].plays, Passes::Times(1));
        assert_eq!(t[1], table[1], "the loop after it is kept");
        // A start inside the loop split it: devamped in that first part, the rest is dropped.
        let split = [
            slice(1, 15, 15, Passes::Times(1)),
            slice(1, 10, 20, Passes::Infinite),
            slice(2, 30, 5, Passes::Times(1)),
        ];
        let (t, leaves) = devamped(&split, 4, false).unwrap();
        assert_eq!(leaves, 15);
        assert_eq!(t, vec![split[0], split[2]]);
        // Past the end of a finite table there is nothing to leave.
        let finite = [slice(0, 0, 10, Passes::Times(2))];
        assert!(devamped(&finite, 20, false).is_none());
        assert!(devamped(&[], 0, false).is_none());
    }
}
