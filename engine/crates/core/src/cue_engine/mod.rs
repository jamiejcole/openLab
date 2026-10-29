// SPDX-License-Identifier: GPL-3.0-or-later

//! The cue engine: cue lifecycle, timing, continue modes, groups, GO and playheads
//! (ARCHITECTURE §3.3).
//!
//! Pure and deterministic: [`CueEngine::handle`] takes the current workspace, one [`Input`] and
//! the sample time, and returns the [`Action`]s to carry out. Semantics are specified in
//! `docs/cue-engine.md`.

mod broken;
mod control;
mod edits;
mod fade;
mod go;
mod group;
mod level;
mod live_levels;
mod operations;
mod playhead;
mod preview;
mod rng;
mod run;
mod second;
mod shape;
mod time;
mod timers;
mod transport;
mod types;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet, HashSet};

pub use level::{EDIT_RAMP_SECONDS, LevelParam, LevelRamp, curve_shape, db_to_gain, ramp_gain};
pub use preview::{PREVIEW_RUN_BASE, PREVIEW_STOP_FADE_SECONDS};
pub use time::*;
pub use timers::{Timer, TimerProgress};
pub use transport::PANIC_CURVE;
pub use types::*;

use crate::cue::{ContinueMode, Cue, CueKind, GroupMode};
use crate::model::{CueId, CueListId, Workspace};
use crate::tree;

use playhead::Playhead;
use preview::{PanickedPreview, Preview};
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
    /// Groups whose run has fired its continue during the playhead's current visit (forgotten
    /// when they start again, are reset, or the playhead enters them any way but the GO on the
    /// group: `forget_continues`): the playhead leaving such a group from a child skips its
    /// continue targets, which have played; leaving a group that has no run and fired none, it
    /// stands on the cue right after it, so those targets are not lost (`playhead::after_go`).
    continued: BTreeSet<CueId>,
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
    /// Groups being previewed and the audio cues inside them that their preview plays still.
    group_previews: BTreeMap<CueId, BTreeSet<CueId>>,
    /// Voices of previews fading out in the panic under way (see `preview.rs`).
    panicked_previews: Vec<PanickedPreview>,
    next_preview: u64,
    /// `chain_limit` of the workspace as of the last such pass.
    limit: usize,
    /// Nominal action lengths of Audio cues, from the binary (`Input::CueLengths`); a cue not
    /// here has none known.
    lengths: BTreeMap<CueId, SampleTime>,
    /// How many such passes have run (a cost measure, see `model_syncs`).
    model_syncs: u64,
    /// Walks of the whole workspace made to follow edits (a cost measure, see
    /// `whole_show_passes`).
    whole_show_passes: u64,
    /// The `now` of the last call and the cue starts made by the calls at that `now` so far.
    /// The chain budget is per sample, not per call: mixer feedback dated on the sample it is
    /// handled at (a zero-length voice ends on its start sample) could otherwise carry a cycle
    /// into call after call at one `now`, each with a fresh budget (see "Chains and the chain
    /// bound").
    spent: (SampleTime, usize),
    /// The outline release builds check for a missed `model_changed` (see `shape.rs`).
    shape: shape::Shape,
    /// Tests: behave as a release build (no full check on every call).
    #[cfg(test)]
    release_checks: bool,
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
            continued: BTreeSet::new(),
            armed: BTreeMap::new(),
            problems: broken::Problems::default(),
            panic: None,
            model_dirty: true,
            previews: BTreeMap::new(),
            group_previews: BTreeMap::new(),
            panicked_previews: Vec::new(),
            next_preview: 0,
            limit: 0,
            lengths: BTreeMap::new(),
            model_syncs: 0,
            whole_show_passes: 0,
            spent: (0, 0),
            shape: shape::Shape::default(),
            #[cfg(test)]
            release_checks: false,
        }
    }

    /// Whether every call checks the model against a full rebuild (debug builds). Release
    /// builds check the cheap outline instead (`shape.rs`).
    fn full_checks(&self) -> bool {
        #[cfg(test)]
        if self.release_checks {
            return false;
        }
        cfg!(debug_assertions)
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
    /// M4.2 contract. Release builds skip those whole-workspace passes on every other call
    /// unless the workspace's outline changed (`shape.rs`: then the miss is counted and the
    /// passes run), so a missed signal inside a cue or group leaves them out of date; what
    /// could then refuse or drop a cue is guarded where it happens (a cue about to be refused for its problems is rechecked, a spent chain
    /// budget is recounted, a GO whose playhead cue is gone resyncs that list) and counted in
    /// [`Self::unsignalled_model_changes`]. Debug builds run the passes on every call and count.
    pub fn model_changed(&mut self) {
        self.model_dirty = true;
        self.mark_problems_dirty();
    }

    /// [`Self::model_changed`] for an edit that changed only the cues `footprint` names (a
    /// command, undo or redo: `Command::footprint`, M4.1a): the static problems are brought up
    /// to date for those cues and the cues that depend on them (targets, group membership), not
    /// for the whole workspace, so an edit costs what it touched (NFR-24). The footprint must
    /// name every cue the edit added, removed, moved or changed; a change to patches, cue lists
    /// or anything it does not name needs `model_changed`. A missed cue is caught as an
    /// unsignalled change in debug builds and when a refused cue is rechecked, as for a missed
    /// `model_changed`. Equal to `model_changed` in every observable way (property-tested).
    pub fn model_changed_cues(&mut self, footprint: &crate::changes::Footprint) {
        self.model_dirty = true;
        self.mark_problems_changed(footprint);
    }

    /// How many times the whole-workspace passes triggered by [`Self::model_changed`] have run
    /// (a cost measure: a call with nothing signalled adds none).
    pub fn model_syncs(&self) -> u64 {
        self.model_syncs
    }

    /// How many times following edits walked the whole workspace: counting its cues, reading a
    /// list's positions for its playhead, collecting the cue ids present (M4.12c). The engine's
    /// cue index answers these, so an edit told with `model_changed_cues` adds none; a full
    /// rebuild (`model_changed`, a newly opened workspace) and an index that cannot be trusted
    /// (duplicate ids) do.
    pub fn whole_show_passes(&self) -> u64 {
        self.whole_show_passes
    }

    /// Applies one input at scheduling time `now` (the earliest sample a command can still hit).
    /// `ws` is the current workspace; edits since the last call are taken into account first.
    pub fn handle(&mut self, ws: &Workspace, input: Input, now: SampleTime) -> Vec<Action> {
        // Whole-tree passes (the chain limit, following edits) only after `model_changed`, so a
        // Tick on a big show costs nothing extra. Debug builds run them every call and count a
        // change nobody signalled, as for the static problems; release builds compare the
        // workspace's outline and, if it changed, count it and follow as if signalled.
        let signalled = std::mem::take(&mut self.model_dirty);
        let full = self.full_checks();
        let outline_changed = !signalled && !full && !self.shape.matches(ws);
        if outline_changed {
            if self.unsignalled_model_changes() == self.shape.unsignalled() {
                self.note_unsignalled();
            }
            self.mark_problems_dirty();
        }
        let follow_edits = signalled || full || outline_changed;
        if signalled {
            self.model_syncs = self.model_syncs.saturating_add(1);
        }
        let spent = match self.spent {
            (at, starts) if at == now => starts,
            _ => 0,
        };
        // The budget is set once the problems (and the cue index with them) follow the edits:
        // nothing before that starts a cue.
        let mut cx = Ctx {
            ws,
            now,
            horizon: self.config.horizon(now),
            budget: 0,
            limit: self.limit,
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
        // Their fades have ended (or the clock went back): a later Panic has nothing to cut.
        if self.panic.is_none() {
            self.panicked_previews.clear();
        }
        // Before the runtimes follow the edits, so a deleted cue loses its problems while its
        // runtime still gives its state, and reports idle once. Only does work after
        // `model_changed`.
        let counted = self.unsignalled_model_changes();
        self.sync_problems(&mut cx);
        if follow_edits {
            self.limit = self.counted_chain_limit(ws);
        }
        cx.limit = self.limit;
        cx.budget = self.limit.saturating_sub(spent);
        if follow_edits {
            let changed = self.sync_model(&mut cx) | self.sync_playheads(&mut cx);
            // Once per call: the problems may have noticed the same edit.
            if changed
                && !signalled
                && !outline_changed
                && self.unsignalled_model_changes() == counted
            {
                self.note_unsignalled();
            }
            self.shape.record(ws, self.unsignalled_model_changes());
        }
        self.advance(&mut cx);
        // After the deadlines due in this call: a fade ending in it has let go of its levels,
        // so an edit of one is not left to it.
        if follow_edits {
            self.follow_level_edits(&mut cx);
        }
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

    /// The cue's reported state: its runtime's, else `broken` if it has static problems or is a
    /// group with a descendant shown with problems (QLab 5), else idle.
    pub fn state(&self, cue: CueId) -> CueState {
        match self.slots.get(&cue) {
            Some(slot) => slot.state(),
            None if self.has_problems(cue) || self.has_broken_descendant(cue) => CueState::Broken,
            None => CueState::Idle,
        }
    }

    /// Whether any cue is doing something (pre-waiting through stopping; `CueState::is_active`).
    /// Cheap: only cues with a runtime are looked at.
    pub fn has_active_cues(&self) -> bool {
        self.slots.values().any(|slot| slot.state().is_active())
    }

    /// The first cue, in id order, that the engine would stop if the workspace became `ws`: an
    /// active cue (`CueState::is_active`, a group's run included) that `ws` no longer has or has
    /// as the other sort (group or not), or a previewed cue `ws` no longer has as an Audio cue
    /// (as a Group, for a group's preview). These are the cues the engine stops when it follows
    /// such an edit. The binary refuses an undo or redo that would stop one (M5.10d): whoever
    /// presses undo cannot predict that, so it must not cut a cue the audience hears.
    ///
    /// Free while nothing plays; otherwise one walk of `ws`, ending once every candidate is
    /// found.
    pub fn cue_an_edit_would_stop(&self, ws: &Workspace) -> Option<CueId> {
        #[derive(Clone, Copy)]
        enum Wants {
            Group,
            NotGroup,
            Audio,
        }
        let mut wanted: Vec<(CueId, Wants)> = Vec::new();
        for (id, slot) in &self.slots {
            if let Slot::Active(run) = slot
                && run.state().is_active()
            {
                let wants = match run.group {
                    Some(_) => Wants::Group,
                    None => Wants::NotGroup,
                };
                wanted.push((*id, wants));
            }
        }
        wanted.extend(self.previews.keys().map(|id| (*id, Wants::Audio)));
        wanted.extend(self.group_previews.keys().map(|id| (*id, Wants::Group)));
        if wanted.is_empty() {
            return None;
        }
        let ids: HashSet<CueId> = wanted.iter().map(|(id, _)| *id).collect();
        // The first cue with each id in `Workspace::find_cue`'s order (lists in order, cues
        // depth-first), as the engine reads it. Iterative: nesting depth cannot overflow.
        let mut found: BTreeMap<CueId, &Cue> = BTreeMap::new();
        for cue in ws.cues() {
            if ids.contains(&cue.id) {
                found.entry(cue.id).or_insert(cue);
                if found.len() == ids.len() {
                    break;
                }
            }
        }
        wanted
            .into_iter()
            .filter(|(id, wants)| {
                let Some(cue) = found.get(id) else {
                    return true;
                };
                let group = matches!(cue.kind, CueKind::Group { .. });
                match wants {
                    Wants::Group => !group,
                    Wants::NotGroup => group,
                    Wants::Audio => !matches!(cue.kind, CueKind::Audio { .. }),
                }
            })
            .map(|(id, _)| id)
            .min()
    }

    /// Every cue that is not idle, in id order.
    pub fn cue_states(&self) -> impl Iterator<Item = (CueId, CueState)> + '_ {
        let mut all: BTreeMap<CueId, CueState> = self
            .slots
            .iter()
            .map(|(id, slot)| (*id, slot.state()))
            .collect();
        let groups = self.problems_below();
        for id in self
            .broken_cues()
            .into_iter()
            .map(|(id, _)| id)
            .chain(groups)
        {
            all.entry(id).or_insert(CueState::Broken);
        }
        all.into_iter()
    }

    /// Whether the cue is paused, including a cue that broke while paused (its state reads
    /// `broken`, but its timers are frozen until it is resumed).
    pub fn is_paused(&self, cue: CueId) -> bool {
        matches!(self.slots.get(&cue), Some(Slot::Active(run)) if run.paused_at.is_some())
    }

    /// The Audio cue's nominal action length as the binary last told it (`Input::CueLengths`).
    pub fn nominal_length(&self, cue: CueId) -> Option<SampleTime> {
        self.lengths.get(&cue).copied()
    }

    /// Whether the cue is a playlist group halted by a child that broke while playing, still in
    /// its action (QLab check Q2: it runs on to that child's nominal end with no busy child).
    pub fn is_halted(&self, cue: CueId) -> bool {
        matches!(
            self.slots.get(&cue),
            Some(Slot::Active(Run {
                group: Some(GroupRun { halt: Some(_), .. }),
                phase: Phase::Action { .. },
                ..
            }))
        )
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

    /// [`Self::chain_limit`] from the cue index's count, which is in step with `ws` once the
    /// problems follow the edits; counted from `ws` when the index has duplicate ids (it counts
    /// each id once).
    fn counted_chain_limit(&mut self, ws: &Workspace) -> usize {
        let tree = self.problems.tree();
        if tree.has_duplicates() {
            self.whole_show_passes = self.whole_show_passes.saturating_add(1);
            Self::chain_limit(ws)
        } else {
            tree.len().saturating_add(CHAIN_MARGIN)
        }
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
            Input::GoFrom { list, cue } => self.go_from(list, cue, cx),
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
            Input::CueLengths { lengths } => {
                for (cue, length) in lengths {
                    match length {
                        Some(frames) => self.lengths.insert(cue, frames),
                        None => self.lengths.remove(&cue),
                    };
                }
            }
            Input::ActionFailed { cue, run, reason } => {
                self.action_failed(cue, run, reason, false, cx);
            }
            Input::StartFailed { cue, run, reason } => {
                self.action_failed(cue, run, reason, true, cx);
            }
            Input::DevampScheduled { cue, run, at } => self.devamp_scheduled(cue, run, at),
            Input::DevampIgnored { cue, run } => self.devamp_ignored(cue, run, cx),
            Input::Preview { cue } => self.preview(cue, cx),
            Input::StopPreview { cue } => self.stop_previews(cue, false, cx),
            Input::Panic => self.panic(cx),
            Input::StopAll => self.stop_all(cx),
            Input::PanicCue { cue } => {
                if self.known(cue, cx) {
                    // A second press on a cue already fading out in a panic stops it at once
                    // (QLab check Q7).
                    if self.fading_in_panic(cue, cx) {
                        self.stop_tree(cue, None, cx);
                    } else {
                        self.panic_tree(cue, cx);
                    }
                }
            }
            Input::PauseAll => self.pause_all(cx),
            Input::ResumeAll => self.resume_all(cx),
            Input::MediaStatus { media, status } => {
                self.availability(vec![(media, status)], Vec::new(), Vec::new(), cx);
            }
            Input::PatchStatus { patch, resolution } => {
                self.availability(Vec::new(), vec![(patch, resolution)], Vec::new(), cx);
            }
            Input::Availability { media, patches } => {
                self.availability(media, patches, Vec::new(), cx);
            }
            Input::NetworkAvailability { patches } => {
                self.availability(Vec::new(), Vec::new(), patches, cx);
            }
            Input::DeviceLost => self.device_lost(cx),
        }
    }

    fn known(&self, cue: CueId, cx: &mut Ctx) -> bool {
        let known = cx.ws.find_cue(cue).is_some();
        if !known {
            cx.warn(EngineWarning::UnknownCue { cue });
        }
        known
    }

    // ----- feedback -----

    fn action_ended(&mut self, cue: CueId, run_id: RunId, at: SampleTime, cx: &mut Ctx) {
        if self.preview_ended(cue, run_id, at, cx) {
            return;
        }
        let before = self.state(cue);
        // Whether the end of a paused run would start a sibling or a runless playlist's next
        // child (looked up before the run is borrowed).
        let paused = matches!(self.slots.get(&cue), Some(Slot::Active(r)) if r.paused_at.is_some());
        let chains = paused && group::chains_siblings(cx.ws, cue);
        let runless = paused && in_runless_playlist(self, cue, cx.ws);
        let Some(Slot::Active(run)) = self.slots.get_mut(&cue) else {
            return;
        };
        if run.run != run_id {
            return;
        }
        // A paused voice cannot end by itself; an end dated after the pause is the pause being
        // carried out (e.g. as a stop), not the action completing. An end dated on the pause
        // sample is an end (M2.34): the mixer dates it on the first sample after the voice's
        // last, so the voice had nothing left to play when the pause applied, and a known end
        // on that sample (`ActionLength`) is carried out before the pause, too.
        if run.paused_at.is_some_and(|paused| at > paused) {
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
        // Dated before or on the pause, arriving after it (M2.33): a paused show starts nothing,
        // so when this end would start a cue (its auto-follow, its Devamp cue's next cue, or the
        // next child of a playlist it was started in on its own), the end is held as a deadline
        // on the pause sample, which the resume moves to the resume sample, as for a failure
        // while paused. The voice is gone, so the resume sends nothing for it. A stop, reset or
        // panic cancels it.
        if let Some(paused) = run.paused_at {
            let start_next = matches!(
                run.devamp,
                Some(DevampWait {
                    start_next: true,
                    at: Some(_),
                    ..
                })
            );
            if (run.follow && chains) || start_next || runless {
                run.voice = Voice::None;
                run.length = None;
                run.phase = Phase::Action {
                    start,
                    end: Some(paused.max(start)),
                };
                // Its next cue starts there too (`step` carries out the devamp first).
                run.devamp = run
                    .devamp
                    .filter(|d| d.start_next && d.at.is_some())
                    .map(|d| DevampWait {
                        at: Some(paused),
                        ..d
                    });
                return self.report(cue, before, cx.now, cx);
            }
        }
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

    /// The run's voice or stream failed; `unplayed`: before it produced any audio, so the cue
    /// is skipped as a cue with static problems is (its auto-follow fires, its group is not
    /// failed) instead of breaking at run time (rule 2).
    fn action_failed(
        &mut self,
        cue: CueId,
        run_id: RunId,
        reason: BrokenReason,
        unplayed: bool,
        cx: &mut Ctx,
    ) {
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
            // Nor a run whose voice already ended while it is paused, its end held for the
            // resume (`action_ended`): the voice is gone, so a failure report cannot concern it.
            Some(Slot::Active(run)) => {
                run.run == run_id
                    && !run.is_broken()
                    && run.group.is_none()
                    && run.fade.is_none()
                    && !(run.paused_at.is_some()
                        && run.voice == Voice::None
                        && matches!(run.phase, Phase::Action { .. }))
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
            // Being stopped with a fade, or fading out in a panic (a workspace Panic or the
            // cue's own): it was leaving anyway and the failure only ends that early. It ends as
            // its stop would have, but broken so the failure shows: its group hears a stop, and
            // nothing moves on from it (no continue, no playlist step, no halt), so a failure
            // never starts a cue during a panic or after a stop (M2.31).
            // A lost device (M4.2g, as QLab) ends the run the same way: the device loss stops
            // everything a moment later, so nothing may move on in between (its pre-wait and
            // post-wait are cancelled too, so nothing it scheduled starts after the device
            // returns). The binary's patch update can have marked every audio cue broken by
            // then, but a control cue would still act.
            Slot::Active(run)
                if run.stop_end.is_some() || run.panicked || reason == BrokenReason::DeviceLost =>
            {
                if !(reason == BrokenReason::DeviceLost && self.device_back(cue)) {
                    self.slots.insert(cue, Slot::Broken(reason));
                }
                cx.vacated.push((cue, cx.now, Exit::Stopped));
                self.report(cue, before, cx.now, cx);
            }
            // Auto-continue is time-based: an armed or still-to-be-armed post-wait fires on its
            // schedule. Auto-follow waits for a successful completion, so it never fires.
            Slot::Active(mut run) => {
                // A child that broke while playing halts a running playlist around it (QLab
                // check Q2).
                let halt = (!unplayed).then(|| self.nominal_end(cue, &run));
                run.broken = Some(reason);
                run.unplayed = unplayed;
                run.voice = Voice::None;
                // Skipped before it played: it follows as of now (as a cue skipped for static
                // problems does at its action start); one that broke while playing never does.
                let follow = run.follow && unplayed;
                run.devamp = None;
                // Paused: a paused show starts nothing, so the action's end is held until the
                // resume (as a deadline on the pause sample, which the resume moves to the
                // resume sample), and with it what that end starts: a skipped cue's follow, or
                // the next child of a playlist it was started in on its own (which has no run to
                // hold it, as a running playlist does: `GroupRun::advance`). A stop or panic
                // cancels it.
                if let (Some(paused), Phase::Action { start, .. }) = (run.paused_at, run.phase)
                    && (follow || in_runless_playlist(self, cue, cx.ws))
                {
                    run.follow = follow;
                    run.phase = Phase::Action {
                        start,
                        end: Some(paused),
                    };
                    run.length = None;
                    self.slots.insert(cue, Slot::Active(run));
                    return self.report(cue, before, cx.now, cx);
                }
                run.follow = false;
                if matches!(run.phase, Phase::Action { .. }) {
                    run.phase = Phase::Done;
                }
                let seq = run.seq;
                self.slots.insert(cue, Slot::Active(run));
                self.settle(cue, cx.now, cx);
                self.report(cue, before, cx.now, cx);
                if follow {
                    self.continue_after(cue, cx.now, cx);
                }
                if let Some(end) = halt {
                    self.halt_playlist(cue, seq, end, cx);
                }
                // A playlist moves on past a child that broke (unless it halted).
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
            // A group's action has an end only while a halted playlist waits out its broken
            // child's nominal end (`halt_playlist`).
            if run.group.is_some() {
                return self.resolve_group(cue, Exit::Completed, at, cx);
            }
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
        // Broke at run time (feedback during its pre-wait): it never auto-follows (rule 2),
        // unless it failed before playing anything (skipped, as for static problems).
        let failed = matches!(self.slots.get(&cue),
            Some(Slot::Active(r)) if r.broken.is_some() && !r.unplayed);
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
            CueKind::Network { .. } => Act::Send,
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
                // The binary starts the voice with the model's levels as they are now.
                if let CueKind::Audio { levels, .. } = &model.kind {
                    run.given = Some(run::Given {
                        model: levels.clone(),
                        played: level::played(levels),
                    });
                }
            }
            Act::Send => {
                if let CueKind::Network {
                    destination,
                    address,
                    args,
                } = &model.kind
                {
                    cx.out.push(Action::SendOsc {
                        cue,
                        at,
                        destination: *destination,
                        address: address.clone(),
                        args: args.clone(),
                    });
                }
                if run.voice == Voice::Loaded {
                    cx.out.push(Action::UnloadVoice { cue, run: run.run });
                }
                run.voice = Voice::None;
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
                Act::Control | Act::Send => Some(at),
                Act::Voice | Act::Group => None,
            },
        };
        run.post_wait = (continue_mode == ContinueMode::AutoContinue)
            .then(|| at.saturating_add(post_wait.saturating_sub(offset)));
        run.post_wait_total = post_wait;
        // A cue skipped for static problems follows as a disarmed one does (QLab 5: GO on a
        // broken auto-follow cue starts the next cue); one that broke at run time does not.
        run.follow = !failed && continue_mode == ContinueMode::AutoFollow;
        self.settle(cue, at, cx);
        self.report(cue, before, at, cx);
        // The cue is in its action while it acts, so a trigger cycle back to it is ignored.
        match act {
            Act::Group => self.start_children(cue, at, cx),
            Act::Fade(_) => self.fade_begin(cue, at, cx),
            Act::Control => self.control(cue, at, cx),
            Act::Voice | Act::Silent(_) | Act::Send => {}
        }
    }

    /// Triggers the cue after `cue` in its list or group, on sample `at`. Inside timeline,
    /// playlist and random groups the group runs its children, so nothing is triggered.
    fn continue_after(&mut self, cue: CueId, at: SampleTime, cx: &mut Ctx) {
        if !group::chains_siblings(cx.ws, cue) {
            return;
        }
        if cx
            .ws
            .find_cue(cue)
            .is_some_and(|c| matches!(c.kind, CueKind::Group { .. }))
        {
            self.continued.insert(cue);
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
            // A run skipped for static problems completed as far as its group is concerned (it
            // is passed over, as a disarmed cue is); one that broke at run time did not.
            let failed = run.group.as_ref().is_some_and(|g| g.failed);
            let exit = if (run.broken.is_some() && !run.unplayed) || failed {
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

/// Whether `cue` is a child of a playlist group with no run of its own, which a child's end
/// carries on (`follow_through`).
fn in_runless_playlist(engine: &CueEngine, cue: CueId, ws: &Workspace) -> bool {
    group::ancestors(ws, cue).first().is_some_and(|parent| {
        !matches!(engine.slots.get(parent), Some(Slot::Active(_)))
            && matches!(
                ws.find_cue(*parent).map(|g| &g.kind),
                Some(CueKind::Group {
                    mode: GroupMode::Playlist { .. },
                    ..
                })
            )
    })
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
    /// Sends a Network cue's OSC message and completes at once.
    Send,
    /// Runs the group's children; completes when none is busy.
    Group,
}

fn collect_ids(cues: &[Cue], ids: &mut HashSet<CueId>) {
    ids.extend(crate::tree::descendants(cues).map(|c| c.id));
}

fn count_cues(cues: &[Cue]) -> usize {
    crate::tree::descendants(cues).count()
}
