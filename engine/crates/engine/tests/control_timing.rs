// SPDX-License-Identifier: GPL-3.0-or-later

//! Control loop on the offline render driver: sample-exact starts, continues, fades, stops
//! (NFR-20, NFR-21, NFR-22; ARCHITECTURE §3.2).

mod show;

use openlab_audio::RAMP_TOLERANCE;
use openlab_core::protocol::WireWarning;
use openlab_core::protocol::{TransportAction, WireCueState};
use openlab_core::{ContinueMode, CueKind, FadeCurve, FadeLevels, FadeSpec, GroupMode};
use openlab_engine::control::MAX_WAIT;
use openlab_engine::control::MIN_STALL_WAIT;
use show::*;

#[test]
fn go_starts_audio_exactly_one_block_after_now() {
    let path = wav(&dc(800, 0.5));
    let mut s = show(workspace(vec![audio(1, &path, 0)]));
    // Primed while standing by (ARCHITECTURE §3.5).
    assert_eq!(s.control().primed(), vec![id(1)]);
    s.run(BLOCK * 3);
    let at = go(&mut s);
    assert_eq!(at, s.rendered() + BLOCK);
    s.run(2000);
    let ch0 = s.channel(0);
    assert_eq!(first_nonzero(&ch0), Some(at as usize));
    assert_eq!(last_nonzero(&ch0), Some(at as usize + 799));
    assert!(
        ch0[at as usize..at as usize + 800]
            .iter()
            .all(|&v| v == 0.5)
    );
    assert!(s.channel(1).iter().all(|&v| v == 0.0));
    let msgs = s.take_outbound();
    let st = states(&msgs, id(1));
    assert_eq!(
        st.iter().map(|s| s.0).collect::<Vec<_>>(),
        vec![
            WireCueState::PreWaiting,
            WireCueState::Running,
            WireCueState::Idle
        ]
    );
    assert_eq!(s.stalls(), 0);
    assert_eq!(s.control().live_voices(), 0);
}

#[test]
fn a_group_starts_its_children_on_the_same_sample() {
    let a = wav(&dc(400, 0.25));
    let b = wav(&dc(400, 0.75));
    let group = cue(
        10,
        CueKind::Group {
            mode: GroupMode::Timeline,
            children: vec![audio(1, &a, 0), audio(2, &b, 1)],
        },
    );
    let mut s = show(workspace(vec![group]));
    s.run(BLOCK);
    let at = go(&mut s) as usize;
    s.run(1000);
    assert_eq!(first_nonzero(&s.channel(0)), Some(at));
    assert_eq!(first_nonzero(&s.channel(1)), Some(at));
    assert_eq!(last_nonzero(&s.channel(0)), last_nonzero(&s.channel(1)));
    assert_eq!(s.stalls(), 0);
}

/// M5.11g: Start Selected on several cues is one `batch`: every cue starts on the same sample
/// (NFR-21), and Stop Selected likewise ends them all on one sample.
#[test]
fn a_batch_starts_and_stops_its_cues_on_the_same_sample() {
    let a = wav(&dc(8000, 0.25));
    let b = wav(&dc(8000, 0.75));
    let mut s = show(workspace(vec![audio(1, &a, 0), audio(2, &b, 1)]));
    s.run(BLOCK * 3 + 17);
    let at = s.now() as usize;
    let starts = [2, 1].map(|n| TransportAction::Start { cue: id(n) });
    s.transport(TransportAction::Batch {
        actions: starts.to_vec(),
    });
    s.run(BLOCK * 5 + 3);
    let stops = [1, 2].map(|n| TransportAction::HardStop { cue: id(n) });
    s.transport(TransportAction::Batch {
        actions: stops.to_vec(),
    });
    s.run(BLOCK * 4);
    assert_eq!(first_nonzero(&s.channel(0)), Some(at));
    assert_eq!(first_nonzero(&s.channel(1)), Some(at));
    let end = last_nonzero(&s.channel(0));
    assert!(end.is_some_and(|e| e < at + 8000 - 1), "stopped early");
    assert_eq!(last_nonzero(&s.channel(1)), end);
    for n in [1, 2] {
        assert_eq!(
            s.control().engine().state(id(n)),
            openlab_core::CueState::Idle
        );
    }
    assert_eq!(s.control().live_voices(), 0);
    assert_eq!(s.stalls(), 0);
}

/// The contrast the batch exists for: two lone starts handled in different wakes, with the
/// device rendering a block between them (as it does while a client's messages arrive one by
/// one), land a block apart. The offline driver renders nothing between two messages, so
/// without this the batch test above would pass with two lone starts as well.
#[test]
fn lone_starts_handled_in_different_wakes_land_a_block_apart() {
    let a = wav(&dc(8000, 0.25));
    let b = wav(&dc(8000, 0.75));
    let mut s = show(workspace(vec![audio(1, &a, 0), audio(2, &b, 1)]));
    s.run(BLOCK * 3 + 17);
    let at = s.now() as usize;
    s.transport(TransportAction::Start { cue: id(2) });
    s.run(BLOCK);
    s.transport(TransportAction::Start { cue: id(1) });
    s.run(BLOCK * 4);
    assert_eq!(first_nonzero(&s.channel(1)), Some(at));
    assert_eq!(first_nonzero(&s.channel(0)), Some(at + BLOCK as usize));
}

/// A cue the batch names that is not in this workspace (deleted meanwhile, or another doc's)
/// gets the `unknown_cue` warning, as a lone `start` does; the others still start together.
#[test]
fn a_batch_with_an_unknown_cue_warns_and_starts_the_rest() {
    let a = wav(&dc(800, 0.5));
    let b = wav(&dc(800, 0.5));
    let mut s = show(workspace(vec![audio(1, &a, 0), audio(2, &b, 1)]));
    s.run(BLOCK);
    s.take_outbound();
    let at = s.now() as usize;
    let replies = s.transport(TransportAction::Batch {
        actions: [1, 99, 2]
            .map(|n| TransportAction::Start { cue: id(n) })
            .to_vec(),
    });
    assert!(
        matches!(
            replies.as_slice(),
            [openlab_core::protocol::ServerMessage::Result {
                outcome: openlab_core::protocol::Outcome::Ok { .. },
                ..
            }]
        ),
        "{replies:?}"
    );
    s.run(1000);
    assert_eq!(first_nonzero(&s.channel(0)), Some(at));
    assert_eq!(first_nonzero(&s.channel(1)), Some(at));
    assert_eq!(
        warnings(&s.take_outbound()),
        vec![WireWarning::UnknownCue { cue: id(99) }]
    );
}

#[test]
fn a_pre_wait_lands_on_its_exact_sample() {
    let path = wav(&dc(200, 0.5));
    let mut c = audio(1, &path, 0);
    // 0.3105 s = 2484 frames: not a multiple of the block.
    c.pre_wait = secs(2484);
    let mut s = show(workspace(vec![c]));
    let at = go(&mut s) as usize;
    s.run(4000);
    assert_eq!(first_nonzero(&s.channel(0)), Some(at + 2484));
    assert_eq!(last_nonzero(&s.channel(0)), Some(at + 2484 + 199));
}

#[test]
fn auto_continue_fires_on_its_exact_sample() {
    let a = wav(&dc(2000, 0.25));
    let b = wav(&dc(200, 0.5));
    let mut c1 = audio(1, &a, 0);
    c1.continue_mode = ContinueMode::AutoContinue;
    c1.post_wait = secs(777);
    let mut s = show(workspace(vec![c1, audio(2, &b, 1)]));
    let at = go(&mut s) as usize;
    s.run(3000);
    assert_eq!(first_nonzero(&s.channel(1)), Some(at + 777));
}

#[test]
fn auto_follow_comes_on_the_exact_end_sample() {
    let a = wav(&dc(500, 0.25));
    let b = wav(&dc(500, 0.5));
    let c = wav(&dc(500, 0.75));
    let mut c1 = audio(1, &a, 0);
    c1.continue_mode = ContinueMode::AutoFollow;
    let mut c2 = audio(2, &b, 1);
    c2.continue_mode = ContinueMode::AutoFollow;
    let mut s = show(workspace(vec![c1, c2, audio(3, &c, 0)]));
    let at = go(&mut s) as usize;
    s.run(4000);
    let ch0 = s.channel(0);
    let ch1 = s.channel(1);
    // The engine knows each action's length (`ActionLength`): the next cue starts on the end
    // sample itself, with no gap and no overlap.
    assert_eq!(last_nonzero(&ch0[..at + 600]), Some(at + 499));
    assert_eq!(first_nonzero(&ch1), Some(at + 500));
    assert_eq!(last_nonzero(&ch1), Some(at + 999));
    assert_eq!(
        first_nonzero(&ch0[at + 600..]).map(|i| i + at + 600),
        Some(at + 1000)
    );
    assert_eq!(ch0[at + 1000], 0.75);
    for n in 1..=3 {
        assert_eq!(
            s.control().engine().state(id(n)),
            openlab_core::CueState::Idle
        );
    }
    let msgs = s.take_outbound();
    assert!(
        !warnings(&msgs)
            .iter()
            .any(|w| matches!(w, WireWarning::LateStart { .. }))
    );
    assert_eq!(s.control().live_voices(), 0);
}

#[test]
fn a_playlist_moves_on_on_the_exact_end_sample() {
    let a = wav(&dc(700, 0.25));
    let b = wav(&dc(300, 0.5));
    let playlist = cue(
        10,
        CueKind::Group {
            mode: GroupMode::Playlist {
                loop_: false,
                shuffle: false,
            },
            children: vec![audio(1, &a, 0), audio(2, &b, 1)],
        },
    );
    let mut s = show(workspace(vec![playlist]));
    let at = go(&mut s) as usize;
    s.run(2000);
    assert_eq!(last_nonzero(&s.channel(0)), Some(at + 699));
    assert_eq!(first_nonzero(&s.channel(1)), Some(at + 700));
}

#[test]
fn auto_follow_after_a_pause_comes_on_the_exact_end_sample() {
    let a = wav(&dc(2000, 0.25));
    let b = wav(&dc(300, 0.5));
    let mut c1 = audio(1, &a, 0);
    c1.continue_mode = ContinueMode::AutoFollow;
    let mut s = show(workspace(vec![c1, audio(2, &b, 1)]));
    let at = go(&mut s) as usize;
    s.run(700);
    let p = s.now() as usize;
    s.transport(TransportAction::Pause { cue: id(1) });
    s.run(1111);
    let r = s.now() as usize;
    s.transport(TransportAction::Resume { cue: id(1) });
    s.run(3000);
    let end = at + 2000 + (r - p);
    assert_eq!(last_nonzero(&s.channel(0)), Some(end - 1));
    assert_eq!(first_nonzero(&s.channel(1)), Some(end));
}

#[test]
fn auto_follow_after_a_devamp_comes_on_the_exact_end_sample() {
    // Intro 400, loop 400..1200 until devamped, outro 1200..1600.
    let a = wav(&dc(1600, 0.25));
    let b = wav(&dc(300, 0.5));
    let mut c1 = with_slices(audio(1, &a, 0), &[(secs(400), 1), (secs(1200), 0)], 1);
    c1.continue_mode = ContinueMode::AutoFollow;
    let devamp = cue(
        3,
        CueKind::Devamp {
            target: Some(id(1)),
            start_next_cue: false,
            stop_target_when_slice_ends: false,
            devamp_mode: openlab_core::DevampMode::CurrentSlice,
        },
    );
    let mut s = show(workspace(vec![c1, audio(2, &b, 1), devamp]));
    go(&mut s);
    s.run(3000);
    start(&mut s, 3);
    s.run(4000);
    let end = last_nonzero(&s.channel(0)).unwrap() + 1;
    assert_eq!(first_nonzero(&s.channel(1)), Some(end));
}

#[test]
fn a_late_tick_starts_late_and_keeps_the_later_schedule() {
    let a = wav(&dc(4000, 0.25));
    let b = wav(&dc(300, 0.5));
    let mut c1 = audio(1, &a, 0);
    c1.pre_wait = secs(1000);
    c1.continue_mode = ContinueMode::AutoContinue;
    c1.post_wait = secs(2000);
    let mut s = show(workspace(vec![c1, audio(2, &b, 1)]));
    let at = go(&mut s) as usize;
    // The control thread is held off past the pre-wait's end.
    s.render_only(1000 + 300);
    s.run(BLOCK * 3);
    let ch0 = s.channel(0);
    let started = first_nonzero(&ch0).unwrap();
    assert!(started > at + 1000, "{started}");
    assert!(
        warnings(&s.take_outbound())
            .iter()
            .any(|w| matches!(w, WireWarning::LateStart { .. }))
    );
    // The auto-continue counts from the action's deadline, not from the late start.
    s.run(3000);
    assert_eq!(first_nonzero(&s.channel(1)), Some(at + 1000 + 2000));
}

#[test]
fn a_fade_cue_ramps_a_running_voice() {
    let a = wav(&dc(8000, 1.0));
    let fade = cue(
        2,
        CueKind::Fade {
            target: Some(id(1)),
            duration: secs(800),
            curve: FadeCurve::Linear,
            levels: FadeLevels {
                master_db: Some(20.0 * 0.5f64.log10()),
                output_db: vec![],
                crosspoints: vec![],
            },
            stop_target_when_done: false,
        },
    );
    let mut s = show(workspace(vec![audio(1, &a, 0), fade]));
    go(&mut s);
    s.run(1000);
    let tf = go(&mut s) as usize;
    s.run(2000);
    let ch0 = s.channel(0);
    assert_eq!(ch0[tf - 1], 1.0);
    let half = 0.5f64.powf(1.0);
    let target = openlab_core::db_to_gain(20.0 * half.log10());
    let mid = ch0[tf + 400];
    assert!((mid - (1.0 + target) / 2.0).abs() < 1e-4, "{mid}");
    for &v in &ch0[tf + 800..tf + 1500] {
        assert!(
            f64::from((v - target).abs()) <= f64::from(target) * RAMP_TOLERANCE * 4.0,
            "{v}"
        );
    }
}

#[test]
fn stop_with_fade_fades_out_and_ends_on_time() {
    let a = wav(&dc(8000, 1.0));
    let mut s = show(workspace(vec![audio(1, &a, 0)]));
    go(&mut s);
    s.run(1000);
    s.take_outbound();
    let ts = s.now() as usize;
    s.transport(TransportAction::Stop {
        cue: id(1),
        fade_out: Some(FadeSpec {
            duration: secs(800),
            curve: FadeCurve::Linear,
        }),
    });
    s.run(1200);
    let ch0 = s.channel(0);
    assert_eq!(ch0[ts - 1], 1.0);
    assert!((ch0[ts + 400] - 0.5).abs() < 1e-3);
    assert!(ch0[ts + 800..].iter().all(|&v| v == 0.0));
    let st: Vec<_> = states(&s.take_outbound(), id(1))
        .into_iter()
        .map(|s| s.0)
        .collect();
    assert_eq!(st, vec![WireCueState::Stopping, WireCueState::Idle]);
    run_until_idle(&mut s, id(1), 1000);
    s.run(BLOCK * 4);
    assert_eq!(s.control().live_voices(), 0);
}

#[test]
fn panic_fades_everything_out_together() {
    let a = wav(&dc(16000, 1.0));
    let b = wav(&dc(16000, 0.5));
    let mut s = show(workspace(vec![audio(1, &a, 0), audio(2, &b, 1)]));
    start(&mut s, 1);
    start(&mut s, 2);
    s.run(1000);
    let tp = s.now() as usize;
    s.transport(TransportAction::Panic);
    // panic_duration 0.1 s = 800 frames.
    s.run(1500);
    for ch in 0..2 {
        let x = s.channel(ch);
        assert!(x[tp - 1] > 0.0);
        assert!(x[tp + 400] > 0.0 && x[tp + 400] < x[tp - 1]);
        assert!(x[tp + 800..].iter().all(|&v| v == 0.0), "channel {ch}");
    }
    s.run(BLOCK * 4);
    assert_eq!(s.control().engine().cue_states().count(), 0);
    assert_eq!(s.control().live_voices(), 0);
}

#[test]
fn pause_and_resume_keep_the_position() {
    let path = wav(&ramp(6000));
    let mut s = show(workspace(vec![audio(1, &path, 0)]));
    let t0 = go(&mut s) as usize;
    s.run(1000);
    let p = s.now() as usize;
    s.transport(TransportAction::Pause { cue: id(1) });
    s.run(1500);
    let r = s.now() as usize;
    s.transport(TransportAction::Resume { cue: id(1) });
    let end = run_until_idle(&mut s, id(1), 10_000) as usize;
    s.run(BLOCK * 4);
    let ch0 = s.channel(0);
    let ramp = ramp(6000);
    // Before the pause: the file from its first frame.
    assert_eq!(ch0[t0..p], ramp[..p - t0]);
    // Paused (after the 5 ms = 40 frame declick): silence.
    assert!(ch0[p + 40..r].iter().all(|&v| v == 0.0));
    // After the resume's declick: exactly where it paused.
    let k = p - t0;
    assert_eq!(ch0[r + 40..r + 1000], ramp[k + 40..k + 1000]);
    // It ends 6000 frames of play after its start, the pause added.
    assert_eq!(last_nonzero(&ch0), Some(t0 + 6000 + (r - p) - 1));
    // The engine knew the end in advance: it completed the cue on it, ahead of the render.
    assert!(end <= t0 + 6000 + (r - p));
    assert_eq!(s.control().live_voices(), 0);
}

#[test]
fn load_to_time_starts_part_way_in() {
    let path = wav(&ramp(4000));
    let mut s = show(workspace(vec![audio(1, &path, 0)]));
    s.transport(TransportAction::Load {
        cue: id(1),
        at: secs(1000),
    });
    s.run(BLOCK * 2);
    let at = start(&mut s, 1) as usize;
    s.run(4000);
    let ch0 = s.channel(0);
    let ramp = ramp(4000);
    assert_eq!(first_nonzero(&ch0), Some(at));
    assert_eq!(ch0[at..at + 3000], ramp[1000..]);
    assert_eq!(last_nonzero(&ch0), Some(at + 2999));
}

/// M4.11b: a load past the end of an audio cue's length loads it to its end (QLab 5). Starting it
/// plays nothing and ends on its start sample, so its auto-follow successor starts on that
/// sample too, with no `late_start` warning.
#[test]
fn load_past_the_end_starts_the_successor_on_time() {
    let a = wav(&dc(500, 0.25));
    let b = wav(&dc(500, 0.5));
    let mut c1 = audio(1, &a, 0);
    c1.continue_mode = ContinueMode::AutoFollow;
    let mut s = show(workspace(vec![c1, audio(2, &b, 1)]));
    s.transport(TransportAction::Load {
        cue: id(1),
        at: secs(10_000),
    });
    s.run(BLOCK * 2);
    // Loaded, not complete: nothing has played and nothing followed.
    assert_eq!(
        s.control().engine().state(id(1)),
        openlab_core::CueState::Loaded
    );
    assert_eq!(
        s.control().engine().state(id(2)),
        openlab_core::CueState::Idle
    );
    let at = start(&mut s, 1) as usize;
    s.run(4000);
    let ch0 = s.channel(0);
    let ch1 = s.channel(1);
    assert_eq!(first_nonzero(&ch0), None);
    assert_eq!(first_nonzero(&ch1), Some(at));
    assert_eq!(last_nonzero(&ch1), Some(at + 499));
    let msgs = s.take_outbound();
    assert!(
        !warnings(&msgs)
            .iter()
            .any(|w| matches!(w, WireWarning::LateStart { .. })),
        "{:?}",
        warnings(&msgs)
    );
    assert_eq!(s.control().live_voices(), 0);
}

/// The `wait_hint` the loop would sleep for before each of `ticks` ticks while the device renders
/// nothing (a stall, before the watchdog declares a loss). Sleeping is virtual: only counted.
fn hints_while_frozen(
    s: &mut openlab_engine::control::offline::OfflineShow,
    ticks: usize,
) -> Vec<std::time::Duration> {
    (0..ticks)
        .map(|_| {
            let hint = s.control().wait_hint();
            s.tick();
            hint
        })
        .collect()
}

/// M4.2j: a deadline one frame outside the window gives a one-frame `wait_hint`. With sample
/// time frozen it never comes into the window, so the loop used to wake every frame (125 us at
/// 8 kHz, 21 us at 48 kHz), ticking at full cost for as long as the stall lasts (up to 3 s).
#[test]
fn a_frozen_sample_clock_does_not_spin_the_loop() {
    let path = wav(&dc(200, 0.5));
    let mut c = audio(1, &path, 0);
    c.pre_wait = secs(BLOCK);
    let mut s = show(workspace(vec![c]));
    s.run(BLOCK * 3);
    go(&mut s);
    let frame = std::time::Duration::from_secs(1) / RATE;
    // Time advances: the hint is the exact distance (no added latency on the normal path).
    assert_eq!(s.control().wait_hint(), frame);
    let hints = hints_while_frozen(&mut s, 3000);
    // The first wake after a tick that saw progress may be short; every later one waits at
    // least the floor, so 3000 ticks of a stall cost at least 3000 ms of waiting.
    let waited: std::time::Duration = hints.iter().sum();
    assert!(
        hints[1..].iter().all(|h| *h >= MIN_STALL_WAIT),
        "a hint under the floor"
    );
    assert!(
        waited >= std::time::Duration::from_millis(2999),
        "{waited:?}"
    );
    // Rendering resumes: the tick sees progress again.
    let before = s.rendered();
    s.step();
    assert!(s.rendered() > before);
}

/// A stall with nothing due waits the full bound.
#[test]
fn an_idle_stalled_loop_waits_the_full_bound() {
    let mut s = show(workspace(vec![]));
    s.run(BLOCK);
    let hints = hints_while_frozen(&mut s, 5);
    assert!(hints.iter().all(|h| *h == MAX_WAIT));
}

/// The floor is a quarter block at most: a small device block (here 16 frames = 2 ms) gets a
/// floor under 1 ms, and a deadline entering the window is still started on its sample.
#[test]
fn a_small_block_keeps_its_floor_below_the_window() {
    use openlab_engine::control::stall_floor;
    assert_eq!(stall_floor(64, 8000), std::time::Duration::from_millis(1));
    assert_eq!(
        stall_floor(64, 48000),
        std::time::Duration::from_secs_f64(64.0 / 48000.0 / 4.0)
    );
    let path = wav(&dc(200, 0.5));
    let mut c = audio(1, &path, 0);
    c.pre_wait = secs(1000);
    let ws = workspace(vec![c]);
    let mut cfg = config(&ws);
    cfg.block_frames = 16;
    let mut s = show_with(ws, cfg);
    let at = go(&mut s) as usize;
    s.run(2000);
    assert_eq!(first_nonzero(&s.channel(0)), Some(at + 1000));
}
