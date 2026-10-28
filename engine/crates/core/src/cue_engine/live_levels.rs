// SPDX-License-Identifier: GPL-3.0-or-later

//! Level edits on a playing audio cue (AUD-06, AUD-08, M5.9b): an edit of the levels, mutes or
//! crosspoints of an audio cue whose voice has started reaches the voice as short `Ramp`s, one
//! per changed gain, the way Fade cues reach it. Rules: `docs/cue-engine.md`, "Level edits on a
//! playing cue".
//!
//! The diff is made here, from the levels the voice was last given (kept on its run) and the
//! model's, so the binary carries out `Ramp` actions without knowing why they came; which
//! levels a running fade owns is the engine's knowledge too.

use crate::cue::{CueKind, FadeCurve};

use super::level::{self, LevelEdit};
use super::run::{Slot, Voice};
use super::types::Action;
use super::{Ctx, CueEngine};

impl CueEngine {
    /// Sends every playing audio cue's voice the gains an edit changed since it was last given
    /// its levels. A gain a fade is moving (or holds, paused) stays the fade's, unless the edit
    /// mutes it; any other gain follows the edit and is the model's again. Work per call is one
    /// comparison per started audio voice, plus the changed gains.
    pub(super) fn follow_level_edits(&mut self, cx: &mut Ctx) {
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
            let Some(given) = run.given.as_ref() else {
                continue;
            };
            let cue = if duplicates {
                ws.find_cue(*id)
            } else {
                tree.locate(ws, *id)
            };
            let Some(CueKind::Audio { levels, .. }) = cue.map(|c| &c.kind) else {
                continue;
            };
            if levels == given {
                continue;
            }
            let edits = level::level_edits(given, levels);
            run.given = Some(levels.clone());
            for LevelEdit { param, gain, muted } in edits {
                if let Some(held) = run.levels.get(&param) {
                    if held.owner.is_some() && !muted {
                        continue;
                    }
                    // A fade set it (and is done with it) or a mute takes it from a fade: the
                    // model rules it again, as an untouched level.
                    run.levels.remove(&param);
                }
                let at = run.commit(cx.now.max(started), cx.horizon);
                cx.out.push(Action::Ramp {
                    cue: *id,
                    run: run.run,
                    at,
                    param,
                    gain,
                    // On the voice's own start sample nothing has been heard yet.
                    frames: if at <= first { 0 } else { frames },
                    curve: FadeCurve::Linear,
                });
            }
        }
    }
}
