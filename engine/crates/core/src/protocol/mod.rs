// SPDX-License-Identifier: GPL-3.0-or-later

//! The engine <-> UI protocol (ARCHITECTURE section 6): message types, the codec and the version.
//! These types are the single source of truth; `docs/protocol.md` is the spec and
//! `engine/fixtures/protocol/` holds one golden JSON file per message, which the Swift app
//! decodes too. No sockets or threads here: the server is the binary's (M4.3).
//!
//! Compatibility rules (see the spec): unknown **fields** are ignored, so a newer peer may add
//! optional fields without a version bump; unknown message **types** are an error
//! ([`ProtocolError::UnknownType`]); removing or re-typing a field, or changing what one means,
//! bumps [`PROTOCOL_VERSION`]. Times are non_negative (f64), never samples.

mod client;
mod server;
mod wire;

pub use client::*;
pub use server::*;
pub use wire::*;

use serde::{Deserialize, Serialize};

/// Version a peer sends in `hello` and the engine answers in `welcome`. The engine accepts a
/// client only on an equal version.
pub const PROTOCOL_VERSION: u32 = 1;

/// Longest frame either side decodes or encodes. A frame this long is a bug or an attack, not a
/// show (a 10 000-cue snapshot is a few MiB).
pub const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;

/// Client-chosen id that ties a `result` to the message that asked. Unique per connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub u64);

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    #[error("frame of {len} bytes is over the {MAX_FRAME_BYTES}-byte limit")]
    TooLarge { len: usize },
    #[error("not valid protocol JSON: {0}")]
    Malformed(String),
    #[error("message has no \"type\" string")]
    MissingType,
    #[error("unknown message type \"{0}\"")]
    UnknownType(String),
    #[error("{field} must be a finite number")]
    NonFinite { field: &'static str },
    #[error("{field} is out of range")]
    OutOfRange { field: &'static str },
    #[error("could not encode: {0}")]
    Encode(String),
}

/// `value` must be finite and not negative.
pub(crate) fn non_negative(field: &'static str, value: f64) -> Result<(), ProtocolError> {
    if !value.is_finite() {
        Err(ProtocolError::NonFinite { field })
    } else if value < 0.0 {
        Err(ProtocolError::OutOfRange { field })
    } else {
        Ok(())
    }
}

/// Parses a frame, checks its envelope, then reads it as `T`. Never panics: JSON deeper than
/// serde_json's recursion limit is an error too.
pub(crate) fn decode<T: serde::de::DeserializeOwned>(
    text: &str,
    known: &[&str],
) -> Result<T, ProtocolError> {
    if text.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::TooLarge { len: text.len() });
    }
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| ProtocolError::Malformed(e.to_string()))?;
    match value.get("type").and_then(serde_json::Value::as_str) {
        None => return Err(ProtocolError::MissingType),
        Some(t) if !known.contains(&t) => return Err(ProtocolError::UnknownType(t.to_owned())),
        Some(_) => {}
    }
    serde_json::from_value(value).map_err(|e| ProtocolError::Malformed(e.to_string()))
}

/// Compact JSON for the wire.
pub(crate) fn encode<T: Serialize>(message: &T) -> Result<String, ProtocolError> {
    let text = serde_json::to_string(message).map_err(|e| ProtocolError::Encode(e.to_string()))?;
    if text.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::TooLarge { len: text.len() });
    }
    Ok(text)
}

/// Pretty JSON with a trailing newline, as the golden fixtures are stored. Keys come out in the
/// types' declaration order, so the files are stable.
pub(crate) fn encode_pretty<T: Serialize>(message: &T) -> Result<String, ProtocolError> {
    let mut text =
        serde_json::to_string_pretty(message).map_err(|e| ProtocolError::Encode(e.to_string()))?;
    text.push('\n');
    Ok(text)
}
