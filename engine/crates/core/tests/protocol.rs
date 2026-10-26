// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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
    }
}

fn all_actions() -> Vec<(&'static str, TransportAction)> {
    let c = cue_id(10);
    vec![
        ("go", TransportAction::Go { list: list() }),
        ("stop_all", TransportAction::StopAll),
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
    ]
}

fn all_requests() -> Vec<(&'static str, Request)> {
    vec![
        ("new", Request::New),
        (
            "open",
            Request::Open {
                path: "/Users/sam/Shows/Hamlet.openlab".into(),
            },
        ),
        ("save", Request::Save),
        (
            "save_as",
            Request::SaveAs {
                path: "/Users/sam/Shows/Hamlet 2.openlab".into(),
            },
        ),
        ("revert", Request::Revert),
        (
            "relink",
            Request::Relink {
                folder: "/Volumes/Audio/Hamlet".into(),
            },
        ),
        ("list_devices", Request::ListDevices),
        ("snapshot", Request::Snapshot),
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
        }),
    );
    add(
        "hello_version_mismatch",
        client(ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION + 1,
            token: "3f9a1c0e5b7d42a8".into(),
            client_name: "openLab for macOS from the future".into(),
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
        }),
    );
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
    add("goodbye", client(ClientMessage::Goodbye { shutdown: true }));

    add(
        "welcome",
        server(ServerMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            snapshot: Box::new(snapshot()),
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
                }),
            }),
        }),
    );
    add(
        "error",
        server(ServerMessage::refusal(
            ErrorCode::ProtocolVersionMismatch,
            "this engine speaks protocol 1; the client sent 2",
        )),
    );
    add(
        "error_bad_token",
        server(ServerMessage::refusal(ErrorCode::BadToken, "wrong token")),
    );
    out
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
        match &sample {
            Sample::Client(m) => client_types.insert(m.type_name()),
            Sample::Server(m) => server_types.insert(m.type_name()),
        };
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
            | TransportAction::StopAll
            | TransportAction::Panic
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
            | TransportAction::StopPreview { .. } => {}
        }
    }
    assert_eq!(all_actions().len(), 19);
    assert_eq!(all_requests().len(), 8);
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

#[test]
fn an_oversized_frame_is_refused_before_parsing() {
    let big = " ".repeat(MAX_FRAME_BYTES + 1);
    assert_eq!(
        decode_client(&big),
        Err(ProtocolError::TooLarge {
            len: MAX_FRAME_BYTES + 1
        })
    );
    assert!(matches!(
        decode_server(&big),
        Err(ProtocolError::TooLarge { .. })
    ));
}

#[test]
fn non_finite_numbers_are_not_encoded() {
    let progress = |elapsed: f64, remaining: Option<f64>| ServerMessage::Progress {
        items: vec![ProgressItem {
            cue: cue_id(1),
            elapsed,
            remaining,
        }],
    };
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(matches!(
            encode_server(&progress(bad, None)),
            Err(ProtocolError::NonFinite { .. })
        ));
        assert!(matches!(
            encode_server(&progress(0.0, Some(bad))),
            Err(ProtocolError::NonFinite { .. })
        ));
    }
    assert!(encode_server(&progress(1.0, Some(2.0))).is_ok());
    let meters = |peak: f32| ServerMessage::Meters {
        outputs: vec![],
        cues: vec![CueMeter {
            cue: cue_id(1),
            peaks: vec![peak],
        }],
    };
    assert!(encode_server(&meters(f32::NAN)).is_err());
    assert!(encode_server(&meters(f32::INFINITY)).is_err());
    assert!(encode_server(&meters(-0.5)).is_err());
    assert!(
        encode_server(&meters(1.5)).is_ok(),
        "above full scale is a clip, not an error"
    );
    // The client side too.
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
    let rate = NonZeroU32::new(48_000).unwrap();
    let c = cue_id(10);
    let map = |a: Action| RuntimeChange::from_action(&a, rate);
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

fn arb_path() -> impl Strategy<Value = String> {
    "\\PC{1,40}"
}

fn arb_action() -> impl Strategy<Value = TransportAction> {
    prop_oneof![
        arb_list().prop_map(|list| TransportAction::Go { list }),
        Just(TransportAction::StopAll),
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
        arb_cue().prop_map(|cue| TransportAction::Pause { cue }),
        arb_cue().prop_map(|cue| TransportAction::Resume { cue }),
        (arb_cue(), arb_time()).prop_map(|(cue, at)| TransportAction::Load { cue, at }),
        arb_cue().prop_map(|cue| TransportAction::Reset { cue }),
        arb_cue().prop_map(|cue| TransportAction::TriggerReleased { cue }),
        arb_cue().prop_map(|cue| TransportAction::Preview { cue }),
        proptest::option::of(arb_cue()).prop_map(|cue| TransportAction::StopPreview { cue }),
    ]
}

fn arb_request() -> impl Strategy<Value = Request> {
    prop_oneof![
        Just(Request::New),
        arb_path().prop_map(|path| Request::Open { path }),
        Just(Request::Save),
        arb_path().prop_map(|path| Request::SaveAs { path }),
        Just(Request::Revert),
        arb_path().prop_map(|folder| Request::Relink { folder }),
        Just(Request::ListDevices),
        Just(Request::Snapshot),
    ]
}

fn arb_command() -> impl Strategy<Value = Command> {
    prop_oneof![
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
        (any::<u32>(), arb_text(), arb_text()).prop_map(
            |(protocol_version, token, client_name)| {
                ClientMessage::Hello {
                    protocol_version,
                    token,
                    client_name,
                }
            }
        ),
        (any::<u64>(), arb_command()).prop_map(|(id, command)| ClientMessage::Command {
            id: rid(id),
            command: Box::new(command)
        }),
        (any::<u64>(), arb_action()).prop_map(|(id, action)| transport(id, action)),
        any::<u64>().prop_map(|id| ClientMessage::Undo { id: rid(id) }),
        any::<u64>().prop_map(|id| ClientMessage::Redo { id: rid(id) }),
        (any::<u64>(), arb_request()).prop_map(|(id, r)| request(id, r)),
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
