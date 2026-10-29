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
//! A name that has resolved is looked up again every [`REFRESH_AFTER`] (sample time, through
//! the same slots and limits as retries), so a host whose address changes (a DHCP lease, a
//! console that rebooted) is not sent to at its old address for the rest of the session. A
//! TCP send that fails clears the destination's addresses and asks for a look-up at once. A
//! refresh that fails for a name that resolved before keeps the last-known addresses and does
//! not break its cues (one log line per run of failures): a DNS blip must not break a working
//! show. Only a name that never resolved is `Unresolved`.
//!
//! Each sender owns a snapshot cache of the addresses, so a replaced sender's old worker never
//! reads the new destination's addresses.
//!
//! Look-up threads are bounded: at most [`MAX_LOOKUPS`] are alive at once, counting those that
//! timed out or whose name was forgotten until they answer, and at most one per (host, port).
//! If hung look-ups fill every slot, the names waiting for one are reported `Unknown` (their
//! cues are not broken on a guess) and [`NetOut::take_limit_note`] says so once. The bound is
//! the engine's: each doc has its own `NetOut` (and send threads), but they count their live
//! look-up threads together ([`LookupThreads`]), and the PB-10 override is one flag for every
//! doc.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::Write;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Sender, TrySendError, bounded};
use openlab_core::{
    CueId, HostKind, NetworkPatch, NetworkPatchId, NetworkProtocol, NetworkReachability, OscArg,
    SampleTime, check_endpoint,
};
use rosc::{OscMessage, OscType};

use super::{DocId, Inbox};
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
/// A name that resolved is looked up again after this long (sample time), to follow a host
/// whose address changed.
pub const REFRESH_AFTER: Duration = Duration::from_secs(60);
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

fn set_cache(cache: &Resolved, addrs: &[SocketAddr]) {
    let mut c = crate::lock(cache);
    c.clear();
    c.extend_from_slice(addrs);
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
        patch: NetworkPatchId,
        destination: Destination,
        reason: String,
        /// The transport failed (as opposed to a message dropped as late): for TCP this
        /// suggests the cached addresses are stale.
        from_transport: bool,
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
    /// This link's own copy of the addresses; the control thread keeps it current while the
    /// link is the patch's current one.
    cache: Resolved,
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
    /// When this name's last look-up started; `None` until its first. Free slots go to the
    /// longest-waiting name, so hung look-ups holding most slots cannot starve the rest.
    last_start: Option<SampleTime>,
    /// The addresses the last successful look-up found (empty before one, or after a TCP
    /// failure cleared them).
    known: Vec<SocketAddr>,
    /// A look-up has succeeded for this name: later failures keep `known` and do not break
    /// its cues.
    resolved_once: bool,
    /// The current run of failed refreshes has been logged.
    failing: bool,
}

/// Look-up slots taken in the whole engine, every doc's (at most [`MAX_LOOKUPS`]). A look-up
/// holds its slot until its doc has taken its answer, as within one doc; a closed doc's
/// look-ups hold theirs until their thread ends (they may hang).
#[derive(Clone, Default)]
pub struct LookupThreads(Arc<AtomicUsize>);

impl LookupThreads {
    pub fn alive(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

/// One look-up's slot, given back exactly once: by its doc when the answer is taken, or, once
/// the doc is gone, by the thread when it ends (whichever of the doc's drop and the thread's
/// end comes second sees the other's flag).
struct Slot {
    count: Arc<AtomicUsize>,
    given_back: AtomicBool,
    ended: AtomicBool,
    orphaned: AtomicBool,
}

impl Slot {
    fn give_back(&self) {
        if !self.given_back.swap(true, Ordering::SeqCst) {
            self.count.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// Marks a look-up thread ended (a panic in the resolver included).
struct Ended(Arc<Slot>);

impl Drop for Ended {
    fn drop(&mut self) {
        self.0.ended.store(true, Ordering::SeqCst);
        if self.0.orphaned.load(Ordering::SeqCst) {
            self.0.give_back();
        }
    }
}

/// The control thread's side of one doc's network output.
pub struct NetOut {
    doc: DocId,
    transport: Arc<dyn Transport>,
    inbox: Sender<Inbox>,
    rate: u64,
    /// Shared with the workers, so turning the override on also stops what is queued.
    disabled: Arc<AtomicBool>,
    links: HashMap<NetworkPatchId, Link>,
    lookups: HashMap<NetworkPatchId, Lookup>,
    /// This doc's look-up threads that have not answered yet, by ticket.
    running: HashMap<u64, (String, u16)>,
    /// Every doc's look-up slots taken: the bound on threads.
    alive: LookupThreads,
    /// This doc's slots, by ticket (as `running`).
    slots: HashMap<u64, Arc<Slot>>,
    limit_hit: bool,
    limit_note: bool,
    /// Log lines for the caller ([`NetOut::take_notes`]).
    notes: Vec<String>,
    next_ticket: u64,
    /// Messages queued or being sent; a worker counts one off after it has reported any
    /// failure, so zero means every answer is already in the inbox (the offline driver waits
    /// on it).
    outstanding: Arc<AtomicUsize>,
}

impl Drop for NetOut {
    /// A closed doc's look-ups in flight keep their slots until their threads end.
    fn drop(&mut self) {
        for slot in self.slots.values() {
            slot.orphaned.store(true, Ordering::SeqCst);
            if slot.ended.load(Ordering::SeqCst) {
                slot.give_back();
            }
        }
    }
}

/// Reachability changes for `Input::NetworkAvailability`.
pub type Reports = Vec<(NetworkPatchId, NetworkReachability)>;

impl NetOut {
    /// Network output with an override and a look-up bound of its own (unit tests of one).
    #[cfg(test)]
    pub fn new(transport: NetTransport, inbox: Sender<Inbox>, sample_rate: u32) -> Self {
        Self::shared(
            transport,
            inbox,
            sample_rate,
            DocId(0),
            Arc::new(AtomicBool::new(false)),
            LookupThreads::default(),
        )
    }

    /// Doc `doc`'s network output, under the engine's PB-10 override `disabled` and look-up
    /// bound `alive`.
    pub fn shared(
        transport: NetTransport,
        inbox: Sender<Inbox>,
        sample_rate: u32,
        doc: DocId,
        disabled: Arc<AtomicBool>,
        alive: LookupThreads,
    ) -> Self {
        Self {
            doc,
            transport: transport.0,
            inbox,
            rate: u64::from(sample_rate.max(1)),
            disabled,
            links: HashMap::new(),
            lookups: HashMap::new(),
            running: HashMap::new(),
            alive,
            slots: HashMap::new(),
            limit_hit: false,
            limit_note: false,
            notes: Vec::new(),
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
    /// send. (The engine sets its shared flag; this is for unit tests of one.)
    #[cfg(test)]
    pub fn set_disabled(&mut self, disabled: bool) {
        self.disabled.store(disabled, Ordering::SeqCst);
    }

    /// True once each time every look-up slot was found taken by hung look-ups (the caller
    /// logs it).
    pub fn take_limit_note(&mut self) -> bool {
        std::mem::take(&mut self.limit_note)
    }

    /// Log lines (warnings) since the last call.
    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
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
                                last_start: None,
                                known: Vec::new(),
                                resolved_once: false,
                                failing: false,
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
        if let Some(link) = self.links.get(&id) {
            set_cache(&link.cache, &[]);
        }
        if let Some(l) = self.lookups.remove(&id)
            && l.reported != NetworkReachability::Unknown
        {
            out.push((id, NetworkReachability::Unknown));
        }
    }

    /// A look-up finished (or the thread gave up), or a send failed. Returns what changed for
    /// the cue engine.
    pub fn answer(&mut self, answer: NetAnswer, now: SampleTime) -> Reports {
        let (patch, ticket, result) = match answer {
            NetAnswer::Lookup {
                patch,
                ticket,
                result,
            } => (patch, ticket, result),
            NetAnswer::SendFailed {
                patch,
                destination,
                from_transport,
                ..
            } => {
                if from_transport && destination.protocol == NetworkProtocol::Tcp {
                    self.stale(patch, &destination, now);
                }
                return Vec::new();
            }
        };
        // The thread is gone, whatever it was for: its slot is free again.
        self.running.remove(&ticket);
        if let Some(slot) = self.slots.remove(&ticket) {
            slot.give_back();
        }
        let retry = now.saturating_add(self.samples(RETRY_AFTER));
        let refresh = now.saturating_add(self.samples(REFRESH_AFTER));
        let mut out = Reports::new();
        // An answer of a look-up that was replaced or given up on is not about this one.
        if let Some(l) = self.lookups.get_mut(&patch)
            && matches!(l.phase, Phase::InFlight { ticket: t, .. } if t == ticket)
        {
            match result {
                Ok(addrs) => {
                    l.known.clone_from(&addrs);
                    l.resolved_once = true;
                    l.failing = false;
                    l.phase = Phase::Idle {
                        retry_at: Some(refresh),
                    };
                    if let Some(link) = self.links.get(&patch)
                        && link.to.host == l.host
                        && link.to.port == l.port
                    {
                        set_cache(&link.cache, &addrs);
                    }
                    out.extend(Self::changed(l, patch, NetworkReachability::Resolved));
                }
                Err(reason) => {
                    l.phase = Phase::Idle {
                        retry_at: Some(retry),
                    };
                    if l.resolved_once {
                        Self::note_failed_refresh(l, &reason, &mut self.notes);
                    } else {
                        out.extend(Self::changed(
                            l,
                            patch,
                            NetworkReachability::Unresolved { reason },
                        ));
                    }
                }
            }
        }
        self.start_wanted(now);
        out.extend(self.relieve());
        out
    }

    /// A refresh that fails keeps the last-known addresses; the first failure of a run is logged.
    fn note_failed_refresh(l: &mut Lookup, reason: &str, notes: &mut Vec<String>) {
        if !l.failing {
            l.failing = true;
            notes.push(format!(
                "could not look up {} again ({reason}); keeping its last known addresses",
                l.host
            ));
        }
    }

    /// A TCP send to `to` failed: the addresses it used may be stale, so forget them (the
    /// sender then resolves the name itself) and look the name up again now.
    fn stale(&mut self, patch: NetworkPatchId, to: &Destination, now: SampleTime) {
        let Some(l) = self.lookups.get_mut(&patch) else {
            return;
        };
        if l.host != to.host || l.port != to.port {
            return;
        }
        l.known.clear();
        if let Some(link) = self.links.get(&patch)
            && link.to.host == l.host
            && link.to.port == l.port
        {
            set_cache(&link.cache, &[]);
        }
        if matches!(l.phase, Phase::Idle { .. }) {
            l.phase = Phase::Wanted;
        }
        self.start_wanted(now);
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
                    if l.resolved_once {
                        Self::note_failed_refresh(l, "the look-up timed out", &mut self.notes);
                    } else {
                        out.extend(Self::changed(
                            l,
                            *id,
                            NetworkReachability::Unresolved {
                                reason: "the look-up timed out".to_owned(),
                            },
                        ));
                    }
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
    /// Every look-up slot is taken: this doc's, or the engine's together.
    fn slots_full(&self) -> bool {
        self.running.len() >= MAX_LOOKUPS || self.alive.alive() >= MAX_LOOKUPS
    }

    fn relieve(&mut self) -> Reports {
        if !self.slots_full() {
            self.limit_hit = false;
            return Vec::new();
        }
        let mut out = Reports::new();
        for (id, l) in &mut self.lookups {
            // A name that resolved before is already known; waiting does not change that.
            if l.phase == Phase::Wanted && !l.resolved_once {
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
        let mut wanted: Vec<(Option<SampleTime>, NetworkPatchId)> = self
            .lookups
            .iter()
            .filter(|(_, l)| l.phase == Phase::Wanted)
            .map(|(id, l)| (l.last_start, *id))
            .collect();
        // Never-tried names first, then the one tried longest ago (id breaks ties), so a few
        // slots serve every name in turn.
        wanted.sort();
        let retry_at = now.saturating_add(self.samples(RETRY_AFTER));
        for (_, id) in wanted {
            if self.slots_full() {
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
            let doc = self.doc;
            self.alive.0.fetch_add(1, Ordering::SeqCst);
            let slot = Arc::new(Slot {
                count: Arc::clone(&self.alive.0),
                given_back: AtomicBool::new(false),
                ended: AtomicBool::new(false),
                orphaned: AtomicBool::new(false),
            });
            let ended = Ended(Arc::clone(&slot));
            let spawned = thread::Builder::new()
                .name("openlab-net-lookup".into())
                .spawn(move || {
                    let _ended = ended;
                    let result = transport.resolve(&name, port);
                    let _ = inbox.send(Inbox::Net(
                        doc,
                        NetAnswer::Lookup {
                            patch: id,
                            ticket,
                            result,
                        },
                    ));
                });
            match spawned {
                Ok(_) => {
                    l.phase = Phase::InFlight { ticket, since: now };
                    l.last_start = Some(now);
                    self.running.insert(ticket, (host, port));
                    self.slots.insert(ticket, slot);
                }
                Err(_) => {
                    slot.give_back();
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
        let known = self
            .lookups
            .get(&patch)
            .filter(|l| l.host == to.host && l.port == to.port)
            .map(|l| l.known.clone())
            .unwrap_or_default();
        let cache: Resolved = Arc::new(Mutex::new(known));
        let (tx, rx) = bounded::<Job>(LINK_QUEUE);
        let transport = Arc::clone(&self.transport);
        let inbox = self.inbox.clone();
        let dest = to.clone();
        let outstanding = Arc::clone(&self.outstanding);
        let disabled = Arc::clone(&self.disabled);
        let resolved = Arc::clone(&cache);
        let doc = self.doc;
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
                        Err(("destination too late".to_owned(), false))
                    } else {
                        let found = crate::lock(&resolved).clone();
                        transport
                            .send(&dest, &found, &job.packet)
                            .map_err(|e| (e, true))
                    };
                    if let Err((reason, from_transport)) = outcome {
                        let _ = inbox.send(Inbox::Net(
                            doc,
                            NetAnswer::SendFailed {
                                cue: job.cue,
                                patch,
                                destination: dest.clone(),
                                reason,
                                from_transport,
                            },
                        ));
                    }
                    outstanding.fetch_sub(1, Ordering::SeqCst);
                }
            })?;
        Ok(Link { to, tx, cache })
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
        // The slot went to a name that had never been tried, not back to one of the hung.
        let fresh = (9..=12)
            .filter(|n| {
                net.lookups
                    .get(&nid(*n))
                    .is_some_and(|l| l.last_start.is_some())
            })
            .count();
        assert_eq!(fresh, 1);
    }

    /// L1: free slots go to never-tried names first, then to the name tried longest ago.
    #[test]
    fn free_slots_go_to_the_longest_waiting_name_first() {
        let (mut net, _rx) = out();
        let mut patches: Vec<NetworkPatch> =
            (1..=8).map(|n| patch(n, &format!("h{n}.local"))).collect();
        net.sync(&patches, false, 0);
        patches.extend((20..=22).map(|n| patch(n, &format!("w{n}.local"))));
        net.sync(&patches, false, 10);
        assert_eq!(net.running.len(), MAX_LOOKUPS);
        // x (20) was tried at 500, y (21) at 100, z (22) never.
        for (n, at) in [(20, Some(500)), (21, Some(100)), (22, None)] {
            if let Some(l) = net.lookups.get_mut(&nid(n)) {
                l.last_start = at;
            }
        }
        let started = |net: &NetOut, n: u128| {
            matches!(
                net.lookups.get(&nid(n)).map(|l| &l.phase),
                Some(Phase::InFlight { .. })
            )
        };
        let mut order: Vec<u128> = Vec::new();
        for (i, hung) in [1u128, 2, 3].into_iter().enumerate() {
            let t = ticket(&net, hung);
            fail(&mut net, hung, t, 1_000 + i as u64);
            for n in [22u128, 21, 20] {
                if started(&net, n) && !order.contains(&n) {
                    order.push(n);
                }
            }
        }
        assert_eq!(order, vec![22, 21, 20]);
    }

    fn a(n: u8) -> SocketAddr {
        SocketAddr::from((Ipv4Addr::new(10, 0, 0, n), 9000))
    }

    /// Records what each send was given and can be told to fail.
    #[derive(Default)]
    struct Rec {
        sent: Mutex<Vec<Vec<SocketAddr>>>,
        fail: AtomicBool,
    }
    impl Transport for Rec {
        fn send(&self, _: &Destination, resolved: &[SocketAddr], _: &[u8]) -> Result<(), String> {
            crate::lock(&self.sent).push(resolved.to_vec());
            if self.fail.load(Ordering::SeqCst) {
                Err("refused".into())
            } else {
                Ok(())
            }
        }
        fn resolve(&self, _: &str, _: u16) -> Result<Vec<SocketAddr>, String> {
            Ok(vec![])
        }
    }

    fn out_rec() -> (NetOut, crossbeam_channel::Receiver<Inbox>, Arc<Rec>) {
        let rec = Arc::new(Rec::default());
        let (tx, rx) = crossbeam_channel::unbounded();
        let net = NetOut::new(NetTransport(rec.clone()), tx, 1000);
        (net, rx, rec)
    }

    fn resolve_ok(net: &mut NetOut, n: u128, addrs: Vec<SocketAddr>, now: SampleTime) -> Reports {
        let t = ticket(net, n);
        net.answer(
            NetAnswer::Lookup {
                patch: nid(n),
                ticket: t,
                result: Ok(addrs),
            },
            now,
        )
    }

    /// Waits (bounded, no sleeping) until every queued message has been handled.
    fn drained(net: &NetOut) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while net.outstanding.load(Ordering::SeqCst) > 0 {
            assert!(Instant::now() < deadline, "workers never drained");
            thread::yield_now();
        }
    }

    fn send_one(net: &mut NetOut, p: &NetworkPatch) {
        assert_eq!(
            net.send(p, CueId(Uuid::nil()), vec![1]),
            SendOutcome::Queued
        );
        drained(net);
    }

    fn sent(rec: &Rec) -> Vec<Vec<SocketAddr>> {
        crate::lock(&rec.sent).clone()
    }

    fn cache_of(net: &NetOut, n: u128) -> Option<Vec<SocketAddr>> {
        net.links
            .get(&nid(n))
            .map(|l| crate::lock(&l.cache).clone())
    }

    /// M1(a): a host that moves is followed: the refresh every 60 s of sample time updates
    /// the addresses a live sender uses.
    #[test]
    fn a_refresh_sends_to_the_new_address() {
        let (mut net, _rx, rec) = out_rec();
        let p = patch(1, "console.local");
        net.sync(std::slice::from_ref(&p), false, 0);
        assert_eq!(
            resolve_ok(&mut net, 1, vec![a(1)], 0),
            vec![(nid(1), NetworkReachability::Resolved)]
        );
        send_one(&mut net, &p);
        assert_eq!(sent(&rec), vec![vec![a(1)]]);
        // Not due yet.
        net.tick(59_000);
        assert!(net.running.is_empty());
        net.tick(60_000);
        assert_eq!(net.running.len(), 1, "the refresh started");
        // The cue stays reachable while it runs and when it answers.
        assert!(resolve_ok(&mut net, 1, vec![a(2)], 60_100).is_empty());
        send_one(&mut net, &p);
        assert_eq!(sent(&rec)[1], vec![a(2)]);
        // And it keeps refreshing.
        net.tick(120_200);
        assert_eq!(net.running.len(), 1);
    }

    /// M1(c): a refresh that fails keeps the last-known addresses and does not break cues; one
    /// log line per run of failures; a never-resolved name is still broken.
    #[test]
    fn a_failed_refresh_keeps_the_old_addresses_and_does_not_break_cues() {
        let (mut net, _rx, rec) = out_rec();
        let p = patch(1, "console.local");
        net.sync(std::slice::from_ref(&p), false, 0);
        resolve_ok(&mut net, 1, vec![a(1)], 0);
        send_one(&mut net, &p);
        net.tick(60_000);
        let t = ticket(&net, 1);
        assert!(fail(&mut net, 1, t, 60_100).is_empty(), "not broken");
        assert_eq!(net.take_notes().len(), 1);
        send_one(&mut net, &p);
        assert_eq!(sent(&rec)[1], vec![a(1)], "still the last-known address");
        // Retried after RETRY_AFTER; failing again (here by timing out) is not logged again,
        // and the cue is still not broken.
        assert!(net.tick(70_200).is_empty());
        assert_eq!(net.running.len(), 1);
        let slow = ticket(&net, 1);
        assert!(net.tick(90_300).is_empty());
        // The slow thread answers at last (stale: ignored), freeing its name for a new try.
        assert!(fail(&mut net, 1, slow, 90_400).is_empty());
        assert!(net.take_notes().is_empty(), "one line per run of failures");
        assert_eq!(
            net.lookups.get(&nid(1)).map(|l| l.reported.clone()),
            Some(NetworkReachability::Resolved)
        );
        // A success ends the run: the next failure is logged again.
        net.tick(100_400);
        assert!(resolve_ok(&mut net, 1, vec![a(3)], 100_500).is_empty());
        net.tick(160_500);
        let t = ticket(&net, 1);
        assert!(fail(&mut net, 1, t, 160_600).is_empty());
        assert_eq!(net.take_notes().len(), 1);
        // A name that never resolved is Unresolved.
        net.sync(&[p, patch(2, "never.local")], false, 160_700);
        let t = ticket(&net, 2);
        assert_eq!(
            fail(&mut net, 2, t, 160_800),
            vec![(nid(2), unresolved("no such host"))]
        );
    }

    /// M1(b): a failed TCP send forgets the cached addresses and asks for a look-up now; a
    /// failed UDP send (which proves nothing about the address) does not.
    #[test]
    fn a_tcp_send_failure_triggers_a_fresh_look_up() {
        for (protocol, relookup) in [(NetworkProtocol::Tcp, true), (NetworkProtocol::Udp, false)] {
            let (mut net, rx, rec) = out_rec();
            let mut p = patch(1, "console.local");
            p.protocol = protocol;
            net.sync(std::slice::from_ref(&p), false, 0);
            resolve_ok(&mut net, 1, vec![a(1)], 0);
            rec.fail.store(true, Ordering::SeqCst);
            send_one(&mut net, &p);
            let failed = loop {
                match rx.recv() {
                    Ok(Inbox::Net(_, f @ NetAnswer::SendFailed { .. })) => break f,
                    Ok(_) => {}
                    Err(e) => panic!("{e}"),
                }
            };
            assert!(net.answer(failed, 5_000).is_empty());
            assert_eq!(net.running.len(), usize::from(relookup));
            let want = if relookup { vec![] } else { vec![a(1)] };
            assert_eq!(cache_of(&net, 1), Some(want));
            if relookup {
                // The fresh look-up finds the host at its new address, and the sender uses it.
                rec.fail.store(false, Ordering::SeqCst);
                resolve_ok(&mut net, 1, vec![a(9)], 5_100);
                send_one(&mut net, &p);
                assert_eq!(sent(&rec).last(), Some(&vec![a(9)]));
            }
        }
    }

    /// L2: a replaced link has its own cache; the old worker never sees the new
    /// destination's addresses, even from a look-up answer that arrives before the first send.
    #[test]
    fn a_replaced_link_does_not_share_its_cache() {
        let (mut net, _rx, rec) = out_rec();
        let p = patch(1, "old.local");
        net.sync(std::slice::from_ref(&p), false, 0);
        resolve_ok(&mut net, 1, vec![a(1)], 0);
        send_one(&mut net, &p);
        let old = net.links.get(&nid(1)).map(|l| Arc::clone(&l.cache));
        let Some(old) = old else { panic!("no link") };
        // The patch now names another host; its look-up answers before any message is sent.
        let q = patch(1, "new.local");
        net.sync(std::slice::from_ref(&q), false, 10);
        resolve_ok(&mut net, 1, vec![a(2)], 20);
        assert_ne!(*crate::lock(&old), vec![a(2)]);
        send_one(&mut net, &q);
        assert_eq!(sent(&rec).last(), Some(&vec![a(2)]));
        let new = net.links.get(&nid(1)).map(|l| Arc::clone(&l.cache));
        assert!(new.is_some_and(|n| !Arc::ptr_eq(&n, &old)));
        assert_ne!(*crate::lock(&old), vec![a(2)]);
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

    /// Look-ups that wait until released.
    #[derive(Default)]
    struct Hang {
        open: Mutex<bool>,
        cv: std::sync::Condvar,
    }

    impl Hang {
        fn release(&self) {
            *self.open.lock().unwrap() = true;
            self.cv.notify_all();
        }
    }

    impl Transport for Hang {
        fn send(&self, _: &Destination, _: &[SocketAddr], _: &[u8]) -> Result<(), String> {
            Ok(())
        }
        fn resolve(&self, _: &str, _: u16) -> Result<Vec<SocketAddr>, String> {
            let mut open = self.open.lock().unwrap();
            while !*open {
                open = self.cv.wait(open).unwrap();
            }
            Ok(vec![])
        }
    }

    /// The bound on look-up threads is the engine's: two docs' network outputs share it, and a
    /// thread counts itself off when it ends, whichever doc it was for.
    #[test]
    fn two_docs_share_the_look_up_bound() {
        let hang = Arc::new(Hang::default());
        let alive = LookupThreads::default();
        let disabled = Arc::new(AtomicBool::new(false));
        let (tx, rx) = crossbeam_channel::unbounded();
        let make = |doc| {
            NetOut::shared(
                NetTransport(Arc::clone(&hang) as Arc<dyn Transport>),
                tx.clone(),
                1000,
                DocId(doc),
                Arc::clone(&disabled),
                alive.clone(),
            )
        };
        let (mut a, mut b) = (make(0), make(1));
        let names = |base: u128| -> Vec<NetworkPatch> {
            (0..6)
                .map(|n| patch(base + n, &format!("d{base}-{n}.local")))
                .collect()
        };
        a.sync(&names(10), false, 0);
        b.sync(&names(20), false, 0);
        assert_eq!(a.running.len(), 6);
        assert_eq!(
            b.running.len(),
            MAX_LOOKUPS - 6,
            "only the engine's slots left"
        );
        assert_eq!(alive.alive(), MAX_LOOKUPS);
        // The override is one flag: turned on through one doc, the other sends nothing.
        a.disabled.store(true, Ordering::SeqCst);
        assert!(b.disabled());
        hang.release();
        // Each answer is tagged with the doc that asked; a slot is free once its doc took it,
        // and B's waiting names take the free slots as answers come in.
        let mut tagged = Vec::new();
        while a.running.len() + b.running.len() > 0 {
            match rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap() {
                Inbox::Net(doc, answer) => {
                    if let NetAnswer::Lookup { patch, .. } = &answer {
                        tagged.push((doc, *patch));
                    }
                    let net = if doc == DocId(0) { &mut a } else { &mut b };
                    net.answer(answer, 1);
                    assert!(alive.alive() <= MAX_LOOKUPS);
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(tagged.iter().filter(|(d, _)| *d == DocId(0)).count(), 6);
        assert!(
            tagged
                .iter()
                .all(|(d, p)| (*d == DocId(0)) == (p.0.as_u128() < 20))
        );
        assert_eq!(alive.alive(), 0);
        // A doc closed with a look-up hung: the slot stays taken until the thread ends.
        let held = Arc::new(Hang::default());
        let mut c = NetOut::shared(
            NetTransport(Arc::clone(&held) as Arc<dyn Transport>),
            tx.clone(),
            1000,
            DocId(2),
            Arc::clone(&disabled),
            alive.clone(),
        );
        c.sync(&[patch(99, "late.local")], false, 0);
        assert_eq!(alive.alive(), 1);
        drop(c);
        assert_eq!(
            alive.alive(),
            1,
            "a hung thread of a closed doc still counts"
        );
        held.release();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while alive.alive() > 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the thread never ended"
            );
            std::thread::yield_now();
        }
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
        let limit = MAX_LINKS as u128;
        for n in 0..limit {
            let o = net.send(&patch(n + 1, "10.0.0.1"), CueId(Uuid::nil()), vec![1]);
            assert_eq!(o, SendOutcome::Queued, "{n}");
        }
        // The workers drain on their own threads; wait until every queue is empty, so the
        // senders are idle (bounded: a failure guard, not a timing assumption).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while net.links.values().any(|l| !l.tx.is_empty()) {
            assert!(
                std::time::Instant::now() < deadline,
                "workers never drained"
            );
            std::thread::yield_now();
        }
        for n in limit..limit + 10 {
            let o = net.send(&patch(n + 1, "10.0.0.1"), CueId(Uuid::nil()), vec![1]);
            assert_eq!(o, SendOutcome::Queued, "{n}");
        }
        assert!(net.links.len() <= MAX_LINKS);
    }
}
