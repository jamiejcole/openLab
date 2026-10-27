// SPDX-License-Identifier: GPL-3.0-or-later

//! Fan-out of the control thread's broadcasts to the connected clients (docs/network.md).
//!
//! One task on the network runtime reads the outbound channel, encodes each message once and
//! offers it to every client's own bounded queue with `try_send`: nothing here waits for a
//! client, so a stalled client can never hold up the control loop (which only ever does
//! `try_send` on the outbound channel) or another client.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use openlab_core::protocol::{MAX_SERVER_FRAME_BYTES, ServerMessage, encode_server};
use tokio::sync::{Notify, mpsc};

use crate::control::OutboundRx;

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
    /// `snapshot` (a workspace was opened, created or reverted): sent unless the client already
    /// holds that revision and seq (the requester got it as its reply).
    Snapshot { revision: u64, seq: u64 },
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
    /// Wakes the connection when it lags (it may be idle in `select!`).
    wake: Arc<Notify>,
}

impl Slot {
    fn mark_lagged(&self) {
        self.lagged.store(true, Ordering::Release);
        self.wake.notify_one();
    }
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
    pub wake: Arc<Notify>,
}

impl Hub {
    pub fn register(&self) -> Registration {
        let (tx, rx) = mpsc::channel(CLIENT_QUEUE);
        let lagged = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(Notify::new());
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.lock().push(Slot {
            id,
            tx,
            lagged: lagged.clone(),
            wake: wake.clone(),
        });
        Registration {
            id,
            rx,
            lagged,
            wake,
        }
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
            ServerMessage::Snapshot { snapshot } => Kind::Snapshot {
                revision: snapshot.revision,
                seq: snapshot.runtime_seq,
            },
            ServerMessage::Progress { .. } | ServerMessage::Meters { .. } => Kind::Lossy,
            _ => Kind::State,
        };
        // Encoded before the lock: a multi-MiB message must not hold up `register`/`unregister`.
        let encoded = encode(message);
        let slots = self.lock();
        let Some(text) = encoded else {
            // Too big for the wire (or unencodable): the clients cannot get this one, so each
            // takes a fresh snapshot instead of being left with a silent hole. A lossy message
            // is just skipped.
            if kind != Kind::Lossy {
                for s in slots.iter() {
                    s.mark_lagged();
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
                s.mark_lagged();
            }
        }
    }
}

fn encode(message: &ServerMessage) -> Option<Arc<str>> {
    let text = encode_server(message).ok()?;
    (text.len() <= MAX_SERVER_FRAME_BYTES).then(|| Arc::from(text))
}

/// A second consumer of the control thread's broadcasts: the OSC side, which keeps a copy of
/// the workspace from them. Only the messages that change it are forwarded.
pub struct Tap {
    pub tx: mpsc::Sender<ServerMessage>,
    /// Set when a message did not fit: the copy is behind and must be rebuilt from a snapshot.
    pub lagged: Arc<AtomicBool>,
    /// Wakes the consumer so it notices `lagged` while idle.
    pub wake: Arc<Notify>,
}

impl Tap {
    fn offer(&self, message: &ServerMessage) {
        if !matches!(
            message,
            ServerMessage::ModelChanged { .. }
                | ServerMessage::Runtime { .. }
                | ServerMessage::Document { .. }
                // A workspace was opened, created or reverted: nothing else announces it.
                | ServerMessage::Snapshot { .. }
        ) {
            return;
        }
        if self.lagged.load(Ordering::Acquire) {
            return;
        }
        if self.tx.try_send(message.clone()).is_err() {
            self.lagged.store(true, Ordering::Release);
            self.wake.notify_one();
        }
    }
}

/// The fan-out task: runs until the control side hangs up (or the runtime stops).
pub async fn distribute(hub: Arc<Hub>, mut outbound: OutboundRx, tap: Option<Tap>) {
    while let Some(message) = outbound.recv().await {
        if let Some(tap) = &tap {
            tap.offer(&message);
        }
        hub.deliver(&message);
    }
}

#[cfg(test)]
mod tests {
    use openlab_core::protocol::LogLevel;

    use super::*;

    fn log(n: usize) -> ServerMessage {
        ServerMessage::Log {
            level: LogLevel::Info,
            message: format!("{n}"),
        }
    }

    fn meters() -> ServerMessage {
        ServerMessage::Meters {
            outputs: vec![],
            cues: vec![],
        }
    }

    fn drain(rx: &mut mpsc::Receiver<Broadcast>) -> Vec<Kind> {
        std::iter::from_fn(|| rx.try_recv().ok().map(|b| b.kind)).collect()
    }

    #[test]
    fn meters_and_progress_are_dropped_once_a_queue_is_half_full() {
        let hub = Hub::default();
        let mut r = hub.register();
        // One under half: the meters still go in, and fill it to half; the progress after them
        // does not.
        for n in 0..CLIENT_QUEUE / 2 - 1 {
            hub.deliver(&log(n));
        }
        hub.deliver(&meters());
        hub.deliver(&ServerMessage::Progress { items: vec![] });
        let kinds = drain(&mut r.rx);
        assert_eq!(kinds.len(), CLIENT_QUEUE / 2);
        assert_eq!(kinds.iter().filter(|k| **k == Kind::Lossy).count(), 1);
        // Exactly half full: dropped, while state messages still go in.
        for n in 0..CLIENT_QUEUE / 2 {
            hub.deliver(&log(n));
        }
        hub.deliver(&meters());
        hub.deliver(&ServerMessage::Progress { items: vec![] });
        hub.deliver(&log(0));
        let kinds = drain(&mut r.rx);
        assert_eq!(kinds.len(), CLIENT_QUEUE / 2 + 1);
        assert!(kinds.iter().all(|k| *k == Kind::State));
        assert!(!r.lagged.load(Ordering::Acquire));
    }

    #[test]
    fn a_state_message_that_does_not_fit_marks_only_that_client_lagged() {
        let hub = Hub::default();
        let mut slow = hub.register();
        let mut fast = hub.register();
        for n in 0..CLIENT_QUEUE {
            hub.deliver(&log(n));
            // The fast client keeps up.
            assert_eq!(drain(&mut fast.rx).len(), 1);
        }
        assert!(!slow.lagged.load(Ordering::Acquire));
        hub.deliver(&ServerMessage::Runtime {
            seq: 1,
            changes: vec![],
        });
        assert!(slow.lagged.load(Ordering::Acquire));
        assert!(!fast.lagged.load(Ordering::Acquire));
        assert_eq!(drain(&mut fast.rx), vec![Kind::Seq(1)]);
        // A lagged client is skipped until it resyncs (it clears the flag).
        assert_eq!(drain(&mut slow.rx).len(), CLIENT_QUEUE);
        hub.deliver(&log(0));
        assert!(drain(&mut slow.rx).is_empty());
        slow.lagged.store(false, Ordering::Release);
        hub.deliver(&log(1));
        assert_eq!(drain(&mut slow.rx), vec![Kind::State]);
    }

    #[test]
    fn a_snapshot_is_delivered_with_its_revision_and_seq() {
        use openlab_core::protocol::{DocumentState, Snapshot};
        use openlab_core::{CueListId, PatchId, Workspace, WorkspaceId};
        let hub = Hub::default();
        let mut r = hub.register();
        let workspace = Workspace::new_empty(
            WorkspaceId(uuid::Uuid::from_u128(1)),
            CueListId(uuid::Uuid::from_u128(2)),
            PatchId(uuid::Uuid::from_u128(3)),
        );
        hub.deliver(&ServerMessage::Snapshot {
            snapshot: Box::new(Snapshot {
                revision: 9,
                runtime_seq: 4,
                document: DocumentState {
                    path: None,
                    dirty: false,
                    can_undo: false,
                    can_redo: false,
                },
                workspace,
                cue_states: vec![],
                problems: vec![],
                group_warnings: vec![],
                playheads: vec![],
                previewing: vec![],
                warnings: vec![],
            }),
        });
        assert_eq!(
            drain(&mut r.rx),
            vec![Kind::Snapshot {
                revision: 9,
                seq: 4
            }]
        );
    }

    #[test]
    fn an_unregistered_client_gets_nothing_more() {
        let hub = Hub::default();
        let mut r = hub.register();
        hub.unregister(r.id);
        hub.deliver(&log(0));
        assert!(r.rx.try_recv().is_err());
    }
}

#[cfg(test)]
mod tap_tests {
    use futures_util::FutureExt;
    use openlab_core::protocol::{DocumentState, LogLevel};

    use super::*;

    fn tap(capacity: usize) -> (Tap, mpsc::Receiver<ServerMessage>) {
        let (tx, rx) = mpsc::channel(capacity);
        (
            Tap {
                tx,
                lagged: Arc::new(AtomicBool::new(false)),
                wake: Arc::new(Notify::new()),
            },
            rx,
        )
    }

    fn replacement_snapshot() -> openlab_core::protocol::Snapshot {
        use openlab_core::{CueListId, PatchId, Workspace, WorkspaceId};
        openlab_core::protocol::Snapshot {
            revision: 9,
            runtime_seq: 4,
            document: DocumentState {
                path: None,
                dirty: false,
                can_undo: false,
                can_redo: false,
            },
            workspace: Workspace::new_empty(
                WorkspaceId(uuid::Uuid::from_u128(1)),
                CueListId(uuid::Uuid::from_u128(2)),
                PatchId(uuid::Uuid::from_u128(3)),
            ),
            cue_states: vec![],
            problems: vec![],
            group_warnings: vec![],
            playheads: vec![],
            previewing: vec![],
            warnings: vec![],
        }
    }

    fn runtime(seq: u64) -> ServerMessage {
        ServerMessage::Runtime {
            seq,
            changes: vec![],
        }
    }

    #[test]
    fn only_the_messages_that_change_the_workspace_are_forwarded() {
        let (tap, mut rx) = tap(8);
        tap.offer(&runtime(1));
        tap.offer(&ServerMessage::ModelChanged {
            revision: 1,
            upserts: vec![],
            deletes: vec![],
        });
        tap.offer(&ServerMessage::Document {
            document: DocumentState {
                path: None,
                dirty: false,
                can_undo: false,
                can_redo: false,
            },
        });
        tap.offer(&ServerMessage::Log {
            level: LogLevel::Info,
            message: String::new(),
        });
        tap.offer(&ServerMessage::Meters {
            outputs: vec![],
            cues: vec![],
        });
        tap.offer(&ServerMessage::Progress { items: vec![] });
        // A workspace replacement is announced by its snapshot alone.
        tap.offer(&ServerMessage::Snapshot {
            snapshot: Box::new(replacement_snapshot()),
        });
        assert_eq!(std::iter::from_fn(|| rx.try_recv().ok()).count(), 4);
        assert!(!tap.lagged.load(Ordering::Acquire));
    }

    #[test]
    fn a_full_queue_marks_the_tap_lagged_wakes_the_consumer_and_stops_copying() {
        let (tap, mut rx) = tap(2);
        tap.offer(&runtime(1));
        tap.offer(&runtime(2));
        assert!(!tap.lagged.load(Ordering::Acquire));
        tap.offer(&runtime(3));
        assert!(tap.lagged.load(Ordering::Acquire));
        // The wake is stored: a consumer that was idle sees it.
        assert!(tap.wake.notified().now_or_never().is_some());
        // Lagged: nothing more is queued, even with room, until the consumer clears the flag.
        while rx.try_recv().is_ok() {}
        tap.offer(&runtime(4));
        assert!(rx.try_recv().is_err());
        tap.lagged.store(false, Ordering::Release);
        tap.offer(&runtime(5));
        assert!(rx.try_recv().is_ok());
    }
}
