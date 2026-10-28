// SPDX-License-Identifier: GPL-3.0-or-later

//! The real-time mixer (ARCHITECTURE §3.4, §3.5). Everything reachable from [`Mixer::process`]
//! runs on the audio thread: no allocation, locks, syscalls or logging. All buffers are sized in
//! [`Mixer::new`]; voice starts that must be freed go back to the controller on the return queue.

use std::cmp::Ordering as CmpOrdering;
use std::collections::BinaryHeap;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rtrb::{Consumer, Producer, PushError, RingBuffer};

use crate::controller::MixerController;
use crate::gain::{Curve, CurveRamp, Fade, Param, ramp_value, sanitize_gain};
use crate::meter::{Meters, XrunNotifier, next_epoch, track_all};
use crate::plan::{MAX_SLICES, Plays, Slice, StreamLink};

/// Absolute time in frames at the device rate since the mixer started (ARCHITECTURE §3.2).
pub type SampleTime = u64;

/// Caller-chosen voice identifier; must be unique among active voices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VoiceId(pub u64);

pub const MAX_OUTPUT_CHANNELS: usize = 64;
pub const MAX_VOICE_CHANNELS: usize = 64;
pub const MAX_PATCHES: usize = 64;
pub const MAX_SAMPLE_RATE: u32 = 384_000;
pub const MAX_BLOCK_FRAMES: usize = 8192;
/// Capacity of the command queue; also the largest atomic batch.
pub const COMMAND_CAPACITY: usize = 1024;
/// Capacity of the mixer's time-ordered pending set.
pub const PENDING_CAPACITY: usize = 4096;
const EVENT_CAPACITY: usize = 1024;
/// De-zipper length for gain changes without an explicit ramp, and delay-change crossfades.
const DEZIP_SECONDS: f64 = 0.005;
/// Longest per-output delay (speaker alignment).
const MAX_DELAY_SECONDS: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MixerConfig {
    pub sample_rate: u32,
    /// Device outputs. Every patch has this many cue outputs.
    pub output_channels: usize,
    /// Internal render chunk; any device block size is accepted and split into chunks this size.
    pub max_block_frames: usize,
    pub max_voices: usize,
    pub max_voice_channels: usize,
    /// Output patches (cue output → device output matrices) a voice can be routed through.
    pub patches: usize,
    /// How far ahead of the clock the controller releases commands to the mixer; later ones are
    /// held on the control thread until [`MixerController::pump`].
    pub lookahead_ms: u32,
}

impl Default for MixerConfig {
    fn default() -> Self {
        MixerConfig {
            sample_rate: 48_000,
            output_channels: 2,
            max_block_frames: 1024,
            max_voices: 128,
            max_voice_channels: 8,
            patches: 8,
            lookahead_ms: 500,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    SampleRate,
    OutputChannels,
    BlockFrames,
    Voices,
    VoiceChannels,
    Patches,
    Lookahead,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ConfigError::SampleRate => "sample rate must be 1..=384000",
            ConfigError::OutputChannels => "output channels must be 1..=64",
            ConfigError::BlockFrames => "max block frames must be 1..=8192",
            ConfigError::Voices => "max voices must be 1..=4096",
            ConfigError::VoiceChannels => "max voice channels must be 1..=64",
            ConfigError::Patches => "patches must be 1..=64",
            ConfigError::Lookahead => "lookahead must be 10..=60000 ms",
        })
    }
}

impl std::error::Error for ConfigError {}

/// A rampable per-voice gain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GainTarget {
    Master,
    CueOutput(u16),
    Crosspoint { input: u16, output: u16 },
}

/// Everything needed to start a voice, including its complete initial routing, so a voice is
/// audible only as intended from its first sample. `source` carries interleaved f32 frames at the
/// device rate; dropping its producer marks end of stream. Boxed on the control thread and
/// returned there for freeing.
#[derive(Debug)]
pub struct VoiceStart {
    pub voice: VoiceId,
    pub source: Consumer<f32>,
    pub channels: u16,
    /// Output patch the voice's cue outputs go through.
    pub patch: u16,
    /// Voice master gain (linear).
    pub gain: f32,
    /// One gain per cue output (`output_channels` of them).
    pub cue_outputs: Vec<f32>,
    /// Row-major `[input][cue output]`, `channels × output_channels`.
    pub crosspoints: Vec<f32>,
    /// Frames discarded from the source before playback (in point).
    pub skip_frames: u64,
    /// Frames played after the skip (out point); `None` plays to end of stream.
    pub play_frames: Option<u64>,
    pub fade_in: Fade,
    /// Integrated fade-out ending on the last played frame. Requires `play_frames` or `slices`.
    pub fade_out: Fade,
    /// Slice table (AUD-12) for the frames after the skip; replaces `play_frames`, which must
    /// then be `None`. Empty = play `play_frames` once.
    pub slices: Vec<Slice>,
    /// Devamp link to the disk stream filling `source`; needed to devamp out of a loop and continue.
    pub link: Option<Arc<StreamLink>>,
}

impl VoiceStart {
    /// Silent routing (all crosspoints 0), unity cue outputs and master, patch 0, no trims or
    /// fades. `cue_outputs` must equal the mixer's `output_channels`.
    pub fn new(voice: VoiceId, source: Consumer<f32>, channels: u16, cue_outputs: usize) -> Self {
        VoiceStart {
            voice,
            source,
            channels,
            patch: 0,
            gain: 1.0,
            cue_outputs: vec![1.0; cue_outputs],
            crosspoints: vec![0.0; usize::from(channels) * cue_outputs],
            skip_frames: 0,
            play_frames: None,
            fade_in: Fade::NONE,
            fade_out: Fade::NONE,
            slices: Vec::new(),
            link: None,
        }
    }

    /// Sets one crosspoint; out-of-range indices are ignored (returns false).
    pub fn set_crosspoint(&mut self, input: usize, output: usize, gain: f32) -> bool {
        let outs = self.cue_outputs.len();
        if input >= usize::from(self.channels) || output >= outs {
            return false;
        }
        match self.crosspoints.get_mut(input * outs + output) {
            Some(g) => {
                *g = gain;
                true
            }
            None => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutputSettings {
    pub level: f32,
    pub mute: bool,
    pub invert: bool,
    pub delay_frames: u32,
}

impl Default for OutputSettings {
    fn default() -> Self {
        OutputSettings {
            level: 1.0,
            mute: false,
            invert: false,
            delay_frames: 0,
        }
    }
}

#[derive(Debug)]
pub enum CommandKind {
    StartVoice(Box<VoiceStart>),
    /// `fade.frames == 0` cuts at the exact sample.
    StopVoice {
        voice: VoiceId,
        fade: Fade,
    },
    /// `fade.frames == 0` sets the gain with the de-zipper (instantly if the voice starts on
    /// this sample).
    Ramp {
        voice: VoiceId,
        target: GainTarget,
        gain: f32,
        fade: Fade,
    },
    SetPatchRouting {
        patch: u16,
        cue_output: u16,
        device_output: u16,
        gain: f32,
    },
    /// The patch matrix takes the gains its routing ramps towards at once: for a matrix just
    /// given to a doc, which nothing plays through yet, so a cue started on the same sample is
    /// heard at its level rather than faded in over the de-zipper.
    SnapPatch {
        patch: u16,
    },
    SetOutput {
        output: u16,
        settings: OutputSettings,
    },
    SetMaster {
        gain: f32,
    },
    /// Leave the current loop at the end of the current play (CT-16): `stop` ends the voice
    /// there, otherwise playback continues with the next slice. Only for voices with a slice
    /// table; continuing out of a loop also needs the voice's `link`. A devamp the voice cannot
    /// carry out is reported as [`Event::DevampIgnored`]. A devamp equal to the one already
    /// pending (same `stop`) is answered again with [`Event::DevampScheduled`]. The integrated
    /// fade-out follows the planned end, so a devamp stop ends at the loop boundary without it.
    /// On a paused voice the [`Event::DevampScheduled`] is sent when it resumes.
    Devamp {
        voice: VoiceId,
        stop: bool,
    },
    /// Pause a playing voice on this sample (CUE-05). It keeps its source, position and state,
    /// consumes nothing and outputs silence (after a de-zipper-length declick fade-out) until
    /// [`CommandKind::ResumeVoice`]. Ignored for an unknown or already paused voice.
    PauseVoice {
        voice: VoiceId,
    },
    /// Resume the paused voice `voice` on this sample from exactly where it paused (with a
    /// declick fade-in), re-keyed as `new_id`: from here on commands address it, and its events
    /// carry it, as `new_id`. Ignored if `voice` is not paused; a `new_id` already in use by
    /// another voice is reported as [`Event::VoiceRejected`] for `new_id` with
    /// [`RejectReason::DuplicateId`] (a rejection of the resume; the voice that has `new_id`
    /// plays on), and the voice stays paused as `voice`. Must be dated at or after the pause:
    /// one dated earlier finds the voice not yet paused and is ignored.
    ResumeVoice {
        voice: VoiceId,
        new_id: VoiceId,
    },
}

/// A command on its way to the mixer: applied at sample `at` (or at the start of the next block
/// if `at` has passed). `seq` is the controller's send order, the tiebreak among equal `at`.
#[derive(Debug)]
pub(crate) struct Command {
    pub(crate) at: SampleTime,
    pub(crate) seq: u64,
    pub(crate) kind: CommandKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinishReason {
    EndOfStream,
    OutPoint,
    Stopped,
    /// Its render panicked in [`VOICE_FAULT_LIMIT`] blocks in a row, so the mixer dropped it.
    Faulted,
}

/// Consecutive faulted blocks, each panicking while the same voice rendered, after which that
/// voice is dropped. A fault silences the whole block, so a voice that panics every block would
/// otherwise silence the show; three blocks (~32 ms at 512 frames, 48 kHz) let a one-off fault
/// pass without losing the cue.
pub const VOICE_FAULT_LIMIT: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    PoolFull,
    DuplicateId,
    BadChannelCount,
    /// Patch index out of range or routing vectors of the wrong length.
    BadRouting,
    /// A fade-out without `play_frames` has nothing to end on.
    BadFade,
    /// A slice table with an empty slice, `Times(0)`, more than `MAX_SLICES` entries, or
    /// combined with `play_frames` or `skip_frames` (the table carries its own start).
    BadSlices,
}

/// Small `Copy` notifications from the audio thread. `at` is the sample time it happened.
/// Events from one block are not sorted by `at`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// `at` is the first sample after the voice's last.
    VoiceFinished {
        voice: VoiceId,
        at: SampleTime,
        reason: FinishReason,
    },
    /// Start of an underrun episode; one event per episode, frames counted in the controller.
    Underrun { voice: VoiceId, at: SampleTime },
    VoiceRejected {
        voice: VoiceId,
        at: SampleTime,
        reason: RejectReason,
    },
    /// The pending set is full, so commands are waiting in the queue and may apply late.
    /// One event per episode.
    PendingFull { at: SampleTime },
    /// A play of a slice ended and the slice plays again from `at`. `count` is the number of
    /// plays of that slice completed so far.
    LoopReached {
        voice: VoiceId,
        at: SampleTime,
        slice: u32,
        count: u64,
    },
    /// The slice's last play ended at `at` (the next slice, if any, starts on `at`).
    SliceEnded {
        voice: VoiceId,
        at: SampleTime,
        slice: u32,
    },
    /// A devamp was accepted. `at` is where the voice leaves the loop if no underrun intervenes;
    /// the matching `SliceEnded` carries the actual sample.
    DevampScheduled { voice: VoiceId, at: SampleTime },
    /// A devamp the voice cannot carry out: no such voice is playing, it has no slice table, a
    /// different devamp (other `stop`) is pending, a cut is needed while an earlier devamp's cut
    /// still waits to be sent, or leaving a loop needs a disk-stream `link` it does not have.
    /// Playback is unchanged. (A devamp while an earlier one's loop exit is still playing is
    /// carried out.)
    /// The same devamp again is not ignored: it is answered with another `DevampScheduled`.
    DevampIgnored { voice: VoiceId, at: SampleTime },
}

#[derive(Debug, Default)]
pub(crate) struct Shared {
    pub(crate) samples_rendered: AtomicU64,
    pub(crate) faults: AtomicU64,
    pub(crate) faulted_voices: AtomicU64,
    pub(crate) underrun_frames: AtomicU64,
    pub(crate) dropped_events: AtomicU64,
    pub(crate) leaked_starts: AtomicU64,
    pub(crate) pending_full_blocks: AtomicU64,
    pub(crate) xruns: AtomicU64,
    pub(crate) meters: Meters,
}

/// Audio-thread side of the queues back to the controller.
pub(crate) struct Outbox {
    pub(crate) events: Producer<Event>,
    pub(crate) returns: Producer<Box<VoiceStart>>,
    pub(crate) shared: Arc<Shared>,
}

impl Outbox {
    pub(crate) fn emit(&mut self, e: Event) {
        if self.events.push(e).is_err() {
            self.shared.dropped_events.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn give_back(&mut self, start: Box<VoiceStart>) {
        if let Err(PushError::Full(start)) = self.returns.push(start) {
            // The return queue is sized so this cannot happen while the controller drains it;
            // leaking is the RT-safe fallback to freeing here.
            std::mem::forget(start);
            self.shared.leaked_starts.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// A command in the pending set, ordered so the heap's top is the earliest `(at, seq)`.
struct Pending {
    at: SampleTime,
    seq: u64,
    cmd: Command,
}

impl Ord for Pending {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        (other.at, other.seq).cmp(&(self.at, self.seq))
    }
}

impl PartialOrd for Pending {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Pending {
    fn eq(&self, other: &Self) -> bool {
        (self.at, self.seq) == (other.at, other.seq)
    }
}

impl Eq for Pending {}

enum ReadOutcome {
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
struct Remaining {
    first: Option<u64>,
    split: usize,
    after: Option<u64>,
    /// The voice's re-started fade-out (see `Voice::refade`) before and after the split.
    refade_first: Option<Refade>,
    refade_after: Option<Refade>,
}

/// A fade-out restarted by a devamp that left less audio than the fade's length: from gain
/// `from` (what the last frame before the jump had) to silence on the last frame, over `len`
/// frames, so the gain never jumps.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Refade {
    from: f32,
    len: u64,
}

/// A paused voice (CUE-05). The fade-out tail plays the frames that follow the pause point
/// without consuming them, so the voice resumes from exactly where it paused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Paused {
    /// Sample the pause took effect on.
    at: SampleTime,
    /// Declick fade-out frames rendered so far, of `len`: the de-zipper length, or fewer if
    /// fewer frames could be peeked (0 for a voice paused before it was heard).
    done: u64,
    len: u64,
    /// Something of the voice had been rendered when it paused.
    heard: bool,
}

/// The rest of a declick tail still fading out after a resume cut it short, crossfaded with the
/// resumed audio so the waveform never jumps.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Xfade {
    tail: Paused,
    /// The voice's `pos` on the resume: the tail plays `tail.done − (pos − pos_at)` frames ahead
    /// of the voice.
    pos_at: u64,
    /// The declick gain the tail was heard at.
    scale: f32,
}

impl Paused {
    /// Declick gain of the tail's frame `j`: falls from 1 and is exactly 0 on frame `len - 1`.
    fn gain(&self, j: u64) -> f32 {
        ramp_value(1.0, 0.0, j + 1, self.len, Curve::Linear)
    }
}

/// Where [`Voice::peek`] copies to: `input` (stride: the block size), `xbuf`, or nowhere (only
/// count the frames).
#[derive(Clone, Copy, Debug)]
enum Target {
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

struct Voice {
    id: VoiceId,
    /// `Some` while the voice is active; holds the source and goes back whole when it ends.
    start: Option<Box<VoiceStart>>,
    channels: usize,
    patch: usize,
    skip: u64,
    /// Frames consumed after the skip.
    pos: u64,
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
    refade: Option<Refade>,
    /// Playing the start of a loop exit's continuation from its exit buffer (see
    /// [`StreamLink`]): entry it was made for, and frames of it played.
    exit: Option<(usize, usize)>,
    fade_in: Fade,
    fade_out: Fade,
    /// Evaluators of the integrated fade-in and fade-out (or refade) curves.
    env_in: CurveRamp,
    env_out: CurveRamp,
    master: Param,
    stop: Param,
    stop_end: Option<SampleTime>,
    cue_out: Vec<Param>,
    /// Row-major `[input][cue output]`.
    xp: Vec<Param>,
    /// Planar `[channel][frame]`, `max_block_frames` per channel.
    input: Vec<f32>,
    /// Not yet rendered: gain sets apply without de-zipper.
    fresh: bool,
    underrunning: bool,
    /// `Some` while paused; then nothing is read and no gain advances.
    paused: Option<Paused>,
    /// Declick gain: ramps up from the tail's level on resume, 1.0 otherwise.
    declick: Param,
    /// See [`Xfade`].
    xfade: Option<Xfade>,
    /// Devamp requests arrived while paused; the resume answers them.
    devamp_due: bool,
    /// Planar `[channel][frame]`, the de-zipper length per channel: the crossfaded tail.
    xbuf: Vec<f32>,
    xstride: usize,
    /// Pool slot index: where this voice's meters live.
    slot: usize,
    /// Meter epoch of the current id (see `meter.rs`).
    epoch: u32,
    /// Peak per cue output since the last meter flush.
    meter: Vec<f32>,
}

impl Voice {
    fn new(slot: usize, outputs: usize, max_channels: usize, block: usize, dezip: usize) -> Self {
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
            devamp_due: false,
            xbuf: vec![0.0; max_channels * dezip],
            xstride: dezip,
            slot,
            epoch: 0,
            meter: vec![0.0; outputs],
        }
    }

    fn is_active(&self) -> bool {
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
    fn remaining(&self) -> Option<u64> {
        self.tail
            .map(|t| t.saturating_add(self.left.saturating_sub(1)))
    }

    /// Takes over a validated start, copying its routing into the voice's gains.
    fn begin(&mut self, s: Box<VoiceStart>, meters: &Meters) {
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
        self.devamp_due = false;
        self.start = Some(s);
        self.enter(0);
    }

    fn param_mut(&mut self, target: GainTarget) -> Option<&mut Param> {
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
    fn request_devamp(&mut self, stop: bool, t: SampleTime, outbox: &mut Outbox) {
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
    fn pause(&mut self, t: SampleTime, dezip: usize) {
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

    /// Resumes on sample `t` as `id` (nothing if not paused). Everything timed in samples (the
    /// stop fade's end) moves by the time spent paused.
    fn resume(&mut self, t: SampleTime, id: VoiceId, dezip: u64, outbox: &mut Outbox) {
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
    fn peek(&mut self, from: u64, n: usize, to: Target) -> usize {
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
                    for (f, frame) in src.chunks_exact(ch).enumerate() {
                        for (c, &x) in frame.iter().enumerate() {
                            if let Some(s) = input.as_mut().and_then(|i| i.get_mut(c * block + f)) {
                                *s = x;
                            }
                        }
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
                for (k, &x) in a.iter().chain(b).skip(first * ch).enumerate() {
                    if let Some(s) = input
                        .as_mut()
                        .and_then(|i| i.get_mut((k % ch) * block + k / ch))
                    {
                        *s = x;
                    }
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
    fn read(
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
                if a.len().is_multiple_of(ch) {
                    // Whole frames on both sides of the wrap: one channel at a time.
                    let fa = a.len() / ch;
                    for c in 0..ch {
                        let Some(dst) = self.input.get_mut(c * block + got..c * block + got + want)
                        else {
                            continue;
                        };
                        let (da, db) = dst.split_at_mut(fa.min(dst.len()));
                        for (d, fr) in da
                            .iter_mut()
                            .zip(a.chunks_exact(ch))
                            .chain(db.iter_mut().zip(b.chunks_exact(ch)))
                        {
                            if let Some(x) = fr.get(c) {
                                *d = *x;
                            }
                        }
                    }
                } else {
                    let (mut f, mut c) = (got, 0);
                    for &x in a.iter().chain(b) {
                        if let Some(s) = self.input.get_mut(c * block + f) {
                            *s = x;
                        }
                        c += 1;
                        if c == ch {
                            c = 0;
                            f += 1;
                        }
                    }
                }
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
        for (f, frame) in src.chunks_exact(ch).enumerate() {
            for (c, &x) in frame.iter().enumerate() {
                if let Some(s) = input.get_mut(c * block + got + f) {
                    *s = x;
                }
            }
        }
        n
    }

    /// Multiplies the integrated fade envelope into `gain` for the `got` frames read from `pos0`.
    fn apply_envelope(&mut self, gain: &mut [f32], pos0: u64, got: usize, rem: &Remaining) {
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

    fn finish(&mut self, at: SampleTime, reason: FinishReason, outbox: &mut Outbox) {
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

struct DelayLine {
    buf: Vec<f32>,
    write: usize,
    delay: usize,
    old_delay: usize,
    xfade: u64,
    xfade_len: u64,
    /// A change requested mid-crossfade; starts when the running crossfade ends.
    next: Option<usize>,
}

impl DelayLine {
    fn new(max_delay: usize, xfade_len: u64) -> Self {
        DelayLine {
            buf: vec![0.0; max_delay + 1],
            write: 0,
            delay: 0,
            old_delay: 0,
            xfade: xfade_len,
            xfade_len,
            next: None,
        }
    }

    fn set_delay(&mut self, frames: usize) {
        let d = frames.min(self.buf.len() - 1);
        if self.xfade < self.xfade_len {
            self.next = Some(d);
            return;
        }
        // A request queued by the crossfade that just ended is superseded by this one.
        self.next = None;
        if d != self.delay {
            self.old_delay = self.delay;
            self.delay = d;
            self.xfade = 0;
        }
    }

    fn tap(&self, d: usize) -> f32 {
        let i = if self.write >= d {
            self.write - d
        } else {
            self.write + self.buf.len() - d
        };
        self.buf.get(i).copied().unwrap_or(0.0)
    }

    /// [`Self::tick`] of every sample of `xs`, in place. Without a delay or a crossfade, each
    /// sample is stored and comes straight back out, so only the store is done (a copy).
    fn process(&mut self, xs: &mut [f32]) {
        let idle = self.xfade >= self.xfade_len && self.next.is_none_or(|d| d == self.delay);
        if !idle || self.delay != 0 {
            xs.iter_mut().for_each(|x| *x = self.tick(*x));
            return;
        }
        self.next = None;
        let mut src: &[f32] = xs;
        while !src.is_empty() {
            let room = self.buf.len().saturating_sub(self.write);
            let k = room.min(src.len());
            let (head, rest) = src.split_at(k);
            if let Some(dst) = self.buf.get_mut(self.write..self.write + k) {
                dst.copy_from_slice(head);
            }
            self.write += k;
            if self.write >= self.buf.len() {
                self.write = 0;
            }
            src = rest;
            if k == 0 {
                break;
            }
        }
    }

    fn tick(&mut self, x: f32) -> f32 {
        if self.xfade >= self.xfade_len
            && let Some(d) = self.next.take()
            && d != self.delay
        {
            self.old_delay = self.delay;
            self.delay = d;
            self.xfade = 0;
        }
        if let Some(s) = self.buf.get_mut(self.write) {
            *s = x;
        }
        let mut y = self.tap(self.delay);
        if self.xfade < self.xfade_len {
            let a = self.xfade as f32 / self.xfade_len as f32;
            y = self.tap(self.old_delay) * (1.0 - a) + y * a;
            self.xfade += 1;
        }
        self.write += 1;
        if self.write == self.buf.len() {
            self.write = 0;
        }
        y
    }
}

/// Per-segment scratch, `max_block_frames` each.
struct Scratch {
    vgain: Vec<f32>,
    og: Vec<f32>,
    tmp: Vec<f32>,
    master: Vec<f32>,
    /// One voice's contribution to one cue output, for its meter.
    acc: Vec<f32>,
    /// Frames summed into `acc` (what metering costs per sample).
    #[cfg(test)]
    metered: u64,
}

/// One output patch: a cue output → device output matrix and its cue-output bus.
struct Patch {
    /// Row-major `[cue output][device output]`.
    gains: Vec<Param>,
    /// Planar `[cue output][frame]`.
    bus: Vec<f32>,
    /// A voice wrote to `bus` in the current segment.
    live: bool,
    /// A routing change may still be ramping; unused patches must advance it.
    dirty: bool,
}

/// The real-time mixer. Create with [`Mixer::new`], move to the audio thread, call
/// [`Mixer::process_guarded`] from the device callback.
pub struct Mixer {
    rate: u32,
    outputs: usize,
    block: usize,
    max_voice_channels: usize,
    dezip: u64,
    now: SampleTime,
    commands: Consumer<Command>,
    /// Earliest `(at, seq)` on top. Never grows past [`PENDING_CAPACITY`].
    pending: BinaryHeap<Pending>,
    pending_full: bool,
    outbox: Outbox,
    voices: Vec<Voice>,
    patches: Vec<Patch>,
    out_gain: Vec<Param>,
    delays: Vec<DelayLine>,
    master: Param,
    /// Planar `[device output][frame]`.
    dev: Vec<f32>,
    /// Peak per device output since the last meter flush.
    out_peak: Vec<f32>,
    scratch: Scratch,
    /// Pool index of the voice being rendered (`None` outside a voice's render): the voice a
    /// panic is blamed on.
    rendering: Option<usize>,
    /// Pool index, id and count of the faults in a row blamed on one voice.
    fault_streak: Option<(usize, VoiceId, u32)>,
    #[cfg(test)]
    pub(crate) panic_next: bool,
    /// Every render of this voice panics.
    #[cfg(test)]
    pub(crate) panic_voice: Option<VoiceId>,
}

impl Mixer {
    /// Allocates everything the mixer will ever use and returns the control-side handle with it.
    pub fn new(config: MixerConfig) -> Result<(MixerController, Mixer), ConfigError> {
        let MixerConfig {
            sample_rate,
            output_channels: outs,
            max_block_frames: block,
            max_voices,
            max_voice_channels,
            patches,
            lookahead_ms,
        } = config;
        if !(1..=MAX_SAMPLE_RATE).contains(&sample_rate) {
            return Err(ConfigError::SampleRate);
        }
        if !(1..=MAX_OUTPUT_CHANNELS).contains(&outs) {
            return Err(ConfigError::OutputChannels);
        }
        if !(1..=MAX_BLOCK_FRAMES).contains(&block) {
            return Err(ConfigError::BlockFrames);
        }
        if !(1..=4096).contains(&max_voices) {
            return Err(ConfigError::Voices);
        }
        if !(1..=MAX_VOICE_CHANNELS).contains(&max_voice_channels) {
            return Err(ConfigError::VoiceChannels);
        }
        if !(1..=MAX_PATCHES).contains(&patches) {
            return Err(ConfigError::Patches);
        }
        if !(10..=60_000).contains(&lookahead_ms) {
            return Err(ConfigError::Lookahead);
        }
        let dezip = ((f64::from(sample_rate) * DEZIP_SECONDS).round() as u64).max(1);
        let max_delay = (sample_rate * MAX_DELAY_SECONDS) as usize;
        let lookahead = u64::from(sample_rate) * u64::from(lookahead_ms) / 1000;

        let (cmd_tx, cmd_rx) = RingBuffer::new(COMMAND_CAPACITY);
        let (ev_tx, ev_rx) = RingBuffer::new(EVENT_CAPACITY);
        // Every start the mixer can hold at once: queued, pending, playing, plus one in flight.
        let (ret_tx, ret_rx) =
            RingBuffer::new(COMMAND_CAPACITY + PENDING_CAPACITY + max_voices + 1);
        let shared = Arc::new(Shared {
            meters: Meters::new(outs, max_voices, outs),
            ..Shared::default()
        });

        let identity = |k: usize| if k / outs == k % outs { 1.0 } else { 0.0 };
        let mixer = Mixer {
            rate: sample_rate,
            outputs: outs,
            block,
            max_voice_channels,
            dezip,
            now: 0,
            commands: cmd_rx,
            pending: BinaryHeap::with_capacity(PENDING_CAPACITY),
            pending_full: false,
            outbox: Outbox {
                events: ev_tx,
                returns: ret_tx,
                shared: Arc::clone(&shared),
            },
            voices: (0..max_voices)
                .map(|k| Voice::new(k, outs, max_voice_channels, block, dezip as usize))
                .collect(),
            patches: (0..patches)
                .map(|_| Patch {
                    gains: (0..outs * outs).map(|k| Param::new(identity(k))).collect(),
                    bus: vec![0.0; outs * block],
                    live: false,
                    dirty: false,
                })
                .collect(),
            out_gain: vec![Param::new(1.0); outs],
            delays: (0..outs)
                .map(|_| DelayLine::new(max_delay, dezip))
                .collect(),
            master: Param::new(1.0),
            dev: vec![0.0; outs * block],
            out_peak: vec![0.0; outs],
            scratch: Scratch {
                vgain: vec![0.0; block],
                og: vec![0.0; block],
                tmp: vec![0.0; block],
                master: vec![0.0; block],
                acc: vec![0.0; block],
                #[cfg(test)]
                metered: 0,
            },
            rendering: None,
            fault_streak: None,
            #[cfg(test)]
            panic_next: false,
            #[cfg(test)]
            panic_voice: None,
        };
        let controller = MixerController::new(cmd_tx, ev_rx, ret_rx, shared, outs, lookahead);
        Ok((controller, mixer))
    }

    pub fn output_channels(&self) -> usize {
        self.outputs
    }

    /// The rate this mixer was built for ([`MixerConfig::sample_rate`]); a device must run at it.
    pub fn sample_rate(&self) -> u32 {
        self.rate
    }

    /// Sample time of the next frame to be rendered.
    pub fn now(&self) -> SampleTime {
        self.now
    }

    /// Records `count` xruns the host reported (NFR-27). Real-time safe: one atomic add. Voice
    /// underruns are not xruns; they are counted separately (`underrun_frames`).
    pub fn note_xruns(&self, count: u64) {
        self.outbox.shared.xruns.fetch_add(count, Ordering::Relaxed);
    }

    /// A handle that records xruns from any thread (e.g. the device's error callback).
    pub fn xrun_notifier(&self) -> XrunNotifier {
        XrunNotifier::new(Arc::clone(&self.outbox.shared))
    }

    /// [`Mixer::process`] inside `catch_unwind`. On a panic the block is silent, the fault count
    /// goes up and the clock still advances. A voice that was rendering when the panic hit in
    /// [`VOICE_FAULT_LIMIT`] blocks in a row is dropped (`VoiceFinished { Faulted }`) so the
    /// others play again. Returns false on a fault.
    pub fn process_guarded(&mut self, out: &mut [f32], frames: usize) -> bool {
        let start = self.now;
        self.rendering = None;
        let ok = catch_unwind(AssertUnwindSafe(|| self.process(out, frames))).is_ok();
        if ok {
            self.fault_streak = None;
        } else {
            self.blame(start);
            // Peaks still held locally from the faulted call are dropped (its output was
            // replaced with silence). A voice that finished earlier in that call has already
            // published its own.
            self.out_peak.fill(0.0);
            self.voices.iter_mut().for_each(|v| v.meter.fill(0.0));
            out.fill(0.0);
            self.now = start + frames.min(out.len() / self.outputs) as u64;
            self.outbox
                .shared
                .samples_rendered
                .store(self.now, Ordering::Release);
            self.outbox.shared.faults.fetch_add(1, Ordering::Relaxed);
        }
        ok
    }

    /// Counts a fault against the voice that was rendering and drops it at the limit. A fault
    /// outside any voice's render (commands, buses, outputs) ends the streak: no voice to drop.
    fn blame(&mut self, at: SampleTime) {
        let Some(i) = self.rendering.take() else {
            self.fault_streak = None;
            return;
        };
        let Some(v) = self.voices.get_mut(i).filter(|v| v.is_active()) else {
            self.fault_streak = None;
            return;
        };
        let n = match self.fault_streak {
            Some((j, id, n)) if j == i && id == v.id => n.saturating_add(1),
            _ => 1,
        };
        if n < VOICE_FAULT_LIMIT {
            self.fault_streak = Some((i, v.id, n));
            return;
        }
        self.fault_streak = None;
        v.finish(at, FinishReason::Faulted, &mut self.outbox);
        self.outbox
            .shared
            .faulted_voices
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Renders `frames` interleaved frames (fewer if `out` is shorter) and advances the clock.
    pub fn process(&mut self, out: &mut [f32], frames: usize) {
        #[cfg(test)]
        if std::mem::take(&mut self.panic_next) {
            panic!("injected mixer fault");
        }
        let ch = self.outputs;
        let frames = frames.min(out.len() / ch);
        let mut done = 0;
        for chunk in out.chunks_mut(self.block * ch) {
            if done >= frames {
                break;
            }
            let n = (chunk.len() / ch).min(frames - done);
            self.process_chunk(chunk, n);
            done += n;
        }
        // One atomic per non-silent meter per call (ended voices flushed when they finished).
        let meters = &self.outbox.shared.meters;
        meters.flush_outputs(&mut self.out_peak);
        for v in self.voices.iter_mut().filter(|v| v.is_active()) {
            meters.flush_voice(v.slot, v.epoch, &mut v.meter);
        }
        self.outbox
            .shared
            .samples_rendered
            .store(self.now, Ordering::Release);
    }

    fn process_chunk(&mut self, out: &mut [f32], n: usize) {
        self.pull_commands();
        let mut pos = 0;
        while pos < n {
            let t = self.now + pos as u64;
            while self.pending.peek().is_some_and(|p| p.at <= t) {
                if let Some(p) = self.pending.pop() {
                    self.apply(p.cmd, t);
                }
            }
            let end = match self.pending.peek() {
                Some(p) if p.at < self.now + n as u64 => (p.at - self.now) as usize,
                _ => n,
            };
            self.render(out, pos, end - pos);
            pos = end;
        }
        self.now += n as u64;
    }

    fn pull_commands(&mut self) {
        while self.pending.len() < PENDING_CAPACITY {
            let Ok(cmd) = self.commands.pop() else { break };
            // The controller's send order breaks ties, however batches and held commands arrive.
            self.pending.push(Pending {
                at: cmd.at,
                seq: cmd.seq,
                cmd,
            });
        }
        if self.pending.len() >= PENDING_CAPACITY && !self.commands.is_empty() {
            self.outbox
                .shared
                .pending_full_blocks
                .fetch_add(1, Ordering::Relaxed);
            if !self.pending_full {
                self.pending_full = true;
                self.outbox.emit(Event::PendingFull { at: self.now });
            }
        } else {
            self.pending_full = false;
        }
    }

    fn apply(&mut self, cmd: Command, t: SampleTime) {
        let outs = self.outputs;
        let dezip = self.dezip;
        match cmd.kind {
            CommandKind::StartVoice(s) => self.start_voice(s, t),
            CommandKind::StopVoice { voice, fade } => {
                let Mixer { voices, outbox, .. } = self;
                if let Some(v) = voices.iter_mut().find(|v| v.is_active() && v.id == voice) {
                    // A paused voice is silent already: any stop cuts it at once.
                    if fade.frames == 0 || v.paused.is_some() {
                        v.finish(t, FinishReason::Stopped, outbox);
                    } else {
                        v.stop.ramp_to(0.0, fade.frames, fade.curve);
                        v.stop_end = Some(t.saturating_add(fade.frames));
                    }
                }
            }
            CommandKind::Ramp {
                voice,
                target,
                gain,
                fade,
            } => {
                if let Some(v) = self.voice_mut(voice) {
                    let (frames, curve) = match (fade.frames, v.fresh) {
                        (0, true) => (0, Curve::Linear),
                        (0, false) => (dezip, Curve::Linear),
                        _ => (fade.frames, fade.curve),
                    };
                    if let Some(p) = v.param_mut(target) {
                        p.ramp_to(sanitize_gain(gain), frames, curve);
                    }
                }
            }
            CommandKind::SetPatchRouting {
                patch,
                cue_output,
                device_output,
                gain,
            } => {
                let (o, d) = (usize::from(cue_output), usize::from(device_output));
                if o < outs
                    && d < outs
                    && let Some(p) = self.patches.get_mut(usize::from(patch))
                    && let Some(g) = p.gains.get_mut(o * outs + d)
                {
                    g.ramp_to(sanitize_gain(gain), dezip, Curve::Linear);
                    p.dirty = true;
                }
            }
            CommandKind::SnapPatch { patch } => {
                if let Some(p) = self.patches.get_mut(usize::from(patch)) {
                    for g in &mut p.gains {
                        g.finish();
                    }
                    p.dirty = true;
                }
            }
            CommandKind::SetOutput { output, settings } => {
                let o = usize::from(output);
                let level = if settings.mute {
                    0.0
                } else {
                    sanitize_gain(settings.level)
                };
                let target = level * if settings.invert { -1.0 } else { 1.0 };
                if let Some(p) = self.out_gain.get_mut(o) {
                    p.ramp_to(target, dezip, Curve::Linear);
                }
                if let Some(d) = self.delays.get_mut(o) {
                    d.set_delay(settings.delay_frames as usize);
                }
            }
            CommandKind::SetMaster { gain } => {
                self.master
                    .ramp_to(sanitize_gain(gain), dezip, Curve::Linear)
            }
            CommandKind::Devamp { voice, stop } => {
                let Mixer { voices, outbox, .. } = self;
                match voices.iter_mut().find(|v| v.is_active() && v.id == voice) {
                    Some(v) => v.request_devamp(stop, t, outbox),
                    // Not started yet or already ended: the engine must not think it worked.
                    None => outbox.emit(Event::DevampIgnored { voice, at: t }),
                }
            }
            CommandKind::PauseVoice { voice } => {
                if let Some(v) = self.voice_mut(voice) {
                    v.pause(t, usize::try_from(dezip).unwrap_or(usize::MAX));
                }
            }
            CommandKind::ResumeVoice { voice, new_id } => self.resume_voice(voice, new_id, t),
        }
    }

    fn resume_voice(&mut self, voice: VoiceId, new_id: VoiceId, t: SampleTime) {
        let dezip = self.dezip;
        let Mixer { voices, outbox, .. } = self;
        let Some(k) = voices
            .iter()
            .position(|v| v.is_active() && v.id == voice && v.paused.is_some())
        else {
            return;
        };
        let taken = voices
            .iter()
            .enumerate()
            .any(|(i, v)| i != k && v.is_active() && v.id == new_id);
        if taken {
            outbox.emit(Event::VoiceRejected {
                voice: new_id,
                at: t,
                reason: RejectReason::DuplicateId,
            });
            return;
        }
        if let Some(v) = voices.get_mut(k) {
            v.resume(t, new_id, dezip, outbox);
        }
    }

    fn voice_mut(&mut self, id: VoiceId) -> Option<&mut Voice> {
        self.voices.iter_mut().find(|v| v.is_active() && v.id == id)
    }

    fn start_voice(&mut self, s: Box<VoiceStart>, t: SampleTime) {
        let ch = usize::from(s.channels);
        let reject = if ch == 0 || ch > self.max_voice_channels {
            Some(RejectReason::BadChannelCount)
        } else if usize::from(s.patch) >= self.patches.len()
            || s.cue_outputs.len() != self.outputs
            || s.crosspoints.len() != ch * self.outputs
        {
            Some(RejectReason::BadRouting)
        } else if !s.slices.is_empty()
            && (s.play_frames.is_some()
                // The devamp cut position is in ring frames from the start of the table.
                || s.skip_frames != 0
                || s.slices.len() > MAX_SLICES
                || s.slices
                    .iter()
                    .any(|x| x.frames == 0 || x.plays == Plays::Times(0)))
        {
            Some(RejectReason::BadSlices)
        } else if s.fade_out.frames > 0 && s.play_frames.is_none() && s.slices.is_empty() {
            Some(RejectReason::BadFade)
        } else if self.voice_mut(s.voice).is_some() {
            Some(RejectReason::DuplicateId)
        } else {
            None
        };
        let free = self.voices.iter_mut().find(|v| !v.is_active());
        match (reject, free) {
            (None, Some(v)) => v.begin(s, &self.outbox.shared.meters),
            (reason, _) => {
                let voice = s.voice;
                self.outbox.give_back(s);
                self.outbox.emit(Event::VoiceRejected {
                    voice,
                    at: t,
                    reason: reason.unwrap_or(RejectReason::PoolFull),
                });
            }
        }
    }

    /// Renders frames `a..a + len` of the chunk: voices → patch buses → patch matrices →
    /// delay → output gain → master.
    fn render(&mut self, out: &mut [f32], a: usize, len: usize) {
        let t0 = self.now + a as u64;
        let (block, outs) = (self.block, self.outputs);
        let Mixer {
            voices,
            outbox,
            patches,
            out_gain,
            delays,
            master,
            dev,
            out_peak,
            scratch,
            rendering,
            #[cfg(test)]
            panic_voice,
            ..
        } = self;

        patches.iter_mut().for_each(|p| p.live = false);
        for (i, v) in voices.iter_mut().enumerate().filter(|(_, v)| v.is_active()) {
            *rendering = Some(i);
            #[cfg(test)]
            if *panic_voice == Some(v.id) {
                panic!("injected voice fault");
            }
            // A paused voice past its declick tail contributes nothing and is not visited.
            if v.paused.is_some_and(|p| p.done >= p.len) {
                continue;
            }
            let Some(p) = patches.get_mut(v.patch) else {
                continue;
            };
            if !p.live {
                p.live = true;
                for b in p.bus.chunks_exact_mut(block) {
                    b.iter_mut().take(len).for_each(|s| *s = 0.0);
                }
            }
            if v.paused.is_some() {
                render_tail(v, len, block, &mut p.bus, scratch);
            } else {
                render_voice(v, len, t0, block, &mut p.bus, scratch, outbox);
            }
        }
        *rendering = None;

        let Some(tmp) = scratch.tmp.get_mut(..len) else {
            return;
        };
        for dv in dev.chunks_exact_mut(block) {
            dv.iter_mut().take(len).for_each(|s| *s = 0.0);
        }
        for p in patches.iter_mut() {
            if !p.live {
                if p.dirty {
                    p.dirty = false;
                    for g in p.gains.iter_mut().filter(|g| g.is_ramping()) {
                        g.advance(len);
                        p.dirty |= g.is_ramping();
                    }
                }
                continue;
            }
            p.dirty = false;
            for (d, dv) in dev.chunks_exact_mut(block).enumerate() {
                for (o, bo) in p.bus.chunks_exact(block).enumerate() {
                    let Some(g) = p.gains.get_mut(o * outs + d) else {
                        continue;
                    };
                    if g.is_silent() {
                        continue;
                    }
                    if let Some(g) = g.steady() {
                        dv.iter_mut()
                            .zip(bo)
                            .take(len)
                            .for_each(|(y, x)| *y += x * g);
                        continue;
                    }
                    g.fill(tmp);
                    p.dirty |= g.is_ramping();
                    for ((y, x), g) in dv.iter_mut().zip(bo).zip(tmp.iter()) {
                        *y += x * g;
                    }
                }
            }
        }

        // Delay first, so output level, mute and polarity act immediately for the operator.
        let Some(mb) = scratch.master.get_mut(..len) else {
            return;
        };
        master.fill(mb);
        for (((dv, g), dl), pk) in dev
            .chunks_exact_mut(block)
            .zip(out_gain.iter_mut())
            .zip(delays.iter_mut())
            .zip(out_peak.iter_mut())
        {
            let Some(dv) = dv.get_mut(..len) else {
                continue;
            };
            dl.process(dv);
            match g.steady() {
                Some(gg) => dv
                    .iter_mut()
                    .zip(mb.iter())
                    .for_each(|(y, m)| *y = *y * gg * m),
                None => {
                    g.fill(tmp);
                    for ((y, gg), m) in dv.iter_mut().zip(tmp.iter()).zip(mb.iter()) {
                        *y = *y * gg * m;
                    }
                }
            }
            track_all(pk, dv);
        }

        for (f, frame) in out.chunks_exact_mut(outs).skip(a).take(len).enumerate() {
            for (d, s) in frame.iter_mut().enumerate() {
                *s = dev.get(d * block + f).copied().unwrap_or(0.0);
            }
        }
    }
}

fn render_voice(
    v: &mut Voice,
    len: usize,
    t0: SampleTime,
    block: usize,
    bus: &mut [f32],
    s: &mut Scratch,
    outbox: &mut Outbox,
) {
    // Nothing after a stop fade ends is read, played or counted as underrun.
    let live = v.stop_end.map_or(len, |e| {
        usize::try_from(e.saturating_sub(t0))
            .unwrap_or(len)
            .min(len)
    });
    let pos0 = v.pos;
    // The crossfaded tail is peeked before the read moves the voice on.
    let xfade = v.xfade.map(|x| {
        let n = usize::try_from(x.tail.len.saturating_sub(x.tail.done))
            .unwrap_or(usize::MAX)
            .min(len);
        let from = x.tail.done.saturating_sub(v.pos.saturating_sub(x.pos_at));
        (x, n, v.peek(from, n, Target::Xfade))
    });
    let (got, outcome, rem) = v.read(live, len, block, t0, outbox);
    let (Some(vg), Some(og), Some(tmp), Some(acc)) = (
        s.vgain.get_mut(..len),
        s.og.get_mut(..len),
        s.tmp.get_mut(..len),
        s.acc.get_mut(..len),
    ) else {
        return;
    };

    // Voice gain: master × stop fade × integrated fade envelope. Multiplying by an exact 1.0 is
    // an identity, so skipping it keeps output independent of block segmentation.
    v.master.fill(vg);
    if v.stop.is_ramping() || v.stop.current() != 1.0 {
        v.stop.fill(tmp);
        vg.iter_mut().zip(tmp.iter()).for_each(|(g, t)| *g *= t);
    }
    // The declick acts on the input, so the tail it crossfades with shares the voice's gains.
    if v.declick.is_ramping() || v.declick.current() != 1.0 {
        v.declick.fill(tmp);
        let Voice {
            input,
            xbuf,
            xstride,
            channels,
            ..
        } = v;
        for c in 0..*channels {
            let Some(inp) = input.get_mut(c * block..c * block + len) else {
                continue;
            };
            inp.iter_mut().zip(tmp.iter()).for_each(|(x, d)| *x *= d);
            if let Some((x, _, xgot)) = xfade
                && let Some(tail) = xbuf.get(c * *xstride..c * *xstride + xgot)
            {
                for (j, (y, t)) in inp.iter_mut().zip(tail).enumerate() {
                    *y += t * x.scale * x.tail.gain(x.tail.done + j as u64);
                }
            }
        }
    }
    if let Some((mut x, n, _)) = xfade {
        x.tail.done += n as u64;
        v.xfade = (x.tail.done < x.tail.len).then_some(x);
    }
    v.apply_envelope(vg, pos0, got, &rem);

    let Voice {
        cue_out,
        xp,
        input,
        channels,
        meter,
        ..
    } = v;
    let (outs, ch) = (cue_out.len(), *channels);
    for (o, co) in cue_out.iter_mut().enumerate() {
        let audible = (0..ch).any(|i| xp.get(i * outs + o).is_some_and(|p| !p.is_silent()));
        if !audible || co.is_silent() {
            co.advance(len);
            xp.iter_mut()
                .skip(o)
                .step_by(outs)
                .take(ch)
                .for_each(|p| p.advance(len));
            continue;
        }
        // A steady gain is the value `fill` would write to every frame: the same products.
        match co.steady() {
            Some(c) => og.iter_mut().zip(vg.iter()).for_each(|(g, v)| *g = c * v),
            None => {
                co.fill(og);
                og.iter_mut().zip(vg.iter()).for_each(|(g, v)| *g *= v);
            }
        }
        let Some(bo) = bus.get_mut(o * block..o * block + len) else {
            continue;
        };
        acc.fill(0.0);
        for i in 0..ch {
            let Some(p) = xp.get_mut(i * outs + o) else {
                continue;
            };
            if p.is_silent() {
                continue;
            }
            let steady = p.steady();
            if steady.is_none() {
                p.fill(tmp);
            }
            let Some(inp) = input.get(i * block..i * block + len) else {
                continue;
            };
            if let Some(g) = steady {
                for (((b, a), x), h) in bo.iter_mut().zip(acc.iter_mut()).zip(inp).zip(og.iter()) {
                    let y = x * g * h;
                    *b += y;
                    *a += y;
                }
                continue;
            }
            for ((((b, a), x), g), h) in bo
                .iter_mut()
                .zip(acc.iter_mut())
                .zip(inp)
                .zip(tmp.iter())
                .zip(og.iter())
            {
                let y = x * g * h;
                *b += y;
                *a += y;
            }
        }
        if let Some(m) = meter.get_mut(o) {
            track_all(m, acc);
        }
        #[cfg(test)]
        {
            s.metered += acc.len() as u64;
        }
    }
    v.fresh = false;

    let mut end = match outcome {
        ReadOutcome::Ended(r) => Some((t0 + got as u64, r)),
        ReadOutcome::Underrun => {
            outbox
                .shared
                .underrun_frames
                .fetch_add((live - got) as u64, Ordering::Relaxed);
            if !v.underrunning {
                v.underrunning = true;
                outbox.emit(Event::Underrun {
                    voice: v.id,
                    at: t0 + got as u64,
                });
            }
            None
        }
        ReadOutcome::Full => {
            v.underrunning = false;
            None
        }
    };
    if let Some(e) = v.stop_end
        && e < t0 + len as u64
        && end.is_none_or(|(at, _)| e < at)
    {
        end = Some((e, FinishReason::Stopped));
    }
    if let Some((at, reason)) = end {
        v.finish(at, reason, outbox);
    }
}

/// Renders the declick fade-out of a paused voice: the frames after its pause point, peeked
/// without consuming them, at the gains it had when it paused (none of them advance), times the
/// falling declick ramp and the integrated fade envelope those frames would have had.
fn render_tail(v: &mut Voice, len: usize, block: usize, bus: &mut [f32], s: &mut Scratch) {
    let Some(p) = v.paused else { return };
    let n = usize::try_from(p.len.saturating_sub(p.done))
        .unwrap_or(usize::MAX)
        .min(len);
    let got = v.peek(p.done, n, Target::Input(block));
    let (Some(vg), Some(acc)) = (s.vgain.get_mut(..n), s.acc.get_mut(..n)) else {
        return;
    };
    let g = v.master.current() * v.stop.current() * v.declick.current();
    for (j, x) in vg.iter_mut().enumerate() {
        *x = g * p.gain(p.done + j as u64);
    }
    let rem = Remaining {
        first: v.remaining().map(|r| r.saturating_sub(p.done)),
        split: usize::MAX,
        after: None,
        refade_first: v.refade,
        refade_after: None,
    };
    v.apply_envelope(vg, v.pos.saturating_add(p.done), got, &rem);

    let Voice {
        cue_out,
        xp,
        input,
        channels,
        meter,
        ..
    } = v;
    let (outs, ch) = (cue_out.len(), *channels);
    for (o, co) in cue_out.iter().enumerate() {
        let co = co.current();
        // As in `render_voice`: an output nothing reaches costs nothing (Pause All on many
        // voices × outputs renders every tail in one block).
        if co == 0.0 || (0..ch).all(|i| xp.get(i * outs + o).is_none_or(|x| x.current() == 0.0)) {
            continue;
        }
        let Some(bo) = bus.get_mut(o * block..o * block + got) else {
            continue;
        };
        acc.fill(0.0);
        for i in 0..ch {
            let g = xp.get(i * outs + o).map_or(0.0, |x| x.current()) * co;
            let Some(inp) = input.get(i * block..i * block + got) else {
                continue;
            };
            if g == 0.0 {
                continue;
            }
            for (((b, a), x), h) in bo.iter_mut().zip(acc.iter_mut()).zip(inp).zip(vg.iter()) {
                let y = x * g * h;
                *b += y;
                *a += y;
            }
        }
        if let Some(m) = meter.get_mut(o) {
            track_all(m, acc);
        }
        #[cfg(test)]
        {
            s.metered += acc.len() as u64;
        }
    }
    if let Some(p) = v.paused.as_mut() {
        p.done += n as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OfflineRenderer;

    #[test]
    fn rejects_bad_config() {
        let bad = |f: fn(&mut MixerConfig)| {
            let mut c = MixerConfig::default();
            f(&mut c);
            Mixer::new(c).err()
        };
        assert_eq!(bad(|c| c.sample_rate = 0), Some(ConfigError::SampleRate));
        assert_eq!(
            bad(|c| c.sample_rate = 384_001),
            Some(ConfigError::SampleRate)
        );
        assert_eq!(
            bad(|c| c.output_channels = 0),
            Some(ConfigError::OutputChannels)
        );
        assert_eq!(
            bad(|c| c.output_channels = 65),
            Some(ConfigError::OutputChannels)
        );
        assert_eq!(
            bad(|c| c.max_block_frames = 0),
            Some(ConfigError::BlockFrames)
        );
        assert_eq!(
            bad(|c| c.max_block_frames = usize::MAX),
            Some(ConfigError::BlockFrames)
        );
        assert_eq!(bad(|c| c.max_voices = 0), Some(ConfigError::Voices));
        assert_eq!(bad(|c| c.max_voices = 4097), Some(ConfigError::Voices));
        assert_eq!(
            bad(|c| c.max_voice_channels = 65),
            Some(ConfigError::VoiceChannels)
        );
        assert_eq!(bad(|c| c.patches = 0), Some(ConfigError::Patches));
        assert_eq!(bad(|c| c.patches = 65), Some(ConfigError::Patches));
        assert_eq!(bad(|c| c.lookahead_ms = 9), Some(ConfigError::Lookahead));
        assert!(
            Mixer::new(MixerConfig {
                output_channels: 64,
                ..Default::default()
            })
            .is_ok()
        );
    }

    #[test]
    fn panic_in_process_outputs_silence_and_counts_fault() {
        let (mut ctl, mixer) = Mixer::new(MixerConfig::default()).unwrap();
        let (mut tx, rx) = RingBuffer::new(4096);
        tx.push_entire_slice(&[0.5; 4096]).unwrap();
        let mut v = VoiceStart::new(VoiceId(1), rx, 2, 2);
        v.set_crosspoint(0, 0, 1.0);
        v.set_crosspoint(1, 1, 1.0);
        ctl.send(0, CommandKind::StartVoice(Box::new(v)));
        let mut r = OfflineRenderer::new(mixer);
        let before = r.render(64, &[64]);
        assert!(before.iter().all(|s| *s == 0.5));

        r.mixer_mut().panic_next = true;
        let mut buf = vec![1.0; 128];
        assert!(!r.mixer_mut().process_guarded(&mut buf, 64));
        assert!(buf.iter().all(|s| *s == 0.0));
        assert_eq!(ctl.faults(), 1);
        assert_eq!(ctl.samples_rendered(), 128);

        // The next block renders normally.
        assert!(r.mixer_mut().process_guarded(&mut buf, 64));
        assert!(buf.iter().all(|s| *s == 0.5));
        assert_eq!(ctl.faults(), 1);
    }

    #[test]
    fn a_voice_that_faults_every_block_is_dropped_and_the_others_play_on() {
        let (mut ctl, mixer) = Mixer::new(MixerConfig::default()).unwrap();
        for (id, out) in [(1, 0), (2, 1)] {
            let (mut tx, rx) = RingBuffer::new(8192);
            tx.push_entire_slice(&[0.5; 8192]).unwrap();
            let mut v = VoiceStart::new(VoiceId(id), rx, 1, 2);
            v.set_crosspoint(0, out, 1.0);
            ctl.send(0, CommandKind::StartVoice(Box::new(v)));
        }
        let mut r = OfflineRenderer::new(mixer);
        r.render(64, &[]);
        r.mixer_mut().panic_voice = Some(VoiceId(1));
        let mut buf = vec![1.0; 128];
        for _ in 0..VOICE_FAULT_LIMIT {
            assert!(!r.mixer_mut().process_guarded(&mut buf, 64));
        }
        assert_eq!(ctl.faults(), u64::from(VOICE_FAULT_LIMIT));
        assert_eq!(ctl.faulted_voices(), 1);
        let mut ev = Vec::new();
        while let Some(e) = ctl.poll_event() {
            ev.push(e);
        }
        assert!(ev.contains(&Event::VoiceFinished {
            voice: VoiceId(1),
            at: 64 * u64::from(VOICE_FAULT_LIMIT),
            reason: FinishReason::Faulted,
        }));
        // Voice 2 plays on; voice 1's output stays silent.
        for _ in 0..4 {
            assert!(r.mixer_mut().process_guarded(&mut buf, 64));
            assert!(buf.chunks(2).all(|f| f == [0.0, 0.5]));
        }
        assert_eq!(ctl.faults(), u64::from(VOICE_FAULT_LIMIT));
    }

    #[test]
    fn a_pause_tail_meters_only_the_outputs_it_reaches() {
        let cfg = MixerConfig {
            output_channels: 8,
            ..MixerConfig::default()
        };
        let (mut ctl, mixer) = Mixer::new(cfg).unwrap();
        let (mut tx, rx) = RingBuffer::new(8192);
        tx.push_entire_slice(&[0.5; 8192]).unwrap();
        let mut v = VoiceStart::new(VoiceId(1), rx, 1, 8);
        v.set_crosspoint(0, 2, 1.0);
        ctl.send(0, CommandKind::StartVoice(Box::new(v)));
        ctl.send(1000, CommandKind::PauseVoice { voice: VoiceId(1) });
        let mut r = OfflineRenderer::new(mixer);
        r.render(1000, &[]);
        ctl.voice_meters();
        let before = r.mixer_mut().scratch.metered;
        let out = r.render(1000, &[]);
        // The 240-frame declick tail on the one routed output, nothing for the other seven.
        assert_eq!(r.mixer_mut().scratch.metered - before, 240);
        let peaks = ctl.voice_meters();
        assert_eq!(peaks.len(), 1);
        assert!(
            peaks[0]
                .1
                .iter()
                .enumerate()
                .all(|(o, p)| (o == 2) == (*p > 0.0))
        );
        assert!(
            out.chunks(8)
                .all(|f| f.iter().enumerate().all(|(o, x)| o == 2 || *x == 0.0))
        );
    }

    #[test]
    fn a_faulted_block_reports_no_meter_peaks() {
        let (mut ctl, mixer) = Mixer::new(MixerConfig::default()).unwrap();
        let (mut tx, rx) = RingBuffer::new(4096);
        tx.push_entire_slice(&[0.5; 4096]).unwrap();
        let mut v = VoiceStart::new(VoiceId(1), rx, 1, 2);
        v.set_crosspoint(0, 0, 1.0);
        ctl.send(0, CommandKind::StartVoice(Box::new(v)));
        let mut r = OfflineRenderer::new(mixer);
        r.render(64, &[]);
        assert_eq!(ctl.output_meters(), vec![0.5, 0.0]);
        assert_eq!(ctl.voice_meters(), vec![(VoiceId(1), vec![0.5, 0.0])]);

        // Peaks tracked before a panic mid-block were never heard.
        let m = r.mixer_mut();
        m.out_peak[1] = 0.75;
        m.voices[0].meter[1] = 0.75;
        m.panic_next = true;
        let mut buf = vec![1.0; 128];
        assert!(!m.process_guarded(&mut buf, 64));
        assert!(m.process_guarded(&mut buf, 64));
        assert_eq!(ctl.output_meters(), vec![0.5, 0.0]);
        assert_eq!(ctl.voice_meters(), vec![(VoiceId(1), vec![0.5, 0.0])]);
    }

    #[test]
    fn full_return_queue_leaks_instead_of_freeing_on_the_audio_thread() {
        let (tx, mut rx) = RingBuffer::new(1);
        let (ev_tx, _ev_rx) = RingBuffer::new(1);
        let shared = Arc::new(Shared::default());
        let mut outbox = Outbox {
            events: ev_tx,
            returns: tx,
            shared: Arc::clone(&shared),
        };
        let start = || {
            let (_p, c) = RingBuffer::new(4);
            Box::new(VoiceStart::new(VoiceId(1), c, 1, 2))
        };
        let (a, b) = (start(), start());
        crate::no_alloc_for_tests(|| {
            outbox.give_back(a);
            outbox.give_back(b);
            outbox.emit(Event::PendingFull { at: 0 });
            outbox.emit(Event::PendingFull { at: 1 });
        });
        assert_eq!(shared.leaked_starts.load(Ordering::Relaxed), 1);
        assert_eq!(shared.dropped_events.load(Ordering::Relaxed), 1);
        assert!(rx.pop().is_ok());
    }

    #[test]
    fn delay_retarget_mid_crossfade_waits_for_the_running_one() {
        let mut d = DelayLine::new(100, 10);
        d.set_delay(20);
        for _ in 0..3 {
            d.tick(1.0);
        }
        d.set_delay(40);
        assert_eq!((d.delay, d.next), (20, Some(40)));
        for _ in 0..7 {
            d.tick(1.0);
        }
        d.tick(1.0);
        assert_eq!((d.old_delay, d.delay, d.next), (20, 40, None));
    }

    /// Review 2 M2: a change on the exact sample a crossfade ends supersedes the queued one.
    #[test]
    fn delay_change_on_the_sample_a_crossfade_ends_wins() {
        let mut d = DelayLine::new(100, 10);
        d.set_delay(20);
        for _ in 0..3 {
            d.tick(1.0);
        }
        d.set_delay(40);
        for _ in 0..7 {
            d.tick(1.0);
        }
        // Crossfade done, queued 40 not yet consumed.
        assert_eq!((d.xfade, d.next), (10, Some(40)));
        d.set_delay(60);
        for _ in 0..30 {
            d.tick(1.0);
        }
        assert_eq!((d.delay, d.next), (60, None));
    }

    /// `DelayLine::process` on a block gives the bits of `tick` on each sample, through delay
    /// changes, crossfades, queued changes and blocks longer than the line.
    #[test]
    fn delay_process_matches_tick_sample_for_sample() {
        let (mut a, mut b) = (DelayLine::new(50, 10), DelayLine::new(50, 10));
        let mut x = 0.0f32;
        let steps: [(Option<usize>, usize); 17] = [
            (None, 7),
            (None, 120),
            (Some(5), 3),
            (Some(0), 4),
            (None, 30),
            (Some(0), 9),
            (Some(30), 64),
            (Some(0), 2),
            (Some(12), 5),
            (None, 200),
            (Some(0), 51),
            // A same-value change left queued when the crossfade ends: the fast path must
            // drop it like `tick` does.
            (Some(5), 30),
            (Some(0), 3),
            (Some(3), 0),
            (Some(0), 7),
            (None, 20),
            (None, 77),
        ];
        let mut buf = [0.0f32; 200];
        for (delay, n) in steps {
            if let Some(d) = delay {
                a.set_delay(d);
                b.set_delay(d);
            }
            let xs = &mut buf[..n];
            for s in xs.iter_mut() {
                x += 0.37;
                *s = x.sin();
            }
            let want: Vec<f32> = xs.iter().map(|s| b.tick(*s)).collect();
            a.process(xs);
            assert_eq!(
                xs.iter().map(|s| s.to_bits()).collect::<Vec<_>>(),
                want.iter().map(|s| s.to_bits()).collect::<Vec<_>>()
            );
            assert_eq!(
                (a.write, a.delay, a.old_delay, a.xfade, a.next),
                (b.write, b.delay, b.old_delay, b.xfade, b.next)
            );
        }
    }

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
