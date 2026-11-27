// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Copying a show's media into its show folder (WS-04; QLab 5 Workspace Settings, General, File
//! Management, and its on-demand command): one test per acceptance flow of the feature, on the
//! offline driver with the real copy thread and temp folders.

mod common;
mod show;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use common::env;
use openlab_core::protocol::{
    ClientMessage, ErrorCode, MediaCopyReason, MediaCopyReport, Outcome, Request, ServerMessage,
    TransportAction,
};
use openlab_core::{Command, CueProperty, CueState, MediaRef, Position, WorkspaceSetting};
use openlab_engine::control::offline::OfflineShow;
use openlab_engine::control::{ControlConfig, DocSource};
use openlab_engine::persist::CopyPacing;
use show::*;

/// A float WAV of `frames` samples of `value` at the test rate (content differs by value and
/// length).
fn wav_bytes(frames: usize, value: f32) -> Vec<u8> {
    let data_len = frames * 4;
    let mut v = Vec::with_capacity(44 + data_len);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&3u16.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&RATE.to_le_bytes());
    v.extend_from_slice(&(RATE * 4).to_le_bytes());
    v.extend_from_slice(&4u16.to_le_bytes());
    v.extend_from_slice(&32u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&(data_len as u32).to_le_bytes());
    for _ in 0..frames {
        v.extend_from_slice(&value.to_le_bytes());
    }
    v
}

/// A gate the copy thread waits at before each file until the test opens it.
#[derive(Clone, Default)]
struct Gate(Arc<(Mutex<bool>, Condvar)>);

impl Gate {
    fn wait(&self) {
        let (open, cv) = &*self.0;
        let mut open = open.lock().unwrap();
        while !*open {
            open = cv.wait(open).unwrap();
        }
    }

    fn open(&self) {
        let (open, cv) = &*self.0;
        *open.lock().unwrap() = true;
        cv.notify_all();
    }
}

struct Rig {
    s: OfflineShow,
    env: common::Env,
    lib: PathBuf,
}

impl Rig {
    fn show_dir(&self) -> PathBuf {
        self.env.show_dir()
    }

    fn audio_dir(&self) -> PathBuf {
        self.show_dir().join("audio")
    }

    /// Writes a WAV into the library folder (outside the show folder).
    fn lib_file(&self, rel: &str, frames: usize, value: f32) -> PathBuf {
        let path = self.lib.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, wav_bytes(frames, value)).unwrap();
        path
    }

    fn command(&mut self, command: Command) -> Vec<ServerMessage> {
        let id = self.s.request_id();
        let msgs = self.s.send(ClientMessage::Command {
            id,
            command: Box::new(command),
            coalesce: None,
        });
        assert!(ok(&msgs), "{msgs:?}");
        msgs
    }

    fn set_media(&mut self, n: u128, path: &Path) {
        self.command(Command::SetCueProperty {
            id: id(n),
            property: CueProperty::Media {
                value: Some(MediaRef::new(path.to_str().unwrap())),
            },
        });
    }

    fn add_cues(&mut self, cues: &[(u128, &Path)]) {
        let commands = cues
            .iter()
            .map(|(n, path)| Command::AddCue {
                at: Position {
                    list: list_id(),
                    parent: None,
                    index: 0,
                },
                cue: cue_named_after_file(*n, path),
                auto_number: false,
            })
            .collect();
        self.command(Command::Transaction { commands });
    }

    fn setting(&mut self, on: bool) {
        self.command(Command::SetWorkspaceSetting {
            setting: WorkspaceSetting::CopyMediaOnAdd { value: on },
        });
    }

    fn copy_media(&mut self) -> Vec<ServerMessage> {
        let id = self.s.request_id();
        self.s.send(ClientMessage::Request {
            id,
            request: Request::CopyMedia,
        })
    }

    /// Waits for every copy to end and returns the reports sent meanwhile.
    fn reports(&mut self) -> Vec<MediaCopyReport> {
        assert!(
            self.s.wait_for(|c| c.copies_busy() == 0),
            "the copy never ended"
        );
        reports(&self.s.take_outbound())
    }

    fn report(&mut self) -> MediaCopyReport {
        let mut all = self.reports();
        assert_eq!(all.len(), 1, "{all:?}");
        all.remove(0)
    }

    /// The cue's file as the workspace stores it.
    fn media(&self, n: u128) -> String {
        self.s
            .control()
            .workspace()
            .find_cue(id(n))
            .and_then(|c| c.kind.media())
            .map(|m| m.as_str().to_owned())
            .unwrap_or_default()
    }

    fn audio_names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.audio_dir())
            .map(|d| {
                d.map(|e| e.unwrap().file_name().into_string().unwrap())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    fn undo(&mut self) {
        let id = self.s.request_id();
        assert!(ok(&self.s.send(ClientMessage::Undo { id })));
    }

    fn redo(&mut self) {
        let id = self.s.request_id();
        assert!(ok(&self.s.send(ClientMessage::Redo { id })));
    }
}

/// An Audio cue on `path` with no name of its own: it is named after its file.
fn cue_named_after_file(n: u128, path: &Path) -> openlab_core::Cue {
    let mut c = audio(n, path, 0);
    c.name = String::new();
    c
}

fn ok(msgs: &[ServerMessage]) -> bool {
    msgs.iter().any(|m| {
        matches!(
            m,
            ServerMessage::Result {
                outcome: Outcome::Ok(_),
                ..
            }
        )
    })
}

fn refusal(msgs: &[ServerMessage]) -> Option<ErrorCode> {
    msgs.iter().find_map(|m| match m {
        ServerMessage::Result {
            outcome: Outcome::Error(e),
            ..
        } => Some(e.code),
        _ => None,
    })
}

fn reports(msgs: &[ServerMessage]) -> Vec<MediaCopyReport> {
    msgs.iter()
        .filter_map(|m| match m {
            ServerMessage::MediaCopied { report } => Some((**report).clone()),
            _ => None,
        })
        .collect()
}

/// A saved show (`<env>/show/My Show.openlab`, written) with cues `cues` (all on files in the
/// library folder made by `files`), copying unpaced. `tweak` changes the config.
/// A path as a show stores it: forward slashes on every platform.
fn portable(path: &Path) -> String {
    openlab_core::to_portable(path.to_str().unwrap(), openlab_core::PathStyle::host())
}

/// Moves a folder. Windows refuses to rename one that holds an open file (the show is still open
/// in the engine), so there its files are copied to the new place.
fn move_dir(from: &Path, to: &Path) {
    if fs::rename(from, to).is_ok() {
        return;
    }
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            move_dir(&entry.path(), &dest);
        } else {
            fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

fn rig_with(
    files: &[(u128, &str, usize, f32)],
    saved: bool,
    tweak: impl FnOnce(&mut ControlConfig),
) -> Rig {
    rig_from(files, saved, false, tweak)
}

/// [`rig_with`], the cues naming their files relative to the show folder (`../lib/...`) when
/// `relative`.
fn rig_from(
    files: &[(u128, &str, usize, f32)],
    saved: bool,
    relative: bool,
    tweak: impl FnOnce(&mut ControlConfig),
) -> Rig {
    let env = env();
    let lib = env.root.path().join("lib");
    fs::create_dir_all(&lib).unwrap();
    let cues = files
        .iter()
        .map(|(n, rel, frames, value)| {
            let path = lib.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, wav_bytes(*frames, *value)).unwrap();
            if relative {
                audio(*n, &Path::new("../lib").join(rel), 0)
            } else {
                audio(*n, &path, 0)
            }
        })
        .collect();
    let ws = workspace(cues);
    let mut c = config(&ws);
    c.app_dirs = Some(env.dirs.clone());
    c.copy.pacing = CopyPacing {
        chunk: 4096,
        pause: Duration::ZERO,
    };
    if saved {
        c.show_path = Some(env.show_path());
    }
    tweak(&mut c);
    let mut rig = Rig {
        s: show_with(ws, c),
        env,
        lib,
    };
    if saved {
        let id = rig.s.request_id();
        assert!(ok(&rig.s.send(ClientMessage::Request {
            id,
            request: Request::Save,
        })));
    }
    rig.s.take_outbound();
    rig
}

fn rig(files: &[(u128, &str, usize, f32)]) -> Rig {
    rig_with(files, true, |_| {})
}

#[test]
fn setting_on_a_file_set_with_the_chooser_is_copied_into_audio_and_the_cue_targets_the_copy() {
    let mut r = rig(&[(1, "old.wav", 800, 0.1)]);
    let chosen = r.lib_file("Sounds/thunder.wav", 1600, 0.5);
    r.set_media(1, &chosen);
    let report = r.report();
    assert_eq!(
        (report.copied, report.reused, report.failed_count),
        (1, 0, 0)
    );
    assert_eq!(r.media(1), "audio/thunder.wav");
    let copy = r.audio_dir().join("thunder.wav");
    assert_eq!(fs::read(&copy).unwrap(), wav_bytes(1600, 0.5));
    assert_eq!(
        fs::read(&chosen).unwrap(),
        wav_bytes(1600, 0.5),
        "original untouched"
    );
    // The cue plays from its copy.
    r.s.settle_media();
    assert_ne!(r.s.control().engine().state(id(1)), CueState::Broken);
}

#[test]
fn setting_on_files_dropped_on_the_list_each_new_cue_targets_its_copy_and_is_named_after_it() {
    let mut r = rig(&[]);
    let a = r.lib_file("Drop/a.wav", 400, 0.1);
    let b = r.lib_file("Drop/b.wav", 500, 0.2);
    let c = r.lib_file("Drop/c.wav", 600, 0.3);
    r.add_cues(&[(1, &a), (2, &b), (3, &c)]);
    let report = r.report();
    assert_eq!(report.copied, 3);
    assert_eq!(
        (r.media(1), r.media(2), r.media(3)),
        (
            "audio/a.wav".into(),
            "audio/b.wav".into(),
            "audio/c.wav".into()
        )
    );
    let ws = r.s.control().workspace();
    assert_eq!(ws.find_cue(id(2)).unwrap().display_name(), "b.wav");
}

#[test]
fn setting_on_a_file_already_in_the_audio_folder_set_again_gets_a_numbered_copy() {
    let mut r = rig(&[(1, "x.wav", 100, 0.1), (2, "y.wav", 100, 0.2)]);
    let inside = r.audio_dir().join("hit.wav");
    fs::create_dir_all(r.audio_dir()).unwrap();
    fs::write(&inside, wav_bytes(300, 0.4)).unwrap();
    r.set_media(1, &inside);
    r.report();
    assert_eq!(r.media(1), "audio/hit 2.wav");
    r.set_media(2, &inside);
    r.report();
    assert_eq!(r.media(2), "audio/hit 3.wav");
    assert_eq!(r.audio_names(), ["hit 2.wav", "hit 3.wav", "hit.wav"]);
}

#[test]
fn setting_on_an_identical_file_already_in_the_folder_is_reused_and_no_copy_is_made() {
    let mut r = rig(&[(1, "x.wav", 100, 0.1), (2, "y.wav", 100, 0.2)]);
    let source = r.lib_file("Sounds/door.wav", 700, 0.3);
    r.set_media(1, &source);
    r.report();
    // The same content under another name, from another folder.
    let same = r.lib_file("Other/door slam.wav", 700, 0.3);
    r.set_media(2, &same);
    let report = r.report();
    assert_eq!((report.copied, report.reused), (0, 1));
    assert_eq!(r.media(2), "audio/door.wav");
    assert_eq!(r.audio_names(), ["door.wav"]);
}

#[test]
fn setting_on_a_different_file_with_a_taken_name_is_numbered_and_no_file_is_replaced() {
    let mut r = rig(&[
        (1, "x.wav", 100, 0.1),
        (2, "y.wav", 100, 0.2),
        (3, "z.wav", 100, 0.3),
    ]);
    let first = r.lib_file("A/bell.wav", 400, 0.1);
    let second = r.lib_file("B/bell.wav", 400, 0.2);
    let third = r.lib_file("C/bell.wav", 500, 0.3);
    r.set_media(1, &first);
    r.report();
    r.set_media(2, &second);
    r.report();
    r.set_media(3, &third);
    r.report();
    assert_eq!(
        (r.media(1), r.media(2), r.media(3)),
        (
            "audio/bell.wav".into(),
            "audio/bell 2.wav".into(),
            "audio/bell 3.wav".into()
        )
    );
    assert_eq!(
        fs::read(r.audio_dir().join("bell.wav")).unwrap(),
        wav_bytes(400, 0.1)
    );
}

/// The owner's flow (2026-11-20): sameness is by content, never by name alone.
#[test]
fn two_different_files_with_the_same_name_dropped_one_after_the_other_each_cue_plays_its_own() {
    let mut r = rig(&[]);
    let loud = r.lib_file("Act 1/cue.wav", 1600, 0.5);
    let soft = r.lib_file("Act 2/cue.wav", 1600, 0.25);
    r.add_cues(&[(1, &loud)]);
    r.report();
    r.add_cues(&[(2, &soft)]);
    r.report();
    assert_eq!(r.audio_names(), ["cue 2.wav", "cue.wav"]);
    assert_eq!(
        (r.media(1), r.media(2)),
        ("audio/cue.wav".into(), "audio/cue 2.wav".into())
    );
    r.s.settle_media();
    // Each cue sounds as its own file: the second (quieter) first, so nothing of the other
    // can be in what is measured.
    for (n, level) in [(2, 0.25f32), (1, 0.5)] {
        let from = r.s.channel(0).len();
        start(&mut r.s, n);
        run_until_idle(&mut r.s, id(n), u64::from(RATE));
        let out = r.s.channel(0);
        let peak = out[from..].iter().fold(0f32, |a, x| a.max(x.abs()));
        assert!((peak - level).abs() < 1e-3, "cue {n} played {peak}");
        // Let the run's tail out before the next is measured.
        r.s.run(BLOCK * 20);
    }
}

#[test]
fn setting_off_the_cue_targets_the_file_where_it_is_and_nothing_is_copied() {
    let mut r = rig(&[(1, "x.wav", 100, 0.1)]);
    r.setting(false);
    let chosen = r.lib_file("Sounds/rain.wav", 300, 0.2);
    r.set_media(1, &chosen);
    assert!(r.reports().is_empty());
    assert_eq!(r.media(1), portable(&chosen));
    assert!(!r.audio_dir().exists());
}

#[test]
fn copy_on_demand_copies_every_outside_file_shares_identical_ones_and_leaves_inside_ones() {
    let mut r = rig(&[
        (1, "a.wav", 400, 0.1),
        (2, "b.wav", 500, 0.2),
        (3, "a copy.wav", 400, 0.1),
    ]);
    r.setting(false);
    // Cue 4 is already inside the show folder.
    let inside = r.show_dir().join("sfx").join("in.wav");
    fs::create_dir_all(inside.parent().unwrap()).unwrap();
    fs::write(&inside, wav_bytes(100, 0.9)).unwrap();
    r.add_cues(&[(4, &inside)]);
    assert_eq!(r.media(4), "sfx/in.wav");
    assert!(ok(&r.copy_media()));
    let report = r.report();
    assert!(report.on_demand);
    assert_eq!((report.copied, report.reused, report.in_folder), (2, 1, 1));
    assert_eq!(r.media(1), "audio/a.wav");
    assert_eq!(r.media(2), "audio/b.wav");
    assert_eq!(r.media(3), "audio/a.wav", "identical files are shared");
    assert_eq!(
        r.media(4),
        "sfx/in.wav",
        "a file in the folder is left alone"
    );
    assert_eq!(r.audio_names(), ["a.wav", "b.wav"]);
    // The command also runs with the setting on: nothing is left to copy.
    r.setting(true);
    assert!(ok(&r.copy_media()));
    let again = r.report();
    assert_eq!((again.copied, again.in_folder), (0, 3));
}

#[test]
fn after_a_copy_undo_returns_the_cues_to_their_previous_targets_in_one_step_and_redo_retargets() {
    let mut r = rig(&[(1, "a.wav", 400, 0.1), (2, "b.wav", 500, 0.2)]);
    let (a, b) = (r.media(1), r.media(2));
    assert!(ok(&r.copy_media()));
    r.report();
    assert_eq!(
        (r.media(1), r.media(2)),
        ("audio/a.wav".into(), "audio/b.wav".into())
    );
    r.undo();
    assert_eq!((r.media(1), r.media(2)), (a.clone(), b.clone()));
    assert_eq!(
        r.audio_names(),
        ["a.wav", "b.wav"],
        "the copies stay on disk"
    );
    r.redo();
    assert_eq!(
        (r.media(1), r.media(2)),
        ("audio/a.wav".into(), "audio/b.wav".into())
    );
    // A copy made on add undoes the same way, and an undo starts no copy.
    let chosen = r.lib_file("c.wav", 300, 0.3);
    r.set_media(1, &chosen);
    r.report();
    assert_eq!(r.media(1), "audio/c.wav");
    r.undo();
    assert_eq!(r.media(1), portable(&chosen));
    assert!(r.reports().is_empty());
}

#[test]
fn copy_on_demand_while_a_cue_plays_keeps_playback_and_the_show_controls_and_cannot_start_twice() {
    let gate = Gate::default();
    let held = gate.clone();
    let mut r = rig_with(
        &[
            (1, "music.wav", 4 * RATE as usize, 0.5),
            (2, "b.wav", 400, 0.2),
            (3, "c.wav", 400, 0.3),
        ],
        true,
        move |c| {
            c.copy.hooks.before_file = Some(Arc::new(move |_| held.wait()));
        },
    );
    start(&mut r.s, 1);
    r.s.run(BLOCK * 8);
    let run = r.s.control().engine().run_of(id(1)).unwrap();
    assert!(ok(&r.copy_media()));
    // The copy is held: the show goes on meanwhile.
    r.s.run(BLOCK * 50);
    assert_eq!(r.s.control().engine().state(id(1)), CueState::Running);
    assert_eq!(r.s.control().engine().run_of(id(1)), Some(run));
    let progress: Vec<Vec<openlab_core::CueId>> =
        r.s.take_outbound()
            .into_iter()
            .filter_map(|m| match m {
                ServerMessage::MediaCopyProgress { cues, .. } => Some(cues),
                _ => None,
            })
            .collect();
    assert_eq!(
        progress.first(),
        Some(&vec![id(1), id(2), id(3)]),
        "the copying cues are named"
    );
    assert_eq!(refusal(&r.copy_media()), Some(ErrorCode::CommandRefused));
    // GO, Stop All and Panic answer while the copy runs.
    go(&mut r.s);
    r.s.run(BLOCK * 4);
    r.s.transport(TransportAction::StopAll);
    r.s.run(BLOCK * 4);
    assert_ne!(r.s.control().engine().state(id(1)), CueState::Running);
    r.s.panic_all();
    r.s.run(BLOCK * 4);
    gate.open();
    let report = r.report();
    assert_eq!(report.copied, 3);
}

/// Where the copied cue's run sounded, with or without a copy finishing while it plays.
fn playing_span(copy_midway: bool) -> (usize, usize, String) {
    let mut r = rig(&[
        (1, "long.wav", 2 * RATE as usize, 0.5),
        (2, "b.wav", 400, 0.2),
    ]);
    r.setting(false);
    start(&mut r.s, 1);
    r.s.run(BLOCK * 8);
    let run = r.s.control().engine().run_of(id(1)).unwrap();
    if copy_midway {
        assert!(ok(&r.copy_media()));
        let report = r.report();
        assert_eq!(report.copied, 2);
        // Both cues switched in the one step.
        assert_eq!(
            (r.media(1), r.media(2)),
            ("audio/long.wav".into(), "audio/b.wav".into())
        );
        assert_eq!(r.s.control().engine().state(id(1)), CueState::Running);
        assert_eq!(r.s.control().engine().run_of(id(1)), Some(run));
    }
    run_until_idle(&mut r.s, id(1), 4 * u64::from(RATE));
    let out = r.s.channel(0);
    let media = r.media(1);
    (common_first(&out), common_last(&out), media)
}

fn common_first(x: &[f32]) -> usize {
    first_nonzero(x).unwrap()
}

fn common_last(x: &[f32]) -> usize {
    last_nonzero(x).unwrap()
}

#[test]
fn when_the_copy_finishes_the_cues_switch_together_and_a_started_cue_plays_its_original_to_the_end()
{
    let (first, last, _) = playing_span(false);
    assert!(last - first >= 2 * RATE as usize - 2 * BLOCK as usize);
    let (f, l, media) = playing_span(true);
    assert_eq!((f, l), (first, last));
    assert_eq!(media, "audio/long.wav");
}

#[test]
fn a_file_missing_or_failing_when_copied_keeps_its_cue_and_the_rest_are_copied() {
    let mut r = rig_with(
        &[
            (1, "gone.wav", 400, 0.1),
            (2, "ok.wav", 500, 0.2),
            (3, "full.wav", 9000, 0.3),
        ],
        true,
        |c| {
            c.copy.hooks.write_fault = Some(Arc::new(|dest: &Path| {
                dest.to_string_lossy()
                    .contains("full.wav")
                    .then(|| std::io::Error::from(std::io::ErrorKind::StorageFull))
            }));
        },
    );
    let (gone, full) = (r.media(1), r.media(3));
    fs::remove_file(r.lib.join("gone.wav")).unwrap();
    assert!(ok(&r.copy_media()));
    let report = r.report();
    assert_eq!((report.copied, report.failed_count), (1, 2));
    let reasons: Vec<_> = report.failed.iter().map(|f| (f.cue, f.reason)).collect();
    assert_eq!(
        reasons,
        vec![
            (id(1), MediaCopyReason::Missing),
            (id(3), MediaCopyReason::DiskFull)
        ]
    );
    assert_eq!(report.failed[0].file, gone);
    assert_eq!(r.media(1), gone, "the failed cue keeps its target");
    assert_eq!(r.media(3), full);
    assert_eq!(r.media(2), "audio/ok.wav");
    assert_eq!(r.audio_names(), ["ok.wav"], "nothing half-written is left");
}

#[test]
fn setting_on_a_show_not_yet_saved_copies_nothing_then_saved_into_a_new_folder_every_file_is_copied()
 {
    let mut r = rig_with(&[(1, "a.wav", 400, 0.1)], false, |_| {});
    let chosen = r.lib_file("b.wav", 300, 0.2);
    r.add_cues(&[(2, &chosen)]);
    assert!(r.reports().is_empty());
    assert_eq!(r.media(2), portable(&chosen));
    // The app's folder choice: a new show folder, the show saved in it, then the copy.
    let folder = r.env.root.path().join("New Show");
    fs::create_dir_all(&folder).unwrap();
    let id = r.s.request_id();
    assert!(ok(&r.s.send(ClientMessage::Request {
        id,
        request: Request::SaveAs {
            path: folder
                .join("New Show.openlab")
                .to_string_lossy()
                .into_owned(),
        },
    })));
    assert!(ok(&r.copy_media()));
    let report = r.report();
    assert_eq!(report.copied, 2);
    assert_eq!(
        (r.media(1), r.media(2)),
        ("audio/a.wav".into(), "audio/b.wav".into())
    );
    assert!(folder.join("audio").join("b.wav").is_file());
}

#[test]
fn copy_on_demand_on_a_show_not_yet_saved_is_refused() {
    let mut r = rig_with(&[(1, "a.wav", 400, 0.1)], false, |_| {});
    assert_eq!(refusal(&r.copy_media()), Some(ErrorCode::CommandRefused));
    assert_eq!(r.s.control().copies_busy(), 0);
}

#[test]
fn show_mode_refuses_the_copy_command_and_stops_a_copy_under_way_retargeting_nothing() {
    let gate = Gate::default();
    let held = gate.clone();
    let mut r = rig_with(
        &[(1, "a.wav", 400, 0.1), (2, "b.wav", 400, 0.2)],
        true,
        move |c| {
            c.copy.hooks.before_file = Some(Arc::new(move |_| held.wait()));
        },
    );
    let before = r.media(1);
    assert!(ok(&r.copy_media()));
    let id = r.s.request_id();
    assert!(ok(&r.s.send(ClientMessage::Request {
        id,
        request: Request::SetShowMode { on: true },
    })));
    gate.open();
    let report = r.report();
    assert!(report.cancelled);
    assert_eq!(r.media(1), before);
    assert!(r.audio_names().is_empty(), "{:?}", r.audio_names());
    assert_eq!(refusal(&r.copy_media()), Some(ErrorCode::CommandRefused));
    // A file set in Show Mode (by OSC, say) is not copied either.
    let chosen = r.lib_file("c.wav", 100, 0.3);
    r.set_media(2, &chosen);
    assert!(r.reports().is_empty());
}

#[test]
fn a_show_moved_with_its_folder_finds_every_file_and_a_cue_plays() {
    let mut r = rig(&[(1, "a.wav", 800, 0.5), (2, "b.wav", 400, 0.2)]);
    assert!(ok(&r.copy_media()));
    r.report();
    let id_ = r.s.request_id();
    assert!(ok(&r.s.send(ClientMessage::Request {
        id: id_,
        request: Request::Save,
    })));
    let moved = r.env.root.path().join("Elsewhere");
    move_dir(&r.show_dir(), &moved);
    fs::remove_dir_all(&r.lib).unwrap();
    let doc =
        r.s.open_doc(DocSource::Open {
            path: moved.join("My Show.openlab"),
            recovery: None,
        })
        .unwrap();
    r.s.settle_media();
    let view = r.s.control().doc(doc).unwrap();
    for n in [1, 2] {
        let media = view
            .workspace()
            .find_cue(id(n))
            .unwrap()
            .kind
            .media()
            .unwrap()
            .clone();
        assert!(!openlab_core::is_absolute(media.as_str()));
        assert!(moved.join(media.as_str()).is_file());
        assert_ne!(view.engine().state(id(n)), CueState::Broken, "cue {n}");
    }
    r.s.transport_doc(doc, TransportAction::Start { cue: id(1) });
    r.s.run(BLOCK * 4);
    assert_eq!(
        r.s.control().doc(doc).unwrap().engine().state(id(1)),
        CueState::Running
    );
}

#[test]
fn the_setting_is_kept_with_the_show_and_is_on_by_default() {
    let mut r = rig(&[(1, "a.wav", 100, 0.1)]);
    assert!(
        r.s.control().workspace().settings.copy_media_on_add,
        "on by default"
    );
    r.setting(false);
    let id_ = r.s.request_id();
    assert!(ok(&r.s.send(ClientMessage::Request {
        id: id_,
        request: Request::Save,
    })));
    let text = fs::read_to_string(r.env.show_path()).unwrap();
    let loaded = openlab_core::load_show(&text).unwrap();
    assert!(!loaded.workspace.settings.copy_media_on_add);
}

#[test]
fn a_copy_stuck_on_a_volume_is_given_up_after_the_timeout_and_retargets_nothing() {
    let gate = Gate::default();
    let held = gate.clone();
    let reached = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let at_gate = Arc::clone(&reached);
    let time = ProbeTime::new();
    let clock = time.clock();
    let mut r = rig_with(&[(1, "a.wav", 400, 0.1)], true, move |c| {
        c.copy.hooks.before_file = Some(Arc::new(move |_| {
            at_gate.store(true, std::sync::atomic::Ordering::SeqCst);
            held.wait();
        }));
        c.stall_clock = clock;
        c.doc_timeout = Duration::from_secs(20);
    });
    let before = r.media(1);
    assert!(ok(&r.copy_media()));
    // The thread is in its first file (stuck there, as on a volume that stopped answering).
    let seen = Arc::clone(&reached);
    assert!(r.s.wait_for(move |_| seen.load(std::sync::atomic::Ordering::SeqCst)));
    r.s.tick();
    time.advance(Duration::from_secs(19));
    r.s.tick();
    assert_eq!(
        r.s.control().copies_busy(),
        1,
        "not given up before the timeout"
    );
    time.advance(Duration::from_secs(2));
    let report = r.report();
    assert!(report.cancelled);
    assert_eq!(report.failed[0].reason, MediaCopyReason::NotResponding);
    assert_eq!(r.media(1), before);
    assert_eq!(r.s.control().copies_stuck(), 1);
    // The stuck thread returns: it is counted back and its late answer changes nothing.
    gate.open();
    assert!(r.s.wait_for(|c| c.copies_stuck() == 0));
    r.s.tick();
    assert_eq!(r.media(1), before);
    assert!(r.audio_names().is_empty(), "{:?}", r.audio_names());
}

/// A Save As into another folder while files are copied: the copies went into the old folder,
/// and each cue is pointed at its copy there by a path that still names it.
#[test]
fn a_save_as_elsewhere_while_copying_points_each_cue_at_its_copy_where_it_is() {
    let gate = Gate::default();
    let held = gate.clone();
    let mut r = rig_with(&[(1, "a.wav", 400, 0.1)], true, move |c| {
        c.copy.hooks.before_file = Some(Arc::new(move |_| held.wait()));
    });
    assert!(ok(&r.copy_media()));
    let elsewhere = r.env.root.path().join("Elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let id_ = r.s.request_id();
    assert!(ok(&r.s.send(ClientMessage::Request {
        id: id_,
        request: Request::SaveAs {
            path: elsewhere
                .join("Moved.openlab")
                .to_string_lossy()
                .into_owned(),
        },
    })));
    gate.open();
    assert_eq!(r.report().copied, 1);
    let copy = r.audio_dir().join("a.wav");
    assert!(copy.is_file());
    assert_eq!(Path::new(&r.media(1)), copy);
}

/// Review #779 M1: lowering the workspace's level limit clamps every cue's levels through a
/// whole-kind edit; that keeps each cue's file, so nothing is copied, however often it is done.
#[test]
fn a_level_limit_change_that_clamps_cues_copies_nothing() {
    let mut r = rig(&[(1, "a.wav", 100, 0.1)]);
    let inside = r.audio_dir().join("hit.wav");
    fs::create_dir_all(r.audio_dir()).unwrap();
    fs::write(&inside, wav_bytes(300, 0.4)).unwrap();
    r.set_media(1, &inside);
    r.report();
    let before = (r.media(1), r.audio_names());
    for max in [-3.0, -6.0] {
        r.command(Command::SetWorkspaceSetting {
            setting: WorkspaceSetting::OutputMaxDb { value: max },
        });
        assert!(r.reports().is_empty(), "no copy at {max} dB");
    }
    assert_eq!((r.media(1), r.audio_names()), before);
}

/// Review #779 M1: an edit of a whole kind that keeps the cue's file (a level, as Paste
/// Properties sends) copies nothing; one that names another file does.
#[test]
fn a_whole_kind_edit_copies_only_when_it_names_another_file() {
    let mut r = rig(&[(1, "a.wav", 100, 0.1)]);
    // Its file is outside: a kind keeping that file is not a new file.
    let mut kind =
        r.s.control()
            .workspace()
            .find_cue(id(1))
            .unwrap()
            .kind
            .clone();
    if let openlab_core::CueKind::Audio { levels, .. } = &mut kind {
        levels.master_db = -10.0;
    }
    r.command(Command::SetCueProperty {
        id: id(1),
        property: CueProperty::Kind {
            value: kind.clone(),
        },
    });
    assert!(r.reports().is_empty());
    assert_eq!(Path::new(&r.media(1)), r.lib.join("a.wav"));
    let other = r.lib_file("b.wav", 200, 0.2);
    if let openlab_core::CueKind::Audio { media, .. } = &mut kind {
        *media = Some(MediaRef::new(other.to_str().unwrap()));
    }
    r.command(Command::SetCueProperty {
        id: id(1),
        property: CueProperty::Kind { value: kind },
    });
    assert_eq!(r.report().copied, 1);
    assert_eq!(r.media(1), "audio/b.wav");
}

/// Review #779 M2: a copy's cues are pointed at their copies a part per wake, and it is still one
/// undo step.
#[test]
fn cues_pointed_at_their_copies_over_several_wakes_are_one_undo_step() {
    let mut r = rig_with(
        &[
            (1, "a.wav", 400, 0.1),
            (2, "b.wav", 500, 0.2),
            (3, "c.wav", 600, 0.3),
        ],
        true,
        |c| c.copy.apply_per_wake = 1,
    );
    let before = (r.media(1), r.media(2), r.media(3));
    assert!(ok(&r.copy_media()));
    assert_eq!(r.report().copied, 3);
    assert_eq!(
        (r.media(1), r.media(2), r.media(3)),
        (
            "audio/a.wav".into(),
            "audio/b.wav".into(),
            "audio/c.wav".into()
        )
    );
    r.undo();
    assert_eq!((r.media(1), r.media(2), r.media(3)), before, "one step");
    r.redo();
    assert_eq!(r.media(3), "audio/c.wav");
}

/// A copy of three cues at one cue per wake, stopped after its first wake of retargeting.
fn halfway() -> (Rig, (String, String, String)) {
    let mut r = rig_with(
        &[
            (1, "a.wav", 400, 0.1),
            (2, "b.wav", 500, 0.2),
            (3, "c.wav", 600, 0.3),
        ],
        true,
        |c| c.copy.apply_per_wake = 1,
    );
    let before = (r.media(1), r.media(2), r.media(3));
    assert!(ok(&r.copy_media()));
    let first = before.0.clone();
    assert!(r.s.wait_for(|c| {
        c.workspace()
            .find_cue(id(1))
            .and_then(|q| q.kind.media())
            .is_some_and(|m| m.as_str() != first)
    }));
    assert_eq!(
        (r.media(1), r.media(2), r.media(3)),
        ("audio/a.wav".into(), before.1.clone(), before.2.clone()),
        "one cue retargeted, two waiting"
    );
    (r, before)
}

/// Review #779 M1: an Undo reaching a copy that still has cues to retarget finishes the copy
/// first, so the Undo reverts all of it and the show is not left half retargeted.
#[test]
fn an_undo_while_a_copy_is_retargeting_undoes_the_whole_copy_in_one_step() {
    let (mut r, before) = halfway();
    r.undo();
    assert_eq!((r.media(1), r.media(2), r.media(3)), before);
    r.report();
    assert_eq!(
        (r.media(1), r.media(2), r.media(3)),
        before,
        "nothing retargets after the undo"
    );
}

/// Review #779 M1: an edit made mid-copy lands after every cue is on its copy, and one Undo of
/// the copy restores all of them.
#[test]
fn an_edit_while_a_copy_is_retargeting_leaves_every_cue_on_its_copy_and_one_undo_restores_them() {
    let (mut r, before) = halfway();
    r.command(Command::SetCueProperty {
        id: id(1),
        property: CueProperty::Name {
            value: "renamed".into(),
        },
    });
    assert_eq!(
        (r.media(1), r.media(2), r.media(3)),
        (
            "audio/a.wav".into(),
            "audio/b.wav".into(),
            "audio/c.wav".into()
        )
    );
    r.report();
    r.undo();
    r.undo();
    assert_eq!((r.media(1), r.media(2), r.media(3)), before);
}

/// Review #779 M2: Show Mode turning on mid-copy finishes retargeting at once, not over later
/// wakes in Show Mode.
#[test]
fn show_mode_while_a_copy_is_retargeting_retargets_the_rest_at_once() {
    let (mut r, _) = halfway();
    let sid = r.s.request_id();
    assert!(ok(&r.s.send(ClientMessage::Request {
        id: sid,
        request: Request::SetShowMode { on: true },
    })));
    assert_eq!(
        (r.media(1), r.media(2), r.media(3)),
        (
            "audio/a.wav".into(),
            "audio/b.wav".into(),
            "audio/c.wav".into()
        ),
        "everything retargeted before Show Mode began"
    );
    assert_eq!(r.report().copied, 3);
}

/// `count` cues `f{n}.wav` whose files differ by length, copied with `tweak`'s pacing.
fn many(count: u128, tweak: impl FnOnce(&mut ControlConfig)) -> Rig {
    many_from(count, false, tweak)
}

fn many_from(count: u128, relative: bool, tweak: impl FnOnce(&mut ControlConfig)) -> Rig {
    let names: Vec<String> = (1..=count).map(|n| format!("f{n}.wav")).collect();
    let files: Vec<(u128, &str, usize, f32)> = (1..=count)
        .zip(&names)
        .map(|(n, name)| (n, name.as_str(), 100 + n as usize, 0.1))
        .collect();
    rig_from(&files, true, relative, tweak)
}

fn original_bytes(n: u128) -> Vec<u8> {
    wav_bytes(100 + n as usize, 0.1)
}

/// Waits until cue 1 is on its copy: the copy has answered and its parts are under way.
fn wait_until_retargeting(r: &mut Rig) {
    let first = r.media(1);
    assert!(r.s.wait_for(|c| {
        c.workspace()
            .find_cue(id(1))
            .and_then(|q| q.kind.media())
            .is_some_and(|m| m.as_str() != first)
    }));
}

/// Review #652 H1: a Save As into another folder answered while a finished copy still has parts
/// to retarget finishes them first; parts left queued would carry forms worked out against the
/// old folder and, resolved against the new one, play another show's file (and the late parts
/// would be a separate undo step).
#[test]
fn a_save_as_elsewhere_while_a_copy_is_retargeting_leaves_every_cue_on_its_own_copy() {
    let mut r = many(60, |c| c.copy.apply_per_wake = 1);
    let elsewhere = r.env.root.path().join("Elsewhere");
    fs::create_dir_all(elsewhere.join("audio")).unwrap();
    for n in 1..=60 {
        fs::write(
            elsewhere.join("audio").join(format!("f{n}.wav")),
            wav_bytes(7, 0.9),
        )
        .unwrap();
    }
    assert!(ok(&r.copy_media()));
    wait_until_retargeting(&mut r);
    assert_ne!(r.media(60), "audio/f60.wav", "parts still waiting");
    let sid = r.s.request_id();
    assert!(ok(&r.s.send(ClientMessage::Request {
        id: sid,
        request: Request::SaveAs {
            path: elsewhere
                .join("Moved.openlab")
                .to_string_lossy()
                .into_owned(),
        },
    })));
    let all = r.reports();
    assert_eq!(all.iter().map(|x| x.copied).sum::<u64>(), 60);
    let dir = portable(&elsewhere);
    for n in 1..=60 {
        let resolved = openlab_core::resolve(&r.media(n), &dir);
        assert_eq!(
            fs::read(&resolved).unwrap(),
            original_bytes(n),
            "cue {n} resolves to {resolved}"
        );
    }
    // A move clears history, and the copy was whole before it did: an undo afterwards has no
    // half-copy to revert and leaves every cue on its copy.
    r.undo();
    for n in 1..=60 {
        let resolved = openlab_core::resolve(&r.media(n), &dir);
        assert_eq!(fs::read(&resolved).unwrap(), original_bytes(n), "cue {n}");
    }
}

/// Review #652 H1 (2nd): a Save As written on the writer thread while the copy answers moves the
/// show folder only when it lands. The parts still waiting are applied before the move, so every
/// cue ends on its own copy (not the other show's same-named file, nor left on its original
/// because the move re-spelled a relative reference), and the copy stays one undo step.
#[test]
fn a_save_as_landing_while_a_copy_retargets_leaves_every_cue_on_its_own_copy() {
    use openlab_engine::control::{Inbox, SaveHook, reply_channel};
    use openlab_engine::persist::JobKind;
    use std::sync::atomic::{AtomicBool, Ordering};
    let copy_gate = Gate::default();
    let save_gate = Gate::default();
    let armed = Arc::new(AtomicBool::new(false));
    // The save is held until the copy has started retargeting, which on a slow runner takes
    // longer than the save watchdog's timeout: on the machine's clock the watchdog would answer
    // the held Save As `io` and the doc would stay where it was. A clock that never moves keeps
    // the watchdog out of a test about the landing.
    let time = ProbeTime::new();
    let mut r = many_from(60, true, |c| {
        c.stall_clock = time.clock();
        c.copy.apply_per_wake = 1;
        let held = copy_gate.clone();
        c.copy.hooks.before_file = Some(Arc::new(move |_| held.wait()));
        let (held, armed) = (save_gate.clone(), armed.clone());
        c.save_hook = Some(SaveHook(Arc::new(move |kind: JobKind| {
            if kind == JobKind::Save && armed.load(Ordering::SeqCst) {
                held.wait();
            }
        })));
    });
    let elsewhere = r.env.root.path().join("Elsewhere");
    fs::create_dir_all(elsewhere.join("audio")).unwrap();
    for n in 1..=60 {
        fs::write(
            elsewhere.join("audio").join(format!("f{n}.wav")),
            wav_bytes(7, 0.9),
        )
        .unwrap();
    }
    armed.store(true, Ordering::SeqCst);
    let copy_id = r.s.request_id();
    let (tx, _copy_rx) = reply_channel();
    let _ = r.s.control_mut().handle(Inbox::Client {
        message: ClientMessage::Request {
            id: copy_id,
            request: Request::CopyMedia,
        },
        reply: tx,
    });
    // The Save As is delivered with the copy still held: its write waits at the save gate.
    let save_id = r.s.request_id();
    let (tx, mut save_rx) = reply_channel();
    let _ = r.s.control_mut().handle(Inbox::Client {
        message: ClientMessage::Request {
            id: save_id,
            request: Request::SaveAs {
                path: elsewhere
                    .join("Moved.openlab")
                    .to_string_lossy()
                    .into_owned(),
            },
        },
        reply: tx,
    });
    // The copy answers while the write is still pending: its parts are planned against the old
    // folder; the save then lands and moves the doc.
    copy_gate.open();
    wait_until_retargeting(&mut r);
    save_gate.open();
    assert!(r.s.wait_for(|c| c.docs_busy() == 0));
    // The Save As itself must have landed: a cue left on a relative form that resolves in the
    // other folder is what a save that did not move the doc would leave.
    let answers: Vec<ServerMessage> = std::iter::from_fn(|| save_rx.try_recv().ok()).collect();
    assert!(ok(&answers), "the Save As was not answered ok: {answers:?}");
    assert_eq!(
        r.s.control().show_path(),
        Some(elsewhere.join("Moved.openlab").as_path()),
        "the doc did not move to the Save As path"
    );
    let all = r.reports();
    assert_eq!(all.iter().map(|x| x.copied).sum::<u64>(), 60);
    let dir = portable(&elsewhere);
    for n in 1..=60 {
        let resolved = openlab_core::resolve(&r.media(n), &dir);
        assert_eq!(
            fs::read(&resolved).unwrap(),
            original_bytes(n),
            "cue {n} resolves to {resolved}"
        );
        assert!(
            same_file(
                Path::new(&resolved),
                &r.audio_dir().join(format!("f{n}.wav"))
            ),
            "cue {n} is not on its copy: {resolved}"
        );
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

/// Review #652 M1: the planner's part size is the config's; at one file a wake every cue still
/// ends on its copy and the report counts every file.
#[test]
fn a_copy_sorted_one_file_per_wake_points_every_cue_at_its_copy_and_counts_them_all() {
    let mut r = many(12, |c| {
        c.copy.plan_per_wake = 1;
        c.copy.apply_per_wake = 3;
    });
    assert!(ok(&r.copy_media()));
    let report = r.report();
    assert_eq!(
        (report.copied, report.reused, report.failed_count),
        (12, 0, 0)
    );
    for n in 1..=12 {
        let media = r.media(n);
        assert_eq!(media, format!("audio/f{n}.wav"));
        assert_eq!(
            fs::read(r.audio_dir().join(format!("f{n}.wav"))).unwrap(),
            original_bytes(n)
        );
    }
}

/// Review #652 M1: an Undo or Show Mode arriving while the planner still has files to sort
/// finishes the plan and the parts first, so the copy is one undo step.
#[test]
fn an_undo_while_the_copy_is_still_being_sorted_finishes_it_and_is_one_undo_step() {
    let mut r = many(12, |c| {
        c.copy.plan_per_wake = 1;
        c.copy.apply_per_wake = 1;
    });
    let before: Vec<String> = (1..=12).map(|n| r.media(n)).collect();
    assert!(ok(&r.copy_media()));
    // Under way: cue 1 is on its copy while the planner has files left to sort one a wake.
    wait_until_retargeting(&mut r);
    assert_eq!(r.media(12), before[11], "the last cue is still waiting");
    r.undo();
    let after: Vec<String> = (1..=12).map(|n| r.media(n)).collect();
    assert_eq!(after, before, "one undo reverts the whole copy");
    assert_eq!(r.report().copied, 12);
    let after: Vec<String> = (1..=12).map(|n| r.media(n)).collect();
    assert_eq!(after, before, "nothing retargets after the undo");
}

#[test]
fn show_mode_while_the_copy_is_still_being_sorted_retargets_everything_at_once() {
    let mut r = many(12, |c| {
        c.copy.plan_per_wake = 1;
        c.copy.apply_per_wake = 1;
    });
    assert!(ok(&r.copy_media()));
    wait_until_retargeting(&mut r);
    assert_ne!(
        r.media(12),
        "audio/f12.wav",
        "the last cue is still waiting"
    );
    let sid = r.s.request_id();
    assert!(ok(&r.s.send(ClientMessage::Request {
        id: sid,
        request: Request::SetShowMode { on: true },
    })));
    for n in 1..=12 {
        assert_eq!(r.media(n), format!("audio/f{n}.wav"));
    }
    assert_eq!(r.report().copied, 12);
}
