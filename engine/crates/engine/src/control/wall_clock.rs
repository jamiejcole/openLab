// SPDX-License-Identifier: GPL-3.0-or-later

//! Wall-clock triggers (TRG-05) on the control thread: each wake reads the epoch clock, turns it
//! into the computer's local time with the zone's offset ([`LocalZone`]), and lets each doc's
//! [`WallClockWatch`] say which cues to start; they start as a hotkey starts them (a `start`
//! transport action: second-trigger rules, the playhead does not move). What the watch passes
//! over (a sleep, a stalled loop, the clock set) is logged, never started late.

use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use openlab_core::protocol::{LogKind, LogLevel, RuntimeChange, TransportAction};
use openlab_core::{CueId, SampleTime, WallClockJump, WallClockWatch, WallTime};

use super::{DocCx, Engine, notice};

/// Seconds ahead of the moment it is worked out that the system zone's offset is known: a
/// daylight-saving change is met on its very second although the offset is worked out off the
/// control thread.
const AHEAD_SECS: i64 = 4;
/// How often the system zone's offsets are worked out again.
const REFRESH: Duration = Duration::from_millis(250);
/// Most cues a passed-over step names one by one in the log and notices (the rest are counted).
const MISSED_LOGGED: usize = 20;
/// How long the local time may be unknown before a doc with a trigger on raises a notice (the
/// zone's first lookup takes a moment at start).
const UNKNOWN_NOTICE_SECS: i64 = 2;
/// The `source` of the wall-clock triggers' log records.
const SOURCE: &str = "wall clock";

/// Offsets of the next few UTC seconds: `offsets[i]` is the offset at `from + i`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OffsetTable {
    from: i64,
    offsets: Vec<i64>,
}

impl OffsetTable {
    /// The table for `from .. from + AHEAD_SECS` from `offset_at`; seconds it fails for are
    /// left out from the first failure on.
    pub fn work_out(from: i64, offset_at: impl Fn(i64) -> Option<i64>) -> Self {
        let offsets = (0..AHEAD_SECS)
            .map_while(|i| offset_at(from.saturating_add(i)))
            .collect();
        Self { from, offsets }
    }

    pub fn offset_at(&self, utc: i64) -> Option<i64> {
        let i = usize::try_from(utc.checked_sub(self.from)?).ok()?;
        self.offsets.get(i).copied()
    }
}

/// Where the control thread gets the zone's offset at a UTC second: the system's (worked out on
/// a thread of its own, as the zone database may be read from disk), a fixed one (the offline
/// driver, a stepped virtual device: UTC), or a test's.
#[derive(Clone)]
pub struct LocalZone(Arc<dyn Fn(i64) -> Option<i64> + Send + Sync>);

impl LocalZone {
    /// No zone: the local time is never known, so no wall-clock trigger fires (the zone's
    /// thread could not start).
    pub fn unknown() -> Self {
        Self(Arc::new(|_| None))
    }

    /// A zone this many seconds ahead of UTC, all year.
    pub fn fixed(offset: i64) -> Self {
        Self(Arc::new(move |_| Some(offset)))
    }

    pub fn new(offset_at: impl Fn(i64) -> Option<i64> + Send + Sync + 'static) -> Self {
        Self(Arc::new(offset_at))
    }

    /// The computer's zone. A thread (`openlab-localtime`) works out the offsets of the next
    /// few seconds every [`REFRESH`] and ends when the last clone of this is dropped. The
    /// control thread never waits for it: while the table is being replaced, or when it does
    /// not cover the second asked for, the offset given last is used. `None` before any.
    pub fn system() -> std::io::Result<Self> {
        let table: Arc<Mutex<OffsetTable>> = Arc::default();
        let weak = Arc::downgrade(&table);
        std::thread::Builder::new()
            .name("openlab-localtime".into())
            .spawn(move || refresh_while_wanted(&weak))?;
        let last: Mutex<Option<i64>> = Mutex::new(None);
        Ok(Self::new(move |utc| {
            let fresh = table.try_lock().ok().and_then(|t| t.offset_at(utc));
            let Ok(mut last) = last.try_lock() else {
                return fresh;
            };
            if fresh.is_some() {
                *last = fresh;
            }
            *last
        }))
    }

    pub fn offset_at(&self, utc: i64) -> Option<i64> {
        (self.0)(utc)
    }
}

impl std::fmt::Debug for LocalZone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LocalZone")
    }
}

fn refresh_while_wanted(table: &Weak<Mutex<OffsetTable>>) {
    loop {
        let utc = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_secs()).ok())
            .unwrap_or(0);
        let fresh = OffsetTable::work_out(utc, crate::platform::local_time::utc_offset_at);
        let Some(table) = table.upgrade() else {
            return;
        };
        if let Ok(mut t) = table.lock() {
            *t = fresh;
        }
        drop(table);
        std::thread::sleep(REFRESH);
    }
}

impl Engine {
    /// Notes the local time now in whole seconds since 1970-01-01 local, once per wake
    /// ([`super::Control::tick`]); `None` while the zone is not known.
    pub(super) fn read_local_time(&mut self) {
        let utc = self.config.clock.now_ms().div_euclid(1000);
        if self.local_time.0 != Some(utc) {
            let local = self
                .config
                .local_zone
                .offset_at(utc)
                .and_then(|o| utc.checked_add(o));
            self.local_time = (Some(utc), local);
        }
    }
}

impl DocCx<'_> {
    /// The cues whose wall-clock time this wake reached start; what the clock passed over is
    /// logged and raised as a notice for each cue missed. While the local time is not known no
    /// trigger fires, the first time known after is a fresh start (nothing caught up), and a
    /// doc with a trigger on raises a notice once the time has been unknown
    /// [`UNKNOWN_NOTICE_SECS`].
    pub(super) fn wall_clock_triggers(&mut self, now: SampleTime) {
        let (utc, local) = self.eng.local_time;
        let Some(utc) = utc else { return };
        let Some(local) = local else {
            self.wall_clock_unknown(utc);
            return;
        };
        if self
            .doc
            .wall_clock_unknown
            .take()
            .is_some_and(|(_, noted)| noted)
        {
            self.record(
                LogKind::Trigger,
                LogLevel::Info,
                "the computer's local time is known again: time-of-day triggers are on".to_owned(),
                None,
                Some(SOURCE.to_owned()),
            );
        }
        let step = {
            let ws = self.doc.model.ws();
            self.doc.wall_clock.advance(ws, WallTime { utc, local })
        };
        if let Some(jump) = step.jump {
            self.log_jump(&jump, step.missed.len());
        }
        for &cue in step.missed.iter().take(MISSED_LOGGED) {
            self.wall_clock_missed(cue);
        }
        for cue in step.start {
            self.wall_clock_start(cue, local, now);
        }
    }

    /// No local time this wake: the watch starts afresh when one comes, and a doc with a
    /// trigger on hears of it once it has lasted.
    fn wall_clock_unknown(&mut self, utc: i64) {
        self.doc.wall_clock = WallClockWatch::new();
        let (since, noted) = *self.doc.wall_clock_unknown.get_or_insert((utc, false));
        let lasted = utc.saturating_sub(since) >= UNKNOWN_NOTICE_SECS;
        let any_on = || {
            self.doc
                .model
                .ws()
                .cues()
                .any(|c| c.triggers.wall_clock.enabled)
        };
        if noted || !lasted || !any_on() {
            return;
        }
        self.doc.wall_clock_unknown = Some((since, true));
        let message = "the computer's local time cannot be read: time-of-day triggers are off \
                       until it can"
            .to_owned();
        self.record(
            LogKind::Trigger,
            LogLevel::Error,
            message.clone(),
            None,
            Some(SOURCE.to_owned()),
        );
        let changes = vec![RuntimeChange::Warning {
            warning: notice("wall_clock_unknown", message, None),
        }];
        self.observe_changes(&changes);
        self.runtime(changes);
    }

    /// A cue whose time the clock passed over: a warning in the log and a notice the operator
    /// sees.
    fn wall_clock_missed(&mut self, cue: CueId) {
        let label = self.cue_label(cue);
        let message = format!(
            "time of day: {label} was not started: its time passed while the clock was not \
             followed (asleep, held up, set, or skipped by a time change)"
        );
        self.record(
            LogKind::Trigger,
            LogLevel::Warn,
            message.clone(),
            Some(cue),
            Some(SOURCE.to_owned()),
        );
        let changes = vec![RuntimeChange::Warning {
            warning: notice("wall_clock_missed", message, Some(cue)),
        }];
        self.observe_changes(&changes);
        self.runtime(changes);
    }

    fn wall_clock_start(&mut self, cue: CueId, local: i64, now: SampleTime) {
        let label = self.cue_label(cue);
        let at = clock_text(local);
        let (armed, broken) = (
            self.doc.model.ws().find_cue(cue).is_some_and(|c| c.armed),
            {
                let engine = self.doc.model.engine();
                engine.broken_reason(cue).is_some() || !engine.problems(cue).is_empty()
            },
        );
        let (level, message) = if broken {
            (
                LogLevel::Warn,
                format!("wall clock {at}: {label} is broken; its action does not run"),
            )
        } else if !armed {
            (
                LogLevel::Warn,
                format!("wall clock {at}: {label} is disarmed; its action does not run"),
            )
        } else {
            (LogLevel::Info, format!("wall clock {at}: {label}"))
        };
        self.record(
            LogKind::Trigger,
            level,
            message,
            Some(cue),
            Some(SOURCE.to_owned()),
        );
        self.transport(TransportAction::Start { cue }, now);
    }

    /// A step in which the local clock did not move with real time, logged only when the
    /// workspace has a wall-clock trigger on (a computer waking from sleep is no news
    /// otherwise).
    fn log_jump(&mut self, jump: &WallClockJump, missed: usize) {
        let any_on =
            missed > 0 || (self.doc.model.ws().cues()).any(|c| c.triggers.wall_clock.enabled);
        if !any_on {
            return;
        }
        let (from, to) = (clock_text(jump.from), clock_text(jump.to));
        let what = if jump.to < jump.from {
            "went back"
        } else {
            "moved on"
        };
        let level = if missed == 0 {
            LogLevel::Info
        } else {
            LogLevel::Warn
        };
        self.record(
            LogKind::Trigger,
            level,
            format!(
                "the computer's clock {what} from {from} to {to} in {} real second(s); {missed} \
                 time-of-day trigger(s) passed over are not started late",
                jump.real
            ),
            None,
            Some(SOURCE.to_owned()),
        );
    }
}

/// `HH:MM:SS` of a local second.
fn clock_text(local: i64) -> String {
    let s = local.rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_table_covers_the_seconds_it_was_worked_out_for_only() {
        let t = OffsetTable::work_out(100, |utc| Some(if utc >= 102 { 3600 } else { 0 }));
        assert_eq!(t.offset_at(99), None);
        assert_eq!(t.offset_at(100), Some(0));
        assert_eq!(t.offset_at(102), Some(3600));
        assert_eq!(t.offset_at(100 + AHEAD_SECS), None);
        assert_eq!(t.offset_at(i64::MIN), None);
        let failing = OffsetTable::work_out(100, |utc| (utc < 101).then_some(60));
        assert_eq!(failing.offset_at(100), Some(60));
        assert_eq!(failing.offset_at(101), None);
        assert_eq!(failing.offset_at(102), None);
    }

    #[test]
    fn clock_text_is_the_time_of_day() {
        assert_eq!(clock_text(0), "00:00:00");
        assert_eq!(clock_text(86_400 + 19 * 3600 + 25 * 60 + 7), "19:25:07");
        assert_eq!(clock_text(-1), "23:59:59");
    }
}
