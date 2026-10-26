// SPDX-License-Identifier: GPL-3.0-or-later

//! Second triggers (M2.14, QLab 5): what a trigger does to a cue that is already active, by the
//! cue's `second_trigger` option, and the "second trigger on release" input. Rules:
//! `docs/cue-engine.md`, "Second trigger".

use crate::cue::{CueKind, GroupMode, SecondTrigger};
use crate::model::CueId;

use super::run::{Phase, Slot, Voice};
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

    /// Whether `option` has something to act on in the paused `cue` (at `at`). If not, the
    /// trigger follows the `nothing` rule and resumes it, as it would any paused cue.
    pub(super) fn acts_on_paused(
        &self,
        cue: CueId,
        option: SecondTrigger,
        at: SampleTime,
        cx: &Ctx,
    ) -> bool {
        let Some(Slot::Active(run)) = self.slots.get(&cue) else {
            return false;
        };
        let in_action = matches!(run.phase, Phase::Action { .. }) && run.stop_end.is_none();
        let kind = cx.ws.find_cue(cue).map(|c| &c.kind);
        let playing_playlist = run.group.as_ref().is_some_and(|g| {
            !g.stopping
                && !g.adopted
                && g.devamp.is_none()
                && g.playlist.is_some()
                && g.skipped != Some(at)
        });
        match option {
            SecondTrigger::Nothing => false,
            SecondTrigger::Devamp => {
                in_action
                    && match kind {
                        Some(CueKind::Audio { .. }) => {
                            matches!(run.voice, Voice::Started { .. }) && run.devamp.is_none()
                        }
                        Some(CueKind::Group {
                            mode: GroupMode::Playlist { loop_: true, .. },
                            ..
                        }) => playing_playlist,
                        _ => false,
                    }
            }
            SecondTrigger::PlayNext | SecondTrigger::PlayPrevious => in_action && playing_playlist,
            SecondTrigger::HardStopAndRestart => run.started != at,
            SecondTrigger::Panic | SecondTrigger::Stop | SecondTrigger::HardStop => true,
        }
    }

    /// Hard-stops the cue (and everything in it) and starts it afresh on `at`: a new run, from
    /// the top of its pre-wait. Charged as a start. At most once per sample: a run triggered on
    /// `at` (also one this restart made) ignores it, so a trigger chain from the cue back to
    /// itself restarts it once, not until the chain limit.
    fn restart(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        let (seq, started, state) = match self.slots.get(&cue) {
            Some(Slot::Active(run)) => (run.seq, run.started, run.state()),
            _ => return,
        };
        if started == at {
            return cx.warn(EngineWarning::TriggerIgnored { cue, state });
        }
        if !self.budget_left(cx) {
            return cx.warn(EngineWarning::ChainLimit {
                cue,
                limit: cx.limit,
            });
        }
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
