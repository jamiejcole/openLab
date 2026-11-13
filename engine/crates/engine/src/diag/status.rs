// SPDX-License-Identifier: GPL-3.0-or-later

//! When to send a `status` message and what is in it (DX-03). Pure: the control thread gathers
//! the counters and passes them in with the wall-clock time, so tests use fake counters and a
//! manual clock.

use openlab_core::protocol::{
    DeviceState, DeviceStatus, DiskStatus, OutputState, OutputStatus, SystemStatus,
};

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
    /// Sequence number of the newest show-log record.
    pub log_seq: u64,
    pub network_output_disabled: bool,
    pub device: DeviceStatus,
    /// Every open output device, the primary first.
    pub outputs: Vec<OutputStatus>,
    /// Process CPU seconds so far, if the platform says.
    pub cpu_seconds: Option<f64>,
    /// Worst audio callback load since the previous status, if the audio thread measures it.
    pub engine_load: Option<f64>,
}

/// The few values that decide whether a status is due, read without copying anything: the
/// control thread builds this every tick, and the full [`Counters`] only when one is due.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peek<'a> {
    pub xruns: u64,
    pub underrun_frames: u64,
    pub device_name: Option<&'a str>,
    pub device_state: DeviceState,
    /// The open output devices and their states, packed so a change is seen without a copy
    /// ([`outputs_fingerprint`]).
    pub outputs: u64,
    pub network_output_disabled: bool,
}

/// The number of outputs and each one's state in a few bits each: two fingerprints differ when
/// an output opened or one's state changed. Outputs past the bits available are not told
/// apart (the periodic status still reports them).
pub fn outputs_fingerprint(states: impl Iterator<Item = OutputState>) -> u64 {
    let mut packed = 0u64;
    let mut count = 0u64;
    for (k, state) in states.enumerate() {
        count += 1;
        let code = match state {
            OutputState::Up => 0u64,
            OutputState::Opening => 1,
            OutputState::Lost => 2,
            OutputState::Reconfiguring => 3,
            OutputState::Closed => 4,
            OutputState::Failed => 5,
        };
        if let Some(shifted) = u32::try_from(k * 3)
            .ok()
            .filter(|&b| b < 48)
            .and_then(|b| code.checked_shl(b))
        {
            packed |= shifted;
        }
    }
    packed | (count.min(0xFF) << 48)
}

impl<'a> Peek<'a> {
    pub fn of(c: &'a Counters) -> Self {
        Self {
            xruns: c.xruns,
            underrun_frames: c.underrun_frames,
            device_name: c.device.name.as_deref(),
            device_state: c.device.state,
            outputs: outputs_fingerprint(c.outputs.iter().map(|o| o.state)),
            network_output_disabled: c.network_output_disabled,
        }
    }
}

/// What a change in must send a status early.
#[derive(Clone, Debug, PartialEq)]
struct Discrete {
    xruns: u64,
    underrun_frames: u64,
    healthy: bool,
    device: (Option<String>, DeviceState),
    outputs: u64,
    network_output_disabled: bool,
}

impl Discrete {
    fn of(p: &Peek<'_>, healthy: bool) -> Self {
        Self {
            xruns: p.xruns,
            underrun_frames: p.underrun_frames,
            healthy,
            device: (p.device_name.map(str::to_owned), p.device_state),
            outputs: p.outputs,
            network_output_disabled: p.network_output_disabled,
        }
    }

    /// `of(p, healthy) == self`, without the copy of the name.
    fn is(&self, p: &Peek<'_>, healthy: bool) -> bool {
        self.xruns == p.xruns
            && self.underrun_frames == p.underrun_frames
            && self.healthy == healthy
            && self.device.0.as_deref() == p.device_name
            && self.device.1 == p.device_state
            && self.outputs == p.outputs
            && self.network_output_disabled == p.network_output_disabled
    }
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

    /// The status to send now, if one is due. `peek` decides; `gather` reads the rest (CPU
    /// time, the device's details) and runs only when a status is going out.
    pub fn poll(
        &mut self,
        now_ms: i64,
        peek: &Peek<'_>,
        gather: impl FnOnce() -> Counters,
    ) -> Option<SystemStatus> {
        if peek.underrun_frames > self.last_underrun_frames {
            self.last_underrun_ms = Some(now_ms);
        }
        self.last_underrun_frames = peek.underrun_frames;
        let healthy = self
            .last_underrun_ms
            .is_none_or(|at| now_ms.saturating_sub(at) >= DISK_RECOVERY_MS);
        let since = self.last_sent_ms.map(|at| now_ms.saturating_sub(at));
        let due = match since {
            None => true,
            // A clock that went back must not silence the status: treat it as due.
            Some(s) if s < 0 || s >= self.config.interval_ms => true,
            Some(s) => {
                s >= self.config.min_gap_ms
                    && !self
                        .last_discrete
                        .as_ref()
                        .is_some_and(|d| d.is(peek, healthy))
            }
        };
        if !due {
            return None;
        }
        let counters = gather();
        let cpu_percent = match (self.last_cpu, counters.cpu_seconds) {
            (Some((then_ms, then_s)), Some(now_s)) if now_ms > then_ms && now_s >= then_s => {
                Some((now_s - then_s) / ((now_ms - then_ms) as f64 / 1000.0) * 100.0)
            }
            _ => None,
        };
        self.last_cpu = counters.cpu_seconds.map(|s| (now_ms, s));
        self.last_sent_ms = Some(now_ms);
        self.last_discrete = Some(Discrete::of(peek, healthy));
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
            outputs: counters.outputs.clone(),
            log_dropped: counters.log_dropped,
            log_seq: counters.log_seq,
            network_output_disabled: counters.network_output_disabled,
        })
    }
}

#[cfg(test)]
mod tests {
    use openlab_core::protocol::DeviceState;

    use super::*;

    /// `poll` with the counters given whole, counting how often they were gathered.
    fn poll(r: &mut StatusReporter, t: i64, c: &Counters) -> Option<SystemStatus> {
        r.poll(t, &Peek::of(c), || c.clone())
    }

    fn counters() -> Counters {
        Counters {
            xruns: 0,
            underrun_frames: 0,
            streams: 2,
            log_dropped: 0,
            log_seq: 0,
            network_output_disabled: false,
            device: DeviceStatus {
                name: Some("Dev".into()),
                state: DeviceState::Ok,
                sample_rate: 48_000,
                block_frames: Some(256),
                channels: 2,
                latency_ms: None,
            },
            outputs: vec![OutputStatus {
                name: "Dev".into(),
                state: OutputState::Up,
                primary: true,
                sample_rate: 48_000,
                channels: 2,
                block_frames: Some(256),
                latency_ms: None,
            }],
            cpu_seconds: Some(1.0),
            engine_load: None,
        }
    }

    #[test]
    fn one_status_per_interval_and_the_first_at_once() {
        let mut r = StatusReporter::new(StatusConfig::default());
        let c = counters();
        assert!(poll(&mut r, 10_000, &c).is_some());
        for t in (10_005..11_000).step_by(5) {
            assert!(poll(&mut r, t, &c).is_none(), "{t}");
        }
        assert!(poll(&mut r, 11_000, &c).is_some());
        assert!(poll(&mut r, 11_999, &c).is_none());
        assert!(poll(&mut r, 12_000, &c).is_some());
    }

    #[test]
    fn cpu_is_the_share_of_one_core_since_the_previous_status() {
        let mut r = StatusReporter::new(StatusConfig::default());
        let mut c = counters();
        assert_eq!(
            poll(&mut r, 0, &c).unwrap().cpu_percent,
            None,
            "no baseline yet"
        );
        c.cpu_seconds = Some(1.25);
        let s = poll(&mut r, 1000, &c).unwrap();
        assert!((s.cpu_percent.unwrap() - 25.0).abs() < 1e-9);
        // Two cores busy for the second.
        c.cpu_seconds = Some(3.25);
        let s = poll(&mut r, 2000, &c).unwrap();
        assert!((s.cpu_percent.unwrap() - 200.0).abs() < 1e-9);
        // A platform without a figure, and a counter that went back: none, not a bad number.
        c.cpu_seconds = None;
        assert_eq!(poll(&mut r, 3000, &c).unwrap().cpu_percent, None);
        c.cpu_seconds = Some(5.0);
        assert_eq!(
            poll(&mut r, 4000, &c).unwrap().cpu_percent,
            None,
            "no baseline"
        );
        c.cpu_seconds = Some(4.0);
        assert_eq!(
            poll(&mut r, 5000, &c).unwrap().cpu_percent,
            None,
            "went back"
        );
    }

    #[test]
    fn the_network_override_sends_early_and_is_reported() {
        let mut r = StatusReporter::new(StatusConfig::default());
        let mut c = counters();
        assert!(!poll(&mut r, 0, &c).unwrap().network_output_disabled);
        c.network_output_disabled = true;
        assert!(poll(&mut r, 100, &c).is_none(), "inside the gap");
        let s = poll(&mut r, 250, &c).expect("changed, gap over");
        assert!(s.network_output_disabled);
        assert!(poll(&mut r, 600, &c).is_none(), "nothing new since");
        c.network_output_disabled = false;
        assert!(!poll(&mut r, 850, &c).unwrap().network_output_disabled);
    }

    /// Any output device opening or changing state sends a status early; the same states do
    /// not.
    #[test]
    fn an_output_device_opening_or_changing_state_sends_early() {
        let mut r = StatusReporter::new(StatusConfig::default());
        let mut c = counters();
        assert!(poll(&mut r, 0, &c).is_some());
        let mut second = c.outputs[0].clone();
        second.name = "Other".into();
        second.primary = false;
        second.state = OutputState::Opening;
        c.outputs.push(second);
        assert!(poll(&mut r, 100, &c).is_none(), "inside the gap");
        assert_eq!(poll(&mut r, 250, &c).expect("opened").outputs.len(), 2);
        assert!(poll(&mut r, 600, &c).is_none(), "nothing new since");
        for (t, state) in [
            (850, OutputState::Up),
            (1100, OutputState::Reconfiguring),
            (1350, OutputState::Lost),
            (1600, OutputState::Closed),
            (1850, OutputState::Failed),
        ] {
            c.outputs[1].state = state;
            let s = poll(&mut r, t, &c).expect("state changed");
            assert_eq!(s.outputs[1].state, state);
        }
        // Every state of every output it can hold is told apart.
        let all = [
            OutputState::Up,
            OutputState::Opening,
            OutputState::Lost,
            OutputState::Reconfiguring,
            OutputState::Closed,
            OutputState::Failed,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for k in 0..16 {
            for &state in &all {
                let states = (0..16).map(|i| if i == k { state } else { OutputState::Up });
                seen.insert(outputs_fingerprint(states));
            }
        }
        assert_eq!(seen.len(), 16 * 5 + 1);
        assert_ne!(
            outputs_fingerprint([OutputState::Up].into_iter()),
            outputs_fingerprint([OutputState::Up, OutputState::Up].into_iter())
        );
    }

    #[test]
    fn a_change_sends_early_but_not_faster_than_the_minimum_gap() {
        let mut r = StatusReporter::new(StatusConfig::default());
        let mut c = counters();
        assert!(poll(&mut r, 0, &c).is_some());
        c.xruns = 1;
        assert!(poll(&mut r, 100, &c).is_none(), "inside the gap");
        let s = poll(&mut r, 250, &c).expect("changed, gap over");
        assert_eq!(s.xruns, 1);
        // Nothing changed since: wait for the interval.
        assert!(poll(&mut r, 600, &c).is_none());
        c.device.state = DeviceState::Lost;
        let s = poll(&mut r, 600 + 250, &c).expect("device state changed");
        assert_eq!(s.device.state, DeviceState::Lost);
    }

    #[test]
    fn disk_health_returns_ten_seconds_after_the_last_underrun() {
        let mut r = StatusReporter::new(StatusConfig::default());
        let mut c = counters();
        assert!(poll(&mut r, 0, &c).unwrap().disk.healthy);
        c.underrun_frames = 480;
        let s = poll(&mut r, 1000, &c).unwrap();
        assert!(!s.disk.healthy);
        assert_eq!(s.disk.underrun_frames, 480);
        assert!(!poll(&mut r, 10_999, &c).unwrap().disk.healthy);
        let s = poll(&mut r, 11_999, &c).unwrap();
        assert!(s.disk.healthy, "ten quiet seconds");
        assert_eq!(s.disk.streams, 2);
    }

    #[test]
    fn a_clock_that_goes_back_does_not_silence_the_status() {
        let mut r = StatusReporter::new(StatusConfig::default());
        let c = counters();
        assert!(poll(&mut r, 50_000, &c).is_some());
        assert!(poll(&mut r, 10_000, &c).is_some());
        assert!(poll(&mut r, 10_100, &c).is_none());
    }

    #[test]
    fn engine_load_and_log_drops_pass_through() {
        let mut r = StatusReporter::new(StatusConfig::default());
        let mut c = counters();
        c.engine_load = Some(0.4);
        c.log_dropped = 9;
        let s = poll(&mut r, 0, &c).unwrap();
        assert_eq!((s.engine_load, s.log_dropped), (Some(0.4), 9));
    }

    #[test]
    fn the_counters_are_gathered_only_when_a_status_is_due() {
        let mut r = StatusReporter::new(StatusConfig::default());
        let mut c = counters();
        let gathered = std::cell::Cell::new(0);
        let ask = |r: &mut StatusReporter, t: i64, c: &Counters| {
            r.poll(t, &Peek::of(c), || {
                gathered.set(gathered.get() + 1);
                c.clone()
            })
        };
        assert!(ask(&mut r, 0, &c).is_some());
        for t in (5..1000).step_by(5) {
            assert!(ask(&mut r, t, &c).is_none());
        }
        assert_eq!(gathered.get(), 1, "nothing was gathered on the quiet ticks");
        // A change inside the minimum gap is not due either, so still nothing gathered.
        c.xruns = 1;
        assert!(ask(&mut r, 100, &c).is_none());
        assert_eq!(gathered.get(), 1);
        assert!(ask(&mut r, 1000, &c).is_some());
        assert_eq!(gathered.get(), 2);
    }
}
