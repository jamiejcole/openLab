// SPDX-License-Identifier: GPL-3.0-or-later

//! The control loop and its output device (NFR-08, AUD-01, AUD-03, WS-07): the
//! device list, hot-plug, loss and reopen, the device-loss policy (cues on a lost device stop
//! at once and do not come back, as in QLab 5), output delays and dropped outputs. A stand-in
//! device layer (`show::device`) replaces cpal; a test-driven wall clock replaces time.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use openlab_core::protocol::{
    ClientMessage, DeviceEntry, ErrorCode, LogLevel, Outcome, OutputState, OutputStatus,
    PatchMappingEntry, Request, ResultData, RuntimeChange, ServerMessage, TransportAction,
    WireBrokenReason, WireCueState, WireProblem, WireResolution, WireWarning,
};
use openlab_core::{
    ContinueMode, CueKind, CueState, GroupMode, MachineSettings, PatchMapping, Workspace,
};
use openlab_engine::control::{ControlConfig, MAX_WAIT, ManualClock};
use openlab_engine::diag::StatusConfig;
use std::sync::Arc;

use crate::show::device::{DeviceShow, Fake, FakeLayer, listed, listed_at};
use crate::show::*;
use openlab_engine::control::devices::{self, DEVICE_JOB_TIMEOUT, STABLE_UP};

const DEV: &str = "fake:1";
const DEV_NAME: &str = "Fake 1";

fn device_config(ws: &Workspace) -> ControlConfig {
    let mut c = ControlConfig::new(RATE, BLOCK);
    c.seed = 7;
    c.watch.mode = openlab_engine::control::watch::WatchMode::Manual;
    let mut machine = MachineSettings::default();
    machine
        .set_patch_mapping(
            ws.id,
            patch_id(),
            PatchMapping {
                device: DEV_NAME.into(),
                channels: vec![0, 1],
                system: false,
            },
        )
        .unwrap();
    c.machine = machine;
    c.mixer_patches = 2;
    c
}

fn device_show(ws: Workspace) -> DeviceShow {
    let c = device_config(&ws);
    DeviceShow::new(ws, vec![listed(DEV, DEV_NAME, 2)], c)
}

fn go(s: &mut DeviceShow) -> u64 {
    let at = s.now();
    s.transport(TransportAction::Go { list: list_id() });
    at
}

fn start(s: &mut DeviceShow, n: u128) -> u64 {
    let at = s.now();
    s.transport(TransportAction::Start { cue: id(n) });
    at
}

fn list_devices(s: &mut DeviceShow) -> Vec<DeviceEntry> {
    let id = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id,
        request: Request::ListDevices,
    });
    let [ServerMessage::Result { outcome, .. }] = replies.as_slice() else {
        panic!("{replies:?}");
    };
    match outcome {
        Outcome::Ok(ok) => match &ok.data {
            Some(ResultData::Devices { devices, .. }) => devices.clone(),
            other => panic!("{other:?}"),
        },
        Outcome::Error(e) => panic!("{e:?}"),
    }
}

fn entry(name: &str, output_channels: u32) -> DeviceEntry {
    DeviceEntry {
        name: name.into(),
        output_channels,
        is_default: false,
        buffer_frames: openlab_core::DEFAULT_BUFFER_FRAMES,
        state: None,
        primary: false,
    }
}

/// The primary device's `list_devices` entry, its output in `state`.
fn primary(state: OutputState) -> DeviceEntry {
    DeviceEntry {
        state: Some(state),
        primary: true,
        ..entry(DEV_NAME, 2)
    }
}

/// Static problems reported for `cue`, in order.
fn problems(msgs: &[ServerMessage], n: u128) -> Vec<Vec<WireProblem>> {
    changes(msgs)
        .into_iter()
        .filter_map(|c| match c {
            RuntimeChange::Problems { cue, problems } if cue == id(n) => Some(problems),
            _ => None,
        })
        .collect()
}

fn notices(msgs: &[ServerMessage]) -> Vec<String> {
    warnings(msgs)
        .into_iter()
        .filter_map(|w| match w {
            WireWarning::Notice { code, .. } => Some(code),
            _ => None,
        })
        .collect()
}

/// The devices the runtime notices name, in order.
fn notice_devices(msgs: &[ServerMessage]) -> Vec<Option<String>> {
    warnings(msgs)
        .into_iter()
        .filter_map(|w| match w {
            WireWarning::Notice { device, .. } => Some(device),
            _ => None,
        })
        .collect()
}

/// The last workspace warnings list sent.
fn last_warnings(msgs: &[ServerMessage]) -> Vec<WireWarning> {
    msgs.iter()
        .rev()
        .find_map(|m| match m {
            ServerMessage::Warnings { warnings } => Some(warnings.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// The last workspace warnings list sent, as notice codes.
fn workspace_warnings(msgs: &[ServerMessage]) -> Option<Vec<String>> {
    msgs.iter()
        .filter_map(|m| match m {
            ServerMessage::Warnings { warnings } => Some(
                warnings
                    .iter()
                    .filter_map(|w| match w {
                        WireWarning::Notice { code, .. } => Some(code.clone()),
                        _ => None,
                    })
                    .collect(),
            ),
            _ => None,
        })
        .next_back()
}

fn logs(msgs: &[ServerMessage], level: LogLevel) -> Vec<String> {
    msgs.iter()
        .filter_map(|m| match m {
            ServerMessage::Log { level: l, message } if *l == level => Some(message.clone()),
            _ => None,
        })
        .collect()
}

fn broken_device_lost(msgs: &[ServerMessage], n: u128) -> bool {
    states(msgs, id(n)).contains(&(WireCueState::Broken, Some(WireBrokenReason::DeviceLost)))
}

/// Whether cue `n` was reported paused by the loss of its device (it played or waited on it).
fn paused_device_lost(msgs: &[ServerMessage], n: u128) -> bool {
    states(msgs, id(n)).contains(&(WireCueState::Paused, Some(WireBrokenReason::DeviceLost)))
}

/// The progress items reported for cue `n`, as `(elapsed, remaining)` in seconds.
fn progress_of(msgs: &[ServerMessage], n: u128) -> Vec<(f64, Option<f64>)> {
    msgs.iter()
        .filter_map(|m| match m {
            ServerMessage::Progress { items, .. } => Some(items),
            _ => None,
        })
        .flatten()
        .filter(|i| i.cue == id(n))
        .map(|i| (i.elapsed, i.remaining))
        .collect()
}

/// The pre-wait countdowns reported for cue `n`, as `(elapsed, remaining)` in seconds.
fn pre_waits_of(msgs: &[ServerMessage], n: u128) -> Vec<(f64, f64)> {
    msgs.iter()
        .filter_map(|m| match m {
            ServerMessage::Progress { waits, .. } => Some(waits),
            _ => None,
        })
        .flatten()
        .filter(|w| w.cue == id(n) && w.wait == openlab_core::protocol::WireWait::PreWait)
        .map(|w| (w.elapsed, w.remaining))
        .collect()
}

/// Unplugs the device (reported lost) and lets the loop see it and close the stream.
fn lose(s: &mut DeviceShow) {
    s.fake.unplug(DEV, true);
    s.settle_devices();
    assert!(s.control().device_down());
}

/// Plugs the device back in and lets the loop find it and reopen.
fn replug(s: &mut DeviceShow) {
    s.fake.plug(listed(DEV, DEV_NAME, 2));
    // The next list read (once a second) finds it; the reopen follows at once.
    s.wait_wall(Duration::from_secs(1), 2);
    assert!(!s.control().device_down(), "the device was not reopened");
}

#[test]
fn list_devices_answers_from_the_host_list_and_follows_hot_plug() {
    let path = wav(&dc(100, 0.5));
    let mut s = device_show(workspace(vec![audio(1, &path, 0)]));
    assert_eq!(list_devices(&mut s), vec![primary(OutputState::Up)]);
    s.take_outbound();

    // Another device comes and goes: the list follows; the patches do not change.
    s.fake.plug(listed("fake:2", "Fake 2", 8));
    s.wait_wall(Duration::from_secs(1), 1);
    assert_eq!(
        list_devices(&mut s),
        vec![primary(OutputState::Up), entry("Fake 2", 8)]
    );
    s.fake.unplug("fake:2", false);
    s.wait_wall(Duration::from_secs(1), 1);
    assert_eq!(list_devices(&mut s), vec![primary(OutputState::Up)]);
    let msgs = s.take_outbound();
    assert!(problems(&msgs, 1).is_empty());
    assert!(!s.control().device_down());

    // The open device leaves the list without any error from the host (some hosts just stop
    // calling back): that is a loss, found by the list.
    s.fake.unplug(DEV, false);
    s.wait_wall(Duration::from_secs(1), 1);
    assert!(s.control().device_down());
    assert_eq!(list_devices(&mut s), vec![]);
    let msgs = s.take_outbound();
    assert_eq!(notices(&msgs), vec!["device_lost".to_owned()]);
    // The notice names its device, so a client can list one entry per device.
    assert_eq!(notice_devices(&msgs), vec![Some(DEV_NAME.to_owned())]);
    assert_eq!(logs(&msgs, LogLevel::Error).len(), 1);
    // At once the patch is reported missing: the idle cue on it shows broken.
    let p = problems(&msgs, 1);
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(matches!(&p[0][..], [WireProblem::DeviceUnavailable { .. }]));
    assert_eq!(s.control().engine().state(id(1)), CueState::Broken);

    // Plugged back in: reopened, the patch maps again, the cue is playable.
    replug(&mut s);
    assert_eq!(list_devices(&mut s), vec![primary(OutputState::Up)]);
    let msgs = s.take_outbound();
    assert_eq!(problems(&msgs, 1), vec![vec![]]);
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    assert_eq!(workspace_warnings(&msgs), Some(vec![]));
    let at = go(&mut s);
    s.run(300);
    assert_eq!(first_nonzero(&s.channel(0)), Some(at as usize));
}

#[test]
fn a_slow_device_open_does_not_hold_up_the_inbox() {
    let path = wav(&dc(400, 0.5));
    let mut s = device_show(workspace(vec![audio(1, &path, 0)]));
    s.run(BLOCK);
    lose(&mut s);
    // The host takes as long as it likes to open the device again.
    s.fake.close_gate();
    s.fake.plug(listed(DEV, DEV_NAME, 2));
    s.advance(Duration::from_secs(1));
    s.tick();
    // The reopen is due (the backoff has passed); the device thread is stuck in it. The list
    // read is on its own thread and answers.
    s.wait_list();
    assert_eq!(s.control().device_jobs_pending(), 1, "an open is under way");
    let pending = 1;
    // Replies and GO meanwhile.
    let rid = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id: rid,
        request: Request::Snapshot,
    });
    assert_eq!(replies.len(), 2);
    assert_eq!(list_devices(&mut s), vec![primary(OutputState::Lost)]);
    let at = go(&mut s);
    let replies = s.transport(TransportAction::Panic);
    assert!(matches!(
        replies.as_slice(),
        [ServerMessage::Result {
            outcome: Outcome::Ok(_),
            ..
        }]
    ));
    for _ in 0..50 {
        s.tick();
    }
    assert_eq!(s.control().device_jobs_pending(), pending);
    s.fake.open_gate();
    s.settle_devices();
    assert!(!s.control().device_down());
    // The GO and the Panic during the outage were handled; the panic (a hard stop during an
    // outage) left nothing to play.
    assert_eq!(at, s.now());
    s.run(600);
    assert!(s.channel(0).iter().skip(at as usize).all(|&v| v == 0.0));
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
}

#[test]
fn a_stop_during_an_outage_applies_on_the_first_sample_after_it() {
    let a = wav(&dc(20_000, 0.25));
    let mut s = device_show(workspace(vec![audio(1, &a, 0)]));
    start(&mut s, 1);
    s.run(BLOCK * 3);
    let cut = s.rendered() as usize;
    lose(&mut s);
    s.transport(TransportAction::HardStop { cue: id(1) });
    replug(&mut s);
    s.run(BLOCK * 2);
    assert!(s.channel(0)[cut..].iter().all(|&v| v == 0.0));
    assert_eq!(s.control().live_voices(), 0);
}

#[test]
fn a_device_with_fewer_outputs_than_a_patch_maps_is_warned_about() {
    let path = wav(&dc(100, 0.5));
    let ws = workspace(vec![audio(1, &path, 0)]);
    let c = device_config(&ws);
    // One channel: the mixer has two outputs and the patch maps device channels 0 and 1.
    let mut s = DeviceShow::new(ws, vec![listed(DEV, DEV_NAME, 1)], c);
    let msgs = s.take_outbound();
    let codes = workspace_warnings(&msgs).unwrap();
    assert_eq!(
        codes,
        vec!["outputs_dropped".to_owned(), "outputs_dropped".to_owned()]
    );
    assert_eq!(s.control().engine().state(id(1)), CueState::Broken);
    let warnings: Vec<WireWarning> = msgs
        .iter()
        .filter_map(|m| match m {
            ServerMessage::Warnings { warnings } => Some(warnings.clone()),
            _ => None,
        })
        .next_back()
        .unwrap();
    let WireWarning::Notice { message, .. } = &warnings[1] else {
        panic!()
    };
    assert!(message.contains("Main"), "{message}");
}

#[test]
fn reopen_attempts_back_off_while_the_device_stays_away() {
    let path = wav(&dc(100, 0.5));
    let mut s = device_show(workspace(vec![audio(1, &path, 0)]));
    lose(&mut s);
    s.take_outbound();
    // 250 ms, then 500, 1000, 2000 ms between attempts: at 0.25, 0.75, 1.75 and 3.75 s.
    let mut seen = Vec::new();
    for step in 1..=50u32 {
        s.advance(Duration::from_millis(100));
        s.settle_devices();
        for m in logs(&s.take_outbound(), LogLevel::Warn) {
            if m.contains("reopen attempt") {
                seen.push((step * 100, m));
            }
        }
    }
    let at: Vec<u32> = seen.iter().map(|(t, _)| *t).collect();
    assert_eq!(at, vec![300, 800, 1800, 3800], "{seen:?}");
    assert!(seen[3].1.contains("attempt 4"), "{}", seen[3].1);
    assert!(seen[3].1.contains("next in 4.00 s"), "{}", seen[3].1);
    assert_eq!(s.fake.opens(), 5);
}

fn running_cue_show() -> DeviceShow {
    let long = wav(&dc(200_000, 0.25));
    let mut s = device_show(workspace(vec![audio(1, &long, 0), audio(2, &long, 1)]));
    start(&mut s, 1);
    s.run(BLOCK * 2);
    s
}

#[test]
fn the_reopen_backoff_starts_again_once_the_stream_has_stayed_up() {
    let mut s = running_cue_show();
    lose(&mut s);
    s.wait_wall(Duration::from_secs(2), 4);
    replug(&mut s);
    s.run(BLOCK);
    s.wait_wall(STABLE_UP + Duration::from_secs(1), 2);
    let opens = s.fake.opens();
    lose(&mut s);
    s.wait_wall(Duration::from_millis(300), 3);
    assert_eq!(
        s.fake.opens(),
        opens + 1,
        "the first attempt comes after 250 ms again"
    );
}

#[test]
fn a_device_thread_stuck_in_an_open_makes_the_device_unrecoverable_but_not_the_list() {
    let mut s = running_cue_show();
    lose(&mut s);
    s.fake.close_gate();
    s.fake.plug(listed(DEV, DEV_NAME, 2));
    s.advance(Duration::from_secs(1));
    s.tick();
    s.wait_list();
    s.tick();
    s.take_outbound();
    for _ in 0..12 {
        s.advance(Duration::from_secs(1));
        s.tick();
        s.wait_list();
    }
    let msgs = s.take_outbound();
    assert!(
        logs(&msgs, LogLevel::Error)
            .iter()
            .any(|m| m.contains("open") && m.contains("restart openLab")),
        "{:?}",
        logs(&msgs, LogLevel::Error)
    );
    let codes = workspace_warnings(&msgs).unwrap();
    assert!(
        codes.contains(&"device_unrecoverable".to_owned()),
        "{codes:?}"
    );
    assert!(
        !codes.contains(&"device_lost".to_owned()),
        "no more 'reconnecting'"
    );
    // The list is still read.
    s.fake.plug(listed("fake:2", "Fake 2", 8));
    s.advance(Duration::from_secs(1));
    s.tick();
    s.wait_list();
    assert_eq!(
        list_devices(&mut s),
        vec![primary(OutputState::Failed), entry("Fake 2", 8)]
    );
    // A late answer still counts.
    s.fake.open_gate();
    s.settle_devices();
    assert!(!s.control().device_down());
    let msgs = s.take_outbound();
    assert_eq!(workspace_warnings(&msgs), Some(vec![]));
}

#[test]
fn a_mixer_the_host_never_hands_back_makes_the_device_unrecoverable() {
    let mut s = running_cue_show();
    s.fake.hold_on_close(true);
    lose(&mut s);
    s.take_outbound();
    s.fake.plug(listed(DEV, DEV_NAME, 2));
    s.wait_wall(DEVICE_JOB_TIMEOUT + Duration::from_secs(1), 12);
    let msgs = s.take_outbound();
    let codes = workspace_warnings(&msgs).unwrap();
    assert!(
        codes.contains(&"device_unrecoverable".to_owned()),
        "{codes:?}"
    );
    assert!(
        logs(&msgs, LogLevel::Error)
            .iter()
            .any(|m| m.contains("handed the audio engine back"))
    );
    assert_eq!(s.fake.opens(), 1);
    assert!(s.control().device_down());
}

#[test]
fn a_panic_in_the_device_layer_during_an_open_is_caught() {
    let mut s = running_cue_show();
    lose(&mut s);
    s.fake.panic_on_open();
    s.take_outbound();
    s.fake.plug(listed(DEV, DEV_NAME, 2));
    s.wait_wall(Duration::from_secs(2), 4);
    let msgs = s.take_outbound();
    assert!(s.control().device_down());
    assert_eq!(
        s.control().device_jobs_pending(),
        0,
        "the open was answered"
    );
    let codes = workspace_warnings(&msgs).unwrap();
    assert!(
        codes.contains(&"device_unrecoverable".to_owned()),
        "{codes:?}"
    );
    // The show still answers.
    assert_eq!(list_devices(&mut s), vec![primary(OutputState::Failed)]);
}

#[test]
fn a_device_missing_from_one_list_read_while_it_plays_is_not_lost() {
    let mut s = running_cue_show();
    s.fake.hide_once(DEV);
    s.run(BLOCK * 4);
    s.advance(Duration::from_secs(1));
    s.tick();
    s.wait_list();
    s.tick();
    assert!(
        !s.control().device_down(),
        "one read missed it while it played"
    );
    s.run(BLOCK * 4);
    // Listed again: the count starts over.
    s.advance(Duration::from_secs(1));
    s.tick();
    s.wait_list();
    s.run(BLOCK * 4);
    // Missing from two reads in a row: lost, though it still calls back.
    s.fake.hide_once(DEV);
    s.advance(Duration::from_secs(1));
    s.tick();
    s.wait_list();
    s.tick();
    assert!(!s.control().device_down());
    s.run(BLOCK * 2);
    s.fake.hide_once(DEV);
    s.advance(Duration::from_secs(1));
    s.tick();
    s.wait_list();
    s.tick();
    assert!(s.control().device_down());
}

#[test]
fn a_first_open_that_never_answers_fails_the_start_with_a_clear_error() {
    let fake = Fake::new(vec![listed(DEV, DEV_NAME, 2)]);
    fake.close_gate();
    let (_ctl, mixer) = openlab_audio::Mixer::new(mixer_config()).unwrap();
    let (inbox, _rx) = crossbeam_channel::unbounded();
    let result = devices::start_with_timeout(
        Box::new(FakeLayer(Arc::clone(&fake))),
        mixer,
        openlab_audio::OpenRequest::default(),
        vec![],
        inbox,
        devices::system_clock(),
        Duration::from_millis(20),
    );
    let Err(e) = result else { panic!("started") };
    assert!(matches!(e, devices::StartError::Timeout(_)));
    assert!(e.to_string().contains("no answer"), "{e}");
    fake.open_gate();
}

// ---- The device-loss policy (measured on QLab 5) ----

/// Cue 1 plays a ramp (every sample distinct) and auto-follows into cue 2; both on the device.
fn follow_show() -> DeviceShow {
    let a = wav(&ramp(3000));
    let b = wav(&dc(500, 0.5));
    let mut c1 = audio(1, &a, 0);
    c1.continue_mode = ContinueMode::AutoFollow;
    device_show(workspace(vec![c1, audio(2, &b, 1)]))
}

/// The device goes while a click in cue 1's waveform waits for the new point's stream (its
/// open held back): the cue gets the lost-device mark like any playing cue, nothing sounds, and
/// neither the voice it was moving from nor the waiting stream is left behind.
#[test]
fn a_device_lost_while_a_click_waits_leaves_the_lost_mark_and_nothing_held() {
    let long = wav(&dc(200_000, 0.25));
    let ws = workspace(vec![audio(1, &long, 0)]);
    let mut c = device_config(&ws);
    c.prime_cues = 0;
    c.streamer.before_open = gated_open;
    let mut s = DeviceShow::new(ws, vec![listed(DEV, DEV_NAME, 2)], c);
    start(&mut s, 1);
    s.run(BLOCK * 4);
    gate(&long);
    s.transport(TransportAction::Seek {
        cue: id(1),
        position: 10.0,
    });
    lose(&mut s);
    assert_eq!(s.control().engine().state(id(1)), CueState::Paused);
    assert!(s.control().engine().is_lost(id(1)));
    // The waiting move is gone at once; the voice it moved from goes when its mixer reports.
    assert!(
        s.control().held_streams() <= 1,
        "{}",
        s.control().held_streams()
    );
    release(&long);
    replug(&mut s);
    s.run(BLOCK * 20);
    assert!(s.control().engine().is_lost(id(1)));
    assert_eq!(s.control().live_voices(), 0);
    assert_eq!(s.control().held_streams(), 0);
    let ch = s.channel(0);
    let tail = &ch[ch.len() - BLOCK as usize * 10..];
    assert!(
        tail.iter().all(|&v| v == 0.0),
        "nothing sounds after the loss"
    );
}

/// The device goes while cue 1 plays and stays away `outage`. Whatever the length of the
/// outage (QLab 5, measured): cue 1 stops at once and shows paused with its time standing still,
/// it stays so when the device returns, its auto-follow never fires, Resume All only clears the
/// mark, and GO plays it from its start.
fn lost_cue_stays_stopped(outage: Duration) {
    let mut s = follow_show();
    s.run(BLOCK * 2);
    let at = go(&mut s);
    s.run(640);
    let cut = s.rendered() as usize;
    s.take_outbound();
    lose(&mut s);
    // At once: shown paused for the lost device, voice released, nothing left to play.
    let msgs = s.take_outbound();
    assert!(paused_device_lost(&msgs, 1));
    assert_eq!(s.control().engine().state(id(1)), CueState::Paused);
    assert!(s.control().engine().is_lost(id(1)));
    // The time it stopped at goes out with the loss, though no device renders: the time shown
    // during the outage is the one shown after it.
    let at_loss = progress_of(&msgs, 1).last().copied();
    s.wait_wall(outage, 8);
    replug(&mut s);
    s.run(BLOCK * 40);
    // Back: still paused, its time standing where the loss left it (sample time stood still
    // while no device played, and runs on now); nothing resumed, the follow-on cue idle.
    let msgs = s.take_outbound();
    let shown = progress_of(&msgs, 1);
    assert!(shown.len() > 1, "{shown:?}");
    assert!(shown.windows(2).all(|w| w[0] == w[1]), "{shown:?}");
    let (elapsed, remaining) = shown[0];
    assert_eq!(at_loss, Some(shown[0]), "{shown:?}");
    let played = (cut - at as usize) as f64;
    assert!(
        (elapsed * f64::from(RATE) - played).abs() <= BLOCK as f64 * 2.0,
        "{elapsed} s for {played} frames"
    );
    assert!(remaining.is_some_and(|r| r > 0.0), "{remaining:?}");
    assert_eq!(s.control().engine().state(id(1)), CueState::Paused);
    assert!(
        states(&msgs, id(1)).is_empty(),
        "{:?}",
        states(&msgs, id(1))
    );
    assert_eq!(s.control().engine().state(id(2)), CueState::Idle);
    // Resume All only clears the mark.
    s.transport(TransportAction::ResumeAll);
    s.run(BLOCK * 4);
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    assert_eq!(
        states(&s.take_outbound(), id(1)),
        vec![(WireCueState::Idle, None)]
    );
    // Not one sample after the cut, and the follow-on cue never started, though its time
    // came (the show runs past the end of cue 1's file).
    s.run(4000);
    assert!(s.channel(0)[cut..].iter().all(|&v| v == 0.0));
    assert!(s.channel(1).iter().all(|&v| v == 0.0));
    assert_eq!(s.control().live_voices(), 0);
    assert_eq!(s.control_mut().mixer_voice_count(), 0);
    assert!(!states(&s.take_outbound(), id(2)).contains(&(WireCueState::Running, None)));

    // GO restarts it from the beginning (its progress went with the loss).
    let at2 = start(&mut s, 1);
    s.run(3600);
    assert_ne!(at2, at);
    assert_eq!(
        s.channel(0)[at2 as usize..at2 as usize + 1000],
        ramp(1000)[..]
    );
    // And its auto-follow works again.
    assert_eq!(first_nonzero(&s.channel(1)), Some(at2 as usize + 3000));
}

/// GO on the paused-looking cue once the device is back plays it from its start without a
/// Resume first (QLab 5, measured).
#[test]
fn go_on_a_cue_its_lost_device_left_paused_plays_it_from_its_start_once_back() {
    let mut s = follow_show();
    s.run(BLOCK * 2);
    go(&mut s);
    s.run(640);
    lose(&mut s);
    // While away GO on it is refused as broken and the mark stays.
    s.take_outbound();
    start(&mut s, 1);
    assert!(warnings(&s.take_outbound()).contains(&WireWarning::CueBroken { cue: id(1) }));
    assert_eq!(s.control().engine().state(id(1)), CueState::Paused);
    replug(&mut s);
    s.run(BLOCK * 2);
    let at = start(&mut s, 1);
    s.run(1200);
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    assert_eq!(
        s.channel(0)[at as usize..at as usize + 1000],
        ramp(1000)[..]
    );
}

/// The full progress items reported for cue `n`, as `(elapsed, remaining, position)`.
fn items_of(msgs: &[ServerMessage], n: u128) -> Vec<(f64, Option<f64>, Option<f64>)> {
    msgs.iter()
        .filter_map(|m| match m {
            ServerMessage::Progress { items, .. } => Some(items),
            _ => None,
        })
        .flatten()
        .filter(|i| i.cue == id(n))
        .map(|i| (i.elapsed, i.remaining, i.position))
        .collect()
}

/// A 500-frame file looping for ever, played 1700 frames, then paused by the operator or by
/// the loss of its device: what is reported for it.
fn stood_after(lost: bool) -> Vec<(f64, Option<f64>, Option<f64>)> {
    let mut c = audio(1, &wav(&ramp(500)), 0);
    if let CueKind::Audio {
        last_slice_play_count,
        ..
    } = &mut c.kind
    {
        *last_slice_play_count = 0.into();
    }
    let mut s = device_show(workspace(vec![c]));
    s.run(BLOCK * 2);
    go(&mut s);
    s.run(1700);
    s.take_outbound();
    if lost {
        lose(&mut s);
    } else {
        s.transport(TransportAction::Pause { cue: id(1) });
        s.run(BLOCK * 8);
    }
    items_of(&s.take_outbound(), 1)
}

/// A looping cue its lost device left paused shows its time as an operator's pause shows it:
/// the position in the pass it was on, with the same elapsed and remaining.
#[test]
fn a_looping_cue_its_lost_device_left_paused_shows_its_place_in_the_loop() {
    let lost = stood_after(true);
    let paused = stood_after(false);
    let (Some(&(le, lr, lp)), Some(&(pe, pr, pp))) = (lost.last(), paused.last()) else {
        panic!("{lost:?} {paused:?}");
    };
    let block = BLOCK as f64 / f64::from(RATE);
    let p = lp.expect("a position");
    assert!(p < 500.0 / f64::from(RATE), "{lost:?}");
    assert!(
        (p - pp.expect("a position")).abs() <= block * 2.0,
        "{lost:?} {paused:?}"
    );
    assert!((le - pe).abs() <= block * 2.0, "{lost:?} {paused:?}");
    assert_eq!(lr.is_some(), pr.is_some(), "{lost:?} {paused:?}");
}

#[test]
fn a_cue_playing_when_the_device_goes_stops_and_stays_stopped_after_3_s() {
    lost_cue_stays_stopped(Duration::from_secs(3));
}

#[test]
fn a_cue_playing_when_the_device_goes_stops_and_stays_stopped_after_20_s() {
    lost_cue_stays_stopped(Duration::from_secs(20));
}

/// The peaks of cue `n` in every meter message of `msgs`.
fn cue_meters(msgs: &[ServerMessage], n: u128) -> Vec<Vec<f32>> {
    msgs.iter()
        .filter_map(|m| match m {
            ServerMessage::Meters { cues, .. } => Some(cues),
            _ => None,
        })
        .flatten()
        .filter(|c| c.cue == id(n))
        .map(|c| c.peaks.clone())
        .collect()
}

/// The meters of `patch` in `msgs` that show any sound: its device outputs, then its cue
/// outputs.
fn lit_patch_meters(msgs: &[ServerMessage], patch: openlab_core::PatchId) -> Vec<Vec<f32>> {
    msgs.iter()
        .filter_map(|m| match m {
            ServerMessage::Meters { outputs, .. } => Some(outputs),
            _ => None,
        })
        .flatten()
        .filter(|o| o.patch == patch)
        .map(|o| {
            o.peaks
                .iter()
                .chain(&o.cue_outputs)
                .copied()
                .collect::<Vec<f32>>()
        })
        .filter(|p| p.iter().any(|&x| x > 0.0))
        .collect()
}

/// Checklist: a cue plays, its device is lost and returned on the virtual clock: no meter
/// message for that cue after the loss until it sounds again. The same for a cue paused by
/// the operator and one a Fade cue is fading, and whichever block of the meter period the
/// loss lands in (the peaks rendered since the last read are still held in the mixer).
#[test]
fn a_cue_its_lost_device_stopped_gets_no_meter_until_it_sounds_again() {
    for blocks in 1..=4 {
        let long = wav(&dc(200_000, 0.5));
        // Ends just before the loss, its end handled after the last meter read.
        let short = wav(&dc(BLOCK as usize * 2 + 10, 0.5));
        let fade = cue(
            4,
            CueKind::Fade {
                target: Some(id(3)),
                duration: secs(40_000),
                curve: openlab_core::FadeCueCurve::like(openlab_core::FadeCurve::Linear),
                levels: openlab_core::FadeLevels {
                    master_db: Some(-40.0),
                    output_db: vec![],
                    crosspoints: vec![],
                },
                stop_target_when_done: false,
                mode: Default::default(),
                geometry: Default::default(),
            },
        );
        let mut s = device_show(workspace(vec![
            audio(1, &long, 0),
            audio(2, &long, 1),
            audio(3, &long, 0),
            fade,
            audio(5, &long, 1),
            audio(6, &short, 0),
        ]));
        s.run(BLOCK * 2);
        for n in [1, 2, 3, 4, 5] {
            start(&mut s, n);
        }
        s.run(BLOCK * 6);
        s.transport(TransportAction::Pause { cue: id(2) });
        s.run(BLOCK * 8 + BLOCK * blocks);
        // Cue 5 is paused and resumed just before the loss: the mixer holds peaks under the id
        // it had before the resume too.
        s.transport(TransportAction::Pause { cue: id(5) });
        s.transport(TransportAction::Resume { cue: id(5) });
        s.run(BLOCK);
        // Cue 6 plays out, and the device goes on the tick its voice's end is handled: the
        // mixer still holds the last peaks of a voice the doc already let go of.
        let voices = s.control().live_voices();
        start(&mut s, 6);
        s.step();
        assert_eq!(s.control().live_voices(), voices + 1);
        // Bounded: a voice that never ends fails the test instead of hanging it.
        for _ in 0..10_000 {
            if s.control().live_voices() <= voices {
                break;
            }
            s.step();
        }
        assert_eq!(s.control().live_voices(), voices, "the faded voice ended");
        assert_eq!(s.control().engine().state(id(3)), CueState::Fading);
        assert_eq!(s.control().engine().state(id(5)), CueState::Running);
        let before = s.take_outbound();
        assert!(!cue_meters(&before, 1).is_empty(), "cue 1 was metered");
        lose(&mut s);
        s.wait_wall(Duration::from_secs(3), 6);
        replug(&mut s);
        // Well past the reopen and several meter periods.
        s.run(u64::from(RATE) * 2);
        let after = s.take_outbound();
        for n in [1, 2, 3, 5, 6] {
            assert_eq!(
                cue_meters(&after, n),
                Vec::<Vec<f32>>::new(),
                "cue {n}, loss {blocks} block(s) into the period"
            );
        }
        // Nor do the patch's meters light with what the mixer rendered before the loss.
        assert_eq!(
            lit_patch_meters(&after, patch_id()),
            Vec::<Vec<f32>>::new(),
            "loss {blocks} block(s) into the period"
        );
        // Nothing is kept for the voices the loss cut once no meter read can name them.
        assert!(s.control().is_idle());
        // It sounds again: metered again.
        start(&mut s, 1);
        s.run(BLOCK * 16);
        assert!(
            cue_meters(&s.take_outbound(), 1)
                .iter()
                .any(|p| p.iter().any(|&x| x > 0.4)),
            "cue 1 metered once it plays again"
        );
    }
}

/// Every state a cue on the lost device can be in, as QLab 5 shows it (measured: playing,
/// fading as playing, pre-waiting, paused by the operator, idle, loaded; a post-wait assumed
/// like a pre-wait): playing and fading cues show paused and keep the mark after the return; a
/// pre-wait and a post-wait show paused with their countdown frozen and are idle again by
/// themselves on the return; a paused, a loaded and an idle cue show broken while the device
/// is away and are idle once it is back. Nothing sounds or fires after the loss.
#[test]
fn each_state_on_a_lost_device_shows_as_qlab_shows_it() {
    let long = wav(&dc(40_000, 0.25));
    let short = wav(&dc(200, 0.5));
    let mut pre = audio(3, &long, 0);
    pre.pre_wait = secs(4000);
    // Its action ends at 200 frames; its auto-continue counts on to 3000.
    let mut post = audio(4, &short, 1);
    post.continue_mode = ContinueMode::AutoContinue;
    post.post_wait = secs(3000);
    let mut fade = cue(
        8,
        CueKind::Fade {
            target: Some(id(2)),
            duration: secs(20_000),
            curve: openlab_core::FadeCueCurve::like(openlab_core::FadeCurve::Linear),
            levels: openlab_core::FadeLevels {
                master_db: Some(-40.0),
                output_db: vec![],
                crosspoints: vec![],
            },
            stop_target_when_done: false,
            mode: Default::default(),
            geometry: Default::default(),
        },
    );
    fade.continue_mode = ContinueMode::DoNotContinue;
    let mut s = device_show(workspace(vec![
        audio(1, &long, 0),
        audio(2, &long, 1),
        pre,
        post,
        cue(5, CueKind::Memo),
        audio(6, &long, 0),
        audio(7, &long, 1),
        fade,
        audio(9, &long, 0),
    ]));
    s.run(BLOCK * 2);
    for n in [1, 2, 3, 4, 6] {
        start(&mut s, n);
    }
    start(&mut s, 8);
    s.transport(TransportAction::Load {
        cue: id(7),
        at: 0.0,
    });
    s.run(BLOCK * 8);
    s.transport(TransportAction::Pause { cue: id(6) });
    s.run(BLOCK * 2);
    let engine = s.control().engine();
    assert_eq!(engine.state(id(1)), CueState::Running);
    assert_eq!(engine.state(id(2)), CueState::Fading);
    assert_eq!(engine.state(id(3)), CueState::PreWaiting);
    assert_eq!(engine.state(id(4)), CueState::PostWaiting);
    assert_eq!(engine.state(id(6)), CueState::Paused);
    assert_eq!(engine.state(id(7)), CueState::Loaded);
    s.take_outbound();
    let cut = s.rendered() as usize;
    lose(&mut s);
    let msgs = s.take_outbound();
    for n in [1, 2, 3, 4] {
        assert!(
            paused_device_lost(&msgs, n),
            "cue {n}: {:?}",
            states(&msgs, id(n))
        );
    }
    assert!(broken_device_lost(&msgs, 6), "{:?}", states(&msgs, id(6)));
    assert_eq!(
        states(&msgs, id(7)).last(),
        Some(&(WireCueState::Broken, None))
    );
    assert_eq!(states(&msgs, id(9)), vec![(WireCueState::Broken, None)]);
    // The Fade cue and the memo are not on a device; the Fade (as every wait) stops with the
    // last device that played.
    assert_ne!(s.control().engine().state(id(8)), CueState::Fading);
    assert_eq!(s.control().engine().state(id(5)), CueState::Idle);
    // Nothing scheduled is left.
    assert_eq!(s.control().engine().next_deadline(), None);
    s.wait_wall(Duration::from_secs(5), 4);
    replug(&mut s);
    s.run(BLOCK * 40);
    let msgs = s.take_outbound();
    // The pre-wait's countdown was frozen and is gone now.
    assert!(
        pre_waits_of(&msgs, 3).is_empty(),
        "{:?}",
        pre_waits_of(&msgs, 3)
    );
    let engine = s.control().engine();
    for n in [1, 2] {
        assert_eq!(engine.state(id(n)), CueState::Paused, "cue {n}");
    }
    for n in [3, 4, 5, 6, 7, 9] {
        assert_eq!(engine.state(id(n)), CueState::Idle, "cue {n}");
    }
    // Past every deadline set before the loss: nothing sounds or starts.
    s.run(8000);
    for ch in 0..2 {
        assert!(
            s.channel(ch)[cut..].iter().all(|&v| v == 0.0),
            "channel {ch}"
        );
    }
    assert_eq!(s.control().engine().state(id(5)), CueState::Idle);
    assert_eq!(s.control().live_voices(), 0);
    assert_eq!(s.control_mut().mixer_voice_count(), 0);
}

/// While the device is away a waiting cue's countdown shows frozen where the loss left it.
#[test]
fn a_pre_wait_on_a_lost_device_shows_its_countdown_frozen() {
    let long = wav(&dc(40_000, 0.25));
    let mut pre = audio(3, &long, 0);
    pre.pre_wait = secs(40_000);
    // A second device keeps sample time running while the first is away.
    let mut s = two_device_show(vec![pre, audio_b(4, &long, 0)]);
    start(&mut s, 4);
    start(&mut s, 3);
    s.run(BLOCK * 8);
    let at = s.rendered();
    unplug(&mut s, DEV);
    s.take_outbound();
    for _ in 0..40 {
        s.play(BLOCK);
    }
    let shown = pre_waits_of(&s.take_outbound(), 3);
    assert!(shown.len() > 1, "{shown:?}");
    assert!(shown.windows(2).all(|w| w[0] == w[1]), "{shown:?}");
    assert!(
        s.control().rendered() > at + BLOCK * 20,
        "sample time ran on"
    );
    assert_eq!(s.control().engine().state(id(3)), CueState::Paused);
    assert_eq!(s.control().engine().state(id(4)), CueState::Running);
    // Back: idle by itself, and it never fires.
    s.fake.plug(listed(DEV, DEV_NAME, 2));
    s.wait_wall(Duration::from_secs(1), 2);
    assert_eq!(s.control().engine().state(id(3)), CueState::Idle);
}

#[test]
fn a_lost_device_stops_exactly_the_runs_on_it_and_breaks_the_cues_routed_to_it() {
    let long = wav(&dc(40_000, 0.25));
    let other = wav(&dc(40_000, 0.5));
    let late = wav(&dc(200, 0.75));
    // Cue 3 is in its pre-wait when the device goes: it has no voice yet.
    let mut c3 = audio(3, &late, 0);
    c3.pre_wait = secs(1000);
    let mut s = device_show(workspace(vec![
        audio(1, &long, 0),
        audio(2, &other, 1),
        c3,
        audio(4, &long, 0),
    ]));
    start(&mut s, 1);
    start(&mut s, 2);
    s.run(BLOCK * 4);
    start(&mut s, 3);
    s.run(BLOCK);
    s.take_outbound();
    let cut = s.rendered();
    lose(&mut s);
    let msgs = s.take_outbound();
    // The playing cues show paused for the lost device (QLab 5).
    assert!(paused_device_lost(&msgs, 1));
    assert!(paused_device_lost(&msgs, 2));
    // The pre-waiting cue is routed to the device too: it shows paused with its countdown
    // frozen (no voice yet).
    assert!(paused_device_lost(&msgs, 3));
    // The idle cue routed to the device has no run: it shows broken for the missing device (a
    // static problem, no reason).
    assert_eq!(states(&msgs, id(4)), vec![(WireCueState::Broken, None)]);
    for n in [1, 2, 3] {
        assert!(s.control().engine().is_lost(id(n)), "cue {n}");
        assert_eq!(s.control().engine().run_of(id(n)), None, "cue {n}");
    }
    assert!(
        logs(&msgs, LogLevel::Error)
            .iter()
            .any(|m| m.contains("3 cue(s)"))
    );
    assert_eq!(
        workspace_warnings(&msgs),
        Some(vec!["device_lost".to_owned()])
    );
    assert!(msgs.iter().any(|m| matches!(
        m,
        ServerMessage::Warnings { warnings } if matches!(
            &warnings[..],
            [WireWarning::Notice { device: Some(d), .. }] if d == DEV_NAME
        )
    )));
    // While the device is away, a cue routed to it does not start: broken, as a missing device.
    s.transport(TransportAction::Start { cue: id(4) });
    let msgs = s.take_outbound();
    assert!(warnings(&msgs).contains(&WireWarning::CueBroken { cue: id(4) }));

    replug(&mut s);
    let msgs = s.take_outbound();
    // Back: the waiting and the idle cues are idle again; the cues that played keep their
    // paused mark; nothing they played resumes.
    for n in [3, 4] {
        assert_eq!(
            states(&msgs, id(n)).last(),
            Some(&(WireCueState::Idle, None)),
            "cue {n}"
        );
    }
    for n in [1, 2] {
        assert!(states(&msgs, id(n)).is_empty(), "cue {n}");
        assert_eq!(s.control().engine().state(id(n)), CueState::Paused);
    }
    s.run(BLOCK * 2);
    assert_eq!(s.control().live_voices(), 0, "the failed voices are freed");
    assert_eq!(s.control_mut().mixer_voice_count(), 0);
    let after = (cut + BLOCK * 2) as usize;
    assert!(s.channel(0)[cut as usize..after].iter().all(|&v| v == 0.0));
    assert!(s.channel(1)[cut as usize..after].iter().all(|&v| v == 0.0));
    // Nothing scheduled before the outage starts by itself after it: the pre-wait of cue 3 was
    // cancelled with its run, however long sample time stood still.
    s.run(3000);
    assert_eq!(s.control().engine().state(id(3)), CueState::Idle);
    assert!(s.channel(0)[cut as usize..].iter().all(|&v| v == 0.0));
    // And the stopped cue plays again, from its start.
    let at = start(&mut s, 1);
    s.run(BLOCK * 3);
    assert_eq!(s.channel(0)[at as usize], 0.25);
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    s.transport(TransportAction::Panic);
    s.transport(TransportAction::Panic);
    s.run(BLOCK * 4);
    assert_eq!(s.control().live_voices(), 0);
    assert_eq!(s.control().held_streams(), s.control().primed().len());
}

#[test]
fn a_cue_started_during_an_outage_is_refused_and_plays_from_its_start_once_the_device_is_back() {
    let a = wav(&dc(20_000, 0.25));
    let b = wav(&ramp(300));
    let mut s = device_show(workspace(vec![audio(1, &a, 0), audio(2, &b, 1)]));
    go(&mut s);
    s.run(BLOCK * 3);
    lose(&mut s);
    s.take_outbound();
    // The next cue is routed to the missing device: broken, not queued for the return.
    s.transport(TransportAction::Go { list: list_id() });
    let msgs = s.take_outbound();
    assert!(warnings(&msgs).contains(&WireWarning::CueBroken { cue: id(2) }));
    s.wait_wall(Duration::from_secs(4), 4);
    replug(&mut s);
    s.run(BLOCK * 3);
    assert_eq!(
        first_nonzero(&s.channel(1)),
        None,
        "nothing played on return"
    );
    let at = start(&mut s, 2);
    s.run(600);
    assert_eq!(s.channel(1)[at as usize..at as usize + 300], ramp(300)[..]);
}

#[test]
fn pause_all_and_resume_all_around_a_loss_resume_nothing() {
    let a = wav(&ramp(3000));
    let mut s = device_show(workspace(vec![audio(1, &a, 0)]));
    s.run(BLOCK * 2);
    go(&mut s);
    s.run(640);
    let cut = s.rendered() as usize;
    lose(&mut s);
    // A pause during the outage finds nothing playing (the cue is broken, not paused).
    s.transport(TransportAction::PauseAll);
    s.wait_wall(Duration::from_secs(2), 4);
    replug(&mut s);
    s.run(BLOCK * 2);
    s.transport(TransportAction::ResumeAll);
    s.run(BLOCK * 4);
    assert!(s.channel(0)[cut..].iter().all(|&v| v == 0.0));
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    assert_eq!(s.control().live_voices(), 0);
    assert_eq!(s.control_mut().mixer_voice_count(), 0);
}

#[test]
fn a_paused_cue_is_stopped_by_a_loss_and_does_not_resume() {
    let a = wav(&ramp(3000));
    let mut s = device_show(workspace(vec![audio(1, &a, 0)]));
    s.run(BLOCK * 2);
    go(&mut s);
    s.run(640);
    s.transport(TransportAction::PauseAll);
    s.run(BLOCK * 2);
    assert_eq!(s.control().engine().state(id(1)), CueState::Paused);
    let cut = s.rendered() as usize;
    lose(&mut s);
    replug(&mut s);
    s.transport(TransportAction::ResumeAll);
    s.run(BLOCK * 6);
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    assert!(s.channel(0)[cut..].iter().all(|&v| v == 0.0));
    assert_eq!(s.control().live_voices(), 0);
    assert_eq!(s.control_mut().mixer_voice_count(), 0);
}

#[test]
fn panic_and_stop_during_an_outage_leave_nothing_to_resume() {
    let a = wav(&dc(20_000, 0.25));
    let b = wav(&dc(20_000, 0.5));
    let mut s = device_show(workspace(vec![audio(1, &a, 0), audio(2, &b, 1)]));
    start(&mut s, 1);
    start(&mut s, 2);
    s.run(BLOCK * 3);
    let cut = s.rendered() as usize;
    lose(&mut s);
    s.take_outbound();
    s.transport(TransportAction::HardStop { cue: id(1) });
    s.transport(TransportAction::Panic);
    s.wait_wall(Duration::from_secs(2), 2);
    replug(&mut s);
    s.run(BLOCK * 4);
    for n in [1, 2] {
        assert_eq!(s.control().engine().state(id(n)), CueState::Idle, "cue {n}");
    }
    assert!(s.channel(0)[cut..].iter().all(|&v| v == 0.0));
    assert!(s.channel(1)[cut..].iter().all(|&v| v == 0.0));
    assert_eq!(s.control().live_voices(), 0);
    assert_eq!(s.control_mut().mixer_voice_count(), 0);
}

#[test]
fn a_panic_fade_under_way_when_the_device_goes_ends_with_it() {
    let a = wav(&dc(20_000, 0.25));
    let mut ws = workspace(vec![audio(1, &a, 0)]);
    ws.settings.panic_duration = 1.0;
    let mut s = device_show(ws);
    start(&mut s, 1);
    s.run(BLOCK * 3);
    s.transport(TransportAction::Panic);
    s.run(BLOCK * 2);
    let cut = s.rendered() as usize;
    lose(&mut s);
    // The fading voice is stopped with the rest; a second Panic is the hard stop (a fade could
    // not play) and starts no new fade.
    s.transport(TransportAction::Panic);
    assert_eq!(s.control().engine().panic_end(), None);
    replug(&mut s);
    s.run(BLOCK * 4);
    assert!(s.channel(0)[cut..].iter().all(|&v| v == 0.0));
    assert_eq!(s.control().live_voices(), 0);
    assert_eq!(s.control_mut().mixer_voice_count(), 0);
}

#[test]
fn a_mixer_the_host_still_holds_is_polled_until_it_comes_back_and_nothing_resumes() {
    let a = wav(&ramp(2000));
    let mut s = device_show(workspace(vec![audio(1, &a, 0)]));
    go(&mut s);
    s.run(640);
    let cut = s.rendered() as usize;
    // CoreAudio drops the callback on its own thread a moment after the loss.
    s.fake.hold_on_close(true);
    lose(&mut s);
    s.fake.plug(listed(DEV, DEV_NAME, 2));
    s.wait_wall(Duration::from_secs(2), 4);
    assert!(s.control().device_down(), "no mixer, no reopen");
    assert_eq!(s.fake.opens(), 1);
    s.fake.hold_on_close(false);
    s.fake.drop_held();
    s.wait_wall(Duration::from_secs(1), 2);
    assert!(!s.control().device_down());
    s.run(2400);
    assert!(s.channel(0)[cut..].iter().all(|&v| v == 0.0));
    assert_eq!(s.control().engine().state(id(1)), CueState::Paused);
}

#[test]
fn a_device_back_with_bigger_buffers_keeps_events_on_their_samples() {
    let mut s = follow_show();
    s.run(BLOCK * 2);
    go(&mut s);
    s.run(640);
    lose(&mut s);
    s.wait_wall(Duration::from_secs(2), 4);
    s.fake.set_block(4 * BLOCK as u32);
    replug(&mut s);
    assert_eq!(s.control().block_frames(), 4 * BLOCK);
    s.run(8 * BLOCK);
    // A fresh run on the bigger blocks still lands its auto-follow on the exact sample.
    let at = start(&mut s, 1);
    s.run(3600);
    let ch0 = s.channel(0);
    assert_eq!(ch0[at as usize..at as usize + 100], ramp(100)[..]);
    assert_eq!(first_nonzero(&s.channel(1)), Some(at as usize + 3000));
}

#[test]
fn a_device_that_keeps_dropping_out_leaks_no_voices_or_streams() {
    let mut s = running_cue_show();
    for outage in 1..=6 {
        // Each time the cue plays on the device when it goes, and is gone with it.
        s.take_outbound();
        lose(&mut s);
        assert!(paused_device_lost(&s.take_outbound(), 1), "outage {outage}");
        s.wait_wall(Duration::from_secs(1), 2);
        replug(&mut s);
        s.run(BLOCK * 6);
        assert_eq!(s.control().live_voices(), 0, "outage {outage}");
        assert_eq!(s.control_mut().mixer_voice_count(), 0, "outage {outage}");
        assert_eq!(
            s.control().held_streams(),
            s.control().primed().len(),
            "outage {outage}"
        );
        assert_eq!(s.control().engine().state(id(1)), CueState::Paused);
        start(&mut s, 1);
        s.run(BLOCK * 2);
        assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    }
}

#[test]
fn a_post_wait_armed_before_the_loss_does_not_fire_after_it() {
    let a = wav(&ramp(6000));
    let b = wav(&dc(500, 0.5));
    let mut c1 = audio(1, &a, 0);
    c1.continue_mode = ContinueMode::AutoContinue;
    c1.post_wait = secs(2000);
    let mut s = device_show(workspace(vec![c1, audio(2, &b, 1)]));
    s.run(BLOCK * 2);
    start(&mut s, 1);
    s.run(640);
    lose(&mut s);
    s.wait_wall(Duration::from_secs(20), 8);
    replug(&mut s);
    // Sample time resumes: the post-wait (2000 frames from the start) would have fallen here.
    s.run(6000);
    assert_eq!(first_nonzero(&s.channel(1)), None, "cue 2 never started");
    assert_eq!(s.control().engine().state(id(1)), CueState::Paused);
    assert_eq!(s.control().engine().state(id(2)), CueState::Idle);
    // GO on cue 1 again auto-continues once, from its own post-wait.
    let at = start(&mut s, 1);
    s.run(3000);
    assert_eq!(first_nonzero(&s.channel(1)), Some(at as usize + 2000));
}

#[test]
fn a_timeline_group_hit_by_a_loss_ends_whole_and_nothing_starts_after_the_return() {
    let a = wav(&dc(20_000, 0.25));
    let b = wav(&dc(20_000, 0.5));
    let mut c2 = audio(2, &b, 1);
    c2.pre_wait = secs(1500);
    let group = cue(
        10,
        CueKind::Group {
            mode: GroupMode::Timeline,
            children: vec![audio(1, &a, 0), c2],
        },
    );
    let mut s = device_show(workspace(vec![group]));
    s.run(BLOCK * 2);
    go(&mut s);
    s.run(640);
    assert_eq!(s.control().engine().state(id(2)), CueState::PreWaiting);
    let cut = s.rendered() as usize;
    lose(&mut s);
    let msgs = s.take_outbound();
    assert!(paused_device_lost(&msgs, 1));
    assert!(paused_device_lost(&msgs, 2));
    s.wait_wall(Duration::from_secs(3), 4);
    replug(&mut s);
    s.run(4000);
    // The child that played keeps its mark and so does the group (whichever child failed
    // last); the pre-waiting one is idle.
    for n in [1, 10] {
        assert_eq!(
            s.control().engine().state(id(n)),
            CueState::Paused,
            "cue {n}"
        );
    }
    assert_eq!(s.control().engine().state(id(2)), CueState::Idle);
    assert!(s.channel(0)[cut..].iter().all(|&v| v == 0.0));
    assert!(s.channel(1)[cut..].iter().all(|&v| v == 0.0));
    assert_eq!(s.control().live_voices(), 0);
    assert_eq!(s.control_mut().mixer_voice_count(), 0);
}

// ---- Nothing scheduled before a loss fires after it (a design rule) ----

/// Lets `s` (640 frames after a start) lose its device for 20 s, bring it back, and run on past
/// every deadline set before the loss. Nothing may sound or run by itself.
fn nothing_starts_after_the_return(mut s: DeviceShow, cues: &[u128]) {
    let cut = s.rendered() as usize;
    lose(&mut s);
    s.wait_wall(Duration::from_secs(20), 8);
    replug(&mut s);
    s.run(6000);
    for ch in 0..2 {
        assert!(
            s.channel(ch)[cut..].iter().all(|&v| v == 0.0),
            "channel {ch}"
        );
    }
    for &n in cues {
        let state = s.control().engine().state(id(n));
        // A cue that played keeps the paused mark its lost device left (it owns nothing).
        assert!(
            state == CueState::Idle
                || (state == CueState::Paused && s.control().engine().is_lost(id(n))),
            "cue {n} is {state:?} after the return"
        );
    }
    assert_eq!(s.control().live_voices(), 0);
    assert_eq!(s.control_mut().mixer_voice_count(), 0);
}

#[test]
fn a_start_cues_pre_wait_does_not_fire_after_the_return() {
    let b = wav(&dc(20_000, 0.5));
    let mut start_cue = cue(
        5,
        CueKind::Start {
            target: Some(id(2)),
        },
    );
    start_cue.pre_wait = secs(2000);
    let mut s = device_show(workspace(vec![audio(2, &b, 1), start_cue]));
    s.run(BLOCK * 2);
    start(&mut s, 5);
    s.run(640);
    nothing_starts_after_the_return(s, &[2, 5]);
}

#[test]
fn a_memos_auto_continue_does_not_fire_after_the_return() {
    let b = wav(&dc(20_000, 0.5));
    let mut memo = cue(5, CueKind::Memo);
    memo.continue_mode = ContinueMode::AutoContinue;
    memo.post_wait = secs(2000);
    let mut s = device_show(workspace(vec![memo, audio(2, &b, 1)]));
    s.run(BLOCK * 2);
    start(&mut s, 5);
    s.run(640);
    nothing_starts_after_the_return(s, &[2, 5]);
}

#[test]
fn a_groups_own_auto_continue_does_not_fire_after_its_children_were_lost() {
    let a = wav(&dc(20_000, 0.25));
    let b = wav(&dc(20_000, 0.5));
    let mut group = cue(
        10,
        CueKind::Group {
            mode: GroupMode::Timeline,
            children: vec![audio(1, &a, 0)],
        },
    );
    group.continue_mode = ContinueMode::AutoContinue;
    group.post_wait = secs(2000);
    let mut s = device_show(workspace(vec![group, audio(2, &b, 1)]));
    s.run(BLOCK * 2);
    start(&mut s, 10);
    s.run(640);
    nothing_starts_after_the_return(s, &[1, 2, 10]);
}

#[test]
fn a_groups_pre_wait_does_not_fire_after_the_return() {
    let a = wav(&dc(20_000, 0.25));
    let mut group = cue(
        10,
        CueKind::Group {
            mode: GroupMode::Timeline,
            children: vec![audio(1, &a, 0)],
        },
    );
    group.pre_wait = secs(2000);
    let mut s = device_show(workspace(vec![group]));
    s.run(BLOCK * 2);
    start(&mut s, 10);
    s.run(640);
    nothing_starts_after_the_return(s, &[1, 10]);
}

#[test]
fn a_cue_broken_before_the_loss_does_not_auto_continue_after_the_return() {
    let b = wav(&dc(20_000, 0.5));
    // A missing file: the cue is broken as it starts, its timing kept (an auto-continue).
    let mut c1 = audio(1, &fresh_path("wav"), 0);
    c1.continue_mode = ContinueMode::AutoContinue;
    c1.post_wait = secs(2000);
    let mut s = device_show(workspace(vec![c1, audio(2, &b, 1)]));
    s.run(BLOCK * 2);
    start(&mut s, 1);
    s.run(640);
    assert_eq!(s.control().engine().state(id(1)), CueState::Broken);
    nothing_starts_after_the_return(s, &[2]);
}

// ---- stall watchdog: a stream that stops calling back with no error, device listed ----
//
// Two stages: a `device_stalled` warning after 500 ms of silence
// (nothing stops), a loss after 3 s. The supervisor samples every tick; these tests tick every
// 25 ms of the injected clock, as the real loop's 5 ms is finer still.

const STALL_STEP_MS: u64 = 25;

/// Moves the wall clock by `ms` in 25 ms steps, ticking after each (no settling: the loss
/// itself is what the test looks at).
fn idle_ms(s: &mut DeviceShow, ms: u64) {
    for _ in 0..ms / STALL_STEP_MS {
        s.advance(Duration::from_millis(STALL_STEP_MS));
        s.tick();
    }
}

fn stalled_warning(msgs: &[ServerMessage]) -> Option<bool> {
    workspace_warnings(msgs).map(|w| w.contains(&"device_stalled".to_owned()))
}

#[test]
fn a_silent_stream_is_warned_about_at_500_ms_and_lost_as_stalled_at_3_s() {
    let mut s = running_cue_show();
    s.take_outbound();
    s.fake.stall();
    idle_ms(&mut s, 450);
    assert_eq!(stalled_warning(&s.take_outbound()), None);
    idle_ms(&mut s, 100);
    // The first stage: a warning and a log, and nothing stops.
    let msgs = s.take_outbound();
    assert_eq!(stalled_warning(&msgs), Some(true), "{msgs:?}");
    assert!(
        logs(&msgs, LogLevel::Warn)
            .iter()
            .any(|m| m.contains("no audio callback"))
    );
    assert!(!s.control().device_down());
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    idle_ms(&mut s, 2300);
    assert!(!s.control().device_down(), "lost before the loss limit");
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    s.take_outbound();
    assert!(
        idle_ms_until_down(&mut s, 300, true),
        "the hung stream was not lost"
    );
    let msgs = s.take_outbound();
    // As an unplug: the cue stops at once, a log and the device_lost notice go out.
    assert!(paused_device_lost(&msgs, 1));
    assert!(
        logs(&msgs, LogLevel::Error)
            .iter()
            .any(|m| m.contains("lost") && m.contains("stalled")),
        "{:?}",
        logs(&msgs, LogLevel::Error)
    );
    assert!(notices(&msgs).contains(&"device_lost".to_owned()));
    // The loss replaces the warning.
    assert_eq!(
        workspace_warnings(&msgs),
        Some(vec!["device_lost".to_owned()])
    );
}

#[test]
fn a_600_ms_pause_is_warned_about_and_then_forgotten_and_the_show_goes_on() {
    let mut s = running_cue_show();
    s.take_outbound();
    s.fake.stall();
    idle_ms(&mut s, 600);
    assert_eq!(stalled_warning(&s.take_outbound()), Some(true));
    s.fake.unstall();
    s.run(BLOCK * 2);
    // The warning stays until callbacks have run steadily for one warning limit.
    idle_ms(&mut s, 400);
    assert_ne!(stalled_warning(&s.take_outbound()), Some(false));
    idle_ms(&mut s, 200);
    let msgs = s.take_outbound();
    assert_eq!(stalled_warning(&msgs), Some(false), "the warning stayed");
    assert!(
        logs(&msgs, LogLevel::Info)
            .iter()
            .any(|m| m.contains("callbacks resumed"))
    );
    idle_ms(&mut s, 5000);
    assert!(!s.control().device_down());
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    // A second pause starts from the new last callback.
    s.fake.stall();
    idle_ms(&mut s, 2500);
    s.fake.unstall();
    s.run(BLOCK);
    idle_ms(&mut s, 1000);
    assert!(!s.control().device_down());
}

#[test]
fn callbacks_that_resume_just_before_the_loss_limit_are_not_a_loss() {
    let mut s = running_cue_show();
    s.fake.stall();
    idle_ms(&mut s, 2900);
    s.fake.unstall();
    s.run(BLOCK);
    idle_ms(&mut s, 2000);
    assert!(!s.control().device_down());
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
}

#[test]
fn a_stalled_device_is_reopened_and_a_cue_plays_again_on_go() {
    let mut s = running_cue_show();
    let opens = s.fake.opens();
    s.fake.stall();
    assert!(idle_ms_until_down(&mut s, 3100, true));
    // Still listed, so no hot-plug shortcut: the reopen waits out the backoff (250 ms).
    s.wait_wall(Duration::from_millis(300), 3);
    assert!(!s.control().device_down(), "the stream was not reopened");
    assert_eq!(s.fake.opens(), opens + 1);
    s.run(BLOCK * 2);
    // Voices released; nothing resumed by itself (the cue keeps its paused mark).
    assert_eq!(s.control().live_voices(), 0);
    assert_eq!(s.control().engine().state(id(1)), CueState::Paused);
    let at = start(&mut s, 1);
    s.run(BLOCK * 4);
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    assert!(first_nonzero(&s.channel(0)[at as usize..]).is_some());
}

#[test]
fn a_reopened_stream_gets_one_loss_limit_for_its_first_callback() {
    let mut s = running_cue_show();
    lose(&mut s);
    // The host takes a long while to start calling back after the reopen.
    s.fake.silent_next_open();
    s.fake.plug(listed(DEV, DEV_NAME, 2));
    s.wait_wall(Duration::from_secs(1), 2);
    assert!(!s.control().device_down());
    s.take_outbound();
    idle_ms(&mut s, 1500);
    assert!(!s.control().device_down(), "lost inside the startup grace");
    assert_ne!(stalled_warning(&s.take_outbound()), Some(true));
    s.fake.unstall();
    s.run(BLOCK);
    idle_ms(&mut s, 5000);
    assert!(!s.control().device_down());

    // A stream that never calls back at all is lost once the grace is over.
    let mut s = running_cue_show();
    lose(&mut s);
    s.fake.silent_next_open();
    s.fake.plug(listed(DEV, DEV_NAME, 2));
    s.wait_wall(Duration::from_secs(1), 2);
    s.take_outbound();
    assert!(idle_ms_until_down(&mut s, 3500, true));
    // Lost (stopped at the tick that lost it, before any reopen).
    assert!(
        logs(&s.take_outbound(), LogLevel::Error)
            .iter()
            .any(|m| m.contains("stalled"))
    );
}

#[test]
fn a_process_freeze_is_no_evidence_of_a_stall() {
    // Both threads stop together for 2 s (and 60 s, an hour): the supervisor wakes first, finds
    // the count unchanged, and must not take that for silence it watched.
    for freeze in [2, 60, 3600] {
        let mut s = running_cue_show();
        s.take_outbound();
        s.fake.stall();
        idle_ms(&mut s, 100);
        s.advance(Duration::from_secs(freeze));
        s.tick();
        s.settle_devices();
        assert!(
            !s.control().device_down(),
            "a {freeze} s freeze was a stall"
        );
        assert_eq!(stalled_warning(&s.take_outbound()), None);
        // The window started over at the freeze: a hang is still found, after its own time.
        idle_ms(&mut s, 2900);
        assert!(!s.control().device_down());
        assert!(idle_ms_until_down(&mut s, 300, true));
    }
    // A live stream is untouched by it.
    let mut s = running_cue_show();
    s.advance(Duration::from_secs(3600));
    s.tick();
    s.run(BLOCK);
    idle_ms(&mut s, 1000);
    assert!(!s.control().device_down());
}

#[test]
fn a_sparse_supervisor_cannot_declare_a_stall() {
    // Ticks 200 ms apart (a starved control thread) never add up to a watched silence.
    let mut s = running_cue_show();
    s.fake.stall();
    for _ in 0..40 {
        s.advance(Duration::from_millis(200));
        s.tick();
    }
    assert!(!s.control().device_down());
}

#[test]
fn a_sleep_gives_the_stream_one_loss_limit_to_wake_up() {
    // The monotonic clock stops while the Mac sleeps (macOS `Instant`); the sleep-counting
    // clock does not. The device is quiet for 2 s before and takes its time after the wake.
    let mut s = running_cue_show();
    s.take_outbound();
    s.fake.stall();
    idle_ms(&mut s, 2000);
    s.sleep(Duration::from_secs(60));
    idle_ms(&mut s, 2900);
    assert!(
        !s.control().device_down(),
        "lost inside the post-wake grace"
    );
    assert_eq!(stalled_warning(&s.take_outbound()), Some(true));
    s.fake.unstall();
    s.run(BLOCK);
    idle_ms(&mut s, 5000);
    assert!(!s.control().device_down());

    // A device that never wakes is found after the grace.
    let mut s = running_cue_show();
    s.fake.stall();
    s.sleep(Duration::from_secs(60));
    assert!(idle_ms_until_down(&mut s, 3200, true));
}

#[test]
fn a_stream_that_is_not_up_is_not_watched() {
    // Closing: the host still holds the callback, the mixer is polled for.
    let mut s = running_cue_show();
    s.fake.hold_on_close(true);
    lose(&mut s);
    let opens = s.fake.opens();
    s.take_outbound();
    idle_ms(&mut s, 5000);
    let msgs = s.take_outbound();
    assert!(logs(&msgs, LogLevel::Error).is_empty(), "{msgs:?}");
    assert!(
        logs(&msgs, LogLevel::Warn)
            .iter()
            .all(|m| !m.contains("no audio callback"))
    );
    assert_eq!(s.fake.opens(), opens, "no reopen while the mixer is held");
    s.fake.drop_held();

    // Opening: an open that takes long.
    let mut s = running_cue_show();
    lose(&mut s);
    s.fake.close_gate();
    s.fake.plug(listed(DEV, DEV_NAME, 2));
    idle_ms(&mut s, 1100);
    s.take_outbound();
    idle_ms(&mut s, 5000);
    let msgs = s.take_outbound();
    assert!(logs(&msgs, LogLevel::Error).is_empty(), "{msgs:?}");
    s.fake.open_gate();
    s.settle_devices();
    assert!(!s.control().device_down());
}

#[test]
fn a_device_missing_from_the_list_is_reported_as_unlisted_not_stalled() {
    let mut s = running_cue_show();
    s.take_outbound();
    s.fake.unplug(DEV, false);
    s.wait_wall(Duration::from_secs(2), 2);
    assert!(s.control().device_down());
    let errors = logs(&s.take_outbound(), LogLevel::Error);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("no longer listed"), "{errors:?}");
}

#[test]
fn the_stall_limits_follow_the_buffer_period() {
    use openlab_engine::control::devices::{stall_limit, stall_loss_limit};
    let ms = Duration::from_millis;
    // Small buffers: the floors.
    assert_eq!(stall_limit(Some(64), 48_000), ms(500));
    assert_eq!(stall_limit(None, 48_000), ms(500));
    assert_eq!(stall_limit(Some(512), 0), ms(500));
    assert_eq!(stall_loss_limit(Some(64), 48_000), ms(3000));
    assert_eq!(stall_loss_limit(None, 48_000), ms(3000));
    // Big buffers: 20 and 50 periods (4096 frames at 44.1 kHz is 92.9 ms).
    let warn = stall_limit(Some(4096), 44_100);
    assert!(warn > ms(1850) && warn < ms(1870), "{warn:?}");
    let loss = stall_loss_limit(Some(4096), 44_100);
    assert!(loss > ms(4640) && loss < ms(4650), "{loss:?}");
}

/// Like [`idle_ms`], but stops at the tick that loses the device. The reopen starts 250 ms of
/// the injected clock later, and the device thread answers it in real time: a loop that only
/// looked after the whole span could find the device already back (a race that showed on
/// Windows runners). With `settle`, the device thread's answers are awaited after every tick,
/// the fastest it can be.
fn idle_ms_until_down(s: &mut DeviceShow, ms: u64, settle: bool) -> bool {
    for _ in 0..ms / STALL_STEP_MS {
        s.advance(Duration::from_millis(STALL_STEP_MS));
        s.tick();
        if settle {
            s.wait_devices();
        }
        if s.control().device_down() {
            return true;
        }
    }
    false
}

#[test]
fn a_device_that_stutters_is_lost_as_intermittent_with_bounded_logs() {
    stutter_until_lost(false);
}

#[test]
fn the_stutter_loss_does_not_depend_on_how_fast_the_device_thread_answers() {
    stutter_until_lost(true);
}

fn stutter_until_lost(settle: bool) {
    // A callback every 525 ms: never a 3 s silence, but silent almost all the time.
    let mut s = running_cue_show();
    s.take_outbound();
    let mut msgs = Vec::new();
    let mut rounds = 0;
    while !s.control().device_down() && rounds < 60 {
        s.fake.stall();
        if idle_ms_until_down(&mut s, 525, settle) {
            break;
        }
        s.fake.unstall();
        s.run(BLOCK);
        msgs.extend(s.take_outbound());
        rounds += 1;
    }
    msgs.extend(s.take_outbound());
    assert!(
        s.control().device_down(),
        "a stuttering device was never lost"
    );
    assert!(rounds <= 20, "lost only after {rounds} stutters (~10 s)");
    let warns = logs(&msgs, LogLevel::Warn)
        .into_iter()
        .filter(|m| m.contains("no audio callback"))
        .count();
    assert!(warns <= 1, "{warns} stall warnings logged");
    assert!(
        logs(&msgs, LogLevel::Info)
            .iter()
            .all(|m| !m.contains("callbacks resumed")),
        "the warning flickered"
    );
    let broadcasts = msgs
        .iter()
        .filter(|m| matches!(m, ServerMessage::Warnings { .. }))
        .count();
    assert!(broadcasts <= 3, "{broadcasts} warnings broadcasts");
    assert!(
        logs(&msgs, LogLevel::Error)
            .iter()
            .any(|m| m.contains("intermittent")),
        "{:?}",
        logs(&msgs, LogLevel::Error)
    );
}

/// The frames per callback at the test's [`RATE`] with the period of `frames` at 48 kHz.
fn frames_like_48k(frames: u32) -> u32 {
    frames * RATE / 48_000
}

/// A show playing a cue on a stream reopened with `frames` per callback, after `setup`.
fn playing_on_block_with(frames: u32, setup: impl FnOnce(&Fake)) -> DeviceShow {
    let long = wav(&dc(400_000, 0.25));
    let mut s = device_show(workspace(vec![audio(1, &long, 0)]));
    s.fake.set_block(frames);
    setup(&s.fake);
    lose(&mut s);
    s.wait_wall(Duration::from_secs(2), 4);
    replug(&mut s);
    start(&mut s, 1);
    s.take_outbound();
    s
}

fn playing_on_block(frames: u32) -> DeviceShow {
    playing_on_block_with(frames, |_| {})
}

/// `span` of a stream making the callbacks `next` asks for (frames, then the gap to the next
/// one), the loop ticking every [`MAX_WAIT`] in between, as a real device does and
/// `DeviceShow::advance` does not. Stops at a loss.
fn callbacks_by(
    s: &mut DeviceShow,
    span: Duration,
    mut next: impl FnMut() -> (u32, Duration),
) -> Vec<ServerMessage> {
    let mut msgs = Vec::new();
    let mut elapsed = Duration::ZERO;
    let (mut frames, mut gap) = next();
    let mut due = gap;
    while elapsed < span && !s.control().device_down() {
        s.advance_between_callbacks(MAX_WAIT);
        elapsed += MAX_WAIT;
        let mut called = false;
        while due <= elapsed && !s.control().device_down() {
            s.fake.set_block(frames);
            s.step();
            called = true;
            (frames, gap) = next();
            due += gap;
        }
        if !called {
            s.tick();
        }
        s.wait_list();
        msgs.extend(s.take_outbound());
    }
    msgs
}

/// `span` of callbacks of the stream's own size every `period`.
fn callbacks_every(s: &mut DeviceShow, period: Duration, span: Duration) -> Vec<ServerMessage> {
    let frames = s.fake.block();
    callbacks_by(s, span, || (frames, period))
}

fn frames_period(frames: u32) -> Duration {
    Duration::from_secs_f64(f64::from(frames) / f64::from(RATE))
}

/// No loss, no warning and the cue not broken after `span` of steady callbacks.
fn assert_healthy(s: &mut DeviceShow, frames: u32, span: Duration) {
    let msgs = callbacks_every(s, frames_period(frames), span);
    assert!(
        !s.control().device_down(),
        "{frames} frames: {:?}",
        logs(&msgs, LogLevel::Error)
    );
    assert_ne!(stalled_warning(&msgs), Some(true), "{frames} frames warned");
    assert!(
        !broken_device_lost(&msgs, 1),
        "{frames} frames: the cue broke"
    );
}

fn lost_as_intermittent(msgs: &[ServerMessage]) -> bool {
    logs(msgs, LogLevel::Error)
        .iter()
        .any(|m| m.contains("intermittent"))
}

#[test]
fn a_healthy_stream_at_256_1024_and_4096_frames_is_never_lost_or_warned() {
    for frames in [256, 1024, 4096].map(frames_like_48k) {
        let mut s = playing_on_block(frames);
        assert_healthy(&mut s, frames, Duration::from_secs(25));
    }
}

#[test]
fn a_steady_stream_with_jitter_stays_up() {
    // The same audio, callbacks arriving at half and one and a half periods in turn.
    let frames = frames_like_48k(1024);
    let period = frames_period(frames);
    let mut s = playing_on_block(frames);
    let mut made = 0;
    let msgs = callbacks_by(&mut s, Duration::from_secs(25), || {
        made += 1;
        let gap = if made % 2 == 0 {
            period / 2
        } else {
            period * 3 / 2
        };
        (frames, gap)
    });
    assert!(
        !s.control().device_down(),
        "{:?}",
        logs(&msgs, LogLevel::Error)
    );
    assert_ne!(stalled_warning(&msgs), Some(true));
}

#[test]
fn a_stuttering_device_with_a_large_buffer_is_lost_as_intermittent() {
    // 85 ms buffers (4096 at 48 kHz), a callback every 525 ms: never a silence near the warning
    // limit of 1.7 s, but a sixth of the audio.
    let mut s = playing_on_block(frames_like_48k(4096));
    let msgs = callbacks_every(&mut s, Duration::from_millis(525), Duration::from_secs(11));
    assert!(
        s.control().device_down(),
        "a stuttering device was not lost"
    );
    assert!(lost_as_intermittent(&msgs), "{msgs:?}");
}

#[test]
fn a_stream_of_empty_callbacks_is_lost_as_intermittent() {
    // Calling back is not enough: no frames, no audio. Each callback holds off the two stages.
    let mut s = playing_on_block(frames_like_48k(1024));
    let msgs = callbacks_by(&mut s, Duration::from_secs(11), || {
        (0, Duration::from_millis(25))
    });
    assert!(s.control().device_down(), "empty callbacks stayed up");
    assert!(lost_as_intermittent(&msgs), "{msgs:?}");
}

#[test]
fn a_stream_of_no_known_rate_is_judged_by_the_two_stages_only() {
    let frames = frames_like_48k(1024);
    let mut s = playing_on_block_with(frames, |fake| fake.report_rate(0));
    // Its delivered audio cannot be judged: a stutter stays up, with its warning limit of 500 ms.
    let msgs = callbacks_every(&mut s, Duration::from_millis(400), Duration::from_secs(25));
    assert!(
        !s.control().device_down(),
        "{:?}",
        logs(&msgs, LogLevel::Error)
    );
    // A hang is still found at the 3 s floor.
    s.step();
    s.fake.stall();
    idle_ms(&mut s, 2900);
    assert!(!s.control().device_down());
    idle_ms(&mut s, 200);
    assert!(s.control().device_down(), "a hung stream was not lost");
}

#[test]
fn a_true_stall_of_a_large_buffer_stream_takes_the_two_stages() {
    let frames = frames_like_48k(4096);
    let mut s = playing_on_block(frames);
    assert_healthy(&mut s, frames, Duration::from_secs(10));
    s.fake.stall();
    // Warned at 20 periods (1.7 s), lost at 50 (4.26 s).
    idle_ms(&mut s, 1600);
    assert_eq!(stalled_warning(&s.take_outbound()), None);
    idle_ms(&mut s, 200);
    assert_eq!(stalled_warning(&s.take_outbound()), Some(true));
    idle_ms(&mut s, 2300);
    assert!(!s.control().device_down(), "lost before the loss limit");
    idle_ms(&mut s, 300);
    assert!(s.control().device_down(), "a stalled stream was not lost");
    s.settle_devices();
    let msgs = s.take_outbound();
    assert!(!lost_as_intermittent(&msgs), "{msgs:?}");
    assert!(
        logs(&msgs, LogLevel::Error)
            .iter()
            .any(|m| m.contains("stalled: ")),
        "{msgs:?}"
    );
}

#[test]
fn a_sleep_in_a_large_buffer_stream_starts_its_silence_over() {
    let frames = frames_like_48k(4096);
    let mut s = playing_on_block(frames);
    assert_healthy(&mut s, frames, Duration::from_secs(5));
    s.fake.stall();
    idle_ms(&mut s, 3000);
    s.sleep(Duration::from_secs(60));
    // Silent for 7.2 s in all, past the 4.26 s loss limit, but only 4.2 s of it after the wake.
    idle_ms(&mut s, 4200);
    assert!(
        !s.control().device_down(),
        "lost inside the post-wake grace"
    );
    s.fake.unstall();
    assert_healthy(&mut s, frames, Duration::from_secs(10));
}

#[test]
fn a_stream_delivering_all_its_audio_in_bursts_stays_up() {
    // Eight 256-frame callbacks at once, then the eight periods they cover.
    let frames = frames_like_48k(256);
    let period = frames_period(frames);
    let mut s = playing_on_block(frames);
    let mut made = 0;
    let msgs = callbacks_by(&mut s, Duration::from_secs(25), || {
        made += 1;
        let gap = if made % 8 == 0 {
            period * 8
        } else {
            Duration::ZERO
        };
        (frames, gap)
    });
    assert!(
        !s.control().device_down(),
        "{:?}",
        logs(&msgs, LogLevel::Error)
    );
}

#[test]
fn a_stream_delivering_all_its_audio_in_alternating_sizes_stays_up() {
    let small = frames_like_48k(256);
    let big = frames_like_48k(2048);
    for reported in [None, Some(small)] {
        let mut s = playing_on_block_with(small, |fake| fake.report_block(reported));
        let mut made = 0;
        let msgs = callbacks_by(&mut s, Duration::from_secs(25), || {
            made += 1;
            let frames = if made % 2 == 0 { big } else { small };
            (frames, frames_period(frames))
        });
        assert!(
            !s.control().device_down(),
            "reported {reported:?}: {:?}",
            logs(&msgs, LogLevel::Error)
        );
    }
}

#[test]
fn a_stream_delivering_half_its_audio_or_less_is_lost() {
    for frames in [256, 1024].map(frames_like_48k) {
        // A callback every 2.2 periods: 45% of the audio.
        let gap = frames_period(frames) * 22 / 10;
        let mut s = playing_on_block(frames);
        let msgs = callbacks_every(&mut s, gap, Duration::from_secs(11));
        assert!(
            s.control().device_down(),
            "{frames} frames at 45% stayed up"
        );
        assert!(lost_as_intermittent(&msgs), "{msgs:?}");
    }
}

#[test]
fn a_stream_delivering_more_than_half_its_audio_stays_up() {
    for frames in [256, 1024].map(frames_like_48k) {
        // A callback every 1.6 periods: 62% of the audio.
        let gap = frames_period(frames) * 16 / 10;
        let mut s = playing_on_block(frames);
        let msgs = callbacks_every(&mut s, gap, Duration::from_secs(25));
        assert!(
            !s.control().device_down(),
            "{frames} frames at 62%: {:?}",
            logs(&msgs, LogLevel::Error)
        );
    }
}

#[test]
fn a_stream_delivering_little_with_an_oversized_report_is_lost() {
    // Reports 4096-frame buffers, makes 256 frames every 150 ms: 3.5% of the audio.
    let frames = frames_like_48k(256);
    let mut s = playing_on_block_with(frames, |fake| {
        fake.report_block(Some(frames_like_48k(4096)));
    });
    let msgs = callbacks_every(&mut s, Duration::from_millis(150), Duration::from_secs(11));
    assert!(s.control().device_down(), "stayed up at 3.5%");
    assert!(lost_as_intermittent(&msgs), "{msgs:?}");
}

#[test]
fn repeated_short_stalls_are_logged_at_most_every_10_s_with_a_count() {
    // 600 ms stalls every 2.1 s: warned each time, never lost (29% silent).
    let mut s = running_cue_show();
    s.take_outbound();
    let mut msgs = Vec::new();
    for _ in 0..6 {
        s.fake.stall();
        idle_ms(&mut s, 600);
        s.fake.unstall();
        s.run(BLOCK);
        idle_ms(&mut s, 1500);
        msgs.extend(s.take_outbound());
    }
    assert!(!s.control().device_down());
    let warns: Vec<String> = logs(&msgs, LogLevel::Warn)
        .into_iter()
        .filter(|m| m.contains("no audio callback"))
        .collect();
    assert_eq!(warns.len(), 2, "{warns:?}");
    assert!(warns[1].contains("4 more stall(s)"), "{warns:?}");
}

#[test]
fn a_stage_needs_ten_samples_of_watched_silence() {
    // Ticks 100 ms apart (the longest allowed gap): 500 ms of silence is only five samples.
    let mut s = running_cue_show();
    s.take_outbound();
    s.fake.stall();
    for _ in 0..9 {
        s.advance(Duration::from_millis(100));
        s.tick();
    }
    assert_eq!(stalled_warning(&s.take_outbound()), None);
    s.advance(Duration::from_millis(100));
    s.tick();
    assert_eq!(stalled_warning(&s.take_outbound()), Some(true));
}

/// A patch opened on a device with fewer outputs than it has is mapped on the outputs the
/// device has, with no warning; the entry is kept when the device is lost and back (an entry is
/// never overwritten by the default mapping).
#[test]
fn a_patch_on_a_small_device_is_mapped_on_its_outputs_and_kept_across_a_loss() {
    let ws = workspace(vec![]);
    let mut c = device_config(&ws);
    c.machine = MachineSettings::default();
    let mut s = DeviceShow::new(ws.clone(), vec![listed(DEV, DEV_NAME, 1)], c);
    let msgs = s.take_outbound();
    assert!(logs(&msgs, LogLevel::Warn).is_empty());
    let mapped = |s: &DeviceShow| {
        s.control()
            .machine()
            .patch_mapping(ws.id, patch_id())
            .cloned()
            .map(|m| (m.device, m.channels))
    };
    assert_eq!(mapped(&s), Some((DEV_NAME.to_owned(), vec![0])));
    lose(&mut s);
    replug(&mut s);
    assert_eq!(mapped(&s), Some((DEV_NAME.to_owned(), vec![0])));
}

#[test]
fn a_workspace_created_during_an_outage_is_mapped_when_the_device_is_back() {
    let mut s = device_show(workspace(vec![]));
    lose(&mut s);
    let id = s.request_id();
    s.send(ClientMessage::Request {
        id,
        request: Request::New {},
    });
    // Another doc opens beside the first, built on the document worker.
    crate::show::spin::spin_until("the new doc to be installed", || {
        s.tick();
        s.control().doc_ids().len() >= 2
    });
    let docs = s.control().doc_ids();
    let new_ws = s.control().doc(docs[1]).unwrap().workspace().clone();
    let patch = new_ws.patches[0].id;
    // No device to map to: the patch stays unmapped, nothing is guessed.
    assert_eq!(s.control().machine().patch_mapping(new_ws.id, patch), None);
    s.take_outbound();
    replug(&mut s);
    let m = s
        .control()
        .machine()
        .patch_mapping(new_ws.id, patch)
        .cloned()
        .expect("mapped when the device came back");
    assert_eq!((m.device.as_str(), m.channels), (DEV_NAME, vec![0, 1]));
    assert!(
        logs(&s.take_outbound(), LogLevel::Info)
            .iter()
            .any(|l| l.contains("mapped patch 'Patch 1' to Fake 1 outputs 1\u{2013}2"))
    );
}

// AUD-02: a device per output patch, several devices at once.

const DEV2: &str = "fake:2";
const DEV2_NAME: &str = "Fake 2";

fn patch_b() -> openlab_core::PatchId {
    openlab_core::PatchId(uuid::Uuid::from_u128(902))
}

/// Audio cue `n` on patch B.
fn audio_b(n: u128, path: &std::path::Path, out: u32) -> openlab_core::Cue {
    let mut c = audio(n, path, out);
    if let CueKind::Audio { patch, .. } = &mut c.kind {
        *patch = patch_b();
    }
    c
}

/// `cues` with a second patch, B, mapped to device `b`: patch A (`patch_id`) plays on
/// "Fake 1", the primary.
fn two_patch_show(
    cues: Vec<openlab_core::Cue>,
    b: &str,
    devices: Vec<devices::ListedDevice>,
) -> DeviceShow {
    two_patch_show_with(cues, b, devices, |_| {})
}

/// [`two_patch_show`] with the config changed by `tweak` first.
fn two_patch_show_with(
    cues: Vec<openlab_core::Cue>,
    b: &str,
    devices: Vec<devices::ListedDevice>,
    tweak: impl FnOnce(&mut ControlConfig),
) -> DeviceShow {
    let mut ws = workspace(cues);
    let mut patch = ws.patches[0].clone();
    patch.id = patch_b();
    patch.name = "B".into();
    ws.patches.push(patch);
    let mut c = device_config(&ws);
    c.mixer_patches = 4;
    c.machine
        .set_patch_mapping(
            ws.id,
            patch_b(),
            PatchMapping {
                device: b.into(),
                channels: vec![0, 1],
                system: false,
            },
        )
        .unwrap();
    tweak(&mut c);
    DeviceShow::new(ws, devices, c)
}

/// Patch A on "Fake 1", patch B on "Fake 2", both open and playing.
fn two_device_show(cues: Vec<openlab_core::Cue>) -> DeviceShow {
    let mut s = two_patch_show(
        cues,
        DEV2_NAME,
        vec![listed(DEV, DEV_NAME, 2), listed(DEV2, DEV2_NAME, 2)],
    );
    s.wait_output(DEV2_NAME);
    s.take_outbound();
    s
}

/// The values heard on `samples`, without silence.
fn heard(samples: &[f32]) -> Vec<f32> {
    let mut v: Vec<f32> = samples.iter().copied().filter(|&x| x != 0.0).collect();
    v.dedup();
    v
}

/// Unplugs device `id`, reported lost, and lets the loop see it.
fn unplug(s: &mut DeviceShow, id: &str) {
    s.fake.unplug(id, true);
    s.settle_devices();
}

/// The last workspace warnings list sent, as (code, message) of each notice.
fn notice_texts(msgs: &[ServerMessage]) -> Option<Vec<(String, String)>> {
    msgs.iter()
        .filter_map(|m| match m {
            ServerMessage::Warnings { warnings } => Some(
                warnings
                    .iter()
                    .filter_map(|w| match w {
                        WireWarning::Notice { code, message, .. } => {
                            Some((code.clone(), message.clone()))
                        }
                        _ => None,
                    })
                    .collect(),
            ),
            _ => None,
        })
        .next_back()
}

/// QLab 5 warns about a patch whose device is missing (manual p. 191): a patch on a device that
/// is unplugged is a workspace warning naming both, gone again once the device is back.
#[test]
fn a_patch_whose_device_is_unplugged_is_warned_about_until_it_is_back() {
    let mut s = two_device_show(vec![]);
    unplug(&mut s, DEV2);
    s.wait_wall(Duration::from_secs(1), 1);
    let msgs = s.take_outbound();
    let texts = notice_texts(&msgs).unwrap();
    let missing: Vec<&String> = texts
        .iter()
        .filter(|(code, _)| code == "patch_device_missing")
        .map(|(_, m)| m)
        .collect();
    assert_eq!(missing.len(), 1, "{texts:?}");
    assert_eq!(
        last_warnings(&msgs)
            .into_iter()
            .find_map(|w| match w {
                WireWarning::Notice { code, device, .. } if code == "patch_device_missing" =>
                    device,
                _ => None,
            })
            .as_deref(),
        Some(DEV2_NAME)
    );
    assert!(
        missing[0].contains("\"B\"") && missing[0].contains(DEV2_NAME),
        "{}",
        missing[0]
    );
    s.fake.plug(listed(DEV2, DEV2_NAME, 2));
    s.wait_wall(Duration::from_secs(1), 2);
    s.wait_output(DEV2_NAME);
    s.tick();
    let after = notice_texts(&s.take_outbound()).expect("the list is sent again");
    assert!(
        !after.iter().any(|(code, _)| code == "patch_device_missing"),
        "{after:?}"
    );
}

/// A listed device that offers no rate openLab can play at (below 8 kHz): the patch mapped to it
/// is warned about with that reason (not as "not connected"), and its resolution says why.
#[test]
fn a_patch_on_a_listed_device_with_no_usable_rate_says_why() {
    let other = listed_at(DEV2, DEV2_NAME, 2, 4_000);
    let mut s = two_patch_show(vec![], DEV2_NAME, vec![listed(DEV, DEV_NAME, 2), other]);
    s.settle_devices();
    s.tick();
    let texts = notice_texts(&s.take_outbound()).unwrap();
    let unusable: Vec<&String> = texts
        .iter()
        .filter(|(code, _)| code == "patch_device_unusable")
        .map(|(_, m)| m)
        .collect();
    assert_eq!(unusable.len(), 1, "{texts:?}");
    assert!(
        unusable[0].contains("\"B\"") && unusable[0].contains("Hz"),
        "{}",
        unusable[0]
    );
    assert!(!texts.iter().any(|(code, _)| code == "patch_device_missing"));
    let reason = resolutions(&mut s).into_iter().find_map(|r| match r {
        Some(WireResolution::DeviceMissing { reason, .. }) => reason,
        _ => None,
    });
    assert!(reason.is_some_and(|r| r.contains("Hz")));
}

/// A listed device whose first open fails (another program holds it): the patch is warned
/// about with the reason; once it opens, the warning goes.
#[test]
fn a_patch_on_a_listed_device_whose_first_open_fails_says_why() {
    let mut s = two_patch_show(vec![], DEV2_NAME, vec![listed(DEV, DEV_NAME, 2)]);
    s.settle_devices();
    s.take_outbound();
    s.fake.refuse_opens(Some(DEV2));
    s.fake.plug(listed(DEV2, DEV2_NAME, 2));
    s.wait_wall(Duration::from_secs(1), 2);
    s.tick();
    let texts = notice_texts(&s.take_outbound()).unwrap();
    assert!(
        texts.iter().any(|(code, m)| code == "patch_device_unusable"
            && m.contains("\"B\"")
            && m.contains("in use by another program")),
        "{texts:?}"
    );
    s.fake.refuse_opens(None);
    s.wait_wall(Duration::from_secs(5), 10);
    s.wait_output(DEV2_NAME);
    s.tick();
    let after = notice_texts(&s.take_outbound()).expect("the list is sent again");
    assert!(
        !after
            .iter()
            .any(|(code, _)| code.starts_with("patch_device")),
        "{after:?}"
    );
}

/// A device whose first open failed, then opened by its own reopen and played, is not reported
/// with the old reason when it is unplugged later: it is not connected.
#[test]
fn a_device_that_recovered_from_a_failed_first_open_is_reported_missing_when_unplugged() {
    let mut s = two_patch_show(vec![], DEV2_NAME, vec![listed(DEV, DEV_NAME, 2)]);
    s.settle_devices();
    s.take_outbound();
    s.fake.refuse_opens(Some(DEV2));
    s.fake.plug(listed(DEV2, DEV2_NAME, 2));
    s.wait_wall(Duration::from_secs(1), 2);
    s.tick();
    assert!(
        notice_texts(&s.take_outbound())
            .unwrap()
            .iter()
            .any(|(code, _)| code == "patch_device_unusable")
    );
    s.fake.refuse_opens(None);
    s.wait_wall(Duration::from_secs(5), 10);
    s.wait_output(DEV2_NAME);
    s.tick();
    s.take_outbound();
    unplug(&mut s, DEV2);
    s.wait_wall(Duration::from_secs(1), 1);
    s.tick();
    let texts = notice_texts(&s.take_outbound()).unwrap();
    assert!(
        texts.iter().any(|(code, _)| code == "patch_device_missing"),
        "{texts:?}"
    );
    assert!(
        !texts
            .iter()
            .any(|(code, _)| code == "patch_device_unusable"),
        "{texts:?}"
    );
    let reason = resolutions(&mut s).into_iter().find_map(|r| match r {
        Some(WireResolution::DeviceMissing { reason, .. }) => Some(reason),
        _ => None,
    });
    assert_eq!(reason, Some(None), "no stale reason");
}

/// A device that cannot be used, then unplugged, is not connected; plugged back and usable, it
/// opens and the patch has no warning at all.
#[test]
fn an_unusable_device_that_leaves_the_list_is_missing_and_plays_when_back() {
    let mut s = two_patch_show(vec![], DEV2_NAME, vec![listed(DEV, DEV_NAME, 2)]);
    s.settle_devices();
    s.take_outbound();
    s.fake.refuse_opens(Some(DEV2));
    s.fake.plug(listed(DEV2, DEV2_NAME, 2));
    s.wait_wall(Duration::from_secs(1), 2);
    s.tick();
    s.take_outbound();
    unplug(&mut s, DEV2);
    s.wait_wall(Duration::from_secs(1), 1);
    s.tick();
    let texts = notice_texts(&s.take_outbound()).unwrap();
    assert!(
        texts.iter().any(|(code, _)| code == "patch_device_missing")
            && !texts
                .iter()
                .any(|(code, _)| code == "patch_device_unusable"),
        "{texts:?}"
    );
    s.fake.refuse_opens(None);
    s.fake.plug(listed(DEV2, DEV2_NAME, 2));
    s.wait_wall(Duration::from_secs(5), 10);
    s.wait_output(DEV2_NAME);
    s.tick();
    let after = notice_texts(&s.take_outbound()).expect("the list is sent again");
    assert!(
        !after
            .iter()
            .any(|(code, _)| code.starts_with("patch_device")),
        "{after:?}"
    );
}

/// A patch the operator set to no device that a cue plays on carries its own warning (QLab 5
/// manual p. 191).
#[test]
fn a_patch_set_to_no_device_is_warned_about() {
    let cues = vec![audio_b(1, &wav(&dc(100, 0.5)), 0)];
    let mut s = two_patch_show_with(cues, DEV_NAME, vec![listed(DEV, DEV_NAME, 2)], |c| {
        let ws = workspace(vec![]);
        c.machine
            .set_patch_mapping(ws.id, patch_b(), PatchMapping::none())
            .unwrap();
    });
    let texts = notice_texts(&s.take_outbound()).unwrap();
    assert_eq!(
        texts
            .iter()
            .filter(|(code, m)| code == "patch_no_device" && m.contains("\"B\""))
            .count(),
        1,
        "{texts:?}"
    );
    assert!(
        !texts.iter().any(|(code, _)| code == "patch_device_missing"),
        "{texts:?}"
    );
}

/// One of two devices goes while a cue plays on each: the other device keeps sample time and
/// the meter reads running, and the lost device's cue gets no meter entry, while it is away or
/// after it returns; the cue on the device that stayed is metered throughout.
#[test]
fn a_cue_on_a_lost_second_device_gets_no_meter_while_the_other_plays() {
    let long = wav(&dc(200_000, 0.5));
    let mut s = two_device_show(vec![audio(1, &long, 0), audio_b(2, &long, 0)]);
    start(&mut s, 1);
    start(&mut s, 2);
    s.run(BLOCK * 9);
    assert!(
        !cue_meters(&s.take_outbound(), 2).is_empty(),
        "cue 2 was metered"
    );
    unplug(&mut s, DEV2);
    for _ in 0..40 {
        s.play(BLOCK);
    }
    let away = s.take_outbound();
    assert_eq!(cue_meters(&away, 2), Vec::<Vec<f32>>::new());
    assert_eq!(lit_patch_meters(&away, patch_b()), Vec::<Vec<f32>>::new());
    assert!(cue_meters(&away, 1).len() > 4, "cue 1 is still metered");
    s.fake.plug(listed(DEV2, DEV2_NAME, 2));
    s.wait_wall(Duration::from_secs(1), 2);
    s.wait_output(DEV2_NAME);
    s.run(u64::from(RATE));
    let back = s.take_outbound();
    assert_eq!(cue_meters(&back, 2), Vec::<Vec<f32>>::new());
    assert_eq!(lit_patch_meters(&back, patch_b()), Vec::<Vec<f32>>::new());
}

/// Checklist: two patches on two devices, one Audio cue on each, GO both: each device carries
/// only its own patch's cue.
#[test]
fn two_patches_on_two_devices_each_play_only_their_own_cue() {
    let a = wav(&dc(2000, 0.25));
    let b = wav(&dc(2000, 0.5));
    let mut s = two_device_show(vec![audio(1, &a, 0), audio_b(2, &b, 0)]);
    assert_eq!(
        s.control().outputs(),
        vec![(DEV_NAME.to_owned(), true), (DEV2_NAME.to_owned(), true)]
    );
    start(&mut s, 1);
    start(&mut s, 2);
    s.run(1000);
    // Each patch's meters come from its own device's mixer.
    let peak = |msgs: &[ServerMessage], patch| {
        msgs.iter()
            .filter_map(|m| match m {
                ServerMessage::Meters { outputs, .. } => outputs
                    .iter()
                    .find(|o| o.patch == patch)
                    .map(|o| o.peaks[0]),
                _ => None,
            })
            .fold(0.0f32, f32::max)
    };
    let msgs = s.take_outbound();
    assert!((peak(&msgs, patch_id()) - 0.25).abs() < 1e-3, "{msgs:?}");
    assert!((peak(&msgs, patch_b()) - 0.5).abs() < 1e-3, "{msgs:?}");
    s.run(2000);
    assert_eq!(heard(&s.device_channel(DEV, 0)), vec![0.25]);
    assert_eq!(heard(&s.device_channel(DEV2, 0)), vec![0.5]);
    assert_eq!(heard(&s.device_channel(DEV, 1)), Vec::<f32>::new());
    assert_eq!(heard(&s.device_channel(DEV2, 1)), Vec::<f32>::new());
    // Both played their whole file, and both cues ended.
    let played = |id| {
        s.device_channel(id, 0)
            .iter()
            .filter(|&&x| x != 0.0)
            .count()
    };
    assert_eq!((played(DEV), played(DEV2)), (2000, 2000));
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    assert_eq!(s.control().engine().state(id(2)), CueState::Idle);
    assert_eq!(s.control().live_voices(), 0);
}

/// A new workspace has exactly one output patch, "Patch 1", mapped on this machine to the
/// primary device's first outputs, however many output devices are connected (a device with no
/// outputs or a second one gets no patch of its own).
#[test]
fn a_new_workspace_has_one_output_patch_whatever_devices_are_connected() {
    let ws = workspace(vec![]);
    let mut c = device_config(&ws);
    let env = crate::common::env();
    c.app_dirs = Some(env.dirs.clone());
    c.mixer_patches = 3;
    let devices = vec![
        listed(DEV, DEV_NAME, 2),
        listed("fake:0", "No Outputs", 0),
        listed(DEV2, DEV2_NAME, 4),
    ];
    let mut s = DeviceShow::new(ws, devices, c);
    let doc = s.new_doc();
    let new = s.control().doc(doc).unwrap().workspace().clone();
    let names: Vec<&str> = new.patches.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Patch 1"]);
    let m = s
        .control()
        .machine()
        .patch_mapping(new.id, new.patches[0].id)
        .map(|m| (m.device.clone(), m.channels.clone()));
    assert_eq!(m, Some((DEV_NAME.to_owned(), vec![0, 1])));

    // Really saved, then opened on a machine with only the primary device: no patch is reported
    // missing, because nothing was ever mapped to the devices that are gone.
    let first = s.control().doc_ids()[0];
    s.control_mut().close_doc(first, true).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("new.openlab");
    let id = s.request_id();
    let target = file.clone();
    s.send_until(
        ClientMessage::Request {
            id,
            request: Request::SaveAs {
                path: file.to_string_lossy().into_owned(),
            },
        },
        move |_| target.exists(),
    );
    assert!(file.exists(), "the file was written");
    let machine = s.control().machine().clone();
    let other = workspace(vec![]);
    let mut c = device_config(&other);
    c.app_dirs = Some(env.dirs.clone());
    c.machine = machine;
    let mut again = DeviceShow::new(other, vec![listed(DEV, DEV_NAME, 2)], c);
    let doc = again.open_file(&file);
    again.wait_wall(Duration::from_secs(1), 1);
    let msgs = again.take_outbound();
    let notices = notice_texts(&msgs).unwrap_or_default();
    assert!(
        !notices
            .iter()
            .any(|(code, _)| code == "patch_device_missing"),
        "{notices:?}"
    );
    let patches = again.control().doc(doc).unwrap().workspace().patches.len();
    assert_eq!(patches, 1, "the saved file holds one patch");
}

/// A start on the second device lands on the sample the master date translates to there:
/// `local = local_rendered + (at - master_rendered)`, from the counters when it was sent.
#[test]
fn a_start_on_the_second_device_lands_on_the_translated_sample() {
    let b = wav(&dc(500, 0.5));
    // The second device comes later than the first: their counters differ.
    let mut s = two_patch_show(
        vec![audio_b(2, &b, 0)],
        DEV2_NAME,
        vec![listed(DEV, DEV_NAME, 2)],
    );
    s.run(BLOCK * 7);
    s.fake.plug(listed(DEV2, DEV2_NAME, 2));
    s.wait_wall(Duration::from_secs(1), 1);
    s.wait_output(DEV2_NAME);
    s.run(BLOCK * 3);
    let master = s.control().rendered();
    let local = s
        .control()
        .output_mixer(DEV2_NAME)
        .unwrap()
        .samples_rendered();
    assert!(master > local, "the second device opened after the first");
    let at = start(&mut s, 2);
    s.run(1000);
    let expected = (local + (at - master)) as usize;
    assert_eq!(first_nonzero(&s.device_channel(DEV2, 0)), Some(expected));
    // The primary heard nothing.
    assert_eq!(first_nonzero(&s.device_channel(DEV, 0)), None);
}

/// A device that offers no rate openLab can play at is not opened (once warned): the patches
/// mapped to it are `device_missing` and their cues broken; the other patches play.
#[test]
fn a_device_with_no_usable_rate_is_not_opened() {
    let a = wav(&dc(200, 0.25));
    let b = wav(&dc(200, 0.5));
    let other = listed_at(DEV2, DEV2_NAME, 2, 4_000);
    let mut s = two_patch_show(
        vec![audio(1, &a, 0), audio_b(2, &b, 0)],
        DEV2_NAME,
        vec![listed(DEV, DEV_NAME, 2), other],
    );
    s.settle_devices();
    let msgs = s.take_outbound();
    assert_eq!(s.control().outputs(), vec![(DEV_NAME.to_owned(), true)]);
    let warned: Vec<String> = logs(&msgs, LogLevel::Warn)
        .into_iter()
        .filter(|m| m.contains("no sample rate openLab can play at"))
        .collect();
    assert_eq!(warned.len(), 1, "{warned:?}");
    assert!(warned[0].contains(DEV2_NAME), "{warned:?}");
    assert_eq!(s.control().engine().state(id(2)), CueState::Broken);
    // Warned once, however often it is looked at again.
    s.wait_wall(Duration::from_secs(1), 2);
    assert!(
        !logs(&s.take_outbound(), LogLevel::Warn)
            .iter()
            .any(|m| m.contains("no sample rate"))
    );
    start(&mut s, 1);
    s.run(400);
    assert_eq!(heard(&s.channel(0)), vec![0.25]);
}

/// Checklist: a patch whose device is absent on this machine: the show loads, the patch is
/// `device_missing`, its cues are broken with that reason, and cues on other patches play.
#[test]
fn a_patch_mapped_to_an_absent_device_is_missing_and_the_other_patches_play() {
    let a = wav(&dc(200, 0.25));
    let b = wav(&dc(200, 0.5));
    let mut s = two_patch_show(
        vec![audio(1, &a, 0), audio_b(2, &b, 0)],
        "Not Here",
        vec![listed(DEV, DEV_NAME, 2)],
    );
    s.settle_devices();
    assert_eq!(s.control().engine().state(id(2)), CueState::Broken);
    let problems = s.control().engine().broken_cues();
    let p2 = &problems.iter().find(|(c, _)| *c == id(2)).unwrap().1;
    assert!(
        p2.iter()
            .any(|p| matches!(p, openlab_core::CueProblem::DeviceUnavailable { .. })),
        "{p2:?}"
    );
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    // The patch itself carries the warning (the pre-show check lists it with its reason).
    let rid = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id: rid,
        request: Request::PreshowCheck,
    });
    let report = replies
        .iter()
        .find_map(|m| match m {
            ServerMessage::Result {
                outcome:
                    Outcome::Ok(openlab_core::protocol::OkBody {
                        data: Some(ResultData::PreshowCheck(report)),
                        ..
                    }),
                ..
            } => Some((**report).clone()),
            _ => None,
        })
        .unwrap();
    let listed: Vec<_> = report.patches.iter().map(|p| p.patch).collect();
    assert_eq!(listed, vec![patch_b()], "{report:?}");
    assert!(
        matches!(
            report.patches[0].resolution,
            Some(openlab_core::protocol::WireResolution::DeviceMissing { .. })
        ),
        "{report:?}"
    );
    assert!(!report.ok);
    start(&mut s, 1);
    s.run(400);
    assert_eq!(heard(&s.channel(0)), vec![0.25]);
    // It is never opened: nothing to open.
    assert_eq!(s.control().outputs(), vec![(DEV_NAME.to_owned(), true)]);
}

/// Checklist: two devices open, one unplugged mid-playback: only that patch's cues stop and a
/// warning is raised; a pre-wait and a cue on the other device carry on; on replug the patch
/// plays again; the other device never drops out.
#[test]
fn losing_the_second_device_stops_only_its_cues() {
    let a = wav(&dc(20_000, 0.25));
    let b = wav(&dc(20_000, 0.5));
    let late = wav(&dc(200, 0.75));
    let mut c3 = audio(3, &late, 1);
    c3.pre_wait = secs(1000);
    let mut s = two_device_show(vec![audio(1, &a, 0), audio_b(2, &b, 0), c3]);
    start(&mut s, 1);
    start(&mut s, 2);
    s.run(BLOCK * 4);
    let at3 = start(&mut s, 3);
    s.run(BLOCK);
    s.take_outbound();
    unplug(&mut s, DEV2);
    let msgs = s.take_outbound();
    assert!(!s.control().output_up(DEV2_NAME));
    assert!(!s.control().device_down());
    assert!(paused_device_lost(&msgs, 2));
    assert!(states(&msgs, id(1)).is_empty());
    assert!(states(&msgs, id(3)).is_empty());
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    assert_eq!(s.control().engine().state(id(3)), CueState::PreWaiting);
    assert_eq!(notices(&msgs), vec!["device_lost".to_owned()]);
    assert!(
        logs(&msgs, LogLevel::Error)
            .iter()
            .any(|m| m.contains(DEV2_NAME) && m.contains("1 cue(s)")),
        "{:?}",
        logs(&msgs, LogLevel::Error)
    );
    // The pre-wait runs out on the primary and its cue plays on time.
    s.run(1500);
    assert_eq!(
        first_nonzero(&s.channel(1)),
        Some(at3 as usize + 1000),
        "cue 3 started after its pre-wait"
    );
    // The primary's cue never dropped out.
    let ch0 = s.channel(0);
    let first = first_nonzero(&ch0).unwrap();
    assert!(ch0.len() > first + 1500);
    assert!(ch0[first..].iter().all(|&v| v == 0.25));
    // Plugged back in: it reopens and its patch plays again.
    s.fake.plug(listed(DEV2, DEV2_NAME, 2));
    s.wait_wall(Duration::from_secs(1), 2);
    s.wait_output(DEV2_NAME);
    // Its cue keeps its paused mark until it is cleared (QLab 5); nothing resumed.
    assert_eq!(s.control().engine().state(id(2)), CueState::Paused);
    // Its patch's gains come back over the de-zipper.
    s.run(400);
    let before = s.device_channel(DEV2, 0).len();
    start(&mut s, 2);
    s.run(600);
    assert_eq!(heard(&s.device_channel(DEV2, 0)[before..]), vec![0.5]);
}

/// The primary device lost while another plays: its cues stop, the other device's cues and the
/// waits carry on, with the master clock running on the wall clock; when it returns the clock
/// hands back to it. The clock never goes back.
#[test]
fn losing_the_primary_with_a_second_device_up_keeps_the_rest_of_the_show_running() {
    let a = wav(&dc(40_000, 0.25));
    let b = wav(&dc(40_000, 0.5));
    let late = wav(&dc(200, 0.75));
    let mut c3 = audio_b(3, &late, 1);
    c3.pre_wait = secs(1000);
    let mut s = two_device_show(vec![audio(1, &a, 0), audio_b(2, &b, 0), c3]);
    start(&mut s, 1);
    start(&mut s, 2);
    s.run(BLOCK * 4);
    start(&mut s, 3);
    s.run(BLOCK);
    s.take_outbound();
    let mut clock = vec![s.control().rendered()];
    unplug(&mut s, DEV);
    let msgs = s.take_outbound();
    assert!(s.control().device_down());
    assert!(s.control().clock_on_wall());
    assert!(paused_device_lost(&msgs, 1));
    assert!(states(&msgs, id(2)).is_empty());
    assert_eq!(s.control().engine().state(id(2)), CueState::Running);
    assert_eq!(s.control().engine().state(id(3)), CueState::PreWaiting);
    // Time goes on: the pre-wait runs out and cue 3 plays on the second device.
    let before = s.device_channel(DEV2, 1).len();
    for _ in 0..30 {
        s.play(BLOCK);
        clock.push(s.control().rendered());
    }
    assert!(
        clock.last() > clock.first(),
        "the clock ran on while the primary was away"
    );
    assert_eq!(
        heard(&s.device_channel(DEV2, 1)[before..]),
        vec![0.75],
        "cue 3 played after its pre-wait"
    );
    assert_eq!(s.control().engine().state(id(2)), CueState::Running);
    // Cue 2 never dropped out on the second device.
    let ch = s.device_channel(DEV2, 0);
    let first = first_nonzero(&ch).unwrap();
    assert!(ch[first..].iter().all(|&v| v == 0.5));
    // The primary returns: the clock is its counter again, and goes on from where it was.
    s.fake.plug(listed(DEV, DEV_NAME, 2));
    s.wait_wall(Duration::from_secs(1), 2);
    clock.push(s.control().rendered());
    assert!(!s.control().device_down());
    assert!(!s.control().clock_on_wall());
    for _ in 0..5 {
        s.run(BLOCK);
        clock.push(s.control().rendered());
    }
    assert!(
        clock.windows(2).all(|w| w[0] <= w[1]),
        "the clock went back: {clock:?}"
    );
    // Cue 1 plays again from GO (it showed paused until then).
    assert_eq!(s.control().engine().state(id(1)), CueState::Paused);
    let before = s.channel(0).len();
    start(&mut s, 1);
    s.run(600);
    assert_eq!(heard(&s.channel(0)[before..]), vec![0.25]);
}

/// Both devices lost, one after the other: the second loss is of the last device that played,
/// so, as with one device, every wait scheduled before it is cancelled.
#[test]
fn losing_the_last_device_that_plays_cancels_every_wait() {
    let a = wav(&dc(200, 0.25));
    let mut memo = cue(3, CueKind::Memo);
    memo.pre_wait = secs(4000);
    let mut s = two_device_show(vec![audio(1, &a, 0), memo]);
    start(&mut s, 3);
    s.run(BLOCK);
    unplug(&mut s, DEV2);
    assert_eq!(s.control().engine().state(id(3)), CueState::PreWaiting);
    unplug(&mut s, DEV);
    assert_ne!(s.control().engine().state(id(3)), CueState::PreWaiting);
    assert!(!s.control().engine().has_active_cues());
}

/// A command for the primary sent while it is away and the master clock runs on the wall
/// clock (here a new patch's slot taking its gains) applies on the first blocks after the
/// primary returns, not as long after its return as it was sent into the outage.
#[test]
fn a_primary_command_sent_during_its_outage_applies_as_soon_as_it_returns() {
    let b = wav(&dc(40_000, 0.5));
    let mut s = two_device_show(vec![audio_b(2, &b, 0)]);
    start(&mut s, 2);
    s.run(BLOCK * 2);
    unplug(&mut s, DEV);
    assert!(s.control().clock_on_wall());
    // A long outage: the master clock runs on far past the primary's counter (and past the
    // mixer's lookahead).
    for _ in 0..200 {
        s.play(BLOCK);
    }
    let mut patch = s.control().workspace().patches[0].clone();
    patch.id = openlab_core::PatchId(uuid::Uuid::from_u128(903));
    patch.name = "C".into();
    let rid = s.request_id();
    let replies = s.send(ClientMessage::Command {
        id: rid,
        command: Box::new(openlab_core::Command::AddPatch { index: 2, patch }),
        coalesce: None,
    });
    assert!(
        replies.iter().any(|m| matches!(
            m,
            ServerMessage::Result {
                outcome: Outcome::Ok(_),
                ..
            }
        )),
        "{replies:?}"
    );
    s.fake.plug(listed(DEV, DEV_NAME, 2));
    s.wait_wall(Duration::from_secs(1), 2);
    assert!(!s.control().device_down());
    s.run(BLOCK * 3);
    assert_eq!(
        s.control().mixer().held(),
        0,
        "a command for the primary is still waiting after its return"
    );
}

// AUD-21: the buffer size is set per device (16 to 1024 frames, 512 by default).

fn set_buffer(s: &mut DeviceShow, device: &str, frames: u32) -> Result<(), ErrorCode> {
    let id = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id,
        request: Request::SetBufferSize {
            device: device.into(),
            frames,
        },
    });
    match replies.iter().find_map(|m| match m {
        ServerMessage::Result { outcome, .. } => Some(outcome.clone()),
        _ => None,
    }) {
        Some(Outcome::Ok(_)) => Ok(()),
        Some(Outcome::Error(e)) => Err(e.code),
        None => panic!("{replies:?}"),
    }
}

/// Checklist: a device's buffer size is changed: that device reopens at that size, the cue on
/// it carries on, and the other device is untouched.
#[test]
fn changing_a_devices_buffer_size_reopens_only_that_device() {
    let a = wav(&dc(20_000, 0.25));
    let b = wav(&dc(20_000, 0.5));
    let mut s = two_device_show(vec![audio(1, &a, 0), audio_b(2, &b, 0)]);
    assert_eq!(s.fake.requested(DEV2), vec![Some(512)], "the default size");
    start(&mut s, 1);
    start(&mut s, 2);
    s.run(BLOCK * 4);
    s.take_outbound();
    let primary_opens = s.fake.requested(DEV).len();
    s.fake.set_device_block(DEV2, 128);
    assert_eq!(set_buffer(&mut s, DEV2_NAME, 128), Ok(()));
    s.settle_devices();
    assert_eq!(s.fake.requested(DEV2).last(), Some(&Some(128)));
    assert_eq!(
        s.fake.requested(DEV).len(),
        primary_opens,
        "the primary was not reopened"
    );
    let msgs = s.take_outbound();
    assert!(
        logs(&msgs, LogLevel::Info)
            .iter()
            .any(|m| m.contains("reopened with 128-frame buffers")),
        "{:?}",
        logs(&msgs, LogLevel::Info)
    );
    assert!(logs(&msgs, LogLevel::Warn).is_empty(), "{msgs:?}");
    assert!(!broken_device_lost(&msgs, 2));
    // The lookahead follows the larger buffer.
    assert_eq!(s.control().block_frames(), 128);
    s.run(BLOCK * 8);
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    assert_eq!(s.control().engine().state(id(2)), CueState::Running);
    // Neither device dropped its cue: each still plays it, at its level.
    assert_eq!(heard(&s.device_channel(DEV, 0)), vec![0.25]);
    assert_eq!(heard(&s.device_channel(DEV2, 0)), vec![0.5]);
    let ch = s.device_channel(DEV2, 0);
    let first = first_nonzero(&ch).unwrap();
    assert!(ch[first..].iter().all(|&v| v == 0.5), "a gap in cue 2");
    // Kept on this machine and listed.
    assert_eq!(s.control().machine().buffer_frames_for(DEV2_NAME), 128);
    let listed = list_devices(&mut s);
    assert_eq!(
        listed.iter().map(|d| d.buffer_frames).collect::<Vec<_>>(),
        vec![512, 128]
    );
}

/// A size the device cannot do: it plays at the nearest size it can, with a warning.
#[test]
fn a_buffer_size_the_device_refuses_falls_back_with_a_warning() {
    let mut s = two_device_show(vec![]);
    s.fake.set_device_block(DEV2, 256);
    assert_eq!(set_buffer(&mut s, DEV2_NAME, 128), Ok(()));
    s.settle_devices();
    let warned = logs(&s.take_outbound(), LogLevel::Warn);
    assert_eq!(
        warned,
        vec![format!(
            "audio device \"{DEV2_NAME}\" cannot use 128-frame buffers; it plays with 256"
        )]
    );
    assert!(s.control().output_up(DEV2_NAME));
}

/// Every path that sets a buffer size checks it: out of 16 to 1024 frames or for a device that
/// is not listed, the request is refused and nothing changes or reopens.
#[test]
fn a_buffer_size_outside_16_to_1024_or_for_an_unknown_device_is_refused() {
    let mut s = two_device_show(vec![]);
    let opens = s.fake.opens();
    for frames in [0, 15, 1025, u32::MAX] {
        assert_eq!(
            set_buffer(&mut s, DEV2_NAME, frames),
            Err(ErrorCode::BadRequest),
            "{frames}"
        );
    }
    assert_eq!(set_buffer(&mut s, "Gone", 256), Err(ErrorCode::NotFound));
    s.settle_devices();
    assert_eq!(s.fake.opens(), opens);
    assert!(s.control().machine().buffer_frames.is_empty());
    // The ends of the range are accepted.
    assert_eq!(set_buffer(&mut s, DEV2_NAME, 16), Ok(()));
    assert_eq!(set_buffer(&mut s, DEV2_NAME, 1024), Ok(()));
}

/// A device opens at the buffer size set for it on this machine.
#[test]
fn a_device_opens_at_the_buffer_size_set_for_it() {
    let mut ws = workspace(vec![]);
    let mut patch = ws.patches[0].clone();
    patch.id = patch_b();
    ws.patches.push(patch);
    let mut c = device_config(&ws);
    c.machine
        .set_patch_mapping(
            ws.id,
            patch_b(),
            PatchMapping {
                device: DEV2_NAME.into(),
                channels: vec![0, 1],
                system: false,
            },
        )
        .unwrap();
    c.machine.set_buffer_frames(DEV2_NAME, 256).unwrap();
    let mut s = DeviceShow::new(
        ws,
        vec![listed(DEV, DEV_NAME, 2), listed(DEV2, DEV2_NAME, 2)],
        c,
    );
    s.wait_output(DEV2_NAME);
    assert_eq!(s.fake.requested(DEV2), vec![Some(256)]);
}

/// A device that cannot be opened again at its new size is lost: its cues stop, the other
/// device plays on.
#[test]
fn a_device_that_cannot_reopen_at_its_new_size_is_lost() {
    let a = wav(&dc(20_000, 0.25));
    let b = wav(&dc(20_000, 0.5));
    let mut s = two_device_show(vec![audio(1, &a, 0), audio_b(2, &b, 0)]);
    start(&mut s, 1);
    start(&mut s, 2);
    s.run(BLOCK * 4);
    s.take_outbound();
    // Gone from the host before the list says so: the reopen finds no device.
    s.fake.unplug(DEV2, false);
    assert_eq!(set_buffer(&mut s, DEV2_NAME, 128), Ok(()));
    s.settle_devices();
    let msgs = s.take_outbound();
    assert!(!s.control().output_up(DEV2_NAME));
    assert!(paused_device_lost(&msgs, 2));
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    assert!(!s.control().device_down());
}

/// Checklist: a cue is playing on patch A; patch B's device is changed: A's cue plays on
/// without a dropout, and the next cue started on B plays on the new device.
#[test]
fn changing_patch_bs_device_leaves_patch_as_cue_playing_and_moves_bs_next_cue() {
    let a = wav(&dc(20_000, 0.25));
    let b = wav(&dc(500, 0.5));
    let mut s = two_patch_show(
        vec![audio(1, &a, 0), audio_b(2, &b, 0)],
        DEV_NAME,
        vec![listed(DEV, DEV_NAME, 2), listed(DEV2, DEV2_NAME, 2)],
    );
    start(&mut s, 1);
    s.run(BLOCK * 4);
    let rid = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id: rid,
        request: Request::SetPatchMapping {
            patch: patch_b(),
            mapping: Some(PatchMapping {
                device: DEV2_NAME.into(),
                channels: vec![0, 1],
                system: false,
            }),
        },
    });
    assert!(
        replies.iter().any(|m| matches!(
            m,
            ServerMessage::Result {
                outcome: Outcome::Ok(_),
                ..
            }
        )),
        "{replies:?}"
    );
    s.wait_output(DEV2_NAME);
    s.run(BLOCK * 2);
    start(&mut s, 2);
    s.run(1000);
    // Cue 2 on the new device only; cue 1 on the first, never interrupted.
    assert_eq!(heard(&s.device_channel(DEV2, 0)), vec![0.5]);
    let ch = s.device_channel(DEV, 0);
    let first = first_nonzero(&ch).unwrap();
    assert!(ch.len() > first + 1000);
    assert!(ch[first..].iter().all(|&v| v == 0.25), "cue 1 dropped out");
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
}

/// Sends a buffer size change for `device`, which must be accepted, with the gate closed, and
/// waits until the device thread is held in the reopen.
fn change_buffer_and_hang(s: &mut DeviceShow, device: &str, frames: u32) {
    let asked = s.fake.opens_asked();
    s.fake.close_gate();
    assert_eq!(set_buffer(s, device, frames), Ok(()));
    crate::show::spin::spin_until("the reopen to start", || {
        s.tick();
        s.fake.opens_asked() > asked
    });
}

/// The primary hangs in a buffer size change while another device plays: during the gap the
/// master clock runs on the wall clock, and once the device thread is past its watchdog the
/// change is a loss: the primary's cue stops (broken, `device_lost`) instead of showing Running
/// for ever, and the pre-wait on the healthy device runs out and its cue plays.
#[test]
fn a_primary_hung_in_a_buffer_size_change_is_lost_and_the_show_runs_on() {
    // Longer than the whole test, so it is still running when the change is given up on.
    let a = wav(&dc(400_000, 0.25));
    let late = wav(&dc(200, 0.75));
    let mut c3 = audio_b(3, &late, 1);
    c3.pre_wait = secs(20_000);
    let mut s = two_device_show(vec![audio(1, &a, 0), c3]);
    start(&mut s, 1);
    s.run(BLOCK * 4);
    start(&mut s, 3);
    s.run(BLOCK);
    s.take_outbound();
    let dev2_before = s.device_channel(DEV2, 1).len();
    change_buffer_and_hang(&mut s, DEV_NAME, 128);
    // Past the job watchdog (the reopen is stuck behind the gate); the primary renders until
    // the close takes effect, so the clock is read after that.
    for _ in 0..400 {
        s.play(BLOCK);
    }
    for _ in 0..24 {
        s.advance(Duration::from_millis(500));
        s.tick();
    }
    let at = s.control().rendered();
    for _ in 0..800 {
        s.play(BLOCK);
    }
    let msgs = s.take_outbound();
    let after = (
        s.control().rendered(),
        s.control().engine().state(id(1)),
        s.control().engine().is_lost(id(1)),
        s.control().engine().state(id(3)),
    );
    let heard3 = heard(&s.device_channel(DEV2, 1)[dev2_before..]);
    s.fake.open_gate();
    assert!(
        after.0 > at + 40_000,
        "the master clock ran on: {at} -> {}",
        after.0
    );
    assert_eq!(after.1, CueState::Paused);
    assert!(after.2);
    assert_ne!(
        after.3,
        CueState::PreWaiting,
        "the pre-wait on device 2 never ended"
    );
    assert_eq!(heard3, vec![0.75]);
    assert!(s.control().output_up(DEV2_NAME));
    assert!(
        workspace_warnings(&msgs)
            .unwrap()
            .contains(&"device_unrecoverable".to_owned())
    );
}

/// The second device hangs in a buffer size change: its cue stops as for a loss, the primary
/// and its cues play on.
#[test]
fn a_second_device_hung_in_a_buffer_size_change_is_lost_alone() {
    let a = wav(&dc(40_000, 0.25));
    let b = wav(&dc(40_000, 0.5));
    let mut s = two_device_show(vec![audio(1, &a, 0), audio_b(2, &b, 0)]);
    start(&mut s, 1);
    start(&mut s, 2);
    s.run(BLOCK * 4);
    s.take_outbound();
    change_buffer_and_hang(&mut s, DEV2_NAME, 128);
    for _ in 0..12 {
        s.advance(Duration::from_secs(1));
        s.step();
    }
    let msgs = s.take_outbound();
    let states = (
        s.control().engine().state(id(1)),
        s.control().engine().is_lost(id(2)),
    );
    // Reported to the app as needing a restart; the primary plays on.
    let outputs: Vec<OutputState> = s
        .control()
        .output_statuses()
        .into_iter()
        .map(|o| o.state)
        .collect();
    s.fake.open_gate();
    assert!(
        paused_device_lost(&msgs, 2),
        "{:?}",
        logs(&msgs, LogLevel::Error)
    );
    assert_eq!(states.0, CueState::Running);
    assert!(states.1);
    assert!(!s.control().device_down());
    assert_eq!(outputs, vec![OutputState::Up, OutputState::Failed]);
}

/// The only device hangs in a buffer size change: as a sole-device loss, its cues stop and
/// every wait scheduled before it is cancelled.
#[test]
fn the_sole_device_hung_in_a_buffer_size_change_is_lost_like_any_sole_device() {
    let a = wav(&dc(40_000, 0.25));
    let mut memo = cue(3, CueKind::Memo);
    memo.pre_wait = secs(40_000);
    let mut s = device_show(workspace(vec![audio(1, &a, 0), memo]));
    start(&mut s, 1);
    start(&mut s, 3);
    s.run(BLOCK * 4);
    s.take_outbound();
    change_buffer_and_hang(&mut s, DEV_NAME, 128);
    for _ in 0..12 {
        s.advance(Duration::from_secs(1));
        s.tick();
    }
    let msgs = s.take_outbound();
    let states = (
        s.control().engine().is_lost(id(1)),
        s.control().engine().state(id(3)),
        s.control().device_down(),
    );
    s.fake.open_gate();
    assert!(paused_device_lost(&msgs, 1));
    assert!(states.0);
    assert_ne!(states.1, CueState::PreWaiting, "the wait was not cancelled");
    assert!(states.2, "the device counts as away");
}

fn map_b(s: &mut DeviceShow, device: &str) {
    let rid = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id: rid,
        request: Request::SetPatchMapping {
            patch: patch_b(),
            mapping: Some(PatchMapping {
                device: device.into(),
                channels: vec![0, 1],
                system: false,
            }),
        },
    });
    assert!(
        replies.iter().any(|m| matches!(
            m,
            ServerMessage::Result {
                outcome: Outcome::Ok(_),
                ..
            }
        )),
        "{replies:?}"
    );
}

/// A device no patch maps to any more closes once nothing plays on it (a cue playing there
/// moves to its patch's new device at once); unplugged then, it raises no warning, no error and
/// no reopen attempts.
#[test]
fn a_device_no_patch_uses_closes_and_its_unplugging_raises_nothing() {
    let b = wav(&dc(4000, 0.5));
    let mut s = two_device_show(vec![audio_b(2, &b, 0)]);
    start(&mut s, 2);
    s.run(BLOCK * 2);
    map_b(&mut s, DEV_NAME);
    s.run(BLOCK * 20);
    // The cue moved to the primary (fading in over the crossfade) and plays on there.
    assert_eq!(s.control().engine().state(id(2)), CueState::Running);
    assert_eq!(heard(&s.channel(0)).last(), Some(&0.5));
    s.settle_devices();
    assert!(
        !s.control().output_up(DEV2_NAME),
        "the unused device stayed open"
    );
    let on_two = s
        .device_channel(DEV2, 0)
        .iter()
        .filter(|&&x| x != 0.0)
        .count();
    assert!(on_two < 4000, "it played its whole file on the old device");
    s.run(4000);
    assert_eq!(s.control().engine().state(id(2)), CueState::Idle);
    s.take_outbound();
    s.fake.unplug(DEV2, true);
    s.wait_wall(Duration::from_secs(1), 2);
    s.wait_wall(Duration::from_secs(4), 4);
    let msgs = s.take_outbound();
    assert!(logs(&msgs, LogLevel::Error).is_empty(), "{msgs:?}");
    assert!(logs(&msgs, LogLevel::Warn).is_empty(), "{msgs:?}");
    assert!(
        !workspace_warnings(&msgs)
            .unwrap_or_default()
            .contains(&"device_lost".to_owned())
    );
    // Patch B now plays on the primary.
    let before = s.channel(0).len();
    start(&mut s, 2);
    s.run(1000);
    assert_eq!(heard(&s.channel(0)[before..]), vec![0.5]);
}

/// A closed device a patch maps to again opens again, and the patch plays there.
#[test]
fn a_closed_device_opens_again_when_a_patch_maps_to_it() {
    let b = wav(&dc(300, 0.5));
    let mut s = two_device_show(vec![audio_b(2, &b, 0)]);
    map_b(&mut s, DEV_NAME);
    s.settle_devices();
    assert!(!s.control().output_up(DEV2_NAME));
    // The stand-in reports the size the device was asked for.
    s.fake
        .set_device_block(DEV2, openlab_core::DEFAULT_BUFFER_FRAMES);
    map_b(&mut s, DEV2_NAME);
    s.wait_output(DEV2_NAME);
    s.run(BLOCK * 4);
    let before = s.device_channel(DEV2, 0).len();
    start(&mut s, 2);
    s.run(600);
    assert_eq!(heard(&s.device_channel(DEV2, 0)[before..]), vec![0.5]);
    let msgs = s.take_outbound();
    assert!(
        logs(&msgs, LogLevel::Warn).is_empty(),
        "{:?}",
        logs(&msgs, LogLevel::Warn)
    );
}

/// Patch B mapped to `channels` of a second device (`devices[1]`), patch A on the primary
/// (`devices[0]`, opened with all its outputs).
fn patch_b_on(
    devices: Vec<devices::ListedDevice>,
    channels: Vec<u32>,
    cues: Vec<openlab_core::Cue>,
) -> DeviceShow {
    let mut ws = workspace(cues);
    let mut patch = ws.patches[0].clone();
    patch.id = patch_b();
    patch.name = "B".into();
    ws.patches.push(patch);
    let mut c = device_config(&ws);
    c.mixer_patches = 4;
    // The primary's mixer has as many outputs as the primary has.
    c.output_mixer = Some(openlab_audio::MixerConfig {
        output_channels: usize::from(devices[0].max_channels()),
        patches: 4,
        ..mixer_config()
    });
    let name = devices[1].name.clone();
    c.machine
        .set_patch_mapping(
            ws.id,
            patch_b(),
            PatchMapping {
                device: name.clone(),
                channels,
                system: false,
            },
        )
        .unwrap();
    let mut s = DeviceShow::new(ws, devices, c);
    s.wait_output(&name);
    s.tick();
    s
}

/// The `outputs_dropped` notices of the last warnings list sent (none sent: none).
fn outputs_dropped(s: &mut DeviceShow) -> Vec<String> {
    s.take_outbound()
        .iter()
        .filter_map(|m| match m {
            ServerMessage::Warnings { warnings } => Some(warnings.clone()),
            _ => None,
        })
        .next_back()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|w| match w {
            WireWarning::Notice { code, message, .. } if code == "outputs_dropped" => Some(message),
            _ => None,
        })
        .collect()
}

/// Checklist (#654, AUD-02): a second device with more outputs than the primary plays a patch
/// mapped to its upper outputs. An 8-output interface beside a 2-output primary: patch B on its
/// outputs 7 and 8 is heard there, on no other output of either device, with no warning; the
/// device is reported with its 8 outputs.
#[test]
fn aud_02_a_second_device_with_more_outputs_than_the_primary_plays_a_patch_mapped_to_its_upper_outputs()
 {
    let b = wav(&dc(500, 0.5));
    let mut cue = audio_b(2, &b, 0);
    if let CueKind::Audio { levels, .. } = &mut cue.kind {
        // The file into both of the patch's outputs.
        levels.crosspoints.push(openlab_core::Crosspoint {
            input: 0,
            output: 1,
            db: 0.0,
            mute: false,
        });
    }
    let mut s = patch_b_on(
        vec![listed(DEV, DEV_NAME, 2), listed(DEV2, DEV2_NAME, 8)],
        vec![6, 7],
        vec![cue],
    );
    assert_eq!(outputs_dropped(&mut s), Vec::<String>::new());
    assert_eq!(
        resolutions(&mut s)[1],
        Some(WireResolution::Mapped {
            device: DEV2_NAME.into(),
            channels: vec![6, 7],
        })
    );
    start(&mut s, 2);
    s.run(1000);
    for ch in 0..8 {
        let want: Vec<f32> = if ch >= 6 { vec![0.5] } else { vec![] };
        assert_eq!(
            heard(&s.device_channel(DEV2, ch)),
            want,
            "output {}",
            ch + 1
        );
    }
    let played = |ch| {
        s.device_channel(DEV2, ch)
            .iter()
            .filter(|&&x| x != 0.0)
            .count()
    };
    assert_eq!((played(6), played(7)), (500, 500));
    assert!(heard(&s.channel(0)).is_empty() && heard(&s.channel(1)).is_empty());
    assert_eq!(s.control().engine().state(id(2)), CueState::Idle);
}

/// Checklist (#654): the warning for a patch that needs more outputs than its device has names
/// that device's real output count (8 here, not the primary's 2). A device with more than 64
/// outputs is opened with 64, and a patch beyond them is told so.
#[test]
fn the_warning_for_a_patch_that_needs_more_outputs_than_its_device_has_names_that_devices_real_output_count()
 {
    let mut s = patch_b_on(
        vec![listed(DEV, DEV_NAME, 2), listed(DEV2, DEV2_NAME, 8)],
        vec![8, 9],
        vec![],
    );
    assert_eq!(
        outputs_dropped(&mut s),
        vec![format!(
            "patch \"B\" maps 10 channel(s) of \"{DEV2_NAME}\", which has 8: its cues cannot play"
        )]
    );
    // A 2-output device beside an 8-output primary: the patch's warning names its 2 outputs,
    // and the device is not said to have fewer than the show opened with (it opens with its
    // own count, all of which play).
    let mut s = patch_b_on(
        vec![listed(DEV, DEV_NAME, 8), listed(DEV2, DEV2_NAME, 2)],
        vec![2, 3],
        vec![],
    );
    assert_eq!(
        outputs_dropped(&mut s),
        vec![format!(
            "patch \"B\" maps 4 channel(s) of \"{DEV2_NAME}\", which has 2: its cues cannot play"
        )]
    );
    let mut s = patch_b_on(
        vec![listed(DEV, DEV_NAME, 2), listed(DEV2, DEV2_NAME, 128)],
        vec![64, 65],
        vec![],
    );
    assert_eq!(
        outputs_dropped(&mut s),
        vec![format!(
            "patch \"B\" maps channel 66 of \"{DEV2_NAME}\", but openLab uses only its first 64 \
             outputs: its cues cannot play"
        )]
    );
}

/// `frames` of a cosine of `hz` at amplitude `amp`, sampled at `rate`: its first sample is
/// `amp`, so its start is seen.
fn cosine(frames: usize, rate: u32, hz: f64, amp: f32) -> Vec<f32> {
    (0..frames)
        .map(|i| {
            let t = i as f64 / f64::from(rate);
            amp * (2.0 * std::f64::consts::PI * hz * t).cos() as f32
        })
        .collect()
}

/// Frequency of a signal sampled at `rate` from its upward zero crossings (interpolated).
fn frequency(x: &[f32], rate: u32) -> f64 {
    let mut crossings = Vec::new();
    for (i, w) in x.windows(2).enumerate() {
        let (a, b) = (f64::from(w[0]), f64::from(w[1]));
        if a < 0.0 && b >= 0.0 {
            crossings.push(i as f64 + a / (a - b));
        }
    }
    let (first, last) = (crossings[0], *crossings.last().unwrap());
    (crossings.len() - 1) as f64 / (last - first) * f64::from(rate)
}

/// The second device's rate in the tests of a device at its own rate: 1.5 times the primary's.
const RATE2: u32 = RATE * 3 / 2;

/// Checklist (#654, AUD-02): a second device at another sample rate than the primary plays its
/// patch's cues at the right pitch and on the right sample. The same 1 s file of 500 Hz plays on
/// the primary (8 kHz) and, from the same GO sample, on a device that runs only at 12 kHz: there
/// it lasts 12 000 of its frames (1 s), is still 500 Hz, matches the file sample for sample to
/// within resampling error, and starts on the sample the master date translates to there
/// (`local_rendered + (at - master_rendered) × 12 000 / 8 000`): the same moment as on the
/// primary, to within a frame (the tolerance across devices is one buffer: counters read when a
/// command is sent; here the two run in step). The device is reported at its own rate.
#[test]
fn aud_02_a_second_device_at_another_sample_rate_than_the_primary_plays_its_patchs_cues_at_the_right_pitch_and_on_the_right_sample()
 {
    let file = cosine(RATE as usize, RATE, 500.0, 0.5);
    let path = wav(&file);
    let clock = ManualClock::new(1_700_000_000_000);
    let epoch = clock.clock();
    let mut s = two_patch_show_with(
        vec![audio(1, &path, 0), audio_b(2, &path, 0)],
        DEV2_NAME,
        vec![listed(DEV, DEV_NAME, 2)],
        |c| {
            c.clock = epoch;
            c.status = Some(StatusConfig::default());
        },
    );
    // The second device comes later, so the two counters differ, and runs only at 12 kHz.
    s.run(BLOCK * 7);
    s.fake.plug(listed_at(DEV2, DEV2_NAME, 2, RATE2));
    s.wait_wall(Duration::from_secs(1), 1);
    s.wait_output(DEV2_NAME);
    s.run(BLOCK * 3);
    let master = s.control().rendered();
    let local = s
        .control()
        .output_mixer(DEV2_NAME)
        .unwrap()
        .samples_rendered();
    let at = start(&mut s, 1);
    assert_eq!(start(&mut s, 2), at, "both start on one sample");
    s.run(u64::from(RATE) + 2_000);
    let one = s.channel(0);
    let two = s.device_channel(DEV2, 0);
    // On the right sample of each device.
    let first_one = first_nonzero(&one).unwrap();
    let first_two = first_nonzero(&two).unwrap();
    let ahead = (at - master) as f64 * f64::from(RATE2) / f64::from(RATE);
    assert_eq!(first_two as u64, local + ahead.round() as u64);
    assert_eq!(first_one as u64, at);
    // The same moment: the second device's capture starts `shift` master samples after the
    // primary's.
    let shift = (master - local * u64::from(RATE) / u64::from(RATE2)) as f64;
    let t_two = shift + first_two as f64 * f64::from(RATE) / f64::from(RATE2);
    assert!(
        (t_two - first_one as f64).abs() <= 1.0,
        "{t_two} vs {first_one}"
    );
    // The right length: 1 s on each.
    let last_two = last_nonzero(&two).unwrap();
    assert_eq!(last_nonzero(&one).unwrap() - first_one + 1, RATE as usize);
    assert!(
        (last_two - first_two + 1).abs_diff(RATE2 as usize) <= 1,
        "{} frames",
        last_two - first_two + 1
    );
    // The right pitch, and the file's samples, on each.
    let heard_two = &two[first_two..=last_two];
    let f = frequency(&heard_two[100..heard_two.len() - 100], RATE2);
    assert!((f - 500.0).abs() < 0.05, "{f} Hz");
    let f = frequency(&one[first_one..first_one + RATE as usize], RATE);
    assert!((f - 500.0).abs() < 0.05, "{f} Hz");
    let want = cosine(RATE2 as usize, RATE2, 500.0, 0.5);
    let worst = heard_two[100..heard_two.len() - 100]
        .iter()
        .zip(&want[100..])
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(worst < 0.01, "differs from the file by {worst}");
    assert_eq!(s.control().engine().state(id(2)), CueState::Idle);
    // Reported at its own rate, with its own block.
    clock.advance(1000);
    s.tick();
    let status = statuses(s.take_outbound()).pop().expect("a status");
    let two = status.outputs.iter().find(|o| o.name == DEV2_NAME).unwrap();
    assert_eq!((two.sample_rate, two.block_frames), (RATE2, Some(96)));
    assert_eq!(status.outputs[0].sample_rate, RATE);
}

/// A cue moved to a device at another rate (its patch's device changed while it plays) is
/// heard there in step: what the new device plays from the swap on is the file at the point the
/// master clock has reached, at the right pitch, and it ends when the file does.
#[test]
fn a_cue_moved_to_a_device_at_another_rate_goes_on_in_step_at_the_right_pitch() {
    // 50 Hz, so a frame of misalignment shows as a small error, and 2 s long.
    let frames = 2 * RATE as usize;
    let file = cosine(frames, RATE, 50.0, 0.5);
    let path = wav(&file);
    let mut s = two_patch_show(
        vec![audio(1, &path, 0)],
        DEV2_NAME,
        vec![
            listed(DEV, DEV_NAME, 2),
            listed_at(DEV2, DEV2_NAME, 2, RATE2),
        ],
    );
    s.wait_output(DEV2_NAME);
    let at = start(&mut s, 1);
    s.run(BLOCK * 40);
    let master = s.control().rendered();
    let local = s
        .control()
        .output_mixer(DEV2_NAME)
        .unwrap()
        .samples_rendered();
    map_patch(&mut s, patch_id(), DEV2_NAME);
    s.run(frames as u64 + 1_000);
    let two = s.device_channel(DEV2, 0);
    // Master time of the second device's frame `j`.
    let shift = master as f64 - local as f64 * f64::from(RATE) / f64::from(RATE2);
    let master_of = |j: usize| shift + j as f64 * f64::from(RATE) / f64::from(RATE2);
    let first = first_nonzero(&two).expect("heard on the second device");
    let last = last_nonzero(&two).unwrap();
    // Past the crossfade (10 ms): the file where the master clock is.
    let worst = (first + 200..last - 100)
        .map(|j| {
            let t = (master_of(j) - at as f64) / f64::from(RATE);
            let want = 0.5 * (2.0 * std::f64::consts::PI * 50.0 * t).cos();
            (f64::from(two[j]) - want).abs()
        })
        .fold(0.0f64, f64::max);
    assert!(worst < 0.02, "out of step by up to {worst}");
    let f = frequency(&two[first + 200..last - 100], RATE2);
    assert!((f - 50.0).abs() < 0.05, "{f} Hz");
    // It ends where the file does, on the master clock.
    let end = master_of(last + 1) - at as f64;
    assert!(
        (end - frames as f64).abs() <= 1.0,
        "ends {end} frames after its start"
    );
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    assert_eq!(s.control().live_voices(), 0);
}

/// A device at another rate than the primary that is lost and comes back: the cue on it stops
/// as on any lost device, its mixer keeps its rate, and GO plays the cue again there at the
/// right pitch.
#[test]
fn a_device_at_another_rate_lost_and_back_plays_its_cue_again_at_its_rate() {
    let file = cosine(RATE as usize, RATE, 500.0, 0.5);
    let path = wav(&file);
    let mut s = two_patch_show(
        vec![audio_b(2, &path, 0)],
        DEV2_NAME,
        vec![
            listed(DEV, DEV_NAME, 2),
            listed_at(DEV2, DEV2_NAME, 2, RATE2),
        ],
    );
    s.wait_output(DEV2_NAME);
    start(&mut s, 2);
    s.run(BLOCK * 10);
    unplug(&mut s, DEV2);
    s.run(BLOCK * 4);
    let msgs = s.take_outbound();
    assert!(paused_device_lost(&msgs, 2), "{msgs:?}");
    s.fake.plug(listed_at(DEV2, DEV2_NAME, 2, RATE2));
    s.wait_wall(Duration::from_secs(1), 2);
    s.wait_output(DEV2_NAME);
    assert_eq!(
        s.control().output_mixer(DEV2_NAME).unwrap().sample_rate(),
        RATE2
    );
    let before = s.device_channel(DEV2, 0).len();
    start(&mut s, 2);
    s.run(u64::from(RATE) + 1_000);
    let two = s.device_channel(DEV2, 0)[before..].to_vec();
    let first = first_nonzero(&two).expect("plays again");
    let last = last_nonzero(&two).unwrap();
    assert!((last - first + 1).abs_diff(RATE2 as usize) <= 1);
    let f = frequency(&two[first + 100..last - 100], RATE2);
    assert!((f - 500.0).abs() < 0.05, "{f} Hz");
    assert_eq!(s.control().engine().state(id(2)), CueState::Idle);
}

/// A device lost while its cue played, then left by its patch: it is closed at once (its dead
/// voice does not hold it open), no voice record is kept, no reopen attempts follow, and when it
/// comes back and a patch maps to it again nothing of the old cue is heard.
#[test]
fn a_lost_device_its_patch_moved_away_from_is_closed_and_forgets_its_voices() {
    let b = wav(&dc(40_000, 0.5));
    let mut s = two_device_show(vec![audio_b(2, &b, 0)]);
    start(&mut s, 2);
    s.run(BLOCK * 4);
    unplug(&mut s, DEV2);
    assert!(paused_device_lost(&s.take_outbound(), 2));
    map_b(&mut s, DEV_NAME);
    s.run(BLOCK * 4);
    let msgs = s.take_outbound();
    assert!(
        logs(&msgs, LogLevel::Info)
            .iter()
            .any(|m| m.contains(&format!("\"{DEV2_NAME}\" closed: no patch plays on it"))),
        "{:?}",
        logs(&msgs, LogLevel::Info)
    );
    assert_eq!(s.control().live_voices(), 0);
    s.wait_wall(Duration::from_secs(30), 30);
    s.run(BLOCK * 4);
    let msgs = s.take_outbound();
    assert!(
        logs(&msgs, LogLevel::Warn).is_empty(),
        "{:?}",
        logs(&msgs, LogLevel::Warn)
    );
    assert!(logs(&msgs, LogLevel::Error).is_empty());
    assert!(
        !workspace_warnings(&msgs)
            .unwrap_or_default()
            .contains(&"device_lost".to_owned())
    );
    assert_eq!(s.control().live_voices(), 0);
    // Back, and a patch maps to it again: the old cue's voice does not sound.
    s.fake.plug(listed(DEV2, DEV2_NAME, 2));
    s.wait_wall(Duration::from_secs(1), 2);
    let before = s.device_channel(DEV2, 0).len();
    map_b(&mut s, DEV2_NAME);
    s.wait_output(DEV2_NAME);
    s.run(2000);
    assert!(s.device_channel(DEV2, 0).len() > before + 1000);
    assert!(heard(&s.device_channel(DEV2, 0)[before..]).is_empty());
    assert_eq!(s.control().live_voices(), 0);
}

/// The same for a cue the operator had paused before the device was lost: the loss dropped its
/// run, so no resume can come for its paused voice and no record of it is kept. Its cue is
/// primed again and its next GO plays from the prime, on the sample it was asked for.
#[test]
fn a_paused_cue_on_a_lost_device_its_patch_moved_away_from_keeps_no_voice_and_primes_again() {
    let b = wav(&dc(40_000, 0.5));
    let mut s = two_device_show(vec![audio_b(2, &b, 0)]);
    start(&mut s, 2);
    s.run(BLOCK * 4);
    s.transport(TransportAction::Pause { cue: id(2) });
    s.run(BLOCK * 4);
    assert_eq!(s.control().engine().state(id(2)), CueState::Paused);
    unplug(&mut s, DEV2);
    s.run(BLOCK * 4);
    assert_eq!(s.control().engine().run_of(id(2)), None, "the run failed");
    map_b(&mut s, DEV_NAME);
    s.run(BLOCK * 4);
    assert!(
        logs(&s.take_outbound(), LogLevel::Info)
            .iter()
            .any(|m| m.contains(&format!("\"{DEV2_NAME}\" closed: no patch plays on it"))),
    );
    assert_eq!(s.control().live_voices(), 0);
    // Only the prime for the next GO holds a stream.
    s.run(BLOCK * 40);
    assert_eq!(s.control().primed(), vec![id(2)]);
    assert_eq!(s.control().held_streams(), 1);
    let hits = s.control().prime_hits();
    let at = start(&mut s, 2) as usize;
    s.run(BLOCK * 4);
    assert_eq!(s.control().prime_hits(), hits + 1, "GO used the prime");
    let one = s.channel(0);
    assert!(one[..at].iter().all(|&v| v == 0.0), "nothing before the GO");
    assert_eq!(one[at], 0.5, "it plays on the sample of its GO");
    s.transport(TransportAction::HardStop { cue: id(2) });
    s.run(BLOCK * 4);
    assert_eq!(s.control().live_voices(), 0);
}

fn list_patch_mappings(s: &mut DeviceShow) -> Vec<PatchMappingEntry> {
    let id = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id,
        request: Request::ListPatchMappings,
    });
    replies
        .iter()
        .find_map(|m| match m {
            ServerMessage::Result {
                outcome:
                    Outcome::Ok(openlab_core::protocol::OkBody {
                        data: Some(ResultData::PatchMappings { patches }),
                        ..
                    }),
                ..
            } => Some(patches.clone()),
            _ => None,
        })
        .expect("patch_mappings")
}

/// Each listed device's engine output state and whether it is the primary.
fn listed_states(s: &mut DeviceShow) -> Vec<(String, Option<OutputState>, bool)> {
    list_devices(s)
        .into_iter()
        .map(|d| (d.name, d.state, d.primary))
        .collect()
}

/// Each patch's resolution, in workspace order.
fn resolutions(s: &mut DeviceShow) -> Vec<Option<WireResolution>> {
    list_patch_mappings(s)
        .into_iter()
        .map(|p| p.resolution)
        .collect()
}

/// The status sent once the epoch clock has moved `ms` on: each output's name and state.
fn output_states(s: &mut DeviceShow, clock: &ManualClock, ms: i64) -> Vec<(String, OutputState)> {
    clock.advance(ms);
    s.tick();
    let sent = statuses(s.take_outbound());
    let last = sent.last().expect("a status");
    last.outputs
        .iter()
        .map(|o| (o.name.clone(), o.state))
        .collect()
}

/// The app sees every device: `status` describes each open output (state, rate, channels,
/// buffer, which is the primary), `list_devices` marks which listed devices the engine has open
/// and in what state, and `list_patch_mappings` says where each patch resolves, through a loss,
/// a return and a device closing once no patch uses it.
#[test]
fn status_list_devices_and_patch_mappings_describe_every_device() {
    let clock = ManualClock::new(1_700_000_000_000);
    let epoch = clock.clock();
    let mut s = two_patch_show_with(
        vec![],
        DEV2_NAME,
        vec![listed(DEV, DEV_NAME, 2), listed(DEV2, DEV2_NAME, 2)],
        |c| {
            c.clock = epoch;
            c.status = Some(StatusConfig::default());
        },
    );
    s.wait_output(DEV2_NAME);
    clock.advance(1000);
    s.tick();
    let status = statuses(s.take_outbound()).pop().expect("a status");
    let block = Some(s.fake.block());
    assert_eq!(
        status.outputs,
        vec![
            OutputStatus {
                name: DEV_NAME.into(),
                state: OutputState::Up,
                primary: true,
                sample_rate: RATE,
                channels: 2,
                block_frames: block,
                latency_ms: None,
            },
            OutputStatus {
                name: DEV2_NAME.into(),
                state: OutputState::Up,
                primary: false,
                sample_rate: RATE,
                channels: 2,
                block_frames: block,
                latency_ms: None,
            },
        ]
    );
    assert_eq!(status.device.name.as_deref(), Some(DEV_NAME));
    assert_eq!(
        listed_states(&mut s),
        vec![
            (DEV_NAME.to_owned(), Some(OutputState::Up), true),
            (DEV2_NAME.to_owned(), Some(OutputState::Up), false),
        ]
    );
    let mapped = |device: &str| {
        Some(WireResolution::Mapped {
            device: device.into(),
            channels: vec![0, 1],
        })
    };
    assert_eq!(
        resolutions(&mut s),
        vec![mapped(DEV_NAME), mapped(DEV2_NAME)]
    );

    // The second device is unplugged: a status goes out early (after the minimum gap) with it
    // lost, the list no longer has it, and its patch says why it cannot play.
    unplug(&mut s, DEV2);
    assert_eq!(
        output_states(&mut s, &clock, 300),
        vec![
            (DEV_NAME.to_owned(), OutputState::Up),
            (DEV2_NAME.to_owned(), OutputState::Lost),
        ]
    );
    s.wait_wall(Duration::from_secs(1), 2);
    assert_eq!(
        listed_states(&mut s),
        vec![(DEV_NAME.to_owned(), Some(OutputState::Up), true)]
    );
    assert_eq!(
        resolutions(&mut s),
        vec![
            mapped(DEV_NAME),
            Some(WireResolution::DeviceMissing {
                device: DEV2_NAME.into(),
                reason: None,
            })
        ]
    );

    // Back: up again and listed.
    s.fake.plug(listed(DEV2, DEV2_NAME, 2));
    s.wait_wall(Duration::from_secs(1), 2);
    s.wait_output(DEV2_NAME);
    assert_eq!(
        output_states(&mut s, &clock, 300),
        vec![
            (DEV_NAME.to_owned(), OutputState::Up),
            (DEV2_NAME.to_owned(), OutputState::Up),
        ]
    );

    // No patch maps to it any more: it closes, and says so.
    map_b(&mut s, DEV_NAME);
    s.settle_devices();
    assert!(!s.control().output_up(DEV2_NAME));
    assert_eq!(
        output_states(&mut s, &clock, 300),
        vec![
            (DEV_NAME.to_owned(), OutputState::Up),
            (DEV2_NAME.to_owned(), OutputState::Closed),
        ]
    );
    assert_eq!(
        listed_states(&mut s),
        vec![
            (DEV_NAME.to_owned(), Some(OutputState::Up), true),
            (DEV2_NAME.to_owned(), Some(OutputState::Closed), false),
        ]
    );
    assert_eq!(
        resolutions(&mut s),
        vec![mapped(DEV_NAME), mapped(DEV_NAME)]
    );
}

/// A closed device a patch maps to again shows as opening until its open answers, and one
/// changing its buffer size as reconfiguring until it is back.
#[test]
fn an_opening_or_reconfiguring_device_is_reported_as_such() {
    let mut s = two_patch_show(
        vec![],
        DEV2_NAME,
        vec![listed(DEV, DEV_NAME, 2), listed(DEV2, DEV2_NAME, 2)],
    );
    let state = |s: &DeviceShow, name: &str| {
        s.control()
            .output_statuses()
            .into_iter()
            .find(|o| o.name == name)
            .map(|o| o.state)
    };
    s.wait_output(DEV2_NAME);
    assert_eq!(state(&s, DEV2_NAME), Some(OutputState::Up));
    // Closed when no patch maps to it; mapped again, it is opening until the device thread's
    // answer is taken in (a tick without the inbox).
    map_b(&mut s, DEV_NAME);
    s.settle_devices();
    assert_eq!(state(&s, DEV2_NAME), Some(OutputState::Closed));
    map_b(&mut s, DEV2_NAME);
    s.control_mut().tick();
    assert_eq!(state(&s, DEV2_NAME), Some(OutputState::Opening));
    s.wait_output(DEV2_NAME);
    assert_eq!(state(&s, DEV2_NAME), Some(OutputState::Up));
    // The close and reopen run on the device thread: until the loop takes the answers in, the
    // device is reconfiguring.
    s.fake.set_device_block(DEV2, 128);
    assert_eq!(set_buffer(&mut s, DEV2_NAME, 128), Ok(()));
    assert_eq!(state(&s, DEV2_NAME), Some(OutputState::Reconfiguring));
    assert_eq!(state(&s, DEV_NAME), Some(OutputState::Up));
    s.settle_devices();
    assert_eq!(state(&s, DEV2_NAME), Some(OutputState::Up));
    let dev2 = s
        .control()
        .output_statuses()
        .into_iter()
        .find(|o| o.name == DEV2_NAME)
        .unwrap();
    assert_eq!(dev2.block_frames, Some(128));
}

/// A device a patch is mapped to for the first time is opening until its first open answers:
/// in `status.outputs` (with no channels or buffer size yet, nothing being known of it) and in
/// `list_devices`.
#[test]
fn a_device_whose_first_open_is_pending_is_reported_as_opening() {
    let mut s = two_patch_show(
        vec![],
        DEV_NAME,
        vec![listed(DEV, DEV_NAME, 2), listed(DEV2, DEV2_NAME, 2)],
    );
    s.settle_devices();
    assert_eq!(s.control().outputs().len(), 1, "nothing maps to device 2");
    s.fake.close_gate();
    map_b(&mut s, DEV2_NAME);
    s.control_mut().tick();
    let outputs = s.control().output_statuses();
    let dev2 = outputs.iter().find(|o| o.name == DEV2_NAME).unwrap();
    assert_eq!(
        (dev2.state, dev2.primary, dev2.channels, dev2.block_frames),
        (OutputState::Opening, false, 0, None)
    );
    assert_eq!(dev2.latency_ms, None);
    assert_eq!(
        listed_states(&mut s),
        vec![
            (DEV_NAME.to_owned(), Some(OutputState::Up), true),
            (DEV2_NAME.to_owned(), Some(OutputState::Opening), false),
        ]
    );
    s.fake.open_gate();
    s.wait_output(DEV2_NAME);
    let outputs = s.control().output_statuses();
    let dev2 = outputs.iter().find(|o| o.name == DEV2_NAME).unwrap();
    assert_eq!(
        (dev2.state, dev2.channels, dev2.block_frames),
        (OutputState::Up, 2, Some(s.fake.block()))
    );
}

/// A closed device mapped to again whose reopen keeps failing (it left the system while the
/// open was under way) stays opening, retried with the backoff, never lost: nothing played on
/// it, so there is no outage to report. Its patch says why it cannot play.
#[test]
fn a_closed_device_whose_reopen_keeps_failing_stays_opening() {
    let mut s = two_device_show(vec![]);
    map_b(&mut s, DEV_NAME);
    s.settle_devices();
    s.fake.close_gate();
    map_b(&mut s, DEV2_NAME);
    s.control_mut().tick();
    s.fake.unplug(DEV2, false);
    s.fake.open_gate();
    s.take_outbound();
    s.wait_wall(Duration::from_secs(10), 20);
    let opens = s.fake.opens();
    assert!(opens >= 3, "retried: {opens} opens");
    let state = s
        .control()
        .output_statuses()
        .into_iter()
        .find(|o| o.name == DEV2_NAME)
        .map(|o| o.state);
    assert_eq!(state, Some(OutputState::Opening));
    assert_eq!(
        resolutions(&mut s)[1],
        Some(WireResolution::DeviceMissing {
            device: DEV2_NAME.into(),
            reason: None,
        })
    );
    let msgs = s.take_outbound();
    assert!(
        !workspace_warnings(&msgs)
            .unwrap_or_default()
            .contains(&"device_lost".to_owned())
    );
}

/// Two interfaces of one model share a name: `list_devices` gives the engine's output state and
/// the primary mark only to the one it has open, matched by the host's id.
#[test]
fn two_devices_with_one_name_are_told_apart_in_list_devices() {
    let ws = workspace(vec![]);
    let c = device_config(&ws);
    let mut s = DeviceShow::new(
        ws,
        vec![listed(DEV, DEV_NAME, 2), listed("fake:9", DEV_NAME, 2)],
        c,
    );
    s.settle_devices();
    assert_eq!(
        list_devices(&mut s),
        vec![primary(OutputState::Up), entry(DEV_NAME, 2)]
    );
}

/// A cue on the second device ducks a cue on the primary; the second device is unplugged: the
/// ducking cue breaks (device lost), the duck comes off over its time, and the primary's cue is
/// heard back at its own level, never left ducked.
#[test]
fn a_duck_from_a_cue_on_a_lost_device_comes_off_the_other_devices_cue() {
    let a = wav(&dc(40_000, 0.25));
    let b = wav(&dc(40_000, 0.5));
    let mut ducker = audio_b(2, &b, 0);
    ducker.duck_others = openlab_core::DuckOthers {
        enabled: true,
        db: -12.0,
        duration: secs(100),
    };
    let mut s = two_device_show(vec![audio(1, &a, 0), ducker]);
    start(&mut s, 1);
    start(&mut s, 2);
    s.run(BLOCK * 8);
    let ch = s.channel(0);
    let ducked = 0.25 * openlab_core::db_to_gain(-12.0);
    assert!(
        (ch[ch.len() - 1] - ducked).abs() < 1e-3,
        "{}",
        ch[ch.len() - 1]
    );
    unplug(&mut s, DEV2);
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    s.run(BLOCK * 8);
    let ch = s.channel(0);
    assert!(
        (ch[ch.len() - 1] - 0.25).abs() < 1e-4,
        "{}",
        ch[ch.len() - 1]
    );
}

/// The messages of the runtime notices with `code`, in order.
fn notice_messages(msgs: &[ServerMessage], code: &str) -> Vec<String> {
    warnings(msgs)
        .into_iter()
        .filter_map(|w| match w {
            WireWarning::Notice {
                code: c, message, ..
            } if c == code => Some(message),
            _ => None,
        })
        .collect()
}

// AUD-07: a playing cue follows its patch to another device at once (QLab 5, measured by the
// owner 2026-11-14).

/// Puts `patch` on `device`, outputs 1 and 2; the request must be accepted.
fn map_patch(s: &mut DeviceShow, patch: openlab_core::PatchId, device: &str) {
    let rid = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id: rid,
        request: Request::SetPatchMapping {
            patch,
            mapping: Some(PatchMapping {
                device: device.into(),
                channels: vec![0, 1],
                system: false,
            }),
        },
    });
    assert!(
        replies.iter().any(|m| matches!(
            m,
            ServerMessage::Result {
                outcome: Outcome::Ok(_),
                ..
            }
        )),
        "{replies:?}"
    );
}

/// What both devices played on channel 0, sample by sample on the master clock (the primary's
/// count): the second device's capture is shifted by the samples it opened after the first.
fn on_both(s: &DeviceShow) -> Vec<(f32, f32)> {
    let one = s.device_channel(DEV, 0);
    let two = s.device_channel(DEV2, 0);
    let shift = one.len() - two.len();
    (0..one.len())
        .map(|t| {
            let b = t.checked_sub(shift).map_or(0.0, |i| two[i]);
            (one[t], b)
        })
        .collect()
}

/// Checks the cue's file (`ramp(frames)`, sample `i` heard as `(i + 1) / 2^20`) was heard in
/// step from its first sample to its last across the two devices: on every sample the two add
/// up to the file's next sample (no gap, no repeat, no level step), and the move crossfaded
/// over at most `crossfade` samples where both sounded. Returns the first sample the second
/// device played.
fn assert_heard_in_step(s: &DeviceShow, frames: usize, crossfade: usize) -> usize {
    let both = on_both(s);
    let first = both
        .iter()
        .position(|&(a, b)| a != 0.0 || b != 0.0)
        .expect("the cue played");
    let unit = 1.0 / 1_048_576.0;
    for (i, &(a, b)) in both[first..first + frames].iter().enumerate() {
        let want = (i + 1) as f32 * unit;
        assert!(
            (a + b - want).abs() < 2.0 * unit,
            "sample {i} of the file: {a} + {b}, want {want}"
        );
    }
    assert!(
        both[first + frames..]
            .iter()
            .all(|&(a, b)| a == 0.0 && b == 0.0),
        "sound after the file's end"
    );
    let overlap = both.iter().filter(|&&(a, b)| a != 0.0 && b != 0.0).count();
    assert!(
        overlap > 0 && overlap <= crossfade,
        "crossfade of {overlap}"
    );
    both.iter()
        .position(|&(_, b)| b != 0.0)
        .expect("heard on the second device")
}

#[test]
fn a_cue_plays_on_a_patch_whose_device_is_changed_and_is_heard_on_the_new_device_at_once_in_step() {
    let frames = 6000;
    let a = wav(&ramp(frames));
    let mut s = two_device_show(vec![audio(1, &a, 0)]);
    start(&mut s, 1);
    s.run(BLOCK * 20);
    let moved_at = s.rendered() as usize;
    map_patch(&mut s, patch_id(), DEV2_NAME);
    // Still playing, still counting, on the same run of time.
    let mut last = 0.0;
    for _ in 0..6 {
        s.run(BLOCK * 8);
        let p = progress_of(&s.take_outbound(), 1);
        if let Some(&(elapsed, _)) = p.last() {
            assert!(elapsed >= last, "the row's time went back: {p:?}");
            last = elapsed;
        }
        assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    }
    s.run(frames as u64);
    let crossfade = (f64::from(RATE) * 0.01) as usize;
    let switched = assert_heard_in_step(&s, frames, crossfade);
    // At once: within the move's lead and one block of the request.
    let lead = (f64::from(RATE) * 0.1) as usize;
    assert!(
        switched >= moved_at && switched <= moved_at + lead + 2 * BLOCK as usize,
        "moved at {moved_at}, heard there from {switched}"
    );
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    assert_eq!(s.control().live_voices(), 0);
}

/// The file positions (`ramp`: sample `i` heard as `(i + 1) / 2^20`) heard at unity gain on
/// `x`, as (sample, position): two neighbours one step apart, so neither fades.
fn unity_positions(x: &[f32]) -> Vec<(usize, i64)> {
    let mut out = Vec::new();
    for j in 1..x.len() {
        let (a, b) = (x[j - 1], x[j]);
        if a != 0.0 && b != 0.0 && ((b - a) * 1_048_576.0 - 1.0).abs() < 1e-3 {
            out.push((j, (b * 1_048_576.0).round() as i64 - 1));
        }
    }
    out
}

/// A cue on patch B (the second device, its clock `ppm` fast) is moved to the primary after
/// `seconds`; with `starve`, its stream's open is held for its first 0.5 s, so it underruns and
/// plays late by about that much. Returns the file positions the move skipped: the first heard
/// at unity on the new device less the last heard so on the old one (the crossfade between
/// them when the move is in step).
fn moved_gap(ppm: i64, seconds: u64, starve: bool) -> i64 {
    let frames = (seconds as usize + 3) * RATE as usize;
    let path = wav(&ramp(frames));
    let mut s = two_patch_show_with(
        vec![audio_b(2, &path, 0)],
        DEV2_NAME,
        vec![listed(DEV, DEV_NAME, 2), listed(DEV2, DEV2_NAME, 2)],
        |c| {
            c.prime_cues = 0;
            c.streamer.before_open = gated_open;
        },
    );
    s.wait_output(DEV2_NAME);
    s.take_outbound();
    s.fake.set_drift(DEV2, ppm);
    if starve {
        gate(&path);
        s.starved = true;
    }
    start(&mut s, 2);
    if starve {
        s.run(u64::from(RATE) / 2);
        release(&path);
        s.starved = false;
    }
    s.run(u64::from(RATE) * seconds);
    map_b(&mut s, DEV_NAME);
    s.run(u64::from(RATE));
    let old = unity_positions(&s.device_channel(DEV2, 0));
    let new = unity_positions(&s.channel(0));
    let old_last = old.last().expect("heard on the old device").1;
    let new_first = new.first().expect("heard on the new device").1;
    assert!(
        old_last > i64::from(RATE),
        "{old_last}: the old device played"
    );
    new_first - old_last
}

/// A cue playing on a second device whose clock drifts from the master's is moved to another
/// device: the new one carries on where the old one's sound was, whether that device ran fast
/// (it played more than the master clock counted) or slow, as for one that does not drift.
/// The drift is read from the device's smoothed clock, which follows a steady drift 2 s late:
/// at 100 ppm that is 1.6 frames at this rate.
#[test]
fn a_cue_that_has_played_on_a_drifting_second_device_moved_to_another_carries_on_within_a_frame() {
    let in_step = moved_gap(0, 60, false);
    // A minute at 100 ppm: the second device's sound is 48 frames from the master clock's count.
    for ppm in [100, -100] {
        let gap = moved_gap(ppm, 60, false);
        assert!(
            (gap - in_step).abs() <= 2,
            "{ppm} ppm: the move skipped {gap} positions where in step it skips {in_step}"
        );
    }
}

/// A cue whose voice underran (its stream came late) plays late by what it missed; moved to
/// another device it carries on from where its sound is, skipping nothing.
#[test]
fn a_cue_whose_voice_underran_earlier_is_moved_with_no_skip() {
    let in_step = moved_gap(0, 3, false);
    let gap = moved_gap(0, 3, true);
    assert!(
        (gap - in_step).abs() <= 2,
        "after an underrun the move skipped {gap} positions where in step it skips {in_step}"
    );
}

/// How a looping cue devamped and moved was heard: the file positions heard at unity on the
/// old device and on the new one, and the first sample cue 2 (the Devamp cue's next) was heard.
struct LoopedMove {
    old: Vec<i64>,
    new: Vec<i64>,
    next: Option<usize>,
}

/// A 10,000-frame ramp whose first 2,000 frames loop until devamped, the rest (the tail) once,
/// on patch B (the second device, its clock `ppm` fast; with `starve` its stream's open held
/// for its first 0.5 s, so it plays late by about that much). After `seconds` a Devamp cue
/// leaves the loop at the end of the pass and starts cue 2 (on the primary's channel 1). With
/// `moves` the patch is moved to the primary: `after` the leave (the sound in the tail), or
/// else just before the devamp, the pass ending before the move's swap.
fn looped_move(ppm: i64, starve: bool, seconds: u64, moves: bool, after: bool) -> LoopedMove {
    let path = wav(&ramp(10_000));
    let b = wav(&dc(200, 0.5));
    let mut looping = audio_b(1, &path, 0);
    if let CueKind::Audio { slices, .. } = &mut looping.kind {
        *slices = vec![openlab_core::SliceMarker {
            time: 0.25,
            play_count: 0.into(),
        }];
    }
    let devamp = cue(
        3,
        CueKind::Devamp {
            target: Some(id(1)),
            start_next_cue: true,
            stop_target_when_slice_ends: false,
            devamp_mode: openlab_core::DevampMode::CurrentSlice,
        },
    );
    let mut s = two_patch_show_with(
        vec![looping, devamp, audio(2, &b, 1)],
        DEV2_NAME,
        vec![listed(DEV, DEV_NAME, 2), listed(DEV2, DEV2_NAME, 2)],
        |c| {
            c.prime_cues = 0;
            c.streamer.before_open = gated_open;
        },
    );
    s.wait_output(DEV2_NAME);
    s.take_outbound();
    s.fake.set_drift(DEV2, ppm);
    if starve {
        gate(&path);
        s.starved = true;
    }
    start(&mut s, 1);
    if starve {
        s.run(u64::from(RATE) / 2);
        release(&path);
        s.starved = false;
    }
    s.run(u64::from(RATE) * seconds);
    let primary_from = s.channel(1).len();
    if after {
        start(&mut s, 3);
        s.run(2600);
        if moves {
            map_b(&mut s, DEV_NAME);
        }
    } else {
        // Where the sound is in the loop: late in a pass, so it ends between the devamp and
        // the swap a move's lead later.
        loop {
            let at = unity_positions(&s.device_channel(DEV2, 0));
            let pos = at.last().expect("heard on the second device").1;
            if (1150..1350).contains(&pos) {
                break;
            }
            s.run(BLOCK);
        }
        if moves {
            map_b(&mut s, DEV_NAME);
        }
        s.run(64);
        start(&mut s, 3);
    }
    s.run(u64::from(RATE) * 2);
    LoopedMove {
        old: unity_positions(&s.device_channel(DEV2, 0))
            .into_iter()
            .map(|(_, p)| p)
            .collect(),
        new: unity_positions(&s.channel(0))
            .into_iter()
            .map(|(_, p)| p)
            .collect(),
        next: s.channel(1)[primary_from..]
            .iter()
            .position(|&v| v != 0.0)
            .map(|p| p + primary_from),
    }
}

/// The move went on from where the sound was, in the tail: the loop not played again, the
/// tail heard once to its end, within `slack` positions of an in-step move (`in_step`), and
/// cue 2 started where it does with no move (`still`).
fn assert_looped_move(h: &LoopedMove, still: &LoopedMove, in_step: i64, slack: i64, what: &str) {
    let old_last = *h.old.last().expect("heard on the old device");
    let new_first = *h.new.first().expect("heard on the new device");
    assert!(
        old_last >= 2000,
        "{what}: moved before the leave ({old_last})"
    );
    assert!(
        (new_first - old_last - in_step).abs() <= slack,
        "{what}: moved from {old_last} to {new_first} (in step: {in_step} on)"
    );
    assert!(
        h.new.windows(2).all(|w| w[1] > w[0]),
        "{what}: the new device went back (the loop again?)"
    );
    assert_eq!(h.new.last(), Some(&9999), "{what}: the tail's end");
    assert_eq!(h.next, still.next, "{what}: cue 2's start");
    assert!(h.next.is_some(), "{what}: cue 2 never started");
}

/// A looping cue devamped (it leaves the loop at the end of the pass) after its voice underran,
/// then moved to another device while its tail plays, or moved just before the devamp so the
/// loop is left before the move's swap: the loop is left once, the tail heard once from where
/// the sound was, and the next cue starts on time.
#[test]
fn a_looping_cue_that_underran_devamped_and_moved_leaves_its_loop_once() {
    let in_step = moved_gap(0, 3, false);
    for after in [true, false] {
        let h = looped_move(0, true, 2, true, after);
        let still = looped_move(0, true, 2, false, after);
        assert_looped_move(&h, &still, in_step, 2, &format!("underran, after {after}"));
    }
}

/// The same on a second device whose clock runs slow: after a minute at -1000 ppm it has
/// played 480 frames fewer than the master clock counted (the smoothed reading of it lags by
/// 16, hence the slack).
#[test]
fn a_looping_cue_on_a_slow_device_devamped_and_moved_leaves_its_loop_once() {
    let in_step = moved_gap(0, 3, false);
    for after in [true, false] {
        let h = looped_move(-1000, false, 60, true, after);
        let still = looped_move(-1000, false, 60, false, after);
        assert_looped_move(&h, &still, in_step, 20, &format!("slow, after {after}"));
    }
}

#[test]
fn a_cue_put_on_a_patch_on_another_device_moves_there_at_once_in_step() {
    let frames = 4000;
    let a = wav(&ramp(frames));
    let mut s = two_device_show(vec![audio(1, &a, 0)]);
    start(&mut s, 1);
    s.run(BLOCK * 10);
    let mut kind = s
        .control()
        .workspace()
        .find_cue(id(1))
        .unwrap()
        .kind
        .clone();
    if let CueKind::Audio { patch, .. } = &mut kind {
        *patch = patch_b();
    }
    let rid = s.request_id();
    let replies = s.send(ClientMessage::Command {
        id: rid,
        command: Box::new(openlab_core::Command::SetCueProperty {
            id: id(1),
            property: openlab_core::CueProperty::Kind { value: kind },
        }),
        coalesce: None,
    });
    assert!(
        replies.iter().any(|m| matches!(
            m,
            ServerMessage::Result {
                outcome: Outcome::Ok(_),
                ..
            }
        )),
        "{replies:?}"
    );
    s.run(frames as u64);
    let crossfade = (f64::from(RATE) * 0.01) as usize;
    assert_heard_in_step(&s, frames, crossfade);
}

#[test]
fn the_same_while_the_cue_is_paused_it_resumes_on_the_new_device() {
    let frames = 4000;
    let a = wav(&ramp(frames));
    let mut s = two_device_show(vec![audio(1, &a, 0)]);
    start(&mut s, 1);
    s.run(BLOCK * 10);
    s.transport(TransportAction::Pause { cue: id(1) });
    s.run(BLOCK * 4);
    map_patch(&mut s, patch_id(), DEV2_NAME);
    s.run(BLOCK * 8);
    assert_eq!(s.control().engine().state(id(1)), CueState::Paused);
    let heard_two = |s: &DeviceShow| heard(&s.device_channel(DEV2, 0)).len();
    assert_eq!(heard_two(&s), 0, "a paused cue sounded on the new device");
    s.transport(TransportAction::Resume { cue: id(1) });
    s.run(frames as u64);
    // The file in order, every sample once: the first device up to the pause, the second from
    // there on (each pause and resume declicks over a few samples).
    let one: Vec<f32> = s.device_channel(DEV, 0);
    let two: Vec<f32> = s.device_channel(DEV2, 0);
    let unit = 1.0 / 1_048_576.0;
    let index = |v: f32| (v / unit).round() as i64 - 1;
    let last_one = one.iter().rev().find(|&&v| v != 0.0).copied().unwrap();
    let first_two = two.iter().find(|&&v| v != 0.0).copied().unwrap();
    // Within the declicks' few samples of each other, and the second device plays to the end.
    assert!(
        (index(first_two) - index(last_one)).abs() < 64,
        "{} then {}",
        index(last_one),
        index(first_two)
    );
    assert_eq!(
        index(*two.iter().rev().find(|&&v| v != 0.0).unwrap()),
        frames as i64 - 1
    );
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
}

#[test]
fn the_same_while_the_cue_is_fading_the_fade_carries_on_on_the_new_device() {
    let a = wav(&dc(16_000, 0.5));
    let fade = cue(
        3,
        CueKind::Fade {
            target: Some(id(1)),
            duration: secs(16_000),
            curve: openlab_core::FadeCueCurve::like(openlab_core::FadeCurve::Linear),
            levels: openlab_core::FadeLevels {
                master_db: Some(-40.0),
                output_db: vec![],
                crosspoints: vec![],
            },
            stop_target_when_done: false,
            mode: Default::default(),
            geometry: Default::default(),
        },
    );
    let mut s = two_device_show(vec![audio(1, &a, 0), fade]);
    start(&mut s, 1);
    s.run(BLOCK * 4);
    start(&mut s, 3);
    s.run(BLOCK * 20);
    let before = s.channel(0);
    let level = before[before.len() - 1];
    map_patch(&mut s, patch_id(), DEV2_NAME);
    s.run(BLOCK * 40);
    let both = on_both(&s);
    // The level never steps up across the move, and keeps falling on the new device.
    let sum: Vec<f32> = both.iter().map(|&(a, b)| a + b).collect();
    let from = before.len() - 1;
    for w in sum[from..].windows(2) {
        assert!(
            w[1] <= w[0] + 1e-4,
            "level stepped up: {} -> {}",
            w[0],
            w[1]
        );
    }
    let two = s.device_channel(DEV2, 0);
    let now = two[two.len() - 1];
    assert!(now > 0.0 && now < level, "{level} then {now}");
    assert_eq!(s.control().engine().state(id(1)), CueState::Fading);
}

#[test]
fn the_same_while_the_cue_is_looping_it_loops_on_the_new_device() {
    let frames = 800;
    let a = wav(&ramp(frames));
    let mut looping = audio(1, &a, 0);
    if let CueKind::Audio {
        last_slice_play_count,
        ..
    } = &mut looping.kind
    {
        *last_slice_play_count = 0.into();
    }
    let mut s = two_device_show(vec![looping]);
    start(&mut s, 1);
    s.run(frames as u64 * 2 + 300);
    map_patch(&mut s, patch_id(), DEV2_NAME);
    s.run(frames as u64 * 4);
    // Every sample on the master clock is the loop's next one: in step across the move, and
    // round the loop again on the new device.
    let both = on_both(&s);
    let first = both
        .iter()
        .position(|&(a, b)| a != 0.0 || b != 0.0)
        .unwrap();
    let unit = 1.0 / 1_048_576.0;
    for (i, &(a, b)) in both[first..].iter().enumerate() {
        let want = ((i % frames) + 1) as f32 * unit;
        assert!(
            (a + b - want).abs() < 2.0 * unit,
            "sample {i}: {a} + {b}, want {want}"
        );
    }
    let two = s.device_channel(DEV2, 0);
    assert!(two.iter().filter(|&&v| v != 0.0).count() > frames * 2);
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
}

#[test]
fn the_new_device_is_missing_or_cannot_be_used_and_the_cue_stops_as_for_a_lost_device_with_the_reason()
 {
    // A device that offers no rate openLab can play at cannot be used: the cue moved to it
    // stops, shows paused (time frozen) as on a lost device, and a notice says why.
    let a = wav(&dc(20_000, 0.5));
    let odd = listed_at(DEV2, DEV2_NAME, 2, 4_000);
    let mut s = two_patch_show(
        vec![audio(1, &a, 0)],
        DEV_NAME,
        vec![listed(DEV, DEV_NAME, 2), odd],
    );
    start(&mut s, 1);
    s.run(BLOCK * 10);
    s.take_outbound();
    map_patch(&mut s, patch_id(), DEV2_NAME);
    s.settle_devices();
    // Patches resolve again before the stopped voice's end comes back from the mixer: the cue
    // is not stopped, or announced, a second time.
    map_b(&mut s, DEV_NAME);
    s.run(BLOCK * 4);
    let msgs = s.take_outbound();
    assert!(paused_device_lost(&msgs, 1), "{msgs:?}");
    assert_eq!(notice_messages(&msgs, "device_lost").len(), 1, "{msgs:?}");
    // Its row shows one time, standing still where it stopped.
    for m in &msgs {
        if let ServerMessage::Progress { items, .. } = m {
            assert!(
                items.iter().filter(|i| i.cue == id(1)).count() <= 1,
                "{items:?}"
            );
        }
    }
    assert_eq!(s.control().engine().state(id(1)), CueState::Paused);
    let notes = notice_messages(&msgs, "device_lost");
    assert!(
        notes
            .iter()
            .any(|n| n.contains(DEV2_NAME) && n.contains("cannot be used") && n.contains("Hz")),
        "{notes:?}"
    );
    assert!(notice_devices(&msgs).contains(&Some(DEV2_NAME.to_owned())));
    // Nothing sounds any more, and nothing comes back.
    let len = s.channel(0).len();
    s.run(BLOCK * 20);
    assert!(heard(&s.channel(0)[len..]).is_empty());
    assert_eq!(s.control().live_voices(), 0);
}

#[test]
fn a_cue_in_its_post_wait_moved_to_an_unusable_device_has_the_end_of_its_sound_stopped_too() {
    // The engine ends an action whose length it knows up to a block ahead, so a cue shows
    // post-waiting while the last samples of its sound still play. Moved to a device that
    // cannot be used then, it shows paused as on a lost device, and that end is cut with it:
    // nothing of a cue shown as stopped by its device goes on sounding on the old one.
    let frames = BLOCK as usize * 6 + BLOCK as usize / 2;
    let a = wav(&dc(frames, 0.5));
    let mut first = audio(1, &a, 0);
    first.continue_mode = ContinueMode::AutoContinue;
    first.post_wait = 5.0;
    // A device with no rate openLab can play at cannot be used.
    let odd = listed_at(DEV2, DEV2_NAME, 2, 4_000);
    let mut s = two_patch_show(vec![first], DEV_NAME, vec![listed(DEV, DEV_NAME, 2), odd]);
    start(&mut s, 1);
    for _ in 0..20 {
        if s.control().engine().state(id(1)) == CueState::PostWaiting {
            break;
        }
        s.run(BLOCK);
    }
    assert_eq!(s.control().engine().state(id(1)), CueState::PostWaiting);
    assert_eq!(s.control().live_voices(), 1, "its sound is not over yet");
    map_patch(&mut s, patch_id(), DEV2_NAME);
    s.settle_devices();
    assert_eq!(s.control().engine().state(id(1)), CueState::Paused);
    s.run(BLOCK * 4);
    let played = s.channel(0).iter().filter(|&&v| v != 0.0).count();
    assert!(played < frames, "{played} of {frames} samples played");
    assert_eq!(s.control().live_voices(), 0);
}

#[test]
fn a_cue_in_its_post_wait_whose_new_device_is_lost_has_the_end_of_its_sound_stopped_too() {
    // The same end of a sound, still on the device the cue was moved away from when the
    // device it was moved to is lost.
    let frames = BLOCK as usize * 6 + BLOCK as usize / 2;
    let a = wav(&dc(frames, 0.5));
    let mut first = audio(1, &a, 0);
    first.continue_mode = ContinueMode::AutoContinue;
    first.post_wait = 5.0;
    let mut s = two_patch_show(
        vec![first],
        DEV2_NAME,
        vec![listed(DEV, DEV_NAME, 2), listed(DEV2, DEV2_NAME, 2)],
    );
    start(&mut s, 1);
    for _ in 0..20 {
        if s.control().engine().state(id(1)) == CueState::PostWaiting {
            break;
        }
        s.run(BLOCK);
    }
    assert_eq!(s.control().engine().state(id(1)), CueState::PostWaiting);
    assert_eq!(s.control().live_voices(), 1, "its sound is not over yet");
    map_patch(&mut s, patch_id(), DEV2_NAME);
    unplug(&mut s, DEV2);
    assert_eq!(s.control().engine().state(id(1)), CueState::Paused);
    s.run(BLOCK * 4);
    let played = s.channel(0).iter().filter(|&&v| v != 0.0).count();
    assert!(played < frames, "{played} of {frames} samples played");
    assert_eq!(s.control().live_voices(), 0);
}

#[test]
fn a_cue_whose_new_device_is_unplugged_before_it_opens_stops_as_for_a_lost_device() {
    let a = wav(&dc(20_000, 0.5));
    let mut s = two_patch_show(
        vec![audio(1, &a, 0)],
        DEV_NAME,
        vec![listed(DEV, DEV_NAME, 2), listed(DEV2, DEV2_NAME, 2)],
    );
    start(&mut s, 1);
    s.run(BLOCK * 10);
    // Its open hangs; the cue plays on where it is meanwhile.
    s.fake.close_gate();
    map_patch(&mut s, patch_id(), DEV2_NAME);
    s.run(BLOCK * 4);
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    let len = s.channel(0).len();
    assert_eq!(heard(&s.channel(0)[len - BLOCK as usize..]), vec![0.5]);
    s.take_outbound();
    s.fake.unplug(DEV2, false);
    s.fake.open_gate();
    s.wait_wall(Duration::from_secs(1), 2);
    s.run(BLOCK * 2);
    let msgs = s.take_outbound();
    assert_eq!(s.control().engine().state(id(1)), CueState::Paused);
    // Its open failed as it went (or the list lost it first): one notice, naming it.
    let notes = notice_messages(&msgs, "device_lost");
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].contains(DEV2_NAME)
            && (notes[0].contains("not connected") || notes[0].contains("could not be opened")),
        "{notes:?}"
    );
    let len = s.channel(0).len();
    s.run(BLOCK * 10);
    assert!(heard(&s.channel(0)[len..]).is_empty());
}

#[test]
fn a_playing_cue_moved_to_another_device_auto_follows_once_at_its_end() {
    let a = wav(&ramp(3000));
    let b = wav(&dc(400, 0.5));
    let mut first = audio(1, &a, 0);
    first.continue_mode = ContinueMode::AutoFollow;
    let mut s = two_device_show(vec![first, audio(2, &b, 0)]);
    start(&mut s, 1);
    s.run(BLOCK * 10);
    map_patch(&mut s, patch_id(), DEV2_NAME);
    let mut msgs = Vec::new();
    for _ in 0..80 {
        s.run(BLOCK);
        msgs.extend(s.take_outbound());
    }
    let starts = states(&msgs, id(2))
        .iter()
        .filter(|(st, _)| *st == WireCueState::Running)
        .count();
    assert_eq!(starts, 1, "{:?}", states(&msgs, id(2)));
    // Cue 2 begins on the sample after cue 1's last, on the new device.
    let two = s.device_channel(DEV2, 0);
    let last_ramp = two.iter().rposition(|&v| v != 0.0 && v != 0.5).unwrap();
    let first_dc = two.iter().position(|&v| v == 0.5).unwrap();
    assert_eq!(first_dc, last_ramp + 1);
}

/// Sets cue 1's end time to `secs` (as the inspector does); the command must be accepted.
fn set_end_time(s: &mut DeviceShow, secs: f64) {
    let rid = s.request_id();
    let json = format!(
        r#"{{"type":"command","id":{},"command":{{"type":"set_cue_property","id":"{}","property":{{"type":"kind_fields","value":{{"end_time":{secs}}}}}}}}}"#,
        rid.0,
        id(1).0
    );
    let replies = s.send(openlab_core::protocol::decode_client(&json).unwrap());
    assert!(replies.iter().any(|m| matches!(
        m,
        ServerMessage::Result {
            outcome: Outcome::Ok(_),
            ..
        }
    )));
}

/// An end time moved while the cue plays goes with it to another device: moved before its
/// patch moves, or while the move waits to swap, the cue plays to the new end there, never
/// past it on either device, and auto-follows once on the sample after its last.
#[test]
fn a_cue_whose_end_time_is_moved_ends_there_after_a_move_to_another_device() {
    for edit_first in [true, false] {
        let a = wav(&ramp(6000));
        let b = wav(&dc(400, 0.5));
        let mut first = audio(1, &a, 0);
        first.continue_mode = ContinueMode::AutoFollow;
        let mut s = two_device_show(vec![first, audio(2, &b, 0)]);
        start(&mut s, 1);
        s.run(BLOCK * 10);
        let set_end = |s: &mut DeviceShow| set_end_time(s, 0.25);
        if edit_first {
            set_end(&mut s);
            map_patch(&mut s, patch_id(), DEV2_NAME);
        } else {
            map_patch(&mut s, patch_id(), DEV2_NAME);
            set_end(&mut s);
        }
        let mut msgs = Vec::new();
        for _ in 0..80 {
            s.run(BLOCK);
            msgs.extend(s.take_outbound());
        }
        let starts = states(&msgs, id(2))
            .iter()
            .filter(|(st, _)| *st == WireCueState::Running)
            .count();
        assert_eq!(starts, 1, "edit first {edit_first}");
        // Frame 1999 (a quarter second at 8 kHz) is the last heard, on the new device.
        let last = 2000.0 / 1_048_576.0;
        let two = s.device_channel(DEV2, 0);
        let last_ramp = two.iter().rposition(|&v| v != 0.0 && v != 0.5).unwrap();
        assert_eq!(two[last_ramp], last, "edit first {edit_first}");
        let first_dc = two.iter().position(|&v| v == 0.5).unwrap();
        assert_eq!(first_dc, last_ramp + 1, "edit first {edit_first}");
        let one = s.device_channel(DEV, 0);
        assert!(
            one.iter().all(|&v| v <= last),
            "edit first {edit_first}: played past the end on the old device"
        );
    }
}

// A patch on the system output (QLab 5 manual p. 7): it plays on whichever device the Mac's
// default output is, and follows it when the operator changes it.

/// Sends `set_patch_mapping` for `patch` and returns the result's outcome.
fn set_mapping(
    s: &mut DeviceShow,
    patch: openlab_core::PatchId,
    mapping: PatchMapping,
) -> Result<(), ErrorCode> {
    let rid = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id: rid,
        request: Request::SetPatchMapping {
            patch,
            mapping: Some(mapping),
        },
    });
    let outcome = replies
        .iter()
        .find_map(|m| match m {
            ServerMessage::Result { outcome, .. } => Some(outcome.clone()),
            _ => None,
        })
        .expect("a result");
    match outcome {
        Outcome::Ok(_) => Ok(()),
        Outcome::Error(e) => Err(e.code),
    }
}

/// Patch A on "Fake 1" (the primary, not the default output); patch B, empty of cues so far,
/// on "Fake 2", the default output; then patch B is put on the system output.
fn system_output_show(cues: Vec<openlab_core::Cue>) -> DeviceShow {
    let mut two = listed(DEV2, DEV2_NAME, 2);
    two.is_default = true;
    let mut s = two_patch_show(cues, DEV2_NAME, vec![listed(DEV, DEV_NAME, 2), two]);
    s.wait_output(DEV2_NAME);
    assert_eq!(
        set_mapping(&mut s, patch_b(), PatchMapping::system_output(vec![0, 1])),
        Ok(())
    );
    s.settle_devices();
    s.take_outbound();
    s
}

/// The default output becomes device `id`, and the engine's list read sees it.
fn change_system_output(s: &mut DeviceShow, id: &str) {
    s.fake.set_default(Some(id));
    s.wait_wall(Duration::from_millis(1100), 2);
    s.settle_devices();
}

#[test]
fn the_device_list_of_a_patch_has_a_system_output_entry_and_choosing_it_plays_on_the_macs_current_output()
 {
    let b = wav(&dc(400, 0.5));
    let mut s = system_output_show(vec![audio_b(2, &b, 0)]);
    let entries = list_patch_mappings(&mut s);
    let b_entry = entries.iter().find(|e| e.patch == patch_b()).unwrap();
    assert_eq!(
        b_entry.mapping,
        Some(PatchMapping::system_output(vec![0, 1]))
    );
    assert!(
        matches!(
            &b_entry.resolution,
            Some(WireResolution::Mapped { device, .. }) if device == DEV2_NAME
        ),
        "{b_entry:?}"
    );
    start(&mut s, 2);
    s.run(600);
    assert_eq!(heard(&s.device_channel(DEV2, 0)), vec![0.5]);
    assert!(heard(&s.channel(0)).is_empty());
}

#[test]
fn the_macs_output_is_changed_while_no_cue_plays_and_the_next_cue_on_that_patch_plays_on_the_new_device()
 {
    let b = wav(&dc(400, 0.5));
    let mut s = system_output_show(vec![audio_b(2, &b, 0)]);
    change_system_output(&mut s, DEV);
    let entries = list_patch_mappings(&mut s);
    assert!(
        entries.iter().any(|e| e.patch == patch_b()
            && matches!(&e.resolution, Some(WireResolution::Mapped { device, .. }) if device == DEV_NAME)),
        "{entries:?}"
    );
    let before = (s.channel(0).len(), s.device_channel(DEV2, 0).len());
    start(&mut s, 2);
    s.run(600);
    assert_eq!(heard(&s.channel(0)[before.0..]), vec![0.5]);
    let two = s.device_channel(DEV2, 0);
    assert!(heard(&two[before.1.min(two.len())..]).is_empty());
}

#[test]
fn the_macs_output_is_changed_while_a_cue_plays_on_that_patch_and_it_follows_at_once() {
    let frames = 12_000;
    let b = wav(&ramp(frames));
    let mut s = system_output_show(vec![audio_b(2, &b, 0)]);
    start(&mut s, 2);
    s.run(BLOCK * 10);
    // Both devices are captured from here on alike until "Fake 2", no longer used, closes.
    let shift = s.channel(0).len() - s.device_channel(DEV2, 0).len();
    change_system_output(&mut s, DEV);
    assert_eq!(s.control().engine().state(id(2)), CueState::Running);
    s.run(frames as u64 + 2000);
    // Heard in step from its first sample to its last, first on "Fake 2", then on "Fake 1".
    let one = s.device_channel(DEV, 0);
    let two = s.device_channel(DEV2, 0);
    let both: Vec<f32> = (0..one.len())
        .map(|t| {
            one[t]
                + t.checked_sub(shift)
                    .and_then(|i| two.get(i))
                    .copied()
                    .unwrap_or(0.0)
        })
        .collect();
    let first = both.iter().position(|&v| v != 0.0).unwrap();
    let unit = 1.0 / 1_048_576.0;
    for (i, &v) in both[first..first + frames].iter().enumerate() {
        let want = (i + 1) as f32 * unit;
        assert!(
            (v - want).abs() < 2.0 * unit,
            "sample {i}: {v}, want {want}"
        );
    }
    assert!(heard(&one).len() > 1, "never moved to the new default");
    assert_eq!(s.control().engine().state(id(2)), CueState::Idle);
    assert_eq!(s.control().live_voices(), 0);
}

#[test]
fn a_patch_that_names_a_device_is_unaffected_by_the_system_output_changing() {
    let b = wav(&dc(20_000, 0.5));
    let mut two = listed(DEV2, DEV2_NAME, 2);
    two.is_default = true;
    let mut s = two_patch_show(
        vec![audio_b(2, &b, 0)],
        DEV2_NAME,
        vec![listed(DEV, DEV_NAME, 2), two],
    );
    s.wait_output(DEV2_NAME);
    start(&mut s, 2);
    s.run(BLOCK * 10);
    change_system_output(&mut s, DEV);
    s.run(BLOCK * 20);
    // It plays on where it is, every sample, and nothing reaches the new default.
    let ch = s.device_channel(DEV2, 0);
    let first = first_nonzero(&ch).unwrap();
    assert!(ch[first..].iter().all(|&v| v == 0.5), "it dropped out");
    assert!(heard(&s.channel(0)).is_empty());
    assert_eq!(s.control().engine().state(id(2)), CueState::Running);
}

#[test]
fn a_system_output_mapping_naming_a_device_or_with_no_default_output_is_refused() {
    let mut s = two_patch_show(
        vec![],
        DEV2_NAME,
        vec![listed(DEV, DEV_NAME, 2), listed(DEV2, DEV2_NAME, 2)],
    );
    s.wait_output(DEV2_NAME);
    let before = list_patch_mappings(&mut s);
    // No device is the default output.
    assert_eq!(
        set_mapping(&mut s, patch_b(), PatchMapping::system_output(vec![0, 1])),
        Err(ErrorCode::NotFound)
    );
    s.fake.set_default(Some(DEV2));
    s.wait_wall(Duration::from_millis(1100), 2);
    let mut named = PatchMapping::system_output(vec![0, 1]);
    named.device = DEV_NAME.into();
    assert_eq!(
        set_mapping(&mut s, patch_b(), named),
        Err(ErrorCode::BadRequest)
    );
    // Channels the default output does not have.
    assert_eq!(
        set_mapping(&mut s, patch_b(), PatchMapping::system_output(vec![0, 5])),
        Err(ErrorCode::BadRequest)
    );
    assert_eq!(list_patch_mappings(&mut s), before);
}

#[test]
fn a_cue_on_the_system_output_whose_default_goes_away_stops_as_for_a_lost_device() {
    let b = wav(&dc(20_000, 0.5));
    let mut s = system_output_show(vec![audio_b(2, &b, 0)]);
    start(&mut s, 2);
    s.run(BLOCK * 10);
    s.take_outbound();
    // The default output is unplugged; the system names no other.
    s.fake.set_default(None);
    unplug(&mut s, DEV2);
    s.run(BLOCK * 4);
    let msgs = s.take_outbound();
    assert!(paused_device_lost(&msgs, 2), "{msgs:?}");
    assert_eq!(s.control().engine().state(id(2)), CueState::Paused);
    assert!(heard(&s.channel(0)).is_empty());
    // The patch's warning says no output device is selected, as the settings table does; it
    // is not an entry for a device named "system output".
    s.wait_wall(Duration::from_millis(1100), 2);
    s.tick();
    let texts = notice_texts(&s.take_outbound()).unwrap();
    assert!(
        texts
            .iter()
            .any(|(code, m)| code == "patch_no_system_output"
                && m.contains("\"B\"")
                && m.contains("no output device selected")),
        "{texts:?}"
    );
    assert!(
        !texts
            .iter()
            .any(|(_, m)| m.contains("audio device \"system output\"")),
        "{texts:?}"
    );
}

/// A looping cue (2,000-frame ramp, looping until devamped) is devamped 1,300 frames into its
/// third pass, its Devamp cue starting cue 2 when it leaves the loop; its patch moves to "Fake 2"
/// `move_after` frames after the devamp, before (or after) the mixer has said where it leaves.
/// Returns the ramp samples heard in order and whether any was out of step, and how many
/// times cue 2 started.
fn devamp_then_move(move_after: u64) -> (usize, Vec<(usize, f32, f32)>, usize) {
    let frames = 2000;
    let a = wav(&ramp(frames));
    let b = wav(&dc(2000, 0.5));
    let mut looping = audio(1, &a, 0);
    if let CueKind::Audio {
        last_slice_play_count,
        ..
    } = &mut looping.kind
    {
        *last_slice_play_count = 0.into();
    }
    let devamp = cue(
        3,
        CueKind::Devamp {
            target: Some(id(1)),
            start_next_cue: true,
            stop_target_when_slice_ends: false,
            devamp_mode: openlab_core::DevampMode::CurrentSlice,
        },
    );
    let mut s = two_device_show(vec![looping, devamp, audio(2, &b, 0)]);
    start(&mut s, 1);
    let t0 = s.rendered();
    s.run(frames as u64 * 2 - (s.rendered() - t0) + 1300);
    start(&mut s, 3);
    s.run(move_after);
    map_patch(&mut s, patch_id(), DEV2_NAME);
    let mut msgs = Vec::new();
    for _ in 0..200 {
        s.run(BLOCK);
        msgs.extend(s.take_outbound());
    }
    let starts = states(&msgs, id(2))
        .iter()
        .filter(|(st, _)| *st == WireCueState::Running)
        .count();
    let both = on_both(&s);
    let unit = 1.0 / 1_048_576.0;
    let first = both
        .iter()
        .position(|&(a, b)| a != 0.0 || b != 0.0)
        .unwrap();
    let (mut bad, mut i) = (Vec::new(), 0usize);
    for (t, &(x, y)) in both[first..].iter().enumerate() {
        let v = x + y;
        if v == 0.5 || v == 0.0 {
            continue;
        }
        let want = ((i % frames) + 1) as f32 * unit;
        if (v - want).abs() >= 2.0 * unit {
            bad.push((t, v / unit, want / unit));
        }
        i += 1;
    }
    (i, bad, starts)
}

/// Review round 1, H1: a move that comes while a devamp is on its way (sent, not yet answered,
/// or answered and not yet left) waits until the voice has left its loop; the loop is heard
/// exactly three times in all, never a fourth time on the new device, and cue 2 starts once.
#[test]
fn a_cue_moved_to_another_device_while_it_is_being_devamped_leaves_its_loop_once() {
    for move_after in [0, BLOCK, 2 * BLOCK, 400, 600] {
        let (heard, bad, starts) = devamp_then_move(move_after);
        assert_eq!(heard, 6000, "moved {move_after} frames after the devamp");
        assert!(
            bad.is_empty(),
            "moved {move_after} after: {:?}",
            &bad[..bad.len().min(5)]
        );
        assert_eq!(starts, 1, "moved {move_after} after");
    }
}

/// Review round 1: the new device's stream is not ready by the planned swap (a slow disk holds
/// its open 0.15 s): the old device plays on and the swap is planned again from where the sound
/// will be then, so nothing is played twice; the file is heard once, in step.
#[test]
fn a_move_whose_stream_is_late_swaps_later_and_plays_nothing_twice() {
    let frames = 8000;
    let a = wav(&ramp(frames));
    let mut s = two_patch_show_with(
        vec![audio(1, &a, 0)],
        DEV2_NAME,
        vec![listed(DEV, DEV_NAME, 2), listed(DEV2, DEV2_NAME, 2)],
        |c| c.streamer.before_open = gated_open,
    );
    s.wait_output(DEV2_NAME);
    start(&mut s, 1);
    s.run(BLOCK * 10);
    gate(&a);
    s.slow_disk = true;
    map_patch(&mut s, patch_id(), DEV2_NAME);
    s.run(1200);
    assert!(
        heard(&s.device_channel(DEV2, 0)).is_empty(),
        "swapped before its stream could be read"
    );
    release(&a);
    s.slow_disk = false;
    s.run(frames as u64);
    let crossfade = (f64::from(RATE) * 0.01) as usize;
    assert_heard_in_step(&s, frames, crossfade);
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
}

/// A 2.5 s ramp whose first 2 s loop until devamped (a 16,000-frame loop, then a 4,000-frame
/// tail), on patch A (`on_b`: patch B); a Devamp cue starts cue 2 (on patch B, channel 2) as it
/// leaves the loop. Cue 1 is started, devamped 200 frames into its second pass (it leaves at
/// the end of that pass, 32,000 frames in), and the run is past the devamp's answer.
/// Returns the show and the master sample cue 1 started on.
fn long_loop_devamped(on_b: bool) -> (DeviceShow, usize) {
    let a = wav(&ramp(20_000));
    let b = wav(&dc(400, 0.5));
    let mut looping = if on_b {
        audio_b(1, &a, 0)
    } else {
        audio(1, &a, 0)
    };
    if let CueKind::Audio {
        slices,
        last_slice_play_count,
        ..
    } = &mut looping.kind
    {
        *slices = vec![openlab_core::SliceMarker {
            time: 2.0,
            play_count: 0.into(),
        }];
        *last_slice_play_count = 1.into();
    }
    let devamp = cue(
        3,
        CueKind::Devamp {
            target: Some(id(1)),
            start_next_cue: true,
            stop_target_when_slice_ends: false,
            devamp_mode: openlab_core::DevampMode::CurrentSlice,
        },
    );
    let mut s = two_device_show(vec![looping, devamp, audio_b(2, &b, 1)]);
    let started = start(&mut s, 1) as usize;
    s.run(16_200);
    start(&mut s, 3);
    s.run(BLOCK * 2);
    (s, started)
}

/// Review round 2: a cue being devamped on a long loop moves at once (QLab 5, Q450), not when
/// it leaves the loop: heard on the new device within the move's lead, in step, the loop played
/// exactly twice and its tail once, and cue 2 started once, on the sample the loop is left.
#[test]
fn a_cue_being_devamped_on_a_long_loop_moves_to_the_new_device_at_once_and_leaves_on_time() {
    let (mut s, started) = long_loop_devamped(false);
    let moved = s.rendered() as usize;
    map_patch(&mut s, patch_id(), DEV2_NAME);
    let mut msgs = Vec::new();
    for _ in 0..300 {
        s.run(BLOCK * 2);
        msgs.extend(s.take_outbound());
    }
    let one = s.device_channel(DEV, 0);
    let shift = one.len() - s.device_channel(DEV2, 0).len();
    let both = on_both(&s);
    let lead = (f64::from(RATE) * 0.1) as usize;
    let switched = both.iter().position(|&(_, b)| b != 0.0).unwrap();
    assert!(
        switched <= moved + lead + 2 * BLOCK as usize,
        "moved at {moved}, heard on the new device from {switched}"
    );
    assert!(
        heard(&one[switched + 200..]).is_empty(),
        "the old device played on"
    );
    let unit = 1.0 / 1_048_576.0;
    let want = |i: usize| {
        let k = if i < 32_000 {
            i % 16_000
        } else {
            16_000 + i - 32_000
        };
        (k + 1) as f32 * unit
    };
    for (i, &(a, b)) in both[started..started + 36_000].iter().enumerate() {
        assert!(
            (a + b - want(i)).abs() < 2.0 * unit,
            "sample {i}: {a} + {b}, want {}",
            want(i)
        );
    }
    assert!(
        both[started + 36_000..]
            .iter()
            .all(|&(a, b)| a == 0.0 && b == 0.0)
    );
    let two_b = s.device_channel(DEV2, 1);
    let next = two_b.iter().position(|&v| v != 0.0).unwrap() + shift;
    assert_eq!(next, started + 32_000, "cue 2 starts as the loop is left");
    let starts = states(&msgs, id(2))
        .iter()
        .filter(|(st, _)| *st == WireCueState::Running)
        .count();
    assert_eq!(starts, 1);
}

/// Review round 2: the old device lost after a cue being devamped was moved off it: the cue
/// plays on on its new device; it is not left paused by the loss.
#[test]
fn a_cue_moved_off_a_device_while_being_devamped_plays_on_when_that_device_is_lost() {
    let (mut s, _) = long_loop_devamped(true);
    map_patch(&mut s, patch_b(), DEV_NAME);
    s.run(4000);
    unplug(&mut s, DEV2);
    let before = s.channel(0).len();
    let mut msgs = s.take_outbound();
    for _ in 0..40 {
        s.run(BLOCK);
        msgs.extend(s.take_outbound());
    }
    assert!(!paused_device_lost(&msgs, 1), "{msgs:?}");
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    assert!(
        !heard(&s.channel(0)[before..]).is_empty(),
        "silent on its new device"
    );
}

/// Runs of consecutive values in `seq` (each `(first, last)`).
fn runs_of(seq: &[u32]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < seq.len() {
        let mut j = i;
        while j + 1 < seq.len() && seq[j + 1] == seq[j] + 1 {
            j += 1;
        }
        out.push((seq[i], seq[j]));
        i = j + 1;
    }
    out
}

/// What a moved devamped cue was heard as.
#[derive(Debug, PartialEq)]
struct Heard {
    /// The ramp's sample numbers (from 1) heard, in runs, both devices together.
    runs: Vec<(u32, u32)>,
    /// The last sample anything of cue 1 was heard, from its start.
    last: Option<i64>,
    /// Where cue 2 (the Devamp cue's next cue) started, from cue 1's start, and how often.
    next: Option<usize>,
    next_starts: usize,
    state: CueState,
    live: usize,
}

/// A 4,000-frame ramp whose first 2,000 frames loop until devamped and whose 2,000-frame tail
/// plays `tail` times; devamped (stopping it there with `stop`) `devamp_at` frames after its
/// start, in its second pass, so it leaves at 4,000 frames in, and the Devamp cue starts cue 2
/// there. Its patch moves to "Fake 2" `move_at` frames after its start (`None`: never), before
/// or after the devamp; with `slow` the new stream's open is held that many frames (a slow
/// disk). `mid` runs right after the later of the two.
fn devamped_move(
    devamp_at: u64,
    move_at: Option<u64>,
    stop: bool,
    tail: u32,
    slow: u64,
    mid: impl FnOnce(&mut DeviceShow),
) -> Heard {
    let a = wav(&ramp(4000));
    let b = wav(&dc(200, 0.5));
    let mut looping = audio(1, &a, 0);
    if let CueKind::Audio {
        slices,
        last_slice_play_count,
        ..
    } = &mut looping.kind
    {
        *slices = vec![openlab_core::SliceMarker {
            time: 0.25,
            play_count: 0.into(),
        }];
        *last_slice_play_count = tail.into();
    }
    let devamp = cue(
        3,
        CueKind::Devamp {
            target: Some(id(1)),
            start_next_cue: true,
            stop_target_when_slice_ends: stop,
            devamp_mode: openlab_core::DevampMode::CurrentSlice,
        },
    );
    let mut s = two_patch_show_with(
        vec![looping, devamp, audio_b(2, &b, 1)],
        DEV2_NAME,
        vec![listed(DEV, DEV_NAME, 2), listed(DEV2, DEV2_NAME, 2)],
        |c| c.streamer.before_open = gated_open,
    );
    s.wait_output(DEV2_NAME);
    s.take_outbound();
    let started = start(&mut s, 1) as usize;
    let t0 = s.rendered() as usize;
    // Each step `gap` frames after the one before (a tie: the devamp first).
    let mut steps = vec![(devamp_at, false)];
    steps.extend(move_at.map(|m| (m, true)));
    steps.sort_unstable();
    let mut gap = (started + steps[0].0 as usize - t0) as u64;
    let mut last = steps[0].0;
    for (at, moves) in steps {
        gap += at - last;
        last = at;
        if gap > 0 {
            s.run(gap);
        }
        gap = 0;
        if !moves {
            start(&mut s, 3);
            continue;
        }
        if slow > 0 {
            gate(&a);
            s.slow_disk = true;
        }
        map_patch(&mut s, patch_id(), DEV2_NAME);
    }
    mid(&mut s);
    if slow > 0 {
        s.run(slow);
        release(&a);
        s.slow_disk = false;
    }
    let mut msgs = Vec::new();
    for _ in 0..250 {
        s.run(BLOCK);
        msgs.extend(s.take_outbound());
    }
    let both = on_both(&s);
    let unit = 1.0 / 1_048_576.0;
    let seq: Vec<u32> = both
        .iter()
        .map(|&(x, y)| ((x + y) / unit).round() as u32)
        .filter(|&v| v != 0)
        .collect();
    let one = s.device_channel(DEV, 0);
    let shift = one.len() - s.device_channel(DEV2, 0).len();
    let two_b = s.device_channel(DEV2, 1);
    Heard {
        runs: runs_of(&seq),
        last: both
            .iter()
            .rposition(|&(a, b)| a != 0.0 || b != 0.0)
            .map(|p| p as i64 - started as i64),
        next: two_b
            .iter()
            .position(|&v| v != 0.0)
            .map(|p| p + shift - started),
        next_starts: states(&msgs, id(2))
            .iter()
            .filter(|(st, _)| *st == WireCueState::Running)
            .count(),
        state: s.control().engine().state(id(1)),
        live: s.control().live_voices(),
    }
}

/// Heard as wanted: the loop twice, then (unless stopped) the tail `tail` times, every sample
/// once and in time, cue 2 started once on the leave sample, and nothing left playing.
fn assert_devamped_in_step(h: &Heard, stop: bool, tail: u32, what: &str) {
    let mut runs = vec![(1, 2000)];
    let mut last = 3999;
    if stop {
        runs.push((1, 2000));
    } else {
        runs.push((1, 4000));
        for _ in 1..tail {
            runs.push((2001, 4000));
        }
        last = 3999 + 2000 * i64::from(tail);
    }
    assert_eq!(h.runs, runs, "{what}: {h:?}");
    assert_eq!(h.last, Some(last), "{what}: {h:?}");
    assert_eq!((h.next, h.next_starts), (Some(4000), 1), "{what}: {h:?}");
    assert_eq!((h.state, h.live), (CueState::Idle, 0), "{what}: {h:?}");
}

/// Fix check 1: a devamp that stops the cue, a move, a slow disk: the swap planned again falls
/// after the cue has ended, so the move is dropped; nothing of it plays after its end.
#[test]
fn a_move_of_a_cue_a_devamp_stops_whose_stream_is_late_plays_nothing_after_its_end() {
    for move_after in [64, 600] {
        let h = devamped_move(2200, Some(2200 + move_after), true, 1, 1500, |_| {});
        assert_devamped_in_step(&h, true, 1, &format!("moved {move_after} after"));
    }
}

/// Fix check 1: a tail played twice after the loop, a move, a slow disk: the swap planned
/// again after the leave plays the tail's both passes (no devamp used up on the tail).
#[test]
fn a_move_whose_stream_is_late_past_the_leave_plays_the_tail_its_full_count() {
    for move_after in [64, 600] {
        let h = devamped_move(2200, Some(2200 + move_after), false, 2, 1500, |_| {});
        assert_devamped_in_step(&h, false, 2, &format!("moved {move_after} after"));
    }
}

/// Fix check 1: the same show, a fast disk, three times: every render the same, no gap.
#[test]
fn a_devamped_move_renders_the_same_every_time() {
    for n in 0..3 {
        let h = devamped_move(2200, Some(2264), false, 2, 0, |_| {});
        assert_devamped_in_step(&h, false, 2, &format!("render {n}"));
    }
}

/// A slow disk sweeping the re-planned swap across the leave (before, on and after it).
#[test]
fn a_re_planned_swap_on_either_side_of_the_leave_plays_every_sample_once() {
    for move_after in (900..1100).step_by(16) {
        let h = devamped_move(2200, Some(2200 + move_after), false, 1, 900, |_| {});
        assert_devamped_in_step(&h, false, 1, &format!("moved {move_after} after"));
    }
}

/// A click in the waveform between the devamp's answer and the move's swap: the seek takes
/// the move's place (on the new device) from the point clicked inside the loop, and leaves the
/// loop at the end of that pass; cue 2 starts once.
#[test]
fn a_seek_between_the_devamps_answer_and_the_swap_plays_from_the_point_and_leaves_the_loop() {
    let h = devamped_move(2200, Some(2264), false, 1, 0, |s| {
        s.transport(TransportAction::Seek {
            cue: id(1),
            position: 0.1,
        });
    });
    // From the point clicked (frame 800, the 10 ms crossfade in from it aside) to the end.
    let (from, to) = *h.runs.last().unwrap();
    assert!((801..=881).contains(&from) && to == 4000, "{h:?}");
    assert_eq!(h.next_starts, 1, "{h:?}");
    assert_eq!((h.state, h.live), (CueState::Idle, 0), "{h:?}");
}

/// Moves first, then devamps: (move, devamp) frames after cue 1's start, in its second pass
/// (2,000 to 4,000), the end of that pass falling between the devamp and the planned swap (a
/// move's lead, 0.1 s or 800 frames, after the move). The last devamp comes too late to leave
/// on that pass, with or without a move (the commands' lookahead and the answer's way back).
const MOVE_THEN_DEVAMP: [(u64, u64); 4] = [(3300, 64), (3700, 64), (3500, 250), (3850, 64)];

/// Cue 1 of [`devamped_move`] moved, then devamped, at each of [`MOVE_THEN_DEVAMP`] (devamp
/// stopping it with `stop`, a slow disk holding the new stream `slow` frames, `mid` right after
/// the devamp), and the same show with no move: what each was heard as, moved first.
fn moved_then_devamped(stop: bool, slow: u64, mid: impl Fn(&mut DeviceShow)) -> Vec<[Heard; 2]> {
    let at = |(m, d): (u64, u64), moves: bool| {
        devamped_move(m + d, moves.then_some(m), stop, 1, slow, &mid)
    };
    (MOVE_THEN_DEVAMP.into_iter())
        .map(|c| [at(c, true), at(c, false)])
        .collect()
}

/// #788: a looping cue is moved to another device and devamped before the swap, the end of the
/// pass falling before the swap: it leaves the loop at the end of the pass the devamp came in
/// (heard as when devamped first, or not moved), and the Devamp cue's next cue starts on that
/// sample.
#[test]
fn a_looping_cue_moved_then_devamped_before_the_swap_leaves_the_loop_on_that_pass() {
    for stop in [false, true] {
        let heard = moved_then_devamped(stop, 0, |_| {});
        for [moved, still] in &heard {
            assert_eq!(moved, still, "stop {stop}");
        }
        for [h, _] in &heard[..3] {
            assert_devamped_in_step(h, stop, 1, &format!("stop {stop}"));
        }
    }
}

/// #788: the same with the swap delayed by a late stream (planned again): the same result.
#[test]
fn a_looping_cue_moved_then_devamped_whose_swap_is_delayed_leaves_the_loop_on_that_pass() {
    for stop in [false, true] {
        let heard = moved_then_devamped(stop, 1500, |_| {});
        for [moved, still] in &heard {
            assert_eq!(moved, still, "stop {stop}");
        }
        for [h, _] in &heard[..3] {
            assert_devamped_in_step(h, stop, 1, &format!("stop {stop}"));
        }
    }
}

/// #788: the same, paused right after the devamp (before its answer) and resumed 1,000 frames
/// later: it ends, and cue 2 starts, as with no move (the pause and the resume fade a little
/// differently for a move waiting); the loop is left at the end of that pass, the tail plays
/// once and cue 2 starts as it begins.
#[test]
fn a_looping_cue_moved_then_devamped_and_paused_before_the_swap_plays_nothing_extra() {
    let heard = moved_then_devamped(false, 0, |s| {
        s.transport(TransportAction::Pause { cue: id(1) });
        s.run(1000);
        s.transport(TransportAction::Resume { cue: id(1) });
    });
    for [moved, still] in &heard {
        let ends = |h: &Heard| (h.last, h.next, h.next_starts, h.state, h.live);
        assert_eq!(ends(moved), ends(still), "{moved:?}");
    }
    for [h, _] in &heard[..3] {
        // The pause and the resume fade, so only whole runs are read: the first pass, the
        // second up to the pause, then from the resume to the end of the tail.
        let whole: Vec<_> = h.runs.iter().filter(|r| r.1 - r.0 > 3).collect();
        assert_eq!(whole.len(), 3, "{h:?}");
        assert_eq!(
            (*whole[0], whole[1].0, whole[2].1),
            ((1, 2000), 1, 4000),
            "{h:?}"
        );
        let tail_from = h.last.map(|l| l as usize - 1999);
        assert_eq!((h.next, h.next_starts), (tail_from, 1), "{h:?}");
        assert_eq!((h.state, h.live), (CueState::Idle, 0), "{h:?}");
    }
}

/// #788: the same with a panic right after the devamp: heard as with no move (the panic's
/// fade), cue 2 never starts, and nothing is left.
#[test]
fn a_looping_cue_moved_then_devamped_and_panicked_before_the_swap_plays_nothing_extra() {
    let heard = moved_then_devamped(false, 0, |s| {
        s.transport(TransportAction::Panic);
    });
    for [h, still] in &heard {
        assert_eq!(h, still);
        assert_eq!(h.next_starts, 0, "{h:?}");
        assert_eq!((h.state, h.live), (CueState::Idle, 0), "{h:?}");
    }
}

/// #788 review: the same, its end time moved into the tail (0.4 s, 800 frames before the
/// tail's end) right after the devamp, while the move waits: heard as with no move; the loop
/// is left at the end of that pass, the tail plays to the new end and cue 2 starts as the loop
/// is left.
#[test]
fn a_looping_cue_moved_then_devamped_whose_end_time_is_moved_in_the_wait_ends_there() {
    let heard = moved_then_devamped(false, 0, |s| set_end_time(s, 0.4));
    for [moved, still] in &heard {
        assert_eq!(moved, still);
    }
    for [h, _] in &heard[..3] {
        assert_eq!(h.runs, [(1, 2000), (1, 3200)], "{h:?}");
        assert_eq!(
            (h.last, h.next, h.next_starts),
            (Some(5199), Some(4000), 1),
            "{h:?}"
        );
        assert_eq!((h.state, h.live), (CueState::Idle, 0), "{h:?}");
    }
}

/// #788 review: cue 1 of [`devamped_move`] on patch B ("Fake 2", not the clock's device) moved
/// to "Fake 1" 1,300 frames into its second pass and devamped 64 frames later, then "Fake 2"
/// lost before its mixer can answer the devamp: the moved voice still carries the devamp out
/// (the swap comes after the end of the pass the devamp came in, so it leaves at the end of the
/// pass under way then, as a devamp held for the moved voice always did; never loops for
/// ever), the Devamp cue's next cue starts once as the loop is left, and nothing is left.
#[test]
fn a_devamp_whose_old_device_is_lost_before_it_answers_is_carried_out_by_the_moved_voice() {
    let a = wav(&ramp(4000));
    let b = wav(&dc(200, 0.5));
    let mut looping = audio_b(1, &a, 0);
    if let CueKind::Audio {
        slices,
        last_slice_play_count,
        ..
    } = &mut looping.kind
    {
        *slices = vec![openlab_core::SliceMarker {
            time: 0.25,
            play_count: 0.into(),
        }];
        *last_slice_play_count = 1.into();
    }
    let devamp = cue(
        3,
        CueKind::Devamp {
            target: Some(id(1)),
            start_next_cue: true,
            stop_target_when_slice_ends: false,
            devamp_mode: openlab_core::DevampMode::CurrentSlice,
        },
    );
    let mut s = two_device_show(vec![looping, devamp, audio(2, &b, 1)]);
    let started = start(&mut s, 1) as usize;
    let t0 = s.rendered() as usize;
    s.run((started + 3300 - t0) as u64);
    map_patch(&mut s, patch_b(), DEV_NAME);
    s.run(64);
    start(&mut s, 3);
    unplug(&mut s, DEV2);
    let mut msgs = Vec::new();
    for _ in 0..300 {
        s.run(BLOCK);
        msgs.extend(s.take_outbound());
    }
    let one = s.channel(0);
    let unit = 1.0 / 1_048_576.0;
    let seq: Vec<u32> = (one.iter())
        .map(|&x| (x / unit).round() as u32)
        .filter(|&v| v != 0)
        .collect();
    let runs = runs_of(&seq);
    let starts = states(&msgs, id(2))
        .iter()
        .filter(|(st, _)| *st == WireCueState::Running)
        .count();
    // Heard on "Fake 1" from the swap (its fade-in aside) to the end of the tail, once: the
    // loop left at the end of the pass under way, never played again.
    let whole: Vec<_> = runs.iter().filter(|r| r.1 - r.0 > 3).collect();
    assert_eq!(whole.len(), 1, "{runs:?}");
    assert_eq!(whole[0].1, 4000, "{runs:?}");
    let next = s.channel(1).iter().position(|&v| v != 0.0);
    let tail = one.iter().position(|&v| v == 2001.0 * unit);
    assert!(tail.is_some());
    assert_eq!(next, tail, "cue 2 starts as the loop is left");
    assert_eq!(starts, 1, "{msgs:?}");
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    assert_eq!(s.control().live_voices(), 0);
}

/// Fix check 1: a new stream still not ready 0.25 s after the first planned swap (a disk far
/// too slow): the move is given up and the cue plays on, in step, on its old device (until a
/// later patch sync moves it again); nothing is played twice and nothing is left behind.
#[test]
fn a_move_whose_stream_never_comes_in_time_is_given_up_and_the_cue_plays_on_where_it_is() {
    let frames = 8000;
    let a = wav(&ramp(frames));
    let mut s = two_patch_show_with(
        vec![audio(1, &a, 0)],
        DEV2_NAME,
        vec![listed(DEV, DEV_NAME, 2), listed(DEV2, DEV2_NAME, 2)],
        |c| c.streamer.before_open = gated_open,
    );
    s.wait_output(DEV2_NAME);
    let started = start(&mut s, 1) as usize;
    s.run(BLOCK * 10);
    gate(&a);
    s.slow_disk = true;
    map_patch(&mut s, patch_id(), DEV2_NAME);
    s.run(3000);
    release(&a);
    s.slow_disk = false;
    s.run(frames as u64);
    // Every sample once, in step, on whichever device (a later patch sync may move it again
    // once the disk answers, in step as any move).
    let both = on_both(&s);
    let unit = 1.0 / 1_048_576.0;
    for (i, &(a, b)) in both[started..started + frames].iter().enumerate() {
        let want = (i + 1) as f32 * unit;
        assert!(
            (a + b - want).abs() < 2.0 * unit,
            "sample {i}: {a} + {b}, want {want}"
        );
    }
    assert!(
        both[started + frames..]
            .iter()
            .all(|&(a, b)| a == 0.0 && b == 0.0)
    );
    // Nothing reached the new device while the move was given up.
    let shift = s.channel(0).len() - s.device_channel(DEV2, 0).len();
    let first_two = s
        .device_channel(DEV2, 0)
        .iter()
        .position(|&v| v != 0.0)
        .map(|p| p + shift);
    assert!(
        first_two.is_none_or(|p| p > started + 10 * BLOCK as usize + 2000),
        "moved late: {first_two:?}"
    );
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    // No voice left; the only stream held is the cue's prime for its next GO.
    assert_eq!(s.control().live_voices(), 0);
    assert!(s.control().held_streams() <= 1);
}

/// Every stream open asked for, by file (for counting a test's own file's opens).
static OPENED: std::sync::Mutex<Vec<std::path::PathBuf>> = std::sync::Mutex::new(Vec::new());

/// [`gated_open`] that also notes the file.
fn counted_open(path: &std::path::Path) {
    OPENED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(path.to_path_buf());
    gated_open(path);
}

fn opens_of(path: &std::path::Path) -> usize {
    OPENED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|p| p.as_path() == path)
        .count()
}

/// A 16,000-frame ramp (cue 1, on patch A, auto-following into cue 2 on patch B channel 2)
/// moved to "Fake 2" 1,000 frames in, its new stream's open held `held` frames.
struct SlowMove {
    s: DeviceShow,
    path: std::path::PathBuf,
    started: usize,
    msgs: Vec<ServerMessage>,
    /// Tries of the move while its open was held: the times the control thread made a voice
    /// for the new device beside the one that plays (its own count of live voices, read after
    /// every block, so it follows the sample clock alone). The opener threads' count of opens
    /// cannot say this: a try cancelled before an opener got to it opens nothing.
    tries: usize,
}

fn slow_move(held: u64) -> SlowMove {
    let a = wav(&ramp(16_000));
    let b = wav(&dc(400, 0.5));
    let mut first = audio(1, &a, 0);
    first.continue_mode = ContinueMode::AutoFollow;
    let mut s = two_patch_show_with(
        vec![first, audio_b(2, &b, 1)],
        DEV2_NAME,
        vec![listed(DEV, DEV_NAME, 2), listed(DEV2, DEV2_NAME, 2)],
        |c| c.streamer.before_open = counted_open,
    );
    s.wait_output(DEV2_NAME);
    s.take_outbound();
    let started = start(&mut s, 1) as usize;
    s.run(1000);
    gate(&a);
    s.slow_disk = true;
    let (mut tries, mut voices) = (0, s.control().live_voices());
    map_patch(&mut s, patch_id(), DEV2_NAME);
    let mut msgs = Vec::new();
    for _ in 0..held.div_ceil(BLOCK) {
        s.run(BLOCK);
        msgs.extend(s.take_outbound());
        let now = s.control().live_voices();
        if now > voices {
            tries += 1;
        }
        voices = now;
    }
    SlowMove {
        s,
        path: a,
        started,
        msgs,
        tries,
    }
}

impl SlowMove {
    fn release_and_run(&mut self, frames: u64) {
        release(&self.path);
        self.s.slow_disk = false;
        for _ in 0..frames.div_ceil(BLOCK) {
            self.s.run(BLOCK);
            self.msgs.extend(self.s.take_outbound());
        }
    }

    /// Every sample of the ramp heard once, in step, on one device or the other.
    fn assert_in_step(&self) {
        let both = on_both(&self.s);
        let unit = 1.0 / 1_048_576.0;
        let from = self.started;
        for (i, &(a, b)) in both[from..from + 16_000].iter().enumerate() {
            let want = (i + 1) as f32 * unit;
            assert!(
                (a + b - want).abs() < 2.0 * unit,
                "sample {i}: {a} + {b}, want {want}"
            );
        }
    }

    /// Where cue 2 started, from cue 1's start.
    fn next_at(&self) -> Option<usize> {
        let shift = self.s.channel(0).len() - self.s.device_channel(DEV2, 0).len();
        self.s
            .device_channel(DEV2, 1)
            .iter()
            .position(|&v| v != 0.0)
            .map(|p| p + shift - self.started)
    }

    fn warns(&self) -> Vec<String> {
        logs(&self.msgs, LogLevel::Warn)
            .into_iter()
            .filter(|m| m.contains("cue 1"))
            .collect()
    }
}

/// Fix check 2: a move given up on a slow disk is tried again once the disk answers: the cue
/// is heard on the new device, every sample once and in step; its auto-follow starts cue 2 on
/// the sample it ends; one warn line says the move waits.
#[test]
fn a_move_given_up_on_a_slow_disk_is_tried_again_and_its_auto_follow_stays_on_its_sample() {
    let mut m = slow_move(3200);
    m.release_and_run(16_000);
    m.assert_in_step();
    let two = m.s.device_channel(DEV2, 0);
    assert!(
        two.iter().any(|&v| v != 0.0),
        "never heard on the new device"
    );
    assert_eq!(m.next_at(), Some(16_000));
    assert_eq!(m.warns().len(), 1, "{:?}", m.warns());
    assert_eq!(m.s.control().live_voices(), 0);
}

/// Fix check 2: a disk that never answers: the cue plays to its end on its old device, every
/// sample once; the move is tried again a bounded number of times (each try opens one stream at
/// most a few times), and one warn line is written, not one per try.
#[test]
fn a_move_to_a_disk_that_never_answers_plays_to_the_end_where_it_is_with_bounded_retries() {
    let mut m = slow_move(17_000);
    let opens = opens_of(&m.path);
    m.release_and_run(2000);
    m.assert_in_step();
    assert_eq!(m.next_at(), Some(16_000));
    // Tried again, as the control thread counts its tries; and never more opens than a few a
    // try (an upper bound only: a try given up before an opener took it opens nothing).
    assert!((2..=12).contains(&m.tries), "{} tries in 2 s", m.tries);
    assert!(opens <= 12, "{opens} stream opens in 2 s");
    assert_eq!(m.warns().len(), 1, "{:?}", m.warns());
    assert_eq!(m.s.control().live_voices(), 0);
}

/// Fix check 2: a stop, or a panic, while a given-up move waits to be tried again: no voice is
/// left and nothing is moved later.
#[test]
fn a_stop_or_panic_while_a_move_waits_to_be_tried_again_leaves_nothing() {
    for panic in [false, true] {
        let mut m = slow_move(3400);
        assert_eq!(m.warns().len(), 1, "the move was given up first");
        if panic {
            m.s.transport(TransportAction::Panic);
        } else {
            m.s.transport(TransportAction::Stop {
                cue: id(1),
                fade_out: None,
            });
        }
        let before = m.s.device_channel(DEV2, 0).len();
        m.release_and_run(4000);
        assert!(heard(&m.s.device_channel(DEV2, 0)[before..]).is_empty());
        assert_eq!(m.s.control().live_voices(), 0);
        assert_eq!(m.s.control().engine().state(id(1)), CueState::Idle);
    }
}

/// Device rates for the loop-count tests: 1.5 times the primary's, and an awkward one near
/// 48/44.1 of it.
const LOOP_RATES: [u32; 2] = [RATE2, 8_711];

/// Master time of the second device's capture frame `j`, from one pair of counters read
/// together (`master`, `local`): the two run in step.
fn master_time(master: u64, local: u64, rate: u32, j: usize) -> f64 {
    master as f64 + (j as f64 - local as f64) * f64::from(RATE) / f64::from(rate)
}

/// Both counters, read together.
fn counters(s: &DeviceShow) -> (u64, u64) {
    let master = s.control().rendered();
    let local = s
        .control()
        .output_mixer(DEV2_NAME)
        .unwrap()
        .samples_rendered();
    (master, local)
}

/// Review round 1, M1: a 401-frame slice played 1000 times on a device at another rate (its
/// length rounds to whole device frames, which the device repeats every pass) ends on the
/// sample the core counts, and its auto-follow starts on it: within one master frame of the
/// last sample heard, not hundreds of frames off.
#[test]
fn a_loop_played_a_thousand_times_on_a_device_at_another_rate_auto_follows_on_its_last_sample() {
    for rate in LOOP_RATES {
        let a = wav(&dc(401, 0.5));
        let b = wav(&dc(400, 0.5));
        let mut first = audio_b(1, &a, 0);
        first.continue_mode = ContinueMode::AutoFollow;
        if let CueKind::Audio {
            last_slice_play_count,
            ..
        } = &mut first.kind
        {
            *last_slice_play_count = 1000.into();
        }
        let mut s = two_patch_show(
            vec![first, audio(2, &b, 1)],
            DEV2_NAME,
            vec![
                listed(DEV, DEV_NAME, 2),
                listed_at(DEV2, DEV2_NAME, 2, rate),
            ],
        );
        s.wait_output(DEV2_NAME);
        s.run(BLOCK * 3);
        let (master, local) = counters(&s);
        start(&mut s, 1);
        s.run(401 * 1000 + 4000);
        let last = last_nonzero(&s.device_channel(DEV2, 0)).unwrap();
        let end = master_time(master, local, rate, last + 1);
        let next = first_nonzero(&s.channel(1)).unwrap() as f64;
        assert!(
            (next - end).abs() <= 1.0,
            "{rate} Hz: the next cue starts {:.1} frames after the end",
            next - end
        );
    }
}

/// Review round 1, M1: a slice looping until devamped, on a device at another rate, devamped
/// after about a thousand passes: it leaves at the end of a pass and the cue its Devamp cue
/// starts begins once, on that sample.
#[test]
fn a_loop_devamped_after_many_passes_on_a_device_at_another_rate_leaves_on_time_and_starts_the_next_once()
 {
    for rate in LOOP_RATES {
        let a = wav(&dc(401, 0.5));
        let b = wav(&dc(400, 0.25));
        let mut looping = audio_b(1, &a, 0);
        if let CueKind::Audio {
            last_slice_play_count,
            ..
        } = &mut looping.kind
        {
            *last_slice_play_count = 0.into();
        }
        let devamp = cue(
            3,
            CueKind::Devamp {
                target: Some(id(1)),
                start_next_cue: true,
                stop_target_when_slice_ends: false,
                devamp_mode: openlab_core::DevampMode::CurrentSlice,
            },
        );
        let mut s = two_patch_show(
            vec![looping, devamp, audio(2, &b, 1)],
            DEV2_NAME,
            vec![
                listed(DEV, DEV_NAME, 2),
                listed_at(DEV2, DEV2_NAME, 2, rate),
            ],
        );
        s.wait_output(DEV2_NAME);
        s.run(BLOCK * 3);
        let (master, local) = counters(&s);
        start(&mut s, 1);
        s.run(401 * 1000 + 150);
        s.take_outbound();
        start(&mut s, 3);
        let mut msgs = Vec::new();
        for _ in 0..40 {
            s.run(BLOCK);
            msgs.extend(s.take_outbound());
        }
        let two = s.device_channel(DEV2, 0);
        let last = last_nonzero(&two).unwrap();
        // It left at the end of a whole pass, as the device plays a pass.
        let played = last + 1 - first_nonzero(&two).unwrap();
        let pass = two
            .iter()
            .skip(first_nonzero(&two).unwrap())
            .take_while(|&&v| v != 0.0)
            .count();
        assert_eq!(played, pass, "{rate} Hz: one stretch of sound");
        // A pass on the device is the slice's 401 frames at its rate, rounded to a frame.
        let pass_frames = openlab_audio::frames_at_rate(401, RATE, rate) as usize;
        assert_eq!(played % pass_frames, 0, "{rate} Hz: {played} frames");
        assert!(played / pass_frames > 900, "{rate} Hz: {played} frames");
        // The leave is dated on the master sample nearest the device's (half a frame), and the
        // device's counter read with the master's is a whole frame of its own (up to one more).
        let end = master_time(master, local, rate, last + 1);
        let next = first_nonzero(&s.channel(1)).unwrap() as f64;
        assert!(
            (next - end).abs() <= 1.5,
            "{rate} Hz: the next cue starts {:.1} frames after the end",
            next - end
        );
        let starts = states(&msgs, id(2))
            .iter()
            .filter(|(st, _)| *st == WireCueState::Running)
            .count();
        assert_eq!(starts, 1, "{rate} Hz");
    }
}

/// Review round 1, M1: a cue looping on a device at another rate for 300 passes, then moved to
/// the primary, goes on there from where its sound is: the file position heard on the primary
/// after the swap is the one the second device had reached, within two frames.
#[test]
fn a_cue_looping_many_passes_on_a_device_at_another_rate_moves_in_step() {
    for rate in LOOP_RATES {
        let frames = 401;
        let a = wav(&ramp(frames));
        let mut looping = audio_b(1, &a, 0);
        if let CueKind::Audio {
            last_slice_play_count,
            ..
        } = &mut looping.kind
        {
            *last_slice_play_count = 0.into();
        }
        let mut s = two_patch_show(
            vec![looping],
            DEV2_NAME,
            vec![
                listed(DEV, DEV_NAME, 2),
                listed_at(DEV2, DEV2_NAME, 2, rate),
            ],
        );
        s.wait_output(DEV2_NAME);
        s.run(BLOCK * 3);
        let (master, local) = counters(&s);
        start(&mut s, 1);
        s.run(frames as u64 * 300);
        map_patch(&mut s, patch_b(), DEV_NAME);
        s.run(4_000);
        let unit = 1.0 / 1_048_576.0;
        let one = s.channel(0);
        let two = s.device_channel(DEV2, 0);
        let swap = first_nonzero(&one).expect("heard on the primary");
        // The file position the second device plays, from a frame mid-pass shortly before the
        // swap (a resampled ramp reads its position back away from the wrap).
        let (j, q) = (0..two.len())
            .rev()
            .filter(|&j| master_time(master, local, rate, j) < swap as f64 - 50.0)
            .map(|j| (j, f64::from(two[j] / unit) - 1.0))
            .find(|&(_, q)| (100.0..300.0).contains(&q))
            .unwrap();
        let tj = master_time(master, local, rate, j);
        // Past the crossfade, the primary plays the file exactly: its position read back.
        let mut worst = 0.0f64;
        for (t, &v) in one.iter().enumerate().skip(swap + 100).take(300) {
            let got = f64::from(v / unit) - 1.0;
            let want = (q + (t as f64 - tj)).rem_euclid(frames as f64);
            let mut d = (got - want).abs();
            d = d.min(frames as f64 - d);
            worst = worst.max(d);
        }
        assert!(worst <= 2.0, "{rate} Hz: out of step by {worst:.1} frames");
    }
}

/// A QLab workspace with one audio patch of `outputs` cue outputs on the system output, and
/// one audio cue on it, written to `path`.
fn qlab_system_patch_file(path: &std::path::Path, outputs: i64) {
    use crate::qlab_archive::{Archive, V, cue, qlab_file};
    let mut top = Archive::new();
    let device = {
        let (name, uid) = (top.string("Built-in"), top.string("uid"));
        top.dict_str(&[
            ("name", name),
            ("deviceUID", uid),
            ("outputChannels", V::Int(2)),
        ])
    };
    let (patch_id, patch_name) = (top.string("P1"), top.string("Main"));
    let patch = top.object(
        "AudioOutputPatch",
        &[
            ("uniqueID", patch_id),
            ("name", patch_name),
            ("cueOutputChannels", V::Int(outputs)),
            ("deviceDescription", device),
            ("useSystemDevice", V::Bool(true)),
        ],
    );
    let patches = top.array(&[patch]);
    let audio = top.dict_str(&[("audioOutputPatches", patches)]);
    let settings = top.dict_str(&[("Audio", audio)]);
    let mut cues = Archive::new();
    let (number, patch) = (cues.string("1"), cues.string("P1"));
    let tone = cue(
        &mut cues,
        "AudioCue",
        "A1",
        &[("number", number), ("audioOutputPatchID", patch)],
    );
    std::fs::write(
        path,
        qlab_file("Sys", top, Some(settings), cues, vec![("Main", vec![tone])]),
    )
    .unwrap();
}

/// Imports a QLab workspace whose one patch has 8 cue outputs on the system output with
/// `channels` chosen for it, on a machine whose default device has two outputs. Returns the
/// mapping stored for the imported patch, its resolution and the imported cue's problems.
fn import_system_patch(
    channels: Vec<u32>,
) -> (
    Option<PatchMapping>,
    Option<WireResolution>,
    Vec<openlab_core::CueProblem>,
) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Sys.qlab5");
    qlab_system_patch_file(&path, 8);
    let mut two = listed(DEV2, DEV2_NAME, 2);
    two.is_default = true;
    let mut s = two_patch_show(vec![], DEV2_NAME, vec![listed(DEV, DEV_NAME, 2), two]);
    s.wait_output(DEV2_NAME);
    let id = s.request_id();
    let before = s.control().doc_ids();
    s.send_until(
        ClientMessage::Request {
            id,
            request: Request::ImportQlab {
                path: path.to_string_lossy().into_owned(),
                cues: vec![],
                other_cues: openlab_core::QlabCueAction::Placeholder,
                audio_patches: vec![openlab_core::QlabPatchChoice {
                    qlab_id: "P1".into(),
                    mapping: Some(PatchMapping::system_output(channels)),
                }],
                video_outputs: vec![],
            },
        },
        |c| c.doc_ids().len() > before.len(),
    );
    let doc = *s
        .control()
        .doc_ids()
        .iter()
        .find(|d| !before.contains(d))
        .unwrap();
    let view = s.control().doc(doc).unwrap();
    let snap = view.snapshot();
    let (ws, patch) = (snap.workspace.id, snap.workspace.patches[0].id);
    let cue = snap.workspace.cues().next().map(|c| c.id).unwrap();
    // The fixture's audio cue has no file, so it is broken for that alone, not for its patch.
    let problems = view.engine().problems(cue).to_vec();
    let mapping = s.control().machine().patch_mapping(ws, patch).cloned();
    // The imported document is the one the client has open on a real run; here the harness's
    // client is on its first document, so ask the engine for the imported doc's resolution.
    let resolution = s
        .control()
        .patch_resolutions(doc)
        .into_iter()
        .find(|(p, _)| *p == patch)
        .map(|(_, r)| r);
    let logged = logs(&s.take_outbound(), LogLevel::Warn);
    assert!(
        !logged
            .iter()
            .any(|l| l.contains("device choice was not kept")),
        "{logged:?}"
    );
    (mapping, resolution, problems)
}

#[test]
fn i1_an_imported_audio_patch_matched_to_the_system_output_keeps_it_however_many_outputs_qlab_gave_it()
 {
    // QLab patches often have more cue outputs than the Mac's speakers have channels: the
    // mapping keeps the outputs the default device has instead of being refused whole.
    let (mapping, resolution, problems) = import_system_patch((0..8).collect());
    assert_eq!(mapping, Some(PatchMapping::system_output(vec![0, 1])));
    assert_eq!(
        resolution,
        Some(WireResolution::Mapped {
            device: DEV2_NAME.into(),
            channels: vec![0, 1],
        })
    );
    assert_eq!(problems, vec![openlab_core::CueProblem::NoMedia]);
}
