// SPDX-License-Identifier: GPL-3.0-or-later

//! The control loop and its output device (M4.2b, M4.2g; NFR-08, AUD-01, AUD-03, WS-07): the
//! device list, hot-plug, loss and reopen, the device-loss policy (cues on a lost device stop
//! at once and do not come back, as in QLab 5), output delays and dropped outputs. A stand-in device layer (`show::device`) replaces cpal; a test-driven wall clock
//! replaces time.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod show;

use std::time::Duration;

use openlab_core::protocol::{
    ClientMessage, DeviceEntry, LogLevel, Outcome, Request, ResultData, RuntimeChange,
    ServerMessage, TransportAction, WireBrokenReason, WireCueState, WireProblem, WireWarning,
};
use openlab_core::{ContinueMode, CueState, MachineSettings, PatchMapping, Workspace};
use openlab_engine::control::ControlConfig;
use std::sync::Arc;

use openlab_engine::control::devices::{self, DEVICE_JOB_TIMEOUT, STABLE_UP};
use show::device::{DeviceShow, Fake, FakeLayer, listed};
use show::*;

const DEV: &str = "fake:1";
const DEV_NAME: &str = "Fake 1";

fn device_config(ws: &Workspace) -> ControlConfig {
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
            Some(ResultData::Devices { devices }) => devices.clone(),
            other => panic!("{other:?}"),
        },
        Outcome::Error(e) => panic!("{e:?}"),
    }
}

fn entry(name: &str, output_channels: u32) -> DeviceEntry {
    DeviceEntry {
        name: name.into(),
        output_channels,
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
    assert_eq!(list_devices(&mut s), vec![entry(DEV_NAME, 2)]);
    s.take_outbound();

    // Another device comes and goes: the list follows; the patches do not change.
    s.fake.plug(listed("fake:2", "Fake 2", 8));
    s.wait_wall(Duration::from_secs(1), 1);
    assert_eq!(
        list_devices(&mut s),
        vec![entry(DEV_NAME, 2), entry("Fake 2", 8)]
    );
    s.fake.unplug("fake:2", false);
    s.wait_wall(Duration::from_secs(1), 1);
    assert_eq!(list_devices(&mut s), vec![entry(DEV_NAME, 2)]);
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
    assert_eq!(list_devices(&mut s), vec![entry(DEV_NAME, 2)]);
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
    assert_eq!(list_devices(&mut s), vec![entry(DEV_NAME, 2)]);
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
fn patch_output_delays_are_applied_on_the_device_outputs() {
    let path = wav(&dc(200, 0.5));
    let mut ws = workspace(vec![audio(1, &path, 0), audio(2, &path, 1)]);
    // 10 ms at 8 kHz: 80 frames on output 0; output 1 undelayed.
    ws.patches[0].outputs[0].delay_ms = 10.0;
    let mut s = show(ws);
    s.run(BLOCK);
    let at = s.now() as usize;
    s.transport(TransportAction::Start { cue: id(1) });
    s.transport(TransportAction::Start { cue: id(2) });
    s.run(800);
    assert_eq!(first_nonzero(&s.channel(0)), Some(at + 80));
    assert_eq!(last_nonzero(&s.channel(0)), Some(at + 80 + 199));
    assert_eq!(first_nonzero(&s.channel(1)), Some(at));
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
        vec![entry(DEV_NAME, 2), entry("Fake 2", 8)]
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
    assert_eq!(list_devices(&mut s), vec![entry(DEV_NAME, 2)]);
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

// ---- The device-loss policy (M4.2g, QLab 5: PROGRESS check X1) ----

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
    let pre_wait_from = s.now();
    start(&mut s, 3);
    s.run(BLOCK);
    s.take_outbound();
    let cut = s.rendered();
    lose(&mut s);
    let msgs = s.take_outbound();
    assert!(broken_device_lost(&msgs, 1));
    assert!(broken_device_lost(&msgs, 2));
    assert!(
        states(&msgs, id(3)).is_empty(),
        "the pre-waiting cue has no voice"
    );
    // The idle cue routed to the device has no run: it shows broken for the missing device (a
    // static problem, no reason).
    assert_eq!(states(&msgs, id(4)), vec![(WireCueState::Broken, None)]);
    assert_eq!(s.control().engine().state(id(3)), CueState::PreWaiting);
    for n in [1, 2] {
        assert_eq!(
            s.control().engine().broken_reason(id(n)),
            Some(openlab_core::BrokenReason::DeviceLost)
        );
    }
    assert!(
        logs(&msgs, LogLevel::Error)
            .iter()
            .any(|m| m.contains("2 cue(s)"))
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
    for n in [1, 2, 4] {
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
    // The pre-wait went on from where it stood (sample time stood still): cue 3 had no voice
    // on the device yet, so it starts on its sample.
    s.run(1200);
    assert_eq!(
        first_nonzero(&s.channel(0)[cut as usize..]).map(|i| i + cut as usize),
        Some(pre_wait_from as usize + 1000)
    );
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
