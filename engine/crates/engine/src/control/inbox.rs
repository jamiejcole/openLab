// SPDX-License-Identifier: GPL-3.0-or-later

//! The control thread's inbox and its outbound channel (ARCHITECTURE §3.1: a crossbeam-channel
//! inbox in, a broadcast to the network out).

use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{Sender, TrySendError};
use openlab_audio::{MediaError, Preloaded, Probed};
use openlab_core::protocol::{ClientMessage, ServerMessage};

/// Everything that wakes the control thread besides its timers.
#[derive(Debug)]
pub enum Inbox {
    /// A client message, already decoded and authenticated by the network layer (M4.3). Every
    /// `command`, `transport`, `undo`, `redo` and `request` is answered on `reply` with exactly
    /// one `result` (a `snapshot` request also gets its `snapshot` there first); broadcasts go
    /// to the outbound channel.
    Client {
        message: ClientMessage,
        reply: Sender<ServerMessage>,
    },
    /// A probe worker's answer (internal feedback).
    Probe(ProbeAnswer),
    /// Stop the control loop.
    Shutdown,
}

/// What a probe worker found out.
#[derive(Debug)]
pub enum ProbeAnswer {
    Probed {
        key: String,
        ticket: u64,
        result: Result<(PathBuf, Probed), MediaError>,
    },
    Preloaded {
        key: String,
        ticket: u64,
        result: Result<Arc<Preloaded>, MediaError>,
    },
}

/// Messages for every client. Bounded, so a stalled network thread cannot grow the engine's
/// memory: a message that does not fit is dropped and counted, and the client sees a gap in
/// `runtime.seq` or `revision` and asks for a snapshot (docs/protocol.md §2).
pub struct Outbound {
    tx: Sender<ServerMessage>,
    dropped: u64,
}

/// Outbound messages that can wait for the network thread before one is dropped.
pub const OUTBOUND_CAPACITY: usize = 4096;

impl Outbound {
    pub fn new(tx: Sender<ServerMessage>) -> Self {
        Self { tx, dropped: 0 }
    }

    pub fn send(&mut self, message: ServerMessage) {
        match self.tx.try_send(message) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(_)) => self.dropped += 1,
        }
    }

    /// Messages dropped because the channel was full.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// Answers one client: a reply that cannot be delivered (the client went away) is dropped.
pub fn reply(to: &Sender<ServerMessage>, message: ServerMessage) {
    let _ = to.try_send(message);
}
