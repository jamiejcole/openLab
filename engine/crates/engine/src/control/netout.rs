// SPDX-License-Identifier: GPL-3.0-or-later

//! Network output (CT-30, PB-10, NFR-07): the sending of Network cues' OSC messages and the
//! look-ups of their hosts. Nothing here blocks the control thread. Each destination has a
//! worker thread with a small bounded queue, so a slow or dead one delays only its own
//! messages; a message that cannot be queued, or fails to send, comes back as a
//! [`NetAnswer::SendFailed`] and becomes a log entry and a warning, never a stall.
//!
//! Host names are looked up (name resolution only, nothing is sent to the host) when a
//! workspace opens or its network patches change, so a name that does not resolve breaks the
//! cues that use it before GO. A failed look-up is repeated every [`RETRY_AFTER`], because a
//! network that was not up yet when the show opened must not leave its cues broken for good.
//! There is no probing of the destination itself: UDP has no way to know, and QLab does not
//! either.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::Write;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

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
/// Destinations with a worker thread at once.
pub const MAX_LINKS: usize = 64;
/// Host names followed at once; a show with more than this many named patches has the rest
/// taken to be reachable (no look-up, no problem).
pub const MAX_TRACKED: usize = 256;
/// Host look-ups under way at once.
pub const MAX_LOOKUPS: usize = 8;
/// A failed look-up is repeated after this long (sample time).
pub const RETRY_AFTER: Duration = Duration::from_secs(10);
/// A look-up that has not answered by then counts as failed, and its thread is given up on.
pub const LOOKUP_TIMEOUT: Duration = Duration::from_secs(20);
/// Given-up look-up threads after which no more are started.
pub const MAX_ABANDONED: usize = 16;
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

/// The two things done on the network, replaceable in tests. Both block, and only ever run on
/// worker threads.
pub trait Transport: Send + Sync {
    /// Delivers one packet (UDP: a datagram; TCP: connects and writes the SLIP frame).
    fn send(&self, to: &Destination, packet: &[u8]) -> Result<(), String>;
    /// Whether `host` has an address: a name look-up only.
    fn resolve(&self, host: &str, port: u16) -> Result<(), String>;
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

impl Transport for SystemTransport {
    fn send(&self, to: &Destination, packet: &[u8]) -> Result<(), String> {
        let found = addresses(&to.host, to.port)?;
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

    fn resolve(&self, host: &str, port: u16) -> Result<(), String> {
        addresses(host, port).map(|_| ())
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
        result: Result<(), String>,
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
}

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
    /// Answered; looked up again at `retry_at` if set.
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
    disabled: bool,
    links: HashMap<NetworkPatchId, Link>,
    lookups: HashMap<NetworkPatchId, Lookup>,
    in_flight: usize,
    abandoned: usize,
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
            disabled: false,
            links: HashMap::new(),
            lookups: HashMap::new(),
            in_flight: 0,
            abandoned: 0,
            next_ticket: 0,
            outstanding: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Look-ups under way plus messages queued or being sent.
    pub fn busy(&self) -> usize {
        self.in_flight + self.outstanding.load(Ordering::SeqCst)
    }

    /// PB-10: whether Network cues send nothing. Runtime state of the engine, never saved.
    pub fn disabled(&self) -> bool {
        self.disabled
    }

    pub fn set_disabled(&mut self, disabled: bool) {
        self.disabled = disabled;
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
                            },
                        );
                    }
                }
            }
        }
        self.start_wanted(now);
        out
    }

    fn forget(&mut self, id: NetworkPatchId, out: &mut Reports) {
        if let Some(l) = self.lookups.remove(&id) {
            if matches!(l.phase, Phase::InFlight { .. }) {
                self.in_flight = self.in_flight.saturating_sub(1);
            }
            if l.reported != NetworkReachability::Unknown {
                out.push((id, NetworkReachability::Unknown));
            }
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
        let retry = now.saturating_add(self.samples(RETRY_AFTER));
        let Some(l) = self.lookups.get_mut(&patch) else {
            return Vec::new();
        };
        // An answer of a look-up that was replaced or given up on is not about this one.
        if !matches!(l.phase, Phase::InFlight { ticket: t, .. } if t == ticket) {
            return Vec::new();
        }
        self.in_flight = self.in_flight.saturating_sub(1);
        let (reached, retry_at) = match result {
            Ok(()) => (NetworkReachability::Resolved, None),
            Err(reason) => (NetworkReachability::Unresolved { reason }, Some(retry)),
        };
        l.phase = Phase::Idle { retry_at };
        Self::changed(l, patch, reached)
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
        let mut freed = 0;
        for (id, l) in &mut self.lookups {
            match l.phase {
                Phase::InFlight { since, .. } if now.saturating_sub(since) >= timeout => {
                    freed += 1;
                    self.abandoned += 1;
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
        self.in_flight = self.in_flight.saturating_sub(freed);
        self.start_wanted(now);
        out
    }

    fn start_wanted(&mut self, now: SampleTime) {
        if self.abandoned >= MAX_ABANDONED {
            return;
        }
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
            if self.in_flight >= MAX_LOOKUPS {
                break;
            }
            self.next_ticket = self.next_ticket.wrapping_add(1);
            let ticket = self.next_ticket;
            let Some(l) = self.lookups.get_mut(&id) else {
                continue;
            };
            let (host, port) = (l.host.clone(), l.port);
            let transport = Arc::clone(&self.transport);
            let inbox = self.inbox.clone();
            let spawned = thread::Builder::new()
                .name("openlab-net-lookup".into())
                .spawn(move || {
                    let result = transport.resolve(&host, port);
                    let _ = inbox.send(Inbox::Net(NetAnswer::Lookup {
                        patch: id,
                        ticket,
                        result,
                    }));
                });
            match spawned {
                Ok(_) => {
                    l.phase = Phase::InFlight { ticket, since: now };
                    self.in_flight += 1;
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
        if self.disabled {
            return SendOutcome::Suppressed;
        }
        let to = Destination::of(patch);
        if self.links.get(&patch.id).is_none_or(|l| l.to != to) {
            if self.links.len() >= MAX_LINKS && !self.links.contains_key(&patch.id) {
                return SendOutcome::Refused("too many network destinations".to_owned());
            }
            match self.spawn_link(to) {
                Ok(link) => {
                    self.links.insert(patch.id, link);
                }
                Err(e) => return SendOutcome::Refused(format!("could not start a sender: {e}")),
            }
        }
        let Some(link) = self.links.get(&patch.id) else {
            return SendOutcome::Refused("no sender".to_owned());
        };
        self.outstanding.fetch_add(1, Ordering::SeqCst);
        match link.tx.try_send(Job { cue, packet }) {
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

    fn spawn_link(&self, to: Destination) -> std::io::Result<Link> {
        let (tx, rx) = bounded::<Job>(LINK_QUEUE);
        let transport = Arc::clone(&self.transport);
        let inbox = self.inbox.clone();
        let dest = to.clone();
        let outstanding = Arc::clone(&self.outstanding);
        thread::Builder::new()
            .name("openlab-net-send".into())
            .spawn(move || {
                // Ends when the control thread drops the link (a destination that changed, or
                // shutdown), after sending what is queued.
                while let Ok(job) = rx.recv() {
                    if let Err(reason) = transport.send(&dest, &job.packet) {
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

    struct Fake(Mutex<Vec<(String, Result<(), String>)>>);
    impl Transport for Fake {
        fn send(&self, _: &Destination, _: &[u8]) -> Result<(), String> {
            Ok(())
        }
        fn resolve(&self, host: &str, _: u16) -> Result<(), String> {
            self.0
                .lock()
                .map_err(|_| "poisoned".to_owned())?
                .push((host.to_owned(), Ok(())));
            Ok(())
        }
    }

    fn out() -> (NetOut, crossbeam_channel::Receiver<Inbox>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let t = NetTransport(Arc::new(Fake(Mutex::new(Vec::new()))));
        (NetOut::new(t, tx, 1000), rx)
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
        // /a/b, ,ifsT, 1, 0.5, "hi" padded.
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
    fn sync_looks_up_names_only_and_reports_changes() {
        let (mut net, _rx) = out();
        let patches = [
            patch(1, "lights.local"),
            patch(2, "10.0.0.2"),
            patch(3, "bad host"),
        ];
        // Nothing to report before an answer; only the name is looked up.
        assert!(
            net.sync(&patches, true, 0)
                .iter()
                .all(|(id, r)| { *id == nid(1) && *r == NetworkReachability::Unknown })
        );
        assert_eq!(net.lookups.len(), 1);
        assert_eq!(net.in_flight, 1);
        let Some(Phase::InFlight { ticket, .. }) = net.lookups.get(&nid(1)).map(|l| &l.phase)
        else {
            panic!("not in flight");
        };
        let ticket = *ticket;
        let failed = NetAnswer::Lookup {
            patch: nid(1),
            ticket,
            result: Err("no such host".into()),
        };
        assert_eq!(
            net.answer(failed, 100),
            vec![(
                nid(1),
                NetworkReachability::Unresolved {
                    reason: "no such host".into()
                }
            )]
        );
        assert_eq!(net.in_flight, 0);
        // The same failure again is no news; a stale ticket is ignored.
        let stale = NetAnswer::Lookup {
            patch: nid(1),
            ticket: ticket + 99,
            result: Ok(()),
        };
        assert!(net.answer(stale, 101).is_empty());
        // Retried after RETRY_AFTER (10 s = 10_000 samples at 1 kHz), not before.
        assert!(net.tick(9_000).is_empty());
        assert_eq!(net.in_flight, 0);
        net.tick(10_100);
        assert_eq!(net.in_flight, 1);
    }

    #[test]
    fn a_look_up_that_never_answers_is_given_up_on() {
        let (mut net, _rx) = out();
        net.sync(&[patch(1, "lights.local")], false, 0);
        assert_eq!(net.in_flight, 1);
        assert!(net.tick(19_000).is_empty());
        let reports = net.tick(20_000);
        assert!(matches!(
            reports.as_slice(),
            [(_, NetworkReachability::Unresolved { .. })]
        ));
        assert_eq!(net.in_flight, 0);
        assert_eq!(net.abandoned, 1);
    }

    #[test]
    fn removing_or_editing_a_patch_clears_its_state() {
        let (mut net, _rx) = out();
        net.sync(&[patch(1, "lights.local")], false, 0);
        let Some(Phase::InFlight { ticket, .. }) = net.lookups.get(&nid(1)).map(|l| &l.phase)
        else {
            panic!("not in flight");
        };
        let ticket = *ticket;
        net.answer(
            NetAnswer::Lookup {
                patch: nid(1),
                ticket,
                result: Err("x".into()),
            },
            0,
        );
        // Edited to an address: nothing to look up, and the engine hears it is fine again.
        let r = net.sync(&[patch(1, "10.0.0.1")], false, 1);
        assert_eq!(r, vec![(nid(1), NetworkReachability::Unknown)]);
        assert!(net.lookups.is_empty());
    }

    #[test]
    fn a_show_with_very_many_named_patches_follows_a_bounded_number() {
        let (mut net, _rx) = out();
        let patches: Vec<NetworkPatch> = (0..(MAX_TRACKED as u128 + 50))
            .map(|n| patch(n + 1, "a.local"))
            .collect();
        net.sync(&patches, false, 0);
        assert_eq!(net.lookups.len(), MAX_TRACKED);
        assert!(net.in_flight <= MAX_LOOKUPS);
    }

    #[test]
    fn at_most_max_lookups_run_at_once() {
        let (mut net, _rx) = out();
        let patches: Vec<NetworkPatch> = (0..20).map(|n| patch(n + 1, "a.local")).collect();
        net.sync(&patches, false, 0);
        assert_eq!(net.in_flight, MAX_LOOKUPS);
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
}
