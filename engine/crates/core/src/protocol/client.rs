// SPDX-License-Identifier: GPL-3.0-or-later

//! Client -> engine messages.

use serde::{Deserialize, Serialize};

use super::{
    MAX_CLIENT_FRAME_BYTES, MAX_HELLO_FRAME_BYTES, ProtocolError, RequestId, decode, encode,
    encode_pretty, non_negative,
};
use crate::command::Command;
use crate::cue::FadeSpec;
use crate::media_path::{PathStyle, is_absolute, to_portable};
use crate::model::{CueId, CueListId, WorkspaceId};

/// Every `type` a client may send.
pub const CLIENT_TYPES: &[&str] = &[
    "hello",
    "command",
    "transport",
    "undo",
    "redo",
    "request",
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
    },
    /// A model edit. `command` is core's [`Command`], with its own wire format.
    Command {
        id: RequestId,
        command: Box<Command>,
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

/// Document and device requests. The reply is a `result` with the same id; `new`, `open`,
/// `open_recovery`, `revert` and `snapshot` also send a fresh `snapshot` message first when they
/// succeed.
///
/// `new`, `open`, `open_recovery` and `revert` replace the open workspace: they stop every
/// running cue first, and are refused with `unsaved_changes` while the workspace has unsaved
/// edits unless `discard_changes` is true (the engine never prompts; the UI asks the operator).
/// A bare `{"type": "new"}` is still accepted: every added field has a default.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    New {
        #[serde(default, skip_serializing_if = "is_false")]
        discard_changes: bool,
    },
    Open {
        path: String,
        #[serde(default, skip_serializing_if = "is_false")]
        discard_changes: bool,
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
        #[serde(default, skip_serializing_if = "is_false")]
        discard_changes: bool,
    },
    /// List the autosave files, for a show that cannot be opened.
    ScanRecovery,
    Save,
    SaveAs {
        path: String,
    },
    /// Reload the saved file, dropping unsaved edits.
    Revert {
        #[serde(default, skip_serializing_if = "is_false")]
        discard_changes: bool,
    },
    /// Relink missing media with files found in `folder` (WS-03).
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
                request: Request::Open { path, .. } | Request::SaveAs { path },
                ..
            } if !is_absolute(&to_portable(path, PathStyle::host())) => {
                Err(ProtocolError::NotAbsolute {
                    field: "request.path",
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
