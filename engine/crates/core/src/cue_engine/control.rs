// SPDX-License-Identifier: GPL-3.0-or-later

//! Control cues (CT-11..CT-17, CT-19): Start, Stop, Pause, Load, Reset, Devamp, GoTo, Arm and
//! Disarm act on their target as the operator would, at their action start. Rules:
//! `docs/cue-engine.md`, "Control cues".

use crate::cue::{Cue, CueKind, GroupMode};
use crate::model::{CueId, CueListId};
use crate::tree;

use super::playhead;
use super::run::{DevampWait, Phase, Run, Slot, Voice};
use super::time::SampleTime;
use super::types::{Action, EngineWarning, GoIgnoredReason, RunId};
use super::{Ctx, CueEngine};

impl CueEngine {
    /// The action of control cue `cue` starts on `at`: it acts on its target. A missing target
    /// only warns.
    pub(super) fn control(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        let ws = cx.ws;
        let Some(model) = ws.find_cue(cue) else {
            return;
        };
        let Some(target) = model.kind.target() else {
            return cx.warn(EngineWarning::NoTarget { cue });
        };
        if ws.find_cue(target).is_none() {
            return cx.warn(EngineWarning::TargetMissing { cue, target });
        }
        match &model.kind {
            // Dated on the action start, like any trigger, so a late tick keeps later timing.
            CueKind::Start { .. } => self.start(target, at, cx),
            CueKind::Stop { fade_out, .. } => {
                let fade = *fade_out;
                self.as_of(at, cx, |e, cx| e.stop_tree(target, Some(fade), cx));
            }
            CueKind::Pause { .. } => self.as_of(at, cx, |e, cx| e.pause_tree(target, cx)),
            CueKind::Load { load_to_time, .. } => {
                let time = *load_to_time;
                self.as_of(at, cx, |e, cx| e.load_tree(target, time, cx));
            }
            CueKind::Reset { .. } => self.as_of(at, cx, |e, cx| e.reset_tree(target, cx)),
            CueKind::Devamp {
                start_next_cue,
                stop_target_when_slice_ends,
                ..
            } => self.devamp(
                cue,
                target,
                *start_next_cue,
                *stop_target_when_slice_ends,
                at,
                cx,
            ),
            CueKind::GoTo { then_go, .. } => self.go_to(target, *then_go, at, cx),
            CueKind::Arm { .. } => self.set_armed(target, true, cx),
            CueKind::Disarm { .. } => self.set_armed(target, false, cx),
            CueKind::Audio { .. }
            | CueKind::Fade { .. }
            | CueKind::Group { .. }
            | CueKind::Wait { .. }
            | CueKind::Memo
            | CueKind::Network { .. } => {}
        }
    }

    /// Carries out an operation as if it came on sample `at` (never earlier than `now`): a
    /// control cue whose action starts inside the lookahead window acts on its own sample.
    pub(super) fn as_of(
        &mut self,
        at: SampleTime,
        cx: &mut Ctx,
        f: impl FnOnce(&mut Self, &mut Ctx),
    ) {
        let now = cx.now;
        cx.now = now.max(at);
        f(self, cx);
        cx.now = now;
    }

    /// Whether a cue counts as armed: an Arm/Disarm cue's change wins for the rest of the call
    /// that made it (the binary applies it to the model before the next call).
    pub(super) fn is_armed(&self, cue: &Cue) -> bool {
        self.armed.get(&cue.id).copied().unwrap_or(cue.armed)
    }

    fn set_armed(&mut self, target: CueId, armed: bool, cx: &mut Ctx) {
        let Some(model) = cx.ws.find_cue(target) else {
            return;
        };
        if self.is_armed(model) == armed {
            return;
        }
        if model.armed == armed {
            self.armed.remove(&target);
        } else {
            self.armed.insert(target, armed);
        }
        cx.out.push(Action::SetArmed { cue: target, armed });
    }

    /// A new call: the binary has applied the last call's `SetArmed` changes to the model
    /// (M4.2 contract), so the model rules again, including any edit made since.
    pub(super) fn sync_armed(&mut self) {
        self.armed.clear();
    }

    /// GoTo: the playhead of the target's list moves to the target; with "and GO", a GO
    /// follows there (not an operator GO: no double-GO check, and it does not restart the
    /// double-GO interval).
    fn go_to(&mut self, target: CueId, then_go: bool, at: SampleTime, cx: &mut Ctx) {
        let ws = cx.ws;
        let Some(list) = tree::find_path(ws, target)
            .and_then(|path| ws.cue_lists.get(path.list))
            .map(|l| l.id)
        else {
            return;
        };
        self.move_playhead(list, Some(target), cx);
        if then_go && !self.go_on(list, at, cx) {
            cx.out.push(Action::GoIgnored {
                list,
                reason: GoIgnoredReason::NoCue,
            });
        }
    }

    /// Starts the list's playhead cue on `at` and moves the playhead past its continue chain.
    /// Returns `false` (doing nothing) if the playhead stands on no cue.
    pub(super) fn go_on(&mut self, list: CueListId, at: SampleTime, cx: &mut Ctx) -> bool {
        let ws = cx.ws;
        let Some(cues) = ws.cue_lists.iter().find(|l| l.id == list).map(|l| &l.cues) else {
            return false;
        };
        let Some(path) = self
            .playhead(list)
            .and_then(|id| playhead::locate(cues, id))
        else {
            return false;
        };
        let Some(cue) = path.last().and_then(|&(level, index)| level.get(index)) else {
            return false;
        };
        self.start(cue.id, at, cx);
        // The playhead skips the rest of the cue's continue chain, as the model stands now, or
        // enters a "start first and enter" group.
        let next = playhead::after_go(&path, |c| self.is_armed(c), |c| self.has_problems(c.id));
        self.move_playhead(list, next, cx);
        true
    }

    /// Devamp: an audio target's voice leaves its loop (the audio side answers); a looping
    /// playlist finishes its current child and then completes.
    fn devamp(
        &mut self,
        by: CueId,
        target: CueId,
        start_next: bool,
        stop: bool,
        at: SampleTime,
        cx: &mut Ctx,
    ) {
        let Some(model) = cx.ws.find_cue(target) else {
            return;
        };
        let not_running = EngineWarning::TargetNotRunning { cue: by, target };
        match &model.kind {
            CueKind::Audio { .. } => {
                let now = cx.now;
                let Some(Slot::Active(run)) = self.slots.get_mut(&target) else {
                    return cx.warn(not_running);
                };
                let playing = run.broken.is_none()
                    && run.paused_at.is_none()
                    && run.stop_end.is_none()
                    && matches!(run.phase, Phase::Action { .. });
                let Voice::Started { at: started, .. } = run.voice else {
                    return cx.warn(not_running);
                };
                if !playing {
                    return cx.warn(not_running);
                }
                // One devamp at a time: the audio side would ignore a second, and its answer
                // could not be told from the first one's.
                if run.devamp.is_some() {
                    return cx.warn(EngineWarning::DevampIgnored { cue: target });
                }
                let at = run.commit(at.max(now).max(started), cx.horizon);
                cx.out.push(Action::DevampVoice {
                    cue: target,
                    run: run.run,
                    at,
                    stop,
                });
                run.devamp = Some(DevampWait {
                    by,
                    start_next,
                    stop,
                    at: None,
                });
            }
            CueKind::Group { mode, .. } => {
                let looping = matches!(mode, GroupMode::Playlist { loop_: true, .. });
                match self.slots.get_mut(&target) {
                    Some(Slot::Active(Run {
                        group: Some(g),
                        phase: Phase::Action { .. },
                        broken: None,
                        ..
                    })) if !g.stopping => {
                        if looping && !g.adopted && g.playlist.is_some() && g.devamp.is_none() {
                            g.devamp = Some((by, start_next));
                        } else {
                            cx.warn(EngineWarning::DevampIgnored { cue: target });
                        }
                    }
                    _ => cx.warn(not_running),
                }
            }
            _ => cx.warn(EngineWarning::TargetWrongKind { cue: by, target }),
        }
    }

    /// Feedback: the devamp of this run's voice leaves the loop on `at`.
    pub(super) fn devamp_scheduled(&mut self, cue: CueId, run_id: RunId, at: SampleTime) {
        if let Some(Slot::Active(run)) = self.slots.get_mut(&cue)
            && run.run == run_id
            && let Some(d) = run.devamp.as_mut()
            && d.at.is_none()
        {
            d.at = Some(at);
        }
    }

    /// Feedback: the devamp of this run's voice was not carried out.
    pub(super) fn devamp_ignored(&mut self, cue: CueId, run_id: RunId, cx: &mut Ctx) {
        if let Some(Slot::Active(run)) = self.slots.get_mut(&cue)
            && run.run == run_id
            && run.devamp.take().is_some()
        {
            cx.warn(EngineWarning::DevampIgnored { cue });
        }
    }
}
