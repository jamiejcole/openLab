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

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener as StdListener, UdpSocket as StdUdp};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::Sender;
use openlab_core::protocol::{
    ClientMessage, DocId, LogLevel, Outcome, Request, RequestId, ServerMessage, TransportAction,
    WorkspaceInfo,
};
use openlab_core::{OscAccess, WorkspaceId};
use rosc::{OscMessage, OscType};
use socket2::{Domain, Socket, Type};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{Notify, Semaphore, mpsc};
use tokio::time::{Instant, MissedTickBehavior, interval_at, sleep_until, timeout};

use super::hub::Tap;
use super::osc_mirror::{Applied, Mirror, MirrorShow, Update, UpdateId};
use super::refusals::Refusals;
use crate::control::{Inbox, Outgoing, ReplyRx, ReplyTx, reply_channel};
use crate::osc::{
    DocShow, MAX_PACKET_BYTES, OSC_PORT, OSC_REPLY_PORT, Osc, OscInput, OscShow, OscStats,
    Outcome as OscOutcome, Peer, ServerInfo, SlipDecoder, encode_message, named_workspaces,
    slip_encode, upper,
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
/// Eviction requests waiting for the actor: one for the table plus a few hosts at their cap.
/// More are dropped (the connection that asked is refused after `EVICT_WAIT`).
const EVICT_QUEUE: usize = 8;
/// TCP keepalive on accepted connections: a peer that vanished without a FIN or RST (a console
/// whose Wi-Fi dropped) is noticed after this much silence plus the system's probes (about
/// 10 s apart, 8 to 10 of them), and its connection closed, so it does not hold its slot.
const TCP_KEEPALIVE_TIME: Duration = Duration::from_secs(30);
const TCP_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
/// Broadcasts waiting for the actor; more marks the copy lagged and it is rebuilt. The actor
/// holds as many again for docs whose copy is not ready (`MAX_HELD_BROADCASTS`), so a lag is
/// certain only past twice this. Public so a test can overflow both whatever their size.
pub const TAP_QUEUE: usize = 1024;
/// Input the actor holds while a doc's copy is not ready (see `Actor::take`), in pieces and in
/// bytes; past either, input waits in the queues above.
/// Public so a test can fill it whatever its size.
pub const MAX_HELD_INPUTS: usize = UDP_QUEUE + EVENT_QUEUE;
const MAX_HELD_INPUT_BYTES: usize = 16 * 1024 * 1024;
/// Broadcasts the actor holds for docs whose copy is not ready; more is a lag, as a full tap.
const MAX_HELD_BROADCASTS: usize = TAP_QUEUE;
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
    /// Open TCP connections from one remote host (IP address). At it, the host's next
    /// connection asks the actor to close that same host's longest-idle connection (by the
    /// `tcp_idle_evict` rules: never another host's) and waits `EVICT_WAIT` for its slot; with
    /// none to close it is refused (logged, counted in `tcp_refused`). So one host, even one
    /// whose connections keep alive with `/updates` or a message every minute, cannot hold
    /// every slot and lock out the other consoles, and a console that reconnected many times
    /// over a flaky network is not locked out by its own dead connections.
    pub max_tcp_connections_per_host: usize,
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
            max_tcp_connections_per_host: 16,
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
    /// Input waiting for a doc's copy (taken from the queues, not handled yet).
    pub held_inputs: usize,
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
        let mut stats = *crate::lock(&self.stats.stats);
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
    let (evict_tx, evict_rx) = mpsc::channel(EVICT_QUEUE);
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
        docs: Vec::new(),
        listed: false,
        pending_total: pending_total.clone(),
        shared: shared.clone(),
        config: config.clone(),
        inbox: inbox.clone(),
        udp: udp.clone(),
        tcp: HashMap::new(),
        reply_tx,
        counter: 0,
        waits: HashMap::new(),
        held: None,
        lagged: false,
        held_input: VecDeque::new(),
        held_input_bytes: 0,
        held_tap: VecDeque::new(),
        release_due: false,
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
    /// The frames the actor decoded from one `TcpData` that had to wait (never queued).
    TcpFrames {
        conn: u64,
        frames: Vec<Vec<u8>>,
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
    evict: mpsc::Sender<Option<IpAddr>>,
    config: Arc<OscConfig>,
    inbox: Sender<Inbox>,
    shared: Arc<Shared>,
    pending_total: Arc<AtomicUsize>,
) {
    let slots = Arc::new(Semaphore::new(
        config.max_tcp_connections.min(Semaphore::MAX_PERMITS),
    ));
    let hosts = Arc::new(HostSlots::default());
    let mut refusals = Refusals::new("OSC server", config.refusal_log_interval);
    // Refusals decided in a host's waiting task, logged here with the rest.
    let (refused_tx, mut refused_rx) = mpsc::unbounded_channel::<String>();
    let next_conn = Arc::new(AtomicU64::new(0));
    loop {
        let due = refusals.due();
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            Some(reason) = refused_rx.recv() => {
                refusals.refused(&inbox, reason);
                continue;
            }
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
        keep_alive(&stream);
        let admit = Admit {
            events: events.clone(),
            config: Arc::clone(&config),
            pending_total: Arc::clone(&pending_total),
            next_conn: Arc::clone(&next_conn),
        };
        // The host's own cap first.
        let ip = from.ip().to_canonical();
        let max = config.max_tcp_connections_per_host;
        let Some(host_slot) = hosts.try_take(ip, max) else {
            // At its cap: the actor may close one of this host's own idle connections (one it
            // left behind when its network dropped). That wait runs in a task of its own, so
            // a host that keeps its connections busy cannot slow the accept loop for others,
            // and one at a time per host.
            let Some(waiting) = hosts.start_waiting(ip) else {
                refusals.refused(&inbox, host_refusal(max, ip));
                shared.tcp_refused.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            let (hosts, slots, evict, refused, shared) = (
                Arc::clone(&hosts),
                Arc::clone(&slots),
                evict.clone(),
                refused_tx.clone(),
                Arc::clone(&shared),
            );
            // Counted before the stream is closed, so a peer that sees the close sees the count.
            let refuse = move |reason: String| {
                shared.tcp_refused.fetch_add(1, Ordering::Relaxed);
                let _ = refused.send(reason);
            };
            tokio::spawn(async move {
                let _ = evict.try_send(Some(ip));
                let host_slot = hosts.take_waiting(ip, max, EVICT_WAIT).await;
                drop(waiting);
                let Some(host_slot) = host_slot else {
                    refuse(host_refusal(max, ip));
                    return;
                };
                match table_slot(&slots, &evict).await {
                    Some(slot) => admit.serve(stream, from.ip(), slot, host_slot).await,
                    None => refuse("connection limit reached".to_owned()),
                }
            });
            continue;
        };
        let Some(slot) = table_slot(&slots, &evict).await else {
            shared.tcp_refused.fetch_add(1, Ordering::Relaxed);
            refusals.refused(&inbox, "connection limit reached");
            continue;
        };
        tokio::spawn(admit.serve(stream, from.ip(), slot, host_slot));
    }
}

fn host_refusal(max: usize, ip: IpAddr) -> String {
    format!("per-host connection limit ({max}) reached by {ip}")
}

/// Asks the system to probe a silent peer (`TCP_KEEPALIVE_TIME`): one that vanished is then
/// closed like one that reset. Best effort: without it such a peer holds its slot until an
/// eviction closes it.
fn keep_alive(stream: &TcpStream) {
    let params = socket2::TcpKeepalive::new()
        .with_time(TCP_KEEPALIVE_TIME)
        .with_interval(TCP_KEEPALIVE_INTERVAL);
    let _ = socket2::SockRef::from(stream).set_tcp_keepalive(&params);
}

/// One of the table's `max_tcp_connections`. Full: the actor may close an idle connection (a
/// vanished peer); waits `EVICT_WAIT` for the slot. The request is its own signal, not the
/// event queue: that one may be full (a flood) or unread (a snapshot is out), and this must
/// not wait on it.
async fn table_slot(
    slots: &Arc<Semaphore>,
    evict: &mpsc::Sender<Option<IpAddr>>,
) -> Option<tokio::sync::OwnedSemaphorePermit> {
    if let Ok(slot) = Arc::clone(slots).try_acquire_owned() {
        return Some(slot);
    }
    let _ = evict.try_send(None);
    match timeout(EVICT_WAIT, Arc::clone(slots).acquire_owned()).await {
        Ok(Ok(slot)) => Some(slot),
        _ => None,
    }
}

/// What serving an admitted connection needs.
struct Admit {
    events: mpsc::Sender<Event>,
    config: Arc<OscConfig>,
    pending_total: Arc<AtomicUsize>,
    next_conn: Arc<AtomicU64>,
}

impl Admit {
    async fn serve(
        self,
        stream: TcpStream,
        ip: IpAddr,
        slot: tokio::sync::OwnedSemaphorePermit,
        host_slot: HostSlot,
    ) {
        let conn = self.next_conn.fetch_add(1, Ordering::Relaxed);
        tcp_connection(
            stream,
            ip,
            conn,
            self.events,
            self.config.write_timeout,
            self.pending_total,
        )
        .await;
        drop(slot);
        drop(host_slot);
    }
}

/// Open TCP connections per remote host, for `max_tcp_connections_per_host`.
#[derive(Default)]
struct HostSlots {
    open: Mutex<HashMap<IpAddr, usize>>,
    /// Hosts with a connection waiting for one of their slots (at most one each).
    waiting: Mutex<HashSet<IpAddr>>,
    /// Rung when a slot is given back.
    freed: Notify,
}

impl HostSlots {
    /// One of `ip`'s `max` slots, or `None` if it holds them all. The slot is given back when
    /// the returned guard is dropped.
    fn try_take(self: &Arc<Self>, ip: IpAddr, max: usize) -> Option<HostSlot> {
        let mut open = crate::lock(&self.open);
        let n = open.entry(ip).or_insert(0);
        if *n >= max {
            if *n == 0 {
                open.remove(&ip);
            }
            return None;
        }
        *n += 1;
        Some(HostSlot {
            hosts: self.clone(),
            ip,
        })
    }

    /// Marks `ip` as having a connection waiting for its slot; `None` if one already is.
    fn start_waiting(self: &Arc<Self>, ip: IpAddr) -> Option<Waiting> {
        let mut waiting = crate::lock(&self.waiting);
        waiting.insert(ip).then(|| Waiting {
            hosts: self.clone(),
            ip,
        })
    }

    /// [`try_take`](Self::try_take), waiting up to `wait` for one of `ip`'s slots to be given
    /// back.
    async fn take_waiting(
        self: &Arc<Self>,
        ip: IpAddr,
        max: usize,
        wait: Duration,
    ) -> Option<HostSlot> {
        let deadline = Instant::now() + wait;
        loop {
            // Registered before the check, so a slot given back in between is not missed.
            let freed = self.freed.notified();
            tokio::pin!(freed);
            freed.as_mut().enable();
            if let Some(slot) = self.try_take(ip, max) {
                return Some(slot);
            }
            if tokio::time::timeout_at(deadline, freed).await.is_err() {
                return self.try_take(ip, max);
            }
        }
    }
}

/// A host's waiting connection; dropping it lets the host's next one wait.
struct Waiting {
    hosts: Arc<HostSlots>,
    ip: IpAddr,
}

impl Drop for Waiting {
    fn drop(&mut self) {
        let mut waiting = crate::lock(&self.hosts.waiting);
        waiting.remove(&self.ip);
    }
}

struct HostSlot {
    hosts: Arc<HostSlots>,
    ip: IpAddr,
}

impl Drop for HostSlot {
    fn drop(&mut self) {
        let mut open = crate::lock(&self.hosts.open);
        if let Some(n) = open.get_mut(&self.ip) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                open.remove(&self.ip);
            }
        }
        drop(open);
        self.hosts.freed.notify_waiters();
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
    /// Close an idle connection to make room: of this host (at its cap), or of any (`None`:
    /// the table is full).
    evict: mpsc::Receiver<Option<IpAddr>>,
    tap: mpsc::Receiver<Outgoing>,
    replies: ReplyRx,
}

/// One open doc as the actor knows it.
struct DocCopy {
    doc: DocId,
    /// `None` until its first snapshot: input that may reach it waits until then.
    mirror: Option<Mirror>,
    /// Its id and name in the last list of open docs that named it: what `/workspace/{id}` may
    /// call it before its first snapshot is in.
    listed_as: Option<(WorkspaceId, String)>,
}

impl DocCopy {
    /// Whether `/workspace/{id}` may name this doc, by what its list entry or its copy says.
    fn may_be_named(&self, id: &str) -> bool {
        let named = |ws: WorkspaceId, name: &str| upper(ws) == id || name == id;
        self.listed_as
            .as_ref()
            .is_some_and(|(ws, name)| named(*ws, name))
            || self.mirror.as_ref().is_some_and(|m| {
                let show = MirrorShow(m);
                named(show.workspace_id(), show.workspace_name())
            })
    }
}

/// The docs a piece of input can reach.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Reach {
    /// Every open doc: an un-prefixed message (QLab sends it to every workspace), `/version`
    /// or `/workspaces`, anything while the list is out, or a TCP read not decoded yet.
    All,
    /// These docs only (none: it reaches no doc, as a packet that does not decode).
    Docs(Vec<DocId>),
}

impl Reach {
    fn overlaps(&self, other: &Reach) -> bool {
        match (self, other) {
            (Reach::Docs(a), Reach::Docs(b)) => a.iter().any(|d| b.contains(d)),
            (Reach::All, Reach::Docs(d)) | (Reach::Docs(d), Reach::All) => !d.is_empty(),
            (Reach::All, Reach::All) => true,
        }
    }
}

/// Input taken from the queues that waits for a doc's copy (see [`Actor::take`]); handled as
/// if just taken once it may go.
struct HeldInput {
    peer: Peer,
    reach: Reach,
    event: Event,
    /// What it holds in memory, counted against [`MAX_HELD_INPUT_BYTES`].
    bytes: usize,
}

/// A request of the actor's own whose answer it waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wait {
    /// The list of open docs (an app connection's `snapshot`).
    List,
    /// A doc's snapshot.
    Snapshot(DocId),
}

struct Actor {
    osc: Osc,
    /// The open docs in open order (doc ids rise in open order). Empty before the first list
    /// and while no doc is open: an un-prefixed message then errors, nothing answers from a
    /// closed doc's copy.
    docs: Vec<DocCopy>,
    /// The first list of open docs has been heard.
    listed: bool,
    shared: Arc<Shared>,
    config: Arc<OscConfig>,
    inbox: Sender<Inbox>,
    udp: Arc<UdpSocket>,
    tcp: HashMap<u64, TcpConn>,
    /// Bytes queued for every TCP connection together.
    pending_total: Arc<AtomicUsize>,
    reply_tx: ReplyTx,
    counter: u64,
    /// Requests of ours not answered yet. While a doc's snapshot is out its copy is not ready:
    /// its broadcasts wait (as in the WebSocket connections: one made after the answer was taken
    /// must not be applied before it, and one made before is dropped as stale once it is in),
    /// and so does input that may reach it. While the list is out, nothing is ready.
    waits: HashMap<RequestId, Wait>,
    /// The answer that came ahead of its `result` (a `snapshot` or `workspaces` message names no
    /// request; the `result` right after it on the channel does).
    held: Option<ServerMessage>,
    /// The copies missed broadcasts and are to be rebuilt (as of the top of this turn): nothing
    /// is ready.
    lagged: bool,
    /// Input that waits for a copy, in arrival order (see [`Actor::take`]).
    held_input: VecDeque<HeldInput>,
    held_input_bytes: usize,
    /// Broadcasts for a doc whose copy is not ready (or the list, while ours is out), in order.
    held_tap: VecDeque<Outgoing>,
    /// A request of ours was answered since held work was last looked at.
    release_due: bool,
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
        if !self.ask(Wait::List) {
            return;
        }
        loop {
            self.lagged = lagged.load(Ordering::Acquire);
            if self.waits.is_empty() && self.lagged {
                // The copies missed broadcasts: the list, then every doc's snapshot. Cleared
                // before asking, so a broadcast made after the answers are taken is queued again
                // and filtered against them. Held broadcasts are older still. Held input waits
                // for the new copies.
                while tap.try_recv().is_ok() {}
                self.held_tap.clear();
                lagged.store(false, Ordering::Release);
                self.lagged = false;
                if !self.ask(Wait::List) {
                    return;
                }
                continue;
            }
            if !self.release() {
                return;
            }
            let room = self.held_input.len() < MAX_HELD_INPUTS
                && self.held_input_bytes < MAX_HELD_INPUT_BYTES;
            tokio::select! {
                biased;
                reply = replies.recv() => match reply {
                    Some(message) => {
                        if !self.reply(message) {
                            return;
                        }
                    }
                    None => return,
                },
                // Only wakes the loop; the lag is handled at the top of the next turn.
                () = wake.notified() => {}
                // Needs neither the copies nor the event queues.
                Some(host) = evict.recv() => {
                    self.evict(host);
                    self.publish();
                }
                // Not while the copies are to be rebuilt: what is queued is dropped then.
                message = tap.recv(), if !self.lagged => match message {
                    Some(message) => {
                        if !self.take_broadcast(message) {
                            return;
                        }
                        if self.held_tap.len() >= MAX_HELD_BROADCASTS {
                            // As if the tap had overflowed: the held ones go, and the copies
                            // are rebuilt once the answers are in.
                            self.held_tap.clear();
                            lagged.store(true, Ordering::Release);
                        }
                    }
                    None => return,
                },
                // Input that may reach a doc whose copy is not ready waits, within the age
                // limit (`take`): a `/cue/5` or a trigger address must not be resolved against
                // a copy just found to be wrong (it could start the cue that used to be 5), and
                // an un-prefixed message must reach every doc. Past the room for held input it
                // waits in the queues.
                event = next_input(&mut udp, &mut events, &mut streak), if room => match event {
                    Some(event) => self.take(event),
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

    /// Asks for the list of open docs (as an app connection does) or a doc's snapshot. `false`:
    /// the control loop is gone.
    fn ask(&mut self, wait: Wait) -> bool {
        let id = self.next_id();
        self.waits.insert(id, wait);
        let doc = match wait {
            Wait::List => None,
            Wait::Snapshot(doc) => Some(doc),
        };
        self.inbox
            .send(Inbox::Conn {
                doc,
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
        self.listener.held_inputs = self.held_input.len();
        *crate::lock(&self.shared.stats) = self.listener;
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

    /// An answer on our reply channel. `false`: the control loop is gone.
    fn reply(&mut self, message: ServerMessage) -> bool {
        match message {
            ServerMessage::Snapshot { .. } | ServerMessage::Workspaces { .. } => {
                self.held = Some(message);
            }
            ServerMessage::Result { id, outcome } => {
                self.inflight.remove(&id);
                let held = self.held.take();
                let ok = matches!(outcome, Outcome::Ok(_));
                let wait = self.waits.remove(&id);
                self.release_due |= wait.is_some();
                match wait {
                    Some(Wait::List) => {
                        if let (true, Some(ServerMessage::Workspaces { workspaces })) = (ok, held) {
                            return self.listed(&workspaces);
                        }
                        // Cannot happen (an app snapshot always answers); the tap's lists
                        // still tell which docs open.
                        self.listed = true;
                    }
                    Some(Wait::Snapshot(doc)) => match (ok, held) {
                        (true, Some(ServerMessage::Snapshot { snapshot })) => {
                            self.install(doc, Mirror::from_snapshot(*snapshot));
                        }
                        // `no_workspace`: it closed meanwhile.
                        _ => self.drop_doc(doc),
                    },
                    None => {
                        if !ok {
                            self.listener.control_refused += 1;
                            self.publish();
                        }
                    }
                }
            }
            _ => {}
        }
        true
    }

    /// The answer to our own list request: exactly the docs open when it was taken. Docs not in
    /// it closed; every doc in it gets a fresh snapshot (the copies are rebuilt). `false`: the
    /// control loop is gone.
    fn listed(&mut self, workspaces: &[WorkspaceInfo]) -> bool {
        self.listed = true;
        let gone: Vec<DocId> = self
            .docs
            .iter()
            .map(|d| d.doc)
            .filter(|doc| !workspaces.iter().any(|w| w.doc == *doc))
            .collect();
        for doc in gone {
            self.drop_doc(doc);
        }
        for info in workspaces {
            self.add_doc(info);
            if !self.ask(Wait::Snapshot(info.doc)) {
                return false;
            }
        }
        true
    }

    /// Knows the doc from now on (no copy yet), in open order; a doc it knows notes what the
    /// list calls it.
    fn add_doc(&mut self, info: &WorkspaceInfo) {
        let doc = info.doc;
        let listed_as = Some((info.workspace_id, info.name.clone()));
        if let Some(copy) = self.copy_mut(doc) {
            copy.listed_as = listed_as;
            return;
        }
        let at = self.docs.partition_point(|d| d.doc < doc);
        self.docs.insert(
            at,
            DocCopy {
                doc,
                mirror: None,
                listed_as,
            },
        );
    }

    /// `doc` closed: its copy goes and its sessions end (ARCHITECTURE section 7).
    fn drop_doc(&mut self, doc: DocId) {
        self.docs.retain(|d| d.doc != doc);
        self.osc.close_doc(doc);
    }

    fn copy_mut(&mut self, doc: DocId) -> Option<&mut DocCopy> {
        self.docs.iter_mut().find(|d| d.doc == doc)
    }

    /// Replaces `doc`'s copy. Its sessions end when the workspace is not the same as before, or
    /// the passcode table lost or changed something (`access_changed`): a console that connected
    /// with a passcode that is gone, or rights that are now smaller, must not keep control.
    fn install(&mut self, doc: DocId, mut new: Mirror) {
        let Some(copy) = self.docs.iter_mut().find(|d| d.doc == doc) else {
            // Closed since we asked.
            return;
        };
        // The active list is the operator's place, not part of the workspace: a replacement of
        // the copy (save as, a resync, a snapshot broadcast) keeps it while its list exists.
        if let Some(old) = &copy.mirror {
            new.inherit_active(old);
        }
        let (updates, reset, end) = match &copy.mirror {
            Some(old) => (
                old.updates_to(&new),
                old.workspace_id() != new.workspace_id(),
                access_changed(old.access(), new.access()),
            ),
            None => (Vec::new(), false, false),
        };
        copy.mirror = Some(new);
        if reset {
            self.osc.reset_doc(doc);
        } else if end {
            self.osc.end_doc_sessions(doc);
        }
        self.fan_out(doc, &updates);
    }

    /// `false`: the control loop is gone.
    fn broadcast(&mut self, out: Outgoing) -> bool {
        let Outgoing { doc, message } = out;
        let Some(doc) = doc else {
            // The list: a doc we do not know has opened. A list read here may be older than
            // what we know (our own list request's answer can overtake it), so it never removes
            // a doc: `closed` and our own list do. A closed doc it names again answers our
            // snapshot request with `no_workspace` and is dropped.
            if let ServerMessage::Workspaces { workspaces } = message {
                for info in &workspaces {
                    let known = self.docs.iter().any(|d| d.doc == info.doc);
                    self.add_doc(info);
                    if !known && !self.ask(Wait::Snapshot(info.doc)) {
                        return false;
                    }
                }
            }
            return true;
        };
        if let ServerMessage::Closed { .. } = message {
            self.drop_doc(doc);
            return true;
        }
        // The doc sent its whole state again (a save-as moved it): the copy is replaced (sessions
        // end if the workspace or its passcode changed). The stale rule of the WebSocket
        // connections: a snapshot at or below what the copy holds is the one it already has.
        // This only runs while no list or snapshot of this doc we asked for is out (held until
        // then: `take_broadcast`), so it cannot be a stale answer to one.
        if let ServerMessage::Snapshot { snapshot } = message {
            let stale = self
                .docs
                .iter()
                .find(|d| d.doc == doc)
                .and_then(|d| d.mirror.as_ref())
                .is_some_and(|m| {
                    snapshot.revision <= m.revision() && snapshot.runtime_seq <= m.runtime_seq()
                });
            if !stale {
                self.install(doc, Mirror::from_snapshot(*snapshot));
            }
            return true;
        }
        // A doc we do not know yet: its snapshot, when its list arrives, holds this.
        let Some(mirror) = self.copy_mut(doc).and_then(|d| d.mirror.as_mut()) else {
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
        // A settings edit that changed the passcode table ends the doc's sessions, as a snapshot
        // with another table does. Checked whatever the outcome: an edit that failed part-way
        // may have applied its settings first.
        if access_changed(&access, mirror.access()) {
            self.osc.end_doc_sessions(doc);
        }
        match applied {
            Applied::Stale => true,
            Applied::Updates(updates) => {
                self.fan_out(doc, &updates);
                true
            }
            Applied::Resync => self.ask(Wait::Snapshot(doc)),
        }
    }

    /// Nothing is ready: the first list is not in, our list request is out, or the copies are to
    /// be rebuilt.
    fn list_out(&self) -> bool {
        !self.listed || self.lagged || self.waits.values().any(|w| *w == Wait::List)
    }

    /// Whether `doc`'s copy is not ready: it has none yet, or a snapshot we asked for is out.
    fn doc_pending(&self, doc: DocId) -> bool {
        self.list_out()
            || self.waits.values().any(|w| *w == Wait::Snapshot(doc))
            || self.docs.iter().any(|d| d.doc == doc && d.mirror.is_none())
    }

    fn any_pending(&self) -> bool {
        self.list_out() || !self.waits.is_empty() || self.docs.iter().any(|d| d.mirror.is_none())
    }

    /// The copies that are ready, in open order.
    fn ready_shows(&self) -> Vec<(DocId, MirrorShow<'_>)> {
        self.docs
            .iter()
            .filter(|d| !self.doc_pending(d.doc))
            .filter_map(|d| d.mirror.as_ref().map(|m| (d.doc, MirrorShow(m))))
            .collect()
    }

    /// The docs `/workspace/{id}` may name: every doc whose copy or list entry goes by that id
    /// or name (more than the dispatcher picks, never fewer); when none does, every doc whose
    /// copy is not ready, as its name may not be known yet. A doc renamed to `id` in a snapshot
    /// not in yet is not counted: the input then goes as it would have a moment earlier, before
    /// the rename reached us. `None`: the docs themselves are not known (the list is out).
    fn docs_named(&self, id: &str) -> Option<Vec<DocId>> {
        if self.list_out() {
            return None;
        }
        let named: Vec<DocId> = self
            .docs
            .iter()
            .filter(|d| d.may_be_named(id))
            .map(|d| d.doc)
            .collect();
        if !named.is_empty() {
            return Some(named);
        }
        Some(
            self.docs
                .iter()
                .filter(|d| self.doc_pending(d.doc))
                .map(|d| d.doc)
                .collect(),
        )
    }

    /// The docs a packet (a datagram or a frame) can reach as the copies stand: by its
    /// prefixes ([`Actor::docs_named`]); every doc for an un-prefixed message.
    fn reach_of(&self, packet: &[u8]) -> Reach {
        let Some(named) = named_workspaces(packet) else {
            return Reach::All;
        };
        let mut docs = Vec::new();
        for id in &named {
            let Some(more) = self.docs_named(id) else {
                return Reach::All;
            };
            for doc in more {
                if !docs.contains(&doc) {
                    docs.push(doc);
                }
            }
        }
        Reach::Docs(docs)
    }

    /// Whether input from `peer` that reaches `reach` must wait: a doc it reaches is not ready,
    /// or input before it (`earlier`) waits that came from the same peer or reaches a doc it
    /// reaches. A peer's input stays in order (its session, its stream decoder and its replies
    /// depend on that), and so does each doc's.
    fn must_wait<'a>(
        &self,
        peer: Peer,
        reach: &Reach,
        mut earlier: impl Iterator<Item = &'a HeldInput>,
    ) -> bool {
        let waits = match reach {
            Reach::All => self.any_pending(),
            Reach::Docs(docs) => docs.iter().any(|d| self.doc_pending(*d)),
        };
        waits || earlier.any(|h| h.peer == peer || h.reach.overlaps(reach))
    }

    /// Input from the queues: handled now, or held while it may reach a doc whose copy is not
    /// ready (only then is a packet looked at before it is handled).
    fn take(&mut self, event: Event) {
        if !self.any_pending() && self.held_input.is_empty() {
            return self.event(event);
        }
        let (peer, reach, event) = match event {
            Event::TcpOpen { .. } | Event::TcpFrames { .. } => return self.event(event),
            Event::Datagram { ip, ref bytes, .. } => (Peer::Udp(ip), self.reach_of(bytes), event),
            Event::TcpClosed { conn } => {
                let Some(connection) = self.tcp.get(&conn) else {
                    return self.event(event);
                };
                let peer = Peer::Tcp {
                    conn,
                    ip: connection.ip,
                };
                (peer, Reach::Docs(Vec::new()), event)
            }
            Event::TcpData { conn, bytes, at } => {
                let Some(connection) = self.tcp.get_mut(&conn) else {
                    return;
                };
                connection.last_rx = Instant::now();
                let peer = Peer::Tcp {
                    conn,
                    ip: connection.ip,
                };
                // Decoded now only when the peer's frame limit depends on no copy that is not
                // ready (it may ask in a ready one: the full limit), and nothing of the peer's
                // waits (its decoder takes its reads in order). Otherwise the read waits whole.
                let early = !self.held_input.iter().any(|h| h.peer == peer) && {
                    let ready = self.ready_shows();
                    self.osc.may_ask_any(peer, &doc_shows(&ready))
                };
                let decoder = self.tcp.get_mut(&conn).map(|c| &mut c.decoder);
                match decoder {
                    Some(decoder) if early => {
                        let frames = self.osc.read_frames(decoder, &bytes, true);
                        let mut reach = Reach::Docs(Vec::new());
                        for frame in &frames {
                            match (self.reach_of(frame), &mut reach) {
                                (Reach::All, _) => {
                                    reach = Reach::All;
                                    break;
                                }
                                (Reach::Docs(more), Reach::Docs(docs)) => {
                                    for doc in more {
                                        if !docs.contains(&doc) {
                                            docs.push(doc);
                                        }
                                    }
                                }
                                (Reach::Docs(_), Reach::All) => {}
                            }
                        }
                        (peer, reach, Event::TcpFrames { conn, frames, at })
                    }
                    _ => (peer, Reach::All, Event::TcpData { conn, bytes, at }),
                }
            }
        };
        if self.must_wait(peer, &reach, self.held_input.iter()) {
            let bytes = match &event {
                Event::Datagram { bytes, .. } | Event::TcpData { bytes, .. } => bytes.len(),
                Event::TcpFrames { frames, .. } => frames.iter().map(Vec::len).sum(),
                Event::TcpOpen { .. } | Event::TcpClosed { .. } => 0,
            };
            self.held_input_bytes = self.held_input_bytes.saturating_add(bytes);
            self.held_input.push_back(HeldInput {
                peer,
                reach,
                event,
                bytes,
            });
            self.publish();
        } else {
            self.event(event);
        }
    }

    /// Whether a broadcast about `doc` (`None`: the list) must wait for a copy.
    fn tap_waits(&self, doc: Option<DocId>) -> bool {
        match doc {
            None => self.list_out(),
            Some(doc) => self.doc_pending(doc),
        }
    }

    /// A broadcast from the tap: applied now, or held, behind any held before it for the same
    /// doc, while that doc's copy is not ready. `false`: the control loop is gone.
    fn take_broadcast(&mut self, out: Outgoing) -> bool {
        if self.tap_waits(out.doc) || self.held_tap.iter().any(|h| h.doc == out.doc) {
            self.held_tap.push_back(out);
            return true;
        }
        self.broadcast(out)
    }

    /// Hands on, in order, what was held and may go now that a request of ours was answered:
    /// broadcasts first, so that input that waited is resolved against what was made before it,
    /// then input. `false`: the control loop is gone.
    fn release(&mut self) -> bool {
        if !std::mem::take(&mut self.release_due) {
            return true;
        }
        let mut kept = VecDeque::new();
        let mut blocked: Vec<Option<DocId>> = Vec::new();
        while let Some(out) = self.held_tap.pop_front() {
            if blocked.contains(&out.doc) || self.tap_waits(out.doc) {
                if !blocked.contains(&out.doc) {
                    blocked.push(out.doc);
                }
                kept.push_back(out);
            } else if !self.broadcast(out) {
                return false;
            }
        }
        self.held_tap = kept;
        let mut kept: VecDeque<HeldInput> = VecDeque::new();
        while let Some(held) = self.held_input.pop_front() {
            if self.must_wait(held.peer, &held.reach, kept.iter()) {
                kept.push_back(held);
            } else {
                self.held_input_bytes = self.held_input_bytes.saturating_sub(held.bytes);
                self.event(held.event);
            }
        }
        self.held_input = kept;
        self.publish();
        true
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Datagram { ip, bytes, at } => {
                let stale = at.elapsed() >= self.config.max_input_age;
                let peer = Peer::Udp(ip);
                let now = self.now();
                let arrived = at.saturating_duration_since(self.start);
                let shows = shows(&self.docs);
                let docs = doc_shows(&shows);
                let outcome = self
                    .osc
                    .handle_datagram_docs(peer, now, arrived, &bytes, &docs);
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
                let Some(connection) = self.tcp.get_mut(&conn) else {
                    return;
                };
                connection.last_rx = Instant::now();
                let peer = Peer::Tcp {
                    conn,
                    ip: connection.ip,
                };
                let shows = shows(&self.docs);
                let docs = doc_shows(&shows);
                let outcome = self.osc.handle_stream_docs(
                    peer,
                    now,
                    arrived,
                    &mut connection.decoder,
                    &bytes,
                    &docs,
                );
                self.carry_out(peer, outcome, stale);
            }
            Event::TcpFrames { conn, frames, at } => {
                let stale = at.elapsed() >= self.config.max_input_age;
                let now = self.now();
                let arrived = at.saturating_duration_since(self.start);
                let Some(connection) = self.tcp.get(&conn) else {
                    return;
                };
                let peer = Peer::Tcp {
                    conn,
                    ip: connection.ip,
                };
                let shows = shows(&self.docs);
                let docs = doc_shows(&shows);
                let outcome = self
                    .osc
                    .handle_frames_docs(peer, now, arrived, &frames, &docs);
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
    /// `tcp_idle_evict` and is not subscribed to updates, to make room for a new one. A
    /// connection that completed `/connect` to a doc with a passcode is never closed this way:
    /// it is a console that was let in, and a stranger's connections must not be able to push
    /// it out (the cost: a console that vanished without closing holds its slot until a write
    /// to it fails, or keepalive notices it). With `host`, only that host's connections are
    /// candidates (it is at its own cap); the rules are the same.
    fn evict(&mut self, host: Option<IpAddr>) {
        let idle = self.config.tcp_idle_evict;
        let subscribed: HashSet<u64> = self
            .osc
            .update_subscribers(self.now())
            .into_iter()
            .filter_map(|p| match p {
                Peer::Tcp { conn, .. } => Some(conn),
                Peer::Udp(_) => None,
            })
            .collect();
        let let_in = |peer: Peer| {
            self.docs.iter().any(|d| {
                d.mirror
                    .as_ref()
                    .is_some_and(|m| m.access().requires_passcode())
                    && self.osc.is_connected_in(d.doc, peer)
            })
        };
        let victim = self
            .tcp
            .iter()
            .filter(|(conn, c)| {
                let peer = Peer::Tcp {
                    conn: **conn,
                    ip: c.ip,
                };
                host.is_none_or(|h| c.ip.to_canonical() == h)
                    && !subscribed.contains(conn)
                    && c.last_rx.elapsed() >= idle
                    && !let_in(peer)
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

    /// Sends what an input produced: the inputs to the control loop (each to its doc), the
    /// replies to the peer.
    fn carry_out(&mut self, peer: Peer, outcome: OscOutcome, stale: bool) {
        let OscOutcome {
            inputs,
            input_docs,
            warnings,
            replies,
        } = outcome;
        for message in warnings {
            let _ = self.inbox.send(Inbox::Log {
                level: LogLevel::Warn,
                message,
            });
        }
        for (doc, input) in input_docs.into_iter().zip(inputs) {
            if stale {
                self.listener.inputs_dropped += 1;
            } else {
                self.note_active_list(doc, &input);
                self.forward(peer, doc, input);
            }
        }
        match peer {
            Peer::Udp(_) => {
                for reply in &replies {
                    if let Ok(bytes) = encode_message(reply) {
                        self.send(peer, bytes);
                    }
                }
            }
            // One queue entry for all of a read's replies: a wildcard can make a thousand.
            Peer::Tcp { conn, .. } => {
                let mut framed = Vec::new();
                for reply in &replies {
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
    fn note_active_list(&mut self, doc: DocId, input: &OscInput) {
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
        if let Some(mirror) = self.copy_mut(doc).and_then(|d| d.mirror.as_mut()) {
            mirror.set_active(*list);
        }
    }

    fn forward(&mut self, peer: Peer, doc: DocId, input: OscInput) {
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
        // Just before the input, so the show log says where a GO came from and which workspace
        // it was for: an un-prefixed GO is one input per open doc (DX-02). Only transport
        // actions are triggers: a command is an edit, and a chatty controller sending those
        // must not double the inbox traffic. The doc goes in the detail, not the source, which
        // the log's per-source rate limit counts by.
        if matches!(message, ClientMessage::Transport { .. }) {
            let name = self
                .docs
                .iter()
                .find(|d| d.doc == doc)
                .and_then(|d| d.mirror.as_ref())
                .map(|m| MirrorShow(m).workspace_name().to_owned())
                .unwrap_or_default();
            let _ = self.inbox.send(Inbox::Trigger {
                source: format!("osc {ip}"),
                detail: format!(
                    "{}, workspace {name:?} (doc {doc})",
                    super::describe(&message)
                ),
            });
        }
        // To the doc it is for, as a connection bound to it would send it.
        let sent = self.inbox.send(Inbox::Conn {
            doc: Some(doc),
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

    /// Tells every peer subscribed to `doc` what changed in it (NET-05, docs/osc.md "Updates").
    fn fan_out(&mut self, doc: DocId, updates: &[Update]) {
        if updates.is_empty() {
            return;
        }
        let now = self.now();
        let Some(mirror) = self
            .docs
            .iter()
            .find(|d| d.doc == doc)
            .and_then(|d| d.mirror.as_ref())
        else {
            return;
        };
        // A doc no packet has reached yet still has subscribers: those who asked before it opened.
        self.osc.open_doc(doc, now, mirror.access());
        // Updates carry cue data, so only subscribers that may view this doc get them (judged
        // now, so a table edit takes effect at once).
        let subscribers: Vec<_> = self
            .osc
            .update_subscribers_in(doc, now)
            .into_iter()
            .filter(|&p| self.osc.may_receive_updates(doc, p, mirror.access()))
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
                    .update_allowed(doc, peer, now, bytes.len(), passcode_set)
                {
                    self.send(peer, bytes.clone());
                }
            }
        }
        self.publish();
    }
}

/// The copies as the dispatcher reads them, in open order. A doc with no copy yet is left out:
/// input that may reach it waits until it has one (`Actor::take`).
fn shows(docs: &[DocCopy]) -> Vec<(DocId, MirrorShow<'_>)> {
    docs.iter()
        .filter_map(|d| d.mirror.as_ref().map(|m| (d.doc, MirrorShow(m))))
        .collect()
}

fn doc_shows<'a>(shows: &'a [(DocId, MirrorShow<'a>)]) -> Vec<DocShow<'a>> {
    shows
        .iter()
        .map(|(doc, show)| DocShow {
            doc: *doc,
            show: show as &dyn OscShow,
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn accepted_connections_have_keepalive_on() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _client = TcpStream::connect(addr).await.unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        assert!(!socket2::SockRef::from(&stream).keepalive().unwrap());
        keep_alive(&stream);
        assert!(socket2::SockRef::from(&stream).keepalive().unwrap());
    }

    #[test]
    fn a_host_at_its_connection_cap_does_not_take_another_hosts_slot() {
        let hosts = Arc::new(HostSlots::default());
        let a: IpAddr = Ipv4Addr::new(10, 0, 0, 1).into();
        let b: IpAddr = Ipv4Addr::new(10, 0, 0, 2).into();
        let held: Vec<_> = (0..2).filter_map(|_| hosts.try_take(a, 2)).collect();
        assert_eq!(held.len(), 2);
        assert!(
            hosts.try_take(a, 2).is_none(),
            "a third connection from one host"
        );
        let other = hosts.try_take(b, 2);
        assert!(other.is_some(), "another host was refused");
        // A closed connection gives its host's slot back, and an idle host leaves no entry.
        drop(held);
        assert!(hosts.try_take(a, 2).is_some());
        drop(other);
        assert_eq!(hosts.open.lock().unwrap().len(), 0);
    }
}
