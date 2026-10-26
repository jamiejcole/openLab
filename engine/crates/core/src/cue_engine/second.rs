// SPDX-License-Identifier: GPL-3.0-or-later

//! Second triggers (M2.14, QLab 5): what a trigger does to a cue that is already active, by the
//! cue's `second_trigger` option, and the "second trigger on release" input. Rules:
//! `docs/cue-engine.md`, "Second trigger".

use crate::cue::SecondTrigger;
use crate::model::CueId;

use super::run::Slot;
use super::time::SampleTime;
use super::types::EngineWarning;
use super::{Ctx, CueEngine};

impl CueEngine {
    /// A trigger on `cue` at `at` while it is active (not broken, not fading out in a panic),
    /// with the cue's effective option. `Nothing` only reaches here for a cue that is not paused.
    pub(super) fn second_trigger(
        &mut self,
        cue: CueId,
        option: SecondTrigger,
        at: SampleTime,
        cx: &mut Ctx,
    ) {
        match option {
            SecondTrigger::Nothing => {
                let state = self.state(cue);
                cx.warn(EngineWarning::TriggerIgnored { cue, state });
            }
            SecondTrigger::Panic => self.as_of(at, cx, |e, cx| e.panic_tree(cue, cx)),
            // The two differ in QLab only for audio effects (a hard stop cuts their tails);
            // v0.0.1 has none.
            SecondTrigger::Stop | SecondTrigger::HardStop => {
                self.as_of(at, cx, |e, cx| e.stop_tree(cue, None, cx));
            }
            SecondTrigger::HardStopAndRestart => self.restart(cue, at, cx),
            SecondTrigger::Devamp => self.devamp(None, cue, false, false, at, cx),
            SecondTrigger::PlayNext => self.playlist_skip(cue, false, at, cx),
            SecondTrigger::PlayPrevious => self.playlist_skip(cue, true, at, cx),
        }
    }

    /// Hard-stops the cue (and everything in it) and starts it afresh on `at`: a new run, from
    /// the top of its pre-wait. Charged as a start, so a cycle of restarts ends at the chain limit.
    fn restart(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        if !self.budget_left(cx) {
            return cx.warn(EngineWarning::ChainLimit {
                cue,
                limit: cx.limit,
            });
        }
        let seq = match self.slots.get(&cue) {
            Some(Slot::Active(run)) => run.seq,
            _ => return,
        };
        self.as_of(at, cx, |e, cx| e.stop_tree(cue, None, cx));
        // The stop left the cue idle, so the start below is a fresh one and cannot come back
        // here. Should it not have (it cannot), nothing more is done rather than loop.
        if self.slots.contains_key(&cue) {
            return;
        }
        self.start(cue, at, cx);
        self.keep_playlist_place(cue, seq, cx);
    }

    /// `Input::TriggerReleased`: a second trigger if the cue asks for one on release and is
    /// active; ignored without a warning otherwise (also with the option `nothing`: a release
    /// never resumes a paused cue, and a cue fading out in a panic is not restarted by it).
    pub(super) fn trigger_released(&mut self, cue: CueId, cx: &mut Ctx) {
        let Some(model) = cx.ws.find_cue(cue) else {
            return;
        };
        if !model.second_trigger_on_release {
            return;
        }
        let option = model.second_trigger.effective(&model.kind);
        let active = matches!(
            self.slots.get(&cue),
            Some(Slot::Active(run)) if !run.is_broken() && !run.panicked
        );
        if active && option != SecondTrigger::Nothing {
            self.second_trigger(cue, option, cx.now, cx);
        }
    }
}
