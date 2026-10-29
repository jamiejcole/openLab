// SPDX-License-Identifier: GPL-3.0-or-later

//! The real-time mixer (ARCHITECTURE §3.4, §3.5). Everything reachable from [`Mixer::process`]
//! runs on the audio thread: no allocation, locks, syscalls or logging. All buffers are sized in
//! [`Mixer::new`]; voice starts that must be freed go back to the controller on the return queue.

mod delay;
mod faults;
mod voice;

use std::cmp::Ordering as CmpOrdering;
use std::collections::BinaryHeap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rtrb::{Consumer, Producer, PushError, RingBuffer};

use crate::controller::MixerController;
use crate::gain::{Curve, Fade, Param, sanitize_gain};
use crate::meter::{Meters, XrunNotifier, track_all};
use crate::plan::{MAX_SLICES, Plays, Slice, StreamLink};

use delay::DelayLine;
use faults::{DisabledFlags, FaultLog, FaultWindow, Health, Stage, StageTracker};
pub use faults::{DisabledUnits, FAULT_LIMIT, FAULT_WINDOW, FaultStage};
use voice::{Move, ReadOutcome, Remaining, Target, Voice};

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
    /// Moves a voice to patch matrix `patch` (its cue's patch changed while it plays): for the
    /// de-zipper length it plays into both patches, crossfading linearly from the old one to the
    /// new, so neither set of outputs clicks. A voice not heard yet (started on this sample, or
    /// paused past its declick tail) moves at once. A move during a move's crossfade waits for
    /// it to end and starts with the next segment the mixer renders, at most a block later (the
    /// latest one wins). Ignored for an unknown voice, an unknown patch or the patch it is on.
    MoveVoice {
        voice: VoiceId,
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
    /// Its render panicked in [`FAULT_LIMIT`] blocks within [`FAULT_WINDOW`], so
    /// the mixer dropped it.
    Faulted,
}

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
    /// A block panicked in `stage` (or, for [`FaultStage::Mixer`], faults no unit could be
    /// blamed for kept repeating). The stage was reset (a patch to silence, an output or the
    /// master to unity); the control thread re-sends its settings. `disabled`: it faulted [`FAULT_LIMIT`] times within [`FAULT_WINDOW`] and
    /// stays off until a [`FaultStage::Mixer`] rebuild (a patch or device output is silent and
    /// ignores its commands, the master is bypassed at unity, the meters stop).
    StageFaulted {
        stage: FaultStage,
        at: SampleTime,
        disabled: bool,
    },
}

#[derive(Debug, Default)]
pub(crate) struct Shared {
    pub(crate) samples_rendered: AtomicU64,
    pub(crate) faults: AtomicU64,
    pub(crate) faulted_voices: AtomicU64,
    pub(crate) faulted_stages: AtomicU64,
    pub(crate) disabled: DisabledFlags,
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
            leak_start(start);
            self.shared.leaked_starts.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
thread_local! {
    /// The start `leak_start` last gave up on, so a test can free it: LeakSanitizer fails the
    /// whole test binary on a deliberate leak.
    static LEAKED_START: std::cell::Cell<*mut VoiceStart> = const { std::cell::Cell::new(std::ptr::null_mut()) };
}

/// Gives up on `start` without freeing it (the audio thread must not).
fn leak_start(start: Box<VoiceStart>) {
    // Same as `mem::forget`; the raw pointer is only kept for the test above.
    #[cfg_attr(not(test), allow(unused_variables))]
    let raw = Box::into_raw(start);
    #[cfg(test)]
    LEAKED_START.with(|c| c.set(raw));
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
    /// A disabled patch is not mixed and ignores routing commands.
    health: Health,
}

impl Patch {
    /// Silence: every gain 0, the bus cleared.
    fn reset(&mut self) {
        self.gains.iter_mut().for_each(|g| g.set(0.0));
        self.bus.fill(0.0);
        self.live = false;
        self.dirty = false;
    }
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
    /// Per device output; a disabled output is silent and ignores `SetOutput`.
    out_health: Vec<Health>,
    master: Param,
    /// A disabled master is bypassed at unity and ignores `SetMaster`.
    master_health: Health,
    /// Disabled meters are no longer published.
    meter_health: Health,
    /// Faults with no unit to blame ([`Stage::Idle`]).
    unblamed: FaultWindow,
    /// Planar `[device output][frame]`.
    dev: Vec<f32>,
    /// Planar `[cue output][frame]`: a moving voice's output, before it is shared out between
    /// its old and new patch ([`Move`]).
    move_bus: Vec<f32>,
    /// Peak per device output since the last meter flush.
    out_peak: Vec<f32>,
    scratch: Scratch,
    /// The unit a panic is blamed on.
    stage: StageTracker,
    /// `process_guarded` calls so far: the clock of [`FAULT_WINDOW`].
    blocks: u64,
    #[cfg(test)]
    pub(crate) panic_next: bool,
    /// Every render of this voice panics.
    #[cfg(test)]
    pub(crate) panic_voice: Option<VoiceId>,
    /// Applying a command this matches panics.
    #[cfg(test)]
    pub(crate) panic_apply: Option<fn(&CommandKind) -> bool>,
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
                    health: Health::OK,
                })
                .collect(),
            out_gain: vec![Param::new(1.0); outs],
            delays: (0..outs)
                .map(|_| DelayLine::new(max_delay, dezip))
                .collect(),
            out_health: vec![Health::OK; outs],
            master: Param::new(1.0),
            master_health: Health::OK,
            meter_health: Health::OK,
            unblamed: FaultWindow::EMPTY,
            dev: vec![0.0; outs * block],
            move_bus: vec![0.0; outs * block],
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
            stage: StageTracker {
                now: Stage::Idle,
                #[cfg(test)]
                panic_at: None,
            },
            blocks: 0,
            #[cfg(test)]
            panic_next: false,
            #[cfg(test)]
            panic_voice: None,
            #[cfg(test)]
            panic_apply: None,
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
        if !self.meter_health.disabled {
            self.stage.enter(Stage::Meters);
            let meters = &self.outbox.shared.meters;
            meters.flush_outputs(&mut self.out_peak);
            for v in self.voices.iter_mut().filter(|v| v.is_active()) {
                meters.flush_voice(v.slot, v.epoch, &mut v.meter);
            }
            self.stage.enter(Stage::Idle);
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
                    // Not `enter`: a test makes commands panic with `panic_apply`.
                    self.stage.now = Stage::of(&p.cmd.kind);
                    self.apply(p.cmd, t);
                    self.stage.now = Stage::Idle;
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
        #[cfg(test)]
        if self.panic_apply.is_some_and(|f| f(&cmd.kind)) {
            panic!("injected command fault");
        }
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
                    && !p.health.disabled
                    && let Some(g) = p.gains.get_mut(o * outs + d)
                {
                    g.ramp_to(sanitize_gain(gain), dezip, Curve::Linear);
                    p.dirty = true;
                }
            }
            CommandKind::SnapPatch { patch } => {
                if let Some(p) = self.patches.get_mut(usize::from(patch))
                    && !p.health.disabled
                {
                    for g in &mut p.gains {
                        g.finish();
                    }
                    p.dirty = true;
                }
            }
            CommandKind::MoveVoice { voice, patch } => {
                let to = usize::from(patch);
                if to < self.patches.len()
                    && let Some(v) = self.voice_mut(voice)
                {
                    v.move_to(to, dezip);
                }
            }
            CommandKind::SetOutput { output, settings } => {
                let o = usize::from(output);
                if self.out_health.get(o).is_none_or(|h| h.disabled) {
                    return;
                }
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
                if !self.master_health.disabled {
                    self.master
                        .ramp_to(sanitize_gain(gain), dezip, Curve::Linear)
                }
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
            out_health,
            master,
            master_health,
            dev,
            move_bus,
            out_peak,
            scratch,
            stage,
            #[cfg(test)]
            panic_voice,
            ..
        } = self;

        patches.iter_mut().for_each(|p| p.live = false);
        for (i, v) in voices.iter_mut().enumerate().filter(|(_, v)| v.is_active()) {
            stage.enter(Stage::Voice(i));
            #[cfg(test)]
            if *panic_voice == Some(v.id) {
                panic!("injected voice fault");
            }
            // A paused voice past its declick tail contributes nothing and is not visited; a
            // move it was making is over, there is nothing left to crossfade.
            if v.paused.is_some_and(|p| p.done >= p.len) {
                v.advance_move(usize::MAX);
                continue;
            }
            if let Some(m) = v.moving {
                render_moving(v, m, len, t0, block, patches, move_bus, scratch, outbox);
                continue;
            }
            let Some(p) = patches.get_mut(v.patch) else {
                continue;
            };
            if !p.live {
                p.live = true;
                clear(&mut p.bus, block, len);
            }
            if v.paused.is_some() {
                render_tail(v, len, block, &mut p.bus, scratch);
            } else {
                render_voice(v, len, t0, block, &mut p.bus, scratch, outbox);
            }
        }
        stage.enter(Stage::Idle);

        let Some(tmp) = scratch.tmp.get_mut(..len) else {
            return;
        };
        clear(dev, block, len);
        // A disabled patch is not mixed: its voices play on into a bus nobody hears.
        for (k, p) in patches
            .iter_mut()
            .enumerate()
            .filter(|(_, p)| !p.health.disabled)
        {
            stage.enter(Stage::Patch(k));
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
        if master_health.disabled {
            mb.fill(1.0);
        } else {
            stage.enter(Stage::Master);
            master.fill(mb);
        }
        for (o, ((((dv, g), dl), pk), h)) in dev
            .chunks_exact_mut(block)
            .zip(out_gain.iter_mut())
            .zip(delays.iter_mut())
            .zip(out_peak.iter_mut())
            .zip(out_health.iter())
            .enumerate()
        {
            let Some(dv) = dv.get_mut(..len) else {
                continue;
            };
            if h.disabled {
                dv.fill(0.0);
                continue;
            }
            stage.enter(Stage::Output(o));
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
        stage.enter(Stage::Idle);

        for (f, frame) in out.chunks_exact_mut(outs).skip(a).take(len).enumerate() {
            for (d, s) in frame.iter_mut().enumerate() {
                *s = dev.get(d * block + f).copied().unwrap_or(0.0);
            }
        }
    }
}

/// Renders a moving voice ([`Move`]) into `move_bus`, then shares it out: `1 - g` of it to the
/// patch it leaves, `g` to the one it moves to.
#[allow(clippy::too_many_arguments)]
fn render_moving(
    v: &mut Voice,
    m: Move,
    len: usize,
    t0: SampleTime,
    block: usize,
    patches: &mut [Patch],
    move_bus: &mut [f32],
    s: &mut Scratch,
    outbox: &mut Outbox,
) {
    clear(move_bus, block, len);
    if v.paused.is_some() {
        render_tail(v, len, block, move_bus, s);
    } else {
        render_voice(v, len, t0, block, move_bus, s, outbox);
    }
    for (k, leaving) in [(m.from, true), (v.patch, false)] {
        let Some(p) = patches.get_mut(k) else {
            continue;
        };
        if !p.live {
            p.live = true;
            clear(&mut p.bus, block, len);
        }
        for (bo, vo) in p
            .bus
            .chunks_exact_mut(block)
            .zip(move_bus.chunks_exact(block))
        {
            for (j, (y, x)) in bo.iter_mut().zip(vo).take(len).enumerate() {
                let g = m.gain(j);
                *y += x * if leaving { 1.0 - g } else { g };
            }
        }
    }
    v.advance_move(len);
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
        let inputs = input.chunks(block).take(ch);
        mix_output(bo, acc, tmp, inputs, og, meter.get_mut(o), |i, tmp| {
            let Some(p) = xp.get_mut(i * outs + o).filter(|p| !p.is_silent()) else {
                return XGain::Skip;
            };
            match p.steady() {
                Some(g) => XGain::Steady(g),
                None => {
                    p.fill(tmp);
                    XGain::Frames
                }
            }
        });
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
        let inputs = input.chunks(block).take(ch);
        mix_output(bo, acc, &mut [], inputs, vg, meter.get_mut(o), |i, _| {
            let g = xp.get(i * outs + o).map_or(0.0, |x| x.current()) * co;
            if g == 0.0 {
                XGain::Skip
            } else {
                XGain::Steady(g)
            }
        });
        #[cfg(test)]
        {
            s.metered += acc.len() as u64;
        }
    }
    if let Some(p) = v.paused.as_mut() {
        p.done += n as u64;
    }
}

/// Zeroes the first `len` frames of each `block`-frame channel of `bus`.
fn clear(bus: &mut [f32], block: usize, len: usize) {
    for b in bus.chunks_exact_mut(block) {
        b.iter_mut().take(len).for_each(|x| *x = 0.0);
    }
}

/// Copies interleaved `ch`-channel frames, `a` then `b` (one run, split anywhere: an `rtrb`
/// chunk wraps mid-frame when the ring's size is not a whole number of frames), into planar
/// `out`: channel `c` of frame `f` goes to `out[c * stride + at + f]`.
#[inline]
fn deinterleave(a: &[f32], b: &[f32], ch: usize, out: &mut [f32], stride: usize, at: usize) {
    let Some(frames) = (a.len() + b.len()).checked_div(ch) else {
        return;
    };
    // Frame `fa` may be split by the wrap: the last `cut` samples of `a`, then `head`.
    let (fa, cut) = (a.len() / ch, a.len() % ch);
    let (head, rest) = b.split_at(if cut == 0 { 0 } else { (ch - cut).min(b.len()) });
    let split = a.get(fa * ch..).unwrap_or_default().iter().chain(head);
    for (c, x) in split.enumerate() {
        if let Some(d) = out.get_mut(c * stride + at + fa) {
            *d = *x;
        }
    }
    for c in 0..ch {
        let Some(dst) = out.get_mut(c * stride + at..c * stride + at + frames) else {
            continue;
        };
        let (da, db) = dst.split_at_mut(fa.min(dst.len()));
        let db = db.get_mut(usize::from(cut > 0)..).unwrap_or_default();
        let src = a
            .chunks_exact(ch)
            .zip(da)
            .chain(rest.chunks_exact(ch).zip(db));
        for (fr, d) in src {
            if let Some(x) = fr.get(c) {
                *d = *x;
            }
        }
    }
}

/// A crosspoint's gain over a segment: none (the input is not mixed), one value, or one per
/// frame, written to `tmp`.
enum XGain {
    Skip,
    Steady(f32),
    Frames,
}

/// Mixes a voice's input channels into one output's bus segment `bo`: input `i` times
/// `gain(i, tmp)` times the per-frame gain `h`. What it adds is summed in `acc` for the meter.
#[inline]
fn mix_output<'a>(
    bo: &mut [f32],
    acc: &mut [f32],
    tmp: &mut [f32],
    inputs: impl Iterator<Item = &'a [f32]>,
    h: &[f32],
    meter: Option<&mut f32>,
    mut gain: impl FnMut(usize, &mut [f32]) -> XGain,
) {
    acc.fill(0.0);
    for (i, inp) in inputs.enumerate() {
        let g = gain(i, tmp);
        let Some(inp) = inp.get(..bo.len()) else {
            continue;
        };
        match g {
            XGain::Skip => {}
            XGain::Steady(g) => {
                for (((b, a), x), h) in bo.iter_mut().zip(acc.iter_mut()).zip(inp).zip(h) {
                    let y = x * g * h;
                    *b += y;
                    *a += y;
                }
            }
            XGain::Frames => {
                let gh = tmp.iter().zip(h);
                for (((b, a), x), (g, h)) in bo.iter_mut().zip(acc.iter_mut()).zip(inp).zip(gh) {
                    let y = x * g * h;
                    *b += y;
                    *a += y;
                }
            }
        }
    }
    if let Some(m) = meter {
        track_all(m, acc);
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
        let raw = LEAKED_START.with(|c| c.replace(std::ptr::null_mut()));
        assert!(!raw.is_null());
        // SAFETY: `raw` came from `Box::into_raw` in `leak_start` on this thread and nothing else
        // has freed or aliased it.
        drop(unsafe { Box::from_raw(raw) });
    }
}
