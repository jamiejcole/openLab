// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

impl Voices {
    /// The probe of `key` answered: starts the voices that waited for it.
    pub fn media_ready(&mut self, key: &str, env: &mut Env<'_>, fx: &mut Effects) {
        let waiting: Vec<VoiceId> = self
            .recs
            .iter()
            .filter(|(_, r)| {
                matches!(r.phase, Phase::Waiting { prepared: None, .. })
                    && cue_media(env.ws, r.cue).as_deref() == Some(key)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in waiting {
            let Some(r) = self.recs.get(&id) else {
                continue;
            };
            let (cue, run, offset, started) = (
                r.cue,
                r.run,
                r.offset,
                matches!(r.phase, Phase::Waiting { start: Some(_), .. }),
            );
            if started {
                self.try_start(id, env, fx);
                continue;
            }
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
    }

    /// Prepares the stream of `cue` from `offset` (no I/O: the disk worker opens the file).
    pub(super) fn prepare(
        &self,
        cue: CueId,
        offset: u64,
        env: &mut Env<'_>,
        fx: &mut Effects,
    ) -> Result<(PreparedStream, Option<u64>), PrepError> {
        let Some(c) = env.ws.find_cue(cue) else {
            return Err(PrepError::Failed);
        };
        let CueKind::Audio {
            media: Some(media), ..
        } = &c.kind
        else {
            return Err(PrepError::Failed);
        };
        let key = media.as_str();
        match env.media.get(key) {
            None | Some(Probe::Pending) => return Err(PrepError::Pending),
            Some(Probe::Failed) => return Err(PrepError::Failed),
            Some(Probe::Ready(_)) => {}
        }
        let Some((ready, source)) = env.media.media(key) else {
            return Err(PrepError::Pending);
        };
        let Some(spec) = plan_spec(&c.kind) else {
            return Err(PrepError::Failed);
        };
        let slices = match cue_slices(&spec, &ready.probed.info, env.rate, offset) {
            Ok(s) => s,
            Err(PlanFailure::Plan(e)) => {
                fx.notices.push((
                    "bad_plan",
                    format!("the cue's slices cannot be played: {e}"),
                    Some(cue),
                ));
                return Err(PrepError::Failed);
            }
        };
        if slices.is_empty() {
            return Err(PrepError::Empty);
        }
        let total = total_frames(&slices);
        let req = StreamRequest {
            media: source,
            info: ready.probed.info,
            seeking: ready.probed.seeking.clone(),
            slices,
        };
        match env.streamer.prepare(req) {
            Ok(p) => Ok((p, total)),
            Err(e) => {
                fx.notices.push((
                    "stream_failed",
                    format!("the cue's audio could not be streamed: {e}"),
                    Some(cue),
                ));
                Err(PrepError::Failed)
            }
        }
    }

    /// A primed stream for a start from the top of `cue`, if one is ready and still matches.
    pub(super) fn take_prime(
        &mut self,
        cue: CueId,
        offset: u64,
        env: &Env<'_>,
    ) -> Option<(PreparedStream, Option<u64>)> {
        let usable = offset == 0
            && self
                .primes
                .get(&cue)
                .is_some_and(|p| sig_of(env, cue).as_ref() == Some(&p.sig));
        let prime = self.primes.remove(&cue)?;
        if usable {
            self.prime_hits = self.prime_hits.saturating_add(1);
        }
        usable.then_some((prime.prepared, prime.total))
    }

    /// Keeps exactly `wanted` primed (in order, most urgent first, at most `max`): new ones are
    /// prepared, at most `burst` of them; ones no longer wanted or out of date are released.
    /// Returns whether the burst limit left some unmade.
    pub fn prime(&mut self, wanted: &[CueId], max: usize, burst: usize, env: &mut Env<'_>) -> bool {
        let wanted: Vec<CueId> = wanted.iter().copied().take(max).collect();
        let mut made = 0;
        self.primes
            .retain(|cue, p| wanted.contains(cue) && sig_of(env, *cue).as_ref() == Some(&p.sig));
        for cue in wanted {
            if self.primes.contains_key(&cue)
                || self.by_run.values().any(|id| {
                    self.recs
                        .get(id)
                        .is_some_and(|r| r.cue == cue && !r.preview)
                })
            {
                continue;
            }
            let Some(sig) = sig_of(env, cue) else {
                continue;
            };
            if made >= burst {
                return true;
            }
            // A prime that cannot be made now (probe pending or failed) is tried again later.
            let mut quiet = Effects::default();
            if let Ok((prepared, total)) = self.prepare(cue, 0, env, &mut quiet) {
                made += 1;
                self.primes.insert(
                    cue,
                    Prime {
                        prepared,
                        total,
                        sig,
                    },
                );
            }
        }
        false
    }

    /// Checks the streams of live voices for failures: each failed run is reported once. A
    /// started voice whose stream is still not primed `limit` samples after its start (an open
    /// hung on a dead volume) is stopped now; its cue breaks when the mixer reports the end.
    pub fn poll_streams(
        &mut self,
        now: SampleTime,
        limit: SampleTime,
        fx: &mut Effects,
        ws: &Workspace,
    ) {
        for (&id, r) in &mut self.recs {
            if r.stream_failed || !matches!(r.phase, Phase::Playing) || r.paused_at.is_some() {
                continue;
            }
            let late = r.stream.as_ref().is_some_and(|h| {
                !h.is_primed()
                    && h.error().is_none()
                    && now
                        >= r.start_at
                            .saturating_add(r.paused_frames)
                            .saturating_add(limit)
            });
            if !late {
                continue;
            }
            r.stream_failed = true;
            let at = now.max(r.start_at).max(r.resumed_at.unwrap_or(0));
            // A resume still on its way: the mixer may know the voice by its old id.
            for voice in std::iter::once(id).chain(r.prev_id) {
                let fade = Fade::NONE;
                fx.commands
                    .push((at, CommandKind::StopVoice { voice, fade }));
            }
            fx.notices.push((
                "stream_timeout",
                "a cue's audio file did not open in time (is its disk or network share \
                 responding?); the cue was stopped"
                    .to_owned(),
                Some(r.cue),
            ));
        }
        for r in self.recs.values_mut() {
            if r.stream_failed {
                continue;
            }
            let failed = match &r.phase {
                Phase::Playing => r.stream.as_ref().is_some_and(|h| h.error().is_some()),
                Phase::Waiting {
                    prepared: Some((p, _)),
                    ..
                } => p.handle.error().is_some(),
                _ => false,
            };
            if failed {
                r.stream_failed = true;
                fx.feedback.push(r.failure(BrokenReason::StreamFailed));
                if let Some(m) = cue_media(ws, r.cue) {
                    fx.reprobe.push(m);
                }
            }
        }
        // A prime whose stream failed is useless; the next priming pass makes it again.
        self.primes
            .retain(|_, p| p.prepared.handle.error().is_none());
    }
}
