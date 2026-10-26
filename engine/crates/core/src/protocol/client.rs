// SPDX-License-Identifier: GPL-3.0-or-later

//! Client -> engine messages.

use serde::{Deserialize, Serialize};

use super::{ProtocolError, RequestId, decode, encode, encode_pretty, non_negative};
use crate::command::Command;
use crate::cue::FadeSpec;
use crate::model::{CueId, CueListId};

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
    /// The Stop All key. The engine runs it as Panic today (the hotkey is "Stop All / Panic",
    /// ARCHITECTURE section 4); a separate variant keeps the two nameable apart.
    StopAll,
    Panic,
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
    /// Loads the cue so it starts `at` non_negative into its timeline (0: a plain load).
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
/// `revert` and `snapshot` also send a fresh `snapshot` message first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    New,
    Open {
        path: String,
    },
    Save,
    SaveAs {
        path: String,
    },
    /// Reload the saved file, dropping unsaved edits.
    Revert,
    /// Relink missing media with files found in `folder` (WS-03).
    Relink {
        folder: String,
    },
    ListDevices,
    /// Send the full state again (after a `revision` or `seq` gap).
    Snapshot,
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
                    Request::Open { path } | Request::SaveAs { path } | Request::Relink { folder: path },
                ..
            } if path.is_empty() => Err(ProtocolError::OutOfRange {
                field: "request.path",
            }),
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
    let message: ClientMessage = decode(text, CLIENT_TYPES)?;
    message.validate()?;
    Ok(message)
}

/// Compact JSON for the wire (what the client sends).
pub fn encode_client(message: &ClientMessage) -> Result<String, ProtocolError> {
    message.validate()?;
    encode(message)
}

/// The golden-fixture form: pretty, with a trailing newline.
pub fn encode_client_pretty(message: &ClientMessage) -> Result<String, ProtocolError> {
    message.validate()?;
    encode_pretty(message)
}
