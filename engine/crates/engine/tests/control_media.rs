// SPDX-License-Identifier: GPL-3.0-or-later

//! Media through the control loop: probes on workers feed the cue engine's availability inputs
//! (broken cues before they are triggered, NFR-07), a failing stream breaks its cue with a
//! reason, damaged media plays with a notice, previews play on the cue's patch, and the control
//! loop never waits for the disk.

mod show;

use openlab_core::protocol::{ClientMessage, Outcome};
use openlab_core::protocol::{
    RuntimeChange, ServerMessage, TransportAction, WireBrokenReason, WireCueState, WireProblem,
    WireWarning,
};
use openlab_core::{Command, ContinueMode, CueKind, CueProblem, CueProperty, CueState, GroupMode};
use openlab_engine::control::offline::OfflineShow;
use show::*;

#[test]
fn a_missing_file_breaks_its_cue_before_it_is_triggered() {
    let gone = fresh_path("wav");
    let ok = wav(&dc(400, 0.5));
    let mut c1 = audio(1, &gone, 0);
    c1.continue_mode = ContinueMode::AutoContinue;
    c1.post_wait = secs(300);
    let mut s = show(workspace(vec![c1, audio(2, &ok, 1)]));
    assert_eq!(s.control().engine().state(id(1)), CueState::Broken);
    assert!(matches!(
        s.control().engine().problems(id(1)),
        [CueProblem::MediaMissing { .. }]
    ));
    let msgs = s.take_outbound();
    assert!(changes(&msgs).iter().any(|c| matches!(
        c,
        RuntimeChange::Problems { cue, problems }
            if *cue == id(1) && matches!(problems[..], [WireProblem::MediaMissing { .. }])
    )));
    // GO: the broken cue plays nothing but keeps its timing, so its auto-continue fires.
    let at = go(&mut s) as usize;
    s.run(1200);
    assert!(s.channel(0).iter().all(|&v| v == 0.0));
    assert_eq!(first_nonzero(&s.channel(1)), Some(at + 300));
    assert!(
        warnings(&s.take_outbound())
            .iter()
            .any(|w| matches!(w, WireWarning::CueBroken { cue } if *cue == id(1)))
    );
}

#[test]
fn an_unplayable_file_breaks_its_cue_with_the_reason() {
    let junk = file("wav", b"RIFF\x10\0\0\0WAVEjunkjunkjunk");
    let s = show(workspace(vec![audio(1, &junk, 0)]));
    assert!(matches!(
        s.control().engine().problems(id(1)),
        [CueProblem::MediaUnsupported { .. }]
    ));
}

#[test]
fn damaged_media_plays_and_is_listed_as_a_warning() {
    // A WAV whose header promises 2000 frames but holds 1000: it plays to its real end.
    let full = std::fs::read(wav(&dc(2000, 0.5))).unwrap();
    let cut = file("wav", &full[..44 + 1000 * 4]);
    let mut s = show(workspace(vec![audio(1, &cut, 0)]));
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    let msgs = s.take_outbound();
    let listed = msgs.iter().any(|m| {
        matches!(m, ServerMessage::Warnings { warnings } if warnings.iter().any(|w| matches!(
            w, WireWarning::Notice { code, .. } if code == "damaged_media")))
    });
    assert!(listed, "{msgs:?}");
    let at = go(&mut s) as usize;
    s.run(3000);
    assert_eq!(last_nonzero(&s.channel(0)), Some(at + 999));
}

#[test]
fn a_stream_that_fails_breaks_its_cue_with_stream_failed() {
    let path = wav(&dc(2000, 0.5));
    let ws = workspace(vec![audio(1, &path, 0)]);
    let mut config = config(&ws);
    // No prime, so the stream is opened at the GO, after the file changed.
    config.prime_cues = 0;
    let mut s = show_with(ws, config);
    // The file changes after its probe: the disk worker refuses it (`Changed`).
    std::fs::write(&path, std::fs::read(wav(&dc(1500, 0.5))).unwrap()).unwrap();
    go(&mut s);
    s.run(1500);
    assert_eq!(s.control().engine().state(id(1)), CueState::Broken);
    let st = states(&s.take_outbound(), id(1));
    assert_eq!(
        st.last(),
        Some(&(WireCueState::Broken, Some(WireBrokenReason::StreamFailed)))
    );
    s.run(BLOCK * 4);
    assert_eq!(s.control().live_voices(), 0);
    // The failed stream made the loop probe the file again: it plays as it is now.
    s.settle_media();
    s.transport(TransportAction::Reset { cue: id(1) });
    s.run(BLOCK);
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
}

#[test]
fn a_preview_plays_on_the_cue_patch_outside_the_show() {
    let path = wav(&dc(800, 0.5));
    let mut s = show(workspace(vec![audio(1, &path, 0)]));
    s.take_outbound();
    let at = s.now() as usize;
    s.transport(TransportAction::Preview { cue: id(1) });
    s.run(400);
    assert_eq!(first_nonzero(&s.channel(0)), Some(at));
    // Not a show state: the cue stays idle and the playhead stays.
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    assert_eq!(s.control().engine().playhead(list_id()), Some(id(1)));
    s.transport(TransportAction::StopPreview { cue: Some(id(1)) });
    s.run(1000);
    let previews: Vec<bool> = changes(&s.take_outbound())
        .into_iter()
        .filter_map(|c| match c {
            RuntimeChange::Preview { cue, active } if cue == id(1) => Some(active),
            _ => None,
        })
        .collect();
    assert_eq!(previews, vec![true, false]);
    assert!(last_nonzero(&s.channel(0)).unwrap() < at + 800);
    assert_eq!(s.control().live_voices(), 0);
}

/// The probe of a named pipe blocks until a writer opens it: the control loop must keep running
/// (the probe is on a worker) and a GO in the meantime must not block either.
#[cfg(unix)]
#[test]
fn the_control_loop_never_waits_for_the_disk() {
    let fifo = fresh_path("wav");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(status.success());
    let ws = workspace(vec![audio(1, &fifo, 0)]);
    let config = config(&ws);
    let mut s =
        openlab_engine::control::offline::OfflineShow::new(ws, mixer_config(), config).unwrap();
    s.run(BLOCK * 10);
    assert_eq!(s.control().probes_pending(), 1);
    // Not known to be missing yet: no problem.
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    go(&mut s);
    s.run(BLOCK * 10);
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    assert!(s.channel(0).iter().all(|&v| v == 0.0));
    // A writer releases the probe with something unplayable: the waiting start fails.
    std::fs::write(&fifo, b"not audio").unwrap();
    s.settle_media();
    s.run(BLOCK * 4);
    assert_eq!(s.control().engine().state(id(1)), CueState::Broken);
    assert!(matches!(
        s.control().engine().problems(id(1)),
        [CueProblem::MediaUnsupported { .. }]
    ));
    assert_eq!(s.control().live_voices(), 0);
}

/// A start the probe held up plays as soon as the probe answers, from its head.
#[test]
fn a_start_held_by_its_probe_plays_once_the_probe_answers() {
    let path = wav(&dc(400, 0.5));
    let ws = workspace(vec![audio(1, &path, 0)]);
    let config = config(&ws);
    let mut s =
        openlab_engine::control::offline::OfflineShow::new(ws, mixer_config(), config).unwrap();
    // The probe's answer waits in the inbox: the GO comes first.
    let (reply, _replies) = openlab_engine::control::reply_channel();
    let message = openlab_core::protocol::ClientMessage::Transport {
        id: openlab_core::protocol::RequestId(1),
        action: TransportAction::Go { list: list_id() },
    };
    let at = s.now() as usize;
    s.deliver(openlab_engine::control::Inbox::Client { message, reply });
    assert_eq!(s.control().probes_pending(), 1);
    s.settle_media();
    s.run(2000);
    let first = first_nonzero(&s.channel(0)).unwrap();
    assert!(first >= at);
    assert_eq!(last_nonzero(&s.channel(0)), Some(first + 399));
    assert!(warnings(&s.take_outbound()).iter().any(|w| matches!(
        w,
        WireWarning::Notice { code, .. } if code == "cold_start"
    )));
    run_until_idle(&mut s, id(1), 2000);
}

#[test]
fn files_near_the_playhead_are_probed_first() {
    let paths: Vec<_> = (0..6).map(|_| wav(&dc(200, 0.5))).collect();
    for p in &paths {
        gate(p);
    }
    let cues = paths
        .iter()
        .enumerate()
        .map(|(i, p)| audio(i as u128 + 1, p, 0))
        .collect();
    let ws = workspace(cues);
    let mut c = config(&ws);
    c.probe = gated_probe;
    // The watchdog's clock stands still: no worker is ever counted stuck here.
    c.probe_clock = ProbeTime::new().clock();
    c.prime_cues = 1;
    let mut s = openlab_engine::control::offline::OfflineShow::new(ws, mixer_config(), c).unwrap();
    // Both workers are stuck on the first files of the workspace.
    wait_probed(&paths, 2);
    s.transport(TransportAction::SetPlayhead {
        list: list_id(),
        cue: Some(id(6)),
    });
    s.tick();
    for p in &paths {
        release(p);
    }
    s.settle_media();
    let order = probe_order(&paths);
    let pos = order.iter().position(|p| *p == paths[5]).unwrap();
    // Workspace order would have probed it last (index 5).
    assert!(pos <= 3, "{order:?}");
    assert_eq!(s.control().probes_pending(), 0);
}

#[test]
fn a_start_held_by_its_probe_fails_after_five_seconds() {
    let path = wav(&dc(200, 0.5));
    let next = wav(&dc(200, 0.5));
    gate(&path);
    // The held start never played, so it is skipped: its auto-follow still fires (M4.14).
    let mut held = audio(1, &path, 0);
    held.continue_mode = ContinueMode::AutoFollow;
    let ws = workspace(vec![held, audio(2, &next, 1)]);
    let mut c = config(&ws);
    c.probe = gated_probe;
    // The hold limit is sample time; the watchdog's clock stands still, so only the limit fails it.
    c.probe_clock = ProbeTime::new().clock();
    let mut s = openlab_engine::control::offline::OfflineShow::new(ws, mixer_config(), c).unwrap();
    let at = go(&mut s);
    // Told at once that it waits.
    assert!(warnings(&s.take_outbound()).iter().any(|w| matches!(
        w,
        WireWarning::Notice { code, .. } if code == "probe_wait"
    )));
    let limit = u64::from(RATE) * 5;
    s.run_until(at + limit - 4 * BLOCK);
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    s.run(BLOCK * 5);
    assert_eq!(s.control().engine().state(id(1)), CueState::Broken);
    let msgs = s.take_outbound();
    assert_eq!(
        states(&msgs, id(1)).last(),
        Some(&(WireCueState::Broken, Some(WireBrokenReason::StreamFailed)))
    );
    assert!(warnings(&msgs).iter().any(|w| matches!(
        w,
        WireWarning::Notice { code, .. } if code == "probe_timeout"
    )));
    s.run(BLOCK * 4);
    assert!(first_nonzero(&s.channel(1)).is_some(), "the follow played");
    s.run(400);
    assert_eq!(s.control().live_voices(), 0);
    release(&path);
}

#[test]
fn a_stuck_probe_worker_is_replaced() {
    let stuck: Vec<_> = (0..2).map(|_| wav(&dc(200, 0.5))).collect();
    let fine = wav(&dc(200, 0.5));
    for p in &stuck {
        gate(p);
    }
    let ws = workspace(vec![
        audio(1, &stuck[0], 0),
        audio(2, &stuck[1], 0),
        audio(3, &fine, 0),
    ]);
    let time = ProbeTime::new();
    let mut c = config(&ws);
    c.probe = gated_probe;
    c.probe_clock = time.clock();
    // No priming: it would queue the stuck files again ahead of the third for the fresh workers.
    c.prime_cues = 0;
    let timeout = c.probe_timeout;
    let mut s = openlab_engine::control::offline::OfflineShow::new(ws, mixer_config(), c).unwrap();
    wait_probed(&stuck, 2);
    // Both workers have been on their file since the clock's start; not stuck until the timeout.
    time.advance(timeout - std::time::Duration::from_secs(1));
    s.tick();
    assert_eq!(s.control().abandoned_probe_workers(), 0);
    // The watchdog gives up on both stuck workers; the third file is probed by a fresh one,
    // which has been on its file for no time at all, so it is not given up on.
    time.advance(std::time::Duration::from_secs(2));
    s.settle_media();
    assert_eq!(s.control().abandoned_probe_workers(), 2);
    assert_eq!(s.control().engine().state(id(3)), CueState::Idle);
    for n in 1..=2 {
        assert!(matches!(
            s.control().engine().problems(id(n)),
            [CueProblem::MediaUnsupported { .. }]
        ));
    }
    // A late answer still counts.
    for p in &stuck {
        release(p);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while s.control().engine().state(id(1)) == CueState::Broken
        || s.control().engine().state(id(2)) == CueState::Broken
    {
        assert!(std::time::Instant::now() < deadline);
        s.settle_media();
        std::thread::yield_now();
    }
}

/// A cue whose stream stalls underruns for its whole planned length, so the engine completes
/// it on its planned end; then its audio arrives and the voice sounds on as a tail its cue no
/// longer owns. The stall: one disk worker, held by another cue's stream opening a pipe nobody
/// writes yet. Returns the show with the tail sounding.
#[cfg(unix)]
#[allow(clippy::unwrap_used, clippy::panic)]
fn stalled_tail() -> openlab_engine::control::offline::OfflineShow {
    let path = wav(&dc(4000, 0.5));
    let pipe = wav(&dc(100, 0.5));
    let ws = workspace(vec![audio(1, &path, 0), audio(2, &pipe, 1)]);
    let mut c = config(&ws);
    c.prime_cues = 0;
    c.streamer.workers = 1;
    let mut s = show_with(ws, c);
    std::fs::remove_file(&pipe).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&pipe)
            .status()
            .unwrap()
            .success()
    );
    start(&mut s, 2);
    start(&mut s, 1);
    s.run_unsettled(4000 + 1000);
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    assert!(s.channel(0).iter().all(|&v| v == 0.0));
    // A writer opens the pipe and closes it: the worker goes on, and cue 1's audio comes.
    std::thread::spawn(move || drop(std::fs::OpenOptions::new().write(true).open(&pipe)))
        .join()
        .unwrap();
    s.run(BLOCK * 4);
    let from = s.channel(0).len() - BLOCK as usize;
    assert!(s.channel(0)[from..].iter().all(|&v| v == 0.5));
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    s
}

#[cfg(unix)]
#[test]
fn a_panic_silences_a_stalled_tail_within_its_fade() {
    let mut s = stalled_tail();
    let at = s.now() as usize;
    s.transport(TransportAction::Panic);
    // panic_duration 0.1 s = 800 frames.
    s.run(1200);
    let ch0 = s.channel(0);
    assert!(ch0[at..at + 400].iter().any(|&v| v > 0.0 && v < 0.5));
    assert!(ch0[at + 800..].iter().all(|&v| v == 0.0));
    assert_eq!(s.control().live_voices(), 0);
}

#[cfg(unix)]
#[test]
fn a_hard_stop_silences_a_stalled_tail_at_once() {
    let mut s = stalled_tail();
    let at = s.now() as usize;
    s.transport(TransportAction::HardStop { cue: id(1) });
    s.run(BLOCK * 4);
    assert!(s.channel(0)[at..].iter().all(|&v| v == 0.0));
    assert_eq!(s.control().live_voices(), 0);
}

#[cfg(unix)]
#[test]
fn a_reset_silences_a_stalled_tail_at_once() {
    let mut s = stalled_tail();
    let at = s.now() as usize;
    s.transport(TransportAction::Reset { cue: id(1) });
    s.run(BLOCK * 4);
    assert!(s.channel(0)[at..].iter().all(|&v| v == 0.0));
    assert_eq!(s.control().live_voices(), 0);
}

// ----- preload follows the cues' settings (M4.12b) -----

#[allow(clippy::unwrap_used)]
fn set_preload(s: &mut OfflineShow, cue: u128, on: bool) {
    let mut kind = s
        .control()
        .workspace()
        .find_cue(id(cue))
        .unwrap()
        .kind
        .clone();
    if let CueKind::Audio { preload, .. } = &mut kind {
        *preload = on;
    }
    let request = s.request_id();
    let replies = s.send(ClientMessage::Command {
        id: request,
        command: Box::new(Command::SetCueProperty {
            id: id(cue),
            property: CueProperty::Kind { value: kind },
        }),
    });
    assert!(replies.iter().any(|m| matches!(
        m,
        ServerMessage::Result {
            outcome: Outcome::Ok(_),
            ..
        }
    )));
}

#[test]
fn switching_preload_on_for_a_probed_file_preloads_it_and_off_releases_it() {
    // Regression (M4.12b): only a cue that was new, or a workspace opened, was preloaded; a
    // file probed already stayed streamed after its cue's preload was switched on.
    let path = wav(&dc(400, 0.5));
    let mut s = show(workspace(vec![audio(1, &path, 0), audio(2, &path, 1)]));
    assert_eq!(s.control().probes_pending(), 0);
    assert_eq!(
        s.control().preload_reserved(),
        0,
        "no cue asks for a preload"
    );

    // On, for one cue: the file (400 frames of f32) is preloaded.
    set_preload(&mut s, 1, true);
    s.settle_media();
    assert_eq!(s.control().preload_reserved(), 1600);
    // Another cue on the same file asks too: nothing more is reserved.
    set_preload(&mut s, 2, true);
    s.settle_media();
    assert_eq!(s.control().preload_reserved(), 1600);
    // The first stops asking: the other still wants the file.
    set_preload(&mut s, 1, false);
    assert_eq!(s.control().preload_reserved(), 1600);
    // The last one stops: the reservation is released.
    set_preload(&mut s, 2, false);
    assert_eq!(s.control().preload_reserved(), 0);
    // And switching it on again preloads again.
    set_preload(&mut s, 2, true);
    s.settle_media();
    assert_eq!(s.control().preload_reserved(), 1600);
}

#[test]
fn a_cue_playing_from_a_preload_keeps_playing_when_the_preload_is_released() {
    let path = wav(&dc(200_000, 0.5));
    let mut c = audio(1, &path, 0);
    if let CueKind::Audio { preload, .. } = &mut c.kind {
        *preload = true;
    }
    let mut s = show(workspace(vec![c]));
    assert_eq!(s.control().preload_reserved(), 800_000);
    s.settle_media();
    // GO starts from the prime, and the prime reads the preloaded samples.
    s.run(BLOCK);
    assert!(s.control().primed_from_memory(id(1)));
    let at = go(&mut s) as usize;
    assert_eq!(
        s.control().prime_hits(),
        1,
        "a primed start, not a cold one"
    );
    s.run(BLOCK * 8);
    set_preload(&mut s, 1, false);
    assert_eq!(s.control().preload_reserved(), 0);
    // The file is emptied behind the voice: one that streamed from it would run dry, only one
    // holding the preloaded samples is unaffected.
    std::fs::write(&path, b"").unwrap();
    s.run(4000);
    let ch = s.channel(0);
    assert!(
        ch[at..at + 4000].iter().all(|&v| v == 0.5),
        "the voice kept its samples"
    );
}

#[test]
fn a_cue_primed_from_disk_is_primed_again_from_memory_when_its_preload_lands() {
    // Regression: a preload answer did not dirty the primes, so a stream primed from the file
    // before the preload landed was stale (its source differs) and GO cold-started the cue.
    let path = wav(&dc(2_000_000, 0.5));
    let mut s = show(workspace(vec![audio(1, &path, 0)]));
    s.settle_media();
    assert_eq!(s.control().primed(), vec![id(1)]);
    assert!(!s.control().primed_from_memory(id(1)));
    // Switching preload on starts the job. Ticking Control directly does not read the inbox, so
    // the preload answer cannot land before this check however fast the worker is: the prime is
    // from the file.
    set_preload(&mut s, 1, true);
    s.control_mut().tick();
    assert!(!s.control().primed_from_memory(id(1)));
    s.settle_media();
    s.run(BLOCK);
    assert!(
        s.control().primed_from_memory(id(1)),
        "re-primed from the preload before GO"
    );
    let hits = s.control().prime_hits();
    go(&mut s);
    assert_eq!(s.control().prime_hits(), hits + 1, "GO used the prime");
}

#[test]
fn the_cue_engine_is_told_each_audio_cues_nominal_length_as_it_changes() {
    // From the plans the loop builds for `bad_plan` (QLab check Q2: a playlist halted by a
    // broken child times its auto-follow by them).
    let path = wav(&dc(800, 0.5));
    let gone = fresh_path("wav");
    let looping = with_slices(audio(2, &path, 0), &[(secs(400), 1)], 0);
    let mut s = show(workspace(vec![
        audio(1, &path, 0),
        looping,
        audio(3, &gone, 0),
    ]));
    s.run(BLOCK);
    let length = |s: &OfflineShow, n: u128| s.control().engine().nominal_length(id(n));
    assert_eq!(length(&s, 1), Some(800));
    // A plan that loops forever, or a file that is not there, has no end.
    assert_eq!(length(&s, 2), None);
    assert_eq!(length(&s, 3), None);
    let set_kind = |s: &mut OfflineShow, edit: &dyn Fn(&mut CueKind)| {
        let mut kind = s
            .control()
            .workspace()
            .find_cue(id(1))
            .unwrap()
            .kind
            .clone();
        edit(&mut kind);
        let request = s.request_id();
        s.send(ClientMessage::Command {
            id: request,
            command: Box::new(Command::SetCueProperty {
                id: id(1),
                property: CueProperty::Kind { value: kind },
            }),
        });
        s.run(BLOCK);
    };
    // A trim changes it.
    set_kind(&mut s, &|k| {
        if let CueKind::Audio { end_time, .. } = k {
            *end_time = Some(secs(500));
        }
    });
    assert_eq!(length(&s, 1), Some(500));
    // Looping for ever withdraws it; finite again tells it again.
    for (count, expected) in [(0, None), (1, Some(500))] {
        set_kind(&mut s, &|k| {
            if let CueKind::Audio {
                last_slice_play_count,
                ..
            } = k
            {
                *last_slice_play_count = count;
            }
        });
        assert_eq!(length(&s, 1), expected, "play count {count}");
    }
    // A removed cue's length is withdrawn.
    let remove = s.request_id();
    s.send(ClientMessage::Command {
        id: remove,
        command: Box::new(Command::RemoveCue { id: id(1) }),
    });
    s.run(BLOCK);
    assert_eq!(length(&s, 1), None);
}

#[test]
fn a_playlist_child_whose_stream_fails_before_it_plays_is_skipped() {
    // M4.14 (QLab P5/G2): c2's file changes after its probe, so its stream fails before it
    // produced any audio: c2 is skipped (StartFailed), the playlist does not halt and c3 plays.
    let one = wav(&dc(300, 0.5));
    let two = wav(&dc(300, 0.5));
    let three = wav(&dc(300, 0.25));
    let playlist = cue(
        10,
        CueKind::Group {
            mode: GroupMode::Playlist {
                loop_: false,
                shuffle: false,
            },
            children: vec![audio(1, &one, 0), audio(2, &two, 0), audio(3, &three, 1)],
        },
    );
    let ws = workspace(vec![playlist]);
    let mut config = config(&ws);
    // No prime, so c2's stream is opened when it starts, after the file changed.
    config.prime_cues = 0;
    let mut s = show_with(ws, config);
    std::fs::write(&two, std::fs::read(wav(&dc(200, 0.5))).unwrap()).unwrap();
    let at = go(&mut s) as usize;
    s.run(3000);
    assert_eq!(s.control().engine().state(id(2)), CueState::Broken);
    assert!(!s.control().engine().is_halted(id(10)));
    let first = first_nonzero(&s.channel(1)).expect("c3 played");
    assert!(first > at + 299, "c3 starts after c1: {first}");
    assert_eq!(
        states(&s.take_outbound(), id(2)).last(),
        Some(&(WireCueState::Broken, Some(WireBrokenReason::StreamFailed)))
    );
}

#[test]
fn a_playlist_child_whose_stream_fails_after_it_played_halts_the_playlist() {
    // The other side of M4.14 (QLab check Q2): c1 has played audio when its file is cut short
    // under it, so its stream fails mid-play (`Changed`): the binary reports `ActionFailed`,
    // the playlist halts and c2 never plays.
    let long = wav(&dc(40_000, 0.5));
    let next = wav(&dc(300, 0.25));
    let playlist = cue(
        10,
        CueKind::Group {
            mode: GroupMode::Playlist {
                loop_: false,
                shuffle: false,
            },
            children: vec![audio(1, &long, 0), audio(2, &next, 1)],
        },
    );
    let mut s = show(workspace(vec![playlist]));
    let at = go(&mut s) as usize;
    s.run(1000);
    assert!(
        first_nonzero(&s.channel(0)).is_some_and(|f| f >= at),
        "c1 played"
    );
    // Only the header is left: the stream's next read comes up short.
    std::fs::write(&long, &std::fs::read(&long).unwrap()[..44]).unwrap();
    let mut waited = 0;
    while s.control().engine().state(id(1)) != CueState::Broken {
        assert!(waited < 30_000, "c1's stream never failed");
        s.step();
        waited += BLOCK;
    }
    assert_eq!(
        states(&s.take_outbound(), id(1)).last(),
        Some(&(WireCueState::Broken, Some(WireBrokenReason::StreamFailed)))
    );
    assert!(s.control().engine().is_halted(id(10)));
    s.run(4000);
    assert!(s.channel(1).iter().all(|&v| v == 0.0), "c2 never plays");
}
