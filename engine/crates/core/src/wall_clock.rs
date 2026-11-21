// SPDX-License-Identifier: GPL-3.0-or-later

//! Wall-clock triggers (TRG-05): which cues' times of day the computer's local clock has
//! reached since the last look. Pure: the engine reads the clock and passes in, at each look,
//! the real time (whole UTC seconds since 1970) and the local time (whole seconds since
//! 1970-01-01 00:00 local: UTC plus the zone's offset, so a daylight-saving change moves it as
//! the wall clock moves).
//!
//! How far REAL time moved decides whether the step counts: a step of more than
//! [`MAX_GAP_SECS`] real seconds, or one back (the computer asleep, the control thread held up,
//! the clock set by hand) fires nothing and reports the times it went past; a missed time
//! never fires late. Within a counting step, LOCAL time decides what was crossed: when the
//! local clock moved further than real time (daylight saving starting, the zone changed) only
//! its last real seconds fire, and the times it skipped over are reported missed. A local clock
//! that went back fires nothing on the way back, and a cue never fires twice for one local date
//! and second (the hour daylight saving repeats when it ends). The first look after the
//! workspace opens only notes the time: a time already passed today does not fire.

use std::collections::BTreeMap;

use crate::cue::Weekday;
use crate::model::{CueId, Workspace};

/// The longest real step between two looks at the clock that still fires the times crossed in
/// it. The engine looks every few milliseconds; a step this long means it was held up, the
/// computer slept, or the clock was set.
pub const MAX_GAP_SECS: i64 = 2;

const DAY: i64 = 86_400;

/// Most days of a long gap looked through for the times it missed (only for reporting).
const MAX_MISSED_DAYS: i64 = 8;

/// One look at the clock: whole seconds of real (UTC) and local time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WallTime {
    pub utc: i64,
    pub local: i64,
}

/// What one look at the clock found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WallClockStep {
    /// Cues to start now, in workspace order.
    pub start: Vec<CueId>,
    /// Cues whose time the clock passed over without firing them, in workspace order.
    pub missed: Vec<CueId>,
    /// The local clock did not move with real time: it jumped (set, slept, held up), skipped
    /// (daylight saving starting) or went back.
    pub jump: Option<WallClockJump>,
}

/// A step in which the local clock did not move as real time did, or real time moved too far
/// (or back) for the step to count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WallClockJump {
    /// Local seconds at the look before, and now.
    pub from: i64,
    pub to: i64,
    /// Real seconds between the two looks.
    pub real: i64,
}

/// The wall-clock triggers of one open workspace.
#[derive(Clone, Debug, Default)]
pub struct WallClockWatch {
    /// The last look; `None` until the first.
    last: Option<WallTime>,
    /// The local instant each cue last fired at.
    fired: BTreeMap<CueId, i64>,
}

impl WallClockWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Looks at the clock. Cheap when neither second has changed (the engine calls it on every
    /// wake).
    pub fn advance(&mut self, ws: &Workspace, now: WallTime) -> WallClockStep {
        let Some(last) = self.last else {
            self.last = Some(now);
            return WallClockStep::default();
        };
        if now == last {
            return WallClockStep::default();
        }
        self.last = Some(now);
        let real = now.utc.saturating_sub(last.utc);
        let moved = now.local.saturating_sub(last.local);
        let counts = (0..=MAX_GAP_SECS).contains(&real);
        let jump = (moved != real || !counts).then_some(WallClockJump {
            from: last.local,
            to: now.local,
            real,
        });
        let mut step = WallClockStep {
            jump,
            ..WallClockStep::default()
        };
        if moved <= 0 {
            return step;
        }
        if !counts {
            step.missed = Self::passed_over(ws, last.local, now.local);
            self.forget_before(now.local);
            return step;
        }
        // Only the last `real` seconds of local time were lived; anything before them was
        // skipped by the zone's offset changing.
        let lived_from = if moved > real {
            now.local.saturating_sub(real)
        } else {
            last.local
        };
        if lived_from > last.local {
            step.missed = Self::passed_over(ws, last.local, lived_from);
        }
        for cue in ws.cues() {
            if let Some(at) = crossed(cue, lived_from, now.local)
                && self.fired.get(&cue.id) != Some(&at)
            {
                self.fired.insert(cue.id, at);
                step.start.push(cue.id);
            }
        }
        if last.local.div_euclid(DAY) != now.local.div_euclid(DAY) || step.jump.is_some() {
            self.forget_before(now.local);
        }
        step
    }

    /// The cues whose time lies in local `(from, to]` (at most the last few days of it).
    fn passed_over(ws: &Workspace, from: i64, to: i64) -> Vec<CueId> {
        let from = from.max(to.saturating_sub(MAX_MISSED_DAYS * DAY));
        ws.cues()
            .filter(|cue| crossed(cue, from, to).is_some())
            .map(|cue| cue.id)
            .collect()
    }

    /// Drops what no step can match again: an instant more than a day and a half old (the
    /// clock going back over a daylight-saving end repeats at most a few hours).
    fn forget_before(&mut self, now: i64) {
        let keep = now.saturating_sub(DAY + DAY / 2);
        self.fired.retain(|_, at| *at >= keep);
    }
}

/// The latest instant in `(from, to]` at which `cue`'s wall-clock trigger fires, if any.
fn crossed(cue: &crate::cue::Cue, from: i64, to: i64) -> Option<i64> {
    let trigger = &cue.triggers.wall_clock;
    if !trigger.enabled {
        return None;
    }
    let second = i64::from(trigger.second_of_day()?);
    let mut day = to.div_euclid(DAY);
    let first = from.div_euclid(DAY);
    while day >= first {
        let at = day.saturating_mul(DAY).saturating_add(second);
        if at > from && at <= to && trigger.days.contains(Weekday::of_day(day)) {
            return Some(at);
        }
        day -= 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_one_was_a_thursday() {
        assert_eq!(Weekday::of_day(0), Weekday::Thursday);
        assert_eq!(Weekday::of_day(3), Weekday::Sunday);
        assert_eq!(Weekday::of_day(-1), Weekday::Wednesday);
    }
}
