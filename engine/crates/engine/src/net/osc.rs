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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crossbeam_channel::Sender;
use openlab_core::OscAccess;
use openlab_core::protocol::{
    ClientMessage, LogLevel, Outcome, Request, RequestId, ServerMessage, TransportAction,
};
use rosc::{OscMessage, OscType};
use socket2::{Domain, Socket, Type};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{Notify, Semaphore, mpsc};
use tokio::time::{Instant, MissedTickBehavior, interval_at, sleep_until, timeout};

use super::hub::Tap;
use super::osc_mirror::{Applied, Mirror, MirrorShow, Update, UpdateId};
use super::refusals::Refusals;
use crate::control::{Inbox, ReplyRx, ReplyTx, reply_channel};
use crate::osc::{
    MAX_PACKET_BYTES, OSC_PORT, OSC_REPLY_PORT, Osc, OscInput, OscStats, Outcome as OscOutcome,
    Peer, ServerInfo, SlipDecoder, encode_message, slip_encode, upper,
};

/// OSC request ids start here, well away from the small numbers clients count up from, and
/// never reach `u64::MAX` (the WebSocket server's own id). The control loop answers each
/// request on the channel it came from, so ids could not clash with a WebSocket client's anyway;
/// the range only keeps logs readable.
const ID_BASE: u64 = 1 << 63;
/// TCP events (data, open, close) waiting for the actor; each connection waits in its own read
/// loop beyond this, which holds that sender back through TCP.
const EVENT_QUEUE: usize = 256;
/// Datagrams waiting for the actor. A queue of their own, taken first (see `next_input`): a TCP peer that floods (it waits, holding its place in line) must not make
/// the datagrams of a console find the queue full.
const UDP_QUEUE: usize = 256;
/// How long an accepted connection waits for the actor to close an idle one when the table is
/// full, before it is refused.
const EVICT_WAIT: Duration = Duration::from_millis(500);
/// Broadcasts waiting for the actor; more marks the copy lagged and it is rebuilt.
const TAP_QUEUE: usize = 1024;
/// Messages waiting to be written to one TCP connection.
const TCP_WRITE_QUEUE: usize = 256;
/// Bytes waiting to be written to one TCP connection, and to all of them together. A reply may
/// be large (a 1 MiB note, `/cueLists`), and a peer that sends queries and does not read must not
/// make us hold gigabytes before the write timeout closes it: past either cap the reply is
/// dropped and counted as `send_failed`.
const MAX_TCP_PENDING_BYTES: usize = 4 * 1024 * 1024;
const MAX_TCP_PENDING_TOTAL: usize = 64 * 1024 * 1024;
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
    /// Open TCP connections. Each costs a task, a decoder (its buffer grows with the frame it
    /// holds, up to 1 MiB once the peer may ask, 4 KiB before) and a write queue. 256 lets a
    /// large installation's controllers and scripts connect (QLab took 1000 in the capture); the
    /// worst case of a hostile network is 256 half-sent 1 MiB frames, 256 MiB, from peers that
    /// may ask: with a passcode that needs one that connected. More are closed at accept, with a
    /// (rate-limited) log entry.
    pub max_tcp_connections: usize,
    /// Requests to the control loop that have not been answered. Past it new inputs are dropped
    /// and counted: the actor never blocks, so this is what keeps a UDP flood from piling up in
    /// the control inbox. The default is above the 1024 inputs one datagram can make (a wildcard
    /// action), so one message is not cut short while the control loop is keeping up.
    pub max_inflight: usize,
    /// One write to a TCP peer that takes longer than this closes the connection.
    pub write_timeout: Duration,
    /// Test seam: the first this many UDP sends are failed without touching the socket, as an
    /// operating system does for a datagram it cannot send (no portable destination makes the
    /// real call fail on every system). They are counted like any other failed send.
    pub fail_udp_sends: u32,
    /// A TCP connection that has sent nothing for this long, and has not asked for updates, may
    /// be closed to make room when the table is full (a peer that vanished without a FIN or
    /// RST would otherwise hold its slot for good). Connections that are subscribed to
    /// updates are never closed for this.
    pub tcp_idle_evict: Duration,
    /// Input (not replies) that waited longer than this for the actor, because the copy of the
    /// workspace was not ready or the actor was behind, is dropped and counted: a console
    /// mashing GO while the engine starts must not have every press replayed at once.
    pub max_input_age: Duration,
    /// An identical message from the same peer within this long of the previous one is dropped
    /// (QLab does: about 100 ms; zero drops nothing).
    pub duplicate_window: Duration,
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
            max_tcp_connections: 256,
            max_inflight: 2048,
            write_timeout: Duration::from_secs(10),
            fail_udp_sends: 0,
            tcp_idle_evict: Duration::from_secs(60),
            max_input_age: Duration::from_secs(2),
            duplicate_window: crate::osc::DUPLICATE_WINDOW,
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
    /// Bytes of replies and updates queued for TCP connections and not yet written.
    pub tcp_pending_bytes: usize,
}

#[derive(Default)]
struct Shared {
    stats: Mutex<OscListenerStats>,
    udp_dropped: AtomicU64,
    tcp_refused: AtomicU64,
    /// Bytes queued for every TCP connection and not yet written.
    pending_total: Arc<AtomicUsize>,
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
        stats.tcp_pending_bytes = self.stats.pending_total.load(Ordering::Relaxed);
        stats
    }
}

/// The bound, not yet running sockets.
pub struct Bound {
    udp: StdUdp,
    tcp: StdListener,
    ports: OscPorts,
}

/// Buffer size asked for on the UDP socket. A datagram may be up to 65507 bytes, and macOS
/// refuses (`EMSGSIZE`) one larger than the send buffer, whose default there is 9216
/// (`net.inet.udp.maxdgram`); on Windows a receive buffer that is too small truncates one. The
/// kernel may grant less, or double it.
const UDP_BUFFER: usize = 1 << 20;

/// Binds the UDP socket with buffers big enough for the largest reply. Failing to set them is not
/// an error: the socket still works, only large replies might not.
fn bind_udp(addr: SocketAddr) -> io::Result<StdUdp> {
    let socket = Socket::new(Domain::for_address(addr), Type::DGRAM, None)?;
    let _ = socket.set_send_buffer_size(UDP_BUFFER);
    let _ = socket.set_recv_buffer_size(UDP_BUFFER);
    socket.bind(&addr.into())?;
    Ok(socket.into())
}

/// Binds both sockets. Done before the tasks start so the caller knows whether OSC is up.
pub fn bind(config: &OscConfig) -> io::Result<Bound> {
    let udp = bind_udp(SocketAddr::new(config.bind, config.port))?;
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
    let (evict_tx, evict_rx) = mpsc::channel(1);
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
    let pending_total = shared.pending_total.clone();

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

    let mut osc = Osc::with_info(ServerInfo {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        port: ports.udp,
        reply_port: config.reply_port,
    });
    osc.set_duplicate_window(config.duplicate_window);
    let actor = Actor {
        osc,
        mirror: None,
        pending_total: pending_total.clone(),
        shared: shared.clone(),
        config: config.clone(),
        inbox: inbox.clone(),
        udp: udp.clone(),
        tcp: HashMap::new(),
        reply_tx,
        counter: 0,
        snapshot_waits: HashSet::new(),
        inflight: HashSet::new(),
        fail_udp_sends: config.fail_udp_sends,
        start: Instant::now(),
        listener: OscListenerStats::default(),
        reported: Problems::default(),
    };
    runtime.spawn(actor.run(
        Queues {
            events: events_rx,
            udp: udp_rx,
            evict: evict_rx,
            tap: tap_rx,
            replies: reply_rx,
        },
        lagged,
        wake,
    ));
    runtime.spawn(udp_loop(udp, udp_tx, shared.clone()));
    runtime.spawn(accept_loop(
        tcp,
        events_tx,
        evict_tx,
        config,
        inbox,
        shared.clone(),
        pending_total,
    ));
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
        /// Bytes queued in `out` and not yet written.
        pending: Arc<AtomicUsize>,
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
}

/// Datagrams taken in a row before a TCP event gets a turn.
const MAX_UDP_STREAK: u32 = 32;

/// The next event from either queue. Datagrams go first: a TCP event is much more work (a
/// 4 KiB read can hold a hundred messages), so taking the queues in turn left an operator's
/// datagram waiting behind a whole read of a flooding peer's frames, long enough under load
/// for it to age out. After [`MAX_UDP_STREAK`] datagrams in a row the queues are taken at
/// random, so a flood of datagrams (bounded: the queue is 256 and rate-limited by the
/// kernel's buffer) cannot starve TCP peers. `None`: a queue closed.
async fn next_input(
    udp: &mut mpsc::Receiver<Event>,
    tcp: &mut mpsc::Receiver<Event>,
    streak: &mut u32,
) -> Option<Event> {
    if *streak < MAX_UDP_STREAK
        && let Ok(event) = udp.try_recv()
    {
        *streak += 1;
        return Some(event);
    }
    *streak = 0;
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
    evict: mpsc::Sender<()>,
    config: Arc<OscConfig>,
    inbox: Sender<Inbox>,
    shared: Arc<Shared>,
    pending_total: Arc<AtomicUsize>,
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
                // Its own signal, not the event queue: that one may be full (a flood) or
                // unread (a snapshot is out), and this loop must not wait on it. One pending
                // request is enough.
                let _ = evict.try_send(());
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
        let pending_total = pending_total.clone();
        tokio::spawn(async move {
            tcp_connection(
                stream,
                from.ip(),
                conn,
                events,
                write_timeout,
                pending_total,
            )
            .await;
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
    pending_total: Arc<AtomicUsize>,
) {
    let _ = stream.set_nodelay(true);
    let stream = Arc::new(stream);
    let (out, out_rx) = mpsc::channel(TCP_WRITE_QUEUE);
    // Rung by the writer when the peer stops reading, and by the actor to close the connection.
    let dead = Arc::new(Notify::new());
    let pending = Arc::new(AtomicUsize::new(0));
    let open = Event::TcpOpen {
        conn,
        ip,
        out,
        pending: pending.clone(),
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
        [pending, pending_total],
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
                        // A connection the writer gave up on must not wait in line behind a
                        // flood to hear it.
                        let sent = tokio::select! {
                            sent = events.send(data) => sent.is_ok(),
                            () = dead.notified() => false,
                        };
                        if !sent {
                            break;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(_) => break,
                }
            }
        }
    }
    // Told in a task of its own: the queue may be full, and this connection's slot must not
    // be held until there is room.
    tokio::spawn(async move {
        let _ = events.send(Event::TcpClosed { conn }).await;
    });
    // The actor drops the queue's sender on `TcpClosed`, which ends the writer once what it
    // was given is written.
    let _ = timeout(write_timeout, writer).await;
}

async fn tcp_writer(
    stream: Arc<TcpStream>,
    mut queue: mpsc::Receiver<Vec<u8>>,
    limit: Duration,
    dead: Arc<Notify>,
    pending: [Arc<AtomicUsize>; 2],
) {
    // Bytes count as pending from the moment the actor queues them until they are written, or the
    // connection is given up on.
    let release = |bytes: usize| {
        for counter in &pending {
            counter.fetch_sub(bytes, Ordering::Relaxed);
        }
    };
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
        release(bytes.len());
        if !matches!(written, Ok(Ok(()))) {
            // A peer that does not read: close the connection.
            dead.notify_one();
            break;
        }
    }
    // What is still queued will never be written.
    queue.close();
    while let Ok(rest) = queue.try_recv() {
        release(rest.len());
    }
}

/// The counters that mean input was dropped or refused, for the periodic log entry.
#[derive(Clone, Copy, Default)]
struct Problems {
    malformed: u64,
    oversize: u64,
    bad_address: u64,
    throttled: u64,
    capped: u64,
    overload: u64,
}

impl Problems {
    fn of(s: &OscListenerStats) -> Self {
        Self {
            malformed: s.osc.malformed,
            oversize: s.osc.oversize_frames,
            bad_address: s.osc.bad_address,
            throttled: s.osc.replies_throttled + s.osc.updates_dropped,
            capped: s.osc.matches_capped + s.osc.inputs_capped,
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
                "replies held back by the byte budget or updates not encodable",
            ),
            (
                self.capped.saturating_sub(before.capped),
                "wildcard messages cut short by the match limits",
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
    pending: Arc<AtomicUsize>,
    close: Arc<Notify>,
    /// When it last sent anything (when it opened, before that).
    last_rx: Instant,
}

/// What the actor waits on.
struct Queues {
    events: mpsc::Receiver<Event>,
    udp: mpsc::Receiver<Event>,
    evict: mpsc::Receiver<()>,
    tap: mpsc::Receiver<ServerMessage>,
    replies: ReplyRx,
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
    /// Bytes queued for every TCP connection together.
    pending_total: Arc<AtomicUsize>,
    reply_tx: ReplyTx,
    counter: u64,
    /// Snapshot requests not answered yet. While any is out, broadcasts wait in their queue (as
    /// in the WebSocket connections): one made after the snapshot was taken must not be applied
    /// before it, and one made before is dropped as stale once it is in.
    snapshot_waits: HashSet<RequestId>,
    inflight: HashSet<RequestId>,
    /// UDP sends still to be failed on purpose (see [`OscConfig::fail_udp_sends`]).
    fail_udp_sends: u32,
    start: Instant,
    listener: OscListenerStats,
    reported: Problems,
}

impl Actor {
    async fn run(mut self, queues: Queues, lagged: Arc<AtomicBool>, wake: Arc<Notify>) {
        let Queues {
            mut events,
            mut udp,
            mut evict,
            mut tap,
            mut replies,
        } = queues;
        // `interval_at` panics on a zero period.
        let period = self.config.stats_log_interval.max(Duration::from_millis(1));
        let mut ticker = interval_at(Instant::now() + period, period);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut streak = 0u32;
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
                // Needs neither the copy nor the event queues.
                Some(()) = evict.recv() => {
                    self.evict();
                    self.publish();
                }
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
                event = next_input(&mut udp, &mut events, &mut streak), if ready && !waiting => match event {
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

    /// Replaces the copy. Sessions end when the workspace is not the same as before, or the
    /// passcode table lost or changed something (`access_changed`): a console that connected with
    /// a passcode that is gone, or rights that are now smaller, must not keep control.
    fn install(&mut self, mut new: Mirror) {
        // The active list is the operator's place, not part of the workspace: a replacement of
        // the copy (save as, a resync, a snapshot broadcast) keeps it while its list exists.
        if let Some(old) = &self.mirror {
            new.inherit_active(old);
        }
        let updates = match &self.mirror {
            Some(old) => {
                if old.workspace_id() != new.workspace_id() {
                    self.osc.reset_sessions();
                } else if access_changed(old.access(), new.access()) {
                    self.osc.end_sessions();
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
        // A workspace was opened, created or reverted: the copy is replaced (sessions end if the
        // workspace or its passcode changed). The stale rule of the WebSocket connections: a
        // snapshot at or below what the copy holds is the one it already has. This only runs
        // while no snapshot request of ours is out, so it cannot be a stale answer to one.
        if let ServerMessage::Snapshot { snapshot } = message {
            let stale = self.mirror.as_ref().is_some_and(|m| {
                snapshot.revision <= m.revision() && snapshot.runtime_seq <= m.runtime_seq()
            });
            if !stale {
                self.install(Mirror::from_snapshot(*snapshot));
            }
            return true;
        }
        let Some(mirror) = &mut self.mirror else {
            return true;
        };
        let access = mirror.access().clone();
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
        // A settings edit that changed the passcode table ends every session, as a snapshot with
        // another table does. Checked whatever the outcome: an edit that failed part-way may
        // have applied its settings first.
        if access_changed(&access, mirror.access()) {
            self.osc.end_sessions();
        }
        match applied {
            Applied::Stale => true,
            Applied::Updates(updates) => {
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
                let arrived = at.saturating_duration_since(self.start);
                let Some(mirror) = &self.mirror else { return };
                let outcome = self.osc.handle_datagram_arrived(
                    peer,
                    now,
                    arrived,
                    &bytes,
                    &MirrorShow(mirror),
                );
                self.carry_out(peer, outcome, stale);
            }
            Event::TcpOpen {
                conn,
                ip,
                out,
                pending,
                close,
            } => {
                self.tcp.insert(
                    conn,
                    TcpConn {
                        ip,
                        decoder: SlipDecoder::default(),
                        out,
                        pending,
                        close,
                        last_rx: Instant::now(),
                    },
                );
            }
            Event::TcpData { conn, bytes, at } => {
                let stale = at.elapsed() >= self.config.max_input_age;
                let now = self.now();
                let arrived = at.saturating_duration_since(self.start);
                let Some(mirror) = &self.mirror else { return };
                let Some(connection) = self.tcp.get_mut(&conn) else {
                    return;
                };
                connection.last_rx = Instant::now();
                let peer = Peer::Tcp {
                    conn,
                    ip: connection.ip,
                };
                let outcome = self.osc.handle_stream_arrived(
                    peer,
                    now,
                    arrived,
                    &mut connection.decoder,
                    &bytes,
                    &MirrorShow(mirror),
                );
                self.carry_out(peer, outcome, stale);
            }
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
    /// `tcp_idle_evict` and is not subscribed to updates, to make room for a new one. With a
    /// passcode set a connection that completed `/connect` is never closed this way: it is a
    /// console that was let in, and a stranger's connections must not be able to push it out
    /// (the cost: a console that vanished without closing holds its slot until a write to it
    /// fails).
    fn evict(&mut self) {
        let idle = self.config.tcp_idle_evict;
        let locked = self
            .mirror
            .as_ref()
            .is_some_and(|m| m.access().requires_passcode());
        let subscribed: HashSet<u64> = self
            .osc
            .update_subscribers(self.now())
            .into_iter()
            .filter_map(|p| match p {
                Peer::Tcp { conn, .. } => Some(conn),
                Peer::Udp(_) => None,
            })
            .collect();
        let victim = self
            .tcp
            .iter()
            .filter(|(conn, c)| {
                let peer = Peer::Tcp {
                    conn: **conn,
                    ip: c.ip,
                };
                !subscribed.contains(conn)
                    && c.last_rx.elapsed() >= idle
                    && !(locked && self.osc.is_connected(peer))
            })
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
                self.note_active_list(&input);
                self.forward(peer, input);
            }
        }
        match peer {
            Peer::Udp(_) => {
                for reply in &outcome.replies {
                    if let Ok(bytes) = encode_message(reply) {
                        self.send(peer, bytes);
                    }
                }
            }
            // One queue entry for all of a read's replies: a wildcard can make a thousand.
            Peer::Tcp { conn, .. } => {
                let mut framed = Vec::new();
                for reply in &outcome.replies {
                    if let Ok(bytes) = encode_message(reply) {
                        framed.extend_from_slice(&slip_encode(&bytes));
                    }
                }
                if !framed.is_empty() {
                    self.queue_tcp(conn, framed);
                }
            }
        }
    }

    /// A message that moves a list's playhead makes that list the active one right away, ahead
    /// of the broadcast that will say so: a `/go` that follows in the same breath must act on it.
    fn note_active_list(&mut self, input: &OscInput) {
        let OscInput::Transport(
            TransportAction::Go { list }
            | TransportAction::GoFrom { list, .. }
            | TransportAction::SetPlayhead { list, .. }
            | TransportAction::SetPlayheadByNumber { list, .. }
            | TransportAction::PlayheadNext { list }
            | TransportAction::PlayheadPrevious { list },
        ) = input
        else {
            return;
        };
        if let Some(mirror) = &mut self.mirror {
            mirror.set_active(*list);
        }
    }

    fn forward(&mut self, peer: Peer, input: OscInput) {
        if self.inflight.len() >= self.config.max_inflight {
            self.listener.inputs_dropped += 1;
            return;
        }
        let id = self.next_id();
        self.inflight.insert(id);
        let message = input.into_client_message(id);
        let ip = match peer {
            Peer::Udp(ip) | Peer::Tcp { ip, .. } => ip,
        };
        // Just before the input, so the show log says where a GO came from (DX-02). Only
        // transport actions are triggers: a command is an edit, and a chatty controller
        // sending those must not double the inbox traffic.
        if matches!(message, ClientMessage::Transport { .. }) {
            let _ = self.inbox.send(Inbox::Trigger {
                source: format!("osc {ip}"),
                detail: super::describe(&message),
            });
        }
        let sent = self.inbox.send(Inbox::Client {
            message,
            reply: self.reply_tx.clone(),
        });
        if sent.is_err() {
            self.inflight.remove(&id);
        }
    }

    /// Sends one encoded OSC message to `peer`: UDP to its address on the reply port (its own, if it set one with `/udpReplyPort`), TCP
    /// SLIP-framed on its connection. Never waits: a full buffer or queue loses the message
    /// (counted).
    fn send(&mut self, peer: Peer, bytes: Vec<u8>) {
        let sent = match peer {
            Peer::Udp(ip) => {
                let port = self
                    .osc
                    .reply_port(peer, self.now())
                    .unwrap_or(self.config.reply_port);
                if self.fail_udp_sends > 0 {
                    self.fail_udp_sends -= 1;
                    false
                } else {
                    self.udp
                        .try_send_to(&bytes, SocketAddr::new(ip, port))
                        .is_ok()
                }
            }
            Peer::Tcp { conn, .. } => return self.queue_tcp(conn, slip_encode(&bytes)),
        };
        if !sent {
            self.listener.send_failed += 1;
        }
    }

    /// Queues already framed bytes for a TCP connection, unless it or all connections together
    /// hold too much unwritten data already (counted as `send_failed`).
    fn queue_tcp(&mut self, conn: u64, framed: Vec<u8>) {
        let len = framed.len();
        let queued = self.tcp.get(&conn).is_some_and(|c| {
            let over = c.pending.load(Ordering::Relaxed).saturating_add(len)
                > MAX_TCP_PENDING_BYTES
                || self
                    .pending_total
                    .load(Ordering::Relaxed)
                    .saturating_add(len)
                    > MAX_TCP_PENDING_TOTAL;
            if over {
                return false;
            }
            c.pending.fetch_add(len, Ordering::Relaxed);
            self.pending_total.fetch_add(len, Ordering::Relaxed);
            if c.out.try_send(framed).is_ok() {
                return true;
            }
            c.pending.fetch_sub(len, Ordering::Relaxed);
            self.pending_total.fetch_sub(len, Ordering::Relaxed);
            false
        });
        if !queued {
            self.listener.send_failed += 1;
        }
    }

    /// Tells every subscribed peer (NET-05, docs/osc.md "Updates").
    fn fan_out(&mut self, updates: &[Update]) {
        if updates.is_empty() {
            return;
        }
        let now = self.now();
        let Some(mirror) = &self.mirror else { return };
        // Updates carry cue data, so only subscribers that may view get them (judged now, so a
        // table edit takes effect at once).
        let subscribers: Vec<_> = self
            .osc
            .update_subscribers(now)
            .into_iter()
            .filter(|&p| self.osc.may_receive_updates(p, mirror.access()))
            .collect();
        if subscribers.is_empty() {
            return;
        }
        let workspace = upper(mirror.workspace_id());
        let passcode_set = mirror.access().requires_passcode();
        for update in updates {
            let message = update_message(&workspace, update);
            let Some(bytes) = self.osc.encode_update(&message) else {
                continue;
            };
            for &peer in &subscribers {
                if self
                    .osc
                    .update_allowed(peer, now, bytes.len(), passcode_set)
                {
                    self.send(peer, bytes.clone());
                }
            }
        }
        self.publish();
    }
}

/// The OSC message for an update, as QLab 5 sends them (docs/osc.md, "Updates"): ids in upper
/// case, `__root__` in lower case, no arguments but the playhead's cue.
fn update_message(workspace: &str, update: &Update) -> OscMessage {
    let base = format!("/update/workspace/{workspace}");
    match update {
        Update::Id(id) => {
            let id = match id {
                UpdateId::Cue(cue) => upper(cue),
                UpdateId::List(list) => upper(list),
                UpdateId::Root => "__root__".to_owned(),
            };
            OscMessage {
                addr: format!("{base}/cue_id/{id}"),
                args: Vec::new(),
            }
        }
        Update::Playhead { list, cue } => OscMessage {
            addr: format!("{base}/cueList/{}/playbackPosition", upper(list)),
            // Past the end there is no argument at all.
            args: cue.map(|c| OscType::String(upper(c))).into_iter().collect(),
        },
        Update::Dashboard => OscMessage {
            addr: format!("{base}/dashboard"),
            args: Vec::new(),
        },
    }
}
/// Whether moving from the table `old` to `new` can take something away from a session: a row
/// was removed or has other rights, or the No Passcode rights changed. Only adding rows keeps
/// sessions, so that adding a passcode mid-show does not make every UDP console connect again.
fn access_changed(old: &OscAccess, new: &OscAccess) -> bool {
    old.no_passcode != new.no_passcode
        || old.passcodes.iter().any(|row| !new.passcodes.contains(row))
}
