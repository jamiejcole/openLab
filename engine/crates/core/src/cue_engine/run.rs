// SPDX-License-Identifier: GPL-3.0-or-later

//! A cue's runtime: its timers and action progress, kept as absolute sample deadlines.

use crate::model::CueId;

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
    /// The voice was (re)started on sample `at`, `from` samples into the action, and has not
    /// ended. Its position is counted from here, not from the timeline: a start can be later
    /// than its deadline (late tick) or after a pause that came before it (lookahead).
    Started {
        at: SampleTime,
        from: SampleTime,
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
    /// The run failed. It owns no voice and never follows, but its auto-continue still fires on
    /// schedule; once nothing is pending the slot becomes [`Slot::Broken`].
    pub broken: Option<BrokenReason>,
    /// Set for Group cues: how the group runs its children.
    pub group: Option<GroupRun>,
}

/// How a cue's runtime ended, as its group sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Exit {
    /// The action completed and nothing is pending.
    Completed,
    /// It broke (a group: a child broke during its run).
    Broken,
    /// Stopped, reset or deleted.
    Stopped,
}

/// A Group cue's part of its run. Its action lasts while any of its children is busy (has a
/// runtime that is not idle, loaded or settled broken).
#[derive(Clone, Debug, Default)]
pub(super) struct GroupRun {
    /// Running because a child was started on its own while the group was idle: it tracks its
    /// children but starts none and fires no continue.
    pub adopted: bool,
    /// Stopping with a fade: ends when its last busy child has stopped.
    pub stopping: bool,
    /// A child broke during this run, so the group does not auto-follow.
    pub failed: bool,
    /// How the last busy child left while the group was paused; the group ends on resume.
    pub pending: Option<Exit>,
    /// A playlist's current child completed or broke while the group was paused (its report
    /// was in flight): the playlist moves on when the group resumes.
    pub advance: bool,
    pub playlist: Option<PlaylistRun>,
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
    pub fn state(&self) -> CueState {
        if self.broken.is_some() {
            CueState::Broken
        } else if self.paused_at.is_some() {
            CueState::Paused
        } else if self.stop_end.is_some() || self.is_group_stopping() {
            CueState::Stopping
        } else {
            match self.phase {
                Phase::PreWait { .. } => CueState::PreWaiting,
                Phase::Action { .. } => CueState::Running,
                Phase::Done => CueState::PostWaiting,
            }
        }
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
        [phase, self.post_wait].into_iter().flatten().min()
    }

    /// Nothing left to do: the action is complete and no post-wait or fade-out is pending.
    pub fn is_finished(&self) -> bool {
        self.phase == Phase::Done && self.post_wait.is_none() && self.stop_end.is_none()
    }

    /// Samples into the action the voice has reached at sample `at` (the action offset while
    /// no voice plays).
    pub fn position(&self, at: SampleTime) -> SampleTime {
        match self.voice {
            Voice::Started { at: started, from } => from.saturating_add(at.saturating_sub(started)),
            Voice::None | Voice::Loaded => self.action_offset,
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
