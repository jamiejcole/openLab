// SPDX-License-Identifier: GPL-3.0-or-later

//! Fault isolation: which unit a panic on the audio thread is blamed on, and what is dropped,
//! reset or disabled so the rest of the show plays on.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::{
    CommandKind, Event, FinishReason, MAX_OUTPUT_CHANNELS, MAX_PATCHES, Mixer, SampleTime, Voice,
    VoiceId,
};

/// Faulted blocks, each blamed on the same unit (a voice, a patch, a device output, the master
/// or the meters), within [`FAULT_WINDOW`] blocks, after which a voice is dropped and any other
/// unit disabled. Faults with no unit to blame rebuild the mixer's non-voice state at this
/// limit. A fault silences the whole block, so a unit that panics every block (or every other
/// one) would otherwise silence the show; a one-off fault, or faults further apart than the
/// window, pass without losing anything.
pub const FAULT_LIMIT: usize = 3;

/// The window of [`FAULT_LIMIT`], in device blocks (`process_guarded` calls): 16 blocks of 512
/// frames at 48 kHz is ~170 ms.
pub const FAULT_WINDOW: u64 = 16;

/// The blocks of a unit's last [`FAULT_LIMIT`] faults. Fixed size: noting a fault never
/// allocates.
#[derive(Clone, Copy, Debug)]
pub(super) struct FaultWindow {
    at: [u64; FAULT_LIMIT],
    len: usize,
}

impl FaultWindow {
    pub(super) const EMPTY: Self = FaultWindow {
        at: [0; FAULT_LIMIT],
        len: 0,
    };

    /// Notes a fault in block `block`; true when that makes [`FAULT_LIMIT`] faults within
    /// [`FAULT_WINDOW`] blocks.
    fn note(&mut self, block: u64) -> bool {
        if self.len == FAULT_LIMIT {
            self.at.rotate_left(1);
            self.len -= 1;
        }
        if let Some(slot) = self.at.get_mut(self.len) {
            *slot = block;
            self.len += 1;
        }
        self.len == FAULT_LIMIT
            && self
                .at
                .first()
                .is_some_and(|&first| block.saturating_sub(first) < FAULT_WINDOW)
    }
}

/// The recent faults of a voice slot, for the voice `id` (a slot that starts another voice
/// starts afresh).
#[derive(Clone, Copy, Debug)]
pub(super) struct FaultLog {
    id: VoiceId,
    window: FaultWindow,
}

impl FaultLog {
    pub(super) const EMPTY: Self = FaultLog {
        id: VoiceId(0),
        window: FaultWindow::EMPTY,
    };

    /// Notes a fault of voice `id` in block `block`; true at the limit ([`FaultWindow::note`]).
    fn note(&mut self, id: VoiceId, block: u64) -> bool {
        if self.id != id {
            *self = FaultLog { id, ..Self::EMPTY };
        }
        self.window.note(block)
    }
}

/// The recent faults of a non-voice unit, and whether they disabled it.
#[derive(Clone, Copy, Debug)]
pub(super) struct Health {
    faults: FaultWindow,
    pub(super) disabled: bool,
}

impl Health {
    pub(super) const OK: Self = Health {
        faults: FaultWindow::EMPTY,
        disabled: false,
    };

    /// Notes a fault in block `block`; true once the unit is disabled.
    fn fault(&mut self, block: u64) -> bool {
        self.disabled |= self.faults.note(block);
        self.disabled
    }
}

/// What the mixer is doing: the unit a panic is blamed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stage {
    /// Nothing one unit owns (taking commands off the queue, scheduling, the copy out).
    Idle,
    /// Rendering the voice at this pool index.
    Voice(usize),
    /// Applying a command to voice `voice` (`alt`: the id a resume gives it, else `voice`).
    /// `start`: the command starts it, so it may not be in the pool.
    VoiceCommand {
        voice: VoiceId,
        alt: VoiceId,
        start: bool,
    },
    /// Mixing patch `k`, or applying a routing command to it.
    Patch(usize),
    /// Device output `o`: the master applied to it and its meter peak.
    Output(usize),
    Master,
    Meters,
}

impl Stage {
    /// The unit a command acts on.
    pub(super) fn of(kind: &CommandKind) -> Self {
        let on = |voice, alt, start| Stage::VoiceCommand { voice, alt, start };
        match kind {
            CommandKind::StartVoice(s) => on(s.voice, s.voice, true),
            CommandKind::StopVoice { voice, .. }
            | CommandKind::Ramp { voice, .. }
            | CommandKind::Devamp { voice, .. }
            | CommandKind::MoveVoice { voice, .. }
            | CommandKind::PauseVoice { voice } => on(*voice, *voice, false),
            CommandKind::ResumeVoice { voice, new_id } => on(*voice, *new_id, false),
            CommandKind::SetPatchRouting { patch, .. } | CommandKind::SnapPatch { patch } => {
                Stage::Patch(usize::from(*patch))
            }
            CommandKind::SetMaster { .. } => Stage::Master,
        }
    }
}

/// The non-voice unit a fault was blamed on ([`Event::StageFaulted`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FaultStage {
    /// A patch matrix (mixing its bus, or a routing command for it). Its gains were reset to
    /// silence; the control thread re-sends its routing.
    Patch(u16),
    /// A device output (the master applied to it, its meter peak). It has no state to reset;
    /// the fault is reported, and the output disabled at the limit.
    Output(u16),
    /// The master gain (or a `SetMaster`). Reset to unity; the control thread re-sends it.
    Master,
    /// Publishing the meters.
    Meters,
    /// Faults with no unit to blame reached [`FAULT_LIMIT`]: every patch was reset to silence,
    /// the master to unity, every unit re-enabled; the control thread re-sends them.
    Mixer,
}

/// The units the mixer disabled, as atomics: what a lost [`Event::StageFaulted`] report would
/// otherwise hide. Bit `k` of `patches` / `outputs` is patch `k` / device output `k`
/// ([`MAX_PATCHES`] and [`MAX_OUTPUT_CHANNELS`] are 64).
#[derive(Debug, Default)]
pub(crate) struct DisabledFlags {
    patches: AtomicU64,
    outputs: AtomicU64,
    master: AtomicBool,
    meters: AtomicBool,
}

// A unit past bit 63 would never be published, so a lost report of it would go unfound.
const _: () = assert!(MAX_PATCHES <= 64 && MAX_OUTPUT_CHANNELS <= 64);

impl DisabledFlags {
    fn note(&self, stage: FaultStage) {
        let bit = |k: u16| 1u64.checked_shl(u32::from(k)).unwrap_or(0);
        match stage {
            FaultStage::Patch(k) => self.patches.fetch_or(bit(k), Ordering::Release),
            FaultStage::Output(o) => self.outputs.fetch_or(bit(o), Ordering::Release),
            FaultStage::Master => u64::from(self.master.swap(true, Ordering::Release)),
            FaultStage::Meters => u64::from(self.meters.swap(true, Ordering::Release)),
            FaultStage::Mixer => 0,
        };
    }

    fn clear(&self) {
        self.patches.store(0, Ordering::Release);
        self.outputs.store(0, Ordering::Release);
        self.master.store(false, Ordering::Release);
        self.meters.store(false, Ordering::Release);
    }

    pub(crate) fn read(&self) -> DisabledUnits {
        DisabledUnits {
            patches: self.patches.load(Ordering::Acquire),
            outputs: self.outputs.load(Ordering::Acquire),
            master: self.master.load(Ordering::Acquire),
            meters: self.meters.load(Ordering::Acquire),
        }
    }
}

/// The units the mixer has disabled after repeated faults, until a rebuild
/// ([`MixerController::disabled_units`](crate::MixerController::disabled_units)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DisabledUnits {
    /// Bit `k`: patch `k`.
    pub patches: u64,
    /// Bit `o`: device output `o`.
    pub outputs: u64,
    pub master: bool,
    pub meters: bool,
}

impl DisabledUnits {
    /// Every unit disabled, as the stage its report names.
    pub fn stages(self) -> impl Iterator<Item = FaultStage> {
        let bits = |mask: u64| (0..64u16).filter(move |&k| mask >> k & 1 == 1);
        bits(self.patches)
            .map(FaultStage::Patch)
            .chain(bits(self.outputs).map(FaultStage::Output))
            .chain(self.master.then_some(FaultStage::Master))
            .chain(self.meters.then_some(FaultStage::Meters))
    }
}

/// The stage the mixer is in ([`Mixer::isolate`]).
pub(super) struct StageTracker {
    pub(super) now: Stage,
    /// Entering this stage panics (a patch, output, master or meter fault).
    #[cfg(test)]
    pub(super) panic_at: Option<Stage>,
}

impl StageTracker {
    pub(super) fn enter(&mut self, stage: Stage) {
        self.now = stage;
        #[cfg(test)]
        if self.panic_at == Some(stage) {
            panic!("injected stage fault");
        }
    }
}

impl Mixer {
    /// [`Mixer::process`] inside `catch_unwind`. On a panic the block is silent, the fault count
    /// goes up and the clock still advances, and the unit the panic hit in is isolated
    /// ([`Self::isolate`]) so the rest of the show plays on. Returns false on a fault.
    pub fn process_guarded(&mut self, out: &mut [f32], frames: usize) -> bool {
        let start = self.now;
        let block = self.blocks;
        self.blocks = self.blocks.wrapping_add(1);
        self.stage.enter(Stage::Idle);
        let ok = catch_unwind(AssertUnwindSafe(|| self.process(out, frames))).is_ok();
        if !ok {
            self.isolate(start, block);
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

    /// Charges the fault of block `block` (starting at sample `at`) to the stage it hit in:
    /// - a voice's render: the voice is dropped at [`FAULT_LIMIT`] faults within
    ///   [`FAULT_WINDOW`] (`VoiceFinished { Faulted }`);
    /// - a command to a voice: the command is gone (taken off the pending set before it was
    ///   applied) and the voice, in whatever state the command left it, is dropped at once; a
    ///   start that never reached the pool is reported finished the same way;
    /// - a patch: reset to silence; the master: reset to unity (the only value the control
    ///   thread gives it); a device output: nothing to reset. Reported
    ///   ([`Event::StageFaulted`]) for the control thread to re-send a patch's or the master's
    ///   settings; disabled at the limit (also published
    ///   in [`DisabledUnits`], so a lost report is found);
    /// - the meters: reported; disabled at the limit;
    /// - none of these: at the limit, every patch and the master are rebuilt
    ///   ([`FaultStage::Mixer`]).
    ///
    /// Real-time safe: resets reuse the mixer's buffers and events go on the preallocated queue.
    fn isolate(&mut self, at: SampleTime, block: u64) {
        let (stage, disabled) = match std::mem::replace(&mut self.stage.now, Stage::Idle) {
            Stage::Voice(i) => {
                let Some(v) = self.voices.get_mut(i).filter(|v| v.is_active()) else {
                    return;
                };
                if v.faults.note(v.id, block) {
                    v.finish(at, FinishReason::Faulted, &mut self.outbox);
                    self.count_faulted_voice();
                }
                return;
            }
            Stage::VoiceCommand { voice, alt, start } => {
                let Mixer { voices, outbox, .. } = self;
                let ours = |v: &Voice| v.id == voice || v.id == alt;
                let known = voices.iter().any(ours);
                match voices.iter_mut().find(|v| v.is_active() && ours(v)) {
                    Some(v) => v.finish(at, FinishReason::Faulted, outbox),
                    // The start's box was dropped by the unwind, or the panic hit inside
                    // `finish` after the voice left the pool and before it was reported: the
                    // cue must still learn the voice is gone. (A slot keeps its last id until
                    // another voice starts in it; ids are never reused.)
                    None if start || known => outbox.emit(Event::VoiceFinished {
                        voice,
                        at,
                        reason: FinishReason::Faulted,
                    }),
                    None => return,
                }
                self.count_faulted_voice();
                return;
            }
            Stage::Patch(k) => {
                let Some(p) = self.patches.get_mut(k) else {
                    return;
                };
                p.reset();
                let stage = FaultStage::Patch(u16::try_from(k).unwrap_or(u16::MAX));
                (stage, p.health.fault(block))
            }
            Stage::Output(o) => {
                let Some(h) = self.out_health.get_mut(o) else {
                    return;
                };
                let stage = FaultStage::Output(u16::try_from(o).unwrap_or(u16::MAX));
                (stage, h.fault(block))
            }
            Stage::Master => {
                self.master.set(1.0);
                (FaultStage::Master, self.master_health.fault(block))
            }
            Stage::Meters => (FaultStage::Meters, self.meter_health.fault(block)),
            Stage::Idle => {
                if !self.unblamed.note(block) {
                    return;
                }
                self.rebuild();
                (FaultStage::Mixer, false)
            }
        };
        let shared = &self.outbox.shared;
        if disabled {
            shared.disabled.note(stage);
        }
        shared.faulted_stages.fetch_add(1, Ordering::Relaxed);
        self.outbox.emit(Event::StageFaulted {
            stage,
            at,
            disabled,
        });
    }

    fn count_faulted_voice(&self) {
        self.outbox
            .shared
            .faulted_voices
            .fetch_add(1, Ordering::Relaxed);
    }

    /// The last resort for faults no unit can be blamed for: every patch back to silence and the
    /// master to unity, every unit enabled, to be set again from the control thread's copy. Voices and pending commands are kept (they are the show; a
    /// fault in one is blamed on it).
    fn rebuild(&mut self) {
        self.outbox.shared.disabled.clear();
        for p in &mut self.patches {
            p.reset();
            p.health = Health::OK;
        }
        self.out_health.fill(Health::OK);
        self.master.set(1.0);
        self.master_health = Health::OK;
        self.meter_health = Health::OK;
        self.dev.fill(0.0);
        self.unblamed = FaultWindow::EMPTY;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OfflineRenderer;
    use crate::controller::MixerController;
    use crate::gain::Fade;
    use crate::mixer::{GainTarget, MixerConfig, VoiceStart};
    use rtrb::RingBuffer;

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
        let (mut ctl, mut r) = two_voices();
        r.mixer_mut().panic_voice = Some(VoiceId(1));
        let mut buf = vec![1.0; 128];
        for _ in 0..FAULT_LIMIT {
            assert!(!r.mixer_mut().process_guarded(&mut buf, 64));
        }
        assert_eq!(ctl.faults(), FAULT_LIMIT as u64);
        assert_eq!(ctl.faulted_voices(), 1);
        let mut ev = Vec::new();
        while let Some(e) = ctl.poll_event() {
            ev.push(e);
        }
        assert!(ev.contains(&Event::VoiceFinished {
            voice: VoiceId(1),
            at: 64 * FAULT_LIMIT as u64,
            reason: FinishReason::Faulted,
        }));
        // Voice 2 plays on; voice 1's output stays silent.
        for _ in 0..4 {
            assert!(r.mixer_mut().process_guarded(&mut buf, 64));
            assert!(buf.chunks(2).all(|f| f == [0.0, 0.5]));
        }
        assert_eq!(ctl.faults(), FAULT_LIMIT as u64);
    }

    /// Two voices: voice 1 on output 0, voice 2 on output 1, both 0.5, rendered for one block.
    fn two_voices() -> (MixerController, OfflineRenderer) {
        two_voices_on([0, 0])
    }

    /// [`two_voices`] through patches `patches` (every patch starts as identity).
    fn two_voices_on(patches: [u16; 2]) -> (MixerController, OfflineRenderer) {
        let (mut ctl, mixer) = Mixer::new(MixerConfig::default()).unwrap();
        for ((id, out), patch) in [(1, 0), (2, 1)].into_iter().zip(patches) {
            let (mut tx, rx) = RingBuffer::new(8192);
            tx.push_entire_slice(&[0.5; 8192]).unwrap();
            let mut v = VoiceStart::new(VoiceId(id), rx, 1, 2);
            v.set_crosspoint(0, out, 1.0);
            v.patch = patch;
            ctl.send(0, CommandKind::StartVoice(Box::new(v)));
        }
        let mut r = OfflineRenderer::new(mixer);
        r.render(64, &[]);
        (ctl, r)
    }

    fn faulted_events(ctl: &mut MixerController) -> Vec<VoiceId> {
        let mut out = Vec::new();
        while let Some(e) = ctl.poll_event() {
            if let Event::VoiceFinished {
                voice,
                reason: FinishReason::Faulted,
                ..
            } = e
            {
                out.push(voice);
            }
        }
        out
    }

    #[test]
    fn a_voice_that_faults_every_other_block_is_dropped() {
        let (mut ctl, mut r) = two_voices();
        let mut buf = vec![1.0; 128];
        for k in 0..2 * FAULT_LIMIT {
            let fault = k % 2 == 0;
            r.mixer_mut().panic_voice = fault.then_some(VoiceId(1));
            assert_eq!(r.mixer_mut().process_guarded(&mut buf, 64), !fault);
        }
        assert_eq!(faulted_events(&mut ctl), [VoiceId(1)]);
        assert_eq!(ctl.faults(), FAULT_LIMIT as u64);
        r.mixer_mut().panic_voice = Some(VoiceId(1));
        for _ in 0..4 {
            assert!(r.mixer_mut().process_guarded(&mut buf, 64));
            assert!(buf.chunks(2).all(|f| f == [0.0, 0.5]));
        }
    }

    #[test]
    fn a_voice_that_faults_once_or_rarely_keeps_playing() {
        let (mut ctl, mut r) = two_voices();
        let mut buf = vec![1.0; 128];
        // One fault, then faults further apart than the window: never dropped.
        let gap = usize::try_from(FAULT_WINDOW).unwrap();
        for _ in 0..FAULT_LIMIT + 1 {
            r.mixer_mut().panic_voice = Some(VoiceId(1));
            assert!(!r.mixer_mut().process_guarded(&mut buf, 64));
            r.mixer_mut().panic_voice = None;
            for _ in 0..gap {
                assert!(r.mixer_mut().process_guarded(&mut buf, 64));
                assert!(buf.chunks(2).all(|f| f == [0.5, 0.5]));
            }
        }
        assert_eq!(faulted_events(&mut ctl), []);
        assert_eq!(ctl.faulted_voices(), 0);
    }

    fn stage_events(ctl: &mut MixerController) -> Vec<(FaultStage, bool)> {
        let mut out = Vec::new();
        while let Some(e) = ctl.poll_event() {
            if let Event::StageFaulted {
                stage, disabled, ..
            } = e
            {
                out.push((stage, disabled));
            }
        }
        out
    }

    /// Renders `n` blocks of 64 frames, none of which may fault; returns the last frame.
    fn play(r: &mut OfflineRenderer, n: usize) -> [f32; 2] {
        let mut buf = [0.0; 128];
        for _ in 0..n {
            assert!(r.mixer_mut().process_guarded(&mut buf, 64));
        }
        [buf[126], buf[127]]
    }

    fn fault(r: &mut OfflineRenderer) {
        let mut buf = [1.0; 128];
        assert!(!r.mixer_mut().process_guarded(&mut buf, 64));
        assert!(buf.iter().all(|s| *s == 0.0));
    }

    fn route(ctl: &mut MixerController, patch: u16, out: u16, gain: f32) {
        let at = ctl.samples_rendered();
        let kind = CommandKind::SetPatchRouting {
            patch,
            cue_output: out,
            device_output: out,
            gain,
        };
        ctl.send(at, kind);
    }

    #[test]
    fn a_command_that_panics_on_apply_is_dropped_and_the_show_plays_on() {
        let (mut ctl, mut r) = two_voices();
        // Only this command panics: were it applied again, it would fault again.
        r.mixer_mut().panic_apply =
            Some(|k| matches!(k, CommandKind::SetMaster { gain } if *gain == 0.25));
        ctl.send(
            ctl.samples_rendered(),
            CommandKind::SetMaster { gain: 0.25 },
        );
        fault(&mut r);
        assert_eq!(stage_events(&mut ctl), [(FaultStage::Master, false)]);
        // Regression (#315): the master is reset to unity, its only legitimate value, so the
        // show plays from the next block without waiting for the control thread.
        assert_eq!(play(&mut r, 1), [0.5, 0.5]);
        assert_eq!(play(&mut r, 8), [0.5, 0.5]);
        assert_eq!(ctl.faults(), 1);
        assert_eq!(ctl.faulted_stages(), 1);
        assert_eq!(ctl.faulted_voices(), 0);
    }

    #[test]
    fn a_command_that_panics_on_a_voice_drops_that_voice_only() {
        let (mut ctl, mut r) = two_voices();
        r.mixer_mut().panic_apply = Some(|k| {
            matches!(k, CommandKind::Ramp { voice, .. } if *voice == VoiceId(1))
                || matches!(k, CommandKind::StartVoice(s) if s.voice == VoiceId(3))
        });
        let ramp = CommandKind::Ramp {
            voice: VoiceId(1),
            target: GainTarget::Master,
            gain: 0.5,
            fade: Fade::NONE,
        };
        ctl.send(ctl.samples_rendered(), ramp);
        fault(&mut r);
        assert_eq!(faulted_events(&mut ctl), [VoiceId(1)]);
        assert_eq!(play(&mut r, 4), [0.0, 0.5]);
        // A start that panics never reaches the pool: it is still reported, so its cue breaks.
        let (_tx, rx) = RingBuffer::new(16);
        let start = VoiceStart::new(VoiceId(3), rx, 1, 2);
        ctl.send(
            ctl.samples_rendered(),
            CommandKind::StartVoice(Box::new(start)),
        );
        fault(&mut r);
        assert_eq!(faulted_events(&mut ctl), [VoiceId(3)]);
        assert_eq!(play(&mut r, 4), [0.0, 0.5]);
        assert_eq!(ctl.faulted_voices(), 2);
        assert_eq!(ctl.faulted_stages(), 0);
    }

    /// Regression (#319): a panic inside `finish`, after the voice left the pool and before it
    /// was reported, still reports it, so its cue does not run for ever.
    #[test]
    fn a_voice_command_that_panics_after_the_voice_left_the_pool_still_reports_it() {
        let (mut ctl, mut r) = two_voices();
        let m = r.mixer_mut();
        let start = m.voices[0].start.take().unwrap();
        m.outbox.give_back(start);
        m.stage.now = Stage::VoiceCommand {
            voice: VoiceId(1),
            alt: VoiceId(1),
            start: false,
        };
        m.isolate(64, 1);
        assert_eq!(faulted_events(&mut ctl), [VoiceId(1)]);
        // A command for a voice the pool never had reports nothing.
        m.stage.now = Stage::VoiceCommand {
            voice: VoiceId(7),
            alt: VoiceId(7),
            start: false,
        };
        m.isolate(64, 2);
        assert_eq!(faulted_events(&mut ctl), []);
    }

    #[test]
    fn a_patch_fault_silences_that_patch_until_its_routing_is_sent_again() {
        let (mut ctl, mut r) = two_voices_on([0, 1]);
        r.mixer_mut().stage.panic_at = Some(Stage::Patch(0));
        fault(&mut r);
        r.mixer_mut().stage.panic_at = None;
        assert_eq!(stage_events(&mut ctl), [(FaultStage::Patch(0), false)]);
        assert_eq!(play(&mut r, 1), [0.0, 0.5]);
        route(&mut ctl, 0, 0, 1.0);
        assert_eq!(play(&mut r, 8), [0.5, 0.5]);
    }

    #[test]
    fn a_patch_whose_mix_panics_every_block_is_disabled_and_the_other_patches_play_on() {
        let (mut ctl, mut r) = two_voices_on([0, 1]);
        r.mixer_mut().stage.panic_at = Some(Stage::Patch(0));
        for _ in 0..FAULT_LIMIT {
            fault(&mut r);
            // The control thread re-sends the routing after each report.
            route(&mut ctl, 0, 0, 1.0);
        }
        let p0 = FaultStage::Patch(0);
        assert_eq!(
            stage_events(&mut ctl),
            [(p0, false), (p0, false), (p0, true)]
        );
        // Also published, for a report lost to a full event queue (#316).
        assert_eq!(
            ctl.disabled_units().stages().collect::<Vec<_>>(),
            [FaultStage::Patch(0)]
        );
        // Disabled: no longer mixed (so no longer faulting), its routing ignored.
        route(&mut ctl, 0, 0, 1.0);
        assert_eq!(play(&mut r, 8), [0.0, 0.5]);
        assert_eq!(ctl.faults(), FAULT_LIMIT as u64);
        assert_eq!(ctl.faulted_stages(), FAULT_LIMIT as u64);
        assert_eq!(ctl.faulted_voices(), 0);
    }

    /// An output has no setting of its own: after a one-off fault it is reported and plays on
    /// at its level with nothing sent back.
    #[test]
    fn an_output_that_panics_once_plays_on_without_a_resend() {
        let (mut ctl, mut r) = two_voices();
        assert_eq!(play(&mut r, 8), [0.5, 0.5]);
        r.mixer_mut().stage.panic_at = Some(Stage::Output(0));
        fault(&mut r);
        r.mixer_mut().stage.panic_at = None;
        assert_eq!(stage_events(&mut ctl), [(FaultStage::Output(0), false)]);
        assert_eq!(play(&mut r, 8), [0.5, 0.5]);
    }

    #[test]
    fn an_output_master_or_meter_stage_that_keeps_panicking_is_disabled_and_the_rest_plays() {
        let cases = [
            (Stage::Output(0), FaultStage::Output(0), [0.0, 0.5]),
            // A disabled master is bypassed at unity.
            (Stage::Master, FaultStage::Master, [0.5, 0.5]),
            (Stage::Meters, FaultStage::Meters, [0.5, 0.5]),
        ];
        for (stage, reported, heard) in cases {
            let (mut ctl, mut r) = two_voices();
            r.mixer_mut().stage.panic_at = Some(stage);
            for _ in 0..FAULT_LIMIT {
                fault(&mut r);
            }
            assert_eq!(
                stage_events(&mut ctl),
                [(reported, false), (reported, false), (reported, true)]
            );
            // Its settings are ignored from here.
            if stage == Stage::Master {
                let at = ctl.samples_rendered();
                ctl.send(at, CommandKind::SetMaster { gain: 0.0 });
            }
            assert_eq!(play(&mut r, 8), heard, "{stage:?}");
        }
    }

    #[test]
    fn repeated_faults_no_unit_is_blamed_for_rebuild_the_mixer_for_the_control_thread() {
        let (mut ctl, mut r) = two_voices_on([0, 1]);
        // A disabled patch comes back with the rebuild.
        r.mixer_mut().stage.panic_at = Some(Stage::Patch(1));
        for _ in 0..FAULT_LIMIT {
            fault(&mut r);
        }
        r.mixer_mut().stage.panic_at = None;
        assert_eq!(
            stage_events(&mut ctl).last(),
            Some(&(FaultStage::Patch(1), true))
        );
        for k in 1..=FAULT_LIMIT {
            r.mixer_mut().panic_next = true;
            fault(&mut r);
            let rebuilt = (k == FAULT_LIMIT).then_some((FaultStage::Mixer, false));
            assert_eq!(stage_events(&mut ctl), rebuilt.as_slice());
        }
        assert_eq!(ctl.disabled_units(), DisabledUnits::default());
        // Silent until the control thread sends its copy again; the voices play on meanwhile.
        assert_eq!(play(&mut r, 1), [0.0, 0.0]);
        for patch in 0..2 {
            for out in 0..2 {
                route(&mut ctl, patch, out, 1.0);
            }
        }
        let at = ctl.samples_rendered();
        ctl.send(at, CommandKind::SetMaster { gain: 1.0 });
        assert_eq!(play(&mut r, 8), [0.5, 0.5]);
        assert_eq!(ctl.faulted_stages(), FAULT_LIMIT as u64 + 1);
        assert_eq!(ctl.faulted_voices(), 0);
    }

    #[test]
    fn isolating_a_fault_does_not_allocate() {
        let (mut ctl, mut r) = two_voices_on([0, 1]);
        let start = |voice| Stage::VoiceCommand {
            voice,
            alt: voice,
            start: true,
        };
        let stages = [
            Stage::Voice(0),
            start(VoiceId(9)),
            start(VoiceId(1)),
            Stage::Patch(0),
            Stage::Output(1),
            Stage::Master,
            Stage::Meters,
            Stage::Idle,
            Stage::Idle,
            Stage::Idle,
        ];
        let m = r.mixer_mut();
        crate::no_alloc_for_tests(|| {
            for (block, stage) in (0..).zip(stages) {
                m.stage.now = stage;
                m.isolate(0, block);
            }
        });
        let mut buf = [0.0; 128];
        crate::no_alloc_for_tests(|| {
            for _ in 0..4 {
                assert!(m.process_guarded(&mut buf, 64));
            }
        });
        assert_eq!(ctl.faulted_voices(), 2);
        assert_eq!(
            stage_events(&mut ctl).last(),
            Some(&(FaultStage::Mixer, false))
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
}
