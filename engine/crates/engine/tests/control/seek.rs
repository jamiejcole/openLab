// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! Seek (a click in a cue's waveform moves where it plays, CUE-05 load to time): rendered
//! offline at 8 kHz on a ramp file whose every sample names its frame, so what is heard after a
//! move shows where in the file it comes from.

use crate::show::*;
use openlab_core::protocol::{
    RuntimeChange, ServerMessage, TransportAction, WireCueState, WireWarning,
};
use openlab_core::{ContinueMode, CueKind, CueState, FadeCurve, FadeSpec};
use openlab_engine::control::offline::OfflineShow;

/// The crossfade a move plays (10 ms at 8 kHz).
const XFADE: usize = 80;

/// Sends the seek; returns what was broadcast since the last call that took the outbound
/// messages, the seek's own reports included.
fn seek(s: &mut OfflineShow, n: u128, seconds: f64) -> Vec<ServerMessage> {
    s.transport(TransportAction::Seek {
        cue: id(n),
        position: seconds,
    });
    s.take_outbound()
}

/// The file frame a ramp sample names (sample `i` of `ramp` is `(i + 1) / 2^20`); `None` for
/// silence.
fn frame(v: f32) -> Option<f64> {
    (v != 0.0).then(|| f64::from(v) * 1_048_576.0 - 1.0)
}

/// Asserts output samples `from..to` play file frames `first..` one after another, exactly.
fn plays_from(ch: &[f32], from: usize, to: usize, first: f64) {
    for (k, i) in (from..to).enumerate() {
        let got = frame(ch[i]);
        assert!(
            got.is_some_and(|f| (f - (first + k as f64)).abs() < 0.01),
            "sample {i}: heard {got:?}, wanted file frame {}",
            first + k as f64
        );
    }
}

/// Where a move's crossfade begins: the sample before the first one after `from` that does
/// not continue the file (on its first sample the moved voice is still silent).
fn jump(ch: &[f32], from: usize) -> usize {
    (from + 1..ch.len())
        .find(|&i| match (frame(ch[i - 1]), frame(ch[i])) {
            (Some(a), Some(b)) => (b - a - 1.0).abs() > 0.01,
            _ => true,
        })
        .unwrap()
        - 1
}

/// The largest step between two output samples in `from..to`: no click is bigger than the
/// crossfade's share of the jump.
/// [`jump`] for a voice looping its first `len` frames since its first sample: where the move
/// out of the loop's passes begins, at or after `from`.
fn loop_jump(ch: &[f32], from: usize, len: usize) -> usize {
    let first = first_nonzero(ch).unwrap();
    (from..ch.len())
        .find(|&i| {
            let want = ((i - first) % len) as f64;
            frame(ch[i]).is_none_or(|f| (f - want).abs() > 0.01)
        })
        .unwrap()
        - 1
}

fn largest_step(ch: &[f32], from: usize, to: usize) -> f32 {
    ch[from..to]
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max)
}

fn states_of(msgs: &[ServerMessage], n: u128) -> Vec<WireCueState> {
    states(msgs, id(n)).into_iter().map(|(s, _)| s).collect()
}

fn state(s: &OfflineShow, n: u128) -> CueState {
    s.control().engine().state(id(n))
}

#[test]
fn idle_cue_click_loads_at_the_point_and_go_plays_from_there() {
    let path = wav(&ramp(8000));
    let mut s = show(workspace(vec![audio(1, &path, 0)]));
    s.take_outbound();
    let msgs = seek(&mut s, 1, 0.5);
    assert_eq!(states_of(&msgs, 1), vec![WireCueState::Loaded]);
    s.run(BLOCK * 4);
    assert!(
        s.channel(0).iter().all(|&v| v == 0.0),
        "a load plays nothing"
    );
    let at = start(&mut s, 1) as usize;
    s.run(800);
    let ch = s.channel(0);
    // From the point clicked, on the start sample.
    assert_eq!(first_nonzero(&ch), Some(at));
    plays_from(&ch, at, at + 700, 4000.0);
}

#[test]
fn playing_cue_click_jumps_there_and_plays_on_without_a_stop_or_a_click() {
    let path = wav(&ramp(8000));
    let mut s = show(workspace(vec![audio(1, &path, 0)]));
    let at = start(&mut s, 1) as usize;
    s.run(1600);
    s.take_outbound();
    let clicked = s.now() as usize;
    let msgs = seek(&mut s, 1, 0.75);
    s.run(1200);
    let msgs = [msgs, s.take_outbound()].concat();
    // No stop, no restart: the cue stays running all along.
    assert!(states_of(&msgs, 1).is_empty(), "{:?}", states_of(&msgs, 1));
    assert_eq!(state(&s, 1), CueState::Running);
    let ch = s.channel(0);
    let swap = jump(&ch, at);
    // The old point played on until the move, at most a couple of blocks after the click.
    plays_from(&ch, at, swap, 0.0);
    assert!(
        swap >= clicked && swap <= clicked + 3 * BLOCK as usize,
        "{swap} {clicked}"
    );
    // After the crossfade the file plays on from the point clicked, sample for sample.
    plays_from(&ch, swap + XFADE, swap + 1000, 6000.0 + XFADE as f64);
    // The crossfade moves at most its share of the jump per sample: no click.
    let leap = (6000.0 - (swap - at) as f32) / 1_048_576.0;
    let step = largest_step(&ch, at, swap + 1000);
    assert!(step <= 2.0 * leap / XFADE as f32, "{step} vs {leap}");
    // The play line is at the point too, and moves on with the sound.
    let items: Vec<(f64, f64)> = msgs
        .iter()
        .filter_map(|m| match m {
            ServerMessage::Progress { items, .. } => {
                items.first().and_then(|i| Some((i.elapsed, i.position?)))
            }
            _ => None,
        })
        .collect();
    let (elapsed, last) = *items.last().unwrap();
    assert!((0.75..0.95).contains(&last), "{items:?}");
    // Elapsed goes on from the point too (no slices: the same as the file position).
    assert!((elapsed - last).abs() < 0.001, "{items:?}");
}

#[test]
fn auto_follow_after_a_click_fires_once_at_the_new_end() {
    let path = wav(&ramp(8000));
    let tone = wav(&dc(4000, 0.5));
    let mut first = audio(1, &path, 0);
    first.continue_mode = ContinueMode::AutoFollow;
    let mut s = show(workspace(vec![first, audio(2, &tone, 1)]));
    let at = start(&mut s, 1) as usize;
    s.run(1600);
    seek(&mut s, 1, 0.9);
    s.run(9000);
    let msgs = s.take_outbound();
    let ch = s.channel(0);
    let swap = jump(&ch, at);
    // 800 frames were left from 0.9 s: the next cue starts as the moved sound ends.
    let followed = first_nonzero(&s.channel(1)).unwrap();
    assert_eq!(followed, swap + 800);
    assert_eq!(last_nonzero(&ch), Some(swap + 799));
    // Once: never again at the old end (8000 frames after the start).
    let runs = states_of(&msgs, 2)
        .into_iter()
        .filter(|s| *s == WireCueState::PreWaiting)
        .count();
    assert_eq!(runs, 1);
}

#[test]
fn paused_cue_click_moves_the_point_stays_paused_and_resume_plays_from_there() {
    let path = wav(&ramp(8000));
    let mut s = show(workspace(vec![audio(1, &path, 0)]));
    start(&mut s, 1);
    s.run(800);
    s.transport(TransportAction::Pause { cue: id(1) });
    s.run(400);
    s.take_outbound();
    let msgs = seek(&mut s, 1, 0.5);
    let quiet_from = s.now() as usize;
    s.run(800);
    assert!(states_of(&[msgs, s.take_outbound()].concat(), 1).is_empty());
    assert_eq!(state(&s, 1), CueState::Paused);
    let ch = s.channel(0);
    assert!(
        ch[quiet_from..].iter().all(|&v| v == 0.0),
        "nothing plays while paused"
    );
    let resumed = s.now() as usize;
    s.transport(TransportAction::Resume { cue: id(1) });
    s.run(800);
    let ch = s.channel(0);
    // Its first sample is the fade-in's silent one.
    assert_eq!(
        first_nonzero(&ch[resumed..]).map(|i| i + resumed),
        Some(resumed + 1)
    );
    // It fades in over the crossfade from the point, then plays on from there.
    plays_from(&ch, resumed + XFADE, resumed + 700, 4000.0 + XFADE as f64);
    assert!(ch[resumed] < ch[resumed + XFADE - 1]);
}

#[test]
fn click_in_a_later_slice_while_a_slice_loops_plays_from_there() {
    // Loop 0..800 until devamped, then 800..1600 once.
    let path = wav(&ramp(1600));
    let looped = with_slices(audio(1, &path, 0), &[(secs(800), 0)], 1);
    let mut s = show(workspace(vec![looped]));
    start(&mut s, 1);
    s.run(2000);
    let clicked = s.now() as usize;
    seek(&mut s, 1, secs(1200));
    s.run(1200);
    let ch = s.channel(0);
    let swap = loop_jump(&ch, clicked, 800);
    plays_from(&ch, swap + XFADE, swap + 400, 1200.0 + XFADE as f64);
    // It ends at the file's end: the loop it left is not heard again.
    assert_eq!(last_nonzero(&ch), Some(swap + 399));
    assert_eq!(state(&s, 1), CueState::Idle);
}

#[test]
fn click_inside_a_looping_slice_plays_from_there_and_keeps_looping() {
    let path = wav(&ramp(1600));
    let looped = with_slices(audio(1, &path, 0), &[(secs(800), 0)], 1);
    let mut s = show(workspace(vec![looped]));
    start(&mut s, 1);
    s.run(1000);
    let clicked = s.now() as usize;
    seek(&mut s, 1, secs(400));
    s.run(3000);
    let ch = s.channel(0);
    let swap = loop_jump(&ch, clicked, 800);
    plays_from(&ch, swap + XFADE, swap + 400, 400.0 + XFADE as f64);
    // Then the loop again, from its start, every pass: never once only.
    plays_from(&ch, swap + 400, swap + 1200, 0.0);
    plays_from(&ch, swap + 1200, swap + 2000, 0.0);
    assert_eq!(state(&s, 1), CueState::Running);
}

#[test]
fn click_inside_a_slice_played_three_times_plays_that_pass_then_the_other_two() {
    // 0..800 three times, then 800..1600 once.
    let path = wav(&ramp(1600));
    let sliced = with_slices(audio(1, &path, 0), &[(secs(800), 3)], 1);
    let mut s = show(workspace(vec![sliced]));
    s.run(BLOCK);
    seek(&mut s, 1, secs(400));
    let at = start(&mut s, 1) as usize;
    s.run(4000);
    let ch = s.channel(0);
    plays_from(&ch, at, at + 400, 400.0);
    plays_from(&ch, at + 400, at + 1200, 0.0);
    plays_from(&ch, at + 1200, at + 2000, 0.0);
    plays_from(&ch, at + 2000, at + 2800, 800.0);
    assert_eq!(last_nonzero(&ch), Some(at + 2799));
}

#[test]
fn click_outside_the_trims_plays_from_the_nearest_trim() {
    let path = wav(&ramp(8000));
    let mut c = audio(1, &path, 0);
    if let CueKind::Audio {
        start_time,
        end_time,
        ..
    } = &mut c.kind
    {
        *start_time = 0.1;
        *end_time = Some(0.5);
    }
    let mut s = show(workspace(vec![c]));
    // Before the start trim: loaded at the start trim.
    seek(&mut s, 1, 0.05);
    let at = start(&mut s, 1) as usize;
    s.run(800);
    plays_from(&s.channel(0), at, at + 700, 800.0);
    // Past the end trim while playing: nothing is left, so the sound fades out and the cue
    // ends there.
    seek(&mut s, 1, 0.9);
    s.run(800);
    assert_eq!(state(&s, 1), CueState::Idle);
    let ch = s.channel(0);
    let end = last_nonzero(&ch).unwrap();
    assert!(end < at + 800 + 3 * BLOCK as usize + XFADE, "{end}");
}

#[test]
fn pre_wait_click_ends_the_pre_wait_and_starts_from_the_point_now() {
    let path = wav(&ramp(8000));
    let mut c = audio(1, &path, 0);
    c.pre_wait = 1.0;
    let mut s = show(workspace(vec![c]));
    start(&mut s, 1);
    s.run(800);
    assert_eq!(state(&s, 1), CueState::PreWaiting);
    let clicked = s.now() as usize;
    seek(&mut s, 1, 0.5);
    s.run(800);
    assert_eq!(state(&s, 1), CueState::Running);
    let ch = s.channel(0);
    let heard = first_nonzero(&ch).unwrap();
    assert!(
        heard >= clicked && heard <= clicked + 2 * BLOCK as usize,
        "{heard}"
    );
    plays_from(&ch, heard, heard + 600, 4000.0);
}

#[test]
fn a_fade_on_the_cue_carries_on_on_the_moved_voice() {
    // A steady tone so the level is what is heard; the Fade takes it to silence over 1 s.
    let tone = wav(&dc(16000, 0.5));
    let fader = cue(
        2,
        CueKind::Fade {
            target: Some(id(1)),
            duration: 1.0,
            curve: FadeCurve::Linear,
            levels: openlab_core::FadeLevels {
                master_db: Some(-40.0),
                output_db: vec![],
                crosspoints: vec![],
            },
            stop_target_when_done: false,
            mode: Default::default(),
        },
    );
    let mut s = show(workspace(vec![audio(1, &tone, 0), fader]));
    start(&mut s, 1);
    start(&mut s, 2);
    s.run(4000);
    let before = s.channel(0)[s.rendered() as usize - 1];
    seek(&mut s, 1, 0.25);
    s.run(800);
    let ch = s.channel(0);
    let after = &ch[ch.len() - 400..];
    // Not back at the model's 0 dB: still on the fade's way down, lower than before the move.
    assert!(
        after.iter().all(|&v| v > 0.0 && v < before),
        "{before} {after:?}"
    );
    s.run(8000);
    let ch = s.channel(0);
    let end = ch[ch.len() - 1];
    assert!((f64::from(end) - 0.5 * 0.01).abs() < 1e-4, "{end}");
}

#[test]
fn a_group_child_moved_while_its_group_runs_leaves_its_sibling_alone() {
    let a = wav(&ramp(8000));
    let b = wav(&ramp(8000));
    let group = cue(
        10,
        CueKind::Group {
            mode: openlab_core::GroupMode::Timeline,
            children: vec![audio(1, &a, 0), audio(2, &b, 1)],
        },
    );
    let mut s = show(workspace(vec![group]));
    let at = start(&mut s, 10) as usize;
    s.run(800);
    seek(&mut s, 1, 0.5);
    s.run(800);
    assert_eq!(state(&s, 10), CueState::Running);
    assert_eq!(state(&s, 2), CueState::Running);
    // The sibling played on untouched, sample for sample.
    let other = s.channel(1);
    let from = first_nonzero(&other).unwrap();
    assert_eq!(from, at);
    plays_from(&other, from, s.rendered() as usize, 0.0);
}

#[test]
fn previewing_cue_click_jumps_the_preview_and_does_not_end_it() {
    let path = wav(&ramp(8000));
    let mut s = show(workspace(vec![audio(1, &path, 0)]));
    s.transport(TransportAction::Preview { cue: id(1) });
    s.run(800);
    seek(&mut s, 1, 0.5);
    s.run(800);
    assert!(s.control().engine().is_previewing(id(1)));
    let ch = s.channel(0);
    let at = first_nonzero(&ch).unwrap();
    let swap = jump(&ch, at);
    plays_from(&ch, swap + XFADE, swap + 600, 4000.0 + XFADE as f64);
}

#[test]
fn stopping_a_cue_while_its_move_waits_silences_the_old_point() {
    let path = wav(&ramp(8000));
    let mut s = show(workspace(vec![audio(1, &path, 0)]));
    start(&mut s, 1);
    s.run(800);
    // The click and the stop in the same wake: the move is still waiting for its stream.
    seek(&mut s, 1, 0.5);
    s.transport(TransportAction::HardStop { cue: id(1) });
    let stopped = s.now() as usize;
    s.run(800);
    assert_eq!(state(&s, 1), CueState::Idle);
    let ch = s.channel(0);
    assert!(
        ch[stopped..].iter().all(|&v| v == 0.0),
        "the old point plays on"
    );
    assert_eq!(s.control().live_voices(), 0);
}

#[test]
fn a_burst_of_clicks_never_piles_up_streams_or_voices() {
    let path = wav(&ramp(8000));
    let mut s = show(workspace(vec![audio(1, &path, 0)]));
    start(&mut s, 1);
    s.run(400);
    // Hundreds in one wake, then one per block (a hostile client, a stuck key).
    for k in 0..300u32 {
        seek(&mut s, 1, f64::from(k % 50) / 100.0);
    }
    for k in 0..60u32 {
        seek(&mut s, 1, f64::from(k) / 200.0);
        s.step();
        assert!(
            s.control().live_voices() <= 3,
            "{}",
            s.control().live_voices()
        );
        // The streams the loop holds (the disk workers let go of a dropped one on their own
        // time): the playing one, one fading out, one waiting.
        assert!(
            s.control().held_streams() <= 3,
            "{}",
            s.control().held_streams()
        );
    }
    let msgs = s.take_outbound();
    assert!(warnings(&msgs).is_empty(), "{:?}", warnings(&msgs));
    assert_eq!(state(&s, 1), CueState::Running);
    // It settles on the last point clicked and plays on from there.
    seek(&mut s, 1, 0.6);
    s.run(1200);
    let ch = s.channel(0);
    let n = ch.len();
    let tail = frame(ch[n - 1]).unwrap();
    plays_from(&ch, n - 200, n, tail - 199.0);
    assert!(tail > 4800.0 && tail < 6000.0, "{tail}");
}

#[test]
fn clicks_while_the_file_is_still_being_read_warn_nothing_and_go_plays_from_the_last() {
    let path = wav(&ramp(8000));
    let ws = workspace(vec![audio(1, &path, 0)]);
    let mut config = config(&ws);
    config.probe = gated_probe;
    gate(&path);
    let mut s = OfflineShow::new(ws, mixer_config(), config).unwrap();
    s.run(BLOCK * 2);
    assert_eq!(s.control().probes_pending(), 1);
    let mut msgs = Vec::new();
    for k in 0..50u32 {
        msgs.extend(seek(&mut s, 1, f64::from(k) / 100.0));
        s.step();
    }
    msgs.extend(s.take_outbound());
    assert!(warnings(&msgs).is_empty(), "{:?}", warnings(&msgs));
    assert_eq!(state(&s, 1), CueState::Loaded);
    assert!(s.control().open_streams() <= 1);
    release(&path);
    s.settle_media();
    let at = start(&mut s, 1) as usize;
    s.run(800);
    plays_from(&s.channel(0), at, at + 600, 3920.0);
}

#[test]
fn broken_cue_click_loads_and_sounds_nothing() {
    let missing = fresh_path("wav");
    let mut s = show(workspace(vec![audio(1, &missing, 0)]));
    assert_eq!(state(&s, 1), CueState::Broken);
    s.take_outbound();
    let msgs = seek(&mut s, 1, 0.5);
    assert!(
        warnings(&msgs).contains(&WireWarning::CueBroken { cue: id(1) }),
        "{msgs:?}"
    );
    start(&mut s, 1);
    s.run(800);
    assert_eq!(state(&s, 1), CueState::Broken);
    assert!(s.channel(0).iter().all(|&v| v == 0.0));
    assert_eq!(s.control().live_voices(), 0);
    assert!(!changes(&msgs).iter().any(|c| matches!(
        c,
        RuntimeChange::CueState {
            state: WireCueState::Loaded,
            ..
        }
    )));
}

#[test]
fn a_stop_with_a_fade_while_the_move_waits_fades_the_old_point() {
    let tone = wav(&dc(16000, 0.5));
    let mut s = show(workspace(vec![audio(1, &tone, 0)]));
    start(&mut s, 1);
    s.run(800);
    seek(&mut s, 1, 1.0);
    s.transport(TransportAction::Stop {
        cue: id(1),
        fade_out: Some(FadeSpec {
            duration: 0.1,
            curve: FadeCurve::Linear,
        }),
    });
    let from = s.now() as usize;
    s.run(2000);
    let ch = s.channel(0);
    // Faded, not cut: the level falls over the 800 frames.
    assert!(
        ch[from + 400] > 0.1 && ch[from + 400] < 0.4,
        "{}",
        ch[from + 400]
    );
    assert!(ch[from + 900..].iter().all(|&v| v == 0.0));
    assert_eq!(state(&s, 1), CueState::Idle);
}

/// A click whose new point is on a volume that hangs: the old point plays on for the wait
/// (0.25 s), then the move is made anyway, silent; the stream start limit then breaks the cue
/// rather than letting it play late if the volume ever answers.
#[test]
fn a_click_whose_new_point_never_opens_breaks_the_cue_instead_of_playing_late() {
    let path = wav(&dc(10 * RATE as usize, 0.5));
    let ws = workspace(vec![audio(1, &path, 0)]);
    let mut c = config(&ws);
    c.prime_cues = 0;
    c.streamer.before_open = gated_open;
    let mut s = show_with(ws, c);
    start(&mut s, 1);
    s.run(800);
    gate(&path);
    let clicked = s.now() as usize;
    seek(&mut s, 1, 2.0);
    s.run_unsettled(u64::from(RATE));
    let ch = s.channel(0);
    // The old point played on through the wait, then the move faded it out.
    let wait = RATE as usize / 4;
    assert!(
        ch[clicked..clicked + wait - BLOCK as usize]
            .iter()
            .all(|&v| v == 0.5)
    );
    let quiet = last_nonzero(&ch).unwrap();
    assert!(
        quiet < clicked + wait + 3 * BLOCK as usize + XFADE,
        "{quiet}"
    );
    assert_eq!(state(&s, 1), CueState::Running);
    s.run_unsettled(5 * u64::from(RATE));
    assert_eq!(state(&s, 1), CueState::Broken);
    release(&path);
    s.run(BLOCK * 8);
    assert_eq!(last_nonzero(&s.channel(0)), Some(quiet));
    assert_eq!(state(&s, 1), CueState::Broken);
}

/// A Fade on cue 1 to -40 dB over `secs`, stopping it when done.
fn fade_and_stop(secs: f64) -> openlab_core::Cue {
    cue(
        2,
        CueKind::Fade {
            target: Some(id(1)),
            duration: secs,
            curve: FadeCurve::Linear,
            levels: openlab_core::FadeLevels {
                master_db: Some(-40.0),
                output_db: vec![],
                crosspoints: vec![],
            },
            stop_target_when_done: true,
            mode: Default::default(),
        },
    )
}

/// What happens around a click while the new point's stream is held back (its open gated for
/// 1200 frames, inside the 0.25 s wait): a steady tone in cue 1, `before` run first (it may
/// start cue 2 or 3), `during` sent right after the click (or with no click: the same timeline
/// without the seek). Returns channel 0 once cue 1 is idle.
fn held_click(
    others: Vec<openlab_core::Cue>,
    click: bool,
    before: impl FnOnce(&mut OfflineShow),
    during: impl FnOnce(&mut OfflineShow),
) -> Vec<f32> {
    let tone = wav(&dc(8 * RATE as usize, 0.5));
    let mut cues = vec![audio(1, &tone, 0)];
    cues.extend(others);
    let ws = workspace(cues);
    let mut c = config(&ws);
    c.prime_cues = 0;
    c.streamer.before_open = gated_open;
    let mut s = show_with(ws, c);
    start(&mut s, 1);
    s.run(800);
    before(&mut s);
    gate(&tone);
    if click {
        seek(&mut s, 1, 3.0);
    }
    during(&mut s);
    s.run_unsettled(1200);
    release(&tone);
    run_until_idle(&mut s, id(1), 8 * u64::from(RATE));
    s.run(BLOCK * 4);
    s.channel(0)
}

/// The level a stopped voice was cut from, and the largest step between two samples: a cut
/// above the level the fade planned there is a click.
fn cut_and_step(ch: &[f32]) -> (f32, f32) {
    let first = first_nonzero(ch).unwrap();
    let last = last_nonzero(ch).unwrap();
    // From the first sample on: the cue starts with no fade-in of its own.
    let step = ch[first..=last + 1]
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    (ch[last], step)
}

#[test]
fn a_fade_fired_while_a_move_waits_is_heard_at_once_and_stops_its_target_where_planned() {
    let run = |click| {
        held_click(
            vec![fade_and_stop(0.5)],
            click,
            |_| {},
            |s| {
                start(s, 2);
            },
        )
    };
    let (plain, moved) = (run(false), run(true));
    let (want, _) = cut_and_step(&plain);
    let (got, step) = cut_and_step(&moved);
    // Stopped where the fade ends, at its level (0.005), not well above it.
    assert!((got - want).abs() < 0.002, "{got} vs {want}");
    // The fade is heard from its start: 400 frames in, both are well on their way down.
    let at = first_nonzero(&plain).unwrap() + 800 + 400;
    assert!(plain[at] < 0.46, "{}", plain[at]);
    assert!(
        (moved[at] - plain[at]).abs() < 0.005,
        "{} {}",
        moved[at],
        plain[at]
    );
    // No click: no step beyond what a 10 ms crossfade of a 0.5 tone makes.
    assert!(step <= 0.5 / XFADE as f32 * 2.0, "{step}");
}

#[test]
fn a_click_during_a_running_fade_and_stop_still_stops_at_the_planned_level() {
    let run = |click| {
        held_click(
            vec![fade_and_stop(1.0)],
            click,
            |s| {
                start(s, 2);
                s.run(2000);
            },
            |_| {},
        )
    };
    let (plain, moved) = (run(false), run(true));
    let (want, _) = cut_and_step(&plain);
    let (got, step) = cut_and_step(&moved);
    assert!((got - want).abs() < 0.002, "{got} vs {want}");
    assert_eq!(last_nonzero(&moved), last_nonzero(&plain));
    assert!(step <= 0.5 / XFADE as f32 * 2.0, "{step}");
}

#[test]
fn a_duck_that_starts_while_a_move_waits_is_heard_at_once_and_after_the_move() {
    let mut ducker = audio(3, &wav(&dc(8 * RATE as usize, 0.25)), 1);
    ducker.duck_others = openlab_core::DuckOthers {
        enabled: true,
        db: -20.0,
        duration: 0.05,
    };
    let run = |click| {
        let tone = wav(&dc(8 * RATE as usize, 0.5));
        let mut first = audio(1, &tone, 0);
        first.continue_mode = ContinueMode::DoNotContinue;
        let ws = workspace(vec![first, ducker.clone()]);
        let mut c = config(&ws);
        c.prime_cues = 0;
        c.streamer.before_open = gated_open;
        let mut s = show_with(ws, c);
        start(&mut s, 1);
        s.run(800);
        gate(&tone);
        if click {
            seek(&mut s, 1, 3.0);
        }
        start(&mut s, 3);
        s.run_unsettled(1200);
        release(&tone);
        s.run(2000);
        s.channel(0)
    };
    let (plain, moved) = (run(false), run(true));
    let ducked = 0.5 * 0.1;
    // Ducked within its 50 ms while the old point still plays, and still after the move.
    let during = first_nonzero(&plain).unwrap() + 800 + 800;
    assert!((moved[during] - ducked).abs() < 0.002, "{}", moved[during]);
    let end = moved.len() - 1;
    assert!((moved[end] - ducked).abs() < 0.002, "{}", moved[end]);
    assert!((plain[end] - ducked).abs() < 0.002, "{}", plain[end]);
}

/// Cue 1 on `path` trimmed to 0.1..0.5 s (frames 800..4000), its one slice played `count`
/// times (0: until devamped).
fn trimmed(path: &std::path::Path, count: u32) -> openlab_core::Cue {
    let mut c = with_slices(audio(1, path, 0), &[], count);
    if let CueKind::Audio {
        start_time,
        end_time,
        ..
    } = &mut c.kind
    {
        *start_time = 0.1;
        *end_time = Some(0.5);
    }
    c
}

#[test]
fn click_past_the_end_trim_of_a_looping_cue_while_playing_loops_on_from_its_start() {
    let path = wav(&ramp(8000));
    let mut s = show(workspace(vec![trimmed(&path, 0)]));
    let at = start(&mut s, 1) as usize;
    s.run(1200);
    seek(&mut s, 1, 0.9);
    s.run(8000);
    assert_eq!(state(&s, 1), CueState::Running);
    let ch = s.channel(0);
    let swap = jump(&ch, at);
    // At the end trim: the end of a pass, so the next pass, from the start trim, and on.
    plays_from(&ch, swap + XFADE, swap + 3200, 800.0 + XFADE as f64);
    plays_from(&ch, swap + 3200, swap + 6400, 800.0);
}

#[test]
fn click_past_the_end_trim_of_a_paused_looping_cue_resumes_from_its_start() {
    let path = wav(&ramp(8000));
    let mut s = show(workspace(vec![trimmed(&path, 0)]));
    start(&mut s, 1);
    s.run(1200);
    s.transport(TransportAction::Pause { cue: id(1) });
    s.run(400);
    seek(&mut s, 1, 0.9);
    s.run(400);
    assert_eq!(state(&s, 1), CueState::Paused);
    let resumed = s.now() as usize;
    s.transport(TransportAction::Resume { cue: id(1) });
    s.run(4000);
    assert_eq!(state(&s, 1), CueState::Running);
    let ch = s.channel(0);
    plays_from(&ch, resumed + XFADE, resumed + 3200, 800.0 + XFADE as f64);
    plays_from(&ch, resumed + 3200, resumed + 3600, 800.0);
}

#[test]
fn click_past_the_end_trim_of_an_idle_looping_cue_then_go_loops_from_its_start() {
    let path = wav(&ramp(8000));
    let mut s = show(workspace(vec![trimmed(&path, 0)]));
    seek(&mut s, 1, 0.9);
    assert_eq!(state(&s, 1), CueState::Loaded);
    let at = start(&mut s, 1) as usize;
    s.run(4000);
    assert_eq!(state(&s, 1), CueState::Running);
    let ch = s.channel(0);
    plays_from(&ch, at, at + 3200, 800.0);
    plays_from(&ch, at + 3200, at + 3600, 800.0);
}

#[test]
fn click_past_the_end_trim_of_a_cue_played_three_times_plays_its_other_two_passes() {
    let path = wav(&ramp(8000));
    let mut s = show(workspace(vec![trimmed(&path, 3)]));
    let at = start(&mut s, 1) as usize;
    s.run(1200);
    seek(&mut s, 1, 0.9);
    s.run(8000);
    assert_eq!(state(&s, 1), CueState::Idle);
    let ch = s.channel(0);
    let swap = jump(&ch, at);
    plays_from(&ch, swap + XFADE, swap + 3200, 800.0 + XFADE as f64);
    plays_from(&ch, swap + 3200, swap + 6400, 800.0);
    assert_eq!(last_nonzero(&ch), Some(swap + 6399));
}
