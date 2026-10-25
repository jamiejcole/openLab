// SPDX-License-Identifier: GPL-3.0-or-later

//! The cue engine: cue lifecycle, timing, continue modes, groups, GO and playheads
//! (ARCHITECTURE §3.3).
//!
//! Pure and deterministic: [`CueEngine::handle`] takes the current workspace, one [`Input`] and
//! the sample time, and returns the [`Action`]s to carry out. Semantics are specified in
//! `docs/cue-engine.md`.

mod group;
mod playhead;
mod rng;
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
use rng::SplitMix64;
use run::{Exit, GroupRun, Loaded, Phase, Run, Slot, Voice};

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
    /// Random groups and shuffled playlists draw from this, seeded from the config.
    rng: SplitMix64,
    /// The child each random group picked last, so it is not picked twice in a row.
    last_random: BTreeMap<CueId, CueId>,
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
    /// Runtimes that ended since groups last looked: `(cue, at, how)`.
    vacated: Vec<(CueId, SampleTime, Exit)>,
}

impl Ctx<'_> {
    fn warn(&mut self, warning: EngineWarning) {
        self.out.push(Action::Warning(warning));
    }

    fn secs(&self, seconds: f64, config: &EngineConfig) -> SampleTime {
        seconds_to_samples(seconds, config.sample_rate())
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
            rng: SplitMix64::new(config.seed()),
            last_random: BTreeMap::new(),
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
            horizon: self.config.horizon(now),
            budget: limit,
            limit,
            out: Vec::new(),
            vacated: Vec::new(),
        };
        self.sync_model(&mut cx);
        self.sync_playheads(&mut cx);
        self.advance(&mut cx);
        self.apply(input, &mut cx);
        self.drain(&mut cx);
        self.advance(&mut cx);
        self.settle_groups(&mut cx);
        cx.out
    }

    /// Safety net: ends groups left running with no busy child (see `sweep_groups`) and carries
    /// out what that triggers. Each round ends at least one group's action and starts only within
    /// the call's budget, so this ends; the guard is a second line of defence.
    fn settle_groups(&mut self, cx: &mut Ctx) {
        let mut guard = self.slots.len().saturating_add(1);
        while guard > 0 && self.sweep_groups(cx) {
            guard -= 1;
            self.drain(cx);
            self.advance(cx);
        }
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

    /// Whether a `Tick` at `now` has a deadline to carry out: the control loop's wake rule. A
    /// deadline is due once it falls inside the call's window (`..now + lookahead`, end excluded,
    /// and always `now` itself, so with lookahead 0 a deadline at `now` is due at `now`).
    pub fn is_due(&self, now: SampleTime) -> bool {
        self.next_deadline()
            .is_some_and(|d| d <= self.config.horizon(now))
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
            Slot::Active(run) => run.broken,
            Slot::Loaded(_) => None,
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
        let cues = &ws.cue_lists.iter().find(|l| l.id == list)?.cues;
        let path = playhead::locate(cues, id)?;
        let &(level, index) = path.last()?;
        level.get(index)
    }

    /// The cues of a list the playhead can stand on, in order: every cue, depth-first.
    pub fn playhead_positions(ws: &Workspace, list: CueListId) -> Vec<CueId> {
        ws.cue_lists
            .iter()
            .find(|l| l.id == list)
            .map(|l| playhead::positions(&l.cues))
            .unwrap_or_default()
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
                    self.stop_tree(cue, fade_out, cx);
                }
            }
            Input::Pause { cue } => {
                if self.known(cue, cx) {
                    self.pause_tree(cue, cx);
                }
            }
            Input::Resume { cue } => {
                if self.known(cue, cx) {
                    self.resume_tree(cue, cx.now, cx);
                }
            }
            Input::Load { cue, time } => {
                if self.known(cue, cx) {
                    self.load_tree(cue, time, cx);
                }
            }
            Input::Reset { cue } => {
                if self.known(cue, cx) {
                    self.reset_tree(cue, cx);
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

    /// Stops and forgets cues that are no longer in the workspace, then lets their groups react
    /// (and ends groups an edit left with no busy child).
    fn sync_model(&mut self, cx: &mut Ctx) {
        if self.slots.is_empty() {
            return;
        }
        let mut present = HashSet::new();
        for list in &cx.ws.cue_lists {
            collect_ids(&list.cues, &mut present);
        }
        self.last_random.retain(|group, _| present.contains(group));
        // Gone, or now a cue of the other sort (group or not) than the one running: a runtime
        // only fits the kind it was started as.
        let ws = cx.ws;
        let gone: Vec<CueId> = self
            .slots
            .iter()
            .filter(|(id, slot)| {
                !present.contains(*id)
                    || matches!(slot, Slot::Active(run) if run.group.is_some()
                        != ws.find_cue(**id).is_some_and(|c| matches!(c.kind, CueKind::Group { .. })))
            })
            .map(|(id, _)| *id)
            .collect();
        for cue in gone {
            let before = self.state(cue);
            if let Some(slot) = self.remove_slot(cue, cx.now, Exit::Stopped, cx) {
                release(cue, &slot, cx);
                if matches!(slot, Slot::Active(_)) {
                    cx.warn(EngineWarning::CueRemoved { cue });
                }
            }
            self.report(cue, before, cx.now, cx);
        }
        self.drain(cx);
        // Each round removes at least one group's run and starts nothing; the guard is a second
        // line of defence.
        let mut guard = self.slots.len().saturating_add(1);
        while guard > 0 && self.sweep_groups(cx) {
            guard -= 1;
            self.drain(cx);
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
        let Some(path) = self
            .playhead(list)
            .and_then(|id| playhead::locate(cues, id))
        else {
            return ignored(GoIgnoredReason::NoCue, cx);
        };
        let Some(cue) = path.last().and_then(|&(level, index)| level.get(index)) else {
            return ignored(GoIgnoredReason::NoCue, cx);
        };
        self.last_go = Some(cx.now);
        self.start(cue.id, cx.now, cx);
        // The playhead skips the rest of the cue's continue chain, as the model stands now, or
        // enters a "start first and enter" group.
        let next = playhead::after_go(&path);
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
            && !playhead::positions(cues).contains(&id)
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
        let numbered = |c: &&Cue| c.number.as_deref() == Some(number.as_str());
        match playhead::position_cues(cues).into_iter().find(numbered) {
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

    /// Starts `cue` at sample `at`. A paused cue resumes; an already active one ignores the
    /// trigger (second-trigger rule, docs/cue-engine.md).
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
            Some(Slot::Active(run)) if run.broken.is_some() => {
                return cx.warn(EngineWarning::CueBroken { cue });
            }
            Some(Slot::Active(run)) if run.paused_at.is_some() => {
                return self.resume_tree(cue, at, cx);
            }
            Some(Slot::Active(run)) => {
                return cx.warn(EngineWarning::TriggerIgnored {
                    cue,
                    state: run.state(),
                });
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
            None => (self.new_run(), 0, Voice::None),
        };
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
                broken: None,
                group: matches!(model.kind, CueKind::Group { .. }).then(GroupRun::default),
            }),
        );
        self.report(cue, before, at, cx);
        self.adopt_parents(cue, at, cx);
    }

    /// Stops one cue (a group: itself only; `stop_tree` stops its descendants first).
    fn stop_one(&mut self, cue: CueId, fade_out: Option<FadeSpec>, cx: &mut Ctx) {
        let before = self.state(cue);
        let now = cx.now;
        let frames = fade_out.map_or(0, |f| cx.secs(f.duration, &self.config));
        let busy_children = self.any_busy_child(cx.ws, cue);
        match self.slots.get_mut(&cue) {
            None | Some(Slot::Broken(_)) => return,
            Some(Slot::Active(run)) if run.broken.is_some() => {
                // It owns no voice; stopping cancels its pending auto-continue.
                if let Some(reason) = run.broken {
                    self.slots.insert(cue, Slot::Broken(reason));
                    cx.vacated.push((cue, now, Exit::Stopped));
                }
                return self.report(cue, before, now, cx);
            }
            // A group whose children are fading out ends when the last of them has stopped.
            // Only during its action: a group that has not reached it (pre-wait) stops at once,
            // so its pending action can never start children after the stop.
            Some(Slot::Active(run @ Run { group: Some(_), .. }))
                if frames > 0
                    && busy_children
                    && run.paused_at.is_none()
                    && matches!(run.phase, Phase::Action { .. }) =>
            {
                if let Some(g) = run.group.as_mut()
                    && !g.stopping
                {
                    g.stopping = true;
                    run.post_wait = None;
                    run.follow = false;
                    return self.report(cue, before, now, cx);
                }
            }
            Some(Slot::Active(run))
                if frames > 0
                    && run.stop_end.is_none()
                    && run.paused_at.is_none()
                    && matches!(run.phase, Phase::Action { .. }) =>
            {
                if let (Voice::Started { at: started, .. }, Some(curve)) =
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
        if let Some(slot) = self.remove_slot(cue, now, Exit::Stopped, cx) {
            release(cue, &slot, cx);
        }
        self.report(cue, before, now, cx);
    }

    fn pause_one(&mut self, cue: CueId, cx: &mut Ctx) {
        let before = self.state(cue);
        let now = cx.now;
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return;
        };
        if run.paused_at.is_some() || run.stop_end.is_some() || run.is_group_stopping() {
            return;
        }
        run.paused_at = Some(now);
        if let (Voice::Started { at: started, .. }, Phase::Action { .. }) = (run.voice, run.phase) {
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
    fn resume_one(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        let before = self.state(cue);
        let now = cx.now;
        let fresh = self.new_run();
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
            // The resumed voice is a new segment with its own id: feedback still on its way for
            // the paused one (e.g. the end report of a pause carried out as a stop) is then
            // stale and ignored, whenever it arrives.
            cx.out.push(Action::ResumeVoice {
                cue,
                run: fresh,
                paused: run.run,
                at: voice_at,
                offset,
            });
            run.run = fresh;
            run.voice = Voice::Started {
                at: voice_at,
                from: offset,
            };
        }
        self.report(cue, before, at, cx);
    }

    fn load(&mut self, cue: CueId, time: f64, cx: &mut Ctx) {
        let Some(model) = cx.ws.find_cue(cue) else {
            return cx.warn(EngineWarning::UnknownCue { cue });
        };
        match self.slots.get(&cue) {
            Some(Slot::Broken(_)) => return cx.warn(EngineWarning::CueBroken { cue }),
            Some(Slot::Active(run)) if run.broken.is_some() => {
                return cx.warn(EngineWarning::CueBroken { cue });
            }
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

    fn reset_one(&mut self, cue: CueId, cx: &mut Ctx) {
        let before = self.state(cue);
        if let Some(slot) = self.remove_slot(cue, cx.now, Exit::Stopped, cx) {
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
        // A paused voice cannot end by itself; an end dated at or after the pause is the pause
        // being carried out (e.g. as a stop), not the action completing.
        if run.paused_at.is_some_and(|paused| at >= paused) {
            return;
        }
        if run.stop_end.is_some() {
            self.remove_slot(cue, at, Exit::Stopped, cx);
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
        let seq = run.seq;
        self.settle(cue, at, cx);
        self.report(cue, before, at, cx);
        if follow {
            self.continue_after(cue, at, cx);
        }
        self.child_action_done(cue, seq, at, cx);
    }

    fn action_failed(&mut self, cue: CueId, run_id: RunId, reason: BrokenReason, cx: &mut Ctx) {
        let before = self.state(cue);
        // A group owns no voice, so no feedback can concern it.
        let group = cx
            .ws
            .find_cue(cue)
            .is_some_and(|c| matches!(c.kind, CueKind::Group { .. }));
        let current = match self.slots.get(&cue) {
            Some(Slot::Active(run)) => {
                run.run == run_id && run.broken.is_none() && run.group.is_none()
            }
            Some(Slot::Loaded(loaded)) => loaded.run == run_id && !group,
            Some(Slot::Broken(_)) | None => false,
        };
        if !current {
            return;
        }
        // Silence and free the failed run now: a broken cue owns no voice, so nothing can keep
        // sounding after it (a failed stream still plays out its ring otherwise).
        let Some(old) = self.slots.remove(&cue) else {
            return;
        };
        release(cue, &old, cx);
        match old {
            // Auto-continue is time-based: an armed or still-to-be-armed post-wait fires on its
            // schedule. Auto-follow waits for a successful completion, so it never fires.
            Slot::Active(mut run) => {
                run.broken = Some(reason);
                run.voice = Voice::None;
                run.stop_end = None;
                run.follow = false;
                if matches!(run.phase, Phase::Action { .. }) {
                    run.phase = Phase::Done;
                }
                let seq = run.seq;
                self.slots.insert(cue, Slot::Active(run));
                self.settle(cue, cx.now, cx);
                self.report(cue, before, cx.now, cx);
                // A playlist moves on past a child that broke.
                self.child_action_done(cue, seq, cx.now, cx);
            }
            Slot::Loaded(_) | Slot::Broken(_) => {
                self.slots.insert(cue, Slot::Broken(reason));
                self.report(cue, before, cx.now, cx);
            }
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
            self.drain(cx);
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
            self.remove_slot(cue, at, Exit::Stopped, cx);
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
            self.settle(cue, at, cx);
            self.report(cue, before, at, cx);
            return self.continue_after(cue, at, cx);
        }
        if let Phase::Action { end: Some(end), .. } = run.phase
            && end <= at
        {
            run.phase = Phase::Done;
            let follow = run.follow;
            let seq = run.seq;
            self.settle(cue, at, cx);
            self.report(cue, before, at, cx);
            if follow {
                self.continue_after(cue, at, cx);
            }
            self.child_action_done(cue, seq, at, cx);
        }
    }

    /// The pre-wait has elapsed: the action starts on sample `at`.
    fn begin_action(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        let before = self.state(cue);
        let ws = cx.ws;
        let Some(model) = ws.find_cue(cue) else {
            // Cannot happen (missing cues are dropped at the start of each call), but a run left
            // due would never let the deadline loop move on.
            self.remove_slot(cue, at, Exit::Stopped, cx);
            return self.report(cue, before, at, cx);
        };
        let post_wait = cx.secs(model.post_wait, &self.config);
        // A run that broke during its pre-wait skips its action but keeps its timing.
        let broken = matches!(self.slots.get(&cue), Some(Slot::Active(r)) if r.broken.is_some());
        let act = match &model.kind {
            _ if !model.armed || broken => Act::Silent(0),
            CueKind::Audio { .. } => Act::Voice,
            CueKind::Wait { duration } => Act::Silent(cx.secs(*duration, &self.config)),
            CueKind::Memo => Act::Silent(0),
            CueKind::Group { .. } => Act::Group,
            CueKind::Fade { .. }
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
                Act::Silent(0)
            }
        };
        let continue_mode = group::effective_continue(ws, model);
        let now = cx.now;
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return;
        };
        let offset = run.action_offset;
        match act {
            Act::Voice => {
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
                run.voice = Voice::Started {
                    at: voice_at,
                    from: offset,
                };
            }
            Act::Silent(_) | Act::Group => {
                if run.voice == Voice::Loaded {
                    cx.out.push(Action::UnloadVoice { cue, run: run.run });
                }
                run.voice = Voice::None;
            }
        }
        run.phase = Phase::Action {
            start: at,
            end: match act {
                Act::Silent(d) => Some(at.saturating_add(d.saturating_sub(offset))),
                Act::Voice | Act::Group => None,
            },
        };
        run.post_wait = (continue_mode == ContinueMode::AutoContinue)
            .then(|| at.saturating_add(post_wait.saturating_sub(offset)));
        run.follow = !broken && continue_mode == ContinueMode::AutoFollow;
        self.settle(cue, at, cx);
        self.report(cue, before, at, cx);
        if act == Act::Group {
            self.start_children(cue, at, cx);
        }
    }

    /// Triggers the cue after `cue` in its list or group, on sample `at`. Inside timeline,
    /// playlist and random groups the group runs its children, so nothing is triggered.
    fn continue_after(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        if !group::chains_siblings(cx.ws, cue) {
            return;
        }
        if let Some(next) = next_sibling(cx.ws, cue) {
            self.start(next, at, cx);
        }
    }

    /// Forgets a run that has nothing left to do (on sample `at`); a broken one stays broken.
    fn settle(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        if let Some(Slot::Active(run)) = self.slots.get(&cue)
            && run.is_finished()
        {
            let failed = run.group.as_ref().is_some_and(|g| g.failed);
            let exit = if run.broken.is_some() || failed {
                Exit::Broken
            } else {
                Exit::Completed
            };
            match run.broken {
                Some(reason) => self.slots.insert(cue, Slot::Broken(reason)),
                None => self.slots.remove(&cue),
            };
            cx.vacated.push((cue, at, exit));
        }
    }

    /// Removes a cue's runtime; if it was active, its group hears that it ended on `at`.
    fn remove_slot(
        &mut self,
        cue: CueId,
        at: SampleTime,
        exit: Exit,
        cx: &mut Ctx,
    ) -> Option<Slot> {
        let slot = self.slots.remove(&cue)?;
        if matches!(slot, Slot::Active(_)) {
            cx.vacated.push((cue, at, exit));
        }
        Some(slot)
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
            Voice::Started { at, .. } => cx.out.push(Action::StopVoice {
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

/// What a cue's action does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Act {
    /// Plays a voice; completes on feedback.
    Voice,
    /// Completes after this many samples.
    Silent(SampleTime),
    /// Runs the group's children; completes when none is busy.
    Group,
}

/// Every cue in `cues`, depth-first. Iterative, so nesting depth cannot overflow the stack.
fn each_cue(cues: &[Cue], mut f: impl FnMut(&Cue)) {
    let mut stack: Vec<std::slice::Iter<'_, Cue>> = vec![cues.iter()];
    while let Some(level) = stack.last_mut() {
        match level.next() {
            Some(cue) => {
                f(cue);
                stack.push(cue.children().iter());
            }
            None => {
                stack.pop();
            }
        }
    }
}

fn collect_ids(cues: &[Cue], ids: &mut HashSet<CueId>) {
    each_cue(cues, |c| {
        ids.insert(c.id);
    });
}

fn count_cues(cues: &[Cue]) -> usize {
    let mut n = 0usize;
    each_cue(cues, |_| n = n.saturating_add(1));
    n
}
