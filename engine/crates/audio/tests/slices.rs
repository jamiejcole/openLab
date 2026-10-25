// SPDX-License-Identifier: GPL-3.0-or-later
// Helpers outside #[test] fns are test code too; clippy's in-test allowances miss them.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Slice tables on mixer voices (AUD-11, AUD-12, CT-16) without disk threads: the ring is filled
//! by hand with the plan unrolled, exactly as a disk stream would fill it. Loop and slice events
//! land on exact samples, the integrated fade-out ends on the last played frame, devamp stop
//! leaves at the end of the current play, and a devamp the voice cannot carry out is reported.
//! Every render runs with allocation forbidden.

mod common;

use common::media::marker;
use common::*;
use openlab_audio::{
    CommandKind, Curve, Event, Fade, FinishReason, MAX_SLICES, MixerController, OfflineRenderer,
    Plays, RejectReason, Slice, VoiceId, plan, total_frames,
};

const ID: VoiceId = VoiceId(1);
/// Varying block sizes, so boundaries fall at every position within a block.
const BLOCKS: &[usize] = &[64, 1, 200, 37];

fn render(r: &mut OfflineRenderer, frames: usize) -> Vec<f32> {
    let mut out = vec![0.0; frames];
    no_alloc(|| r.render_into(&mut out, BLOCKS));
    out
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

/// Starts a mono voice at `at` playing `ring` with slice table `slices`.
fn start_sliced(
    ctl: &mut MixerController,
    at: u64,
    ring: &[f32],
    slices: Vec<Slice>,
    fade_out: Fade,
) {
    let mut v = voice(ctl, ID.0, source(ring), 1);
    v.slices = slices;
    v.fade_out = fade_out;
    send_start(ctl, at, v);
}

fn looped(at: u64, slice: u32, count: u64) -> Event {
    Event::LoopReached {
        voice: ID,
        at,
        slice,
        count,
    }
}

fn slice_ended(at: u64, slice: u32) -> Event {
    Event::SliceEnded {
        voice: ID,
        at,
        slice,
    }
}

fn finished(at: u64) -> Event {
    Event::VoiceFinished {
        voice: ID,
        at,
        reason: FinishReason::OutPoint,
    }
}

#[test]
fn finite_loops_play_in_order_with_events_on_exact_samples() {
    let (mut ctl, mut r) = setup(config(1));
    let slices = plan(
        0..300,
        &[100, 200],
        &[Plays::Times(1), Plays::Times(3), Plays::Times(2)],
        0,
    )
    .unwrap();
    assert_eq!(total_frames(&slices), Some(600));
    let ring = unrolled(&slices);
    start_sliced(&mut ctl, 7, &ring, slices, Fade::NONE);
    let out = render(&mut r, 700);
    assert!(out[..7].iter().all(|&s| s == 0.0));
    assert_eq!(&out[7..607], &ring[..]);
    assert!(out[607..].iter().all(|&s| s == 0.0));
    assert_eq!(
        events(&mut ctl),
        vec![
            slice_ended(107, 0),
            looped(207, 1, 1),
            looped(307, 1, 2),
            slice_ended(407, 1),
            looped(507, 2, 1),
            slice_ended(607, 2),
            finished(607),
        ]
    );
}

#[test]
fn integrated_fade_out_ends_on_the_last_frame_of_a_looped_plan() {
    let (mut ctl, mut r) = setup(config(1));
    let slices = plan(0..300, &[100], &[Plays::Times(4), Plays::Times(1)], 0).unwrap();
    // Constant source, so the output is the envelope. The fade crosses the last slice boundary.
    let ring = vec![0.5; 600];
    start_sliced(&mut ctl, 0, &ring, slices, Fade::new(250, Curve::Linear));
    let out = render(&mut r, 650);
    // Frame L − 1 − N is the last at full level; the last played frame is exactly silent.
    assert_eq!(out[600 - 1 - 250], 0.5);
    assert!(out[..349].iter().all(|&s| s == 0.5));
    assert_eq!(out[599], 0.0);
    assert!((out[600 - 1 - 125] - 0.25).abs() < 1e-6);
    assert!(out[349..600].windows(2).all(|w| w[1] <= w[0]));
    assert!(out[600..].iter().all(|&s| s == 0.0));
}

#[test]
fn infinite_loop_has_no_fade_out_and_devamp_stop_ends_it_at_the_play_end() {
    let (mut ctl, mut r) = setup(config(1));
    let slices = plan(
        0..300,
        &[100, 200],
        &[Plays::Times(1), Plays::Infinite, Plays::Times(1)],
        0,
    )
    .unwrap();
    // Intro, then plays of the loop as a disk stream would unroll them.
    let mut ring: Vec<f32> = (0..100).map(marker).collect();
    for _ in 0..20 {
        ring.extend((100..200).map(marker));
    }
    start_sliced(&mut ctl, 0, &ring, slices, Fade::new(50, Curve::Linear));
    let mut out = render(&mut r, 750);
    ctl.send(
        750,
        CommandKind::Devamp {
            voice: ID,
            stop: true,
        },
    );
    out.extend(render(&mut r, 500));
    // Untouched by the fade-out (its end is unknown) and cut at the end of the play under way.
    assert_eq!(&out[..800], &ring[..800]);
    assert!(out[800..].iter().all(|&s| s == 0.0));
    let ev = events(&mut ctl);
    assert_eq!(
        &ev[ev.len() - 4..],
        &[
            looped(700, 1, 6),
            Event::DevampScheduled { voice: ID, at: 800 },
            slice_ended(800, 1),
            finished(800),
        ]
    );
}

#[test]
fn devamp_on_a_boundary_sample_leaves_at_the_end_of_the_play_starting_there() {
    let (mut ctl, mut r) = setup(config(1));
    let slices = plan(0..200, &[100], &[Plays::Times(1), Plays::Infinite], 0).unwrap();
    let mut ring: Vec<f32> = (0..100).map(marker).collect();
    for _ in 0..10 {
        ring.extend((100..200).map(marker));
    }
    start_sliced(&mut ctl, 0, &ring, slices, Fade::NONE);
    ctl.send(
        300,
        CommandKind::Devamp {
            voice: ID,
            stop: true,
        },
    );
    let out = render(&mut r, 600);
    assert_eq!(&out[..400], &ring[..400]);
    assert!(out[400..].iter().all(|&s| s == 0.0));
    let ev = events(&mut ctl);
    assert!(
        ev.contains(&Event::DevampScheduled { voice: ID, at: 400 }),
        "{ev:?}"
    );
    assert!(ev.contains(&finished(400)));
}

#[test]
fn devamp_continue_in_the_last_play_of_a_finite_loop_needs_no_stream() {
    let (mut ctl, mut r) = setup(config(1));
    let slices = plan(
        0..300,
        &[100, 200],
        &[Plays::Times(1), Plays::Times(2), Plays::Times(1)],
        0,
    )
    .unwrap();
    let ring = unrolled(&slices);
    start_sliced(&mut ctl, 0, &ring, slices, Fade::NONE);
    // In the second (last) play of slice 1: the ring already continues with slice 2.
    ctl.send(
        250,
        CommandKind::Devamp {
            voice: ID,
            stop: false,
        },
    );
    let out = render(&mut r, 500);
    assert_eq!(&out[..400], &ring[..]);
    let ev = events(&mut ctl);
    assert!(
        ev.contains(&Event::DevampScheduled { voice: ID, at: 300 }),
        "{ev:?}"
    );
    assert!(ev.contains(&slice_ended(300, 1)));
    assert!(ev.contains(&finished(400)));
}

#[test]
fn devamps_the_voice_cannot_carry_out_are_reported_and_change_nothing() {
    let (mut ctl, mut r) = setup(config(1));
    let slices = plan(0..200, &[100], &[Plays::Times(1), Plays::Times(5)], 0).unwrap();
    let ring = unrolled(&slices);
    start_sliced(&mut ctl, 0, &ring, slices, Fade::NONE);
    // A voice without a slice table.
    let other: Vec<f32> = (0..50).map(marker).collect();
    start(&mut ctl, 0, 2, source(&other), 1);
    // Leaving a loop with plays still to come needs a disk stream to cut them.
    ctl.send(
        150,
        CommandKind::Devamp {
            voice: ID,
            stop: false,
        },
    );
    ctl.send(
        10,
        CommandKind::Devamp {
            voice: VoiceId(2),
            stop: true,
        },
    );
    let out = render(&mut r, 700);
    let mixed: Vec<f32> = (0..600)
        .map(|i| ring[i] + other.get(i).copied().unwrap_or(0.0))
        .collect();
    assert_eq!(&out[..600], &mixed[..]);
    let ev = events(&mut ctl);
    assert!(
        ev.contains(&Event::DevampIgnored { voice: ID, at: 150 }),
        "{ev:?}"
    );
    assert!(ev.contains(&Event::DevampIgnored {
        voice: VoiceId(2),
        at: 10
    }));
    assert!(ev.contains(&finished(600)));
    // A voice that is not playing (never started, or already ended at 600).
    let devamp = |voice| CommandKind::Devamp { voice, stop: true };
    ctl.send(705, devamp(VoiceId(9)));
    ctl.send(710, devamp(ID));
    render(&mut r, 64);
    let ev = events(&mut ctl);
    assert!(
        ev.contains(&Event::DevampIgnored {
            voice: VoiceId(9),
            at: 705
        }),
        "{ev:?}"
    );
    assert!(
        ev.contains(&Event::DevampIgnored { voice: ID, at: 710 }),
        "{ev:?}"
    );
}

#[test]
fn a_second_devamp_while_one_is_armed_is_reported() {
    let (mut ctl, mut r) = setup(config(1));
    let slices = plan(0..200, &[100], &[Plays::Times(1), Plays::Infinite], 0).unwrap();
    let mut ring: Vec<f32> = (0..100).map(marker).collect();
    for _ in 0..5 {
        ring.extend((100..200).map(marker));
    }
    start_sliced(&mut ctl, 0, &ring, slices, Fade::NONE);
    ctl.send(
        120,
        CommandKind::Devamp {
            voice: ID,
            stop: true,
        },
    );
    ctl.send(
        150,
        CommandKind::Devamp {
            voice: ID,
            stop: true,
        },
    );
    let out = render(&mut r, 400);
    assert!(out[200..].iter().all(|&s| s == 0.0));
    let ev = events(&mut ctl);
    assert!(
        ev.contains(&Event::DevampIgnored { voice: ID, at: 150 }),
        "{ev:?}"
    );
    assert!(ev.contains(&finished(200)));
}

#[test]
fn bad_slice_tables_are_rejected() {
    let (mut ctl, mut r) = setup(config(1));
    let one = |frames, plays| Slice {
        index: 0,
        start: 0,
        frames,
        plays,
    };
    let cases: Vec<(Vec<Slice>, Option<u64>)> = vec![
        (vec![one(10, Plays::Times(1))], Some(10)),
        // Given a skip below: the table carries its own start.
        (vec![one(10, Plays::Times(1))], None),
        (vec![one(0, Plays::Times(1))], None),
        (vec![one(10, Plays::Times(0))], None),
        (vec![one(10, Plays::Times(1)); MAX_SLICES + 1], None),
    ];
    let n = cases.len() as u64;
    for (i, (slices, play_frames)) in cases.into_iter().enumerate() {
        let mut v = voice(&ctl, i as u64 + 1, source(&[0.5; 10]), 1);
        v.slices = slices;
        v.play_frames = play_frames;
        if i == 1 {
            v.skip_frames = 3;
        }
        send_start(&mut ctl, 0, v);
    }
    let out = render(&mut r, 64);
    assert!(out.iter().all(|&s| s == 0.0));
    let rejected: Vec<u64> = events(&mut ctl)
        .into_iter()
        .filter_map(|e| match e {
            Event::VoiceRejected {
                voice,
                reason: RejectReason::BadSlices,
                ..
            } => Some(voice.0),
            _ => None,
        })
        .collect();
    assert_eq!(rejected, (1..=n).collect::<Vec<_>>());
}
