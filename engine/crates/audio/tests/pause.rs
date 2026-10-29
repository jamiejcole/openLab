// SPDX-License-Identifier: GPL-3.0-or-later
// Helpers outside #[test] fns are test code too; clippy's in-test allowances miss them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! Voice pause and resume (CUE-05, NFR-21, NFR-22): a voice paused on sample P and resumed on
//! sample R plays exactly the un-paused render shifted by R − P, outside the two declick
//! windows, for ring-fed (preloaded), disk-streamed, resampled and looping voices. The resumed
//! voice is re-keyed to a fresh id. Every render runs with allocation forbidden; disk-stream
//! tests wait on stream signals, never on timers.

mod common;

use std::time::Duration;

use common::media::*;
use common::*;
use openlab_audio::{
    CommandKind, Curve, DiskStreamer, Event, Fade, FinishReason, GainTarget, Media, MediaInfo,
    MixerController, OfflineRenderer, Plays, RejectReason, Seeking, Slice, StreamHandle,
    StreamRequest, StreamerConfig, VoiceId, VoiceStart, plan, preload, probe,
};

const SR: u32 = 48_000;
/// The mixer's de-zipper length at 48 kHz (5 ms): the declick fade-out and fade-in length.
const D: u64 = 240;
const BLOCKS: &[usize] = &[64, 1, 200, 37];
const OTHER_BLOCKS: &[usize] = &[256, 13, 7];
const FAILSAFE: Duration = Duration::from_secs(10);

fn render(r: &mut OfflineRenderer, frames: usize, blocks: &[usize]) -> Vec<f32> {
    let ch = r.mixer_mut().output_channels();
    let mut out = vec![0.0; frames * ch];
    no_alloc(|| r.render_into(&mut out, blocks));
    out
}

fn pause(voice: u64) -> CommandKind {
    CommandKind::PauseVoice {
        voice: VoiceId(voice),
    }
}

fn resume(voice: u64, new_id: u64) -> CommandKind {
    CommandKind::ResumeVoice {
        voice: VoiceId(voice),
        new_id: VoiceId(new_id),
    }
}

fn stop(voice: u64, fade: Fade) -> CommandKind {
    CommandKind::StopVoice {
        voice: VoiceId(voice),
        fade,
    }
}

fn master(voice: u64, gain: f32, fade: Fade) -> CommandKind {
    CommandKind::Ramp {
        voice: VoiceId(voice),
        target: GainTarget::Master,
        gain,
        fade,
    }
}

fn finished(voice: u64, at: u64, reason: FinishReason) -> Event {
    Event::VoiceFinished {
        voice: VoiceId(voice),
        at,
        reason,
    }
}

fn event_at(e: &Event) -> u64 {
    match *e {
        Event::VoiceFinished { at, .. }
        | Event::Underrun { at, .. }
        | Event::VoiceRejected { at, .. }
        | Event::PendingFull { at }
        | Event::LoopReached { at, .. }
        | Event::SliceEnded { at, .. }
        | Event::DevampScheduled { at, .. }
        | Event::DevampIgnored { at, .. }
        | Event::StageFaulted { at, .. } => at,
    }
}

fn sorted(mut v: Vec<Event>) -> Vec<Event> {
    v.sort_by_key(event_at);
    v
}

/// The reference events as the paused run must report them: those after the pause point move by
/// R − P and carry the resumed id.
fn shifted(reference: &[Event], p: u64, r: u64, new_id: u64) -> Vec<Event> {
    let mv = |at: u64| if at > p { at + (r - p) } else { at };
    let id = |at: u64, v: VoiceId| if at > p { VoiceId(new_id) } else { v };
    assert!(
        reference.iter().all(|e| event_at(e) != p),
        "pick a pause point without an event on it"
    );
    sorted(
        reference
            .iter()
            .map(|e| match *e {
                Event::VoiceFinished { voice, at, reason } => Event::VoiceFinished {
                    voice: id(at, voice),
                    at: mv(at),
                    reason,
                },
                Event::LoopReached {
                    voice,
                    at,
                    slice,
                    count,
                } => Event::LoopReached {
                    voice: id(at, voice),
                    at: mv(at),
                    slice,
                    count,
                },
                Event::SliceEnded { voice, at, slice } => Event::SliceEnded {
                    voice: id(at, voice),
                    at: mv(at),
                    slice,
                },
                other => panic!("unexpected reference event {other:?}"),
            })
            .collect(),
    )
}

/// `out` (mono) is `reference` before P, silent from the end of the declick fade-out to R, and
/// `reference` shifted by R − P from the end of the declick fade-in on.
fn assert_shifted(out: &[f32], reference: &[f32], p: usize, r: usize) {
    let d = D as usize;
    assert_eq!(out.len(), reference.len() + (r - p));
    assert_eq!(&out[..p], &reference[..p], "before the pause");
    assert!(
        out[(p + d).min(r)..r].iter().all(|&s| s == 0.0),
        "audio while paused"
    );
    for (t, (&o, &w)) in out[r + d..].iter().zip(&reference[p + d..]).enumerate() {
        assert_eq!(o, w, "frame {} after the resume window", t);
    }
}

/// Renders the scenario un-paused and paused at `p` / resumed at `r` (as voice `new_id`), and
/// checks the paused run is the reference shifted by R − P, events included.
fn check_no_drift(
    frames: usize,
    p: u64,
    r: u64,
    new_id: u64,
    scenario: impl Fn(&mut MixerController),
) -> Vec<f32> {
    let (mut ctl, mut rr) = setup(config(1));
    scenario(&mut ctl);
    let reference = render(&mut rr, frames, BLOCKS);
    let ref_events = sorted(events(&mut ctl));

    let (mut ctl, mut rr) = setup(config(1));
    scenario(&mut ctl);
    ctl.send(p, pause(1));
    ctl.send(r, resume(1, new_id));
    let out = render(&mut rr, frames + (r - p) as usize, OTHER_BLOCKS);
    assert_shifted(&out, &reference, p as usize, r as usize);
    assert_eq!(sorted(events(&mut ctl)), shifted(&ref_events, p, r, new_id));
    assert_eq!(ctl.underrun_frames(), 0);
    assert_eq!(ctl.collect_returns(), 1, "the voice start comes back once");
    out
}

/// A non-repeating signal, so any shift or repeat shows.
fn ramp_signal(frames: u64) -> Vec<f32> {
    markers(frames)
}

/// The frames a disk stream would write for a finite table (file frame `i` holds `marker(i)`).
fn unrolled(slices: &[Slice]) -> Vec<f32> {
    let mut v = Vec::new();
    for s in slices {
        let Plays::Times(n) = s.plays else {
            panic!("finite tables only")
        };
        for _ in 0..n {
            v.extend((s.start..s.start + s.frames).map(marker));
        }
    }
    v
}

#[test]
fn preloaded_voice_resumes_without_drift() {
    let data = ramp_signal(5000);
    let out = check_no_drift(5200, 1234, 3001, 2, |ctl| {
        let mut v = voice(ctl, 1, source(&data), 1);
        v.fade_in = Fade::new(300, Curve::EqualPower);
        v.play_frames = Some(4800);
        v.fade_out = Fade::new(1000, Curve::SCurve);
        send_start(ctl, 10, v);
    });
    // The declick fade-out moves towards silence and ends exactly on it.
    assert_eq!(out[1234 + D as usize - 1], 0.0);
    assert_ne!(out[1234], 0.0);
}

#[test]
fn resume_inside_the_integrated_fade_out_ends_on_the_shifted_sample() {
    let data = vec![0.5; 3000];
    check_no_drift(3100, 2500, 2600 + 7 * D, 9, |ctl| {
        let mut v = voice(ctl, 1, source(&data), 1);
        v.play_frames = Some(3000);
        v.fade_out = Fade::new(800, Curve::Linear);
        send_start(ctl, 0, v);
    });
}

#[test]
fn looping_slice_voice_paused_mid_loop_resumes_without_drift() {
    let slices = plan(
        0..300,
        &[100, 200],
        &[Plays::Times(1), Plays::Times(4), Plays::Times(1)],
        0,
    )
    .unwrap();
    let ring = unrolled(&slices);
    // Pause in the second play of the loop; the tail crosses a loop boundary.
    check_no_drift(800, 7 + 280, 2000, 5, |ctl| {
        let mut v = voice(ctl, 1, source(&ring), 1);
        v.slices = slices.clone();
        v.fade_out = Fade::new(150, Curve::Linear);
        send_start(ctl, 7, v);
    });
}

#[test]
fn ramps_and_stop_fades_freeze_while_paused_and_finish_after_resume() {
    let data = ramp_signal(5000);
    // A master ramp in progress across the pause completes its remaining frames after it.
    check_no_drift(6000, 700, 4000, 2, |ctl| {
        start(ctl, 0, 1, source(&data), 1);
        ctl.send(100, master(1, 0.1, Fade::new(2000, Curve::Exponential)));
    });
    // A stop fade in progress across the pause ends R − P later.
    let out = check_no_drift(3000, 700, 4000, 2, |ctl| {
        start(ctl, 0, 1, source(&data), 1);
        ctl.send(100, stop(1, Fade::new(1000, Curve::Linear)));
    });
    assert!(out[1100 + 3300..].iter().all(|&s| s == 0.0));
}

#[test]
fn commands_on_the_same_sample_apply_in_send_order() {
    let data = ramp_signal(2000);
    let start_at = |ctl: &mut MixerController| start(ctl, 100, 1, source(&data), 1);
    let (mut ctl, mut r) = setup(config(1));
    start_at(&mut ctl);
    let reference = render(&mut r, 2300, BLOCKS);

    // Start then pause on its first sample: nothing has been heard, so no declick either way:
    // the resumed voice is exactly a start at the resume sample.
    let (mut ctl, mut r) = setup(config(1));
    start_at(&mut ctl);
    ctl.send(100, pause(1));
    ctl.send(500, resume(1, 2));
    let out = render(&mut r, 2700, OTHER_BLOCKS);
    assert!(out[..500].iter().all(|&s| s == 0.0));
    assert_eq!(&out[500..], &reference[100..]);
    assert_eq!(
        events(&mut ctl),
        vec![finished(2, 2500, FinishReason::EndOfStream)]
    );

    // Pause before the start on the same sample: the voice is unknown then, so it just plays.
    let (mut ctl, mut r) = setup(config(1));
    ctl.send(100, pause(1));
    start_at(&mut ctl);
    assert_eq!(render(&mut r, 2300, OTHER_BLOCKS), reference);

    // Pause and resume on one sample: nothing audible changes, only the id.
    let (mut ctl, mut r) = setup(config(1));
    start_at(&mut ctl);
    ctl.send(777, pause(1));
    ctl.send(777, resume(1, 2));
    ctl.send(800, master(1, 0.0, Fade::NONE));
    assert_eq!(render(&mut r, 2300, OTHER_BLOCKS), reference);
    assert_eq!(
        events(&mut ctl),
        vec![finished(2, 2100, FinishReason::EndOfStream)]
    );
}

#[test]
fn resumed_voice_is_rekeyed_and_released_once() {
    let data = ramp_signal(4000);
    let (tx, rx) = open_source(4000, &data);
    let (mut ctl, mut r) = setup(config(1));
    let mut v = voice(&ctl, 1, rx, 1);
    v.play_frames = Some(3000);
    send_start(&mut ctl, 0, v);
    ctl.send(1000, pause(1));
    ctl.send(2000, resume(1, 7));
    // The old id is unknown after the resume; the new one addresses the voice.
    ctl.send(2500, master(1, 0.0, Fade::NONE));
    ctl.send(2500, stop(1, Fade::NONE));
    ctl.send(2500, pause(1));
    ctl.send(2600, master(7, 0.5, Fade::NONE));
    let out = render(&mut r, 4500, BLOCKS);
    assert_eq!(
        &out[2000 + D as usize..2600],
        &data[1000 + D as usize..1600]
    );
    // The de-zipped gain change lands D frames later.
    for (o, w) in out[2600 + D as usize..4000]
        .iter()
        .zip(&data[1600 + D as usize..])
    {
        assert_eq!(*o, w * 0.5);
    }
    assert!(out[4000..].iter().all(|&s| s == 0.0));
    assert_eq!(
        events(&mut ctl),
        vec![finished(7, 4000, FinishReason::OutPoint)]
    );
    assert!(
        !tx.is_abandoned(),
        "the source is freed on the control thread"
    );
    assert_eq!(ctl.collect_returns(), 1);
    assert!(tx.is_abandoned());
    assert_eq!(ctl.collect_returns(), 0);
}

#[test]
fn resume_onto_a_live_id_is_rejected_and_the_voice_stays_paused() {
    let (mut ctl, mut r) = setup(config(1));
    start(&mut ctl, 0, 1, source(&vec![0.25; 3000]), 1);
    start(&mut ctl, 0, 2, source(&vec![0.5; 1000]), 1);
    ctl.send(200, pause(1));
    ctl.send(600, resume(1, 2));
    ctl.send(1500, resume(1, 3));
    let out = render(&mut r, 4500, BLOCKS);
    assert!(out[200 + D as usize..1000].iter().all(|&s| s == 0.5));
    assert!(out[1000..1500].iter().all(|&s| s == 0.0));
    assert!(out[1500 + D as usize..4300].iter().all(|&s| s == 0.25));
    assert!(out[4300..].iter().all(|&s| s == 0.0));
    assert_eq!(
        events(&mut ctl),
        vec![
            Event::VoiceRejected {
                voice: VoiceId(2),
                at: 600,
                reason: RejectReason::DuplicateId,
            },
            finished(2, 1000, FinishReason::EndOfStream),
            finished(3, 4300, FinishReason::EndOfStream),
        ]
    );
    // Resuming as its own id is no collision.
    let (mut ctl, mut r) = setup(config(1));
    start(&mut ctl, 0, 1, source(&vec![0.25; 300]), 1);
    ctl.send(100, pause(1));
    ctl.send(200, resume(1, 1));
    render(&mut r, 1000, BLOCKS);
    assert_eq!(
        events(&mut ctl),
        vec![finished(1, 400, FinishReason::EndOfStream)]
    );
}

#[test]
fn stop_while_paused_finishes_at_once_with_or_without_fade() {
    for (at, fade) in [
        (1000, Fade::NONE),
        (1000, Fade::new(5000, Curve::Linear)),
        // During the declick fade-out.
        (310, Fade::new(100, Curve::Linear)),
    ] {
        let (mut ctl, mut r) = setup(config(1));
        start(&mut ctl, 0, 1, source(&vec![0.25; 30_000]), 1);
        ctl.send(300, pause(1));
        ctl.send(at, stop(1, fade));
        ctl.send(at + 1, resume(1, 2));
        let out = render(&mut r, 3000, BLOCKS);
        assert!(out[at as usize..].iter().all(|&s| s == 0.0));
        assert_eq!(
            events(&mut ctl),
            vec![finished(1, at, FinishReason::Stopped)]
        );
        assert_eq!(ctl.collect_returns(), 1);
    }
}

#[test]
fn pause_and_resume_of_unknown_or_unpaused_voices_are_ignored() {
    let data = ramp_signal(3000);
    let (mut ctl, mut r) = setup(config(1));
    start(&mut ctl, 0, 1, source(&data), 1);
    let reference = render(&mut r, 3500, BLOCKS);

    let (mut ctl, mut r) = setup(config(1));
    start(&mut ctl, 0, 1, source(&data), 1);
    ctl.send(50, pause(9));
    ctl.send(60, resume(9, 10));
    // Resuming a playing voice does nothing, re-keying included.
    ctl.send(70, resume(1, 11));
    ctl.send(80, resume(11, 12));
    assert_eq!(render(&mut r, 3500, OTHER_BLOCKS), reference);
    assert_eq!(
        events(&mut ctl),
        vec![finished(1, 3000, FinishReason::EndOfStream)]
    );

    // A repeated pause changes nothing: the shift counts from the first.
    let (mut ctl, mut r) = setup(config(1));
    start(&mut ctl, 0, 1, source(&data), 1);
    ctl.send(1000, pause(1));
    ctl.send(1500, pause(1));
    ctl.send(2000, resume(1, 2));
    ctl.send(2100, resume(1, 3));
    let out = render(&mut r, 4500, OTHER_BLOCKS);
    assert_shifted(&out, &reference, 1000, 2000);
    assert_eq!(
        events(&mut ctl),
        vec![finished(2, 4000, FinishReason::EndOfStream)]
    );
}

#[test]
fn resume_during_the_declick_fade_out_never_jumps_the_gain() {
    let data = vec![1.0; 3000];
    let (mut ctl, mut r) = setup(config(1));
    start(&mut ctl, 0, 1, source(&data), 1);
    ctl.send(500, pause(1));
    ctl.send(560, resume(1, 2));
    let out = render(&mut r, 3200, BLOCKS);
    // Continuous gain: no step larger than one ramp step anywhere.
    let step = out.windows(2).take(2900).map(|w| (w[1] - w[0]).abs());
    assert!(step.fold(0.0f32, f32::max) <= 1.0 / D as f32 + 1e-6);
    assert!(out[560 + D as usize..3060].iter().all(|&s| s == 1.0));
    assert_eq!(
        events(&mut ctl),
        vec![finished(2, 3060, FinishReason::EndOfStream)]
    );
}

#[test]
fn ramp_and_devamp_on_a_paused_voice_take_effect_on_resume() {
    let data = ramp_signal(3000);
    let (mut ctl, mut r) = setup(config(1));
    start(&mut ctl, 0, 1, source(&data), 1);
    ctl.send(500, pause(1));
    ctl.send(600, master(1, 0.5, Fade::NONE));
    ctl.send(1500, resume(1, 2));
    let out = render(&mut r, 4000, BLOCKS);
    assert!(out[500 + D as usize..1500].iter().all(|&s| s == 0.0));
    // The de-zipped change runs alongside the declick fade-in, then holds.
    for (o, w) in out[1500 + D as usize..3500]
        .iter()
        .zip(&data[500 + D as usize..])
    {
        assert_eq!(*o, w * 0.5);
    }

    let slices = plan(0..300, &[100], &[Plays::Infinite, Plays::Times(1)], 0).unwrap();
    let ring: Vec<f32> = (0..20).flat_map(|_| (0..100).map(marker)).collect();
    let (mut ctl, mut r) = setup(config(1));
    let mut v = voice(&ctl, 1, source(&ring), 1);
    v.slices = slices;
    send_start(&mut ctl, 0, v);
    ctl.send(150, pause(1));
    ctl.send(
        400,
        CommandKind::Devamp {
            voice: VoiceId(1),
            stop: true,
        },
    );
    ctl.send(1000, resume(1, 4));
    render(&mut r, 1500, BLOCKS);
    let slice_ended = |at| Event::SliceEnded {
        voice: VoiceId(4),
        at,
        slice: 0,
    };
    // Paused 50 frames into the second play: it ends 50 frames after the resume.
    assert_eq!(
        events(&mut ctl),
        vec![
            Event::LoopReached {
                voice: VoiceId(1),
                at: 100,
                slice: 0,
                count: 1
            },
            Event::DevampScheduled {
                voice: VoiceId(4),
                at: 1050
            },
            slice_ended(1050),
            finished(4, 1050, FinishReason::OutPoint),
        ]
    );
}

// --- Disk streams -------------------------------------------------------------------------

fn streamer(buffer_ms: u32) -> DiskStreamer {
    let mut c = StreamerConfig::new(SR);
    c.buffer_ms = buffer_ms;
    DiskStreamer::new(c).unwrap()
}

fn prepare(
    d: &mut DiskStreamer,
    ctl: &MixerController,
    media: Media,
    info: MediaInfo,
) -> (u64, StreamHandle, VoiceStart) {
    let total = info.frames_at(SR);
    let slices = plan(0..total, &[], &[], 0).unwrap();
    let p = d
        .prepare(StreamRequest {
            media,
            info,
            seeking: Seeking::Direct,
            slices,
        })
        .unwrap();
    assert!(p.handle.wait_primed(FAILSAFE).unwrap());
    let (mut v, h) = p.into_voice(VoiceId(1), ctl.cue_outputs());
    v.set_crosspoint(0, 0, 1.0);
    (total, h, v)
}

/// Renders `frames` in blocks of 256; before each block waits until the stream has written
/// `lead` frames beyond what the voice will have consumed by the block's end (`consumed(t)`).
fn render_synced(
    r: &mut OfflineRenderer,
    ctl: &mut MixerController,
    h: &StreamHandle,
    total: u64,
    frames: usize,
    lead: u64,
    consumed: impl Fn(u64) -> u64,
) -> Vec<f32> {
    let mut out = vec![0.0; frames];
    let mut done = 0;
    while done < frames {
        let n = 256.min(frames - done);
        let need = (consumed((done + n) as u64) + lead).min(total);
        assert!(h.wait_written(need, FAILSAFE).unwrap(), "stream stalled");
        no_alloc(|| r.render_into(&mut out[done..done + n], &[n]));
        // Releases commands beyond the lookahead, as the engine's control loop does.
        ctl.pump();
        done += n;
    }
    out
}

/// Plays `media` through a disk stream, un-paused and paused at P / resumed at R, and checks
/// no drift.
fn check_stream_no_drift(
    media: impl Fn() -> Media,
    info: MediaInfo,
    buffer_ms: u32,
    p: u64,
    r: u64,
) {
    let run = |paused: bool| {
        let mut d = streamer(buffer_ms);
        let (mut ctl, mut rr) = setup(config(1));
        let (total, h, v) = prepare(&mut d, &ctl, media(), info);
        send_start(&mut ctl, 0, v);
        let frames = total as usize + 500;
        let (out, shift, id) = if paused {
            ctl.send(p, pause(1));
            ctl.send(r, resume(1, 2));
            let consumed = |t: u64| {
                if t <= p {
                    t
                } else if t <= r {
                    p
                } else {
                    t - (r - p)
                }
            };
            let n = frames + (r - p) as usize;
            (
                render_synced(&mut rr, &mut ctl, &h, total, n, 1024, consumed),
                r - p,
                2,
            )
        } else {
            (
                render_synced(&mut rr, &mut ctl, &h, total, frames, 1024, |t| t),
                0,
                1,
            )
        };
        assert_eq!(events(&mut ctl), ended(id, total + shift));
        assert_eq!(ctl.underrun_frames(), 0);
        // `pump` has already freed the returned start.
        assert_eq!(ctl.leaked_starts(), 0);
        out
    };
    let reference = run(false);
    let out = run(true);
    assert!(reference.iter().any(|&s| s != 0.0));
    assert_shifted(&out, &reference, p as usize, r as usize);
}

/// A streamed voice's events at its end: its one-slice table ends, then the voice.
fn ended(voice: u64, at: u64) -> Vec<Event> {
    vec![
        Event::SliceEnded {
            voice: VoiceId(voice),
            at,
            slice: 0,
        },
        finished(voice, at, FinishReason::OutPoint),
    ]
}

fn file(name: &str, samples: &[f32], rate: u32) -> (std::path::PathBuf, MediaInfo) {
    let path = temp_file(name, &wav(samples, 1, rate, Pcm::F32));
    let info = probe(&path).unwrap().info;
    (path, info)
}

#[test]
fn streamed_voice_resumes_without_drift() {
    let (path, info) = file("pause-stream.wav", &markers(30_000), SR);
    check_stream_no_drift(|| Media::Path(path.clone()), info, 2000, 12_345, 20_000);
}

/// Source rate ≠ mixer rate: the resampler lives on the disk thread, ahead of the ring, so a
/// pause in the mixer leaves its state untouched.
#[test]
fn resampled_voice_resumes_without_drift() {
    let (path, info) = file(
        "pause-resampled.wav",
        &sine(30_000, 44_100, 997.0, 0.5),
        44_100,
    );
    check_stream_no_drift(|| Media::Path(path.clone()), info, 2000, 9_999, 31_000);
}

#[test]
fn preloaded_media_resumes_without_drift() {
    let (path, info) = file(
        "pause-preloaded.wav",
        &sine(20_000, 44_100, 440.0, 0.5),
        44_100,
    );
    let pre = preload(Media::Path(path)).unwrap();
    check_stream_no_drift(|| Media::Preloaded(pre.clone()), info, 100, 7_000, 9_001);
}

/// A pause many ring lengths long: the stream thread keeps the ring full, nothing is reported
/// while paused, and the voice resumes without an underrun.
#[test]
fn long_pause_of_a_streamed_voice_resumes_without_underrun() {
    let all = markers(48_000);
    let (path, info) = file("pause-long.wav", &all, SR);
    // A 50 ms ring (2400 frames); paused for 100 ring lengths.
    check_stream_no_drift(|| Media::Path(path.clone()), info, 50, 10_000, 250_000);

    let mut d = streamer(50);
    let (mut ctl, mut r) = setup(config(1));
    let (total, h, v) = prepare(&mut d, &ctl, Media::Path(path.clone()), info);
    send_start(&mut ctl, 0, v);
    ctl.send(5_000, pause(1));
    let before = render_synced(&mut r, &mut ctl, &h, total, 5_000, 1024, |t| t);
    assert_eq!(&before[..], &all[..5_000]);
    let mut silent = vec![0.0; 240_000];
    for chunk in silent.chunks_mut(4096) {
        no_alloc(|| r.render_into(chunk, &[256]));
        ctl.pump();
        assert!(events(&mut ctl).is_empty(), "events while paused");
    }
    assert!(silent[D as usize..].iter().all(|&s| s == 0.0));
    assert_eq!(ctl.underrun_frames(), 0);
    // The ring stays bounded: the stream is only ever one ring ahead of the paused voice.
    assert!(h.frames_written() <= 5_000 + 4_800);
    ctl.send(245_000, resume(1, 2));
    let rest = render_synced(&mut r, &mut ctl, &h, total, 44_000, 1024, |t| 5_000 + t);
    let d = D as usize;
    assert_eq!(&rest[d..43_000], &all[5_000 + d..]);
    assert!(rest[43_000..].iter().all(|&s| s == 0.0));
    assert_eq!(ctl.underrun_frames(), 0);
    assert_eq!(events(&mut ctl), ended(2, total + 240_000));
}

// --- Review fixes -------------------------------------------------------------------------

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-6 * (1.0 + b.abs())
}

fn declick_out(j: u64, len: u64) -> f32 {
    openlab_audio::ramp_value(1.0, 0.0, j + 1, len, Curve::Linear)
}

fn declick_in(j: u64) -> f32 {
    openlab_audio::ramp_value(0.0, 1.0, j, D, Curve::Linear)
}

/// Review M3: the declick windows hold the frames after the pause point (not a repeated frame),
/// at the frozen gains, across odd block splits.
#[test]
fn declick_windows_play_the_frames_after_the_pause_point() {
    let data = ramp_signal(4000);
    let scenario = |ctl: &mut MixerController| {
        start(ctl, 0, 1, source(&data), 1);
        ctl.send(0, master(1, 0.5, Fade::NONE));
    };
    let (p, r) = (1003u64, 2501u64);
    let (mut ctl, mut rr) = setup(config(1));
    scenario(&mut ctl);
    let reference = render(&mut rr, 4000, BLOCKS);
    for blocks in [&[1usize][..], &[3, 1, 7, 13], OTHER_BLOCKS] {
        let (mut ctl, mut rr) = setup(config(1));
        scenario(&mut ctl);
        ctl.send(p, pause(1));
        ctl.send(r, resume(1, 2));
        let out = render(&mut rr, 4000 + (r - p) as usize, blocks);
        assert_shifted(&out, &reference, p as usize, r as usize);
        for j in 0..D {
            let (o, w) = (out[(p + j) as usize], reference[(p + j) as usize]);
            assert!(
                close(o, w * declick_out(j, D)),
                "tail frame {j}: {o} vs {w}"
            );
            let (o, w) = (out[(r + j) as usize], reference[(p + j) as usize]);
            assert!(close(o, w * declick_in(j)), "fade-in frame {j}: {o} vs {w}");
        }
    }
}

/// Review M2: a resume during the declick fade-out crossfades from the tail (which keeps
/// playing on from where it was) into the frame after the pause point, so the waveform never
/// jumps.
#[test]
fn resume_mid_tail_crossfades_into_the_pause_point() {
    let data = sine(6000, SR, 440.0, 0.8);
    let natural = data
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max);
    let (p, r) = (1000u64, 1020u64);
    let (mut ctl, mut rr) = setup(config(1));
    start(&mut ctl, 0, 1, source(&data), 1);
    let reference = render(&mut rr, 6000, BLOCKS);
    for blocks in [&[1usize][..], BLOCKS, OTHER_BLOCKS] {
        let (mut ctl, mut rr) = setup(config(1));
        start(&mut ctl, 0, 1, source(&data), 1);
        ctl.send(p, pause(1));
        ctl.send(r, resume(1, 2));
        let out = render(&mut rr, 6020, blocks);
        let step = out[900..1400]
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(step <= 1.5 * natural, "step {step} vs {natural}");
        let lag = r - p;
        for j in 0..D {
            let tail = if lag + j < D {
                reference[(p + lag + j) as usize] * declick_out(lag + j, D)
            } else {
                0.0
            };
            let want = reference[(p + j) as usize] * declick_in(j) + tail;
            let o = out[(r + j) as usize];
            assert!(
                (o - want).abs() < 1e-5,
                "crossfade frame {j}: {o} vs {want}"
            );
        }
        assert_shifted(&out, &reference, p as usize, r as usize);
    }
}

fn devamp(voice: u64, stop: bool) -> CommandKind {
    CommandKind::Devamp {
        voice: VoiceId(voice),
        stop,
    }
}

/// Starts voice 1 at 0 on an infinite 100-frame loop followed by one more slice.
fn start_vamp(ctl: &mut MixerController) {
    let slices = plan(0..300, &[100], &[Plays::Infinite, Plays::Times(1)], 0).unwrap();
    let ring: Vec<f32> = (0..20)
        .flat_map(|_| (0..100).map(marker))
        .chain((100..300).map(marker))
        .collect();
    let mut v = voice(ctl, 1, source(&ring), 1);
    v.slices = slices;
    send_start(ctl, 0, v);
}

fn scheduled(voice: u64, at: u64) -> Event {
    Event::DevampScheduled {
        voice: VoiceId(voice),
        at,
    }
}

/// A repeated devamp (same `stop`) is answered again at the same boundary; one with the other
/// `stop` is ignored. Neither changes playback.
#[test]
fn repeated_devamp_is_idempotent() {
    for stop in [true, false] {
        let (mut ctl, mut r) = setup(config(1));
        // A two-play loop, devamped in its last play: continuing needs no stream link.
        let slices = plan(0..300, &[100], &[Plays::Times(2), Plays::Times(1)], 0).unwrap();
        let mut v = voice(&ctl, 1, source(&unrolled(&slices)), 1);
        v.slices = slices;
        send_start(&mut ctl, 0, v);
        ctl.send(140, devamp(1, stop));
        ctl.send(160, devamp(1, stop));
        ctl.send(170, devamp(1, !stop));
        ctl.send(199, devamp(1, stop));
        let out = render(&mut r, 600, BLOCKS);
        let mut want = vec![
            Event::LoopReached {
                voice: VoiceId(1),
                at: 100,
                slice: 0,
                count: 1,
            },
            scheduled(1, 200),
            scheduled(1, 200),
            Event::DevampIgnored {
                voice: VoiceId(1),
                at: 170,
            },
            scheduled(1, 200),
            Event::SliceEnded {
                voice: VoiceId(1),
                at: 200,
                slice: 0,
            },
        ];
        if stop {
            want.push(finished(1, 200, FinishReason::OutPoint));
            assert!(out[200..].iter().all(|&s| s == 0.0));
        } else {
            // One devamp, carried out once: the next slice plays straight through.
            let next: Vec<f32> = (100..300).map(marker).collect();
            assert_eq!(&out[200..400], &next[..]);
            want.push(Event::SliceEnded {
                voice: VoiceId(1),
                at: 400,
                slice: 1,
            });
            want.push(finished(1, 400, FinishReason::OutPoint));
        }
        assert_eq!(events(&mut ctl), want, "stop: {stop}");
    }
}

/// The cue engine's sequence (docs/cue-engine.md, M4.2 contract): a devamp pending across a
/// pause is re-sent for the fresh id after the resume. Exactly one `DevampScheduled` answers
/// each devamp sent, the re-sent one with the corrected sample, and the loop is left there.
#[test]
fn devamp_resent_after_resume_gets_one_answer_for_the_new_id() {
    let (mut ctl, mut r) = setup(config(1));
    start_vamp(&mut ctl);
    ctl.send(140, devamp(1, true));
    ctl.send(150, pause(1));
    ctl.send(1000, resume(1, 4));
    ctl.send(1000, devamp(4, true));
    let out = render(&mut r, 1500, BLOCKS);
    assert_eq!(
        events(&mut ctl),
        vec![
            Event::LoopReached {
                voice: VoiceId(1),
                at: 100,
                slice: 0,
                count: 1
            },
            scheduled(1, 200),
            scheduled(4, 1050),
            Event::SliceEnded {
                voice: VoiceId(4),
                at: 1050,
                slice: 0
            },
            finished(4, 1050, FinishReason::OutPoint),
        ]
    );
    assert!(out[1050..].iter().all(|&s| s == 0.0));
    assert_ne!(out[1049], 0.0);
}

/// Devamps sent while paused (new, or repeats of the pending one) are answered once, on the
/// resume, under the new id.
#[test]
fn devamps_while_paused_are_answered_once_on_resume() {
    let (mut ctl, mut r) = setup(config(1));
    start_vamp(&mut ctl);
    ctl.send(140, devamp(1, true));
    ctl.send(150, pause(1));
    ctl.send(400, devamp(1, true));
    ctl.send(500, devamp(1, true));
    ctl.send(600, devamp(1, false));
    ctl.send(1000, resume(1, 4));
    render(&mut r, 1500, BLOCKS);
    assert_eq!(
        events(&mut ctl),
        vec![
            Event::LoopReached {
                voice: VoiceId(1),
                at: 100,
                slice: 0,
                count: 1
            },
            scheduled(1, 200),
            Event::DevampIgnored {
                voice: VoiceId(1),
                at: 600,
            },
            scheduled(4, 1050),
            Event::SliceEnded {
                voice: VoiceId(4),
                at: 1050,
                slice: 0
            },
            finished(4, 1050, FinishReason::OutPoint),
        ]
    );
}

/// Review M1: a resume dated before its pause applies is for a voice that is not paused yet, so
/// it is ignored and the voice stays paused (the engine must date it at or after the pause).
#[test]
fn resume_dated_before_its_pause_is_ignored() {
    let (mut ctl, mut r) = setup(config(1));
    start(&mut ctl, 100, 1, source(&vec![0.5; 1000]), 1);
    ctl.send(100, pause(1));
    ctl.send(97, resume(1, 2));
    let out = render(&mut r, 3000, BLOCKS);
    assert!(out.iter().all(|&s| s == 0.0));
    assert!(events(&mut ctl).is_empty());
    // Still paused under its old id: a correctly dated resume plays it.
    ctl.send(3000, resume(1, 2));
    let out = render(&mut r, 2000, BLOCKS);
    assert!(out[D as usize..1000].iter().all(|&s| s == 0.5));
    assert_eq!(
        events(&mut ctl),
        vec![finished(2, 4000, FinishReason::EndOfStream)]
    );
}

/// Review L1: with fewer than D frames to peek, the declick still reaches silence on the last.
#[test]
fn short_tail_reaches_silence_on_the_last_peekable_frame() {
    let (_tx, rx) = open_source(2000, &vec![1.0; 1100]);
    let (mut ctl, mut r) = setup(config(1));
    start(&mut ctl, 0, 1, rx, 1);
    ctl.send(1000, pause(1));
    let out = render(&mut r, 1400, BLOCKS);
    for j in 0..100 {
        assert_eq!(out[1000 + j], declick_out(j as u64, 100), "frame {j}");
    }
    assert!(out[1100..].iter().all(|&s| s == 0.0));
    assert_eq!(events(&mut ctl), vec![]);
}
