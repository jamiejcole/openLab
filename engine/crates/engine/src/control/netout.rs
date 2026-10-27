// SPDX-License-Identifier: GPL-3.0-or-later

//! Network output (CT-30, PB-10, NFR-07): the sending of Network cues' OSC messages and the
//! look-ups of their hosts. Nothing here blocks the control thread. Each destination has a
//! worker thread with a small bounded queue, so a slow or dead one delays only its own
//! messages; a message that cannot be queued, or fails to send, comes back as a
//! [`NetAnswer::SendFailed`] and becomes a log entry and a warning, never a stall. A message
//! that waited longer than [`MAX_MESSAGE_AGE_MS`] for its destination is dropped, not sent: a
//! lighting cue that arrives seconds late, after the operator has moved on, is worse than none.
//!
//! Host names are looked up (name resolution only, nothing is sent to the host) when a
//! workspace opens or its network patches change, so a name that does not resolve breaks the
//! cues that use it before GO; the addresses found are what the sender then uses. A failed
//! look-up is repeated every [`RETRY_AFTER`], because a network that was not up yet when the
//! show opened must not leave its cues broken for good. The retry and the look-up timeout run
//! on sample time, which stands still while the audio device is down, so during an outage a
//! failed name is retried only once audio is back (Network cues do not need audio, but the
//! control loop's clock is the audio clock). There is no probing of the destination itself:
//! UDP has no way to know, and QLab does not either.
//!
//! Look-up threads are bounded: at most [`MAX_LOOKUPS`] are alive at once, counting those that
//! timed out or whose name was forgotten until they answer, and at most one per (host, port).
//! If hung look-ups fill every slot, the names waiting for one are reported `Unknown` (their
//! cues are not broken on a guess) and [`NetOut::take_limit_note`] says so once.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::Write;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Sender, TrySendError, bounded};
use openlab_core::{
    CueId, HostKind, NetworkPatch, NetworkPatchId, NetworkProtocol, NetworkReachability, OscArg,
    SampleTime, check_endpoint,
};
use rosc::{OscMessage, OscType};

use super::Inbox;
use crate::osc::{encode_message, slip_encode};

/// Messages queued for one destination before more are refused.
pub const LINK_QUEUE: usize = 32;
/// Destinations with a worker thread at once; an idle one (empty queue) is dropped to make room.
pub const MAX_LINKS: usize = 64;
/// Host names followed at once; a show with more than this many named patches has the rest
/// taken to be reachable (no look-up, no problem).
pub const MAX_TRACKED: usize = 256;
/// Host look-up threads alive at once (including ones that timed out or were forgotten).
pub const MAX_LOOKUPS: usize = 8;
/// A failed look-up is repeated after this long (sample time).
pub const RETRY_AFTER: Duration = Duration::from_secs(10);
/// A look-up that has not answered by then counts as failed (its thread is not waited for).
pub const LOOKUP_TIMEOUT: Duration = Duration::from_secs(20);
/// A queued message older than this when its turn comes is dropped (milliseconds).
pub const MAX_MESSAGE_AGE_MS: u64 = 1000;
/// TCP connect and write time limit.
pub const TCP_TIMEOUT: Duration = Duration::from_secs(2);

/// Where a message goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Destination {
    pub host: String,
    pub port: u16,
    pub protocol: NetworkProtocol,
}

impl Destination {
    fn of(patch: &NetworkPatch) -> Self {
        Self {
            host: patch.host.clone(),
            port: patch.port,
            protocol: patch.protocol,
        }
    }
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let proto = match self.protocol {
            NetworkProtocol::Udp => "udp",
            NetworkProtocol::Tcp => "tcp",
        };
        write!(f, "{proto}://{}:{}", self.host, self.port)
    }
}

/// The things done on the network, replaceable in tests. All block, and only ever run on
/// worker threads.
pub trait Transport: Send + Sync {
    /// Delivers one packet (UDP: a datagram; TCP: connects and writes the SLIP frame).
    /// `resolved` is what the last successful look-up of the host found, empty if there is
    /// none (an IP address, or a name not looked up yet): then the sender resolves it itself.
    fn send(&self, to: &Destination, resolved: &[SocketAddr], packet: &[u8]) -> Result<(), String>;
    /// The addresses of `host`: a name look-up only.
    fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, String>;
    /// Milliseconds on a monotonic clock; only differences matter. The age of a queued
    /// message is measured on it (tests move it by hand).
    fn now_ms(&self) -> u64 {
        monotonic_ms()
    }
}

fn monotonic_ms() -> u64 {
    static BASE: OnceLock<Instant> = OnceLock::new();
    let base = BASE.get_or_init(Instant::now);
    u64::try_from(base.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// The real network.
#[derive(Default)]
pub struct SystemTransport {
    udp4: Mutex<Option<Arc<UdpSocket>>>,
    udp6: Mutex<Option<Arc<UdpSocket>>>,
}

impl SystemTransport {
    /// One socket per address family, kept so every UDP message leaves from the same source
    /// port (a receiver that answers, or filters by source, sees one sender).
    fn udp(&self, to: &SocketAddr) -> Result<Arc<UdpSocket>, String> {
        let (slot, bind) = if to.is_ipv4() {
            (&self.udp4, SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))
        } else {
            (&self.udp6, SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)))
        };
        let mut slot = slot
            .lock()
            .map_err(|_| "socket state poisoned".to_owned())?;
        if let Some(socket) = slot.as_ref() {
            return Ok(Arc::clone(socket));
        }
        let socket = Arc::new(UdpSocket::bind(bind).map_err(|e| e.to_string())?);
        *slot = Some(Arc::clone(&socket));
        Ok(socket)
    }
}

fn addresses(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    let found: Vec<SocketAddr> = (bare, port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .collect();
    if found.is_empty() {
        Err("the host has no address".to_owned())
    } else {
        Ok(found)
    }
}

/// IPv4 addresses first, each family in the resolver's order. Most OSC receivers bind IPv4
/// only, and on macOS a name such as `localhost` can resolve to `::1` first, which a UDP
/// sender would never learn was the wrong choice (QLab's behaviour here is unverified).
pub fn prefer_ipv4(addrs: &[SocketAddr]) -> Vec<SocketAddr> {
    let (v4, v6): (Vec<SocketAddr>, Vec<SocketAddr>) =
        addrs.iter().copied().partition(SocketAddr::is_ipv4);
    v4.into_iter().chain(v6).collect()
}

impl Transport for SystemTransport {
    fn send(&self, to: &Destination, resolved: &[SocketAddr], packet: &[u8]) -> Result<(), String> {
        let found = if resolved.is_empty() {
            addresses(&to.host, to.port)?
        } else {
            resolved.to_vec()
        };
        let found = prefer_ipv4(&found);
        match to.protocol {
            NetworkProtocol::Udp => {
                let first = found.first().ok_or("the host has no address")?;
                let socket = self.udp(first)?;
                socket
                    .send_to(packet, first)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
            NetworkProtocol::Tcp => {
                let mut last = "the host has no address".to_owned();
                for addr in found {
                    match TcpStream::connect_timeout(&addr, TCP_TIMEOUT) {
                        Ok(mut stream) => {
                            stream
                                .set_write_timeout(Some(TCP_TIMEOUT))
                                .and_then(|()| stream.set_nodelay(true))
                                .and_then(|()| stream.write_all(packet))
                                .and_then(|()| stream.flush())
                                .map_err(|e| e.to_string())?;
                            return Ok(());
                        }
                        Err(e) => last = e.to_string(),
                    }
                }
                Err(last)
            }
        }
    }

    fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
        addresses(host, port)
    }
}

/// The transport a [`Control`](super::Control) uses.
#[derive(Clone)]
pub struct NetTransport(pub Arc<dyn Transport>);

impl Default for NetTransport {
    fn default() -> Self {
        Self(Arc::new(SystemTransport::default()))
    }
}

impl fmt::Debug for NetTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NetTransport")
    }
}

/// What a worker thread tells the control thread.
#[derive(Debug)]
pub enum NetAnswer {
    /// A message could not be delivered.
    SendFailed {
        cue: CueId,
        destination: String,
        reason: String,
    },
    /// A host look-up finished.
    Lookup {
        patch: NetworkPatchId,
        ticket: u64,
        result: Result<Vec<SocketAddr>, String>,
    },
}

/// What became of a message handed to [`NetOut::send`].
#[derive(Debug, PartialEq, Eq)]
pub enum SendOutcome {
    /// Queued for its destination's worker.
    Queued,
    /// Network output is disabled (PB-10): nothing was sent.
    Suppressed,
    /// Refused here; the reason is for the log.
    Refused(String),
}

struct Job {
    cue: CueId,
    packet: Vec<u8>,
    queued_ms: u64,
}

type Resolved = Arc<Mutex<Vec<SocketAddr>>>;

struct Link {
    to: Destination,
    tx: Sender<Job>,
}

#[derive(Debug, PartialEq, Eq)]
enum Phase {
    /// To be started when a look-up slot is free.
    Wanted,
    InFlight {
        ticket: u64,
        since: SampleTime,
    },
    /// Answered or timed out; looked up again at `retry_at` if set.
    Idle {
        retry_at: Option<SampleTime>,
    },
}

struct Lookup {
    host: String,
    port: u16,
    phase: Phase,
    /// What the cue engine has been told.
    reported: NetworkReachability,
}

/// The control thread's side of network output.
pub struct NetOut {
    transport: Arc<dyn Transport>,
    inbox: Sender<Inbox>,
    rate: u64,
    /// Shared with the workers, so turning the override on also stops what is queued.
    disabled: Arc<AtomicBool>,
    links: HashMap<NetworkPatchId, Link>,
    /// The addresses the last successful look-up of each patch's host found.
    resolved: HashMap<NetworkPatchId, Resolved>,
    lookups: HashMap<NetworkPatchId, Lookup>,
    /// Look-up threads that have not answered yet, by ticket: the bound on threads.
    running: HashMap<u64, (String, u16)>,
    limit_hit: bool,
    limit_note: bool,
    next_ticket: u64,
    /// Messages queued or being sent; a worker counts one off after it has reported any
    /// failure, so zero means every answer is already in the inbox (the offline driver waits
    /// on it).
    outstanding: Arc<AtomicUsize>,
}

/// Reachability changes for `Input::NetworkAvailability`.
pub type Reports = Vec<(NetworkPatchId, NetworkReachability)>;

impl NetOut {
    pub fn new(transport: NetTransport, inbox: Sender<Inbox>, sample_rate: u32) -> Self {
        Self {
            transport: transport.0,
            inbox,
            rate: u64::from(sample_rate.max(1)),
            disabled: Arc::new(AtomicBool::new(false)),
            links: HashMap::new(),
            resolved: HashMap::new(),
            lookups: HashMap::new(),
            running: HashMap::new(),
            limit_hit: false,
            limit_note: false,
            next_ticket: 0,
            outstanding: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Look-ups under way plus messages queued or being sent.
    pub fn busy(&self) -> usize {
        self.running.len() + self.outstanding.load(Ordering::SeqCst)
    }

    /// PB-10: whether Network cues send nothing. Runtime state of the engine, never saved.
    pub fn disabled(&self) -> bool {
        self.disabled.load(Ordering::SeqCst)
    }

    /// Turning it on also stops messages already queued: their workers check it before they
    /// send.
    pub fn set_disabled(&mut self, disabled: bool) {
        self.disabled.store(disabled, Ordering::SeqCst);
    }

    /// True once each time every look-up slot was found taken by hung look-ups (the caller
    /// logs it).
    pub fn take_limit_note(&mut self) -> bool {
        std::mem::take(&mut self.limit_note)
    }

    fn samples(&self, d: Duration) -> SampleTime {
        let secs = d.as_secs().saturating_mul(self.rate);
        secs.saturating_add(u64::from(d.subsec_millis()).saturating_mul(self.rate) / 1000)
    }

    /// Follows the workspace's network patches: starts look-ups for new host names, forgets
    /// what is gone. `all` reports every name's state, not only what changed (a newly opened
    /// workspace).
    pub fn sync(&mut self, patches: &[NetworkPatch], all: bool, now: SampleTime) -> Reports {
        let mut out = Reports::new();
        let keep: HashSet<NetworkPatchId> = patches.iter().map(|p| p.id).collect();
        let gone: Vec<NetworkPatchId> = self
            .lookups
            .keys()
            .filter(|id| !keep.contains(id))
            .copied()
            .collect();
        for id in gone {
            self.forget(id, &mut out);
        }
        self.links.retain(|id, _| keep.contains(id));
        self.resolved.retain(|id, _| keep.contains(id));
        for patch in patches {
            let named = matches!(check_endpoint(&patch.host, patch.port), Ok(HostKind::Name));
            match self.lookups.get(&patch.id) {
                Some(l) if named && l.host == patch.host && l.port == patch.port => {
                    if all {
                        out.push((patch.id, l.reported.clone()));
                    }
                }
                _ => {
                    self.forget(patch.id, &mut out);
                    if named && self.lookups.len() < MAX_TRACKED {
                        self.lookups.insert(
                            patch.id,
                            Lookup {
                                host: patch.host.clone(),
                                port: patch.port,
                                phase: Phase::Wanted,
                                reported: NetworkReachability::Unknown,
                            },
                        );
                    }
                }
            }
        }
        self.start_wanted(now);
        out.extend(self.relieve());
        out
    }

    /// Drops a patch's look-up state. A look-up thread still running for it stays counted in
    /// `running` until it answers.
    fn forget(&mut self, id: NetworkPatchId, out: &mut Reports) {
        if let Some(r) = self.resolved.get(&id) {
            r.lock().unwrap_or_else(PoisonError::into_inner).clear();
        }
        if let Some(l) = self.lookups.remove(&id)
            && l.reported != NetworkReachability::Unknown
        {
            out.push((id, NetworkReachability::Unknown));
        }
    }

    /// A look-up finished (or the thread gave up). Returns what changed for the cue engine.
    pub fn answer(&mut self, answer: NetAnswer, now: SampleTime) -> Reports {
        let NetAnswer::Lookup {
            patch,
            ticket,
            result,
        } = answer
        else {
            return Vec::new();
        };
        // The thread is gone, whatever it was for: its slot is free again.
        self.running.remove(&ticket);
        let retry = now.saturating_add(self.samples(RETRY_AFTER));
        let mut out = Reports::new();
        // An answer of a look-up that was replaced or given up on is not about this one.
        if let Some(l) = self.lookups.get_mut(&patch)
            && matches!(l.phase, Phase::InFlight { ticket: t, .. } if t == ticket)
        {
            let (reached, retry_at) = match result {
                Ok(addrs) => {
                    self.resolved
                        .entry(patch)
                        .or_default()
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone_from(&addrs);
                    (NetworkReachability::Resolved, None)
                }
                Err(reason) => (NetworkReachability::Unresolved { reason }, Some(retry)),
            };
            l.phase = Phase::Idle { retry_at };
            out.extend(Self::changed(l, patch, reached));
        }
        self.start_wanted(now);
        out.extend(self.relieve());
        out
    }

    fn changed(l: &mut Lookup, patch: NetworkPatchId, now: NetworkReachability) -> Reports {
        if l.reported == now {
            return Vec::new();
        }
        l.reported = now.clone();
        vec![(patch, now)]
    }

    /// Once per tick: look-ups that timed out, retries that are due, look-ups waiting for a slot.
    pub fn tick(&mut self, now: SampleTime) -> Reports {
        let timeout = self.samples(LOOKUP_TIMEOUT);
        let retry = now.saturating_add(self.samples(RETRY_AFTER));
        let mut out = Reports::new();
        for (id, l) in &mut self.lookups {
            match l.phase {
                Phase::InFlight { since, .. } if now.saturating_sub(since) >= timeout => {
                    // The thread is not waited for, but it stays counted in `running`.
                    l.phase = Phase::Idle {
                        retry_at: Some(retry),
                    };
                    out.extend(Self::changed(
                        l,
                        *id,
                        NetworkReachability::Unresolved {
                            reason: "the look-up timed out".to_owned(),
                        },
                    ));
                }
                Phase::Idle { retry_at: Some(at) } if now >= at => l.phase = Phase::Wanted,
                _ => {}
            }
        }
        self.start_wanted(now);
        out.extend(self.relieve());
        out
    }

    /// With every slot taken by look-ups that never answered, the names that wait for a slot
    /// are not known to be bad: report them `Unknown` instead of leaving them broken.
    fn relieve(&mut self) -> Reports {
        if self.running.len() < MAX_LOOKUPS {
            self.limit_hit = false;
            return Vec::new();
        }
        let mut out = Reports::new();
        for (id, l) in &mut self.lookups {
            if l.phase == Phase::Wanted {
                out.extend(Self::changed(l, *id, NetworkReachability::Unknown));
            }
        }
        if !self.limit_hit && !out.is_empty() {
            self.limit_hit = true;
            self.limit_note = true;
        }
        out
    }

    fn start_wanted(&mut self, now: SampleTime) {
        let mut wanted: Vec<NetworkPatchId> = self
            .lookups
            .iter()
            .filter(|(_, l)| l.phase == Phase::Wanted)
            .map(|(id, _)| *id)
            .collect();
        // Always the same order, so a few slots serve every name in turn.
        wanted.sort_by_key(|id| id.0);
        let retry_at = now.saturating_add(self.samples(RETRY_AFTER));
        for id in wanted {
            if self.running.len() >= MAX_LOOKUPS {
                break;
            }
            let Some(l) = self.lookups.get_mut(&id) else {
                continue;
            };
            let (host, port) = (l.host.clone(), l.port);
            // One live thread per name: a second would only wait on the same resolver.
            if self.running.values().any(|(h, p)| *h == host && *p == port) {
                continue;
            }
            self.next_ticket = self.next_ticket.wrapping_add(1);
            let ticket = self.next_ticket;
            let transport = Arc::clone(&self.transport);
            let inbox = self.inbox.clone();
            let name = host.clone();
            let spawned = thread::Builder::new()
                .name("openlab-net-lookup".into())
                .spawn(move || {
                    let result = transport.resolve(&name, port);
                    let _ = inbox.send(Inbox::Net(NetAnswer::Lookup {
                        patch: id,
                        ticket,
                        result,
                    }));
                });
            match spawned {
                Ok(_) => {
                    l.phase = Phase::InFlight { ticket, since: now };
                    self.running.insert(ticket, (host, port));
                }
                Err(_) => {
                    l.phase = Phase::Idle {
                        retry_at: Some(retry_at),
                    };
                }
            }
        }
    }

    /// Hands a message to its destination's worker.
    pub fn send(&mut self, patch: &NetworkPatch, cue: CueId, packet: Vec<u8>) -> SendOutcome {
        if self.disabled() {
            return SendOutcome::Suppressed;
        }
        let to = Destination::of(patch);
        if self.links.get(&patch.id).is_none_or(|l| l.to != to) {
            self.links.remove(&patch.id);
            if self.links.len() >= MAX_LINKS {
                // Free a sender with nothing queued; its worker ends after any message it is
                // sending now.
                let idle = self
                    .links
                    .iter()
                    .find(|(_, l)| l.tx.is_empty())
                    .map(|(id, _)| *id);
                match idle {
                    Some(id) => {
                        self.links.remove(&id);
                    }
                    None => {
                        return SendOutcome::Refused("too many network destinations".to_owned());
                    }
                }
            }
            match self.spawn_link(patch.id, to) {
                Ok(link) => {
                    self.links.insert(patch.id, link);
                }
                Err(e) => return SendOutcome::Refused(format!("could not start a sender: {e}")),
            }
        }
        let queued_ms = self.transport.now_ms();
        let Some(link) = self.links.get(&patch.id) else {
            return SendOutcome::Refused("no sender".to_owned());
        };
        self.outstanding.fetch_add(1, Ordering::SeqCst);
        match link.tx.try_send(Job {
            cue,
            packet,
            queued_ms,
        }) {
            Ok(()) => SendOutcome::Queued,
            Err(TrySendError::Full(_)) => {
                self.outstanding.fetch_sub(1, Ordering::SeqCst);
                SendOutcome::Refused("too many messages waiting for the destination".to_owned())
            }
            Err(TrySendError::Disconnected(_)) => {
                self.outstanding.fetch_sub(1, Ordering::SeqCst);
                // The worker is gone; the next message starts a new one.
                self.links.remove(&patch.id);
                SendOutcome::Refused("the sender stopped".to_owned())
            }
        }
    }

    fn spawn_link(&mut self, patch: NetworkPatchId, to: Destination) -> std::io::Result<Link> {
        let (tx, rx) = bounded::<Job>(LINK_QUEUE);
        let transport = Arc::clone(&self.transport);
        let inbox = self.inbox.clone();
        let dest = to.clone();
        let outstanding = Arc::clone(&self.outstanding);
        let disabled = Arc::clone(&self.disabled);
        let resolved = Arc::clone(self.resolved.entry(patch).or_default());
        thread::Builder::new()
            .name("openlab-net-send".into())
            .spawn(move || {
                // Ends when the control thread drops the link (a destination that changed, or
                // shutdown), after sending what is queued.
                while let Ok(job) = rx.recv() {
                    let outcome = if disabled.load(Ordering::SeqCst) {
                        Ok(())
                    } else if transport.now_ms().saturating_sub(job.queued_ms) > MAX_MESSAGE_AGE_MS
                    {
                        Err("destination too late".to_owned())
                    } else {
                        let found = resolved
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .clone();
                        transport.send(&dest, &found, &job.packet)
                    };
                    if let Err(reason) = outcome {
                        let _ = inbox.send(Inbox::Net(NetAnswer::SendFailed {
                            cue: job.cue,
                            destination: dest.to_string(),
                            reason,
                        }));
                    }
                    outstanding.fetch_sub(1, Ordering::SeqCst);
                }
            })?;
        Ok(Link { to, tx })
    }
}

/// The OSC packet for a Network cue's message, framed for the protocol: a UDP datagram, or a
/// SLIP frame (OSC 1.1, as QLab's own TCP listener and ours read) for TCP.
pub fn packet(
    address: &str,
    args: &[OscArg],
    protocol: NetworkProtocol,
) -> Result<Vec<u8>, String> {
    let message = OscMessage {
        addr: address.to_owned(),
        args: args
            .iter()
            .map(|a| match a {
                OscArg::Int { value } => OscType::Int(*value),
                OscArg::Float { value } => OscType::Float(*value),
                OscArg::String { value } => OscType::String(value.clone()),
                OscArg::Bool { value } => OscType::Bool(*value),
            })
            .collect(),
    };
    let bytes = encode_message(&message).map_err(|e| e.to_string())?;
    Ok(match protocol {
        NetworkProtocol::Udp => bytes,
        NetworkProtocol::Tcp => slip_encode(&bytes),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlab_core::{MAX_OSC_MESSAGE_BYTES, check_message};
    use uuid::Uuid;

    fn nid(n: u128) -> NetworkPatchId {
        NetworkPatchId(Uuid::from_u128(n))
    }

    fn patch(n: u128, host: &str) -> NetworkPatch {
        NetworkPatch {
            id: nid(n),
            name: "p".into(),
            host: host.into(),
            port: 9000,
            protocol: NetworkProtocol::Udp,
        }
    }

    struct Fake;
    impl Transport for Fake {
        fn send(&self, _: &Destination, _: &[SocketAddr], _: &[u8]) -> Result<(), String> {
            Ok(())
        }
        fn resolve(&self, _: &str, _: u16) -> Result<Vec<SocketAddr>, String> {
            Ok(vec![])
        }
    }

    fn out() -> (NetOut, crossbeam_channel::Receiver<Inbox>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        (NetOut::new(NetTransport(Arc::new(Fake)), tx, 1000), rx)
    }

    /// The ticket of `patch`'s look-up in flight.
    fn ticket(net: &NetOut, n: u128) -> u64 {
        match net.lookups.get(&nid(n)).map(|l| &l.phase) {
            Some(Phase::InFlight { ticket, .. }) => *ticket,
            other => panic!("not in flight: {other:?}"),
        }
    }

    fn fail(net: &mut NetOut, n: u128, t: u64, now: SampleTime) -> Reports {
        net.answer(
            NetAnswer::Lookup {
                patch: nid(n),
                ticket: t,
                result: Err("no such host".into()),
            },
            now,
        )
    }

    fn unresolved(reason: &str) -> NetworkReachability {
        NetworkReachability::Unresolved {
            reason: reason.into(),
        }
    }

    #[test]
    fn packets_are_udp_datagrams_or_slip_frames() {
        let args = [
            OscArg::Int { value: 1 },
            OscArg::Float { value: 0.5 },
            OscArg::String { value: "hi".into() },
            OscArg::Bool { value: true },
        ];
        let udp = packet("/a/b", &args, NetworkProtocol::Udp).unwrap_or_default();
        let want: Vec<u8> = [
            &b"/a/b\0\0\0\0"[..],
            b",ifsT\0\0\0",
            &1i32.to_be_bytes(),
            &0.5f32.to_be_bytes(),
            b"hi\0\0",
        ]
        .concat();
        assert_eq!(udp, want);
        let tcp = packet("/a/b", &args, NetworkProtocol::Tcp).unwrap_or_default();
        assert_eq!(tcp, slip_encode(&want));
        assert_eq!(tcp.first(), Some(&0xC0));
        assert_eq!(tcp.last(), Some(&0xC0));
    }

    /// The core's size check is exact: a message it passes encodes to at most the cap, and one
    /// it refuses for size encodes to more.
    #[test]
    fn the_cores_size_check_matches_the_encoder() {
        for n in [1usize, 3999, 4000, 4001] {
            for count in 1..4 {
                let args = vec![
                    OscArg::String {
                        value: "x".repeat(n)
                    };
                    count
                ];
                let encoded = packet("/a", &args, NetworkProtocol::Udp).unwrap_or_default();
                assert_eq!(
                    check_message("/a", &args).is_ok(),
                    encoded.len() <= MAX_OSC_MESSAGE_BYTES,
                    "{n} x {count}: {}",
                    encoded.len()
                );
            }
        }
    }

    #[test]
    fn ipv4_comes_first_when_the_resolver_returns_both() {
        let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, 9000));
        let v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, 9000));
        let v4b = SocketAddr::from((Ipv4Addr::new(10, 0, 0, 1), 9000));
        assert_eq!(prefer_ipv4(&[v6, v4]), vec![v4, v6]);
        assert_eq!(prefer_ipv4(&[v6, v4, v4b]), vec![v4, v4b, v6]);
        assert_eq!(prefer_ipv4(&[v6]), vec![v6]);
        assert!(prefer_ipv4(&[]).is_empty());
    }

    #[test]
    fn sync_looks_up_names_only_and_reports_changes() {
        let (mut net, _rx) = out();
        let patches = [
            patch(1, "lights.local"),
            patch(2, "10.0.0.2"),
            patch(3, "bad host"),
        ];
        assert!(
            net.sync(&patches, true, 0)
                .iter()
                .all(|(id, r)| { *id == nid(1) && *r == NetworkReachability::Unknown })
        );
        assert_eq!(net.lookups.len(), 1);
        assert_eq!(net.running.len(), 1);
        let t = ticket(&net, 1);
        assert_eq!(
            fail(&mut net, 1, t, 100),
            vec![(nid(1), unresolved("no such host"))]
        );
        assert_eq!(net.running.len(), 0);
        // A stale ticket is ignored.
        let stale = NetAnswer::Lookup {
            patch: nid(1),
            ticket: t + 99,
            result: Ok(vec![]),
        };
        assert!(net.answer(stale, 101).is_empty());
        // Retried after RETRY_AFTER (10 s = 10_000 samples at 1 kHz), not before.
        assert!(net.tick(9_000).is_empty());
        assert_eq!(net.running.len(), 0);
        net.tick(10_100);
        assert_eq!(net.running.len(), 1);
    }

    /// H1: look-ups that time out and answer late free their slots, so a slow resolver does not
    /// end look-ups for the rest of the session.
    #[test]
    fn a_slow_resolver_times_out_retries_and_recovers() {
        let (mut net, _rx) = out();
        net.sync(&[patch(1, "lights.local")], false, 0);
        let mut now = 0;
        let mut tickets = Vec::new();
        // Far more tries than the old give-up limit; each thread answers only after its
        // timeout, as a resolver slower than 20 s would.
        for _ in 0..40 {
            tickets.push(ticket_or_none(&net));
            now += 20_000;
            let r = net.tick(now);
            assert!(
                matches!(
                    r.as_slice(),
                    [] | [(_, NetworkReachability::Unresolved { .. })]
                ),
                "{r:?}"
            );
            // The thread finally answers (failed), after its timeout.
            if let Some(t) = tickets.last().copied().flatten() {
                fail(&mut net, 1, t, now);
            }
            now += 10_000;
            net.tick(now);
        }
        assert!(net.running.len() <= 1, "{}", net.running.len());
        // The resolver recovers: the next try resolves and the name is mended.
        let t = loop {
            if let Some(Phase::InFlight { ticket, .. }) = net.lookups.get(&nid(1)).map(|l| &l.phase)
            {
                break *ticket;
            }
            now += 1_000;
            net.tick(now);
        };
        let r = net.answer(
            NetAnswer::Lookup {
                patch: nid(1),
                ticket: t,
                result: Ok(vec![]),
            },
            now,
        );
        assert_eq!(r, vec![(nid(1), NetworkReachability::Resolved)]);
    }

    fn ticket_or_none(net: &NetOut) -> Option<u64> {
        match net.lookups.get(&nid(1)).map(|l| &l.phase) {
            Some(Phase::InFlight { ticket, .. }) => Some(*ticket),
            _ => None,
        }
    }

    /// H1: when hung look-ups fill every slot, waiting names are `Unknown`, not broken, the
    /// limit is noted once, and look-ups resume when a thread answers.
    #[test]
    fn hung_look_ups_filling_every_slot_do_not_break_cues() {
        let (mut net, _rx) = out();
        let names: Vec<NetworkPatch> = (1..=12).map(|n| patch(n, &format!("h{n}.local"))).collect();
        net.sync(&names, false, 0);
        assert_eq!(net.running.len(), MAX_LOOKUPS);
        // Every started look-up times out and none answers.
        let r = net.tick(20_000);
        assert_eq!(
            r.iter()
                .filter(|(_, v)| matches!(v, NetworkReachability::Unresolved { .. }))
                .count(),
            MAX_LOOKUPS
        );
        // 10 s later they are due again, but all slots are held by the hung threads: the
        // names waiting are reported Unknown (mended), and the limit is noted once.
        let r = net.tick(30_000);
        assert_eq!(
            r.iter()
                .filter(|(_, v)| *v == NetworkReachability::Unknown)
                .count(),
            MAX_LOOKUPS
        );
        assert!(net.take_limit_note());
        net.tick(31_000);
        assert!(!net.take_limit_note(), "noted once");
        assert_eq!(net.running.len(), MAX_LOOKUPS);
        // One hung thread finally answers: its slot serves a waiting name.
        let hung = *net.running.keys().min().unwrap_or(&0);
        let key = net.running.get(&hung).cloned();
        net.answer(
            NetAnswer::Lookup {
                patch: nid(999),
                ticket: hung,
                result: Err("late".into()),
            },
            32_000,
        );
        assert_eq!(
            net.running.len(),
            MAX_LOOKUPS,
            "a waiting name took the slot"
        );
        assert!(!net.running.values().any(|v| Some(v) == key.as_ref()) || key.is_none());
    }

    /// M2: forgetting an in-flight look-up does not free its thread's slot, and a name has
    /// at most one live thread.
    #[test]
    fn rapid_forget_and_restart_cannot_pile_up_threads() {
        let (mut net, _rx) = out();
        let names: Vec<NetworkPatch> = (1..=8).map(|n| patch(n, &format!("h{n}.local"))).collect();
        for round in 0..50 {
            net.sync(&names, false, round);
            net.sync(&[], false, round);
        }
        assert!(net.running.len() <= MAX_LOOKUPS, "{}", net.running.len());
        // The same name restarted while its old thread runs does not get a second one.
        let (mut net, _rx) = out();
        net.sync(&[patch(1, "a.local")], false, 0);
        net.sync(&[], false, 1);
        net.sync(&[patch(1, "a.local")], false, 2);
        assert_eq!(net.running.len(), 1);
        assert!(
            net.lookups
                .get(&nid(1))
                .is_some_and(|l| l.phase == Phase::Wanted)
        );
        // Its old thread answers: the name is looked up again.
        let old = *net.running.keys().next().unwrap_or(&0);
        net.answer(
            NetAnswer::Lookup {
                patch: nid(1),
                ticket: old,
                result: Ok(vec![]),
            },
            3,
        );
        assert_eq!(net.running.len(), 1);
        assert_ne!(*net.running.keys().next().unwrap_or(&0), old);
    }

    #[test]
    fn removing_or_editing_a_patch_clears_its_state() {
        let (mut net, _rx) = out();
        net.sync(&[patch(1, "lights.local")], false, 0);
        let t = ticket(&net, 1);
        fail(&mut net, 1, t, 0);
        let r = net.sync(&[patch(1, "10.0.0.1")], false, 1);
        assert_eq!(r, vec![(nid(1), NetworkReachability::Unknown)]);
        assert!(net.lookups.is_empty());
    }

    #[test]
    fn a_show_with_very_many_named_patches_follows_a_bounded_number() {
        let (mut net, _rx) = out();
        let patches: Vec<NetworkPatch> = (0..(MAX_TRACKED as u128 + 50))
            .map(|n| patch(n + 1, &format!("a{n}.local")))
            .collect();
        net.sync(&patches, false, 0);
        assert_eq!(net.lookups.len(), MAX_TRACKED);
        assert!(net.running.len() <= MAX_LOOKUPS);
    }

    #[test]
    fn at_most_max_lookups_run_at_once() {
        let (mut net, _rx) = out();
        let patches: Vec<NetworkPatch> = (0..20)
            .map(|n| patch(n + 1, &format!("a{n}.local")))
            .collect();
        net.sync(&patches, false, 0);
        assert_eq!(net.running.len(), MAX_LOOKUPS);
    }

    #[test]
    fn disabled_output_sends_nothing() {
        let (mut net, _rx) = out();
        net.set_disabled(true);
        assert!(net.disabled());
        let o = net.send(&patch(1, "10.0.0.1"), CueId(Uuid::nil()), vec![1]);
        assert_eq!(o, SendOutcome::Suppressed);
        assert!(net.links.is_empty());
    }

    /// L2: an idle sender is dropped to make room once MAX_LINKS is reached.
    #[test]
    fn idle_senders_are_freed_when_the_limit_is_reached() {
        let (mut net, _rx) = out();
        for n in 0..(MAX_LINKS as u128 + 10) {
            let o = net.send(&patch(n + 1, "10.0.0.1"), CueId(Uuid::nil()), vec![1]);
            assert_eq!(o, SendOutcome::Queued, "{n}");
        }
        assert!(net.links.len() <= MAX_LINKS);
    }
}
