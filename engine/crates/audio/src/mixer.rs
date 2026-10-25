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
use crate::gain::{Curve, Fade, Param, ramp_value, sanitize_gain};

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
    /// Integrated fade-out ending on the last played frame. Requires `play_frames`.
    pub fade_out: Fade,
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
    SetOutput {
        output: u16,
        settings: OutputSettings,
    },
    SetMaster {
        gain: f32,
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
}

#[derive(Debug, Default)]
pub(crate) struct Shared {
    pub(crate) samples_rendered: AtomicU64,
    pub(crate) faults: AtomicU64,
    pub(crate) underrun_frames: AtomicU64,
    pub(crate) dropped_events: AtomicU64,
    pub(crate) leaked_starts: AtomicU64,
    pub(crate) pending_full_blocks: AtomicU64,
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

struct Voice {
    id: VoiceId,
    /// `Some` while the voice is active; holds the source and goes back whole when it ends.
    start: Option<Box<VoiceStart>>,
    channels: usize,
    patch: usize,
    skip: u64,
    remaining: Option<u64>,
    play_frames: Option<u64>,
    /// Frames consumed after the skip.
    pos: u64,
    fade_in: Fade,
    fade_out: Fade,
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
}

impl Voice {
    fn new(outputs: usize, max_channels: usize, block: usize) -> Self {
        Voice {
            id: VoiceId(0),
            start: None,
            channels: 0,
            patch: 0,
            skip: 0,
            remaining: None,
            play_frames: None,
            pos: 0,
            fade_in: Fade::NONE,
            fade_out: Fade::NONE,
            master: Param::new(1.0),
            stop: Param::new(1.0),
            stop_end: None,
            cue_out: vec![Param::new(1.0); outputs],
            xp: vec![Param::new(0.0); max_channels * outputs],
            input: vec![0.0; max_channels * block],
            fresh: false,
            underrunning: false,
        }
    }

    fn is_active(&self) -> bool {
        self.start.is_some()
    }

    /// Takes over a validated start, copying its routing into the voice's gains.
    fn begin(&mut self, s: Box<VoiceStart>) {
        self.id = s.voice;
        self.channels = usize::from(s.channels);
        self.patch = usize::from(s.patch);
        self.skip = s.skip_frames;
        self.remaining = s.play_frames;
        self.play_frames = s.play_frames;
        self.pos = 0;
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
        self.start = Some(s);
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

    /// Reads up to `live` frames into `input` and zero-fills it up to `len`.
    fn read(&mut self, live: usize, len: usize, block: usize) -> (usize, ReadOutcome) {
        let ch = self.channels;
        let Some(src) = self.start.as_mut().map(|s| &mut s.source) else {
            return (0, ReadOutcome::Ended(FinishReason::Stopped));
        };
        let mut avail = src.slots() / ch;
        if self.skip > 0 {
            let s = avail.min(usize::try_from(self.skip).unwrap_or(usize::MAX));
            if let Ok(chunk) = src.read_chunk(s * ch) {
                chunk.commit_all();
                self.skip -= s as u64;
                avail -= s;
            }
        }
        let mut want = live.min(avail);
        if let Some(r) = self.remaining {
            want = want.min(usize::try_from(r).unwrap_or(usize::MAX));
        }
        let mut got = 0;
        if want > 0
            && let Ok(chunk) = src.read_chunk(want * ch)
        {
            let (a, b) = chunk.as_slices();
            let (mut f, mut c) = (0, 0);
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
            chunk.commit_all();
            got = want;
        }
        for c in 0..ch {
            if let Some(rest) = self.input.get_mut(c * block + got..c * block + len) {
                rest.fill(0.0);
            }
        }
        self.pos += got as u64;
        if let Some(r) = &mut self.remaining {
            *r -= got as u64;
            if *r == 0 {
                return (got, ReadOutcome::Ended(FinishReason::OutPoint));
            }
        }
        if got == live {
            return (got, ReadOutcome::Full);
        }
        // Check abandonment before re-reading slots, so frames pushed just before the producer
        // was dropped are never mistaken for end of stream.
        if src.is_abandoned() && src.slots() / ch == 0 {
            (got, ReadOutcome::Ended(FinishReason::EndOfStream))
        } else {
            (got, ReadOutcome::Underrun)
        }
    }

    /// Multiplies the integrated fade envelope into `gain` for the `got` frames read from `pos0`.
    fn apply_envelope(&self, gain: &mut [f32], pos0: u64, got: usize) {
        let (fin, fout) = (self.fade_in, self.fade_out);
        let end = pos0 + got as u64;
        let in_active = fin.frames > 0 && pos0 < fin.frames;
        // Fade-out step k for frame p is p + frames + 1 − L, so frame L − 1 lands on k = frames.
        let out_len = self
            .play_frames
            .filter(|&l| fout.frames > 0 && end.saturating_add(fout.frames) >= l);
        if !in_active && out_len.is_none() {
            return;
        }
        for (p, g) in (pos0..end).zip(gain.iter_mut()) {
            if in_active && p < fin.frames {
                *g *= ramp_value(0.0, 1.0, p, fin.frames, fin.curve);
            }
            if let Some(l) = out_len {
                let q = p.saturating_add(fout.frames).saturating_add(1);
                if q >= l {
                    *g *= ramp_value(1.0, 0.0, q - l, fout.frames, fout.curve);
                }
            }
        }
    }

    fn finish(&mut self, at: SampleTime, reason: FinishReason, outbox: &mut Outbox) {
        if let Some(start) = self.start.take() {
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
    scratch: Scratch,
    #[cfg(test)]
    pub(crate) panic_next: bool,
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
        let shared = Arc::new(Shared::default());

        let identity = |k: usize| if k / outs == k % outs { 1.0 } else { 0.0 };
        let mixer = Mixer {
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
                .map(|_| Voice::new(outs, max_voice_channels, block))
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
            scratch: Scratch {
                vgain: vec![0.0; block],
                og: vec![0.0; block],
                tmp: vec![0.0; block],
                master: vec![0.0; block],
            },
            #[cfg(test)]
            panic_next: false,
        };
        let controller = MixerController::new(cmd_tx, ev_rx, ret_rx, shared, outs, lookahead);
        Ok((controller, mixer))
    }

    pub fn output_channels(&self) -> usize {
        self.outputs
    }

    /// Sample time of the next frame to be rendered.
    pub fn now(&self) -> SampleTime {
        self.now
    }

    /// [`Mixer::process`] inside `catch_unwind`. On a panic the block is silent, the fault count
    /// goes up and the clock still advances. Returns false on a fault.
    pub fn process_guarded(&mut self, out: &mut [f32], frames: usize) -> bool {
        let start = self.now;
        let ok = catch_unwind(AssertUnwindSafe(|| self.process(out, frames))).is_ok();
        if !ok {
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
                    if fade.frames == 0 {
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
        } else if s.fade_out.frames > 0 && s.play_frames.is_none() {
            Some(RejectReason::BadFade)
        } else if self.voice_mut(s.voice).is_some() {
            Some(RejectReason::DuplicateId)
        } else {
            None
        };
        let free = self.voices.iter_mut().find(|v| !v.is_active());
        match (reject, free) {
            (None, Some(v)) => v.begin(s),
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
            scratch,
            ..
        } = self;

        patches.iter_mut().for_each(|p| p.live = false);
        for v in voices.iter_mut().filter(|v| v.is_active()) {
            let Some(p) = patches.get_mut(v.patch) else {
                continue;
            };
            if !p.live {
                p.live = true;
                for b in p.bus.chunks_exact_mut(block) {
                    b.iter_mut().take(len).for_each(|s| *s = 0.0);
                }
            }
            render_voice(v, len, t0, block, &mut p.bus, scratch, outbox);
        }

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
        for ((dv, g), dl) in dev
            .chunks_exact_mut(block)
            .zip(out_gain.iter_mut())
            .zip(delays.iter_mut())
        {
            g.fill(tmp);
            for ((y, gg), m) in dv.iter_mut().zip(tmp.iter()).zip(mb.iter()) {
                *y = dl.tick(*y) * gg * m;
            }
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
    let (got, outcome) = v.read(live, len, block);
    let (Some(vg), Some(og), Some(tmp)) = (
        s.vgain.get_mut(..len),
        s.og.get_mut(..len),
        s.tmp.get_mut(..len),
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
    v.apply_envelope(vg, pos0, got);

    let Voice {
        cue_out,
        xp,
        input,
        channels,
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
        co.fill(og);
        og.iter_mut().zip(vg.iter()).for_each(|(g, v)| *g *= v);
        let Some(bo) = bus.get_mut(o * block..o * block + len) else {
            continue;
        };
        for i in 0..ch {
            let Some(p) = xp.get_mut(i * outs + o) else {
                continue;
            };
            if p.is_silent() {
                continue;
            }
            p.fill(tmp);
            let Some(inp) = input.get(i * block..i * block + len) else {
                continue;
            };
            for (((b, x), g), h) in bo.iter_mut().zip(inp).zip(tmp.iter()).zip(og.iter()) {
                *b += x * g * h;
            }
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
}
