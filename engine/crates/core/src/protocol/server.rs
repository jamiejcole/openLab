// SPDX-License-Identifier: GPL-3.0-or-later

//! Engine -> client messages.

use serde::{Deserialize, Serialize};

use super::wire::{RuntimeChange, WireBrokenReason, WireCueState, WireProblem, WireWarning};
use super::{
    MAX_SERVER_FRAME_BYTES, PROTOCOL_VERSION, ProtocolError, RequestId, decode, encode,
    encode_pretty,
};
use crate::command::CommandError;
use crate::cue::Cue;
use crate::model::{
    CueId, CueListId, NetworkPatch, NetworkPatchId, OutputPatch, PatchId, Workspace,
    WorkspaceSettings,
};

/// Every `type` the engine may send.
pub const SERVER_TYPES: &[&str] = &[
    "welcome",
    "snapshot",
    "model_changed",
    "document",
    "runtime",
    "progress",
    "meters",
    "log",
    "warnings",
    "result",
    "error",
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Answer to an accepted `hello`: everything the client needs to draw the show.
    Welcome {
        protocol_version: u32,
        snapshot: Box<Snapshot>,
    },
    /// The whole state again: after `new`, `open`, `revert` and the `snapshot` request. A client
    /// replaces everything it holds.
    Snapshot {
        snapshot: Box<Snapshot>,
    },
    /// Edits since the client's last `revision`. Apply `deletes` first, then `upserts` in order.
    ModelChanged {
        revision: u64,
        upserts: Vec<Upsert>,
        deletes: Vec<Delete>,
    },
    /// File name, unsaved edits and undo/redo availability changed (save, edit, undo).
    Document {
        document: DocumentState,
    },
    /// Cue state changes, event-driven. `seq` rises by one per message, so a client sees a gap.
    Runtime {
        seq: u64,
        changes: Vec<RuntimeChange>,
    },
    /// Elapsed and remaining time of running cues, at most 30 Hz, only while something runs.
    Progress {
        items: Vec<ProgressItem>,
    },
    /// Peak levels, at most 30 Hz, only while something plays.
    Meters {
        outputs: Vec<OutputMeter>,
        cues: Vec<CueMeter>,
    },
    Log {
        level: LogLevel,
        message: String,
    },
    /// The workspace-level notices now in force (load repairs, damaged media, ...). Replaces the
    /// previous list; empty clears it. Show-time warnings come in `runtime`.
    Warnings {
        warnings: Vec<WireWarning>,
    },
    /// Answer to a `command`, `transport`, `undo`, `redo` or `request`.
    Result {
        id: RequestId,
        #[serde(flatten)]
        outcome: Outcome,
    },
    /// A refusal or failure not tied to a request: version mismatch or bad token (then the engine
    /// closes the connection), or a frame it could not read.
    Error {
        code: ErrorCode,
        message: String,
        /// The engine's own version, so the client can say what it needs.
        protocol_version: u32,
    },
}

/// Outcome of a request: `{"ok": {...}}` or `{"error": {...}}` next to the id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok(OkBody),
    Error(ErrorBody),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OkBody {
    /// The model revision after the request, for those that edit (`command`, `undo`, `redo`,
    /// `relink`). The matching `model_changed` is sent before this result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<ResultData>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResultData {
    Devices {
        devices: Vec<DeviceEntry>,
    },
    Relink {
        relinked: u64,
        ambiguous: Vec<AmbiguousMatch>,
        unmatched: Vec<CueId>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceEntry {
    pub name: String,
    pub output_channels: u32,
}

/// A missing file that matched several; the operator picks (a `set_cue_property` command).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AmbiguousMatch {
    pub cue: CueId,
    pub candidates: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    /// English, for logs and a fallback; the UI localises from `code`.
    pub message: String,
}

impl ErrorBody {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl From<&CommandError> for ErrorBody {
    fn from(error: &CommandError) -> Self {
        Self::new(ErrorCode::CommandRefused, error.to_string())
    }
}

/// Stable machine-readable reasons.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// `hello` named a version the engine does not speak.
    ProtocolVersionMismatch,
    /// `hello` carried the wrong token, or a message came before `hello`.
    BadToken,
    /// The frame could not be read (bad JSON, wrong types, missing fields).
    BadRequest,
    UnknownMessage,
    /// A command was refused; the workspace is unchanged.
    CommandRefused,
    /// A transport or request named a cue, list or file that does not exist.
    NotFound,
    /// A request needs a workspace and none is open, or a save has no path yet.
    NoWorkspace,
    /// A file could not be read or written.
    Io,
    /// A show file the engine could not load (damaged, newer format, ...).
    BadShowFile,
    Unsupported,
    Internal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
}

/// Save state and undo availability of the open workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentState {
    /// Where it is saved; `null` = never saved.
    pub path: Option<String>,
    /// Edits since the last save.
    pub dirty: bool,
    pub can_undo: bool,
    pub can_redo: bool,
}

/// An entity that was added, changed or moved. A group's upsert carries its whole subtree and
/// replaces the group's; do not also upsert its children.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Upsert {
    /// Place the cue at `index` among the cues of `parent` (`null`: the list's top level).
    Cue {
        list: CueListId,
        parent: Option<CueId>,
        index: u64,
        cue: Box<Cue>,
    },
    /// A list's name and place among the lists; its cues arrive as `cue` upserts.
    CueList {
        id: CueListId,
        name: String,
        index: u64,
    },
    Patch {
        index: u64,
        patch: OutputPatch,
    },
    NetworkPatch {
        index: u64,
        patch: NetworkPatch,
    },
    /// The workspace's name and settings.
    Settings {
        name: String,
        settings: Box<WorkspaceSettings>,
    },
}

/// An entity that is gone (with its children, for a cue).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Delete {
    Cue { id: CueId },
    CueList { id: CueListId },
    Patch { id: PatchId },
    NetworkPatch { id: NetworkPatchId },
}

/// Everything about the open workspace and the running show.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The model revision this snapshot is at; the next `model_changed` is above it.
    pub revision: u64,
    /// The `seq` of the last `runtime` message folded into this snapshot; the next is above it.
    pub runtime_seq: u64,
    pub document: DocumentState,
    pub workspace: Workspace,
    /// Cues that are not idle.
    pub cue_states: Vec<CueStateEntry>,
    /// Cues with static problems.
    pub problems: Vec<CueProblems>,
    /// Groups with a broken descendant.
    pub group_warnings: Vec<CueId>,
    pub playheads: Vec<PlayheadEntry>,
    pub previewing: Vec<CueId>,
    /// As in `warnings`.
    pub warnings: Vec<WireWarning>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CueStateEntry {
    pub cue: CueId,
    pub state: WireCueState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<WireBrokenReason>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CueProblems {
    pub cue: CueId,
    pub problems: Vec<WireProblem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayheadEntry {
    pub list: CueListId,
    pub cue: Option<CueId>,
}

/// Seconds. `remaining` is `null` when there is no end (an infinite loop, a cue waiting for a
/// feedback that has not come).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProgressItem {
    pub cue: CueId,
    pub elapsed: f64,
    pub remaining: Option<f64>,
}

/// Peak since the previous meters message: linear amplitude per channel, 0.0 silent, 1.0 full
/// scale (it can go above 1.0: that is a clip). Linear rather than dBFS so silence is a plain
/// 0 and the UI chooses its own scale.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutputMeter {
    pub patch: PatchId,
    pub peaks: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CueMeter {
    pub cue: CueId,
    pub peaks: Vec<f32>,
}

impl ServerMessage {
    /// A refusal before `welcome`: sent, then the connection is closed.
    pub fn refusal(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Error {
            code,
            message: message.into(),
            protocol_version: PROTOCOL_VERSION,
        }
    }

    /// Makes a progress or meters message encodable: a time or peak that is negative, NaN or
    /// infinite becomes 0 (serde_json would write a non-finite number as `null`). A loop that
    /// overruns its end by a hair must not cost the UI a whole tick of progress.
    pub fn sanitise(&mut self) {
        let clean = |v: f64| if v.is_finite() && v > 0.0 { v } else { 0.0 };
        match self {
            Self::Progress { items } => {
                for item in items {
                    item.elapsed = clean(item.elapsed);
                    item.remaining = item.remaining.map(clean);
                }
            }
            Self::Meters { outputs, cues } => {
                let peaks = outputs
                    .iter_mut()
                    .flat_map(|m| m.peaks.iter_mut())
                    .chain(cues.iter_mut().flat_map(|m| m.peaks.iter_mut()));
                for peak in peaks {
                    *peak = clean(f64::from(*peak)) as f32;
                }
            }
            _ => {}
        }
    }

    /// The `type` string of this message.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Welcome { .. } => "welcome",
            Self::Snapshot { .. } => "snapshot",
            Self::ModelChanged { .. } => "model_changed",
            Self::Document { .. } => "document",
            Self::Runtime { .. } => "runtime",
            Self::Progress { .. } => "progress",
            Self::Meters { .. } => "meters",
            Self::Log { .. } => "log",
            Self::Warnings { .. } => "warnings",
            Self::Result { .. } => "result",
            Self::Error { .. } => "error",
        }
    }
}

/// Reads one text frame from the engine (the Rust client in tests, the fixtures). Total, like
/// [`decode_client`](super::decode_client).
pub fn decode_server(text: &str) -> Result<ServerMessage, ProtocolError> {
    decode(text, SERVER_TYPES, MAX_SERVER_FRAME_BYTES)
}

/// Compact JSON for the wire. Progress and meters are sanitised first
/// ([`ServerMessage::sanitise`]), so they never fail on a stray value.
pub fn encode_server(message: &ServerMessage) -> Result<String, ProtocolError> {
    match message {
        ServerMessage::Progress { .. } | ServerMessage::Meters { .. } => {
            let mut clean = message.clone();
            clean.sanitise();
            encode(&clean, MAX_SERVER_FRAME_BYTES)
        }
        _ => encode(message, MAX_SERVER_FRAME_BYTES),
    }
}

/// The golden-fixture form: pretty, with a trailing newline. Sanitised like the wire form.
pub fn encode_server_pretty(message: &ServerMessage) -> Result<String, ProtocolError> {
    let mut clean = message.clone();
    clean.sanitise();
    encode_pretty(&clean)
}
