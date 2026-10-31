// SPDX-License-Identifier: GPL-3.0-or-later

//! Engine -> client messages.

use serde::{Deserialize, Serialize};

use super::wire::{
    CueMediaDuration, RuntimeChange, WireBrokenReason, WireCueState, WireProblem, WireResolution,
    WireWarning,
};
use super::{
    DocId, MAX_SERVER_FRAME_BYTES, PROTOCOL_VERSION, ProtocolError, RequestId, decode, encode,
    encode_pretty,
};
use crate::command::CommandError;
use crate::cue::Cue;
use crate::model::{
    CueId, CueListId, NetworkPatch, NetworkPatchId, NetworkProtocol, OutputPatch, PatchId,
    Workspace, WorkspaceId, WorkspaceSettings,
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
    "log_entry",
    "status",
    "warnings",
    "workspaces",
    "closed",
    "result",
    "error",
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Answer to an accepted `hello`. On a connection bound to a workspace: `doc` and its
    /// `snapshot`, everything the client needs to draw the show. On the app connection:
    /// `workspaces`, the open workspaces.
    Welcome {
        protocol_version: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        doc: Option<DocId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        snapshot: Option<Box<Snapshot>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspaces: Option<Vec<WorkspaceInfo>>,
    },
    /// The whole state again, after the `snapshot` request (or a resync). A client replaces
    /// everything it holds.
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
    /// Elapsed and remaining time of running cues, at most 30 Hz, only while something runs or
    /// waits. `items`: actions (audio cues' voices, and cues without a voice); `waits`: pre-wait
    /// and post-wait countdowns (absent when there are none, M4.2e).
    Progress {
        items: Vec<ProgressItem>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        waits: Vec<WaitProgress>,
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
    /// One new show-log record, as it is made (DX-02, M4.7c): for a live log view. Lossy like
    /// `meters`, and rate-limited: `seq` rises by one per record the engine makes, pushed or
    /// not, so a gap means records to fetch with `log_tail` (whose `seq` says where it ends).
    LogEntry {
        seq: u64,
        entry: LogEntry,
    },
    /// System health, about once a second and when something changes (DX-03). Lossy like
    /// `meters`: the next one replaces it.
    Status {
        status: Box<SystemStatus>,
    },
    /// The workspace-level notices now in force (load repairs, damaged media, ...). Replaces the
    /// previous list; empty clears it. Show-time warnings come in `runtime`.
    Warnings {
        warnings: Vec<WireWarning>,
    },
    /// To app connections: the open workspaces, in open order, whenever one opens or closes or
    /// one's name, path or unsaved state changes. Replaces the previous list.
    Workspaces {
        workspaces: Vec<WorkspaceInfo>,
    },
    /// To the connections bound to `doc`: the workspace was closed. The engine closes the
    /// connection next; a client must not reconnect to it.
    Closed {
        doc: DocId,
    },
    /// Answer to a `command`, `transport`, `undo`, `redo`, `request`, `close`, `panic` or
    /// `hotkey`.
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

/// One open workspace, as the app connection lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    /// What `hello { workspace }` and `close { doc }` name.
    pub doc: DocId,
    pub workspace_id: WorkspaceId,
    pub name: String,
    /// Where it is saved; `null` = never saved.
    pub path: Option<String>,
    /// Edits since the last save.
    pub dirty: bool,
    /// [`crate::path_hash`] of `path` as the engine names this doc's autosave
    /// (`<workspace_id>-<path_hash>`, the `path_hash` of a `recovery_entries` entry); `null` for a
    /// doc never saved. Absent from an engine that predates it: read as `null`.
    #[serde(default)]
    pub path_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResultData {
    /// `new`, `open` or `open_recovery` opened this workspace; bind a connection to it.
    Opened { doc: DocId },
    Devices {
        devices: Vec<DeviceEntry>,
        /// The device chosen with `set_output_device` (opened at the next engine start); absent
        /// when none is chosen. The device playing now is `status.device`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected: Option<String>,
    },
    Relink {
        relinked: u64,
        ambiguous: Vec<AmbiguousMatch>,
        unmatched: Vec<CueId>,
        /// A scan limit (depth, folders, files or time) ended the search early, so files beyond
        /// it were not considered: `unmatched` may name cues whose file is in the folder.
        truncated: bool,
        /// Folders or entries the scan could not read, plus names that are not UTF-8.
        skipped: u64,
    },
    /// `open` found an autosave newer than the show file and nothing was opened. Send `open`
    /// again with a `recovery` choice.
    RecoveryOffer {
        /// The `path` of the `open` this answers, so a client can match the offer to its request.
        path: String,
        workspace_name: String,
        /// When the autosave was written: milliseconds since the Unix epoch (wall clock, UTC).
        modified_ms: i64,
    },
    /// Answer to `scan_recovery`, newest first.
    RecoveryEntries { entries: Vec<RecoveryEntryInfo> },
    /// Answer to `log_tail`: the newest show-log entries, oldest first.
    LogTail {
        entries: Vec<LogEntry>,
        /// Entries the engine could not write to the log file so far (the disk fell behind).
        /// They are still in `entries` while they are in the engine's memory.
        dropped: u64,
        /// The `seq` of the newest record the engine has made (0: none yet), the last of
        /// `entries` unless `max` cut them short. A `log_entry` with a `seq` up to this one is
        /// already covered by this answer.
        #[serde(default)]
        seq: u64,
    },
    /// Answer to `preshow_check` (SM-06).
    /// Boxed: the report is large and a `ServerMessage` is passed around by value.
    PreshowCheck(Box<PreshowReport>),
}

/// The pre-show check (SM-06, NFR-07): everything that would keep the show from running,
/// computed from the engine's state when the request is handled (no disk or network I/O: media
/// status is the latest probe, `docs/control.md`). Every list is sorted for display.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PreshowReport {
    /// True when nothing is a problem: no broken cue, no `system` problem, and none of the
    /// lists below holds a problem. Not problems: `network` entries that are only `not_checked`,
    /// patches no cue uses, and the informational `preload_over_budget` warning.
    pub ok: bool,
    pub summary: PreshowSummary,
    /// Cues with static problems, in workspace order (lists in order, cues depth-first).
    pub broken_cues: Vec<PreshowCue>,
    /// Media files that are missing or cannot be played, by path.
    pub media: Vec<PreshowMedia>,
    /// Output patches with no device to play on, in workspace order, then patches cues name that
    /// the workspace does not have, by id. A patch no cue uses is listed (`cues` is empty) but is
    /// not counted in the summary and does not fail the check.
    pub patches: Vec<PreshowPatch>,
    /// Network destinations cues use, by name, then destinations cues name that the workspace
    /// does not have, by id.
    pub network: Vec<PreshowTarget>,
    /// The workspace-level warnings in force (device lost or stalled, damaged media, cues whose
    /// slices cannot play, ...), as in the `warnings` message; by `code`, then cue, then message.
    /// `preload_over_budget` is information: listed, not counted, never a failure.
    pub system: Vec<WireWarning>,
}

/// How many problems a [`PreshowReport`] holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreshowSummary {
    pub broken_cues: u64,
    /// Media files missing or unsupported (the cause of broken cues, not counted in `total`).
    pub media: u64,
    /// Patches with no device that some cue uses (not counted in `total`; unused ones are not
    /// counted at all).
    pub patches: u64,
    /// Destinations with status `missing` (the problems).
    pub network: u64,
    /// Destinations with status `not_checked`: reachability is not probed, so they are neither
    /// a problem nor known to be fine.
    pub network_unchecked: u64,
    /// `system` entries that are problems (not `preload_over_budget`).
    pub system: u64,
    /// `broken_cues + system`: the problem rows a list shows. Media, patches and destinations
    /// are the causes of broken cues, already in `broken_cues`.
    pub total: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreshowCue {
    pub cue: CueId,
    pub list: CueListId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<String>,
    /// The name shown in the cue list (a cue named after its file shows the file name).
    pub name: String,
    pub problems: Vec<WireProblem>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreshowMediaStatus {
    /// Not found on disk.
    Missing,
    /// Found but cannot be played; see `reason`.
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreshowMedia {
    pub path: String,
    pub status: PreshowMediaStatus,
    /// For `unsupported`: the probe's message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The cues that use the file, in workspace order.
    pub cues: Vec<CueId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreshowPatch {
    pub patch: PatchId,
    /// Absent when the workspace has no such patch (a cue names a deleted one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The device this machine maps the patch to; absent when it is not mapped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// Whether this machine has a device mapping for the patch (WS-07).
    pub mapped: bool,
    /// Why the patch cannot play; absent when the workspace has no such patch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<WireResolution>,
    /// The audio cues on the patch, in workspace order (empty for an unused patch).
    pub cues: Vec<CueId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreshowTargetStatus {
    /// The workspace has no such destination (a cue names a deleted one).
    Missing,
    /// The destination exists; reachability is not probed in this version.
    NotChecked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreshowTarget {
    pub patch: NetworkPatchId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<NetworkProtocol>,
    pub status: PreshowTargetStatus,
    /// The network cues that use the destination, in workspace order.
    pub cues: Vec<CueId>,
}

/// One show-log record (DX-02): what the engine writes to its log file, one JSON line each.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    /// Wall clock, milliseconds since the Unix epoch (UTC).
    pub time_ms: i64,
    /// Show time: seconds of audio rendered since the engine started the device; absent before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_time: Option<f64>,
    pub level: LogLevel,
    pub kind: LogKind,
    pub message: String,
    /// The cue the record is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cue: Option<CueId>,
    /// For `trigger`: where it came from (`ui`, `osc 192.0.2.7`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// What a show-log record is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogKind {
    /// A GO, with the cue it starts.
    Go,
    /// Any other transport action (stop, pause, panic, playhead moves...).
    Transport,
    /// An input arrived from a client or OSC, before it was acted on.
    Trigger,
    CueStart,
    CueStop,
    /// A cue broke while running or could not start.
    CueFail,
    /// Audio device events.
    Device,
    /// Open, save, close.
    Document,
    /// Everything else the engine reports (warnings, errors, health).
    Engine,
}

/// The `status` message (DX-03).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SystemStatus {
    /// CPU used by the engine process since the previous status, in percent of one core (can
    /// exceed 100 on several cores); `null` where the platform gives no figure.
    pub cpu_percent: Option<f64>,
    /// Audio engine load: time the audio callback takes over the time of its block, worst case
    /// since the previous status (1.0 = no headroom); `null` when no callback ran since then
    /// (device lost) and for offline rendering.
    pub engine_load: Option<f64>,
    /// Output underruns and overruns the device layer reported, since start.
    pub xruns: u64,
    pub disk: DiskStatus,
    pub device: DeviceStatus,
    /// Show-log records lost since start because the disk could not keep up.
    pub log_dropped: u64,
    /// Sequence number of the newest show-log record (as in `log_entry` and `log_tail`; 0:
    /// none yet). A client following `log_entry` that sees a higher number than the last it
    /// received missed a push and reads the gap with `log_tail`. Absent from older engines: 0.
    #[serde(default)]
    pub log_seq: u64,
    /// The OSC/network output override is on (PB-10): Network cues send nothing. The UI must
    /// show this plainly while it is true.
    #[serde(default)]
    pub network_output_disabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskStatus {
    /// Files streaming from disk now.
    pub streams: u32,
    /// Frames of silence played because a stream was not read in time, since start.
    pub underrun_frames: u64,
    /// No underrun in the last ten seconds.
    pub healthy: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceStatus {
    /// The output device the engine plays on; `null` when there is none.
    pub name: Option<String>,
    pub state: DeviceState,
    pub sample_rate: u32,
    /// Frames per callback, when the host says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_frames: Option<u32>,
    /// Mixer outputs that reach the device.
    pub channels: u32,
    /// Callback to output, from the host's timestamps, when it gives them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceState {
    Ok,
    /// Gone or silent; the engine is trying to reopen it.
    Lost,
    /// Will not come back without a restart.
    Failed,
    /// No device (offline rendering).
    Absent,
}

/// One autosave file found by `scan_recovery`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryEntryInfo {
    pub workspace_id: WorkspaceId,
    /// Which show path the autosave is bound to; absent for a workspace that was never saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_hash: Option<String>,
    /// The workspace's name, when the file could be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_name: Option<String>,
    /// Milliseconds since the Unix epoch (wall clock, UTC), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_ms: Option<i64>,
    /// Moved aside as not newer than its show file; it may still hold the newest work.
    pub stale: bool,
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
    /// A request needs a workspace and none is open (an app connection, a closed doc), or a
    /// save has no path yet.
    NoWorkspace,
    /// `close` would drop unsaved edits and did not say `discard_changes`.
    UnsavedChanges,
    /// `open` of a show file that is open already (the result names no doc: the app finds it
    /// by path in its `workspaces` list).
    AlreadyOpen,
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
    /// The workspace is in Show Mode (SM-01, SM-07; set with `set_show_mode`): kept by the
    /// engine while it runs, not saved in the show file, so a relaunched UI gets it back.
    /// Absent: `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub show_mode: bool,
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
    /// The media lengths of the audio cues whose file is probed and playable, by cue id
    /// (M5.4d); later changes come as `media_duration` runtime changes. Absent: none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media_durations: Vec<CueMediaDuration>,
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

/// A pre-wait or post-wait counting down, in seconds. A cue can have both an action item and a
/// post-wait at once.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WaitProgress {
    pub cue: CueId,
    pub wait: WireWait,
    pub elapsed: f64,
    pub remaining: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireWait {
    PreWait,
    PostWait,
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
            Self::Progress { items, waits } => {
                for item in items {
                    item.elapsed = clean(item.elapsed);
                    item.remaining = item.remaining.map(clean);
                }
                for wait in waits {
                    wait.elapsed = clean(wait.elapsed);
                    wait.remaining = clean(wait.remaining);
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
            Self::LogEntry { entry, .. } => {
                entry.show_time = entry.show_time.filter(|t| t.is_finite());
            }
            Self::Status { status } => {
                status.cpu_percent = status.cpu_percent.map(clean);
                status.engine_load = status.engine_load.map(clean);
                status.device.latency_ms = status.device.latency_ms.map(clean);
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
            Self::LogEntry { .. } => "log_entry",
            Self::Status { .. } => "status",
            Self::Warnings { .. } => "warnings",
            Self::Workspaces { .. } => "workspaces",
            Self::Closed { .. } => "closed",
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
        ServerMessage::Progress { .. }
        | ServerMessage::Meters { .. }
        | ServerMessage::LogEntry { .. }
        | ServerMessage::Status { .. } => {
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
