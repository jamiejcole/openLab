// SPDX-License-Identifier: GPL-3.0-or-later

//! What the operator is told about the sound going wrong while it plays (NFR-27, DX-03): an
//! output device dropping out, the disk falling behind, the audio callback running close to its
//! deadline, and a cue starved by the disk. Pure: the control thread reads the counters every
//! tick and passes them in with the wall-clock time in milliseconds; tests use a manual clock.
//!
//! A problem is told at once, then at most once per [`REPORT_EVERY_MS`] with how often it
//! happened since, so a device dropping out hundreds of times a second writes one log line and
//! one warning update every ten seconds, not hundreds.

/// A disk is healthy when no underrun happened in this long.
pub const DISK_RECOVERY_MS: i64 = 10_000;

/// A device's dropout warning stays until it has played this long without one.
pub const DROPOUT_CLEAR_MS: i64 = 10_000;

/// The least time between two reports of the same problem (as the stall watchdog's log).
pub const REPORT_EVERY_MS: i64 = 10_000;

/// Engine load at or above this fraction of the block time is high: the callback is close to
/// missing its deadline, and above 1.0 it did.
pub const HIGH_LOAD: f64 = 0.8;

/// Engine load is judged per window of this long (its worst callback in the window).
pub const LOAD_WINDOW_MS: i64 = 1_000;

/// Whether `gap` ms have passed from `then` to `now`. A clock that went back counts as passed,
/// so a problem is never held back for ever.
fn passed(now: i64, then: i64, gap: i64) -> bool {
    let d = now.saturating_sub(then);
    d < 0 || d >= gap
}

/// Occurrences of one problem: the first is told at once, later ones at most once per
/// [`REPORT_EVERY_MS`], each report with the number since the previous one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Throttle {
    last_told_ms: Option<i64>,
    pending: u64,
}

impl Throttle {
    /// `n` more occurrences at `now`: the number to tell now, if a report is due.
    pub fn hit(&mut self, now: i64, n: u64) -> Option<u64> {
        self.pending = self.pending.saturating_add(n);
        self.due(now)
    }

    /// Occurrences held back whose report is due by now.
    pub fn due(&mut self, now: i64) -> Option<u64> {
        if self.pending == 0 {
            return None;
        }
        if self
            .last_told_ms
            .is_some_and(|at| !passed(now, at, REPORT_EVERY_MS))
        {
            return None;
        }
        self.last_told_ms = Some(now);
        Some(std::mem::take(&mut self.pending))
    }

    /// Nothing is held back and the last report is old enough that the next is told at once:
    /// the throttle can be forgotten.
    pub fn idle(&self, now: i64) -> bool {
        self.pending == 0
            && self
                .last_told_ms
                .is_none_or(|at| passed(now, at, REPORT_EVERY_MS))
    }
}

/// A change in one device's dropouts to tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropoutChange {
    /// Dropouts began: `count` so far.
    Raised { count: u64 },
    /// `more` dropouts since the last report, `count` since they began.
    More { more: u64, count: u64 },
    /// None for [`DROPOUT_CLEAR_MS`]: `count` in all while it lasted.
    Cleared { count: u64 },
}

/// One output device's dropouts (xruns): raised on the first, counted while they go on,
/// cleared after [`DROPOUT_CLEAR_MS`] without one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dropouts {
    /// The device's xrun counter at the last look.
    seen: u64,
    /// While raised: (dropouts told so far, when the last one was seen).
    raised: Option<(u64, i64)>,
    throttle: Throttle,
}

impl Dropouts {
    /// Starts from the counter as it is: dropouts before the watch began are not told.
    pub fn new(xruns: u64) -> Self {
        Self {
            seen: xruns,
            ..Self::default()
        }
    }

    /// The dropouts told so far while raised.
    pub fn count(&self) -> Option<u64> {
        self.raised.map(|(count, _)| count)
    }

    /// The device's xrun counter (monotonic; a lower value is a new counter and starts again)
    /// at `now`.
    pub fn update(&mut self, now: i64, xruns: u64) -> Option<DropoutChange> {
        let new = xruns.saturating_sub(self.seen);
        self.seen = xruns;
        if new > 0 {
            match &mut self.raised {
                None => {
                    self.throttle = Throttle::default();
                    let count = self.throttle.hit(now, new).unwrap_or(new);
                    self.raised = Some((count, now));
                    return Some(DropoutChange::Raised { count });
                }
                Some((_, last)) => *last = now,
            }
            self.throttle.hit(now, new);
        }
        let (count, last) = self.raised.as_mut()?;
        if passed(now, *last, DROPOUT_CLEAR_MS) {
            let all = count.saturating_add(std::mem::take(&mut self.throttle.pending));
            self.raised = None;
            return Some(DropoutChange::Cleared { count: all });
        }
        let more = self.throttle.due(now)?;
        *count = count.saturating_add(more);
        Some(DropoutChange::More {
            more,
            count: *count,
        })
    }
}

/// The disk's health from the voices' underruns: unhealthy from an underrun until
/// [`DISK_RECOVERY_MS`] without one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiskWatch {
    seen_frames: u64,
    last_underrun_ms: Option<i64>,
}

impl DiskWatch {
    /// Starts from the counter as it is.
    pub fn new(underrun_frames: u64) -> Self {
        Self {
            seen_frames: underrun_frames,
            last_underrun_ms: None,
        }
    }

    /// The voices' underrun frames so far at `now`; whether the disk keeps up.
    pub fn update(&mut self, now: i64, underrun_frames: u64) -> bool {
        if underrun_frames > self.seen_frames {
            self.last_underrun_ms = Some(now);
        }
        self.seen_frames = underrun_frames;
        self.healthy(now)
    }

    /// Whether the disk keeps up at `now`, from what was last seen.
    pub fn healthy(&self, now: i64) -> bool {
        self.last_underrun_ms
            .is_none_or(|at| passed(now, at, DISK_RECOVERY_MS))
    }
}

/// The audio callback's load, judged per [`LOAD_WINDOW_MS`]: a window whose worst callback
/// reached [`HIGH_LOAD`] after one that did not is a crossing, told through a [`Throttle`].
/// Also keeps the worst load since the previous `status` for the status reporter.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoadWatch {
    window_start_ms: Option<i64>,
    window_peak: Option<f64>,
    high: bool,
    throttle: Throttle,
    /// The worst high window's load since the last report.
    untold_peak: Option<f64>,
    /// The worst load since the last [`Self::take_status_peak`].
    status_peak: Option<f64>,
}

/// A report of high engine load: `crossings` times the load rose to `peak` or more since the
/// previous report.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HighLoad {
    pub crossings: u64,
    pub peak: f64,
}

impl LoadWatch {
    /// The worst load measured since the previous call (`None`: no callback ran), at `now`.
    pub fn update(&mut self, now: i64, load: Option<f64>) -> Option<HighLoad> {
        let load = load.filter(|l| l.is_finite() && *l >= 0.0);
        if let Some(l) = load {
            self.status_peak = Some(self.status_peak.map_or(l, |p| p.max(l)));
            self.window_peak = Some(self.window_peak.map_or(l, |p| p.max(l)));
        }
        let start = *self.window_start_ms.get_or_insert(now);
        if !passed(now, start, LOAD_WINDOW_MS) {
            return None;
        }
        self.window_start_ms = Some(now);
        let peak = self.window_peak.take();
        let high = peak.filter(|&p| p >= HIGH_LOAD);
        if let Some(p) = high {
            self.untold_peak = Some(self.untold_peak.map_or(p, |u| u.max(p)));
        }
        let crossed = high.is_some() && !self.high;
        self.high = high.is_some();
        let told = if crossed {
            self.throttle.hit(now, 1)
        } else {
            self.throttle.due(now)
        }?;
        Some(HighLoad {
            crossings: told,
            peak: self.untold_peak.take().unwrap_or(HIGH_LOAD),
        })
    }

    /// The worst load since the previous call, for the `status` message.
    pub fn take_status_peak(&mut self) -> Option<f64> {
        self.status_peak.take()
    }

    /// The worst load since the previous [`Self::take_status_peak`], left in place.
    pub fn status_peak(&self) -> Option<f64> {
        self.status_peak
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_throttle_tells_the_first_at_once_and_then_at_most_every_ten_seconds() {
        let mut t = Throttle::default();
        assert_eq!(t.hit(0, 1), Some(1));
        assert_eq!(t.hit(100, 3), None);
        assert_eq!(t.due(9_999), None);
        assert!(!t.idle(9_999));
        assert_eq!(t.due(10_000), Some(3));
        assert_eq!(t.due(30_000), None, "nothing held back");
        assert!(t.idle(20_000));
        assert_eq!(t.hit(30_000, 2), Some(2), "quiet long enough: at once");
        // A clock that went back does not hold a report back for ever.
        assert_eq!(t.hit(1_000, 1), Some(1));
    }

    #[test]
    fn dropouts_raise_once_count_while_they_go_on_and_clear_after_ten_quiet_seconds() {
        let mut d = Dropouts::new(5);
        assert_eq!(d.update(0, 5), None, "earlier dropouts are not told");
        assert_eq!(d.update(10, 7), Some(DropoutChange::Raised { count: 2 }));
        assert_eq!(d.count(), Some(2));
        // More within ten seconds: held, still one warning.
        for t in (20..5_000).step_by(100) {
            assert_eq!(d.update(t, 7 + (t as u64) / 1000), None, "{t}");
        }
        assert_eq!(d.update(9_000, 20), None);
        assert_eq!(
            d.update(10_010, 20),
            Some(DropoutChange::More {
                more: 13,
                count: 15
            })
        );
        assert_eq!(d.update(18_000, 20), None, "quiet, not yet cleared");
        assert_eq!(
            d.update(19_000, 20),
            Some(DropoutChange::Cleared { count: 15 })
        );
        assert_eq!(d.count(), None);
        // Later: raised again, counted afresh.
        assert_eq!(
            d.update(40_000, 21),
            Some(DropoutChange::Raised { count: 1 })
        );
    }

    #[test]
    fn dropouts_held_back_when_they_clear_are_counted_in_the_total() {
        let mut d = Dropouts::new(0);
        assert_eq!(d.update(0, 1), Some(DropoutChange::Raised { count: 1 }));
        assert_eq!(d.update(1_000, 4), None);
        // The ten quiet seconds end before the next report is due.
        assert_eq!(
            d.update(11_000, 4),
            Some(DropoutChange::Cleared { count: 4 })
        );
    }

    #[test]
    fn a_counter_that_starts_again_is_followed_from_where_it_is() {
        let mut d = Dropouts::new(100);
        assert_eq!(d.update(0, 3), None, "a new counter, nothing new on it");
        assert_eq!(d.update(10, 4), Some(DropoutChange::Raised { count: 1 }));
    }

    #[test]
    fn disk_health_returns_ten_seconds_after_the_last_underrun() {
        let mut w = DiskWatch::new(0);
        assert!(w.update(0, 0));
        assert!(!w.update(1_000, 480));
        assert!(!w.update(10_999, 480));
        assert!(w.update(11_000, 480), "ten quiet seconds");
        assert!(!w.update(12_000, 481));
    }

    #[test]
    fn high_load_is_told_when_it_crosses_the_threshold_and_at_most_every_ten_seconds() {
        let mut w = LoadWatch::default();
        assert_eq!(w.update(0, Some(0.5)), None, "the first window starts");
        assert_eq!(w.update(500, Some(0.9)), None, "judged at the window's end");
        let told = w.update(1_000, None).expect("crossed");
        assert_eq!(told.crossings, 1);
        assert!((told.peak - 0.9).abs() < 1e-9);
        // Staying high is not a new crossing.
        assert_eq!(w.update(1_500, Some(0.95)), None);
        assert_eq!(w.update(2_000, None), None);
        // Down, then up again within ten seconds: held back, counted.
        assert_eq!(w.update(3_000, Some(0.2)), None);
        assert_eq!(w.update(3_500, Some(1.2)), None);
        assert_eq!(w.update(4_000, None), None);
        assert_eq!(w.update(5_000, Some(0.1)), None);
        let later = w.update(11_000, None).expect("the held crossing is told");
        assert_eq!(later.crossings, 1);
        assert!(
            (later.peak - 1.2).abs() < 1e-9,
            "the worst since the last report"
        );
        // A load below the threshold never is.
        let mut calm = LoadWatch::default();
        for t in (0..60_000).step_by(5) {
            assert_eq!(calm.update(t, Some(0.79)), None);
        }
        // Nonsense from the meter is ignored.
        let mut odd = LoadWatch::default();
        odd.update(0, Some(f64::NAN));
        assert_eq!(odd.update(1_000, Some(f64::INFINITY)), None);
        assert_eq!(odd.take_status_peak(), None);
    }

    #[test]
    fn the_status_peak_is_the_worst_since_it_was_last_taken() {
        let mut w = LoadWatch::default();
        w.update(0, Some(0.3));
        w.update(5, Some(0.6));
        w.update(10, None);
        assert_eq!(w.status_peak(), Some(0.6));
        assert_eq!(w.take_status_peak(), Some(0.6));
        assert_eq!(w.take_status_peak(), None);
    }
}
