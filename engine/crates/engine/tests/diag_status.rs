// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The `status` message through the control loop (M4.7; DX-03): cadence, content and the
//! device state, on a manual wall clock and fake counters. Nothing waits for real time.

mod show;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use openlab_core::protocol::{DeviceState, LogKind, ServerMessage, SystemStatus};
use openlab_core::{MachineSettings, PatchMapping};
use openlab_engine::control::offline::OfflineShow;
use openlab_engine::control::{ControlConfig, ManualClock};
use openlab_engine::diag::StatusConfig;
use show::device::{DeviceShow, listed};
use show::*;

const T0: i64 = 1_700_000_000_000;

/// Milliseconds of CPU time the fake probe reports.
static CPU_MS: AtomicU64 = AtomicU64::new(0);

fn fake_cpu() -> Option<f64> {
    Some(CPU_MS.load(Ordering::SeqCst) as f64 / 1000.0)
}

fn statuses(msgs: Vec<ServerMessage>) -> Vec<SystemStatus> {
    msgs.into_iter()
        .filter_map(|m| match m {
            ServerMessage::Status { status } => Some(*status),
            _ => None,
        })
        .collect()
}

fn rig(ws: openlab_core::Workspace) -> (OfflineShow, ManualClock) {
    let clock = ManualClock::new(T0);
    let mut c = config(&ws);
    c.clock = clock.clock();
    c.status = Some(StatusConfig::default());
    c.cpu_probe = fake_cpu;
    let mut s = show_with(ws, c);
    s.take_outbound();
    (s, clock)
}

#[test]
fn status_goes_out_once_a_second_and_never_when_not_asked_for() {
    {
        let ws = workspace(vec![]);
        let clock = ManualClock::new(T0);
        let mut c = config(&ws);
        c.clock = clock.clock();
        // `status` left at its default: off.
        let mut s = show_with(ws, c);
        for k in 0..30 {
            clock.set(T0 + k * 1000);
            s.tick();
        }
        assert!(statuses(s.take_outbound()).is_empty());
    }

    let (mut s, clock) = rig(workspace(vec![]));
    let mut count = 0;
    let mut seen_at = vec![];
    // Ten seconds in 50 ms steps: the first status was sent while the show was built.
    for ms in (50..=10_000).step_by(50) {
        clock.set(T0 + ms);
        s.tick();
        let n = statuses(s.take_outbound()).len();
        if n > 0 {
            seen_at.push(ms);
        }
        count += n;
    }
    assert_eq!(seen_at, (1..=10).map(|k| k * 1000).collect::<Vec<_>>());
    assert_eq!(count, 10);
}

#[test]
fn status_reports_counters_cpu_and_the_disk() {
    let path = wav(&dc(40_000, 0.5));
    let (mut s, clock) = rig(workspace(vec![audio(1, &path, 0)]));
    CPU_MS.store(1000, Ordering::SeqCst);
    // A baseline status, then a second with 400 ms of CPU in the second.
    clock.set(T0 + 1000);
    s.tick();
    statuses(s.take_outbound());
    go(&mut s);
    s.run(500);
    CPU_MS.store(1400, Ordering::SeqCst);
    clock.set(T0 + 2000);
    s.tick();
    let [status] = statuses(s.take_outbound()).try_into().unwrap();
    assert!((status.cpu_percent.unwrap() - 40.0).abs() < 1e-6);
    assert_eq!(status.engine_load, None, "not measured yet");
    assert_eq!(status.xruns, 0);
    assert_eq!(status.disk.streams, 1, "the playing cue's stream");
    assert!(status.disk.healthy);
    assert_eq!(status.disk.underrun_frames, 0);
    assert_eq!(status.log_dropped, 0);
    // The offline driver has no device.
    assert_eq!(status.device.state, DeviceState::Absent);
    assert_eq!(status.device.name, None);
    assert_eq!(status.device.sample_rate, RATE);
    assert_eq!(status.device.channels, 2);
    // Every message survives the wire.
    check_wire(&ServerMessage::Status {
        status: Box::new(status),
    });
}

#[test]
fn an_xrun_sends_a_status_early_but_not_faster_than_the_minimum_gap() {
    let (mut s, clock) = rig(workspace(vec![]));
    s.renderer_mut().inject_xruns(3);
    s.step();
    // Inside the minimum gap after the status at construction: nothing yet.
    clock.set(T0 + 100);
    s.tick();
    assert!(statuses(s.take_outbound()).is_empty());
    clock.set(T0 + 250);
    s.tick();
    let [status] = statuses(s.take_outbound()).try_into().unwrap();
    assert_eq!(status.xruns, 3);
    // Unchanged since: the next waits for the second.
    clock.set(T0 + 500);
    s.tick();
    assert!(statuses(s.take_outbound()).is_empty());
    clock.set(T0 + 1250);
    s.tick();
    assert_eq!(statuses(s.take_outbound()).len(), 1);
}

#[test]
fn status_follows_the_wall_clock_even_when_sample_time_stands_still() {
    // A lost device stops the audio clock; the status must still say so.
    let (mut s, clock) = rig(workspace(vec![]));
    for k in 1..=3 {
        clock.set(T0 + k * 1000);
        s.tick();
    }
    assert_eq!(statuses(s.take_outbound()).len(), 3);
}

const DEV: &str = "fake:1";
const DEV_NAME: &str = "Fake 1";

fn device_show(clock: &ManualClock) -> DeviceShow {
    let ws = workspace(vec![]);
    let mut c = ControlConfig::new(RATE, BLOCK);
    c.seed = 7;
    let mut machine = MachineSettings::default();
    machine
        .set_patch_mapping(
            ws.id,
            patch_id(),
            PatchMapping {
                device: DEV_NAME.into(),
                channels: vec![0, 1],
            },
        )
        .unwrap();
    c.machine = machine;
    c.mixer_patches = 2;
    c.clock = clock.clock();
    c.status = Some(StatusConfig::default());
    DeviceShow::new(ws, vec![listed(DEV, DEV_NAME, 2)], c)
}

#[test]
fn status_names_the_device_and_reports_its_loss_and_the_log_records_it() {
    let clock = ManualClock::new(T0);
    let mut s = device_show(&clock);
    s.take_outbound();
    clock.set(T0 + 1000);
    s.tick();
    let [up] = statuses(s.take_outbound()).try_into().unwrap();
    assert_eq!(up.device.name.as_deref(), Some(DEV_NAME));
    assert_eq!(up.device.state, DeviceState::Ok);
    assert_eq!(up.device.channels, 2);
    assert_eq!(up.device.block_frames, Some(BLOCK as u32));

    s.fake.unplug(DEV, true);
    s.settle_devices();
    assert!(s.control().device_down());
    s.advance(Duration::from_secs(1));
    clock.set(T0 + 1300);
    s.tick();
    let down = statuses(s.take_outbound())
        .pop()
        .expect("a status on the change");
    assert_eq!(down.device.state, DeviceState::Lost);
    assert_eq!(down.device.name.as_deref(), Some(DEV_NAME));

    let log = s.control().show_log().tail(100);
    let lost = log
        .iter()
        .find(|e| e.kind == LogKind::Device)
        .unwrap_or_else(|| panic!("{log:#?}"));
    assert!(lost.message.contains("lost"), "{}", lost.message);
}

/// Calls of [`counting_cpu`].
static CPU_CALLS: AtomicU64 = AtomicU64::new(0);

fn counting_cpu() -> Option<f64> {
    CPU_CALLS.fetch_add(1, Ordering::SeqCst);
    Some(0.0)
}

#[test]
fn the_cpu_is_probed_only_when_a_status_goes_out() {
    let ws = workspace(vec![]);
    let clock = ManualClock::new(T0);
    let mut c = config(&ws);
    c.clock = clock.clock();
    c.status = Some(StatusConfig::default());
    c.cpu_probe = counting_cpu;
    let mut s = show_with(ws, c);
    let mut sent = 0;
    for ms in (50..=10_000).step_by(50) {
        clock.set(T0 + ms);
        s.tick();
        sent += statuses(s.take_outbound()).len() as u64;
    }
    // The first was sent while the show was built, then one a second.
    assert_eq!(sent, 11);
    // One probe per status, never one per tick (200 ticks).
    assert_eq!(CPU_CALLS.load(Ordering::SeqCst), sent);
}

#[test]
fn engine_load_is_the_worst_callback_since_the_previous_status() {
    let clock = ManualClock::new(T0);
    let mut s = device_show(&clock);
    s.take_outbound();
    // Callbacks ran: the status carries a measured load (wall-clock time, so only its range is
    // checked; the max-and-reset logic is unit-tested in openlab-audio).
    s.run(BLOCK * 4);
    s.take_outbound();
    clock.set(T0 + 1000);
    s.tick();
    let [measured] = statuses(s.take_outbound()).try_into().unwrap();
    let load = measured.engine_load.expect("the callbacks were timed");
    assert!(load.is_finite() && load >= 0.0, "{load}");
    // No callback since that status: nothing to report, not a stale figure.
    clock.set(T0 + 2000);
    s.tick();
    let [idle] = statuses(s.take_outbound()).try_into().unwrap();
    assert_eq!(idle.engine_load, None);
    // Offline there is no audio callback at all.
    let (mut offline, clock) = rig(workspace(vec![]));
    offline.run(BLOCK * 4);
    clock.set(T0 + 1000);
    offline.tick();
    let [status] = statuses(offline.take_outbound()).try_into().unwrap();
    assert_eq!(status.engine_load, None);
}
