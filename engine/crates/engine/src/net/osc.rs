// SPDX-License-Identifier: GPL-3.0-or-later

//! The OSC listeners (docs/osc.md, ARCHITECTURE section 7): UDP and TCP sockets on the network
//! runtime, feeding the socket-free [`crate::osc::Osc`] layer.
//!
//! One **actor task** owns everything stateful: the `Osc` sessions, the copy of the workspace
//! ([`Mirror`]) and the table of TCP connections. The sockets' tasks only move bytes to it
//! through bounded queues, so a slow or hostile peer is held back (TCP) or dropped (UDP) at its
//! own socket and never delays another peer, the actor or the control thread. The actor reaches
//! the control thread only through the control inbox (an unbounded send, never blocking) and
//! hears back on its own reply channel and the hub's broadcasts ([`super::hub::Tap`]); it never
//! asks the control thread a question it must wait for.

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener as StdListener, UdpSocket as StdUdp};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crossbeam_channel::Sender;
use openlab_core::protocol::{ClientMessage, LogLevel, Outcome, Request, RequestId, ServerMessage};
use rosc::{OscMessage, OscType};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{Notify, Semaphore, mpsc};
use tokio::time::{Instant, MissedTickBehavior, interval_at, sleep_until, timeout};

use super::hub::Tap;
use super::osc_mirror::{Applied, Mirror, MirrorShow, Update};
use super::refusals::Refusals;
use crate::control::{Inbox, ReplyRx, ReplyTx, reply_channel};
use crate::osc::{
    MAX_PACKET_BYTES, OSC_PORT, OSC_REPLY_PORT, Osc, OscInput, OscStats, Outcome as OscOutcome,
    Peer, SlipDecoder, encode_message, slip_encode,
};

/// OSC request ids start here, well away from the small numbers clients count up from, and
/// never reach `u64::MAX` (the WebSocket server's own id). The control loop answers each
/// request on the channel it came from, so ids could not clash with a WebSocket client's anyway;
/// the range only keeps logs readable.
const ID_BASE: u64 = 1 << 63;
/// Requests to the control loop that have not been answered. Past it new inputs are dropped
/// and counted: the actor never blocks, so this is what keeps a UDP flood from piling up in the
/// control inbox.
const MAX_INFLIGHT: usize = 256;
/// TCP events (data, open, close) waiting for the actor; each connection waits in its own read
/// loop beyond this, which holds that sender back through TCP.
const EVENT_QUEUE: usize = 256;
/// Datagrams waiting for the actor. A queue of their own, and the actor takes from the two
/// queues in turn: a TCP peer that floods (it waits, holding its place in line) must not make
/// the datagrams of a console find the queue full.
const UDP_QUEUE: usize = 256;
/// How long an accepted connection waits for the actor to close an idle one when the table is
/// full, before it is refused.
const EVICT_WAIT: Duration = Duration::from_millis(500);
/// Broadcasts waiting for the actor; more marks the copy lagged and it is rebuilt.
const TAP_QUEUE: usize = 1024;
/// Messages waiting to be written to one TCP connection.
const TCP_WRITE_QUEUE: usize = 256;
/// Bytes read from a TCP connection at a time.
const READ_CHUNK: usize = 4096;

/// Where and how the OSC server listens. The defaults are QLab's ports on every interface;
/// tests use port 0 and the loopback address.
#[derive(Clone, Debug)]
pub struct OscConfig {
    /// The local address for both sockets. QLab listens on every interface, so remotes on the
    /// network reach it: set a passcode on a network you do not control.
    pub bind: IpAddr,
    /// UDP and TCP listening port (0: the OS picks one for each).
    pub port: u16,
    /// The UDP port replies and updates go to, on the sender's IP address.
    pub reply_port: u16,
    /// Open TCP connections. Each costs a task, a decoder and a write queue; 16 is the
    /// WebSocket server's number and far above the handful of consoles, Companion and scripts
    /// of a show. More are closed at accept, with a (rate-limited) log entry.
    pub max_tcp_connections: usize,
    /// One write to a TCP peer that takes longer than this closes the connection.
    pub write_timeout: Duration,
    /// A TCP connection that has sent nothing for this long, and has not asked for updates, may
    /// be closed to make room when the table is full (a peer that vanished without a FIN or
    /// RST would otherwise hold its slot for good). Connections that are subscribed to
    /// updates are never closed for this.
    pub tcp_idle_evict: Duration,
    /// Input (not replies) that waited longer than this for the actor, because the copy of the
    /// workspace was not ready or the actor was behind, is dropped and counted: a console
    /// mashing GO while the engine starts must not have every press replayed at once.
    pub max_input_age: Duration,
    /// Refused connections are logged at most this often, with a count.
    pub refusal_log_interval: Duration,
    /// Dropped or malformed input is logged (once, with counts) this often, if there was any.
    pub stats_log_interval: Duration,
}

impl Default for OscConfig {
    fn default() -> Self {
        Self {
            bind: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            port: OSC_PORT,
            reply_port: OSC_REPLY_PORT,
            max_tcp_connections: 16,
            write_timeout: Duration::from_secs(10),
            tcp_idle_evict: Duration::from_secs(60),
            max_input_age: Duration::from_secs(2),
            refusal_log_interval: Duration::from_secs(10),
            stats_log_interval: Duration::from_secs(10),
        }
    }
}

/// The ports the OSC sockets are bound to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OscPorts {
    pub udp: u16,
    pub tcp: u16,
}

/// Counters of the listeners and the OSC layer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OscListenerStats {
    pub osc: OscStats,
    /// Datagrams dropped because the actor was behind.
    pub udp_dropped: u64,
    /// TCP connections closed at accept for want of a free slot.
    pub tcp_refused: u64,
    /// Accepted messages not sent to the control loop (too many unanswered).
    pub inputs_dropped: u64,
    /// Replies and updates not sent: a full socket buffer or write queue.
    pub send_failed: u64,
    /// Inputs the control loop answered with an error.
    pub control_refused: u64,
    /// Open TCP connections.
    pub tcp_connections: usize,
}

#[derive(Default)]
struct Shared {
    stats: Mutex<OscListenerStats>,
    udp_dropped: AtomicU64,
    tcp_refused: AtomicU64,
}

/// The running OSC server, as seen from outside.
pub struct Running {
    pub ports: OscPorts,
    stats: Arc<Shared>,
}

impl Running {
    pub fn stats(&self) -> OscListenerStats {
        let mut stats = *self
            .stats
            .stats
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        stats.udp_dropped = self.stats.udp_dropped.load(Ordering::Relaxed);
        stats.tcp_refused = self.stats.tcp_refused.load(Ordering::Relaxed);
        stats
    }
}

/// The bound, not yet running sockets.
pub struct Bound {
    udp: StdUdp,
    tcp: StdListener,
    ports: OscPorts,
}

/// Binds both sockets. Done before the tasks start so the caller knows whether OSC is up.
pub fn bind(config: &OscConfig) -> io::Result<Bound> {
    let udp = StdUdp::bind(SocketAddr::new(config.bind, config.port))?;
    let tcp = StdListener::bind(SocketAddr::new(config.bind, config.port))?;
    udp.set_nonblocking(true)?;
    tcp.set_nonblocking(true)?;
    let ports = OscPorts {
        udp: udp.local_addr()?.port(),
        tcp: tcp.local_addr()?.port(),
    };
    Ok(Bound { udp, tcp, ports })
}

/// Starts the tasks on `runtime`. `inbox` is the control inbox. The [`Tap`] goes to the hub's
/// fan-out task. `None` (and a log entry): the sockets could not be registered, OSC is off.
pub fn start(
    runtime: &tokio::runtime::Runtime,
    bound: Bound,
    config: OscConfig,
    inbox: Sender<Inbox>,
) -> Option<(Running, Tap)> {
    let shared = Arc::new(Shared::default());
    let (events_tx, events_rx) = mpsc::channel(EVENT_QUEUE);
    let (udp_tx, udp_rx) = mpsc::channel(UDP_QUEUE);
    let (tap_tx, tap_rx) = mpsc::channel(TAP_QUEUE);
    let lagged = Arc::new(AtomicBool::new(false));
    let wake = Arc::new(Notify::new());
    let tap = Tap {
        tx: tap_tx,
        lagged: lagged.clone(),
        wake: wake.clone(),
    };
    let ports = bound.ports;
    let (reply_tx, reply_rx) = reply_channel();
    let config = Arc::new(config);

    let (udp, tcp) = {
        // `from_std` needs the runtime's context.
        let _guard = runtime.enter();
        (
            UdpSocket::from_std(bound.udp),
            TcpListener::from_std(bound.tcp),
        )
    };
    let (Ok(udp), Ok(tcp)) = (udp, tcp) else {
        let _ = inbox.send(Inbox::Log {
            level: LogLevel::Warn,
            message: "OSC server could not register its sockets and is off".into(),
        });
        return None;
    };
    let udp = Arc::new(udp);

    let actor = Actor {
        osc: Osc::new(),
        mirror: None,
        shared: shared.clone(),
        config: config.clone(),
        inbox: inbox.clone(),
        udp: udp.clone(),
        tcp: HashMap::new(),
        reply_tx,
        counter: 0,
        snapshot_waits: HashSet::new(),
        inflight: HashSet::new(),
        start: Instant::now(),
        listener: OscListenerStats::default(),
        reported: Problems::default(),
    };
    runtime.spawn(actor.run(events_rx, udp_rx, tap_rx, reply_rx, lagged, wake));
    runtime.spawn(udp_loop(udp, udp_tx, shared.clone()));
    runtime.spawn(accept_loop(tcp, events_tx, config, inbox, shared.clone()));
    Some((
        Running {
            ports,
            stats: shared,
        },
        tap,
    ))
}

enum Event {
    Datagram {
        ip: IpAddr,
        bytes: Vec<u8>,
        at: Instant,
    },
    TcpOpen {
        conn: u64,
        ip: IpAddr,
        out: mpsc::Sender<Vec<u8>>,
        /// Closes the connection.
        close: Arc<Notify>,
    },
    TcpData {
        conn: u64,
        bytes: Vec<u8>,
        at: Instant,
    },
    TcpClosed {
        conn: u64,
    },
    /// The table is full and a connection is waiting: close an idle one if there is one.
    Evict,
}

/// The next event from either queue, fairly: with both ready, each is as likely to be taken.
/// `None`: a queue closed (the sockets' tasks are gone).
async fn next_input(
    udp: &mut mpsc::Receiver<Event>,
    tcp: &mut mpsc::Receiver<Event>,
) -> Option<Event> {
    tokio::select! {
        event = udp.recv() => event,
        event = tcp.recv() => event,
    }
}

async fn udp_loop(socket: Arc<UdpSocket>, events: mpsc::Sender<Event>, shared: Arc<Shared>) {
    let mut buf = vec![0u8; MAX_PACKET_BYTES + 1];
    loop {
        match socket.recv_from(&mut buf).await {
            Ok((len, from)) => {
                let bytes = buf.get(..len).unwrap_or_default().to_vec();
                let event = Event::Datagram {
                    ip: from.ip(),
                    bytes,
                    at: Instant::now(),
                };
                match events.try_send(event) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        shared.udp_dropped.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => return,
                }
            }
            // Windows reports an ICMP "port unreachable" for an earlier reply as a receive
            // error (ConnectionReset): nothing is wrong with the socket. Any other error may
            // repeat at once: do not spin.
            Err(e) if e.kind() == io::ErrorKind::ConnectionReset => {}
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
}

async fn accept_loop(
    listener: TcpListener,
    events: mpsc::Sender<Event>,
    config: Arc<OscConfig>,
    inbox: Sender<Inbox>,
    shared: Arc<Shared>,
) {
    let slots = Arc::new(Semaphore::new(
        config.max_tcp_connections.min(Semaphore::MAX_PERMITS),
    ));
    let mut refusals = Refusals::new("OSC server", config.refusal_log_interval);
    let mut next_conn = 0u64;
    loop {
        let due = refusals.due();
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            // The last refusals of a burst are logged even if no connection follows.
            () = sleep_until(due.unwrap_or_else(Instant::now)), if due.is_some() => {
                refusals.flush(&inbox);
                continue;
            }
        };
        let Ok((stream, from)) = accepted else {
            // A transient accept failure (descriptor exhaustion): do not spin.
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        let slot = match slots.clone().try_acquire_owned() {
            Ok(slot) => Some(slot),
            Err(_) => {
                // Full: the actor may close an idle connection (a vanished peer); wait a
                // moment for the slot.
                if events.send(Event::Evict).await.is_err() {
                    return;
                }
                match timeout(EVICT_WAIT, slots.clone().acquire_owned()).await {
                    Ok(Ok(slot)) => Some(slot),
                    _ => None,
                }
            }
        };
        let Some(slot) = slot else {
            shared.tcp_refused.fetch_add(1, Ordering::Relaxed);
            refusals.refused(&inbox, "connection limit reached");
            continue;
        };
        let conn = next_conn;
        next_conn += 1;
        let events = events.clone();
        let write_timeout = config.write_timeout;
        tokio::spawn(async move {
            tcp_connection(stream, from.ip(), conn, events, write_timeout).await;
            drop(slot);
        });
    }
}

async fn tcp_connection(
    stream: TcpStream,
    ip: IpAddr,
    conn: u64,
    events: mpsc::Sender<Event>,
    write_timeout: Duration,
) {
    let _ = stream.set_nodelay(true);
    let stream = Arc::new(stream);
    let (out, out_rx) = mpsc::channel(TCP_WRITE_QUEUE);
    // Rung by the writer when the peer stops reading, and by the actor to close the connection.
    let dead = Arc::new(Notify::new());
    let open = Event::TcpOpen {
        conn,
        ip,
        out,
        close: dead.clone(),
    };
    if events.send(open).await.is_err() {
        return;
    }
    let writer = tokio::spawn(tcp_writer(
        stream.clone(),
        out_rx,
        write_timeout,
        dead.clone(),
    ));
    let mut buf = vec![0u8; READ_CHUNK];
    loop {
        tokio::select! {
            () = dead.notified() => break,
            readable = stream.readable() => {
                if readable.is_err() {
                    break;
                }
                match stream.try_read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let bytes = buf.get(..n).unwrap_or_default().to_vec();
                        // Waits when the actor is behind: this connection stops reading and
                        // TCP holds the peer back.
                        let data = Event::TcpData { conn, bytes, at: Instant::now() };
                        if events.send(data).await.is_err() {
                            break;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(_) => break,
                }
            }
        }
    }
    let _ = events.send(Event::TcpClosed { conn }).await;
    // The actor drops the queue's sender on `TcpClosed`, which ends the writer once what it
    // was given is written.
    let _ = timeout(write_timeout, writer).await;
}

async fn tcp_writer(
    stream: Arc<TcpStream>,
    mut queue: mpsc::Receiver<Vec<u8>>,
    limit: Duration,
    dead: Arc<Notify>,
) {
    while let Some(bytes) = queue.recv().await {
        let written = timeout(limit, async {
            let mut rest = bytes.as_slice();
            while !rest.is_empty() {
                stream.writable().await?;
                match stream.try_write(rest) {
                    Ok(n) => rest = rest.split_at(n.min(rest.len())).1,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e),
                }
            }
            Ok::<(), io::Error>(())
        })
        .await;
        if !matches!(written, Ok(Ok(()))) {
            // A peer that does not read: close the connection.
            dead.notify_one();
            return;
        }
    }
}

/// The counters that mean input was dropped or refused, for the periodic log entry.
#[derive(Clone, Copy, Default)]
struct Problems {
    malformed: u64,
    oversize: u64,
    bad_address: u64,
    throttled: u64,
    overload: u64,
}

impl Problems {
    fn of(s: &OscListenerStats) -> Self {
        Self {
            malformed: s.osc.malformed,
            oversize: s.osc.oversize_frames,
            bad_address: s.osc.bad_address,
            throttled: s.osc.replies_throttled + s.osc.updates_throttled,
            overload: s.udp_dropped + s.inputs_dropped + s.send_failed,
        }
    }

    fn since(self, before: Self) -> Option<String> {
        let parts: Vec<String> = [
            (self.malformed.saturating_sub(before.malformed), "malformed"),
            (self.oversize.saturating_sub(before.oversize), "oversize"),
            (
                self.bad_address.saturating_sub(before.bad_address),
                "with a bad address",
            ),
            (
                self.throttled.saturating_sub(before.throttled),
                "replies or updates held back by the byte budget",
            ),
            (
                self.overload.saturating_sub(before.overload),
                "dropped under load",
            ),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, what)| format!("{n} {what}"))
        .collect();
        (!parts.is_empty()).then(|| format!("OSC input problems: {}", parts.join(", ")))
    }
}

struct TcpConn {
    ip: IpAddr,
    decoder: SlipDecoder,
    out: mpsc::Sender<Vec<u8>>,
    close: Arc<Notify>,
    /// When it last sent anything (when it opened, before that).
    last_rx: Instant,
}

struct Actor {
    osc: Osc,
    /// `None` until the first snapshot: input waits in the queues until then.
    mirror: Option<Mirror>,
    shared: Arc<Shared>,
    config: Arc<OscConfig>,
    inbox: Sender<Inbox>,
    udp: Arc<UdpSocket>,
    tcp: HashMap<u64, TcpConn>,
    reply_tx: ReplyTx,
    counter: u64,
    /// Snapshot requests not answered yet. While any is out, broadcasts wait in their queue (as
    /// in the WebSocket connections): one made after the snapshot was taken must not be applied
    /// before it, and one made before is dropped as stale once it is in.
    snapshot_waits: HashSet<RequestId>,
    inflight: HashSet<RequestId>,
    start: Instant,
    listener: OscListenerStats,
    reported: Problems,
}

impl Actor {
    async fn run(
        mut self,
        mut events: mpsc::Receiver<Event>,
        mut udp: mpsc::Receiver<Event>,
        mut tap: mpsc::Receiver<ServerMessage>,
        mut replies: ReplyRx,
        lagged: Arc<AtomicBool>,
        wake: Arc<Notify>,
    ) {
        // `interval_at` panics on a zero period.
        let period = self.config.stats_log_interval.max(Duration::from_millis(1));
        let mut ticker = interval_at(Instant::now() + period, period);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        if !self.ask_snapshot() {
            return;
        }
        loop {
            let ready = self.mirror.is_some();
            let waiting = !self.snapshot_waits.is_empty();
            if !waiting && lagged.load(Ordering::Acquire) {
                // The copy missed broadcasts. Cleared before asking, so a broadcast made after
                // the snapshot is taken is queued again and filtered against it.
                while tap.try_recv().is_ok() {}
                lagged.store(false, Ordering::Release);
                if !self.ask_snapshot() {
                    return;
                }
                continue;
            }
            tokio::select! {
                biased;
                reply = replies.recv() => match reply {
                    Some(message) => self.reply(message),
                    None => return,
                },
                // Only wakes the loop; the lag is handled at the top of the next turn.
                () = wake.notified() => {}
                message = tap.recv(), if ready && !waiting => match message {
                    Some(message) => {
                        if !self.broadcast(message) {
                            return;
                        }
                    }
                    None => return,
                },
                // Not while a snapshot is out: a `/cue/5` or a trigger address must not be
                // resolved against a copy just found to be wrong (it could start the cue that
                // used to be 5). Input waits, within the age limit.
                event = next_input(&mut udp, &mut events), if ready && !waiting => match event {
                    Some(event) => self.event(event),
                    None => return,
                },
                _ = ticker.tick() => self.report(),
            }
        }
    }

    fn now(&self) -> Duration {
        self.start.elapsed()
    }

    fn next_id(&mut self) -> RequestId {
        let id = RequestId(ID_BASE + self.counter);
        // Never `u64::MAX`, which is the WebSocket server's.
        self.counter = (self.counter + 1) % (u64::MAX - ID_BASE);
        id
    }

    /// `false`: the control loop is gone.
    fn ask_snapshot(&mut self) -> bool {
        let id = self.next_id();
        self.snapshot_waits.insert(id);
        self.inbox
            .send(Inbox::Client {
                message: ClientMessage::Request {
                    id,
                    request: Request::Snapshot,
                },
                reply: self.reply_tx.clone(),
            })
            .is_ok()
    }

    fn publish(&mut self) {
        self.listener.osc = *self.osc.stats();
        self.listener.tcp_connections = self.tcp.len();
        *self
            .shared
            .stats
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = self.listener;
    }

    fn report(&mut self) {
        let mut now = self.listener;
        now.udp_dropped = self.shared.udp_dropped.load(Ordering::Relaxed);
        now.osc = *self.osc.stats();
        let problems = Problems::of(&now);
        if let Some(message) = problems.since(self.reported) {
            let _ = self.inbox.send(Inbox::Log {
                level: LogLevel::Warn,
                message,
            });
        }
        self.reported = problems;
    }

    fn reply(&mut self, message: ServerMessage) {
        match message {
            ServerMessage::Snapshot { snapshot } => self.install(Mirror::from_snapshot(*snapshot)),
            ServerMessage::Result { id, outcome } => {
                self.snapshot_waits.remove(&id);
                self.inflight.remove(&id);
                if matches!(outcome, Outcome::Error(_)) {
                    self.listener.control_refused += 1;
                    self.publish();
                }
            }
            _ => {}
        }
    }

    /// Replaces the copy. Sessions end when the passcode or the workspace is not the same as
    /// before: a console that connected with the old passcode must not keep control.
    fn install(&mut self, new: Mirror) {
        let updates = match &self.mirror {
            Some(old) => {
                if old.workspace_id() != new.workspace_id() || old.passcode() != new.passcode() {
                    self.osc.reset_sessions();
                }
                old.updates_to(&new)
            }
            None => Vec::new(),
        };
        self.mirror = Some(new);
        self.fan_out(&updates);
    }

    /// `false`: the control loop is gone.
    fn broadcast(&mut self, message: ServerMessage) -> bool {
        let Some(mirror) = &mut self.mirror else {
            return true;
        };
        let passcode = mirror.passcode().map(str::to_owned);
        let applied = match &message {
            ServerMessage::ModelChanged {
                revision,
                upserts,
                deletes,
            } => mirror.model_changed(*revision, upserts, deletes),
            ServerMessage::Runtime { seq, changes } => mirror.runtime(*seq, changes),
            ServerMessage::Document { document } => mirror.document(document),
            _ => return true,
        };
        match applied {
            Applied::Stale => true,
            Applied::Updates(updates) => {
                // A settings edit that changed the passcode ends every session, as a snapshot
                // with another passcode does.
                if mirror.passcode() != passcode.as_deref() {
                    self.osc.reset_sessions();
                }
                self.fan_out(&updates);
                true
            }
            Applied::Resync => self.ask_snapshot(),
        }
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Datagram { ip, bytes, at } => {
                let stale = at.elapsed() >= self.config.max_input_age;
                let peer = Peer::Udp(ip);
                let now = self.now();
                let Some(mirror) = &self.mirror else { return };
                let outcome = self
                    .osc
                    .handle_datagram(peer, now, &bytes, &MirrorShow(mirror));
                self.carry_out(peer, outcome, stale);
            }
            Event::TcpOpen {
                conn,
                ip,
                out,
                close,
            } => {
                self.tcp.insert(
                    conn,
                    TcpConn {
                        ip,
                        decoder: SlipDecoder::default(),
                        out,
                        close,
                        last_rx: Instant::now(),
                    },
                );
            }
            Event::TcpData { conn, bytes, at } => {
                let stale = at.elapsed() >= self.config.max_input_age;
                let now = self.now();
                let Some(mirror) = &self.mirror else { return };
                let Some(connection) = self.tcp.get_mut(&conn) else {
                    return;
                };
                connection.last_rx = Instant::now();
                let peer = Peer::Tcp {
                    conn,
                    ip: connection.ip,
                };
                let outcome = self.osc.handle_stream(
                    peer,
                    now,
                    &mut connection.decoder,
                    &bytes,
                    &MirrorShow(mirror),
                );
                self.carry_out(peer, outcome, stale);
            }
            Event::Evict => self.evict(),
            Event::TcpClosed { conn } => {
                if let Some(connection) = self.tcp.remove(&conn) {
                    self.osc.disconnect(Peer::Tcp {
                        conn,
                        ip: connection.ip,
                    });
                }
            }
        }
        self.publish();
    }

    /// Closes the connection that has been silent longest, if it has been silent for
    /// `tcp_idle_evict` and is not subscribed to updates, to make room for a new one.
    fn evict(&mut self) {
        let idle = self.config.tcp_idle_evict;
        let subscribed: HashSet<u64> = self
            .osc
            .update_subscribers()
            .into_iter()
            .filter_map(|p| match p {
                Peer::Tcp { conn, .. } => Some(conn),
                Peer::Udp(_) => None,
            })
            .collect();
        let victim = self
            .tcp
            .iter()
            .filter(|(conn, c)| !subscribed.contains(conn) && c.last_rx.elapsed() >= idle)
            .min_by_key(|(_, c)| c.last_rx)
            .map(|(conn, _)| *conn);
        let Some(conn) = victim else { return };
        if let Some(c) = self.tcp.remove(&conn) {
            self.osc.disconnect(Peer::Tcp { conn, ip: c.ip });
            c.close.notify_one();
            let _ = self.inbox.send(Inbox::Log {
                level: LogLevel::Warn,
                message: format!(
                    "OSC server closed an idle TCP connection from {} to make room",
                    c.ip
                ),
            });
        }
    }

    /// Sends what an input produced: the inputs to the control loop, the replies to the peer.
    fn carry_out(&mut self, peer: Peer, outcome: OscOutcome, stale: bool) {
        for input in outcome.inputs {
            if stale {
                self.listener.inputs_dropped += 1;
            } else {
                self.forward(input);
            }
        }
        for reply in &outcome.replies {
            if let Ok(bytes) = encode_message(reply) {
                self.send(peer, bytes);
            }
        }
    }

    fn forward(&mut self, input: OscInput) {
        if self.inflight.len() >= MAX_INFLIGHT {
            self.listener.inputs_dropped += 1;
            return;
        }
        let id = self.next_id();
        self.inflight.insert(id);
        let sent = self.inbox.send(Inbox::Client {
            message: input.into_client_message(id),
            reply: self.reply_tx.clone(),
        });
        if sent.is_err() {
            self.inflight.remove(&id);
        }
    }

    /// Sends one encoded OSC message to `peer`: UDP to its address on the reply port, TCP
    /// SLIP-framed on its connection. Never waits: a full buffer or queue loses the message
    /// (counted).
    fn send(&mut self, peer: Peer, bytes: Vec<u8>) {
        let sent = match peer {
            Peer::Udp(ip) => self
                .udp
                .try_send_to(&bytes, SocketAddr::new(ip, self.config.reply_port))
                .is_ok(),
            Peer::Tcp { conn, .. } => self
                .tcp
                .get(&conn)
                .is_some_and(|c| c.out.try_send(slip_encode(&bytes)).is_ok()),
        };
        if !sent {
            self.listener.send_failed += 1;
        }
    }

    /// Tells every subscribed peer (NET-05, docs/osc.md "Updates").
    fn fan_out(&mut self, updates: &[Update]) {
        if updates.is_empty() {
            return;
        }
        let subscribers = self.osc.update_subscribers();
        let Some(mirror) = &self.mirror else { return };
        if subscribers.is_empty() {
            return;
        }
        let workspace = mirror.workspace_id().to_string();
        let now = self.now();
        for update in updates {
            let message = update_message(&workspace, update);
            for &peer in &subscribers {
                if let Some(bytes) = self.osc.prepare_update(peer, now, &message) {
                    self.send(peer, bytes);
                }
            }
        }
        self.publish();
    }
}

/// The OSC message for an update. These are our reading of QLab 5's scheme (docs/osc.md).
fn update_message(workspace: &str, update: &Update) -> OscMessage {
    match update {
        Update::Workspace => OscMessage {
            addr: format!("/update/workspace/{workspace}"),
            args: Vec::new(),
        },
        Update::Cue(cue) => OscMessage {
            addr: format!("/update/workspace/{workspace}/cue_id/{cue}"),
            args: Vec::new(),
        },
        Update::Playhead { list, cue } => OscMessage {
            addr: format!("/update/workspace/{workspace}/cueList/{list}/playbackPosition"),
            args: vec![OscType::String(
                cue.map(|c| c.to_string()).unwrap_or_default(),
            )],
        },
    }
}
