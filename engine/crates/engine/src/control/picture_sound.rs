// SPDX-License-Identifier: GPL-3.0-or-later

//! A Video cue's sound (VID-06): its movie's audio track plays as a mixer voice of the run, made,
//! routed, levelled, faded, paused, devamped and stopped by the same code as an Audio cue's
//! voice.
//!
//! The run's picture decides when the run starts, where it is and how long it lasts (its plan,
//! a start held by its probe, hold at end), so each voice action is passed on from where the
//! picture carries it out, dated as the picture is: both follow one slice table on one sample
//! clock. Of what the voice reports back, only failures reach the cue engine; its length, end
//! and devamp answers are the picture's to give, once.
//!
//! The sound is the clock: a voice the disk starved plays on late by what it missed, and its
//! picture is moved back by as much, so the two stay together.

use openlab_audio::{CommandKind, Fade, MediaInfo};
use openlab_core::{Action, CueId, CueKind, CueState, Input, PatchId};

use super::DocCx;
use super::voices::{Effects, Env};

/// Feedback about a run's length, end or devamp: for a Video cue's run, its picture's to give.
fn lifecycle(input: &Input) -> Option<CueId> {
    match input {
        Input::ActionLength { cue, .. }
        | Input::ActionEnded { cue, .. }
        | Input::DevampScheduled { cue, .. }
        | Input::DevampIgnored { cue, .. } => Some(*cue),
        _ => None,
    }
}

impl DocCx<'_> {
    fn video_kind(&self, cue: CueId) -> Option<&CueKind> {
        let ws = self.doc.model.ws();
        let index = self.doc.media.index();
        if !index.contains(cue) {
            return None;
        }
        (index.locate(ws, cue).map(|c| &c.kind)).filter(|k| matches!(k, CueKind::Video { .. }))
    }

    /// Whether `cue` is a Video cue.
    pub(super) fn is_video(&self, cue: CueId) -> bool {
        self.video_kind(cue).is_some()
    }

    /// The probe of the audio track a Video cue's voice plays (fitted to its movie's length),
    /// if `cue` is a Video cue whose probed movie has sound: its picture's plan is built on it,
    /// so picture and sound follow the same table to the sample.
    pub(super) fn sound_info(&self, cue: CueId) -> Option<MediaInfo> {
        let Some(CueKind::Video {
            media: Some(media),
            sound,
            ..
        }) = self.video_kind(cue)
        else {
            return None;
        };
        let (ready, _) = self.doc.media.sound(media.as_str(), sound.audio_track)?;
        Some(ready.probed.info)
    }

    /// The output patch `cue` sounds on: an Audio cue's, or that of a Video cue whose probed
    /// movie has sound. A Video cue without sound is on no patch and no device.
    pub(super) fn cue_patch(&self, cue: CueId) -> Option<PatchId> {
        let kind = &self.doc.model.ws().find_cue(cue)?.kind;
        match kind {
            CueKind::Audio { patch, .. } => Some(*patch),
            CueKind::Video { sound, .. } => sound.patch.filter(|_| self.sound_info(cue).is_some()),
            _ => None,
        }
    }

    /// Carries out a voice action of a Video cue's run on its sound, if its movie has any.
    pub(super) fn sound_action(&mut self, action: &Action, fx: &mut Effects) {
        let cue = match action {
            Action::LoadVoice { cue, .. }
            | Action::UnloadVoice { cue, .. }
            | Action::StartVoice { cue, .. }
            | Action::StopVoice { cue, .. }
            | Action::PauseVoice { cue, .. }
            | Action::ResumeVoice { cue, .. }
            | Action::Ramp { cue, .. }
            | Action::DuckVoice { cue, .. }
            | Action::DevampVoice { cue, .. } => *cue,
            _ => return,
        };
        // A voice is made only for a movie with sound; one already there is seen through
        // whatever its cue was edited into since.
        let makes = matches!(
            action,
            Action::LoadVoice { .. } | Action::StartVoice { .. }
        );
        if makes && self.sound_info(cue).is_none() {
            return;
        }
        let mut own = Effects::default();
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
            cue_outputs: self.eng.cue_outputs,
            now,
            window: self.eng.block,
            floor,
        };
        self.doc.voices.apply(action, &mut env, &mut own);
        own.feedback.retain(|i| lifecycle(i).is_none());
        own.deferred.retain(|(_, i)| lifecycle(i).is_none());
        fx.commands.append(&mut own.commands);
        fx.feedback.append(&mut own.feedback);
        fx.deferred.append(&mut own.deferred);
        fx.notices.append(&mut own.notices);
        fx.reprobe.append(&mut own.reprobe);
        fx.warnings.append(&mut own.warnings);
        fx.late.append(&mut own.late);
    }

    /// Drops what the voices reported about the length, end or devamp of Video cues' runs
    /// (`fx` holds only the voices' reports): their pictures report those.
    pub(super) fn quiet_sound(&self, fx: &mut Effects) {
        fx.feedback
            .retain(|i| !lifecycle(i).is_some_and(|cue| self.is_video(cue)));
        fx.deferred
            .retain(|(_, i)| !lifecycle(i).is_some_and(|cue| self.is_video(cue)));
    }

    /// The voices of Video cues whose action is over (`StateChanged` to idle, post-wait or
    /// broken: it ended, stopped, broke or was reset) are cut on that sample, as their pictures
    /// go: a picture that failed takes its sound with it. A voice that ended by itself, or is
    /// already stopping to that sample, is not there to cut. A run the cue started since is
    /// left alone.
    pub(super) fn end_sounds(&mut self, actions: &[Action]) {
        let mut commands = Vec::new();
        for action in actions {
            let Action::StateChanged { cue, state, at } = *action else {
                continue;
            };
            if !matches!(
                state,
                CueState::Idle | CueState::PostWaiting | CueState::Broken
            ) || !self.is_video(cue)
            {
                continue;
            }
            let current = self.doc.model.engine().run_of(cue);
            for (voice, of, run) in self.doc.voices.sounding() {
                if of == cue && Some(run) != current {
                    commands.push((
                        at,
                        CommandKind::StopVoice {
                            voice,
                            fade: Fade::NONE,
                        },
                    ));
                }
            }
        }
        if !commands.is_empty() {
            self.send_commands(commands);
        }
    }
}
