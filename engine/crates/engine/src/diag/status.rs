// SPDX-License-Identifier: GPL-3.0-or-later

//! When to send a `status` message and what is in it (DX-03). Pure: the control thread gathers
//! the counters and passes them in with the wall-clock time, so tests use fake counters and a
//! manual clock.

use openlab_core::protocol::{DeviceStatus, DiskStatus, SystemStatus};

/// A disk is healthy when no underrun happened in this long.
pub const DISK_RECOVERY_MS: i64 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusConfig {
    /// A status is sent at least this often.
    pub interval_ms: i64,
    /// A change in xruns, underruns, the disk's health or the device sends one early, but not
    /// sooner than this after the previous.
    pub min_gap_ms: i64,
}

impl Default for StatusConfig {
    fn default() -> Self {
        Self {
            interval_ms: 1000,
            min_gap_ms: 250,
        }
    }
}

/// What the control thread reads for one status.
#[derive(Clone, Debug, PartialEq)]
pub struct Counters {
    pub xruns: u64,
    pub underrun_frames: u64,
    pub streams: u32,
    pub log_dropped: u64,
    pub device: DeviceStatus,
    /// Process CPU seconds so far, if the platform says.
    pub cpu_seconds: Option<f64>,
    /// Worst audio callback load since the previous status, if the audio thread measures it.
    pub engine_load: Option<f64>,
}

/// What a change in must send a status early.
#[derive(Clone, Debug, PartialEq)]
struct Discrete {
    xruns: u64,
    underrun_frames: u64,
    healthy: bool,
    device: (Option<String>, openlab_core::protocol::DeviceState),
}

#[derive(Debug)]
pub struct StatusReporter {
    config: StatusConfig,
    last_sent_ms: Option<i64>,
    last_discrete: Option<Discrete>,
    /// CPU seconds and time at the previous status.
    last_cpu: Option<(i64, f64)>,
    last_underrun_frames: u64,
    last_underrun_ms: Option<i64>,
}

impl StatusReporter {
    pub fn new(config: StatusConfig) -> Self {
        Self {
            config,
            last_sent_ms: None,
            last_discrete: None,
            last_cpu: None,
            last_underrun_frames: 0,
            last_underrun_ms: None,
        }
    }

    /// The status to send now, if one is due. Cheap when none is: the caller gathers
    /// `counters` first only because they are atomics and a few field reads.
    pub fn poll(&mut self, now_ms: i64, counters: &Counters) -> Option<SystemStatus> {
        if counters.underrun_frames > self.last_underrun_frames {
            self.last_underrun_ms = Some(now_ms);
        }
        self.last_underrun_frames = counters.underrun_frames;
        let healthy = self
            .last_underrun_ms
            .is_none_or(|at| now_ms.saturating_sub(at) >= DISK_RECOVERY_MS);
        let discrete = Discrete {
            xruns: counters.xruns,
            underrun_frames: counters.underrun_frames,
            healthy,
            device: (counters.device.name.clone(), counters.device.state),
        };
        let since = self.last_sent_ms.map(|at| now_ms.saturating_sub(at));
        let due = match since {
            None => true,
            // A clock that went back must not silence the status: treat it as due.
            Some(s) if s < 0 || s >= self.config.interval_ms => true,
            Some(s) => {
                s >= self.config.min_gap_ms && self.last_discrete.as_ref() != Some(&discrete)
            }
        };
        if !due {
            return None;
        }
        let cpu_percent = match (self.last_cpu, counters.cpu_seconds) {
            (Some((then_ms, then_s)), Some(now_s)) if now_ms > then_ms && now_s >= then_s => {
                Some((now_s - then_s) / ((now_ms - then_ms) as f64 / 1000.0) * 100.0)
            }
            _ => None,
        };
        self.last_cpu = counters.cpu_seconds.map(|s| (now_ms, s));
        self.last_sent_ms = Some(now_ms);
        self.last_discrete = Some(discrete);
        Some(SystemStatus {
            cpu_percent,
            engine_load: counters.engine_load,
            xruns: counters.xruns,
            disk: DiskStatus {
                streams: counters.streams,
                underrun_frames: counters.underrun_frames,
                healthy,
            },
            device: counters.device.clone(),
            log_dropped: counters.log_dropped,
        })
    }
}

#[cfg(test)]
mod tests {
    use openlab_core::protocol::DeviceState;

    use super::*;

    fn counters() -> Counters {
        Counters {
            xruns: 0,
            underrun_frames: 0,
            streams: 2,
            log_dropped: 0,
            device: DeviceStatus {
                name: Some("Dev".into()),
                state: DeviceState::Ok,
                sample_rate: 48_000,
                block_frames: Some(256),
                channels: 2,
                latency_ms: None,
            },
            cpu_seconds: Some(1.0),
            engine_load: None,
        }
    }

    #[test]
    fn one_status_per_interval_and_the_first_at_once() {
        let mut r = StatusReporter::new(StatusConfig::default());
        let c = counters();
        assert!(r.poll(10_000, &c).is_some());
        for t in (10_005..11_000).step_by(5) {
            assert!(r.poll(t, &c).is_none(), "{t}");
        }
        assert!(r.poll(11_000, &c).is_some());
        assert!(r.poll(11_999, &c).is_none());
        assert!(r.poll(12_000, &c).is_some());
    }

    #[test]
    fn cpu_is_the_share_of_one_core_since_the_previous_status() {
        let mut r = StatusReporter::new(StatusConfig::default());
        let mut c = counters();
        assert_eq!(r.poll(0, &c).unwrap().cpu_percent, None, "no baseline yet");
        c.cpu_seconds = Some(1.25);
        let s = r.poll(1000, &c).unwrap();
        assert!((s.cpu_percent.unwrap() - 25.0).abs() < 1e-9);
        // Two cores busy for the second.
        c.cpu_seconds = Some(3.25);
        let s = r.poll(2000, &c).unwrap();
        assert!((s.cpu_percent.unwrap() - 200.0).abs() < 1e-9);
        // A platform without a figure, and a counter that went back: none, not a bad number.
        c.cpu_seconds = None;
        assert_eq!(r.poll(3000, &c).unwrap().cpu_percent, None);
        c.cpu_seconds = Some(5.0);
        assert_eq!(r.poll(4000, &c).unwrap().cpu_percent, None, "no baseline");
        c.cpu_seconds = Some(4.0);
        assert_eq!(r.poll(5000, &c).unwrap().cpu_percent, None, "went back");
    }

    #[test]
    fn a_change_sends_early_but_not_faster_than_the_minimum_gap() {
        let mut r = StatusReporter::new(StatusConfig::default());
        let mut c = counters();
        assert!(r.poll(0, &c).is_some());
        c.xruns = 1;
        assert!(r.poll(100, &c).is_none(), "inside the gap");
        let s = r.poll(250, &c).expect("changed, gap over");
        assert_eq!(s.xruns, 1);
        // Nothing changed since: wait for the interval.
        assert!(r.poll(600, &c).is_none());
        c.device.state = DeviceState::Lost;
        let s = r.poll(600 + 250, &c).expect("device state changed");
        assert_eq!(s.device.state, DeviceState::Lost);
    }

    #[test]
    fn disk_health_returns_ten_seconds_after_the_last_underrun() {
        let mut r = StatusReporter::new(StatusConfig::default());
        let mut c = counters();
        assert!(r.poll(0, &c).unwrap().disk.healthy);
        c.underrun_frames = 480;
        let s = r.poll(1000, &c).unwrap();
        assert!(!s.disk.healthy);
        assert_eq!(s.disk.underrun_frames, 480);
        assert!(!r.poll(10_999, &c).unwrap().disk.healthy);
        let s = r.poll(11_999, &c).unwrap();
        assert!(s.disk.healthy, "ten quiet seconds");
        assert_eq!(s.disk.streams, 2);
    }

    #[test]
    fn a_clock_that_goes_back_does_not_silence_the_status() {
        let mut r = StatusReporter::new(StatusConfig::default());
        let c = counters();
        assert!(r.poll(50_000, &c).is_some());
        assert!(r.poll(10_000, &c).is_some());
        assert!(r.poll(10_100, &c).is_none());
    }

    #[test]
    fn engine_load_and_log_drops_pass_through() {
        let mut r = StatusReporter::new(StatusConfig::default());
        let mut c = counters();
        c.engine_load = Some(0.4);
        c.log_dropped = 9;
        let s = r.poll(0, &c).unwrap();
        assert_eq!((s.engine_load, s.log_dropped), (Some(0.4), 9));
    }
}
