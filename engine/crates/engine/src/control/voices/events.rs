// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

impl Voices {
    /// Turns one mixer event into feedback.
    pub fn event(&mut self, e: Event, fx: &mut Effects) {
        match e {
            Event::VoiceFinished { voice, at, reason } => self.finished(voice, at, reason, fx),
            Event::VoiceRejected { voice, at, .. } => {
                let Some((id, _)) = self.resolve(voice) else {
                    return;
                };
                let Some(r) = self.remove(id) else { return };
                // A refused start: the voice never played (M4.14). Decided by the id the mixer
                // refused, not by whether a resume was sent since (a start refused while a
                // pause and resume were on their way is still a start that never played).
                fx.feedback.push(failure(
                    r.cue,
                    r.run,
                    BrokenReason::VoiceRejected,
                    r.start_id == Some(voice),
                ));
                // A refused resume leaves the voice paused under its old id: let it go.
                if voice == id
                    && let Some(old) = r.prev_id
                {
                    fx.commands.push((
                        at,
                        CommandKind::StopVoice {
                            voice: old,
                            fade: Fade::NONE,
                        },
                    ));
                }
            }
            Event::DevampScheduled { voice, at } => {
                if let Some((cue, run)) = self.name(voice) {
                    fx.feedback.push(Input::DevampScheduled { cue, run, at });
                    // The voice's end is known again: where it leaves the loop plus what
                    // follows (nothing for a devamp stop).
                    if let Some(r) = self.recs.get(&voice)
                        && !r.preview
                        && let Some(frames) = r.length_after_devamp(at)
                    {
                        fx.feedback.push(Input::ActionLength { cue, run, frames });
                    }
                }
            }
            Event::DevampIgnored { voice, .. } => {
                if let Some((cue, run)) = self.name(voice) {
                    fx.feedback.push(Input::DevampIgnored { cue, run });
                }
            }
            Event::Underrun { voice, .. } => {
                // A starved voice does not skip what it missed (docs/audio.md, Voices): it ends
                // later than planned by the underrun, while the cue engine keeps the planned end.
                if let Some(cue) = self.cue_of(voice) {
                    fx.notices.push((
                        "underrun",
                        "the disk could not keep up with a cue; it plays late from here".to_owned(),
                        Some(cue),
                    ));
                }
            }
            Event::MoveEnded { voice, moves, .. } => {
                // Settled after the last move sent: no crossfade plays out of a former slot,
                // so they can be reused. A move sent since is still on its way (the counts
                // differ) and its crossfade will be reported in turn; a lost report keeps
                // them until the voice ends.
                if let Some((id, _)) = self.resolve(voice)
                    && let Some(r) = self.recs.get_mut(&id)
                    && r.moves == moves
                {
                    r.former.clear();
                }
            }
            Event::PendingFull { .. }
            | Event::LoopReached { .. }
            | Event::SliceEnded { .. }
            | Event::StageFaulted { .. } => {}
        }
    }

    /// The live record an id (current or alias) belongs to, and whether it is the current id.
    fn resolve(&self, voice: VoiceId) -> Option<(VoiceId, bool)> {
        if self.recs.contains_key(&voice) {
            return Some((voice, true));
        }
        self.aliases.get(&voice).map(|&(cur, _)| (cur, false))
    }

    /// The cue and run an id stood for (an alias keeps its old run, so its feedback is stale).
    fn name(&self, voice: VoiceId) -> Option<(CueId, RunId)> {
        if let Some(r) = self.recs.get(&voice) {
            return Some((r.cue, r.run));
        }
        let &(cur, run) = self.aliases.get(&voice)?;
        self.recs.get(&cur).map(|r| (r.cue, run))
    }

    pub(super) fn finished(
        &mut self,
        voice: VoiceId,
        at: SampleTime,
        reason: FinishReason,
        fx: &mut Effects,
    ) {
        let Some((id, current)) = self.resolve(voice) else {
            return;
        };
        let Some(r) = self.recs.get_mut(&id) else {
            return;
        };
        let (cue, run) = (r.cue, r.run);
        // The mixer dropped the voice because its render kept panicking: the cue breaks, also
        // when a resume that re-keyed it is still on its way.
        let faulted = reason == FinishReason::Faulted;
        if !current && !faulted {
            // The voice ended before the resume that re-keyed it applied, so the mixer ignored
            // the resume: report the end with the fresh run (M2-E (a)), after the resume.
            if let Some(r) = self.remove(id) {
                let at = at.max(r.resumed_at.unwrap_or(at));
                fx.deferred.push((at, Input::ActionEnded { cue, run, at }));
            }
            return;
        }
        let failed = faulted
            || r.stream_failed
            || (reason == FinishReason::EndOfStream
                && r.stream.as_ref().is_some_and(|h| h.error().is_some()));
        // Ended after a pending pause: the engine ignores the report (a paused voice cannot
        // end), so keep the record for the resume to report the end afresh. An end on the pause
        // sample left nothing to pause: the engine takes it as the end (M2.34) and resumes
        // nothing, so a record kept for it would stay for the session (and, for a show cue,
        // keep the cue from being primed again).
        if !failed
            && let Some(p) = r.paused_at
            && at > p
            && reason != FinishReason::Stopped
        {
            r.phase = Phase::Ended;
            r.stream = None;
            fx.feedback.push(Input::ActionEnded { cue, run, at });
            return;
        }
        let gone = self.remove(id);
        if failed {
            let input = gone.map_or(
                Input::ActionFailed {
                    cue,
                    run,
                    reason: BrokenReason::StreamFailed,
                },
                |r| r.failure(BrokenReason::StreamFailed),
            );
            fx.feedback.push(input);
            if faulted {
                fx.notices.push((
                    "audio_fault",
                    "a cue's audio kept failing while it played and was stopped".to_owned(),
                    Some(cue),
                ));
            }
        } else {
            fx.feedback.push(Input::ActionEnded { cue, run, at });
        }
    }

    /// Ids that left since the last meter read, with their cues. Cleared by the caller's read.
    pub fn take_gone(&mut self) -> Vec<(VoiceId, CueId)> {
        std::mem::take(&mut self.gone)
    }

    /// Whether a mixer event of `voice` could still mean anything here (a live record or an
    /// alias of one): an id this returns false for is never looked up again.
    pub fn knows(&self, voice: VoiceId) -> bool {
        self.resolve(voice).is_some()
    }

    /// The cue a mixer voice id belongs to (current or alias).
    pub fn cue_of(&self, voice: VoiceId) -> Option<CueId> {
        self.resolve(voice)
            .and_then(|(id, _)| self.recs.get(&id))
            .map(|r| r.cue)
    }

    /// Started voices the mixer should be playing, with their start sample.
    pub fn started(&self) -> impl Iterator<Item = (VoiceId, SampleTime)> + '_ {
        self.recs
            .iter()
            .filter(|(_, r)| matches!(r.phase, Phase::Playing))
            .map(|(id, r)| (*id, r.start_at))
    }

    /// A voice the mixer no longer has although no end was reported (its event was dropped):
    /// reported as ended on `at` (or failed, if its stream failed).
    /// A voice no meter read ever listed was never taken by the mixer: its lost report was a
    /// rejection, so it fails (`voice_rejected`) rather than ends.
    pub fn lost(&mut self, id: VoiceId, at: SampleTime, fx: &mut Effects) {
        let Some(r) = self.recs.get(&id) else { return };
        if !matches!(r.phase, Phase::Playing) {
            return;
        }
        if r.listed {
            self.finished(id, at, FinishReason::EndOfStream, fx);
        } else if let Some(r) = self.remove(id) {
            // Never listed: the mixer never took its start, so it never played.
            fx.feedback.push(Input::StartFailed {
                cue: r.cue,
                run: r.run,
                reason: BrokenReason::VoiceRejected,
            });
        }
    }

    pub fn mark_absent(&mut self, id: VoiceId, absent: bool) -> u8 {
        match self.recs.get_mut(&id) {
            Some(r) if absent => {
                r.absent = r.absent.saturating_add(1);
                r.absent
            }
            Some(r) => {
                r.absent = 0;
                r.listed = true;
                0
            }
            None => 0,
        }
    }

    /// When a stop dated `at` is sent to voice `id`. While the device is down (`floor`: the
    /// first sample the mixer renders when it is back), `now` is a block past that sample, so
    /// a stop dated `at` would let the voice play one more block after the outage: it is dated
    /// on the floor instead, but never before the voice's own (re)start, which may itself be
    /// queued after the floor.
    pub fn stop_date(&self, id: VoiceId, at: SampleTime, floor: Option<SampleTime>) -> SampleTime {
        let Some(floor) = floor else { return at };
        let Some(r) = self.recs.get(&id) else {
            return at;
        };
        let earliest = floor.max(r.start_at).max(r.resumed_at.unwrap_or(0));
        at.min(earliest)
    }

    /// The mixer disabled patch slot `slot` after repeated faults: every voice playing through
    /// it is stopped (no fade; dated no earlier than its own start or resume, so it cannot start
    /// after the stop) and its cue breaks as a faulted voice does ([`Self::finished`]).
    pub fn fail_slot(&mut self, slot: u16, now: SampleTime, fx: &mut Effects) {
        let on: Vec<(VoiceId, Option<VoiceId>, SampleTime)> = self
            .recs
            .iter()
            .filter(|(_, r)| matches!(r.phase, Phase::Playing) && r.slot == Some(slot))
            .map(|(&id, r)| {
                let at = now.max(r.start_at).max(r.resumed_at.unwrap_or(0));
                (id, r.prev_id, at)
            })
            .collect();
        for (id, prev, at) in on {
            // A resume still on its way: the mixer may know the voice by its old id.
            for voice in std::iter::once(id).chain(prev) {
                let fade = Fade::NONE;
                fx.commands
                    .push((at, CommandKind::StopVoice { voice, fade }));
            }
            self.finished(id, at, FinishReason::Faulted, fx);
        }
    }
}
