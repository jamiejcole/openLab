// SPDX-License-Identifier: GPL-3.0-or-later

//! The network layer (ARCHITECTURE §3.1, §6): a tokio runtime with two threads that serves the
//! UI protocol over WebSocket on `127.0.0.1` (docs/network.md). It talks to the control thread
//! only through the control inbox (`Inbox::Client`, one reply channel per connection; `Inbox::Log`)
//! and the outbound broadcast channel; it never touches the workspace.

mod hub;
mod osc;
mod osc_mirror;
mod refusals;
mod relaunch;
mod token;
mod ws;

pub use osc::{OscConfig, OscListenerStats, OscPorts};
pub use relaunch::{AppProcess, ProcessNameFn, Relaunch, RelaunchFn};
pub use token::{constant_time_eq, generate_token};
pub use ws::{ServerConfig, WsServer};

/// What a client message asks for, for the show log: `transport go`, `request save`, `command`.
/// From the wire form, so new messages and actions need no change here.
pub(crate) fn describe(message: &openlab_core::protocol::ClientMessage) -> String {
    let value = serde_json::to_value(message).unwrap_or_default();
    let field = |v: &serde_json::Value, key: &str| {
        v.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let mut text = field(&value, "type").unwrap_or_else(|| "message".to_owned());
    // The tagged payload of a transport action or a request.
    let inner = value.get("action").or_else(|| value.get("request"));
    if let Some(name) = inner.and_then(|v| field(v, "type")) {
        text.push(' ');
        text.push_str(&name);
    }
    text
}

/// Why the server could not start.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("no randomness for the session token: {0}")]
    Entropy(String),
    #[error("could not start the network runtime: {0}")]
    Runtime(std::io::Error),
    #[error("could not listen on 127.0.0.1: {0}")]
    Bind(std::io::Error),
}
