// SPDX-License-Identifier: GPL-3.0-or-later

//! Wall-clock triggers (TRG-05): which cues' times of day the computer's local clock has
//! reached since the last look. Pure: the engine reads the clock and passes the local time in
//! as whole seconds since 1970-01-01 00:00 local time (UTC seconds plus the zone's offset, so
//! a daylight-saving change moves it as the wall clock moves).
//!
//! A time fires only when it is crossed between two looks no more than [`MAX_GAP_SECS`]
//! apart. A longer gap (the computer asleep, the control thread held up, the clock set
//! forward, the hour lost when daylight saving starts) fires nothing and reports the times it
//! went past: a missed time never fires late. A clock that goes back fires nothing on the way
//! back, and a cue never fires twice for the same local date and second (the hour daylight
//! saving repeats when it ends). The first look after the workspace opens only notes the time:
//! a time already passed today does not fire.

use std::collections::BTreeMap;

use crate::cue::Weekday;
use crate::model::{CueId, Workspace};

/// The longest step between two looks at the clock that still fires the times crossed in it.
/// The engine looks every few milliseconds; a step this long means it was held up, the
/// computer slept, or the clock was set.
pub const MAX_GAP_SECS: i64 = 2;

const DAY: i64 = 86_400;

/// Most days of a long gap looked through for the times it missed (only for reporting).
const MAX_MISSED_DAYS: i64 = 8;

/// What one look at the clock found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WallClockStep {
    /// Cues to start now, in workspace order.
    pub start: Vec<CueId>,
    /// The clock moved further than a step: nothing fired.
    pub jump: Option<WallClockJump>,
}

/// A step of the clock that fired nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WallClockJump {
    /// Local seconds at the look before, and now.
    pub from: i64,
    pub to: i64,
    /// Cues whose time was passed over (forward jumps only), in workspace order.
    pub missed: Vec<CueId>,
}

/// The wall-clock triggers of one open workspace.
#[derive(Clone, Debug, Default)]
pub struct WallClockWatch {
    /// The local second of the last look; `None` until the first.
    last: Option<i64>,
    /// The local instant each cue last fired at.
    fired: BTreeMap<CueId, i64>,
}

impl WallClockWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Looks at the clock: `now` is the local time in whole seconds since 1970-01-01 local.
    /// Cheap when the second has not changed (the engine calls it on every wake).
    pub fn advance(&mut self, ws: &Workspace, now: i64) -> WallClockStep {
        let Some(last) = self.last else {
            self.last = Some(now);
            return WallClockStep::default();
        };
        if now == last {
            return WallClockStep::default();
        }
        self.last = Some(now);
        if now < last {
            return WallClockStep {
                start: Vec::new(),
                jump: Some(WallClockJump {
                    from: last,
                    to: now,
                    missed: Vec::new(),
                }),
            };
        }
        let gap = now.saturating_sub(last);
        if gap > MAX_GAP_SECS {
            let from = last.max(now.saturating_sub(MAX_MISSED_DAYS * DAY));
            let missed = ws
                .cues()
                .filter(|cue| crossed(cue, from, now).is_some())
                .map(|cue| cue.id)
                .collect();
            self.forget_before(now);
            return WallClockStep {
                start: Vec::new(),
                jump: Some(WallClockJump {
                    from: last,
                    to: now,
                    missed,
                }),
            };
        }
        let mut start = Vec::new();
        for cue in ws.cues() {
            if let Some(at) = crossed(cue, last, now)
                && self.fired.get(&cue.id) != Some(&at)
            {
                self.fired.insert(cue.id, at);
                start.push(cue.id);
            }
        }
        if last.div_euclid(DAY) != now.div_euclid(DAY) {
            self.forget_before(now);
        }
        WallClockStep { start, jump: None }
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
