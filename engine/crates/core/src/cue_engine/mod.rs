// SPDX-License-Identifier: GPL-3.0-or-later

//! The cue engine: cue lifecycle, timing, continue modes, groups, GO and playheads
//! (ARCHITECTURE §3.3).
//!
//! Pure and deterministic: [`CueEngine::handle`] takes the current workspace, one [`Input`] and
//! the sample time, and returns the [`Action`]s to carry out. Semantics are specified in
//! `docs/cue-engine.md`.

mod broken;
mod control;
mod fade;
mod group;
mod level;
mod playhead;
mod preview;
mod rng;
mod run;
mod second;
mod time;
mod transport;
mod types;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashSet};

pub use level::{LevelParam, LevelRamp, curve_shape, db_to_gain, ramp_gain};
pub use preview::{PREVIEW_RUN_BASE, PREVIEW_STOP_FADE_SECONDS};
pub use time::*;
pub use transport::PANIC_CURVE;
pub use types::*;

use crate::cue::{ContinueMode, Cue, CueKind, FadeSpec, SecondTrigger};
use crate::model::{CueId, CueListId, Workspace};
use crate::tree;

use playhead::Playhead;
use preview::Preview;
use rng::SplitMix64;
use run::{DevampWait, Exit, GroupRun, Loaded, Phase, Run, Slot, Voice};

/// Cue starts allowed per call on top of the workspace's cue count. Without trigger cycles a call
/// starts each cue at most once, so only a cycle reaches the limit.
pub const CHAIN_MARGIN: usize = 64;

/// Cue starts of a call that a playlist leaves alone for what follows it: its own children stop
/// starting when this few are left, so a looping playlist of zero-length children (a cycle that
/// would otherwise spend the whole call) still lets the cue after the group start.
pub(super) const PLAYLIST_RESERVE: usize = 8;

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
    /// Each random group's round-robin cycle (QLab 5: memory lasts while the engine does).
    random_cycles: BTreeMap<CueId, group::RandomCycle>,
    /// Arm/Disarm cue changes made in this call (see `Action::SetArmed`).
    armed: BTreeMap<CueId, bool>,
    /// Static problems (CUE-06) and what they are computed from.
    problems: broken::Problems,
    /// The panic fade under way: the sample it came on and the one it ends on.
    panic: Option<(SampleTime, SampleTime)>,
    /// `model_changed` was called (or the engine is new) since the last call: the next call
    /// follows the edits (stops deleted cues, moves playheads) and recounts the chain limit.
    /// Without it those passes are skipped (release builds): see `model_changed`.
    model_dirty: bool,
    /// Cues being previewed (see `preview.rs`); apart from the show's runs.
    previews: BTreeMap<CueId, Preview>,
    next_preview: u64,
    /// `chain_limit` of the workspace as of the last such pass.
    limit: usize,
    /// How many such passes have run (a cost measure, see `model_syncs`).
    model_syncs: u64,
    /// The `now` of the last call and the cue starts made by the calls at that `now` so far.
    /// The chain budget is per sample, not per call: mixer feedback dated on the sample it is
    /// handled at (a zero-length voice ends on its start sample) could otherwise carry a cycle
    /// into call after call at one `now`, each with a fresh budget (see "Chains and the chain
    /// bound").
    spent: (SampleTime, usize),
}

/// One `handle` call's inputs and outputs.
struct Ctx<'w> {
    ws: &'w Workspace,
    now: SampleTime,
    /// Deadlines up to here are carried out in this call.
    horizon: SampleTime,
    /// Cue starts left at this `now` (what earlier calls at the same `now` left).
    budget: usize,
    limit: usize,
    /// The limit was recounted this call (see `CueEngine::budget_left`).
    limit_verified: bool,
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
            random_cycles: BTreeMap::new(),
            armed: BTreeMap::new(),
            problems: broken::Problems::default(),
            panic: None,
            model_dirty: true,
            previews: BTreeMap::new(),
            next_preview: 0,
            limit: 0,
            model_syncs: 0,
            spent: (0, 0),
        }
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// Changes the lookahead between calls (the binary's device came back with another block
    /// size). Commands already committed keep their samples (each run remembers the latest it
    /// committed); the next call commits deadlines in `now .. now + lookahead`. At most one
    /// second, as in [`EngineConfig::new`]; on error nothing changes.
    pub fn set_lookahead(&mut self, lookahead: SampleTime) -> Result<(), EngineConfigError> {
        self.config = EngineConfig::new(
            self.config.sample_rate().get(),
            lookahead,
            self.config.seed(),
        )?;
        Ok(())
    }

    /// Tells the engine the workspace changed (any edit, undo or redo, relink, an applied
    /// `SetArmed`, a newly opened workspace): the next `handle` call follows the edits (stops
    /// deleted cues, moves playheads, recounts the chain limit) and rebuilds the static problems.
    /// M4.2 contract. Release builds skip those whole-workspace passes on every other call, so a
    /// missed signal leaves them out of date; what could then refuse or drop a cue is guarded
    /// where it happens (a cue about to be refused for its problems is rechecked, a spent chain
    /// budget is recounted, a GO whose playhead cue is gone resyncs that list) and counted in
    /// [`Self::unsignalled_model_changes`]. Debug builds run the passes on every call and count.
    pub fn model_changed(&mut self) {
        self.model_dirty = true;
        self.mark_problems_dirty();
    }

    /// How many times the whole-workspace passes triggered by [`Self::model_changed`] have run
    /// (a cost measure: a call with nothing signalled adds none).
    pub fn model_syncs(&self) -> u64 {
        self.model_syncs
    }

    /// Applies one input at scheduling time `now` (the earliest sample a command can still hit).
    /// `ws` is the current workspace; edits since the last call are taken into account first.
    pub fn handle(&mut self, ws: &Workspace, input: Input, now: SampleTime) -> Vec<Action> {
        // Whole-tree passes (the chain limit, following edits) only after `model_changed`, so a
        // Tick on a big show costs nothing extra. Debug builds run them every call and count a
        // change nobody signalled, as for the static problems.
        let signalled = std::mem::take(&mut self.model_dirty);
        let follow_edits = signalled || cfg!(debug_assertions);
        if follow_edits {
            self.limit = Self::chain_limit(ws);
        }
        if signalled {
            self.model_syncs = self.model_syncs.saturating_add(1);
        }
        let limit = self.limit;
        let spent = match self.spent {
            (at, starts) if at == now => starts,
            _ => 0,
        };
        let mut cx = Ctx {
            ws,
            now,
            horizon: self.config.horizon(now),
            budget: limit.saturating_sub(spent),
            limit,
            limit_verified: follow_edits,
            out: Vec::new(),
            vacated: Vec::new(),
        };
        self.sync_armed();
        // Over, or the clock went back (a device restart): a panic fade on the old clock is not
        // under way on the new one.
        if self
            .panic
            .is_some_and(|(start, end)| now >= end || now < start)
        {
            self.panic = None;
        }
        // Before the runtimes follow the edits, so a deleted cue loses its problems while its
        // runtime still gives its state, and reports idle once. Only does work after
        // `model_changed`.
        let counted = self.unsignalled_model_changes();
        self.sync_problems(&mut cx);
        if follow_edits {
            let changed = self.sync_model(&mut cx) | self.sync_playheads(&mut cx);
            // Once per call: the problems may have noticed the same edit.
            if changed && !signalled && self.unsignalled_model_changes() == counted {
                self.note_unsignalled();
            }
        }
        self.advance(&mut cx);
        self.apply(input, &mut cx);
        self.drain(&mut cx);
        self.advance(&mut cx);
        self.settle_groups(&mut cx);
        // `limit - budget` is what this call and the earlier ones at `now` started (a raised
        // limit raises the budget by as much, see `budget_left`).
        self.spent = (now, cx.limit.saturating_sub(cx.budget));
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

    /// The cue's reported state: its runtime's, else `broken` if it has static problems, else
    /// idle.
    pub fn state(&self, cue: CueId) -> CueState {
        match self.slots.get(&cue) {
            Some(slot) => slot.state(),
            None if self.has_problems(cue) => CueState::Broken,
            None => CueState::Idle,
        }
    }

    /// Every cue that is not idle, in id order.
    pub fn cue_states(&self) -> impl Iterator<Item = (CueId, CueState)> + '_ {
        let mut all: BTreeMap<CueId, CueState> = self
            .slots
            .iter()
            .map(|(id, slot)| (*id, slot.state()))
            .collect();
        for (id, _) in self.broken_cues() {
            all.entry(id).or_insert(CueState::Broken);
        }
        all.into_iter()
    }

    /// Why a cue broke while it ran, if it did (static problems: `problems`).
    pub fn broken_reason(&self, cue: CueId) -> Option<BrokenReason> {
        match self.slots.get(&cue)? {
            Slot::Broken(reason) => Some(*reason),
            Slot::Active(run) => run.broken,
            Slot::Loaded(_) => None,
        }
    }

    /// The run of a cue that is loaded or running. A broken cue (at run time, or started broken)
    /// owns no voice and so has none.
    pub fn run_of(&self, cue: CueId) -> Option<RunId> {
        match self.slots.get(&cue)? {
            Slot::Loaded(loaded) => Some(loaded.run),
            Slot::Active(run) if !run.is_broken() => Some(run.run),
            Slot::Active(_) | Slot::Broken(_) => None,
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
            Input::TriggerReleased { cue } => {
                if self.known(cue, cx) {
                    self.trigger_released(cue, cx);
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
            Input::ActionLength { cue, run, frames } => self.action_length(cue, run, frames),
            Input::ActionFailed { cue, run, reason } => self.action_failed(cue, run, reason, cx),
            Input::DevampScheduled { cue, run, at } => self.devamp_scheduled(cue, run, at),
            Input::DevampIgnored { cue, run } => self.devamp_ignored(cue, run, cx),
            Input::Preview { cue } => self.preview(cue, cx),
            Input::StopPreview { cue } => self.stop_previews(cue, false, cx),
            Input::Panic => self.panic(cx),
            Input::PauseAll => self.pause_all(cx),
            Input::ResumeAll => self.resume_all(cx),
            Input::MediaStatus { media, status } => {
                self.availability(vec![(media, status)], Vec::new(), cx);
            }
            Input::PatchStatus { patch, resolution } => {
                self.availability(Vec::new(), vec![(patch, resolution)], cx);
            }
            Input::Availability { media, patches } => self.availability(media, patches, cx),
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
    fn sync_model(&mut self, cx: &mut Ctx) -> bool {
        let previews_ended = self.sync_previews(cx);
        if self.slots.is_empty() {
            return previews_ended;
        }
        let mut present = HashSet::new();
        for list in &cx.ws.cue_lists {
            collect_ids(&list.cues, &mut present);
        }
        self.random_cycles
            .retain(|group, _| present.contains(group));
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
        let changed = !gone.is_empty() || previews_ended;
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
        changed
    }

    /// Follows edits to the cue lists. Returns whether any list's cues (or the set of lists)
    /// differ from the last pass.
    fn sync_playheads(&mut self, cx: &mut Ctx) -> bool {
        let before = self.playheads.len();
        self.playheads
            .retain(|id, _| cx.ws.cue_lists.iter().any(|l| l.id == *id));
        let mut changed = self.playheads.len() != before;
        for list in &cx.ws.cue_lists {
            match self.playheads.get_mut(&list.id) {
                Some(playhead) => {
                    let (edited, moved) = playhead.sync(&list.cues);
                    changed |= edited;
                    if moved {
                        cx.out.push(Action::PlayheadChanged {
                            list: list.id,
                            cue: playhead.cue,
                        });
                    }
                }
                None => {
                    changed = true;
                    let playhead = Playhead::new(&list.cues);
                    cx.out.push(Action::PlayheadChanged {
                        list: list.id,
                        cue: playhead.cue,
                    });
                    self.playheads.insert(list.id, playhead);
                }
            }
        }
        changed
    }

    // ----- GO and playheads -----

    fn go(&mut self, list: CueListId, cx: &mut Ctx) {
        let ignored = |reason, cx: &mut Ctx| cx.out.push(Action::GoIgnored { list, reason });
        if !cx.ws.cue_lists.iter().any(|l| l.id == list) {
            return ignored(GoIgnoredReason::UnknownList, cx);
        }
        let interval = cx.secs(cx.ws.settings.min_go_interval, &self.config);
        // A clock that went backwards (device restart) never blocks GO.
        if let Some(last) = self.last_go
            && cx.now >= last
            && cx.now - last < interval
        {
            return ignored(GoIgnoredReason::DoubleGo, cx);
        }
        if let Err(reason) = self.go_on(list, cx.now, cx) {
            return ignored(reason, cx);
        }
        self.last_go = Some(cx.now);
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

    /// Whether the call may start another cue. When the budget is spent, the limit is recounted
    /// once per call (calls skip the count when nothing was signalled, see `model_changed`): if
    /// cues were added without the signal the limit was too low, and the budget is raised to the
    /// real one instead of dropping a legitimate start. A genuine cycle (the recount agrees) costs
    /// one recount per call.
    fn budget_left(&mut self, cx: &mut Ctx) -> bool {
        if cx.budget > 0 {
            return true;
        }
        if !cx.limit_verified {
            cx.limit_verified = true;
            let fresh = Self::chain_limit(cx.ws);
            if fresh > cx.limit {
                cx.budget = fresh - cx.limit;
                cx.limit = fresh;
                self.limit = fresh;
                self.note_unsignalled();
                return true;
            }
        }
        false
    }

    /// Starts `cue` at sample `at`. On a cue that is already active its second-trigger option
    /// applies (`second.rs`); with `nothing` the trigger is ignored, or a paused cue resumes
    /// (docs/cue-engine.md, "Second trigger").
    fn start(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        let Some(model) = cx.ws.find_cue(cue) else {
            return cx.warn(EngineWarning::UnknownCue { cue });
        };
        // A cue fading out in a panic is not active for the second-trigger rule: the trigger
        // ends that fade at once and starts the cue afresh.
        let panicked = matches!(self.slots.get(&cue), Some(Slot::Active(r)) if r.panicked);
        // Triggers that start nothing cost no budget: the budget is shared by every call at one
        // `now`, so ignored triggers (a stuck controller key) must not use it up for a GO. A
        // cycle still ends, since each of its laps creates or resumes a runtime, which is charged
        // below (a second trigger that restarts or moves a playlist on is charged for its
        // start), and a trigger back to an active cue starts nothing else.
        if !panicked {
            match self.slots.get(&cue) {
                Some(Slot::Broken(_)) => return cx.warn(EngineWarning::CueBroken { cue }),
                // A broken run keeps the rule as it was: a paused one resumes.
                Some(Slot::Active(run)) if run.is_broken() => {
                    if run.paused_at.is_none() {
                        return cx.warn(EngineWarning::CueBroken { cue });
                    }
                }
                Some(Slot::Active(run)) => {
                    let paused = run.paused_at.is_some();
                    let mut option = model.second_trigger.effective(&model.kind);
                    // An option with nothing to act on leaves a paused cue to the `nothing`
                    // rule: it resumes.
                    if paused && !self.acts_on_paused(cue, option, at, cx) {
                        option = SecondTrigger::Nothing;
                    }
                    if !(option == SecondTrigger::Nothing && paused) {
                        return self.second_trigger(cue, option, at, cx);
                    }
                }
                Some(Slot::Loaded(_)) | None => {}
            }
        }
        if !self.budget_left(cx) {
            return cx.warn(EngineWarning::ChainLimit {
                cue,
                limit: cx.limit,
            });
        }
        cx.budget -= 1;
        if panicked {
            self.as_of(at, cx, |e, cx| e.stop_tree(cue, None, cx));
        }
        let before = self.state(cue);
        let (run, split, voice) = match self.slots.get(&cue) {
            Some(Slot::Broken(_)) => return cx.warn(EngineWarning::CueBroken { cue }),
            // Also a broken one: its frozen timers (a pending auto-continue) run again.
            Some(Slot::Active(run)) if run.paused_at.is_some() => {
                return self.resume_tree(cue, at, cx);
            }
            Some(Slot::Active(run)) if run.is_broken() => {
                return cx.warn(EngineWarning::CueBroken { cue });
            }
            Some(Slot::Active(run)) => {
                return cx.warn(EngineWarning::TriggerIgnored {
                    cue,
                    state: run.state(),
                });
            }
            Some(Slot::Loaded(loaded)) => (
                loaded.run,
                (loaded.pre_wait_left, loaded.action_offset),
                if loaded.voice {
                    Voice::Loaded
                } else {
                    Voice::None
                },
            ),
            None => (
                self.new_run(),
                (cx.secs(model.pre_wait, &self.config), 0),
                Voice::None,
            ),
        };
        let (pre_wait_left, action_offset) = split;
        let until = at.saturating_add(pre_wait_left);
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        let group = matches!(model.kind, CueKind::Group { .. }).then(GroupRun::default);
        let mut new = Run::new(run, seq, Phase::PreWait { until }, group);
        new.action_offset = action_offset;
        new.voice = voice;
        new.started = at;
        // A statically broken cue does not start, but keeps its timing as a disarmed cue does,
        // so its auto-continue fires on schedule (user decision, M2.13).
        if self.refuses(cue, cx) {
            cx.warn(EngineWarning::CueBroken { cue });
            new.static_broken = true;
        }
        self.slots.insert(cue, Slot::Active(new));
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
            // Started broken: it owns no voice; stopping ends it (and its pending continue).
            Some(Slot::Active(run)) if run.static_broken => {}
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
                    let at = run.commit(now.max(started), cx.horizon);
                    cx.out.push(Action::StopVoice {
                        cue,
                        run: run.run,
                        at,
                        fade: Some(StopFade { frames, curve }),
                    });
                    run.stop_end = Some(at.saturating_add(frames));
                    run.post_wait = None;
                    run.follow = false;
                    // A stopped target cancels a pending devamp and its next cue.
                    run.devamp = None;
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
        // Not before a command already committed for the voice in the lookahead window (a ramp,
        // a devamp): the pause comes after it, and the timers freeze there.
        let now = now.max(run.floor(cx.horizon));
        run.paused_at = Some(now);
        if let (Voice::Started { at: started, .. }, Phase::Action { .. }) = (run.voice, run.phase) {
            let at = run.commit(now.max(started), cx.horizon);
            cx.out.push(Action::PauseVoice {
                cue,
                run: run.run,
                at,
                offset: run.position(at),
            });
        }
        // A paused Fade cue holds its ramps where they are.
        if let Some(fade) = run.fade.clone() {
            let seq = run.seq;
            self.hold_fade(cue, seq, &fade, now, true, cx);
        }
        self.report(cue, before, now, cx);
    }

    /// Resumes a paused cue as of sample `at`: every timer keeps the time it had left.
    fn resume_one(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        let before = self.state(cue);
        let now = cx.now;
        // Only a paused cue takes a fresh id (a resume of anything else changes nothing).
        if !matches!(self.slots.get(&cue), Some(Slot::Active(run)) if run.paused_at.is_some()) {
            return;
        }
        let fresh = self.new_run();
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return;
        };
        let Some(paused_at) = run.paused_at.take() else {
            return;
        };
        // Where the action was frozen. A resume dated before the pause (a trigger from a late
        // deadline, or an input before a pause committed in the lookahead window) resumes as of
        // the pause: nothing for the voice goes backwards in time.
        let at = at.max(run::after_restart(paused_at, cx.horizon));
        let offset = run.position(paused_at);
        run.shift(at.saturating_sub(paused_at));
        let mut devamp = None;
        if let (Voice::Started { .. }, Phase::Action { .. }) = (run.voice, run.phase) {
            let voice_at = run.commit(at.max(now), cx.horizon);
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
            let first = match run.voice {
                Voice::Started { first, .. } => first,
                Voice::None | Voice::Loaded => voice_at,
            };
            run.voice = Voice::Started {
                at: voice_at,
                from: offset,
                first,
            };
            // A known end follows the voice, which resumes on its own sample.
            if let (Some(end), Phase::Action { start, .. }) = (run.planned_end(), run.phase) {
                run.phase = Phase::Action {
                    start,
                    end: Some(end),
                };
            }
            // A devamp still pending is sent again for the resumed segment (QLab keeps it
            // through a pause); answers for the paused segment's id are stale.
            if let Some(d) = run.devamp.as_mut() {
                d.at = None;
                devamp = Some(Action::DevampVoice {
                    cue,
                    run: fresh,
                    at: voice_at,
                    stop: d.stop,
                });
            }
        }
        cx.out.extend(devamp);
        if run.fade.is_some() {
            self.resume_fade(cue, at, cx);
        }
        self.report(cue, before, at, cx);
    }

    fn load(&mut self, cue: CueId, time: f64, cx: &mut Ctx) {
        let Some(model) = cx.ws.find_cue(cue) else {
            return cx.warn(EngineWarning::UnknownCue { cue });
        };
        if matches!(self.slots.get(&cue), Some(Slot::Loaded(_)) | None) && self.refuses(cue, cx) {
            return cx.warn(EngineWarning::CueBroken { cue });
        }
        match self.slots.get(&cue) {
            Some(Slot::Broken(_)) => return cx.warn(EngineWarning::CueBroken { cue }),
            Some(Slot::Active(run)) if run.is_broken() => {
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
        let voice = self.is_armed(model) && matches!(model.kind, CueKind::Audio { .. });
        // The split between pre-wait and action is fixed here, for the stream and the start.
        let pre_wait = cx.secs(model.pre_wait, &self.config);
        let (pre_wait_left, action_offset) = if offset < pre_wait {
            (pre_wait - offset, 0)
        } else {
            (0, offset - pre_wait)
        };
        if voice {
            cx.out.push(Action::LoadVoice {
                cue,
                run,
                offset: action_offset,
            });
        }
        self.slots.insert(
            cue,
            Slot::Loaded(Loaded {
                run,
                pre_wait_left,
                action_offset,
                voice,
            }),
        );
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
        if self.preview_ended(cue, run_id, at, cx) {
            return;
        }
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
        // An end known in advance (`ActionLength`) is a deadline; a report before it is an
        // earlier end and still completes the action.
        let (Phase::Action { start, .. }, Voice::Started { .. }) = (run.phase, run.voice) else {
            return;
        };
        let at = at.max(start);
        run.phase = Phase::Done;
        run.voice = Voice::None;
        let follow = run.follow;
        let seq = run.seq;
        // The voice ended before the devamp's deadline was carried out (it left the loop
        // there): the Devamp cue's next cue still starts, on the devamp sample.
        let devamp = run.devamp.take();
        self.settle(cue, at, cx);
        self.report(cue, before, at, cx);
        if follow {
            self.continue_after(cue, at, cx);
        }
        if let Some(DevampWait {
            by,
            start_next: true,
            at: Some(left),
            ..
        }) = devamp
        {
            self.continue_after(by, left.min(at), cx);
        }
        self.child_action_done(cue, seq, at, cx);
    }

    /// The binary knows how long the run's action is: its end becomes a deadline. Ignored for a
    /// run that is not playing its voice in its action (stale, stopping, broken, paused before
    /// its start), and for previews (their end changes nothing in the show).
    fn action_length(&mut self, cue: CueId, run_id: RunId, frames: SampleTime) {
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return;
        };
        if run.run != run_id || run.is_broken() || run.stop_end.is_some() {
            return;
        }
        let (Phase::Action { start, .. }, Voice::Started { .. }) = (run.phase, run.voice) else {
            return;
        };
        // Counted from the voice's (re)start; a resume counts it again from the resumed voice.
        run.length = Some(frames);
        run.phase = Phase::Action {
            start,
            end: run.planned_end(),
        };
    }

    fn action_failed(&mut self, cue: CueId, run_id: RunId, reason: BrokenReason, cx: &mut Ctx) {
        if self.preview_failed(cue, run_id, reason, cx) {
            return;
        }
        let before = self.state(cue);
        // A group owns no voice, so no feedback can concern it.
        let group = cx
            .ws
            .find_cue(cue)
            .is_some_and(|c| matches!(c.kind, CueKind::Group { .. }));
        let current = match self.slots.get(&cue) {
            // Nor a running fade (it owns no voice either).
            Some(Slot::Active(run)) => {
                run.run == run_id && !run.is_broken() && run.group.is_none() && run.fade.is_none()
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
                run.devamp = None;
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
        // budget), a run's own progress (pre-wait, action end, post-wait) and devamp answers (at
        // most one per run), so this ends; the guard is a second line of defence against a bug
        // turning into a hung show.
        let mut steps = 0usize;
        while let Some((cue, at)) = self.earliest_due(cx.horizon) {
            // Read each round: a stale limit can be raised while this runs (`budget_left`).
            let guard = self
                .slots
                .len()
                .saturating_add(cx.limit)
                .saturating_add(1)
                .saturating_mul(4);
            if steps >= guard {
                return cx.warn(EngineWarning::ChainLimit {
                    cue,
                    limit: cx.limit,
                });
            }
            steps += 1;
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
        if let Some(d) = run.devamp
            && d.at.is_some_and(|left| left <= at)
        {
            // The voice leaves its loop here: the Devamp cue's next cue starts on this sample.
            run.devamp = None;
            if d.start_next {
                self.continue_after(d.by, at, cx);
            }
            return;
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
            // An audio action ended on its known length: its voice is done with, and the
            // mixer's report that follows is stale.
            run.voice = Voice::None;
            run.length = None;
            let follow = run.follow;
            let seq = run.seq;
            if run.fade.is_some() {
                self.fade_finish(cue, at, cx);
                // Stopping its target stopped the Fade cue too (it is inside the group); that
                // stop reported it.
                if !matches!(self.slots.get(&cue), Some(Slot::Active(r)) if r.seq == seq) {
                    return;
                }
            }
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
        // A run that broke during its pre-wait, or a cue that has static problems now, skips its
        // action but keeps its timing.
        let problems = self.refuses(cue, cx);
        let broken =
            problems || matches!(self.slots.get(&cue), Some(Slot::Active(r)) if r.is_broken());
        let act = match &model.kind {
            _ if !self.is_armed(model) || broken => Act::Silent(0),
            CueKind::Audio { .. } => Act::Voice,
            CueKind::Wait { duration } => Act::Silent(cx.secs(*duration, &self.config)),
            CueKind::Memo => Act::Silent(0),
            CueKind::Group { .. } => Act::Group,
            CueKind::Fade { duration, .. } => Act::Fade(cx.secs(*duration, &self.config)),
            CueKind::Start { .. }
            | CueKind::Stop { .. }
            | CueKind::Pause { .. }
            | CueKind::Load { .. }
            | CueKind::Reset { .. }
            | CueKind::Devamp { .. }
            | CueKind::GoTo { .. }
            | CueKind::Arm { .. }
            | CueKind::Disarm { .. } => Act::Control,
            CueKind::Network { .. } => {
                cx.warn(EngineWarning::Unsupported { cue });
                Act::Silent(0)
            }
        };
        // QLab: a Devamp cue's "start next cue" is its continue, fired when the loop is left
        // (see `devamp`), so its own continue mode does not also start the next cue. A broken
        // one sends no devamp, so, like a disarmed one, its own continue mode applies.
        let continue_mode = if starts_next_on_devamp(model, self.is_armed(model) && !broken) {
            ContinueMode::DoNotContinue
        } else {
            group::effective_continue(ws, model)
        };
        let now = cx.now;
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return;
        };
        let offset = run.action_offset;
        if problems && run.broken.is_none() {
            run.static_broken = true;
        }
        match act {
            Act::Voice => {
                let voice_at = run.commit(at.max(now), cx.horizon);
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
                    first: voice_at,
                };
            }
            Act::Silent(_) | Act::Group | Act::Fade(_) | Act::Control => {
                if run.voice == Voice::Loaded {
                    cx.out.push(Action::UnloadVoice { cue, run: run.run });
                }
                run.voice = Voice::None;
            }
        }
        run.phase = Phase::Action {
            start: at,
            end: match act {
                Act::Silent(d) | Act::Fade(d) => Some(at.saturating_add(d.saturating_sub(offset))),
                Act::Control => Some(at),
                Act::Voice | Act::Group => None,
            },
        };
        run.post_wait = (continue_mode == ContinueMode::AutoContinue)
            .then(|| at.saturating_add(post_wait.saturating_sub(offset)));
        run.follow = !broken && continue_mode == ContinueMode::AutoFollow;
        self.settle(cue, at, cx);
        self.report(cue, before, at, cx);
        // The cue is in its action while it acts, so a trigger cycle back to it is ignored.
        match act {
            Act::Group => self.start_children(cue, at, cx),
            Act::Fade(_) => self.fade_begin(cue, at, cx),
            Act::Control => self.control(cue, at, cx),
            Act::Voice | Act::Silent(_) => {}
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
            let exit = if run.is_broken() || failed {
                Exit::Broken
            } else {
                Exit::Completed
            };
            match run.broken {
                // Its device is back already: nothing keeps it from playing.
                Some(BrokenReason::DeviceLost) if self.device_back(cue) => self.slots.remove(&cue),
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
        if let Slot::Active(run) = &slot {
            cx.vacated.push((cue, at, exit));
            // A Fade cue stopped mid-fade leaves its targets' levels where they are.
            if let Some(fade) = &run.fade {
                self.hold_fade(cue, run.seq, fade, at.max(cx.now), false, cx);
            }
        }
        Some(slot)
    }

    fn report(&mut self, cue: CueId, before: CueState, at: SampleTime, cx: &mut Ctx) {
        let state = self.state(cue);
        if state != before {
            cx.out.push(Action::StateChanged { cue, state, at });
            self.settle_problems(cue, state, cx);
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
                // Never before a command already committed for the voice.
                at: cx.now.max(at).max(run.floor(cx.horizon)),
                fade: None,
            }),
            Voice::Loaded => cx.out.push(Action::UnloadVoice { cue, run: run.run }),
            Voice::None => {}
        },
        Slot::Loaded(_) | Slot::Broken(_) => {}
    }
}

/// Whether `cue` is an armed Devamp cue that starts its next cue when the loop is left; that
/// replaces its own continue mode (and counts as a continue for the playhead).
pub(super) fn starts_next_on_devamp(cue: &Cue, armed: bool) -> bool {
    armed
        && matches!(
            cue.kind,
            CueKind::Devamp {
                start_next_cue: true,
                ..
            }
        )
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
    /// Fades its target's levels; completes after this many samples.
    Fade(SampleTime),
    /// Acts on its target at once and completes.
    Control,
    /// Runs the group's children; completes when none is busy.
    Group,
}

/// Every cue in `cues`, depth-first. Iterative, so nesting depth cannot overflow the stack.
fn each_cue<'a>(cues: &'a [Cue], mut f: impl FnMut(&'a Cue)) {
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
