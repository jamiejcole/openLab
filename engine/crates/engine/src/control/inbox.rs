// SPDX-License-Identifier: GPL-3.0-or-later

//! The control thread's inbox and its outbound channel (ARCHITECTURE §3.1: a crossbeam-channel
//! inbox in, a broadcast to the network out).
//!
//! Replies and broadcasts travel on tokio channels: the network layer's tasks wait on them
//! without a thread of their own, and sending from the (synchronous) control thread never
//! blocks (`UnboundedSender::send`, `Sender::try_send`).

use std::path::PathBuf;
use std::sync::Arc;

use openlab_audio::{MediaError, Preloaded, Probed};
use openlab_core::protocol::{ClientMessage, LogLevel, ServerMessage};
use tokio::sync::mpsc::{self, error::TrySendError};

/// Everything that wakes the control thread besides its timers.
#[derive(Debug)]
pub enum Inbox {
    /// A client message, already decoded and authenticated by the network layer (M4.3). Every
    /// `command`, `transport`, `undo`, `redo` and `request` is answered on `reply` with exactly
    /// one `result` (a `snapshot` request also gets its `snapshot` there first); broadcasts go
    /// to the outbound channel.
    Client {
        message: ClientMessage,
        reply: ReplyTx,
    },
    /// A probe worker's answer (internal feedback).
    Probe(ProbeAnswer),
    /// The device thread's answer (internal feedback, docs/control.md "Devices").
    Device(super::devices::DeviceAnswer),
    /// A log entry from another thread (the network layer), sent out like the loop's own.
    Log { level: LogLevel, message: String },
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
    tx: OutboundTx,
    dropped: u64,
    lossy_dropped: u64,
}

/// Outbound messages that can wait for the network thread before one is dropped.
pub const OUTBOUND_CAPACITY: usize = 4096;

/// Where the control loop answers one client.
pub type ReplyTx = mpsc::UnboundedSender<ServerMessage>;
pub type ReplyRx = mpsc::UnboundedReceiver<ServerMessage>;

/// A reply channel. Unbounded: a client has at most a bounded number of requests in flight
/// (the network layer stops reading beyond that), and each gets a bounded number of replies.
pub fn reply_channel() -> (ReplyTx, ReplyRx) {
    mpsc::unbounded_channel()
}

/// The control loop's side of the broadcast channel.
pub type OutboundTx = mpsc::Sender<ServerMessage>;
pub type OutboundRx = mpsc::Receiver<ServerMessage>;

/// The broadcast channel, holding `capacity` messages (at least one).
pub fn outbound_channel(capacity: usize) -> (OutboundTx, OutboundRx) {
    mpsc::channel(capacity.max(1))
}

impl Outbound {
    pub fn new(tx: OutboundTx) -> Self {
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
        let max = self.tx.max_capacity();
        let queued = max.saturating_sub(self.tx.capacity());
        if queued * 2 >= max || self.offer(message).is_err() {
            self.lossy_dropped += 1;
        }
    }

    /// A state message; `false` if it did not fit and must be sent again later.
    pub fn send_state(&mut self, message: ServerMessage) -> bool {
        self.offer(message).is_ok()
    }

    /// Whether a message would fit now.
    pub fn has_room(&self) -> bool {
        self.tx.capacity() > 0
    }

    fn offer(&mut self, message: ServerMessage) -> Result<(), ()> {
        match self.tx.try_send(message) {
            // Nobody listens (no network layer yet): nothing to lose.
            Ok(()) | Err(TrySendError::Closed(_)) => Ok(()),
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
pub fn reply(to: &ReplyTx, message: ServerMessage) {
    let _ = to.send(message);
}
