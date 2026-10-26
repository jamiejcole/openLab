// SPDX-License-Identifier: GPL-3.0-or-later

//! Fan-out of the control thread's broadcasts to the connected clients (docs/network.md).
//!
//! One distributor thread reads the outbound channel, encodes each message once and offers it to
//! every client's own bounded queue with `try_send`: nothing here blocks, so a stalled client
//! can never hold up the control loop (which only ever touches the outbound channel) or another
//! client.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError};
use openlab_core::protocol::{MAX_SERVER_FRAME_BYTES, ServerMessage, encode_server};
use tokio::sync::mpsc;

/// Broadcasts that can wait in one client's queue. Meters and progress are dropped for the
/// client once it is half full.
pub const CLIENT_QUEUE: usize = 1024;

/// How a connection treats a broadcast.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `model_changed`: stale ones (at or below the client's revision) are skipped.
    Revision(u64),
    /// `runtime`: stale ones are skipped.
    Seq(u64),
    /// `document`, `warnings`, `log`: always sent; the latest is the truth.
    State,
    /// `meters`, `progress`: dropped when the client is behind.
    Lossy,
}

#[derive(Clone, Debug)]
pub struct Broadcast {
    pub kind: Kind,
    pub text: Arc<str>,
}

struct Slot {
    id: u64,
    tx: mpsc::Sender<Broadcast>,
    /// Set when a state message did not fit the queue or could not be encoded: the connection
    /// then throws its queue away and sends a fresh `snapshot` (it clears the flag before
    /// asking, so nothing is lost in between).
    lagged: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct Hub {
    slots: Mutex<Vec<Slot>>,
    next: AtomicU64,
}

pub struct Registration {
    pub id: u64,
    pub rx: mpsc::Receiver<Broadcast>,
    pub lagged: Arc<AtomicBool>,
}

impl Hub {
    pub fn register(&self) -> Registration {
        let (tx, rx) = mpsc::channel(CLIENT_QUEUE);
        let lagged = Arc::new(AtomicBool::new(false));
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.lock().push(Slot {
            id,
            tx,
            lagged: lagged.clone(),
        });
        Registration { id, rx, lagged }
    }

    pub fn unregister(&self, id: u64) {
        self.lock().retain(|s| s.id != id);
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Slot>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Offers one message to every client.
    pub fn deliver(&self, message: &ServerMessage) {
        let kind = match message {
            ServerMessage::ModelChanged { revision, .. } => Kind::Revision(*revision),
            ServerMessage::Runtime { seq, .. } => Kind::Seq(*seq),
            ServerMessage::Progress { .. } | ServerMessage::Meters { .. } => Kind::Lossy,
            _ => Kind::State,
        };
        let slots = self.lock();
        let Some(text) = encode(message) else {
            // Too big for the wire (or unencodable): the clients cannot get this one, so each
            // takes a fresh snapshot instead of being left with a silent hole. A lossy message
            // is just skipped.
            if kind != Kind::Lossy {
                for s in slots.iter() {
                    s.lagged.store(true, Ordering::Release);
                }
            }
            return;
        };
        let item = Broadcast { kind, text };
        for s in slots.iter() {
            if s.lagged.load(Ordering::Acquire) {
                continue;
            }
            if kind == Kind::Lossy {
                if s.tx.capacity() * 2 > s.tx.max_capacity() {
                    let _ = s.tx.try_send(item.clone());
                }
            } else if let Err(mpsc::error::TrySendError::Full(_)) = s.tx.try_send(item.clone()) {
                s.lagged.store(true, Ordering::Release);
            }
        }
    }
}

fn encode(message: &ServerMessage) -> Option<Arc<str>> {
    let text = encode_server(message).ok()?;
    (text.len() <= MAX_SERVER_FRAME_BYTES).then(|| Arc::from(text))
}

/// The distributor loop: runs until the control side hangs up or `stop` is set.
pub fn distribute(hub: &Hub, outbound: &Receiver<ServerMessage>, stop: &AtomicBool) {
    while !stop.load(Ordering::Acquire) {
        match outbound.recv_timeout(Duration::from_millis(100)) {
            Ok(message) => hub.deliver(&message),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}
