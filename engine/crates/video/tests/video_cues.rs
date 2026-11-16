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
use openlab_core::{
    Cue, CueKind, CueProblem, CueState, MediaRef, Stage, StageId, VideoGeometry, Workspace,
};
use openlab_engine::control::ControlConfig;
use openlab_engine::control::offline::OfflineShow;
use openlab_engine::stage::StageHostConfig;
use openlab_engine::stage_probe::probe_file;
use show::*;
use uuid::Uuid;

const STAGE_HOST: &str = env!("CARGO_BIN_EXE_openlab-stage");
const LIMIT: Duration = Duration::from_secs(30);
/// How long a test's stage host may take to answer a ping: far beyond any machine's slowness.
const SLOW_MACHINE: Duration = Duration::from_secs(120);

fn stage_id() -> StageId {
    StageId(Uuid::from_u128(0x5_0001))
}

/// Makes `name` in `dir`: a movie of `frames` frames at `fps` for a codec, or frame 0's card as
/// a still. Each kind of file is made once per test run by the stage host (`--make-test-media`)
/// and copied for each test, which may then move, damage or replace its copy.
fn make(dir: &Path, name: &str, kind: &str, frames_fps: Option<(u16, u16)>) -> PathBuf {
    let path = dir.join(name);
    std::fs::copy(fixture(kind, frames_fps), &path).unwrap();
    path
}

/// The run's one copy of a test file, made on first use. Made one at a time, so tests running
/// at once never encode together.
fn fixture(kind: &str, frames_fps: Option<(u16, u16)>) -> PathBuf {
    static MADE: std::sync::OnceLock<std::sync::Mutex<(PathBuf, Vec<PathBuf>)>> =
        std::sync::OnceLock::new();
    let made = MADE.get_or_init(|| {
        // Under the target's own temporary directory, one per test process: kept for the run.
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("video-cues-media-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::sync::Mutex::new((dir, Vec::new()))
    });
    let mut made = made
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let ext = match kind {
        "h264" | "hevc" | "prores" | "prores4444" => "mov",
        "huge-png" | "wide-png" | "tall-png" | "alpha-png" | "tiny-png" => "png",
        other => other,
    };
    let name = match frames_fps {
        Some((frames, fps)) => format!("{kind}-{frames}-{fps}.{ext}"),
        None => format!("{kind}.{ext}"),
    };
    let path = made.0.join(name);
    if made.1.contains(&path) {
        return path;
    }
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
    made.1.push(path.clone());
    path
}

/// Whether a movie of codec `kind` is decoded reliably here, so a test may count on its
/// frames being drawn: by a software decoder (as the stage host's `--test-decoder` finds
/// VideoToolbox picks for it), or by the video hardware of a machine that is not virtual. A
/// virtual machine's paravirtual video decoder has failed and stalled good movies on CI.
fn decodes_reliably(kind: &str) -> bool {
    static FOUND: std::sync::Mutex<Vec<(String, bool)>> = std::sync::Mutex::new(Vec::new());
    let mut found = FOUND
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((_, ok)) = found.iter().find(|(k, _)| k == kind) {
        return *ok;
    }
    let out = Command::new(STAGE_HOST)
        .arg("--test-decoder")
        .arg(fixture(kind, Some((10, 25))))
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert!(
        out.status.success() && (said == "software" || said == "hardware"),
        "{kind}: {said} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let vm = virtual_machine();
    let ok = said == "software" || !vm;
    record(&format!(
        "decoder {kind}: {said}{}",
        if vm { ", in a virtual machine" } else { "" }
    ));
    found.push((kind.to_owned(), ok));
    ok
}

/// Whether this Mac runs under a hypervisor.
fn virtual_machine() -> bool {
    let out = Command::new("sysctl")
        .args(["-n", "kern.hv_vmm_present"])
        .output()
        .unwrap();
    assert!(out.status.success(), "no kern.hv_vmm_present");
    String::from_utf8_lossy(&out.stdout).trim() == "1"
}

/// The codec for a movie a test plays and draws: H.264 where it decodes reliably
/// ([`decodes_reliably`]), else the first of ProRes and HEVC that does.
fn play_codec() -> Option<&'static str> {
    ["h264", "prores", "hevc"]
        .into_iter()
        .find(|kind| decodes_reliably(kind))
}

/// `movie!(dir, name, (frames, fps))` makes a movie for a test to play in [`play_codec`];
/// `movie!(dir, name, (frames, fps), kind)` in codec `kind`. When no such codec decodes
/// reliably here, the test records a skip and ends ([`skipped`]), or fails where skips are
/// not recorded.
macro_rules! movie {
    ($dir:expr, $name:expr, $frames_fps:expr) => {
        match play_codec() {
            Some(kind) => make($dir, $name, kind, Some($frames_fps)),
            None => return skipped("no codec decodes reliably on this machine"),
        }
    };
    ($dir:expr, $name:expr, $frames_fps:expr, $kind:expr) => {
        if decodes_reliably($kind) {
            make($dir, $name, $kind, Some($frames_fps))
        } else {
            return skipped(concat!(
                $kind,
                " decodes in this virtual machine's video hardware"
            ));
        }
    };
}

/// The file a test run's skips (and what this machine decodes with) are recorded in, set by
/// `scripts/rust-checks.sh test`, which prints it after the run, passing or not.
const SKIPS_ENV: &str = "OPENLAB_TEST_SKIPS";

/// Records a skip of the running test and why, in the file [`SKIPS_ENV`] names. Without that
/// file a test that cannot run here fails: a skip happens only where it is reported.
fn skipped(why: &str) {
    let test = std::thread::current()
        .name()
        .unwrap_or("a video_cues test")
        .to_owned();
    if !record(&format!("skip {test}: {why}")) {
        panic!("{why} (set {SKIPS_ENV} to record this as a skip)");
    }
}

/// Appends `line` to the file [`SKIPS_ENV`] names; `false` when none is set.
fn record(line: &str) -> bool {
    use std::io::Write;
    let Some(file) = std::env::var_os(SKIPS_ENV) else {
        return false;
    };
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .unwrap();
    // One write per line: tests running at once never mix their lines.
    out.write_all(format!("{line}\n").as_bytes()).unwrap();
    true
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
/// noise, an empty file, a movie cut inside its index, a still cut short (PNG, JPEG, TIFF), a
/// still too large to hold, a still format it does not play, a missing file.
#[test]
fn damaged_or_unsupported_files_give_a_reason() {
    let dir = tempfile::tempdir().unwrap();
    let movie = std::fs::read(make(dir.path(), "good.mov", "h264", Some((25, 25)))).unwrap();
    // Cut just inside its index (`moov`): no track can be read, on every decoder. (A cut further
    // in may leave the index whole and the first frames playable: see the next test.)
    let index = movie
        .windows(4)
        .position(|w| w == b"moov")
        .expect("the test movie has an index");
    let cut = dir.path().join("cut.mov");
    std::fs::write(&cut, movie.get(..index + 16).unwrap()).unwrap();
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
    // Stills cut short: ImageIO reads their headers, so only decoding every pixel (and, for a
    // JPEG, finding its end) tells them from whole ones.
    for (kind, keep) in [("png", 3), ("jpeg", 2), ("tiff", 2)] {
        let whole = std::fs::read(make(dir.path(), &format!("whole.{kind}"), kind, None)).unwrap();
        let file = dir.path().join(format!("cut.{kind}"));
        std::fs::write(&file, whole.get(..whole.len() / keep).unwrap()).unwrap();
        let why = unsupported(probe(&file));
        assert!(why.contains("damaged"), "{kind}: {why}");
    }
    // A still with more pixels than a picture may have is refused before it is decoded.
    let huge = make(dir.path(), "huge.png", "huge-png", None);
    let why = unsupported(probe(&huge));
    assert!(why.contains("16384 x 8192"), "{why}");
    assert!(matches!(
        probe(&dir.path().join("gone.mov")),
        Err(MediaError::NotFound(_))
    ));
}

/// A movie cut short in its picture data gives an answer within the limit, never a crash or a
/// hang: where its index (`moov`) survives the cut its first frames still decode, so it may
/// play (its frames then run out on stage: `a_decode_failure_mid_play_breaks_only_that_cue_with_a_notice`);
/// otherwise it is unsupported. Which depends on the encoder's output (Apple silicon and Intel
/// Macs encode the test movie to different sizes).
#[test]
fn a_movie_cut_short_gives_an_answer_within_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let movie = std::fs::read(make(dir.path(), "good.mov", "h264", Some((25, 25)))).unwrap();
    for keep in [2, 3, 4] {
        let cut = dir.path().join(format!("cut{keep}.mov"));
        std::fs::write(&cut, movie.get(..movie.len() * (keep - 1) / keep).unwrap()).unwrap();
        match probe(&cut) {
            Ok(ProbeReport::Video { .. }) | Err(MediaError::Unsupported(_)) => {}
            other => panic!("cut to {}/{keep}: {other:?}", keep - 1),
        }
    }
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
            geometry: Default::default(),
            timing: Default::default(),
            sound: Default::default(),
        },
    )
}

fn video_show(cues: Vec<Cue>) -> OfflineShow {
    video_show_of(workspace(cues))
}

/// An offline show of `ws` with stage 1 (1920 x 1080, keeping black) and the real stage host
/// on a virtual display.
fn video_show_of(ws: Workspace) -> OfflineShow {
    video_show_args(ws, &[])
}

/// As [`video_show_of`], the stage host given `extra` arguments too.
fn video_show_args(ws: Workspace, extra: &[&str]) -> OfflineShow {
    video_show_host(ws, extra, |_| {})
}

/// As [`video_show_args`], the stage host's supervision changed by `tweak`.
fn video_show_host(
    mut ws: Workspace,
    extra: &[&str],
    tweak: impl FnOnce(&mut StageHostConfig),
) -> OfflineShow {
    let mut stage = Stage::new(stage_id(), "Stage 1", 1920, 1080);
    stage.keep_black = true;
    ws.stages = vec![stage];
    let mut c: ControlConfig = config(&ws);
    let mut host = StageHostConfig::new(PathBuf::from(STAGE_HOST));
    host.args = vec!["--virtual-displays".into(), "V1:1920x1080".into()];
    host.args.extend(extra.iter().map(|a| (*a).to_owned()));
    // A stage host slow to answer on a loaded machine (a probe waiting on a file's open) is
    // never killed as hung: the tests kill one when they mean to.
    host.answer_within = SLOW_MACHINE;
    tweak(&mut host);
    c.stage_host = Some(host);
    let mut s = show_with(ws, c);
    s.settle_stages();
    // The files' probes are answered too: the tests read what they found.
    s.settle_media();
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

/// The frame stage 1 shows, which the test expects there (`what`). When there is none the
/// failure says what the engine and the stage host saw ([`seen`]).
fn on_stage(s: &mut OfflineShow, what: &str) -> u16 {
    shown(s).unwrap_or_else(|| panic!("{what}: nothing on the stage\n{}", seen(s)))
}

/// Checks that cue `n` is in state `want`; when not, the failure says what the engine and the
/// stage host saw (as [`on_stage`]).
fn expect_state(s: &OfflineShow, n: u128, want: CueState, what: &str) {
    let got = state(s, n);
    if got != want {
        panic!("{what}: cue {n} is {got:?}, not {want:?}\n{}", seen(s));
    }
}

/// What the engine and the stage host saw: the sample, the pictures up and open, any wait that
/// gave up, and the end of the show log (a picture failure, a stage host restart, a probe the
/// stage host could not draw).
fn seen(s: &OfflineShow) -> String {
    let c = s.control();
    let log: Vec<String> = c
        .show_log()
        .tail(30)
        .iter()
        .map(|e| format!("{:?} {:?} {}", e.level, e.kind, e.message))
        .collect();
    format!(
        "at sample {}; pictures up {}, open {}, waits given up {}\n{}",
        s.now(),
        c.pictures_live(),
        c.pictures_opened(),
        s.stalls(),
        log.join("\n")
    )
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
            geometry: Default::default(),
            timing: Default::default(),
            sound: Default::default(),
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
    expect_state(&s, 1, CueState::Running, "");
    assert_eq!(shown(&mut s), Some(0));
    s.transport(TransportAction::Stop {
        cue: id(1),
        fade_out: None,
    });
    s.run(BLOCK);
    expect_state(&s, 1, CueState::Idle, "");
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
    expect_state(&s, 1, CueState::Running, "");
    assert_eq!(shown(&mut s), Some(0));
    s.run(samples(0.2));
    expect_state(&s, 1, CueState::Idle, "");
    assert_eq!(shown(&mut s), None);
}

/// GO: the picture plays on the cue's stage from its first frame in step with the engine's
/// clock, and the cue ends at its last frame (the file's length).
#[test]
fn go_plays_the_movie_from_its_first_frame_in_step_and_ends_at_its_last() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (50, 25));
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
    expect_state(&s, 1, CueState::Idle, "");
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
    let movie = movie!(dir.path(), "clip.mov", (50, 25), "prores");
    let mut s = video_show(vec![video_cue(1, Some(&movie))]);
    let at = go(&mut s);
    run_to(&mut s, at + samples(0.81));
    s.transport(TransportAction::Pause { cue: id(1) });
    let paused_at = s.now();
    s.run(BLOCK);
    expect_state(&s, 1, CueState::Paused, "");
    let held = on_stage(&mut s, "a frame while paused");
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
    let now = on_stage(&mut s, "playing again");
    assert!(
        now >= held + 11 && now <= held + 14,
        "{now} half a second after {held}"
    );
    // Two seconds of play in all: still running just before, idle just after.
    let end = at + samples(2.0) + (resumed_at - paused_at);
    run_to(&mut s, end - samples(0.1));
    expect_state(&s, 1, CueState::Running, "");
    run_to(&mut s, end + samples(0.1));
    expect_state(&s, 1, CueState::Idle, "");
    assert_eq!(shown(&mut s), None);
}

/// Load to time: GO then starts the picture there, not at its first frame; a plain load
/// starts it from the top.
#[test]
fn load_to_time_starts_the_picture_there() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (50, 25), "hevc");
    let mut s = video_show(vec![video_cue(1, Some(&movie))]);
    s.transport(TransportAction::Load {
        cue: id(1),
        at: 1.0,
    });
    s.run(BLOCK);
    expect_state(&s, 1, CueState::Loaded, "");
    assert_eq!(shown(&mut s), None, "a loaded picture is not shown");
    let at = go(&mut s);
    run_to(&mut s, at + samples(0.02));
    assert_eq!(shown(&mut s), Some(25));
    // It has a second left.
    run_to(&mut s, at + samples(1.1));
    expect_state(&s, 1, CueState::Idle, "");

    s.transport(TransportAction::Load {
        cue: id(1),
        at: 0.0,
    });
    s.run(BLOCK);
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(0.02));
    assert_eq!(shown(&mut s), Some(0));
}

/// A Load cue with a negative time loads a Video cue that long before its movie's end (QLab 5
/// manual pp. 680-681): GO then starts the picture there.
#[test]
fn a_load_cue_with_a_negative_time_counts_back_from_the_movies_end() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (50, 25), "hevc");
    let load = cue(
        2,
        CueKind::Load {
            target: Some(id(1)),
            load_to_time: -0.5,
        },
    );
    let mut s = video_show(vec![video_cue(1, Some(&movie)), load]);
    start(&mut s, 2);
    s.run(BLOCK);
    expect_state(&s, 1, CueState::Loaded, "");
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(0.02));
    // 2 s of movie, from half a second before its end: 1.52 s in now.
    assert_eq!(shown(&mut s), Some(frame_at(1.52)));
    run_to(&mut s, at + samples(0.6));
    expect_state(&s, 1, CueState::Idle, "half a second left");
}

/// Two Video cues on one stage are both drawn, the later-started on top: stopping the top one
/// shows the one below, still playing.
#[test]
fn two_cues_on_one_stage_are_drawn_with_the_later_started_on_top() {
    let dir = tempfile::tempdir().unwrap();
    let a = movie!(dir.path(), "a.mov", (100, 25));
    let b = movie!(dir.path(), "b.mov", (100, 25), "prores");
    let mut s = video_show(vec![video_cue(1, Some(&a)), video_cue(2, Some(&b))]);
    let first = start(&mut s, 1);
    run_to(&mut s, first + samples(1.0));
    let second = start(&mut s, 2);
    run_to(&mut s, second + samples(0.22));
    let top = (s.now() - second) as f64 / f64::from(RATE);
    let shown_now = on_stage(&mut s, "a frame");
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
    let shown_now = on_stage(&mut s, "cue 1 still plays");
    assert!(
        shown_now.abs_diff(frame_at(below)) <= 1,
        "{shown_now}: cue 1's frame at {below} s"
    );
    expect_state(&s, 1, CueState::Running, "");
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
    expect_state(&s, 1, CueState::Idle, "");
    assert_eq!(probe_at(&mut s, &red).1, vec![[0, 0, 0, 255]]);
}

/// The notices the doc's clients were sent since the last call, as (code, text).
fn notice_texts(s: &mut OfflineShow) -> Vec<(String, String)> {
    s.take_outbound()
        .into_iter()
        .flat_map(|m| match m {
            ServerMessage::Runtime { changes, .. } => changes,
            _ => Vec::new(),
        })
        .filter_map(|c| match c {
            openlab_core::protocol::RuntimeChange::Warning {
                warning: openlab_core::protocol::WireWarning::Notice { code, message, .. },
            } => Some((code, message)),
            _ => None,
        })
        .collect()
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
    let doomed = movie!(dir.path(), "doomed.mov", (250, 25), "prores");
    let fine = movie!(dir.path(), "fine.mov", (250, 25));
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
    expect_state(&s, 1, CueState::Running, "the other cue plays on");
    let at = (s.now() - first) as f64 / f64::from(RATE);
    let now = on_stage(&mut s, "cue 1 still shown");
    assert!(now.abs_diff(frame_at(at)) <= 1, "{now} at {at} s");
}

/// The stage host killed mid-play: the engine keeps running, the cue keeps its state, and a
/// relaunched stage host shows the picture again where the clock has got to.
#[test]
fn a_stage_host_killed_mid_play_shows_the_picture_again_when_relaunched() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (250, 25));
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
    expect_state(&s, 1, CueState::Running, "");
    run_to(&mut s, at + samples(2.0));
    let played = (s.now() - at) as f64 / f64::from(RATE);
    let now = on_stage(&mut s, "the picture is back");
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

/// Files whose picture probe is held back until released (`gated_probe`).
static GATED: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
static GATE_OPENED: std::sync::Condvar = std::sync::Condvar::new();

/// The real probe, held while its file is in `GATED` (bounded by `LIMIT`).
fn gated_probe(path: &Path) -> Result<ProbeReport, MediaError> {
    let mut gated = GATED.lock().unwrap();
    // A hang guard: at most `LIMIT` in waits of 50 ms.
    let mut rounds = LIMIT.as_millis() / 50;
    while gated.iter().any(|p| p == path) && rounds > 0 {
        rounds -= 1;
        gated = GATE_OPENED
            .wait_timeout(gated, Duration::from_millis(50))
            .unwrap()
            .0;
    }
    drop(gated);
    probe(path)
}

fn release_probe(path: &Path) {
    GATED.lock().unwrap().retain(|p| p != path);
    GATE_OPENED.notify_all();
}

/// A show like `video_show_of` whose picture probes go through `gated_probe`.
fn gated_video_show(cues: Vec<Cue>) -> OfflineShow {
    let mut ws = workspace(cues);
    let mut stage = Stage::new(stage_id(), "Stage 1", 1920, 1080);
    stage.keep_black = true;
    ws.stages = vec![stage];
    let mut c: ControlConfig = config(&ws);
    let mut host = StageHostConfig::new(PathBuf::from(STAGE_HOST));
    host.args = vec!["--virtual-displays".into(), "V1:1920x1080".into()];
    host.answer_within = SLOW_MACHINE;
    c.stage_host = Some(host);
    c.picture_probe = Some(gated_probe);
    let mut s = show_with(ws, c);
    s.settle_stages();
    // The files' probes are answered too: the tests read what they found.
    s.settle_media();
    s
}

/// Sets cue `n`'s file.
fn set_media(s: &mut OfflineShow, n: u128, file: &Path) {
    let request = s.request_id();
    s.send(ClientMessage::Command {
        id: request,
        command: Box::new(openlab_core::Command::SetCueProperty {
            id: id(n),
            property: openlab_core::CueProperty::Media {
                value: Some(MediaRef::new(file.to_str().unwrap())),
            },
        }),
        coalesce: None,
    });
}

/// GO on a Video cue whose new file is still being probed waits for the probe, as an Audio
/// cue does: it is not broken, the operator is told it waits, and the picture starts (late,
/// from its first frame) when the file is known to play. A file whose probe then fails breaks
/// the cue.
#[test]
fn go_on_a_video_cue_whose_file_is_being_probed_waits_and_then_plays() {
    let dir = tempfile::tempdir().unwrap();
    let a = movie!(dir.path(), "a.mov", (50, 25));
    let b = movie!(dir.path(), "b.mov", (50, 25));
    let gif = dir.path().join("bad.gif");
    std::fs::write(&gif, b"GIF89a\x01\x00\x01\x00\x00\x00\x00;").unwrap();
    let mut s = gated_video_show(vec![video_cue(1, Some(&a)), video_cue(2, Some(&a))]);
    assert_eq!(problems(&s, 1), vec![]);
    GATED.lock().unwrap().extend([b.clone(), gif.clone()]);
    set_media(&mut s, 1, &b);
    set_media(&mut s, 2, &gif);
    s.take_outbound();
    let at = go(&mut s);
    start(&mut s, 2);
    s.run(samples(1.0));
    expect_state(&s, 1, CueState::Running, "waits, not broken");
    expect_state(&s, 2, CueState::Running, "");
    assert!(notices(&mut s).iter().any(|c| c == "probe_wait"));
    assert_eq!(
        shown(&mut s),
        None,
        "nothing on stage before the file is read"
    );
    release_probe(&b);
    release_probe(&gif);
    s.settle_media();
    let released = s.now();
    expect_state(&s, 1, CueState::Running, "");
    assert_eq!(
        state(&s, 2),
        CueState::Broken,
        "a file that does not play breaks it"
    );
    run_to(&mut s, released + samples(0.5));
    let played = (s.now() - released) as f64 / f64::from(RATE);
    let now = on_stage(&mut s, "the picture plays once its file is read");
    assert!(now.abs_diff(frame_at(played)) <= 1, "{now} at {played} s");
    // Its whole length from the late start.
    run_to(&mut s, released + samples(1.9));
    expect_state(&s, 1, CueState::Running, "");
    run_to(&mut s, released + samples(2.1));
    expect_state(&s, 1, CueState::Idle, "");
    assert!(s.now() > at);
}

/// A start held by its file's probe fails after the hold limit (sample time) with a notice,
/// as an Audio cue's does.
#[test]
fn a_video_start_held_by_its_probe_fails_after_the_hold_limit() {
    let dir = tempfile::tempdir().unwrap();
    let a = movie!(dir.path(), "a.mov", (50, 25));
    let b = movie!(dir.path(), "b.mov", (50, 25));
    let mut s = gated_video_show(vec![video_cue(1, Some(&a))]);
    GATED.lock().unwrap().push(b.clone());
    set_media(&mut s, 1, &b);
    let at = go(&mut s);
    run_to(&mut s, at + samples(4.9));
    expect_state(&s, 1, CueState::Running, "");
    s.take_outbound();
    run_to(&mut s, at + samples(5.1));
    expect_state(&s, 1, CueState::Broken, "");
    assert!(notices(&mut s).iter().any(|c| c == "probe_timeout"));
    release_probe(&b);
    s.settle_media();
    assert_eq!(
        s.control().pictures_live(),
        0,
        "nothing put up after the limit"
    );
}

/// A start waiting for its probe can be paused (the answer then loads the picture without
/// showing it, and resume plays it) or stopped (nothing goes up when the answer comes).
#[test]
fn a_video_start_waiting_for_its_probe_can_be_paused_resumed_and_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let a = movie!(dir.path(), "a.mov", (50, 25));
    let b = movie!(dir.path(), "b.mov", (50, 25));
    let c = movie!(dir.path(), "c.mov", (50, 25));
    let mut s = gated_video_show(vec![video_cue(1, Some(&a)), video_cue(2, Some(&a))]);
    GATED.lock().unwrap().extend([b.clone(), c.clone()]);
    set_media(&mut s, 1, &b);
    set_media(&mut s, 2, &c);
    start(&mut s, 1);
    start(&mut s, 2);
    s.run(BLOCK);
    s.transport(TransportAction::Pause { cue: id(1) });
    s.transport(TransportAction::Stop {
        cue: id(2),
        fade_out: None,
    });
    s.run(BLOCK);
    expect_state(&s, 1, CueState::Paused, "");
    expect_state(&s, 2, CueState::Idle, "");
    release_probe(&b);
    release_probe(&c);
    s.settle_media();
    s.run(samples(0.5));
    assert_eq!(
        s.control().pictures_live(),
        1,
        "only the paused cue's picture"
    );
    assert_eq!(shown(&mut s), None, "paused before it ever played");
    s.transport(TransportAction::Resume { cue: id(1) });
    let resumed = s.now();
    run_to(&mut s, resumed + samples(0.5));
    let now = on_stage(&mut s, "plays on resume");
    assert!(now.abs_diff(frame_at(0.5)) <= 1, "{now}");
}

/// Relink finds a Video cue's moved file like an Audio cue's: the cue points at it, the file
/// is probed, the missing mark goes, and GO shows the picture.
#[test]
fn relink_finds_a_video_cues_moved_file_and_it_plays() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let moved = movie!(&lib, "clip.mov", (50, 25));
    let gone = dir.path().join("gone").join("clip.mov");
    let mut s = video_show(vec![video_cue(1, Some(&gone))]);
    assert!(matches!(
        problems(&s, 1).as_slice(),
        [CueProblem::MediaMissing { .. }]
    ));
    let id_ = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id: id_,
        request: Request::Relink {
            folder: lib.to_str().unwrap().into(),
            preview: false,
        },
    });
    let relinked = replies.iter().find_map(|m| match m {
        ServerMessage::Result {
            outcome: Outcome::Ok(body),
            ..
        } => match &body.data {
            Some(ResultData::Relink(report)) => Some(report.relinked),
            _ => None,
        },
        _ => None,
    });
    assert_eq!(relinked, Some(1), "{replies:?}");
    let media = openlab_core::media_of_cues(s.control().workspace());
    assert_eq!(
        std::fs::canonicalize(media[0].1.as_str()).unwrap(),
        std::fs::canonicalize(&moved).unwrap()
    );
    s.settle_media();
    assert_eq!(problems(&s, 1), vec![]);
    let at = go(&mut s);
    run_to(&mut s, at + samples(0.5));
    let now = on_stage(&mut s, "the relinked picture plays");
    assert!(now.abs_diff(frame_at(0.5)) <= 1, "{now}");
}

/// A paused picture comes back, on its held frame, when the stage host is relaunched.
#[test]
fn a_paused_picture_is_shown_again_when_the_stage_host_is_relaunched() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (250, 25));
    let mut s = video_show(vec![video_cue(1, Some(&movie))]);
    let at = go(&mut s);
    run_to(&mut s, at + samples(0.5));
    s.transport(TransportAction::Pause { cue: id(1) });
    s.run(BLOCK);
    let held = on_stage(&mut s, "a frame while paused");
    let pid = s.control().stage_host_pid().unwrap();
    let killed = Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    assert!(s.wait_for(|c| c.stage_host_pid().is_some_and(|p| p != pid)));
    s.settle_stages();
    s.run(samples(1.0));
    expect_state(&s, 1, CueState::Paused, "");
    assert_eq!(shown(&mut s), Some(held), "the held frame is back");
}

/// The stage host's media limits measured on a clock the test moves (`advance_stage_media_clock`,
/// and each slow-decode frame), never on real time: a picture fails only when the test moves
/// that clock past a limit, however slow the machine. The limits given apply to the files a
/// fault switch names; a good file keeps the shipped ones.
const HAND_LIMITS: [&str; 3] = ["--media-limits", "400,400", "--media-clock-by-hand"];

/// A copy of a good movie under a name that makes the test-support decoder hang or crash.
fn faulty(dir: &Path, good: &Path, fault: &str) -> PathBuf {
    let path = dir.join(format!("{fault}.mov"));
    std::fs::copy(good, &path).unwrap();
    path
}

/// Waits (bounded, ticking) until cue `n` is broken.
fn wait_broken(s: &mut OfflineShow, n: u128) -> bool {
    s.wait_for(|c| c.engine().state(id(n)) == CueState::Broken)
}

/// A picture whose file's open hangs (a stalled volume) holds up nothing else: the stage host
/// keeps answering (it is not killed and relaunched), its keep-black stage stays black, another
/// picture plays on it, and only the hung cue breaks, with the reason.
#[test]
fn a_picture_whose_open_hangs_fails_alone_and_the_stage_host_answers() {
    let dir = tempfile::tempdir().unwrap();
    let good = movie!(dir.path(), "good.mov", (250, 25));
    let hung = faulty(dir.path(), &good, "openlab-test-hang-open");
    let ws = workspace(vec![video_cue(1, Some(&good)), video_cue(2, Some(&hung))]);
    let mut s = video_show_args(ws, &HAND_LIMITS);
    let pid = s.control().stage_host_pid().unwrap();
    s.take_outbound();
    start(&mut s, 2);
    s.run(BLOCK);
    let (frame, colours) = probe_at(&mut s, &[[0.5, 0.5]]);
    assert_eq!(frame, None, "nothing drawn for the hung picture");
    assert_eq!(
        colours,
        vec![[0, 0, 0, 255]],
        "the keep-black stage is black"
    );
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(0.5));
    let now = on_stage(&mut s, "the other picture plays");
    assert!(now.abs_diff(frame_at(0.5)) <= 1, "{now}");
    assert_eq!(
        state(&s, 2),
        CueState::Running,
        "not failed before its limit"
    );
    // Past the open's limit.
    s.control().advance_stage_media_clock(500);
    assert!(wait_broken(&mut s, 2), "the hung cue breaks");
    assert_eq!(
        s.control().engine().broken_reason(id(2)),
        Some(openlab_core::BrokenReason::DecodeFailed)
    );
    let texts = notice_texts(&mut s);
    assert!(
        texts
            .iter()
            .any(|(c, t)| c == "video_decode_failed" && t.contains("to open")),
        "{texts:?}"
    );
    expect_state(&s, 1, CueState::Running, "the other cue plays on");
    assert_eq!(
        s.control().stage_host_pid(),
        Some(pid),
        "never killed as hung"
    );
}

/// A picture whose frame decode hangs fails alone after the frame limit; the stage host keeps
/// running and answering.
#[test]
fn a_picture_whose_decode_hangs_fails_alone() {
    let dir = tempfile::tempdir().unwrap();
    let good = movie!(dir.path(), "good.mov", (250, 25));
    let hung = faulty(dir.path(), &good, "openlab-test-hang-decode");
    let ws = workspace(vec![video_cue(1, Some(&good)), video_cue(2, Some(&hung))]);
    let mut s = video_show_args(ws, &HAND_LIMITS);
    let pid = s.control().stage_host_pid().unwrap();
    // The hung one first, so the good one is drawn over it.
    start(&mut s, 2);
    let at = start(&mut s, 1);
    // Both open (the hung one hangs only at the frames after its open).
    assert!(s.wait_for(|c| c.pictures_opened() == 2));
    s.take_outbound();
    run_to(&mut s, at + samples(0.5));
    // Drawing asks the hung decoder for a frame.
    let now = on_stage(&mut s, "the good picture is drawn");
    assert!(now.abs_diff(frame_at(0.5)) <= 1, "{now}");
    assert_eq!(
        state(&s, 2),
        CueState::Running,
        "not failed before its limit"
    );
    // Past the frame limit with no frame read.
    s.control().advance_stage_media_clock(500);
    assert!(wait_broken(&mut s, 2), "the hung cue breaks");
    let texts = notice_texts(&mut s);
    assert!(
        texts
            .iter()
            .any(|(c, t)| c == "video_decode_failed" && t.contains("to decode")),
        "{texts:?}"
    );
    expect_state(&s, 1, CueState::Running, "");
    assert_eq!(s.control().stage_host_pid(), Some(pid));
}

/// Draws stage 1 (a probe) and, if that stopped the stage host, waits for the next one; the
/// stage host pids seen are kept in `pids`.
fn probe_through_crashes(s: &mut OfflineShow, pids: &mut Vec<u32>) {
    let id_ = s.request_id();
    s.send_until(
        ClientMessage::Request {
            id: id_,
            request: Request::StageProbe {
                stage: stage_id(),
                points: vec![[0.5, 0.5]],
            },
        },
        |c| c.stages_settled(),
    );
    s.settle_stages();
    if let Some(pid) = s.control().stage_host_pid()
        && pids.last() != Some(&pid)
    {
        pids.push(pid);
    }
}

/// Plays on through stage host crashes, drawing the stage every quarter second (so pictures
/// are decoded), until cue `n` is broken; the stage host pids seen are in `pids`.
fn play_until_broken(s: &mut OfflineShow, n: u128, pids: &mut Vec<u32>) -> bool {
    for _ in 0..120 {
        if state(s, n) == CueState::Broken {
            return true;
        }
        probe_through_crashes(s, pids);
        s.run(samples(0.25));
        s.wait_for(|c| c.stages_settled());
    }
    state(s, n) == CueState::Broken
}

/// After a run of crashes: the stage host stays up for `seconds` more of playing, drawing
/// every quarter second.
fn stays_up(s: &mut OfflineShow, seconds: f64) {
    let pid = s.control().stage_host_pid();
    let until = s.now() + samples(seconds);
    while s.now() < until {
        shown(s);
        s.run(samples(0.25));
    }
    assert_eq!(
        s.control().stage_host_pid(),
        pid,
        "the stage host crashed again"
    );
}

/// A file that crashes the stage host as it opens (its first frame decoded) while a good movie
/// plays is isolated: the two are held back and tried one at a time, so only the bad file's cue
/// breaks, and the good movie comes back in step and plays on.
#[test]
fn a_picture_that_crashes_the_stage_host_as_it_opens_breaks_only_its_cue() {
    let dir = tempfile::tempdir().unwrap();
    let good = movie!(dir.path(), "good.mov", (500, 25));
    let bomb = faulty(dir.path(), &good, "openlab-test-crash-open");
    let ws = workspace(vec![video_cue(1, Some(&good)), video_cue(2, Some(&bomb))]);
    let mut s = video_show_of(ws);
    let mut pids = vec![s.control().stage_host_pid().unwrap()];
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(0.2));
    on_stage(&mut s, "cue 1 shown");
    start(&mut s, 2);
    assert!(play_until_broken(&mut s, 2, &mut pids), "{pids:?}");
    stays_up(&mut s, 6.0);
    let played = (s.now() - at) as f64 / f64::from(RATE);
    let now = on_stage(&mut s, "the good picture is back");
    assert!(now.abs_diff(frame_at(played)) <= 1, "{now} at {played}");
    expect_state(&s, 1, CueState::Running, "");
}

/// A movie that crashes the stage host decoding a frame after its open, while a good movie
/// plays: both are suspects, so after a second crash they are tried one at a time, and only
/// the crashing one's cue breaks.
#[test]
fn of_two_movies_playing_only_the_one_crashing_the_stage_host_breaks() {
    let dir = tempfile::tempdir().unwrap();
    let good = movie!(dir.path(), "good.mov", (500, 25));
    let bomb = faulty(dir.path(), &good, "openlab-test-crash-decode");
    let ws = workspace(vec![video_cue(1, Some(&good)), video_cue(2, Some(&bomb))]);
    let mut s = video_show_of(ws);
    let mut pids = vec![s.control().stage_host_pid().unwrap()];
    // The bomb first, so the good movie is drawn on top and its frame read.
    start(&mut s, 2);
    let at = start(&mut s, 1);
    s.wait_for(|c| c.pictures_opened() == 2);
    assert!(play_until_broken(&mut s, 2, &mut pids), "{pids:?}");
    stays_up(&mut s, 6.0);
    let played = (s.now() - at) as f64 / f64::from(RATE);
    let now = on_stage(&mut s, "the good picture is back");
    assert!(now.abs_diff(frame_at(played)) <= 1, "{now} at {played}");
    expect_state(&s, 1, CueState::Running, "");
}

/// A stage host killed from outside, or by the supervisor for not answering, has not crashed:
/// each time, every picture (a movie and a still) comes back on the next one. At the restart
/// limit (5 starts within a minute of the supervisor's clock) video output pauses, the cues
/// running on, and after the retry time it starts again with the pictures shown.
#[test]
fn pictures_come_back_after_kills_and_after_the_pause_at_the_restart_limit() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (1500, 25));
    let png = make(dir.path(), "card.png", "png", None);
    let ws = workspace(vec![still_cue(1, &png, None), video_cue(2, Some(&movie))]);
    let hand = HandClock::new();
    let mut s = video_show_host(ws, &[], |host| {
        hand.drive(host);
        host.answer_within = Duration::from_secs(5);
        host.max_restarts = 5;
        host.restart_window = Duration::from_secs(60);
        host.retry_after = Duration::from_secs(30);
    });
    start(&mut s, 1);
    let at = start(&mut s, 2);
    s.run(BLOCK);
    let back = |s: &mut OfflineShow, round: &str| {
        s.settle_stages();
        s.run(samples(0.5));
        expect_state(s, 1, CueState::Running, "{round}");
        expect_state(s, 2, CueState::Running, "{round}");
        let played = (s.now() - at) as f64 / f64::from(RATE);
        let now = on_stage(s, "the movie is back");
        assert!(
            now.abs_diff(frame_at(played)) <= 1,
            "{round}: {now} at {played}"
        );
    };
    let kill = |s: &mut OfflineShow| {
        let pid = s.control().stage_host_pid().unwrap();
        let killed = Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()
            .unwrap();
        assert!(killed.success());
        pid
    };
    // Four stops: two kills from outside, two hangs the supervisor kills.
    for round in 0..4 {
        let pid = if round % 2 == 0 {
            kill(&mut s)
        } else {
            hand.hang_until_killed(&mut s, 5)
        };
        assert!(s.wait_for(|c| c.stage_host_pid().is_some_and(|p| p != pid)));
        back(&mut s, &format!("round {round}"));
    }
    // A fifth stop within the minute: video output pauses; the cues run on.
    let pid = kill(&mut s);
    assert!(s.wait_for(|c| c.stage_host_pid().is_none_or(|p| p != pid)));
    s.run(samples(0.5));
    assert_eq!(s.control().stage_host_pid(), None, "paused, not restarted");
    expect_state(&s, 2, CueState::Running, "");
    assert_eq!(s.control().pictures_live(), 2);
    // After the retry time it starts again, with the pictures.
    hand.advance(&mut s, 31);
    assert!(s.wait_for(|c| c.stage_host_pid().is_some()));
    back(&mut s, "after the pause");
    assert_eq!(s.control().pictures_live(), 2);
}

/// A movie that crashes the stage host decoding a frame after its open, playing alone, is
/// sent again once; at a second crash it is the only picture that was decoding both times, so
/// its cue breaks; video output carries on: the next stage host stays up and the next GO
/// plays.
#[test]
fn a_picture_that_crashes_the_stage_host_twice_is_not_sent_a_third_time() {
    let dir = tempfile::tempdir().unwrap();
    let good = movie!(dir.path(), "good.mov", (250, 25));
    let bomb = faulty(dir.path(), &good, "openlab-test-crash-decode");
    let mut s = video_show(vec![video_cue(1, Some(&bomb)), video_cue(2, Some(&good))]);
    let mut pids = vec![s.control().stage_host_pid().unwrap()];
    start(&mut s, 1);
    s.run(BLOCK);
    for _ in 0..4 {
        if state(&s, 1) == CueState::Broken {
            break;
        }
        // Open on this stage host (its first frame decoded), so only a frame after it crashes.
        s.wait_for(|c| c.pictures_opened() == 1);
        probe_through_crashes(&mut s, &mut pids);
        s.run(BLOCK);
    }
    expect_state(&s, 1, CueState::Broken, "{pids:?}");
    assert_eq!(
        pids.len(),
        3,
        "two crashes, then a stage host that stays: {pids:?}"
    );
    s.take_outbound();
    let at = start(&mut s, 2);
    // Its file open on the stage host before it is drawn (not the probe's own wait for it).
    assert!(
        s.wait_for(|c| c.pictures_opened() == 1),
        "the next cue's file never opened"
    );
    run_to(&mut s, at + samples(0.5));
    let shown_now = shown(&mut s);
    if shown_now.is_none() {
        // Say what the engine saw, so a failure on a CI runner can be read from its log.
        let logs: Vec<String> = s
            .take_outbound()
            .iter()
            .filter_map(|m| match m {
                ServerMessage::Log { message, .. } => Some(message.clone()),
                ServerMessage::LogEntry { entry, .. } => Some(entry.message.clone()),
                _ => None,
            })
            .collect();
        panic!(
            "the next cue shows nothing: stage hosts {pids:?}, pictures up {}, opened {}, \
             cue 2 {:?}, log {logs:?}",
            s.control().pictures_live(),
            s.control().pictures_opened(),
            state(&s, 2)
        );
    }
    let now = shown_now.expect("the next cue plays");
    assert!(now.abs_diff(frame_at(0.5)) <= 1, "{now}");
    assert_eq!(s.control().stage_host_pid(), pids.last().copied());
}

/// A still with more pixels than a picture may have (its file swapped after the probe passed)
/// is refused by the stage host before it allocates the picture: only its cue breaks, with
/// the reason.
#[test]
fn a_still_too_large_to_hold_is_refused_by_the_stage_host() {
    let dir = tempfile::tempdir().unwrap();
    let png = make(dir.path(), "card.png", "png", None);
    let huge = make(dir.path(), "huge.png", "huge-png", None);
    let mut s = video_show(vec![still_cue(1, &png, None)]);
    let pid = s.control().stage_host_pid().unwrap();
    std::fs::rename(&huge, &png).unwrap();
    s.take_outbound();
    start(&mut s, 1);
    s.run(BLOCK);
    shown(&mut s);
    assert!(wait_broken(&mut s, 1));
    let texts = notice_texts(&mut s);
    assert!(
        texts
            .iter()
            .any(|(c, t)| c == "video_decode_failed" && t.contains("16384 x 8192")),
        "{texts:?}"
    );
    assert_eq!(s.control().stage_host_pid(), Some(pid));
}

/// Cue `n`'s last progress item since the last call: (elapsed, remaining, position).
fn progress_of(s: &mut OfflineShow, n: u128) -> Option<(f64, Option<f64>, Option<f64>)> {
    s.take_outbound()
        .into_iter()
        .filter_map(|m| match m {
            ServerMessage::Progress { items, .. } => items.into_iter().find(|i| i.cue == id(n)),
            _ => None,
        })
        .next_back()
        .map(|i| (i.elapsed, i.remaining, i.position))
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() <= 0.05
}

/// Runs to `until` and returns cue `n`'s last progress item, read from the last tenth of a
/// second only (progress is dropped for a client that does not keep up).
fn progress_at(
    s: &mut OfflineShow,
    n: u128,
    until: u64,
) -> Option<(f64, Option<f64>, Option<f64>)> {
    run_to(s, until.saturating_sub(samples(0.1)));
    s.take_outbound();
    run_to(s, until);
    progress_of(s, n)
}

/// Clients are told a running Video cue's progress, as an Audio cue's: elapsed on the engine
/// clock from its start with the time left of its movie, held while paused, on from there when
/// resumed, from the load-to-time point when loaded there; a still with no duration counts up
/// with no end.
#[test]
fn clients_are_told_a_video_cues_progress_playing_paused_resumed_and_loaded_to_time() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (100, 25));
    let png = make(dir.path(), "card.png", "png", None);
    let mut s = video_show(vec![video_cue(1, Some(&movie)), still_cue(2, &png, None)]);
    let at = start(&mut s, 1);
    let (elapsed, remaining, position) =
        progress_at(&mut s, 1, at + samples(1.0)).expect("playing");
    assert!(near(elapsed, 1.0), "{elapsed}");
    assert!(near(remaining.unwrap(), 3.0), "{remaining:?}");
    assert!(near(position.unwrap(), elapsed));
    s.transport(TransportAction::Pause { cue: id(1) });
    let now = s.now();
    let paused = progress_at(&mut s, 1, now + BLOCK).expect("paused").0;
    let (still_paused, _, _) =
        progress_at(&mut s, 1, now + samples(1.0)).expect("still told while paused");
    assert!(
        (still_paused - paused).abs() < 1e-9,
        "{still_paused} {paused}"
    );
    s.transport(TransportAction::Resume { cue: id(1) });
    let resumed = s.now();
    let (on, left, _) = progress_at(&mut s, 1, resumed + samples(0.5)).expect("resumed");
    assert!(near(on, paused + 0.5), "{on} after {paused}");
    assert!(near(left.unwrap(), 4.0 - on));
    // Load to time, then GO: counts from there.
    s.transport(TransportAction::Stop {
        cue: id(1),
        fade_out: None,
    });
    s.run(BLOCK);
    s.transport(TransportAction::Load {
        cue: id(1),
        at: 2.5,
    });
    s.run(BLOCK);
    let at = start(&mut s, 1);
    let (from_there, left, _) = progress_at(&mut s, 1, at + samples(0.5)).expect("loaded to time");
    assert!(near(from_there, 3.0), "{from_there}");
    assert!(near(left.unwrap(), 1.0));
    // A still with no duration: elapsed with no end and no file position.
    let at = start(&mut s, 2);
    let (shown_for, end, position) = progress_at(&mut s, 2, at + samples(0.5)).expect("the still");
    assert!(near(shown_for, 0.5), "{shown_for}");
    assert_eq!((end, position), (None, None));
}

/// A decoder slower than the frame limit allows for a whole catch-up, but making progress
/// frame by frame, keeps its picture up (the limit times a decoder that stopped, not one that
/// is slow): the picture shows the frames it gets to and its cue runs on. One that stops
/// decoding still fails alone (`a_picture_whose_decode_hangs_fails_alone`).
#[test]
fn a_slow_decoder_that_keeps_decoding_keeps_its_picture_up() {
    let dir = tempfile::tempdir().unwrap();
    let good = movie!(dir.path(), "good.mov", (250, 25));
    let slow = faulty(dir.path(), &good, "openlab-test-slow-decode");
    let ws = workspace(vec![video_cue(1, Some(&slow))]);
    // Each slow frame moves the media clock on 400 ms, a catch-up of a third of a second
    // (8 frames) 3.2 s, past the 2.5 s limit in all; no frame takes longer than 400 ms.
    let mut s = video_show_args(
        ws,
        &["--media-limits", "2500,2500", "--media-clock-by-hand"],
    );
    let at = start(&mut s, 1);
    s.take_outbound();
    for step in 1..=4 {
        let played = f64::from(step) * 0.33;
        run_to(&mut s, at + samples(played));
        let now = on_stage(&mut s, "the slow picture is drawn");
        assert!(now.abs_diff(frame_at(played)) <= 1, "{now} at {played}");
        expect_state(&s, 1, CueState::Running, "");
    }
    assert!(
        !notices(&mut s).iter().any(|c| c == "video_decode_failed"),
        "the slow picture was failed"
    );
    assert_eq!(s.control().pictures_live(), 1);
}

// Geometry (VID-04; QLab 5 manual pp. 212-214). The stage is 1920 x 1080; the test PNGs are
// made by `--make-test-media` (`wide-png`, `tall-png`, `alpha-png`, `tiny-png`).

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const YELLOW: [u8; 4] = [255, 255, 0, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];

/// A still shown until stopped, with `geometry`.
fn placed_still(n: u128, file: &Path, geometry: VideoGeometry) -> Cue {
    let mut c = still_cue(n, file, None);
    if let CueKind::Video { geometry: g, .. } = &mut c.kind {
        *g = geometry;
    }
    c
}

/// Sets geometry fields of cue `n` as the app does (`kind_fields`), and lets the edit through;
/// the replies.
fn set_geometry(s: &mut OfflineShow, n: u128, fields: serde_json::Value) -> Vec<ServerMessage> {
    let request = s.request_id();
    let serde_json::Value::Object(value) = fields else {
        panic!("fields are an object");
    };
    let replies = s.send(ClientMessage::Command {
        id: request,
        command: Box::new(openlab_core::Command::SetCueProperty {
            id: id(n),
            property: openlab_core::CueProperty::KindFields { value },
        }),
        coalesce: None,
    });
    s.run(BLOCK);
    replies
}

fn colours(s: &mut OfflineShow, points: &[[f32; 2]]) -> Vec<[u8; 4]> {
    probe_at(s, points).1
}

fn style(name: &str) -> serde_json::Value {
    serde_json::json!({ "fill_style": name })
}

fn stretched(layer: u16) -> VideoGeometry {
    VideoGeometry {
        fill_style: openlab_core::FillStyle::Stretch,
        layer,
        ..VideoGeometry::default()
    }
}

/// Fill stage: fit shows the whole picture as large as the stage allows with its shape kept
/// (the stage's black beside it); fill covers the stage and cuts the overflow; stretch takes
/// the stage's shape. For a wide and a tall picture.
#[test]
fn geometry_tab_fill_stage_fit_fill_and_stretch() {
    let dir = tempfile::tempdir().unwrap();
    let wide = make(dir.path(), "wide.png", "wide-png", None);
    let tall = make(dir.path(), "tall.png", "tall-png", None);
    let mut s = video_show(vec![
        placed_still(1, &wide, VideoGeometry::default()),
        placed_still(2, &tall, VideoGeometry::default()),
    ]);
    // Wide (4:1, bands left to right) on a 16:9 stage. Fit: 1920 x 480, rows 300 to 780.
    start(&mut s, 1);
    s.run(samples(0.3));
    let pts = [[0.1, 0.1], [0.1, 0.5], [0.3, 0.5], [0.9, 0.5]];
    assert_eq!(colours(&mut s, &pts), vec![BLACK, RED, GREEN, YELLOW]);
    // Fill: 4320 wide from x -1200: x 0.1 falls in the second band.
    set_geometry(&mut s, 1, style("fill"));
    assert_eq!(colours(&mut s, &pts), vec![GREEN, GREEN, GREEN, BLUE]);
    // Stretch: the whole picture over the whole stage.
    set_geometry(&mut s, 1, style("stretch"));
    assert_eq!(colours(&mut s, &pts), vec![RED, RED, GREEN, YELLOW]);
    s.transport(TransportAction::Stop {
        cue: id(1),
        fade_out: None,
    });
    // Tall (1:4, bands top to bottom). Fit: 270 x 1080 in the middle.
    start(&mut s, 2);
    s.run(samples(0.3));
    let pts = [[0.1, 0.6], [0.5, 0.1], [0.5, 0.3], [0.5, 0.9]];
    assert_eq!(colours(&mut s, &pts), vec![BLACK, RED, GREEN, YELLOW]);
    // Fill: 7680 high from y -3300: y 0.1 falls in the second band.
    set_geometry(&mut s, 2, style("fill"));
    assert_eq!(colours(&mut s, &pts), vec![BLUE, GREEN, GREEN, BLUE]);
    set_geometry(&mut s, 2, style("stretch"));
    assert_eq!(colours(&mut s, &pts), vec![BLUE, RED, GREEN, YELLOW]);
}

/// Custom: the picture at its own pixel size in the middle of the stage; moved, scaled about
/// its anchor, rotated, cropped.
#[test]
fn geometry_tab_custom_mode_with_position_scale_rotation_crop_and_anchor() {
    let dir = tempfile::tempdir().unwrap();
    // 400 x 100, bands of 100 px (red, green, blue, yellow): on the stage x 760 to 1160, y 490
    // to 590.
    let wide = make(dir.path(), "wide.png", "wide-png", None);
    let custom = VideoGeometry {
        fill_stage: false,
        ..VideoGeometry::default()
    };
    let mut s = video_show(vec![placed_still(1, &wide, custom)]);
    go(&mut s);
    s.run(samples(0.3));
    let x = |px: f32| px / 1920.0;
    let y = |px: f32| px / 1080.0;
    let row = |py: f32| [[x(700.0), y(py)], [x(810.0), y(py)], [x(1110.0), y(py)]];
    assert_eq!(colours(&mut s, &row(540.0)), vec![BLACK, RED, YELLOW]);
    assert_eq!(colours(&mut s, &[[0.5, y(470.0)]]), vec![BLACK]);
    // Moved 500 px right and 200 up.
    set_geometry(
        &mut s,
        1,
        serde_json::json!({"translation": {"x": 500, "y": 200}}),
    );
    assert_eq!(
        colours(&mut s, &[[x(810.0), y(540.0)], [x(1310.0), y(340.0)]]),
        vec![BLACK, RED]
    );
    // Scaled 2 x about the middle: 800 x 200, x 560 to 1360, y 440 to 640.
    set_geometry(
        &mut s,
        1,
        serde_json::json!({"translation": {"x": 0, "y": 0}, "scale": {"x": 2, "y": 2}}),
    );
    assert_eq!(
        colours(&mut s, &[[x(600.0), y(460.0)], [x(1340.0), y(620.0)]]),
        vec![RED, YELLOW]
    );
    // Half a turn: yellow now on the left.
    set_geometry(
        &mut s,
        1,
        serde_json::json!({"scale": {"x": 1, "y": 1}, "rotation": 180}),
    );
    assert_eq!(colours(&mut s, &row(540.0)), vec![BLACK, YELLOW, RED]);
    // A quarter turn about the middle of its left end: it stands up from there.
    set_geometry(
        &mut s,
        1,
        serde_json::json!({"rotation": 90, "anchor": {"x": -200, "y": 0}}),
    );
    assert_eq!(
        colours(
            &mut s,
            &[
                [x(760.0), y(530.0)],
                [x(760.0), y(200.0)],
                [x(1000.0), y(540.0)]
            ]
        ),
        vec![RED, YELLOW, BLACK]
    );
    // 100 px cropped off the left: the red band goes, the rest stays where it was.
    set_geometry(
        &mut s,
        1,
        serde_json::json!({"rotation": 0, "anchor": {"x": 0, "y": 0},
            "crop": {"top": 0, "bottom": 0, "left": 100, "right": 0}}),
    );
    assert_eq!(
        colours(&mut s, &[[x(810.0), y(540.0)], [x(900.0), y(540.0)]]),
        vec![BLACK, GREEN]
    );
}

/// A value changed while the cue plays reaches the stage at once, and the picture runs on
/// from where it was: it is not restarted.
#[test]
fn geometry_value_changed_while_the_cue_plays_follows_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let a = movie!(dir.path(), "a.mov", (100, 25));
    let mut s = video_show(vec![video_cue(1, Some(&a))]);
    let at = go(&mut s);
    run_to(&mut s, at + samples(1.0));
    on_stage(&mut s, "playing");
    // Moved off the stage: nothing there.
    set_geometry(
        &mut s,
        1,
        serde_json::json!({"fill_stage": false, "translation": {"x": 5000, "y": 0}}),
    );
    assert_eq!(shown(&mut s), None, "moved off the stage at once");
    expect_state(&s, 1, CueState::Running, "still playing");
    // Back to filling the stage: the frame due now, not the first.
    run_to(&mut s, at + samples(2.0));
    set_geometry(&mut s, 1, serde_json::json!({"fill_stage": true}));
    let played = (s.now() - at) as f64 / f64::from(RATE);
    let frame = on_stage(&mut s, "back on the stage");
    assert!(
        frame.abs_diff(frame_at(played)) <= 1,
        "{frame}: the frame at {played} s, not restarted"
    );
}

/// Opacity: at 100 % a picture's transparent pixels show what is below; at 50 % its opaque
/// pixels are mixed half over it; at 0 % it is not seen.
#[test]
fn geometry_opacity_blends_and_keeps_transparent_pixels_transparent() {
    let dir = tempfile::tempdir().unwrap();
    let card = make(dir.path(), "card.png", "png", None);
    let alpha = make(dir.path(), "alpha.png", "alpha-png", None);
    let mut s = video_show(vec![
        placed_still(1, &card, stretched(1)),
        placed_still(2, &alpha, stretched(2)),
    ]);
    start(&mut s, 1);
    start(&mut s, 2);
    s.run(samples(0.3));
    // Bottom left: the red half over the card's blue; bottom right: transparent, the card's
    // yellow.
    let pts = [[0.25, 0.8], [0.75, 0.8]];
    assert_eq!(colours(&mut s, &pts), vec![RED, YELLOW]);
    set_geometry(&mut s, 2, serde_json::json!({"opacity": 0.5}));
    let read = colours(&mut s, &pts);
    let [r, g, b, _] = read[0];
    assert!(
        (120..=135).contains(&r) && g < 5 && (120..=135).contains(&b),
        "half red over blue: {read:?}"
    );
    assert_eq!(read[1], YELLOW);
    set_geometry(&mut s, 2, serde_json::json!({"opacity": 0}));
    assert_eq!(colours(&mut s, &pts), vec![BLUE, YELLOW]);
}

/// Layer: the higher layer is on top whatever the start order; on one layer the later-started
/// is on top (QLab 5 manual p. 213).
#[test]
fn geometry_layer_higher_on_top_and_on_one_layer_the_later_started() {
    let dir = tempfile::tempdir().unwrap();
    let card = make(dir.path(), "card.png", "png", None);
    let wide = make(dir.path(), "wide.png", "wide-png", None);
    // At (0.3, 0.5) the card is red, the wide picture green.
    let point = [[0.3, 0.5]];
    let mut s = video_show(vec![
        placed_still(1, &card, stretched(10)),
        placed_still(2, &wide, stretched(5)),
    ]);
    start(&mut s, 1);
    s.run(samples(0.2));
    start(&mut s, 2);
    s.run(samples(0.2));
    assert_eq!(
        colours(&mut s, &point),
        vec![RED],
        "layer 10 over layer 5 started later"
    );
    set_geometry(&mut s, 2, serde_json::json!({"layer": 10}));
    assert_eq!(
        colours(&mut s, &point),
        vec![GREEN],
        "same layer: the later-started"
    );
    set_geometry(&mut s, 1, serde_json::json!({"layer": 11}));
    assert_eq!(colours(&mut s, &point), vec![RED], "raised while playing");
}

/// The smooth box off: a picture scaled up keeps hard pixel edges; on, its edges blend.
#[test]
fn geometry_smooth_off_keeps_hard_pixel_edges() {
    let dir = tempfile::tempdir().unwrap();
    let tiny = make(dir.path(), "tiny.png", "tiny-png", None);
    let hard = VideoGeometry {
        smooth: false,
        ..stretched(1)
    };
    let mut s = video_show(vec![placed_still(1, &tiny, hard)]);
    go(&mut s);
    s.run(samples(0.3));
    let pts = [[0.45, 0.5], [0.55, 0.5]];
    assert_eq!(colours(&mut s, &pts), vec![RED, BLUE]);
    set_geometry(&mut s, 1, serde_json::json!({"smooth": true}));
    let read = colours(&mut s, &pts);
    assert!(read[0][2] > 0 && read[1][0] > 0, "blended: {read:?}");
}

/// A geometry value out of range sent by a client is refused, the workspace keeps what it
/// had and the stage what it showed.
#[test]
fn geometry_values_outside_their_ranges_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let card = make(dir.path(), "card.png", "png", None);
    let mut s = video_show(vec![still_cue(1, &card, None)]);
    go(&mut s);
    s.run(samples(0.3));
    let before = s.control().workspace().clone();
    for fields in [
        serde_json::json!({"opacity": 1.5}),
        serde_json::json!({"layer": 1001}),
        serde_json::json!({"crop": {"top": -1, "bottom": 0, "left": 0, "right": 0}}),
        serde_json::json!({"rotation": 400}),
    ] {
        let replies = set_geometry(&mut s, 1, fields.clone());
        assert!(
            replies.iter().any(|m| matches!(
                m,
                ServerMessage::Result {
                    outcome: Outcome::Error(e),
                    ..
                } if e.code == openlab_core::protocol::ErrorCode::CommandRefused
            )),
            "{fields} refused: {replies:?}"
        );
    }
    assert_eq!(s.control().workspace(), &before);
    assert_eq!(colours(&mut s, &[[0.25, 0.3]]), vec![RED]);
}

/// A geometry edited while the picture is up is kept when the stage host is relaunched: the
/// picture comes back placed as last edited, not as it was loaded.
#[test]
fn geometry_edited_while_up_survives_a_stage_host_relaunch() {
    let dir = tempfile::tempdir().unwrap();
    let wide = make(dir.path(), "wide.png", "wide-png", None);
    let mut s = video_show(vec![placed_still(1, &wide, stretched(1))]);
    go(&mut s);
    s.run(samples(0.3));
    let pts = [[0.1, 0.5], [0.9, 0.5]];
    assert_eq!(colours(&mut s, &pts), vec![RED, YELLOW]);
    set_geometry(
        &mut s,
        1,
        serde_json::json!({"rotation": 180, "fill_stage": false}),
    );
    let pid = s.control().stage_host_pid().unwrap();
    let killed = Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    assert!(s.wait_for(|c| c.stage_host_pid().is_some_and(|p| p != pid)));
    s.settle_stages();
    s.run(samples(0.5));
    let x = |px: f32| px / 1920.0;
    // Custom, half a turn: 400 x 100 in the middle, yellow on the left.
    assert_eq!(
        colours(&mut s, &[[x(810.0), 0.5], [x(1110.0), 0.5], [0.1, 0.5]]),
        vec![YELLOW, RED, BLACK]
    );
}

/// A movie with its own alpha (ProRes 4444; the card's right half at half alpha, straight, as
/// the file holds it) over the stage's black: the opaque half shows its colour, the half-alpha
/// half its colour at half strength (255 x 128 / 255 = 128, by hand): not at full strength
/// (alpha ignored) and not at a quarter (multiplied twice).
#[test]
fn geometry_a_movies_own_alpha_blends_once_over_what_is_below() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "alpha.mov", (25, 25), "prores4444");
    let mut s = video_show(vec![video_cue(1, Some(&movie))]);
    go(&mut s);
    s.run(samples(0.2));
    let read = colours(&mut s, &[[0.25, 0.3], [0.75, 0.3], [0.75, 0.8]]);
    let near = |c: [u8; 4], want: [u8; 3]| {
        c[..3]
            .iter()
            .zip(want)
            .all(|(got, want)| got.abs_diff(want) <= 6)
    };
    assert!(near(read[0], [255, 0, 0]), "opaque: {read:?}");
    assert!(
        near(read[1], [0, 128, 0]),
        "half green over black: {read:?}"
    );
    assert!(
        near(read[2], [128, 128, 0]),
        "half yellow over black: {read:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// Time and loops (VID-07; QLab 5 manual p. 214): the flows of issue #646. The movies are 25 fps
// test cards, so the barcode read back is the frame's number in the file.

/// A movie cue with its time and loops set by `edit`.
fn timed_cue(n: u128, movie: &Path, edit: impl Fn(&mut openlab_core::VideoTiming)) -> Cue {
    let mut c = video_cue(n, Some(movie));
    if let CueKind::Video { timing, .. } = &mut c.kind {
        edit(timing);
    }
    c
}

/// The lengths clients are told for cue `n` (the Duration column's): (trimmed pass, duration).
fn told_lengths(s: &OfflineShow, n: u128) -> (Option<f64>, Option<f64>) {
    let told = s.control().snapshot().media_durations;
    let m = told
        .iter()
        .find(|m| m.cue == id(n))
        .expect("the movie's lengths");
    (m.trimmed, m.duration)
}

/// The frames stage 1 shows from `from` to `to` seconds after `at`, every `step` seconds: a
/// stage that shows no frame at any of them (a black frame) fails the test there.
fn frames_between(s: &mut OfflineShow, at: u64, from: f64, to: f64, step: f64) -> Vec<u16> {
    let mut out = Vec::new();
    let mut t = from;
    while t <= to + 1e-9 {
        run_to(s, at + samples(t));
        out.push(on_stage(s, &format!("{t} s after GO")));
        t += step;
    }
    out
}

/// Start and end trims → GO plays only that range, and the Duration cell shows its length.
#[test]
fn start_and_end_trims_go_plays_only_that_range_and_the_duration_cell_shows_its_length() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (50, 25));
    let mut s = video_show(vec![timed_cue(1, &movie, |t| {
        t.start_time = 0.4;
        t.end_time = Some(1.2);
    })]);
    assert_eq!(told_lengths(&s, 1), (Some(0.8), Some(0.8)));
    let at = go(&mut s);
    run_to(&mut s, at + samples(0.02));
    assert_eq!(shown(&mut s), Some(10), "the start trim's frame first");
    run_to(&mut s, at + samples(0.5));
    assert_eq!(shown(&mut s), Some(frame_at(0.9)));
    run_to(&mut s, at + samples(0.78));
    assert_eq!(
        shown(&mut s),
        Some(frame_at(1.18)),
        "the last frame before the end trim"
    );
    expect_state(&s, 1, CueState::Running, "inside the range");
    run_to(&mut s, at + samples(0.8) + BLOCK);
    expect_state(&s, 1, CueState::Idle, "the range is played");
    assert_eq!(shown(&mut s), None);
    // An edit of the trims is told at once: the Duration cell follows.
    set_geometry(&mut s, 1, serde_json::json!({"end_time": 1.6}));
    assert_eq!(told_lengths(&s, 1), (Some(1.2), Some(1.2)));
}

/// A play count above one, and infinite loop → the range repeats without a black frame at the
/// seam; a Devamp cue ends the loop at the end of the pass.
#[test]
fn a_play_count_and_infinite_loop_repeat_the_range_without_a_black_frame_and_a_devamp_cue_ends_the_loop_at_the_end_of_the_pass()
 {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (50, 25));
    // Played three times: 0.4 to 1.2 s of the file, three passes, then the cue ends.
    let mut s = video_show(vec![timed_cue(1, &movie, |t| {
        t.start_time = 0.4;
        t.end_time = Some(1.2);
        t.last_slice_play_count = 3;
    })]);
    assert_eq!(told_lengths(&s, 1), (Some(0.8), Some(2.4)));
    let at = go(&mut s);
    // Across the first seam a frame is up at every look: the pass's last frames, then the
    // range's first again.
    let seam = frames_between(&mut s, at, 0.74, 0.90, 0.04);
    assert_eq!(seam, vec![28, 29, 10, 11, 12], "across the seam");
    run_to(&mut s, at + samples(1.6) + samples(0.02));
    assert_eq!(shown(&mut s), Some(10), "the third pass");
    run_to(&mut s, at + samples(2.4) - samples(0.05));
    expect_state(&s, 1, CueState::Running, "in the third pass");
    run_to(&mut s, at + samples(2.4) + BLOCK);
    expect_state(&s, 1, CueState::Idle, "three passes played");
    assert_eq!(shown(&mut s), None);

    // Infinite loop over 0.4 to 0.8 s, then 0.8 to 1.2 s once; a Devamp cue leaves the loop at
    // the end of the pass under way and the movie plays on into the next slice.
    let devamp = cue(
        2,
        CueKind::Devamp {
            target: Some(id(1)),
            start_next_cue: false,
            stop_target_when_slice_ends: false,
            devamp_mode: openlab_core::DevampMode::CurrentSlice,
        },
    );
    let looping = timed_cue(1, &movie, |t| {
        t.start_time = 0.4;
        t.end_time = Some(1.2);
        t.slices = vec![openlab_core::SliceMarker {
            time: 0.8,
            play_count: 0,
        }];
        t.last_slice_play_count = 1;
    });
    let mut s = video_show(vec![looping, devamp]);
    assert_eq!(told_lengths(&s, 1), (Some(0.8), None), "loops for ever");
    let at = start(&mut s, 1);
    // Many passes later it still loops, with a frame up across each seam.
    let seam = frames_between(&mut s, at, 4.34, 4.46, 0.04);
    assert_eq!(seam, vec![18, 19, 10, 11], "a seam ten passes in");
    expect_state(&s, 1, CueState::Running, "looping");
    // The Devamp cue a tenth of a second into a pass: the pass plays out, then the next slice.
    run_to(&mut s, at + samples(4.5));
    start(&mut s, 2);
    run_to(&mut s, at + samples(4.78));
    assert_eq!(shown(&mut s), Some(19), "the pass under way plays out");
    run_to(&mut s, at + samples(4.82));
    assert_eq!(shown(&mut s), Some(20), "into the slice after the loop");
    run_to(&mut s, at + samples(5.18));
    assert_eq!(shown(&mut s), Some(29));
    expect_state(&s, 1, CueState::Running, "in the last slice");
    run_to(&mut s, at + samples(5.2) + BLOCK);
    expect_state(&s, 1, CueState::Idle, "the cue ends after the last slice");
    assert_eq!(shown(&mut s), None);
}

/// A Devamp cue set to stop the target when the slice ends: the loop's pass plays out and the
/// cue ends there, its picture gone.
#[test]
fn a_devamp_that_stops_its_target_ends_the_video_cue_at_the_end_of_the_pass() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (50, 25));
    let devamp = cue(
        2,
        CueKind::Devamp {
            target: Some(id(1)),
            start_next_cue: false,
            stop_target_when_slice_ends: true,
            devamp_mode: openlab_core::DevampMode::CurrentSlice,
        },
    );
    let looping = timed_cue(1, &movie, |t| {
        t.end_time = Some(0.8);
        t.last_slice_play_count = 0;
    });
    let mut s = video_show(vec![looping, devamp]);
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(1.0));
    start(&mut s, 2);
    run_to(&mut s, at + samples(1.58));
    assert_eq!(shown(&mut s), Some(19), "the second pass plays out");
    run_to(&mut s, at + samples(1.6) + BLOCK);
    expect_state(&s, 1, CueState::Idle, "stopped at the end of the pass");
    assert_eq!(shown(&mut s), None);
}

/// "Hold at end" on → at the last frame the picture stays and the cue stays active until
/// stopped; off → the cue ends and its picture leaves the stage.
#[test]
fn hold_at_end_on_keeps_the_last_frame_and_the_cue_until_stopped_and_off_ends_the_cue_and_clears_the_stage()
 {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (50, 25));
    let mut s = video_show(vec![
        timed_cue(1, &movie, |t| {
            t.end_time = Some(1.2);
            t.hold_at_end = true;
        }),
        timed_cue(2, &movie, |t| t.end_time = Some(1.2)),
    ]);
    // The Duration cell still shows the movie's played length.
    assert_eq!(told_lengths(&s, 1), (Some(1.2), Some(1.2)));
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(1.2) + BLOCK);
    expect_state(&s, 1, CueState::Running, "held at its end");
    assert_eq!(
        shown(&mut s),
        Some(29),
        "the last frame before the end trim"
    );
    run_to(&mut s, at + samples(10.0));
    expect_state(&s, 1, CueState::Running, "still held");
    assert_eq!(shown(&mut s), Some(29));
    s.transport(TransportAction::Stop {
        cue: id(1),
        fade_out: None,
    });
    s.run(BLOCK);
    expect_state(&s, 1, CueState::Idle, "stopped");
    assert_eq!(shown(&mut s), None, "gone once stopped");

    let at = start(&mut s, 2);
    run_to(&mut s, at + samples(1.18));
    assert_eq!(shown(&mut s), Some(29));
    run_to(&mut s, at + samples(1.2) + BLOCK);
    expect_state(&s, 2, CueState::Idle, "hold off: it ends");
    assert_eq!(shown(&mut s), None, "and its picture leaves the stage");
}

/// The red of stage 1's picture at the card's top-left quadrant (the movie fills the stage).
fn red_at(s: &mut OfflineShow) -> u8 {
    colours(s, &[[0.25, 0.4]]).first().unwrap()[0]
}

/// Integrated fade in and out → opacity follows the fade lengths (the sound half waits for
/// sound from video files, #645).
#[test]
fn integrated_fade_in_and_out_opacity_follows_the_fade_lengths() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (50, 25));
    let linear = |duration| openlab_core::FadeSpec {
        duration,
        curve: openlab_core::FadeCurve::Linear,
    };
    let mut s = video_show(vec![timed_cue(1, &movie, |t| {
        t.fade_in = linear(0.5);
        t.fade_out = linear(0.5);
    })]);
    let at = go(&mut s);
    run_to(&mut s, at + samples(0.25));
    let half_in = red_at(&mut s);
    run_to(&mut s, at + samples(1.0));
    // The encoder's red, at full opacity between the fades.
    let full = red_at(&mut s);
    assert!(full >= 200, "between the fades: {full}");
    let half = |got: u8| (f64::from(got) / f64::from(full) - 0.5).abs() <= 0.06;
    assert!(
        half(half_in),
        "halfway into the fade in: {half_in} of {full}"
    );
    run_to(&mut s, at + samples(1.75));
    let half_out = red_at(&mut s);
    assert!(
        half(half_out),
        "halfway through the fade out: {half_out} of {full}"
    );
    // A loop has no end until it is devamped: no fade out while it loops.
    let mut s = video_show(vec![timed_cue(1, &movie, |t| {
        t.fade_out = linear(0.5);
        t.last_slice_play_count = 0;
    })]);
    let at = go(&mut s);
    run_to(&mut s, at + samples(3.9));
    let looping = red_at(&mut s);
    assert!(
        looping >= full - 10,
        "looping, not fading: {looping} of {full}"
    );
}

/// The tab's playhead line follows playback inside a loop: the cue's position in its file, as
/// clients are told it, is the loop's pass, not time since GO; held at the last frame.
#[test]
fn the_playhead_follows_playback_inside_a_loop() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (50, 25));
    let mut s = video_show(vec![timed_cue(1, &movie, |t| {
        t.start_time = 0.4;
        t.end_time = Some(1.2);
        t.last_slice_play_count = 0;
    })]);
    let at = start(&mut s, 1);
    let (elapsed, remaining, position) =
        progress_at(&mut s, 1, at + samples(2.0)).expect("looping");
    assert!(near(elapsed, 2.0), "{elapsed}");
    assert_eq!(remaining, None, "a loop has no end");
    assert!(
        near(position.unwrap(), 0.8),
        "two seconds in: 0.4 into the third pass, {position:?}"
    );
    let (_, _, position) = progress_at(&mut s, 1, at + samples(2.7)).expect("looping");
    assert!(near(position.unwrap(), 0.7), "{position:?}");
}

/// Load to time inside the range → GO starts on that frame (the time counted in the cue's
/// action: from the start trim, through its passes).
#[test]
fn load_to_time_inside_the_range_go_starts_on_that_frame() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (50, 25));
    let mut s = video_show(vec![timed_cue(1, &movie, |t| {
        t.start_time = 0.4;
        t.end_time = Some(1.2);
        t.last_slice_play_count = 2;
    })]);
    // 0.5 s into the action: 0.9 s into the file.
    s.transport(TransportAction::Load {
        cue: id(1),
        at: 0.5,
    });
    s.run(BLOCK);
    expect_state(&s, 1, CueState::Loaded, "");
    let at = go(&mut s);
    run_to(&mut s, at + samples(0.02));
    assert_eq!(shown(&mut s), Some(frame_at(0.92)), "that frame first");
    // 1.1 s left of the action (two passes of 0.8 s): through the seam into the second pass.
    run_to(&mut s, at + samples(0.32));
    assert_eq!(shown(&mut s), Some(10), "the second pass");
    run_to(&mut s, at + samples(1.1) + BLOCK);
    expect_state(&s, 1, CueState::Idle, "the rest of the action played");
    // Into the second pass: 1.0 s into the action is 0.6 s into the file's second pass.
    s.transport(TransportAction::Load {
        cue: id(1),
        at: 1.0,
    });
    s.run(BLOCK);
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(0.02));
    assert_eq!(
        on_stage(&mut s, "loaded into the second pass"),
        frame_at(0.62)
    );
}

/// Trims the movie cannot play (a start trim past its end) break the cue with why, as an Audio
/// cue's slices that cannot play do; GO plays nothing. Fixed, it plays.
#[test]
fn trims_past_the_movies_end_break_the_cue_with_why_until_fixed() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (50, 25));
    let mut s = video_show(vec![timed_cue(1, &movie, |t| t.start_time = 3.0)]);
    assert!(
        matches!(
            problems(&s, 1).as_slice(),
            [CueProblem::SlicesUnplayable { .. }]
        ),
        "{:?}",
        problems(&s, 1)
    );
    go(&mut s);
    s.run(samples(0.2));
    assert_eq!(shown(&mut s), None, "nothing plays");
    set_geometry(&mut s, 1, serde_json::json!({"start_time": 1.0}));
    assert_eq!(problems(&s, 1), vec![]);
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(0.02));
    assert_eq!(shown(&mut s), Some(25));
}

/// A stage host that starts again while a devamped loop plays is sent the plan as the devamp
/// left it: the picture goes on into the slice after the loop, not round the loop again.
#[test]
fn a_devamped_loop_survives_a_stage_host_relaunch() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (50, 25));
    let devamp = cue(
        2,
        CueKind::Devamp {
            target: Some(id(1)),
            start_next_cue: false,
            stop_target_when_slice_ends: false,
            devamp_mode: openlab_core::DevampMode::CurrentSlice,
        },
    );
    let looping = timed_cue(1, &movie, |t| {
        t.end_time = Some(1.6);
        t.slices = vec![openlab_core::SliceMarker {
            time: 0.8,
            play_count: 0,
        }];
    });
    let mut s = video_show(vec![looping, devamp]);
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(1.0));
    start(&mut s, 2);
    s.run(BLOCK);
    let pid = s.control().stage_host_pid().unwrap();
    let ended = Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    assert!(ended.success());
    assert!(s.wait_for(|c| c.stage_host_pid().is_some_and(|p| p != pid)));
    s.settle_stages();
    run_to(&mut s, at + samples(1.62));
    assert_eq!(
        on_stage(&mut s, "after the relaunch, past the loop"),
        frame_at(0.82)
    );
}

// ---------------------------------------------------------------------------------------------
// Review round 1 of #646.

/// A movie held at its end (or played past it) never runs on into the file past its end trim:
/// pause and resume, Pause All and Resume All, and a load past the end then GO all hold the
/// range's last frame with hold at end on; with it off the cue ends and its picture leaves.
#[test]
fn a_held_movie_never_plays_past_its_end_trim_after_pause_resume_or_a_load_past_its_end() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (50, 25));
    let held = |n| {
        timed_cue(n, &movie, |t| {
            t.start_time = 0.4;
            t.end_time = Some(1.2);
            t.hold_at_end = true;
        })
    };
    let ends = timed_cue(3, &movie, |t| {
        t.start_time = 0.4;
        t.end_time = Some(1.2);
    });
    let mut s = video_show(vec![held(1), held(2), ends]);
    // Held, then paused and resumed: still the range's last frame, however long after.
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(1.2));
    assert_eq!(shown(&mut s), Some(29), "held at its end");
    s.transport(TransportAction::Pause { cue: id(1) });
    s.run(samples(0.2));
    s.transport(TransportAction::Resume { cue: id(1) });
    let r = s.now();
    for later in [0.4, 0.8, 2.0] {
        run_to(&mut s, r + samples(later));
        assert_eq!(shown(&mut s), Some(29), "{later} s after the resume");
    }
    expect_state(&s, 1, CueState::Running, "held");
    // Pause All and Resume All, the same.
    s.transport(TransportAction::PauseAll);
    s.run(samples(0.2));
    s.transport(TransportAction::ResumeAll);
    let r = s.now();
    run_to(&mut s, r + samples(0.6));
    assert_eq!(shown(&mut s), Some(29), "after Resume All");
    s.transport(TransportAction::Stop {
        cue: id(1),
        fade_out: None,
    });
    s.run(BLOCK);
    assert_eq!(shown(&mut s), None);
    // Loaded past its end, then GO: the range's last frame, held.
    s.transport(TransportAction::Load {
        cue: id(2),
        at: 1.5,
    });
    s.run(BLOCK);
    let at = start(&mut s, 2);
    for later in [0.02, 0.5, 1.0] {
        run_to(&mut s, at + samples(later));
        assert_eq!(
            shown(&mut s),
            Some(29),
            "{later} s after GO, loaded past the end"
        );
    }
    expect_state(&s, 2, CueState::Running, "held");
    s.transport(TransportAction::Stop {
        cue: id(2),
        fade_out: None,
    });
    s.run(BLOCK);
    // Without hold at end: loaded past its end, GO ends it at once and shows nothing.
    s.transport(TransportAction::Load {
        cue: id(3),
        at: 1.5,
    });
    s.run(BLOCK);
    let at = start(&mut s, 3);
    run_to(&mut s, at + samples(0.3));
    expect_state(&s, 3, CueState::Idle, "nothing left to play");
    assert_eq!(shown(&mut s), None);
}

/// GO on a loaded Video cue whose file was changed while loaded (its new file still being
/// probed) waits for the probe as a fresh start does: once the file is known to play, the new
/// file plays from its head and the cue ends at its end; a file that does not play breaks it.
/// Never running with nothing on the stage for ever.
#[test]
fn go_on_a_loaded_video_cue_whose_file_is_probed_again_plays_it_or_breaks_once_answered() {
    let dir = tempfile::tempdir().unwrap();
    let a = movie!(dir.path(), "a.mov", (50, 25));
    let b = movie!(dir.path(), "b.mov", (25, 25));
    let gif = dir.path().join("bad.gif");
    std::fs::write(&gif, b"GIF89a\x01\x00\x01\x00\x00\x00\x00;").unwrap();
    let mut s = gated_video_show(vec![video_cue(1, Some(&a)), video_cue(2, Some(&a))]);
    for n in [1, 2] {
        s.transport(TransportAction::Load {
            cue: id(n),
            at: 0.0,
        });
    }
    s.run(BLOCK);
    expect_state(&s, 1, CueState::Loaded, "");
    GATED.lock().unwrap().extend([b.clone(), gif.clone()]);
    set_media(&mut s, 1, &b);
    set_media(&mut s, 2, &gif);
    s.run(BLOCK);
    start(&mut s, 1);
    start(&mut s, 2);
    s.run(samples(0.5));
    assert_eq!(shown(&mut s), None, "the old file is not played");
    release_probe(&b);
    release_probe(&gif);
    s.settle_media();
    let released = s.now();
    expect_state(
        &s,
        2,
        CueState::Broken,
        "a file that does not play breaks it",
    );
    run_to(&mut s, released + samples(0.5));
    let now = on_stage(&mut s, "the new file plays once read");
    assert!(now.abs_diff(frame_at(0.5)) <= 1, "{now}");
    // The new file is one second long: the cue ends then.
    run_to(&mut s, released + samples(1.1));
    expect_state(&s, 1, CueState::Idle, "ended at the new file's end");
    assert_eq!(shown(&mut s), None);
}

/// The shortest loop allowed (50 ms: at 25 fps frame 10, then 10 ms of frame 11), a loop of
/// two whole frames and one of three go round: looked at every block (8 ms) for a while, each
/// shows all its frames and keeps coming back to its first. A seam steps back by a frame or
/// less here, which must not be held as a clock correction.
#[test]
fn the_shortest_loops_jump_back_at_each_seam() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (50, 25));
    for (end, frames) in [
        (0.45, vec![10u16, 11]),
        (0.48, vec![10, 11]),
        (0.52, vec![10, 11, 12]),
    ] {
        let mut s = video_show(vec![timed_cue(1, &movie, |t| {
            t.start_time = 0.4;
            t.end_time = Some(end);
            t.last_slice_play_count = 0;
        })]);
        let at = start(&mut s, 1);
        run_to(&mut s, at + samples(0.5));
        let mut seen = Vec::new();
        for _ in 0..60 {
            s.run(BLOCK);
            seen.push(on_stage(&mut s, "looping"));
        }
        let mut shown: Vec<u16> = seen.clone();
        shown.sort_unstable();
        shown.dedup();
        assert_eq!(shown, frames, "a loop to {end} s: {seen:?}");
        let rounds = seen.windows(2).filter(|w| w[1] == 10 && w[0] != 10).count();
        assert!(rounds >= 4, "a loop to {end} s goes round: {seen:?}");
    }
}

/// Trims edited while the cue is paused take effect at its next start, not at the resume: the
/// run plays on with the plan it started with, as a paused Audio cue's voice does.
#[test]
fn a_resume_plays_on_with_the_plan_the_run_started_with() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (50, 25));
    let mut s = video_show(vec![timed_cue(1, &movie, |t| {
        t.start_time = 0.4;
        t.end_time = Some(1.2);
    })]);
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(0.3));
    s.transport(TransportAction::Pause { cue: id(1) });
    s.run(BLOCK);
    let paused = s.now();
    set_geometry(&mut s, 1, serde_json::json!({"end_time": 1.8}));
    s.transport(TransportAction::Resume { cue: id(1) });
    let resumed = s.now();
    let end = at + samples(0.8) + (resumed - paused);
    run_to(&mut s, end - samples(0.02));
    assert_eq!(shown(&mut s), Some(29), "the old end trim's last frame");
    run_to(&mut s, end + BLOCK);
    expect_state(&s, 1, CueState::Idle, "ended at the old end trim");
    // The next start plays the new range.
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(1.3));
    assert_eq!(shown(&mut s), Some(frame_at(1.7)));
}
