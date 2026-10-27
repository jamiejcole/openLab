// SPDX-License-Identifier: GPL-3.0-or-later

//! The network layer (ARCHITECTURE §3.1, §6): a tokio runtime with two threads that serves the
//! UI protocol over WebSocket on `127.0.0.1` (docs/network.md). It talks to the control thread
//! only through the control inbox (`Inbox::Client`, one reply channel per connection; `Inbox::Log`)
//! and the outbound broadcast channel; it never touches the workspace.

mod hub;
mod token;
mod ws;

pub use token::{constant_time_eq, generate_token};
pub use ws::{ServerConfig, WsServer};

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
