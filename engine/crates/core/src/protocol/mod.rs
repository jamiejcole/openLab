// SPDX-License-Identifier: GPL-3.0-or-later

//! The engine <-> UI protocol (ARCHITECTURE section 6): message types, the codec and the version.
//! These types are the single source of truth; `docs/protocol.md` is the spec and
//! `engine/fixtures/protocol/` holds one golden JSON file per message, which the Swift app
//! decodes too. No sockets or threads here: the server is the binary's (M4.3).
//!
//! Compatibility rules (see the spec): unknown **fields** are ignored, so a newer peer may add
//! optional fields without a version bump; unknown message **types** are an error
//! ([`ProtocolError::UnknownType`]); removing or re-typing a field, or changing what one means,
//! bumps [`PROTOCOL_VERSION`]. Times are seconds (f64), never samples.

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

/// Longest frame the engine reads from a client. Commands and requests are small; a big paste is
/// well under this. Decoding buffers a frame several times over, so the limit bounds the memory
/// a local process can make the engine use.
pub const MAX_CLIENT_FRAME_BYTES: usize = 1024 * 1024;

/// Longest first frame (`hello`), checked before the token is. The server applies it to the first
/// frame of a connection with [`decode_hello`].
pub const MAX_HELLO_FRAME_BYTES: usize = 4096;

/// Longest frame a client reads from the engine (a snapshot of a big show is a few MiB).
pub const MAX_SERVER_FRAME_BYTES: usize = 32 * 1024 * 1024;

/// Client-chosen id that ties a `result` to the message that asked. Unique per connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub u64);

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    #[error("frame of {len} bytes is over the {limit}-byte limit")]
    TooLarge { len: usize, limit: usize },
    /// Not JSON, wrong field types, missing fields, or an unknown variant *inside* a known
    /// message (a transport action or request kind the receiver does not know).
    #[error("not valid protocol JSON: {0}")]
    Malformed(String),
    /// Also returned for a `type` that is not a string.
    #[error("message has no \"type\" string")]
    MissingType,
    /// The envelope's `type` is not a message type of this direction.
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

/// Reads a frame as `T`. The size limit is checked first, on the bytes, so an oversized frame
/// costs nothing. Never panics: JSON deeper than serde_json's recursion limit is an error too.
pub(crate) fn decode<T: serde::de::DeserializeOwned>(
    text: &str,
    known: &[&str],
    limit: usize,
) -> Result<T, ProtocolError> {
    if text.len() > limit {
        return Err(ProtocolError::TooLarge {
            len: text.len(),
            limit,
        });
    }
    // Peek at the envelope without building a tree: other fields are skipped, not stored.
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(rename = "type")]
        kind: Option<serde_json::Value>,
    }
    let envelope: Envelope =
        serde_json::from_str(text).map_err(|e| ProtocolError::Malformed(e.to_string()))?;
    match envelope.kind.as_ref().and_then(serde_json::Value::as_str) {
        None => return Err(ProtocolError::MissingType),
        Some(t) if !known.contains(&t) => return Err(ProtocolError::UnknownType(t.to_owned())),
        Some(_) => {}
    }
    serde_json::from_str(text).map_err(|e| ProtocolError::Malformed(e.to_string()))
}

/// Compact JSON for the wire.
pub(crate) fn encode<T: Serialize>(message: &T, limit: usize) -> Result<String, ProtocolError> {
    let text = serde_json::to_string(message).map_err(|e| ProtocolError::Encode(e.to_string()))?;
    if text.len() > limit {
        return Err(ProtocolError::TooLarge {
            len: text.len(),
            limit,
        });
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
