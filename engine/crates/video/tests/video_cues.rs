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
    Crosspoint, Cue, CueKind, CueLevels, CueOutput, CueProblem, CueState, MediaRef, PatchId, Stage,
    StageId, VideoGeometry, Workspace,
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
        // A movie with sound tracks (`h264+pcm:2:48000`).
        with_sound if with_sound.contains('+') => "mov",
        "huge-png" | "wide-png" | "tall-png" | "alpha-png" | "tiny-png" => "png",
        other => other,
    };
    let name = match frames_fps {
        Some((frames, fps)) => format!("{}-{frames}-{fps}.{ext}", kind.replace(':', "_")),
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
    ws: Workspace,
    extra: &[&str],
    tweak: impl FnOnce(&mut StageHostConfig),
) -> OfflineShow {
    video_show_config(ws, extra, tweak, |_| {})
}

/// As [`video_show_host`], the engine's configuration changed by `engine` too.
fn video_show_config(
    ws: Workspace,
    extra: &[&str],
    tweak: impl FnOnce(&mut StageHostConfig),
    engine: impl FnOnce(&mut ControlConfig),
) -> OfflineShow {
    video_show_rate(ws, RATE, BLOCK, extra, tweak, engine)
}

/// As [`video_show_config`], on an offline device at `rate` Hz in blocks of `block` frames.
fn video_show_rate(
    mut ws: Workspace,
    rate: u32,
    block: u64,
    extra: &[&str],
    tweak: impl FnOnce(&mut StageHostConfig),
    engine: impl FnOnce(&mut ControlConfig),
) -> OfflineShow {
    let mut stage = Stage::new(stage_id(), "Stage 1", 1920, 1080);
    stage.keep_black = true;
    ws.stages = vec![stage];
    let mut c: ControlConfig = config_at(&ws, rate, block);
    engine(&mut c);
    let mut host = StageHostConfig::new(PathBuf::from(STAGE_HOST));
    host.args = vec!["--virtual-displays".into(), "V1:1920x1080".into()];
    host.args.extend(extra.iter().map(|a| (*a).to_owned()));
    // A stage host slow to answer on a loaded machine (a probe waiting on a file's open) is
    // never killed as hung: the tests kill one when they mean to.
    host.answer_within = SLOW_MACHINE;
    tweak(&mut host);
    c.stage_host = Some(host);
    let mut s = OfflineShow::new(ws, mixer_config_at(rate, block), c).unwrap();
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
/// gave up, every cue's problems (a cue broken before its GO says why: what its file's probe
/// answered) and the end of the show log (a picture failure, a stage host restart, a probe the
/// stage host could not draw).
fn seen(s: &OfflineShow) -> String {
    let c = s.control();
    let problems: Vec<String> = c
        .workspace()
        .cues()
        .map(|cue| (cue.id, c.engine().problems(cue.id)))
        .filter(|(_, problems)| !problems.is_empty())
        .map(|(cue, problems)| format!("cue {cue:?} has problems: {problems:?}"))
        .collect();
    let log: Vec<String> = c
        .show_log()
        .tail(30)
        .iter()
        .map(|e| format!("{:?} {:?} {}", e.level, e.kind, e.message))
        .collect();
    format!(
        "at sample {}; pictures up {}, open {}, waits given up {}\n{}\n{}",
        s.now(),
        c.pictures_live(),
        c.pictures_opened(),
        s.stalls(),
        problems.join("\n"),
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
// Time and loops (VID-07; QLab 5 manual p. 214). The movies are 25 fps
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
        t.last_slice_play_count = 3.into();
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
            play_count: 0.into(),
        }];
        t.last_slice_play_count = 1.into();
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
        t.last_slice_play_count = 0.into();
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

/// Cue 1 loops file 0 to 0.8 s for ever; Devamp cue 2 stops it at the end of the pass under
/// way, and with `start_next` starts cue 3 (a long Wait) then.
fn devamp_stop_show(movie: &Path, start_next: bool) -> OfflineShow {
    let looping = timed_cue(1, movie, |t| {
        t.end_time = Some(0.8);
        t.last_slice_play_count = 0.into();
    });
    let devamp = cue(
        2,
        CueKind::Devamp {
            target: Some(id(1)),
            start_next_cue: start_next,
            stop_target_when_slice_ends: true,
            devamp_mode: openlab_core::DevampMode::CurrentSlice,
        },
    );
    let wait = cue(3, CueKind::Wait { duration: 5.0 });
    video_show(vec![looping, devamp, wait])
}

/// Paused while a devamp that stops it is pending, and kept paused past the sample the pass
/// would have ended on: the frame stays and the cue stays paused. The resume plays the rest
/// of the pass, the picture leaves when it ends, the cue ends then, and the Devamp cue starts
/// the next cue then.
#[test]
fn a_pause_past_a_pending_devamp_stop_holds_the_frame_and_the_resume_plays_the_pass_out() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (50, 25));
    let mut s = devamp_stop_show(&movie, true);
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(1.0));
    start(&mut s, 2); // the pass under way ends at 1.6 s
    run_to(&mut s, at + samples(1.1));
    s.transport(TransportAction::Pause { cue: id(1) });
    let paused = s.now();
    s.run(BLOCK);
    let held = on_stage(&mut s, "the frame held");
    let into_pass = (paused - at) as f64 / f64::from(RATE) - 0.8;
    assert!(held.abs_diff(frame_at(into_pass)) <= 1, "{held} held");
    run_to(&mut s, at + samples(2.4));
    expect_state(
        &s,
        1,
        CueState::Paused,
        "paused past the pass's planned end",
    );
    assert_eq!(shown(&mut s), Some(held), "the frame stays while paused");
    expect_state(&s, 3, CueState::Idle, "the loop is not left yet");
    s.transport(TransportAction::Resume { cue: id(1) });
    let resumed = s.now();
    let leaves = at + samples(1.6) + (resumed - paused);
    run_to(&mut s, resumed + samples(0.2));
    expect_state(&s, 1, CueState::Running, "after the resume");
    let now = on_stage(&mut s, "playing on after the resume");
    assert!(now.abs_diff(frame_at(into_pass + 0.2)) <= 1, "{now}");
    run_to(&mut s, leaves - samples(0.05));
    let now = on_stage(&mut s, "the end of the pass");
    assert!(now.abs_diff(frame_at(0.75)) <= 1, "{now}");
    expect_state(&s, 3, CueState::Idle, "the loop is not left yet");
    run_to(&mut s, leaves + 2 * BLOCK);
    expect_state(&s, 1, CueState::Idle, "stopped at the end of the pass");
    assert_eq!(shown(&mut s), None);
    assert_eq!(s.control().pictures_live(), 0);
    expect_state(&s, 3, CueState::Running, "started as the loop was left");
}

/// A short pause before a pending devamp stop is reached moves the end of the pass by the
/// time paused: the pass plays out whole and the cue ends with its picture.
#[test]
fn a_short_pause_before_a_pending_devamp_stop_moves_the_end_by_the_time_paused() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (50, 25));
    let mut s = devamp_stop_show(&movie, false);
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(1.0));
    start(&mut s, 2); // the pass under way ends at 1.6 s
    run_to(&mut s, at + samples(1.1));
    s.transport(TransportAction::Pause { cue: id(1) });
    let paused = s.now();
    s.run(samples(0.2));
    s.transport(TransportAction::Resume { cue: id(1) });
    let resumed = s.now();
    let leaves = at + samples(1.6) + (resumed - paused);
    run_to(&mut s, at + samples(1.65));
    expect_state(
        &s,
        1,
        CueState::Running,
        "past the planned end, before the moved one",
    );
    on_stage(&mut s, "past the planned end, before the moved one");
    run_to(&mut s, leaves - samples(0.05));
    let now = on_stage(&mut s, "the end of the pass");
    assert!(now.abs_diff(frame_at(0.75)) <= 1, "{now}");
    run_to(&mut s, leaves + 2 * BLOCK);
    expect_state(
        &s,
        1,
        CueState::Idle,
        "stopped at the moved end of the pass",
    );
    assert_eq!(shown(&mut s), None);
    assert_eq!(s.control().pictures_live(), 0);
}

/// With hold at end on, a devamp that stops the cue still ends it, and its picture, at the
/// end of the pass.
#[test]
fn a_devamp_stop_ends_a_hold_at_end_cue_and_its_picture_with_the_pass() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (50, 25));
    let looping = timed_cue(1, &movie, |t| {
        t.end_time = Some(0.8);
        t.last_slice_play_count = 0.into();
        t.hold_at_end = true;
    });
    let devamp = cue(
        2,
        CueKind::Devamp {
            target: Some(id(1)),
            start_next_cue: false,
            stop_target_when_slice_ends: true,
            devamp_mode: openlab_core::DevampMode::CurrentSlice,
        },
    );
    let mut s = video_show(vec![looping, devamp]);
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(1.0));
    start(&mut s, 2); // the pass under way ends at 1.6 s
    run_to(&mut s, at + samples(1.58));
    assert_eq!(shown(&mut s), Some(19), "the pass plays out");
    run_to(&mut s, at + samples(1.6) + 2 * BLOCK);
    expect_state(&s, 1, CueState::Idle, "stopped at the end of the pass");
    assert_eq!(shown(&mut s), None);
    assert_eq!(s.control().pictures_live(), 0);
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

/// Integrated fade in and out → opacity follows the fade lengths (the sound's half is
/// `integrated_fades_shape_the_sound_with_the_picture`).
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
        t.last_slice_play_count = 0.into();
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
        t.last_slice_play_count = 0.into();
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
        t.last_slice_play_count = 2.into();
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

/// A slice with a count of 0 is skipped, as an Audio cue's: the picture goes from the end of
/// the slice before it to the start of the next that plays with no black frame, the Duration
/// cell counts only what plays, and a movie every slice of which is skipped is broken with its
/// own problem until one plays.
#[test]
fn a_skipped_slice_is_jumped_over_without_a_black_frame_and_all_skipped_breaks_the_cue() {
    use openlab_core::{PlayCount, SliceMarker};
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (50, 25));
    // 0–0.4 s once, 0.4–1.2 s skipped, 1.2–2 s once.
    let marks = |middle| {
        vec![
            SliceMarker {
                time: 0.4,
                play_count: PlayCount::ONCE,
            },
            SliceMarker {
                time: 1.2,
                play_count: middle,
            },
        ]
    };
    let mut s = video_show(vec![timed_cue(1, &movie, |t| {
        t.slices = marks(PlayCount::Skip);
    })]);
    assert_eq!(told_lengths(&s, 1), (Some(1.2), Some(1.2)));
    let at = go(&mut s);
    let seen = frames_between(&mut s, at, 0.02, 1.14, 0.04);
    assert!(
        seen.iter()
            .all(|f| !(frame_at(0.44)..frame_at(1.16)).contains(f)),
        "no frame of the skipped slice: {seen:?}"
    );
    // 0.30 s and 0.62 s after GO (the 8th and 16th looks).
    assert_eq!(seen[7], frame_at(0.3), "in the first slice: {seen:?}");
    assert_eq!(seen[15], frame_at(1.42), "in the last slice: {seen:?}");
    run_to(&mut s, at + samples(1.2) + BLOCK);
    expect_state(&s, 1, CueState::Idle, "what plays is played");

    let mut s = video_show(vec![timed_cue(1, &movie, |t| {
        t.slices = marks(PlayCount::Skip);
        t.slices[0].play_count = PlayCount::Skip;
        t.last_slice_play_count = PlayCount::Skip;
    })]);
    assert_eq!(problems(&s, 1), vec![CueProblem::NothingPlays]);
    go(&mut s);
    s.run(samples(0.2));
    assert_eq!(shown(&mut s), None, "nothing plays");
    set_geometry(&mut s, 1, serde_json::json!({"last_slice_play_count": 1}));
    assert_eq!(problems(&s, 1), vec![]);
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
            play_count: 0.into(),
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
// A held movie, a file probed again, short loops and the stage host started again.

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
            t.last_slice_play_count = 0.into();
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

/// A Devamp cue of cue 1 that lets it play on.
fn devamp_cue(n: u128) -> Cue {
    cue(
        n,
        CueKind::Devamp {
            target: Some(id(1)),
            start_next_cue: false,
            stop_target_when_slice_ends: false,
            devamp_mode: openlab_core::DevampMode::CurrentSlice,
        },
    )
}

/// A devamp is done once the picture has left its loop: the next one in the same run is of the
/// loop under way then, not an echo of the first.
#[test]
fn a_second_devamp_in_the_same_run_leaves_the_second_loop() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (50, 25));
    // File 0.0 to 0.4 s for ever, 0.4 to 0.8 s for ever, then 0.8 to 1.2 s once.
    let looping = timed_cue(1, &movie, |t| {
        t.end_time = Some(1.2);
        t.slices = vec![
            openlab_core::SliceMarker {
                time: 0.4,
                play_count: 0.into(),
            },
            openlab_core::SliceMarker {
                time: 0.8,
                play_count: 0.into(),
            },
        ];
        t.last_slice_play_count = 1.into();
    });
    let mut s = video_show(vec![looping, devamp_cue(2), devamp_cue(3)]);
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(1.0));
    start(&mut s, 2); // the first loop is left at 1.2 s
    run_to(&mut s, at + samples(1.3));
    assert_eq!(shown(&mut s), Some(frame_at(0.5)), "in the second loop");
    run_to(&mut s, at + samples(1.7));
    start(&mut s, 3); // its pass under way is 1.6 to 2.0 s
    run_to(&mut s, at + samples(2.1));
    assert_eq!(shown(&mut s), Some(frame_at(0.9)), "in the last slice");
    run_to(&mut s, at + samples(2.6));
    expect_state(&s, 1, CueState::Idle, "the last slice played out");
    assert_eq!(shown(&mut s), None);
}

/// A devamp not yet carried out when its target pauses is asked again at the resume.
#[test]
fn a_devamp_pending_at_a_pause_leaves_the_loop_after_the_resume() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (50, 25));
    let looping = timed_cue(1, &movie, |t| {
        t.start_time = 0.4;
        t.end_time = Some(1.2);
        t.slices = vec![openlab_core::SliceMarker {
            time: 0.8,
            play_count: 0.into(),
        }];
        t.last_slice_play_count = 1.into();
    });
    let mut s = video_show(vec![looping, devamp_cue(2)]);
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(1.0));
    start(&mut s, 2); // the loop is left at 1.2 s
    run_to(&mut s, at + samples(1.1));
    s.transport(TransportAction::Pause { cue: id(1) });
    s.run(samples(0.2));
    s.transport(TransportAction::Resume { cue: id(1) });
    let resumed = s.now();
    // 0.1 s of the pass is left, then the slice after the loop (file 0.8 s on).
    run_to(&mut s, resumed + samples(0.2));
    assert_eq!(shown(&mut s), Some(frame_at(0.9)));
}

/// A devamp carried out stays carried out over a pause: the resume plays on in the slice after
/// the loop, to the end the cue was told.
#[test]
fn a_resume_in_the_slice_after_a_devamped_loop_plays_on_and_ends() {
    let dir = tempfile::tempdir().unwrap();
    let movie = movie!(dir.path(), "clip.mov", (50, 25));
    let looping = timed_cue(1, &movie, |t| {
        t.start_time = 0.4;
        t.end_time = Some(1.6);
        t.slices = vec![openlab_core::SliceMarker {
            time: 0.8,
            play_count: 0.into(),
        }];
        t.last_slice_play_count = 1.into();
    });
    let mut s = video_show(vec![looping, devamp_cue(2)]);
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(1.0));
    start(&mut s, 2); // the loop is left at 1.2 s, the action ends at 2.0 s
    run_to(&mut s, at + samples(1.42));
    assert_eq!(shown(&mut s), Some(frame_at(1.02)), "in the slice after");
    s.transport(TransportAction::Pause { cue: id(1) });
    s.run(samples(0.2));
    s.transport(TransportAction::Resume { cue: id(1) });
    let resumed = s.now();
    run_to(&mut s, resumed + samples(0.2));
    assert_eq!(shown(&mut s), Some(frame_at(1.22)), "plays on from there");
    run_to(&mut s, resumed + samples(0.7));
    expect_state(&s, 1, CueState::Idle, "ended where the devamp put its end");
    assert_eq!(shown(&mut s), None);
}

// A Video cue's sound (VID-06): its movie's audio track through the mixer, in step with its
// picture. The test movies' sound tracks say where in the file they are (`--make-test-media`):
// channel 0 rises from 0 to 0.5 over each second, channel 1 holds the second's number.

/// A movie of `frames_fps` in [`play_codec`] with the sound tracks `sound` names
/// (`pcm:2:48000`, `aac:2:44100+pcm:6:48000`), or `None` when no codec decodes reliably here
/// (the test then records a skip).
fn sound_movie(dir: &Path, name: &str, frames_fps: (u16, u16), sound: &str) -> Option<PathBuf> {
    let kind = format!("{}+{sound}", play_codec()?);
    Some(make(dir, name, &kind, Some(frames_fps)))
}

/// `sound_movie!(dir, name, (frames, fps), sound)`: [`sound_movie`], or the test ends skipped.
macro_rules! sound_movie {
    ($dir:expr, $name:expr, $frames_fps:expr, $sound:expr) => {
        match sound_movie($dir, $name, $frames_fps, $sound) {
            Some(movie) => movie,
            None => return skipped("no codec decodes reliably on this machine"),
        }
    };
}

/// A Video cue on `movie` whose sound plays on the test patch: file channel 0 to output 0,
/// channel 1 to output 1, every level 0 dB.
fn sounding_cue(n: u128, movie: &Path) -> Cue {
    let mut c = video_cue(n, Some(movie));
    if let CueKind::Video { sound, .. } = &mut c.kind {
        sound.patch = Some(patch_id());
        sound.levels = straight_levels();
    }
    c
}

fn straight_levels() -> CueLevels {
    let out = CueOutput {
        db: 0.0,
        mute: false,
    };
    let to = |input, output| Crosspoint {
        input,
        output,
        db: 0.0,
        mute: false,
    };
    CueLevels {
        master_db: 0.0,
        outputs: vec![out; 2],
        crosspoints: vec![to(0, 0), to(1, 1)],
        gangs: vec![],
    }
}

/// The two outputs' values on rendered sample `at`.
fn outputs_at(s: &OfflineShow, at: u64) -> (f32, f32) {
    let i = usize::try_from(at).unwrap() * 2;
    match s.audio().get(i..i + 2) {
        Some(&[left, right]) => (left, right),
        _ => panic!("sample {at} is not rendered yet"),
    }
}

/// Where in its file the test track heard on rendered sample `at` is, in seconds (`None`:
/// silence there), read from the two outputs at unity.
fn sound_at(s: &OfflineShow, at: u64) -> Option<f64> {
    let (rise, second) = outputs_at(s, at);
    let second = (f64::from(second) * 2048.0).round() - 1.0;
    (second >= 0.0).then(|| second + f64::from(rise) * 2.0)
}

/// Where the sound will be on the sample the stage shows now (the picture is drawn for the
/// first sample not yet rendered, a block ahead of the last one heard), at `rate`.
fn sound_now(s: &OfflineShow, rate: u32) -> Option<f64> {
    let heard = s.rendered().checked_sub(1)?;
    let ahead = (s.now() - heard) as f64 / f64::from(rate);
    sound_at(s, heard).map(|p| p + ahead)
}

/// Picture and sound are within one frame of each other now: the frame on the stage is the
/// one whose time the sound has reached (`fps` frames a second; the sound may be anywhere in
/// that frame, so the two are less than a frame period apart). Returns the sound's position.
fn in_step(s: &mut OfflineShow, rate: u32, fps: f64, what: &str) -> f64 {
    let sound = sound_now(s, rate).unwrap_or_else(|| panic!("{what}: no sound\n{}", seen(s)));
    let frame = f64::from(on_stage(s, what));
    let apart = sound - frame / fps;
    // A millisecond of slack for the resampled ramp the position is read from.
    assert!(
        (-0.001..1.0 / fps + 0.001).contains(&apart),
        "{what}: frame {frame} with the sound at {sound} s ({apart} s apart)"
    );
    sound
}

/// `movie` as an old QuickTime file, written to `to`: its `ftyp` box made a `free` one, so the
/// file has no file-type box and every offset in it stays right.
fn without_file_type(movie: &Path, to: &Path) -> PathBuf {
    let mut bytes = std::fs::read(movie).unwrap();
    let kind = bytes.get_mut(4..8).expect("a first box");
    assert_eq!(kind, b"ftyp");
    kind.copy_from_slice(b"free");
    std::fs::write(to, &bytes).unwrap();
    to.to_path_buf()
}

/// The audio decoder reads a movie's sound tracks: PCM and AAC, each track's format, and its
/// samples where the file has them (AAC without its encoder's priming).
#[test]
fn a_movies_sound_tracks_are_probed_with_their_format() {
    let dir = tempfile::tempdir().unwrap();
    let silent = make(dir.path(), "silent.mov", "h264", Some((25, 25)));
    assert!(openlab_audio::probe_movie(&silent).unwrap().is_empty());
    let kind = "h264+aac:2:44100+pcm:6:48000";
    let two = make(dir.path(), "two.mov", kind, Some((50, 25)));
    let tracks = openlab_audio::probe_movie(&two).unwrap();
    let found: Vec<(&str, u16, u32)> = tracks
        .iter()
        .map(|t| {
            let info = t.probed.as_ref().unwrap().info;
            (t.codec.as_str(), info.channels, info.sample_rate)
        })
        .collect();
    assert_eq!(found, [("mp4a", 2, 44_100), ("lpcm", 6, 48_000)]);
    assert_eq!(tracks[1].probed.as_ref().unwrap().info.frames, 96_000);
    // Each track's samples are where the file has them: half a second in, channel 0 is half
    // way up its rise, in the AAC track too (to its coding's accuracy).
    for (track, rate, slack) in [(0u32, 44_100usize, 0.01f32), (1, 48_000, 0.0001)] {
        let media = openlab_audio::Media::Track(openlab_audio::MovieTrack {
            path: two.clone(),
            track,
            fit: None,
        });
        let all = openlab_audio::preload(media).unwrap();
        let channels = usize::from(all.info().channels);
        let rise = all.samples()[rate / 2 * channels];
        assert!((rise - 0.25).abs() < slack, "track {track}: {rise}");
    }
    assert!(openlab_audio::probe_movie(&dir.path().join("gone.mov")).is_err());
    // An old QuickTime file has no file-type box: its tracks are read all the same.
    let old = without_file_type(&two, &dir.path().join("old.mov"));
    let tracks = openlab_audio::probe_movie(&old).unwrap();
    let frames: Vec<u64> = (tracks.iter())
        .map(|t| t.probed.as_ref().unwrap().info.frames)
        .collect();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[1], 96_000);
    // Fitted to a movie's length a track reads as exactly that long: silence after its own
    // end, or cut short.
    let fitted = |frames: u64| {
        let media = openlab_audio::Media::Track(openlab_audio::MovieTrack {
            path: two.clone(),
            track: 1,
            fit: Some(openlab_audio::Fit {
                real: 96_000,
                frames,
            }),
        });
        openlab_audio::preload(media).unwrap()
    };
    let longer = fitted(96_500);
    assert_eq!(longer.samples().len(), 96_500 * 6);
    assert!(longer.samples()[95_999 * 6 + 2] != 0.0);
    assert!(longer.samples()[96_000 * 6..].iter().all(|x| *x == 0.0));
    assert_eq!(fitted(48_000).samples().len(), 48_000 * 6);
}

/// Flow 1 (engine): a movie with sound on a patch → GO puts its picture on the stage and its
/// sound on the patch's outputs, in step, and both end at the movie's end.
#[test]
fn a_movie_with_sound_gets_a_levels_tab_and_is_heard_on_its_patch() {
    let dir = tempfile::tempdir().unwrap();
    let movie = sound_movie!(dir.path(), "clip.mov", (75, 25), "pcm:2:48000");
    let mut s = video_show(vec![sounding_cue(1, &movie)]);
    assert_eq!(problems(&s, 1), vec![]);
    let at = go(&mut s);
    for t in [0.1, 0.5, 1.5, 2.5, 2.9] {
        run_to(&mut s, at + samples(t));
        let sound = in_step(&mut s, RATE, 25.0, &format!("{t} s after GO"));
        assert!(
            (sound - t).abs() < 0.02,
            "the sound is at {sound} s, {t} s after GO"
        );
    }
    // Three seconds long: picture and sound end together, once.
    run_to(&mut s, at + samples(3.0) + 2 * BLOCK);
    expect_state(&s, 1, CueState::Idle, "");
    assert_eq!(shown(&mut s), None);
    s.run(samples(0.2));
    let end = usize::try_from(at + samples(3.0)).unwrap();
    let last = last_nonzero(&s.channel(1)).unwrap();
    assert!(
        (end - 8..end).contains(&last),
        "the last sample heard is {last}, the movie's end {end}"
    );
    assert_eq!(s.control().live_voices(), 0);
    assert_eq!(s.stalls(), 0);
    // Its sound was ready before the GO, as an Audio cue's at the playhead is.
    assert_eq!(s.control().prime_hits(), 1);
}

/// A Video cue's sound is kept ready as an Audio cue's is, and a Video cue with nothing to
/// hear (a silent movie, though it has a patch) takes no place among the primed cues from one
/// that has.
#[test]
fn a_silent_movie_takes_no_prime_from_a_movie_with_sound() {
    let dir = tempfile::tempdir().unwrap();
    let quiet = movie!(dir.path(), "quiet.mov", (50, 25));
    let loud = sound_movie!(dir.path(), "loud.mov", (50, 25), "pcm:2:48000");
    let ws = workspace(vec![sounding_cue(1, &quiet), sounding_cue(2, &loud)]);
    let mut s = video_show_rate(ws, RATE, BLOCK, &[], |_| {}, |c| c.max_primes = 1);
    s.run(BLOCK * 4);
    assert_eq!(s.control().primed(), vec![id(2)]);
}

/// A Video cue with sound and the time and loops `edit` gives it.
fn timed_sounding_cue(n: u128, movie: &Path, edit: impl Fn(&mut openlab_core::VideoTiming)) -> Cue {
    let mut c = sounding_cue(n, movie);
    if let CueKind::Video { timing, .. } = &mut c.kind {
        edit(timing);
    }
    c
}

/// Whether nothing is heard on rendered samples `from..to`.
fn silent(s: &OfflineShow, from: u64, to: u64) -> bool {
    let (from, to) = (usize::try_from(from).unwrap(), usize::try_from(to).unwrap());
    let heard = s.audio().get(from * 2..to * 2).expect("rendered samples");
    heard.iter().all(|x| *x == 0.0)
}

/// The sound's file position `after` samples past sample `at` is `want` seconds, to half a
/// millisecond.
fn sound_is(s: &OfflineShow, at: u64, want: f64, what: &str) {
    let got = sound_at(s, at).unwrap_or_else(|| panic!("{what}: silence"));
    assert!(
        (got - want).abs() < 0.0005,
        "{what}: the sound is at {got} s, not {want} s"
    );
}

/// Flow 8 (a), (b), (d): pause holds the frame and stops the sound, resume goes on from the
/// same place for both; load to time then GO starts both there; with hold at end the picture
/// stays, the sound has ended and none restarts.
#[test]
fn pause_resume_load_to_time_and_a_loop_seam_keep_them_together() {
    let dir = tempfile::tempdir().unwrap();
    let movie = sound_movie!(dir.path(), "clip.mov", (100, 25), "pcm:2:48000");
    let mut s = video_show(vec![
        sounding_cue(1, &movie),
        timed_sounding_cue(2, &movie, |t| {
            t.end_time = Some(1.2);
            t.hold_at_end = true;
        }),
    ]);
    // (a) Pause and resume.
    let at = go(&mut s);
    run_to(&mut s, at + samples(1.0));
    in_step(&mut s, RATE, 25.0, "before the pause");
    s.transport(TransportAction::Pause { cue: id(1) });
    let paused = s.now();
    s.run(samples(0.5));
    expect_state(&s, 1, CueState::Paused, "");
    let held = on_stage(&mut s, "the frame held");
    assert!(held.abs_diff(frame_at((paused - at) as f64 / f64::from(RATE))) <= 1);
    // The sound stopped with the pause's short ramp, and stays stopped.
    assert!(silent(&s, paused + samples(0.01), s.rendered()));
    s.transport(TransportAction::Resume { cue: id(1) });
    let resumed = s.now();
    run_to(&mut s, resumed + samples(0.5));
    let sound = in_step(&mut s, RATE, 25.0, "after the resume");
    let played = (s.now() - at - (resumed - paused)) as f64 / f64::from(RATE);
    assert!(
        (sound - played).abs() < 0.002,
        "{sound} s after {played} s of play"
    );
    // Both end when the rest has played.
    let end = at + samples(4.0) + (resumed - paused);
    run_to(&mut s, end + 2 * BLOCK);
    expect_state(&s, 1, CueState::Idle, "");
    assert_eq!(shown(&mut s), None);
    s.run(samples(0.1));
    assert!(silent(&s, end, s.rendered()));

    // (b) Load to time, then GO: the first frame and the first sound are from there.
    s.transport(TransportAction::Load {
        cue: id(1),
        at: 3.0,
    });
    s.run(BLOCK);
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(0.1));
    sound_is(&s, at + 40, 3.0 + 40.0 / f64::from(RATE), "the first sound");
    let sound = in_step(&mut s, RATE, 25.0, "after a load to time");
    assert!((sound - 3.1).abs() < 0.01);
    run_to(&mut s, at + samples(1.0) + 2 * BLOCK);
    expect_state(&s, 1, CueState::Idle, "");

    // (d) Hold at end: the last frame stays, the sound has ended, the cue stays active.
    let at = start(&mut s, 2);
    run_to(&mut s, at + samples(1.1));
    in_step(&mut s, RATE, 25.0, "before its end");
    run_to(&mut s, at + samples(3.0));
    expect_state(&s, 2, CueState::Running, "held at its end");
    assert_eq!(shown(&mut s), Some(29));
    assert!(
        silent(&s, at + samples(1.2), s.rendered()),
        "no sound after the end"
    );
    assert_eq!(s.control().live_voices(), 0);
    s.transport(TransportAction::Stop {
        cue: id(2),
        fade_out: None,
    });
    s.run(BLOCK);
    expect_state(&s, 2, CueState::Idle, "");
    assert_eq!(s.stalls(), 0);
}

/// Flow 8 (c): with trims and a play count the sound jumps back to the range's start on the
/// sample the picture's plan does, with no gap and nothing heard twice, for a seam on a frame
/// boundary and one off it; a Devamp ends picture and sound at the end of the same pass.
#[test]
fn each_loop_seam_and_a_devamp_keep_picture_and_sound_together() {
    let dir = tempfile::tempdir().unwrap();
    let movie = sound_movie!(dir.path(), "clip.mov", (50, 25), "pcm:2:48000");
    for (start_time, end_time) in [(0.4, 1.2), (0.41, 1.23)] {
        let mut s = video_show(vec![timed_sounding_cue(1, &movie, |t| {
            t.start_time = start_time;
            t.end_time = Some(end_time);
            t.last_slice_play_count = 3.into();
        })]);
        let at = go(&mut s);
        let pass = samples(end_time) - samples(start_time);
        for seam in [at + pass, at + 2 * pass] {
            run_to(&mut s, seam - samples(0.1));
            in_step(&mut s, RATE, 25.0, "before the seam");
            run_to(&mut s, seam + samples(0.1));
            in_step(&mut s, RATE, 25.0, "after the seam");
            // Two milliseconds each side of the seam the sound is that far from the range's
            // end and start: nothing missing, nothing twice.
            sound_is(&s, seam - 16, end_time - 0.002, "before the seam");
            sound_is(&s, seam + 16, start_time + 0.002, "after the seam");
        }
        // Three passes, then both end on the same sample.
        let end = at + 3 * pass;
        run_to(&mut s, end + 2 * BLOCK);
        expect_state(&s, 1, CueState::Idle, "three passes played");
        assert_eq!(shown(&mut s), None);
        s.run(samples(0.1));
        sound_is(&s, end - 16, end_time - 0.002, "the last pass's end");
        assert!(silent(&s, end + 8, s.rendered()));
    }

    // A loop until devamped over 0.4 to 0.8 s, then 0.8 to 1.2 s once.
    let devamp = cue(
        2,
        CueKind::Devamp {
            target: Some(id(1)),
            start_next_cue: false,
            stop_target_when_slice_ends: false,
            devamp_mode: openlab_core::DevampMode::CurrentSlice,
        },
    );
    let looping = timed_sounding_cue(1, &movie, |t| {
        t.start_time = 0.4;
        t.end_time = Some(1.2);
        t.slices = vec![openlab_core::SliceMarker {
            time: 0.8,
            play_count: 0.into(),
        }];
    });
    let mut s = video_show(vec![looping, devamp]);
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(4.5));
    in_step(&mut s, RATE, 25.0, "ten passes in");
    start(&mut s, 2);
    // The pass under way (the twelfth) plays out to 4.8 s; then the slice after the loop.
    let leaves = at + samples(4.8);
    run_to(&mut s, leaves + samples(0.2));
    sound_is(&s, leaves - 16, 0.798, "the end of the last pass");
    sound_is(&s, leaves + 16, 0.802, "into the slice after the loop");
    let sound = in_step(&mut s, RATE, 25.0, "after the devamp");
    assert!((sound - 1.0).abs() < 0.01);
    let end = at + samples(5.2);
    run_to(&mut s, end + 2 * BLOCK);
    expect_state(&s, 1, CueState::Idle, "ended after the last slice");
    assert_eq!(shown(&mut s), None);
    s.run(samples(0.1));
    assert!(silent(&s, end + 8, s.rendered()));
    assert_eq!(s.stalls(), 0);
}

/// A pause while a devamp that stops the cue is pending, kept past the pass's planned end:
/// picture and sound both hold, both play the rest of the pass after the resume, and both end
/// on the pass's moved end.
#[test]
fn a_pause_with_a_pending_devamp_stop_keeps_picture_and_sound_together() {
    let dir = tempfile::tempdir().unwrap();
    let movie = sound_movie!(dir.path(), "clip.mov", (50, 25), "pcm:2:48000");
    let looping = timed_sounding_cue(1, &movie, |t| {
        t.end_time = Some(0.8);
        t.last_slice_play_count = 0.into();
    });
    let devamp = cue(
        2,
        CueKind::Devamp {
            target: Some(id(1)),
            start_next_cue: false,
            stop_target_when_slice_ends: true,
            devamp_mode: openlab_core::DevampMode::CurrentSlice,
        },
    );
    let mut s = video_show(vec![looping, devamp]);
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(1.0));
    start(&mut s, 2); // the pass under way ends at 1.6 s
    run_to(&mut s, at + samples(1.1));
    s.transport(TransportAction::Pause { cue: id(1) });
    let paused = s.now();
    run_to(&mut s, at + samples(2.0));
    expect_state(
        &s,
        1,
        CueState::Paused,
        "paused past the pass's planned end",
    );
    on_stage(&mut s, "the frame held");
    assert!(silent(&s, paused + samples(0.01), s.rendered()));
    s.transport(TransportAction::Resume { cue: id(1) });
    let resumed = s.now();
    let leaves = at + samples(1.6) + (resumed - paused);
    run_to(&mut s, resumed + samples(0.2));
    in_step(&mut s, RATE, 25.0, "after the resume");
    run_to(&mut s, leaves - samples(0.05));
    let sound = in_step(&mut s, RATE, 25.0, "the end of the pass");
    assert!((sound - 0.75).abs() < 0.01, "the sound at {sound} s");
    run_to(&mut s, leaves + 2 * BLOCK);
    expect_state(&s, 1, CueState::Idle, "both ended at the end of the pass");
    assert_eq!(shown(&mut s), None);
    s.run(samples(0.1));
    assert!(silent(&s, leaves + 8, s.rendered()));
    assert_eq!(s.stalls(), 0);
}

/// A stop with a fade longer than what is left of the pass, while a devamp that stops the cue
/// is pending: the picture leaves at the end of the pass, with its sound, not after the fade.
#[test]
fn a_stop_fading_past_a_pending_devamp_stop_ends_the_picture_with_the_pass() {
    let dir = tempfile::tempdir().unwrap();
    let movie = sound_movie!(dir.path(), "clip.mov", (50, 25), "pcm:2:48000");
    let looping = timed_sounding_cue(1, &movie, |t| {
        t.end_time = Some(0.8);
        t.last_slice_play_count = 0.into();
    });
    let devamp = cue(
        2,
        CueKind::Devamp {
            target: Some(id(1)),
            start_next_cue: false,
            stop_target_when_slice_ends: true,
            devamp_mode: openlab_core::DevampMode::CurrentSlice,
        },
    );
    let mut s = video_show(vec![looping, devamp]);
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(1.0));
    start(&mut s, 2); // the pass under way ends at 1.6 s
    run_to(&mut s, at + samples(1.1));
    s.transport(TransportAction::Stop {
        cue: id(1),
        fade_out: Some(openlab_core::FadeSpec {
            duration: 10.0,
            curve: openlab_core::FadeCurve::Linear,
        }),
    });
    run_to(&mut s, at + samples(1.5));
    on_stage(&mut s, "fading, before the end of the pass");
    assert!(
        !silent(&s, at + samples(1.4), s.rendered()),
        "the sound fades"
    );
    let leaves = at + samples(1.6);
    run_to(&mut s, leaves + 2 * BLOCK);
    assert_eq!(shown(&mut s), None, "gone at the end of the pass");
    assert_eq!(s.control().pictures_live(), 0);
    assert!(
        silent(&s, leaves + 8, s.rendered()),
        "the sound ended there"
    );
    run_to(&mut s, at + samples(11.1) + 2 * BLOCK);
    expect_state(&s, 1, CueState::Idle, "after the fade");
    assert_eq!(s.control().pictures_live(), 0);
}

/// A movie with sound and a skipped slice: sound and picture go over it on the same sample,
/// nothing of it heard or shown; a devamp of the loop before it goes on to the slice after it
/// for both; and a load to a time counts only what plays, for both (a Video cue takes no seek
/// to a point of its file, so no point inside the skipped slice can be asked for).
#[test]
fn a_skipped_slice_keeps_picture_and_sound_together_played_devamped_and_loaded() {
    use openlab_core::{PlayCount, SliceMarker};
    let dir = tempfile::tempdir().unwrap();
    let movie = sound_movie!(dir.path(), "clip.mov", (50, 25), "pcm:2:48000");
    // 0–0.4 s as `first` says, 0.4–1.2 s skipped, 1.2–2 s once.
    let sliced = |n: u128, first: PlayCount| {
        timed_sounding_cue(n, &movie, move |t| {
            t.slices = vec![
                SliceMarker {
                    time: 0.4,
                    play_count: first,
                },
                SliceMarker {
                    time: 1.2,
                    play_count: PlayCount::Skip,
                },
            ];
        })
    };
    let mut s = video_show(vec![sliced(1, PlayCount::ONCE)]);
    assert_eq!(told_lengths(&s, 1), (Some(1.2), Some(1.2)));
    let at = go(&mut s);
    let seam = at + samples(0.4);
    run_to(&mut s, seam - samples(0.1));
    in_step(&mut s, RATE, 25.0, "before the skipped slice");
    run_to(&mut s, seam + samples(0.1));
    let sound = in_step(&mut s, RATE, 25.0, "after the skipped slice");
    assert!((sound - 1.3).abs() < 0.01, "{sound}");
    sound_is(&s, seam - 16, 0.398, "the end of the first slice");
    sound_is(&s, seam + 16, 1.202, "the start of the last slice");
    let end = at + samples(1.2);
    run_to(&mut s, end + 2 * BLOCK);
    expect_state(&s, 1, CueState::Idle, "what plays is played");
    assert_eq!(shown(&mut s), None);
    s.run(samples(0.1));
    assert!(silent(&s, end + 8, s.rendered()));

    // A load to 0.6 s of the action is 0.2 s into the last slice.
    s.transport(TransportAction::Load {
        cue: id(1),
        at: 0.6,
    });
    s.run(BLOCK);
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(0.1));
    sound_is(
        &s,
        at + 40,
        1.4 + 40.0 / f64::from(RATE),
        "loaded to a time",
    );
    in_step(&mut s, RATE, 25.0, "after a load to time");
    run_to(&mut s, at + samples(0.6) + 2 * BLOCK);
    expect_state(&s, 1, CueState::Idle, "");

    // The first slice loops until devamped: the devamp goes over the skipped slice.
    let mut s = video_show(vec![sliced(1, PlayCount::Endless), devamp_cue(2)]);
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(0.9));
    in_step(&mut s, RATE, 25.0, "in the third pass");
    start(&mut s, 2);
    let leaves = at + samples(1.2);
    run_to(&mut s, leaves + samples(0.2));
    sound_is(&s, leaves - 16, 0.398, "the end of the last pass");
    sound_is(
        &s,
        leaves + 16,
        1.202,
        "into the slice after the skipped one",
    );
    let sound = in_step(&mut s, RATE, 25.0, "after the devamp");
    assert!((sound - 1.4).abs() < 0.01, "{sound}");
    let end = at + samples(2.0);
    run_to(&mut s, end + 2 * BLOCK);
    expect_state(&s, 1, CueState::Idle, "ended after the last slice");
    assert_eq!(shown(&mut s), None);
    assert_eq!(s.stalls(), 0);
}

/// A Video cue's integrated fades shape its sound as an Audio cue's: half way into a linear
/// fade in and half way through the fade out the sound is at half its level, full between
/// them, while the picture's opacity follows the same fades.
#[test]
fn integrated_fades_shape_the_sound_with_the_picture() {
    let dir = tempfile::tempdir().unwrap();
    let movie = sound_movie!(dir.path(), "clip.mov", (50, 25), "pcm:2:48000");
    let linear = |duration| openlab_core::FadeSpec {
        duration,
        curve: openlab_core::FadeCurve::Linear,
    };
    let mut s = video_show(vec![timed_sounding_cue(1, &movie, |t| {
        t.fade_in = linear(0.5);
        t.fade_out = linear(0.5);
    })]);
    let at = go(&mut s);
    // The level of the sound `t` seconds after GO: output 1 holds the second's number, one
    // over 2048 a step, at unity.
    let level = |s: &OfflineShow, t: f64| {
        let (_, second) = outputs_at(s, at + samples(t));
        f64::from(second) * 2048.0 / (t.floor() + 1.0)
    };
    run_to(&mut s, at + samples(1.0));
    let full = red_at(&mut s);
    run_to(&mut s, at + samples(1.75) + BLOCK);
    let half_out = red_at(&mut s);
    assert!(
        (f64::from(half_out) / f64::from(full) - 0.5).abs() <= 0.08,
        "the picture half way through the fade out: {half_out} of {full}"
    );
    run_to(&mut s, at + samples(2.0) + 2 * BLOCK);
    for (t, want) in [(0.25, 0.5), (0.75, 1.0), (1.25, 1.0), (1.75, 0.5)] {
        let got = level(&s, t);
        assert!(
            (got - want).abs() < 0.01,
            "{t} s after GO the sound is at {got} of its level, not {want}"
        );
    }
    expect_state(&s, 1, CueState::Idle, "");
}

/// Flow 8 (e): GO while the file's probe is still pending → picture and sound start late,
/// together, from the top.
#[test]
fn go_while_the_probe_is_pending_starts_picture_and_sound_late_together() {
    let dir = tempfile::tempdir().unwrap();
    let silent_movie = movie!(dir.path(), "a.mov", (50, 25));
    let movie = sound_movie!(dir.path(), "b.mov", (75, 25), "pcm:2:48000");
    let mut s = gated_video_show(vec![sounding_cue(1, &silent_movie)]);
    GATED.lock().unwrap().push(movie.clone());
    set_media(&mut s, 1, &movie);
    let at = go(&mut s);
    s.run(samples(0.5));
    expect_state(&s, 1, CueState::Running, "waits for its probe");
    assert!(
        silent(&s, at, s.rendered()),
        "no sound before the file is read"
    );
    release_probe(&movie);
    s.settle_media();
    let released = s.now();
    run_to(&mut s, released + samples(1.0));
    let sound = in_step(&mut s, RATE, 25.0, "started late");
    assert!(
        (sound - 1.0).abs() < 0.03,
        "{sound} s, a second after the late start"
    );
    run_to(&mut s, released + samples(3.0) + samples(0.1));
    expect_state(
        &s,
        1,
        CueState::Idle,
        "its whole length from the late start",
    );
}

/// Flow 9: Panic fades the sound and the picture over the panic time and both are gone at the
/// same moment; a second Panic cuts both at once; a hard stop cuts the sound on the sample the
/// picture leaves; another playing Video cue is untouched by a cue's own panic; GO during the
/// panic fade starts picture and sound again from the top.
#[test]
fn stop_and_panic_fade_the_sound_with_the_picture() {
    let dir = tempfile::tempdir().unwrap();
    let movie = sound_movie!(dir.path(), "clip.mov", (100, 25), "pcm:2:48000");
    let mut ws: Workspace = workspace(vec![sounding_cue(1, &movie), sounding_cue(2, &movie)]);
    ws.settings.panic_duration = 0.5;
    let mut s = video_show_of(ws);
    let at = go(&mut s);
    run_to(&mut s, at + samples(1.0));
    s.transport(TransportAction::Panic);
    let panicked = s.now();
    // Half way: both still there, the sound quieter.
    run_to(&mut s, panicked + samples(0.25));
    let red = red_at(&mut s);
    assert!((60..200).contains(&red), "the picture is fading: {red}");
    let level = s.audio()[usize::try_from(s.rendered() - 1).unwrap() * 2 + 1];
    assert!(
        level > 0.0 && level < 2.0 / 2048.0,
        "the sound is fading: {level}"
    );
    run_to(&mut s, panicked + samples(0.5) + 2 * BLOCK);
    expect_state(&s, 1, CueState::Idle, "");
    assert_eq!(shown(&mut s), None);
    s.run(samples(0.1));
    assert!(silent(&s, panicked + samples(0.5), s.rendered()));
    assert_eq!(s.control().live_voices(), 0);

    // GO during a panic fade: both start again from the top.
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(0.5));
    s.transport(TransportAction::Panic);
    s.run(samples(0.1));
    let again = start(&mut s, 1);
    run_to(&mut s, again + samples(0.3));
    expect_state(&s, 1, CueState::Running, "started again");
    run_to(&mut s, again + samples(1.0));
    let sound = in_step(&mut s, RATE, 25.0, "restarted in the panic fade");
    assert!((sound - 1.0).abs() < 0.01, "{sound}");

    // A cue's own panic leaves another Video cue's picture and sound alone; Panic twice cuts.
    let other = start(&mut s, 2);
    run_to(&mut s, other + samples(0.3));
    s.transport(TransportAction::PanicCue { cue: id(1) });
    run_to(&mut s, other + samples(1.0));
    expect_state(&s, 1, CueState::Idle, "");
    expect_state(&s, 2, CueState::Running, "untouched");
    sound_is(
        &s,
        s.rendered() - 1,
        (s.rendered() - 1 - other) as f64 / f64::from(RATE),
        "cue 2",
    );
    s.transport(TransportAction::Panic);
    s.transport(TransportAction::Panic);
    let cut = s.now();
    s.run(samples(0.1));
    expect_state(&s, 2, CueState::Idle, "cut at once");
    assert_eq!(shown(&mut s), None);
    assert!(silent(&s, cut + BLOCK, s.rendered()));

    // A hard stop: the sound is cut on the sample the picture leaves.
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(0.5));
    s.transport(TransportAction::Stop {
        cue: id(1),
        fade_out: None,
    });
    let stopped = s.now();
    s.run(samples(0.1));
    expect_state(&s, 1, CueState::Idle, "");
    assert_eq!(shown(&mut s), None);
    assert!(sound_at(&s, stopped - 1).is_some(), "heard up to the stop");
    assert!(silent(&s, stopped, s.rendered()));
    assert_eq!(s.control().live_voices(), 0);
}

/// Sets fields of cue `n`'s kind as the app does (`kind_fields`); whether it was accepted.
fn set_fields(s: &mut OfflineShow, n: u128, fields: serde_json::Value) -> bool {
    set_geometry(s, n, fields).iter().any(|m| {
        matches!(
            m,
            ServerMessage::Result {
                outcome: Outcome::Ok(_),
                ..
            }
        )
    })
}

/// The two outputs' values on the last rendered sample.
fn heard(s: &OfflineShow) -> (f32, f32) {
    outputs_at(s, s.rendered() - 1)
}

/// Flow 5: a level typed and an output muted while the cue plays (and while it is paused) are
/// heard at once (on resume), and the picture plays on without a jump; undo puts them back.
#[test]
fn levels_and_mutes_change_the_sound_live_while_the_picture_plays_on() {
    let dir = tempfile::tempdir().unwrap();
    let movie = sound_movie!(dir.path(), "clip.mov", (100, 25), "pcm:2:48000");
    let mut ws: Workspace = workspace(vec![sounding_cue(1, &movie)]);
    ws.settings.output_max_db = 6.0;
    let mut s = video_show_of(ws);
    let at = go(&mut s);
    run_to(&mut s, at + samples(0.5));
    let levels = |master: f64, mute: bool| {
        serde_json::json!({"levels": {
            "master_db": master,
            "outputs": [{"db": 0.0, "mute": mute}, {"db": 0.0, "mute": false}],
            "crosspoints": [
                {"input": 0, "output": 0, "db": 0.0, "mute": false},
                {"input": 1, "output": 1, "db": 0.0, "mute": false}],
            "gangs": []}})
    };
    // The main level down 6 dB: the second's number on output 1 is heard at half its value.
    assert!(set_fields(&mut s, 1, levels(-6.0206, false)));
    s.run(samples(0.2));
    let (_, second) = heard(&s);
    assert!((second * 2048.0 - 0.5).abs() < 0.01, "{second}");
    let frame = on_stage(&mut s, "playing on");
    assert_eq!(frame, frame_at((s.now() - at) as f64 / f64::from(RATE)));
    expect_state(&s, 1, CueState::Running, "");
    // Output 0 muted: nothing there, output 1 as before.
    assert!(set_fields(&mut s, 1, levels(-6.0206, true)));
    s.run(samples(0.2));
    let (rise, second) = heard(&s);
    assert!(rise == 0.0 && second > 0.0, "{rise} {second}");
    // A level above the workspace's limit is brought to it, as an Audio cue's.
    assert!(set_fields(&mut s, 1, levels(10.0, true)));
    let stored = s
        .control()
        .workspace()
        .find_cue(id(1))
        .unwrap()
        .kind
        .levels()
        .unwrap()
        .master_db;
    assert_eq!(stored, 6.0);
    // While paused an edit is heard on the resume.
    s.transport(TransportAction::Pause { cue: id(1) });
    s.run(samples(0.1));
    assert!(set_fields(&mut s, 1, levels(0.0, false)));
    s.transport(TransportAction::Resume { cue: id(1) });
    s.run(samples(0.2));
    let position = sound_at(&s, s.rendered() - 1);
    assert!(position.is_some(), "unity again: the position reads true");
    in_step(&mut s, RATE, 25.0, "after edits and a pause");
    // Undo: the levels before the last edit.
    let request = s.request_id();
    s.send(ClientMessage::Undo { id: request });
    let stored = s
        .control()
        .workspace()
        .find_cue(id(1))
        .unwrap()
        .kind
        .levels()
        .unwrap()
        .master_db;
    assert_eq!(stored, 6.0);
}

/// Flow 6: a Fade cue targeting a Video cue with sound fades its sound along the Fade's time
/// while the picture plays on; with "stop target when done" picture and sound end with it.
#[test]
fn a_fade_cue_fades_a_video_cues_sound() {
    let dir = tempfile::tempdir().unwrap();
    let movie = sound_movie!(dir.path(), "clip.mov", (100, 25), "pcm:2:48000");
    let fade = |n: u128, db: f64, stop: bool| {
        cue(
            n,
            CueKind::Fade {
                target: Some(id(1)),
                duration: 0.5,
                curve: openlab_core::FadeCurve::Linear,
                levels: openlab_core::FadeLevels {
                    master_db: Some(db),
                    output_db: vec![],
                    crosspoints: vec![],
                },
                stop_target_when_done: stop,
                mode: Default::default(),
            },
        )
    };
    let mut s = video_show(vec![
        sounding_cue(1, &movie),
        fade(2, -60.0, false),
        fade(3, 0.0, false),
        fade(4, -60.0, true),
    ]);
    assert_eq!(problems(&s, 2), vec![], "a Video cue is a Fade's target");
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(0.5));
    let faded = start(&mut s, 2);
    run_to(&mut s, faded + samples(0.25));
    let (_, mid) = heard(&s);
    assert!(mid > 0.0 && mid < 1.0 / 2048.0, "part way down: {mid}");
    run_to(&mut s, faded + samples(0.6));
    let (_, low) = heard(&s);
    assert!(
        low * 2048.0 < 0.01,
        "at the global minimum the sound is gone: {low}"
    );
    // The picture played on at its own time, and the cue runs.
    expect_state(&s, 1, CueState::Running, "");
    let frame = on_stage(&mut s, "the picture plays on");
    assert_eq!(frame, frame_at((s.now() - at) as f64 / f64::from(RATE)));
    // A second Fade back up.
    let up = start(&mut s, 3);
    run_to(&mut s, up + samples(0.6));
    in_step(&mut s, RATE, 25.0, "faded back up");
    // Stop target when done: picture and sound end when the Fade does.
    let last = start(&mut s, 4);
    run_to(&mut s, last + samples(0.5) + 2 * BLOCK);
    expect_state(&s, 1, CueState::Idle, "stopped by the Fade");
    assert_eq!(shown(&mut s), None);
    assert_eq!(s.control().live_voices(), 0);
}

/// Flow 2 and 3 (engine): a movie without sound and a still have no voice and are never broken
/// by patches; a movie with sound and no patch (or a patch with no device) is broken, GO starts
/// nothing, and choosing a patch that plays clears it.
#[test]
fn sound_but_nowhere_to_play_it_marks_the_cue_broken() {
    let dir = tempfile::tempdir().unwrap();
    let quiet = movie!(dir.path(), "quiet.mov", (50, 25));
    let loud = sound_movie!(dir.path(), "loud.mov", (50, 25), "pcm:2:48000");
    let png = make(dir.path(), "card.png", "png", None);
    let nowhere = PatchId(Uuid::from_u128(77));
    let mut ws: Workspace = workspace(vec![
        video_cue(1, Some(&quiet)),
        still_cue(2, &png, None),
        video_cue(3, Some(&loud)),
    ]);
    // A second patch, mapped to a device this machine does not have.
    let mut unmapped = ws.patches[0].clone();
    unmapped.id = nowhere;
    ws.patches.push(unmapped);
    let workspace_id = ws.id;
    let mut s = video_show_config(
        ws,
        &[],
        |_| {},
        |c| {
            let gone = openlab_core::PatchMapping {
                device: "Gone".into(),
                channels: vec![0, 1],
                system: false,
            };
            c.machine
                .set_patch_mapping(workspace_id, nowhere, gone)
                .unwrap();
        },
    );
    assert_eq!(problems(&s, 1), vec![], "a silent movie needs no patch");
    assert_eq!(problems(&s, 2), vec![], "nor a still");
    assert_eq!(problems(&s, 3), vec![CueProblem::NoPatch]);
    let told = s.control().snapshot().media_durations;
    let of = |n: u128| told.iter().find(|m| m.cue == id(n)).unwrap().clone();
    assert!(of(1).audio_tracks.is_empty() && of(1).channels.is_none());
    assert_eq!(of(3).audio_tracks.len(), 1);
    assert_eq!((of(3).channels, of(3).audio_track), (Some(2), Some(0)));
    assert_eq!(of(3).audio_tracks[0].codec, "lpcm");
    // GO on the broken cue starts nothing: no picture, no sound.
    start(&mut s, 3);
    s.run(samples(0.3));
    assert_eq!(shown(&mut s), None);
    assert_eq!(s.control().live_voices(), 0);
    assert_ne!(state(&s, 3), CueState::Running);
    // A silent movie plays its picture with no voice.
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(0.5));
    assert!(shown(&mut s).is_some());
    assert_eq!(s.control().live_voices(), 0);
    assert!(silent(&s, 0, s.rendered()));
    run_to(&mut s, at + samples(2.1));
    // A patch with no device: the device problem an Audio cue on it shows.
    assert!(set_fields(&mut s, 3, serde_json::json!({"patch": nowhere})));
    assert!(
        matches!(problems(&s, 3)[..], [CueProblem::DeviceUnavailable { patch, .. }] if patch == nowhere),
        "{:?}",
        problems(&s, 3)
    );
    // A patch that is not in the workspace at all.
    assert!(set_fields(
        &mut s,
        3,
        serde_json::json!({"patch": Uuid::from_u128(99)})
    ));
    assert!(matches!(
        problems(&s, 3)[..],
        [CueProblem::PatchMissing { .. }]
    ));
    // The workspace's patch: the mark clears and the cue plays with sound.
    assert!(set_fields(
        &mut s,
        3,
        serde_json::json!({"patch": patch_id(), "levels": straight_levels()})
    ));
    assert_eq!(problems(&s, 3), vec![]);
    let at = start(&mut s, 3);
    run_to(&mut s, at + samples(0.5));
    in_step(&mut s, RATE, 25.0, "with a patch");
    // Replacing the file by a silent one: no patch problem whatever the patch, and back.
    assert!(set_fields(&mut s, 3, serde_json::json!({"patch": null})));
    s.transport(TransportAction::Stop {
        cue: id(3),
        fade_out: None,
    });
    set_media(&mut s, 3, &quiet);
    s.settle_media();
    s.run(BLOCK);
    assert_eq!(problems(&s, 3), vec![]);
    set_media(&mut s, 3, &loud);
    s.settle_media();
    s.run(BLOCK);
    assert_eq!(problems(&s, 3), vec![CueProblem::NoPatch]);
}

/// The sound is the clock: a voice the disk starved (its file's open stalled) plays late by
/// what it missed, its picture is put back by as much, and the cue ends when the sound does.
#[test]
fn a_starved_sound_puts_its_picture_back_so_they_stay_together() {
    let dir = tempfile::tempdir().unwrap();
    let movie = sound_movie!(dir.path(), "clip.mov", (100, 25), "pcm:2:48000");
    let ws = workspace(vec![sounding_cue(1, &movie)]);
    let mut s = video_show_config(ws, &[], |_| {}, |c| c.streamer.before_open = gated_open);
    gate(&movie);
    let at = go(&mut s);
    s.run_unsettled(samples(0.4));
    assert!(
        silent(&s, at, s.rendered()),
        "nothing heard while the disk stalls"
    );
    release(&movie);
    s.run(samples(0.6));
    let sound = in_step(&mut s, RATE, 25.0, "after the disk caught up");
    let late = (s.now() - at) as f64 / f64::from(RATE) - sound;
    assert!(late > 0.3, "the sound started {late} s late");
    // Its whole length from where the sound really began.
    let end = at + samples(4.0 + late);
    run_to(&mut s, end - samples(0.1));
    expect_state(
        &s,
        1,
        CueState::Running,
        "the cue lasts as long as its sound",
    );
    in_step(&mut s, RATE, 25.0, "near the end");
    run_to(&mut s, end + samples(0.1));
    expect_state(&s, 1, CueState::Idle, "");
    assert_eq!(shown(&mut s), None);
}

/// The sound comes back from a starved disk and the cue is paused before the engine has heard
/// of it: the paused picture is put back to where its sound is, the resume plays both on in
/// step, and the cue lasts until the sound has played to its end.
#[test]
fn a_pause_as_the_starved_sound_comes_back_still_puts_the_picture_back() {
    let dir = tempfile::tempdir().unwrap();
    let movie = sound_movie!(dir.path(), "clip.mov", (100, 25), "pcm:2:48000");
    let ws = workspace(vec![sounding_cue(1, &movie)]);
    let mut s = video_show_config(ws, &[], |_| {}, |c| c.streamer.before_open = gated_open);
    gate(&movie);
    let at = go(&mut s);
    s.run_unsettled(samples(0.4));
    release(&movie);
    // The block the sound comes back in is rendered, and the pause is asked before the engine
    // reads that block's report.
    s.render_only(BLOCK);
    s.transport(TransportAction::Pause { cue: id(1) });
    let paused = s.now();
    s.run(samples(0.3));
    expect_state(&s, 1, CueState::Paused, "");
    let late = (paused - at) as f64 / f64::from(RATE);
    assert!(late > 0.35, "the sound came back {late} s after GO");
    // Held on the frame its sound had reached: the start of the movie, not 0.4 s in.
    assert!(on_stage(&mut s, "paused") <= 1, "{:?}", shown(&mut s));
    s.transport(TransportAction::Resume { cue: id(1) });
    let waited = s.now() - paused;
    for _ in 0..5 {
        s.run(samples(0.1));
        in_step(&mut s, RATE, 25.0, "after the resume");
    }
    // The whole four seconds of sound are heard: the cue ends when they have played.
    let end = paused + waited + samples(4.0);
    run_to(&mut s, end - samples(0.1));
    expect_state(&s, 1, CueState::Running, "the sound has not ended yet");
    in_step(&mut s, RATE, 25.0, "near the end");
    run_to(&mut s, end + samples(0.1));
    expect_state(&s, 1, CueState::Idle, "");
    assert_eq!(shown(&mut s), None);
    s.run(samples(0.1));
    let last = last_nonzero(&s.channel(1)).unwrap() as u64;
    // 200 samples before the last one heard the sound is that far from the movie's end
    // (looked at clear of the resampler's tail at the very end).
    let heard = sound_at(&s, last - 200).expect("sound near the end");
    assert!(
        (heard - 3.975).abs() < 0.003,
        "the sound ends at the movie's end: {heard} s, 200 samples before its last"
    );
}

/// `bytes` with the first `find` replaced by `with` (the same length).
fn patched(bytes: &[u8], find: &[u8], at: usize, with: &[u8]) -> Vec<u8> {
    let found = bytes
        .windows(find.len())
        .position(|w| w == find)
        .expect("the bytes to patch");
    let mut out = bytes.to_vec();
    out.get_mut(found + at..found + at + with.len())
        .expect("room for the patch")
        .copy_from_slice(with);
    out
}

/// Flow 11 (a), (c): a movie whose audio track is in a codec the engine cannot decode, or has
/// more channels than a cue can route, breaks its cue at the probe with a reason naming the
/// track; GO does not start it (no silent picture) and other cues play.
#[test]
fn a_damaged_or_unplayable_audio_track_breaks_only_that_cue() {
    let dir = tempfile::tempdir().unwrap();
    let good = sound_movie!(dir.path(), "good.mov", (50, 25), "pcm:2:48000");
    let bytes = std::fs::read(&good).unwrap();
    // The sample description's format code, and its channel count (a 32-bit number 44 bytes
    // after the code in a QuickTime sound description of version 2).
    let codec = dir.path().join("codec.mov");
    std::fs::write(&codec, patched(&bytes, b"lpcm", 0, b"zzzz")).unwrap();
    let wide = dir.path().join("wide.mov");
    std::fs::write(&wide, patched(&bytes, b"lpcm", 44, &65u32.to_be_bytes())).unwrap();
    let mut s = video_show(vec![
        sounding_cue(1, &codec),
        sounding_cue(2, &wide),
        sounding_cue(3, &good),
    ]);
    for n in [1, 2] {
        let found = problems(&s, n);
        assert!(
            matches!(&found[..], [CueProblem::MediaUnsupported { reason, .. }]
                if reason.contains("audio track 1")),
            "cue {n}: {found:?}"
        );
        start(&mut s, n);
        s.run(samples(0.2));
        assert_ne!(state(&s, n), CueState::Running, "cue {n} does not start");
        assert_eq!(shown(&mut s), None, "no silent picture");
    }
    assert_eq!(problems(&s, 3), vec![]);
    let at = start(&mut s, 3);
    run_to(&mut s, at + samples(0.5));
    in_step(&mut s, RATE, 25.0, "the good cue plays");
}

/// Flow 11 (b): a movie that is damaged part-way while it plays (its file cut short under the
/// engine) plays to the damage; then that cue alone breaks and its picture leaves the stage,
/// while another Video cue's picture and sound go on and the stage host keeps running.
#[test]
fn damage_met_mid_play_breaks_that_cue_and_the_others_play_on() {
    let dir = tempfile::tempdir().unwrap();
    let cut = sound_movie!(dir.path(), "cut.mov", (250, 25), "pcm:2:48000");
    let whole = sound_movie!(dir.path(), "whole.mov", (250, 25), "pcm:2:48000");
    let mut s = video_show(vec![sounding_cue(1, &cut), sounding_cue(2, &whole)]);
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(0.5));
    in_step(&mut s, RATE, 25.0, "before the damage");
    let length = std::fs::metadata(&cut).unwrap().len();
    let file = std::fs::OpenOptions::new().write(true).open(&cut).unwrap();
    file.set_len(length / 2).unwrap();
    drop(file);
    let other = start(&mut s, 2);
    // It plays on to the damage (the movie is ten seconds long), then breaks.
    let mut played = 0;
    while state(&s, 1) != CueState::Broken && played < 100 {
        s.run(samples(0.1));
        played += 1;
    }
    expect_state(&s, 1, CueState::Broken, "the cut movie's cue breaks");
    assert!(played > 5, "it played to the damage first");
    s.run(samples(0.2));
    expect_state(&s, 2, CueState::Running, "the other cue plays on");
    assert_eq!(
        s.control().pictures_live(),
        1,
        "only the other picture is up"
    );
    let sound = in_step(&mut s, RATE, 25.0, "the other cue, in step");
    let played = (s.now() - other) as f64 / f64::from(RATE);
    assert!((sound - played).abs() < 0.01, "{sound} s after {played} s");
}

/// Flow 11 (d): seeded-mutated movies with sound (a walk over the whole file, and one over its
/// index at the start) each give the audio probe an answer, tracks or a reason: never a panic
/// or a hang.
#[test]
fn mutated_movies_with_sound_give_the_audio_probe_an_answer() {
    let dir = tempfile::tempdir().unwrap();
    let kind = "h264+aac:2:44100+pcm:2:48000";
    let good = std::fs::read(make(dir.path(), "good.mov", kind, Some((25, 25)))).unwrap();
    let mut seed: u64 = 0x50d5;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut answers = (0, 0);
    for round in 0..64u64 {
        let mut bytes = good.clone();
        // Every other round stays in the first 4 KiB, where the tracks are described.
        let span = if round % 2 == 0 { bytes.len() } else { 4096 } as u64;
        for _ in 0..(1 + round % 8) {
            let at = (next() % span) as usize;
            if let Some(b) = bytes.get_mut(at) {
                *b = next() as u8;
            }
        }
        let file = dir.path().join(format!("m{round}.mov"));
        std::fs::write(&file, &bytes).unwrap();
        match openlab_audio::probe_movie(&file) {
            Ok(tracks) => {
                answers.0 += 1;
                // A track that probed plays to its end without a panic too.
                for (n, t) in tracks.iter().enumerate() {
                    if t.probed.is_ok() {
                        let media = openlab_audio::Media::Track(openlab_audio::MovieTrack {
                            path: file.clone(),
                            track: n as u32,
                            fit: None,
                        });
                        let _ = openlab_audio::preload(media);
                    }
                }
            }
            Err(_) => answers.1 += 1,
        }
    }
    assert_eq!(answers.0 + answers.1, 64);
}

/// Flow 4 (engine): a movie with two audio tracks plays the one its cue chose, clients are told
/// the tracks and the one played, and a choice the file does not have plays the first track
/// with a warning.
#[test]
fn a_movie_with_several_audio_tracks_plays_the_one_chosen() {
    let dir = tempfile::tempdir().unwrap();
    let movie = sound_movie!(dir.path(), "two.mov", (75, 25), "pcm:2:48000+pcm:6:44100");
    let mut second = sounding_cue(1, &movie);
    if let CueKind::Video { sound, .. } = &mut second.kind {
        sound.audio_track = 1;
    }
    let mut s = video_show(vec![second]);
    assert_eq!(problems(&s, 1), vec![]);
    let told = |s: &OfflineShow| {
        let all = s.control().snapshot().media_durations;
        let m = all.iter().find(|m| m.cue == id(1)).unwrap().clone();
        let formats: Vec<(u16, u32)> = (m.audio_tracks.iter())
            .map(|t| (t.channels, t.sample_rate))
            .collect();
        (formats, m.audio_track, m.channels)
    };
    let both = vec![(2, 48_000), (6, 44_100)];
    assert_eq!(told(&s), (both.clone(), Some(1), Some(6)));
    let missing = |s: &OfflineShow| {
        (s.control().snapshot().warnings.iter()).any(|w| {
            matches!(w, openlab_core::protocol::WireWarning::Notice { code, cue, .. }
                if code == "audio_track_missing" && *cue == Some(id(1)))
        })
    };
    assert!(!missing(&s));
    // The second track's second-number channel is negative (the test movie's mark).
    let at = go(&mut s);
    run_to(&mut s, at + samples(0.5));
    assert!(
        heard(&s).1 < 0.0,
        "the second track is heard: {:?}",
        heard(&s)
    );
    s.transport(TransportAction::Stop {
        cue: id(1),
        fade_out: None,
    });
    s.run(BLOCK);
    // A track the file does not have: the first plays, with a warning.
    assert!(set_fields(&mut s, 1, serde_json::json!({"audio_track": 5})));
    s.run(samples(0.1));
    assert_eq!(told(&s), (both.clone(), Some(0), Some(2)));
    assert!(missing(&s), "the workspace warns");
    assert_eq!(problems(&s, 1), vec![]);
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(0.5));
    in_step(&mut s, RATE, 25.0, "the first track");
    // Undo returns the second.
    let request = s.request_id();
    s.send(ClientMessage::Undo { id: request });
    s.run(samples(0.1));
    assert_eq!(told(&s), (both, Some(1), Some(6)));
    assert!(!missing(&s));
}

/// Flow 7's measure over a movie `length` seconds long whose frames carry their number and
/// whose track says where it is: the frame on the stage and the sound in the rendered output
/// are less than one frame period apart at every look (the start, every `every` seconds, the
/// last second), at 24, 25, 30 and 60 frames a second and with a 48 kHz track on a 44.1 kHz
/// device; the cue ends once, the last frame and the sound's last sample together, and its
/// length told is the movie's.
fn in_step_throughout(length: u16, every: u16) {
    let dir = tempfile::tempdir().unwrap();
    // (frames a second, the track's rate, the device's rate and block)
    let cases: [(u16, u32, u32, u64); 5] = [
        (24, 8_000, RATE, BLOCK),
        (25, 8_000, RATE, BLOCK),
        (30, 8_000, RATE, BLOCK),
        (60, 8_000, RATE, BLOCK),
        (25, 48_000, 44_100, 512),
    ];
    let seconds = f64::from(length);
    for (fps, track, rate, block) in cases {
        let what = format!("{fps} fps, {track} Hz on {rate} Hz");
        let sound = format!("pcm:2:{track}");
        let movie = sound_movie!(dir.path(), "long.mov", (fps * length, fps), &sound);
        let ws = workspace(vec![sounding_cue(1, &movie)]);
        let mut s = video_show_rate(ws, rate, block, &[], |_| {}, |_| {});
        assert_eq!(
            told_lengths(&s, 1),
            (Some(seconds), Some(seconds)),
            "{what}"
        );
        let at = go(&mut s);
        let sample = |seconds: f64| at + (seconds * f64::from(rate)).round() as u64;
        let mut looks = vec![0.5];
        looks.extend(
            (every..length)
                .step_by(usize::from(every))
                .map(|t| f64::from(t) + 0.5),
        );
        looks.push(seconds - 0.5);
        for t in looks {
            s.run_until(sample(t));
            let heard = in_step(&mut s, rate, f64::from(fps), &format!("{what}, {t} s"));
            let played = (s.now() - at) as f64 / f64::from(rate);
            assert!(
                (heard - played).abs() < 0.002,
                "{what}: {heard} s after {played} s"
            );
        }
        expect_state(&s, 1, CueState::Running, &what);
        s.run_until(sample(seconds) + 2 * block);
        expect_state(&s, 1, CueState::Idle, &what);
        assert_eq!(shown(&mut s), None, "{what}");
        s.run(block * 8);
        let end = usize::try_from(sample(seconds)).unwrap();
        let last = last_nonzero(&s.channel(1)).unwrap();
        let frame = usize::try_from(u64::from(rate) / u64::from(fps)).unwrap();
        assert!(
            last < end + 8 && last + frame > end,
            "{what}: the last sample heard is {last}, the movie's end {end}"
        );
        assert_eq!(s.stalls(), 0, "{what}");
        std::fs::remove_file(&movie).unwrap();
    }
}

/// Flow 7 over thirty seconds of movie, looked at every five; the ten-minute run is
/// [`picture_and_sound_stay_within_one_frame_of_each_other_for_ten_minutes`].
#[test]
fn picture_and_sound_stay_within_one_frame_of_each_other() {
    in_step_throughout(30, 5);
}

/// Flow 7 as written: ten minutes of movie, looked at each minute. Minutes of rendering, so
/// not part of `cargo test`; CI runs it with the release performance gates (not on pull
/// requests):
///
/// `cargo test --release -p openlab-video --test video_cues -- --ignored ten_minutes`
#[test]
#[ignore = "ten minutes of movie at five rates: run in release with -- --ignored"]
fn picture_and_sound_stay_within_one_frame_of_each_other_for_ten_minutes() {
    in_step_throughout(600, 60);
}

/// A show of `ws` on stand-in audio devices (the first is the primary) with the real stage
/// host on a virtual display, each device `(id, name, rate)`; `mappings` name each patch's
/// device.
fn device_video_show(
    mut ws: Workspace,
    devices: &[(&str, &str, u32)],
    mappings: &[(PatchId, &str)],
) -> show::device::DeviceShow {
    let mut stage = Stage::new(stage_id(), "Stage 1", 1920, 1080);
    stage.keep_black = true;
    ws.stages = vec![stage];
    let mut c = ControlConfig::new(RATE, BLOCK);
    c.seed = 7;
    c.watch.mode = openlab_engine::control::watch::WatchMode::Manual;
    for (patch, device) in mappings {
        let mapping = openlab_core::PatchMapping {
            device: (*device).to_owned(),
            channels: vec![0, 1],
            system: false,
        };
        c.machine.set_patch_mapping(ws.id, *patch, mapping).unwrap();
    }
    // A patch moved to another device needs a slot there while its old one plays out.
    c.mixer_patches = 4;
    let mut host = StageHostConfig::new(PathBuf::from(STAGE_HOST));
    host.args = vec!["--virtual-displays".into(), "V1:1920x1080".into()];
    host.answer_within = SLOW_MACHINE;
    c.stage_host = Some(host);
    let listed = devices
        .iter()
        .map(|(id, name, rate)| show::device::listed_at(id, name, 2, *rate))
        .collect();
    show::device::DeviceShow::new(ws, listed, c)
}

/// Flow 10: with Video cue A (sound on device 1), B (sound on device 2) and a silent C all
/// playing, losing device 2 stops B as an Audio cue on it stops (shown paused, owning nothing,
/// its picture off the stage) while A and C play on; when the device is back nothing of B
/// resumes, its problem is gone, and a start plays it from the top. With one device, losing it
/// stops every Video cue.
#[test]
fn the_audio_device_is_lost_mid_cue() {
    let dir = tempfile::tempdir().unwrap();
    let loud = sound_movie!(dir.path(), "loud.mov", (250, 25), "pcm:2:48000");
    let quiet = movie!(dir.path(), "quiet.mov", (250, 25));
    let patch_b = PatchId(Uuid::from_u128(902));
    let mut b = sounding_cue(2, &loud);
    if let CueKind::Video { sound, .. } = &mut b.kind {
        sound.patch = Some(patch_b);
    }
    let cues = vec![sounding_cue(1, &loud), b, video_cue(3, Some(&quiet))];
    let mut ws: Workspace = workspace(cues.clone());
    let mut second = ws.patches[0].clone();
    second.id = patch_b;
    ws.patches.push(second);
    let two = [("fake:1", "Fake 1", RATE), ("fake:2", "Fake 2", RATE)];
    let mut s = device_video_show(ws, &two, &[(patch_id(), "Fake 1"), (patch_b, "Fake 2")]);
    s.wait_output("Fake 2");
    let state = |s: &show::device::DeviceShow, n: u128| s.control().engine().state(id(n));
    let sounding = |s: &show::device::DeviceShow, device: &str| {
        let heard = s.device_channel(device, 1);
        heard.iter().rev().take(64).any(|x| *x != 0.0)
    };
    for n in 1..=3 {
        assert!(s.control().engine().problems(id(n)).is_empty(), "cue {n}");
        s.transport(TransportAction::Start { cue: id(n) });
    }
    s.run(samples(0.5));
    assert_eq!(s.control().pictures_live(), 3);
    assert!(sounding(&s, "fake:1") && sounding(&s, "fake:2"));

    s.fake.unplug("fake:2", true);
    s.settle_devices();
    s.run(samples(0.3));
    assert_eq!(
        state(&s, 2),
        CueState::Paused,
        "B stops as an Audio cue does"
    );
    assert!(s.control().engine().is_lost(id(2)));
    assert_eq!(s.control().pictures_live(), 2, "B's picture left the stage");
    // B's time stands still where it stopped, as a paused cue's; A's runs on.
    let elapsed = |s: &mut show::device::DeviceShow, n: u128| -> Vec<f64> {
        let sent = s.take_outbound().into_iter().filter_map(|m| match m {
            ServerMessage::Progress { items, .. } => Some(items),
            _ => None,
        });
        let of_cue = sent.flatten().filter(|i| i.cue == id(n));
        of_cue.map(|i| i.elapsed).collect()
    };
    s.take_outbound();
    s.run(samples(0.2));
    let then = elapsed(&mut s, 2);
    s.run(samples(0.3));
    let later = elapsed(&mut s, 2);
    let stood = then.last().copied().expect("B's time is told");
    assert!(stood > 0.3 && stood < 0.8, "B stopped {stood} s in");
    assert!(!later.is_empty(), "B's time is still told");
    assert!(
        later.iter().all(|e| *e == stood),
        "B's time moved on from {stood}: {later:?}"
    );
    s.run(samples(0.1));
    let a = elapsed(&mut s, 1);
    assert!(a.last().is_some_and(|e| *e > 1.0), "A's time: {a:?}");
    assert_eq!(state(&s, 1), CueState::Running);
    assert_eq!(state(&s, 3), CueState::Running);
    assert!(sounding(&s, "fake:1"), "A's sound plays on");

    // The device is back: nothing of B resumes; a start plays it from the top.
    s.fake.plug(show::device::listed("fake:2", "Fake 2", 2));
    s.wait_wall(Duration::from_secs(1), 2);
    s.wait_output("Fake 2");
    s.run(samples(0.3));
    assert_eq!(s.control().pictures_live(), 2, "nothing resumes by itself");
    assert_ne!(state(&s, 2), CueState::Running);
    assert!(s.control().engine().problems(id(2)).is_empty());
    let before = s.device_channel("fake:2", 1).len();
    s.transport(TransportAction::Start { cue: id(2) });
    s.run(samples(0.5));
    assert_eq!(state(&s, 2), CueState::Running);
    assert_eq!(s.control().pictures_live(), 3);
    // From the top: the track's first second (its number, one over 2048) is heard.
    let heard = s.device_channel("fake:2", 1);
    let second = heard[before..].iter().rev().find(|x| **x != 0.0).unwrap();
    assert!((second * 2048.0 - 1.0).abs() < 0.01, "{second}");

    // One device only: losing it stops every Video cue, silent ones too.
    let one = [("fake:1", "Fake 1", RATE)];
    let mut s = device_video_show(workspace(cues), &one, &[(patch_id(), "Fake 1")]);
    for n in [1, 3] {
        s.transport(TransportAction::Start { cue: id(n) });
    }
    s.run(samples(0.5));
    assert_eq!(s.control().pictures_live(), 2);
    s.fake.unplug("fake:1", true);
    s.settle_devices();
    for _ in 0..8 {
        s.tick();
    }
    assert_ne!(state(&s, 1), CueState::Running);
    assert_ne!(state(&s, 3), CueState::Running);
    // The clock stands still while no device plays; once one does, the stopped pictures are
    // gone and nothing has started again.
    s.fake.plug(show::device::listed("fake:1", "Fake 1", 2));
    s.wait_wall(Duration::from_secs(1), 2);
    s.run(samples(0.3));
    assert_eq!(
        s.control().pictures_live(),
        0,
        "every picture left the stage"
    );
    assert_ne!(state(&s, 1), CueState::Running);
    assert_ne!(state(&s, 3), CueState::Running);
}

/// A Video cue's sound follows its patch to another device, as an Audio cue's voice does: the
/// patch is put on a device at another rate while the cue plays, and from the swap on the
/// sound is heard there at the point the master clock (and so the picture's plan) has reached,
/// at that device's rate; the picture stays up, and both end once at the movie's end. Started
/// again with its patch on that device, it plays there in step from the top.
#[test]
fn a_video_cues_sound_follows_its_patch_to_another_device() {
    const RATE2: u32 = 44_100;
    let dir = tempfile::tempdir().unwrap();
    let movie = sound_movie!(dir.path(), "clip.mov", (75, 25), "pcm:2:48000");
    let patch_b = PatchId(Uuid::from_u128(902));
    let mut ws: Workspace = workspace(vec![sounding_cue(1, &movie)]);
    let mut second = ws.patches[0].clone();
    second.id = patch_b;
    ws.patches.push(second);
    let two = [("fake:1", "Fake 1", RATE), ("fake:2", "Fake 2", RATE2)];
    let mut s = device_video_show(ws, &two, &[(patch_id(), "Fake 1"), (patch_b, "Fake 2")]);
    s.wait_output("Fake 2");
    assert!(s.control().engine().problems(id(1)).is_empty());
    let state = |s: &show::device::DeviceShow| s.control().engine().state(id(1));
    // The master sample device 2's frame `j` is played on, from both counters at one moment.
    let clocks = |s: &show::device::DeviceShow| {
        let local = s.control().output_mixer("Fake 2").unwrap();
        let shift = s.control().rendered() as f64
            - local.samples_rendered() as f64 * f64::from(RATE) / f64::from(RATE2);
        move |j: usize| shift + j as f64 * f64::from(RATE) / f64::from(RATE2)
    };
    // Where in its file the track heard on device 2's frame `j` is (as `sound_at`).
    let position = |s: &show::device::DeviceShow, j: usize| {
        let rise = s.device_channel("fake:2", 0)[j];
        let second = s.device_channel("fake:2", 1)[j];
        (f64::from(second) * 2048.0).round() - 1.0 + f64::from(rise) * 2.0
    };
    // Device 2's frames `from..to` play the file where the master clock is since `at`. Looked
    // at away from the whole seconds, where the resampled ramp and number step.
    let in_step = |s: &show::device::DeviceShow, at: u64, from: usize, to: usize, what: &str| {
        let master_of = clocks(s);
        let mut looked = 0;
        for j in (from..to).step_by(441) {
            let played = (master_of(j) - at as f64) / f64::from(RATE);
            if !(0.1..0.9).contains(&played.fract()) {
                continue;
            }
            let heard = position(s, j);
            assert!(
                (heard - played).abs() < 0.001,
                "{what}: device frame {j} plays {heard} s, {played} s after the start"
            );
            looked += 1;
        }
        assert!(looked > 30, "{what}: only {looked} looks");
    };

    let at = s.now();
    s.transport(TransportAction::Start { cue: id(1) });
    s.run(samples(1.0));
    assert!(first_nonzero(&s.device_channel("fake:2", 1)).is_none());
    let rid = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id: rid,
        request: Request::SetPatchMapping {
            patch: patch_id(),
            mapping: Some(openlab_core::PatchMapping {
                device: "Fake 2".into(),
                channels: vec![0, 1],
                system: false,
            }),
        },
    });
    let accepted = |m: &ServerMessage| {
        let ServerMessage::Result { outcome, .. } = m else {
            return false;
        };
        matches!(outcome, Outcome::Ok(_))
    };
    assert!(replies.iter().any(accepted), "{replies:?}");
    let swapped = s.device_channel("fake:1", 1).len();
    s.run(samples(1.0));
    assert_eq!(state(&s), CueState::Running);
    assert_eq!(s.control().pictures_live(), 1, "the picture stays up");
    let heard = s.device_channel("fake:2", 1);
    let first = first_nonzero(&heard).expect("heard on the second device");
    // Past the move's lead and crossfade: the first device is silent, the second in step.
    let one = s.device_channel("fake:1", 1);
    let last_one = last_nonzero(&one).unwrap();
    assert!(
        last_one < swapped + samples(0.2) as usize,
        "the first device plays on to {last_one}, the move was at {swapped}"
    );
    in_step(&s, at, first + 441, heard.len(), "after the move");
    // The moved run is still its picture's: a pause holds both, a resume plays both on.
    s.transport(TransportAction::Pause { cue: id(1) });
    let paused = s.now();
    s.run(samples(0.5));
    assert_eq!(state(&s), CueState::Paused);
    assert_eq!(s.control().pictures_live(), 1);
    let quiet_from = s.device_channel("fake:2", 1).len() - 4_410;
    let heard = s.device_channel("fake:2", 1);
    assert!(
        heard[quiet_from..].iter().all(|x| *x == 0.0),
        "silent while paused"
    );
    s.transport(TransportAction::Resume { cue: id(1) });
    let waited = s.now() - paused;
    let resumed = s.device_channel("fake:2", 1).len();
    s.run(samples(0.6));
    assert_eq!(state(&s), CueState::Running);
    let heard = s.device_channel("fake:2", 1);
    in_step(
        &s,
        at + waited,
        resumed + 4_410,
        heard.len(),
        "after the resume",
    );
    // Both end once, at the movie's end on the master clock.
    s.run(samples(0.7));
    assert_eq!(state(&s), CueState::Idle);
    assert_eq!(s.control().pictures_live(), 0);
    assert_eq!(s.control().live_voices(), 0);
    let last = last_nonzero(&s.device_channel("fake:2", 1)).unwrap();
    let end = clocks(&s)(last + 1) - (at + waited) as f64;
    assert!(
        (end - samples(3.0) as f64).abs() <= 2.0,
        "the sound ends {end} frames after the start"
    );

    // From the top on the second device: in step throughout, and one end.
    let before = s.device_channel("fake:2", 1).len();
    let at = s.now();
    s.transport(TransportAction::Start { cue: id(1) });
    s.run(samples(2.5));
    assert_eq!(state(&s), CueState::Running);
    assert_eq!(s.control().pictures_live(), 1);
    let heard = s.device_channel("fake:2", 1);
    in_step(&s, at, before + 4_410, heard.len(), "started there");
    s.run(samples(0.8));
    assert_eq!(state(&s), CueState::Idle);
    assert_eq!(s.control().pictures_live(), 0);
    assert_eq!(s.control().live_voices(), 0);
}

/// The waveform of a Video cue with sound is that of the audio track it plays, over the
/// movie's length (the Time & Loops timeline draws it); a silent movie has none.
#[test]
fn a_video_cues_sound_has_a_waveform_and_a_silent_movie_has_none() {
    let dir = tempfile::tempdir().unwrap();
    let loud = sound_movie!(dir.path(), "loud.mov", (75, 25), "pcm:2:48000");
    let quiet = movie!(dir.path(), "quiet.mov", (50, 25));
    let mut s = video_show(vec![sounding_cue(1, &loud), video_cue(2, Some(&quiet))]);
    let ask = |s: &mut OfflineShow, n: u128| {
        let (reply, rx) = openlab_engine::control::reply_channel();
        let message = ClientMessage::Request {
            id: s.request_id(),
            request: Request::Waveform {
                cue: id(n),
                start: 0.0,
                end: 3.0,
                buckets: 3,
            },
        };
        s.deliver(openlab_engine::control::Inbox::Client { message, reply });
        rx
    };
    let mut rx = ask(&mut s, 1);
    s.settle_media();
    let Some(ServerMessage::Result {
        outcome: Outcome::Ok(ok),
        ..
    }) = rx.try_recv().ok()
    else {
        panic!("no waveform");
    };
    let Some(ResultData::Waveform(w)) = ok.data else {
        panic!("not a waveform");
    };
    assert_eq!((w.duration, w.channels.len()), (3.0, 2));
    // Channel 0 rises to a half over each second; channel 1 holds each second's number.
    assert!(
        w.channels[0]
            .max
            .iter()
            .all(|m| (16_000..16_500).contains(m))
    );
    // (a bucket's edges round outwards, so it may take in a sample of its neighbour)
    assert_eq!((w.channels[1].min[0], w.channels[1].max[2]), (16, 48));
    let mut rx = ask(&mut s, 2);
    assert!(matches!(
        rx.try_recv().ok(),
        Some(ServerMessage::Result {
            outcome: Outcome::Error(_),
            ..
        })
    ));
}

/// Flow 9 (others): a cue that ducks others turns a playing Video cue's sound down while its
/// picture plays on, and a cue that fades and stops others ends its picture and sound together.
#[test]
fn duck_others_and_fade_and_stop_others_act_on_a_video_cues_sound() {
    let dir = tempfile::tempdir().unwrap();
    let movie = sound_movie!(dir.path(), "clip.mov", (100, 25), "pcm:2:48000");
    let mut ducker = cue(2, CueKind::Wait { duration: 0.5 });
    ducker.duck_others = openlab_core::DuckOthers {
        enabled: true,
        db: -6.0206,
        duration: 0.1,
    };
    let mut stopper = cue(3, CueKind::Memo);
    stopper.fade_and_stop_others = openlab_core::FadeAndStopOthers {
        enabled: true,
        scope: openlab_core::OthersScope::Peers,
        duration: 0.2,
    };
    let mut s = video_show(vec![sounding_cue(1, &movie), ducker, stopper]);
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(0.5));
    let ducked = start(&mut s, 2);
    run_to(&mut s, ducked + samples(0.3));
    let (_, second) = heard(&s);
    assert!(
        (second * 2048.0 - 0.5).abs() < 0.01,
        "ducked by half: {second}"
    );
    assert_eq!(
        on_stage(&mut s, "ducked"),
        frame_at((s.now() - at) as f64 / f64::from(RATE))
    );
    // The duck ends with the cue that asked for it.
    run_to(&mut s, ducked + samples(1.0));
    in_step(&mut s, RATE, 25.0, "after the duck");
    let stopped = start(&mut s, 3);
    run_to(&mut s, stopped + samples(0.2) + 2 * BLOCK);
    expect_state(&s, 1, CueState::Idle, "faded and stopped");
    assert_eq!(shown(&mut s), None);
    s.run(samples(0.1));
    assert!(silent(&s, stopped + samples(0.2) + BLOCK, s.rendered()));
    assert_eq!(s.control().live_voices(), 0);
}
/// A devamp that stops its target, asked while the sound is starved: when the disk catches up
/// the picture is put back with its sound, and both play the pass under way to its end before
/// the cue stops.
#[test]
fn a_devamp_stop_asked_while_the_sound_is_starved_ends_both_after_the_whole_pass() {
    let dir = tempfile::tempdir().unwrap();
    let movie = sound_movie!(dir.path(), "clip.mov", (50, 25), "pcm:2:48000");
    let looping = timed_sounding_cue(1, &movie, |t| {
        t.start_time = 0.4;
        t.end_time = Some(1.2);
        t.slices = vec![openlab_core::SliceMarker {
            time: 0.8,
            play_count: 0.into(),
        }];
    });
    let stop = cue(
        2,
        CueKind::Devamp {
            target: Some(id(1)),
            start_next_cue: false,
            stop_target_when_slice_ends: true,
            devamp_mode: openlab_core::DevampMode::CurrentSlice,
        },
    );
    let ws = workspace(vec![looping, stop]);
    let mut s = video_show_config(ws, &[], |_| {}, |c| c.streamer.before_open = gated_open);
    gate(&movie);
    let at = start(&mut s, 1);
    s.run_unsettled(samples(0.2));
    start(&mut s, 2);
    s.run_unsettled(samples(0.1));
    release(&movie);
    s.run(samples(0.25));
    let sound = in_step(&mut s, RATE, 25.0, "after the disk caught up");
    let late = (s.now() - at) as f64 / f64::from(RATE) - (sound - 0.4);
    assert!(late > 0.25, "the sound started {late} s late");
    // The pass under way ends 0.4 s after the sound really began; the cue stops there.
    let end = at + samples(0.4 + late);
    run_to(&mut s, end - samples(0.05));
    expect_state(&s, 1, CueState::Running, "the pass is not over yet");
    in_step(&mut s, RATE, 25.0, "near the end of the pass");
    run_to(&mut s, end + samples(0.1));
    expect_state(&s, 1, CueState::Idle, "stopped at the end of the pass");
    assert_eq!(shown(&mut s), None);
    s.run(samples(0.1));
    let last = last_nonzero(&s.channel(1)).unwrap() as u64;
    sound_is(
        &s,
        last - 16,
        0.798,
        "the last sound heard is the pass's end",
    );
}

/// The pre-show check's patches with no device, as (cues listed on each, patches counted as
/// problems).
fn preshow_patches(s: &mut OfflineShow) -> (Vec<Vec<openlab_core::CueId>>, u64) {
    let request = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id: request,
        request: Request::PreshowCheck,
    });
    let report = replies
        .into_iter()
        .find_map(|m| match m {
            ServerMessage::Result {
                outcome: Outcome::Ok(body),
                ..
            } => match body.data {
                Some(ResultData::PreshowCheck(report)) => Some(report),
                _ => None,
            },
            _ => None,
        })
        .expect("a pre-show report");
    let cues = report.patches.iter().map(|p| p.cues.clone()).collect();
    (cues, report.summary.patches)
}

/// A patch's device is cleared while a silent movie, a still and a movie with sound play, all
/// naming that patch: only the cue that sounds on it pauses (with its picture held); the
/// pictures with nothing to hear play on. The pre-show check lists the patch with the
/// sounding cue alone, and a patch only silent pictures name is no problem of the show.
#[test]
fn clearing_a_patchs_device_leaves_stills_and_silent_movies_playing() {
    let dir = tempfile::tempdir().unwrap();
    let quiet = movie!(dir.path(), "quiet.mov", (250, 25));
    let loud = sound_movie!(dir.path(), "loud.mov", (250, 25), "pcm:2:48000");
    let png = make(dir.path(), "card.png", "png", None);
    let mut still = still_cue(2, &png, None);
    if let CueKind::Video { sound, .. } = &mut still.kind {
        sound.patch = Some(patch_id());
    }
    let clear = |s: &mut OfflineShow| {
        let request = s.request_id();
        s.send(ClientMessage::Request {
            id: request,
            request: Request::SetPatchMapping {
                patch: patch_id(),
                mapping: None,
            },
        });
    };
    let cues = vec![sounding_cue(1, &quiet), still, sounding_cue(3, &loud)];
    let mut s = video_show(cues.clone());
    for n in 1..=3 {
        start(&mut s, n);
    }
    s.run(samples(0.5));
    clear(&mut s);
    s.run(samples(0.3));
    expect_state(&s, 1, CueState::Running, "a silent movie");
    expect_state(&s, 2, CueState::Running, "a still");
    expect_state(&s, 3, CueState::Paused, "a movie that sounds on the patch");
    assert_eq!(s.control().pictures_live(), 3);
    assert_eq!(problems(&s, 1), vec![]);
    assert_eq!(problems(&s, 2), vec![]);
    assert_eq!(preshow_patches(&mut s), (vec![vec![id(3)]], 1));

    // With silent pictures alone the patch is listed, with no cue on it, and not counted.
    let mut s = video_show(cues.into_iter().take(2).collect());
    clear(&mut s);
    s.run(BLOCK);
    assert_eq!(preshow_patches(&mut s), (vec![vec![]], 0));
}

/// A Video cue with sound and hold at end stays up when the report of its voice's end was
/// lost (the control thread held off while other cues' events fill the mixer's queue): the
/// voice's end is made up for, but the cue's end is its picture's to tell.
#[test]
fn a_held_video_cue_stays_up_when_its_voices_end_report_is_lost() {
    let dir = tempfile::tempdir().unwrap();
    let movie = sound_movie!(dir.path(), "clip.mov", (250, 25), "pcm:2:48000");
    let looping = wav(&dc(800, 0.25));
    let mut cues: Vec<Cue> = (11..=20)
        .map(|n| with_slices(audio(n, &looping, 0), &[(secs(400), 0)], 1))
        .collect();
    cues.push(timed_sounding_cue(2, &movie, |t| {
        t.end_time = Some(9.0);
        t.hold_at_end = true;
    }));
    let mut s = video_show(cues);
    for n in 11..=20 {
        start(&mut s, n);
    }
    let at = start(&mut s, 2);
    s.run(BLOCK * 12);
    let until = at + samples(9.3);
    let now = s.rendered();
    s.render_only(until - now);
    assert!(s.control().mixer().dropped_events() > 0);
    s.run(samples(1.0));
    expect_state(&s, 2, CueState::Running, "held at its end");
    assert_eq!(shown(&mut s), Some(224));
    assert_eq!(s.control().live_voices(), 10, "its own voice is gone");
}

/// An old QuickTime movie (no file-type box) with sound has no problem, and plays picture
/// and sound in step from a load to time (a seek in the track) to its end.
#[test]
fn an_old_quicktime_movie_without_a_file_type_box_plays_its_sound() {
    let dir = tempfile::tempdir().unwrap();
    let movie = sound_movie!(dir.path(), "clip.mov", (75, 25), "pcm:2:48000");
    let old = without_file_type(&movie, &dir.path().join("old.mov"));
    let mut s = video_show(vec![sounding_cue(1, &old)]);
    assert_eq!(problems(&s, 1), vec![]);
    let at = go(&mut s);
    run_to(&mut s, at + samples(0.5));
    let sound = in_step(&mut s, RATE, 25.0, "from the top");
    assert!((sound - 0.5).abs() < 0.02, "{sound}");
    s.transport(TransportAction::Stop {
        cue: id(1),
        fade_out: None,
    });
    s.run(BLOCK * 2);
    s.transport(TransportAction::Load {
        cue: id(1),
        at: 2.0,
    });
    s.run(BLOCK);
    let at = start(&mut s, 1);
    run_to(&mut s, at + samples(0.5));
    let sound = in_step(&mut s, RATE, 25.0, "from a load to time");
    assert!((sound - 2.5).abs() < 0.02, "{sound}");
    run_to(&mut s, at + samples(1.0) + 2 * BLOCK);
    expect_state(&s, 1, CueState::Idle, "");
}
