// SPDX-License-Identifier: GPL-3.0-or-later

//! Seek: an Audio cue's playback moved to a point of its media file (a click in its waveform,
//! CUE-05 load to time). The core does not know the file's slices; it says where in the file
//! (`position`, a frame at the device rate) and the binary plans the slices from there, so a
//! point past a slice that loops until devamped is reachable too.

use crate::cue::{CueKind, FadeCurve};
use crate::model::CueId;

use super::level::Level;
use super::run::{DuckLevel, Loaded, Phase, Run, Slot, Voice};
use super::time::SampleTime;
use super::types::{Action, EngineWarning, RunId};
use super::{Ctx, CueEngine, release};

impl CueEngine {
    /// Moves `cue` to `seconds` into its media file. A preview of it jumps there; the show side
    /// does what its state allows (see [`Input::Seek`](super::Input::Seek)).
    pub(super) fn seek(&mut self, cue: CueId, seconds: f64, cx: &mut Ctx) {
        let Some(model) = cx.ws.find_cue(cue) else {
            return cx.warn(EngineWarning::UnknownCue { cue });
        };
        if !matches!(model.kind, CueKind::Audio { .. }) {
            return cx.warn(EngineWarning::LoadIgnored { cue });
        }
        let position = cx.secs(seconds, &self.config);
        self.seek_preview(cue, position, cx);
        match self.slots.get(&cue) {
            None | Some(Slot::Loaded(_) | Slot::Lost(_)) => self.seek_load(cue, position, cx),
            Some(Slot::Broken(_)) => cx.warn(EngineWarning::CueBroken { cue }),
            Some(Slot::Active(run)) if run.is_broken() => {
                cx.warn(EngineWarning::CueBroken { cue });
            }
            Some(Slot::Active(_)) => self.seek_run(cue, position, cx),
        }
    }

    /// An idle or loaded cue is loaded at `position`: GO starts its action there, past its
    /// pre-wait (the point clicked is where the sound starts). Refused as a load is: a broken
    /// cue or one whose lost device is still away loads nothing.
    fn seek_load(&mut self, cue: CueId, position: SampleTime, cx: &mut Ctx) {
        let Some(model) = cx.ws.find_cue(cue) else {
            return;
        };
        if self.is_skipped(model) {
            return;
        }
        if !self.clear_lost_to_start(cue, cx.now, cx) {
            return cx.warn(EngineWarning::CueBroken { cue });
        }
        if self.refuses(cue, cx) {
            return cx.warn(EngineWarning::CueBroken { cue });
        }
        let before = self.state(cue);
        if let Some(old) = self.slots.remove(&cue) {
            release(cue, &old, cx);
        }
        let run = self.new_run();
        let voice = self.is_armed(model);
        if voice {
            cx.out.push(Action::LoadVoiceAt { cue, run, position });
        }
        let pre_wait = cx.secs(model.pre_wait, &self.config);
        self.slots.insert(
            cue,
            Slot::Loaded(Loaded {
                run,
                pre_wait_left: 0,
                action_offset: 0,
                pre_wait_total: pre_wait,
                voice,
                seek: Some(position),
            }),
        );
        self.report(cue, before, cx.now, cx);
    }

    /// An active run: in its pre-wait the action starts at the point now (or on its resume when
    /// paused); in its action the voice moves there and plays on (or stays paused). Its timers,
    /// post-wait and continues are untouched; its end is the moved voice's, once the binary
    /// tells its length again.
    fn seek_run(&mut self, cue: CueId, position: SampleTime, cx: &mut Ctx) {
        let now = cx.now;
        let fresh = self.new_run();
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return;
        };
        if run.stop_end.is_some() {
            return cx.warn(EngineWarning::LoadIgnored { cue });
        }
        match (run.phase, run.voice) {
            (Phase::PreWait { .. }, voice) => {
                // A stream loaded for a load to time is at the wrong point now.
                if voice == Voice::Loaded {
                    cx.out.push(Action::UnloadVoice { cue, run: run.run });
                    run.voice = Voice::None;
                }
                run.seek = Some(position);
                run.action_offset = 0;
                // Ends at once; the deadline loop starts the action in this call (on the
                // resume when it is paused).
                let until = run.paused_at.unwrap_or(now.max(run.committed));
                run.phase = Phase::PreWait { until };
            }
            (Phase::Action { start, .. }, Voice::Started { .. }) => {
                let at = run.commit(now);
                let from = run.run;
                cx.out.push(Action::SeekVoice {
                    cue,
                    run: fresh,
                    from,
                    at,
                    position,
                });
                run.run = fresh;
                // Positions count from the moved voice (the binary's lengths are from it too).
                run.voice = Voice::Started {
                    at: run.paused_at.map_or(at, |p| p.min(at)),
                    from: 0,
                    first: at,
                };
                run.action_offset = 0;
                run.length = None;
                run.phase = Phase::Action { start, end: None };
                regive_levels(cue, run, at, cx);
                // A pending devamp goes to the moved voice (a paused one gets it on its resume,
                // as any paused voice does).
                if run.paused_at.is_none()
                    && let Some(d) = run.devamp.as_mut()
                {
                    d.at = None;
                    cx.out.push(Action::DevampVoice {
                        cue,
                        run: fresh,
                        at,
                        stop: d.stop,
                    });
                }
            }
            // A disarmed or skipped run plays no voice; a post-waiting one has finished its
            // sound.
            _ => cx.warn(EngineWarning::LoadIgnored { cue }),
        }
    }

    /// The patch of `cue`'s run `run_id` now plays on another device (`Input::DeviceMoved`):
    /// its voice moves there at once and plays on in step, from where the run is now (a paused
    /// one from where it froze, staying paused). Timers, post-wait, continues and the end are
    /// the run's own and stay; the moved voice tells its length again. A run stopping with a
    /// fade ends where it is; a stale, broken or not yet started run has nothing to move. A run
    /// being devamped moves once its voice has left the loop (or the devamp was ignored).
    pub(super) fn device_moved(&mut self, cue: CueId, run_id: RunId, cx: &mut Ctx) {
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return;
        };
        if run.run != run_id || run.is_broken() || run.stop_end.is_some() {
            return;
        }
        // A devamp on its way (sent and not answered, or answered and its loop not left yet):
        // the audio side alone knows where the voice is in its loop then, and a voice moved
        // now would play that loop again. It moves once the devamp is done with.
        if run.devamp.is_some() {
            run.move_pending = true;
            return;
        }
        self.move_device(cue, cx);
    }

    /// A move held for a devamp (`Run::move_pending`) is made now that the devamp is done
    /// with: the voice left its loop, or the devamp was ignored.
    pub(super) fn move_held(&mut self, cue: CueId, cx: &mut Ctx) {
        if let Some(Slot::Active(run)) = self.slots.get_mut(&cue)
            && std::mem::take(&mut run.move_pending)
            && run.devamp.is_none()
        {
            self.move_device(cue, cx);
        }
    }

    /// Moves the run of `cue` to its patch's device now (see [`Self::device_moved`]).
    fn move_device(&mut self, cue: CueId, cx: &mut Ctx) {
        let now = cx.now;
        let fresh = self.new_run();
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return;
        };
        if run.is_broken() || run.stop_end.is_some() {
            return;
        }
        let (
            Phase::Action { start, .. },
            Voice::Started {
                at: started,
                from,
                first,
            },
        ) = (run.phase, run.voice)
        else {
            return;
        };
        let at = run.commit(now);
        let from_run = run.run;
        run.run = fresh;
        let position = match run.paused_at {
            // Paused: the position stands where it froze; the resume plays on from there.
            Some(paused) => {
                run.voice = Voice::Started {
                    at: started,
                    from,
                    first,
                };
                run.position(paused)
            }
            None => {
                let position = run.position(at);
                run.voice = Voice::Started {
                    at,
                    from: position,
                    first: at,
                };
                run.length = None;
                run.phase = Phase::Action { start, end: None };
                position
            }
        };
        cx.out.push(Action::RelocateVoice {
            cue,
            run: fresh,
            from: from_run,
            at,
            position,
        });
        // No devamp is on its way (`device_moved` holds the move until it is done with).
        regive_levels(cue, run, at, cx);
    }

    /// A preview of `cue` (an audio cue's own) jumps to `position`, playing or paused.
    fn seek_preview(&mut self, cue: CueId, position: SampleTime, cx: &mut Ctx) {
        if !self.previews.contains_key(&cue) {
            return;
        }
        let fresh = self.new_preview_run();
        let Some(preview) = self.previews.get_mut(&cue) else {
            return;
        };
        let at = cx.now.max(preview.started);
        cx.out.push(Action::SeekVoice {
            cue,
            run: fresh,
            from: preview.run,
            at,
            position,
        });
        preview.run = fresh;
        preview.started = at;
        preview.offset = 0;
    }
}

/// The moved voice starts with the model's levels: it is given what Fade cues have set or are
/// still moving, and its duck, on its first sample, as a voice restarted there needs.
fn regive_levels(cue: CueId, run: &mut Run, at: SampleTime, cx: &mut Ctx) {
    let voice: RunId = run.run;
    for (&param, level) in &run.levels {
        cx.out.push(Action::Ramp {
            cue,
            run: voice,
            at,
            param,
            gain: level.value(at),
            frames: 0,
            curve: FadeCurve::Linear,
        });
        if let Some(frames) = moving(level, at) {
            cx.out.push(Action::Ramp {
                cue,
                run: voice,
                at,
                param,
                gain: level.to,
                frames,
                curve: level.curve,
            });
        }
    }
    let duck = run.duck;
    let value = duck.value(at);
    let left = duck.start.saturating_add(duck.frames).saturating_sub(at);
    if value == 1.0 && !duck.moving(at) {
        return;
    }
    cx.out.push(Action::DuckVoice {
        cue,
        run: voice,
        at,
        gain: value,
        frames: 0,
        cap: duck.cap,
    });
    if duck.moving(at) {
        cx.out.push(Action::DuckVoice {
            cue,
            run: voice,
            at,
            gain: duck.to,
            frames: left,
            cap: duck.cap,
        });
    }
    run.duck = DuckLevel {
        from: value,
        to: duck.to,
        start: at,
        frames: if duck.moving(at) { left } else { 0 },
        cap: duck.cap,
    };
}

/// Frames a Fade's ramp still moves after `at`, if it is moving.
fn moving(level: &Level, at: SampleTime) -> Option<SampleTime> {
    (at < level.end() && level.frames > 0).then(|| level.end() - at)
}
