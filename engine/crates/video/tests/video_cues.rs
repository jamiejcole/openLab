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
use openlab_core::protocol::{
    ClientMessage, Outcome, Request, ResultData, ServerMessage, TransportAction,
};
use openlab_core::video_probe::{ProbeReport, StillFormat, VideoCodec};
use openlab_core::{Cue, CueKind, CueProblem, CueState, MediaRef, Stage, StageId, Workspace};
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
    video_show_of(workspace(cues))
}

/// An offline show of `ws` with stage 1 (1920 x 1080, keeping black) and the real stage host
/// on a virtual display.
fn video_show_of(mut ws: Workspace) -> OfflineShow {
    let mut stage = Stage::new(stage_id(), "Stage 1", 1920, 1080);
    stage.keep_black = true;
    ws.stages = vec![stage];
    let mut c: ControlConfig = config(&ws);
    let mut host = StageHostConfig::new(PathBuf::from(STAGE_HOST));
    host.args = vec!["--virtual-displays".into(), "V1:1920x1080".into()];
    c.stage_host = Some(host);
    let mut s = show_with(ws, c);
    s.settle_stages();
    s
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

// ---------------------------------------------------------------------------------------------
// Playback: what the stage shows, read back as the test card's frame barcode.

/// The frame number stage 1 shows (its barcode), or `None` when it shows no card.
fn shown(s: &mut OfflineShow) -> Option<u16> {
    probe_at(s, &[[0.5, 0.5]]).0
}

/// The barcode stage 1 shows, and the colours at `points`.
fn probe_at(s: &mut OfflineShow, points: &[[f32; 2]]) -> (Option<u16>, Vec<[u8; 4]>) {
    let id = s.request_id();
    let request = Request::StageProbe {
        stage: stage_id(),
        points: points.to_vec(),
    };
    let replies = s.send_until(ClientMessage::Request { id, request }, |c| {
        c.stages_settled()
    });
    let outcome = replies
        .into_iter()
        .find_map(|m| match m {
            ServerMessage::Result { id: got, outcome } if got == id => Some(outcome),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no answer to the probe"));
    match outcome {
        Outcome::Ok(body) => match body.data {
            Some(ResultData::StageProbe { frame, colours }) => (frame, colours),
            other => panic!("{other:?}"),
        },
        Outcome::Error(e) => panic!("probe refused: {e:?}"),
    }
}

fn still_cue(n: u128, file: &Path, duration: Option<f64>) -> Cue {
    cue(
        n,
        CueKind::Video {
            media: Some(MediaRef::new(file.to_str().unwrap())),
            stage: Some(stage_id()),
            duration,
        },
    )
}

/// Samples of `seconds` at the offline rate.
fn samples(seconds: f64) -> u64 {
    (seconds * f64::from(RATE)).round() as u64
}

fn state(s: &OfflineShow, n: u128) -> CueState {
    s.control().engine().state(id(n))
}

/// Runs the show until sample `at` is reached.
fn run_to(s: &mut OfflineShow, at: u64) {
    s.run(at.saturating_sub(s.now()));
}

/// A still as target: GO shows it until the cue is stopped (QLab 5 manual p. 209).
#[test]
fn a_still_shows_from_go_until_the_cue_is_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let png = make(dir.path(), "card.png", "png", None);
    let mut s = video_show(vec![still_cue(1, &png, None)]);
    assert_eq!(shown(&mut s), None, "nothing before GO");
    go(&mut s);
    s.run(samples(0.5));
    assert_eq!(shown(&mut s), Some(0));
    // Much later it is still up: a still has no end of its own.
    s.run(samples(5.0));
    assert_eq!(state(&s, 1), CueState::Running);
    assert_eq!(shown(&mut s), Some(0));
    s.transport(TransportAction::Stop {
        cue: id(1),
        fade_out: None,
    });
    s.run(BLOCK);
    assert_eq!(state(&s, 1), CueState::Idle);
    assert_eq!(shown(&mut s), None, "gone once stopped");
}

/// A still with a duration typed: GO shows it, and it ends after that time.
#[test]
fn a_still_with_a_duration_ends_after_it() {
    let dir = tempfile::tempdir().unwrap();
    let png = make(dir.path(), "card.png", "png", None);
    let mut s = video_show(vec![still_cue(1, &png, Some(1.0))]);
    go(&mut s);
    s.run(samples(0.9));
    assert_eq!(state(&s, 1), CueState::Running);
    assert_eq!(shown(&mut s), Some(0));
    s.run(samples(0.2));
    assert_eq!(state(&s, 1), CueState::Idle);
    assert_eq!(shown(&mut s), None);
}

/// GO: the picture plays on the cue's stage from its first frame in step with the engine's
/// clock, and the cue ends at its last frame (the file's length).
#[test]
fn go_plays_the_movie_from_its_first_frame_in_step_and_ends_at_its_last() {
    let dir = tempfile::tempdir().unwrap();
    let movie = make(dir.path(), "clip.mov", "h264", Some((50, 25)));
    let mut s = video_show(vec![video_cue(1, Some(&movie))]);
    let at = go(&mut s);
    // The frame shown is the one whose time the clock has reached.
    for want in [0u16, 1, 12, 25, 49] {
        run_to(&mut s, at + samples(f64::from(want) / 25.0) + samples(0.01));
        let position = (s.now() - at) as f64 / f64::from(RATE);
        let expected = (position * 25.0).floor() as u16;
        assert_eq!(shown(&mut s), Some(expected), "at {position} s");
        assert!(expected >= want);
    }
    // Two seconds long: idle (and the stage clear) once the clock passes its end.
    run_to(&mut s, at + samples(2.0) + BLOCK);
    assert_eq!(state(&s, 1), CueState::Idle);
    assert_eq!(shown(&mut s), None);
    assert_eq!(s.control().pictures_live(), 0);
}

/// The frame the clock has reached `played` seconds into a 25 fps movie.
fn frame_at(played: f64) -> u16 {
    (played * 25.0 + 1e-9).floor() as u16
}

/// Pause holds the frame on the stage; resume plays on from it, and the cue ends when the
/// rest of the movie has played (its end moved by the pause).
#[test]
fn pause_holds_the_frame_and_resume_plays_on_from_it() {
    let dir = tempfile::tempdir().unwrap();
    let movie = make(dir.path(), "clip.mov", "prores", Some((50, 25)));
    let mut s = video_show(vec![video_cue(1, Some(&movie))]);
    let at = go(&mut s);
    run_to(&mut s, at + samples(0.81));
    s.transport(TransportAction::Pause { cue: id(1) });
    let paused_at = s.now();
    s.run(BLOCK);
    assert_eq!(state(&s, 1), CueState::Paused);
    let held = shown(&mut s).expect("a frame while paused");
    let played = (paused_at - at) as f64 / f64::from(RATE);
    assert!(
        held.abs_diff(frame_at(played)) <= 1,
        "{held} held at {played} s"
    );
    // A second later the same frame.
    s.run(samples(1.0));
    assert_eq!(shown(&mut s), Some(held));
    s.transport(TransportAction::Resume { cue: id(1) });
    let resumed_at = s.now();
    run_to(&mut s, resumed_at + samples(0.5));
    let now = shown(&mut s).expect("playing again");
    assert!(
        now >= held + 11 && now <= held + 14,
        "{now} half a second after {held}"
    );
    // Two seconds of play in all: still running just before, idle just after.
    let end = at + samples(2.0) + (resumed_at - paused_at);
    run_to(&mut s, end - samples(0.1));
    assert_eq!(state(&s, 1), CueState::Running);
    run_to(&mut s, end + samples(0.1));
    assert_eq!(state(&s, 1), CueState::Idle);
    assert_eq!(shown(&mut s), None);
}

/// Load to time: GO then starts the picture there, not at its first frame; a plain load
/// starts it from the top.
#[test]
fn load_to_time_starts_the_picture_there() {
    let dir = tempfile::tempdir().unwrap();
    let movie = make(dir.path(), "clip.mov", "hevc", Some((50, 25)));
    let mut s = video_show(vec![video_cue(1, Some(&movie))]);
    s.transport(TransportAction::Load {
        cue: id(1),
        at: 1.0,
    });
    s.run(BLOCK);
    assert_eq!(state(&s, 1), CueState::Loaded);
    assert_eq!(shown(&mut s), None, "a loaded picture is not shown");
    let at = go(&mut s);
    run_to(&mut s, at + samples(0.02));
    assert_eq!(shown(&mut s), Some(25));
    // It has a second left.
    run_to(&mut s, at + samples(1.1));
    assert_eq!(state(&s, 1), CueState::Idle);

    s.transport(TransportAction::Load {
        cue: id(1),
        at: 0.0,
    });
    s.run(BLOCK);
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(0.02));
    assert_eq!(shown(&mut s), Some(0));
}

/// Two Video cues on one stage are both drawn, the later-started on top: stopping the top one
/// shows the one below, still playing.
#[test]
fn two_cues_on_one_stage_are_drawn_with_the_later_started_on_top() {
    let dir = tempfile::tempdir().unwrap();
    let a = make(dir.path(), "a.mov", "h264", Some((100, 25)));
    let b = make(dir.path(), "b.mov", "prores", Some((100, 25)));
    let mut s = video_show(vec![video_cue(1, Some(&a)), video_cue(2, Some(&b))]);
    let first = start(&mut s, 1);
    run_to(&mut s, first + samples(1.0));
    let second = start(&mut s, 2);
    run_to(&mut s, second + samples(0.22));
    let top = (s.now() - second) as f64 / f64::from(RATE);
    let shown_now = shown(&mut s).expect("a frame");
    assert!(
        shown_now.abs_diff(frame_at(top)) <= 1,
        "{shown_now}: cue 2's frame at {top} s is on top"
    );
    s.transport(TransportAction::Stop {
        cue: id(2),
        fade_out: None,
    });
    s.run(BLOCK);
    let below = (s.now() - first) as f64 / f64::from(RATE);
    let shown_now = shown(&mut s).expect("cue 1 still plays");
    assert!(
        shown_now.abs_diff(frame_at(below)) <= 1,
        "{shown_now}: cue 1's frame at {below} s"
    );
    assert_eq!(state(&s, 1), CueState::Running);
}

/// Panic fades the picture out over the panic time and then stops the cue: halfway, the
/// card's red is about half as bright over the black stage; after it, nothing is shown.
#[test]
fn panic_fades_the_picture_over_the_panic_time() {
    let dir = tempfile::tempdir().unwrap();
    let png = make(dir.path(), "card.png", "png", None);
    let mut ws: Workspace = workspace(vec![still_cue(1, &png, None)]);
    ws.settings.panic_duration = 1.0;
    let mut s = video_show_of(ws);
    go(&mut s);
    s.run(samples(0.2));
    let red = [[0.25, 0.3]];
    assert_eq!(probe_at(&mut s, &red).1, vec![[255, 0, 0, 255]]);
    s.transport(TransportAction::Panic);
    let at = s.now();
    run_to(&mut s, at + samples(0.5));
    let half = probe_at(&mut s, &red).1[0];
    assert!((100..=160).contains(&half[0]) && half[1] < 10, "{half:?}");
    run_to(&mut s, at + samples(1.1));
    assert_eq!(state(&s, 1), CueState::Idle);
    assert_eq!(probe_at(&mut s, &red).1, vec![[0, 0, 0, 255]]);
}

/// The notices the doc's clients were sent since the last call, by code.
fn notices(s: &mut OfflineShow) -> Vec<String> {
    s.take_outbound()
        .into_iter()
        .flat_map(|m| match m {
            ServerMessage::Runtime { changes, .. } => changes,
            _ => Vec::new(),
        })
        .filter_map(|c| match c {
            openlab_core::protocol::RuntimeChange::Warning {
                warning: openlab_core::protocol::WireWarning::Notice { code, .. },
            } => Some(code),
            _ => None,
        })
        .collect()
}

/// A picture that cannot be decoded mid-show (a movie whose first frames are good, so it
/// probes as playable, cut short further in) stops only its own cue, broken with
/// `decode_failed` and a notice; another cue on the stage plays on.
#[test]
fn a_decode_failure_mid_play_breaks_only_that_cue_with_a_notice() {
    let dir = tempfile::tempdir().unwrap();
    // ProRes: every frame is whole in itself, so a cut removes the frames after it.
    let doomed = make(dir.path(), "doomed.mov", "prores", Some((250, 25)));
    let fine = make(dir.path(), "fine.mov", "h264", Some((250, 25)));
    // Cut short at a fifth: the index (at the start in the test movies) still promises
    // every frame, so the file probes as playable, but the frames past there are not in it.
    let mut bytes = std::fs::read(&doomed).unwrap();
    bytes.truncate(bytes.len() / 5);
    std::fs::write(&doomed, &bytes).unwrap();
    let mut s = video_show(vec![video_cue(1, Some(&fine)), video_cue(2, Some(&doomed))]);
    let problems_2 = problems(&s, 2);
    if !problems_2.is_empty() {
        panic!("the damaged movie must still probe as playable: {problems_2:?}");
    }
    let first = start(&mut s, 1);
    s.run(BLOCK);
    start(&mut s, 2);
    s.take_outbound();
    let mut broke = false;
    for step in 1..=20 {
        run_to(&mut s, first + samples(0.5 * f64::from(step)));
        shown(&mut s);
        s.run(BLOCK);
        if state(&s, 2) == CueState::Broken {
            broke = true;
            break;
        }
    }
    assert!(broke, "cue 2 never broke");
    assert_eq!(
        s.control().engine().broken_reason(id(2)),
        Some(openlab_core::BrokenReason::DecodeFailed)
    );
    assert!(notices(&mut s).iter().any(|c| c == "video_decode_failed"));
    assert_eq!(state(&s, 1), CueState::Running, "the other cue plays on");
    let at = (s.now() - first) as f64 / f64::from(RATE);
    let now = shown(&mut s).expect("cue 1 still shown");
    assert!(now.abs_diff(frame_at(at)) <= 1, "{now} at {at} s");
}

/// The stage host killed mid-play: the engine keeps running, the cue keeps its state, and a
/// relaunched stage host shows the picture again where the clock has got to.
#[test]
fn a_stage_host_killed_mid_play_shows_the_picture_again_when_relaunched() {
    let dir = tempfile::tempdir().unwrap();
    let movie = make(dir.path(), "clip.mov", "h264", Some((250, 25)));
    let mut s = video_show(vec![video_cue(1, Some(&movie))]);
    let at = go(&mut s);
    run_to(&mut s, at + samples(0.5));
    assert!(shown(&mut s).is_some());
    let pid = s.control().stage_host_pid().unwrap();
    let killed = Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    assert!(s.wait_for(|c| c.stage_host_pid().is_some_and(|p| p != pid)));
    s.settle_stages();
    assert_eq!(state(&s, 1), CueState::Running);
    run_to(&mut s, at + samples(2.0));
    let played = (s.now() - at) as f64 / f64::from(RATE);
    let now = shown(&mut s).expect("the picture is back");
    assert!(now.abs_diff(frame_at(played)) <= 1, "{now} at {played} s");
}

/// What the app is told of a Video cue's file: a video's length and format (codec, pixel size,
/// frame rate), a still's format and size with its typed duration (none: until stopped), and
/// nothing for a file that does not play.
#[test]
fn clients_are_told_a_video_cues_length_and_format() {
    let dir = tempfile::tempdir().unwrap();
    let movie = make(dir.path(), "clip.mov", "prores", Some((50, 25)));
    let png = make(dir.path(), "card.png", "png", None);
    let gone = dir.path().join("gone.mov");
    let mut s = video_show(vec![
        video_cue(1, Some(&movie)),
        still_cue(2, &png, Some(5.0)),
        still_cue(3, &png, None),
        video_cue(4, Some(&gone)),
    ]);
    let told = s.control().snapshot().media_durations;
    let of = |n: u128| told.iter().find(|m| m.cue == id(n)).cloned();
    let video = of(1).expect("the movie's lengths");
    assert_eq!((video.file, video.duration), (Some(2.0), Some(2.0)));
    assert!(matches!(
        video.picture,
        Some(ProbeReport::Video {
            codec: VideoCodec::ProRes,
            width: 288,
            height: 160,
            ..
        })
    ));
    let still = of(2).expect("the still's");
    assert_eq!((still.file, still.duration), (None, Some(5.0)));
    assert!(matches!(
        still.picture,
        Some(ProbeReport::Still {
            format: StillFormat::Png,
            ..
        })
    ));
    let until_stopped = of(3).expect("the untimed still's");
    assert_eq!(until_stopped.duration, None);
    assert!(until_stopped.picture.is_some());
    assert!(of(4).is_none_or(|m| m.picture.is_none() && m.duration.is_none()));
    // A duration typed on the still is told at once.
    s.take_outbound();
    let request = s.request_id();
    let kind = still_cue(3, &png, Some(2.5)).kind;
    s.send(ClientMessage::Command {
        id: request,
        command: Box::new(openlab_core::Command::SetCueProperty {
            id: id(3),
            property: openlab_core::CueProperty::Kind { value: kind },
        }),
        coalesce: None,
    });
    s.run(BLOCK);
    let changed = s.control().snapshot().media_durations;
    assert_eq!(
        changed
            .iter()
            .find(|m| m.cue == id(3))
            .and_then(|m| m.duration),
        Some(2.5)
    );
}
