// SPDX-License-Identifier: GPL-3.0-or-later

//! Fan-out of the control thread's broadcasts to the connected clients (docs/network.md).
//!
//! One task on the network runtime reads the outbound channel, encodes each message once and
//! offers it to the bounded queue of every client it is for with `try_send`: nothing here waits
//! for a client, so a stalled client can never hold up the control loop (which only ever does
//! `try_send` on the outbound channel) or another client. A message about a doc goes to the
//! connections bound to that doc, the engine's own (`log`, `status`, ...) to every connection,
//! and the `workspaces` list to the app connections (M4.15f).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use openlab_core::protocol::{DocId, MAX_SERVER_FRAME_BYTES, ServerMessage, encode_server};
use tokio::sync::{Notify, mpsc};

use crate::control::{OutboundRx, Outgoing};

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
    /// `snapshot` (the doc's model changed in a way no `model_changed` says: a save-as to
    /// another folder re-spelled its media references): sent unless the client already holds
    /// that revision and seq.
    Snapshot { revision: u64, seq: u64 },
    /// `closed`: the connection writes it and closes.
    Closed,
    /// `document`, `warnings`, `log`, `workspaces`: always sent; the latest is the truth.
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
    /// The doc the connection is bound to; `None`: an app connection.
    doc: Option<DocId>,
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
    /// A connection bound to `doc` (`None`: an app connection).
    pub fn register(&self, doc: Option<DocId>) -> Registration {
        let (tx, rx) = mpsc::channel(CLIENT_QUEUE);
        let lagged = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(Notify::new());
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.lock().push(Slot {
            id,
            doc,
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

    /// App connections (bound to no doc) registered now.
    pub fn app_connections(&self) -> usize {
        self.lock().iter().filter(|s| s.doc.is_none()).count()
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Slot>> {
        crate::lock(&self.slots)
    }

    /// Offers one message to every client it is for.
    pub fn deliver(&self, out: &Outgoing) {
        let message = &out.message;
        let app_only = matches!(message, ServerMessage::Workspaces { .. });
        let wants = |s: &Slot| {
            if app_only {
                s.doc.is_none()
            } else {
                out.doc.is_none_or(|d| s.doc == Some(d))
            }
        };
        let kind = match message {
            ServerMessage::ModelChanged { revision, .. } => Kind::Revision(*revision),
            ServerMessage::Runtime { seq, .. } => Kind::Seq(*seq),
            ServerMessage::Snapshot { snapshot } => Kind::Snapshot {
                revision: snapshot.revision,
                seq: snapshot.runtime_seq,
            },
            ServerMessage::Closed { .. } => Kind::Closed,
            // A `log` notice is also a show-log record (`log_tail` has it): a burst of them must
            // not mark every client lagged and cost a snapshot each.
            ServerMessage::Progress { .. }
            | ServerMessage::Meters { .. }
            | ServerMessage::Log { .. }
            | ServerMessage::LogEntry { .. }
            | ServerMessage::Status { .. } => Kind::Lossy,
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
                for s in slots.iter().filter(|s| wants(s)) {
                    s.mark_lagged();
                }
            }
            return;
        };
        let item = Broadcast { kind, text };
        for s in slots.iter().filter(|s| wants(s)) {
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
/// every open doc's workspace from them (ARCHITECTURE section 7, "the OSC task sees all"). Only
/// the messages that change a copy are forwarded, with their doc: each doc's `model_changed`,
/// `runtime`, `document`, `snapshot` and `closed`, and the `workspaces` list (which docs are
/// open, in open order).
pub struct Tap {
    pub tx: mpsc::Sender<Outgoing>,
    /// Set when a message did not fit: the copies are behind and must be rebuilt.
    pub lagged: Arc<AtomicBool>,
    /// Wakes the consumer so it notices `lagged` while idle.
    pub wake: Arc<Notify>,
}

impl Tap {
    fn mark_lagged(&self) {
        self.lagged.store(true, Ordering::Release);
        self.wake.notify_one();
    }

    fn offer(&self, out: &Outgoing) {
        let wanted = match (out.doc, &out.message) {
            (None, ServerMessage::Workspaces { .. }) => true,
            (
                Some(_),
                ServerMessage::ModelChanged { .. }
                | ServerMessage::Runtime { .. }
                | ServerMessage::Document { .. }
                // Its media references were re-spelled (a save-as to another folder).
                | ServerMessage::Snapshot { .. }
                | ServerMessage::Closed { .. },
            ) => true,
            _ => false,
        };
        if !wanted || self.lagged.load(Ordering::Acquire) {
            return;
        }
        if self.tx.try_send(out.clone()).is_err() {
            self.mark_lagged();
        }
    }
}

/// The fan-out task: runs until the control side hangs up (or the runtime stops).
pub async fn distribute(hub: Arc<Hub>, mut outbound: OutboundRx, tap: Option<Tap>) {
    while let Some(out) = outbound.recv().await {
        if let Some(tap) = &tap {
            tap.offer(&out);
        }
        hub.deliver(&out);
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

    /// A state message: one that must reach the client or mark it lagged.
    fn state(n: usize) -> ServerMessage {
        ServerMessage::Error {
            code: openlab_core::protocol::ErrorCode::Internal,
            message: format!("{n}"),
            protocol_version: 1,
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
        let mut r = hub.register(None);
        // One under half: the meters still go in, and fill it to half; the progress after them
        // does not.
        for n in 0..CLIENT_QUEUE / 2 - 1 {
            hub.deliver(&Outgoing::all(state(n)));
        }
        hub.deliver(&Outgoing::all(meters()));
        hub.deliver(&Outgoing::all(ServerMessage::Progress {
            items: vec![],
            waits: vec![],
        }));
        let kinds = drain(&mut r.rx);
        assert_eq!(kinds.len(), CLIENT_QUEUE / 2);
        assert_eq!(kinds.iter().filter(|k| **k == Kind::Lossy).count(), 1);
        // Exactly half full: dropped, while state messages still go in.
        for n in 0..CLIENT_QUEUE / 2 {
            hub.deliver(&Outgoing::all(state(n)));
        }
        hub.deliver(&Outgoing::all(meters()));
        hub.deliver(&Outgoing::all(ServerMessage::Progress {
            items: vec![],
            waits: vec![],
        }));
        hub.deliver(&Outgoing::all(state(0)));
        let kinds = drain(&mut r.rx);
        assert_eq!(kinds.len(), CLIENT_QUEUE / 2 + 1);
        assert!(kinds.iter().all(|k| *k == Kind::State));
        assert!(!r.lagged.load(Ordering::Acquire));
    }

    #[test]
    fn log_entries_are_lossy_like_meters() {
        use openlab_core::protocol::{LogEntry, LogKind};
        let hub = Hub::default();
        let mut r = hub.register(None);
        for n in 0..CLIENT_QUEUE / 2 {
            hub.deliver(&Outgoing::all(state(n)));
        }
        hub.deliver(&Outgoing::all(ServerMessage::LogEntry {
            seq: 1,
            entry: LogEntry {
                time_ms: 0,
                show_time: None,
                level: LogLevel::Info,
                kind: LogKind::Engine,
                message: "x".into(),
                cue: None,
                source: None,
            },
        }));
        let kinds = drain(&mut r.rx);
        assert_eq!(kinds.len(), CLIENT_QUEUE / 2, "dropped at half full");
        assert!(
            !r.lagged.load(Ordering::Acquire),
            "a lost push is not a lag"
        );
    }

    /// A burst of `log` notices fills no queue past half and marks no client lagged.
    #[test]
    fn log_notices_are_lossy_and_never_mark_a_client_lagged() {
        let hub = Hub::default();
        let mut r = hub.register(None);
        for n in 0..CLIENT_QUEUE * 2 {
            hub.deliver(&Outgoing::all(log(n)));
        }
        assert!(!r.lagged.load(Ordering::Acquire));
        let kinds = drain(&mut r.rx);
        assert_eq!(kinds.len(), CLIENT_QUEUE / 2);
        assert!(kinds.iter().all(|k| *k == Kind::Lossy));
    }

    #[test]
    fn a_state_message_that_does_not_fit_marks_only_that_client_lagged() {
        let hub = Hub::default();
        let mut slow = hub.register(None);
        let mut fast = hub.register(None);
        for n in 0..CLIENT_QUEUE {
            hub.deliver(&Outgoing::all(state(n)));
            // The fast client keeps up.
            assert_eq!(drain(&mut fast.rx).len(), 1);
        }
        assert!(!slow.lagged.load(Ordering::Acquire));
        hub.deliver(&Outgoing::all(ServerMessage::Runtime {
            seq: 1,
            changes: vec![],
        }));
        assert!(slow.lagged.load(Ordering::Acquire));
        assert!(!fast.lagged.load(Ordering::Acquire));
        assert_eq!(drain(&mut fast.rx), vec![Kind::Seq(1)]);
        // A lagged client is skipped until it resyncs (it clears the flag).
        assert_eq!(drain(&mut slow.rx).len(), CLIENT_QUEUE);
        hub.deliver(&Outgoing::all(state(0)));
        assert!(drain(&mut slow.rx).is_empty());
        slow.lagged.store(false, Ordering::Release);
        hub.deliver(&Outgoing::all(state(1)));
        assert_eq!(drain(&mut slow.rx), vec![Kind::State]);
    }

    #[test]
    fn a_snapshot_is_delivered_with_its_revision_and_seq() {
        use openlab_core::protocol::{DocumentState, Snapshot};
        use openlab_core::{CueListId, PatchId, Workspace, WorkspaceId};
        let hub = Hub::default();
        let mut r = hub.register(None);
        let workspace = Workspace::new_empty(
            WorkspaceId(uuid::Uuid::from_u128(1)),
            CueListId(uuid::Uuid::from_u128(2)),
            PatchId(uuid::Uuid::from_u128(3)),
        );
        hub.deliver(&Outgoing::all(ServerMessage::Snapshot {
            snapshot: Box::new(Snapshot {
                revision: 9,
                runtime_seq: 4,
                document: DocumentState {
                    path: None,
                    dirty: false,
                    can_undo: false,
                    can_redo: false,
                    show_mode: false,
                },
                workspace,
                cue_states: vec![],
                problems: vec![],
                group_warnings: vec![],
                playheads: vec![],
                previewing: vec![],
                warnings: vec![],
                media_durations: vec![],
            }),
        }));
        assert_eq!(
            drain(&mut r.rx),
            vec![Kind::Snapshot {
                revision: 9,
                seq: 4
            }]
        );
    }

    /// What the relaunch checks after its delay (M4.9): is an app connection back?
    #[test]
    fn app_connections_are_counted_apart_from_bound_ones() {
        let hub = Hub::default();
        let bound = hub.register(Some(DocId(0)));
        assert_eq!(hub.app_connections(), 0);
        let app = hub.register(None);
        assert_eq!(hub.app_connections(), 1);
        hub.unregister(app.id);
        hub.unregister(bound.id);
        assert_eq!(hub.app_connections(), 0);
    }

    #[test]
    fn a_doc_s_messages_reach_its_connections_only_and_the_list_the_app_connections() {
        use openlab_core::protocol::WorkspaceInfo;
        let hub = Hub::default();
        let mut a = hub.register(Some(DocId(0)));
        let mut b = hub.register(Some(DocId(1)));
        let mut app = hub.register(None);
        let runtime = ServerMessage::Runtime {
            seq: 1,
            changes: vec![],
        };
        hub.deliver(&Outgoing::doc(DocId(1), runtime));
        hub.deliver(&Outgoing::all(state(0)));
        hub.deliver(&Outgoing::all(ServerMessage::Workspaces {
            workspaces: vec![WorkspaceInfo {
                doc: DocId(0),
                workspace_id: openlab_core::WorkspaceId(uuid::Uuid::from_u128(1)),
                name: "A".into(),
                path: None,
                dirty: false,
                path_hash: None,
            }],
        }));
        hub.deliver(&Outgoing::doc(
            DocId(0),
            ServerMessage::Closed { doc: DocId(0) },
        ));
        assert_eq!(drain(&mut a.rx), vec![Kind::State, Kind::Closed]);
        assert_eq!(drain(&mut b.rx), vec![Kind::Seq(1), Kind::State]);
        assert_eq!(drain(&mut app.rx), vec![Kind::State, Kind::State]);
    }

    #[test]
    fn an_unregistered_client_gets_nothing_more() {
        let hub = Hub::default();
        let mut r = hub.register(None);
        hub.unregister(r.id);
        hub.deliver(&Outgoing::all(state(0)));
        assert!(r.rx.try_recv().is_err());
    }
}

#[cfg(test)]
mod tap_tests {
    use futures_util::FutureExt;
    use openlab_core::protocol::{DocumentState, LogLevel};

    use super::*;

    fn tap(capacity: usize) -> (Tap, mpsc::Receiver<Outgoing>) {
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
                show_mode: false,
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
            media_durations: vec![],
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
        tap.offer(&Outgoing::doc(DocId(0), runtime(1)));
        tap.offer(&Outgoing::doc(
            DocId(0),
            ServerMessage::ModelChanged {
                revision: 1,
                upserts: vec![],
                deletes: vec![],
            },
        ));
        tap.offer(&Outgoing::doc(
            DocId(0),
            ServerMessage::Document {
                document: DocumentState {
                    path: None,
                    dirty: false,
                    can_undo: false,
                    can_redo: false,
                    show_mode: false,
                },
            },
        ));
        tap.offer(&Outgoing::doc(
            DocId(0),
            ServerMessage::Log {
                level: LogLevel::Info,
                message: String::new(),
            },
        ));
        tap.offer(&Outgoing::doc(
            DocId(0),
            ServerMessage::Meters {
                outputs: vec![],
                cues: vec![],
            },
        ));
        tap.offer(&Outgoing::doc(
            DocId(0),
            ServerMessage::Progress {
                items: vec![],
                waits: vec![],
            },
        ));
        // A workspace replacement is announced by its snapshot alone.
        tap.offer(&Outgoing::doc(
            DocId(0),
            ServerMessage::Snapshot {
                snapshot: Box::new(replacement_snapshot()),
            },
        ));
        assert_eq!(std::iter::from_fn(|| rx.try_recv().ok()).count(), 4);
        assert!(!tap.lagged.load(Ordering::Acquire));
    }

    #[test]
    fn the_tap_forwards_every_doc_and_the_list_with_their_doc() {
        use openlab_core::protocol::WorkspaceInfo;
        let (tap, mut rx) = tap(8);
        tap.offer(&Outgoing::doc(DocId(1), runtime(1)));
        tap.offer(&Outgoing::doc(DocId(0), runtime(1)));
        tap.offer(&Outgoing::doc(
            DocId(1),
            ServerMessage::Closed { doc: DocId(1) },
        ));
        tap.offer(&Outgoing::all(ServerMessage::Workspaces {
            workspaces: vec![WorkspaceInfo {
                doc: DocId(0),
                workspace_id: openlab_core::WorkspaceId(uuid::Uuid::from_u128(1)),
                name: "A".into(),
                path: None,
                dirty: false,
                path_hash: None,
            }],
        }));
        // An engine-wide message that is not the list changes no copy.
        tap.offer(&Outgoing::all(runtime(9)));
        let docs: Vec<Option<DocId>> =
            std::iter::from_fn(|| rx.try_recv().ok().map(|o| o.doc)).collect();
        assert_eq!(
            docs,
            vec![Some(DocId(1)), Some(DocId(0)), Some(DocId(1)), None]
        );
    }

    #[test]
    fn a_full_queue_marks_the_tap_lagged_wakes_the_consumer_and_stops_copying() {
        let (tap, mut rx) = tap(2);
        tap.offer(&Outgoing::doc(DocId(0), runtime(1)));
        tap.offer(&Outgoing::doc(DocId(0), runtime(2)));
        assert!(!tap.lagged.load(Ordering::Acquire));
        tap.offer(&Outgoing::doc(DocId(0), runtime(3)));
        assert!(tap.lagged.load(Ordering::Acquire));
        // The wake is stored: a consumer that was idle sees it.
        assert!(tap.wake.notified().now_or_never().is_some());
        // Lagged: nothing more is queued, even with room, until the consumer clears the flag.
        while rx.try_recv().is_ok() {}
        tap.offer(&Outgoing::doc(DocId(0), runtime(4)));
        assert!(rx.try_recv().is_err());
        tap.lagged.store(false, Ordering::Release);
        tap.offer(&Outgoing::doc(DocId(0), runtime(5)));
        assert!(rx.try_recv().is_ok());
    }
}
