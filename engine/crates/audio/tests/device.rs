// SPDX-License-Identifier: GPL-3.0-or-later

//! Device layer without hardware (AUD-03, AUD-21, NFR-02, NFR-08, NFR-27): the cpal callback's
//! adapter `OutputHost` driven with fake buffers and timestamps, the mixer handoff across a
//! close or device loss, error reporting, xrun gaps and allocation freedom. The real device is
//! only touched by the `#[ignore]`d smoke test at the end.

// Helpers outside #[test] fns unwrap too (clippy.toml covers only test fns).
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use openlab_audio::{
    CallbackTime, Curve, Fade, HostError, Mixer, MixerConfig, MixerController, OfflineRenderer,
    OutputHost, OutputSample, SCRATCH_FRAMES, StreamErrors,
};

const FRAMES: usize = 6000;

fn mixer_config(outputs: usize) -> MixerConfig {
    MixerConfig {
        output_channels: outputs,
        max_block_frames: 256,
        max_voices: 16,
        ..MixerConfig::default()
    }
}

fn signal(channels: usize) -> Vec<f32> {
    (0..FRAMES * channels)
        .map(|k| ((k * 7 + (k % channels) * 131) % 1000) as f32 / 1000.0 - 0.5)
        .collect()
}

/// A mixer with one voice on every output: a fade-in and a stream position that both have to
/// survive a handoff for the output to match.
fn scenario(outputs: usize) -> (MixerController, Mixer) {
    let (mut ctl, mixer) = Mixer::new(mixer_config(outputs)).unwrap();
    let ch = u16::try_from(outputs).unwrap();
    let mut v = voice(&ctl, 1, source(&signal(outputs)), ch);
    v.fade_in = Fade::new(3000, Curve::EqualPower);
    send_start(&mut ctl, 10, v);
    (ctl, mixer)
}

/// The same scenario through the offline driver, `frames` frames.
fn reference(outputs: usize, frames: usize) -> Vec<f32> {
    let (_ctl, mixer) = scenario(outputs);
    OfflineRenderer::new(mixer).render(frames, &[256])
}

/// Drives `host` like a device: `frames` frames in callbacks of `block` frames.
fn drive<S: OutputSample>(host: &mut OutputHost, out: &mut [S], block: usize) {
    let dc = host.device_channels();
    for chunk in out.chunks_mut(block * dc) {
        host.render(chunk, None);
    }
}

fn expected<S: OutputSample>(reference: &[f32], outputs: usize, dc: usize) -> Vec<S> {
    reference
        .chunks_exact(outputs)
        .flat_map(|frame| {
            (0..dc).map(move |d| frame.get(d).map_or(S::SILENCE, |x| S::from_f32(*x)))
        })
        .collect()
}

#[test]
fn f32_output_matches_the_offline_driver() {
    let want = reference(2, FRAMES);
    let (_ctl, mixer) = scenario(2);
    let (mut host, _link) = OutputHost::new(mixer, 2);
    let mut out = vec![0.0f32; FRAMES * 2];
    drive(&mut host, &mut out, 480);
    assert_eq!(out, want);
    assert!(want.iter().any(|x| *x != 0.0));
}

#[test]
fn every_sample_format_matches_the_converted_mix() {
    let want = reference(2, FRAMES);
    fn check<S: OutputSample + PartialEq + std::fmt::Debug>(want: &[f32]) {
        let (_ctl, mixer) = scenario(2);
        let (mut host, link) = OutputHost::new(mixer, 2);
        let mut out = vec![S::SILENCE; FRAMES * 2];
        no_alloc(|| drive(&mut host, &mut out, 333));
        assert_eq!(out, expected::<S>(want, 2, 2));
        assert_eq!(link.monitor().take_errors(), StreamErrors::default());
    }
    check::<f64>(&want);
    check::<i32>(&want);
    check::<i16>(&want);
    check::<u16>(&want);
}

#[test]
fn extra_device_channels_are_silent_and_missing_ones_drop_the_last_outputs() {
    let want = reference(4, FRAMES);
    // Six device channels for four outputs: 4 and 5 silent.
    let (_ctl, mixer) = scenario(4);
    let (mut host, _link) = OutputHost::new(mixer, 6);
    let mut out = vec![1.0f32; FRAMES * 6];
    no_alloc(|| drive(&mut host, &mut out, 512));
    assert_eq!(out, expected::<f32>(&want, 4, 6));
    assert!(channel(&out, 6, 4).iter().all(|x| *x == 0.0));
    // Two device channels for four outputs: the first two.
    let (_ctl, mixer) = scenario(4);
    let (mut host, _link) = OutputHost::new(mixer, 2);
    let mut out = vec![0i16; FRAMES * 2];
    no_alloc(|| drive(&mut host, &mut out, 512));
    assert_eq!(out, expected::<i16>(&want, 4, 2));
}

#[test]
fn callbacks_larger_than_the_scratch_render_in_pieces_without_allocating() {
    let frames = SCRATCH_FRAMES * 3 + 17;
    let want = reference(2, frames);
    let (_ctl, mixer) = scenario(2);
    let (mut host, link) = OutputHost::new(mixer, 3);
    let mut out = vec![0i32; frames * 3];
    no_alloc(|| host.render(&mut out, None));
    assert_eq!(out, expected::<i32>(&want, 2, 3));
    assert_eq!(link.monitor().last_callback_frames(), Some(frames as u32));
    assert_eq!(link.monitor().callbacks(), 1);
}

#[test]
fn a_partial_frame_is_silenced_and_reported() {
    let (ctl, mixer) = scenario(2);
    let (mut host, link) = OutputHost::new(mixer, 2);
    let mut out = [7i16; 7];
    host.render(&mut out, None);
    assert_eq!(out[6], 0);
    assert_eq!(ctl.samples_rendered(), 3);
    let errors = link.monitor().take_errors();
    assert_eq!(errors.bad_buffer, 1);
    host.reject_buffer();
    assert_eq!(link.monitor().take_errors().bad_buffer, 1);
    assert!(link.monitor().take_errors().is_empty());
}

#[test]
fn the_mixer_comes_back_intact_and_continues_sample_exactly() {
    let want = reference(2, FRAMES);
    let (ctl, mixer) = scenario(2);
    let (mut host, mut link) = OutputHost::new(mixer, 2);
    let mut out = vec![0.0f32; FRAMES * 2];
    let split = 2500 * 2;
    drive(&mut host, &mut out[..split], 256);
    assert!(link.take_mixer().is_none(), "the host still owns it");
    // Dropping the host (cpal drops the callback) must not allocate or free: it may run on the
    // audio thread.
    no_alloc(|| drop(host));
    let mixer = link.take_mixer().unwrap();
    assert_eq!(mixer.now(), 2500);
    assert!(link.take_mixer().is_none());
    // "Reopen": a new host on the same mixer, other buffer size.
    let (mut host, _link) = OutputHost::new(mixer, 2);
    drive(&mut host, &mut out[split..], 441);
    assert_eq!(out, want);
    assert_eq!(ctl.samples_rendered(), FRAMES as u64);
}

#[test]
fn device_loss_then_reopen_on_a_device_with_other_channels() {
    let want = reference(2, FRAMES);
    let (ctl, mixer) = scenario(2);
    let (mut host, mut link) = OutputHost::new(mixer, 2);
    let reporter = link.reporter();
    let monitor = link.monitor();
    let mut first = vec![0.0f32; 1000 * 2];
    drive(&mut host, &mut first, 500);
    // The host's error callback (another thread) reports the unplug; callbacks stop.
    std::thread::scope(|s| {
        s.spawn(|| reporter.report(HostError::Lost));
    });
    assert!(monitor.needs_reopen());
    assert_eq!(
        monitor.take_errors(),
        StreamErrors {
            lost: 1,
            ..StreamErrors::default()
        }
    );
    // Lost is sticky; the count is read once.
    assert!(monitor.needs_reopen());
    assert!(monitor.take_errors().is_empty());
    drop(host);
    let mixer = link.take_mixer().unwrap();
    // Replugged as a four-channel interface (or the engine fell back to another device).
    let (mut host, link2) = OutputHost::new(mixer, 4);
    assert!(!link2.monitor().needs_reopen());
    let mut rest = vec![0i16; (FRAMES - 1000) * 4];
    drive(&mut host, &mut rest, 256);
    let want_first: Vec<f32> = want[..2000].to_vec();
    assert_eq!(first, want_first);
    assert_eq!(rest, expected::<i16>(&want[2000..], 2, 4));
    assert_eq!(ctl.samples_rendered(), FRAMES as u64);
}

#[test]
fn host_errors_are_counted_once_per_kind() {
    let (ctl, mixer) = scenario(2);
    let (_host, link) = OutputHost::new(mixer, 2);
    let r = link.reporter();
    let m = link.monitor();
    for e in [
        HostError::Rerouted,
        HostError::RealtimeDenied,
        HostError::Other,
        HostError::Other,
        HostError::Xrun,
        HostError::Invalidated,
    ] {
        r.report(e);
    }
    assert_eq!(
        m.take_errors(),
        StreamErrors {
            invalidated: 1,
            rerouted: 1,
            realtime_denied: 1,
            other: 2,
            ..StreamErrors::default()
        }
    );
    assert!(m.needs_reopen());
    // Host xruns go to the mixer's counter, not the error counts.
    assert_eq!(ctl.xruns(), 1);
    assert_eq!(m.host_xruns(), 1);
    assert!(m.take_errors().is_empty());
}

#[test]
fn a_reroute_asks_for_a_reopen_and_a_realtime_refusal_does_not() {
    // A show must never move devices on its own: a reroute (or a system-default change) makes
    // the negotiated configuration stale, so the engine has to notice and reopen.
    let (_ctl, mixer) = scenario(2);
    let (_host, link) = OutputHost::new(mixer, 2);
    link.reporter().report(HostError::RealtimeDenied);
    link.reporter().report(HostError::Other);
    assert!(!link.monitor().needs_reopen());
    link.reporter().report(HostError::Rerouted);
    assert!(link.monitor().needs_reopen());
}

fn at(ms_tenths: u64) -> Option<CallbackTime> {
    let ns = ms_tenths * 100_000;
    Some(CallbackTime {
        callback_ns: ns,
        playback_ns: ns + 5_000_000,
    })
}

#[test]
fn timestamp_gaps_count_xruns_and_host_reports_are_not_doubled() {
    let (ctl, mixer) = scenario(2);
    let (mut host, link) = OutputHost::new(mixer, 2);
    let m = link.monitor();
    let mut buf = vec![0.0f32; 480 * 2]; // 10 ms at 48 kHz
    // Steady 10 ms callbacks with jitter up to 14 ms: no xrun.
    for t in [0, 100, 240, 300, 400] {
        host.render(&mut buf, at(t));
    }
    assert_eq!(ctl.xruns(), 0);
    // A 30 ms gap: one xrun.
    host.render(&mut buf, at(700));
    assert_eq!((ctl.xruns(), m.gap_xruns()), (1, 1));
    // The host reports a dropout and the next callback shows its gap: counted once.
    link.reporter().report(HostError::Xrun);
    host.render(&mut buf, at(1000));
    assert_eq!((ctl.xruns(), m.gap_xruns(), m.host_xruns()), (2, 1, 1));
    // Callbacks without timestamps never count.
    host.render(&mut buf, None);
    host.render(&mut buf, at(5000));
    assert_eq!(ctl.xruns(), 3);
    assert_eq!(m.device_latency_seconds(), Some(0.005));
    assert_eq!(m.last_callback_frames(), Some(480));
}

#[test]
fn a_playback_time_before_the_callback_gives_no_latency() {
    let (_ctl, mixer) = scenario(2);
    let (mut host, link) = OutputHost::new(mixer, 2);
    let mut buf = vec![0.0f32; 64];
    assert_eq!(link.monitor().device_latency_seconds(), None);
    host.render(
        &mut buf,
        Some(CallbackTime {
            callback_ns: 10,
            playback_ns: 5,
        }),
    );
    assert_eq!(link.monitor().device_latency_seconds(), None);
}

#[test]
fn no_allocation_on_any_callback_path() {
    let (_ctl, mixer) = scenario(4);
    let (mut host, mut link) = OutputHost::new(mixer, 4);
    let reporter = link.reporter();
    let mut f = vec![0.0f32; 4 * 512];
    let mut g = vec![0.0f64; 4 * (SCRATCH_FRAMES + 5)];
    let mut i = vec![0i16; 4 * 300 + 3];
    let mut u = vec![0u16; 4 * 64];
    let mut w = vec![0i32; 4 * 64];
    no_alloc(|| {
        for k in 0..20u64 {
            host.render(&mut f, at(k * 100));
            host.render(&mut g, at(k * 100 + 50));
            host.render(&mut i, None);
            host.render(&mut u, at(k * 100 + 99));
            host.render(&mut w, None);
            reporter.report(HostError::Xrun);
            reporter.report(HostError::Lost);
        }
        drop(host);
    });
    assert!(link.take_mixer().is_some());
    let m = link.monitor();
    assert_eq!(m.alloc_violations(), 0);
    assert_eq!(m.callback_faults(), 0);
    assert_eq!(m.leaked_mixers(), 0);
    // The callbacks were timed (M4.7a) inside the same no-allocation guard.
    let load = m.take_engine_load().expect("the callbacks were timed");
    assert!(load.is_finite() && load >= 0.0, "{load}");
}

/// Manual: plays 0.5 s of silence on the default output device and gets the mixer back.
/// `cargo test -p openlab-audio --test device -- --ignored`. Uses a real sleep (it needs the
/// device clock). Unplugging during a longer run is checked by hand (PROGRESS M3.8).
#[cfg(feature = "device")]
#[test]
#[ignore = "needs an audio output device"]
fn hardware_smoke_default_output() {
    use openlab_audio::{OpenRequest, open_output, output_devices};
    for d in output_devices().unwrap() {
        println!(
            "{} [{}] default={} max_ch={}",
            d.name,
            d.id,
            d.is_default,
            d.max_channels()
        );
    }
    let (_ctl, mixer) = Mixer::new(MixerConfig::default()).unwrap();
    let mut req = OpenRequest {
        buffer_frames: Some(256),
        ..OpenRequest::default()
    };
    let stream = match open_output(mixer, &req) {
        Ok(s) => s,
        Err(mut e) => {
            // The default device may not run at 48 kHz: retry at 44.1 kHz.
            assert!(e.take_mixer().is_some());
            let (_ctl, mixer) = Mixer::new(MixerConfig {
                sample_rate: 44_100,
                ..MixerConfig::default()
            })
            .unwrap();
            req.buffer_frames = None;
            open_output(mixer, &req).unwrap()
        }
    };
    println!("{stream:?} latency {:?}", stream.latency());
    std::thread::sleep(std::time::Duration::from_millis(500));
    let monitor = stream.monitor();
    assert!(monitor.callbacks() > 0);
    let mixer = stream.close().unwrap();
    assert!(mixer.now() > 0);
    println!("errors {:?}", monitor.take_errors());
}

#[test]
fn handles_can_cross_threads() {
    fn send<T: Send>() {}
    fn send_sync<T: Send + Sync>() {}
    send::<OutputHost>();
    send::<openlab_audio::HostLink>();
    send_sync::<openlab_audio::DeviceMonitor>();
    send_sync::<openlab_audio::ErrorReporter>();
    #[cfg(feature = "device")]
    send::<openlab_audio::OutputStream>();
}
