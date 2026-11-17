// SPDX-License-Identifier: GPL-3.0-or-later

//! One GO's sequence (QLab 5 manual p. 680: a sequence starts only once every cue in it is
//! ready): the cues one cue-engine call starts (a GO with the cues it auto-continues into at
//! once, a fire-all group's children) wait together while a Video cue among them has its picture
//! still opening, so pictures and sounds started together stay in step whichever were opened
//! ahead. Once every picture among them is open they all start, each as far after the sample
//! it was dated as the first was held, so their spacing is kept and each one's end moves by
//! the hold (`ActionLength` counts it, for a sound as for a picture).
//!
//! A sound held so is a start the mixer's voices keep waiting (`Voices::apply_gated`): a stop,
//! panic, pause, resume or level change on it is carried out on that waiting start as on any
//! (a stop ends it with nothing heard, a pause keeps it from starting until its resume, a level
//! change is queued for its start), and the sequence lets it go (`Voices::open_gate`).

use openlab_core::{Action, CueId, RunId, SampleTime};

use super::super::voices::{Effects, Env};
use super::{DocCx, Wait};

#[derive(Default)]
pub(in crate::control) struct Sequences {
    /// The sequence of the call being carried out, while it starts a picture.
    batch: Option<u64>,
    next: u64,
}

impl Sequences {
    pub(super) fn batch(&self) -> Option<u64> {
        self.batch
    }
}

impl DocCx<'_> {
    /// Before one call's voice actions (`pictures` marks a Video cue's): a call that starts a
    /// picture is a sequence, and its sound starts (marked in the second value) are held with
    /// it. Returns the sequence.
    pub(in crate::control) fn begin_sequence(
        &mut self,
        actions: &[Action],
        pictures: &[bool],
    ) -> (Option<u64>, Vec<bool>) {
        let starts = (actions.iter().zip(pictures))
            .any(|(a, p)| *p && matches!(a, Action::StartVoice { .. }));
        let seq = &mut self.eng.pictures.sequence;
        seq.batch = starts.then(|| {
            seq.next += 1;
            seq.next
        });
        let hold = (actions.iter().zip(pictures))
            .map(|(a, p)| starts && !p && matches!(a, Action::StartVoice { .. }))
            .collect();
        (seq.batch, hold)
    }

    /// After one call's picture actions: its sequence starts now if every picture in it is
    /// open.
    pub(in crate::control) fn end_sequence(&mut self, fx: &mut Effects) {
        if let Some(group) = self.eng.pictures.sequence.batch.take() {
            self.start_sequence_if_open(group, fx);
        }
    }

    /// Starts sequence `group` if every picture in it waiting (not paused) is open, or nothing
    /// would open it (the stage host down; a picture held back after crashes plays unseen):
    /// each of its pictures and held sounds as long after its dated sample as the earliest was
    /// held. `false` while one still opens.
    pub(super) fn start_sequence_if_open(&mut self, group: u64, fx: &mut Effects) -> bool {
        let doc = self.doc.id;
        let opens = self.eng.opens_pictures();
        let mut members: Vec<(u64, Wait, CueId, RunId)> = Vec::new();
        for (id, l) in &self.eng.pictures.live {
            let Some(w) = l
                .wait
                .filter(|w| l.doc == doc && w.group == Some(group) && !w.paused)
            else {
                continue;
            };
            if !(l.opened || l.held_back || !opens) {
                return false;
            }
            members.push((*id, w, l.cue, l.run));
        }
        let first = (members.iter().map(|(_, w, ..)| w.at))
            .chain(self.doc.voices.gated(group))
            .min();
        let Some(first) = first else {
            // Only paused starts are left: they start on their resume.
            self.open_sounds(group, 0, fx);
            return false;
        };
        let shift = self.now().saturating_sub(first);
        for (id, w, cue, run) in members {
            if let Some(l) = self.eng.pictures.live.get_mut(&id) {
                l.wait = None;
            }
            let on = w.at.saturating_add(shift);
            self.play_from(cue, run, (w.at, w.offset), w.from, on, w.devamp, fx);
        }
        self.open_sounds(group, shift, fx);
        true
    }

    /// Each tick: a sequence whose pictures have all gone (stopped, failed, or paused) lets its
    /// held sounds go.
    pub(super) fn start_lone_sounds(&mut self, fx: &mut Effects) {
        let doc = self.doc.id;
        for group in self.doc.voices.gated_groups() {
            let waits = (self.eng.pictures.live.values()).any(|l| {
                l.doc == doc && l.wait.is_some_and(|w| w.group == Some(group) && !w.paused)
            });
            if !waits {
                self.start_sequence_if_open(group, fx);
            }
        }
    }

    /// Lets the sounds held with `group` start, each `shift` after its dated sample.
    fn open_sounds(&mut self, group: u64, shift: SampleTime, fx: &mut Effects) {
        let now = self.now();
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
        self.doc.voices.open_gate(group, shift, &mut env, fx);
    }
}
