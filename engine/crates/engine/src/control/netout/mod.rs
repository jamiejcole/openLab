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
}

mod lookup;
mod send;
mod transport;

pub use transport::*;

#[cfg(test)]
mod tests;

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
