// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The media watch through the control loop (M2.25, QLab checks R3/R4; docs/control.md "Media
//! watch"): a file sent to the Trash breaks its cues within the tick that hears of it, a file
//! put back un-breaks them after two passes and a probe, a replaced file is probed again, an
//! unreadable one is unsupported, a playing cue keeps playing, a volume that stops answering is
//! skipped (with a notice) while the others are still watched, and an idle show costs the
//! control thread nothing. Files are an in-memory `FakeFs`; passes run on the test thread
//! (manual mode) or on the watch thread with a test clock; nothing sleeps.

mod common;
mod show;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use openlab_audio::{MediaError, MediaInfo, Probed, Seeking};
use openlab_core::protocol::{
    ClientMessage, Outcome, Request, RuntimeChange, ServerMessage, WireProblem, WireWarning,
};
use openlab_core::{CueProblem, CueState, Workspace};
use openlab_engine::control::offline::OfflineShow;
use openlab_engine::control::watch::{FakeFs, WatchConfig, WatchMode};
use openlab_engine::control::{ControlConfig, Inbox, ManualClock};
use show::*;

const WAIT: Duration = Duration::from_secs(30);

/// A config whose watch runs on `fs`, passes on the test thread.
fn manual(ws: &Workspace, fs: &FakeFs) -> ControlConfig {
    let mut c = config(ws);
    c.watch = WatchConfig {
        fs: Arc::new(fs.clone()),
        mode: WatchMode::Manual,
        ..WatchConfig::default()
    };
    c
}

/// A show of `ws` whose files are all in `fs` (device 1) and seen by a first, silent pass.
fn watched(ws: Workspace, fs: &FakeFs, files: &[&Path]) -> OfflineShow {
    for f in files {
        fs.put(f, 1);
    }
    let c = manual(&ws, fs);
    let mut s = show_with(ws, c);
    assert!(!s.watch_pass(), "the first pass is silent");
    s.take_outbound();
    s
}

fn problems(msgs: &[ServerMessage], cue: u128) -> Vec<Vec<WireProblem>> {
    changes(msgs)
        .into_iter()
        .filter_map(|c| match c {
            RuntimeChange::Problems { cue: c, problems } if c == id(cue) => Some(problems),
            _ => None,
        })
        .collect()
}

/// The workspace notices in the last `warnings` message, if one was sent.
fn notices(msgs: &[ServerMessage]) -> Option<Vec<(String, String)>> {
    msgs.iter().rev().find_map(|m| match m {
        ServerMessage::Warnings { warnings } => Some(
            warnings
                .iter()
                .filter_map(|w| match w {
                    WireWarning::Notice { code, message, .. } => {
                        Some((code.clone(), message.clone()))
                    }
                    _ => None,
                })
                .collect(),
        ),
        _ => None,
    })
}

#[test]
fn a_file_sent_to_the_trash_breaks_its_cue_within_the_tick() {
    let path = wav(&dc(400, 0.5));
    let fs = FakeFs::new();
    let mut s = watched(workspace(vec![audio(1, &path, 0)]), &fs, &[&path]);
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    let probes = s.control().probes_pending();

    fs.trash(&path);
    assert!(s.watch_pass());
    // The pass's batch was handled in one tick: the cue is broken now, no probe asked.
    assert_eq!(s.control().engine().state(id(1)), CueState::Broken);
    assert!(matches!(
        s.control().engine().problems(id(1)),
        [CueProblem::MediaMissing { .. }]
    ));
    assert_eq!(s.control().probes_pending(), probes);
    let msgs = s.take_outbound();
    assert!(
        problems(&msgs, 1)
            .iter()
            .any(|p| matches!(p[..], [WireProblem::MediaMissing { .. }])),
        "{msgs:?}"
    );
    // Nothing more is said while it stays gone.
    assert!(!s.watch_pass());
    assert!(s.take_outbound().is_empty());
    // GO plays nothing.
    let at = go(&mut s);
    s.run(800);
    assert!(s.channel(0).iter().skip(at as usize).all(|&v| v == 0.0));
    assert_eq!(s.stalls(), 0);
}

#[test]
fn a_file_put_back_unbreaks_its_cue_after_two_passes_and_a_probe() {
    let path = wav(&dc(400, 0.5));
    let fs = FakeFs::new();
    let mut s = watched(workspace(vec![audio(1, &path, 0)]), &fs, &[&path]);
    fs.trash(&path);
    assert!(s.watch_pass());
    assert_eq!(s.control().engine().state(id(1)), CueState::Broken);

    fs.put_back(&path);
    // Once is not enough (it could still be being copied).
    assert!(!s.watch_pass());
    assert_eq!(s.control().engine().state(id(1)), CueState::Broken);
    assert!(s.watch_pass());
    // Back: probed again; the cue stays broken until the probe says the file plays.
    s.settle_media();
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    assert!(s.control().engine().problems(id(1)).is_empty());
    let msgs = s.take_outbound();
    assert_eq!(problems(&msgs, 1).last(), Some(&vec![]), "{msgs:?}");
    let at = go(&mut s) as usize;
    s.run(800);
    assert_eq!(first_nonzero(&s.channel(0)), Some(at));
    assert_eq!(last_nonzero(&s.channel(0)), Some(at + 399));
    assert_eq!(s.stalls(), 0);
}

#[test]
fn a_replaced_file_is_probed_again_and_plays_its_new_length() {
    let path = wav(&dc(400, 0.5));
    let fs = FakeFs::new();
    let mut s = watched(workspace(vec![audio(1, &path, 0)]), &fs, &[&path]);
    // Another file of twice the length takes its place (written beside it, then renamed over).
    let longer = wav(&dc(800, 0.25));
    std::fs::rename(&longer, &path).unwrap();
    fs.replace(&path);
    assert!(!s.watch_pass());
    assert!(s.watch_pass());
    s.settle_media();
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    let at = go(&mut s) as usize;
    s.run(1600);
    assert_eq!(first_nonzero(&s.channel(0)), Some(at));
    assert_eq!(last_nonzero(&s.channel(0)), Some(at + 799));
    assert_eq!(s.channel(0)[at + 500], 0.25);
    assert_eq!(s.stalls(), 0);
}

static DENIED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// A probe that cannot read the files in [`DENIED`] (permissions).
fn denying_probe(path: &Path) -> Result<Probed, MediaError> {
    if DENIED.lock().unwrap().iter().any(|d| d == path) {
        return Err(MediaError::Io("permission denied".into()));
    }
    openlab_audio::probe(path)
}

#[test]
fn a_file_that_cannot_be_read_any_more_is_unsupported() {
    let path = wav(&dc(400, 0.5));
    let fs = FakeFs::new();
    fs.put(&path, 1);
    let ws = workspace(vec![audio(1, &path, 0)]);
    let mut c = manual(&ws, &fs);
    c.probe = denying_probe;
    let mut s = show_with(ws, c);
    assert!(!s.watch_pass());
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);

    fs.deny(&path);
    DENIED.lock().unwrap().push(path.clone());
    assert!(s.watch_pass());
    s.settle_media();
    assert!(matches!(
        s.control().engine().problems(id(1)),
        [CueProblem::MediaUnsupported { .. }]
    ));
    assert!(
        problems(&s.take_outbound(), 1)
            .iter()
            .any(|p| matches!(p[..], [WireProblem::MediaUnsupported { .. }]))
    );
    // Once per change: nothing more while it stays unreadable.
    assert!(!s.watch_pass());
    // Readable again: back after two passes, playable.
    fs.allow(&path);
    DENIED.lock().unwrap().retain(|d| d != &path);
    assert!(!s.watch_pass());
    assert!(s.watch_pass());
    s.settle_media();
    assert!(s.control().engine().problems(id(1)).is_empty());
}

#[test]
fn a_playing_cue_whose_file_goes_keeps_playing() {
    let path = wav(&dc(4000, 0.5));
    let fs = FakeFs::new();
    let mut s = watched(workspace(vec![audio(1, &path, 0)]), &fs, &[&path]);
    let at = go(&mut s) as usize;
    s.run(1000);
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    // The file goes to the Trash mid-play; the voice holds it open.
    fs.trash(&path);
    #[cfg(unix)]
    std::fs::remove_file(&path).unwrap();
    assert!(s.watch_pass());
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    s.run(4000);
    let ch = s.channel(0);
    assert_eq!(first_nonzero(&ch), Some(at));
    assert_eq!(last_nonzero(&ch), Some(at + 3999));
    assert!(ch[at..at + 4000].iter().all(|&v| v == 0.5), "no gap");
    // Once it ended, it shows the missing file.
    assert_eq!(s.control().engine().state(id(1)), CueState::Broken);
    assert_eq!(s.stalls(), 0);
}

#[test]
fn a_cue_removed_before_the_batch_is_left_alone() {
    let a = wav(&dc(400, 0.5));
    let fs = FakeFs::new();
    let mut s = watched(workspace(vec![audio(1, &a, 0)]), &fs, &[&a]);
    fs.trash(&a);
    let batch = s.watch_pass_batch().unwrap();
    // The cue (and so the file) goes, then comes back by undo before the batch is handled:
    // the batch belongs to the earlier watch of the file.
    let rid = s.request_id();
    s.send(ClientMessage::Command {
        id: rid,
        command: Box::new(openlab_core::Command::RemoveCue { id: id(1) }),
    });
    s.tick();
    let rid = s.request_id();
    s.send(ClientMessage::Undo { id: rid });
    fs.put_back(&a);
    s.settle_media();
    let before = s.control().watch_batches();
    s.deliver(Inbox::Watch(
        openlab_engine::control::watch::WatchAnswer::Batch(batch),
    ));
    s.tick();
    assert_eq!(s.control().watch_batches(), before + 1);
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    // The new watch's first pass is silent: the file is there.
    assert!(!s.watch_pass());
}

#[test]
fn a_batch_from_before_a_revert_is_dropped() {
    let path = wav(&dc(400, 0.5));
    let fs = FakeFs::new();
    fs.put(&path, 1);
    let ws = workspace(vec![audio(1, &path, 0)]);
    let env = common::env();
    let mut c = manual(&ws, &fs);
    c.app_dirs = Some(env.dirs.clone());
    let mut s = show_with(ws, c);
    assert!(!s.watch_pass());
    let rid = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id: rid,
        request: Request::SaveAs {
            path: env.show_path().to_string_lossy().into_owned(),
        },
    });
    assert!(replies.iter().any(|m| matches!(
        m,
        ServerMessage::Result {
            outcome: Outcome::Ok(_),
            ..
        }
    )));
    fs.remove(&path);
    let stale = s.watch_pass_batch().unwrap();
    fs.put(&path, 1);
    let rid = s.request_id();
    s.send(ClientMessage::Request {
        id: rid,
        request: Request::Revert {
            discard_changes: true,
        },
    });
    s.settle_media();
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    let before = s.control().watch_batches();
    s.deliver(Inbox::Watch(
        openlab_engine::control::watch::WatchAnswer::Batch(stale),
    ));
    s.tick();
    assert_eq!(s.control().watch_batches(), before, "an older generation");
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    assert!(!s.watch_pass());
    assert_eq!(s.stalls(), 0);
}

fn fake_probe(_: &Path) -> Result<Probed, MediaError> {
    Ok(Probed {
        info: MediaInfo {
            channels: 1,
            sample_rate: RATE,
            frames: 400,
            duration: Duration::from_millis(50),
        },
        warnings: vec![],
        seeking: Seeking::Direct,
    })
}

/// The cost pin: a pass over 2,000 unchanged files sends the control thread nothing, so it
/// reads no cue, builds no plan and broadcasts nothing.
#[test]
fn an_unchanged_pass_over_two_thousand_files_costs_control_nothing() {
    let fs = FakeFs::new();
    let paths: Vec<PathBuf> = (0..2000)
        .map(|n| PathBuf::from(format!("/show/sounds/{n:04}.wav")))
        .collect();
    let cues = paths
        .iter()
        .enumerate()
        .map(|(n, p)| audio(n as u128 + 1, p, 0))
        .collect();
    let ws = workspace(cues);
    for p in &paths {
        fs.put(p, 1);
    }
    let mut c = manual(&ws, &fs);
    c.probe = fake_probe;
    let mut s = show_with(ws, c);
    assert!(!s.watch_pass());
    s.take_outbound();
    let (read, builds, stats) = (
        s.control().media_cues_read(),
        s.control().plan_builds(),
        fs.stats(),
    );
    for _ in 0..3 {
        assert!(!s.watch_pass());
    }
    assert_eq!(fs.stats(), stats + 3 * 2000, "every file looked at");
    assert_eq!(s.control().watch_batches(), 0);
    assert_eq!(s.control().media_cues_read(), read);
    assert_eq!(s.control().plan_builds(), builds);
    assert_eq!(s.control().probes_pending(), 0);
    assert!(s.take_outbound().is_empty());
    // One file gone: one batch, one cue.
    fs.remove(&paths[7]);
    assert!(s.watch_pass());
    assert_eq!(s.control().watch_batches(), 1);
    assert_eq!(s.control().engine().state(id(8)), CueState::Broken);
    assert_eq!(s.control().engine().broken_cues().len(), 1);
}

// ----- the watch thread and its watchdog -----

struct Threaded {
    s: OfflineShow,
    fs: FakeFs,
    clock: ManualClock,
}

/// A show whose watch runs on its thread over `fs`, as fast as it can (1 ms passes), with a
/// test clock for the watchdog; cue `n + 1` plays `/Volumes/<V n>/f.wav`.
fn threaded(volumes: usize) -> (Threaded, Vec<PathBuf>) {
    let fs = FakeFs::new();
    let paths: Vec<PathBuf> = (0..volumes)
        .map(|n| PathBuf::from(format!("/Volumes/V{n}/f.wav")))
        .collect();
    for p in &paths {
        fs.put(p, 1 + p.as_os_str().len() as u64);
    }
    let ws = workspace(
        paths
            .iter()
            .enumerate()
            .map(|(n, p)| audio(n as u128 + 1, p, 0))
            .collect(),
    );
    let clock = ManualClock::new(1_000_000);
    let mut c = config(&ws);
    c.probe = fake_probe;
    c.clock = clock.clock();
    c.watch = WatchConfig {
        fs: Arc::new(fs.clone()),
        period: Duration::from_millis(1),
        mode: WatchMode::Thread,
        ..WatchConfig::default()
    };
    let s = show_with(ws, c);
    (Threaded { s, fs, clock }, paths)
}

impl Threaded {
    /// Waits until the watcher is blocked in a stat of `path`, then lets the stall limit pass
    /// on the clock and ticks.
    fn stall(&mut self, path: &Path) {
        assert!(self.fs.wait_hanging(path, WAIT), "never statted {path:?}");
        self.clock.advance(10_000);
        self.s.tick();
    }

    /// Handles watch messages until `done` (bounded).
    fn wait_until(&mut self, mut done: impl FnMut(&OfflineShow) -> bool) {
        for _ in 0..1000 {
            if done(&self.s) {
                return;
            }
            assert!(self.s.wait_watch(), "the watch thread said nothing");
        }
        panic!("never happened");
    }
}

#[test]
fn a_volume_that_stops_answering_is_skipped_with_a_notice_and_the_others_are_watched() {
    let (mut t, paths) = threaded(2);
    t.s.take_outbound();
    let hang = t.fs.hang(&paths[0]);
    t.stall(&paths[0]);
    assert_eq!(t.s.control().abandoned_watchers(), 1);
    let listed = notices(&t.s.take_outbound()).unwrap();
    assert!(
        listed
            .iter()
            .any(|(code, m)| code == "media_volume_not_responding" && m.contains("/Volumes/V0")),
        "{listed:?}"
    );
    // The fresh watcher still sees the other volume.
    t.fs.remove(&paths[1]);
    t.wait_until(|s| s.control().engine().state(id(2)) == CueState::Broken);
    // The stalled volume's file keeps its last state.
    assert_eq!(t.s.control().engine().state(id(1)), CueState::Idle);
    // It answers again: the notice goes, and its files are watched again.
    t.fs.remove(&paths[0]);
    hang.release();
    t.wait_until(|s| s.control().engine().state(id(1)) == CueState::Broken);
    let listed = notices(&t.s.take_outbound()).unwrap();
    assert!(listed.is_empty(), "{listed:?}");
    assert_eq!(t.s.control().abandoned_watchers(), 1);
    assert_eq!(t.s.stalls(), 0);
}

#[test]
fn after_four_stuck_watchers_the_watch_stops_with_a_notice_until_the_volume_answers() {
    let (mut t, paths) = threaded(5);
    let mut held = Vec::new();
    for (n, p) in paths.iter().take(4).enumerate() {
        held.push(t.fs.hang(p));
        t.stall(p);
        assert_eq!(t.s.control().abandoned_watchers(), n + 1);
    }
    t.s.take_outbound();
    let fifth = t.fs.hang(&paths[4]);
    t.stall(&paths[4]);
    // Not abandoned: no fifth thread is left behind.
    assert_eq!(t.s.control().abandoned_watchers(), 4);
    let msgs = t.s.take_outbound();
    let listed = notices(&msgs).unwrap();
    assert!(
        listed
            .iter()
            .any(|(code, m)| code == "media_watch_stopped" && m.contains("/Volumes/V4")),
        "{listed:?}"
    );
    assert_eq!(
        listed
            .iter()
            .filter(|(code, _)| code == "media_volume_not_responding")
            .count(),
        4
    );
    assert!(msgs.iter().any(|m| matches!(
        m,
        ServerMessage::Log { level: openlab_core::protocol::LogLevel::Error, message }
            if message.contains("media watch stops")
    )));
    // The volume answers: the watch goes on.
    let stats = t.fs.stats();
    fifth.release();
    assert!(t.fs.wait_stats(stats, 1, WAIT));
    t.s.tick();
    let listed = notices(&t.s.take_outbound()).unwrap();
    assert!(
        !listed.iter().any(|(code, _)| code == "media_watch_stopped"),
        "{listed:?}"
    );
    drop(held);
}
