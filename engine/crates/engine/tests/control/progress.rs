// SPDX-License-Identifier: GPL-3.0-or-later

//! `progress` for cues without a voice: pre-wait and post-wait countdowns in `waits`,
//! silent actions (Wait cues) in `items`, at the existing rate, per doc, and nothing when idle
//! (docs/protocol.md section 4). Offline render driver at [`RATE`].

use crate::show::*;
use openlab_core::protocol::{
    DocId, ProgressItem, ServerMessage, TransportAction, WaitProgress, WireWait,
};
use openlab_core::{ContinueMode, CueKind, WorkspaceId};
use openlab_engine::control::DocSource;
use openlab_engine::control::REPORT_HZ;
use openlab_engine::control::offline::OfflineShow;
use uuid::Uuid;

const SEC: u64 = RATE as u64;

fn timed(mut c: openlab_core::Cue, pre: f64, post: f64) -> openlab_core::Cue {
    c.pre_wait = pre;
    c.post_wait = post;
    c.continue_mode = ContinueMode::AutoContinue;
    c
}

fn wait(n: u128, seconds: f64) -> openlab_core::Cue {
    cue(n, CueKind::Wait { duration: seconds })
}

type Report = (Vec<ProgressItem>, Vec<WaitProgress>);

fn reports(msgs: &[ServerMessage]) -> Vec<Report> {
    msgs.iter()
        .filter_map(|m| match m {
            ServerMessage::Progress { items, waits } => Some((items.clone(), waits.clone())),
            _ => None,
        })
        .collect()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

/// Pre-wait 1 s, Wait 2 s, post-wait 3 s (auto-continue to a memo).
fn timed_show() -> OfflineShow {
    show(workspace(vec![
        timed(wait(1, 2.0), 1.0, 3.0),
        cue(2, CueKind::Memo),
    ]))
}

#[test]
fn pre_wait_wait_cue_and_post_wait_count_in_progress_at_the_report_rate() {
    let mut s = timed_show();
    s.take_outbound();
    let at = start(&mut s, 1);
    s.run(SEC * 5);
    let msgs = s.take_outbound();
    for m in &msgs {
        check_wire(m);
    }
    let r = reports(&msgs);
    // About 30 Hz over the 4 s the cue counts (at most 30 Hz; ticks every block).
    let limit = (5 * REPORT_HZ + 1) as usize;
    assert!(r.len() <= limit && r.len() >= 4 * 25, "{}", r.len());

    // While pre-waiting: one pre-wait countdown, no action.
    let pre: Vec<&WaitProgress> = r
        .iter()
        .filter(|(i, _)| i.is_empty())
        .filter_map(|(_, w)| w.first())
        .filter(|w| w.wait == WireWait::PreWait)
        .collect();
    assert!(pre.len() >= 20, "{}", pre.len());
    for w in &pre {
        assert_eq!(w.cue, id(1));
        assert!(close(w.elapsed + w.remaining, 1.0), "{w:?}");
    }
    assert!(pre.last().unwrap().elapsed > pre.first().unwrap().elapsed);

    // While the Wait cue's action runs: its action in `items`, its post-wait in `waits`.
    let action: Vec<&Report> = r.iter().filter(|(i, _)| !i.is_empty()).collect();
    assert!(action.len() >= 50, "{}", action.len());
    for (items, waits) in &action {
        let [item] = items.as_slice() else {
            panic!("{items:?}")
        };
        assert_eq!(item.cue, id(1));
        assert!(
            close(item.elapsed + item.remaining.unwrap(), 2.0),
            "{item:?}"
        );
        let [post] = waits.as_slice() else {
            panic!("{waits:?}")
        };
        assert_eq!(post.wait, WireWait::PostWait);
        // The post-wait counts from the action start, as the action does.
        assert!(close(post.elapsed, item.elapsed), "{post:?} {item:?}");
        assert!(close(post.elapsed + post.remaining, 3.0), "{post:?}");
    }

    // After the action, the post-wait alone; then nothing once the memo is triggered.
    let post_only = r
        .iter()
        .filter(|(i, w)| i.is_empty() && w.iter().any(|w| w.wait == WireWait::PostWait))
        .count();
    assert!(post_only >= 20, "{post_only}");
    assert!(s.rendered() > at + 4 * SEC);
    s.run(SEC);
    assert!(reports(&s.take_outbound()).is_empty());
}

#[test]
fn paused_timers_report_frozen_values() {
    let mut s = timed_show();
    start(&mut s, 1);
    s.run(SEC * 2);
    s.transport(TransportAction::PauseAll);
    s.run(SEC / 10);
    s.take_outbound();
    s.run(SEC);
    let r = reports(&s.take_outbound());
    assert!(r.len() >= 25, "{}", r.len());
    assert!(r.iter().all(|x| *x == r[0]), "{r:?}");
    let (items, waits) = &r[0];
    assert_eq!(items.len(), 1);
    assert_eq!(waits.len(), 1);
    assert!(
        items[0].elapsed > 0.9 && items[0].elapsed < 1.2,
        "{items:?}"
    );
    // Resumed, they count on from there.
    s.transport(TransportAction::ResumeAll);
    s.run(SEC / 2);
    let after = reports(&s.take_outbound());
    assert!(after.last().unwrap().0[0].elapsed > items[0].elapsed + 0.4);
}

#[test]
fn panic_and_stop_end_the_progress_messages() {
    for panic in [true, false] {
        let mut s = timed_show();
        start(&mut s, 1);
        s.run(SEC * 2);
        assert!(!reports(&s.take_outbound()).is_empty());
        if panic {
            s.transport(TransportAction::Panic);
        } else {
            s.transport(TransportAction::StopAll);
        }
        // Silent cues stop at once; the panic window lasts 0.1 s.
        s.run(SEC / 5);
        s.take_outbound();
        s.run(SEC);
        assert!(reports(&s.take_outbound()).is_empty(), "panic {panic}");
    }
}

#[test]
fn no_progress_is_sent_while_nothing_counts() {
    let mut s = timed_show();
    s.take_outbound();
    s.run(SEC * 2);
    assert!(reports(&s.take_outbound()).is_empty());
    // A loaded cue does not count either.
    s.transport(TransportAction::Load {
        cue: id(1),
        at: 0.5,
    });
    s.run(SEC);
    assert!(reports(&s.take_outbound()).is_empty());
}

#[test]
fn a_docs_timers_go_to_that_doc_only() {
    let mut s = timed_show();
    let mut other = workspace(vec![timed(wait(1, 2.0), 1.0, 0.0)]);
    other.id = WorkspaceId(Uuid::from_u128(200));
    let doc = s.open_doc(DocSource::Workspace(Box::new(other))).unwrap();
    s.settle_media();
    s.take_outbound_tagged();
    s.transport_doc(doc, TransportAction::Start { cue: id(1) });
    s.run(SEC / 2);
    let tagged = s.take_outbound_tagged();
    let docs: Vec<Option<DocId>> = tagged
        .iter()
        .filter(|o| matches!(o.message, ServerMessage::Progress { .. }))
        .map(|o| o.doc)
        .collect();
    assert!(docs.len() >= 10, "{}", docs.len());
    assert!(docs.iter().all(|d| *d == Some(doc)), "{docs:?}");
}

/// What the voice's output was playing, as a file frame, at the sample `elapsed` seconds into the
/// cue (the ramp file's sample `i` is `(i + 1) / 2^20`).
fn heard_frame(ch: &[f32], first: usize, elapsed: f64) -> Option<f64> {
    let at = first + (elapsed * f64::from(RATE)).round() as usize;
    ch.get(at).map(|v| f64::from(*v) * 1_048_576.0 - 1.0)
}

fn item_positions(msgs: &[ServerMessage], cue: u128) -> Vec<(f64, f64)> {
    reports(msgs)
        .iter()
        .flat_map(|(items, _)| items.iter())
        .filter(|i| i.cue == id(cue))
        .filter_map(|i| i.position.map(|p| (i.elapsed, p)))
        .collect()
}

fn frames_of(seconds: f64) -> f64 {
    seconds * f64::from(RATE)
}

#[test]
fn a_looping_slice_reports_file_positions_inside_the_slice_on_every_pass() {
    // 1600 frames cut at 800: the first slice plays three times, then the rest once.
    let path = wav(&ramp(1600));
    let c = with_slices(audio(1, &path, 0), &[(secs(800), 3)], 1);
    let mut s = show(workspace(vec![c]));
    s.take_outbound();
    start(&mut s, 1);
    s.run(3200);
    let msgs = s.take_outbound();
    let ch = s.channel(0);
    let first = first_nonzero(&ch).unwrap();
    let pos = item_positions(&msgs, 1);
    assert!(pos.len() >= 8, "{}", pos.len());
    let wraps = pos.windows(2).filter(|w| w[1].1 < w[0].1).count();
    // Three passes of the slice: it returned to the start twice, then moved on to the rest.
    assert_eq!(wraps, 2, "{pos:?}");
    for &(elapsed, p) in &pos {
        if frames_of(elapsed) < 2400.0 - 200.0 {
            assert!(
                (0.0..800.0).contains(&frames_of(p)),
                "{elapsed} {}",
                frames_of(p)
            );
        }
        // It is where the audio is.
        if let Some(heard) = heard_frame(&ch, first, elapsed) {
            assert!(
                (heard - frames_of(p)).abs() <= 1.0,
                "{elapsed}: {heard} vs {}",
                frames_of(p)
            );
        }
    }
    assert!(pos.iter().any(|(_, p)| frames_of(*p) >= 800.0));
    for m in &msgs {
        check_wire(m);
    }
}

fn devamp_cue(n: u128, target: u128, stop: bool) -> openlab_core::Cue {
    cue(
        n,
        CueKind::Devamp {
            target: Some(id(target)),
            start_next_cue: false,
            stop_target_when_slice_ends: stop,
            devamp_mode: openlab_core::DevampMode::CurrentSlice,
        },
    )
}

#[test]
fn after_a_devamp_the_file_position_moves_on_to_the_next_slice() {
    // Loop 0..800 until devamped, then 800..1600 once.
    let path = wav(&ramp(1600));
    let looped = with_slices(audio(1, &path, 0), &[(secs(800), 0)], 1);
    let mut s = show(workspace(vec![looped, devamp_cue(2, 1, false)]));
    s.take_outbound();
    start(&mut s, 1);
    s.run(2000);
    let looping = item_positions(&s.take_outbound(), 1);
    assert!(
        looping.iter().all(|(_, p)| frames_of(*p) < 800.0),
        "{looping:?}"
    );
    start(&mut s, 2);
    s.run(2400);
    let msgs = s.take_outbound();
    let ch = s.channel(0);
    let first = first_nonzero(&ch).unwrap();
    let after = item_positions(&msgs, 1);
    let moved = after
        .iter()
        .position(|(_, p)| frames_of(*p) >= 800.0)
        .unwrap_or_else(|| panic!("it moved on to the next slice: {after:?}"));
    // It never goes back once it has moved on, and it is where the audio is.
    for w in after[moved..].windows(2) {
        assert!(w[1].1 >= w[0].1, "{after:?}");
    }
    for &(elapsed, p) in &after {
        if let Some(heard) = heard_frame(&ch, first, elapsed) {
            assert!(
                (heard - frames_of(p)).abs() <= 1.0,
                "{elapsed}: {heard} vs {}",
                frames_of(p)
            );
        }
    }
}

#[test]
fn a_devamp_stop_keeps_the_position_inside_the_loop() {
    let path = wav(&ramp(1600));
    let looped = with_slices(audio(1, &path, 0), &[(secs(800), 0)], 1);
    let mut s = show(workspace(vec![looped, devamp_cue(2, 1, true)]));
    start(&mut s, 1);
    s.run(1000);
    start(&mut s, 2);
    s.take_outbound();
    s.run(2400);
    let pos = item_positions(&s.take_outbound(), 1);
    assert!(pos.iter().all(|(_, p)| frames_of(*p) < 800.0), "{pos:?}");
}
