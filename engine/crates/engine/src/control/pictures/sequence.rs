// SPDX-License-Identifier: GPL-3.0-or-later

//! One GO's sequence (QLab 5 manual p. 680: a sequence starts only once every cue in it is
//! ready): the cues one cue-engine call starts (a GO with the cues it auto-continues into at
//! once, a fire-all group's children) wait together while a Video cue among them has its picture
//! still opening, so pictures and sounds started together stay in step whichever were opened
//! ahead. Once every picture among them is open they all start, each as far after the sample
//! it was dated as the first was held, so their spacing is kept and each one's end moves by
//! the hold (`ActionLength` counts it, for a sound as for a picture).
//!
//! A sound held so is not in the mixer yet. Any later action on its run (a stop, a pause, a
//! level change) starts it first, so that action finds it as it would have.

use openlab_core::{Action, CueId, RunId, SampleTime};

use super::super::voices::{Effects, Env};
use super::{DocCx, DocId, Engine, Wait};

/// A sound start held with a sequence.
struct HeldSound {
    doc: DocId,
    group: u64,
    /// Its `StartVoice`.
    start: Action,
}

#[derive(Default)]
pub(in crate::control) struct Sequences {
    /// The sequence of the call being carried out, while it starts a picture.
    batch: Option<u64>,
    next: u64,
    sounds: Vec<HeldSound>,
}

impl Sequences {
    pub(super) fn batch(&self) -> Option<u64> {
        self.batch
    }
}

/// The run a voice action is about, for a sound held with a sequence.
fn run_of(action: &Action) -> Option<RunId> {
    match action {
        Action::LoadVoice { run, .. }
        | Action::LoadVoiceAt { run, .. }
        | Action::UnloadVoice { run, .. }
        | Action::StartVoice { run, .. }
        | Action::StopVoice { run, .. }
        | Action::PauseVoice { run, .. }
        | Action::Ramp { run, .. }
        | Action::DuckVoice { run, .. }
        | Action::DevampVoice { run, .. } => Some(*run),
        Action::SeekVoice { from, .. } | Action::RelocateVoice { from, .. } => Some(*from),
        Action::ResumeVoice { paused, .. } => Some(*paused),
        _ => None,
    }
}

fn start_at(action: &Action) -> Option<SampleTime> {
    match action {
        Action::StartVoice { at, .. } => Some(*at),
        _ => None,
    }
}

impl Engine {
    /// A doc closed: the sounds it held go.
    pub(super) fn drop_doc_sequences(&mut self, doc: DocId) {
        self.pictures.sequence.sounds.retain(|h| h.doc != doc);
    }
}

impl DocCx<'_> {
    /// Before one call's voice actions (`pictures` marks a Video cue's): which of them are
    /// sound starts to hold with the sequence (the call starts a picture, and the start is all
    /// it does to that run, besides a load), and the sounds held earlier that this call acts on,
    /// to start first.
    pub(in crate::control) fn hold_sounds(
        &mut self,
        actions: &[Action],
        pictures: &[bool],
    ) -> (Vec<bool>, Vec<Action>) {
        let doc = self.doc.id;
        let sounds = &mut self.eng.pictures.sequence.sounds;
        let acted = |run: RunId| {
            (actions.iter().zip(pictures)).any(|(a, p)| {
                !p && run_of(a) == Some(run) && !matches!(a, Action::StartVoice { .. })
            })
        };
        let mut due = Vec::new();
        let mut i = 0;
        while i < sounds.len() {
            let ours = sounds
                .get(i)
                .is_some_and(|h| h.doc == doc && run_of(&h.start).is_some_and(acted));
            if ours {
                due.push(sounds.swap_remove(i).start);
            } else {
                i += 1;
            }
        }
        let starts_picture = (actions.iter().zip(pictures))
            .any(|(a, p)| *p && matches!(a, Action::StartVoice { .. }));
        let hold = (actions.iter().zip(pictures))
            .map(|(a, p)| {
                starts_picture
                    && !p
                    && matches!(a, Action::StartVoice { .. })
                    && run_of(a).is_some_and(|run| {
                        !(actions.iter().zip(pictures)).any(|(b, q)| {
                            !q && run_of(b) == Some(run)
                                && !matches!(
                                    b,
                                    Action::StartVoice { .. } | Action::LoadVoice { .. }
                                )
                        })
                    })
            })
            .collect();
        (hold, due)
    }

    /// Before one call's picture actions: a call that starts a picture is a sequence.
    pub(in crate::control) fn begin_sequence(&mut self, actions: &[Action], pictures: &[bool]) {
        let starts = (actions.iter().zip(pictures))
            .any(|(a, p)| *p && matches!(a, Action::StartVoice { .. }));
        let seq = &mut self.eng.pictures.sequence;
        seq.batch = starts.then(|| {
            seq.next += 1;
            seq.next
        });
    }

    /// After one call's picture actions: the sound starts `held` wait with its sequence, and
    /// the sequence starts now if every picture in it is open.
    pub(in crate::control) fn end_sequence(&mut self, held: Vec<Action>, fx: &mut Effects) {
        let doc = self.doc.id;
        let seq = &mut self.eng.pictures.sequence;
        let Some(group) = seq.batch.take() else {
            for start in held {
                self.start_sound(&start, None, fx);
            }
            return;
        };
        seq.sounds.extend(
            held.into_iter()
                .map(|start| HeldSound { doc, group, start }),
        );
        self.start_sequence_if_open(group, fx);
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
        let seq = &mut self.eng.pictures.sequence;
        let mut sounds = Vec::new();
        let mut i = 0;
        while i < seq.sounds.len() {
            if seq
                .sounds
                .get(i)
                .is_some_and(|h| h.doc == doc && h.group == group)
            {
                sounds.push(seq.sounds.swap_remove(i).start);
            } else {
                i += 1;
            }
        }
        let first = (members.iter().map(|(_, w, ..)| w.at))
            .chain(sounds.iter().filter_map(start_at))
            .min();
        let Some(first) = first else {
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
        for start in sounds {
            let on = start_at(&start).map(|at| at.saturating_add(shift));
            self.start_sound(&start, on, fx);
        }
        true
    }

    /// Each tick: a sequence whose pictures have all gone (stopped, failed) starts its held
    /// sounds.
    pub(super) fn start_lone_sounds(&mut self, fx: &mut Effects) {
        let doc = self.doc.id;
        let mut groups: Vec<u64> = (self.eng.pictures.sequence.sounds.iter())
            .filter(|h| h.doc == doc)
            .map(|h| h.group)
            .collect();
        groups.dedup();
        for group in groups {
            let waits = (self.eng.pictures.live.values())
                .any(|l| l.doc == doc && l.wait.is_some_and(|w| w.group == Some(group)));
            if !waits {
                self.start_sequence_if_open(group, fx);
            }
        }
    }

    /// Hands a held sound start to the mixer's voices, as if carried out on sample `on` (or
    /// now): the voice starts there and counts the hold into its length.
    pub(in crate::control) fn start_sound(
        &mut self,
        start: &Action,
        on: Option<SampleTime>,
        fx: &mut Effects,
    ) {
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
            now: on.map_or(now, |on| on.max(now)),
            window: self.eng.block,
            floor,
        };
        self.doc.voices.apply(start, &mut env, fx);
    }
}
