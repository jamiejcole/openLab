// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

impl Voices {
    /// Runs whose voice is on the device: started in the mixer, or started and waiting for
    /// their file's probe. Loaded runs are not (a load only prepares a stream).
    pub fn on_device(&self) -> Vec<(CueId, RunId)> {
        let mut v: Vec<(CueId, RunId)> = self
            .recs
            .values()
            .filter(|r| {
                matches!(
                    r.phase,
                    Phase::Playing | Phase::Waiting { start: Some(_), .. }
                )
            })
            .map(|r| (r.cue, r.run))
            .collect();
        v.sort();
        v
    }

    /// Mixer patch slots a voice in the mixer may still play through (started, paused or
    /// stopping; previews and orphans included): a slot is reused only once none does.
    pub fn slots_in_use(&self) -> BTreeSet<u16> {
        self.recs
            .values()
            .filter(|r| matches!(r.phase, Phase::Playing))
            .flat_map(|r| r.slot.iter().chain(&r.former).copied())
            .collect()
    }

    /// Moves every show voice whose cue now names another patch onto that patch's mixer slot,
    /// crossfading (`MoveVoice`; #293, docs/control.md "Patch slots"). Outputs the voice was
    /// started without (beyond the outputs of the patch it started on) that the new patch has
    /// get their levels first, with their crosspoints: the model's, except a level a Fade cue
    /// has set, which the voice already has (the fade's ramps reached it). A voice whose new
    /// patch has no slot (none free) stays where it is; a later sync moves it once it has one.
    /// Previews keep the patch they started on, as they keep their levels.
    pub fn follow_patches(
        &mut self,
        doc: super::super::DocId,
        ws: &Workspace,
        patches: &Patches,
        engine: &CueEngine,
        now: SampleTime,
    ) -> Vec<(SampleTime, CommandKind)> {
        let mut commands = Vec::new();
        for (&id, r) in &mut self.recs {
            if r.preview || !matches!(r.phase, Phase::Playing) {
                continue;
            }
            let Some(CueKind::Audio { patch, levels, .. }) = ws.find_cue(r.cue).map(|c| &c.kind)
            else {
                continue;
            };
            if r.patch == Some(*patch) {
                continue;
            }
            let Some(slot) = patches.mixer_patch(doc, *patch) else {
                continue;
            };
            // Not before the voice's own (re)start, which may still be queued in the mixer.
            let at = now.max(r.start_at).max(r.resumed_at.unwrap_or(0));
            let outputs = patches.outputs(doc, *patch);
            if outputs > r.levelled {
                let faded: BTreeSet<LevelParam> = engine
                    .fade_ramps(r.cue, at)
                    .iter()
                    .map(|l| l.param)
                    .collect();
                let new = r.levelled..outputs;
                let mut give = |param: LevelParam, gain: f32| {
                    if gain != 0.0 && !faded.contains(&param) {
                        let target = target(param);
                        let fade = Fade::NONE;
                        commands.push((
                            at,
                            CommandKind::Ramp {
                                voice: id,
                                target,
                                gain,
                                fade,
                            },
                        ));
                    }
                };
                for (o, out) in levels.outputs.iter().enumerate() {
                    if new.contains(&o) && !out.mute {
                        let o = u32::try_from(o).unwrap_or(u32::MAX);
                        give(LevelParam::Output(o), db_to_gain(out.db));
                    }
                }
                for xp in &levels.crosspoints {
                    let output = usize::try_from(xp.output).unwrap_or(usize::MAX);
                    if new.contains(&output) && !xp.mute {
                        let param = LevelParam::Crosspoint {
                            input: xp.input,
                            output: xp.output,
                        };
                        give(param, db_to_gain(xp.db));
                    }
                }
                r.levelled = outputs;
            }
            if let Some(old) = r.slot.filter(|&s| s != slot)
                && !r.former.contains(&old)
            {
                r.former.push(old);
            }
            r.slot = Some(slot);
            r.patch = Some(*patch);
            r.moves = r.moves.wrapping_add(1);
            commands.push((
                at,
                CommandKind::MoveVoice {
                    voice: id,
                    patch: slot,
                },
            ));
        }
        commands
    }

    /// Every voice started in the mixer (previews included).
    pub fn playing_ids(&self) -> Vec<VoiceId> {
        let mut v: Vec<VoiceId> = self
            .recs
            .iter()
            .filter(|(_, r)| matches!(r.phase, Phase::Playing))
            .map(|(id, _)| *id)
            .collect();
        v.sort_by_key(|id| id.0);
        v
    }

    /// Show voices in the mixer (not stopped by a command already) for which `orphan(cue, run)`
    /// holds.
    pub fn orphans(&self, orphan: impl Fn(CueId, RunId) -> bool) -> Vec<VoiceId> {
        self.recs
            .iter()
            .filter(|(_, r)| matches!(r.phase, Phase::Playing) && !r.preview)
            .filter(|(_, r)| orphan(r.cue, r.run))
            .map(|(id, _)| *id)
            .collect()
    }

    /// Fails starts that have waited for their file's probe since before `now - limit`: the
    /// cue goes broken instead of showing as running without a sound.
    pub fn expire_held(&mut self, now: SampleTime, limit: SampleTime, fx: &mut Effects) {
        let expired: Vec<VoiceId> = self
            .recs
            .iter()
            .filter(|(_, r)| r.held.is_some_and(|t| now >= t.saturating_add(limit)))
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            if let Some(r) = self.remove(id) {
                // A held start never reached the mixer: nothing played.
                fx.feedback.push(Input::StartFailed {
                    cue: r.cue,
                    run: r.run,
                    reason: BrokenReason::StreamFailed,
                });
                fx.notices.push((
                    "probe_timeout",
                    "a cue's audio file could not be read in time; the cue did not play".to_owned(),
                    Some(r.cue),
                ));
            }
        }
    }

    /// Show cues playing on rendered sample `t`: position in the action and frames left.
    pub fn progress(&self, t: SampleTime) -> Vec<(CueId, u64, Option<u64>)> {
        let mut v: Vec<(CueId, u64, Option<u64>)> = self
            .recs
            .values()
            .filter(|r| !r.preview)
            .filter_map(|r| {
                let pos = r.position(t)?;
                let left = r
                    .total
                    .map(|total| r.offset.saturating_add(total).saturating_sub(pos));
                Some((r.cue, pos, left))
            })
            .collect();
        v.sort_by_key(|(c, _, _)| *c);
        v
    }

    /// Every voice is gone and nothing is held for one: a stopped show leaks nothing.
    pub fn is_idle(&self) -> bool {
        self.recs.is_empty() && self.by_run.is_empty() && self.aliases.is_empty()
    }

    /// Forgets every voice, alias and prime (a new workspace replaces the engine they belong
    /// to). The mixer is stopped separately; its late events find no record and are ignored. Ids
    /// are never reused, so they cannot be taken for the new workspace's voices.
    pub fn forget_all(&mut self) {
        self.recs.clear();
        self.by_run.clear();
        self.aliases.clear();
        self.primes.clear();
        self.gone.clear();
    }

    /// Lets go of every primed stream (a panic, shutdown).
    pub fn clear_primes(&mut self) {
        self.primes.clear();
    }

    pub fn has_playing(&self) -> bool {
        self.recs
            .values()
            .any(|r| matches!(r.phase, Phase::Playing))
    }

    /// Stream handles of playing voices, with the frames each voice will have read by rendered
    /// sample `t` (offline settling).
    pub fn stream_needs(&self, t: SampleTime) -> impl Iterator<Item = (&StreamHandle, u64)> {
        self.recs.values().filter_map(move |r| {
            let h = r.stream.as_ref()?;
            let pos = r.position(t)?;
            Some((h, pos.saturating_sub(r.offset)))
        })
    }

    /// Streams prepared but not playing yet (loads and primes).
    pub fn waiting_streams(&self) -> impl Iterator<Item = &StreamHandle> {
        self.recs
            .values()
            .filter_map(|r| match &r.phase {
                Phase::Waiting {
                    prepared: Some((p, _)),
                    ..
                } => Some(&p.handle),
                _ => None,
            })
            .chain(self.primes.values().map(|p| &p.prepared.handle))
    }
}
