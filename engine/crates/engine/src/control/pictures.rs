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
//! A stage host that starts (again, after a crash) is sent every picture again, so a picture
//! comes back by itself. A picture the stage host cannot decode breaks only its own cue, with a
//! notice. The control thread never waits on the stage host.

use std::collections::BTreeMap;

use openlab_core::protocol::LogLevel;
use openlab_core::stage_link::ToStage;
use openlab_core::video_probe::ProbeReport;

use super::media::Probe;
use openlab_audio::{Plays as Passes, Slice, total_frames};
use openlab_core::{
    Action, BrokenReason, CueId, CueKind, CueState, HOLD_BEFORE_END, Input, PictureFades,
    PlanPass, RunId, SampleTime, StageId, VideoGeometry, VideoTiming, plan_point,
    seconds_to_samples,
};

use super::plan::{PlanSpec, cue_slices, picture_info, skip};

use super::voices::Effects;
use super::{DocCx, DocId, Engine};

/// How far the engine's clock may move before the stage host is told it again (it moves the
/// picture on by its own clock in between).
const CLOCK_EVERY_MS: u64 = 10;

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
    /// Its last `play`, `pause` or `stop`, likewise.
    now: Option<ToStage>,
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
}

/// A movie run's plan from its last play: the slice table (engine samples) from that play's
/// sample `at`, `action` samples into its cue's action then, and the passes sent for it.
struct RunPlan {
    table: Vec<Slice>,
    at: SampleTime,
    action: SampleTime,
    passes: Vec<PlanPass>,
    /// A devamp carried out on it: its stop, and the sample it leaves the loop on.
    devamp: Option<(bool, SampleTime)>,
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
    /// Pictures the stage host reported failed, for their doc's next tick.
    failed: Vec<(DocId, CueId, RunId, String)>,
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
    /// A movie's own length in seconds, and its time and loops.
    movie: Option<(f64, VideoTiming)>,
}

/// A movie's plan `offset` samples into its action at `rate`: the slice table from there, and
/// the action's length from the top (`None` if it loops for ever).
fn movie_table(
    file: f64,
    timing: &VideoTiming,
    rate: std::num::NonZeroU32,
    offset: SampleTime,
) -> Result<(Vec<Slice>, Option<SampleTime>), &'static str> {
    let spec = PlanSpec {
        start_time: timing.start_time,
        end_time: timing.end_time,
        slices: &timing.slices,
        last_slice_play_count: timing.last_slice_play_count,
    };
    let top = cue_slices(&spec, &picture_info(file, rate), rate, 0)
        .map_err(|_| "its trims and loops cannot be played")?;
    Ok((skip(&top, offset), total_frames(&top)))
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

/// Where in the file a movie with `table` (from its start) is: its first frame, or, with
/// nothing left to play, just before the end of `top`'s last pass (the frame it holds).
fn first_position(table: &[Slice], top_end: Option<f64>, rate: u32) -> f64 {
    match table.first() {
        Some(s) => seconds(s.start, rate),
        None => top_end.map_or(0.0, |end| (end - HOLD_BEFORE_END).max(0.0)),
    }
}

/// The file's end of a movie's trims: where it holds once its plan is over.
fn trimmed_end(file: f64, timing: &VideoTiming) -> f64 {
    timing.end_time.map_or(file, |e| e.min(file))
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
            .flat_map(|l| {
                let now = l.now.as_ref().filter(|n| Some(*n) != l.first.as_ref());
                std::iter::once(l.load.clone())
                    .chain(l.first.clone())
                    .chain(now.cloned())
            })
            .collect();
        for m in messages {
            self.picture(m);
        }
    }

    /// The stage host has `picture`'s file open.
    pub(super) fn picture_opened(&mut self, picture: u64) {
        if let Some(l) = self.pictures.live.get_mut(&picture) {
            l.opened = true;
        }
    }

    /// The stage host stopped. Only a crash of its own (`crashed`: not a kill by the
    /// supervisor or from outside) can have been caused by a picture's file, and only by one
    /// it was opening or decoding then: one not yet open, or a movie playing (a still once open,
    /// and a movie paused or not yet played, have no decoder running). Those are suspects.
    /// - One tried alone after a hold-back (`trial`) when the crash came is the culprit: its
    ///   cue breaks.
    /// - A suspect at a second crash within `SUSPECT_WINDOW_S`, if it is the only one, is the
    ///   culprit too. If several are, they are held back (kept off the stage host, their cues
    ///   running unseen) and tried again one at a time, the earliest suspected last
    ///   (`try_held_back_pictures`), so the bad file ends up alone and only its cue breaks.
    /// - Everything else is sent to the next stage host.
    pub(super) fn pictures_host_stopped(&mut self, crashed: bool) {
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
        if !on_trial.is_empty() {
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
            let decoding = !l.opened || (!l.still && matches!(l.motion, Some((Some(_), _))));
            if !decoding || l.held_back {
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
        match repeat.as_slice() {
            [] => {}
            [culprit] => self.picture_failed(
                *culprit,
                "the video output crashed twice while it played this file".to_owned(),
            ),
            several => {
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
        let now_msg = l.now.as_ref().filter(|n| Some(*n) != l.first.as_ref());
        let messages: Vec<ToStage> = std::iter::once(l.load.clone())
            .chain(l.first.clone())
            .chain(now_msg.cloned())
            .collect();
        self.picture_clock(true);
        for m in messages {
            self.picture(m);
        }
    }

    /// The stage host could not decode or draw `picture`: its cue breaks at its doc's next
    /// tick.
    pub(super) fn picture_failed(&mut self, picture: u64, reason: String) {
        if let Some(l) = self.pictures.live.remove(&picture) {
            self.pictures.failed.push((l.doc, l.cue, l.run, reason));
        }
    }

    /// Pictures whose stop is over at `now` are forgotten.
    pub(super) fn prune_pictures(&mut self, now: SampleTime) {
        self.pictures
            .live
            .retain(|_, l| l.ends.is_none_or(|end| end >= now));
    }

    /// A doc closed: its pictures go.
    pub(super) fn drop_doc_pictures(&mut self, doc: DocId) {
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
                    Some(p) => {
                        let into = elapsed - seconds(p.action, rate);
                        plan_point(&p.passes, into).map(|pt| pt.position)
                    }
                    None => (!l.still).then_some(elapsed),
                };
                Some((l.cue, elapsed, remaining, position))
            })
            .collect()
    }

    /// Pictures up whose file the stage host has open (tests).
    pub(super) fn pictures_opened(&self) -> usize {
        self.pictures.live.values().filter(|l| l.opened).count()
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
                movie: Some((d, (**timing).clone())),
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
            | Action::StopVoice { cue, .. }
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
            Action::LoadVoice { cue, run, offset } => match self.load_picture(cue, run, offset) {
                Ok(()) => {}
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
                self.play_picture(cue, run, at, offset, 0, fx);
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
                if let Some(l) = self.eng.pictures.live.get_mut(&id) {
                    l.run = run;
                }
                self.play_picture(cue, run, at, offset, 0, fx);
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
                    let action = seconds(offset, rate);
                    // A movie holds where its plan is then; a still has no position.
                    let at_file = self.eng.pictures.live.get(&id).map_or(action, |l| {
                        l.plan.as_ref().map_or(action, |p| {
                            let elapsed = seconds(offset.saturating_sub(p.action), rate);
                            plan_point(&p.passes, elapsed).map_or(action, |pt| pt.position)
                        })
                    });
                    let message = ToStage::Pause {
                        picture: id,
                        at,
                        offset: at_file.min(openlab_core::MAX_TIME),
                        action: action.min(openlab_core::MAX_TIME),
                    };
                    if let Some(l) = self.eng.pictures.live.get_mut(&id) {
                        l.now = Some(message.clone());
                        // Held at the frame it pauses on.
                        l.motion = Some((None, action));
                    }
                    self.eng.picture(message);
                }
            }
            Action::StopVoice {
                cue, run, at, fade, ..
            } => {
                let frames = fade.map_or(0, |f| f.frames);
                self.eng.pictures.take_held(doc, run);
                if let Some(id) = self.eng.pictures.find(doc, run) {
                    self.stop_picture(id, at, frames);
                }
                // The stop (its fade, if any) is over on this sample, as a mixer voice's is.
                let end = at.saturating_add(frames);
                fx.deferred
                    .push((end, Input::ActionEnded { cue, run, at: end }));
            }
            Action::UnloadVoice { run, .. } => {
                self.eng.pictures.take_held(doc, run);
                if let Some(id) = self.eng.pictures.find(doc, run) {
                    self.eng.pictures.live.remove(&id);
                    self.eng.picture(ToStage::Unload { picture: id });
                }
            }
            Action::DevampVoice { cue, run, at, stop } => {
                self.devamp_picture(cue, run, at, stop, fx);
            }
            // Levels and ducks act on sound; a picture has none (yet: #645).
            _ => {}
        }
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
                let late = now.saturating_sub(at);
                self.play_picture(h.cue, h.run, at.max(now), offset, late, &mut fx);
                if h.since.is_some() {
                    fx.notices.push((
                        "cold_start",
                        "a cue started late: its video file had to be read first".to_owned(),
                        Some(h.cue),
                    ));
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

    fn load_picture(&mut self, cue: CueId, run: RunId, offset: SampleTime) -> Result<(), Refused> {
        let plays = self.plays(cue)?;
        let rate = self.eng.rate.get();
        // A movie opens on the frame its plan is at there (inside its trims and loops).
        let at_file = match &plays.movie {
            Some((file, timing)) => {
                let (table, _) =
                    movie_table(*file, timing, self.eng.rate, offset).map_err(Refused::Failed)?;
                first_position(&table, Some(trimmed_end(*file, timing)), rate)
            }
            None => seconds(offset, rate),
        };
        self.eng.pictures.next += 1;
        let id = self.eng.pictures.next;
        let load = ToStage::Load {
            picture: id,
            stage: plays.stage,
            file: plays.path,
            still: plays.still,
            offset: at_file.min(openlab_core::MAX_TIME),
            geometry: plays.geometry,
        };
        let (length, still) = (plays.length, plays.still);
        self.eng.pictures.live.insert(
            id,
            Live {
                doc: self.doc.id,
                cue,
                run,
                load: load.clone(),
                first: None,
                now: None,
                ends: None,
                opened: false,
                suspected: 0,
                suspected_at: None,
                held_back: false,
                trial: None,
                motion: None,
                length,
                still,
                plan: None,
            },
        );
        self.eng.picture(load);
        Ok(())
    }

    /// Plays the run's picture from sample `at`, `offset` into its action; `late` is how much
    /// later than the engine dated it the start is (a start held by its probe), counted into
    /// the action's length. A movie follows its plan from `offset` (its trims, slices and
    /// loops) with its integrated fades; with hold at end the cue is told no length.
    fn play_picture(
        &mut self,
        cue: CueId,
        run: RunId,
        at: SampleTime,
        offset: SampleTime,
        late: SampleTime,
        fx: &mut Effects,
    ) {
        let doc = self.doc.id;
        let rate = self.eng.rate.get();
        let Some(id) = self.eng.pictures.find(doc, run) else {
            return;
        };
        let Ok(plays) = self.plays(cue) else {
            return;
        };
        let action = seconds(offset, rate).min(openlab_core::MAX_TIME);
        // (the file position then, the passes, the fades, the action's end if it ends, and
        // whether the cue is told that end)
        let (position, plan, fades, length, ends) = match &plays.movie {
            Some((file, timing)) => {
                let (table, total) = match movie_table(*file, timing, self.eng.rate, offset) {
                    Ok(t) => t,
                    Err(why) => return self.picture_refused(cue, run, why, true, fx),
                };
                let position = first_position(&table, Some(trimmed_end(*file, timing)), rate);
                let sent = passes(&table, rate);
                let end = total.map(|t| seconds(t, rate));
                let fades = PictureFades {
                    fade_in: timing.fade_in,
                    fade_out: timing.fade_out,
                    end: end.map(|e| e.min(openlab_core::MAX_TIME)),
                };
                let plan = RunPlan {
                    table,
                    at,
                    action: offset,
                    passes: sent,
                    devamp: None,
                };
                let ends = !timing.hold_at_end;
                (position, Some(plan), fades, end, ends)
            }
            None => (action, None, PictureFades::NONE, plays.length, true),
        };
        let message = ToStage::Play {
            picture: id,
            at,
            offset: position.min(openlab_core::MAX_TIME),
            action,
            plan: plan.as_ref().map(|p| p.passes.clone()).unwrap_or_default(),
            fades,
        };
        if let Some(l) = self.eng.pictures.live.get_mut(&id) {
            l.now = Some(message.clone());
            l.motion = Some((Some(at), action));
            l.length = length.filter(|_| ends);
            l.plan = plan;
            if l.first.is_none() {
                l.first = Some(message.clone());
            }
        }
        self.eng.picture_clock(true);
        self.eng.picture(message);
        // The action's length from its start: a movie's plan, a still's duration; none while
        // it loops for ever or holds its last frame.
        if let (Some(length), true) = (length, ends) {
            let frames = seconds_to_samples(length, self.eng.rate).saturating_add(late);
            fx.feedback.push(Input::ActionLength { cue, run, frames });
        }
    }

    /// A devamp of a movie run's picture (CT-16, VID-07): the loop under way at `at` is left at
    /// the end of its pass, as an Audio cue's voice leaves it. The plan is sent again from the
    /// same start with that pass the loop's last (and, with `stop`, nothing after it: the
    /// picture stops there), so what plays before the seam is unchanged. Said again for the
    /// same devamp; a different one while one is pending, a still, a paused picture, or a plan
    /// already over is ignored, as the mixer ignores it.
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
            return fx.feedback.push(ignored);
        };
        let hold = matches!(&self.plays(cue), Ok(Plays { movie: Some((_, t)), .. }) if t.hold_at_end);
        let Some(l) = self.eng.pictures.live.get_mut(&id) else {
            return fx.feedback.push(ignored);
        };
        let playing = matches!(l.motion, Some((Some(_), _)));
        let Some(plan) = l.plan.as_mut().filter(|_| playing) else {
            return fx.feedback.push(ignored);
        };
        match plan.devamp {
            Some((same, leaves)) if same == stop => {
                return fx.feedback.push(Input::DevampScheduled {
                    cue,
                    run,
                    at: leaves,
                });
            }
            Some(_) => return fx.feedback.push(ignored),
            None => {}
        }
        let elapsed = at.saturating_sub(plan.at);
        let Some((table, leaves)) = devamped(&plan.table, elapsed, stop) else {
            return fx.feedback.push(ignored);
        };
        let leaves_at = plan.at.saturating_add(leaves);
        plan.devamp = Some((stop, leaves_at));
        plan.passes = passes(&table, rate);
        let total = total_frames(&table).map(|t| t.saturating_add(plan.action));
        plan.table = table;
        let Some(ToStage::Play {
            offset,
            action,
            fades,
            ..
        }) = l.now.clone()
        else {
            return fx.feedback.push(ignored);
        };
        let fades = PictureFades {
            end: total.map(|t| seconds(t, rate).min(openlab_core::MAX_TIME)),
            ..fades
        };
        let message = ToStage::Play {
            picture: id,
            at: plan.at,
            offset,
            action,
            plan: plan.passes.clone(),
            fades,
        };
        let ends = stop || !hold;
        l.length = total.map(|t| seconds(t, rate)).filter(|_| ends);
        l.now = Some(message.clone());
        if l.first.as_ref().is_some_and(|f| matches!(f, ToStage::Play { at, .. } if *at == plan.at)) {
            l.first = Some(message.clone());
        }
        let held_back = l.held_back;
        if !held_back {
            self.eng.picture(message);
        }
        if stop {
            self.stop_picture(id, leaves_at, 0);
        }
        fx.feedback.push(Input::DevampScheduled {
            cue,
            run,
            at: leaves_at,
        });
        if let (Some(frames), true) = (total, ends) {
            fx.feedback.push(Input::ActionLength { cue, run, frames });
        }
    }

    fn stop_picture(&mut self, id: u64, at: SampleTime, frames: SampleTime) {
        let end = at.saturating_add(frames);
        if let Some(l) = self.eng.pictures.live.get_mut(&id) {
            if l.ends.is_some_and(|e| e <= end) {
                return;
            }
            l.ends = Some(end);
            l.now = Some(ToStage::Stop {
                picture: id,
                at,
                fade: frames,
            });
        }
        self.eng.picture(ToStage::Stop {
            picture: id,
            at,
            fade: frames,
        });
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
            if !matches!(
                state,
                CueState::Idle | CueState::PostWaiting | CueState::Broken
            ) {
                continue;
            }
            let doc = self.doc.id;
            let ids: Vec<u64> = self
                .eng
                .pictures
                .live
                .iter()
                .filter(|(_, l)| l.doc == doc && l.cue == cue && l.ends.is_none())
                .map(|(id, _)| *id)
                .collect();
            for id in ids {
                self.stop_picture(id, at, 0);
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
        for (_, cue, run, reason) in mine {
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
            fx.feedback.push(Input::ActionFailed {
                cue,
                run,
                reason: BrokenReason::DecodeFailed,
            });
            self.follow(fx, now);
        }
    }
}
