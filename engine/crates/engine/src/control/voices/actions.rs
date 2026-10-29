// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

impl Voices {
    /// Carries out one engine action; non-voice actions are ignored.
    pub fn apply(&mut self, action: &Action, env: &mut Env<'_>, fx: &mut Effects) {
        match *action {
            Action::LoadVoice { cue, run, offset } => self.load(cue, run, offset, env, fx),
            Action::UnloadVoice { run, .. } => self.release(run),
            Action::StartVoice {
                cue,
                run,
                at,
                offset,
            } => self.start(cue, run, at, offset, false, env, fx),
            Action::PreviewStart { cue, run, at } => self.start(cue, run, at, 0, true, env, fx),
            Action::StopVoice { run, at, fade, .. } | Action::PreviewStop { run, at, fade, .. } => {
                self.stop(run, at, fade, env.floor, fx);
            }
            Action::PauseVoice { run, at, .. } => self.pause(run, at, env.floor, fx),
            Action::ResumeVoice {
                cue,
                run,
                paused,
                at,
                ..
            } => self.resume(cue, run, paused, at, env, fx),
            Action::Ramp {
                run,
                at,
                param,
                gain,
                frames,
                curve,
                ..
            } => {
                let cmd = Queued::Ramp {
                    target: target(param),
                    gain,
                    fade: Fade::new(frames, to_curve(curve)),
                };
                self.command(run, at, cmd, fx);
            }
            Action::DevampVoice { run, at, stop, .. } => {
                if let Some(r) = self.by_run.get(&run).and_then(|id| self.recs.get_mut(id)) {
                    r.devamp_stop = stop;
                }
                self.command(run, at, Queued::Devamp { stop }, fx);
            }
            _ => {}
        }
    }

    fn load(&mut self, cue: CueId, run: RunId, offset: u64, env: &mut Env<'_>, fx: &mut Effects) {
        self.release(run);
        let id = env.ids.next(env.doc);
        let prepared = match self.prepare(cue, offset, env, fx) {
            Ok(p) => Some(p),
            Err(PrepError::Pending) => None,
            Err(PrepError::Empty) => {
                // A load past the end: it starts and ends at once (`start` reports it).
                None
            }
            Err(PrepError::Failed) => {
                fx.feedback.push(Input::ActionFailed {
                    cue,
                    run,
                    reason: BrokenReason::StreamFailed,
                });
                return;
            }
        };
        self.insert(
            id,
            VoiceRec {
                phase: Phase::Waiting {
                    prepared,
                    start: None,
                    paused: false,
                    queue: Vec::new(),
                },
                offset,
                ..rec(cue, run, false)
            },
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        cue: CueId,
        run: RunId,
        at: SampleTime,
        offset: u64,
        preview: bool,
        env: &mut Env<'_>,
        fx: &mut Effects,
    ) {
        let id = match self.by_run.get(&run) {
            Some(&id)
                if self.recs.get(&id).is_some_and(|r| {
                    r.offset == offset && matches!(r.phase, Phase::Waiting { .. })
                }) =>
            {
                id
            }
            _ => {
                self.release(run);
                let id = env.ids.next(env.doc);
                let prepared = if preview {
                    None
                } else {
                    self.take_prime(cue, offset, env)
                };
                self.insert(
                    id,
                    VoiceRec {
                        phase: Phase::Waiting {
                            prepared,
                            start: None,
                            paused: false,
                            queue: Vec::new(),
                        },
                        offset,
                        ..rec(cue, run, preview)
                    },
                );
                id
            }
        };
        if let Some(VoiceRec {
            phase: Phase::Waiting { start, .. },
            ..
        }) = self.recs.get_mut(&id)
        {
            *start = Some(at);
        }
        self.try_start(id, env, fx);
    }

    /// Puts a waiting voice into the mixer once it has a stream and a start and is not paused.
    pub(super) fn try_start(&mut self, id: VoiceId, env: &mut Env<'_>, fx: &mut Effects) {
        let Some(r) = self.recs.get(&id) else { return };
        let (cue, run, offset) = (r.cue, r.run, r.offset);
        let Phase::Waiting {
            prepared,
            start: Some(_),
            paused: false,
            ..
        } = &r.phase
        else {
            return;
        };
        if prepared.is_none() {
            match self.prepare(cue, offset, env, fx) {
                Ok(p) => {
                    if let Some(VoiceRec {
                        phase: Phase::Waiting { prepared, .. },
                        ..
                    }) = self.recs.get_mut(&id)
                    {
                        *prepared = Some(p);
                    }
                }
                Err(PrepError::Pending) => {
                    if let Some(r) = self.recs.get_mut(&id)
                        && r.held.is_none()
                    {
                        r.held = Some(env.now);
                        fx.notices.push((
                            "probe_wait",
                            "a cue waits for its audio file to be read; it starts once it is"
                                .to_owned(),
                            Some(cue),
                        ));
                    }
                    return;
                }
                Err(PrepError::Empty) => {
                    // Nothing to play (a load or start at or past the cue's end, M4.11b): the
                    // action ends on its start sample. Its length is the offset itself, so the
                    // core dates the end, and a chained successor, on that sample instead of
                    // on the late report below (a `late_start` warning).
                    if let Some(r) = self.remove(id) {
                        let at = match r.phase {
                            Phase::Waiting { start, .. } => start.unwrap_or(env.now),
                            _ => env.now,
                        };
                        if !r.preview {
                            // A start the probe held up ends when it is released, as a played
                            // one counts the delay in.
                            let frames = offset.saturating_add(env.now.saturating_sub(at));
                            fx.feedback.push(Input::ActionLength { cue, run, frames });
                        }
                        fx.deferred.push((at, Input::ActionEnded { cue, run, at }));
                    }
                    return;
                }
                Err(PrepError::Failed) => {
                    // A started voice not yet in the mixer: nothing has played.
                    self.remove(id);
                    fx.feedback.push(Input::StartFailed {
                        cue,
                        run,
                        reason: BrokenReason::StreamFailed,
                    });
                    return;
                }
            }
        }
        let Some(r) = self.recs.get_mut(&id) else {
            return;
        };
        let Phase::Waiting {
            prepared,
            start,
            queue,
            ..
        } = std::mem::replace(&mut r.phase, Phase::Playing)
        else {
            return;
        };
        let (Some((prepared, total)), Some(start)) = (prepared, start) else {
            return;
        };
        // A start the probe held up plays as soon as it can, from its head (a cold start).
        let at = start.max(env.now);
        let table = prepared.slices.clone();
        match voice_start(env, cue, prepared, id, offset) {
            Some((vs, handle)) => {
                r.stream = Some(handle);
                r.start_at = at;
                r.total = total;
                r.table = table;
                r.delay = at - start;
                let preview = r.preview;
                let held = r.held.take().is_some();
                r.start_id = Some(id);
                r.slot = Some(vs.patch);
                if let Some(CueKind::Audio { patch, .. }) = env.ws.find_cue(cue).map(|c| &c.kind) {
                    r.patch = Some(*patch);
                    r.levelled = env.patches.outputs(env.doc, *patch);
                }
                fx.commands.push((at, CommandKind::StartVoice(vs)));
                for (t, q) in queue {
                    fx.commands.push((t.max(at), queued(id, q)));
                }
                // The engine owns the end of a finite action (`ActionLength`): it is the
                // action's length counted from the start the engine dated, so a start held up
                // by its probe counts the delay in.
                if let Some(total) = total
                    && !preview
                {
                    let frames = offset.saturating_add(total).saturating_add(at - start);
                    fx.feedback.push(Input::ActionLength { cue, run, frames });
                }
                if held {
                    fx.notices.push((
                        "cold_start",
                        "a cue started late: its audio file had to be read first".to_owned(),
                        Some(cue),
                    ));
                }
            }
            None => {
                self.remove(id);
                fx.feedback.push(Input::StartFailed {
                    cue,
                    run,
                    reason: BrokenReason::VoiceRejected,
                });
            }
        }
    }

    fn stop(
        &mut self,
        run: RunId,
        at: SampleTime,
        fade: Option<StopFade>,
        floor: Option<SampleTime>,
        fx: &mut Effects,
    ) {
        let Some(&id) = self.by_run.get(&run) else {
            return;
        };
        let playing = self
            .recs
            .get(&id)
            .is_some_and(|r| matches!(r.phase, Phase::Playing));
        if playing {
            let at = self.stop_date(id, at, floor);
            let fade = fade.map_or(Fade::NONE, |f| Fade::new(f.frames, to_curve(f.curve)));
            fx.commands
                .push((at, CommandKind::StopVoice { voice: id, fade }));
            // The record goes when the mixer reports the voice finished.
        } else {
            self.remove(id);
        }
    }

    /// A pause during an outage (`floor`) is dated, like a stop, on the first sample the mixer
    /// renders when the device is back, so no voice plays one more block. The mixer then holds
    /// the voice `at − date` frames earlier than the engine thinks: that is counted into the
    /// action's length (`delay`) and sent again, so the engine's end stays the voice's.
    fn pause(&mut self, run: RunId, at: SampleTime, floor: Option<SampleTime>, fx: &mut Effects) {
        let Some(&id) = self.by_run.get(&run) else {
            return;
        };
        let date = self.stop_date(id, at, floor);
        let Some(r) = self.recs.get_mut(&id) else {
            return;
        };
        match &mut r.phase {
            Phase::Playing => {
                r.paused_at = Some(date);
                fx.commands
                    .push((date, CommandKind::PauseVoice { voice: id }));
                let early = at.saturating_sub(date);
                if early > 0 {
                    r.delay = r.delay.saturating_add(early);
                    if let Some(total) = r.total
                        && !r.preview
                    {
                        let frames = r.offset.saturating_add(total).saturating_add(r.delay);
                        fx.feedback.push(Input::ActionLength {
                            cue: r.cue,
                            run,
                            frames,
                        });
                    }
                }
            }
            Phase::Waiting { paused, .. } => *paused = true,
            Phase::Ended => {}
        }
    }

    fn resume(
        &mut self,
        cue: CueId,
        run: RunId,
        paused: RunId,
        at: SampleTime,
        env: &mut Env<'_>,
        fx: &mut Effects,
    ) {
        let Some(old) = self.by_run.get(&paused).copied() else {
            return;
        };
        let new = env.ids.next(env.doc);
        let Some(mut r) = self.recs.remove(&old) else {
            return;
        };
        self.by_run.remove(&paused);
        r.run = run;
        r.resumed_at = Some(at);
        // Only the id just replaced can still carry an event; an older one is named for one
        // more meter read (`gone`) and then forgotten, so a cue paused and resumed many times
        // keeps one alias, not one per resume.
        if let Some(older) = r.prev_id.replace(old) {
            self.aliases.remove(&older);
        }
        self.gone.push((old, cue));
        self.aliases.insert(old, (new, paused));
        match &mut r.phase {
            Phase::Playing => {
                if let Some(p) = r.paused_at.take() {
                    r.paused_frames = r.paused_frames.saturating_add(at.saturating_sub(p));
                }
                fx.commands.push((
                    at,
                    CommandKind::ResumeVoice {
                        voice: old,
                        new_id: new,
                    },
                ));
                self.insert(new, r);
            }
            Phase::Waiting {
                paused: held,
                start,
                ..
            } => {
                *held = false;
                if start.is_some() {
                    *start = Some(at);
                }
                self.insert(new, r);
                self.try_start(new, env, fx);
            }
            Phase::Ended => {
                // The voice ended while paused: the mixer ignores the resume of an unknown voice,
                // so the end is reported for the fresh run (M2-E (a)).
                fx.deferred.push((at, Input::ActionEnded { cue, run, at }));
                self.drop_aliases(&r);
            }
        }
    }

    fn command(&mut self, run: RunId, at: SampleTime, cmd: Queued, fx: &mut Effects) {
        let Some(&id) = self.by_run.get(&run) else {
            return;
        };
        let Some(r) = self.recs.get_mut(&id) else {
            return;
        };
        match &mut r.phase {
            Phase::Playing => fx.commands.push((at, queued(id, cmd))),
            Phase::Waiting { queue, .. } => queue.push((at, cmd)),
            Phase::Ended => {}
        }
    }

    /// Lets go of a run's voice that is not playing (or forgets one that is).
    fn release(&mut self, run: RunId) {
        if let Some(&id) = self.by_run.get(&run) {
            self.remove(id);
        }
    }

    pub(super) fn insert(&mut self, id: VoiceId, r: VoiceRec) {
        self.by_run.insert(r.run, id);
        self.recs.insert(id, r);
    }

    pub(super) fn remove(&mut self, id: VoiceId) -> Option<VoiceRec> {
        let r = self.recs.remove(&id)?;
        if self.by_run.get(&r.run) == Some(&id) {
            self.by_run.remove(&r.run);
        }
        self.drop_aliases(&r);
        if self.gone.len() >= GONE_LIMIT {
            self.gone.remove(0);
        }
        self.gone.push((id, r.cue));
        Some(r)
    }

    fn drop_aliases(&mut self, r: &VoiceRec) {
        if let Some(o) = r.prev_id {
            self.aliases.remove(&o);
        }
    }
}
