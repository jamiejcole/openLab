// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// The stage host reads video with AVFoundation on macOS only; elsewhere it says it has no video
// (the engine's own tests cover that).
#![cfg(target_os = "macos")]

//! Video cues (CT-03, VID-03) on the real engine with the real stage host: the first look at
//! their files. Media is made at test time by the test-support stage host
//! (`--make-test-media`: AVFoundation's encoders, ImageIO for stills), every frame the test card
//! for its own frame number. Nothing is checked in.

#[path = "../../engine/tests/show/mod.rs"]
mod show;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use openlab_audio::MediaError;
use openlab_core::video_probe::{ProbeReport, StillFormat, VideoCodec};
use openlab_core::{Cue, CueKind, CueProblem, MediaRef, Stage, StageId, Workspace};
use openlab_engine::control::ControlConfig;
use openlab_engine::control::offline::OfflineShow;
use openlab_engine::stage::StageHostConfig;
use openlab_engine::stage_probe::probe_file;
use show::*;
use uuid::Uuid;

const STAGE_HOST: &str = env!("CARGO_BIN_EXE_openlab-stage");
const LIMIT: Duration = Duration::from_secs(30);

fn stage_id() -> StageId {
    StageId(Uuid::from_u128(0x5_0001))
}

/// Makes `name` in `dir` with the stage host: a movie of `frames` frames at `fps` for a codec,
/// or frame 0's card as a still.
fn make(dir: &Path, name: &str, kind: &str, frames_fps: Option<(u16, u16)>) -> PathBuf {
    let path = dir.join(name);
    let mut command = Command::new(STAGE_HOST);
    command.arg("--make-test-media").arg(kind).arg(&path);
    if let Some((frames, fps)) = frames_fps {
        command.arg(frames.to_string()).arg(fps.to_string());
    }
    let out = command.output().unwrap();
    assert!(
        out.status.success(),
        "{kind}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    path
}

fn probe(path: &Path) -> Result<ProbeReport, MediaError> {
    probe_file(Path::new(STAGE_HOST), path, LIMIT)
}

/// H.264, HEVC and ProRes in a QuickTime movie, and PNG, JPEG and TIFF stills, each read with
/// their codec, pixel size, frame rate and length (VID-03).
#[test]
fn h264_hevc_and_prores_movies_and_png_jpeg_and_tiff_stills_are_read() {
    let dir = tempfile::tempdir().unwrap();
    for (kind, codec) in [
        ("h264", VideoCodec::H264),
        ("hevc", VideoCodec::Hevc),
        ("prores", VideoCodec::ProRes),
    ] {
        let file = make(dir.path(), &format!("{kind}.mov"), kind, Some((50, 25)));
        assert_eq!(
            probe(&file).unwrap(),
            ProbeReport::Video {
                codec,
                width: 288,
                height: 160,
                fps: 25.0,
                duration: 2.0,
            },
            "{kind}"
        );
    }
    for (kind, format) in [
        ("png", StillFormat::Png),
        ("jpeg", StillFormat::Jpeg),
        ("tiff", StillFormat::Tiff),
    ] {
        let file = make(dir.path(), &format!("still.{kind}"), kind, None);
        assert_eq!(
            probe(&file).unwrap(),
            ProbeReport::Still {
                format,
                width: 288,
                height: 160,
            },
            "{kind}"
        );
    }
}

fn unsupported(r: Result<ProbeReport, MediaError>) -> String {
    match r {
        Err(MediaError::Unsupported(why)) => why,
        other => panic!("{other:?}"),
    }
}

/// A file that is not a movie or still openLab plays gives a reason, never a crash or a hang:
/// noise, an empty file, a movie cut short, a still format it does not play, a missing file.
#[test]
fn damaged_or_unsupported_files_give_a_reason() {
    let dir = tempfile::tempdir().unwrap();
    let movie = std::fs::read(make(dir.path(), "good.mov", "h264", Some((25, 25)))).unwrap();
    let cut = dir.path().join("cut.mov");
    std::fs::write(&cut, movie.get(..movie.len() / 2).unwrap()).unwrap();
    let empty = dir.path().join("empty.mov");
    std::fs::write(&empty, b"").unwrap();
    let noise = dir.path().join("noise.mp4");
    std::fs::write(
        &noise,
        (0..20_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    for file in [&cut, &empty, &noise] {
        let why = unsupported(probe(file));
        assert!(!why.is_empty(), "{}", file.display());
    }
    assert!(matches!(
        probe(&dir.path().join("gone.mov")),
        Err(MediaError::NotFound(_))
    ));
}

/// Mutated movies (a seeded walk over every part of a good file) each give an answer within
/// the limit: the file plays, or a reason. A decoder that crashed or hung on one would take
/// only its probe process (fuzzed; bounded time).
#[test]
fn mutated_movies_give_an_answer_within_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let good = std::fs::read(make(dir.path(), "good.mov", "h264", Some((10, 25)))).unwrap();
    let mut seed: u64 = 0x5eed;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for round in 0..24 {
        let mut bytes = good.clone();
        for _ in 0..(1 + round % 8) {
            let at = (next() % bytes.len() as u64) as usize;
            if let Some(b) = bytes.get_mut(at) {
                *b = next() as u8;
            }
        }
        let file = dir.path().join(format!("m{round}.mov"));
        std::fs::write(&file, &bytes).unwrap();
        match probe(&file) {
            Ok(ProbeReport::Video { .. }) | Err(MediaError::Unsupported(_)) => {}
            other => panic!("round {round}: {other:?}"),
        }
    }
}

fn video_cue(n: u128, media: Option<&Path>) -> Cue {
    cue(
        n,
        CueKind::Video {
            media: media.map(|p| MediaRef::new(p.to_str().unwrap())),
            stage: Some(stage_id()),
            duration: None,
        },
    )
}

fn video_show(cues: Vec<Cue>) -> OfflineShow {
    let mut ws: Workspace = workspace(cues);
    ws.stages = vec![Stage::new(stage_id(), "Stage 1", 1920, 1080)];
    let mut c: ControlConfig = config(&ws);
    let mut host = StageHostConfig::new(PathBuf::from(STAGE_HOST));
    host.args = vec!["--virtual-displays".into(), "V1:1920x1080".into()];
    c.stage_host = Some(host);
    show_with(ws, c)
}

fn problems(s: &OfflineShow, n: u128) -> Vec<CueProblem> {
    s.control().engine().problems(id(n)).to_vec()
}

/// A Video cue's file is looked at as soon as the show opens: one that plays leaves the cue
/// ready with its format known; one that is not there, or not a file openLab plays, breaks it
/// with that reason; and a cue with no file is broken with that reason, as an Audio cue is.
#[test]
fn a_video_cues_file_is_probed_and_a_bad_or_missing_one_breaks_it_with_a_reason() {
    let dir = tempfile::tempdir().unwrap();
    let good = make(dir.path(), "good.mov", "h264", Some((25, 25)));
    let gif = dir.path().join("bad.gif");
    std::fs::write(&gif, b"GIF89a\x01\x00\x01\x00\x00\x00\x00;").unwrap();
    let gone = dir.path().join("gone.mov");
    let s = video_show(vec![
        video_cue(1, Some(&good)),
        video_cue(2, Some(&gone)),
        video_cue(3, Some(&gif)),
        video_cue(4, None),
    ]);
    assert_eq!(problems(&s, 1), vec![]);
    let (_, report) = s
        .control()
        .picture_probe(good.to_str().unwrap())
        .expect("probed");
    assert!(matches!(
        report,
        ProbeReport::Video {
            codec: VideoCodec::H264,
            width: 288,
            height: 160,
            ..
        }
    ));
    assert!(matches!(
        problems(&s, 2).as_slice(),
        [CueProblem::MediaMissing { .. }]
    ));
    assert!(matches!(
        problems(&s, 3).as_slice(),
        [CueProblem::MediaUnsupported { .. }]
    ));
    assert_eq!(problems(&s, 4), vec![CueProblem::NoMedia]);
}

/// A Video cue's file that goes missing gets the missing-file mark at the media watch's next
/// pass, and comes back when the file does, as for audio.
#[test]
fn a_video_file_that_goes_missing_breaks_its_cue_until_it_is_back() {
    let dir = tempfile::tempdir().unwrap();
    let good = make(dir.path(), "good.mov", "h264", Some((25, 25)));
    let mut s = video_show(vec![video_cue(1, Some(&good))]);
    assert_eq!(problems(&s, 1), vec![]);
    let aside = dir.path().join("aside.mov");
    std::fs::rename(&good, &aside).unwrap();
    assert!(s.watch_pass());
    assert!(matches!(
        problems(&s, 1).as_slice(),
        [CueProblem::MediaMissing { .. }]
    ));
    std::fs::rename(&aside, &good).unwrap();
    // Back after two passes that see it the same, then probed again.
    s.watch_pass();
    s.watch_pass();
    s.settle_media();
    assert_eq!(problems(&s, 1), vec![]);
}
