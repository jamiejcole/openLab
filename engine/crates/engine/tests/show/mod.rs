// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

//! Helpers for the control-loop tests: small workspaces, WAV files, and an offline show at
//! 8 kHz in 64-frame blocks (sample counts stay small and exact).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use openlab_audio::MixerConfig;
use openlab_core::protocol::{
    RuntimeChange, ServerMessage, TransportAction, WireBrokenReason, WireCueState, WireWarning,
    decode_server, encode_server,
};
use openlab_core::{
    ContinueMode, Crosspoint, Cue, CueColor, CueId, CueKind, CueLevels, CueList, CueListId,
    CueOutput, DeviceInfo, FadeCurve, FadeSpec, MachineSettings, MediaRef, OutputPatch, PatchId,
    PatchMapping, PatchOutput, SecondTrigger, SliceMarker, TransportHotkeys, Triggers, Workspace,
    WorkspaceId, WorkspaceSettings,
};
use openlab_engine::control::ControlConfig;
use openlab_engine::control::offline::OfflineShow;
use uuid::Uuid;

pub mod device;

pub const RATE: u32 = 8000;
pub const BLOCK: u64 = 64;

pub fn id(n: u128) -> CueId {
    CueId(Uuid::from_u128(n))
}

pub fn list_id() -> CueListId {
    CueListId(Uuid::from_u128(900))
}

pub fn patch_id() -> PatchId {
    PatchId(Uuid::from_u128(901))
}

/// Seconds for `frames` at [`RATE`].
pub fn secs(frames: u64) -> f64 {
    frames as f64 / f64::from(RATE)
}

pub fn cue(n: u128, kind: CueKind) -> Cue {
    Cue {
        id: id(n),
        number: Some(n.to_string()),
        name: format!("cue {n}"),
        notes: String::new(),
        color: CueColor::None,
        flagged: false,
        armed: true,
        pre_wait: 0.0,
        post_wait: 0.0,
        continue_mode: ContinueMode::DoNotContinue,
        triggers: Triggers {
            hotkey: None,
            osc: None,
        },
        second_trigger: SecondTrigger::Nothing,
        second_trigger_on_release: false,
        kind,
    }
}

/// A mono audio cue on `path`, routed to cue output `out` only, all levels 0 dB.
pub fn audio(n: u128, path: &Path, out: u32) -> Cue {
    cue(
        n,
        CueKind::Audio {
            media: Some(MediaRef::new(path.to_str().unwrap())),
            patch: patch_id(),
            start_time: 0.0,
            end_time: None,
            fade_in: FadeSpec {
                duration: 0.0,
                curve: FadeCurve::Linear,
            },
            fade_out: FadeSpec {
                duration: 0.0,
                curve: FadeCurve::Linear,
            },
            slices: vec![],
            last_slice_play_count: 1,
            levels: CueLevels {
                master_db: 0.0,
                outputs: vec![
                    CueOutput {
                        db: 0.0,
                        mute: false,
                    };
                    2
                ],
                crosspoints: vec![Crosspoint {
                    input: 0,
                    output: out,
                    db: 0.0,
                    mute: false,
                }],
                gangs: vec![],
            },
            preload: false,
        },
    )
}

/// Sets an audio cue's slice markers.
pub fn with_slices(mut c: Cue, markers: &[(f64, u32)], last: u32) -> Cue {
    if let CueKind::Audio {
        slices,
        last_slice_play_count,
        ..
    } = &mut c.kind
    {
        *slices = markers
            .iter()
            .map(|&(time, play_count)| SliceMarker { time, play_count })
            .collect();
        *last_slice_play_count = last;
    }
    c
}

pub fn workspace(cues: Vec<Cue>) -> Workspace {
    Workspace {
        id: WorkspaceId(Uuid::from_u128(1)),
        name: "test".into(),
        settings: WorkspaceSettings {
            min_go_interval: 0.0,
            panic_duration: 0.1,
            autosave_interval: 30.0,
            osc_passcode: None,
            master_db: 0.0,
            hotkeys: TransportHotkeys::default(),
            auto_number_start: 1.0,
            auto_number_increment: 1.0,
        },
        patches: vec![OutputPatch {
            id: patch_id(),
            name: "Main".into(),
            master_db: 0.0,
            outputs: vec![
                PatchOutput {
                    level_db: 0.0,
                    mute: false,
                    delay_ms: 0.0,
                    polarity_inverted: false,
                };
                2
            ],
        }],
        network_patches: vec![],
        cue_lists: vec![CueList {
            id: list_id(),
            name: "Main".into(),
            cues,
        }],
    }
}

pub fn mixer_config() -> MixerConfig {
    MixerConfig {
        sample_rate: RATE,
        output_channels: 2,
        max_block_frames: BLOCK as usize,
        max_voices: 64,
        max_voice_channels: 8,
        patches: 2,
        lookahead_ms: 500,
    }
}

/// Patch mapped to the offline device's two outputs.
pub fn config(ws: &Workspace) -> ControlConfig {
    let mut c = ControlConfig::new(RATE, BLOCK);
    c.seed = 7;
    c.devices = vec![DeviceInfo {
        name: "Offline".into(),
        output_channels: 2,
    }];
    let mut machine = MachineSettings::default();
    machine
        .set_patch_mapping(
            ws.id,
            patch_id(),
            PatchMapping {
                device: "Offline".into(),
                channels: vec![0, 1],
            },
        )
        .unwrap();
    c.machine = machine;
    c.mixer_patches = 2;
    c
}

/// An offline show of `ws` with every probe answered.
pub fn show(ws: Workspace) -> OfflineShow {
    let c = config(&ws);
    show_with(ws, c)
}

pub fn show_with(ws: Workspace, config: ControlConfig) -> OfflineShow {
    let mut s = OfflineShow::new(ws, mixer_config(), config).unwrap();
    s.settle_media();
    s
}

pub fn go(s: &mut OfflineShow) -> u64 {
    let at = s.now();
    s.transport(TransportAction::Go { list: list_id() });
    at
}

pub fn start(s: &mut OfflineShow, n: u128) -> u64 {
    let at = s.now();
    s.transport(TransportAction::Start { cue: id(n) });
    at
}

/// Writes a mono 32-bit float WAV at [`RATE`] to a fresh file.
pub fn wav(samples: &[f32]) -> PathBuf {
    let data_len = samples.len() * 4;
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
    for x in samples {
        v.extend_from_slice(&x.to_le_bytes());
    }
    file("wav", &v)
}

/// Writes `bytes` to a fresh file (tests run in parallel and must never share one).
pub fn file(ext: &str, bytes: &[u8]) -> PathBuf {
    let p = fresh_path(ext);
    std::fs::write(&p, bytes).unwrap();
    p
}

pub fn fresh_path(ext: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("control-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("{n}.{ext}"))
}

/// `frames` samples of `value`.
pub fn dc(frames: usize, value: f32) -> Vec<f32> {
    vec![value; frames]
}

/// A ramp whose every sample is distinct: sample `i` is `(i + 1) / 2^20`.
pub fn ramp(frames: usize) -> Vec<f32> {
    (0..frames).map(|i| (i + 1) as f32 / 1_048_576.0).collect()
}

pub fn first_nonzero(x: &[f32]) -> Option<usize> {
    x.iter().position(|&v| v != 0.0)
}

pub fn last_nonzero(x: &[f32]) -> Option<usize> {
    x.iter().rposition(|&v| v != 0.0)
}

/// Every runtime change broadcast in `msgs`, in order; checks each message encodes and decodes.
pub fn changes(msgs: &[ServerMessage]) -> Vec<RuntimeChange> {
    msgs.iter()
        .inspect(|m| check_wire(m))
        .filter_map(|m| match m {
            ServerMessage::Runtime { changes, .. } => Some(changes.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

/// The states `cue` went through.
pub fn states(msgs: &[ServerMessage], cue: CueId) -> Vec<(WireCueState, Option<WireBrokenReason>)> {
    changes(msgs)
        .into_iter()
        .filter_map(|c| match c {
            RuntimeChange::CueState {
                cue: c,
                state,
                reason,
            } if c == cue => Some((state, reason)),
            _ => None,
        })
        .collect()
}

pub fn warnings(msgs: &[ServerMessage]) -> Vec<WireWarning> {
    changes(msgs)
        .into_iter()
        .filter_map(|c| match c {
            RuntimeChange::Warning { warning } => Some(warning),
            _ => None,
        })
        .collect()
}

/// A message the engine sends must encode, and decode back to itself.
pub fn check_wire(m: &ServerMessage) {
    let text = encode_server(m).unwrap();
    assert_eq!(&decode_server(&text).unwrap(), m, "{text}");
}

/// Steps until `cue` is idle (at most `limit` frames); returns the sample it was seen idle.
pub fn run_until_idle(s: &mut OfflineShow, cue: CueId, limit: u64) -> u64 {
    let until = s.rendered() + limit;
    while s.rendered() < until {
        if s.control().engine().state(cue) == openlab_core::CueState::Idle {
            return s.rendered();
        }
        s.step();
    }
    panic!("cue {cue} still {:?}", s.control().engine().state(cue));
}

static GATED: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
static PROBED: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
// One condvar per mutex: macOS pthreads refuse a condvar used with two mutexes.
static GATE_CHANGED: std::sync::Condvar = std::sync::Condvar::new();
static PROBED_CHANGED: std::sync::Condvar = std::sync::Condvar::new();

/// A probe that records each file it is asked for and blocks while the file is gated (a stalled
/// volume), then probes it for real.
pub fn gated_probe(path: &Path) -> Result<openlab_audio::Probed, openlab_audio::MediaError> {
    PROBED.lock().unwrap().push(path.to_path_buf());
    PROBED_CHANGED.notify_all();
    let mut gated = GATED.lock().unwrap();
    while gated.iter().any(|g| g == path) {
        gated = GATE_CHANGED.wait(gated).unwrap();
    }
    drop(gated);
    openlab_audio::probe(path)
}

pub fn gate(path: &Path) {
    GATED.lock().unwrap().push(path.to_path_buf());
}

pub fn release(path: &Path) {
    GATED.lock().unwrap().retain(|g| g != path);
    GATE_CHANGED.notify_all();
}

/// The order `gated_probe` was asked for these files in (repeats included).
pub fn probe_order(paths: &[PathBuf]) -> Vec<PathBuf> {
    PROBED
        .lock()
        .unwrap()
        .iter()
        .filter(|p| paths.contains(p))
        .cloned()
        .collect()
}

/// Waits (event-driven, bounded) until `gated_probe` has been asked for `n` of these files.
pub fn wait_probed(paths: &[PathBuf], n: usize) {
    let mut probed = PROBED.lock().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while probed.iter().filter(|p| paths.contains(p)).count() < n {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(!left.is_zero(), "probes never came");
        probed = PROBED_CHANGED.wait_timeout(probed, left).unwrap().0;
    }
}
