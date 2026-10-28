// SPDX-License-Identifier: GPL-3.0-or-later

//! The cue engine's voice actions carried out on the mixer and the disk streamer, and the
//! mixer's events turned back into feedback (docs/control.md "Voices" and "Feedback").
//!
//! Every run segment (`RunId`) gets its own mixer `VoiceId`, from the engine's [`VoiceIds`] (never
//! reused), so
//! voices of a replaced engine (a new workspace) can never be mistaken for the new one's. A
//! resume re-keys the mixer voice to a fresh `VoiceId` for the fresh `RunId`; the old id stays an
//! alias until the voice is gone, so late events and meter entries under it still name the cue.

use std::collections::{BTreeSet, HashMap};
use std::num::NonZeroU32;
use std::sync::Arc;

use openlab_audio::{
    CommandKind, Curve, DiskStreamer, Event, Fade, FinishReason, GainTarget, Plays, PreparedStream,
    Slice, StreamHandle, StreamRequest, VoiceId, VoiceStart, total_frames,
};
use openlab_core::{
    Action, BrokenReason, CueId, CueKind, FadeCurve, FadeSpec, Input, LevelParam, RunId,
    SampleTime, StopFade, Workspace, db_to_gain, seconds_to_samples,
};

use super::media::{MediaCatalog, Probe};
use super::patches::Patches;
use super::plan::{PlanFailure, PlanSpec, cue_slices};
use super::voice_ids::VoiceIds;

/// Most ids kept for the next meter read (meters are read at 30 Hz, so this is never reached in
/// practice; it bounds memory if they are not).
const GONE_LIMIT: usize = 4096;

/// What the control loop must do after the voice actions of one `handle` call.
#[derive(Default)]
pub struct Effects {
    /// Mixer commands, in the order the actions came (sent as one batch).
    pub commands: Vec<(SampleTime, CommandKind)>,
    /// Feedback to hand to the engine at once (no sample of its own: failures).
    pub feedback: Vec<Input>,
    /// Feedback dated `at`, handed to the engine only once `now > at` (docs/control.md).
    pub deferred: Vec<(SampleTime, Input)>,
    /// Notices for the operator (`code`, message, cue).
    pub notices: Vec<(&'static str, String, Option<CueId>)>,
    /// Media to probe again (a stream of it failed).
    pub reprobe: Vec<String>,
}

/// The environment voice actions are carried out in.
pub struct Env<'a> {
    pub ws: &'a Workspace,
    pub media: &'a MediaCatalog,
    pub streamer: &'a mut DiskStreamer,
    pub patches: &'a Patches,
    /// Where new voice ids come from (shared by the engine, not the voice table).
    pub ids: &'a mut VoiceIds,
    pub rate: NonZeroU32,
    pub cue_outputs: usize,
    /// The scheduling time of the call.
    pub now: SampleTime,
    /// During a device outage: the sample the mixer renders first when it is back. Stops are
    /// dated there (see [`Voices::stop_date`]).
    pub floor: Option<SampleTime>,
}

/// A mixer command for a voice that is not in the mixer yet, replayed when it starts.
#[derive(Clone, Copy, Debug)]
enum Queued {
    Ramp {
        target: GainTarget,
        gain: f32,
        fade: Fade,
    },
    Devamp {
        stop: bool,
    },
}

enum Phase {
    /// Not in the mixer yet. `prepared` is `None` while the file's probe has not answered;
    /// `start` is set once the engine has started it; `paused` holds it.
    Waiting {
        prepared: Option<(PreparedStream, Option<u64>)>,
        start: Option<SampleTime>,
        paused: bool,
        queue: Vec<(SampleTime, Queued)>,
    },
    /// Started in the mixer (its start may still be ahead in the mixer's queue).
    Playing,
    /// Ended in the mixer while a pause was pending or in force: kept so the resume that follows
    /// can report the end with the fresh run (M2-E (a)).
    Ended,
}

pub struct VoiceRec {
    pub cue: CueId,
    pub run: RunId,
    pub preview: bool,
    phase: Phase,
    stream: Option<StreamHandle>,
    /// Offset into the action the voice started at, and the sample it started on.
    offset: SampleTime,
    start_at: SampleTime,
    /// Frames the voice plays from `offset` (`None`: it loops until devamped).
    total: Option<u64>,
    paused_at: Option<SampleTime>,
    paused_frames: u64,
    /// The sample of the latest resume (a re-key).
    resumed_at: Option<SampleTime>,
    stream_failed: bool,
    /// The id it had before its latest resume: the only old id the mixer can still report an
    /// event under (an end before that resume applied). Older ones go with the next meter read.
    prev_id: Option<VoiceId>,
    /// Voice-meter reads in a row that did not list this started voice (reconciliation).
    pub absent: u8,
    /// A meter read has listed it: the mixer took the start.
    listed: bool,
    /// The slice table it plays (from its start offset), for its length after a devamp.
    table: Vec<Slice>,
    /// The `stop` of the latest devamp sent for it.
    devamp_stop: bool,
    /// Its start waited for the file's probe, since this sample (`now`).
    held: Option<SampleTime>,
    /// How much later than the engine dated it the voice started (held by its probe).
    delay: u64,
    /// The id its `StartVoice` was sent with: a rejection under that id refused the start, so
    /// the voice never played (any other rejection is of a resume).
    start_id: Option<VoiceId>,
    /// The mixer patch slot its start was routed through (it keeps it until it ends).
    slot: Option<u16>,
}

impl VoiceRec {
    /// The failure to report for this voice: `StartFailed` if it never produced any audio (a
    /// start not yet in the mixer, or a stream that failed before writing a frame), so the cue
    /// engine skips the cue (M4.14, QLab P5/G2); else `ActionFailed`. A loaded voice that was
    /// never started stays `ActionFailed` (the cue is loaded, not playing).
    fn failure(&self, reason: BrokenReason) -> Input {
        let unplayed = match &self.phase {
            Phase::Waiting { start, .. } => start.is_some(),
            // `frames_written` counts frames the disk worker put in the ring, not frames the
            // mixer played: frames in the ring are about to sound (or have), so a failure after
            // any was written is taken as during play. A prime fills the ring before the start,
            // but a prime whose stream failed is dropped every tick (`poll_streams`) and is never
            // handed to a start, so it cannot make an unplayed start look played.
            Phase::Playing => self.stream.as_ref().is_none_or(|h| h.frames_written() == 0),
            Phase::Ended => false,
        };
        failure(self.cue, self.run, reason, unplayed)
    }

    /// Position in the action on rendered sample `t`, while playing.
    fn position(&self, t: SampleTime) -> Option<u64> {
        if !matches!(self.phase, Phase::Playing) || t < self.start_at {
            return None;
        }
        let until = self.paused_at.filter(|&p| p <= t).unwrap_or(t);
        let played = until
            .saturating_sub(self.start_at)
            .saturating_sub(self.paused_frames);
        Some(self.offset.saturating_add(played))
    }
}

/// A prepared stream kept ready for a cue near the playhead (ARCHITECTURE §3.5).
struct Prime {
    prepared: PreparedStream,
    total: Option<u64>,
    sig: Sig,
}

/// What a primed stream was built from; it is used only while the cue still matches.
#[derive(Clone, Debug, PartialEq)]
struct Sig {
    media: String,
    probe: usize,
    /// Built from the preloaded samples: a stream primed from disk before a preload landed is
    /// replaced, and one primed from memory is when the preload is released.
    preloaded: bool,
    start: u64,
    end: Option<u64>,
    marks: Vec<(u64, u32)>,
    last: u32,
}

#[derive(Default)]
pub struct Voices {
    recs: HashMap<VoiceId, VoiceRec>,
    by_run: HashMap<RunId, VoiceId>,
    /// Old id of a re-keyed voice → (current id, the run it had).
    aliases: HashMap<VoiceId, (VoiceId, RunId)>,
    primes: HashMap<CueId, Prime>,
    /// Starts that used a primed stream (a cold start prepares one on the spot).
    prime_hits: u64,
    /// Ids that left since the last voice-meter read, and their cue: the read reports them once
    /// more (docs/audio.md, Meters).
    gone: Vec<(VoiceId, CueId)>,
}

/// Why a voice could not be prepared.
enum PrepError {
    /// The probe has not answered yet: wait for it.
    Pending,
    /// Nothing to play (an offset at or past the end).
    Empty,
    Failed,
}

impl Voices {
    pub fn new() -> Self {
        Self::default()
    }

    /// Voice records alive (loaded, waiting, playing or kept for a resume).
    pub fn live(&self) -> usize {
        self.recs.len()
    }

    /// Stream handles held (voices and primes).
    pub fn streams(&self) -> usize {
        self.recs.values().filter(|r| r.stream.is_some()).count()
            + self
                .recs
                .values()
                .filter(|r| {
                    matches!(
                        r.phase,
                        Phase::Waiting {
                            prepared: Some(_),
                            ..
                        }
                    )
                })
                .count()
            + self.primes.len()
    }

    pub fn primed(&self) -> impl Iterator<Item = CueId> + '_ {
        self.primes.keys().copied()
    }

    /// Starts that used a primed stream.
    pub fn prime_hits(&self) -> u64 {
        self.prime_hits
    }

    /// Whether `cue`'s primed stream reads preloaded samples (not the file).
    pub fn primed_from_memory(&self, cue: CueId) -> bool {
        self.primes.get(&cue).is_some_and(|p| p.sig.preloaded)
    }

    /// Carries out one engine action; non-voice actions are ignored.
    pub fn apply(&mut self, action: &Action, env: &mut Env<'_>, fx: &mut Effects) {
        match *action {
            Action::LoadVoice { cue, run, offset } => self.load(cue, run, offset, env, fx),
            Action::UnloadVoice { run, .. } => self.release(run),
            Action::StartVoice {
                cue,
                run,
                at,
                offset,
            } => self.start(cue, run, at, offset, false, env, fx),
            Action::PreviewStart { cue, run, at } => self.start(cue, run, at, 0, true, env, fx),
            Action::StopVoice { run, at, fade, .. } | Action::PreviewStop { run, at, fade, .. } => {
                self.stop(run, at, fade, env.floor, fx);
            }
            Action::PauseVoice { run, at, .. } => self.pause(run, at, env.floor, fx),
            Action::ResumeVoice {
                cue,
                run,
                paused,
                at,
                ..
            } => self.resume(cue, run, paused, at, env, fx),
            Action::Ramp {
                run,
                at,
                param,
                gain,
                frames,
                curve,
                ..
            } => {
                let cmd = Queued::Ramp {
                    target: target(param),
                    gain,
                    fade: Fade::new(frames, to_curve(curve)),
                };
                self.command(run, at, cmd, fx);
            }
            Action::DevampVoice { run, at, stop, .. } => {
                if let Some(r) = self.by_run.get(&run).and_then(|id| self.recs.get_mut(id)) {
                    r.devamp_stop = stop;
                }
                self.command(run, at, Queued::Devamp { stop }, fx);
            }
            _ => {}
        }
    }

    fn load(&mut self, cue: CueId, run: RunId, offset: u64, env: &mut Env<'_>, fx: &mut Effects) {
        self.release(run);
        let id = env.ids.next();
        let prepared = match self.prepare(cue, offset, env, fx) {
            Ok(p) => Some(p),
            Err(PrepError::Pending) => None,
            Err(PrepError::Empty) => {
                // A load past the end: it starts and ends at once (`start` reports it).
                None
            }
            Err(PrepError::Failed) => {
                fx.feedback.push(Input::ActionFailed {
                    cue,
                    run,
                    reason: BrokenReason::StreamFailed,
                });
                return;
            }
        };
        self.insert(
            id,
            VoiceRec {
                phase: Phase::Waiting {
                    prepared,
                    start: None,
                    paused: false,
                    queue: Vec::new(),
                },
                offset,
                ..rec(cue, run, false)
            },
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        cue: CueId,
        run: RunId,
        at: SampleTime,
        offset: u64,
        preview: bool,
        env: &mut Env<'_>,
        fx: &mut Effects,
    ) {
        let id = match self.by_run.get(&run) {
            Some(&id)
                if self.recs.get(&id).is_some_and(|r| {
                    r.offset == offset && matches!(r.phase, Phase::Waiting { .. })
                }) =>
            {
                id
            }
            _ => {
                self.release(run);
                let id = env.ids.next();
                let prepared = if preview {
                    None
                } else {
                    self.take_prime(cue, offset, env)
                };
                self.insert(
                    id,
                    VoiceRec {
                        phase: Phase::Waiting {
                            prepared,
                            start: None,
                            paused: false,
                            queue: Vec::new(),
                        },
                        offset,
                        ..rec(cue, run, preview)
                    },
                );
                id
            }
        };
        if let Some(VoiceRec {
            phase: Phase::Waiting { start, .. },
            ..
        }) = self.recs.get_mut(&id)
        {
            *start = Some(at);
        }
        self.try_start(id, env, fx);
    }

    /// Puts a waiting voice into the mixer once it has a stream and a start and is not paused.
    fn try_start(&mut self, id: VoiceId, env: &mut Env<'_>, fx: &mut Effects) {
        let Some(r) = self.recs.get(&id) else { return };
        let (cue, run, offset) = (r.cue, r.run, r.offset);
        let Phase::Waiting {
            prepared,
            start: Some(_),
            paused: false,
            ..
        } = &r.phase
        else {
            return;
        };
        if prepared.is_none() {
            match self.prepare(cue, offset, env, fx) {
                Ok(p) => {
                    if let Some(VoiceRec {
                        phase: Phase::Waiting { prepared, .. },
                        ..
                    }) = self.recs.get_mut(&id)
                    {
                        *prepared = Some(p);
                    }
                }
                Err(PrepError::Pending) => {
                    if let Some(r) = self.recs.get_mut(&id)
                        && r.held.is_none()
                    {
                        r.held = Some(env.now);
                        fx.notices.push((
                            "probe_wait",
                            "a cue waits for its audio file to be read; it starts once it is"
                                .to_owned(),
                            Some(cue),
                        ));
                    }
                    return;
                }
                Err(PrepError::Empty) => {
                    // Nothing to play (a load or start at or past the cue's end, M4.11b): the
                    // action ends on its start sample. Its length is the offset itself, so the
                    // core dates the end, and a chained successor, on that sample instead of
                    // on the late report below (a `late_start` warning).
                    if let Some(r) = self.remove(id) {
                        let at = match r.phase {
                            Phase::Waiting { start, .. } => start.unwrap_or(env.now),
                            _ => env.now,
                        };
                        if !r.preview {
                            // A start the probe held up ends when it is released, as a played
                            // one counts the delay in.
                            let frames = offset.saturating_add(env.now.saturating_sub(at));
                            fx.feedback.push(Input::ActionLength { cue, run, frames });
                        }
                        fx.deferred.push((at, Input::ActionEnded { cue, run, at }));
                    }
                    return;
                }
                Err(PrepError::Failed) => {
                    // A started voice not yet in the mixer: nothing has played.
                    self.remove(id);
                    fx.feedback.push(Input::StartFailed {
                        cue,
                        run,
                        reason: BrokenReason::StreamFailed,
                    });
                    return;
                }
            }
        }
        let Some(r) = self.recs.get_mut(&id) else {
            return;
        };
        let Phase::Waiting {
            prepared,
            start,
            queue,
            ..
        } = std::mem::replace(&mut r.phase, Phase::Playing)
        else {
            return;
        };
        let (Some((prepared, total)), Some(start)) = (prepared, start) else {
            return;
        };
        // A start the probe held up plays as soon as it can, from its head (a cold start).
        let at = start.max(env.now);
        let table = prepared.slices.clone();
        match voice_start(env, cue, prepared, id, offset) {
            Some((vs, handle)) => {
                r.stream = Some(handle);
                r.start_at = at;
                r.total = total;
                r.table = table;
                r.delay = at - start;
                let preview = r.preview;
                let held = r.held.take().is_some();
                r.start_id = Some(id);
                r.slot = Some(vs.patch);
                fx.commands.push((at, CommandKind::StartVoice(vs)));
                for (t, q) in queue {
                    fx.commands.push((t.max(at), queued(id, q)));
                }
                // The engine owns the end of a finite action (`ActionLength`): it is the
                // action's length counted from the start the engine dated, so a start held up
                // by its probe counts the delay in.
                if let Some(total) = total
                    && !preview
                {
                    let frames = offset.saturating_add(total).saturating_add(at - start);
                    fx.feedback.push(Input::ActionLength { cue, run, frames });
                }
                if held {
                    fx.notices.push((
                        "cold_start",
                        "a cue started late: its audio file had to be read first".to_owned(),
                        Some(cue),
                    ));
                }
            }
            None => {
                self.remove(id);
                fx.feedback.push(Input::StartFailed {
                    cue,
                    run,
                    reason: BrokenReason::VoiceRejected,
                });
            }
        }
    }

    fn stop(
        &mut self,
        run: RunId,
        at: SampleTime,
        fade: Option<StopFade>,
        floor: Option<SampleTime>,
        fx: &mut Effects,
    ) {
        let Some(&id) = self.by_run.get(&run) else {
            return;
        };
        let playing = self
            .recs
            .get(&id)
            .is_some_and(|r| matches!(r.phase, Phase::Playing));
        if playing {
            let at = self.stop_date(id, at, floor);
            let fade = fade.map_or(Fade::NONE, |f| Fade::new(f.frames, to_curve(f.curve)));
            fx.commands
                .push((at, CommandKind::StopVoice { voice: id, fade }));
            // The record goes when the mixer reports the voice finished.
        } else {
            self.remove(id);
        }
    }

    /// A pause during an outage (`floor`) is dated, like a stop, on the first sample the mixer
    /// renders when the device is back, so no voice plays one more block. The mixer then holds
    /// the voice `at − date` frames earlier than the engine thinks: that is counted into the
    /// action's length (`delay`) and sent again, so the engine's end stays the voice's.
    fn pause(&mut self, run: RunId, at: SampleTime, floor: Option<SampleTime>, fx: &mut Effects) {
        let Some(&id) = self.by_run.get(&run) else {
            return;
        };
        let date = self.stop_date(id, at, floor);
        let Some(r) = self.recs.get_mut(&id) else {
            return;
        };
        match &mut r.phase {
            Phase::Playing => {
                r.paused_at = Some(date);
                fx.commands
                    .push((date, CommandKind::PauseVoice { voice: id }));
                let early = at.saturating_sub(date);
                if early > 0 {
                    r.delay = r.delay.saturating_add(early);
                    if let Some(total) = r.total
                        && !r.preview
                    {
                        let frames = r.offset.saturating_add(total).saturating_add(r.delay);
                        fx.feedback.push(Input::ActionLength {
                            cue: r.cue,
                            run,
                            frames,
                        });
                    }
                }
            }
            Phase::Waiting { paused, .. } => *paused = true,
            Phase::Ended => {}
        }
    }

    fn resume(
        &mut self,
        cue: CueId,
        run: RunId,
        paused: RunId,
        at: SampleTime,
        env: &mut Env<'_>,
        fx: &mut Effects,
    ) {
        let Some(old) = self.by_run.get(&paused).copied() else {
            return;
        };
        let new = env.ids.next();
        let Some(mut r) = self.recs.remove(&old) else {
            return;
        };
        self.by_run.remove(&paused);
        r.run = run;
        r.resumed_at = Some(at);
        // Only the id just replaced can still carry an event; an older one is named for one
        // more meter read (`gone`) and then forgotten, so a cue paused and resumed many times
        // keeps one alias, not one per resume.
        if let Some(older) = r.prev_id.replace(old) {
            self.aliases.remove(&older);
        }
        self.gone.push((old, cue));
        self.aliases.insert(old, (new, paused));
        match &mut r.phase {
            Phase::Playing => {
                if let Some(p) = r.paused_at.take() {
                    r.paused_frames = r.paused_frames.saturating_add(at.saturating_sub(p));
                }
                fx.commands.push((
                    at,
                    CommandKind::ResumeVoice {
                        voice: old,
                        new_id: new,
                    },
                ));
                self.insert(new, r);
            }
            Phase::Waiting {
                paused: held,
                start,
                ..
            } => {
                *held = false;
                if start.is_some() {
                    *start = Some(at);
                }
                self.insert(new, r);
                self.try_start(new, env, fx);
            }
            Phase::Ended => {
                // The voice ended while paused: the mixer ignores the resume of an unknown voice,
                // so the end is reported for the fresh run (M2-E (a)).
                fx.deferred.push((at, Input::ActionEnded { cue, run, at }));
                self.drop_aliases(&r);
            }
        }
    }

    fn command(&mut self, run: RunId, at: SampleTime, cmd: Queued, fx: &mut Effects) {
        let Some(&id) = self.by_run.get(&run) else {
            return;
        };
        let Some(r) = self.recs.get_mut(&id) else {
            return;
        };
        match &mut r.phase {
            Phase::Playing => fx.commands.push((at, queued(id, cmd))),
            Phase::Waiting { queue, .. } => queue.push((at, cmd)),
            Phase::Ended => {}
        }
    }

    /// Lets go of a run's voice that is not playing (or forgets one that is).
    fn release(&mut self, run: RunId) {
        if let Some(&id) = self.by_run.get(&run) {
            self.remove(id);
        }
    }

    fn insert(&mut self, id: VoiceId, r: VoiceRec) {
        self.by_run.insert(r.run, id);
        self.recs.insert(id, r);
    }

    fn remove(&mut self, id: VoiceId) -> Option<VoiceRec> {
        let r = self.recs.remove(&id)?;
        if self.by_run.get(&r.run) == Some(&id) {
            self.by_run.remove(&r.run);
        }
        self.drop_aliases(&r);
        if self.gone.len() >= GONE_LIMIT {
            self.gone.remove(0);
        }
        self.gone.push((id, r.cue));
        Some(r)
    }

    fn drop_aliases(&mut self, r: &VoiceRec) {
        if let Some(o) = r.prev_id {
            self.aliases.remove(&o);
        }
    }

    /// The probe of `key` answered: starts the voices that waited for it.
    pub fn media_ready(&mut self, key: &str, env: &mut Env<'_>, fx: &mut Effects) {
        let waiting: Vec<VoiceId> = self
            .recs
            .iter()
            .filter(|(_, r)| {
                matches!(r.phase, Phase::Waiting { prepared: None, .. })
                    && cue_media(env.ws, r.cue).as_deref() == Some(key)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in waiting {
            let Some(r) = self.recs.get(&id) else {
                continue;
            };
            let (cue, run, offset, started) = (
                r.cue,
                r.run,
                r.offset,
                matches!(r.phase, Phase::Waiting { start: Some(_), .. }),
            );
            if started {
                self.try_start(id, env, fx);
                continue;
            }
            match self.prepare(cue, offset, env, fx) {
                Ok(p) => {
                    if let Some(VoiceRec {
                        phase: Phase::Waiting { prepared, .. },
                        ..
                    }) = self.recs.get_mut(&id)
                    {
                        *prepared = Some(p);
                    }
                }
                Err(PrepError::Pending | PrepError::Empty) => {}
                Err(PrepError::Failed) => {
                    self.remove(id);
                    fx.feedback.push(Input::ActionFailed {
                        cue,
                        run,
                        reason: BrokenReason::StreamFailed,
                    });
                }
            }
        }
    }

    /// Prepares the stream of `cue` from `offset` (no I/O: the disk worker opens the file).
    fn prepare(
        &self,
        cue: CueId,
        offset: u64,
        env: &mut Env<'_>,
        fx: &mut Effects,
    ) -> Result<(PreparedStream, Option<u64>), PrepError> {
        let Some(c) = env.ws.find_cue(cue) else {
            return Err(PrepError::Failed);
        };
        let CueKind::Audio {
            media: Some(media), ..
        } = &c.kind
        else {
            return Err(PrepError::Failed);
        };
        let key = media.as_str();
        match env.media.get(key) {
            None | Some(Probe::Pending) => return Err(PrepError::Pending),
            Some(Probe::Failed) => return Err(PrepError::Failed),
            Some(Probe::Ready(_)) => {}
        }
        let Some((ready, source)) = env.media.media(key) else {
            return Err(PrepError::Pending);
        };
        let Some(spec) = plan_spec(&c.kind) else {
            return Err(PrepError::Failed);
        };
        let slices = match cue_slices(&spec, &ready.probed.info, env.rate, offset) {
            Ok(s) => s,
            Err(PlanFailure::Plan(e)) => {
                fx.notices.push((
                    "bad_plan",
                    format!("the cue's slices cannot be played: {e}"),
                    Some(cue),
                ));
                return Err(PrepError::Failed);
            }
        };
        if slices.is_empty() {
            return Err(PrepError::Empty);
        }
        let total = total_frames(&slices);
        let req = StreamRequest {
            media: source,
            info: ready.probed.info,
            seeking: ready.probed.seeking.clone(),
            slices,
        };
        match env.streamer.prepare(req) {
            Ok(p) => Ok((p, total)),
            Err(e) => {
                fx.notices.push((
                    "stream_failed",
                    format!("the cue's audio could not be streamed: {e}"),
                    Some(cue),
                ));
                Err(PrepError::Failed)
            }
        }
    }

    /// A primed stream for a start from the top of `cue`, if one is ready and still matches.
    fn take_prime(
        &mut self,
        cue: CueId,
        offset: u64,
        env: &Env<'_>,
    ) -> Option<(PreparedStream, Option<u64>)> {
        let usable = offset == 0
            && self
                .primes
                .get(&cue)
                .is_some_and(|p| sig_of(env, cue).as_ref() == Some(&p.sig));
        let prime = self.primes.remove(&cue)?;
        if usable {
            self.prime_hits = self.prime_hits.saturating_add(1);
        }
        usable.then_some((prime.prepared, prime.total))
    }

    /// Keeps exactly `wanted` primed (in order, at most `max`): new ones are prepared, ones no
    /// longer wanted or out of date are released.
    pub fn prime(&mut self, wanted: &[CueId], max: usize, env: &mut Env<'_>) {
        let wanted: Vec<CueId> = wanted.iter().copied().take(max).collect();
        self.primes
            .retain(|cue, p| wanted.contains(cue) && sig_of(env, *cue).as_ref() == Some(&p.sig));
        for cue in wanted {
            if self.primes.contains_key(&cue)
                || self.by_run.values().any(|id| {
                    self.recs
                        .get(id)
                        .is_some_and(|r| r.cue == cue && !r.preview)
                })
            {
                continue;
            }
            let Some(sig) = sig_of(env, cue) else {
                continue;
            };
            // A prime that cannot be made now (probe pending or failed) is tried again later.
            let mut quiet = Effects::default();
            if let Ok((prepared, total)) = self.prepare(cue, 0, env, &mut quiet) {
                self.primes.insert(
                    cue,
                    Prime {
                        prepared,
                        total,
                        sig,
                    },
                );
            }
        }
    }

    /// Checks the streams of live voices for failures: each failed run is reported once.
    pub fn poll_streams(&mut self, fx: &mut Effects, ws: &Workspace) {
        for r in self.recs.values_mut() {
            if r.stream_failed {
                continue;
            }
            let failed = match &r.phase {
                Phase::Playing => r.stream.as_ref().is_some_and(|h| h.error().is_some()),
                Phase::Waiting {
                    prepared: Some((p, _)),
                    ..
                } => p.handle.error().is_some(),
                _ => false,
            };
            if failed {
                r.stream_failed = true;
                fx.feedback.push(r.failure(BrokenReason::StreamFailed));
                if let Some(m) = cue_media(ws, r.cue) {
                    fx.reprobe.push(m);
                }
            }
        }
        // A prime whose stream failed is useless; the next priming pass makes it again.
        self.primes
            .retain(|_, p| p.prepared.handle.error().is_none());
    }

    /// Turns one mixer event into feedback.
    pub fn event(&mut self, e: Event, fx: &mut Effects) {
        match e {
            Event::VoiceFinished { voice, at, reason } => self.finished(voice, at, reason, fx),
            Event::VoiceRejected { voice, at, .. } => {
                let Some((id, _)) = self.resolve(voice) else {
                    return;
                };
                let Some(r) = self.remove(id) else { return };
                // A refused start: the voice never played (M4.14). Decided by the id the mixer
                // refused, not by whether a resume was sent since (a start refused while a
                // pause and resume were on their way is still a start that never played).
                fx.feedback.push(failure(
                    r.cue,
                    r.run,
                    BrokenReason::VoiceRejected,
                    r.start_id == Some(voice),
                ));
                // A refused resume leaves the voice paused under its old id: let it go.
                if voice == id
                    && let Some(old) = r.prev_id
                {
                    fx.commands.push((
                        at,
                        CommandKind::StopVoice {
                            voice: old,
                            fade: Fade::NONE,
                        },
                    ));
                }
            }
            Event::DevampScheduled { voice, at } => {
                if let Some((cue, run)) = self.name(voice) {
                    fx.feedback.push(Input::DevampScheduled { cue, run, at });
                    // The voice's end is known again: where it leaves the loop plus what
                    // follows (nothing for a devamp stop).
                    if let Some(r) = self.recs.get(&voice)
                        && !r.preview
                        && let Some(frames) = r.length_after_devamp(at)
                    {
                        fx.feedback.push(Input::ActionLength { cue, run, frames });
                    }
                }
            }
            Event::DevampIgnored { voice, .. } => {
                if let Some((cue, run)) = self.name(voice) {
                    fx.feedback.push(Input::DevampIgnored { cue, run });
                }
            }
            Event::Underrun { voice, .. } => {
                // A starved voice does not skip what it missed (docs/audio.md, Voices): it ends
                // later than planned by the underrun, while the cue engine keeps the planned end.
                if let Some(cue) = self.cue_of(voice) {
                    fx.notices.push((
                        "underrun",
                        "the disk could not keep up with a cue; it plays late from here".to_owned(),
                        Some(cue),
                    ));
                }
            }
            Event::PendingFull { .. } | Event::LoopReached { .. } | Event::SliceEnded { .. } => {}
        }
    }

    /// The live record an id (current or alias) belongs to, and whether it is the current id.
    fn resolve(&self, voice: VoiceId) -> Option<(VoiceId, bool)> {
        if self.recs.contains_key(&voice) {
            return Some((voice, true));
        }
        self.aliases.get(&voice).map(|&(cur, _)| (cur, false))
    }

    /// The cue and run an id stood for (an alias keeps its old run, so its feedback is stale).
    fn name(&self, voice: VoiceId) -> Option<(CueId, RunId)> {
        if let Some(r) = self.recs.get(&voice) {
            return Some((r.cue, r.run));
        }
        let &(cur, run) = self.aliases.get(&voice)?;
        self.recs.get(&cur).map(|r| (r.cue, run))
    }

    fn finished(&mut self, voice: VoiceId, at: SampleTime, reason: FinishReason, fx: &mut Effects) {
        let Some((id, current)) = self.resolve(voice) else {
            return;
        };
        let Some(r) = self.recs.get_mut(&id) else {
            return;
        };
        let (cue, run) = (r.cue, r.run);
        if !current {
            // The voice ended before the resume that re-keyed it applied, so the mixer ignored
            // the resume: report the end with the fresh run (M2-E (a)), after the resume.
            if let Some(r) = self.remove(id) {
                let at = at.max(r.resumed_at.unwrap_or(at));
                fx.deferred.push((at, Input::ActionEnded { cue, run, at }));
            }
            return;
        }
        let failed = r.stream_failed
            || (reason == FinishReason::EndOfStream
                && r.stream.as_ref().is_some_and(|h| h.error().is_some()));
        // Ended after a pending pause: the engine ignores the report (a paused voice cannot
        // end), so keep the record for the resume to report the end afresh. An end on the pause
        // sample left nothing to pause: the engine takes it as the end (M2.34) and resumes
        // nothing, so a record kept for it would stay for the session (and, for a show cue,
        // keep the cue from being primed again).
        if !failed
            && let Some(p) = r.paused_at
            && at > p
            && reason != FinishReason::Stopped
        {
            r.phase = Phase::Ended;
            r.stream = None;
            fx.feedback.push(Input::ActionEnded { cue, run, at });
            return;
        }
        let gone = self.remove(id);
        if failed {
            let input = gone.map_or(
                Input::ActionFailed {
                    cue,
                    run,
                    reason: BrokenReason::StreamFailed,
                },
                |r| r.failure(BrokenReason::StreamFailed),
            );
            fx.feedback.push(input);
        } else {
            fx.feedback.push(Input::ActionEnded { cue, run, at });
        }
    }

    /// Ids that left since the last meter read, with their cues. Cleared by the caller's read.
    pub fn take_gone(&mut self) -> Vec<(VoiceId, CueId)> {
        std::mem::take(&mut self.gone)
    }

    /// The cue a mixer voice id belongs to (current or alias).
    pub fn cue_of(&self, voice: VoiceId) -> Option<CueId> {
        self.resolve(voice)
            .and_then(|(id, _)| self.recs.get(&id))
            .map(|r| r.cue)
    }

    /// Started voices the mixer should be playing, with their start sample.
    pub fn started(&self) -> impl Iterator<Item = (VoiceId, SampleTime)> + '_ {
        self.recs
            .iter()
            .filter(|(_, r)| matches!(r.phase, Phase::Playing))
            .map(|(id, r)| (*id, r.start_at))
    }

    /// A voice the mixer no longer has although no end was reported (its event was dropped):
    /// reported as ended on `at` (or failed, if its stream failed).
    /// A voice no meter read ever listed was never taken by the mixer: its lost report was a
    /// rejection, so it fails (`voice_rejected`) rather than ends.
    pub fn lost(&mut self, id: VoiceId, at: SampleTime, fx: &mut Effects) {
        let Some(r) = self.recs.get(&id) else { return };
        if !matches!(r.phase, Phase::Playing) {
            return;
        }
        if r.listed {
            self.finished(id, at, FinishReason::EndOfStream, fx);
        } else if let Some(r) = self.remove(id) {
            // Never listed: the mixer never took its start, so it never played.
            fx.feedback.push(Input::StartFailed {
                cue: r.cue,
                run: r.run,
                reason: BrokenReason::VoiceRejected,
            });
        }
    }

    pub fn mark_absent(&mut self, id: VoiceId, absent: bool) -> u8 {
        match self.recs.get_mut(&id) {
            Some(r) if absent => {
                r.absent = r.absent.saturating_add(1);
                r.absent
            }
            Some(r) => {
                r.absent = 0;
                r.listed = true;
                0
            }
            None => 0,
        }
    }

    /// When a stop dated `at` is sent to voice `id`. While the device is down (`floor`: the
    /// first sample the mixer renders when it is back), `now` is a block past that sample, so
    /// a stop dated `at` would let the voice play one more block after the outage: it is dated
    /// on the floor instead, but never before the voice's own (re)start, which may itself be
    /// queued after the floor.
    pub fn stop_date(&self, id: VoiceId, at: SampleTime, floor: Option<SampleTime>) -> SampleTime {
        let Some(floor) = floor else { return at };
        let Some(r) = self.recs.get(&id) else {
            return at;
        };
        let earliest = floor.max(r.start_at).max(r.resumed_at.unwrap_or(0));
        at.min(earliest)
    }

    /// Runs whose voice is on the device: started in the mixer, or started and waiting for
    /// their file's probe. Loaded runs are not (a load only prepares a stream).
    pub fn on_device(&self) -> Vec<(CueId, RunId)> {
        let mut v: Vec<(CueId, RunId)> = self
            .recs
            .values()
            .filter(|r| {
                matches!(
                    r.phase,
                    Phase::Playing | Phase::Waiting { start: Some(_), .. }
                )
            })
            .map(|r| (r.cue, r.run))
            .collect();
        v.sort();
        v
    }

    /// Mixer patch slots a voice in the mixer may still play through (started, paused or
    /// stopping; previews and orphans included): a slot is reused only once none does.
    pub fn slots_in_use(&self) -> BTreeSet<u16> {
        self.recs
            .values()
            .filter(|r| matches!(r.phase, Phase::Playing))
            .filter_map(|r| r.slot)
            .collect()
    }

    /// Every voice started in the mixer (previews included).
    pub fn playing_ids(&self) -> Vec<VoiceId> {
        let mut v: Vec<VoiceId> = self
            .recs
            .iter()
            .filter(|(_, r)| matches!(r.phase, Phase::Playing))
            .map(|(id, _)| *id)
            .collect();
        v.sort_by_key(|id| id.0);
        v
    }

    /// Show voices in the mixer (not stopped by a command already) for which `orphan(cue, run)`
    /// holds.
    pub fn orphans(&self, orphan: impl Fn(CueId, RunId) -> bool) -> Vec<VoiceId> {
        self.recs
            .iter()
            .filter(|(_, r)| matches!(r.phase, Phase::Playing) && !r.preview)
            .filter(|(_, r)| orphan(r.cue, r.run))
            .map(|(id, _)| *id)
            .collect()
    }

    /// Fails starts that have waited for their file's probe since before `now - limit`: the
    /// cue goes broken instead of showing as running without a sound.
    pub fn expire_held(&mut self, now: SampleTime, limit: SampleTime, fx: &mut Effects) {
        let expired: Vec<VoiceId> = self
            .recs
            .iter()
            .filter(|(_, r)| r.held.is_some_and(|t| now >= t.saturating_add(limit)))
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            if let Some(r) = self.remove(id) {
                // A held start never reached the mixer: nothing played.
                fx.feedback.push(Input::StartFailed {
                    cue: r.cue,
                    run: r.run,
                    reason: BrokenReason::StreamFailed,
                });
                fx.notices.push((
                    "probe_timeout",
                    "a cue's audio file could not be read in time; the cue did not play".to_owned(),
                    Some(r.cue),
                ));
            }
        }
    }

    /// Show cues playing on rendered sample `t`: position in the action and frames left.
    pub fn progress(&self, t: SampleTime) -> Vec<(CueId, u64, Option<u64>)> {
        let mut v: Vec<(CueId, u64, Option<u64>)> = self
            .recs
            .values()
            .filter(|r| !r.preview)
            .filter_map(|r| {
                let pos = r.position(t)?;
                let left = r
                    .total
                    .map(|total| r.offset.saturating_add(total).saturating_sub(pos));
                Some((r.cue, pos, left))
            })
            .collect();
        v.sort_by_key(|(c, _, _)| *c);
        v
    }

    /// Every voice is gone and nothing is held for one: a stopped show leaks nothing.
    pub fn is_idle(&self) -> bool {
        self.recs.is_empty() && self.by_run.is_empty() && self.aliases.is_empty()
    }

    /// Forgets every voice, alias and prime (a new workspace replaces the engine they belong
    /// to). The mixer is stopped separately; its late events find no record and are ignored. Ids
    /// are never reused, so they cannot be taken for the new workspace's voices.
    pub fn forget_all(&mut self) {
        self.recs.clear();
        self.by_run.clear();
        self.aliases.clear();
        self.primes.clear();
        self.gone.clear();
    }

    /// Lets go of every primed stream (a panic, shutdown).
    pub fn clear_primes(&mut self) {
        self.primes.clear();
    }

    pub fn has_playing(&self) -> bool {
        self.recs
            .values()
            .any(|r| matches!(r.phase, Phase::Playing))
    }

    /// Stream handles of playing voices, with the frames each voice will have read by rendered
    /// sample `t` (offline settling).
    pub fn stream_needs(&self, t: SampleTime) -> impl Iterator<Item = (&StreamHandle, u64)> {
        self.recs.values().filter_map(move |r| {
            let h = r.stream.as_ref()?;
            let pos = r.position(t)?;
            Some((h, pos.saturating_sub(r.offset)))
        })
    }

    /// Streams prepared but not playing yet (loads and primes).
    pub fn waiting_streams(&self) -> impl Iterator<Item = &StreamHandle> {
        self.recs
            .values()
            .filter_map(|r| match &r.phase {
                Phase::Waiting {
                    prepared: Some((p, _)),
                    ..
                } => Some(&p.handle),
                _ => None,
            })
            .chain(self.primes.values().map(|p| &p.prepared.handle))
    }
}

impl VoiceRec {
    /// The action's length once a devamp leaves the loop it plays on sample `at`: where it is
    /// then, plus the rest of its table after that loop (nothing for a devamp stop). `None`
    /// while that rest loops for ever.
    fn length_after_devamp(&self, at: SampleTime) -> Option<u64> {
        let played = at
            .checked_sub(self.start_at)?
            .saturating_sub(self.paused_frames);
        let rest = if self.devamp_stop {
            0
        } else {
            rest_after_loop(&self.table, played)?
        };
        Some(
            self.offset
                .saturating_add(played)
                .saturating_add(rest)
                .saturating_add(self.delay),
        )
    }
}

/// Frames the table plays after the loop entry whose play ends on frame `played` (counted from
/// the table's start), skipping the rest of that slice (its later plays and, for a slice split
/// by a start offset, its second entry). `None` if what follows loops for ever.
pub fn rest_after_loop(table: &[Slice], played: u64) -> Option<u64> {
    // The entry the last frame before the exit belongs to.
    let last = played.checked_sub(1)?;
    let mut pos = 0u64;
    let mut current = None;
    for (k, s) in table.iter().enumerate() {
        let len = match s.plays {
            Plays::Times(n) => s.frames.saturating_mul(u64::from(n)),
            Plays::Infinite => u64::MAX,
        };
        if last < pos.saturating_add(len) {
            current = Some(k);
            break;
        }
        pos = pos.saturating_add(len);
    }
    let k = current?;
    let index = table.get(k)?.index;
    table
        .iter()
        .skip(k + 1)
        .skip_while(|s| s.index == index)
        .try_fold(0u64, |acc, s| match s.plays {
            Plays::Times(n) => Some(acc.saturating_add(s.frames.saturating_mul(u64::from(n)))),
            Plays::Infinite => None,
        })
}

fn failure(cue: CueId, run: RunId, reason: BrokenReason, unplayed: bool) -> Input {
    if unplayed {
        Input::StartFailed { cue, run, reason }
    } else {
        Input::ActionFailed { cue, run, reason }
    }
}

fn rec(cue: CueId, run: RunId, preview: bool) -> VoiceRec {
    VoiceRec {
        cue,
        run,
        preview,
        phase: Phase::Playing,
        stream: None,
        offset: 0,
        start_at: 0,
        total: None,
        paused_at: None,
        paused_frames: 0,
        resumed_at: None,
        stream_failed: false,
        prev_id: None,
        absent: 0,
        listed: false,
        table: Vec::new(),
        devamp_stop: false,
        held: None,
        delay: 0,
        start_id: None,
        slot: None,
    }
}

fn queued(id: VoiceId, q: Queued) -> CommandKind {
    match q {
        Queued::Ramp { target, gain, fade } => CommandKind::Ramp {
            voice: id,
            target,
            gain,
            fade,
        },
        Queued::Devamp { stop } => CommandKind::Devamp { voice: id, stop },
    }
}

pub fn to_curve(c: FadeCurve) -> Curve {
    match c {
        FadeCurve::Linear => Curve::Linear,
        FadeCurve::EqualPower => Curve::EqualPower,
        FadeCurve::SCurve => Curve::SCurve,
        FadeCurve::Logarithmic => Curve::Logarithmic,
        FadeCurve::Exponential => Curve::Exponential,
    }
}

fn target(p: LevelParam) -> GainTarget {
    let clamp = |n: u32| u16::try_from(n).unwrap_or(u16::MAX);
    match p {
        LevelParam::Master => GainTarget::Master,
        LevelParam::Output(o) => GainTarget::CueOutput(clamp(o)),
        LevelParam::Crosspoint { input, output } => GainTarget::Crosspoint {
            input: clamp(input),
            output: clamp(output),
        },
    }
}

fn plan_spec(kind: &CueKind) -> Option<PlanSpec<'_>> {
    match kind {
        CueKind::Audio {
            start_time,
            end_time,
            slices,
            last_slice_play_count,
            ..
        } => Some(PlanSpec {
            start_time: *start_time,
            end_time: *end_time,
            slices,
            last_slice_play_count: *last_slice_play_count,
        }),
        _ => None,
    }
}

fn cue_media(ws: &Workspace, cue: CueId) -> Option<String> {
    match &ws.find_cue(cue)?.kind {
        CueKind::Audio { media: Some(m), .. } => Some(m.as_str().to_owned()),
        _ => None,
    }
}

fn sig_of(env: &Env<'_>, cue: CueId) -> Option<Sig> {
    let c = env.ws.find_cue(cue)?;
    let CueKind::Audio {
        media: Some(media),
        start_time,
        end_time,
        slices,
        last_slice_play_count,
        ..
    } = &c.kind
    else {
        return None;
    };
    let Some(Probe::Ready(ready)) = env.media.get(media.as_str()) else {
        return None;
    };
    Some(Sig {
        media: media.as_str().to_owned(),
        probe: Arc::as_ptr(ready) as usize,
        preloaded: env.media.is_preloaded(media.as_str()),
        start: start_time.to_bits(),
        end: end_time.map(f64::to_bits),
        marks: slices
            .iter()
            .map(|m| (m.time.to_bits(), m.play_count))
            .collect(),
        last: *last_slice_play_count,
    })
}

fn frames(spec: &FadeSpec, rate: NonZeroU32) -> Fade {
    Fade::new(
        seconds_to_samples(spec.duration, rate),
        to_curve(spec.curve),
    )
}

/// The mixer start of `cue`'s voice `id` on `prepared`, routed and levelled as the model says
/// now: master, cue outputs (those beyond the patch's outputs are silent), crosspoints, fades.
fn voice_start(
    env: &Env<'_>,
    cue: CueId,
    prepared: PreparedStream,
    id: VoiceId,
    offset: u64,
) -> Option<(Box<VoiceStart>, StreamHandle)> {
    let c = env.ws.find_cue(cue)?;
    let CueKind::Audio {
        patch,
        levels,
        fade_in,
        fade_out,
        ..
    } = &c.kind
    else {
        return None;
    };
    let mixer_patch = env.patches.mixer_patch(*patch)?;
    let patch_outputs = env.patches.outputs(*patch);
    let (mut vs, handle) = prepared.into_voice(id, env.cue_outputs);
    vs.patch = mixer_patch;
    vs.gain = db_to_gain(levels.master_db);
    for (o, g) in vs.cue_outputs.iter_mut().enumerate() {
        *g = match levels.outputs.get(o) {
            Some(out) if o < patch_outputs && !out.mute => db_to_gain(out.db),
            _ => 0.0,
        };
    }
    for xp in &levels.crosspoints {
        let (Ok(input), Ok(output)) = (usize::try_from(xp.input), usize::try_from(xp.output))
        else {
            continue;
        };
        if !xp.mute && output < patch_outputs {
            vs.set_crosspoint(input, output, db_to_gain(xp.db));
        }
    }
    // A start part way in (load to time) begins past the fade-in.
    vs.fade_in = if offset == 0 {
        frames(fade_in, env.rate)
    } else {
        Fade::NONE
    };
    vs.fade_out = frames(fade_out, env.rate);
    Some((Box::new(vs), handle))
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
    fn rest_after_a_loop_is_what_follows_it() {
        // Intro 100, loop 200 for ever, outro 50.
        let t = [
            s(0, 0, 100, Plays::Times(1)),
            s(1, 100, 200, Plays::Infinite),
            s(2, 300, 50, Plays::Times(1)),
        ];
        // Leaving the loop at the end of its third play.
        assert_eq!(rest_after_loop(&t, 100 + 3 * 200), Some(50));
        // A finite loop's later plays are skipped too.
        let t = [
            s(0, 0, 100, Plays::Times(4)),
            s(1, 100, 30, Plays::Times(2)),
        ];
        assert_eq!(rest_after_loop(&t, 200), Some(60));
        // A start offset split the loop into a one-play entry and the rest: both are left.
        let t = [
            s(0, 40, 60, Plays::Times(1)),
            s(0, 0, 100, Plays::Infinite),
            s(1, 100, 10, Plays::Times(1)),
        ];
        assert_eq!(rest_after_loop(&t, 60), Some(10));
        // What follows loops for ever: no length.
        let t = [
            s(0, 0, 100, Plays::Infinite),
            s(1, 100, 10, Plays::Infinite),
        ];
        assert_eq!(rest_after_loop(&t, 100), None);
        assert_eq!(rest_after_loop(&t, 0), None);
    }

    /// M2.34: a voice the mixer reports ended on its pause sample is gone (the engine takes it
    /// as the end and sends no resume); one ended after it is kept for the resume. Show cues and
    /// previews alike: a kept show-cue record would block its cue's prime.
    #[test]
    fn an_end_on_the_pause_sample_frees_the_record_and_one_after_it_keeps_it() {
        let cue = CueId(uuid::Uuid::from_u128(1));
        for preview in [false, true] {
            for (at, kept) in [(99, false), (100, false), (101, true)] {
                let mut v = Voices::new();
                let run = RunId(7);
                let mut r = rec(cue, run, preview);
                r.paused_at = Some(100);
                v.insert(VoiceId(1), r);
                let mut fx = Effects::default();
                v.finished(VoiceId(1), at, FinishReason::EndOfStream, &mut fx);
                let case = format!("preview {preview}, end on {at}");
                assert_eq!(
                    fx.feedback,
                    vec![Input::ActionEnded { cue, run, at }],
                    "{case}"
                );
                assert_eq!(v.live(), usize::from(kept), "{case}");
                assert_eq!(v.is_idle(), !kept, "{case}");
            }
        }
    }
}
