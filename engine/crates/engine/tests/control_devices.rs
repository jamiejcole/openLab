// SPDX-License-Identifier: GPL-3.0-or-later

//! The control loop and its output device (M4.2b, M3.22; NFR-08, AUD-01, AUD-03, WS-07): the
//! device list, hot-plug, loss and reopen, the outage policy, output delays and dropped
//! outputs. A stand-in device layer (`show::device`) replaces cpal; a test-driven wall clock
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

use openlab_engine::control::devices::{self, DEVICE_JOB_TIMEOUT, DEVICE_LOSS_GRACE, STABLE_UP};
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

fn any_broken(msgs: &[ServerMessage]) -> bool {
    changes(msgs).iter().any(|c| {
        matches!(
            c,
            RuntimeChange::CueState {
                state: WireCueState::Broken,
                ..
            }
        )
    })
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
    // Within the grace period nothing is broken.
    assert!(problems(&msgs, 1).is_empty());

    // Past it, the patch is reported missing: the idle cue on it shows broken.
    s.wait_wall(DEVICE_LOSS_GRACE, 4);
    let msgs = s.take_outbound();
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

/// Two auto-following cues on a ramp (every sample distinct), rendered on the device: with
/// `outage`, the device is lost mid-way through cue 1 for 5 s and comes back.
fn play_through(outage: bool) -> (DeviceShow, u64) {
    play_through_with(outage, None)
}

/// [`play_through`]; with `reopen_block` the device comes back with another buffer size.
fn play_through_with(outage: bool, reopen_block: Option<u32>) -> (DeviceShow, u64) {
    let a = wav(&ramp(1500));
    let b = wav(&ramp(700));
    let mut c1 = audio(1, &a, 0);
    c1.continue_mode = ContinueMode::AutoFollow;
    let mut s = device_show(workspace(vec![c1, audio(2, &b, 1)]));
    s.run(BLOCK * 2);
    let at = go(&mut s);
    s.run(640);
    if outage {
        lose(&mut s);
        // Reopen attempts fail meanwhile (the device is not there).
        s.wait_wall(Duration::from_secs(5), 10);
        assert!(s.fake.opens() > 1);
        assert!(s.control().device_down());
        if let Some(b) = reopen_block {
            s.fake.set_block(b);
        }
        replug(&mut s);
    }
    s.run(3000);
    (s, at)
}

#[test]
fn a_device_back_within_the_grace_period_resumes_on_the_sample_it_was_cut() {
    let (mut reference, at_ref) = play_through(false);
    let (mut s, at) = play_through(true);
    assert_eq!(at, at_ref);
    // Sample time stood still through the outage: the capture is the same, sample for sample,
    // and cue 2 follows cue 1 on its exact end sample, as without the outage.
    for ch in 0..2 {
        assert_eq!(s.channel(ch), reference.channel(ch), "channel {ch}");
    }
    let ch1 = s.channel(1);
    assert_eq!(first_nonzero(&ch1), Some(at as usize + 1500));
    assert_eq!(ch1[at as usize + 1500], ramp(1)[0]);
    let msgs = s.take_outbound();
    assert!(!any_broken(&msgs));
    assert!(notices(&msgs).contains(&"device_lost".to_owned()));
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    assert_eq!(s.control().engine().state(id(2)), CueState::Idle);
    assert_eq!(s.control().live_voices(), 0);
    reference.take_outbound();
}

#[test]
fn a_device_lost_beyond_the_grace_period_fails_exactly_the_runs_on_it() {
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
    // Not yet: running cues stay running through the grace period.
    s.wait_wall(DEVICE_LOSS_GRACE - Duration::from_millis(100), 5);
    let msgs = s.take_outbound();
    assert!(!any_broken(&msgs));
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    s.wait_wall(Duration::from_millis(200), 1);
    let msgs = s.take_outbound();
    assert!(broken_device_lost(&msgs, 1));
    assert!(broken_device_lost(&msgs, 2));
    assert!(
        states(&msgs, id(3)).is_empty(),
        "the pre-waiting cue has no voice"
    );
    // The idle cue on the device has no run: it shows broken for the missing device (a static
    // problem, no reason).
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
    // While the device is still away, a cue on it does not start: broken, as a missing device.
    s.transport(TransportAction::Start { cue: id(4) });
    let msgs = s.take_outbound();
    assert!(warnings(&msgs).contains(&WireWarning::CueBroken { cue: id(4) }));

    replug(&mut s);
    let msgs = s.take_outbound();
    // Back: the failed cues are playable again; nothing they played resumes.
    for n in [1, 2] {
        assert_eq!(
            states(&msgs, id(n)),
            vec![(WireCueState::Idle, None)],
            "cue {n}"
        );
    }
    s.run(BLOCK * 2);
    assert_eq!(s.control().live_voices(), 0, "the failed voices are freed");
    assert_eq!(s.control_mut().mixer_voice_count(), 0);
    let after = (cut + BLOCK * 2) as usize;
    assert!(s.channel(0)[cut as usize..after].iter().all(|&v| v == 0.0));
    assert!(s.channel(1)[cut as usize..after].iter().all(|&v| v == 0.0));
    // The pre-wait went on from where it stood: cue 3 starts on its sample.
    s.run(1200);
    assert_eq!(
        first_nonzero(&s.channel(0)[cut as usize..]).map(|i| i + cut as usize),
        Some(pre_wait_from as usize + 1000)
    );
    // And the failed cue plays again.
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
fn panic_during_an_outage_stops_everything_and_nothing_resumes() {
    let a = wav(&dc(20_000, 0.25));
    let b = wav(&dc(20_000, 0.5));
    let mut s = device_show(workspace(vec![audio(1, &a, 0), audio(2, &b, 1)]));
    start(&mut s, 1);
    start(&mut s, 2);
    s.run(BLOCK * 3);
    let cut = s.rendered() as usize;
    lose(&mut s);
    s.take_outbound();
    s.transport(TransportAction::Panic);
    // At once, though sample time stands still: a panic fade could not play.
    for n in [1, 2] {
        assert_eq!(s.control().engine().state(id(n)), CueState::Idle, "cue {n}");
    }
    let msgs = s.take_outbound();
    assert!(!any_broken(&msgs));
    s.wait_wall(Duration::from_secs(2), 2);
    replug(&mut s);
    s.run(BLOCK * 4);
    // Not one sample of either cue after the outage.
    assert!(s.channel(0)[cut..].iter().all(|&v| v == 0.0));
    assert!(s.channel(1)[cut..].iter().all(|&v| v == 0.0));
    assert_eq!(s.control().live_voices(), 0);
    assert_eq!(s.control_mut().mixer_voice_count(), 0);
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
fn go_during_an_outage_plays_when_the_device_is_back_in_time() {
    let a = wav(&dc(20_000, 0.25));
    let b = wav(&ramp(300));
    let mut s = device_show(workspace(vec![audio(1, &a, 0), audio(2, &b, 1)]));
    let at1 = go(&mut s);
    s.run(BLOCK * 3);
    let cut = s.rendered();
    lose(&mut s);
    // Accepted, dated on the frozen clock: one block after the sample the outage cut.
    let at2 = go(&mut s);
    assert_eq!(at2, cut + BLOCK);
    s.wait_wall(Duration::from_secs(3), 3);
    replug(&mut s);
    s.run(600);
    let ch1 = s.channel(1);
    assert_eq!(first_nonzero(&ch1), Some(at2 as usize));
    assert_eq!(ch1[at2 as usize..at2 as usize + 300], ramp(300)[..]);
    // Cue 1 played on through.
    assert!(
        s.channel(0)[at1 as usize..cut as usize + 600]
            .iter()
            .all(|&v| v == 0.25)
    );
    assert!(!any_broken(&s.take_outbound()));
}

#[test]
fn go_during_an_outage_fails_with_the_others_at_the_grace_deadline() {
    let a = wav(&dc(20_000, 0.25));
    let b = wav(&dc(20_000, 0.5));
    let mut s = device_show(workspace(vec![audio(1, &a, 0), audio(2, &b, 1)]));
    start(&mut s, 1);
    s.run(BLOCK * 2);
    lose(&mut s);
    start(&mut s, 2);
    s.take_outbound();
    s.wait_wall(DEVICE_LOSS_GRACE + Duration::from_secs(1), 6);
    let msgs = s.take_outbound();
    assert!(broken_device_lost(&msgs, 1));
    assert!(broken_device_lost(&msgs, 2));
    replug(&mut s);
    s.run(BLOCK * 4);
    assert_eq!(s.control().live_voices(), 0);
    assert_eq!(s.control_mut().mixer_voice_count(), 0);
}

#[test]
fn a_mixer_the_host_still_holds_is_polled_until_it_comes_back() {
    let a = wav(&ramp(2000));
    let mut reference = device_show(workspace(vec![audio(1, &a, 0)]));
    let at_ref = go(&mut reference);
    reference.run(2400);
    let mut s = device_show(workspace(vec![audio(1, &a, 0)]));
    let at = go(&mut s);
    assert_eq!(at, at_ref);
    s.run(640);
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
    s.run(2400 - 640);
    assert_eq!(s.channel(0), reference.channel(0));
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
fn panic_during_an_outage_while_a_panic_fades_also_stops_everything() {
    let a = wav(&dc(20_000, 0.25));
    let mut ws = workspace(vec![audio(1, &a, 0)]);
    ws.settings.panic_duration = 1.0;
    let mut s = device_show(ws);
    start(&mut s, 1);
    s.run(BLOCK * 3);
    // A panic fade is under way when the device goes.
    s.transport(TransportAction::Panic);
    s.run(BLOCK * 2);
    let cut = s.rendered() as usize;
    lose(&mut s);
    assert_eq!(s.control().engine().state(id(1)), CueState::Stopping);
    s.transport(TransportAction::Panic);
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    // That Panic was the hard stop: no new panic fade was started.
    assert_eq!(s.control().engine().panic_end(), None);
    replug(&mut s);
    s.run(BLOCK * 4);
    assert!(s.channel(0)[cut..].iter().all(|&v| v == 0.0));
    assert_eq!(s.control().live_voices(), 0);
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

/// Runs whole blocks until `at` has been rendered.
fn run_to(s: &mut DeviceShow, at: u64) {
    let left = at.saturating_sub(s.rendered());
    s.run(left);
    assert_eq!(s.rendered(), at);
}

fn running_cue_show() -> DeviceShow {
    let long = wav(&dc(200_000, 0.25));
    let mut s = device_show(workspace(vec![audio(1, &long, 0), audio(2, &long, 1)]));
    start(&mut s, 1);
    s.run(BLOCK * 2);
    s
}

#[test]
fn a_device_that_keeps_dropping_out_fails_the_cues_after_ten_seconds_down_in_all() {
    let mut s = running_cue_show();
    let mut failed_in = None;
    for outage in 1..=6 {
        s.take_outbound();
        lose(&mut s);
        let opens = s.fake.opens();
        // Each outage is short (3 s and the replug), far below the grace period.
        s.wait_wall(Duration::from_secs(3), 6);
        if outage > 1 {
            // The backoff kept growing: the stream never stayed up, so the next attempt
            // after this loss is 4 s away, not 250 ms.
            assert_eq!(
                s.fake.opens(),
                opens,
                "outage {outage}: an early reopen attempt"
            );
        }
        if broken_device_lost(&s.take_outbound(), 1) {
            failed_in = Some(outage);
            break;
        }
        replug(&mut s);
        s.run(BLOCK * 2);
    }
    // 10 s of downtime build up in the third outage (about 3.5 s each).
    assert_eq!(failed_in, Some(3));
    assert!(s.control().device_down());
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
    // The cues failed at the grace deadline all the same.
    assert!(broken_device_lost(&msgs, 1));
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
fn pause_all_during_an_outage_holds_the_voices_on_the_sample_they_were_cut() {
    let a = wav(&ramp(3000));
    let b = wav(&ramp(500));
    let build = || {
        let mut c1 = audio(1, &a, 0);
        c1.continue_mode = ContinueMode::AutoFollow;
        let mut s = device_show(workspace(vec![c1, audio(2, &b, 1)]));
        s.run(BLOCK * 2);
        let at = go(&mut s);
        (s, at)
    };
    // Reference: no outage, Pause All dated on the sample the outage will cut.
    let (mut reference, at) = build();
    run_to(&mut reference, at + 640 - BLOCK);
    let cut = reference.now();
    reference.transport(TransportAction::PauseAll);
    run_to(&mut reference, cut + 1280);
    reference.transport(TransportAction::ResumeAll);
    run_to(&mut reference, cut + 6400);

    let (mut s, at2) = build();
    assert_eq!(at2, at);
    run_to(&mut s, cut);
    lose(&mut s);
    s.transport(TransportAction::PauseAll);
    s.wait_wall(Duration::from_secs(2), 4);
    replug(&mut s);
    run_to(&mut s, cut + 1280);
    s.transport(TransportAction::ResumeAll);
    run_to(&mut s, cut + 6400);
    // Not one sample after the cut until the resume, the same continuation, and cue 2 on the
    // same sample.
    for ch in 0..2 {
        assert_eq!(s.channel(ch), reference.channel(ch), "channel {ch}");
    }
    let ch1 = s.channel(1);
    assert!(first_nonzero(&ch1).is_some());
    assert!(!any_broken(&s.take_outbound()));
}

#[test]
fn a_device_back_with_bigger_buffers_keeps_events_on_their_samples() {
    let (reference, _) = play_through(false);
    let (s, _) = play_through_with(true, Some(4 * BLOCK as u32));
    assert_eq!(s.control().block_frames(), 4 * BLOCK);
    for ch in 0..2 {
        let (a, b) = (s.channel(ch), reference.channel(ch));
        let n = a.len().min(b.len());
        assert!(n > 2500);
        assert_eq!(a[..n], b[..n], "channel {ch}");
    }
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
