// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! The UI protocol (ARCHITECTURE section 6, docs/protocol.md): golden fixtures, codec and
//! hostile input. Regenerate the fixtures after an intended change with
//! `UPDATE_PROTOCOL_FIXTURES=1 cargo test -p openlab-core --test protocol`.

mod common;

use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::path::PathBuf;

use common::*;
use openlab_core::protocol::*;
use openlab_core::*;
use proptest::prelude::*;

enum Sample {
    Client(ClientMessage),
    Server(Box<ServerMessage>),
}

impl Sample {
    fn encode(&self) -> String {
        match self {
            Self::Client(m) => encode_client_pretty(m).unwrap(),
            Self::Server(m) => encode_server_pretty(m).unwrap(),
        }
    }
}

fn client(m: ClientMessage) -> Sample {
    Sample::Client(m)
}

fn server(m: ServerMessage) -> Sample {
    Sample::Server(Box::new(m))
}

fn workspace_id(n: u128) -> WorkspaceId {
    WorkspaceId(uuid::Uuid::from_u128(n))
}

fn rid(n: u64) -> RequestId {
    RequestId(n)
}

fn transport(n: u64, action: TransportAction) -> ClientMessage {
    ClientMessage::Transport { id: rid(n), action }
}

fn request(n: u64, request: Request) -> ClientMessage {
    ClientMessage::Request {
        id: rid(n),
        request,
    }
}

const LIST: u128 = 1;
fn list() -> CueListId {
    list_id(LIST)
}

fn sample_cue() -> Cue {
    Cue {
        number: Some("1".into()),
        name: "Intro".into(),
        ..cue(10, audio())
    }
}

fn snapshot() -> Snapshot {
    Snapshot {
        revision: 7,
        runtime_seq: 42,
        document: DocumentState {
            path: Some("/Users/sam/Shows/Hamlet.openlab".into()),
            dirty: true,
            can_undo: true,
            can_redo: false,
            show_mode: false,
        },
        workspace: workspace(vec![sample_cue(), cue(11, fade(10))]),
        cue_states: vec![CueStateEntry {
            cue: cue_id(10),
            state: WireCueState::Running,
            reason: None,
        }],
        problems: vec![CueProblems {
            cue: cue_id(11),
            problems: vec![WireProblem::TargetMissing { target: cue_id(99) }],
        }],
        group_warnings: vec![cue_id(12)],
        playheads: vec![PlayheadEntry {
            list: list(),
            cue: Some(cue_id(11)),
        }],
        previewing: vec![cue_id(10)],
        warnings: vec![WireWarning::Notice {
            code: "media_damaged".into(),
            message: "intro.wav has a damaged tail".into(),
            cue: Some(cue_id(10)),
        }],
        media_durations: vec![CueMediaDuration {
            cue: cue_id(10),
            file: Some(62.5),
            trimmed: Some(60.0),
            duration: Some(120.0),
        }],
    }
}

fn all_actions() -> Vec<(&'static str, TransportAction)> {
    let c = cue_id(10);
    vec![
        ("go", TransportAction::Go { list: list() }),
        ("panic", TransportAction::Panic),
        ("pause_all", TransportAction::PauseAll),
        ("resume_all", TransportAction::ResumeAll),
        (
            "set_playhead",
            TransportAction::SetPlayhead {
                list: list(),
                cue: Some(c),
            },
        ),
        (
            "set_playhead_by_number",
            TransportAction::SetPlayheadByNumber {
                list: list(),
                number: "12.5".into(),
            },
        ),
        (
            "playhead_next",
            TransportAction::PlayheadNext { list: list() },
        ),
        (
            "playhead_previous",
            TransportAction::PlayheadPrevious { list: list() },
        ),
        ("start", TransportAction::Start { cue: c }),
        (
            "stop",
            TransportAction::Stop {
                cue: c,
                fade_out: Some(FadeSpec {
                    duration: 2.5,
                    curve: FadeCurve::Linear,
                }),
            },
        ),
        ("hard_stop", TransportAction::HardStop { cue: c }),
        ("pause", TransportAction::Pause { cue: c }),
        ("resume", TransportAction::Resume { cue: c }),
        ("load", TransportAction::Load { cue: c, at: 12.25 }),
        ("reset", TransportAction::Reset { cue: c }),
        (
            "trigger_released",
            TransportAction::TriggerReleased { cue: c },
        ),
        ("preview", TransportAction::Preview { cue: c }),
        (
            "stop_preview",
            TransportAction::StopPreview { cue: Some(c) },
        ),
        // Added last: a fixture's request id is its position in this list.
        ("stop_all", TransportAction::StopAll),
        ("hard_stop_all", TransportAction::HardStopAll),
        ("panic_cue", TransportAction::PanicCue { cue: c }),
        (
            "go_from",
            TransportAction::GoFrom {
                list: list(),
                cue: c,
            },
        ),
        // Start/Pause Selected: items in selection order, one scheduling time.
        (
            "batch",
            TransportAction::Batch {
                actions: vec![
                    TransportAction::Start { cue: c },
                    TransportAction::Start { cue: cue_id(11) },
                    TransportAction::Pause { cue: cue_id(12) },
                    TransportAction::Resume { cue: cue_id(13) },
                ],
            },
        ),
    ]
}

fn all_requests() -> Vec<(&'static str, Request)> {
    vec![
        ("new", Request::New {}),
        (
            "open",
            Request::Open {
                path: "/Users/sam/Shows/Hamlet.openlab".into(),
                recovery: None,
            },
        ),
        ("save", Request::Save),
        (
            "save_as",
            Request::SaveAs {
                path: "/Users/sam/Shows/Hamlet 2.openlab".into(),
            },
        ),
        (
            "relink",
            Request::Relink {
                folder: "/Volumes/Audio/Hamlet".into(),
            },
        ),
        ("list_devices", Request::ListDevices),
        ("snapshot", Request::Snapshot),
        (
            "open_recovery",
            Request::OpenRecovery {
                workspace_id: workspace_id(7),
                path_hash: Some("0123456789abcdef".into()),
                modified_ms: Some(1_700_000_000_000),
            },
        ),
        ("scan_recovery", Request::ScanRecovery),
        ("log_tail", Request::LogTail { max: None }),
        (
            "set_output_overrides",
            Request::SetOutputOverrides {
                network_disabled: true,
            },
        ),
        ("preshow_check", Request::PreshowCheck),
        ("set_show_mode", Request::SetShowMode { on: true }),
        (
            "set_patch_mapping",
            Request::SetPatchMapping {
                patch: PatchId(uuid::Uuid::from_u128(0x70)),
                mapping: Some(PatchMapping {
                    device: "Scarlett 18i20".into(),
                    channels: vec![2, 3],
                }),
            },
        ),
    ]
}

/// Requests with their flags set, beyond the one sample per type of [`all_requests`].
fn request_variants() -> Vec<(&'static str, Request)> {
    vec![
        (
            "open_restore",
            Request::Open {
                path: "/Users/sam/Shows/Hamlet.openlab".into(),
                recovery: Some(RecoveryChoice::Restore),
            },
        ),
        (
            "open_ignore_recovery",
            Request::Open {
                path: "/Users/sam/Shows/Hamlet.openlab".into(),
                recovery: Some(RecoveryChoice::Ignore),
            },
        ),
        (
            "open_recovery_newest",
            Request::OpenRecovery {
                workspace_id: workspace_id(7),
                path_hash: None,
                modified_ms: None,
            },
        ),
        ("log_tail_max", Request::LogTail { max: Some(50) }),
        (
            "clear_patch_mapping",
            Request::SetPatchMapping {
                patch: PatchId(uuid::Uuid::from_u128(0x70)),
                mapping: None,
            },
        ),
    ]
}

/// `(file name without extension, message)`, grouped by direction. The first fixture of a
/// type is named after the type.
fn fixtures() -> Vec<(String, Sample)> {
    let mut out: Vec<(String, Sample)> = Vec::new();
    let mut add = |name: &str, sample: Sample| {
        let dir = match sample {
            Sample::Client(_) => "client",
            Sample::Server(_) => "server",
        };
        out.push((format!("{dir}/{name}"), sample));
    };

    add(
        "hello",
        client(ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            token: "3f9a1c0e5b7d42a8".into(),
            client_name: "openLab for macOS 0.0.1".into(),
            workspace: Some(DocId(3)),
            pid: Some(4242),
        }),
    );
    add(
        "hello_app",
        client(ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            token: "3f9a1c0e5b7d42a8".into(),
            client_name: "openLab for macOS 0.0.1".into(),
            workspace: None,
            pid: Some(4242),
        }),
    );
    add(
        "hello_version_mismatch",
        client(ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION + 1,
            token: "3f9a1c0e5b7d42a8".into(),
            client_name: "openLab for macOS from the future".into(),
            workspace: None,
            pid: None,
        }),
    );
    add(
        "command",
        client(ClientMessage::Command {
            id: rid(1),
            command: Box::new(Command::SetCueProperty {
                id: cue_id(10),
                property: CueProperty::Name {
                    value: "Storm".into(),
                },
            }),
            coalesce: None,
        }),
    );
    add(
        "command_coalesce",
        client(ClientMessage::Command {
            id: rid(3),
            command: Box::new(Command::SetWorkspaceSetting {
                setting: WorkspaceSetting::MasterDb { value: -6.0 },
            }),
            coalesce: Some("master:1".into()),
        }),
    );
    add(
        "command_add_cue",
        client(ClientMessage::Command {
            id: rid(2),
            command: Box::new(Command::Transaction {
                commands: vec![
                    Command::AddCue {
                        at: Position {
                            list: list(),
                            parent: None,
                            index: 0,
                        },
                        cue: sample_cue(),
                        auto_number: true,
                    },
                    Command::RemoveCue { id: cue_id(11) },
                ],
            }),
            coalesce: None,
        }),
    );
    // A Devamp cue's settings, `devamp_mode` included, travel as its kind.
    add(
        "command_set_devamp_kind",
        client(ClientMessage::Command {
            id: rid(3),
            command: Box::new(Command::SetCueProperty {
                id: cue_id(12),
                property: CueProperty::Kind {
                    value: CueKind::Devamp {
                        target: Some(cue_id(10)),
                        start_next_cue: true,
                        stop_target_when_slice_ends: false,
                        devamp_mode: DevampMode::LoopingCue,
                    },
                },
            }),
            coalesce: None,
        }),
    );
    // An Audio cue's file alone, and a raw clipboard to paste.
    add(
        "command_set_cue_media",
        client(ClientMessage::Command {
            id: rid(3),
            command: Box::new(Command::SetCueProperty {
                id: cue_id(12),
                property: CueProperty::Media {
                    value: Some(MediaRef::new("/Volumes/Show/thunder.wav")),
                },
            }),
            coalesce: None,
        }),
    );
    // A Wait or Fade cue's duration and a Load cue's time, alone (M5.6b).
    for (name, property) in [
        ("duration", CueProperty::Duration { value: 12.5 }),
        ("load_to_time", CueProperty::LoadToTime { value: 90.25 }),
    ] {
        add(
            &format!("command_set_cue_{name}"),
            client(ClientMessage::Command {
                id: rid(3),
                command: Box::new(Command::SetCueProperty {
                    id: cue_id(12),
                    property,
                }),
                coalesce: None,
            }),
        );
    }
    add(
        "command_paste_clipboard",
        client(ClientMessage::Command {
            id: rid(5),
            command: Box::new(Command::PasteClipboard {
                at: Position {
                    list: list(),
                    parent: Some(cue_id(10)),
                    index: 2,
                },
                cues: vec![sample_cue()],
                source_dir: None,
            }),
            coalesce: None,
        }),
    );
    add(
        "command_paste_clipboard_from_folder",
        client(ClientMessage::Command {
            id: rid(5),
            command: Box::new(Command::PasteClipboard {
                at: Position {
                    list: list(),
                    parent: Some(cue_id(10)),
                    index: 2,
                },
                cues: vec![sample_cue()],
                source_dir: Some("/Volumes/Show A".to_owned()),
            }),
            coalesce: None,
        }),
    );
    // A Network cue's destination, address and arguments.
    for property in [
        CueProperty::Destination { value: net_id(4) },
        CueProperty::Address {
            value: "/cue/1/start".into(),
        },
        CueProperty::Args {
            value: vec![
                OscArg::Int { value: 3 },
                OscArg::Float { value: 0.5 },
                OscArg::String { value: "go".into() },
                OscArg::Bool { value: true },
            ],
        },
    ] {
        add(
            &format!("command_set_network_cue_{}", tag(&property)),
            client(ClientMessage::Command {
                id: rid(3),
                command: Box::new(Command::SetCueProperty {
                    id: cue_id(12),
                    property,
                }),
                coalesce: None,
            }),
        );
    }
    // A Group's mode, every variant (the Mac app's encoding is checked against these).
    for mode in [
        GroupMode::Timeline,
        GroupMode::StartFirstAndEnter,
        GroupMode::StartFirstAndGoToNext,
        GroupMode::Playlist {
            loop_: true,
            shuffle: false,
        },
        GroupMode::StartRandom,
    ] {
        add(
            &format!("command_set_group_mode_{}", tag(&mode)),
            client(ClientMessage::Command {
                id: rid(3),
                command: Box::new(Command::SetCueProperty {
                    id: cue_id(12),
                    property: CueProperty::GroupMode { value: mode },
                }),
                coalesce: None,
            }),
        );
    }
    for (name, command) in model_commands() {
        add(
            &format!("command_{name}"),
            client(ClientMessage::Command {
                id: rid(4),
                command: Box::new(command),
                coalesce: None,
            }),
        );
    }
    for (i, (name, action)) in all_actions().into_iter().enumerate() {
        let file = if i == 0 {
            "transport".to_owned()
        } else {
            format!("transport_{name}")
        };
        add(&file, client(transport(10 + i as u64, action)));
    }
    add("undo", client(ClientMessage::Undo { id: rid(3) }));
    add("redo", client(ClientMessage::Redo { id: rid(4) }));
    for (i, (name, req)) in all_requests().into_iter().enumerate() {
        let file = if i == 0 {
            "request".to_owned()
        } else {
            format!("request_{name}")
        };
        add(&file, client(request(40 + i as u64, req)));
    }
    for (i, (name, req)) in request_variants().into_iter().enumerate() {
        add(
            &format!("request_{name}"),
            client(request(60 + i as u64, req)),
        );
    }
    add(
        "close",
        client(ClientMessage::Close {
            id: rid(70),
            doc: None,
            discard_changes: false,
        }),
    );
    add(
        "close_doc",
        client(ClientMessage::Close {
            id: rid(71),
            doc: Some(DocId(3)),
            discard_changes: true,
        }),
    );
    add("panic", client(ClientMessage::Panic { id: rid(72) }));
    add(
        "hotkey",
        client(ClientMessage::Hotkey {
            id: rid(73),
            key: "space".into(),
            modifiers: Modifiers::default(),
            phase: HotkeyPhase::Down,
        }),
    );
    add(
        "hotkey_up",
        client(ClientMessage::Hotkey {
            id: rid(74),
            key: "f5".into(),
            modifiers: Modifiers {
                shift: true,
                control: false,
                alt: false,
                command: true,
            },
            phase: HotkeyPhase::Up,
        }),
    );
    add("goodbye", client(ClientMessage::Goodbye { shutdown: true }));

    add(
        "welcome",
        server(ServerMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            doc: Some(DocId(3)),
            snapshot: Some(Box::new(snapshot())),
            workspaces: None,
        }),
    );
    add(
        "welcome_app",
        server(ServerMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            doc: None,
            snapshot: None,
            workspaces: Some(workspace_list()),
        }),
    );
    add(
        "workspaces",
        server(ServerMessage::Workspaces {
            workspaces: workspace_list(),
        }),
    );
    add("closed", server(ServerMessage::Closed { doc: DocId(3) }));
    add(
        "result_ok_opened",
        server(ServerMessage::Result {
            id: rid(41),
            outcome: Outcome::Ok(OkBody {
                revision: None,
                data: Some(ResultData::Opened { doc: DocId(4) }),
            }),
        }),
    );
    add(
        "snapshot",
        server(ServerMessage::Snapshot {
            snapshot: Box::new(snapshot()),
        }),
    );
    add(
        "model_changed",
        server(ServerMessage::ModelChanged {
            revision: 8,
            upserts: vec![
                Upsert::Cue {
                    list: list(),
                    parent: None,
                    index: 0,
                    cue: Box::new(sample_cue()),
                },
                Upsert::CueList {
                    id: list(),
                    name: "Main".into(),
                    index: 0,
                },
                Upsert::Patch {
                    index: 0,
                    patch: patch(1, 2),
                },
                Upsert::NetworkPatch {
                    index: 0,
                    patch: workspace(vec![]).network_patches[0].clone(),
                },
                Upsert::Settings {
                    name: "Hamlet".into(),
                    settings: Box::new(workspace(vec![]).settings),
                },
            ],
            deletes: vec![
                Delete::Cue { id: cue_id(11) },
                Delete::CueList { id: list_id(2) },
                Delete::Patch { id: patch_id(2) },
                Delete::NetworkPatch { id: net_id(2) },
            ],
        }),
    );
    add(
        "document",
        server(ServerMessage::Document {
            document: DocumentState {
                path: None,
                dirty: false,
                can_undo: false,
                can_redo: false,
                show_mode: false,
            },
        }),
    );
    add(
        "document_show_mode",
        server(ServerMessage::Document {
            document: DocumentState {
                path: Some("/Users/sam/Shows/Hamlet.openlab".into()),
                dirty: false,
                can_undo: true,
                can_redo: false,
                show_mode: true,
            },
        }),
    );
    add(
        "runtime",
        server(ServerMessage::Runtime {
            seq: 43,
            changes: vec![
                RuntimeChange::CueState {
                    cue: cue_id(10),
                    state: WireCueState::Broken,
                    reason: Some(WireBrokenReason::DeviceLost),
                },
                RuntimeChange::CueState {
                    cue: cue_id(11),
                    state: WireCueState::PreWaiting,
                    reason: None,
                },
                RuntimeChange::Problems {
                    cue: cue_id(10),
                    problems: vec![
                        WireProblem::NoTarget,
                        WireProblem::MediaMissing {
                            media: MediaRef::new("audio/storm.wav"),
                        },
                        WireProblem::DeviceUnavailable {
                            patch: patch_id(1),
                            resolution: WireResolution::ChannelsMissing {
                                device: "Scarlett 2i2".into(),
                                needed: 4,
                                available: 2,
                            },
                        },
                    ],
                },
                RuntimeChange::GroupWarning {
                    group: cue_id(12),
                    warning: true,
                },
                RuntimeChange::Playhead {
                    list: list(),
                    cue: None,
                },
                RuntimeChange::Preview {
                    cue: cue_id(10),
                    active: true,
                },
                RuntimeChange::Warning {
                    warning: WireWarning::TriggerIgnored {
                        cue: cue_id(10),
                        state: WireCueState::Running,
                    },
                },
            ],
        }),
    );
    // A probed cue whose last slice loops for ever (no `duration`), and a cue whose file
    // went missing (every field absent: not known).
    add(
        "runtime_media_duration",
        server(ServerMessage::Runtime {
            seq: 44,
            changes: vec![
                RuntimeChange::MediaDuration(CueMediaDuration {
                    cue: cue_id(10),
                    file: Some(62.5),
                    trimmed: Some(60.25),
                    duration: None,
                }),
                RuntimeChange::MediaDuration(CueMediaDuration {
                    cue: cue_id(11),
                    file: None,
                    trimmed: None,
                    duration: None,
                }),
            ],
        }),
    );
    add(
        "progress",
        server(ServerMessage::Progress {
            items: vec![
                ProgressItem {
                    cue: cue_id(10),
                    elapsed: 1.5,
                    remaining: Some(58.5),
                },
                ProgressItem {
                    cue: cue_id(11),
                    elapsed: 0.25,
                    remaining: None,
                },
            ],
            waits: vec![],
        }),
    );
    // Pre-wait and post-wait countdowns; cue 12's Wait action runs beside its post-wait.
    add(
        "progress_waits",
        server(ServerMessage::Progress {
            items: vec![ProgressItem {
                cue: cue_id(12),
                elapsed: 1.0,
                remaining: Some(4.0),
            }],
            waits: vec![
                WaitProgress {
                    cue: cue_id(12),
                    wait: WireWait::PostWait,
                    elapsed: 1.0,
                    remaining: 2.0,
                },
                WaitProgress {
                    cue: cue_id(13),
                    wait: WireWait::PreWait,
                    elapsed: 0.5,
                    remaining: 1.5,
                },
            ],
        }),
    );
    add(
        "meters",
        server(ServerMessage::Meters {
            outputs: vec![OutputMeter {
                patch: patch_id(1),
                peaks: vec![0.5, 0.25],
            }],
            cues: vec![CueMeter {
                cue: cue_id(10),
                peaks: vec![0.5, 0.0],
            }],
        }),
    );
    add(
        "log",
        server(ServerMessage::Log {
            level: LogLevel::Warn,
            message: "audio device \"Scarlett 2i2\" was unplugged".into(),
        }),
    );
    add(
        "log_entry",
        server(ServerMessage::LogEntry {
            seq: 41,
            entry: log_entry_sample(LogKind::Trigger),
        }),
    );
    add(
        "log_entry_document",
        server(ServerMessage::LogEntry {
            seq: 42,
            entry: log_entry_sample(LogKind::Document),
        }),
    );
    add(
        "status",
        server(ServerMessage::Status {
            status: Box::new(status_sample()),
        }),
    );
    let mut lost = status_sample();
    lost.cpu_percent = None;
    lost.engine_load = None;
    lost.disk.healthy = false;
    lost.network_output_disabled = true;
    lost.device = DeviceStatus {
        name: None,
        state: DeviceState::Lost,
        sample_rate: 48_000,
        block_frames: None,
        channels: 2,
        latency_ms: None,
    };
    add(
        "status_device_lost",
        server(ServerMessage::Status {
            status: Box::new(lost),
        }),
    );
    add(
        "warnings",
        server(ServerMessage::Warnings {
            warnings: vec![
                WireWarning::Notice {
                    code: "file_repaired".into(),
                    message: "duplicate cue number 3 was cleared".into(),
                    cue: None,
                },
                WireWarning::LateStart {
                    cue: cue_id(10),
                    late_ms: 12,
                },
            ],
        }),
    );
    add(
        "result",
        server(ServerMessage::Result {
            id: rid(1),
            outcome: Outcome::Ok(OkBody {
                revision: Some(8),
                data: None,
            }),
        }),
    );
    add(
        "result_error",
        server(ServerMessage::Result {
            id: rid(2),
            outcome: Outcome::Error(ErrorBody::new(
                ErrorCode::CommandRefused,
                format!("cue {} does not exist", cue_id(99)),
            )),
        }),
    );
    add(
        "result_devices",
        server(ServerMessage::Result {
            id: rid(46),
            outcome: Outcome::Ok(OkBody {
                revision: None,
                data: Some(ResultData::Devices {
                    devices: vec![DeviceEntry {
                        name: "Scarlett 2i2".into(),
                        output_channels: 2,
                    }],
                }),
            }),
        }),
    );
    add(
        "result_log_tail",
        server(ServerMessage::Result {
            id: rid(47),
            outcome: Outcome::Ok(OkBody {
                revision: None,
                data: Some(ResultData::LogTail {
                    entries: all_log_kinds().into_iter().map(log_entry_sample).collect(),
                    dropped: 2,
                    seq: 1009,
                }),
            }),
        }),
    );
    add(
        "result_preshow_check",
        server(ServerMessage::Result {
            id: rid(49),
            outcome: Outcome::Ok(OkBody {
                revision: None,
                data: Some(ResultData::PreshowCheck(Box::new(preshow_report_sample()))),
            }),
        }),
    );
    add(
        "result_preshow_check_ok",
        server(ServerMessage::Result {
            id: rid(50),
            outcome: Outcome::Ok(OkBody {
                revision: None,
                data: Some(ResultData::PreshowCheck(Box::new(PreshowReport {
                    ok: true,
                    summary: PreshowSummary::default(),
                    broken_cues: vec![],
                    media: vec![],
                    patches: vec![],
                    network: vec![],
                    system: vec![],
                }))),
            }),
        }),
    );
    add(
        "result_relink",
        server(ServerMessage::Result {
            id: rid(45),
            outcome: Outcome::Ok(OkBody {
                revision: Some(9),
                data: Some(ResultData::Relink {
                    relinked: 3,
                    ambiguous: vec![AmbiguousMatch {
                        cue: cue_id(10),
                        candidates: vec!["a/storm.wav".into(), "b/storm.wav".into()],
                    }],
                    unmatched: vec![cue_id(11)],
                    truncated: true,
                    skipped: 2,
                }),
            }),
        }),
    );
    add(
        "result_recovery_offer",
        server(ServerMessage::Result {
            id: rid(47),
            outcome: Outcome::Ok(OkBody {
                revision: None,
                data: Some(ResultData::RecoveryOffer {
                    path: "/Users/sam/Shows/Hamlet.openlab".into(),
                    workspace_name: "Hamlet".into(),
                    modified_ms: 1_700_000_000_000,
                }),
            }),
        }),
    );
    add(
        "result_recovery_entries",
        server(ServerMessage::Result {
            id: rid(48),
            outcome: Outcome::Ok(OkBody {
                revision: None,
                data: Some(ResultData::RecoveryEntries {
                    entries: vec![
                        RecoveryEntryInfo {
                            workspace_id: workspace_id(7),
                            path_hash: Some("0123456789abcdef".into()),
                            show_name: Some("Hamlet".into()),
                            modified_ms: Some(1_700_000_000_000),
                            stale: false,
                        },
                        RecoveryEntryInfo {
                            workspace_id: workspace_id(8),
                            path_hash: None,
                            show_name: None,
                            modified_ms: None,
                            stale: true,
                        },
                    ],
                }),
            }),
        }),
    );
    add(
        "result_not_found",
        server(ServerMessage::Result {
            id: rid(50),
            outcome: Outcome::Error(ErrorBody::new(
                ErrorCode::NotFound,
                "cannot open /Users/sam/Shows/Hamlet.openlab: No such file or directory",
            )),
        }),
    );
    add(
        "result_unsaved_changes",
        server(ServerMessage::Result {
            id: rid(49),
            outcome: Outcome::Error(ErrorBody::new(
                ErrorCode::UnsavedChanges,
                "the workspace has unsaved changes",
            )),
        }),
    );
    add(
        "error",
        server(ServerMessage::refusal(
            ErrorCode::ProtocolVersionMismatch,
            "this engine speaks protocol 1; the client sent 2",
        )),
    );
    extra_fixtures(&mut add);
    out
}

/// Two open workspaces, a saved one with unsaved edits and an untitled one.
fn workspace_list() -> Vec<WorkspaceInfo> {
    vec![
        WorkspaceInfo {
            doc: DocId(0),
            workspace_id: workspace_id(7),
            name: "Hamlet".into(),
            path: Some("/Users/sam/Shows/Hamlet.openlab".into()),
            dirty: true,
        },
        WorkspaceInfo {
            doc: DocId(3),
            workspace_id: workspace_id(8),
            name: "Untitled".into(),
            path: None,
            dirty: false,
        },
    ]
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/protocol")
}

/// Ok if `committed` is what the types produce now; otherwise the message says how to fix it.
fn check_fixture(name: &str, committed: Option<&str>, current: &str) -> Result<(), String> {
    match committed {
        Some(text) if text == current => Ok(()),
        Some(_) => Err(format!(
            "protocol fixture {name}.json has drifted from the Rust types. If the change is \
             intended (and the Swift types and docs/protocol.md follow), regenerate with \
             UPDATE_PROTOCOL_FIXTURES=1 cargo test -p openlab-core --test protocol"
        )),
        None => Err(format!(
            "protocol fixture {name}.json is missing. Generate it with \
             UPDATE_PROTOCOL_FIXTURES=1 cargo test -p openlab-core --test protocol"
        )),
    }
}

fn json_files(dir: &std::path::Path) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "json") {
            found.insert(path.file_stem().unwrap().to_string_lossy().into_owned());
        }
    }
    found
}

#[test]
fn golden_fixtures_match_the_types() {
    let update = std::env::var_os("UPDATE_PROTOCOL_FIXTURES").is_some_and(|v| v == "1");
    let root = fixture_dir();
    let all = fixtures();
    let mut failures = Vec::new();
    for (name, sample) in &all {
        let path = root.join(format!("{name}.json"));
        let current = sample.encode();
        if update {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &current).unwrap();
            continue;
        }
        let committed = std::fs::read_to_string(&path).ok();
        if let Err(e) = check_fixture(name, committed.as_deref(), &current) {
            failures.push(e);
        }
    }
    if update {
        // Drop files for messages that no longer exist.
        for dir in ["client", "server"] {
            let wanted: BTreeSet<String> = all
                .iter()
                .filter_map(|(n, _)| n.strip_prefix(&format!("{dir}/")).map(str::to_owned))
                .collect();
            for stale in json_files(&root.join(dir)).difference(&wanted) {
                std::fs::remove_file(root.join(dir).join(format!("{stale}.json"))).unwrap();
            }
        }
        return;
    }
    // A committed file nothing produces any more is drift too.
    for dir in ["client", "server"] {
        let wanted: BTreeSet<String> = all
            .iter()
            .filter_map(|(n, _)| n.strip_prefix(&format!("{dir}/")).map(str::to_owned))
            .collect();
        for stale in json_files(&root.join(dir)).difference(&wanted) {
            failures.push(format!(
                "protocol fixture {dir}/{stale}.json is committed but no longer generated; \
                 regenerate with UPDATE_PROTOCOL_FIXTURES=1 cargo test -p openlab-core --test protocol"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn drift_is_detected_by_comparison() {
    let current = "{\n  \"type\": \"undo\"\n}\n";
    assert!(check_fixture("client/undo", Some(current), current).is_ok());
    let drifted = check_fixture("client/undo", Some("{}\n"), current).unwrap_err();
    assert!(drifted.contains("drifted") && drifted.contains("UPDATE_PROTOCOL_FIXTURES=1"));
    let missing = check_fixture("client/undo", None, current).unwrap_err();
    assert!(missing.contains("missing") && missing.contains("UPDATE_PROTOCOL_FIXTURES=1"));
    // Trailing whitespace counts: the files are compared byte for byte.
    assert!(check_fixture("x", Some("{}"), "{}\n").is_err());
}

/// `revert` is gone; `new` is a bare object, and a `discard_changes` an older client
/// still sends is an unknown field, ignored.
#[test]
fn revert_is_gone_and_new_is_bare() {
    let frame = |request: &str| {
        decode_client(&format!(
            r#"{{"type":"request","id":1,"request":{request}}}"#
        ))
    };
    assert_eq!(frame(r#"{"type":"new"}"#), Ok(request(1, Request::New {})));
    assert_eq!(
        frame(r#"{"type":"new","discard_changes":true}"#),
        Ok(request(1, Request::New {}))
    );
    assert!(matches!(
        frame(r#"{"type":"revert"}"#),
        Err(ProtocolError::Malformed(_))
    ));
    let new = encode_client(&request(1, Request::New {})).unwrap();
    assert!(new.contains(r#""request":{"type":"new"}"#), "{new}");
}

#[test]
fn a_hello_without_a_workspace_is_the_app_connection() {
    let text = format!(
        r#"{{"type":"hello","protocol_version":{PROTOCOL_VERSION},"token":"t","client_name":"c"}}"#
    );
    assert!(matches!(
        decode_hello(&text),
        Ok(ClientMessage::Hello {
            workspace: None,
            ..
        })
    ));
    let bound = format!(
        r#"{{"type":"hello","protocol_version":{PROTOCOL_VERSION},"token":"t","client_name":"c","workspace":5}}"#
    );
    assert!(matches!(
        decode_hello(&bound),
        Ok(ClientMessage::Hello {
            workspace: Some(DocId(5)),
            ..
        })
    ));
    // A doc is a number, never a string or negative.
    for bad in [r#""5""#, "-1", "1.5"] {
        let text = format!(
            r#"{{"type":"hello","protocol_version":{PROTOCOL_VERSION},"token":"t","client_name":"c","workspace":{bad}}}"#
        );
        assert!(decode_hello(&text).is_err(), "{bad}");
    }
}

/// The app says its process id in hello, so the engine can tell a slow reconnect from a
/// crash; a client that does not say it still connects.
#[test]
fn a_hello_may_carry_the_client_pid() {
    let with = format!(
        r#"{{"type":"hello","protocol_version":{PROTOCOL_VERSION},"token":"t","client_name":"c","pid":812}}"#
    );
    assert!(matches!(
        decode_hello(&with),
        Ok(ClientMessage::Hello { pid: Some(812), .. })
    ));
    let without = format!(
        r#"{{"type":"hello","protocol_version":{PROTOCOL_VERSION},"token":"t","client_name":"c"}}"#
    );
    assert!(matches!(
        decode_hello(&without),
        Ok(ClientMessage::Hello { pid: None, .. })
    ));
    // A pid is a non-negative 32-bit number.
    for bad in [r#""812""#, "-1", "4294967296", "1.5"] {
        let text = format!(
            r#"{{"type":"hello","protocol_version":{PROTOCOL_VERSION},"token":"t","client_name":"c","pid":{bad}}}"#
        );
        assert!(decode_hello(&text).is_err(), "{bad}");
    }
}

#[test]
fn a_hotkey_key_is_bounded() {
    let frame = |key: &str| decode_client(&format!(r#"{{"type":"hotkey","id":1,"key":"{key}"}}"#));
    assert_eq!(
        frame("a"),
        Ok(ClientMessage::Hotkey {
            id: RequestId(1),
            key: "a".into(),
            modifiers: Modifiers::default(),
            phase: HotkeyPhase::Down,
        })
    );
    for key in [String::new(), "k".repeat(MAX_HOTKEY_KEY + 1)] {
        assert!(
            matches!(frame(&key), Err(ProtocolError::OutOfRange { .. })),
            "{key:?}"
        );
    }
    assert!(frame(&"k".repeat(MAX_HOTKEY_KEY)).is_ok());
}

#[test]
fn relative_paths_are_refused_for_open_and_save_as() {
    for request in [
        r#"{"type":"open","path":"show.openlab"}"#,
        r#"{"type":"open","path":"../show.openlab"}"#,
        r#"{"type":"save_as","path":"out/show.openlab"}"#,
    ] {
        let text = format!(r#"{{"type":"request","id":1,"request":{request}}}"#);
        assert!(
            matches!(decode_client(&text), Err(ProtocolError::NotAbsolute { .. })),
            "{request}"
        );
    }
    assert!(
        decode_client(r#"{"type":"request","id":1,"request":{"type":"open","path":"/a.openlab"}}"#)
            .is_ok()
    );
}

#[test]
fn bad_workspace_requests_are_errors_not_panics() {
    for text in [
        r#"{"type":"request","id":1,"request":{"type":"open","path":"/a","recovery":"maybe"}}"#,
        r#"{"type":"close","id":1,"doc":"one"}"#,
        r#"{"type":"close","id":1,"discard_changes":"yes"}"#,
        r#"{"type":"hotkey","id":1,"key":"a","phase":"sideways"}"#,
        r#"{"type":"panic"}"#,
        r#"{"type":"request","id":1,"request":{"type":"open_recovery"}}"#,
        r#"{"type":"request","id":1,"request":{"type":"open_recovery","workspace_id":"nope"}}"#,
    ] {
        assert!(decode_client(text).is_err(), "{text}");
    }
}

#[test]
fn every_fixture_decodes_and_re_encodes_byte_for_byte() {
    for (name, sample) in fixtures() {
        let path = fixture_dir().join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("{name}.json missing; regenerate the fixtures"));
        match sample {
            Sample::Client(expected) => {
                let decoded = decode_client(&text).unwrap();
                assert_eq!(decoded, expected, "{name}");
                assert_eq!(encode_client_pretty(&decoded).unwrap(), text, "{name}");
                let wire = encode_client(&decoded).unwrap();
                assert!(!wire.contains('\n'), "{name}: wire frames are compact");
                assert_eq!(decode_client(&wire).unwrap(), expected, "{name}");
            }
            Sample::Server(expected) => {
                let decoded = decode_server(&text).unwrap();
                assert_eq!(decoded, *expected, "{name}");
                assert_eq!(encode_server_pretty(&decoded).unwrap(), text, "{name}");
                let wire = encode_server(&decoded).unwrap();
                assert!(!wire.contains('\n'), "{name}: wire frames are compact");
                assert_eq!(decode_server(&wire).unwrap(), *expected, "{name}");
            }
        }
    }
}

#[test]
fn every_message_type_and_variant_has_a_fixture() {
    let mut client_types = BTreeSet::new();
    let mut server_types = BTreeSet::new();
    let mut files = BTreeSet::new();
    for (name, sample) in fixtures() {
        // The hand-written `type_name` tables say what serde writes as the frame's "type".
        let (json, type_name) = match &sample {
            Sample::Client(m) => {
                client_types.insert(m.type_name());
                (serde_json::to_value(m).unwrap(), m.type_name())
            }
            Sample::Server(m) => {
                server_types.insert(m.type_name());
                (serde_json::to_value(m).unwrap(), m.type_name())
            }
        };
        assert_eq!(json["type"], type_name, "{name}");
        files.insert(name);
    }
    let want = |list: &[&'static str]| list.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(client_types, want(CLIENT_TYPES));
    assert_eq!(server_types, want(SERVER_TYPES));
    // The first fixture of each type is named after it.
    for t in CLIENT_TYPES {
        assert!(files.contains(&format!("client/{t}")), "client/{t}");
    }
    for t in SERVER_TYPES {
        assert!(files.contains(&format!("server/{t}")), "server/{t}");
    }
    // Every transport action and request kind: the "type" in the file is the sub-fixture name.
    for (name, action) in all_actions() {
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["type"], name);
    }
    for (name, req) in all_requests() {
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["type"], name);
    }
    // Exhaustive: adding an action or request without a sample stops compiling here.
    for (_, action) in all_actions() {
        match action {
            TransportAction::Go { .. }
            | TransportAction::Panic
            | TransportAction::StopAll
            | TransportAction::HardStopAll
            | TransportAction::PanicCue { .. }
            | TransportAction::GoFrom { .. }
            | TransportAction::PauseAll
            | TransportAction::ResumeAll
            | TransportAction::SetPlayhead { .. }
            | TransportAction::SetPlayheadByNumber { .. }
            | TransportAction::PlayheadNext { .. }
            | TransportAction::PlayheadPrevious { .. }
            | TransportAction::Start { .. }
            | TransportAction::Stop { .. }
            | TransportAction::HardStop { .. }
            | TransportAction::Pause { .. }
            | TransportAction::Resume { .. }
            | TransportAction::Load { .. }
            | TransportAction::Reset { .. }
            | TransportAction::TriggerReleased { .. }
            | TransportAction::Preview { .. }
            | TransportAction::StopPreview { .. }
            | TransportAction::Batch { .. } => {}
        }
    }
    assert_eq!(all_actions().len(), 23);
    assert_eq!(all_requests().len(), 14);
}

#[test]
fn a_mismatched_hello_decodes_so_the_engine_can_refuse_it() {
    let text = r#"{"type":"hello","protocol_version":999,"token":"t","client_name":"old"}"#;
    match decode_client(text).unwrap() {
        ClientMessage::Hello {
            protocol_version, ..
        } => assert_ne!(protocol_version, PROTOCOL_VERSION),
        other => panic!("{other:?}"),
    }
    let refusal = ServerMessage::refusal(ErrorCode::ProtocolVersionMismatch, "speak 1");
    let wire = encode_server(&refusal).unwrap();
    assert!(wire.contains("\"protocol_version_mismatch\""));
    assert!(wire.contains(&format!("\"protocol_version\":{PROTOCOL_VERSION}")));
}

#[test]
fn unknown_fields_are_ignored_and_unknown_types_refused() {
    let hello =
        r#"{"type":"hello","protocol_version":1,"token":"t","client_name":"c","future":{"x":1}}"#;
    assert!(decode_client(hello).is_ok());
    let action = r#"{"type":"transport","id":1,"action":{"type":"panic","also":true},"extra":1}"#;
    assert!(decode_client(action).is_ok());
    assert_eq!(
        decode_client(r#"{"type":"teleport","id":1}"#),
        Err(ProtocolError::UnknownType("teleport".into()))
    );
    // A server type is not a client type and vice versa.
    assert!(matches!(
        decode_client(r#"{"type":"welcome"}"#),
        Err(ProtocolError::UnknownType(_))
    ));
    assert!(matches!(
        decode_server(r#"{"type":"hello"}"#),
        Err(ProtocolError::UnknownType(_))
    ));
    // An unknown action or request kind is a malformed message.
    assert!(matches!(
        decode_client(r#"{"type":"transport","id":1,"action":{"type":"explode"}}"#),
        Err(ProtocolError::Malformed(_))
    ));
    assert!(matches!(
        decode_client(r#"{"type":"request","id":1,"request":{"type":"format_disk"}}"#),
        Err(ProtocolError::Malformed(_))
    ));
}

#[test]
fn defaults_fill_optional_fields() {
    let goodbye = decode_client(r#"{"type":"goodbye"}"#).unwrap();
    assert_eq!(goodbye, ClientMessage::Goodbye { shutdown: false });
    let stop = decode_client(&format!(
        r#"{{"type":"transport","id":1,"action":{{"type":"stop","cue":"{}"}}}}"#,
        cue_id(1)
    ))
    .unwrap();
    assert_eq!(
        stop,
        transport(
            1,
            TransportAction::Stop {
                cue: cue_id(1),
                fade_out: None
            }
        )
    );
    let all = decode_client(r#"{"type":"transport","id":1,"action":{"type":"stop_preview"}}"#);
    assert_eq!(
        all,
        Ok(transport(1, TransportAction::StopPreview { cue: None }))
    );
}

#[test]
fn malformed_input_is_an_error_never_a_panic() {
    let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    let deep_obj = format!("{}1{}", "{\"a\":".repeat(5000), "}".repeat(5000));
    let c = cue_id(1);
    let bad: Vec<String> = vec![
        String::new(),
        "   ".into(),
        "null".into(),
        "[]".into(),
        "42".into(),
        "\"hello\"".into(),
        "{".into(),
        "{\"type\":".into(),
        "{}".into(),
        r#"{"type":null}"#.into(),
        r#"{"type":7}"#.into(),
        r#"{"type":["hello"]}"#.into(),
        r#"{"type":"hello"}"#.into(),
        r#"{"type":"hello","protocol_version":"1","token":"t","client_name":"c"}"#.into(),
        r#"{"type":"hello","protocol_version":-1,"token":"t","client_name":"c"}"#.into(),
        r#"{"type":"hello","protocol_version":4294967296,"token":"t","client_name":"c"}"#.into(),
        r#"{"type":"hello","protocol_version":1.5,"token":"t","client_name":"c"}"#.into(),
        r#"{"type":"hello","protocol_version":1,"token":null,"client_name":"c"}"#.into(),
        r#"{"type":"undo"}"#.into(),
        r#"{"type":"undo","id":-1}"#.into(),
        r#"{"type":"undo","id":1.5}"#.into(),
        r#"{"type":"undo","id":"1"}"#.into(),
        r#"{"type":"undo","id":18446744073709551616}"#.into(),
        r#"{"type":"undo","id":1e999}"#.into(),
        r#"{"type":"command","id":1}"#.into(),
        r#"{"type":"command","id":1,"command":{"type":"rm_rf"}}"#.into(),
        r#"{"type":"command","id":1,"command":{"type":"remove_cue","id":"nope"}}"#.into(),
        r#"{"type":"transport","id":1}"#.into(),
        format!(
            r#"{{"type":"transport","id":1,"action":{{"type":"load","cue":"{c}","at":1e999}}}}"#
        ),
        format!(
            r#"{{"type":"transport","id":1,"action":{{"type":"load","cue":"{c}","at":-1e999}}}}"#
        ),
        format!(r#"{{"type":"transport","id":1,"action":{{"type":"load","cue":"{c}","at":-1}}}}"#),
        format!(r#"{{"type":"transport","id":1,"action":{{"type":"load","cue":"{c}","at":NaN}}}}"#),
        format!(
            r#"{{"type":"transport","id":1,"action":{{"type":"load","cue":"{c}","at":null}}}}"#
        ),
        format!(r#"{{"type":"transport","id":1,"action":{{"type":"load","cue":"{c}","at":"1"}}}}"#),
        format!(r#"{{"type":"transport","id":1,"action":{{"type":"load","cue":"{c}"}}}}"#),
        format!(
            r#"{{"type":"transport","id":1,"action":{{"type":"stop","cue":"{c}","fade_out":{{"duration":-1,"curve":"linear"}}}}}}"#
        ),
        format!(
            r#"{{"type":"transport","id":1,"action":{{"type":"stop","cue":"{c}","fade_out":{{"duration":1e400,"curve":"linear"}}}}}}"#
        ),
        r#"{"type":"transport","id":1,"action":{"type":"go","list":"not-a-uuid"}}"#.into(),
        r#"{"type":"request","id":1,"request":{"type":"open"}}"#.into(),
        r#"{"type":"request","id":1,"request":{"type":"open","path":""}}"#.into(),
        r#"{"type":"request","id":1,"request":{"type":"save_as","path":""}}"#.into(),
        r#"{"type":"request","id":1,"request":{"type":"relink","folder":""}}"#.into(),
        r#"{"type":"request","id":1,"request":{"type":"relink","folder":"media/relative"}}"#.into(),
        format!(
            r#"{{"type":"request","id":1,"request":{{"type":"relink","folder":"/{}"}}}}"#,
            "a".repeat(5000)
        ),
        r#"{"type":"request","id":1,"request":{"type":"open","path":5}}"#.into(),
        r#"{"type":"goodbye","shutdown":"yes"}"#.into(),
        "\u{0}\u{1}\u{2}".into(),
        "{\"type\":\"undo\",\"id\":1}garbage".into(),
        deep,
        deep_obj,
    ];
    for text in &bad {
        assert!(
            decode_client(text).is_err(),
            "accepted: {}",
            &text[..text.len().min(120)]
        );
        // A client frame is never a valid server frame either way round, and must not panic.
        let _ = decode_server(text);
    }
    assert_eq!(
        decode_client(
            r#"{"type":"transport","id":1,"action":{"type":"load","cue":"00000000-0000-0000-0000-000000000001","at":-1}}"#
        ),
        Err(ProtocolError::OutOfRange {
            field: "transport.load.at"
        })
    );
}

/// A `batch` is refused whole when it is empty, too long, names a cue twice, carries
/// an action that is not per-cue (a nested batch among them) or an item out of range.
#[test]
fn a_batch_is_refused_whole_unless_well_formed() {
    let batch = |actions: Vec<TransportAction>| {
        // Written past `encode_client`, which refuses a bad batch too: a hostile peer would.
        let text =
            serde_json::to_string(&transport(1, TransportAction::Batch { actions })).unwrap();
        decode_client(&text)
    };
    let field = |field| Err(ProtocolError::OutOfRange { field });
    let start = |n| TransportAction::Start { cue: cue_id(n) };
    assert_eq!(batch(vec![]), field("transport.batch.actions"));
    assert_eq!(
        batch((0..=MAX_BATCH_ACTIONS as u128).map(start).collect()),
        field("transport.batch.actions")
    );
    assert_eq!(
        batch(vec![start(1), start(2), start(1)]),
        field("transport.batch.actions.cue")
    );
    // One cue paused and resumed in the same breath is a duplicate too.
    assert_eq!(
        batch(vec![
            TransportAction::Pause { cue: cue_id(1) },
            TransportAction::Resume { cue: cue_id(1) }
        ]),
        field("transport.batch.actions.cue")
    );
    for not_per_cue in [
        TransportAction::Go { list: list() },
        TransportAction::Panic,
        TransportAction::StopAll,
        TransportAction::PauseAll,
        TransportAction::TriggerReleased { cue: cue_id(2) },
        TransportAction::StopPreview { cue: None },
        TransportAction::Batch {
            actions: vec![start(2)],
        },
    ] {
        assert_eq!(
            batch(vec![start(1), not_per_cue.clone()]),
            field("transport.batch.actions.type"),
            "{not_per_cue:?}"
        );
    }
    assert_eq!(
        batch(vec![
            start(1),
            TransportAction::Load {
                cue: cue_id(2),
                at: -1.0
            }
        ]),
        field("transport.load.at")
    );
    // The largest batch fits a frame and decodes; every per-cue kind is accepted.
    let most: Vec<_> = (0..MAX_BATCH_ACTIONS as u128).map(start).collect();
    assert!(batch(most).is_ok());
    let kinds = vec![
        start(1),
        TransportAction::Stop {
            cue: cue_id(2),
            fade_out: None,
        },
        TransportAction::HardStop { cue: cue_id(3) },
        TransportAction::PanicCue { cue: cue_id(4) },
        TransportAction::Pause { cue: cue_id(5) },
        TransportAction::Resume { cue: cue_id(6) },
        TransportAction::Load {
            cue: cue_id(7),
            at: 0.0,
        },
        TransportAction::Reset { cue: cue_id(8) },
        TransportAction::Preview { cue: cue_id(9) },
        TransportAction::StopPreview {
            cue: Some(cue_id(10)),
        },
    ];
    assert_eq!(
        batch(kinds.clone()),
        Ok(transport(1, TransportAction::Batch { actions: kinds }))
    );
}

#[test]
fn oversized_frames_are_refused_before_parsing() {
    // Just over each limit: refused on length alone (the content is not even valid JSON).
    let over = |limit: usize| "x".repeat(limit + 1);
    assert_eq!(
        decode_client(&over(MAX_CLIENT_FRAME_BYTES)),
        Err(ProtocolError::TooLarge {
            len: MAX_CLIENT_FRAME_BYTES + 1,
            limit: MAX_CLIENT_FRAME_BYTES
        })
    );
    assert_eq!(
        decode_hello(&over(MAX_HELLO_FRAME_BYTES)),
        Err(ProtocolError::TooLarge {
            len: MAX_HELLO_FRAME_BYTES + 1,
            limit: MAX_HELLO_FRAME_BYTES
        })
    );
    assert!(matches!(
        decode_server(&over(MAX_SERVER_FRAME_BYTES)),
        Err(ProtocolError::TooLarge { .. })
    ));
    // At the limit, a frame is read (and found to be garbage, not too large).
    assert!(matches!(
        decode_client(&"x".repeat(MAX_CLIENT_FRAME_BYTES)),
        Err(ProtocolError::Malformed(_))
    ));
    const {
        assert!(MAX_HELLO_FRAME_BYTES < MAX_CLIENT_FRAME_BYTES);
        assert!(MAX_CLIENT_FRAME_BYTES < MAX_SERVER_FRAME_BYTES);
    }
    // A valid hello with a long client name is fine until the hello limit.
    let hello = |name: &str| {
        format!(r#"{{"type":"hello","protocol_version":1,"token":"t","client_name":"{name}"}}"#)
    };
    assert!(decode_hello(&hello(&"n".repeat(1000))).is_ok());
    assert!(matches!(
        decode_hello(&hello(&"n".repeat(MAX_HELLO_FRAME_BYTES))),
        Err(ProtocolError::TooLarge { .. })
    ));
    // The first frame must be a hello; a valid command is not.
    assert_eq!(
        decode_hello(r#"{"type":"undo","id":1}"#),
        Err(ProtocolError::UnknownType("undo".into()))
    );
    // The encoder keeps the same limits.
    let big = ClientMessage::Hello {
        protocol_version: 1,
        token: "t".into(),
        client_name: "n".repeat(MAX_CLIENT_FRAME_BYTES),
        workspace: None,
        pid: None,
    };
    assert!(matches!(
        encode_client(&big),
        Err(ProtocolError::TooLarge { .. })
    ));
}

#[test]
fn status_is_sanitised_not_refused() {
    let mut bad = status_sample();
    bad.cpu_percent = Some(f64::NAN);
    bad.engine_load = Some(f64::INFINITY);
    bad.device.latency_ms = Some(-1.0);
    let wire = encode_server(&ServerMessage::Status {
        status: Box::new(bad),
    })
    .unwrap();
    let ServerMessage::Status { status } = decode_server(&wire).unwrap() else {
        panic!("not a status");
    };
    assert_eq!(status.cpu_percent, Some(0.0));
    assert_eq!(status.engine_load, Some(0.0));
    assert_eq!(status.device.latency_ms, Some(0.0));
}

#[test]
fn a_log_entry_with_a_bad_show_time_is_sent_without_it() {
    let mut entry = log_entry_sample(LogKind::Go);
    entry.show_time = Some(f64::NAN);
    let wire = encode_server(&ServerMessage::LogEntry { seq: 3, entry }).unwrap();
    let ServerMessage::LogEntry { seq, entry } = decode_server(&wire).unwrap() else {
        panic!("not a log_entry");
    };
    assert_eq!((seq, entry.show_time), (3, None));
}

#[test]
fn a_log_tail_result_without_seq_still_decodes() {
    // Additive field: an answer from before reads as seq 0.
    let text =
        r#"{"type":"result","id":1,"ok":{"data":{"type":"log_tail","entries":[],"dropped":0}}}"#;
    let ServerMessage::Result { outcome, .. } = decode_server(text).unwrap() else {
        panic!("not a result");
    };
    let Outcome::Ok(OkBody {
        data: Some(ResultData::LogTail { seq, .. }),
        ..
    }) = outcome
    else {
        panic!("not a log_tail");
    };
    assert_eq!(seq, 0);
}

#[test]
fn a_status_without_log_seq_still_decodes() {
    // Additive field: a status from before it reads as seq 0.
    let mut json = serde_json::to_value(ServerMessage::Status {
        status: Box::new(status_sample()),
    })
    .unwrap();
    json["status"].as_object_mut().unwrap().remove("log_seq");
    let ServerMessage::Status { status } = decode_server(&json.to_string()).unwrap() else {
        panic!("not a status");
    };
    assert_eq!(status.log_seq, 0);
    assert_eq!(status.log_dropped, status_sample().log_dropped);
}

#[test]
fn progress_and_meters_are_sanitised_not_refused() {
    let progress = |elapsed: f64, remaining: Option<f64>| ServerMessage::Progress {
        items: vec![ProgressItem {
            cue: cue_id(1),
            elapsed,
            remaining,
        }],
        waits: vec![WaitProgress {
            cue: cue_id(2),
            wait: WireWait::PreWait,
            elapsed,
            remaining: remaining.unwrap_or(0.0),
        }],
    };
    let reread = |m: &ServerMessage| decode_server(&encode_server(m).unwrap()).unwrap();
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.001] {
        assert_eq!(reread(&progress(bad, None)), progress(0.0, None), "{bad}");
        assert_eq!(
            reread(&progress(2.0, Some(bad))),
            progress(2.0, Some(0.0)),
            "{bad}"
        );
    }
    assert_eq!(reread(&progress(1.0, Some(2.0))), progress(1.0, Some(2.0)));
    let meters = |peak: f32| ServerMessage::Meters {
        outputs: vec![OutputMeter {
            patch: patch_id(1),
            peaks: vec![peak, 0.5],
        }],
        cues: vec![CueMeter {
            cue: cue_id(1),
            peaks: vec![peak],
        }],
    };
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.5] {
        assert_eq!(
            reread(&meters(bad)),
            ServerMessage::Meters {
                outputs: vec![OutputMeter {
                    patch: patch_id(1),
                    peaks: vec![0.0, 0.5]
                }],
                cues: vec![CueMeter {
                    cue: cue_id(1),
                    peaks: vec![0.0]
                }],
            }
        );
    }
    assert_eq!(
        reread(&meters(1.5)),
        meters(1.5),
        "above full scale is a clip"
    );
    // The pretty (fixture) form sanitises too.
    assert!(
        !encode_server_pretty(&meters(f32::NAN))
            .unwrap()
            .contains("null")
    );
    // Client input stays strict.
    assert!(
        encode_client(&transport(
            1,
            TransportAction::Load {
                cue: cue_id(1),
                at: f64::NAN
            }
        ))
        .is_err()
    );
    // A server frame with a null where a number belongs is malformed.
    assert!(decode_server(r#"{"type":"progress","items":[{"cue":"00000000-0000-0000-0000-000000000001","elapsed":null,"remaining":null}]}"#).is_err());
}

#[test]
fn progress_waits_are_optional_and_strict() {
    // An engine before sends no `waits`: it reads as none, and none is not written.
    let bare = r#"{"type":"progress","items":[]}"#;
    assert_eq!(
        decode_server(bare).unwrap(),
        ServerMessage::Progress {
            items: vec![],
            waits: vec![],
        }
    );
    assert_eq!(encode_server(&decode_server(bare).unwrap()).unwrap(), bare);
    let wait = |body: &str| {
        decode_server(&format!(
            r#"{{"type":"progress","items":[],"waits":[{{"cue":"00000000-0000-0000-0000-000000000001",{body}}}]}}"#
        ))
    };
    assert!(wait(r#""wait":"pre_wait","elapsed":1,"remaining":2"#).is_ok());
    // A wait always has an end; an unknown kind is an error, as for any nested enum.
    assert!(wait(r#""wait":"pre_wait","elapsed":1,"remaining":null"#).is_err());
    assert!(wait(r#""wait":"action","elapsed":1,"remaining":2"#).is_err());
}

#[test]
fn a_result_is_ok_or_error_next_to_its_id() {
    let ok = decode_server(r#"{"type":"result","id":5,"ok":{}}"#).unwrap();
    assert_eq!(
        ok,
        ServerMessage::Result {
            id: rid(5),
            outcome: Outcome::Ok(OkBody::default())
        }
    );
    assert!(decode_server(r#"{"type":"result","id":5}"#).is_err());
    assert!(
        decode_server(r#"{"type":"result","id":5,"error":{"code":"nonsense","message":"x"}}"#)
            .is_err()
    );
}

#[test]
fn command_errors_map_to_a_refusal_code() {
    let error = CommandError::CueNotFound(cue_id(5));
    let body = ErrorBody::from(&error);
    assert_eq!(body.code, ErrorCode::CommandRefused);
    assert_eq!(body.message, error.to_string());
}

#[test]
fn engine_actions_map_to_runtime_changes() {
    let engine = CueEngine::new(EngineConfig::new(48_000, 0, 1).unwrap());
    let c = cue_id(10);
    let map = |a: Action| RuntimeChange::from_action(&a, &engine);
    assert_eq!(
        map(Action::StateChanged {
            cue: c,
            state: CueState::PostWaiting,
            at: 5
        }),
        Some(RuntimeChange::CueState {
            cue: c,
            state: WireCueState::PostWaiting,
            reason: None
        })
    );
    assert_eq!(
        map(Action::PlayheadChanged {
            list: list(),
            cue: None
        }),
        Some(RuntimeChange::Playhead {
            list: list(),
            cue: None
        })
    );
    assert_eq!(
        map(Action::PreviewChanged {
            cue: c,
            active: true,
            at: 0
        }),
        Some(RuntimeChange::Preview {
            cue: c,
            active: true
        })
    );
    assert_eq!(
        map(Action::GroupWarningChanged {
            group: c,
            warning: true
        }),
        Some(RuntimeChange::GroupWarning {
            group: c,
            warning: true
        })
    );
    assert_eq!(
        map(Action::ProblemsChanged {
            cue: c,
            problems: vec![
                CueProblem::TargetSelf,
                CueProblem::DeviceUnavailable {
                    patch: patch_id(1),
                    resolution: PatchResolution::OutputCountMismatch {
                        mapped: 2,
                        outputs: 4
                    }
                }
            ]
        }),
        Some(RuntimeChange::Problems {
            cue: c,
            problems: vec![
                WireProblem::TargetSelf,
                WireProblem::DeviceUnavailable {
                    patch: patch_id(1),
                    resolution: WireResolution::OutputCountMismatch {
                        mapped: 2,
                        outputs: 4
                    }
                }
            ]
        })
    );
    // 2400 samples at 48 kHz = 50 ms.
    assert_eq!(
        map(Action::Warning(EngineWarning::LateStart {
            cue: c,
            late_by: 2400
        })),
        Some(RuntimeChange::Warning {
            warning: WireWarning::LateStart {
                cue: c,
                late_ms: 50
            }
        })
    );
    assert_eq!(
        map(Action::GoIgnored {
            list: list(),
            reason: GoIgnoredReason::DoubleGo
        }),
        Some(RuntimeChange::Warning {
            warning: WireWarning::GoIgnored {
                list: list(),
                reason: WireGoIgnored::DoubleGo
            }
        })
    );
    // A sample count near u64::MAX must not overflow the conversion.
    assert_eq!(
        WireWarning::from_engine(
            &EngineWarning::LateStart {
                cue: c,
                late_by: u64::MAX
            },
            NonZeroU32::new(1).unwrap()
        ),
        WireWarning::LateStart {
            cue: c,
            late_ms: u64::MAX
        }
    );
    // Mixer and model actions are not the UI's business.
    assert_eq!(
        map(Action::StartVoice {
            cue: c,
            run: RunId(1),
            at: 0,
            offset: 0
        }),
        None
    );
    assert_eq!(
        map(Action::SetArmed {
            cue: c,
            armed: false
        }),
        None
    );
}

// ---- Nested enums: fixtures, coverage and engine mappings ----

/// The `type` tag of a tagged enum value, or the string of a unit enum value.
fn tag<T: serde::Serialize>(value: &T) -> String {
    let json = serde_json::to_value(value).unwrap();
    json.get("type")
        .unwrap_or(&json)
        .as_str()
        .unwrap()
        .to_owned()
}

/// Every index in `0..count` is in `seen`.
fn assert_covered(what: &str, seen: impl Iterator<Item = usize>, count: usize) {
    let seen: BTreeSet<usize> = seen.collect();
    let want: BTreeSet<usize> = (0..count).collect();
    assert_eq!(seen, want, "{what}: a variant has no sample");
}

// Each `*_idx` is an exhaustive match: a new variant stops compiling here until it is given
// the next index and bumped in its `*_COUNT`, and the coverage tests then want a sample for it.

const ENGINE_WARNING_COUNT: usize = 17;
fn engine_warning_idx(w: &EngineWarning) -> usize {
    match w {
        EngineWarning::UnknownList { .. } => 0,
        EngineWarning::UnknownCue { .. } => 1,
        EngineWarning::NotInList { .. } => 2,
        EngineWarning::NumberNotFound { .. } => 3,
        EngineWarning::CueBroken { .. } => 4,
        EngineWarning::TriggerIgnored { .. } => 5,
        EngineWarning::LoadIgnored { .. } => 6,
        EngineWarning::LateStart { .. } => 7,
        EngineWarning::ChainLimit { .. } => 8,
        EngineWarning::CueRemoved { .. } => 9,
        EngineWarning::NoTarget { .. } => 10,
        EngineWarning::TargetMissing { .. } => 11,
        EngineWarning::TargetWrongKind { .. } => 12,
        EngineWarning::TargetNotRunning { .. } => 13,
        EngineWarning::PreviewIgnored { .. } => 14,
        EngineWarning::PreviewFailed { .. } => 15,
        EngineWarning::DevampIgnored { .. } => 16,
    }
}

const WIRE_WARNING_COUNT: usize = 19;
fn wire_warning_idx(w: &WireWarning) -> usize {
    match w {
        WireWarning::UnknownList { .. } => 0,
        WireWarning::UnknownCue { .. } => 1,
        WireWarning::NotInList { .. } => 2,
        WireWarning::NumberNotFound { .. } => 3,
        WireWarning::CueBroken { .. } => 4,
        WireWarning::TriggerIgnored { .. } => 5,
        WireWarning::LoadIgnored { .. } => 6,
        WireWarning::LateStart { .. } => 7,
        WireWarning::ChainLimit { .. } => 8,
        WireWarning::CueRemoved { .. } => 9,
        WireWarning::NoTarget { .. } => 10,
        WireWarning::TargetMissing { .. } => 11,
        WireWarning::TargetWrongKind { .. } => 12,
        WireWarning::TargetNotRunning { .. } => 13,
        WireWarning::PreviewIgnored { .. } => 14,
        WireWarning::PreviewFailed { .. } => 15,
        WireWarning::DevampIgnored { .. } => 16,
        WireWarning::GoIgnored { .. } => 17,
        WireWarning::Notice { .. } => 18,
    }
}

const ENGINE_PROBLEM_COUNT: usize = 15;
fn engine_problem_idx(p: &CueProblem) -> usize {
    match p {
        CueProblem::NoTarget => 0,
        CueProblem::TargetMissing { .. } => 1,
        CueProblem::TargetSelf => 2,
        CueProblem::TargetWrongKind { .. } => 3,
        CueProblem::NoMedia => 4,
        CueProblem::MediaMissing { .. } => 5,
        CueProblem::MediaUnsupported { .. } => 6,
        CueProblem::PatchMissing { .. } => 7,
        CueProblem::DeviceUnavailable { .. } => 8,
        CueProblem::NetworkPatchMissing { .. } => 9,
        CueProblem::NetworkHostInvalid { .. } => 10,
        CueProblem::NetworkHostUnresolved { .. } => 11,
        CueProblem::NetworkMessageInvalid { .. } => 12,
        CueProblem::StartedBroken => 13,
        CueProblem::NothingToFade => 14,
    }
}

fn wire_problem_idx(p: &WireProblem) -> usize {
    match p {
        WireProblem::NoTarget => 0,
        WireProblem::TargetMissing { .. } => 1,
        WireProblem::TargetSelf => 2,
        WireProblem::TargetWrongKind { .. } => 3,
        WireProblem::NoMedia => 4,
        WireProblem::MediaMissing { .. } => 5,
        WireProblem::MediaUnsupported { .. } => 6,
        WireProblem::PatchMissing { .. } => 7,
        WireProblem::DeviceUnavailable { .. } => 8,
        WireProblem::NetworkPatchMissing { .. } => 9,
        WireProblem::NetworkHostInvalid { .. } => 10,
        WireProblem::NetworkHostUnresolved { .. } => 11,
        WireProblem::NetworkMessageInvalid { .. } => 12,
        WireProblem::StartedBroken => 13,
        WireProblem::NothingToFade => 14,
    }
}

const RESOLUTION_COUNT: usize = 5;
fn engine_resolution_idx(r: &PatchResolution) -> usize {
    match r {
        PatchResolution::Mapped { .. } => 0,
        PatchResolution::Unmapped => 1,
        PatchResolution::DeviceMissing { .. } => 2,
        PatchResolution::ChannelsMissing { .. } => 3,
        PatchResolution::OutputCountMismatch { .. } => 4,
    }
}

fn wire_resolution_idx(r: &WireResolution) -> usize {
    match r {
        WireResolution::Mapped { .. } => 0,
        WireResolution::Unmapped => 1,
        WireResolution::DeviceMissing { .. } => 2,
        WireResolution::ChannelsMissing { .. } => 3,
        WireResolution::OutputCountMismatch { .. } => 4,
    }
}

const STATE_COUNT: usize = 9;
fn engine_state_idx(s: CueState) -> usize {
    match s {
        CueState::Idle => 0,
        CueState::Loaded => 1,
        CueState::PreWaiting => 2,
        CueState::Running => 3,
        CueState::Paused => 4,
        CueState::PostWaiting => 5,
        CueState::Fading => 6,
        CueState::Stopping => 7,
        CueState::Broken => 8,
    }
}

const REASON_COUNT: usize = 3;
fn engine_reason_idx(r: BrokenReason) -> usize {
    match r {
        BrokenReason::VoiceRejected => 0,
        BrokenReason::StreamFailed => 1,
        BrokenReason::DeviceLost => 2,
    }
}

const GO_IGNORED_COUNT: usize = 4;
fn engine_go_idx(r: GoIgnoredReason) -> usize {
    match r {
        GoIgnoredReason::DoubleGo => 0,
        GoIgnoredReason::NoCue => 1,
        GoIgnoredReason::UnknownList => 2,
        GoIgnoredReason::ChainLimit => 3,
    }
}

const PREVIEW_IGNORED_COUNT: usize = 4;
fn engine_preview_idx(r: PreviewIgnoredReason) -> usize {
    match r {
        PreviewIgnoredReason::NotAudio => 0,
        PreviewIgnoredReason::AlreadyPreviewing => 1,
        PreviewIgnoredReason::Disarmed => 2,
        PreviewIgnoredReason::NothingToPreview => 3,
    }
}

const ERROR_CODE_COUNT: usize = 13;
fn error_code_idx(c: ErrorCode) -> usize {
    match c {
        ErrorCode::ProtocolVersionMismatch => 0,
        ErrorCode::BadToken => 1,
        ErrorCode::BadRequest => 2,
        ErrorCode::UnknownMessage => 3,
        ErrorCode::CommandRefused => 4,
        ErrorCode::NotFound => 5,
        ErrorCode::NoWorkspace => 6,
        ErrorCode::UnsavedChanges => 7,
        ErrorCode::Io => 8,
        ErrorCode::BadShowFile => 9,
        ErrorCode::Unsupported => 10,
        ErrorCode::Internal => 11,
        ErrorCode::AlreadyOpen => 12,
    }
}

const LOG_LEVEL_COUNT: usize = 4;
fn log_level_idx(l: LogLevel) -> usize {
    match l {
        LogLevel::Error => 0,
        LogLevel::Warn => 1,
        LogLevel::Info => 2,
        LogLevel::Debug => 3,
    }
}

fn ids() -> (CueId, CueId, CueListId) {
    (cue_id(1), cue_id(2), list_id(3))
}

fn warning_table() -> Vec<(EngineWarning, WireWarning)> {
    let (c, t, l) = ids();
    vec![
        (
            EngineWarning::UnknownList { list: l },
            WireWarning::UnknownList { list: l },
        ),
        (
            EngineWarning::UnknownCue { cue: c },
            WireWarning::UnknownCue { cue: c },
        ),
        (
            EngineWarning::NotInList { list: l, cue: c },
            WireWarning::NotInList { list: l, cue: c },
        ),
        (
            EngineWarning::NumberNotFound {
                list: l,
                number: "7".into(),
            },
            WireWarning::NumberNotFound {
                list: l,
                number: "7".into(),
            },
        ),
        (
            EngineWarning::CueBroken { cue: c },
            WireWarning::CueBroken { cue: c },
        ),
        (
            EngineWarning::TriggerIgnored {
                cue: c,
                state: CueState::Paused,
            },
            WireWarning::TriggerIgnored {
                cue: c,
                state: WireCueState::Paused,
            },
        ),
        (
            EngineWarning::LoadIgnored { cue: c },
            WireWarning::LoadIgnored { cue: c },
        ),
        // 4800 samples at 48 kHz.
        (
            EngineWarning::LateStart {
                cue: c,
                late_by: 4800,
            },
            WireWarning::LateStart {
                cue: c,
                late_ms: 100,
            },
        ),
        (
            EngineWarning::ChainLimit { cue: c, limit: 64 },
            WireWarning::ChainLimit { cue: c, limit: 64 },
        ),
        (
            EngineWarning::CueRemoved { cue: c },
            WireWarning::CueRemoved { cue: c },
        ),
        (
            EngineWarning::NoTarget { cue: c },
            WireWarning::NoTarget { cue: c },
        ),
        (
            EngineWarning::TargetMissing { cue: c, target: t },
            WireWarning::TargetMissing { cue: c, target: t },
        ),
        (
            EngineWarning::TargetWrongKind { cue: c, target: t },
            WireWarning::TargetWrongKind { cue: c, target: t },
        ),
        (
            EngineWarning::TargetNotRunning { cue: c, target: t },
            WireWarning::TargetNotRunning { cue: c, target: t },
        ),
        (
            EngineWarning::PreviewIgnored {
                cue: c,
                reason: PreviewIgnoredReason::AlreadyPreviewing,
            },
            WireWarning::PreviewIgnored {
                cue: c,
                reason: WirePreviewIgnored::AlreadyPreviewing,
            },
        ),
        (
            EngineWarning::PreviewFailed {
                cue: c,
                reason: BrokenReason::StreamFailed,
            },
            WireWarning::PreviewFailed {
                cue: c,
                reason: WireBrokenReason::StreamFailed,
            },
        ),
        (
            EngineWarning::DevampIgnored { cue: c },
            WireWarning::DevampIgnored { cue: c },
        ),
    ]
}

/// One of every wire warning (reasons vary in the dedicated fixtures).
fn all_warnings() -> Vec<WireWarning> {
    let (c, _, l) = ids();
    let mut all: Vec<WireWarning> = warning_table().into_iter().map(|(_, w)| w).collect();
    all.push(WireWarning::GoIgnored {
        list: l,
        reason: WireGoIgnored::DoubleGo,
    });
    all.push(WireWarning::Notice {
        code: "media_damaged".into(),
        message: "intro.wav has a damaged tail".into(),
        cue: Some(c),
    });
    all
}

fn resolution_table() -> Vec<(PatchResolution, WireResolution)> {
    vec![
        (
            PatchResolution::Mapped {
                device: "Scarlett 2i2".into(),
                channels: vec![0, 1],
            },
            WireResolution::Mapped {
                device: "Scarlett 2i2".into(),
                channels: vec![0, 1],
            },
        ),
        (PatchResolution::Unmapped, WireResolution::Unmapped),
        (
            PatchResolution::DeviceMissing {
                device: "Scarlett 2i2".into(),
            },
            WireResolution::DeviceMissing {
                device: "Scarlett 2i2".into(),
            },
        ),
        (
            PatchResolution::ChannelsMissing {
                device: "Scarlett 2i2".into(),
                needed: 4,
                available: 2,
            },
            WireResolution::ChannelsMissing {
                device: "Scarlett 2i2".into(),
                needed: 4,
                available: 2,
            },
        ),
        (
            PatchResolution::OutputCountMismatch {
                mapped: 2,
                outputs: 4,
            },
            WireResolution::OutputCountMismatch {
                mapped: 2,
                outputs: 4,
            },
        ),
    ]
}

fn problem_table() -> Vec<(CueProblem, WireProblem)> {
    let (_, t, _) = ids();
    let media = || MediaRef::new("audio/storm.wav");
    let resolution = PatchResolution::OutputCountMismatch {
        mapped: 2,
        outputs: 4,
    };
    vec![
        (CueProblem::NoTarget, WireProblem::NoTarget),
        (
            CueProblem::TargetMissing { target: t },
            WireProblem::TargetMissing { target: t },
        ),
        (CueProblem::TargetSelf, WireProblem::TargetSelf),
        (
            CueProblem::TargetWrongKind { target: t },
            WireProblem::TargetWrongKind { target: t },
        ),
        (CueProblem::NothingToFade, WireProblem::NothingToFade),
        (CueProblem::NoMedia, WireProblem::NoMedia),
        (
            CueProblem::MediaMissing { media: media() },
            WireProblem::MediaMissing { media: media() },
        ),
        (
            CueProblem::MediaUnsupported {
                media: media(),
                reason: "no decoder".into(),
            },
            WireProblem::MediaUnsupported {
                media: media(),
                reason: "no decoder".into(),
            },
        ),
        (
            CueProblem::PatchMissing { patch: patch_id(5) },
            WireProblem::PatchMissing { patch: patch_id(5) },
        ),
        (
            CueProblem::DeviceUnavailable {
                patch: patch_id(5),
                resolution,
            },
            WireProblem::DeviceUnavailable {
                patch: patch_id(5),
                resolution: WireResolution::OutputCountMismatch {
                    mapped: 2,
                    outputs: 4,
                },
            },
        ),
        (
            CueProblem::NetworkPatchMissing { patch: net_id(6) },
            WireProblem::NetworkPatchMissing { patch: net_id(6) },
        ),
        (
            CueProblem::NetworkHostInvalid {
                patch: net_id(6),
                reason: "the port must be 1 to 65535".into(),
            },
            WireProblem::NetworkHostInvalid {
                patch: net_id(6),
                reason: "the port must be 1 to 65535".into(),
            },
        ),
        (
            CueProblem::NetworkHostUnresolved {
                patch: net_id(6),
                reason: "no such host".into(),
            },
            WireProblem::NetworkHostUnresolved {
                patch: net_id(6),
                reason: "no such host".into(),
            },
        ),
        (
            CueProblem::NetworkMessageInvalid {
                reason: "the OSC address must start with /".into(),
            },
            WireProblem::NetworkMessageInvalid {
                reason: "the OSC address must start with /".into(),
            },
        ),
        (CueProblem::StartedBroken, WireProblem::StartedBroken),
    ]
}

fn state_table() -> Vec<(CueState, WireCueState)> {
    vec![
        (CueState::Idle, WireCueState::Idle),
        (CueState::Loaded, WireCueState::Loaded),
        (CueState::PreWaiting, WireCueState::PreWaiting),
        (CueState::Running, WireCueState::Running),
        (CueState::Paused, WireCueState::Paused),
        (CueState::PostWaiting, WireCueState::PostWaiting),
        (CueState::Fading, WireCueState::Fading),
        (CueState::Stopping, WireCueState::Stopping),
        (CueState::Broken, WireCueState::Broken),
    ]
}

fn reason_table() -> Vec<(BrokenReason, WireBrokenReason)> {
    vec![
        (BrokenReason::VoiceRejected, WireBrokenReason::VoiceRejected),
        (BrokenReason::StreamFailed, WireBrokenReason::StreamFailed),
        (BrokenReason::DeviceLost, WireBrokenReason::DeviceLost),
    ]
}

fn go_table() -> Vec<(GoIgnoredReason, WireGoIgnored)> {
    vec![
        (GoIgnoredReason::DoubleGo, WireGoIgnored::DoubleGo),
        (GoIgnoredReason::NoCue, WireGoIgnored::NoCue),
        (GoIgnoredReason::UnknownList, WireGoIgnored::UnknownList),
        (GoIgnoredReason::ChainLimit, WireGoIgnored::ChainLimit),
    ]
}

fn preview_table() -> Vec<(PreviewIgnoredReason, WirePreviewIgnored)> {
    vec![
        (PreviewIgnoredReason::NotAudio, WirePreviewIgnored::NotAudio),
        (
            PreviewIgnoredReason::AlreadyPreviewing,
            WirePreviewIgnored::AlreadyPreviewing,
        ),
        (PreviewIgnoredReason::Disarmed, WirePreviewIgnored::Disarmed),
        (
            PreviewIgnoredReason::NothingToPreview,
            WirePreviewIgnored::NothingToPreview,
        ),
    ]
}

fn all_error_codes() -> Vec<ErrorCode> {
    vec![
        ErrorCode::ProtocolVersionMismatch,
        ErrorCode::BadToken,
        ErrorCode::BadRequest,
        ErrorCode::UnknownMessage,
        ErrorCode::CommandRefused,
        ErrorCode::NotFound,
        ErrorCode::NoWorkspace,
        ErrorCode::UnsavedChanges,
        ErrorCode::Io,
        ErrorCode::BadShowFile,
        ErrorCode::Unsupported,
        ErrorCode::Internal,
        ErrorCode::AlreadyOpen,
    ]
}

fn all_log_kinds() -> [LogKind; 9] {
    [
        LogKind::Go,
        LogKind::Transport,
        LogKind::Trigger,
        LogKind::CueStart,
        LogKind::CueStop,
        LogKind::CueFail,
        LogKind::Device,
        LogKind::Document,
        LogKind::Engine,
    ]
}

const LOG_KIND_COUNT: usize = 9;
fn log_kind_idx(k: LogKind) -> usize {
    match k {
        LogKind::Go => 0,
        LogKind::Transport => 1,
        LogKind::Trigger => 2,
        LogKind::CueStart => 3,
        LogKind::CueStop => 4,
        LogKind::CueFail => 5,
        LogKind::Device => 6,
        LogKind::Document => 7,
        LogKind::Engine => 8,
    }
}

/// Every shape of the pre-show report: both media and destination statuses, a patch the
/// workspace has and one it lacks, a number-less cue, a system notice.
fn preshow_report_sample() -> PreshowReport {
    // Exhaustive: a new status without a sample stops compiling here.
    for status in [PreshowMediaStatus::Missing, PreshowMediaStatus::Unsupported] {
        match status {
            PreshowMediaStatus::Missing | PreshowMediaStatus::Unsupported => {}
        }
    }
    for status in [
        PreshowTargetStatus::Missing,
        PreshowTargetStatus::NotChecked,
    ] {
        match status {
            PreshowTargetStatus::Missing | PreshowTargetStatus::NotChecked => {}
        }
    }
    PreshowReport {
        ok: false,
        summary: PreshowSummary {
            broken_cues: 2,
            media: 2,
            patches: 2,
            network: 1,
            network_unchecked: 1,
            system: 1,
            total: 3,
        },
        broken_cues: vec![
            PreshowCue {
                cue: cue_id(10),
                list: list_id(1),
                number: Some("3".into()),
                name: "Storm".into(),
                problems: vec![WireProblem::MediaMissing {
                    media: MediaRef::new("/Shows/storm.wav"),
                }],
            },
            PreshowCue {
                cue: cue_id(11),
                list: list_id(1),
                number: None,
                name: String::new(),
                problems: vec![
                    WireProblem::MediaUnsupported {
                        media: MediaRef::new("/Shows/rain.mp3"),
                        reason: "unsupported codec".into(),
                    },
                    WireProblem::PatchMissing { patch: patch_id(9) },
                ],
            },
        ],
        media: vec![
            PreshowMedia {
                path: "/Shows/rain.mp3".into(),
                status: PreshowMediaStatus::Unsupported,
                reason: Some("unsupported codec".into()),
                cues: vec![cue_id(11)],
            },
            PreshowMedia {
                path: "/Shows/storm.wav".into(),
                status: PreshowMediaStatus::Missing,
                reason: None,
                cues: vec![cue_id(10)],
            },
        ],
        patches: vec![
            PreshowPatch {
                patch: patch_id(1),
                name: Some("Main".into()),
                device: Some("Scarlett 2i2".into()),
                mapped: true,
                resolution: Some(WireResolution::DeviceMissing {
                    device: "Scarlett 2i2".into(),
                }),
                cues: vec![cue_id(10)],
            },
            PreshowPatch {
                patch: patch_id(9),
                name: None,
                device: None,
                mapped: false,
                resolution: None,
                cues: vec![cue_id(11)],
            },
        ],
        network: vec![
            PreshowTarget {
                patch: net_id(1),
                name: Some("Lighting".into()),
                host: Some("10.0.0.5".into()),
                port: Some(8000),
                protocol: Some(NetworkProtocol::Udp),
                status: PreshowTargetStatus::NotChecked,
                cues: vec![cue_id(12)],
            },
            PreshowTarget {
                patch: net_id(2),
                name: None,
                host: None,
                port: None,
                protocol: None,
                status: PreshowTargetStatus::Missing,
                cues: vec![cue_id(13)],
            },
        ],
        system: vec![WireWarning::Notice {
            code: "device_lost".into(),
            message: "audio device \"Scarlett 2i2\" is lost".into(),
            cue: None,
        }],
    }
}

fn log_entry_sample(kind: LogKind) -> LogEntry {
    let with_cue = matches!(
        kind,
        LogKind::Go | LogKind::CueStart | LogKind::CueStop | LogKind::CueFail
    );
    LogEntry {
        time_ms: 1_700_000_000_123,
        show_time: (kind != LogKind::Document).then_some(12.5),
        level: if kind == LogKind::CueFail {
            LogLevel::Error
        } else {
            LogLevel::Info
        },
        kind,
        message: "message".into(),
        cue: with_cue.then(|| cue_id(10)),
        source: (kind == LogKind::Trigger).then(|| "osc 192.0.2.7".to_owned()),
    }
}

fn status_sample() -> SystemStatus {
    SystemStatus {
        cpu_percent: Some(7.5),
        engine_load: Some(0.12),
        xruns: 1,
        disk: DiskStatus {
            streams: 3,
            underrun_frames: 0,
            healthy: true,
        },
        device: DeviceStatus {
            name: Some("Scarlett 2i2".into()),
            state: DeviceState::Ok,
            sample_rate: 48_000,
            block_frames: Some(256),
            channels: 2,
            latency_ms: Some(11.5),
        },
        log_dropped: 0,
        log_seq: 42,
        network_output_disabled: false,
    }
}

fn all_log_levels() -> [LogLevel; 4] {
    [
        LogLevel::Error,
        LogLevel::Warn,
        LogLevel::Info,
        LogLevel::Debug,
    ]
}

fn runtime(changes: Vec<RuntimeChange>) -> Sample {
    server(ServerMessage::Runtime { seq: 1, changes })
}

/// One fixture per variant (and per reason) of the nested enums Swift must decode.
fn extra_fixtures(add: &mut dyn FnMut(&str, Sample)) {
    let (c, _, l) = ids();
    let warnings = |w: WireWarning| server(ServerMessage::Warnings { warnings: vec![w] });
    for w in all_warnings() {
        if !matches!(
            w,
            WireWarning::GoIgnored { .. }
                | WireWarning::PreviewIgnored { .. }
                | WireWarning::PreviewFailed { .. }
        ) {
            add(&format!("warnings_{}", tag(&w)), warnings(w));
        }
    }
    for (_, r) in go_table() {
        let name = format!("warnings_go_ignored_{}", tag(&r));
        add(
            &name,
            warnings(WireWarning::GoIgnored { list: l, reason: r }),
        );
    }
    for (_, r) in preview_table() {
        let name = format!("warnings_preview_ignored_{}", tag(&r));
        add(
            &name,
            warnings(WireWarning::PreviewIgnored { cue: c, reason: r }),
        );
    }
    for (_, r) in reason_table() {
        let name = format!("warnings_preview_failed_{}", tag(&r));
        add(
            &name,
            warnings(WireWarning::PreviewFailed { cue: c, reason: r }),
        );
    }
    for (_, p) in problem_table() {
        if !matches!(p, WireProblem::DeviceUnavailable { .. }) {
            let name = format!("runtime_problem_{}", tag(&p));
            add(
                &name,
                runtime(vec![RuntimeChange::Problems {
                    cue: c,
                    problems: vec![p],
                }]),
            );
        }
    }
    for (_, resolution) in resolution_table() {
        let name = format!("runtime_problem_device_unavailable_{}", tag(&resolution));
        let problem = WireProblem::DeviceUnavailable {
            patch: patch_id(5),
            resolution,
        };
        add(
            &name,
            runtime(vec![RuntimeChange::Problems {
                cue: c,
                problems: vec![problem],
            }]),
        );
    }
    for (_, state) in state_table() {
        let name = format!("runtime_state_{}", tag(&state));
        add(
            &name,
            runtime(vec![RuntimeChange::CueState {
                cue: c,
                state,
                reason: None,
            }]),
        );
    }
    for (_, reason) in reason_table() {
        let name = format!("runtime_state_broken_{}", tag(&reason));
        let change = RuntimeChange::CueState {
            cue: c,
            state: WireCueState::Broken,
            reason: Some(reason),
        };
        add(&name, runtime(vec![change]));
    }
    for code in all_error_codes() {
        add(
            &format!("error_{}", tag(&code)),
            server(ServerMessage::refusal(code, "message")),
        );
    }
    for level in all_log_levels() {
        add(
            &format!("log_{}", tag(&level)),
            server(ServerMessage::Log {
                level,
                message: "message".into(),
            }),
        );
    }
}

/// One sample of every command that edits more than the cue tree, named for its
/// fixture.
fn model_commands() -> Vec<(String, Command)> {
    let net = NetworkPatch {
        id: net_id(4),
        name: "Lights".into(),
        host: "10.0.0.7".into(),
        port: 8000,
        protocol: NetworkProtocol::Udp,
    };
    vec![
        (
            "add_patch",
            Command::AddPatch {
                index: 1,
                patch: patch(2, 4),
            },
        ),
        (
            "remove_patch",
            Command::RemovePatch {
                id: patch_id(2),
                even_if_used: false,
            },
        ),
        (
            "remove_patch_even_if_used",
            Command::RemovePatch {
                id: patch_id(2),
                even_if_used: true,
            },
        ),
        (
            "move_patch",
            Command::MovePatch {
                id: patch_id(2),
                index: 0,
            },
        ),
        (
            "add_network_patch",
            Command::AddNetworkPatch {
                index: 0,
                patch: net,
            },
        ),
        (
            "remove_network_patch",
            Command::RemoveNetworkPatch {
                id: net_id(4),
                even_if_used: false,
            },
        ),
        (
            "move_network_patch",
            Command::MoveNetworkPatch {
                id: net_id(4),
                index: 0,
            },
        ),
        (
            "add_cue_list",
            Command::AddCueList {
                index: 1,
                id: list_id(5),
                name: "Understudy".into(),
            },
        ),
        ("remove_cue_list", Command::RemoveCueList { id: list_id(5) }),
        (
            "move_cue_list",
            Command::MoveCueList {
                id: list_id(5),
                index: 0,
            },
        ),
        (
            "rename_cue_list",
            Command::RenameCueList {
                id: list_id(5),
                name: "Act 2".into(),
            },
        ),
        (
            "rename_workspace",
            Command::RenameWorkspace {
                name: "Hamlet".into(),
            },
        ),
        (
            "add_osc_passcode",
            Command::AddOscPasscode {
                index: 1,
                passcode: "4821".into(),
                rights: OscRights {
                    view: true,
                    edit: true,
                    control: false,
                },
            },
        ),
        (
            "remove_osc_passcode",
            Command::RemoveOscPasscode {
                passcode: "4821".into(),
            },
        ),
        (
            "set_osc_rights_passcode",
            Command::SetOscRights {
                target: OscRightsTarget::Passcode {
                    passcode: "4821".into(),
                },
                rights: OscRights::ALL,
            },
        ),
        (
            "set_osc_rights_no_passcode",
            Command::SetOscRights {
                target: OscRightsTarget::NoPasscode,
                rights: OscRights::NONE,
            },
        ),
    ]
    .into_iter()
    .map(|(name, command)| (name.to_owned(), command))
    .chain(property_commands())
    .collect()
}

/// One sample of every tagged variant of the setting and property enums, named by its tag.
fn property_commands() -> Vec<(String, Command)> {
    let settings = [
        WorkspaceSetting::MinGoInterval { value: 2.0 },
        WorkspaceSetting::PanicDuration { value: 1.5 },
        WorkspaceSetting::AutosaveInterval { value: 60.0 },
        WorkspaceSetting::MasterDb { value: -6.0 },
        WorkspaceSetting::Hotkey {
            key: TransportKey::StopAll,
            value: Some(Hotkey {
                key: "f1".into(),
                modifiers: Modifiers::default(),
            }),
        },
        WorkspaceSetting::Hotkeys {
            value: Box::new(TransportHotkeys {
                load_selected: Some(Hotkey {
                    key: "f2".into(),
                    modifiers: Modifiers::default(),
                }),
                ..TransportHotkeys::default()
            }),
        },
        WorkspaceSetting::AutoNumberStart { value: 10.0 },
        WorkspaceSetting::AutoNumberIncrement { value: 0.5 },
    ];
    let properties = [
        PatchProperty::Name {
            value: "FOH".into(),
        },
        PatchProperty::MasterDb { value: -3.0 },
        PatchProperty::OutputLevel {
            output: 1,
            value: -3.0,
        },
        PatchProperty::OutputMute {
            output: 0,
            value: true,
        },
        PatchProperty::OutputDelay {
            output: 1,
            value: 12.5,
        },
        PatchProperty::OutputPolarity {
            output: 0,
            value: true,
        },
        PatchProperty::ChannelCount { value: 6 },
        PatchProperty::Outputs {
            value: vec![patch_output(); 2],
        },
    ];
    let network = [
        NetworkPatchProperty::Name {
            value: "Lights".into(),
        },
        NetworkPatchProperty::Host {
            value: "192.168.1.9".into(),
        },
        NetworkPatchProperty::Port { value: 65535 },
        NetworkPatchProperty::Protocol {
            value: NetworkProtocol::Tcp,
        },
    ];
    let mut out = Vec::new();
    for setting in settings {
        out.push((
            format!("set_workspace_setting_{}", tag(&setting)),
            Command::SetWorkspaceSetting { setting },
        ));
    }
    for property in properties {
        out.push((
            format!("set_patch_property_{}", tag(&property)),
            Command::SetPatchProperty {
                id: patch_id(2),
                property,
            },
        ));
    }
    for property in network {
        out.push((
            format!("set_network_patch_property_{}", tag(&property)),
            Command::SetNetworkPatchProperty {
                id: net_id(4),
                property,
            },
        ));
    }
    out
}

const MODEL_COMMAND_COUNT: usize = 17;
/// Index of a model command; `None` for the cue-tree commands. Exhaustive, so a new command
/// stops compiling here until it has an index and a sample in [`model_commands`].
fn model_command_idx(c: &Command) -> Option<usize> {
    Some(match c {
        Command::AddCue { .. }
        | Command::Paste { .. }
        | Command::PasteClipboard { .. }
        | Command::RemoveCue { .. }
        | Command::MoveCue { .. }
        | Command::ReparentCue { .. }
        | Command::SetCueProperty { .. }
        | Command::BatchSet { .. }
        | Command::SetCueNumbers { .. }
        | Command::Renumber { .. }
        | Command::Transaction { .. } => return None,
        Command::SetWorkspaceSetting { .. } => 0,
        Command::AddPatch { .. } => 1,
        Command::RemovePatch { .. } => 2,
        Command::MovePatch { .. } => 3,
        Command::SetPatchProperty { .. } => 4,
        Command::AddNetworkPatch { .. } => 5,
        Command::RemoveNetworkPatch { .. } => 6,
        Command::MoveNetworkPatch { .. } => 7,
        Command::SetNetworkPatchProperty { .. } => 8,
        Command::AddCueList { .. } => 9,
        Command::RemoveCueList { .. } => 10,
        Command::MoveCueList { .. } => 11,
        Command::RenameCueList { .. } => 12,
        Command::AddOscPasscode { .. } => 13,
        Command::RemoveOscPasscode { .. } => 14,
        Command::SetOscRights { .. } => 15,
        Command::RenameWorkspace { .. } => 16,
    })
}

#[test]
fn every_model_command_and_nested_shape_has_a_fixture_sample() {
    assert_covered(
        "model commands",
        model_commands()
            .iter()
            .filter_map(|(_, c)| model_command_idx(c)),
        MODEL_COMMAND_COUNT,
    );
    let tags: BTreeSet<String> = model_commands().iter().map(|(_, c)| tag(c)).collect();
    assert_eq!(tags.len(), MODEL_COMMAND_COUNT);
}

#[test]
fn the_default_even_if_used_is_left_out_and_read_back() {
    let json = serde_json::to_value(Command::RemovePatch {
        id: patch_id(2),
        even_if_used: false,
    })
    .unwrap();
    assert!(json.get("even_if_used").is_none());
    let back: Command = serde_json::from_value(json).unwrap();
    assert_eq!(
        back,
        Command::RemovePatch {
            id: patch_id(2),
            even_if_used: false
        }
    );
}

#[test]
fn a_coalesce_key_round_trips_and_is_optional() {
    let msg = ClientMessage::Command {
        id: rid(9),
        command: Box::new(Command::RenameCueList {
            id: list_id(5),
            name: "x".into(),
        }),
        coalesce: Some("rename:1".into()),
    };
    let text = encode_client(&msg).unwrap();
    assert_eq!(decode_client(&text).unwrap(), msg);
    let no_key = text.replace(r#","coalesce":"rename:1""#, "");
    assert!(!no_key.contains("coalesce"));
    match decode_client(&no_key).unwrap() {
        ClientMessage::Command { coalesce, .. } => assert_eq!(coalesce, None),
        other => panic!("{other:?}"),
    }
}

#[test]
fn every_nested_enum_variant_has_a_fixture_sample() {
    assert_covered(
        "WireWarning",
        all_warnings().iter().map(wire_warning_idx),
        WIRE_WARNING_COUNT,
    );
    assert_covered(
        "WireProblem",
        problem_table().iter().map(|(_, w)| wire_problem_idx(w)),
        ENGINE_PROBLEM_COUNT,
    );
    assert_covered(
        "WireResolution",
        resolution_table()
            .iter()
            .map(|(_, w)| wire_resolution_idx(w)),
        RESOLUTION_COUNT,
    );
    assert_covered(
        "ErrorCode",
        all_error_codes().into_iter().map(error_code_idx),
        ERROR_CODE_COUNT,
    );
    assert_covered(
        "LogKind",
        all_log_kinds().into_iter().map(log_kind_idx),
        LOG_KIND_COUNT,
    );
    assert_covered(
        "LogLevel",
        all_log_levels().into_iter().map(log_level_idx),
        LOG_LEVEL_COUNT,
    );
    // And each sample is in the committed fixture set.
    let names: BTreeSet<String> = fixtures().into_iter().map(|(n, _)| n).collect();
    for w in all_warnings() {
        let t = tag(&w);
        assert!(
            names
                .iter()
                .any(|n| n.starts_with(&format!("server/warnings_{t}"))),
            "{t}"
        );
    }
    for (_, p) in problem_table() {
        let t = tag(&p);
        assert!(
            names
                .iter()
                .any(|n| n.starts_with(&format!("server/runtime_problem_{t}"))),
            "{t}"
        );
    }
    for (_, r) in resolution_table() {
        assert!(names.contains(&format!(
            "server/runtime_problem_device_unavailable_{}",
            tag(&r)
        )));
    }
    // 9 states + 3 broken reasons, 11 error codes, 4 log levels.
    for (_, s) in state_table() {
        assert!(names.contains(&format!("server/runtime_state_{}", tag(&s))));
    }
    for code in all_error_codes() {
        assert!(names.contains(&format!("server/error_{}", tag(&code))));
    }
}

#[test]
fn every_engine_warning_maps_to_its_wire_form() {
    let rate = NonZeroU32::new(48_000).unwrap();
    let table = warning_table();
    assert_covered(
        "EngineWarning",
        table.iter().map(|(e, _)| engine_warning_idx(e)),
        ENGINE_WARNING_COUNT,
    );
    for (engine, wire) in &table {
        assert_eq!(&WireWarning::from_engine(engine, rate), wire, "{engine:?}");
        // The wire form keeps the engine's variant name as its tag (snake case).
        assert_eq!(wire_warning_idx(wire), engine_warning_idx(engine));
    }
}

#[test]
fn every_problem_resolution_state_and_reason_maps_to_its_wire_form() {
    let problems = problem_table();
    assert_covered(
        "CueProblem",
        problems.iter().map(|(e, _)| engine_problem_idx(e)),
        ENGINE_PROBLEM_COUNT,
    );
    for (engine, wire) in &problems {
        assert_eq!(&WireProblem::from(engine), wire, "{engine:?}");
        assert_eq!(wire_problem_idx(wire), engine_problem_idx(engine));
    }
    let resolutions = resolution_table();
    assert_covered(
        "PatchResolution",
        resolutions.iter().map(|(e, _)| engine_resolution_idx(e)),
        RESOLUTION_COUNT,
    );
    for (engine, wire) in &resolutions {
        assert_eq!(&WireResolution::from(engine), wire, "{engine:?}");
    }
    assert_covered(
        "CueState",
        state_table().iter().map(|(e, _)| engine_state_idx(*e)),
        STATE_COUNT,
    );
    for (engine, wire) in state_table() {
        assert_eq!(WireCueState::from(engine), wire);
    }
    assert_covered(
        "BrokenReason",
        reason_table().iter().map(|(e, _)| engine_reason_idx(*e)),
        REASON_COUNT,
    );
    for (engine, wire) in reason_table() {
        assert_eq!(WireBrokenReason::from(engine), wire);
    }
    assert_covered(
        "GoIgnoredReason",
        go_table().iter().map(|(e, _)| engine_go_idx(*e)),
        GO_IGNORED_COUNT,
    );
    for (engine, wire) in go_table() {
        assert_eq!(WireGoIgnored::from(engine), wire);
    }
    assert_covered(
        "PreviewIgnoredReason",
        preview_table().iter().map(|(e, _)| engine_preview_idx(*e)),
        PREVIEW_IGNORED_COUNT,
    );
    for (engine, wire) in preview_table() {
        assert_eq!(WirePreviewIgnored::from(engine), wire);
    }
}

#[test]
fn a_broken_state_change_carries_the_engines_reason() {
    let c = cue_id(1);
    let ws = workspace(vec![cue(1, audio())]);
    let mut engine = CueEngine::new(EngineConfig::new(1000, 0, 1).unwrap());
    let mut last = None;
    handle_as_binary(&mut engine, &ws, &mut last, Input::Start { cue: c }, 0);
    let run = engine.run_of(c).unwrap();
    let actions = handle_as_binary(
        &mut engine,
        &ws,
        &mut last,
        Input::ActionFailed {
            cue: c,
            run,
            reason: BrokenReason::DeviceLost,
        },
        10,
    );
    let changes: Vec<_> = actions
        .iter()
        .filter_map(|a| RuntimeChange::from_action(a, &engine))
        .collect();
    assert!(
        changes.contains(&RuntimeChange::CueState {
            cue: c,
            state: WireCueState::Broken,
            reason: Some(WireBrokenReason::DeviceLost),
        }),
        "{changes:?}"
    );
    // A cue broken only by static problems has no run-time reason.
    let ws = workspace(vec![cue(1, fade(99))]);
    let mut engine = CueEngine::new(EngineConfig::new(1000, 0, 1).unwrap());
    let mut last = None;
    let actions = handle_as_binary(&mut engine, &ws, &mut last, Input::Tick, 0);
    let broken: Vec<_> = actions
        .iter()
        .filter_map(|a| RuntimeChange::from_action(a, &engine))
        .filter(|c| {
            matches!(
                c,
                RuntimeChange::CueState {
                    state: WireCueState::Broken,
                    ..
                }
            )
        })
        .collect();
    assert_eq!(
        broken,
        vec![RuntimeChange::CueState {
            cue: c,
            state: WireCueState::Broken,
            reason: None
        }]
    );
    // A reason is only ever attached to a broken state.
    let running = Action::StateChanged {
        cue: c,
        state: CueState::Running,
        at: 0,
    };
    assert_eq!(
        RuntimeChange::from_action(&running, &engine),
        Some(RuntimeChange::CueState {
            cue: c,
            state: WireCueState::Running,
            reason: None
        })
    );
}

// ---- Property: decode(encode(x)) == x for generated client messages ----

fn arb_cue() -> impl Strategy<Value = CueId> {
    any::<u128>().prop_map(cue_id)
}

fn arb_list() -> impl Strategy<Value = CueListId> {
    any::<u128>().prop_map(list_id)
}

fn arb_time() -> impl Strategy<Value = f64> {
    prop_oneof![
        Just(0.0),
        0.0f64..1e7,
        any::<f64>()
            .prop_map(f64::abs)
            .prop_filter("finite", |v| v.is_finite())
    ]
}

fn arb_text() -> impl Strategy<Value = String> {
    prop_oneof!["[a-z0-9 ]{0,12}", "\\PC{0,12}"]
}

/// A path `open`, `save_as` and `relink` accept.
fn arb_abs_path() -> impl Strategy<Value = String> {
    "/\\PC{0,40}"
}

fn arb_action() -> impl Strategy<Value = TransportAction> {
    prop_oneof![
        arb_list().prop_map(|list| TransportAction::Go { list }),
        Just(TransportAction::Panic),
        Just(TransportAction::PauseAll),
        Just(TransportAction::ResumeAll),
        (arb_list(), proptest::option::of(arb_cue()))
            .prop_map(|(list, cue)| TransportAction::SetPlayhead { list, cue }),
        (arb_list(), arb_text())
            .prop_map(|(list, number)| TransportAction::SetPlayheadByNumber { list, number }),
        arb_list().prop_map(|list| TransportAction::PlayheadNext { list }),
        arb_list().prop_map(|list| TransportAction::PlayheadPrevious { list }),
        arb_cue().prop_map(|cue| TransportAction::Start { cue }),
        (arb_cue(), proptest::option::of(arb_time())).prop_map(|(cue, d)| TransportAction::Stop {
            cue,
            fade_out: d.map(|duration| FadeSpec {
                duration,
                curve: FadeCurve::Linear
            })
        }),
        arb_cue().prop_map(|cue| TransportAction::HardStop { cue }),
        arb_cue().prop_map(|cue| TransportAction::PanicCue { cue }),
        (arb_list(), arb_cue()).prop_map(|(list, cue)| TransportAction::GoFrom { list, cue }),
        Just(TransportAction::StopAll),
        Just(TransportAction::HardStopAll),
        arb_cue().prop_map(|cue| TransportAction::Pause { cue }),
        arb_cue().prop_map(|cue| TransportAction::Resume { cue }),
        (arb_cue(), arb_time()).prop_map(|(cue, at)| TransportAction::Load { cue, at }),
        arb_cue().prop_map(|cue| TransportAction::Reset { cue }),
        arb_cue().prop_map(|cue| TransportAction::TriggerReleased { cue }),
        arb_cue().prop_map(|cue| TransportAction::Preview { cue }),
        proptest::option::of(arb_cue()).prop_map(|cue| TransportAction::StopPreview { cue }),
        // Distinct cues: a batch naming one twice is refused by design.
        (
            proptest::collection::btree_set(any::<u128>(), 1..8),
            arb_time()
        )
            .prop_map(|(cues, at)| TransportAction::Batch {
                actions: cues
                    .into_iter()
                    .map(|n| match n % 3 {
                        0 => TransportAction::Start { cue: cue_id(n) },
                        1 => TransportAction::Load { cue: cue_id(n), at },
                        _ => TransportAction::HardStop { cue: cue_id(n) },
                    })
                    .collect(),
            }),
    ]
}

fn arb_request() -> impl Strategy<Value = Request> {
    prop_oneof![
        Just(Request::New {}),
        (
            arb_abs_path(),
            proptest::option::of(prop_oneof![
                Just(RecoveryChoice::Restore),
                Just(RecoveryChoice::Ignore)
            ])
        )
            .prop_map(|(path, recovery)| Request::Open { path, recovery }),
        (
            any::<u128>(),
            proptest::option::of("[0-9a-f]{16}"),
            proptest::option::of(any::<i64>()),
        )
            .prop_map(|(id, path_hash, modified_ms)| {
                Request::OpenRecovery {
                    workspace_id: workspace_id(id),
                    path_hash,
                    modified_ms,
                }
            }),
        Just(Request::ScanRecovery),
        Just(Request::Save),
        arb_abs_path().prop_map(|path| Request::SaveAs { path }),
        arb_abs_path().prop_map(|folder| Request::Relink { folder }),
        Just(Request::ListDevices),
        any::<bool>().prop_map(|network_disabled| Request::SetOutputOverrides { network_disabled }),
        Just(Request::PreshowCheck),
        any::<bool>().prop_map(|on| Request::SetShowMode { on }),
        (
            any::<u128>(),
            proptest::option::of(("[ -~]{1,20}", proptest::collection::vec(0u32..64, 0..8))),
        )
            .prop_map(|(patch, mapping)| Request::SetPatchMapping {
                patch: PatchId(uuid::Uuid::from_u128(patch)),
                mapping: mapping.map(|(device, channels)| PatchMapping { device, channels }),
            }),
        proptest::option::of(0u32..2000).prop_map(|max| Request::LogTail { max }),
        Just(Request::Snapshot),
    ]
}

fn arb_command() -> impl Strategy<Value = Command> {
    prop_oneof![
        (arb_text(), any::<bool>(), any::<bool>(), any::<bool>()).prop_map(
            |(passcode, view, edit, control)| Command::AddOscPasscode {
                index: 0,
                passcode,
                rights: OscRights {
                    view,
                    edit,
                    control
                },
            }
        ),
        (arb_text(), any::<bool>()).prop_map(|(passcode, none)| Command::SetOscRights {
            target: if none {
                OscRightsTarget::NoPasscode
            } else {
                OscRightsTarget::Passcode { passcode }
            },
            rights: OscRights::ALL,
        }),
        arb_text().prop_map(|passcode| Command::RemoveOscPasscode { passcode }),
        (arb_text(), 0usize..9).prop_map(|(name, index)| Command::AddCueList {
            index,
            id: list_id(7),
            name
        }),
        (0usize..70, arb_time()).prop_map(|(output, value)| Command::SetPatchProperty {
            id: patch_id(1),
            property: PatchProperty::OutputLevel { output, value }
        }),
        arb_cue().prop_map(|id| Command::RemoveCue { id }),
        (arb_cue(), arb_text()).prop_map(|(id, value)| Command::SetCueProperty {
            id,
            property: CueProperty::Name { value }
        }),
        (arb_cue(), 0usize..100).prop_map(|(id, index)| Command::MoveCue { id, index }),
        (
            proptest::collection::vec(arb_cue(), 0..4),
            arb_time(),
            arb_time()
        )
            .prop_map(|(ids, start, increment)| Command::Renumber {
                ids,
                start,
                increment
            }),
    ]
}

fn arb_client() -> impl Strategy<Value = ClientMessage> {
    prop_oneof![
        (
            any::<u32>(),
            arb_text(),
            arb_text(),
            proptest::option::of(any::<u64>()),
            proptest::option::of(any::<u32>())
        )
            .prop_map(|(protocol_version, token, client_name, workspace, pid)| {
                ClientMessage::Hello {
                    protocol_version,
                    token,
                    client_name,
                    workspace: workspace.map(DocId),
                    pid,
                }
            }),
        (
            any::<u64>(),
            arb_command(),
            proptest::option::of(arb_text())
        )
            .prop_map(|(id, command, coalesce)| ClientMessage::Command {
                id: rid(id),
                command: Box::new(command),
                coalesce
            }),
        (any::<u64>(), arb_action()).prop_map(|(id, action)| transport(id, action)),
        any::<u64>().prop_map(|id| ClientMessage::Undo { id: rid(id) }),
        any::<u64>().prop_map(|id| ClientMessage::Redo { id: rid(id) }),
        (any::<u64>(), arb_request()).prop_map(|(id, r)| request(id, r)),
        (
            any::<u64>(),
            proptest::option::of(any::<u64>()),
            any::<bool>()
        )
            .prop_map(|(id, doc, discard_changes)| ClientMessage::Close {
                id: rid(id),
                doc: doc.map(DocId),
                discard_changes
            }),
        any::<u64>().prop_map(|id| ClientMessage::Panic { id: rid(id) }),
        (any::<u64>(), "[a-z0-9]{1,8}", any::<bool>(), any::<bool>()).prop_map(
            |(id, key, shift, up)| ClientMessage::Hotkey {
                id: rid(id),
                key,
                modifiers: Modifiers {
                    shift,
                    ..Modifiers::default()
                },
                phase: if up {
                    HotkeyPhase::Up
                } else {
                    HotkeyPhase::Down
                },
            }
        ),
        any::<bool>().prop_map(|shutdown| ClientMessage::Goodbye { shutdown }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn client_messages_round_trip(message in arb_client()) {
        let wire = encode_client(&message).unwrap();
        prop_assert_eq!(decode_client(&wire).unwrap(), message.clone());
        let pretty = encode_client_pretty(&message).unwrap();
        prop_assert_eq!(decode_client(&pretty).unwrap(), message);
    }

    /// Any text at all is an error or a message, never a panic.
    #[test]
    fn decoding_arbitrary_text_is_total(text in "\\PC{0,200}") {
        let _ = decode_client(&text);
        let _ = decode_server(&text);
    }

    /// Mutating a valid frame (cut off, byte flipped) never panics.
    #[test]
    fn decoding_damaged_frames_is_total(message in arb_client(), cut in 0usize..400, flip in 0usize..400) {
        let wire = encode_client(&message).unwrap();
        let mut bytes = wire.clone().into_bytes();
        if !bytes.is_empty() {
            let i = flip % bytes.len();
            bytes[i] = bytes[i].wrapping_add(1);
        }
        let _ = decode_client(&String::from_utf8_lossy(&bytes));
        let mut end = cut.min(wire.len());
        while !wire.is_char_boundary(end) {
            end -= 1;
        }
        let _ = decode_client(&wire[..end]);
    }
}

#[test]
fn a_snapshot_without_media_durations_reads_as_none_known() {
    // The field is optional, so a snapshot with none known leaves it out and decodes.
    let mut snap = snapshot();
    snap.media_durations.clear();
    let text = encode_server(&ServerMessage::Snapshot {
        snapshot: Box::new(snap),
    })
    .unwrap();
    assert!(!text.contains("media_durations"));
    let ServerMessage::Snapshot { snapshot } = decode_server(&text).unwrap() else {
        panic!("not a snapshot");
    };
    assert!(snapshot.media_durations.is_empty());
}

/// A key map that leaves a key out, or misspells one, used to be read as "clear that key".
#[test]
fn a_partial_or_misspelt_key_map_is_bad_request() {
    let frame = |value: &str| {
        decode_client(&format!(
            r#"{{"type":"command","id":1,"command":{{"type":"set_workspace_setting","setting":{{"type":"hotkeys","value":{value}}}}}}}"#
        ))
    };
    let all = |extra: &str| {
        let keys = [
            "go",
            "stop_all",
            "pause_all",
            "resume_all",
            "playhead_next",
            "playhead_previous",
            "load_selected",
            "pause_resume_selected",
            "start_selected",
            "stop_selected",
            "reset_selected",
        ];
        let body: Vec<String> = keys.iter().map(|k| format!(r#""{k}":null"#)).collect();
        format!("{{{}{extra}}}", body.join(","))
    };
    assert!(frame(&all("")).is_ok());
    assert!(frame(r#"{"go":null,"pause_all":null}"#).is_err(), "missing");
    assert!(frame(&all(r#","load_selcted":null"#)).is_err(), "unknown");
    assert!(frame("{}").is_err());
}
