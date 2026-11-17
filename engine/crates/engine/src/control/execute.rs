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
        let key = match &answer {
            ProbeAnswer::Probed { key, .. } | ProbeAnswer::Pictured { key, .. } => {
                Some(key.clone())
            }
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
        let unresolved = self.doc.media.take_unresolved();
        self.doc.pending_media.extend(unresolved);
        if !self.doc.pending_media.is_empty() {
            let media = std::mem::take(&mut self.doc.pending_media);
            // Which of the files are movies with sound, first: their cues need their patch.
            let sound = media
                .iter()
                .map(|(file, _)| (file.clone(), self.doc.media.has_sound(file.as_str())))
                .collect();
            self.feed(Input::MediaSound { media: sound }, now);
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
                rates: self.eng.outputs.rates(),
                cue_outputs: self.eng.cue_outputs,
                now,
                window: self.eng.block,
                floor,
            };
            self.doc.voices.media_ready(&key, &mut env, &mut fx);
            self.follow(fx, now);
            self.release_pictures(&key, now);
        }
    }

    /// Swaps in the seeks whose streams are ready (each tick; see `Voices::start_seeks`).
    pub(super) fn start_seeks(&mut self, now: SampleTime, fx: &mut Effects) {
        let floor = self.stop_floor();
        let mut env = Env {
            doc: self.doc.id,
            ws: self.doc.model.ws(),
            media: &self.doc.media,
            streamer: &mut self.eng.streamer,
            patches: &self.eng.patches,
            ids: &mut self.eng.voice_ids,
            rate: self.eng.rate,
            rates: self.eng.outputs.rates(),
            cue_outputs: self.eng.cue_outputs,
            now,
            window: self.eng.block,
            floor,
        };
        self.doc.voices.start_seeks(&mut env, fx);
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
        // A Video cue's picture is a voice of the stage host's, not the mixer's.
        let pictures: Vec<bool> = actions.iter().map(|a| self.is_picture_action(a)).collect();
        // Sound starts of a call that starts a picture wait with it (one GO's sequence).
        let (hold, due) = self.hold_sounds(actions, &pictures);
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
                rates: self.eng.outputs.rates(),
                cue_outputs: self.eng.cue_outputs,
                now,
                window: self.eng.block,
                floor,
            };
            // Sounds held with an earlier sequence that this call acts on start first.
            for start in &due {
                self.doc.voices.apply(start, &mut env, &mut fx);
            }
            for ((action, picture), held) in actions.iter().zip(&pictures).zip(&hold) {
                if let Some(change) = RuntimeChange::from_action(action, self.doc.model.engine()) {
                    changes.push(change);
                }
                if matches!(action, Action::PlayheadChanged { .. }) {
                    self.doc.primes_dirty = true;
                }
                if !picture && !held {
                    self.doc.voices.apply(action, &mut env, &mut fx);
                }
            }
        }
        for action in actions {
            if let Action::RelocateVoice { run, from, .. } = *action {
                self.relocate_picture(run, from, &mut fx);
            }
        }
        self.begin_sequence(actions, &pictures);
        for (action, picture) in actions.iter().zip(&pictures) {
            if *picture {
                self.picture_action(action, &mut fx);
            }
        }
        let held: Vec<Action> = (actions.iter().zip(&hold))
            .filter(|(_, h)| **h)
            .map(|(a, _)| a.clone())
            .collect();
        self.end_sequence(held, &mut fx);
        self.end_pictures(actions);
        self.end_sounds(actions);
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
            warnings,
            late,
            pictured,
        } = fx;
        // Of a Video cue's voice only failures are passed on: its run's length, end and
        // devamp are its picture's to tell. Dropped here, where every report of the voices
        // passes, whichever way it was made (an event, a lost event made up for, a device
        // closed).
        let (mut feedback, mut deferred) = (feedback, deferred);
        feedback.retain(|i| !self.pictures_report(i));
        deferred.retain(|(_, i)| !self.pictures_report(i));
        for (at, input) in pictured {
            match at {
                Some(at) => deferred.push((at, input)),
                None => feedback.push(input),
            }
        }
        for (run, at, frames) in late {
            self.picture_late(run, at, frames, &mut feedback);
        }
        for message in warnings {
            self.log(LogLevel::Warn, message);
        }
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
                        patch: None,
                        code: code.to_owned(),
                        message,
                        cue,
                        device: None,
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
