// SPDX-License-Identifier: GPL-3.0-or-later

//! The real-time mixer (ARCHITECTURE §3.4, §3.5). Everything reachable from [`Mixer::process`]
//! runs on the audio thread: no allocation, locks, syscalls or logging. All buffers are sized in
//! [`Mixer::new`]; voice starts that must be freed go back to the controller on the return queue.

mod commands;
mod delay;
mod faults;
mod render;
mod voice;

use std::collections::BinaryHeap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rtrb::{Consumer, RingBuffer};

use crate::controller::MixerController;
use crate::gain::{Fade, Param};
use crate::meter::{Meters, XrunNotifier};
use crate::plan::{Slice, StreamLink};

use commands::{Outbox, Pending};
use delay::DelayLine;
use faults::{DisabledFlags, FaultLog, FaultWindow, Health, Stage, StageTracker};
pub use faults::{DisabledUnits, FAULT_LIMIT, FAULT_WINDOW, FaultStage};
use voice::Voice;

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
    /// The voice plays through its current patch only, from `at` on: the crossfade of its last
    /// move ended, or the move took no crossfade. `moves` counts the [`CommandKind::MoveVoice`]s
    /// it has taken since its start (wrapping; one for an unknown patch too), so the control
    /// thread can tell whether a move it sent since is still on its way. Not sent while another
    /// move waits for the crossfade (the one that starts then is reported when it ends).
    MoveEnded {
        voice: VoiceId,
        at: SampleTime,
        moves: u32,
    },
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
