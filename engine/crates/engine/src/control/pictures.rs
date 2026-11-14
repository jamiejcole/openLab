// SPDX-License-Identifier: GPL-3.0-or-later

//! Video cues' pictures (CT-03, VID-03): the cue engine runs a Video cue's picture as a voice,
//! and this carries its voice actions out on the stage host instead of the mixer. Each run's
//! picture gets an id of the engine's; the stage host is told to load, play, pause and stop it
//! on sample times of the engine's clock, which it is sent as it moves, so the picture follows
//! the audio clock (#641).
//!
//! The run's length is known from the probe (a video's length, a still's typed duration, none
//! for a still shown until stopped) and told to the cue engine when the picture starts
//! (`ActionLength`), so the cue ends on its last frame's sample. When the cue's action is over
//! (it ends, stops, breaks or is reset) its picture goes.
//!
//! A stage host that starts (again, after a crash) is sent every picture again, so a picture
//! comes back by itself. A picture the stage host cannot decode breaks only its own cue, with a
//! notice. The control thread never waits on the stage host.

use std::collections::BTreeMap;

use openlab_core::protocol::LogLevel;
use openlab_core::stage_link::ToStage;
use openlab_core::video_probe::ProbeReport;
use openlab_core::{
    Action, BrokenReason, CueId, CueKind, CueState, Input, RunId, SampleTime, StageId,
    seconds_to_samples,
};

use super::voices::Effects;
use super::{DocCx, DocId, Engine};

/// How far the engine's clock may move before the stage host is told it again (it moves the
/// picture on by its own clock in between).
const CLOCK_EVERY_MS: u64 = 10;

/// One run's picture.
struct Live {
    doc: DocId,
    cue: CueId,
    run: RunId,
    /// The `load` it was given, to send again to a new stage host.
    load: ToStage,
    /// Its last `play` or `pause`, likewise.
    now: Option<ToStage>,
    /// It is stopping: gone from the stage host after this sample.
    ends: Option<SampleTime>,
}

/// Every open doc's pictures, and the clock the stage host was last told.
#[derive(Default)]
pub(super) struct Pictures {
    next: u64,
    live: BTreeMap<u64, Live>,
    clock_sent: Option<SampleTime>,
    /// Pictures the stage host reported failed, for their doc's next tick.
    failed: Vec<(DocId, CueId, RunId, String)>,
}

impl Pictures {
    fn find(&self, doc: DocId, run: RunId) -> Option<u64> {
        self.live
            .iter()
            .find(|(_, l)| l.doc == doc && l.run == run)
            .map(|(id, _)| *id)
    }
}

/// What a Video cue plays: its file's probe and its stage.
struct Plays {
    path: String,
    still: bool,
    stage: StageId,
    /// The action's length in seconds, if it has one.
    length: Option<f64>,
}

impl Engine {
    /// Sends a picture message to the stage host, if one runs.
    fn picture(&self, message: ToStage) {
        if let Some(host) = self.stages_host() {
            host.picture(message);
        }
    }

    /// Tells the stage host the clock now, when it moved far enough since it was last told
    /// (or `force`) and any picture is up.
    pub(super) fn picture_clock(&mut self, force: bool) {
        if self.pictures.live.is_empty() {
            return;
        }
        let now = self.now();
        let every = u64::from(self.rate.get()) * CLOCK_EVERY_MS / 1000;
        if !force
            && self
                .pictures
                .clock_sent
                .is_some_and(|sent| now.saturating_sub(sent) < every.max(1))
        {
            return;
        }
        self.pictures.clock_sent = Some(now);
        self.picture(ToStage::Clock {
            sample: now,
            rate: self.rate.get(),
        });
    }

    /// A stage host started: it is sent the clock and every picture again.
    pub(super) fn resend_pictures(&mut self) {
        if self.pictures.live.is_empty() {
            return;
        }
        self.pictures.clock_sent = None;
        self.picture_clock(true);
        let messages: Vec<ToStage> = self
            .pictures
            .live
            .values()
            .flat_map(|l| std::iter::once(l.load.clone()).chain(l.now.clone()))
            .collect();
        for m in messages {
            self.picture(m);
        }
    }

    /// The stage host could not decode or draw `picture`: its cue breaks at its doc's next
    /// tick.
    pub(super) fn picture_failed(&mut self, picture: u64, reason: String) {
        if let Some(l) = self.pictures.live.remove(&picture) {
            self.pictures.failed.push((l.doc, l.cue, l.run, reason));
        }
    }

    /// Pictures whose stop is over at `now` are forgotten.
    pub(super) fn prune_pictures(&mut self, now: SampleTime) {
        self.pictures
            .live
            .retain(|_, l| l.ends.is_none_or(|end| end >= now));
    }

    /// A doc closed: its pictures go.
    pub(super) fn drop_doc_pictures(&mut self, doc: DocId) {
        let gone: Vec<u64> = self
            .pictures
            .live
            .iter()
            .filter(|(_, l)| l.doc == doc)
            .map(|(id, _)| *id)
            .collect();
        for picture in gone {
            self.pictures.live.remove(&picture);
            self.picture(ToStage::Unload { picture });
        }
    }

    /// Test support: the stage host is told the clock now, before it draws for a probe.
    #[cfg(feature = "test-support")]
    pub(super) fn picture_clock_now(&mut self) {
        let now = self.now();
        self.pictures.clock_sent = Some(now);
        self.picture(ToStage::Clock {
            sample: now,
            rate: self.rate.get(),
        });
    }

    /// Pictures up, every doc's (tests).
    pub(super) fn pictures_live(&self) -> usize {
        self.pictures.live.len()
    }
}

fn seconds(samples: SampleTime, rate: u32) -> f64 {
    samples as f64 / f64::from(rate.max(1))
}

impl DocCx<'_> {
    /// What `cue` plays, if it is a Video cue whose file has been probed and plays.
    fn plays(&self, cue: CueId) -> Result<Plays, &'static str> {
        let Some(CueKind::Video {
            media: Some(media),
            stage: Some(stage),
            duration,
        }) = self.doc.model.ws().find_cue(cue).map(|c| &c.kind)
        else {
            return Err("the cue has no file or stage");
        };
        let Some(ready) = self.doc.media.picture(media.as_str()) else {
            return Err("its file has not been read, or cannot be played");
        };
        let path = ready
            .path
            .to_str()
            .ok_or("its file's path is not valid Unicode")?
            .to_owned();
        Ok(match ready.report {
            ProbeReport::Video { duration: d, .. } => Plays {
                path,
                still: false,
                stage: *stage,
                length: Some(d),
            },
            ProbeReport::Still { .. } => Plays {
                path,
                still: true,
                stage: *stage,
                length: *duration,
            },
            ProbeReport::Missing | ProbeReport::Unsupported { .. } => {
                return Err("its file cannot be played");
            }
        })
    }

    /// Whether `action` is a voice action of a Video cue's run (carried out here, not by the
    /// mixer's voices).
    pub(super) fn is_picture_action(&self, action: &Action) -> bool {
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
            _ => return false,
        };
        // A run whose picture is up stays one even if its cue was edited into another kind.
        self.picture_of_cue(cue).is_some()
            || matches!(
                self.doc.model.ws().find_cue(cue).map(|c| &c.kind),
                Some(CueKind::Video { .. })
            )
    }

    fn picture_of_cue(&self, cue: CueId) -> Option<u64> {
        let doc = self.doc.id;
        self.eng
            .pictures
            .live
            .iter()
            .find(|(_, l)| l.doc == doc && l.cue == cue)
            .map(|(id, _)| *id)
    }

    /// Carries out a Video cue run's voice action on the stage host.
    pub(super) fn picture_action(&mut self, action: &Action, fx: &mut Effects) {
        let rate = self.eng.rate.get();
        let doc = self.doc.id;
        match *action {
            Action::LoadVoice { cue, run, offset } => {
                if let Err(why) = self.load_picture(cue, run, offset) {
                    self.picture_refused(cue, run, why, false, fx);
                }
            }
            Action::StartVoice {
                cue,
                run,
                at,
                offset,
            } => {
                let loaded = self.eng.pictures.find(doc, run).is_some();
                if !loaded && let Err(why) = self.load_picture(cue, run, offset) {
                    return self.picture_refused(cue, run, why, true, fx);
                }
                self.play_picture(cue, run, at, offset, fx);
            }
            Action::ResumeVoice {
                cue,
                run,
                paused,
                at,
                offset,
            } => {
                let Some(id) = self.eng.pictures.find(doc, paused) else {
                    return;
                };
                if let Some(l) = self.eng.pictures.live.get_mut(&id) {
                    l.run = run;
                }
                self.play_picture(cue, run, at, offset, fx);
            }
            Action::PauseVoice {
                run, at, offset, ..
            } => {
                if let Some(id) = self.eng.pictures.find(doc, run) {
                    let message = ToStage::Pause {
                        picture: id,
                        at,
                        offset: seconds(offset, rate),
                    };
                    if let Some(l) = self.eng.pictures.live.get_mut(&id) {
                        l.now = Some(message.clone());
                    }
                    self.eng.picture(message);
                }
            }
            Action::StopVoice {
                cue, run, at, fade, ..
            } => {
                let frames = fade.map_or(0, |f| f.frames);
                if let Some(id) = self.eng.pictures.find(doc, run) {
                    self.stop_picture(id, at, frames);
                }
                // The stop (its fade, if any) is over on this sample, as a mixer voice's is.
                let end = at.saturating_add(frames);
                fx.deferred
                    .push((end, Input::ActionEnded { cue, run, at: end }));
            }
            Action::UnloadVoice { run, .. } => {
                if let Some(id) = self.eng.pictures.find(doc, run) {
                    self.eng.pictures.live.remove(&id);
                    self.eng.picture(ToStage::Unload { picture: id });
                }
            }
            // A picture has no loop to leave.
            Action::DevampVoice { cue, run, .. } => {
                fx.feedback.push(Input::DevampIgnored { cue, run });
            }
            // Levels and ducks act on sound; a picture has none (yet: #645).
            _ => {}
        }
    }

    fn load_picture(
        &mut self,
        cue: CueId,
        run: RunId,
        offset: SampleTime,
    ) -> Result<(), &'static str> {
        let plays = self.plays(cue)?;
        let rate = self.eng.rate.get();
        self.eng.pictures.next += 1;
        let id = self.eng.pictures.next;
        let load = ToStage::Load {
            picture: id,
            stage: plays.stage,
            file: plays.path,
            still: plays.still,
            offset: seconds(offset, rate).min(openlab_core::MAX_TIME),
        };
        self.eng.pictures.live.insert(
            id,
            Live {
                doc: self.doc.id,
                cue,
                run,
                load: load.clone(),
                now: None,
                ends: None,
            },
        );
        self.eng.picture(load);
        Ok(())
    }

    fn play_picture(
        &mut self,
        cue: CueId,
        run: RunId,
        at: SampleTime,
        offset: SampleTime,
        fx: &mut Effects,
    ) {
        let doc = self.doc.id;
        let rate = self.eng.rate.get();
        let Some(id) = self.eng.pictures.find(doc, run) else {
            return;
        };
        let message = ToStage::Play {
            picture: id,
            at,
            offset: seconds(offset, rate).min(openlab_core::MAX_TIME),
        };
        if let Some(l) = self.eng.pictures.live.get_mut(&id) {
            l.now = Some(message.clone());
        }
        self.eng.picture_clock(true);
        self.eng.picture(message);
        // The action's length from its start: a video's own, a still's duration.
        if let Ok(Plays {
            length: Some(length),
            ..
        }) = self.plays(cue)
        {
            let frames = seconds_to_samples(length, self.eng.rate);
            fx.feedback.push(Input::ActionLength { cue, run, frames });
        }
    }

    fn stop_picture(&mut self, id: u64, at: SampleTime, frames: SampleTime) {
        let end = at.saturating_add(frames);
        if let Some(l) = self.eng.pictures.live.get_mut(&id) {
            if l.ends.is_some_and(|e| e <= end) {
                return;
            }
            l.ends = Some(end);
            l.now = Some(ToStage::Stop {
                picture: id,
                at,
                fade: frames,
            });
        }
        self.eng.picture(ToStage::Stop {
            picture: id,
            at,
            fade: frames,
        });
    }

    /// A picture that cannot be loaded: the cue breaks, as a stream that cannot be opened does
    /// (`started`: at its start, so it was only skipped).
    fn picture_refused(
        &mut self,
        cue: CueId,
        run: RunId,
        why: &str,
        started: bool,
        fx: &mut Effects,
    ) {
        fx.notices.push((
            "video_unplayable",
            format!("the Video cue's picture cannot be shown: {why}"),
            Some(cue),
        ));
        let reason = BrokenReason::DecodeFailed;
        fx.feedback.push(if started {
            Input::StartFailed { cue, run, reason }
        } else {
            Input::ActionFailed { cue, run, reason }
        });
    }

    /// The pictures of cues whose action is over (`StateChanged` to idle, loaded, post-wait or
    /// broken) go on that sample, unless a stop with a fade already takes them.
    pub(super) fn end_pictures(&mut self, actions: &[Action]) {
        for action in actions {
            let Action::StateChanged { cue, state, at } = *action else {
                continue;
            };
            if !matches!(
                state,
                CueState::Idle | CueState::PostWaiting | CueState::Broken
            ) {
                continue;
            }
            let doc = self.doc.id;
            let ids: Vec<u64> = self
                .eng
                .pictures
                .live
                .iter()
                .filter(|(_, l)| l.doc == doc && l.cue == cue && l.ends.is_none())
                .map(|(id, _)| *id)
                .collect();
            for id in ids {
                self.stop_picture(id, at, 0);
            }
        }
    }

    /// Pictures of this doc the stage host reported failed: each breaks its cue, with a
    /// notice, and nothing else (#643: a decode failure mid-show stops only that cue).
    pub(super) fn picture_failures(&mut self, now: SampleTime) {
        let doc = self.doc.id;
        let (mine, others): (Vec<_>, Vec<_>) = std::mem::take(&mut self.eng.pictures.failed)
            .into_iter()
            .partition(|(d, ..)| *d == doc);
        self.eng.pictures.failed = others;
        for (_, cue, run, reason) in mine {
            self.log(
                LogLevel::Warn,
                format!("a Video cue's picture failed: {reason}"),
            );
            let mut fx = Effects::default();
            fx.notices.push((
                "video_decode_failed",
                format!("the Video cue's picture failed: {reason}"),
                Some(cue),
            ));
            fx.feedback.push(Input::ActionFailed {
                cue,
                run,
                reason: BrokenReason::DecodeFailed,
            });
            self.follow(fx, now);
        }
    }
}
