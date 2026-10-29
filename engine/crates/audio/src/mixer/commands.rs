// SPDX-License-Identifier: GPL-3.0-or-later

//! Commands in, events and returned voice starts out: the pending set ordered by sample time,
//! applying each command on its sample, and the outbox back to the controller.

use std::cmp::Ordering as CmpOrdering;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use rtrb::{Producer, PushError};

use crate::gain::{Curve, sanitize_gain};
use crate::plan::{MAX_SLICES, Plays};

use super::{
    Command, CommandKind, Event, FinishReason, Mixer, PENDING_CAPACITY, RejectReason, SampleTime,
    Shared, Voice, VoiceId, VoiceStart,
};

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
pub(super) struct Pending {
    pub(super) at: SampleTime,
    seq: u64,
    pub(super) cmd: Command,
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

impl Mixer {
    pub(super) fn pull_commands(&mut self) {
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

    pub(super) fn apply(&mut self, cmd: Command, t: SampleTime) {
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
                let known = to < self.patches.len();
                let Mixer { voices, outbox, .. } = self;
                if let Some(v) = voices.iter_mut().find(|v| v.is_active() && v.id == voice) {
                    v.moves = v.moves.wrapping_add(1);
                    if known {
                        v.move_to(to, dezip);
                    }
                    if v.moving.is_none() {
                        let moves = v.moves;
                        outbox.emit(Event::MoveEnded {
                            voice,
                            at: t,
                            moves,
                        });
                    }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtrb::RingBuffer;

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
