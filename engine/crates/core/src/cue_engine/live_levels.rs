// SPDX-License-Identifier: GPL-3.0-or-later

//! Level edits on a playing audio cue (AUD-06, AUD-08, M5.9b): an edit of the levels, mutes or
//! crosspoints of an audio cue whose voice has started reaches the voice as short `Ramp`s, one
//! per changed gain, the way Fade cues reach it. Rules: `docs/cue-engine.md`, "Level edits on a
//! playing cue".
//!
//! The diff is made here, from the levels the voice was last given (kept on its run) and the
//! model's, so the binary carries out `Ramp` actions without knowing why they came; which
//! levels a running fade owns is the engine's knowledge too.

use std::collections::BTreeSet;

use crate::cue::{CueKind, FadeCurve};
use crate::model::CueId;

use super::level::{self, LevelEdit};
use super::run::{Phase, Slot, Voice};
use super::types::Action;
use super::{Ctx, CueEngine};

impl CueEngine {
    /// Sends every playing audio cue's voice the gains an edit changed since it was last given
    /// its levels; each follows the edit and is the model's again. An edit of a gain a running
    /// Fade cue is moving ends that Fade (QLab 5.5.3): it completes at once, holding the other
    /// levels it moves where they are (`complete_fades_now`); a paused Fade only loses the
    /// edited gain. Work per call is one comparison per started audio voice, plus the changed
    /// gains.
    pub(super) fn follow_level_edits(&mut self, cx: &mut Ctx) {
        let mut ended: BTreeSet<(CueId, u64)> = BTreeSet::new();
        let ws = cx.ws;
        let frames = level::edit_frames(self.config.sample_rate().get());
        let tree = self.problems.tree();
        let duplicates = tree.has_duplicates();
        for (id, slot) in &mut self.slots {
            let Slot::Active(run) = slot else { continue };
            let Voice::Started {
                at: started, first, ..
            } = run.voice
            else {
                continue;
            };
            if run.is_broken() {
                continue;
            }
            if run.given.is_none() {
                continue;
            }
            let cue = if duplicates {
                ws.find_cue(*id)
            } else {
                tree.locate(ws, *id)
            };
            let Some(CueKind::Audio { levels, .. }) = cue.map(|c| &c.kind) else {
                continue;
            };
            let Some(given) = run.given.as_mut() else {
                continue;
            };
            if *levels == given.model {
                continue;
            }
            given.model = levels.clone();
            let edits = level::level_edits(&given.played, &level::played(levels));
            for LevelEdit { param, to } in edits {
                // A fade set it or is moving it: the edit wins, and the model rules it again,
                // as an untouched level.
                if let Some(held) = run.levels.remove(&param) {
                    ended.extend(held.owner);
                }
                if let Some(given) = run.given.as_mut() {
                    given.played.insert(param, to);
                }
                let at = run.commit(cx.now.max(started), cx.horizon);
                cx.out.push(Action::Ramp {
                    cue: *id,
                    run: run.run,
                    at,
                    param,
                    gain: to.0,
                    // On the voice's own start sample nothing has been heard yet.
                    frames: if at <= first { 0 } else { frames },
                    curve: FadeCurve::Linear,
                });
            }
        }
        self.complete_fades_now(&ended, cx);
    }

    /// Each Fade cue in `fades` (cue, run) that is running and not paused completes now, as on
    /// its end (its auto-follow fires), except that the levels it is still moving are held where
    /// they are, as for a Fade whose duration was shortened to now, and its "stop target when
    /// done" does not apply: an operator nudging a fader during a fade-out must not cut the sound.
    fn complete_fades_now(&mut self, fades: &BTreeSet<(CueId, u64)>, cx: &mut Ctx) {
        let mut any = false;
        for (cue, seq) in fades {
            if let Some(Slot::Active(run)) = self.slots.get_mut(cue)
                && run.seq == *seq
                && run.paused_at.is_none()
                && run.stop_end.is_none()
                && let (
                    Some(fade),
                    Phase::Action {
                        start,
                        end: Some(_),
                    },
                ) = (run.fade.as_mut(), run.phase)
            {
                fade.retimed = true;
                // The edit is the operator taking the level over: the target plays on.
                fade.stop_target = None;
                run.phase = Phase::Action {
                    start,
                    end: Some(start.max(cx.now)),
                };
                any = true;
            }
        }
        if any {
            self.advance(cx);
        }
    }
}
