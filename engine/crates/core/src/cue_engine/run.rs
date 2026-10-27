// SPDX-License-Identifier: GPL-3.0-or-later

//! A cue's runtime: its timers and action progress, kept as absolute sample deadlines.

use std::collections::{BTreeMap, BTreeSet};

use crate::cue::FadeCurve;
use crate::model::CueId;

use super::level::{Level, LevelParam};
use super::time::SampleTime;
use super::types::{BrokenReason, CueState, RunId};

/// Runtime of a cue that is not idle. Idle cues have no slot.
// Runs live in one ordered map with an entry per non-idle cue; boxing them would add an
// allocation per start for no gain.
#[expect(clippy::large_enum_variant, reason = "few slots, one per non-idle cue")]
#[derive(Clone, Debug)]
pub(super) enum Slot {
    Loaded(Loaded),
    Active(Run),
    Broken(BrokenReason),
}

impl Slot {
    pub fn state(&self) -> CueState {
        match self {
            Self::Loaded(_) => CueState::Loaded,
            Self::Active(run) => run.state(),
            Self::Broken(_) => CueState::Broken,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Loaded {
    pub run: RunId,
    /// Pre-wait left when it starts and the samples into the action it starts from: the split
    /// of the load-to-time offset into pre-wait and action, frozen at load so the stream primed
    /// by `LoadVoice` is the one `StartVoice` asks for even if the cue's pre-wait is edited
    /// meanwhile.
    pub pre_wait_left: SampleTime,
    pub action_offset: SampleTime,
    /// A `LoadVoice` was sent for this run.
    pub voice: bool,
}

/// Where the cue is between its start and its action's end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    /// The action starts at `until`.
    PreWait {
        until: SampleTime,
    },
    /// The action started on `start`; `end` is `None` until feedback says (audio).
    Action {
        start: SampleTime,
        end: Option<SampleTime>,
    },
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Voice {
    None,
    /// A `LoadVoice` was sent but no `StartVoice`.
    Loaded,
    /// The voice was (re)started on sample `at`, `from` samples into the action, and has not
    /// ended. Its position is counted from here, not from the timeline: a start can be later
    /// than its deadline (late tick) or after a pause that came before it (lookahead).
    Started {
        at: SampleTime,
        from: SampleTime,
        /// The sample the mixer voice first started on. A resume re-keys the same voice, so
        /// this stays put while `at` becomes the resume sample: only a ramp on this sample is
        /// immediate, any later 0-frame ramp is de-zippered (docs/audio.md, De-zipper).
        first: SampleTime,
    },
}

#[derive(Clone, Debug)]
pub(super) struct Run {
    pub run: RunId,
    /// Creation order; breaks ties between equal deadlines so causes come before effects.
    pub seq: u64,
    pub phase: Phase,
    /// Samples into the action it starts from (load-to-time past the pre-wait).
    pub action_offset: SampleTime,
    /// Auto-continue: the next cue is triggered on this sample.
    pub post_wait: Option<SampleTime>,
    /// Auto-follow, as the continue mode was when the action started.
    pub follow: bool,
    pub voice: Voice,
    /// Audio cues: the action's length in samples (from its start, offset included), when the
    /// binary knows it (`Input::ActionLength`); the action then ends when the voice gets there.
    pub length: Option<SampleTime>,
    pub paused_at: Option<SampleTime>,
    /// Stopping with a fade-out that ends on this sample.
    pub stop_end: Option<SampleTime>,
    /// The latest sample a command for this run's voice was sent for. Commands in the lookahead
    /// window are committed, so a later command for the voice is never dated before it.
    pub committed: SampleTime,
    /// The run failed. It owns no voice and never follows, but its auto-continue still fires on
    /// schedule (a `device_lost` failure cancels it); once nothing is pending the slot becomes [`Slot::Broken`].
    pub broken: Option<BrokenReason>,
    /// Started (or reached its action) while the cue had static problems (`CueProblem`): its
    /// action is skipped, as for a disarmed cue, and its continue (auto-continue or auto-follow)
    /// keeps its schedule (QLab 5). Once nothing is pending the slot is removed (the problems,
    /// not the run, make it report `broken` then).
    pub static_broken: bool,
    /// Broke (`broken`) before its voice produced any audio (`Input::StartFailed`): skipped,
    /// as a cue with static problems is, so it still follows and its group is not failed.
    pub unplayed: bool,
    /// Stopping because of a Panic. Such a cue is no longer active for the second-trigger rule:
    /// a trigger hard-stops it and starts it afresh (the operator recovering from a panic).
    pub panicked: bool,
    /// The sample the run was triggered on. A second trigger that would restart it on that same
    /// sample is ignored, so a trigger chain back to a restarting cue cannot restart it again and
    /// again within one sample.
    pub started: SampleTime,
    /// Set for Group cues: how the group runs its children.
    pub group: Option<GroupRun>,
    /// Audio cues: the gains Fade cues have set or are moving (the model's levels apply to the
    /// rest). They belong to this run and go with it.
    pub levels: BTreeMap<LevelParam, Level>,
    /// Audio cues: the Fade cues whose ramps are moving its levels now (it reports `fading`).
    pub faders: BTreeSet<CueId>,
    /// Audio cues: a devamp sent for its voice and not yet carried out.
    pub devamp: Option<DevampWait>,
    /// Fade cues, from their action start: what the fade is doing.
    pub fade: Option<FadeRun>,
}

/// A devamp sent to a cue's voice by a Devamp cue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DevampWait {
    /// The Devamp cue.
    pub by: CueId,
    /// Start the cue after the Devamp cue when the loop is left.
    pub start_next: bool,
    /// End the voice there ("stop target when slice ends").
    pub stop: bool,
    /// Where the voice leaves the loop, once the audio side has said (`DevampScheduled`).
    pub at: Option<SampleTime>,
}

/// One gain a Fade cue ramps: on `cue`'s run `seq`, towards `gain`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct FadeRamp {
    pub cue: CueId,
    pub seq: u64,
    pub param: LevelParam,
    pub gain: f32,
}

/// A Fade cue's part of its run.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct FadeRun {
    pub ramps: Vec<FadeRamp>,
    /// The audio cues (and runs) it fades.
    pub targets: Vec<(CueId, u64)>,
    /// Stop this cue (the Fade's target) when the fade completes.
    pub stop_target: Option<CueId>,
    pub curve: FadeCurve,
}

/// How a cue's runtime ended, as its group sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Exit {
    /// The action completed and nothing is pending.
    Completed,
    /// It broke at run time (a group: a child broke at run time during its run). A run skipped
    /// for static problems is `Completed`: its group passes over it.
    Broken,
    /// Stopped, reset or deleted.
    Stopped,
}

/// A Group cue's part of its run. Its action lasts while any of its children is busy (has a
/// runtime that is not idle, loaded or settled broken).
#[derive(Clone, Debug, Default)]
pub(super) struct GroupRun {
    /// Stopping with a fade: ends when its last busy child has stopped.
    pub stopping: bool,
    /// A child broke at run time during this run, so the group does not auto-follow.
    pub failed: bool,
    /// A Devamp cue asked this playlist to finish its current child and then complete: the
    /// Devamp cue, and whether it starts the cue after it then.
    pub devamp: Option<(CueId, bool)>,
    /// How the last busy child left while the group was paused; the group ends on resume.
    pub pending: Option<Exit>,
    /// A playlist's current child completed or broke while the group was paused (its report
    /// was in flight): the playlist moves on when the group resumes.
    pub advance: bool,
    pub playlist: Option<PlaylistRun>,
    /// The sample of the last "play next/previous" second trigger: at most one per sample.
    pub skipped: Option<SampleTime>,
}

/// Where a playlist group is in its pass.
#[derive(Clone, Debug, Default)]
pub(super) struct PlaylistRun {
    /// The child playing now and its run's `seq`, so a late report of an earlier run of the same
    /// child (a one-child loop) cannot move the playlist on twice.
    pub current: Option<(CueId, u64)>,
    /// Unshuffled pass: the index the current child had among the group's children when it
    /// started (the next child is the one after it as the group stands, or the one that took its
    /// place if it was removed).
    pub index: usize,
    /// Shuffled pass: the pass's order (fixed when the pass starts) and the current position.
    /// Empty for an unshuffled pass.
    pub order: Vec<CueId>,
    pub pos: usize,
}

impl Run {
    /// A run in `phase` with nothing else pending.
    pub fn new(run: RunId, seq: u64, phase: Phase, group: Option<GroupRun>) -> Self {
        Self {
            run,
            seq,
            phase,
            action_offset: 0,
            post_wait: None,
            follow: false,
            voice: Voice::None,
            length: None,
            paused_at: None,
            stop_end: None,
            committed: 0,
            broken: None,
            static_broken: false,
            unplayed: false,
            panicked: false,
            started: 0,
            group,
            levels: BTreeMap::new(),
            faders: BTreeSet::new(),
            devamp: None,
            fade: None,
        }
    }

    pub fn state(&self) -> CueState {
        if self.is_broken() {
            CueState::Broken
        } else if self.paused_at.is_some() {
            CueState::Paused
        } else if self.stop_end.is_some() || self.is_group_stopping() {
            CueState::Stopping
        } else if !self.faders.is_empty() && matches!(self.phase, Phase::Action { .. }) {
            CueState::Fading
        } else {
            match self.phase {
                Phase::PreWait { .. } => CueState::PreWaiting,
                Phase::Action { .. } => CueState::Running,
                Phase::Done => CueState::PostWaiting,
            }
        }
    }

    /// Broken at run time or started broken: it owns no voice and never follows.
    pub fn is_broken(&self) -> bool {
        self.broken.is_some() || self.static_broken
    }

    pub fn is_group_stopping(&self) -> bool {
        self.group.as_ref().is_some_and(|g| g.stopping)
    }

    /// The next sample something happens on by itself; `None` while paused or waiting on
    /// feedback only.
    pub fn next_deadline(&self) -> Option<SampleTime> {
        if self.paused_at.is_some() {
            return None;
        }
        if self.stop_end.is_some() {
            return self.stop_end;
        }
        let phase = match self.phase {
            Phase::PreWait { until } => Some(until),
            Phase::Action { end, .. } => end,
            Phase::Done => None,
        };
        let devamp = self.devamp.and_then(|d| d.at);
        [phase, self.post_wait, devamp].into_iter().flatten().min()
    }

    /// Nothing left to do: the action is complete and no post-wait or fade-out is pending.
    pub fn is_finished(&self) -> bool {
        self.phase == Phase::Done && self.post_wait.is_none() && self.stop_end.is_none()
    }

    /// Samples into the action the voice has reached at sample `at` (the action offset while
    /// no voice plays).
    pub fn position(&self, at: SampleTime) -> SampleTime {
        match self.voice {
            Voice::Started {
                at: started, from, ..
            } => from.saturating_add(at.saturating_sub(started)),
            Voice::None | Voice::Loaded => self.action_offset,
        }
    }

    /// The earliest sample a new command for the voice may be dated on, for a call that commits
    /// up to `horizon`. A committed sample beyond the horizon can only come from a clock that went
    /// back (device restart); it is then ignored, as double-GO ignores such a GO.
    pub fn floor(&self, horizon: SampleTime) -> SampleTime {
        after_restart(self.committed, horizon)
    }

    /// Dates a command for this run's voice: no earlier than `at` nor than any command already
    /// committed for it (see `floor`).
    pub fn commit(&mut self, at: SampleTime, horizon: SampleTime) -> SampleTime {
        self.committed = at.max(self.floor(horizon));
        self.committed
    }

    /// The sample the action ends on, from its known length and where the voice (re)started.
    pub fn planned_end(&self) -> Option<SampleTime> {
        let (Phase::Action { start, .. }, Voice::Started { at, from, .. }, Some(length)) =
            (self.phase, self.voice, self.length)
        else {
            return None;
        };
        Some(at.saturating_add(length.saturating_sub(from)).max(start))
    }

    /// Moves every pending deadline `by` samples later (resume after a pause).
    pub fn shift(&mut self, by: SampleTime) {
        let later = |t: SampleTime| t.saturating_add(by);
        self.phase = match self.phase {
            Phase::PreWait { until } => Phase::PreWait {
                until: later(until),
            },
            Phase::Action { start, end } => Phase::Action {
                start: later(start),
                end: end.map(later),
            },
            Phase::Done => Phase::Done,
        };
        self.post_wait = self.post_wait.map(later);
        self.stop_end = self.stop_end.map(later);
        if let Some(d) = self.devamp.as_mut() {
            d.at = d.at.map(later);
        }
    }
}

/// `t` if a call that commits up to `horizon` can have committed it, else 0 (the clock went back).
pub(super) fn after_restart(t: SampleTime, horizon: SampleTime) -> SampleTime {
    if t <= horizon { t } else { 0 }
}
