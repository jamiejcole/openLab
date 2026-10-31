// SPDX-License-Identifier: GPL-3.0-or-later

//! Feeding a doc's cue engine and carrying out what it decides: inputs in (probe answers, media
//! status, deferred reports), actions out to the voices, the network and the model.

use std::collections::VecDeque;

use openlab_core::protocol::{LogLevel, RuntimeChange, WireWarning};
use openlab_core::{Action, CueId, Footprint, Input, SampleTime, Workspace};

use super::voices::{Effects, Env};
use super::{DocCx, FEEDBACK_LIMIT, ProbeAnswer};

impl DocCx<'_> {
    pub(super) fn probe_answer(&mut self, answer: ProbeAnswer) {
        let key = match answer {
            ProbeAnswer::Waveform {
                key,
                ticket,
                result,
            } => return self.doc.waveforms.answer(&key, ticket, result),
            ProbeAnswer::Probed { ref key, .. } => Some(key.clone()),
            ProbeAnswer::Preloaded { .. } => None,
        };
        if let ProbeAnswer::Preloaded { key, .. } = &answer {
            // A preload that lands changes what a stream of the file reads: primes built from
            // the file before it are rebuilt from memory, before the operator's GO needs them.
            let was = self.doc.media.is_preloaded(key);
            let key = key.clone();
            self.doc.media.answer(answer);
            if self.doc.media.is_preloaded(&key) != was {
                self.doc.primes_dirty = true;
            }
            return;
        }
        if let Some(status) = self.doc.media.answer(answer) {
            self.doc.pending_media.push(status);
            if let Some(key) = key {
                self.doc.ready_media.push(key);
            }
            self.doc.warnings_dirty = true;
            self.doc.primes_dirty = true;
        }
    }

    /// Hands the probe answers gathered since the last tick to the engine in one
    /// `Availability`, then starts the voices that waited for them.
    pub(super) fn flush_media(&mut self, now: SampleTime) {
        if !self.doc.pending_media.is_empty() {
            let media = std::mem::take(&mut self.doc.pending_media);
            self.feed(
                Input::Availability {
                    media,
                    patches: Vec::new(),
                },
                now,
            );
        }
        for key in std::mem::take(&mut self.doc.ready_media) {
            let mut fx = Effects::default();
            let floor = self.stop_floor();
            let mut env = Env {
                doc: self.doc.id,
                ws: self.doc.model.ws(),
                media: &self.doc.media,
                streamer: &mut self.eng.streamer,
                patches: &self.eng.patches,
                ids: &mut self.eng.voice_ids,
                rate: self.eng.rate,
                cue_outputs: self.eng.cue_outputs,
                now,
                floor,
            };
            self.doc.voices.media_ready(&key, &mut env, &mut fx);
            self.follow(fx, now);
        }
    }

    pub(super) fn deliver_deferred(&mut self, now: SampleTime) {
        if self.doc.deferred.is_empty() {
            return;
        }
        let (due, wait): (Vec<_>, Vec<_>) = std::mem::take(&mut self.doc.deferred)
            .into_iter()
            .partition(|(at, _)| *at < now);
        self.doc.deferred = wait;
        for (_, input) in due {
            self.feed(input, now);
        }
    }

    /// Hands `input` to the engine and carries out what it says, then the feedback that causes.
    pub(super) fn feed(&mut self, input: Input, now: SampleTime) {
        let mut queue = VecDeque::from([input]);
        let mut rounds = 0;
        while let Some(input) = queue.pop_front() {
            rounds += 1;
            if rounds > FEEDBACK_LIMIT {
                self.eng.seen.feedback_overflow += 1;
                self.log(
                    LogLevel::Error,
                    format!("feedback loop cut after {FEEDBACK_LIMIT} rounds"),
                );
                break;
            }
            let actions = self.doc.model.handle(input, now);
            let unsignalled = self.doc.model.engine().unsignalled_model_changes();
            if unsignalled > self.doc.seen_unsignalled {
                self.doc.seen_unsignalled = unsignalled;
                self.log(
                    LogLevel::Error,
                    format!("cue engine saw {unsignalled} model change(s) nobody signalled"),
                );
            }
            queue.extend(self.execute(&actions, now));
        }
    }

    /// Carries out one `handle` call's actions: runtime changes to the clients, `SetArmed` to the
    /// model, voice actions to the mixer and streams. Returns feedback to hand back at once.
    pub(super) fn execute(&mut self, actions: &[Action], now: SampleTime) -> Vec<Input> {
        let mut changes = Vec::new();
        let mut armed = Vec::new();
        let mut fx = Effects::default();
        {
            let floor = self.stop_floor();
            let mut env = Env {
                doc: self.doc.id,
                ws: self.doc.model.ws(),
                media: &self.doc.media,
                streamer: &mut self.eng.streamer,
                patches: &self.eng.patches,
                ids: &mut self.eng.voice_ids,
                rate: self.eng.rate,
                cue_outputs: self.eng.cue_outputs,
                now,
                floor,
            };
            for action in actions {
                if let Some(change) = RuntimeChange::from_action(action, self.doc.model.engine()) {
                    changes.push(change);
                }
                if matches!(action, Action::PlayheadChanged { .. }) {
                    self.doc.primes_dirty = true;
                }
                self.doc.voices.apply(action, &mut env, &mut fx);
            }
        }
        for action in actions {
            match action {
                Action::SetArmed { cue, armed: value } => {
                    let mut edit = self.doc.model.edit();
                    let applied = set_armed(&mut edit, *cue, *value);
                    edit.touched(&Footprint::edited([*cue]));
                    if applied {
                        armed.push(*cue);
                    }
                }
                Action::SendOsc {
                    cue,
                    destination,
                    address,
                    args,
                    ..
                } => self.send_osc(*cue, *destination, address, args),
                _ => {}
            }
        }
        if !changes.is_empty() {
            self.observe_changes(&changes);
            self.runtime(changes);
        }
        let mut feedback = self.after(fx);
        if !armed.is_empty() {
            feedback.extend(self.model_changed(&Footprint::edited(armed)));
            self.document();
        }
        feedback
    }

    /// Sends the mixer commands and notices of `fx`; queues its deferred feedback; returns its
    /// immediate feedback (which `feed` hands to the engine).
    fn after(&mut self, fx: Effects) -> Vec<Input> {
        let Effects {
            commands,
            feedback,
            deferred,
            notices,
            reprobe,
        } = fx;
        self.send_commands(commands);
        self.doc.deferred.extend(deferred);
        for key in reprobe {
            self.doc.media.reprobe(&key);
        }
        if !notices.is_empty() {
            let changes = notices
                .into_iter()
                .map(|(code, message, cue)| RuntimeChange::Warning {
                    warning: WireWarning::Notice {
                        code: code.to_owned(),
                        message,
                        cue,
                    },
                })
                .collect::<Vec<_>>();
            self.observe_changes(&changes);
            self.runtime(changes);
        }
        feedback
    }

    /// [`Self::after`], then hands the feedback to the engine (outside a `feed`).
    pub(super) fn follow(&mut self, fx: Effects, now: SampleTime) {
        for input in self.after(fx) {
            self.feed(input, now);
        }
    }
}

/// Applies an Arm/Disarm cue's change to the model, as a runtime change (no undo step).
fn set_armed(ws: &mut Workspace, cue: CueId, armed: bool) -> bool {
    ws.find_cue_mut(cue)
        .is_some_and(|c| std::mem::replace(&mut c.armed, armed) != armed)
}
