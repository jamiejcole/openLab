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
/// memory. Meters and progress are dropped first (once the channel is half full: the next ones
/// replace them anyway). Sequenced messages (`runtime`, `model_changed`) that do not fit are
/// dropped and counted; the client sees the gap in `seq`/`revision` and asks for a snapshot
/// (docs/protocol.md §2). State messages without a sequence (`document`, `warnings`) are never
/// lost silently: the caller resends the latest once there is room ([`Outbound::send_state`]).
pub struct Outbound {
    tx: Sender<ServerMessage>,
    dropped: u64,
    lossy_dropped: u64,
}

/// Outbound messages that can wait for the network thread before one is dropped.
pub const OUTBOUND_CAPACITY: usize = 4096;

impl Outbound {
    pub fn new(tx: Sender<ServerMessage>) -> Self {
        Self {
            tx,
            dropped: 0,
            lossy_dropped: 0,
        }
    }

    /// A sequenced or log message.
    pub fn send(&mut self, message: ServerMessage) {
        if self.offer(message).is_err() {
            self.dropped += 1;
        }
    }

    /// Meters and progress: dropped while the channel is half full, to keep room for the rest.
    pub fn send_lossy(&mut self, message: ServerMessage) {
        if self.tx.capacity().is_some_and(|c| self.tx.len() * 2 >= c)
            || self.offer(message).is_err()
        {
            self.lossy_dropped += 1;
        }
    }

    /// A state message; `false` if it did not fit and must be sent again later.
    pub fn send_state(&mut self, message: ServerMessage) -> bool {
        self.offer(message).is_ok()
    }

    /// Whether a message would fit now.
    pub fn has_room(&self) -> bool {
        !self.tx.is_full()
    }

    fn offer(&mut self, message: ServerMessage) -> Result<(), ()> {
        match self.tx.try_send(message) {
            // Nobody listens (no network layer yet): nothing to lose.
            Ok(()) | Err(TrySendError::Disconnected(_)) => Ok(()),
            Err(TrySendError::Full(_)) => Err(()),
        }
    }

    /// Sequenced and log messages dropped because the channel was full.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Meters and progress messages dropped.
    pub fn lossy_dropped(&self) -> u64 {
        self.lossy_dropped
    }
}

/// Answers one client: a reply that cannot be delivered (the client went away) is dropped.
pub fn reply(to: &Sender<ServerMessage>, message: ServerMessage) {
    let _ = to.try_send(message);
}
