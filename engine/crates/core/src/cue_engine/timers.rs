// SPDX-License-Identifier: GPL-3.0-or-later

//! The countdowns a client shows for cues without a voice: pre-waits, silent actions (Wait,
//! Fade, Group, ...) and post-waits (docs/cue-engine.md, "Timer progress").

use crate::model::CueId;

use super::CueEngine;
use super::run::{Phase, Run, Slot, Voice};
use super::time::SampleTime;

/// Which of a run's timers a [`TimerProgress`] is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Timer {
    PreWait,
    /// The action of a cue that has no voice (an audio cue's comes from its voice).
    Action,
    /// The auto-continue's post-wait (or a halted playlist's wait for its auto-follow).
    PostWait,
}

/// One running timer at a sample: how far it has got and how much is left, in samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimerProgress {
    pub cue: CueId,
    pub timer: Timer,
    pub elapsed: SampleTime,
    /// `None` when the end is not known (a group still running its children).
    pub remaining: Option<SampleTime>,
}

impl CueEngine {
    /// Every timer counting at rendered sample `at`, by cue, then pre-wait, action, post-wait.
    /// A paused run's timers stand where the pause froze them. A run that is broken (at run
    /// time or by static problems) skips its action, so only its pre-wait and post-wait show:
    /// they keep their schedule. Loaded and idle cues have none. Allocates nothing itself.
    pub fn timers(&self, at: SampleTime) -> impl Iterator<Item = TimerProgress> + '_ {
        self.slots.iter().flat_map(move |(cue, slot)| {
            let run = match slot {
                Slot::Active(run) => Some(run),
                Slot::Loaded(_) | Slot::Broken(_) => None,
            };
            let [pre, action, post] = run.map_or([None; 3], |run| run_timers(*cue, run, at));
            pre.into_iter().chain(action).chain(post)
        })
    }
}

fn run_timers(cue: CueId, run: &Run, at: SampleTime) -> [Option<TimerProgress>; 3] {
    // The pause froze the timers on its sample; a pause committed ahead of `at` (lookahead)
    // has not been reached yet.
    let at = run.paused_at.map_or(at, |paused| at.min(paused));
    // A deadline more than the timer's length ahead was scheduled from a sample after `at`
    // (a start in the lookahead window): the timer has not begun, so it shows its whole length.
    let countdown = |timer, until: SampleTime, total: SampleTime| {
        let remaining = until.saturating_sub(at).min(total);
        TimerProgress {
            cue,
            timer,
            elapsed: total.saturating_sub(remaining),
            remaining: Some(remaining),
        }
    };
    let pre = match run.phase {
        Phase::PreWait { until } => Some(countdown(Timer::PreWait, until, run.pre_wait_total)),
        Phase::Action { .. } | Phase::Done => None,
    };
    let action = match (run.phase, run.voice) {
        (Phase::Action { start, end }, Voice::None) if !run.is_broken() => {
            let at = at.max(start);
            Some(TimerProgress {
                cue,
                timer: Timer::Action,
                elapsed: run.action_offset.saturating_add(at.saturating_sub(start)),
                remaining: end.map(|end| end.saturating_sub(at)),
            })
        }
        _ => None,
    };
    let post = run
        .post_wait
        .map(|until| countdown(Timer::PostWait, until, run.post_wait_total));
    [pre, action, post]
}
