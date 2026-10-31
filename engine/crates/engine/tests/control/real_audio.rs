// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! End to end with real audio files: WAV, AIFF, FLAC, ALAC, MP3, AAC and Ogg files are added,
//! edited, played and saved through the client messages the app sends (decoded from JSON),
//! rendered offline at 44.1 and 48 kHz, and the output is measured: levels, routing, mutes,
//! fades and their curves, groups, playlists, continues, Stop and Panic, rate conversion, save
//! and reopen (AUD-01, AUD-06, AUD-09, AUD-10, GRP-01).

use std::path::{Path, PathBuf};

use crate::show::media::{self, Pcm};
use crate::show::*;
use openlab_core::protocol::{
    ClientMessage, Outcome, ServerMessage, WireCueState, WireWarning, decode_client,
};
use openlab_core::{
    Command, ContinueMode, Crosspoint, Cue, CueKind, CueLevels, CueState, FadeCurve, FadeLevels,
    GroupMode, MediaRef, SILENCE_DB, Workspace, db_to_gain,
};
use openlab_engine::control::offline::OfflineShow;

const R48: u32 = 48_000;
const R44: u32 = 44_100;
const B: u64 = 256;
/// A quarter second at 48 kHz.
const Q: usize = 12_000;

fn uuid(n: u128) -> String {
    uuid::Uuid::from_u128(n).to_string()
}

/// A compressed fixture of the audio crate (its fixtures/generate.sh): 0.5 s, 44.1 kHz
/// stereo, left 440 Hz at 0.5, right 1000 Hz at 0.25.
fn audio_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../audio/tests/fixtures")
        .join(name)
}

/// The reference stereo tone (left 440 Hz at 0.5, right 1000 Hz at 0.25), `secs` long.
fn tone_file(name: &str, rate: u32, secs: f64, aiff: bool, fmt: Pcm) -> PathBuf {
    let frames = (secs * f64::from(rate)).round() as usize;
    let samples = media::tone(frames, rate);
    let bytes = if aiff {
        media::aiff(&samples, 2, rate, fmt)
    } else {
        media::wav(&samples, 2, rate, fmt)
    };
    media::temp_file(name, &bytes)
}

/// A mono 24-bit WAV of constant 0.5 at 48 kHz: its output is the gain envelope times 0.5.
fn dc_wav(frames: usize) -> PathBuf {
    media::temp_file("dc.wav", &media::wav(&dc(frames, 0.5), 1, R48, Pcm::I24))
}

/// An audio cue with file channel n routed to cue output n at 0 dB, as the app creates one.
fn stereo(n: u128, path: &Path) -> Cue {
    let mut c = audio(n, path, 0);
    levels(&mut c).crosspoints.push(Crosspoint {
        input: 1,
        output: 1,
        db: 0.0,
        mute: false,
    });
    c
}

fn levels(c: &mut Cue) -> &mut CueLevels {
    match &mut c.kind {
        CueKind::Audio { levels, .. } => levels,
        _ => panic!("not an audio cue"),
    }
}

fn fade(n: u128, target: u128, secs: f64, curve: FadeCurve, master_db: f64, stop: bool) -> Cue {
    cue(
        n,
        CueKind::Fade {
            target: Some(id(target)),
            duration: secs,
            curve,
            levels: FadeLevels {
                master_db: Some(master_db),
                output_db: vec![],
                crosspoints: vec![],
            },
            stop_target_when_done: stop,
        },
    )
}

fn group(n: u128, mode: GroupMode, children: Vec<Cue>) -> Cue {
    cue(n, CueKind::Group { mode, children })
}

fn playlist(loop_: bool, shuffle: bool) -> GroupMode {
    GroupMode::Playlist { loop_, shuffle }
}

/// Sends one client message given as JSON (what the app writes on its socket) and checks the
/// engine accepted it.
fn app(s: &mut OfflineShow, json: &str) -> Vec<ServerMessage> {
    let replies = s.send(decode_client(json).unwrap());
    for r in &replies {
        if let ServerMessage::Result { outcome, .. } = r {
            assert!(matches!(outcome, Outcome::Ok(_)), "{json} -> {outcome:?}");
        }
    }
    assert_eq!(s.stalls(), 0);
    s.settle_media();
    replies
}

fn command(s: &mut OfflineShow, command: &str) {
    let json = format!(
        r#"{{"type":"command","id":{},"command":{command}}}"#,
        s.request_id().0
    );
    app(s, &json);
}

/// A transport action; returns the sample it is scheduled on.
fn transport(s: &mut OfflineShow, action: &str) -> usize {
    let at = s.now() as usize;
    let json = format!(
        r#"{{"type":"transport","id":{},"action":{action}}}"#,
        s.request_id().0
    );
    app(s, &json);
    at
}

/// GO on the main list.
fn go_app(s: &mut OfflineShow) -> usize {
    transport(s, &format!(r#"{{"type":"go","list":"{}"}}"#, uuid(900)))
}

/// `AddCue` of `cue` at `index` of the main list, the cue as JSON like the app's.
fn add(s: &mut OfflineShow, cue: &Cue, index: usize) {
    let cue = serde_json::to_string(cue).unwrap();
    command(
        s,
        &format!(
            r#"{{"type":"add_cue","at":{{"list":"{}","parent":null,"index":{index}}},"cue":{cue},"auto_number":false}}"#,
            uuid(900)
        ),
    );
}

/// `SetCueProperty { kind }`: the app's Levels tab sends the whole kind.
fn set_kind(s: &mut OfflineShow, n: u128, kind: &CueKind) {
    let kind = serde_json::to_string(kind).unwrap();
    command(
        s,
        &format!(
            r#"{{"type":"set_cue_property","id":"{}","property":{{"type":"kind","value":{kind}}}}}"#,
            uuid(n)
        ),
    );
}

/// An offline show at `rate` built from the app's commands: an empty list, then each cue.
fn app_show(cues: &[Cue], rate: u32) -> OfflineShow {
    let mut s = show_at(workspace(vec![]), rate, B);
    for (i, c) in cues.iter().enumerate() {
        add(&mut s, c, i);
    }
    s
}

fn db(x: f64) -> f64 {
    20.0 * x.log10()
}

fn rms_db(x: &[f32]) -> f64 {
    db(media::rms(x))
}

fn close(got: f64, want: f64, tol: f64, what: &str) {
    assert!(
        (got - want).abs() <= tol,
        "{what}: {got:.4}, want {want:.4} ± {tol}"
    );
}

fn state(s: &OfflineShow, n: u128) -> CueState {
    s.control().engine().state(id(n))
}

/// The tone's left (0.5 peak) and right (0.25 peak) RMS in dBFS.
fn tone_db() -> (f64, f64) {
    (db(0.5 / 2f64.sqrt()), db(0.25 / 2f64.sqrt()))
}

/// The app's Finder drop of `path`: the golden `AddCue` the Swift side is checked against
/// (`engine/fixtures/protocol/client/command_add_new_audio_cue.json`), with only the list, patch
/// and file pointed at this show's. Its levels and routing are sent as the app sends them.
fn drop_like_app(s: &mut OfflineShow, path: &Path) -> u128 {
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/protocol/client/command_add_new_audio_cue.json"),
    )
    .unwrap();
    let mut message = decode_client(&text).unwrap();
    let ClientMessage::Command { id, command, .. } = &mut message else {
        panic!("not a command");
    };
    *id = s.request_id();
    let Command::AddCue { at, cue, .. } = command.as_mut() else {
        panic!("not an AddCue");
    };
    at.list = list_id();
    let CueKind::Audio { media, patch, .. } = &mut cue.kind else {
        panic!("not an audio cue");
    };
    *media = Some(MediaRef::new(path.to_str().unwrap()));
    *patch = patch_id();
    let cue = cue.id.0.as_u128();
    let replies = s.send(message);
    assert!(
        replies.iter().all(|r| !matches!(
            r,
            ServerMessage::Result {
                outcome: Outcome::Error(_),
                ..
            }
        )),
        "{replies:?}"
    );
    s.settle_media();
    cue
}

#[test]
fn a_stereo_file_dropped_like_the_app_plays_left_on_one_and_right_on_two() {
    let path = tone_file("drop.wav", R48, 0.5, false, Pcm::I24);
    let mut s = show_at(workspace(vec![]), R48, B);
    let n = drop_like_app(&mut s, &path);
    let at = go_app(&mut s);
    s.run(u64::from(R48));
    let (l, r) = (s.channel(0), s.channel(1));
    let w = at + 4800..at + 19_200;
    close(rms_db(&l[w.clone()]), tone_db().0, 0.01, "left dB");
    close(rms_db(&r[w.clone()]), tone_db().1, 0.01, "right dB");
    close(media::frequency(&l[w.clone()], R48), 440.0, 0.05, "left Hz");
    close(media::frequency(&r[w], R48), 1000.0, 0.05, "right Hz");
    assert_eq!(state(&s, n), CueState::Idle);
}

#[test]
fn a_mono_file_dropped_like_the_app_plays_on_output_one_only() {
    // QLab 5's default levels: input 1 to output 1 only (a mono file is not spread).
    let x = media::sine(R48 as usize / 2, R48, 440.0, 0.5);
    let path = media::temp_file("mono.wav", &media::wav(&x, 1, R48, Pcm::I16));
    let mut s = show_at(workspace(vec![]), R48, B);
    drop_like_app(&mut s, &path);
    let at = go_app(&mut s);
    s.run(u64::from(R48));
    let w = at + 4800..at + 19_200;
    close(rms_db(&s.channel(0)[w]), tone_db().0, 0.01, "output 1 dB");
    assert!(
        s.channel(1).iter().all(|&v| v == 0.0),
        "output 2 not silent"
    );
}

#[test]
fn every_format_plays_at_its_recorded_level_pitch_and_length() {
    let tone = tone_db();
    // The lossy fixtures' own levels, 0.1 to 0.4 s in, as macOS's decoder (afconvert) measures
    // them: the MP3 encoder left the tone 0.45 dB low, the AAC encoder its right channel 0.1 dB.
    let mp3 = (-9.479, -15.499);
    let aac = (-9.035, -15.156);
    // (file, expected left/right dBFS, level tolerance dB, right channel Hz tolerance, length
    // tolerance in frames at 48 kHz)
    let files = [
        (
            tone_file("i16.wav", R48, 0.5, false, Pcm::I16),
            tone,
            0.01,
            0.1,
            0,
        ),
        (
            tone_file("i24.wav", R48, 0.5, false, Pcm::I24),
            tone,
            0.01,
            0.1,
            0,
        ),
        (
            tone_file("f32.wav", R48, 0.5, false, Pcm::F32),
            tone,
            0.01,
            0.1,
            0,
        ),
        (
            tone_file("i16.aiff", R48, 0.5, true, Pcm::I16),
            tone,
            0.01,
            0.1,
            0,
        ),
        (
            tone_file("i24.aiff", R48, 0.5, true, Pcm::I24),
            tone,
            0.01,
            0.1,
            0,
        ),
        // 44.1 kHz files on a 48 kHz device: resampled.
        (
            tone_file("i16-44.wav", R44, 0.5, false, Pcm::I16),
            tone,
            0.01,
            0.1,
            2,
        ),
        (
            tone_file("i24-44.aiff", R44, 0.5, true, Pcm::I24),
            tone,
            0.01,
            0.1,
            2,
        ),
        (audio_fixture("tone.flac"), tone, 0.01, 0.1, 2),
        (audio_fixture("tone-alac.m4a"), tone, 0.01, 0.1, 2),
        // Lossy: encoders pad or trim the ends.
        (audio_fixture("tone.mp3"), mp3, 0.01, 0.1, 2400),
        (audio_fixture("tone.m4a"), aac, 0.01, 0.1, 2400),
        // ffmpeg's built-in Vorbis encoder (the only one that made the fixture) distorts the
        // quieter channel to ~991 Hz; ffmpeg's own decoder hears the same (audio decode tests).
        (audio_fixture("tone.ogg"), tone, 0.1, 12.0, 2400),
    ];
    for (path, (l_db, r_db), tol, r_hz_tol, len_tol) in files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let mut s = app_show(&[stereo(1, &path)], R48);
        let at = go_app(&mut s);
        s.run(u64::from(R48));
        assert_eq!(s.stalls(), 0, "{name}");
        let (l, r) = (s.channel(0), s.channel(1));
        let w = at + 4800..at + 19_200;
        close(rms_db(&l[w.clone()]), l_db, tol, &format!("{name} left dB"));
        close(
            rms_db(&r[w.clone()]),
            r_db,
            tol,
            &format!("{name} right dB"),
        );
        let hz = media::frequency(&l[w.clone()], R48);
        close(hz, 440.0, 0.05, &format!("{name} left Hz"));
        let hz = media::frequency(&r[w], R48);
        close(hz, 1000.0, r_hz_tol, &format!("{name} right Hz"));
        // 0.5 s is 24000 frames at the device rate, whatever the file's rate.
        let end = last_nonzero(&l).unwrap().max(last_nonzero(&r).unwrap()) + 1;
        let len = end as i64 - at as i64;
        assert!(
            (len - 24_000).abs() <= len_tol,
            "{name}: played {len} frames, want 24000 ± {len_tol}"
        );
        assert_eq!(state(&s, 1), CueState::Idle, "{name}");
        assert_eq!(s.control().live_voices(), 0, "{name}");
    }
}

#[test]
fn a_48k_file_on_a_44k_device_keeps_its_pitch_and_length() {
    let path = tone_file("i24-48.wav", R48, 0.5, false, Pcm::I24);
    let mut s = app_show(&[stereo(1, &path)], R44);
    let at = go_app(&mut s);
    s.run(u64::from(R44));
    let (l, r) = (s.channel(0), s.channel(1));
    let w = at + 4410..at + 17_640;
    close(media::frequency(&l[w.clone()], R44), 440.0, 0.05, "left Hz");
    close(
        media::frequency(&r[w.clone()], R44),
        1000.0,
        0.1,
        "right Hz",
    );
    close(rms_db(&l[w]), tone_db().0, 0.02, "left dB");
    let end = last_nonzero(&l).unwrap().max(last_nonzero(&r).unwrap()) + 1;
    let len = end as i64 - at as i64;
    assert!((len - 22_050).abs() <= 2, "played {len} frames, want 22050");
}

/// Left and right RMS in dBFS of the tone file played once, 0.1 to 0.4 s in, with `edit`
/// applied to the cue and `ws_edit` to the workspace.
fn measure(edit: impl FnOnce(&mut Cue), ws_edit: impl FnOnce(&mut Workspace)) -> (f64, f64) {
    let path = tone_file("lv.wav", R48, 0.5, false, Pcm::I24);
    let mut c = stereo(1, &path);
    edit(&mut c);
    let mut ws = workspace(vec![]);
    ws_edit(&mut ws);
    let mut s = show_at(ws, R48, B);
    add(&mut s, &c, 0);
    let at = go_app(&mut s);
    s.run(u64::from(R48));
    let w = at + 4800..at + 19_200;
    (rms_db(&s.channel(0)[w.clone()]), rms_db(&s.channel(1)[w]))
}

#[test]
fn levels_routing_and_mutes_are_heard_to_the_hundredth_of_a_db() {
    let (l0, r0) = measure(|_| {}, |_| {});
    close(l0, tone_db().0, 0.01, "unity left");
    close(r0, tone_db().1, 0.01, "unity right");
    let check = |(l, r): (f64, f64), (wl, wr): (f64, f64), what: &str| {
        close(l - l0, wl, 0.01, &format!("{what} left"));
        close(r - r0, wr, 0.01, &format!("{what} right"));
    };
    check(
        measure(|c| levels(c).master_db = -6.0, |_| {}),
        (-6.0, -6.0),
        "cue master -6",
    );
    check(
        measure(|c| levels(c).outputs[0].db = -3.0, |_| {}),
        (-3.0, 0.0),
        "cue output 1 -3",
    );
    check(
        measure(|c| levels(c).crosspoints[1].db = -10.0, |_| {}),
        (0.0, -10.0),
        "crosspoint 2,2 -10",
    );
    check(
        measure(
            |c| {
                let l = levels(c);
                l.master_db = -6.0;
                l.outputs[0].db = -3.0;
                l.crosspoints[0].db = -1.5;
            },
            |_| {},
        ),
        (-10.5, -6.0),
        "gains in series add",
    );
    check(
        measure(|_| {}, |ws| ws.patches[0].outputs[0].level_db = -6.0),
        (-6.0, 0.0),
        "patch output 1 -6",
    );
    check(
        measure(|_| {}, |ws| ws.patches[0].master_db = -6.0),
        (-6.0, -6.0),
        "patch master -6",
    );
    check(
        measure(|_| {}, |ws| ws.settings.master_db = -6.0),
        (-6.0, -6.0),
        "workspace master -6",
    );
    // A mute is silence, exactly, and leaves the other side alone.
    let (l, r) = measure(|c| levels(c).outputs[1].mute = true, |_| {});
    close(l, l0, 0.01, "cue output 2 muted: left");
    assert_eq!(r, f64::NEG_INFINITY, "cue output 2 muted");
    let (l, r) = measure(|c| levels(c).crosspoints[0].mute = true, |_| {});
    assert_eq!(l, f64::NEG_INFINITY, "crosspoint 1,1 muted");
    close(r, r0, 0.01, "crosspoint 1,1 muted: right");
    let (l, r) = measure(|_| {}, |ws| ws.patches[0].outputs[1].mute = true);
    close(l, l0, 0.01, "patch output 2 muted: left");
    assert_eq!(r, f64::NEG_INFINITY, "patch output 2 muted");
}

#[test]
fn crossed_routing_sends_each_file_channel_where_the_matrix_says() {
    let path = tone_file("x.wav", R48, 0.5, false, Pcm::I24);
    let mut c = audio(1, &path, 1);
    levels(&mut c).crosspoints.push(Crosspoint {
        input: 1,
        output: 0,
        db: 0.0,
        mute: false,
    });
    let mut s = app_show(&[c], R48);
    let at = go_app(&mut s);
    s.run(u64::from(R48));
    let w = at + 4800..at + 19_200;
    let (l, r) = (s.channel(0), s.channel(1));
    close(media::frequency(&l[w.clone()], R48), 1000.0, 0.1, "left Hz");
    close(
        media::frequency(&r[w.clone()], R48),
        440.0,
        0.05,
        "right Hz",
    );
    close(rms_db(&l[w.clone()]), tone_db().1, 0.01, "left dB");
    close(rms_db(&r[w]), tone_db().0, 0.01, "right dB");
}

#[test]
fn a_level_edit_on_a_playing_cue_is_heard_at_once() {
    let path = tone_file("edit.wav", R48, 1.0, false, Pcm::I24);
    let c = stereo(1, &path);
    let mut s = app_show(std::slice::from_ref(&c), R48);
    let at = go_app(&mut s);
    s.run(9600);
    let mut kind = c.kind.clone();
    if let CueKind::Audio { levels, .. } = &mut kind {
        levels.master_db = -6.0;
    }
    let te = s.now() as usize;
    set_kind(&mut s, 1, &kind);
    s.run(u64::from(R48));
    let l = s.channel(0);
    let before = rms_db(&l[at + 2400..te - 480]);
    // The edit ramps over 10 ms (480 frames); measured past it.
    let after = rms_db(&l[te + 960..at + 45_600]);
    close(after - before, -6.0, 0.01, "edited master");
}

/// The documented curve `f(t)` (docs/audio.md, Curves), written out here independently.
fn shape(curve: FadeCurve, t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    let k = 1000f64.ln();
    let e = |t: f64| ((k * t).exp() - 1.0) / (k.exp() - 1.0);
    match curve {
        FadeCurve::Linear => t,
        FadeCurve::EqualPower => (std::f64::consts::FRAC_PI_2 * t).sin(),
        FadeCurve::SCurve => (1.0 - (std::f64::consts::PI * t).cos()) / 2.0,
        FadeCurve::Exponential => e(t),
        FadeCurve::Logarithmic => 1.0 - e(1.0 - t),
    }
}

/// The documented ramp from gain `a` to `b`: rising follows `f(t)`, falling `1 − f(1 − t)`.
fn ramp(a: f64, b: f64, t: f64, curve: FadeCurve) -> f64 {
    let s = if b >= a {
        shape(curve, t)
    } else {
        1.0 - shape(curve, 1.0 - t)
    };
    a + (b - a) * s
}

const CURVES: [FadeCurve; 5] = [
    FadeCurve::Linear,
    FadeCurve::EqualPower,
    FadeCurve::SCurve,
    FadeCurve::Exponential,
    FadeCurve::Logarithmic,
];

/// Checks `out[start + k] = 0.5 · ramp(a, b, k / n)` on every frame of an `n`-frame fade, then
/// `b` held for `hold` frames.
fn assert_envelope(
    out: &[f32],
    start: usize,
    n: usize,
    (a, b): (f64, f64),
    curve: FadeCurve,
    hold: usize,
) {
    for k in 0..=n + hold {
        let want = 0.5 * ramp(a, b, k as f64 / n as f64, curve);
        let got = f64::from(out[start + k]);
        // 24-bit quantisation of 0.5 (6e-8) and the mixer's ramp bound (1e-6 of full scale).
        assert!(
            (got - want).abs() <= 1.1e-6,
            "{curve:?} frame {k} of {n}: {got}, want {want}"
        );
    }
}

#[test]
fn a_fade_to_minus_20_follows_each_documented_curve_sample_by_sample() {
    let to = f64::from(db_to_gain(-20.0));
    for curve in CURVES {
        let path = dc_wav(5 * R48 as usize);
        let cues = [audio(1, &path, 0), fade(2, 1, 2.0, curve, -20.0, false)];
        let mut s = app_show(&cues, R48);
        go_app(&mut s);
        s.run(4800);
        let tf = go_app(&mut s);
        s.run(3 * u64::from(R48));
        let out = s.channel(0);
        assert!(
            (f64::from(out[tf - 1]) - 0.5).abs() < 1e-7,
            "{curve:?} before"
        );
        assert_envelope(&out, tf, 2 * R48 as usize, (1.0, to), curve, 4800);
        // Halfway, in dB: -5.19 linear and S-curve, -2.66 equal power, -17.88 exponential,
        // -0.24 logarithmic.
        let mid = db(f64::from(out[tf + R48 as usize]) / 0.5);
        close(
            mid,
            db(ramp(1.0, to, 0.5, curve)),
            0.001,
            &format!("{curve:?} halfway"),
        );
        assert_eq!(state(&s, 1), CueState::Running, "{curve:?}");
        assert_eq!(state(&s, 2), CueState::Idle, "{curve:?}");
    }
}

#[test]
fn a_fade_from_silence_rises_on_its_curve_to_the_target() {
    for curve in CURVES {
        let path = dc_wav(4 * R48 as usize);
        let mut c = audio(1, &path, 0);
        levels(&mut c).master_db = SILENCE_DB;
        let mut s = app_show(&[c, fade(2, 1, 1.0, curve, 0.0, false)], R48);
        let at = go_app(&mut s);
        s.run(4800);
        assert!(s.channel(0)[at..].iter().all(|&v| v == 0.0), "{curve:?}");
        let tf = go_app(&mut s);
        s.run(2 * u64::from(R48));
        assert_envelope(&s.channel(0), tf, R48 as usize, (0.0, 1.0), curve, 4800);
    }
}

#[test]
fn stop_target_when_done_stops_it_on_the_fades_last_sample() {
    let path = dc_wav(5 * R48 as usize);
    let cues = [
        audio(1, &path, 0),
        fade(2, 1, 1.0, FadeCurve::Linear, SILENCE_DB, true),
    ];
    let mut s = app_show(&cues, R48);
    go_app(&mut s);
    s.run(4800);
    let tf = go_app(&mut s);
    s.run(2 * u64::from(R48));
    let out = s.channel(0);
    assert_envelope(&out, tf, R48 as usize, (1.0, 0.0), FadeCurve::Linear, 0);
    assert!(out[tf + R48 as usize..].iter().all(|&v| v == 0.0));
    assert_eq!(state(&s, 1), CueState::Idle);
    assert_eq!(s.control().live_voices(), 0);
}

#[test]
fn a_fade_with_a_pre_wait_starts_its_ramp_when_the_wait_ends() {
    let path = dc_wav(5 * R48 as usize);
    let mut f = fade(2, 1, 1.0, FadeCurve::Linear, -20.0, false);
    f.pre_wait = 0.5;
    let mut s = app_show(&[audio(1, &path, 0), f], R48);
    go_app(&mut s);
    s.run(4800);
    s.take_outbound();
    let tf = go_app(&mut s);
    s.run(Q as u64);
    assert!(
        states(&s.take_outbound(), id(2))
            .iter()
            .any(|(st, _)| *st == WireCueState::PreWaiting)
    );
    s.run(2 * u64::from(R48));
    let out = s.channel(0);
    // Untouched through the pre-wait, then the ramp from its end.
    assert!(out[tf..tf + 24_000].iter().all(|&v| (v - 0.5).abs() < 1e-7));
    let to = f64::from(db_to_gain(-20.0));
    assert_envelope(
        &out,
        tf + 24_000,
        R48 as usize,
        (1.0, to),
        FadeCurve::Linear,
        4800,
    );
}

#[test]
fn a_fade_fired_while_its_target_is_in_pre_wait_leaves_it_alone() {
    // Only playing cues are faded (docs/cue-engine.md, Fade): a target still in its pre-wait
    // starts at its own level.
    let path = dc_wav(2 * R48 as usize);
    let mut c = audio(1, &path, 0);
    c.pre_wait = 0.5;
    let mut s = app_show(&[c, fade(2, 1, 0.1, FadeCurve::Linear, -20.0, false)], R48);
    let at = go_app(&mut s);
    s.run(4800);
    s.take_outbound();
    go_app(&mut s);
    s.run(2 * u64::from(R48));
    let out = s.channel(0);
    assert_eq!(state(&s, 2), CueState::Idle);
    assert!(
        warnings(&s.take_outbound())
            .iter()
            .any(|w| matches!(w, WireWarning::TargetNotRunning { .. }))
    );
    assert!(
        out[at + 24_000..at + 24_000 + R48 as usize]
            .iter()
            .all(|&v| (v - 0.5).abs() < 1e-7)
    );
}

/// Frequency in the middle of the quarter second of `out` starting at `start`.
fn hz_at(out: &[f32], start: usize) -> f64 {
    media::frequency(&out[start + 3000..start + 9000], R48)
}

#[test]
fn a_timeline_group_starts_real_files_together() {
    let a = tone_file("g.aiff", R48, 0.5, true, Pcm::I24);
    let b = audio_fixture("tone.flac");
    let mut c2 = audio(2, &b, 1);
    levels(&mut c2).crosspoints[0].input = 1;
    let mut s = app_show(
        &[group(10, GroupMode::Timeline, vec![audio(1, &a, 0), c2])],
        R48,
    );
    let at = go_app(&mut s);
    s.run(u64::from(R48));
    let (l, r) = (s.channel(0), s.channel(1));
    // Both sines start at phase 0: the first non-zero sample is the one after the start.
    assert_eq!(first_nonzero(&l), Some(at + 1));
    let r0 = first_nonzero(&r).unwrap();
    assert!((at..=at + 2).contains(&r0), "{r0} vs {at}");
    close(hz_at(&l, at), 440.0, 0.1, "left Hz");
    close(hz_at(&r, at), 1000.0, 0.2, "right Hz");
    assert_eq!(state(&s, 10), CueState::Idle);
}

/// Three quarter-second mono files at 300, 600 and 900 Hz: 16-bit WAV, 24-bit AIFF, 24-bit WAV.
fn three() -> [PathBuf; 3] {
    let x = |f| media::sine(Q, R48, f, 0.5);
    [
        media::temp_file("p1.wav", &media::wav(&x(300.0), 1, R48, Pcm::I16)),
        media::temp_file("p2.aiff", &media::aiff(&x(600.0), 1, R48, Pcm::I24)),
        media::temp_file("p3.wav", &media::wav(&x(900.0), 1, R48, Pcm::I24)),
    ]
}

/// Which of 300/600/900 Hz plays in each of `n` quarter seconds from `at`.
fn order(out: &[f32], at: usize, n: usize) -> Vec<u32> {
    (0..n)
        .map(|i| (hz_at(out, at + i * Q) / 100.0).round() as u32 * 100)
        .collect()
}

fn three_kids() -> Vec<Cue> {
    let [a, b, c] = three();
    vec![audio(21, &a, 0), audio(22, &b, 0), audio(23, &c, 0)]
}

#[test]
fn a_playlist_plays_its_files_back_to_back_then_stops() {
    let mut s = app_show(&[group(10, playlist(false, false), three_kids())], R48);
    let at = go_app(&mut s);
    s.run(u64::from(R48) + Q as u64);
    let out = s.channel(0);
    assert_eq!(order(&out, at, 3), vec![300, 600, 900]);
    // No gap at a join: each file's sine is non-zero right up to its last frame and from its
    // second.
    for i in 1..3 {
        let j = at + i * Q;
        assert!(out[j - 1] != 0.0 && out[j + 1] != 0.0, "join {i}");
    }
    assert!(out[at + 3 * Q..].iter().all(|&v| v == 0.0));
    assert_eq!(state(&s, 10), CueState::Idle);
}

#[test]
fn a_looping_playlist_starts_over_until_stopped() {
    let mut s = app_show(&[group(10, playlist(true, false), three_kids())], R48);
    let at = go_app(&mut s);
    s.run(7 * Q as u64);
    assert_eq!(
        order(&s.channel(0), at, 6),
        vec![300, 600, 900, 300, 600, 900]
    );
    let ts = transport(&mut s, r#"{"type":"stop_all"}"#);
    s.run(u64::from(R48));
    assert!(s.channel(0)[ts + 480..].iter().all(|&v| v == 0.0));
    assert_eq!(state(&s, 10), CueState::Idle);
}

#[test]
fn a_shuffled_looping_playlist_plays_each_file_once_per_pass() {
    let mut s = app_show(&[group(10, playlist(true, true), three_kids())], R48);
    let at = go_app(&mut s);
    s.run(13 * Q as u64);
    let heard = order(&s.channel(0), at, 12);
    for pass in heard.chunks(3) {
        let mut p = pass.to_vec();
        p.sort_unstable();
        assert_eq!(p, vec![300, 600, 900], "{heard:?}");
    }
    assert!(
        heard.chunks(3).any(|p| p != [300, 600, 900]),
        "never shuffled: {heard:?}"
    );
}

#[test]
fn auto_follow_and_auto_continue_chain_real_files_on_time() {
    let [a, b, _] = three();
    let mut c1 = audio(1, &a, 0);
    c1.continue_mode = ContinueMode::AutoFollow;
    let mut c2 = audio(2, &b, 0);
    c2.continue_mode = ContinueMode::AutoContinue;
    c2.post_wait = 0.1;
    let c3 = audio(3, &audio_fixture("tone.flac"), 1);
    let mut s = app_show(&[c1, c2, c3], R48);
    let at = go_app(&mut s);
    s.run(2 * u64::from(R48));
    let (l, r) = (s.channel(0), s.channel(1));
    assert_eq!(order(&l, at, 2), vec![300, 600]);
    assert!(l[at + 2 * Q..].iter().all(|&v| v == 0.0));
    // Cue 3 starts 0.1 s after cue 2 did; the FLAC's left channel starts at phase 0.
    let r0 = first_nonzero(&r).unwrap();
    let want = at + Q + 4800;
    assert!((want..=want + 2).contains(&r0), "{r0} vs {want}");
}

#[test]
fn stop_all_and_panic_silence_every_output() {
    let tone = tone_file("st.wav", R48, 4.0, false, Pcm::I24);
    let flac = audio_fixture("tone.flac");
    let cues = [stereo(1, &tone), stereo(2, &flac)];
    let start = |s: &mut OfflineShow, n: u128| {
        transport(s, &format!(r#"{{"type":"start","cue":"{}"}}"#, uuid(n)));
    };
    // Stop All: everything stops at once (a de-click at most).
    let mut s = app_show(&cues, R48);
    start(&mut s, 1);
    start(&mut s, 2);
    s.run(4800);
    let ts = transport(&mut s, r#"{"type":"stop_all"}"#);
    s.run(u64::from(R48));
    for ch in 0..2 {
        let x = s.channel(ch);
        assert!(
            x[ts - 480..ts].iter().any(|&v| v != 0.0),
            "ch {ch} was playing"
        );
        assert!(x[ts + 480..].iter().all(|&v| v == 0.0), "stop all ch {ch}");
    }
    assert_eq!(s.control().live_voices(), 0);
    // Panic (the app's Esc, app-wide): a fade over the workspace's panic duration, then
    // silence.
    let mut ws = workspace(vec![]);
    ws.settings.panic_duration = 1.0;
    let mut s = show_at(ws, R48, B);
    for (i, c) in cues.iter().enumerate() {
        add(&mut s, c, i);
    }
    start(&mut s, 1);
    s.run(4800);
    let tp = s.now() as usize;
    let json = format!(r#"{{"type":"panic","id":{}}}"#, s.request_id().0);
    app(&mut s, &json);
    s.run(2 * u64::from(R48));
    let l = s.channel(0);
    let before = rms_db(&l[tp - 4800..tp]);
    let half = rms_db(&l[tp + 21_600..tp + 26_400]);
    assert!(
        half < before - 3.0 && half > before - 30.0,
        "{before} {half}"
    );
    assert!(l[tp + R48 as usize..].iter().all(|&v| v == 0.0), "panic");
    assert!(s.channel(1)[tp + R48 as usize..].iter().all(|&v| v == 0.0));
    assert_eq!(s.control().live_voices(), 0);
}

/// One fixed run: GO three times (a file at -3 dB, an S-curve fade that stops it, a
/// playlist). Returns everything rendered.
fn script(s: &mut OfflineShow) -> Vec<f32> {
    go_app(s);
    s.run(9600);
    go_app(s);
    s.run(u64::from(R48));
    go_app(s);
    s.run(u64::from(R48));
    s.audio().to_vec()
}

#[test]
fn a_saved_and_reopened_show_renders_the_same_samples() {
    let env = crate::common::env();
    let path = env.show_path();
    let tone = tone_file("sv.wav", R48, 3.0, false, Pcm::I24);
    let mut c1 = stereo(1, &tone);
    levels(&mut c1).master_db = -3.0;
    let cues = [
        c1,
        fade(2, 1, 0.5, FadeCurve::SCurve, -12.0, true),
        group(10, playlist(false, false), three_kids()),
    ];
    // An engine that can save: an application folder of its own.
    let engine = || {
        let ws = workspace(vec![]);
        let mut c = config_at(&ws, R48, B);
        c.app_dirs = Some(env.dirs.clone());
        let mut s = OfflineShow::new(ws, mixer_config_at(R48, B), c).unwrap();
        s.settle_media();
        s
    };
    let request = |s: &mut OfflineShow, kind: &str| {
        let json = format!(
            r#"{{"type":"request","id":{},"request":{{"type":"{kind}","path":{}}}}}"#,
            s.request_id().0,
            serde_json::to_string(path.to_str().unwrap()).unwrap()
        );
        app(s, &json)
    };
    let mut s = engine();
    for (i, c) in cues.iter().enumerate() {
        add(&mut s, c, i);
    }
    request(&mut s, "save_as");
    let first = script(&mut s);
    assert!(first.iter().any(|&v| v != 0.0));
    // A fresh engine opens the file as the app does, and plays it.
    let mut again = engine();
    let replies = request(&mut again, "open");
    let opened = replies
        .iter()
        .find_map(|m| match m {
            ServerMessage::Result {
                outcome: Outcome::Ok(ok),
                ..
            } => match &ok.data {
                Some(openlab_core::protocol::ResultData::Opened { doc }) => Some(*doc),
                _ => None,
            },
            _ => None,
        })
        .expect("not opened");
    for other in again.control().doc_ids() {
        if other != opened {
            again.close_doc(other, true).unwrap();
        }
    }
    again.settle_media();
    // The same show; only its id is new (the fresh engine's untitled doc had that id open).
    let mut reopened = again.control().workspace().clone();
    reopened.id = s.control().workspace().id;
    assert_eq!(&reopened, s.control().workspace());
    let second = script(&mut again);
    assert_eq!(first.len(), second.len());
    assert!(first == second, "the reopened show sounds different");
}
