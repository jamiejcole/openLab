// SPDX-License-Identifier: GPL-3.0-or-later

//! A cue's runtime: its timers and action progress, kept as absolute sample deadlines.

use super::time::SampleTime;
use super::types::{BrokenReason, CueState, RunId};

/// Runtime of a cue that is not idle. Idle cues have no slot.
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
    /// Samples into the cue's timeline (pre-wait, then action) it will start from.
    pub offset: SampleTime,
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
    /// A `StartVoice` was sent for sample `at` and the voice has not ended.
    Started {
        at: SampleTime,
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
    pub paused_at: Option<SampleTime>,
    /// Stopping with a fade-out that ends on this sample.
    pub stop_end: Option<SampleTime>,
}

impl Run {
    pub fn state(&self) -> CueState {
        if self.paused_at.is_some() {
            CueState::Paused
        } else if self.stop_end.is_some() {
            CueState::Stopping
        } else {
            match self.phase {
                Phase::PreWait { .. } => CueState::PreWaiting,
                Phase::Action { .. } => CueState::Running,
                Phase::Done => CueState::PostWaiting,
            }
        }
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
        [phase, self.post_wait].into_iter().flatten().min()
    }

    /// Nothing left to do: the action is complete and no post-wait or fade-out is pending.
    pub fn is_finished(&self) -> bool {
        self.phase == Phase::Done && self.post_wait.is_none() && self.stop_end.is_none()
    }

    /// Samples into the action at sample `at`.
    pub fn position(&self, at: SampleTime) -> SampleTime {
        match self.phase {
            Phase::Action { start, .. } => {
                self.action_offset.saturating_add(at.saturating_sub(start))
            }
            Phase::PreWait { .. } | Phase::Done => self.action_offset,
        }
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
    }
}
