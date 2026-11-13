// SPDX-License-Identifier: GPL-3.0-or-later

//! The cue engine's voice actions carried out on the mixer and the disk streamer, and the
//! mixer's events turned back into feedback.
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
    CommandKind, Curve, DiskStreamer, Event, Fade, FinishReason, GainTarget, MAX_SLICES, Plays,
    PreparedStream, Slice, StreamHandle, StreamRequest, VoiceId, VoiceStart, total_frames,
};
use openlab_core::{
    Action, BrokenReason, Cue, CueEngine, CueId, CueKind, FadeCurve, FadeSpec, Input, LevelParam,
    PatchId, RunId, SampleTime, StopFade, Workspace, db_to_gain, seconds_to_samples,
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
    /// Feedback dated `at`, handed to the engine only once `now > at`.
    pub deferred: Vec<(SampleTime, Input)>,
    /// Notices for the operator (`code`, message, cue).
    pub notices: Vec<(&'static str, String, Option<CueId>)>,
    /// Media to probe again (a stream of it failed).
    pub reprobe: Vec<String>,
}

/// The environment voice actions are carried out in.
pub struct Env<'a> {
    /// The doc the voices belong to: new ids are recorded as its, and patch slots are its.
    pub doc: super::DocId,
    pub ws: &'a Workspace,
    pub media: &'a MediaCatalog,
    pub streamer: &'a mut DiskStreamer,
    pub patches: &'a Patches,
    /// Where new voice ids come from (shared by the engine, not the voice table).
    pub ids: &'a mut VoiceIds,
    pub rate: NonZeroU32,
    /// The mixers' cue outputs: the widest a voice can be.
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
    /// The file its stream reads (the cue's file when the record was made, which a relink
    /// since does not change): probed again when the stream fails.
    media: Option<String>,
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
    /// Table frames played when each accepted devamp leaves its loop, in order: where the file
    /// position jumps on (see [`VoiceRec::file_frame`]).
    cuts: Vec<u64>,
    /// Its start waited for the file's probe, since this sample (`now`).
    held: Option<SampleTime>,
    /// How much later than the engine dated it the voice started (held by its probe).
    delay: u64,
    /// The id its `StartVoice` was sent with: a rejection under that id refused the start, so
    /// the voice never played (any other rejection is of a resume).
    start_id: Option<VoiceId>,
    /// The mixer patch slot it plays through: the one its start was routed through, or the one
    /// it was moved to when its cue's patch changed ([`Voices::follow_patches`]).
    slot: Option<u16>,
    /// The workspace patch `slot` belongs to.
    patch: Option<PatchId>,
    /// Slots it was moved off: its crossfade out of them may still be playing, so they count as
    /// in use until the mixer reports it settled after every move sent (`moves`) or the voice
    /// ends (at most one per patch slot).
    former: Vec<u16>,
    /// `MoveVoice`s sent for it (wrapping), matched against the mixer's `Event::MoveEnded`.
    moves: u32,
    /// Its cue outputs below this have their levels; those from here on were silenced at its
    /// start, beyond the outputs of the patch it started on ([`voice_start`]).
    levelled: usize,
}

impl VoiceRec {
    /// The failure to report for this voice: `StartFailed` if it never produced any audio (a
    /// start not yet in the mixer, or a stream that failed before writing a frame), so the cue
    /// engine skips the cue (QLab P5/G2); else `ActionFailed`. A loaded voice that was
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

/// A prepared stream kept ready for a cue near the playhead.
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
    /// more.
    gone: Vec<(VoiceId, CueId)>,
    /// A show cue's record left since the last [`Voices::take_freed`]: a priming pass skips a
    /// cue that still has one, so the one after it is gone may prime the cue again.
    freed: bool,
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

    /// Whether a show cue's record left since the last call (and clears it).
    pub fn take_freed(&mut self) -> bool {
        std::mem::take(&mut self.freed)
    }

    /// Starts that used a primed stream.
    pub fn prime_hits(&self) -> u64 {
        self.prime_hits
    }

    /// Whether `cue`'s primed stream reads preloaded samples (not the file).
    pub fn primed_from_memory(&self, cue: CueId) -> bool {
        self.primes.get(&cue).is_some_and(|p| p.sig.preloaded)
    }
}

mod actions;
mod events;
mod priming;
mod queries;

impl VoiceRec {
    /// Notes that an accepted devamp leaves its loop on sample `at`. Answered again for the same
    /// devamp, so a cut is kept once; the table is at most `MAX_SLICES` long, which bounds the
    /// cuts a voice can have (each needs a loop).
    fn note_devamp_cut(&mut self, at: SampleTime) {
        let Some(played) = at.checked_sub(self.start_at) else {
            return;
        };
        let played = played.saturating_sub(self.paused_frames);
        if self.cuts.last().is_none_or(|&c| played > c) && self.cuts.len() < MAX_SLICES {
            self.cuts.push(played);
        }
    }

    /// The frame of the media file the voice is at when the action is `pos` frames in (the
    /// table starts at the action offset); `None` without a table or past its end.
    fn file_frame(&self, pos: u64) -> Option<u64> {
        let played = pos.saturating_sub(self.offset);
        // A devamp stop ends the voice on the loop's boundary: nothing is played after it.
        if self.devamp_stop && self.cuts.last().is_some_and(|&c| c <= played) {
            return None;
        }
        file_frame(&self.table, &self.cuts, played)
    }

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

/// The entry a voice is in after playing `rem` frames from the start of entry `from` on, and the
/// frames it is into that entry's plays. `None` past the end of a finite table.
fn walk(table: &[Slice], from: usize, rem: u64) -> Option<(usize, u64)> {
    let mut left = rem;
    for (k, s) in table.iter().enumerate().skip(from) {
        let len = match s.plays {
            Plays::Times(n) => s.frames.saturating_mul(u64::from(n)),
            Plays::Infinite => u64::MAX,
        };
        if left < len {
            return Some((k, left));
        }
        left -= len;
    }
    None
}

/// Where in the media file (frames) a voice playing `table` is after `played` frames, given the
/// frames played when devamps left their loops (`cuts`, increasing). A devamp jumps to the first
/// entry of the slice after the loop's; the position runs on from that entry's start. `None`
/// without a table, past a finite table's end, or when a devamp left the table's last slice
/// (the voice ends there).
pub fn file_frame(table: &[Slice], cuts: &[u64], played: u64) -> Option<u64> {
    // The voice has played from entry `entry` since voice frame `from`.
    let (mut entry, mut from) = (0usize, 0u64);
    for &cut in cuts.iter().take_while(|&&c| c <= played) {
        let (k, _) = walk(table, entry, (cut - from).checked_sub(1)?)?;
        let index = table.get(k)?.index;
        entry = table
            .iter()
            .enumerate()
            .skip(k + 1)
            .find(|(_, s)| s.index != index)
            .map(|(n, _)| n)?;
        from = cut;
    }
    let (k, within) = walk(table, entry, played - from)?;
    let s = table.get(k)?;
    Some(s.start.saturating_add(within.checked_rem(s.frames)?))
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
        media: None,
        prev_id: None,
        absent: 0,
        listed: false,
        table: Vec::new(),
        devamp_stop: false,
        cuts: Vec::new(),
        held: None,
        delay: 0,
        start_id: None,
        slot: None,
        patch: None,
        former: Vec::new(),
        moves: 0,
        levelled: 0,
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

/// The cue `id`, through the catalog's cue index (a step per level, not a search of the show);
/// a cue the index does not place is looked for in the workspace.
pub(super) fn cue_of<'a>(env: &Env<'a>, id: CueId) -> Option<&'a Cue> {
    env.media
        .index()
        .locate(env.ws, id)
        .or_else(|| env.ws.find_cue(id))
}

fn sig_of(env: &Env<'_>, cue: CueId) -> Option<Sig> {
    let c = cue_of(env, cue)?;
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
    let mixer_patch = env.patches.mixer_patch(env.doc, *patch)?;
    let patch_outputs = env.patches.outputs(env.doc, *patch);
    // As many cue outputs as the patch has (a voice costs the mixer only those); a later move
    // to a bigger patch widens it with the ramps that give the new outputs their levels.
    let width = patch_outputs.clamp(1, env.cue_outputs.max(1));
    let (mut vs, handle) = prepared.into_voice(id, width);
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

    #[test]
    fn file_frame_follows_loops_and_each_devamp_cut() {
        let s = |index, start, frames, plays| Slice {
            index,
            start,
            frames,
            plays,
        };
        // Slice 0 (0..100) twice, slice 1 (100..200) for ever, slice 2 (200..300) for ever,
        // slice 3 (300..350) once.
        let t = [
            s(0, 0, 100, Plays::Times(2)),
            s(1, 100, 100, Plays::Infinite),
            s(2, 200, 100, Plays::Infinite),
            s(3, 300, 50, Plays::Times(1)),
        ];
        assert_eq!(file_frame(&t, &[], 0), Some(0));
        assert_eq!(file_frame(&t, &[], 150), Some(50));
        assert_eq!(file_frame(&t, &[], 450), Some(150));
        // Devamped out of slice 1 at 450 (a play boundary: 200 + 2 * 100 + 50 is not, so the
        // cut is at 500).
        assert_eq!(file_frame(&t, &[500], 499), Some(199));
        assert_eq!(file_frame(&t, &[500], 530), Some(230));
        // A second devamp, out of slice 2 at 700.
        assert_eq!(file_frame(&t, &[500, 700], 699), Some(299));
        assert_eq!(file_frame(&t, &[500, 700], 720), Some(320));
        assert_eq!(file_frame(&t, &[500, 700], 749), Some(349));
        // Past the table's end, and a devamp out of its last slice: the voice has ended.
        assert_eq!(file_frame(&t, &[500, 700], 750), None);
        assert_eq!(file_frame(&t[3..], &[10], 20), None);
        assert_eq!(file_frame(&[], &[], 0), None);
    }

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

    /// A voice the mixer reports ended on its pause sample is gone (the engine takes it
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

    /// A voice the mixer dropped for faulting breaks its cue with a notice, also after a
    /// pending pause (a fault is not an end the resume could report).
    #[test]
    fn a_faulted_voice_breaks_its_cue_with_a_notice() {
        let cue = CueId(uuid::Uuid::from_u128(1));
        for paused_at in [None, Some(100)] {
            let mut v = Voices::new();
            let run = RunId(7);
            let mut r = rec(cue, run, false);
            r.paused_at = paused_at;
            v.insert(VoiceId(1), r);
            let mut fx = Effects::default();
            v.finished(VoiceId(1), 200, FinishReason::Faulted, &mut fx);
            assert!(
                matches!(
                    fx.feedback.as_slice(),
                    [Input::ActionFailed {
                        reason: BrokenReason::StreamFailed,
                        ..
                    } | Input::StartFailed {
                        reason: BrokenReason::StreamFailed,
                        ..
                    }]
                ),
                "{:?}",
                fx.feedback
            );
            assert_eq!(fx.notices.len(), 1);
            assert_eq!(fx.notices[0].0, "audio_fault");
            assert_eq!(fx.notices[0].2, Some(cue));
            assert!(v.is_idle());
        }
    }

    /// The mixer disabled a patch slot: the voices on it (and only those) stop and break their
    /// cues; a stop is never dated before the voice's own start.
    #[test]
    fn the_voices_on_a_disabled_slot_stop_and_break_their_cues() {
        let cue = |n| CueId(uuid::Uuid::from_u128(n));
        let mut v = Voices::new();
        let mut on = rec(cue(1), RunId(1), false);
        on.slot = Some(3);
        on.start_at = 500;
        v.insert(VoiceId(1), on);
        let mut other = rec(cue(2), RunId(2), false);
        other.slot = Some(4);
        v.insert(VoiceId(2), other);
        let mut fx = Effects::default();
        v.fail_slot(3, 200, &mut fx);
        assert!(
            matches!(
                fx.commands.as_slice(),
                [(
                    500,
                    CommandKind::StopVoice {
                        voice: VoiceId(1),
                        ..
                    }
                )]
            ),
            "{:?}",
            fx.commands
        );
        assert!(matches!(
            fx.feedback.as_slice(),
            [Input::ActionFailed { cue: c, .. } | Input::StartFailed { cue: c, .. }] if *c == cue(1)
        ));
        assert_eq!(fx.notices[0].0, "audio_fault");
        assert_eq!(v.slots_in_use(), BTreeSet::from([4]));
    }

    /// A voice moved to another patch (#293) keeps the slot it left in use: its crossfade out
    /// of it may still be playing, so the slot must not be zeroed or handed out under it.
    #[test]
    fn a_moved_voice_keeps_the_slot_it_left_in_use() {
        let mut v = Voices::new();
        let mut moved = rec(CueId(uuid::Uuid::from_u128(1)), RunId(1), false);
        moved.slot = Some(3);
        moved.former = vec![1];
        v.insert(VoiceId(1), moved);
        assert_eq!(v.slots_in_use(), BTreeSet::from([1, 3]));
    }

    /// Regression (#302): a voice dropped under its old id while the resume that re-keyed it
    /// is on its way breaks its cue too, rather than ending it.
    #[test]
    fn a_faulted_voice_under_its_old_id_breaks_its_cue() {
        let cue = CueId(uuid::Uuid::from_u128(1));
        let mut v = Voices::new();
        v.insert(VoiceId(2), rec(cue, RunId(8), false));
        v.aliases.insert(VoiceId(1), (VoiceId(2), RunId(7)));
        let mut fx = Effects::default();
        v.finished(VoiceId(1), 200, FinishReason::Faulted, &mut fx);
        assert!(fx.deferred.is_empty(), "{:?}", fx.deferred);
        assert!(
            matches!(
                fx.feedback.as_slice(),
                [Input::ActionFailed {
                    reason: BrokenReason::StreamFailed,
                    ..
                } | Input::StartFailed {
                    reason: BrokenReason::StreamFailed,
                    ..
                }]
            ),
            "{:?}",
            fx.feedback
        );
        assert_eq!(fx.notices.len(), 1);
        assert_eq!(fx.notices[0].0, "audio_fault");
    }
}
