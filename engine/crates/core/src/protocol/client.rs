// SPDX-License-Identifier: GPL-3.0-or-later

//! Client -> engine messages.

use serde::{Deserialize, Serialize};

use super::{
    DocId, MAX_CLIENT_FRAME_BYTES, MAX_HELLO_FRAME_BYTES, ProtocolError, RequestId, decode, encode,
    encode_pretty, non_negative,
};
use crate::command::Command;
use crate::cue::{FadeSpec, Modifiers};
use crate::media_path::{PathStyle, is_absolute, to_portable};
use crate::model::{CueId, CueListId, WorkspaceId};

/// Longest `coalesce` key of a `command`, bytes.
pub const MAX_COALESCE_KEY: usize = 128;

/// Longest key name of a `hotkey` message, bytes (key names are short: `"a"`, `"f5"`, `"space"`).
pub const MAX_HOTKEY_KEY: usize = 64;

/// Every `type` a client may send.
pub const CLIENT_TYPES: &[&str] = &[
    "hello",
    "command",
    "transport",
    "undo",
    "redo",
    "request",
    "close",
    "panic",
    "hotkey",
    "goodbye",
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// First message on a connection. Any `protocol_version` decodes (the engine, not the codec,
    /// refuses a mismatch with `error { code: protocol_version_mismatch }`), so an old client
    /// can still be told why it is refused.
    Hello {
        protocol_version: u32,
        /// From `engine.json`.
        token: String,
        /// Shown in logs, e.g. "openLab for macOS 0.0.1".
        client_name: String,
        /// The open workspace this connection is bound to (`opened.doc`, or a `doc` of the
        /// `workspaces` list): its snapshot comes in `welcome` and everything sent acts on it.
        /// Absent: the app connection, which sees the list of open workspaces and may send only
        /// the app-scope messages (docs/protocol.md section 2).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<DocId>,
    },
    /// A model edit. `command` is core's [`Command`], with its own wire format.
    ///
    /// `coalesce` joins this edit to the previous one in a single undo step when both carry the
    /// same key and nothing else happened between (a fader drag sends many edits, one gesture).
    /// A fresh key per gesture; at most [`MAX_COALESCE_KEY`] bytes (longer is refused).
    Command {
        id: RequestId,
        command: Box<Command>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        coalesce: Option<String>,
    },
    Transport {
        id: RequestId,
        action: TransportAction,
    },
    Undo {
        id: RequestId,
    },
    Redo {
        id: RequestId,
    },
    Request {
        id: RequestId,
        request: Request,
    },
    /// Close a workspace: `doc`, or the one this connection is bound to when absent. Refused
    /// with `unsaved_changes` while it has unsaved edits unless `discard_changes`. Its cues
    /// stop at once; its connections get `closed` and are closed.
    Close {
        id: RequestId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        doc: Option<DocId>,
        #[serde(default, skip_serializing_if = "is_false")]
        discard_changes: bool,
    },
    /// The app-scope Panic (Esc): every open workspace panics, as a `panic` transport in each
    /// would (twice within the fade: a hard stop). On any connection.
    Panic {
        id: RequestId,
    },
    /// A hotkey went down or up (app-scope, on any connection): it triggers the cues with this
    /// hotkey in every open workspace, in front or not. `down` starts them (a second trigger on
    /// an active one), `up` is the release (`trigger_released`) for each.
    Hotkey {
        id: RequestId,
        key: String,
        #[serde(default)]
        modifiers: Modifiers,
        #[serde(default)]
        phase: HotkeyPhase,
    },
    /// Closing politely. `shutdown` also asks the engine to quit (the app is closing); without it
    /// the engine keeps running for a reconnect.
    Goodbye {
        #[serde(default)]
        shutdown: bool,
    },
}

/// What the operator does at the transport. Each maps to one cue-engine `Input`
/// (`docs/protocol.md` has the table).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TransportAction {
    Go {
        list: CueListId,
    },
    Panic,
    /// GO from `cue` in one step (OSC `/go "3"`): the playhead moves to it, then GO. Nothing is
    /// started if `cue` is not a playhead position of `list` (`not_in_list` warning).
    GoFrom {
        list: CueListId,
        cue: CueId,
    },
    /// Stops every cue and preview at once (`Input::StopAll`).
    StopAll,
    /// The same as `stop_all` today: there are no stop fade-outs yet, so a stop is already at
    /// once (as `hard_stop` is to `stop` for one cue). Kept apart so clients can say which they
    /// mean once stops can fade.
    HardStopAll,
    /// The cue's own panic: it and what is in it fade out over the workspace's `panic_duration`
    /// (0: at once), as a cue's `panic` second trigger. Not a workspace panic.
    PanicCue {
        cue: CueId,
    },
    PauseAll,
    ResumeAll,
    /// `cue: null` puts the playhead past the end.
    SetPlayhead {
        list: CueListId,
        cue: Option<CueId>,
    },
    SetPlayheadByNumber {
        list: CueListId,
        number: String,
    },
    PlayheadNext {
        list: CueListId,
    },
    PlayheadPrevious {
        list: CueListId,
    },
    /// Starts the cue without moving the playhead.
    Start {
        cue: CueId,
    },
    /// Stops the cue, fading out if `fade_out` has a duration.
    Stop {
        cue: CueId,
        #[serde(default)]
        fade_out: Option<FadeSpec>,
    },
    /// Stops the cue at once, whatever it is doing.
    HardStop {
        cue: CueId,
    },
    Pause {
        cue: CueId,
    },
    Resume {
        cue: CueId,
    },
    /// Loads the cue so it starts `at` seconds into its timeline (0: a plain load).
    Load {
        cue: CueId,
        at: f64,
    },
    Reset {
        cue: CueId,
    },
    /// The hotkey or MIDI key that triggered the cue went up (second trigger on release).
    TriggerReleased {
        cue: CueId,
    },
    /// Audition an audio cue outside the show.
    Preview {
        cue: CueId,
    },
    /// End the cue's preview; `null` ends every preview.
    StopPreview {
        #[serde(default)]
        cue: Option<CueId>,
    },
}

/// Document and device requests. The reply is a `result` with the same id; `snapshot` also
/// sends a fresh `snapshot` message first (a `workspaces` message on the app connection).
///
/// `new`, `open` and `open_recovery` open another workspace beside the ones open, which keep
/// playing; the `result` carries `opened { doc }`, and a connection bound to that doc gets its
/// snapshot. They are app-scope: any connection may send them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    New {},
    Open {
        path: String,
        /// What to do about an autosave newer than the show file. Absent: nothing is opened and
        /// the `result` carries a `recovery_offer`; send the request again with a choice.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recovery: Option<RecoveryChoice>,
    },
    /// Open the autosave of a show that could not be opened (listed by `scan_recovery`) as an
    /// untitled, unsaved workspace: saving it has to choose a path (`save_as`).
    OpenRecovery {
        workspace_id: WorkspaceId,
        /// Which file when several autosaves share the workspace id (a duplicated show):
        /// the `path_hash` of the listed entry. Absent: any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path_hash: Option<String>,
        /// Likewise, the entry's `modified_ms` (a stale file moved aside has its own time).
        /// Absent: the newest.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        modified_ms: Option<i64>,
    },
    /// List the autosave files, for a show that cannot be opened.
    ScanRecovery,
    Save,
    SaveAs {
        path: String,
    },
    /// Relink missing media with files found in `folder` (WS-03; an absolute path, searched on
    /// the document worker within core's scan limits).
    Relink {
        folder: String,
    },
    ListDevices,
    /// Switches output overrides (PB-10) for the whole engine, every open workspace: not saved
    /// in any show file and gone when the engine restarts, as QLab's Overrides are. The
    /// `status` message reports them. Answers `ok` with no data.
    SetOutputOverrides {
        /// Network cues still run, complete and continue, but send nothing.
        network_disabled: bool,
    },
    /// Puts the connection's workspace in Show Mode or back in Edit Mode (SM-01). The engine
    /// keeps it (not in the show file) so it survives a UI restart, reports it in `document`,
    /// and prevents system and display sleep while any open workspace is in Show Mode (SM-07).
    /// Answers `ok` with no data. Workspace-bound.
    SetShowMode {
        on: bool,
    },
    /// One report of every broken cue, missing file, unmapped device and unreachable target
    /// (SM-06), from the engine's current state.
    PreshowCheck,
    /// The newest show-log entries the engine holds in memory (DX-02), for a log window opened
    /// after the fact. `max` absent: all of them (at most 1000).
    LogTail {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<u32>,
    },
    /// Send the full state again (after a `revision` or `seq` gap).
    Snapshot,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Whether a `hotkey` message is the key going down or coming up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyPhase {
    #[default]
    Down,
    Up,
}

/// The operator's answer to a recovery offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryChoice {
    /// Open the autosave (unsaved changes included). The workspace is dirty until saved.
    Restore,
    /// Open the show file as saved. The autosave stays where it is and is offered again next
    /// time, until a save, or the next autosave, replaces it.
    Ignore,
}

/// Longest `relink` folder path accepted, in bytes (the longest real path on any platform
/// openLab runs on is far shorter).
pub const MAX_FOLDER_BYTES: usize = 4096;

impl ClientMessage {
    /// Checks what serde cannot: numbers that must be finite and in range, paths not empty.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Self::Transport { action, .. } => match action {
                TransportAction::Load { at, .. } => non_negative("transport.load.at", *at),
                TransportAction::Stop {
                    fade_out: Some(fade),
                    ..
                } => non_negative("transport.stop.fade_out.duration", fade.duration),
                _ => Ok(()),
            },
            Self::Request {
                request:
                    Request::Open { path, .. }
                    | Request::SaveAs { path }
                    | Request::Relink { folder: path },
                ..
            } if path.is_empty() => Err(ProtocolError::OutOfRange {
                field: "request.path",
            }),
            Self::Request {
                request: Request::Relink { folder },
                ..
            } if folder.len() > MAX_FOLDER_BYTES => Err(ProtocolError::OutOfRange {
                field: "request.folder",
            }),
            Self::Request {
                request:
                    Request::Open { path, .. }
                    | Request::SaveAs { path }
                    | Request::Relink { folder: path },
                ..
            } if !is_absolute(&to_portable(path, PathStyle::host())) => {
                Err(ProtocolError::NotAbsolute {
                    field: "request.path",
                })
            }
            Self::Hotkey { key, .. } if key.is_empty() || key.len() > MAX_HOTKEY_KEY => {
                Err(ProtocolError::OutOfRange {
                    field: "hotkey.key",
                })
            }
            _ => Ok(()),
        }
    }

    /// The `type` string of this message.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::Command { .. } => "command",
            Self::Transport { .. } => "transport",
            Self::Undo { .. } => "undo",
            Self::Redo { .. } => "redo",
            Self::Request { .. } => "request",
            Self::Close { .. } => "close",
            Self::Panic { .. } => "panic",
            Self::Hotkey { .. } => "hotkey",
            Self::Goodbye { .. } => "goodbye",
        }
    }
}

/// Reads one text frame from a client. Total: any input gives a message or an error, never a
/// panic. Unknown fields are ignored; an unknown `type` is [`ProtocolError::UnknownType`].
pub fn decode_client(text: &str) -> Result<ClientMessage, ProtocolError> {
    let message: ClientMessage = decode(text, CLIENT_TYPES, MAX_CLIENT_FRAME_BYTES)?;
    message.validate()?;
    Ok(message)
}

/// Reads the first frame of a connection, before the token has been checked: at most
/// [`MAX_HELLO_FRAME_BYTES`], and it must be a `hello`. Anything else is refused as a bad token
/// by the server (the error here says why).
pub fn decode_hello(text: &str) -> Result<ClientMessage, ProtocolError> {
    let message: ClientMessage = decode(text, CLIENT_TYPES, MAX_HELLO_FRAME_BYTES)?;
    match message {
        ClientMessage::Hello { .. } => Ok(message),
        other => Err(ProtocolError::UnknownType(other.type_name().to_owned())),
    }
}

/// Compact JSON for the wire (what the client sends).
pub fn encode_client(message: &ClientMessage) -> Result<String, ProtocolError> {
    message.validate()?;
    encode(message, MAX_CLIENT_FRAME_BYTES)
}

/// The golden-fixture form: pretty, with a trailing newline.
pub fn encode_client_pretty(message: &ClientMessage) -> Result<String, ProtocolError> {
    message.validate()?;
    encode_pretty(message)
}
