// SPDX-License-Identifier: GPL-3.0-or-later

//! The output devices the engine plays on (AUD-02): one entry per open device, each with its
//! own mixer and device supervisor. Entry 0 is the primary device, opened at start; the others
//! open when a patch is mapped to them and close when none is (once no voice plays there). A
//! voice plays in the mixer of its patch's device, chosen at its start; every later command of
//! it goes to that mixer.
//!
//! **Clock.** The engine's time (the master clock) is the primary device's sample counter while
//! that device plays, so timing on it stays sample-accurate. Another device's commands are dated
//! by translation from one pair of counter reads: `local = local_rendered + (at -
//! master_rendered) × local rate / master rate`; its events come back through the inverse.
//! Simultaneous starts on two devices land within one buffer of each other.
//!
//! **Rates.** Each device runs at its own rate (the primary's when it can) with its own mixer.
//! Everything the control thread counts is in frames of the master clock (the primary's rate);
//! a command routed to a device at another rate has its frame counts (fades, trims) converted
//! to that device's frames here, the one place every voice command passes. A voice's stream and
//! slice table are made at its device's rate when it is prepared.
//! While the primary device is away and another plays, the master clock carries on from the
//! monotonic wall clock, from the last sample the primary rendered; when it returns its counter
//! is offset to continue from there. With no device playing, the clock stands still (nothing
//! can play). It never goes back.

use std::collections::HashMap;
use std::time::Instant;

use openlab_audio::{CommandKind, Event, MixerController, VoiceId};
use openlab_core::SampleTime;
use openlab_core::protocol::OutputState;

use super::Backend;
use super::devices::{OutputKey, WallClock};

/// Most output devices open at once (the primary included).
pub const MAX_OUTPUTS: usize = 16;

/// One open output device.
pub(super) struct Output {
    /// The device's name, as patch mappings name it.
    pub name: String,
    pub mixer: MixerController,
    pub backend: Backend,
}

impl Output {
    /// The device plays (always, for the offline driver).
    pub fn is_up(&self) -> bool {
        match &self.backend {
            Backend::Device(d) => d.is_playing(),
            Backend::Offline => true,
        }
    }

    /// Where the device is, as `status.outputs` and `list_devices` report it.
    pub fn state(&self) -> OutputState {
        let Backend::Device(d) = &self.backend else {
            return OutputState::Up;
        };
        if d.unrecoverable().is_some() {
            OutputState::Failed
        } else if d.is_parked() {
            OutputState::Closed
        } else if d.is_reconfiguring() {
            OutputState::Reconfiguring
        } else if !d.has_opened() || d.is_resuming() {
            OutputState::Opening
        } else if d.is_down() {
            OutputState::Lost
        } else {
            OutputState::Up
        }
    }

    /// Frames per callback of its stream, if known.
    pub fn block(&self) -> Option<u64> {
        match &self.backend {
            Backend::Device(d) => d.info().block_frames.map(u64::from),
            Backend::Offline => None,
        }
    }
}

/// Where the master clock's time comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Clock {
    /// The primary's counter plus `Outputs::offset`.
    Primary,
    /// The wall clock since `since`, from `base` (the primary is away, another device plays).
    Wall { base: SampleTime, since: Instant },
    /// Stands at `base`: no device plays.
    Frozen { base: SampleTime },
}

/// The engine's output devices and its master clock.
pub(super) struct Outputs {
    /// Output 0.
    primary: Output,
    /// Outputs 1, 2, ... in the order they opened (never removed in a run).
    others: Vec<Output>,
    /// Master time less the primary's counter (grows each time the primary returns from an
    /// outage the clock ran through).
    offset: SampleTime,
    clock: Clock,
    /// The wall clock the master clock runs on while the primary is away (the primary
    /// device's policy clock; none for the offline driver, which never loses its device).
    wall: Option<WallClock>,
    rate: u32,
    /// Each output's mixer rate, by key (the primary's is `rate`).
    rates: Vec<u32>,
    /// Voices on a device other than the primary: their device and the last local sample a
    /// command of theirs was dated, so commands of one voice never go backwards. A voice not
    /// here plays on the primary.
    voices: HashMap<VoiceId, (OutputKey, SampleTime)>,
}

impl Outputs {
    pub fn new(primary: Output, rate: u32) -> Self {
        let wall = match &primary.backend {
            Backend::Device(d) => Some(d.wall()),
            Backend::Offline => None,
        };
        Self {
            primary,
            others: Vec::new(),
            offset: 0,
            clock: Clock::Primary,
            wall,
            rate,
            rates: vec![rate],
            voices: HashMap::new(),
        }
    }

    /// Each output's rate, by key: the frames of its mixer, streams and commands.
    pub fn rates(&self) -> &[u32] {
        &self.rates
    }

    /// Output `key`'s rate (the master clock's for an unknown key).
    pub fn rate_of(&self, key: OutputKey) -> u32 {
        self.rates.get(key).copied().unwrap_or(self.rate)
    }

    pub fn primary(&self) -> &Output {
        &self.primary
    }

    #[cfg(test)]
    pub fn primary_mut(&mut self) -> &mut Output {
        &mut self.primary
    }

    pub fn get(&self, key: OutputKey) -> Option<&Output> {
        match key {
            0 => Some(&self.primary),
            k => self.others.get(k - 1),
        }
    }

    pub fn get_mut(&mut self, key: OutputKey) -> Option<&mut Output> {
        match key {
            0 => Some(&mut self.primary),
            k => self.others.get_mut(k - 1),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (OutputKey, &Output)> {
        std::iter::once(&self.primary)
            .chain(&self.others)
            .enumerate()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Output> {
        std::iter::once(&mut self.primary).chain(&mut self.others)
    }

    pub fn len(&self) -> usize {
        self.others.len() + 1
    }

    /// The output playing on device `name`, if one is open (or opening).
    pub fn by_name(&self, name: &str) -> Option<OutputKey> {
        self.iter().find(|(_, o)| o.name == name).map(|(k, _)| k)
    }

    /// Adds an output; `None` when [`MAX_OUTPUTS`] are open.
    pub fn add(&mut self, output: Output) -> Option<OutputKey> {
        if self.len() >= MAX_OUTPUTS {
            return None;
        }
        self.rates.push(output.mixer.sample_rate());
        self.others.push(output);
        Some(self.others.len())
    }

    /// The key the next output added gets.
    pub fn next_key(&self) -> OutputKey {
        self.len()
    }

    /// The master clock: samples of show time rendered.
    pub fn rendered(&self) -> SampleTime {
        match self.clock {
            Clock::Primary => self
                .primary()
                .mixer
                .samples_rendered()
                .saturating_add(self.offset),
            Clock::Wall { base, since } => base.saturating_add(self.wall_samples(since)),
            Clock::Frozen { base } => base,
        }
    }

    /// The master clock stands still: no device plays.
    pub fn is_frozen(&self) -> bool {
        matches!(self.clock, Clock::Frozen { .. })
    }

    /// The master clock runs on the wall clock (the primary is away, another device plays).
    pub fn on_wall_clock(&self) -> bool {
        matches!(self.clock, Clock::Wall { .. })
    }

    fn wall_samples(&self, since: Instant) -> SampleTime {
        let Some(wall) = &self.wall else { return 0 };
        let elapsed = wall().saturating_duration_since(since);
        let samples = elapsed.as_nanos() * u128::from(self.rate) / 1_000_000_000;
        u64::try_from(samples).unwrap_or(u64::MAX)
    }

    /// Follows the devices: the master clock comes from the primary while it plays, from the
    /// wall clock while it is away and another device plays, and stands still while none
    /// does. Called each tick and after every device event; the value never goes back.
    pub fn follow_clock(&mut self) {
        let primary_up = self.primary().is_up();
        let others_up = self.others.iter().any(Output::is_up);
        let now = self.rendered();
        let wall_now = self.wall.as_ref().map(|w| w());
        self.clock = match (self.clock, primary_up) {
            (Clock::Primary, true) => Clock::Primary,
            (_, true) => {
                // The primary is back: its counter, offset to go on from here.
                self.offset = now.saturating_sub(self.primary().mixer.samples_rendered());
                Clock::Primary
            }
            (Clock::Wall { .. }, false) if others_up => self.clock,
            (_, false) => match wall_now {
                Some(since) if others_up => Clock::Wall { base: now, since },
                _ => Clock::Frozen { base: now },
            },
        };
    }

    /// The output voice `voice` plays on.
    pub fn output_of(&self, voice: VoiceId) -> OutputKey {
        self.voices.get(&voice).map_or(0, |&(o, _)| o)
    }

    /// Voices on output `key` among `voices`.
    pub fn on_output(&self, key: OutputKey, voices: Vec<VoiceId>) -> Vec<VoiceId> {
        voices
            .into_iter()
            .filter(|&v| self.output_of(v) == key)
            .collect()
    }

    /// Whether a voice may still play on output `key`.
    pub fn has_voices(&self, key: OutputKey) -> bool {
        self.voices.values().any(|&(o, _)| o == key)
    }

    /// Forgets every voice of output `key` (its records were settled as ended).
    pub fn forget_output(&mut self, key: OutputKey) {
        self.voices.retain(|_, &mut (o, _)| o != key);
    }

    /// Forgets voices `keep` says are gone.
    pub fn retain_voices(&mut self, mut keep: impl FnMut(VoiceId) -> bool) {
        self.voices.retain(|&v, _| keep(v));
    }

    /// Routes one batch of master-dated commands: each goes to the output its voice plays on
    /// (`slot_output`: the output of a patch slot, for a start), dated in that output's samples.
    /// Commands keep their order within each output.
    pub fn route(
        &mut self,
        commands: Vec<(SampleTime, CommandKind)>,
        slot_output: impl Fn(u16) -> Option<OutputKey>,
    ) -> Vec<(OutputKey, Vec<(SampleTime, CommandKind)>)> {
        let mut out: Vec<(OutputKey, Vec<(SampleTime, CommandKind)>)> = Vec::new();
        let master = self.rendered();
        for (at, kind) in commands {
            let key = match &kind {
                CommandKind::StartVoice(v) => {
                    let key = slot_output(v.patch).unwrap_or(0);
                    if key != 0 {
                        self.voices.insert(v.voice, (key, 0));
                    }
                    key
                }
                CommandKind::ResumeVoice { voice, new_id } => {
                    let entry = self.voices.get(voice).copied();
                    if let Some(entry) = entry {
                        self.voices.insert(*new_id, entry);
                    }
                    entry.map_or(0, |(o, _)| o)
                }
                CommandKind::StopVoice { voice, .. }
                | CommandKind::Ramp { voice, .. }
                | CommandKind::RampSet { voice, .. }
                | CommandKind::MoveVoice { voice, .. }
                | CommandKind::Devamp { voice, .. }
                | CommandKind::PauseVoice { voice } => self.output_of(*voice),
                CommandKind::SetPatchRouting { patch, .. } | CommandKind::SnapPatch { patch } => {
                    slot_output(*patch).unwrap_or(0)
                }
                CommandKind::SetMaster { .. } => 0,
            };
            let local = self.local_date(key, at, master, command_voice(&kind));
            let mut kind = kind;
            at_rate(&mut kind, self.rate, self.rate_of(key));
            match out.iter_mut().find(|(k, _)| *k == key) {
                Some((_, batch)) => batch.push((local, kind)),
                None => out.push((key, vec![(local, kind)])),
            }
        }
        out
    }

    /// Master-dated commands that are not a voice's (patch slots, the master), dated on output
    /// `key`.
    pub fn dated_on(
        &mut self,
        key: OutputKey,
        commands: Vec<(SampleTime, CommandKind)>,
    ) -> Vec<(SampleTime, CommandKind)> {
        let master = self.rendered();
        commands
            .into_iter()
            .map(|(at, kind)| (self.local_date(key, at, master, None), kind))
            .collect()
    }

    /// Master sample `at` on output `key`, for a command of `voice`. The primary's samples are
    /// the master's less the offset, exactly. Another device's are translated from the pair of
    /// counters read now; while that device is away, its commands are dated on the first sample
    /// it renders when it is back. A voice's commands never go backwards.
    fn local_date(
        &mut self,
        key: OutputKey,
        at: SampleTime,
        master: SampleTime,
        voice: Option<VoiceId>,
    ) -> SampleTime {
        if key == 0 {
            if self.primary.is_up() {
                return at.saturating_sub(self.offset);
            }
            // Away: dated from the sample it renders first when it is back, as far after that
            // as the command is after the master clock now, not as far on as the master clock
            // has run on the wall clock meanwhile. While no device plays this is the same as
            // the offset (the clock stands on the primary's last sample).
            let rendered = self.primary.mixer.samples_rendered();
            let ahead = at.saturating_sub(master);
            return rendered.saturating_add(ahead);
        }
        let Some(output) = self.get(key) else {
            return at;
        };
        let local_rendered = output.mixer.samples_rendered();
        let ahead = i128::from(at) - i128::from(master);
        let mut local = if output.is_up() {
            let shifted = i128::from(local_rendered)
                + rescale_signed(ahead, self.rate, output.mixer.sample_rate());
            u64::try_from(shifted.max(i128::from(local_rendered))).unwrap_or(local_rendered)
        } else {
            local_rendered
        };
        if let Some(entry) = voice.and_then(|v| self.voices.get_mut(&v)) {
            local = local.max(entry.1);
            entry.1 = local;
        }
        local
    }

    /// Output `key`'s sample `at` as master time (the inverse of the commands' translation).
    pub fn master_date(&self, key: OutputKey, at: SampleTime) -> SampleTime {
        if key == 0 {
            return at.saturating_add(self.offset);
        }
        let Some(output) = self.get(key) else {
            return at;
        };
        let behind = i128::from(at) - i128::from(output.mixer.samples_rendered());
        let shifted = i128::from(self.rendered())
            + rescale_signed(behind, output.mixer.sample_rate(), self.rate);
        u64::try_from(shifted.max(0)).unwrap_or(0)
    }

    /// The events of output `key`'s mixer, dated in master time. A voice that finished is
    /// forgotten.
    pub fn poll_events(&mut self, key: OutputKey) -> Vec<Event> {
        let mut events = Vec::new();
        let Some(output) = self.get_mut(key) else {
            return events;
        };
        while let Some(e) = output.mixer.poll_event() {
            events.push(e);
        }
        let local = output.mixer.sample_rate();
        for e in &mut events {
            if let Some(at) = event_date(e) {
                *at = self.master_date(key, *at);
            }
            span_at_master(e, local, self.rate);
            if let Event::VoiceFinished { voice, .. } = e {
                self.voices.remove(voice);
            }
        }
        events
    }

    /// Hands every output's held commands to its mixer.
    pub fn pump(&mut self) {
        for o in self.iter_mut() {
            o.mixer.pump();
        }
    }
}

/// `x` frames at rate `from` as frames at rate `to`, rounded half away from zero (a rate of 0
/// gives 0).
fn rescale_signed(x: i128, from: u32, to: u32) -> i128 {
    if from == to {
        return x;
    }
    if from == 0 {
        return 0;
    }
    let (from, to) = (i128::from(from), i128::from(to));
    let scaled = x.saturating_mul(to);
    let half = from / 2;
    if scaled >= 0 {
        (scaled + half) / from
    } else {
        (scaled - half) / from
    }
}

/// A command's frame counts (fades, trims), made in frames at rate `from` (the master clock's),
/// as frames at rate `to` (its device's). A fade that had any length keeps at least one frame,
/// so a ramp stays a ramp and a fading stop is not turned into a cut.
fn at_rate(kind: &mut CommandKind, from: u32, to: u32) {
    if from == to {
        return;
    }
    let frames = |n: u64| openlab_audio::frames_at_rate(n, from, to);
    let fade = |f: &mut openlab_audio::Fade| {
        if f.frames > 0 {
            f.frames = frames(f.frames).max(1);
        }
    };
    match kind {
        CommandKind::StartVoice(v) => {
            fade(&mut v.fade_in);
            fade(&mut v.fade_out);
            v.skip_frames = frames(v.skip_frames);
            v.play_frames = v.play_frames.map(frames);
        }
        CommandKind::StopVoice { fade: f, .. }
        | CommandKind::Ramp { fade: f, .. }
        | CommandKind::RampSet { fade: f, .. } => fade(f),
        CommandKind::SetPatchRouting { .. }
        | CommandKind::SnapPatch { .. }
        | CommandKind::MoveVoice { .. }
        | CommandKind::SetMaster { .. }
        | CommandKind::Devamp { .. }
        | CommandKind::PauseVoice { .. }
        | CommandKind::ResumeVoice { .. } => {}
    }
}

fn command_voice(kind: &CommandKind) -> Option<VoiceId> {
    match kind {
        CommandKind::StartVoice(v) => Some(v.voice),
        CommandKind::StopVoice { voice, .. }
        | CommandKind::Ramp { voice, .. }
        | CommandKind::RampSet { voice, .. }
        | CommandKind::MoveVoice { voice, .. }
        | CommandKind::Devamp { voice, .. }
        | CommandKind::PauseVoice { voice } => Some(*voice),
        CommandKind::ResumeVoice { new_id, .. } => Some(*new_id),
        CommandKind::SetPatchRouting { .. }
        | CommandKind::SnapPatch { .. }
        | CommandKind::SetMaster { .. } => None,
    }
}

/// An event's count of frames, counted by a mixer at `local`, as master frames: what follows
/// it (a picture following its sound) is planned in master time.
fn span_at_master(e: &mut Event, local: u32, master: u32) {
    if let Event::UnderrunEnded { frames, .. } = e {
        let scaled = rescale_signed(i128::from(*frames), local, master);
        *frames = u64::try_from(scaled).unwrap_or(u64::MAX);
    }
}

/// The sample an event is dated on.
fn event_date(e: &mut Event) -> Option<&mut SampleTime> {
    match e {
        Event::VoiceFinished { at, .. }
        | Event::Underrun { at, .. }
        | Event::UnderrunEnded { at, .. }
        | Event::VoiceRejected { at, .. }
        | Event::PendingFull { at }
        | Event::LoopReached { at, .. }
        | Event::SliceEnded { at, .. }
        | Event::DevampScheduled { at, .. }
        | Event::DevampIgnored { at, .. }
        | Event::MoveEnded { at, .. }
        | Event::StageFaulted { at, .. } => Some(at),
    }
}

#[cfg(test)]
mod tests {
    use openlab_audio::{Curve, Fade, Mixer, MixerConfig, VoiceStart};

    use super::*;

    fn output(name: &str, rate: u32) -> Output {
        let (mixer, _) = Mixer::new(MixerConfig {
            sample_rate: rate,
            ..MixerConfig::default()
        })
        .unwrap();
        Output {
            name: name.into(),
            mixer,
            backend: Backend::Offline,
        }
    }

    fn start(voice: u64, patch: u16) -> CommandKind {
        let (_, rx) = rtrb::RingBuffer::new(4);
        let mut v = VoiceStart::new(VoiceId(voice), rx, 1, 1);
        v.patch = patch;
        v.fade_in = Fade::new(480, Curve::Linear);
        v.fade_out = Fade::new(1, Curve::Linear);
        CommandKind::StartVoice(Box::new(v))
    }

    /// A voice on a device at another rate: its commands are dated `(at - master) × local /
    /// master` after that device's counter and their fades are as long in time there (at least
    /// a frame, so a fade is never turned into a cut); the primary's are left alone; the
    /// device's events come back through the inverse.
    #[test]
    fn commands_for_a_device_at_another_rate_are_dated_and_timed_at_its_rate() {
        let mut o = Outputs::new(output("one", 48_000), 48_000);
        assert_eq!(o.add(output("two", 44_100)), Some(1));
        assert_eq!(o.rates(), &[48_000, 44_100]);
        let fade = |frames| Fade::new(frames, Curve::Linear);
        let routed = o.route(
            vec![
                (4_800, start(1, 1)),
                (
                    9_600,
                    CommandKind::StopVoice {
                        voice: VoiceId(1),
                        fade: fade(480),
                    },
                ),
                (
                    9_600,
                    CommandKind::Ramp {
                        voice: VoiceId(1),
                        target: openlab_audio::GainTarget::Master,
                        gain: 0.5,
                        fade: fade(1),
                    },
                ),
                (4_800, start(2, 0)),
            ],
            |patch| Some(OutputKey::from(patch)),
        );
        let (one, two) = (&routed[1], &routed[0]);
        assert_eq!(two.0, 1);
        let dates: Vec<SampleTime> = two.1.iter().map(|(at, _)| *at).collect();
        assert_eq!(dates, vec![4_410, 8_820, 8_820]);
        let CommandKind::StartVoice(v) = &two.1[0].1 else {
            panic!()
        };
        assert_eq!((v.fade_in.frames, v.fade_out.frames), (441, 1));
        assert!(matches!(two.1[1].1, CommandKind::StopVoice { fade, .. } if fade.frames == 441));
        assert!(matches!(two.1[2].1, CommandKind::Ramp { fade, .. } if fade.frames == 1));
        // The primary's: as sent.
        assert_eq!(one.0, 0);
        let CommandKind::StartVoice(v) = &one.1[0].1 else {
            panic!()
        };
        assert_eq!((one.1[0].0, v.fade_in.frames), (4_800, 480));
        assert_eq!(o.output_of(VoiceId(1)), 1);
        // The inverse: the device's sample 4 410 is master sample 4 800.
        assert_eq!(o.master_date(1, 4_410), 4_800);
        assert_eq!(o.master_date(1, 0), 0);
    }

    /// What a voice on a device at another rate missed is told in master frames, as its date
    /// is: the picture put back by it is planned in master time.
    #[test]
    fn an_underruns_length_on_a_device_at_another_rate_is_in_master_frames() {
        let mut e = Event::UnderrunEnded {
            voice: VoiceId(1),
            at: 0,
            frames: 4_410,
        };
        span_at_master(&mut e, 44_100, 48_000);
        assert!(matches!(e, Event::UnderrunEnded { frames: 4_800, .. }));
        span_at_master(&mut e, 48_000, 48_000);
        assert!(matches!(e, Event::UnderrunEnded { frames: 4_800, .. }));
        let mut other = Event::PendingFull { at: 7 };
        span_at_master(&mut other, 44_100, 48_000);
        assert!(matches!(other, Event::PendingFull { at: 7 }));
    }

    #[test]
    fn signed_rescaling_rounds_half_away_from_zero() {
        assert_eq!(rescale_signed(3, 2, 3), 5);
        assert_eq!(rescale_signed(-3, 2, 3), -5);
        assert_eq!(rescale_signed(-1, 48_000, 44_100), -1);
        assert_eq!(rescale_signed(7, 0, 44_100), 0);
        assert_eq!(rescale_signed(i128::MAX, 1, 2), i128::MAX);
    }
}
