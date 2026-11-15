// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

impl Voices {
    /// Carries out one engine action; non-voice actions are ignored.
    pub fn apply(&mut self, action: &Action, env: &mut Env<'_>, fx: &mut Effects) {
        match *action {
            Action::LoadVoice { cue, run, offset } => {
                self.load(cue, run, StartAt::Offset(offset), env, fx);
            }
            Action::LoadVoiceAt { cue, run, position } => {
                self.load(cue, run, StartAt::File(position), env, fx);
            }
            Action::SeekVoice {
                cue,
                run,
                from,
                at,
                position,
            } => self.seek(cue, run, from, at, position, env, fx),
            Action::RelocateVoice {
                cue,
                run,
                from,
                at,
                position,
            } => self.relocate(cue, run, from, at, position, env, fx),
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
            Action::DuckVoice {
                run,
                at,
                gain,
                frames,
                cap,
                ..
            } => {
                // The cap first, so the boost is bounded from the ramp's first sample; it moves over
                // the same time (a new maximum over the edit ramp, so no step).
                let cap = Queued::Ramp {
                    target: GainTarget::DuckCap,
                    gain: cap,
                    fade: Fade::new(frames, Curve::Linear),
                };
                self.command(run, at, cap, fx);
                // The stage moves evenly in dB; a curve is not read.
                let cmd = Queued::Ramp {
                    target: GainTarget::Duck,
                    gain,
                    fade: Fade::new(frames, Curve::Linear),
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

    fn load(&mut self, cue: CueId, run: RunId, from: StartAt, env: &mut Env<'_>, fx: &mut Effects) {
        self.release(run);
        let id = env.ids.next(env.doc);
        let (offset, file) = match from {
            StartAt::Offset(offset) => (offset, None),
            StartAt::File(frame) => (0, Some(frame)),
        };
        self.insert(
            id,
            VoiceRec {
                phase: Phase::Waiting {
                    prepared: None,
                    start: None,
                    paused: false,
                    queue: Vec::new(),
                },
                offset,
                file,
                shown: offset,
                media: cue_media(env.ws, cue),
                ..rec(cue, run, false)
            },
        );
        // A load at a point of the file (a seek) waits its turn in a burst (`advance_seek`).
        if file.is_some() && !self.seek_gate_open(cue, env.now) {
            return;
        }
        self.prepare_loaded(id, env, fx);
    }

    /// Prepares the stream of the loaded (not started) record `id`. A load past the end gets
    /// none: it starts and ends at once (`try_start` reports it). One that cannot be made fails
    /// the run.
    fn prepare_loaded(&mut self, id: VoiceId, env: &mut Env<'_>, fx: &mut Effects) {
        let Some(r) = self.recs.get(&id) else { return };
        let (cue, run) = (r.cue, r.run);
        let from = r.file.map_or(StartAt::Offset(r.offset), StartAt::File);
        if r.file.is_some() {
            self.close_seek_gate(cue, env);
        }
        match self.prepare(cue, from, env, fx) {
            Ok(made) => {
                if let Some(r) = self.recs.get_mut(&id)
                    && let Phase::Waiting { prepared, .. } = &mut r.phase
                {
                    *prepared = Some((made.prepared, made.total));
                    r.shown = made.shown;
                }
            }
            Err(PrepError::Pending | PrepError::Empty) => {}
            Err(PrepError::Failed) => {
                self.remove(id);
                fx.feedback.push(Input::ActionFailed {
                    cue,
                    run,
                    reason: BrokenReason::StreamFailed,
                });
            }
        }
    }

    /// Whether a seek of `cue` may prepare a stream on sample `now`.
    pub(super) fn seek_gate_open(&self, cue: CueId, now: SampleTime) -> bool {
        self.seek_gate.get(&cue).is_none_or(|&t| now >= t)
    }

    /// A seek of `cue` prepares a stream now: the next waits [`SEEK_PREPARE_INTERVAL`].
    pub(super) fn close_seek_gate(&mut self, cue: CueId, env: &Env<'_>) {
        let next = env
            .now
            .saturating_add(seconds_to_samples(SEEK_PREPARE_INTERVAL, env.rate));
        self.seek_gate.insert(cue, next);
    }

    /// Moves the voice of run `from` to file frame `position` as run `run` from sample `at`
    /// (`Action::SeekVoice`). A playing voice plays on until the moved one's stream is ready,
    /// then the two crossfade; a paused one is let go at once and the moved one waits for the
    /// resume. Seeks that come faster than that replace the one waiting, so a burst costs one
    /// stream and at most two sounding voices of the cue.
    #[allow(clippy::too_many_arguments)]
    fn seek(
        &mut self,
        cue: CueId,
        run: RunId,
        from: RunId,
        at: SampleTime,
        position: u64,
        env: &mut Env<'_>,
        fx: &mut Effects,
    ) {
        let mut replaces = None;
        let mut paused = false;
        let mut swap_after = 0;
        let mut preview = from.0 >= openlab_core::PREVIEW_RUN_BASE;
        if let Some(&old) = self.by_run.get(&from) {
            let stop_at = self.stop_date(old, at, env.floor);
            let mut drop_old = false;
            if let Some(r) = self.recs.get_mut(&old) {
                preview = r.preview;
                swap_after = r.swap_after;
                match &r.phase {
                    Phase::Playing if r.paused_at.is_some() => {
                        // Paused: silent already, so it goes at once.
                        paused = true;
                        r.retired = true;
                        let fade = Fade::NONE;
                        fx.commands
                            .push((stop_at, CommandKind::StopVoice { voice: old, fade }));
                    }
                    Phase::Playing => {
                        // Plays on until the moved voice starts.
                        replaces = Some(old);
                        r.retired = true;
                    }
                    Phase::Waiting { paused: held, .. } => {
                        // Not sounding yet (an earlier seek still waiting, a start waiting for
                        // its probe): this one takes its place, and the voice it was to replace.
                        paused = *held;
                        replaces = r.replaces.take();
                        drop_old = true;
                    }
                    Phase::Ended => {
                        // Ended while paused: the moved voice waits for the resume.
                        paused = true;
                        drop_old = true;
                    }
                }
            }
            self.by_run.remove(&from);
            if drop_old {
                self.remove(old);
            }
        }
        let id = env.ids.next(env.doc);
        self.insert(
            id,
            VoiceRec {
                phase: Phase::Waiting {
                    prepared: None,
                    start: Some(at),
                    paused,
                    queue: Vec::new(),
                },
                file: Some(position),
                moved: true,
                replaces,
                swap_after,
                release_fade: Fade::new(
                    seconds_to_samples(SEEK_CROSSFADE, env.rate),
                    Curve::Linear,
                ),
                media: cue_media(env.ws, cue),
                ..rec(cue, run, preview)
            },
        );
        self.advance_seek(id, env, fx);
    }

    /// Moves the voice of run `from` to its patch's current device as run `run`
    /// (`Action::RelocateVoice`): the core's run is `position` frames into its action on `at`.
    /// A playing voice plays on on its old device until [`MOVE_LEAD`] after `at`; a voice on
    /// the new device, its stream read from where the sound is then, starts on that sample and
    /// the two crossfade over [`SEEK_CROSSFADE`]. A paused one is let go at once (it is
    /// silent) and the moved one waits for the resume. One not sounding yet (a start or seek
    /// waiting for its stream) starts on the new device anyway: it only takes the fresh run.
    #[allow(clippy::too_many_arguments)]
    fn relocate(
        &mut self,
        cue: CueId,
        run: RunId,
        from: RunId,
        at: SampleTime,
        position: u64,
        env: &mut Env<'_>,
        fx: &mut Effects,
    ) {
        let Some(&old) = self.by_run.get(&from) else {
            // Its voice has ended (the report is on its way, under the old run): the run ends.
            fx.deferred.push((at, Input::ActionEnded { cue, run, at }));
            return;
        };
        let stop_at = self.stop_date(old, at, env.floor);
        let crossfade = Fade::new(seconds_to_samples(SEEK_CROSSFADE, env.rate), Curve::Linear);
        let lead = seconds_to_samples(MOVE_LEAD, env.rate);
        let Some(r) = self.recs.get_mut(&old) else {
            return;
        };
        let paused = match (&r.phase, r.paused_at) {
            (Phase::Playing, Some(p)) => Some(p),
            (Phase::Playing, None) => None,
            // Not sounding, or ended while paused: the record is the run's under its new id;
            // a voice it starts plays through the patch's slot as it is then.
            (Phase::Waiting { .. } | Phase::Ended, _) => {
                r.run = run;
                self.by_run.remove(&from);
                self.by_run.insert(run, old);
                return;
            }
        };
        // Where the sound is when the moved voice takes over: on the swap, or where it froze.
        let swap = match paused {
            Some(p) => p.max(r.start_at),
            None => at.saturating_add(lead).max(r.start_at),
        };
        let here = r.position(swap).unwrap_or(r.offset);
        // The core counts the run `position` frames in on `at` (its count runs ahead of the
        // voice's by any delay the voice's start had); the moved voice's length is told in the
        // core's count.
        let counted = match paused {
            Some(_) => position,
            None => position.saturating_add(swap - at),
        };
        // From the frame of the file the sound is at when the voice left loops by devamps or
        // started at a point of the file (its table is then not the action's from `offset`);
        // else from the same point of the action, so loop counts carry on.
        let from_file = r.file.is_some() || !r.cuts.is_empty();
        let start_from = if from_file {
            match r.file_frame(here) {
                Some(frame) => StartAt::File(frame),
                // At or past its end (a devamp stop leaving now): it ends where it is.
                None => {
                    r.run = run;
                    self.by_run.remove(&from);
                    self.by_run.insert(run, old);
                    return;
                }
            }
        } else {
            StartAt::Offset(here)
        };
        let elapsed = r.shown.saturating_add(here.saturating_sub(r.offset));
        let swap_after = r.swap_after;
        r.retired = true;
        let replaces = match paused {
            Some(_) => {
                fx.commands.push((
                    stop_at,
                    CommandKind::StopVoice {
                        voice: old,
                        fade: Fade::NONE,
                    },
                ));
                None
            }
            None => Some(old),
        };
        self.by_run.remove(&from);
        let (offset, file) = match start_from {
            StartAt::Offset(o) => (o, None),
            StartAt::File(f) => (0, Some(f)),
        };
        let id = env.ids.next(env.doc);
        self.insert(
            id,
            VoiceRec {
                phase: Phase::Waiting {
                    prepared: None,
                    start: Some(swap),
                    paused: paused.is_some(),
                    queue: Vec::new(),
                },
                offset,
                file,
                shown: elapsed,
                delay: counted.saturating_sub(offset),
                moved: true,
                relocated: true,
                replaces,
                swap_after,
                release_fade: crossfade,
                media: cue_media(env.ws, cue),
                ..rec(cue, run, false)
            },
        );
        self.advance_seek(id, env, fx);
    }

    /// Moves on a record waiting at a point of the file: a seek's stream is prepared when the
    /// cue's gate is open (also while paused, so it is ready by the resume) and it starts once
    /// that stream is ready ([`Self::try_start`]); a load there that a burst held back is
    /// prepared.
    fn advance_seek(&mut self, id: VoiceId, env: &mut Env<'_>, fx: &mut Effects) {
        let Some(r) = self.recs.get(&id) else { return };
        let Phase::Waiting {
            prepared: None,
            start,
            paused,
            ..
        } = r.phase
        else {
            return self.try_start(id, env, fx);
        };
        if (r.file.is_none() && !r.relocated) || !self.seek_gate_open(r.cue, env.now) {
            return;
        }
        if start.is_none() {
            return self.prepare_loaded(id, env, fx);
        }
        if paused {
            // A failure is reported when it resumes (`try_start` prepares again then).
            let cue = r.cue;
            let from = r.file.map_or(StartAt::Offset(r.offset), StartAt::File);
            self.close_seek_gate(cue, env);
            let made = self.prepare(cue, from, env, &mut Effects::default());
            if let (Ok(made), Some(r)) = (made, self.recs.get_mut(&id))
                && let Phase::Waiting { prepared, .. } = &mut r.phase
            {
                *prepared = Some((made.prepared, made.total));
                if !r.relocated {
                    r.shown = made.shown;
                }
            }
            return;
        }
        self.try_start(id, env, fx);
    }

    /// Moves on every record waiting at a point of the file (`Self::advance_seek`). Each tick.
    pub fn start_seeks(&mut self, env: &mut Env<'_>, fx: &mut Effects) {
        let mut waiting: Vec<VoiceId> = self
            .recs
            .iter()
            .filter(|(_, r)| {
                (r.file.is_some() || r.relocated)
                    && matches!(r.phase, Phase::Waiting { .. })
                    && (r.moved || matches!(r.phase, Phase::Waiting { prepared: None, .. }))
            })
            .map(|(id, _)| *id)
            .collect();
        waiting.sort_by_key(|id| id.0);
        for id in waiting {
            self.advance_seek(id, env, fx);
        }
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
                        shown: offset,
                        media: cue_media(env.ws, cue),
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
        let from = r.file.map_or(StartAt::Offset(offset), StartAt::File);
        let Phase::Waiting {
            prepared,
            start: Some(start),
            paused: false,
            ..
        } = &r.phase
        else {
            return;
        };
        let start = *start;
        if prepared.is_none() {
            // A seek in a burst waits its turn to prepare (`advance_seek`).
            if r.moved {
                if !self.seek_gate_open(cue, env.now) {
                    return;
                }
                self.close_seek_gate(cue, env);
            }
            match self.prepare(cue, from, env, fx) {
                Ok(made) => {
                    if let Some(r) = self.recs.get_mut(&id)
                        && let Phase::Waiting { prepared, .. } = &mut r.phase
                    {
                        *prepared = Some((made.prepared, made.total));
                        if !r.relocated {
                            r.shown = made.shown;
                        }
                        // The file read now, which a relink since the record was made may
                        // have changed: a failure probes this one.
                        r.media = cue_media(env.ws, cue);
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
                    // Nothing to play (a load or start at or past the cue's end): the
                    // action ends on its start sample. Its length is the offset itself, so the
                    // core dates the end, and a chained successor, on that sample instead of
                    // on the late report below (a `late_start` warning). A seek past the end
                    // lets the voice it moves from go.
                    self.fade_replaced(id, start.max(env.now), env.floor, fx);
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
                    // A started voice not yet in the mixer: nothing has played (unless it is a
                    // move of one that has).
                    self.fade_replaced(id, start.max(env.now), env.floor, fx);
                    if let Some(r) = self.remove(id) {
                        fx.feedback.push(r.failure(BrokenReason::StreamFailed));
                    }
                    return;
                }
            }
        }
        // A seek waits for its stream, the voice it moves from playing on, so the two can
        // crossfade; it waits no longer than `SEEK_PRIME_WAIT`, and swaps no sooner than the
        // last crossfade of the cue's voice has ended.
        if let Some(r) = self.recs.get(&id)
            && r.moved
            && let Phase::Waiting {
                prepared: Some((p, _)),
                ..
            } = &r.phase
        {
            let limit = start.saturating_add(seconds_to_samples(SEEK_PRIME_WAIT, env.rate));
            let ready = p.handle.is_primed() || env.now >= limit;
            if !ready || env.now < r.swap_after {
                return;
            }
            // A move to another device swaps on its planned sample, sent once that is in the
            // commit window (a stop or pause decided before then reaches the old voice).
            if r.relocated && start >= env.now.saturating_add(env.window) {
                return;
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
        let crossfade = Fade::new(seconds_to_samples(SEEK_CROSSFADE, env.rate), Curve::Linear);
        let fade_in = if r.moved {
            FadeIn::Crossfade(crossfade)
        } else if r.shown == 0 {
            FadeIn::Cue
        } else {
            FadeIn::None
        };
        let replaces = r.replaces.take();
        if r.moved {
            r.swap_after = at.saturating_add(crossfade.frames);
        }
        match voice_start(env, cue, prepared, id, fade_in) {
            Some((vs, handle)) => {
                // The voice it moves from fades out as this one fades in, on the same sample.
                if let Some(old) = replaces {
                    let old_at = self.stop_date(old, at, env.floor);
                    fx.commands.push((
                        old_at,
                        CommandKind::StopVoice {
                            voice: old,
                            fade: crossfade,
                        },
                    ));
                }
                let Some(r) = self.recs.get_mut(&id) else {
                    return;
                };
                r.stream = Some(handle);
                r.start_at = at;
                r.total = total;
                r.table = table;
                // A moved voice's delay starts at the difference between the core's count and
                // its own (`relocate`); a start held up adds the hold.
                r.delay = r.delay.saturating_add(at - start);
                let delay = r.delay;
                let preview = r.preview;
                let held = r.held.take().is_some();
                r.start_id = Some(id);
                r.slot = Some(vs.patch);
                if let Some(CueKind::Audio { patch, .. }) = env.ws.find_cue(cue).map(|c| &c.kind) {
                    r.patch = Some(*patch);
                    r.levelled = env.patches.outputs(env.doc, *patch);
                }
                // A move swapped in while playing takes over levels already on their way:
                // each where it stands on the swap, then the rest of its ramp, so it ends on
                // the sample the engine planned. Otherwise (a start held up, a resume) a
                // queued command applies from the start, as a paused voice's does.
                let queue = if replaces.is_some() {
                    let initial = |t: GainTarget| initial_gain(&vs, t);
                    caught_up(queue, at, initial)
                } else {
                    queue.into_iter().map(|(t, q)| (t.max(at), q)).collect()
                };
                fx.commands.push((at, CommandKind::StartVoice(vs)));
                for (t, q) in queue {
                    fx.commands.push((t, queued(id, q)));
                }
                // The engine owns the end of a finite action (`ActionLength`): it is the
                // action's length counted from the start the engine dated, so a start held up
                // by its probe counts the delay in.
                if let Some(total) = total
                    && !preview
                {
                    let frames = offset.saturating_add(total).saturating_add(delay);
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
                if let Some(old) = replaces {
                    let old_at = self.stop_date(old, at, env.floor);
                    fx.commands.push((
                        old_at,
                        CommandKind::StopVoice {
                            voice: old,
                            fade: crossfade,
                        },
                    ));
                }
                if let Some(r) = self.remove(id) {
                    fx.feedback.push(r.failure(BrokenReason::VoiceRejected));
                }
            }
        }
    }

    /// Lets go of the voice a seek waiting in record `id` was to replace: it fades out over the
    /// crossfade from `at` (the seek did not happen, or was stopped or paused first).
    pub(super) fn fade_replaced(
        &mut self,
        id: VoiceId,
        at: SampleTime,
        floor: Option<SampleTime>,
        fx: &mut Effects,
    ) {
        let Some(r) = self.recs.get_mut(&id) else {
            return;
        };
        let Some(old) = r.replaces.take() else {
            return;
        };
        let fade = r.release_fade;
        let at = self.stop_date(old, at, floor);
        fx.commands
            .push((at, CommandKind::StopVoice { voice: old, fade }));
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
            // A seek still waiting: the voice it was to replace is what sounds, and stops so.
            if let Some(r) = self.recs.get_mut(&id) {
                r.release_fade =
                    fade.map_or(Fade::NONE, |f| Fade::new(f.frames, to_curve(f.curve)));
            }
            self.fade_replaced(id, at, floor, fx);
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
        // A seek still waiting: the voice it was to replace fades out (as a pause declicks);
        // the moved voice waits for the resume.
        self.fade_replaced(id, date, floor, fx);
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
            Phase::Waiting {
                paused,
                start,
                prepared,
                ..
            } => {
                *paused = true;
                // A move to another device was to take over later, from where the sound would
                // be then: it resumes from where the pause stops it instead (its stream is
                // read again on the resume).
                if r.relocated
                    && let Some(s) = *start
                    && s > date
                {
                    let back = s - date;
                    *start = Some(date);
                    *prepared = None;
                    match r.file.as_mut() {
                        Some(f) => {
                            *f = f.saturating_sub(back);
                            r.delay = r.delay.saturating_sub(back);
                        }
                        None => r.offset = r.offset.saturating_sub(back),
                    }
                    r.shown = r.shown.saturating_sub(back);
                }
            }
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
            Phase::Waiting { queue, .. } => {
                // A seek still waiting: the voice it moves from is what sounds until the swap,
                // so it follows every level change too (a devamp is the moved voice's alone).
                if let (Some(old), Queued::Ramp { .. }) = (r.replaces, cmd) {
                    fx.commands.push((at, queued(old, cmd)));
                }
                queue.push((at, cmd));
            }
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
        self.freed |= !r.preview;
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
