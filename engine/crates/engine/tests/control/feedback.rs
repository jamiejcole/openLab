// SPDX-License-Identifier: GPL-3.0-or-later

//! Mixer feedback through the control loop: pause/resume re-keying, a resume of a voice that had
//! already ended, devamp, Pause All / Resume All, lost events, and a long show that must leave no
//! voice or stream behind (docs/control.md).

use crate::show::*;
use openlab_core::protocol::{TransportAction, WireCueState, WireProblem, WireWarning};
use openlab_core::{ContinueMode, CueKind, CueState, FadeCurve, FadeLevels, GroupMode};

#[test]
fn a_pause_on_the_end_sample_comes_after_the_known_end() {
    // 768 frames = 12 blocks: the pause is dated on the voice's end sample. The engine knows the
    // action's length (`ActionLength`), so the cue completes on that sample before the pause;
    // the resume then has nothing to do.
    let path = wav(&dc(768, 0.5));
    let mut s = show(workspace(vec![audio(1, &path, 0)]));
    let t0 = go(&mut s);
    s.run(768);
    assert_eq!(s.now(), t0 + 768);
    s.transport(TransportAction::Pause { cue: id(1) });
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    s.run(BLOCK * 6);
    s.transport(TransportAction::Resume { cue: id(1) });
    s.run(BLOCK * 4);
    assert_eq!(last_nonzero(&s.channel(0)), Some(t0 as usize + 767));
    assert!(s.control().is_idle());
    assert_eq!(s.control().live_voices(), 0);
}

#[test]
fn a_resumed_cue_is_metered_and_ends_under_its_fresh_id_only() {
    let path = wav(&dc(3000, 0.5));
    let mut c2 = audio(2, &path, 1);
    c2.pre_wait = 0.0;
    let mut c1 = audio(1, &path, 0);
    c1.continue_mode = ContinueMode::AutoFollow;
    let mut s = show(workspace(vec![c1, c2]));
    let t0 = go(&mut s) as usize;
    s.run(1000);
    let p = s.now() as usize;
    s.transport(TransportAction::Pause { cue: id(1) });
    s.run(1000);
    let r = s.now() as usize;
    s.transport(TransportAction::Resume { cue: id(1) });
    s.run(500);
    let msgs = s.take_outbound();
    // Meters named the cue throughout, also for the entry under its old id after the re-key.
    let metered = msgs.iter().any(|m| {
        matches!(m, openlab_core::protocol::ServerMessage::Meters { cues, .. }
            if cues.iter().any(|c| c.cue == id(1)))
    });
    assert!(metered);
    // Still running: no report under the paused segment's id completed it.
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    run_until_idle(&mut s, id(1), 8000);
    s.run(1000);
    let end1 = t0 + 3000 + (r - p);
    assert_eq!(last_nonzero(&s.channel(0)), Some(end1 - 1));
    // Its auto-follow fired from the fresh id's end.
    let start2 = first_nonzero(&s.channel(1)).unwrap();
    assert!(start2 >= end1 && start2 <= end1 + 2 * BLOCK as usize);
}

#[test]
fn devamp_leaves_the_loop_and_the_cue_ends() {
    // Intro 400 frames, loop 400..1200 until devamped, outro 1200..1600.
    let path = wav(&dc(1600, 0.5));
    let looped = with_slices(audio(1, &path, 0), &[(secs(400), 1), (secs(1200), 0)], 1);
    let devamp = cue(
        2,
        CueKind::Devamp {
            target: Some(id(1)),
            start_next_cue: false,
            stop_target_when_slice_ends: false,
            devamp_mode: openlab_core::DevampMode::CurrentSlice,
        },
    );
    let mut s = show(workspace(vec![looped, devamp]));
    let t0 = go(&mut s) as usize;
    s.run(4000);
    // Still looping.
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    let td = go(&mut s) as usize;
    let end = run_until_idle(&mut s, id(1), 4000) as usize;
    s.run(BLOCK * 4);
    let ch0 = s.channel(0);
    let last = last_nonzero(&ch0).unwrap();
    // It left the loop at the end of the play under way, then played the 400-frame outro.
    let pass = (td - t0 - 400) / 800;
    let loop_end = t0 + 400 + (pass + 1) * 800;
    assert_eq!(last, loop_end + 400 - 1);
    // The engine knew the end from the devamp answer: it completed the cue on it (ahead of the
    // render), not when the mixer's report came.
    assert!(end <= last + 1);
    assert_eq!(s.stalls(), 0);
    assert_eq!(s.control().live_voices(), 0);
}

#[test]
fn devamp_with_stop_ends_at_the_loop_boundary() {
    let path = wav(&dc(1600, 0.5));
    let looped = with_slices(audio(1, &path, 0), &[(secs(400), 1), (secs(1200), 0)], 1);
    let devamp = cue(
        2,
        CueKind::Devamp {
            target: Some(id(1)),
            start_next_cue: false,
            stop_target_when_slice_ends: true,
            devamp_mode: openlab_core::DevampMode::CurrentSlice,
        },
    );
    let mut s = show(workspace(vec![looped, devamp]));
    let t0 = go(&mut s) as usize;
    s.run(2000);
    let td = go(&mut s) as usize;
    run_until_idle(&mut s, id(1), 4000);
    s.run(BLOCK * 4);
    let pass = (td - t0 - 400) / 800;
    let loop_end = t0 + 400 + (pass + 1) * 800;
    assert_eq!(last_nonzero(&s.channel(0)), Some(loop_end - 1));
}

#[test]
fn pause_all_and_resume_all() {
    let a = wav(&dc(3000, 0.5));
    let mut s = show(workspace(vec![audio(1, &a, 0), audio(2, &a, 1)]));
    let t0 = start(&mut s, 1) as usize;
    start(&mut s, 2);
    s.run(1000);
    let p = s.now() as usize;
    s.transport(TransportAction::PauseAll);
    s.run(1000);
    assert_eq!(s.control().engine().state(id(1)), CueState::Paused);
    assert_eq!(s.control().engine().state(id(2)), CueState::Paused);
    let r = s.now() as usize;
    s.transport(TransportAction::ResumeAll);
    run_until_idle(&mut s, id(1), 8000);
    run_until_idle(&mut s, id(2), 8000);
    s.run(BLOCK * 4);
    for ch in 0..2 {
        let x = s.channel(ch);
        assert!(x[p + 40..r].iter().all(|&v| v == 0.0));
        assert!(x[r + 40..r + 100].iter().all(|&v| v == 0.5));
    }
    assert_eq!(last_nonzero(&s.channel(0)), Some(t0 + 3000 + (r - p) - 1));
    assert_eq!(s.control().live_voices(), 0);
}

#[test]
fn a_lost_voice_end_is_reconciled() {
    // Cue 1 loops a 10 ms (80 frame) slice for ever: one loop event per 80 frames. With the
    // control thread held off, 1100 of them overflow the mixer's event queue (1024). Cue 2 loops
    // too, so the engine cannot know its end; a Devamp cue (stop) ends it while the queue is
    // full, so both its devamp answer and its end report are lost.
    let looping = wav(&dc(800, 0.25));
    let other = wav(&dc(600, 0.5));
    let c1 = with_slices(audio(1, &looping, 0), &[(secs(80), 0)], 1);
    let c2 = with_slices(audio(2, &other, 1), &[(secs(300), 0)], 1);
    let devamp = cue(
        3,
        CueKind::Devamp {
            target: Some(id(2)),
            start_next_cue: false,
            stop_target_when_slice_ends: true,
            devamp_mode: openlab_core::DevampMode::CurrentSlice,
        },
    );
    let mut s = show(workspace(vec![c1, c2, devamp]));
    start(&mut s, 1);
    start(&mut s, 2);
    // Long enough for meter reads to see both voices.
    s.run(BLOCK * 12);
    s.render_only(80 * 1100);
    start(&mut s, 3);
    s.render_only(1000);
    assert!(s.control().mixer().dropped_events() > 0);
    // Cue 2 has ended in the mixer, but its reports are gone.
    assert_eq!(s.control().engine().state(id(2)), CueState::Running);
    let from = s.rendered() as usize;
    assert!(s.channel(1)[from - 200..].iter().all(|&v| v == 0.0));
    run_until_idle(&mut s, id(2), 4000);
    // Cue 1 still runs, its voice untouched.
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    let msgs = s.take_outbound();
    assert!(msgs.iter().any(|m| matches!(
        m,
        openlab_core::protocol::ServerMessage::Log { message, .. } if message.contains("lost")
    )));
    // It was listed by a meter read before, so it ended (not a rejected start).
    assert!(
        states(&msgs, id(2))
            .iter()
            .all(|(st, _)| *st != WireCueState::Broken)
    );
    s.transport(TransportAction::HardStop { cue: id(1) });
    s.run(BLOCK * 4);
    assert_eq!(s.control().live_voices(), 0);
}

#[test]
fn a_long_show_leaves_no_voice_or_stream_behind() {
    // 30 cues in auto-continue chains of three, some looping, a devamp, restarts, pauses and
    // stops along the way, then a panic.
    let short = wav(&dc(600, 0.1));
    let looping = wav(&dc(1600, 0.1));
    let mut cues = Vec::new();
    for n in 1..=30u128 {
        let mut c = if n % 5 == 0 {
            with_slices(audio(n, &looping, 0), &[(secs(400), 1), (secs(1200), 0)], 1)
        } else {
            audio(n, &short, (n % 2) as u32)
        };
        if n % 3 != 0 {
            c.continue_mode = ContinueMode::AutoContinue;
            c.post_wait = secs(200);
        }
        cues.push(c);
    }
    cues.push(cue(
        100,
        CueKind::Group {
            mode: GroupMode::Playlist {
                loop_: true,
                shuffle: true,
            },
            children: vec![audio(101, &short, 0), audio(102, &short, 1)],
        },
    ));
    let mut s = show(workspace(cues));
    for round in 0..12 {
        go(&mut s);
        s.run(700);
        if round % 4 == 1 {
            s.transport(TransportAction::Start { cue: id(100) });
            s.transport(TransportAction::PauseAll);
            s.run(300);
            s.transport(TransportAction::ResumeAll);
        }
        if round % 4 == 2 {
            // A second trigger and a stop of a looping cue.
            s.transport(TransportAction::Start { cue: id(5) });
            s.transport(TransportAction::Stop {
                cue: id(10),
                fade_out: None,
            });
        }
        s.run(500);
    }
    s.transport(TransportAction::Panic);
    s.run(2000);
    assert_eq!(s.control().engine().cue_states().count(), 0);
    assert!(s.control().is_idle());
    assert_eq!(s.control().live_voices(), 0);
    s.control_mut().mixer_voice_count();
    assert_eq!(s.control_mut().mixer_voice_count(), 0);
    assert_eq!(s.control().mixer().leaked_starts(), 0);
    // Only the primed streams near the playhead are held.
    assert_eq!(s.control().held_streams(), s.control().primed().len());
    assert_eq!(s.control().mixer().faults(), 0);
    assert_eq!(s.stalls(), 0);
}

#[test]
fn state_changes_are_reported_in_order() {
    let path = wav(&dc(400, 0.5));
    let mut s = show(workspace(vec![audio(1, &path, 0)]));
    s.take_outbound();
    go(&mut s);
    run_until_idle(&mut s, id(1), 2000);
    let msgs = s.take_outbound();
    let seqs: Vec<u64> = msgs
        .iter()
        .filter_map(|m| match m {
            openlab_core::protocol::ServerMessage::Runtime { seq, .. } => Some(*seq),
            _ => None,
        })
        .collect();
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
    let st: Vec<WireCueState> = states(&msgs, id(1)).into_iter().map(|s| s.0).collect();
    assert_eq!(st.last(), Some(&WireCueState::Idle));
}

#[test]
fn a_lost_rejection_breaks_the_cue() {
    // One voice in the pool: cue 2's start is refused (`PoolFull`) while the event queue is full,
    // so the refusal is lost. A voice no meter read ever listed was never taken: the cue breaks
    // (`voice_rejected`) instead of completing and firing its continue.
    let looping = wav(&dc(800, 0.25));
    let long = wav(&dc(8000, 0.5));
    let c1 = with_slices(audio(1, &looping, 0), &[(secs(80), 0)], 1);
    let ws = workspace(vec![c1, audio(2, &long, 1)]);
    let mut mixer = mixer_config();
    mixer.max_voices = 1;
    let config = config(&ws);
    let mut s = openlab_engine::control::offline::OfflineShow::new(ws, mixer, config).unwrap();
    s.settle_media();
    start(&mut s, 1);
    s.run(BLOCK * 12);
    s.render_only(80 * 1100);
    start(&mut s, 2);
    s.render_only(BLOCK * 4);
    assert!(s.control().mixer().dropped_events() > 0);
    s.run(BLOCK * 30);
    assert_eq!(s.control().engine().state(id(2)), CueState::Broken);
    let st = states(&s.take_outbound(), id(2));
    assert_eq!(
        st.last(),
        Some(&(
            WireCueState::Broken,
            Some(openlab_core::protocol::WireBrokenReason::VoiceRejected)
        ))
    );
}

#[test]
fn a_refused_first_start_is_skipped_and_fires_its_auto_follow() {
    // One voice in the pool, so cue 2's start is refused (`PoolFull`). It never played:
    // the binary reports `StartFailed`, so cue 2 is skipped and its auto-follow starts cue 3.
    let long = wav(&dc(8000, 0.5));
    let other = wav(&dc(8000, 0.5));
    let mut c2 = audio(2, &other, 1);
    c2.continue_mode = ContinueMode::AutoFollow;
    let c3 = cue(3, CueKind::Wait { duration: 1.0 });
    let ws = workspace(vec![audio(1, &long, 0), c2, c3]);
    let mut mixer = mixer_config();
    mixer.max_voices = 1;
    let config = config(&ws);
    let mut s = openlab_engine::control::offline::OfflineShow::new(ws, mixer, config).unwrap();
    s.settle_media();
    start(&mut s, 1);
    s.run(BLOCK * 4);
    start(&mut s, 2);
    s.run(BLOCK * 8);
    assert_eq!(s.control().engine().state(id(2)), CueState::Broken);
    assert_eq!(
        states(&s.take_outbound(), id(2)).last(),
        Some(&(
            WireCueState::Broken,
            Some(openlab_core::protocol::WireBrokenReason::VoiceRejected)
        ))
    );
    assert_eq!(s.control().engine().state(id(3)), CueState::Running);
}

#[test]
fn a_start_refused_while_a_pause_and_resume_were_on_their_way_is_still_skipped() {
    // As above, with Pause All and Resume All sent before the refusal comes back: the mixer
    // refused the start (under the id it was started with), so the voice never played.
    let long = wav(&dc(8000, 0.5));
    let other = wav(&dc(8000, 0.5));
    let mut c2 = audio(2, &other, 1);
    c2.continue_mode = ContinueMode::AutoFollow;
    let c3 = cue(3, CueKind::Wait { duration: 1.0 });
    let ws = workspace(vec![audio(1, &long, 0), c2, c3]);
    let mut mixer = mixer_config();
    mixer.max_voices = 1;
    let config = config(&ws);
    let mut s = openlab_engine::control::offline::OfflineShow::new(ws, mixer, config).unwrap();
    s.settle_media();
    start(&mut s, 1);
    s.run(BLOCK * 4);
    start(&mut s, 2);
    s.transport(TransportAction::PauseAll);
    s.transport(TransportAction::ResumeAll);
    s.run(BLOCK * 8);
    assert_eq!(s.control().engine().state(id(2)), CueState::Broken);
    assert_eq!(s.control().engine().state(id(3)), CueState::Running);
}

#[test]
fn a_stop_all_and_a_cues_own_panic_end_playback_through_the_control_loop() {
    // OSC `/cue/{n}/panic`, `/stop` and `/hardStop` arrive as these transport actions.
    let long = wav(&dc(40_000, 0.5));
    let mut s = show(workspace(vec![audio(1, &long, 0), audio(2, &long, 1)]));
    s.transport(TransportAction::Start { cue: id(1) });
    s.transport(TransportAction::Start { cue: id(2) });
    s.run(BLOCK * 4);
    // One cue's own panic fades that cue out and leaves the other playing.
    s.transport(TransportAction::PanicCue { cue: id(1) });
    s.run(8000);
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    assert_eq!(s.control().engine().state(id(2)), CueState::Running);
    assert_eq!(s.control().live_voices(), 1);
    // Stop All silences everything at once: no sound after the next few blocks.
    let at = s.now() as usize;
    s.transport(TransportAction::StopAll);
    s.run(BLOCK * 4);
    assert_eq!(s.control().engine().cue_states().count(), 0);
    assert!(
        s.channel(0)[at + BLOCK as usize * 2..]
            .iter()
            .all(|&v| v == 0.0)
    );
    assert_eq!(s.control().live_voices(), 0);
    // The same for the hard form.
    s.transport(TransportAction::Start { cue: id(1) });
    s.run(BLOCK * 4);
    s.transport(TransportAction::HardStopAll);
    s.run(BLOCK * 4);
    assert_eq!(s.control().engine().cue_states().count(), 0);
    assert_eq!(s.control().live_voices(), 0);
}

#[test]
fn a_second_panic_on_a_fading_cue_stops_it_within_the_next_blocks() {
    // QLab check Q7, through the control loop: the first panic fades the cue over the
    // panic time, the second hard-stops it at once; the other cue plays on.
    let long = wav(&dc(80_000, 0.5));
    let mut s = show(workspace(vec![audio(1, &long, 0), audio(2, &long, 1)]));
    s.transport(TransportAction::Start { cue: id(1) });
    s.transport(TransportAction::Start { cue: id(2) });
    s.run(BLOCK * 4);
    s.transport(TransportAction::PanicCue { cue: id(1) });
    s.run(BLOCK * 4);
    assert_eq!(s.control().engine().state(id(1)), CueState::Stopping);
    s.transport(TransportAction::PanicCue { cue: id(1) });
    let at = s.now() as usize;
    s.run(BLOCK * 6);
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    assert_eq!(s.control().engine().state(id(2)), CueState::Running);
    assert_eq!(s.control().live_voices(), 1);
    assert!(
        s.channel(0)[at + BLOCK as usize * 3..]
            .iter()
            .all(|&v| v == 0.0),
        "cue 1 is silent soon after the second press"
    );
}

#[test]
fn a_fade_with_nothing_to_fade_is_broken_in_the_snapshot_and_go_skips_it() {
    // QLab check F4: the problem reaches the clients, GO skips the Fade, and its
    // "stop target when done" does not stop the target.
    let long = wav(&dc(80_000, 0.5));
    let nothing = cue(
        2,
        CueKind::Fade {
            target: Some(id(1)),
            duration: 1.0,
            curve: FadeCurve::Linear,
            levels: FadeLevels {
                master_db: None,
                output_db: vec![None],
                crosspoints: vec![],
            },
            stop_target_when_done: true,
        },
    );
    let mut s = show(workspace(vec![audio(1, &long, 0), nothing]));
    s.run(BLOCK);
    let found = s
        .control()
        .snapshot()
        .problems
        .into_iter()
        .find(|p| p.cue == id(2))
        .map(|p| p.problems);
    assert_eq!(found, Some(vec![WireProblem::NothingToFade]));
    go(&mut s);
    s.run(BLOCK * 4);
    s.take_outbound();
    s.transport(TransportAction::Go { list: list_id() });
    s.run(BLOCK * 8);
    let sent = warnings(&s.take_outbound());
    assert!(
        sent.contains(&WireWarning::CueBroken { cue: id(2) }),
        "{sent:?}"
    );
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    assert_eq!(s.control().engine().state(id(2)), CueState::Broken);
}
