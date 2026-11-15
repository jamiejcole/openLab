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
use super::plan::{PlanFailure, PlanSpec, cue_slices, cue_slices_from};
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
    /// Lines for the show log at `warn`.
    pub warnings: Vec<String>,
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
    /// The commit window: a voice start dated before `now + window` is in it; one later waits
    /// (a later stop must be able to reach it).
    pub window: SampleTime,
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
    /// It starts at this frame of the file (a seek, or a load there), not at `offset` (then 0).
    file: Option<u64>,
    /// The elapsed time it shows on its start: `offset`, or for a start at a file frame, how far
    /// into the action that frame is when the cue plays from the top.
    shown: u64,
    /// Made by a seek: it waits for its stream to be primed (at most [`SEEK_PRIME_WAIT`]) so
    /// the move crossfades rather than drops out, and fades in over [`SEEK_CROSSFADE`].
    moved: bool,
    /// The voice a seek moves from, playing on until this one starts and then fading out.
    replaces: Option<VoiceId>,
    /// Moved elsewhere by a seek: it is fading out (or going) and stands for no run; its end
    /// is reported to no one.
    retired: bool,
    /// A seek of this voice swaps no earlier than this sample: one crossfade at a time, so a
    /// burst of seeks never has more than two of the cue's voices sounding.
    swap_after: SampleTime,
    /// How the voice it `replaces` fades out when the seek is let go of before it swaps.
    release_fade: Fade,
    /// Made by a move to another device (`Action::RelocateVoice`): it goes on from where the
    /// voice it moves from is, so it shows that voice's elapsed time (`shown`, set when it is
    /// made) and starts on its planned sample, not before it is in the commit window.
    relocated: bool,
    /// The table a move to another device plays ([`move_plan`]), instead of one planned from
    /// `offset` or `file`.
    rest: Option<Vec<Slice>>,
    /// A move to another device whose stream is late is planned again until this sample, then
    /// given up ([`SEEK_PRIME_WAIT`] after its first planned swap).
    swap_limit: SampleTime,
    /// A move to another device given up (its stream was not ready in time) is tried again
    /// from this sample on (`Voices::take_due_moves`).
    retry_at: Option<SampleTime>,
    /// Moves of its run given up so far: each retry waits twice as long as the one before.
    gave_up: u32,
}

/// A cue's seeks prepare at most one stream per this many seconds: a burst of seeks (a stuck
/// key, a hostile client) replaces the one waiting instead of opening a stream each, so it can
/// never use up the streamer's open-stream limit or its memory.
pub const SEEK_PREPARE_INTERVAL: f64 = 0.05;

/// How long a seek's crossfade lasts: the voice it moves from fades out while the moved one
/// fades in, linear both, so the jump does not click.
pub const SEEK_CROSSFADE: f64 = 0.01;

/// The longest a seek of a playing voice waits for its stream at the new point to be primed,
/// the old voice playing on meanwhile; after it the move is made anyway and plays once the disk
/// has caught up (an underrun, silence, rather than a sound that never moves).
pub const SEEK_PRIME_WAIT: f64 = 0.25;

/// How far ahead a move to another device swaps the voices: the old device plays on meanwhile,
/// so the new one's stream can be read and primed from the point where the sound then is, and
/// the two crossfade in step. Short enough to be heard as at once.
pub const MOVE_LEAD: f64 = 0.1;

/// How long after a move to another device is given up (its stream not ready in time) it is
/// tried again; doubled after each give-up of the same run, at most 16 times this.
pub const MOVE_RETRY: f64 = SEEK_PRIME_WAIT;

/// Where a voice's stream starts.
#[derive(Clone, Debug, PartialEq, Eq)]
enum StartAt {
    /// Device frames into the action, loops unrolled (a start, a load to time).
    Offset(u64),
    /// A frame of the file at the device rate (a seek).
    File(u64),
    /// This slice table: what the voice a move to another device replaces still had to play.
    Table(Vec<Slice>),
}

/// A move to another device planned for one swap sample: the table the new voice plays (what
/// the old one still has to play from then), where in the action that is, and the elapsed time
/// it shows there.
struct MovePlan {
    table: Vec<Slice>,
    offset: u64,
    shown: u64,
}

/// Plans the move of voice `o` to another device on sample `swap` from its own state there:
/// its position, its table, the devamp cuts it has been given and whether the latest stops it.
/// `None` when it has nothing left to play by then (it ended, or will have).
fn move_plan(o: &VoiceRec, swap: SampleTime) -> Option<MovePlan> {
    let here = o.position(swap.max(o.start_at)).unwrap_or(o.offset);
    let played = here.saturating_sub(o.offset);
    let table = rest_of_table(&o.table, &o.cuts, o.devamp_stop, played)?;
    Some(MovePlan {
        table,
        offset: here,
        shown: o.shown.saturating_add(played),
    })
}

/// A stream made for a voice.
struct Made {
    prepared: PreparedStream,
    total: Option<u64>,
    /// The elapsed time it shows at its start (see [`VoiceRec::shown`]).
    shown: u64,
}

impl VoiceRec {
    /// The failure to report for this voice: `StartFailed` if it never produced any audio (a
    /// start not yet in the mixer, or a stream that failed before writing a frame), so the cue
    /// engine skips the cue (QLab P5/G2); else `ActionFailed`. A loaded voice that was
    /// never started stays `ActionFailed` (the cue is loaded, not playing).
    fn failure(&self, reason: BrokenReason) -> Input {
        // A voice moved by a seek is the cue's sound going on: it played before.
        let unplayed = !self.moved
            && match &self.phase {
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

    /// Where its stream starts, as planned for it.
    fn plan_of(&self) -> StartAt {
        match (&self.rest, self.file) {
            (Some(table), _) => StartAt::Table(table.clone()),
            (None, Some(frame)) => StartAt::File(frame),
            (None, None) => StartAt::Offset(self.offset),
        }
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
    /// Per cue, the sample before which a seek (or a load at a point of the file) prepares no
    /// stream ([`SEEK_PREPARE_INTERVAL`]).
    seek_gate: HashMap<CueId, SampleTime>,
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

/// What a voice playing `table` from its start still has to play after `played` frames, as a
/// table of its own (`cuts` and `stop` as for [`file_frame`]): the rest of the pass it is in,
/// the passes of that slice still to come and every entry after it. A devamp cut still ahead
/// ends that slice's plays there (it cuts on the end of a pass) and goes on with the entry after
/// the slice, or nothing for a devamp stop. `None` once nothing is left: past a finite table's
/// end, or past a devamp stop.
pub fn rest_of_table(table: &[Slice], cuts: &[u64], stop: bool, played: u64) -> Option<Vec<Slice>> {
    // The first entry after the slice of entry `k` (a slice split by a start offset is two).
    let after = |k: usize| -> Option<usize> {
        let index = table.get(k)?.index;
        Some(
            table
                .iter()
                .enumerate()
                .skip(k + 1)
                .find(|(_, s)| s.index != index)
                .map_or(table.len(), |(n, _)| n),
        )
    };
    let (mut entry, mut from) = (0usize, 0u64);
    for &cut in cuts.iter().take_while(|&&c| c <= played) {
        if stop {
            return None;
        }
        let (k, _) = walk(table, entry, (cut - from).checked_sub(1)?)?;
        entry = after(k)?;
        from = cut;
    }
    let (k, within) = walk(table, entry, played - from)?;
    let s = table.get(k)?;
    let pass = within.checked_div(s.frames)?;
    let at = within.checked_rem(s.frames)?;
    let mut rest = vec![Slice {
        start: s.start.saturating_add(at),
        frames: s.frames - at,
        plays: Plays::Times(1),
        ..*s
    }];
    match cuts.iter().find(|&&c| c > played) {
        Some(&cut) => {
            // Whole passes of the slice before the cut (it cuts on the end of a pass).
            let to_cut = (cut - from).saturating_sub(played - from);
            let full = to_cut.saturating_sub(s.frames - at) / s.frames;
            if let Ok(n @ 1..) = u32::try_from(full) {
                rest.push(Slice {
                    plays: Plays::Times(n),
                    ..*s
                });
            }
            if !stop {
                rest.extend(table.get(after(k)?..)?.iter().copied());
            }
        }
        None => {
            match s.plays {
                Plays::Infinite => rest.push(*s),
                Plays::Times(n) => {
                    let left = u64::from(n).saturating_sub(pass).saturating_sub(1);
                    if let Ok(left @ 1..) = u32::try_from(left) {
                        rest.push(Slice {
                            plays: Plays::Times(left),
                            ..*s
                        });
                    }
                }
            }
            rest.extend(table.get(k + 1..)?.iter().copied());
        }
    }
    Some(rest)
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
        file: None,
        shown: 0,
        moved: false,
        replaces: None,
        retired: false,
        swap_after: 0,
        release_fade: Fade::NONE,
        relocated: false,
        rest: None,
        swap_limit: 0,
        retry_at: None,
        gave_up: 0,
    }
}

/// The gain `target` has on a voice's first sample: what `vs` starts it with (the duck stage at
/// 1.0, its cap at the mixer's ceiling).
fn initial_gain(vs: &VoiceStart, target: GainTarget) -> f32 {
    let width = vs.cue_outputs.len();
    match target {
        GainTarget::Master => vs.gain,
        GainTarget::Duck => 1.0,
        GainTarget::DuckCap => openlab_audio::MAX_GAIN,
        GainTarget::CueOutput(o) => vs.cue_outputs.get(usize::from(o)).copied().unwrap_or(0.0),
        GainTarget::Crosspoint { input, output } => {
            let (i, o) = (usize::from(input), usize::from(output));
            if o >= width {
                return 0.0;
            }
            vs.crosspoints
                .get(i.saturating_mul(width).saturating_add(o))
                .copied()
                .unwrap_or(0.0)
        }
    }
}

/// One gain's ramp as the commands queued for a voice set it.
#[derive(Clone, Copy, Debug)]
struct Track {
    target: GainTarget,
    from: f32,
    to: f32,
    start: SampleTime,
    fade: Fade,
}

impl Track {
    /// The gain on sample `at`; the duck stage moves evenly in dB, as the mixer's does.
    fn value(&self, at: SampleTime) -> f32 {
        let pos = at.saturating_sub(self.start);
        if pos >= self.fade.frames {
            return self.to;
        }
        if self.target == GainTarget::Duck {
            let floor = 1e-6_f64;
            let from = f64::from(self.from).max(floor);
            let to = f64::from(self.to).max(floor);
            let t = pos as f64 / self.fade.frames as f64;
            return (from * (to / from).powf(t)) as f32;
        }
        openlab_audio::ramp_value(self.from, self.to, pos, self.fade.frames, self.fade.curve)
    }
}

/// The commands queued for a voice that starts on `at`, caught up with time: every gain a
/// command before `at` set is set on `at` to where it stands then (`initial` gives a gain's
/// value before any command), followed by the rest of a ramp still moving, so it ends on the
/// sample it was planned to; commands from `at` on (and devamps) are kept as they are, after.
fn caught_up(
    queue: Vec<(SampleTime, Queued)>,
    at: SampleTime,
    initial: impl Fn(GainTarget) -> f32,
) -> Vec<(SampleTime, Queued)> {
    let mut tracks: Vec<Track> = Vec::new();
    let mut later = Vec::new();
    for (t, q) in queue {
        match q {
            Queued::Ramp { target, gain, fade } if t < at => {
                let from = tracks
                    .iter()
                    .find(|k| k.target == target)
                    .map_or_else(|| initial(target), |k| k.value(t));
                // The mixer sets a voice's first cap at once (from no cap there is no step to
                // smooth), whatever the ramp asked.
                let fade = if target == GainTarget::DuckCap && from >= openlab_audio::MAX_GAIN {
                    Fade::NONE
                } else {
                    fade
                };
                let track = Track {
                    target,
                    from,
                    to: gain,
                    start: t,
                    fade,
                };
                match tracks.iter_mut().find(|k| k.target == target) {
                    Some(k) => *k = track,
                    None => tracks.push(track),
                }
            }
            Queued::Ramp { .. } => later.push((t, q)),
            Queued::Devamp { .. } => later.push((t.max(at), q)),
        }
    }
    let mut out = Vec::with_capacity(tracks.len() * 2 + later.len());
    for k in &tracks {
        out.push((
            at,
            Queued::Ramp {
                target: k.target,
                gain: k.value(at),
                fade: Fade::NONE,
            },
        ));
        let end = k.start.saturating_add(k.fade.frames);
        if end > at && k.value(at) != k.to {
            out.push((
                at,
                Queued::Ramp {
                    target: k.target,
                    gain: k.to,
                    fade: Fade::new(end - at, k.fade.curve),
                },
            ));
        }
    }
    out.extend(later);
    out
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

/// How a voice fades in on its first frames.
#[derive(Clone, Copy, Debug)]
enum FadeIn {
    /// The cue's own fade-in: a start from the top.
    Cue,
    /// None: a start part way in (load to time) begins past the fade-in.
    None,
    /// A seek's crossfade, the voice it moves from fading out meanwhile.
    Crossfade(Fade),
}

/// The mixer start of `cue`'s voice `id` on `prepared`, routed and levelled as the model says
/// now: master, cue outputs (those beyond the patch's outputs are silent), crosspoints, fades.
fn voice_start(
    env: &Env<'_>,
    cue: CueId,
    prepared: PreparedStream,
    id: VoiceId,
    fade_in: FadeIn,
) -> Option<(Box<VoiceStart>, StreamHandle)> {
    let c = env.ws.find_cue(cue)?;
    let CueKind::Audio {
        patch,
        levels,
        fade_in: cue_fade_in,
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
    vs.fade_in = match fade_in {
        FadeIn::Cue => frames(cue_fade_in, env.rate),
        FadeIn::None => Fade::NONE,
        FadeIn::Crossfade(fade) => fade,
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

    /// What a voice still has to play, for a move to another device: the rest of its pass, the
    /// passes to come and what follows; a devamp cut ahead ends the loop on its pass and goes on
    /// after the slice (nothing for a stop); nothing past the end or past a devamp stop.
    #[test]
    fn rest_of_table_follows_passes_cuts_and_stops() {
        let s = |index, start, frames, plays| Slice {
            index,
            start,
            frames,
            plays,
        };
        // Slice 0 (0..100) three times, slice 1 (100..200) for ever, slice 2 (200..250) twice.
        let t = [
            s(0, 0, 100, Plays::Times(3)),
            s(1, 100, 100, Plays::Infinite),
            s(2, 200, 50, Plays::Times(2)),
        ];
        // 30 into slice 0's second pass: 70 more of it, one more pass, then the rest.
        assert_eq!(
            rest_of_table(&t, &[], false, 130),
            Some(vec![
                s(0, 30, 70, Plays::Times(1)),
                s(0, 0, 100, Plays::Times(1)),
                t[1],
                t[2]
            ])
        );
        // In the loop with a devamp cut at the end of its pass after next: this pass's rest,
        // one whole pass, then slice 2 (none of it for a stop).
        let cut = 300 + 300;
        assert_eq!(
            rest_of_table(&t, &[cut], false, 420),
            Some(vec![
                s(1, 120, 80, Plays::Times(1)),
                s(1, 100, 100, Plays::Times(1)),
                t[2]
            ])
        );
        assert_eq!(
            rest_of_table(&t, &[cut], true, 420),
            Some(vec![
                s(1, 120, 80, Plays::Times(1)),
                s(1, 100, 100, Plays::Times(1))
            ])
        );
        // Past the cut: slice 2 from where the voice is in it, its second pass to come.
        assert_eq!(
            rest_of_table(&t, &[cut], false, cut + 10),
            Some(vec![
                s(2, 210, 40, Plays::Times(1)),
                s(2, 200, 50, Plays::Times(1))
            ])
        );
        // Past a devamp stop, or past the end of the table: nothing.
        assert_eq!(rest_of_table(&t, &[cut], true, cut), None);
        assert_eq!(rest_of_table(&t, &[cut], false, cut + 100), None);
        assert_eq!(rest_of_table(&[], &[], false, 0), None);
    }

    /// Commands queued while a move waited are caught up on the swap: each gain where it stands
    /// then and the rest of its ramp, ending on its planned sample; later commands kept.
    #[test]
    fn queued_ramps_are_caught_up_with_the_swap() {
        let ramp = |target, gain, frames| Queued::Ramp {
            target,
            gain,
            fade: Fade::new(frames, Curve::Linear),
        };
        let queue = vec![
            // Master from 1.0 to 0.0 over 100 from 100: half way on 150.
            (100, ramp(GainTarget::Master, 0.0, 100)),
            // An output set at once, then over by the swap.
            (110, ramp(GainTarget::CueOutput(1), 0.25, 0)),
            (120, ramp(GainTarget::CueOutput(0), 0.5, 10)),
            // A first cap is set at once, as the mixer sets it (no ramp down from no cap).
            (100, ramp(GainTarget::DuckCap, 4.0, 100)),
            // The duck stage halves in dB terms: from 1.0 to 0.01 over 100 from 100.
            (100, ramp(GainTarget::Duck, 0.01, 100)),
            (150, ramp(GainTarget::Master, 1.0, 0)),
            (160, Queued::Devamp { stop: false }),
        ];
        let out = caught_up(queue, 150, |t| match t {
            GainTarget::CueOutput(_) => 2.0,
            GainTarget::DuckCap => openlab_audio::MAX_GAIN,
            _ => 1.0,
        });
        type Got = (SampleTime, Option<(GainTarget, f32, u64)>);
        let got: Vec<Got> = out
            .iter()
            .map(|(t, q)| match q {
                Queued::Ramp { target, gain, fade } => (*t, Some((*target, *gain, fade.frames))),
                Queued::Devamp { .. } => (*t, None),
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (150, Some((GainTarget::Master, 0.5, 0))),
                (150, Some((GainTarget::Master, 0.0, 50))),
                (150, Some((GainTarget::CueOutput(1), 0.25, 0))),
                (150, Some((GainTarget::CueOutput(0), 0.5, 0))),
                (150, Some((GainTarget::DuckCap, 4.0, 0))),
                (150, Some((GainTarget::Duck, 0.1, 0))),
                (150, Some((GainTarget::Duck, 0.01, 50))),
                // On the swap sample itself: kept, after.
                (150, Some((GainTarget::Master, 1.0, 0))),
                (160, None),
            ]
        );
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
