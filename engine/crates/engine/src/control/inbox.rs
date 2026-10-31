// SPDX-License-Identifier: GPL-3.0-or-later

//! The control thread's inbox and its outbound channel (ARCHITECTURE §3.1: a crossbeam-channel
//! inbox in, a broadcast to the network out).
//!
//! Replies and broadcasts travel on tokio channels: the network layer's tasks wait on them
//! without a thread of their own, and sending from the (synchronous) control thread never
//! blocks (`UnboundedSender::send`, `Sender::try_send`).

use std::path::PathBuf;
use std::sync::Arc;

use openlab_audio::{MediaError, Preloaded, Probed, Waveform, WaveformError};
use openlab_core::protocol::{ClientMessage, LogLevel, ServerMessage};
use tokio::sync::mpsc::{self, error::TrySendError};

/// Everything that wakes the control thread besides its timers.
#[derive(Debug)]
pub enum Inbox {
    /// A client message, already decoded and authenticated by the network layer (M4.3). Every
    /// `command`, `transport`, `undo`, `redo` and `request` is answered on `reply` with exactly
    /// one `result` (a `snapshot` request also gets its `snapshot` there first); broadcasts go
    /// to the outbound channel.
    ///
    /// Addressed to the first open doc: the engine's tests and the offline driver. (The OSC
    /// listeners send [`Inbox::Conn`], to each doc, since M4.15h.)
    Client {
        message: ClientMessage,
        reply: ReplyTx,
    },
    /// A message on a WebSocket connection (M4.15f): one bound to `doc` (`hello { workspace }`),
    /// or the app connection (`None`), which may send only the app-scope messages. Answered as
    /// [`Inbox::Client`] is.
    Conn {
        doc: Option<super::DocId>,
        message: ClientMessage,
        reply: ReplyTx,
    },
    /// A probe worker's answer (internal feedback), for the doc whose catalog asked: the loop
    /// drops it if that doc has closed since.
    Probe(super::DocId, ProbeAnswer),
    /// The device thread's answer (internal feedback, docs/control.md "Devices").
    Device(super::devices::DeviceAnswer),
    /// A network worker's answer: a message that could not be sent, a host look-up done
    /// (internal feedback, docs/control.md "Network output"), for the doc that sent it (dropped
    /// if it has closed since).
    Net(super::DocId, super::netout::NetAnswer),
    /// The waveform reader's answer for doc `doc`'s read `ticket` (AUD-14): the loop starts the
    /// next read, and drops the answer if the doc has closed since.
    Waveform {
        doc: super::DocId,
        ticket: u64,
        result: Result<Arc<Waveform>, WaveformError>,
    },
    /// The document worker's answer to an open or a recovery scan (internal feedback,
    /// docs/control.md "Documents"). One read runs at a time engine-wide, so its ticket names
    /// the request, and the request the doc.
    Doc(super::docs::DocAnswer),
    /// The media watch's findings (internal feedback, docs/control.md "Media watch"). One
    /// watch serves every doc: its events name files, and the loop hands each to the docs that
    /// watch it.
    Watch(super::watch::WatchAnswer),
    /// A log entry from another thread (the network layer), sent out like the loop's own.
    Log { level: LogLevel, message: String },
    /// An input from a client or OSC peer is about to arrive: sent by the network layer just
    /// before it, so the show log shows what triggered a GO and from where (DX-02).
    Trigger { source: String, detail: String },
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

/// One broadcast and the doc it is about: `doc: None` is the engine's own (`log`, `log_entry`,
/// `status`) and goes to every connection; a doc's goes to the connections bound to it. The
/// `workspaces` list goes to the app connections whatever `doc` says.
#[derive(Clone, Debug)]
pub struct Outgoing {
    pub doc: Option<super::DocId>,
    pub message: ServerMessage,
}

impl Outgoing {
    /// An engine-wide message.
    pub fn all(message: ServerMessage) -> Self {
        Self { doc: None, message }
    }

    /// A message about doc `doc`.
    pub fn doc(doc: super::DocId, message: ServerMessage) -> Self {
        Self {
            doc: Some(doc),
            message,
        }
    }
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
pub type OutboundTx = mpsc::Sender<Outgoing>;
pub type OutboundRx = mpsc::Receiver<Outgoing>;

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

    /// A sequenced or log message, about `doc` (`None`: the engine's).
    pub fn send(&mut self, doc: Option<super::DocId>, message: ServerMessage) {
        if self.offer(Outgoing { doc, message }).is_err() {
            self.dropped += 1;
        }
    }

    /// Meters and progress: dropped while the channel is half full, to keep room for the rest.
    pub fn send_lossy(&mut self, doc: Option<super::DocId>, message: ServerMessage) {
        if self.try_lossy(doc, message).is_err() {
            self.lossy_dropped += 1;
        }
    }

    /// The rule of [`Outbound::send_lossy`], handing a dropped message back and not counting
    /// it: for `log_entry`, whose losses the client sees in its `seq` and the caller offers
    /// again. (Counting them would make the "not keeping up" notice, itself a pushed record,
    /// report its own loss.)
    pub fn try_lossy(
        &mut self,
        doc: Option<super::DocId>,
        message: ServerMessage,
    ) -> Result<(), ServerMessage> {
        let max = self.tx.max_capacity();
        let queued = max.saturating_sub(self.tx.capacity());
        if queued * 2 >= max {
            return Err(message);
        }
        match self.tx.try_send(Outgoing { doc, message }) {
            Ok(()) | Err(TrySendError::Closed(_)) => Ok(()),
            Err(TrySendError::Full(m)) => Err(m.message),
        }
    }

    /// A state message about `doc`; `false` if it did not fit and must be sent again later.
    pub fn send_state(&mut self, doc: Option<super::DocId>, message: ServerMessage) -> bool {
        self.offer(Outgoing { doc, message }).is_ok()
    }

    /// Whether a message would fit now.
    pub fn has_room(&self) -> bool {
        self.tx.capacity() > 0
    }

    fn offer(&mut self, message: Outgoing) -> Result<(), ()> {
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
