// SPDX-License-Identifier: GPL-3.0-or-later

//! The real-time mixer (ARCHITECTURE §3.4, §3.5). Everything reachable from [`Mixer::process`]
//! runs on the audio thread: no allocation, locks, syscalls or logging. All buffers are sized in
//! [`Mixer::new`]; sources that must be freed go back to the controller on the return queue.

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rtrb::{Consumer, Producer, PushError, RingBuffer};

use crate::controller::MixerController;
use crate::gain::{Curve, Fade, Param, ramp_value};

/// Absolute time in frames at the device rate since the mixer started (ARCHITECTURE §3.2).
pub type SampleTime = u64;

/// Caller-chosen voice identifier; must be unique among active voices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VoiceId(pub u64);

pub const MAX_OUTPUT_CHANNELS: usize = 64;
pub const MAX_VOICE_CHANNELS: usize = 64;
/// Capacity of the command queue and of the mixer's pending (time-sorted) command list.
pub const COMMAND_CAPACITY: usize = 1024;
const EVENT_CAPACITY: usize = 1024;
/// De-zipper length for gain changes without an explicit ramp, and delay-change crossfades.
const DEZIP_SECONDS: f64 = 0.005;
/// Longest per-output delay (speaker alignment).
const MAX_DELAY_SECONDS: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MixerConfig {
    pub sample_rate: u32,
    /// Device outputs. The mixer has the same number of cue outputs.
    pub output_channels: usize,
    /// Internal render chunk; any device block size is accepted and split into chunks this size.
    pub max_block_frames: usize,
    pub max_voices: usize,
    pub max_voice_channels: usize,
}

impl Default for MixerConfig {
    fn default() -> Self {
        MixerConfig {
            sample_rate: 48_000,
            output_channels: 2,
            max_block_frames: 1024,
            max_voices: 128,
            max_voice_channels: 8,
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
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ConfigError::SampleRate => "sample rate must be non-zero",
            ConfigError::OutputChannels => "output channels must be 1..=64",
            ConfigError::BlockFrames => "max block frames must be non-zero",
            ConfigError::Voices => "max voices must be non-zero",
            ConfigError::VoiceChannels => "max voice channels must be 1..=64",
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

/// Everything needed to start a voice. `source` carries interleaved f32 frames at the device rate;
/// dropping its producer marks end of stream once the remaining frames are consumed.
#[derive(Debug)]
pub struct VoiceStart {
    pub voice: VoiceId,
    pub source: Consumer<f32>,
    pub channels: u16,
    /// Voice master gain (linear).
    pub gain: f32,
    /// Frames discarded from the source before playback (in point).
    pub skip_frames: u64,
    /// Frames played after the skip (out point); `None` plays to end of stream.
    pub play_frames: Option<u64>,
    pub fade_in: Fade,
    /// Integrated fade-out ending on the last played frame. Needs `play_frames`.
    pub fade_out: Fade,
}

impl VoiceStart {
    /// Unity gain, no trims or fades, identity crosspoints (input n → cue output n).
    pub fn new(voice: VoiceId, source: Consumer<f32>, channels: u16) -> Self {
        VoiceStart {
            voice,
            source,
            channels,
            gain: 1.0,
            skip_frames: 0,
            play_frames: None,
            fade_in: Fade::NONE,
            fade_out: Fade::NONE,
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
    StartVoice(VoiceStart),
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

/// A command applied at sample `at` (or at the start of the next block if `at` has passed).
#[derive(Debug)]
pub struct Command {
    pub at: SampleTime,
    pub kind: CommandKind,
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
}

/// Small `Copy` notifications from the audio thread. `at` is the sample time it happened.
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
}

#[derive(Debug, Default)]
pub(crate) struct Shared {
    pub(crate) samples_rendered: AtomicU64,
    pub(crate) faults: AtomicU64,
    pub(crate) underrun_frames: AtomicU64,
    pub(crate) dropped_events: AtomicU64,
    pub(crate) leaked_sources: AtomicU64,
}

/// Audio-thread side of the queues back to the controller.
struct Outbox {
    events: Producer<Event>,
    returns: Producer<Consumer<f32>>,
    shared: Arc<Shared>,
}

impl Outbox {
    fn emit(&mut self, e: Event) {
        if self.events.push(e).is_err() {
            self.shared.dropped_events.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn give_back(&mut self, source: Consumer<f32>) {
        if let Err(PushError::Full(source)) = self.returns.push(source) {
            // The return queue is sized so this cannot happen while the controller drains it;
            // leaking is the RT-safe fallback to freeing here.
            std::mem::forget(source);
            self.shared.leaked_sources.fetch_add(1, Ordering::Relaxed);
        }
    }
}

enum ReadOutcome {
    Full,
    Underrun,
    Ended(FinishReason),
}

struct Voice {
    id: VoiceId,
    /// `Some` while the voice is active.
    source: Option<Consumer<f32>>,
    channels: usize,
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
            source: None,
            channels: 0,
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

    fn start(&mut self, s: VoiceStart) {
        let outs = self.cue_out.len();
        self.id = s.voice;
        self.channels = usize::from(s.channels);
        self.skip = s.skip_frames;
        self.remaining = s.play_frames;
        self.play_frames = s.play_frames;
        self.pos = 0;
        self.fade_in = s.fade_in;
        self.fade_out = s.fade_out;
        self.master.set(sanitize(s.gain));
        self.stop.set(1.0);
        self.stop_end = None;
        self.cue_out.iter_mut().for_each(|p| p.set(1.0));
        for (k, p) in self.xp.iter_mut().enumerate() {
            let (i, o) = (k / outs, k % outs);
            p.set(if i == o { 1.0 } else { 0.0 });
        }
        self.fresh = true;
        self.underrunning = false;
        self.source = Some(s.source);
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

    /// Reads up to `len` frames into `input`, zero-filling the rest.
    fn read(&mut self, len: usize, block: usize) -> (usize, ReadOutcome) {
        let ch = self.channels;
        let Some(src) = self.source.as_mut() else {
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
        let mut want = len.min(avail);
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
        if got == len {
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
        if let Some(src) = self.source.take() {
            outbox.give_back(src);
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
}

impl DelayLine {
    fn new(max_delay: usize, xfade_len: u64) -> Self {
        DelayLine {
            buf: vec![0.0; max_delay + 1],
            write: 0,
            delay: 0,
            old_delay: 0,
            xfade: xfade_len,
        }
    }

    fn set_delay(&mut self, frames: usize) {
        let d = frames.min(self.buf.len() - 1);
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

    fn tick(&mut self, x: f32, xfade_len: u64) -> f32 {
        if let Some(s) = self.buf.get_mut(self.write) {
            *s = x;
        }
        let mut y = self.tap(self.delay);
        if self.xfade < xfade_len {
            let a = self.xfade as f32 / xfade_len as f32;
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

/// The real-time mixer. Create with [`Mixer::new`], move to the audio thread, call
/// [`Mixer::process_guarded`] from the device callback.
pub struct Mixer {
    outputs: usize,
    block: usize,
    max_voice_channels: usize,
    dezip: u64,
    now: SampleTime,
    commands: Consumer<Command>,
    /// Sorted by `at`, latest first; the next due command is last. Never grows past capacity.
    pending: Vec<Command>,
    outbox: Outbox,
    voices: Vec<Voice>,
    /// Row-major `[cue output][device output]`.
    patch: Vec<Param>,
    out_gain: Vec<Param>,
    delays: Vec<DelayLine>,
    master: Param,
    /// Planar `[cue output][frame]`.
    bus: Vec<f32>,
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
        } = config;
        if sample_rate == 0 {
            return Err(ConfigError::SampleRate);
        }
        if !(1..=MAX_OUTPUT_CHANNELS).contains(&outs) {
            return Err(ConfigError::OutputChannels);
        }
        if block == 0 {
            return Err(ConfigError::BlockFrames);
        }
        if max_voices == 0 {
            return Err(ConfigError::Voices);
        }
        if !(1..=MAX_VOICE_CHANNELS).contains(&max_voice_channels) {
            return Err(ConfigError::VoiceChannels);
        }
        let dezip = ((f64::from(sample_rate) * DEZIP_SECONDS).round() as u64).max(1);
        let max_delay = (sample_rate * MAX_DELAY_SECONDS) as usize;

        let (cmd_tx, cmd_rx) = RingBuffer::new(COMMAND_CAPACITY);
        let (ev_tx, ev_rx) = RingBuffer::new(EVENT_CAPACITY);
        // Every source the mixer can hold at once: queued, pending, playing, plus one in flight.
        let (ret_tx, ret_rx) = RingBuffer::new(2 * COMMAND_CAPACITY + max_voices + 1);
        let shared = Arc::new(Shared::default());

        let mut patch = vec![Param::new(0.0); outs * outs];
        for (k, p) in patch.iter_mut().enumerate() {
            if k / outs == k % outs {
                p.set(1.0);
            }
        }
        let mixer = Mixer {
            outputs: outs,
            block,
            max_voice_channels,
            dezip,
            now: 0,
            commands: cmd_rx,
            pending: Vec::with_capacity(COMMAND_CAPACITY),
            outbox: Outbox {
                events: ev_tx,
                returns: ret_tx,
                shared: Arc::clone(&shared),
            },
            voices: (0..max_voices)
                .map(|_| Voice::new(outs, max_voice_channels, block))
                .collect(),
            patch,
            out_gain: vec![Param::new(1.0); outs],
            delays: (0..outs)
                .map(|_| DelayLine::new(max_delay, dezip))
                .collect(),
            master: Param::new(1.0),
            bus: vec![0.0; outs * block],
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
        let controller = MixerController::new(cmd_tx, ev_rx, ret_rx, shared);
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
            while self.pending.last().is_some_and(|c| c.at <= t) {
                if let Some(c) = self.pending.pop() {
                    self.apply(c, t);
                }
            }
            let end = match self.pending.last() {
                Some(c) if c.at < self.now + n as u64 => (c.at - self.now) as usize,
                _ => n,
            };
            self.render(out, pos, end - pos);
            pos = end;
        }
        self.now += n as u64;
    }

    fn pull_commands(&mut self) {
        while self.pending.len() < self.pending.capacity() {
            let Ok(c) = self.commands.pop() else { break };
            // Equal times keep queue order: the new command goes before (= pops after) its peers.
            let i = self.pending.partition_point(|p| p.at > c.at);
            self.pending.insert(i, c);
        }
    }

    fn apply(&mut self, cmd: Command, t: SampleTime) {
        let outs = self.outputs;
        let dezip = self.dezip;
        match cmd.kind {
            CommandKind::StartVoice(s) => self.start_voice(s, t),
            CommandKind::StopVoice { voice, fade } => {
                let Mixer { voices, outbox, .. } = self;
                if let Some(v) = voices
                    .iter_mut()
                    .find(|v| v.source.is_some() && v.id == voice)
                {
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
                        p.ramp_to(sanitize(gain), frames, curve);
                    }
                }
            }
            CommandKind::SetPatchRouting {
                cue_output,
                device_output,
                gain,
            } => {
                let (o, d) = (usize::from(cue_output), usize::from(device_output));
                if o < outs
                    && d < outs
                    && let Some(p) = self.patch.get_mut(o * outs + d)
                {
                    p.ramp_to(sanitize(gain), dezip, Curve::Linear);
                }
            }
            CommandKind::SetOutput { output, settings } => {
                let o = usize::from(output);
                let target = if settings.mute {
                    0.0
                } else {
                    sanitize(settings.level)
                } * if settings.invert { -1.0 } else { 1.0 };
                if let Some(p) = self.out_gain.get_mut(o) {
                    p.ramp_to(target, dezip, Curve::Linear);
                }
                if let Some(d) = self.delays.get_mut(o) {
                    d.set_delay(settings.delay_frames as usize);
                }
            }
            CommandKind::SetMaster { gain } => {
                self.master.ramp_to(sanitize(gain), dezip, Curve::Linear)
            }
        }
    }

    fn voice_mut(&mut self, id: VoiceId) -> Option<&mut Voice> {
        self.voices
            .iter_mut()
            .find(|v| v.source.is_some() && v.id == id)
    }

    fn start_voice(&mut self, s: VoiceStart, t: SampleTime) {
        let reject = if s.channels == 0 || usize::from(s.channels) > self.max_voice_channels {
            Some(RejectReason::BadChannelCount)
        } else if self.voice_mut(s.voice).is_some() {
            Some(RejectReason::DuplicateId)
        } else {
            None
        };
        let free = self.voices.iter_mut().find(|v| v.source.is_none());
        match (reject, free) {
            (None, Some(v)) => v.start(s),
            (reason, _) => {
                let voice = s.voice;
                self.outbox.give_back(s.source);
                self.outbox.emit(Event::VoiceRejected {
                    voice,
                    at: t,
                    reason: reason.unwrap_or(RejectReason::PoolFull),
                });
            }
        }
    }

    /// Renders frames `a..a + len` of the chunk: voices → cue-output bus → patch → outputs.
    fn render(&mut self, out: &mut [f32], a: usize, len: usize) {
        let t0 = self.now + a as u64;
        let (block, outs, dezip) = (self.block, self.outputs, self.dezip);
        let Mixer {
            voices,
            outbox,
            patch,
            out_gain,
            delays,
            master,
            bus,
            dev,
            scratch,
            ..
        } = self;

        for b in bus.chunks_exact_mut(block) {
            b.iter_mut().take(len).for_each(|s| *s = 0.0);
        }
        for v in voices.iter_mut().filter(|v| v.source.is_some()) {
            render_voice(v, len, t0, block, bus, scratch, outbox);
        }

        let Some(tmp) = scratch.tmp.get_mut(..len) else {
            return;
        };
        for (d, dv) in dev.chunks_exact_mut(block).enumerate() {
            let Some(dv) = dv.get_mut(..len) else {
                continue;
            };
            dv.fill(0.0);
            for (o, bo) in bus.chunks_exact(block).enumerate() {
                let Some(p) = patch.get_mut(o * outs + d) else {
                    continue;
                };
                if p.is_silent() {
                    continue;
                }
                p.fill(tmp);
                for ((y, x), g) in dv.iter_mut().zip(bo).zip(tmp.iter()) {
                    *y += x * g;
                }
            }
        }

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
                *y = dl.tick(*y * gg, dezip) * m;
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
    let pos0 = v.pos;
    let (got, outcome) = v.read(len, block);
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
                .fetch_add((len - got) as u64, Ordering::Relaxed);
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

/// Non-finite gains would poison the whole output; treat them as silence.
fn sanitize(g: f32) -> f32 {
    if g.is_finite() { g } else { 0.0 }
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
        assert_eq!(bad(|c| c.max_voices = 0), Some(ConfigError::Voices));
        assert_eq!(
            bad(|c| c.max_voice_channels = 65),
            Some(ConfigError::VoiceChannels)
        );
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
        ctl.send(
            0,
            CommandKind::StartVoice(VoiceStart::new(VoiceId(1), rx, 2)),
        )
        .unwrap();
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
}
