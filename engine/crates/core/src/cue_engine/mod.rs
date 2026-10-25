// SPDX-License-Identifier: GPL-3.0-or-later

//! The cue engine: cue lifecycle, timing, continue modes, GO and playheads (ARCHITECTURE §3.3).
//!
//! Pure and deterministic: [`CueEngine::handle`] takes the current workspace, one [`Input`] and
//! the sample time, and returns the [`Action`]s to carry out. Semantics are specified in
//! `docs/cue-engine.md`.

mod playhead;
mod run;
mod time;
mod types;

use std::collections::{BTreeMap, HashSet};

pub use time::*;
pub use types::*;

use crate::cue::{ContinueMode, Cue, CueKind, FadeSpec};
use crate::model::{CueId, CueListId, Workspace};
use crate::tree;

use playhead::Playhead;
use run::{Loaded, Phase, Run, Slot, Voice};

/// Cue starts allowed per call on top of the workspace's cue count. Without trigger cycles a call
/// starts each cue at most once, so only a cycle reaches the limit.
pub const CHAIN_MARGIN: usize = 64;

/// Runtime state of every cue and playhead of one workspace.
#[derive(Clone, Debug)]
pub struct CueEngine {
    config: EngineConfig,
    /// Ordered maps so every scan, and so the order of actions, is deterministic.
    slots: BTreeMap<CueId, Slot>,
    playheads: BTreeMap<CueListId, Playhead>,
    last_go: Option<SampleTime>,
    next_run: u64,
    next_seq: u64,
}

/// One `handle` call's inputs and outputs.
struct Ctx<'w> {
    ws: &'w Workspace,
    now: SampleTime,
    /// Deadlines up to here are carried out in this call.
    horizon: SampleTime,
    /// Cue starts left in this call.
    budget: usize,
    limit: usize,
    out: Vec<Action>,
}

impl Ctx<'_> {
    fn warn(&mut self, warning: EngineWarning) {
        self.out.push(Action::Warning(warning));
    }

    fn secs(&self, seconds: f64, config: &EngineConfig) -> SampleTime {
        seconds_to_samples(seconds, config.sample_rate)
    }
}

impl CueEngine {
    /// An engine with every cue idle. Playheads appear on the first `handle` call.
    pub fn new(config: EngineConfig) -> Self {
        Self {
            config,
            slots: BTreeMap::new(),
            playheads: BTreeMap::new(),
            last_go: None,
            next_run: 1,
            next_seq: 0,
        }
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// Applies one input at scheduling time `now` (the earliest sample a command can still hit).
    /// `ws` is the current workspace; edits since the last call are taken into account first.
    pub fn handle(&mut self, ws: &Workspace, input: Input, now: SampleTime) -> Vec<Action> {
        let limit = Self::chain_limit(ws);
        let mut cx = Ctx {
            ws,
            now,
            horizon: now.saturating_add(self.config.lookahead),
            budget: limit,
            limit,
            out: Vec::new(),
        };
        self.sync_model(&mut cx);
        self.sync_playheads(&mut cx);
        self.advance(&mut cx);
        self.apply(input, &mut cx);
        self.advance(&mut cx);
        cx.out
    }

    /// The earliest sample at which a timer of a cue expires, if any.
    pub fn next_deadline(&self) -> Option<SampleTime> {
        self.slots
            .values()
            .filter_map(|slot| match slot {
                Slot::Active(run) => run.next_deadline(),
                Slot::Loaded(_) | Slot::Broken(_) => None,
            })
            .min()
    }

    pub fn state(&self, cue: CueId) -> CueState {
        self.slots.get(&cue).map_or(CueState::Idle, Slot::state)
    }

    /// Every cue that is not idle, in id order.
    pub fn cue_states(&self) -> impl Iterator<Item = (CueId, CueState)> + '_ {
        self.slots.iter().map(|(id, slot)| (*id, slot.state()))
    }

    /// Why a cue is broken, if it is.
    pub fn broken_reason(&self, cue: CueId) -> Option<BrokenReason> {
        match self.slots.get(&cue)? {
            Slot::Broken(reason) => Some(*reason),
            Slot::Loaded(_) | Slot::Active(_) => None,
        }
    }

    /// The run of a cue that is loaded or running.
    pub fn run_of(&self, cue: CueId) -> Option<RunId> {
        match self.slots.get(&cue)? {
            Slot::Loaded(loaded) => Some(loaded.run),
            Slot::Active(run) => Some(run.run),
            Slot::Broken(_) => None,
        }
    }

    /// The list's playhead: the standing-by cue's id, `None` past the end or for an empty list.
    pub fn playhead(&self, list: CueListId) -> Option<CueId> {
        self.playheads.get(&list)?.cue
    }

    /// The standing-by cue of a list (CL-06), looked up in `ws`.
    pub fn standing_by<'w>(&self, ws: &'w Workspace, list: CueListId) -> Option<&'w Cue> {
        let id = self.playhead(list)?;
        ws.cue_lists
            .iter()
            .find(|l| l.id == list)?
            .cues
            .iter()
            .find(|c| c.id == id)
    }

    /// Most cue starts one call carries out: the workspace's cue count plus [`CHAIN_MARGIN`].
    pub fn chain_limit(ws: &Workspace) -> usize {
        ws.cue_lists
            .iter()
            .map(|l| count_cues(&l.cues))
            .sum::<usize>()
            .saturating_add(CHAIN_MARGIN)
    }

    fn apply(&mut self, input: Input, cx: &mut Ctx) {
        match input {
            Input::Tick => {}
            Input::Go { list } => self.go(list, cx),
            Input::Start { cue } => {
                if self.known(cue, cx) {
                    self.start(cue, cx.now, cx);
                }
            }
            Input::Stop { cue, fade_out } => {
                if self.known(cue, cx) {
                    self.stop(cue, fade_out, cx);
                }
            }
            Input::Pause { cue } => {
                if self.known(cue, cx) {
                    self.pause(cue, cx);
                }
            }
            Input::Resume { cue } => {
                if self.known(cue, cx) {
                    self.resume(cue, cx.now, cx);
                }
            }
            Input::Load { cue, time } => {
                if self.known(cue, cx) {
                    self.load(cue, time, cx);
                }
            }
            Input::Reset { cue } => {
                if self.known(cue, cx) {
                    self.reset(cue, cx);
                }
            }
            Input::PlayheadNext { list } => self.navigate(list, cx, Playhead::next),
            Input::PlayheadPrevious { list } => self.navigate(list, cx, Playhead::previous),
            Input::SetPlayhead { list, cue } => self.set_playhead(list, cue, cx),
            Input::SetPlayheadByNumber { list, number } => {
                self.set_playhead_by_number(list, number, cx)
            }
            Input::ActionEnded { cue, run, at } => self.action_ended(cue, run, at, cx),
            Input::ActionFailed { cue, run, reason } => self.action_failed(cue, run, reason, cx),
        }
    }

    fn known(&self, cue: CueId, cx: &mut Ctx) -> bool {
        let known = cx.ws.find_cue(cue).is_some();
        if !known {
            cx.warn(EngineWarning::UnknownCue { cue });
        }
        known
    }

    // ----- model edits -----

    /// Stops and forgets cues that are no longer in the workspace.
    fn sync_model(&mut self, cx: &mut Ctx) {
        if self.slots.is_empty() {
            return;
        }
        let mut present = HashSet::new();
        for list in &cx.ws.cue_lists {
            collect_ids(&list.cues, &mut present);
        }
        let gone: Vec<CueId> = self
            .slots
            .keys()
            .filter(|id| !present.contains(*id))
            .copied()
            .collect();
        for cue in gone {
            let before = self.state(cue);
            if let Some(slot) = self.slots.remove(&cue) {
                release(cue, &slot, cx);
                if matches!(slot, Slot::Active(_)) {
                    cx.warn(EngineWarning::CueRemoved { cue });
                }
            }
            self.report(cue, before, cx.now, cx);
        }
    }

    fn sync_playheads(&mut self, cx: &mut Ctx) {
        self.playheads
            .retain(|id, _| cx.ws.cue_lists.iter().any(|l| l.id == *id));
        for list in &cx.ws.cue_lists {
            match self.playheads.get_mut(&list.id) {
                Some(playhead) => {
                    if playhead.sync(&list.cues) {
                        cx.out.push(Action::PlayheadChanged {
                            list: list.id,
                            cue: playhead.cue,
                        });
                    }
                }
                None => {
                    let playhead = Playhead::new(&list.cues);
                    cx.out.push(Action::PlayheadChanged {
                        list: list.id,
                        cue: playhead.cue,
                    });
                    self.playheads.insert(list.id, playhead);
                }
            }
        }
    }

    // ----- GO and playheads -----

    fn go(&mut self, list: CueListId, cx: &mut Ctx) {
        let ignored = |reason, cx: &mut Ctx| cx.out.push(Action::GoIgnored { list, reason });
        let Some(cues) = cx
            .ws
            .cue_lists
            .iter()
            .find(|l| l.id == list)
            .map(|l| &l.cues)
        else {
            return ignored(GoIgnoredReason::UnknownList, cx);
        };
        let interval = cx.secs(cx.ws.settings.min_go_interval, &self.config);
        // A clock that went backwards (device restart) never blocks GO.
        if let Some(last) = self.last_go
            && cx.now >= last
            && cx.now - last < interval
        {
            return ignored(GoIgnoredReason::DoubleGo, cx);
        }
        let Some(index) = self
            .playhead(list)
            .and_then(|id| cues.iter().position(|c| c.id == id))
        else {
            return ignored(GoIgnoredReason::NoCue, cx);
        };
        let Some(cue) = cues.get(index) else {
            return ignored(GoIgnoredReason::NoCue, cx);
        };
        self.last_go = Some(cx.now);
        self.start(cue.id, cx.now, cx);
        // The playhead skips the rest of the cue's continue chain, as the model stands now.
        let mut last = index;
        while let Some(c) = cues.get(last)
            && c.continue_mode != ContinueMode::DoNotContinue
            && last.saturating_add(1) < cues.len()
        {
            last += 1;
        }
        let next = cues.get(last.saturating_add(1)).map(|c| c.id);
        self.move_playhead(list, next, cx);
    }

    fn navigate(&mut self, list: CueListId, cx: &mut Ctx, step: fn(&mut Playhead, &[Cue]) -> bool) {
        let Some(cues) = cx
            .ws
            .cue_lists
            .iter()
            .find(|l| l.id == list)
            .map(|l| &l.cues)
        else {
            return cx.warn(EngineWarning::UnknownList { list });
        };
        if let Some(playhead) = self.playheads.get_mut(&list)
            && step(playhead, cues)
        {
            cx.out.push(Action::PlayheadChanged {
                list,
                cue: playhead.cue,
            });
        }
    }

    fn set_playhead(&mut self, list: CueListId, cue: Option<CueId>, cx: &mut Ctx) {
        let Some(cues) = cx
            .ws
            .cue_lists
            .iter()
            .find(|l| l.id == list)
            .map(|l| &l.cues)
        else {
            return cx.warn(EngineWarning::UnknownList { list });
        };
        if let Some(id) = cue
            && !cues.iter().any(|c| c.id == id)
        {
            return cx.warn(EngineWarning::NotInList { list, cue: id });
        }
        self.move_playhead(list, cue, cx);
    }

    fn set_playhead_by_number(&mut self, list: CueListId, number: String, cx: &mut Ctx) {
        let Some(cues) = cx
            .ws
            .cue_lists
            .iter()
            .find(|l| l.id == list)
            .map(|l| &l.cues)
        else {
            return cx.warn(EngineWarning::UnknownList { list });
        };
        match cues
            .iter()
            .find(|c| c.number.as_deref() == Some(number.as_str()))
        {
            Some(cue) => self.move_playhead(list, Some(cue.id), cx),
            None => cx.warn(EngineWarning::NumberNotFound { list, number }),
        }
    }

    fn move_playhead(&mut self, list: CueListId, cue: Option<CueId>, cx: &mut Ctx) {
        if let Some(playhead) = self.playheads.get_mut(&list)
            && playhead.set(cue)
        {
            cx.out.push(Action::PlayheadChanged { list, cue });
        }
    }

    // ----- operations (CUE-05) -----

    /// Starts `cue` at sample `at`: a restart if it is running, a resume if it is paused.
    fn start(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        if cx.budget == 0 {
            return cx.warn(EngineWarning::ChainLimit {
                cue,
                limit: cx.limit,
            });
        }
        cx.budget -= 1;
        let Some(model) = cx.ws.find_cue(cue) else {
            return cx.warn(EngineWarning::UnknownCue { cue });
        };
        let before = self.state(cue);
        let (run, offset, voice) = match self.slots.get(&cue) {
            Some(Slot::Broken(_)) => return cx.warn(EngineWarning::CueBroken { cue }),
            Some(Slot::Active(run)) if run.paused_at.is_some() => {
                return self.resume(cue, at, cx);
            }
            Some(Slot::Loaded(loaded)) => (
                loaded.run,
                loaded.offset,
                if loaded.voice {
                    Voice::Loaded
                } else {
                    Voice::None
                },
            ),
            Some(Slot::Active(_)) => {
                if let Some(old) = self.slots.remove(&cue) {
                    release(cue, &old, cx);
                }
                self.report(cue, before, at, cx);
                (self.new_run(), 0, Voice::None)
            }
            None => (self.new_run(), 0, Voice::None),
        };
        let before = self.state(cue);
        let pre_wait = cx.secs(model.pre_wait, &self.config);
        let (until, action_offset) = if offset < pre_wait {
            (at.saturating_add(pre_wait - offset), 0)
        } else {
            (at, offset - pre_wait)
        };
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        self.slots.insert(
            cue,
            Slot::Active(Run {
                run,
                seq,
                phase: Phase::PreWait { until },
                action_offset,
                post_wait: None,
                follow: false,
                voice,
                paused_at: None,
                stop_end: None,
            }),
        );
        self.report(cue, before, at, cx);
    }

    fn stop(&mut self, cue: CueId, fade_out: Option<FadeSpec>, cx: &mut Ctx) {
        let before = self.state(cue);
        let now = cx.now;
        let frames = fade_out.map_or(0, |f| cx.secs(f.duration, &self.config));
        match self.slots.get_mut(&cue) {
            None | Some(Slot::Broken(_)) => return,
            Some(Slot::Active(run))
                if frames > 0
                    && run.stop_end.is_none()
                    && run.paused_at.is_none()
                    && matches!(run.phase, Phase::Action { .. }) =>
            {
                if let (Voice::Started { at: started }, Some(curve)) =
                    (run.voice, fade_out.map(|f| f.curve))
                {
                    let at = now.max(started);
                    cx.out.push(Action::StopVoice {
                        cue,
                        run: run.run,
                        at,
                        fade: Some(StopFade { frames, curve }),
                    });
                    run.stop_end = Some(at.saturating_add(frames));
                    run.post_wait = None;
                    run.follow = false;
                    return self.report(cue, before, now, cx);
                }
            }
            Some(_) => {}
        }
        if let Some(slot) = self.slots.remove(&cue) {
            release(cue, &slot, cx);
        }
        self.report(cue, before, now, cx);
    }

    fn pause(&mut self, cue: CueId, cx: &mut Ctx) {
        let before = self.state(cue);
        let now = cx.now;
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return;
        };
        if run.paused_at.is_some() || run.stop_end.is_some() {
            return;
        }
        run.paused_at = Some(now);
        if let (Voice::Started { at: started }, Phase::Action { .. }) = (run.voice, run.phase) {
            cx.out.push(Action::PauseVoice {
                cue,
                run: run.run,
                at: now.max(started),
                offset: run.position(now),
            });
        }
        self.report(cue, before, now, cx);
    }

    /// Resumes a paused cue as of sample `at`: every timer keeps the time it had left.
    fn resume(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        let before = self.state(cue);
        let now = cx.now;
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return;
        };
        let Some(paused_at) = run.paused_at.take() else {
            return;
        };
        // Where the action was frozen; a resume dated before the pause (a trigger from a late
        // deadline) resumes as of the pause.
        let offset = run.position(paused_at);
        run.shift(at.saturating_sub(paused_at));
        if let (Voice::Started { .. }, Phase::Action { .. }) = (run.voice, run.phase) {
            let voice_at = at.max(now);
            cx.out.push(Action::ResumeVoice {
                cue,
                run: run.run,
                at: voice_at,
                offset,
            });
            run.voice = Voice::Started { at: voice_at };
        }
        self.report(cue, before, at, cx);
    }

    fn load(&mut self, cue: CueId, time: f64, cx: &mut Ctx) {
        let Some(model) = cx.ws.find_cue(cue) else {
            return cx.warn(EngineWarning::UnknownCue { cue });
        };
        match self.slots.get(&cue) {
            Some(Slot::Broken(_)) => return cx.warn(EngineWarning::CueBroken { cue }),
            Some(Slot::Active(_)) => return cx.warn(EngineWarning::LoadIgnored { cue }),
            Some(Slot::Loaded(_)) | None => {}
        }
        let before = self.state(cue);
        if let Some(old) = self.slots.remove(&cue) {
            release(cue, &old, cx);
        }
        let run = self.new_run();
        let offset = cx.secs(time, &self.config);
        let voice = has_voice(model);
        if voice {
            let pre_wait = cx.secs(model.pre_wait, &self.config);
            cx.out.push(Action::LoadVoice {
                cue,
                run,
                offset: offset.saturating_sub(pre_wait),
            });
        }
        self.slots
            .insert(cue, Slot::Loaded(Loaded { run, offset, voice }));
        self.report(cue, before, cx.now, cx);
    }

    fn reset(&mut self, cue: CueId, cx: &mut Ctx) {
        let before = self.state(cue);
        if let Some(slot) = self.slots.remove(&cue) {
            release(cue, &slot, cx);
        }
        self.report(cue, before, cx.now, cx);
    }

    // ----- feedback -----

    fn action_ended(&mut self, cue: CueId, run_id: RunId, at: SampleTime, cx: &mut Ctx) {
        let before = self.state(cue);
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return;
        };
        if run.run != run_id {
            return;
        }
        if run.stop_end.is_some() {
            self.slots.remove(&cue);
            return self.report(cue, before, at, cx);
        }
        let (Phase::Action { start, end: None }, Voice::Started { .. }) = (run.phase, run.voice)
        else {
            return;
        };
        let at = at.max(start);
        run.phase = Phase::Done;
        run.voice = Voice::None;
        let follow = run.follow;
        self.settle(cue);
        self.report(cue, before, at, cx);
        if follow {
            self.continue_after(cue, at, cx);
        }
    }

    fn action_failed(&mut self, cue: CueId, run_id: RunId, reason: BrokenReason, cx: &mut Ctx) {
        let before = self.state(cue);
        let current = match self.slots.get(&cue) {
            Some(Slot::Active(run)) => run.run == run_id,
            Some(Slot::Loaded(loaded)) => loaded.run == run_id,
            Some(Slot::Broken(_)) | None => false,
        };
        if current {
            self.slots.insert(cue, Slot::Broken(reason));
            self.report(cue, before, cx.now, cx);
        }
    }

    // ----- timers -----

    /// Carries out every deadline up to the horizon, earliest first, each at its own sample.
    fn advance(&mut self, cx: &mut Ctx) {
        // Each step consumes a pending deadline. Deadlines come only from starts (bounded by the
        // budget) and a run's own progress (at most three per run), so this ends; the guard is a
        // second line of defence against a bug turning into a hung show.
        let mut guard = self
            .slots
            .len()
            .saturating_add(cx.budget)
            .saturating_add(1)
            .saturating_mul(4);
        while let Some((cue, at)) = self.earliest_due(cx.horizon) {
            if guard == 0 {
                return cx.warn(EngineWarning::ChainLimit {
                    cue,
                    limit: cx.limit,
                });
            }
            guard -= 1;
            self.step(cue, at, cx);
        }
    }

    fn earliest_due(&self, horizon: SampleTime) -> Option<(CueId, SampleTime)> {
        self.slots
            .iter()
            .filter_map(|(id, slot)| match slot {
                Slot::Active(run) => run
                    .next_deadline()
                    .filter(|d| *d <= horizon)
                    .map(|d| (d, run.seq, *id)),
                Slot::Loaded(_) | Slot::Broken(_) => None,
            })
            .min()
            .map(|(d, _, id)| (id, d))
    }

    /// Carries out the deadline of `cue` that falls on `at`.
    fn step(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        let before = self.state(cue);
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return;
        };
        if run.stop_end.is_some_and(|end| end <= at) {
            self.slots.remove(&cue);
            return self.report(cue, before, at, cx);
        }
        if let Phase::PreWait { until } = run.phase {
            if until <= at {
                self.begin_action(cue, at, cx);
            }
            return;
        }
        if run.post_wait.is_some_and(|p| p <= at) {
            run.post_wait = None;
            self.settle(cue);
            self.report(cue, before, at, cx);
            return self.continue_after(cue, at, cx);
        }
        if let Phase::Action { end: Some(end), .. } = run.phase
            && end <= at
        {
            run.phase = Phase::Done;
            let follow = run.follow;
            self.settle(cue);
            self.report(cue, before, at, cx);
            if follow {
                self.continue_after(cue, at, cx);
            }
        }
    }

    /// The pre-wait has elapsed: the action starts on sample `at`.
    fn begin_action(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        let before = self.state(cue);
        let Some(model) = cx.ws.find_cue(cue) else {
            // Cannot happen (missing cues are dropped at the start of each call), but a run left
            // due would never let the deadline loop move on.
            self.slots.remove(&cue);
            return self.report(cue, before, at, cx);
        };
        let post_wait = cx.secs(model.post_wait, &self.config);
        let duration = match &model.kind {
            _ if !model.armed => Some(0),
            CueKind::Audio { .. } => None,
            CueKind::Wait { duration } => Some(cx.secs(*duration, &self.config)),
            CueKind::Memo => Some(0),
            CueKind::Fade { .. }
            | CueKind::Group { .. }
            | CueKind::Start { .. }
            | CueKind::Stop { .. }
            | CueKind::Pause { .. }
            | CueKind::Load { .. }
            | CueKind::Reset { .. }
            | CueKind::Devamp { .. }
            | CueKind::GoTo { .. }
            | CueKind::Arm { .. }
            | CueKind::Disarm { .. }
            | CueKind::Network { .. } => {
                cx.warn(EngineWarning::Unsupported { cue });
                Some(0)
            }
        };
        let now = cx.now;
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return;
        };
        let offset = run.action_offset;
        match duration {
            None => {
                let voice_at = at.max(now);
                if at < now {
                    cx.warn(EngineWarning::LateStart {
                        cue,
                        late_by: now - at,
                    });
                }
                cx.out.push(Action::StartVoice {
                    cue,
                    run: run.run,
                    at: voice_at,
                    offset,
                });
                run.voice = Voice::Started { at: voice_at };
            }
            Some(_) => {
                if run.voice == Voice::Loaded {
                    cx.out.push(Action::UnloadVoice { cue, run: run.run });
                }
                run.voice = Voice::None;
            }
        }
        run.phase = Phase::Action {
            start: at,
            end: duration.map(|d| at.saturating_add(d.saturating_sub(offset))),
        };
        run.post_wait = (model.continue_mode == ContinueMode::AutoContinue)
            .then(|| at.saturating_add(post_wait.saturating_sub(offset)));
        run.follow = model.continue_mode == ContinueMode::AutoFollow;
        self.report(cue, before, at, cx);
    }

    /// Triggers the cue after `cue` in its list or group, on sample `at`.
    fn continue_after(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        if let Some(next) = next_sibling(cx.ws, cue) {
            self.start(next, at, cx);
        }
    }

    /// Forgets a run that has nothing left to do.
    fn settle(&mut self, cue: CueId) {
        if let Some(Slot::Active(run)) = self.slots.get(&cue)
            && run.is_finished()
        {
            self.slots.remove(&cue);
        }
    }

    fn report(&self, cue: CueId, before: CueState, at: SampleTime, cx: &mut Ctx) {
        let state = self.state(cue);
        if state != before {
            cx.out.push(Action::StateChanged { cue, state, at });
        }
    }

    fn new_run(&mut self) -> RunId {
        let run = RunId(self.next_run);
        self.next_run = self.next_run.saturating_add(1);
        run
    }
}

/// Sends what is needed to silence and free a slot's voice at once.
fn release(cue: CueId, slot: &Slot, cx: &mut Ctx) {
    match slot {
        Slot::Loaded(Loaded {
            run, voice: true, ..
        }) => cx.out.push(Action::UnloadVoice { cue, run: *run }),
        Slot::Active(run) => match run.voice {
            Voice::Started { at } => cx.out.push(Action::StopVoice {
                cue,
                run: run.run,
                at: cx.now.max(at),
                fade: None,
            }),
            Voice::Loaded => cx.out.push(Action::UnloadVoice { cue, run: run.run }),
            Voice::None => {}
        },
        Slot::Loaded(_) | Slot::Broken(_) => {}
    }
}

fn has_voice(cue: &Cue) -> bool {
    cue.armed && matches!(cue.kind, CueKind::Audio { .. })
}

fn next_sibling(ws: &Workspace, cue: CueId) -> Option<CueId> {
    let path = tree::find_path(ws, cue)?;
    let (parents, index) = path.split()?;
    tree::container(ws, path.list, parents)?
        .get(index.checked_add(1)?)
        .map(|c| c.id)
}

fn collect_ids(cues: &[Cue], ids: &mut HashSet<CueId>) {
    for cue in cues {
        ids.insert(cue.id);
        collect_ids(cue.children(), ids);
    }
}

fn count_cues(cues: &[Cue]) -> usize {
    cues.iter()
        .map(|c| count_cues(c.children()).saturating_add(1))
        .fold(0, usize::saturating_add)
}
