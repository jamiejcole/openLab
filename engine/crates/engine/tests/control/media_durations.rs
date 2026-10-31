// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Each audio cue's media lengths for the cue list's Duration column (CL-02): in the
//! snapshot, and as `media_duration` runtime changes when a probe answers, a trim or slice is
//! edited, the cue is pointed at another file, or its file goes missing; one cue's change costs
//! one change, never a whole-show broadcast. Files are an in-memory `FakeFs` with a fake probe;
//! nothing sleeps.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::show::*;
use openlab_audio::{MediaError, MediaInfo, Probed, Seeking};
use openlab_core::protocol::{
    ClientMessage, CueMediaDuration, Outcome, RuntimeChange, ServerMessage,
};
use openlab_core::{Command, CueKind, CueProperty, MediaRef, Workspace};
use openlab_engine::control::ControlConfig;
use openlab_engine::control::offline::OfflineShow;
use openlab_engine::control::watch::{FakeFs, WatchConfig, WatchMode};

/// One second at [`RATE`], or two for a file named `long.wav`.
fn fake_probe(path: &Path) -> Result<Probed, MediaError> {
    let frames = if path.ends_with("long.wav") {
        2 * u64::from(RATE)
    } else {
        u64::from(RATE)
    };
    Ok(Probed {
        info: MediaInfo {
            channels: 1,
            sample_rate: RATE,
            frames,
            duration: Duration::from_secs(frames / u64::from(RATE)),
        },
        warnings: vec![],
        seeking: Seeking::Direct,
    })
}

fn sound(n: usize) -> PathBuf {
    PathBuf::from(format!("/show/sounds/{n:04}.wav"))
}

/// A config whose watch runs on `fs` (passes on the test thread) and whose probe is fake.
fn manual(ws: &Workspace, fs: &FakeFs) -> ControlConfig {
    let mut c = config(ws);
    c.watch = WatchConfig {
        fs: Arc::new(fs.clone()),
        mode: WatchMode::Manual,
        ..WatchConfig::default()
    };
    c.probe = fake_probe;
    c
}

/// A show of `cues` audio cues, cue `n + 1` on [`sound`]`(n)`, all probed, first watch pass
/// done, outbound drained.
fn shows(cues: usize, fs: &FakeFs) -> OfflineShow {
    let ws = workspace(
        (0..cues)
            .map(|n| audio(n as u128 + 1, &sound(n), 0))
            .collect(),
    );
    for n in 0..cues {
        fs.put(sound(n), 1);
    }
    let c = manual(&ws, fs);
    let mut s = show_with(ws, c);
    assert!(!s.watch_pass(), "the first pass is silent");
    s.take_outbound();
    s
}

fn lengths(cue: u128, file: f64, trimmed: f64, duration: Option<f64>) -> CueMediaDuration {
    CueMediaDuration {
        cue: id(cue),
        file: Some(file),
        trimmed: Some(trimmed),
        duration,
    }
}

fn unknown(cue: u128) -> CueMediaDuration {
    CueMediaDuration {
        cue: id(cue),
        file: None,
        trimmed: None,
        duration: None,
    }
}

fn durations(msgs: &[ServerMessage]) -> Vec<CueMediaDuration> {
    changes(msgs)
        .into_iter()
        .filter_map(|c| match c {
            RuntimeChange::MediaDuration(d) => Some(d),
            _ => None,
        })
        .collect()
}

fn edit_kind(s: &mut OfflineShow, cue: u128, edit: impl Fn(&mut CueKind)) -> Vec<ServerMessage> {
    let mut kind = s
        .control()
        .workspace()
        .find_cue(id(cue))
        .unwrap()
        .kind
        .clone();
    edit(&mut kind);
    let request = s.request_id();
    let replies = s.send(ClientMessage::Command {
        id: request,
        command: Box::new(Command::SetCueProperty {
            id: id(cue),
            property: CueProperty::Kind { value: kind },
        }),
        coalesce: None,
    });
    assert!(replies.iter().any(|m| matches!(
        m,
        ServerMessage::Result {
            outcome: Outcome::Ok(_),
            ..
        }
    )));
    s.settle_media();
    assert_eq!(s.control().engine().unsignalled_model_changes(), 0);
    let mut msgs = replies;
    msgs.extend(s.take_outbound());
    msgs
}

#[test]
fn the_snapshot_carries_each_probed_cues_lengths_with_its_trims_and_loops() {
    let fs = FakeFs::new();
    let paths: Vec<PathBuf> = (0..3).map(sound).collect();
    let mut trimmed = audio(2, &paths[1], 0);
    if let CueKind::Audio {
        start_time,
        end_time,
        ..
    } = &mut trimmed.kind
    {
        *start_time = 0.25;
        *end_time = Some(0.75);
    }
    // 0.5 s played twice, then 0.5 s once; and a last slice that loops until devamped.
    let looped = with_slices(audio(3, &paths[2], 0), &[(0.5, 2)], 1);
    let forever = with_slices(audio(4, &paths[2], 0), &[], 0);
    let ws = workspace(vec![audio(1, &paths[0], 0), trimmed, looped, forever]);
    for p in &paths {
        fs.put(p, 1);
    }
    let c = manual(&ws, &fs);
    let s = show_with(ws, c);
    assert_eq!(
        s.control().snapshot().media_durations,
        vec![
            lengths(1, 1.0, 1.0, Some(1.0)),
            lengths(2, 1.0, 0.5, Some(0.5)),
            lengths(3, 1.0, 1.0, Some(1.5)),
            lengths(4, 1.0, 1.0, None),
        ]
    );
}

#[test]
fn a_probe_that_answers_later_sends_the_cues_lengths_then() {
    let path = wav(&dc(800, 0.5));
    let ws = workspace(vec![audio(1, &path, 0), audio(2, &path, 1)]);
    let config = config(&ws);
    let mut s = OfflineShow::new(ws, mixer_config(), config).unwrap();
    assert_eq!(s.control().probes_pending(), 1);
    // Not probed yet: not known, so not in the snapshot.
    assert!(s.control().snapshot().media_durations.is_empty());
    s.take_outbound();
    s.settle_media();
    let msgs = s.take_outbound();
    // Both cues on the file, in one runtime message.
    assert_eq!(
        durations(&msgs),
        vec![
            lengths(1, 0.1, 0.1, Some(0.1)),
            lengths(2, 0.1, 0.1, Some(0.1))
        ]
    );
    let runtime = msgs
        .iter()
        .filter(|m| {
            matches!(m, ServerMessage::Runtime { changes, .. }
                if changes.iter().any(|c| matches!(c, RuntimeChange::MediaDuration(_))))
        })
        .count();
    assert_eq!(runtime, 1);
    assert_eq!(
        s.control().snapshot().media_durations,
        durations(&msgs),
        "the snapshot agrees"
    );
}

#[test]
fn trims_and_play_counts_change_the_playable_length() {
    let fs = FakeFs::new();
    let mut s = shows(1, &fs);
    let msgs = edit_kind(&mut s, 1, |k| {
        if let CueKind::Audio {
            start_time,
            end_time,
            ..
        } = k
        {
            *start_time = 0.125;
            *end_time = Some(0.625);
        }
    });
    assert_eq!(durations(&msgs), vec![lengths(1, 1.0, 0.5, Some(0.5))]);
    // Played three times: the duration triples, one pass does not change.
    let msgs = edit_kind(&mut s, 1, |k| {
        if let CueKind::Audio {
            last_slice_play_count,
            ..
        } = k
        {
            *last_slice_play_count = 3;
        }
    });
    assert_eq!(durations(&msgs), vec![lengths(1, 1.0, 0.5, Some(1.5))]);
    // Looping until devamped has no duration.
    let msgs = edit_kind(&mut s, 1, |k| {
        if let CueKind::Audio {
            last_slice_play_count,
            ..
        } = k
        {
            *last_slice_play_count = 0;
        }
    });
    assert_eq!(durations(&msgs), vec![lengths(1, 1.0, 0.5, None)]);
    // An edit that does not touch the lengths (a level) sends none.
    let msgs = edit_kind(&mut s, 1, |k| {
        if let CueKind::Audio { preload, .. } = k {
            *preload = !*preload;
        }
    });
    assert!(durations(&msgs).is_empty(), "{msgs:?}");
    assert_eq!(
        s.control().snapshot().media_durations,
        vec![lengths(1, 1.0, 0.5, None)]
    );
}

#[test]
fn a_cue_pointed_at_another_file_gets_that_files_lengths() {
    // What a relink does to a cue: its media is changed in one edit.
    let fs = FakeFs::new();
    let mut s = shows(2, &fs);
    let long = PathBuf::from("/show/sounds/long.wav");
    fs.put(&long, 1);
    let msgs = edit_kind(&mut s, 1, |k| {
        if let CueKind::Audio { media, .. } = k {
            *media = Some(MediaRef::new(long.to_str().unwrap()));
        }
    });
    // Never the old file's length: either cleared first or the new one straight away; the
    // last word is the new file's.
    let told = durations(&msgs);
    assert_eq!(
        told.last(),
        Some(&lengths(1, 2.0, 2.0, Some(2.0))),
        "{told:?}"
    );
    assert!(told.iter().all(|d| d.cue == id(1) && d.file != Some(1.0)));
    assert_eq!(
        s.control().snapshot().media_durations,
        vec![
            lengths(1, 2.0, 2.0, Some(2.0)),
            lengths(2, 1.0, 1.0, Some(1.0))
        ]
    );
}

#[test]
fn a_file_that_goes_missing_clears_its_cues_lengths() {
    let fs = FakeFs::new();
    let mut s = shows(2, &fs);
    fs.remove(sound(0));
    assert!(s.watch_pass());
    assert_eq!(durations(&s.take_outbound()), vec![unknown(1)]);
    assert_eq!(
        s.control().snapshot().media_durations,
        vec![lengths(2, 1.0, 1.0, Some(1.0))]
    );
    // A cue whose file is removed from it (no media) is cleared too.
    let msgs = edit_kind(&mut s, 2, |k| {
        if let CueKind::Audio { media, .. } = k {
            *media = None;
        }
    });
    assert_eq!(durations(&msgs), vec![unknown(2)]);
    assert!(s.control().snapshot().media_durations.is_empty());
}

#[test]
fn a_removed_cues_lengths_are_withdrawn() {
    let fs = FakeFs::new();
    let mut s = shows(2, &fs);
    let request = s.request_id();
    let mut msgs = s.send(ClientMessage::Command {
        id: request,
        command: Box::new(Command::RemoveCue { id: id(1) }),
        coalesce: None,
    });
    s.run(BLOCK);
    msgs.extend(s.take_outbound());
    assert_eq!(durations(&msgs), vec![unknown(1)]);
    assert_eq!(s.control().snapshot().media_durations.len(), 1);
    // Undone, it is told again, once the re-added cue's file is probed: the probe answers from
    // a worker thread, so a block's tick may come before it.
    let request = s.request_id();
    let mut msgs = s.send(ClientMessage::Undo { id: request });
    s.settle_media();
    msgs.extend(s.take_outbound());
    assert_eq!(durations(&msgs), vec![lengths(1, 1.0, 1.0, Some(1.0))]);
}

/// The cost pin: in a show of 2,000 audio cues, opening sends no snapshot of its own, and one
/// cue's edit or file change sends one change for that cue, never a whole-show broadcast.
#[test]
fn one_cues_change_in_two_thousand_sends_one_change_and_no_snapshot() {
    let fs = FakeFs::new();
    let ws = workspace(
        (0..2000)
            .map(|n| audio(n as u128 + 1, &sound(n), 0))
            .collect(),
    );
    for n in 0..2000 {
        fs.put(sound(n), 1);
    }
    let c = manual(&ws, &fs);
    let mut s = OfflineShow::new(ws, mixer_config(), c).unwrap();
    s.settle_media();
    assert!(!s.watch_pass());
    let opening = s.take_outbound();
    let snapshots = |msgs: &[ServerMessage]| {
        msgs.iter()
            .filter(|m| matches!(m, ServerMessage::Snapshot { .. }))
            .count()
    };
    assert_eq!(snapshots(&opening), 0);
    // Every cue told once while the probes answered.
    assert_eq!(durations(&opening).len(), 2000);
    assert_eq!(s.control().snapshot().media_durations.len(), 2000);

    let msgs = edit_kind(&mut s, 7, |k| {
        if let CueKind::Audio { end_time, .. } = k {
            *end_time = Some(0.5);
        }
    });
    assert_eq!(snapshots(&msgs), 0);
    assert_eq!(durations(&msgs), vec![lengths(7, 1.0, 0.5, Some(0.5))]);

    fs.remove(sound(41));
    assert!(s.watch_pass());
    let msgs = s.take_outbound();
    assert_eq!(snapshots(&msgs), 0);
    assert_eq!(durations(&msgs), vec![unknown(42)]);
}
