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

use crate::show::device::{DeviceShow, Fake, FakeLayer, listed};
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
    // The snapshot, its result and the doc's video outputs.
    assert_eq!(replies.len(), 3);
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

/// The device goes while cue 1 plays and stays away `outage`. Whatever the length of the
/// outage: cue 1 stops at once and does not come back, its auto-follow never fires, Resume All
/// does nothing, and GO plays it from its start.
fn lost_cue_stays_stopped(outage: Duration) {
    let mut s = follow_show();
    s.run(BLOCK * 2);
    let at = go(&mut s);
    s.run(640);
    let cut = s.rendered() as usize;
    s.take_outbound();
    lose(&mut s);
    // At once: a run-time break, voice released, nothing left to play.
    let msgs = s.take_outbound();
    assert!(broken_device_lost(&msgs, 1));
    assert_eq!(s.control().engine().state(id(1)), CueState::Broken);
    s.transport(TransportAction::ResumeAll);
    assert_eq!(s.control().engine().state(id(1)), CueState::Broken);

    s.wait_wall(outage, 8);
    replug(&mut s);
    s.run(BLOCK * 4);
    // The patch maps again: the cue is idle (the break clears), nothing resumed.
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    assert_eq!(s.control().engine().state(id(2)), CueState::Idle);
    s.transport(TransportAction::ResumeAll);
    s.run(BLOCK * 4);
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
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

#[test]
fn a_cue_playing_when_the_device_goes_stops_and_stays_stopped_after_3_s() {
    lost_cue_stays_stopped(Duration::from_secs(3));
}

#[test]
fn a_cue_playing_when_the_device_goes_stops_and_stays_stopped_after_20_s() {
    lost_cue_stays_stopped(Duration::from_secs(20));
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
    assert!(broken_device_lost(&msgs, 1));
    assert!(broken_device_lost(&msgs, 2));
    // The pre-waiting cue is routed to the device too: it ends with the others (no voice yet).
    assert!(broken_device_lost(&msgs, 3));
    // The idle cue routed to the device has no run: it shows broken for the missing device (a
    // static problem, no reason).
    assert_eq!(states(&msgs, id(4)), vec![(WireCueState::Broken, None)]);
    for n in [1, 2, 3] {
        assert_eq!(
            s.control().engine().broken_reason(id(n)),
            Some(openlab_core::BrokenReason::DeviceLost)
        );
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
    // While the device is away, a cue routed to it does not start: broken, as a missing device.
    s.transport(TransportAction::Start { cue: id(4) });
    let msgs = s.take_outbound();
    assert!(warnings(&msgs).contains(&WireWarning::CueBroken { cue: id(4) }));

    replug(&mut s);
    let msgs = s.take_outbound();
    // Back: the cues un-break (idle); nothing they played resumes.
    for n in [1, 2, 3, 4] {
        assert_eq!(
            states(&msgs, id(n)).last(),
            Some(&(WireCueState::Idle, None)),
            "cue {n}"
        );
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
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
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
        assert!(broken_device_lost(&s.take_outbound(), 1), "outage {outage}");
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
        assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
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
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
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
    assert!(broken_device_lost(&s.take_outbound(), 2));
    s.wait_wall(Duration::from_secs(3), 4);
    replug(&mut s);
    s.run(4000);
    for n in [1, 2, 10] {
        assert_eq!(s.control().engine().state(id(n)), CueState::Idle, "cue {n}");
    }
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
        assert!(
            matches!(state, CueState::Idle),
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
    assert!(broken_device_lost(&msgs, 1));
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
    // Voices released, cues un-break; nothing resumed by itself.
    assert_eq!(s.control().live_voices(), 0);
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
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

#[test]
fn a_patch_left_unmapped_by_a_small_device_is_mapped_when_a_bigger_one_is_back() {
    let ws = workspace(vec![]);
    let mut c = device_config(&ws);
    c.machine = MachineSettings::default();
    let mut s = DeviceShow::new(ws.clone(), vec![listed(DEV, DEV_NAME, 1)], c);
    let msgs = s.take_outbound();
    assert_eq!(logs(&msgs, LogLevel::Warn).len(), 1);
    assert_eq!(s.control().machine().patch_mapping(ws.id, patch_id()), None);
    lose(&mut s);
    replug(&mut s);
    let m = s
        .control()
        .machine()
        .patch_mapping(ws.id, patch_id())
        .cloned()
        .expect("mapped on the bigger device");
    assert_eq!((m.device.as_str(), m.channels), (DEV_NAME, vec![0, 1]));
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
            .any(|l| l.contains("mapped patch 'Main Outputs' to Fake 1 outputs 1\u{2013}2"))
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

/// A device that cannot play at the primary's rate is not opened (once warned): the patches
/// mapped to it are `device_missing` and their cues broken; the other patches play.
#[test]
fn a_device_that_cannot_play_at_the_primary_rate_is_not_opened() {
    let a = wav(&dc(200, 0.25));
    let b = wav(&dc(200, 0.5));
    let mut other = listed(DEV2, DEV2_NAME, 2);
    for c in &mut other.configs {
        c.min_rate = RATE * 2;
        c.max_rate = RATE * 2;
    }
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
        .filter(|m| m.contains("cannot play at"))
        .collect();
    assert_eq!(warned.len(), 1, "{warned:?}");
    assert_eq!(s.control().engine().state(id(2)), CueState::Broken);
    // Warned once, however often it is looked at again.
    s.wait_wall(Duration::from_secs(1), 2);
    assert!(
        !logs(&s.take_outbound(), LogLevel::Warn)
            .iter()
            .any(|m| m.contains("cannot play at"))
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
    assert!(broken_device_lost(&msgs, 2));
    assert!(!broken_device_lost(&msgs, 1));
    assert!(!broken_device_lost(&msgs, 3));
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
    assert_eq!(s.control().engine().state(id(2)), CueState::Idle);
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
    assert!(broken_device_lost(&msgs, 1));
    assert!(!broken_device_lost(&msgs, 2));
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
    // Cue 1 plays again from GO.
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
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
    assert!(broken_device_lost(&msgs, 2));
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
        s.control().engine().broken_reason(id(1)),
        s.control().engine().state(id(3)),
    );
    let heard3 = heard(&s.device_channel(DEV2, 1)[dev2_before..]);
    s.fake.open_gate();
    assert!(
        after.0 > at + 40_000,
        "the master clock ran on: {at} -> {}",
        after.0
    );
    assert_eq!(after.1, CueState::Broken);
    assert_eq!(after.2, Some(openlab_core::BrokenReason::DeviceLost));
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
        s.control().engine().broken_reason(id(2)),
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
        broken_device_lost(&msgs, 2),
        "{:?}",
        logs(&msgs, LogLevel::Error)
    );
    assert_eq!(states.0, CueState::Running);
    assert_eq!(states.1, Some(openlab_core::BrokenReason::DeviceLost));
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
        s.control().engine().broken_reason(id(1)),
        s.control().engine().state(id(3)),
        s.control().device_down(),
    );
    s.fake.open_gate();
    assert!(broken_device_lost(&msgs, 1));
    assert_eq!(states.0, Some(openlab_core::BrokenReason::DeviceLost));
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

/// A device no patch maps to any more closes once the cue still playing on it ends; unplugged
/// then, it raises no warning, no error and no reopen attempts.
#[test]
fn a_device_no_patch_uses_closes_and_its_unplugging_raises_nothing() {
    let b = wav(&dc(800, 0.5));
    let mut s = two_device_show(vec![audio_b(2, &b, 0)]);
    start(&mut s, 2);
    s.run(BLOCK * 2);
    map_b(&mut s, DEV_NAME);
    s.run(BLOCK * 2);
    // The cue on it plays to its end there first.
    assert!(s.control().output_up(DEV2_NAME));
    s.run(1200);
    assert_eq!(s.control().engine().state(id(2)), CueState::Idle);
    s.settle_devices();
    assert!(
        !s.control().output_up(DEV2_NAME),
        "the unused device stayed open"
    );
    // A playing cue does not move to its patch's new device: it played on to its end where it
    // started, and the primary heard none of it.
    assert_eq!(heard(&s.device_channel(DEV2, 0)), vec![0.5]);
    assert_eq!(
        s.device_channel(DEV2, 0)
            .iter()
            .filter(|&&x| x != 0.0)
            .count(),
        800
    );
    assert!(heard(&s.channel(0)).is_empty());
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

/// A patch mapped to channels a second device has but openLab cannot use yet (above the
/// primary's output count) is warned about truthfully: not as a device with too few channels.
#[test]
fn channels_above_the_primarys_output_count_are_warned_about_truthfully() {
    let mut ws = workspace(vec![]);
    let mut patch = ws.patches[0].clone();
    patch.id = patch_b();
    patch.name = "B".into();
    ws.patches.push(patch);
    let mut c = device_config(&ws);
    c.machine
        .set_patch_mapping(
            ws.id,
            patch_b(),
            PatchMapping {
                device: DEV2_NAME.into(),
                channels: vec![4, 5],
            },
        )
        .unwrap();
    let mut s = DeviceShow::new(
        ws,
        vec![listed(DEV, DEV_NAME, 2), listed(DEV2, DEV2_NAME, 8)],
        c,
    );
    s.wait_output(DEV2_NAME);
    s.tick();
    let warnings: Vec<String> = s
        .take_outbound()
        .iter()
        .filter_map(|m| match m {
            ServerMessage::Warnings { warnings } => Some(warnings.clone()),
            _ => None,
        })
        .next_back()
        .unwrap()
        .into_iter()
        .filter_map(|w| match w {
            WireWarning::Notice { code, message, .. } if code == "outputs_dropped" => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(
        warnings,
        vec![format!(
            "patch \"B\" maps channel 6 of \"{DEV2_NAME}\", but openLab uses only its first 2 \
             outputs (as many as the primary device has): its cues cannot play"
        )]
    );
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
    assert!(broken_device_lost(&s.take_outbound(), 2));
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
                device: DEV2_NAME.into()
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
            device: DEV2_NAME.into()
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
