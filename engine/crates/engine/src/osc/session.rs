// SPDX-License-Identifier: GPL-3.0-or-later

//! Per-peer OSC state: whether the peer has given the passcode (`/connect`) and whether it wants
//! updates (`/updates 1`), plus the guards around them: failed-passcode tracking per remote IP
//! and for all peers together, and a per-IP budget of reply bytes. Pure logic: the caller passes
//! the time (a monotonic `Duration`).
//!
//! A peer only gets a session by succeeding at `/connect` or by subscribing, so stray traffic
//! from spoofed UDP sources costs a table slot only if it subscribes (possible only without a
//! passcode, which `docs/osc.md` says is for trusted networks).

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;

use openlab_core::{OscAccess, OscRights};

/// Who sent a message. A UDP peer is its IP address (the source port changes per sender
/// socket, and replies go to port 53001 unless the peer set another with `/udpReplyPort`); a TCP peer is the connection, with the
/// remote address so failures count per machine, not per connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Peer {
    Udp(IpAddr),
    Tcp {
        /// Caller-chosen connection number, unique while the connection lives.
        conn: u64,
        ip: IpAddr,
    },
}

impl Peer {
    pub fn ip(self) -> IpAddr {
        match self {
            Self::Udp(ip) | Self::Tcp { ip, .. } => ip,
        }
    }
}

/// Most peers with a session. Entries are never evicted to make room (a flood of spoofed
/// subscribers must not push out a real console): a full table refuses newcomers until idle
/// UDP peers expire or TCP connections close.
pub const MAX_PEERS: usize = 1024;
/// A UDP peer silent this long is forgotten (there is no disconnect on UDP): its `/connect` and
/// its update subscription end, as in QLab 5 (measured: gone after 60 s of silence).
pub const UDP_IDLE: Duration = Duration::from_secs(60);
/// Wrong passcodes from one IP (connected or not; a correct one does not reset the count)
/// before that IP is locked out.
pub const MAX_BAD_PASSCODES: u32 = 5;
/// How long a lockout lasts; attempts during it are refused without being checked.
pub const LOCKOUT: Duration = Duration::from_secs(30);
/// Wrong passcodes from all IPs together allowed per [`GLOBAL_WINDOW`]; past it every new
/// `/connect` is refused until the window ends (so rotating source addresses does not give
/// unlimited guesses). Peers that are already connected are not affected.
pub const GLOBAL_MAX_FAILURES: u32 = 50;
pub const GLOBAL_WINDOW: Duration = Duration::from_secs(60);
/// Addresses remembered as having connected. A correct passcode from one of them passes the
/// global lockout above, never its own address's.
pub const MAX_KNOWN_IPS: usize = 64;

/// Sessions held by peers that have no rights (a bare `/updates` or `/udpReplyPort`). Past this
/// the one silent longest among them is evicted, so forged subscribers cannot fill the table
/// and keep a console from connecting. A session with rights is never evicted.
pub const MAX_RIGHTLESS_SESSIONS: usize = 64;
/// IPs tracked for failures / reply budgets.
pub const MAX_TRACKED_IPS: usize = 1024;
/// Reply bytes one UDP source may be sent at once, and per second after that. Bounds how much
/// traffic a forged source address can make us send to a victim.
pub const REPLY_BURST_BYTES: u64 = 16 * 1024;
pub const REPLY_BYTES_PER_SEC: u64 = 8 * 1024;
/// The budget of a UDP source that has not completed `/connect` when no passcode is set (so it
/// may ask and may be sent large replies, but has not proved the source address is its own): 256
/// KiB at once, which is four full datagrams (65507 bytes), then 64 KiB/s, about one more full
/// datagram a second or a few hundred updates. Sustained reflection at a forged victim is held to
/// that rate, and a 60 KB notes read still works.
pub const OPEN_REPLY_BURST_BYTES: u64 = 256 * 1024;
pub const OPEN_REPLY_BYTES_PER_SEC: u64 = 64 * 1024;

/// Which reply budget a UDP source is held to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Budget {
    /// Nothing may be asked without a passcode and the source has not connected: it is only sent
    /// small refusals.
    Strict,
    /// A peer without a passcode may ask and the source has not connected with one.
    Open,
}

impl Budget {
    fn limits(self) -> (u64, u64) {
        match self {
            Self::Strict => (REPLY_BURST_BYTES, REPLY_BYTES_PER_SEC),
            Self::Open => (OPEN_REPLY_BURST_BYTES, OPEN_REPLY_BYTES_PER_SEC),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectResult {
    /// Connected; the peer has these rights from now on (those of the passcode it gave, or the
    /// No Passcode row's if it gave none).
    Accepted(OscRights),
    /// Wrong passcode. A peer that was already connected stays connected.
    BadPasscode,
    /// This IP, or everyone, has failed too often lately.
    LockedOut,
    /// The peer table is full.
    TableFull,
}

#[derive(Debug)]
struct Session {
    /// The rights `/connect` gave, from a passcode of the table. A connect without one stores
    /// nothing: the peer has the No Passcode rights already.
    rights: Option<OscRights>,
    updates: bool,
    /// Where `/udpReplyPort` sent this UDP peer's replies and updates; `None`: the server default.
    /// Lives in the session, so it goes with it (idle expiry, `clear`).
    reply_port: Option<u16>,
    last_seen: Duration,
}

#[derive(Debug, Default)]
struct Failures {
    count: u32,
    locked_until: Option<Duration>,
    last: Duration,
}

#[derive(Debug)]
struct Bucket {
    tokens: u64,
    updated: Duration,
}

#[derive(Debug, Default)]
pub struct Sessions {
    peers: HashMap<Peer, Session>,
    failures: HashMap<IpAddr, Failures>,
    global_start: Duration,
    global_count: u32,
    /// Addresses that completed a `/connect`, oldest first, until [`Sessions::clear`].
    known: Vec<IpAddr>,
    /// Whether a peer that has not connected may view (No Passcode has View): then sessions
    /// without rights are ordinary subscribers and are not capped.
    strangers_may_view: bool,
}

impl Sessions {
    /// Told by the caller on every message, from the workspace's table as it is now.
    pub fn set_strangers_may_view(&mut self, may: bool) {
        self.strangers_may_view = may;
    }

    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Notes that `peer` sent something. A UDP peer that had been silent for [`UDP_IDLE`] is
    /// forgotten first (so this packet finds it unconnected and unsubscribed); anything sent
    /// within the window keeps it.
    pub fn touch(&mut self, peer: Peer, now: Duration) {
        let idle = matches!(peer, Peer::Udp(_))
            && self
                .peers
                .get(&peer)
                .is_some_and(|s| now.saturating_sub(s.last_seen) >= UDP_IDLE);
        if idle {
            self.peers.remove(&peer);
        } else if let Some(session) = self.peers.get_mut(&peer) {
            session.last_seen = now;
        }
    }

    /// Whether `peer` completed `/connect` with a passcode of the table.
    pub fn is_connected(&self, peer: Peer) -> bool {
        self.peers.get(&peer).is_some_and(|s| s.rights.is_some())
    }

    /// The rights `/connect` gave `peer`, or `None` if it has not connected with a passcode (it
    /// then has the No Passcode rights).
    pub fn rights(&self, peer: Peer) -> Option<OscRights> {
        self.peers.get(&peer)?.rights
    }

    /// `/connect` against the passcode table; `given` is what the peer sent (`None`: nothing).
    ///
    /// - A passcode in the table: the peer gets that row's rights and a session that keeps them.
    /// - No argument: the reply is the No Passcode rights (QLab: a peer that connects without a
    ///   passcode gets the rights of "No Passcode"). Nothing is stored, because they are what an
    ///   unconnected peer has anyway, so this costs no table slot, counts no failure and proves
    ///   nothing (not a known address). The same holds for any argument when the table has no
    ///   passcodes, as a workspace without a passcode accepted any `/connect`.
    /// - A passcode not in the table: `BadPasscode`, counted towards the lockouts.
    ///
    /// A peer that connected with a passcode is never disconnected, nor downgraded by a connect
    /// without one or a wrong one: UDP peers are only an IP address, so anyone sharing it (or
    /// forging it) could otherwise cut a console off mid-show. Its wrong guesses still count
    /// towards the lockouts like any peer's, and while locked out its `/connect` is refused
    /// (it keeps its rights); otherwise it could search the table for a row with more rights
    /// without limit. A correct passcode can raise or lower its rights to that row's.
    pub fn connect(
        &mut self,
        peer: Peer,
        now: Duration,
        access: &OscAccess,
        given: Option<&str>,
    ) -> ConnectResult {
        self.connect_counting(peer, now, access, given, true)
    }

    /// As [`Sessions::connect`]; with `count_failure` false a passcode not in this table is
    /// answered `BadPasscode` without counting towards the lockouts. For an un-prefixed
    /// `/connect` that another open doc's table accepted: a console connecting to two docs with
    /// their two passcodes is not guessing, and must not lock itself out of either.
    pub fn connect_counting(
        &mut self,
        peer: Peer,
        now: Duration,
        access: &OscAccess,
        given: Option<&str>,
        count_failure: bool,
    ) -> ConnectResult {
        let current = self.rights(peer);
        let guess = given.filter(|_| !access.passcodes.is_empty());
        let Some(guess) = guess else {
            return ConnectResult::Accepted(current.unwrap_or(access.no_passcode));
        };
        // Every row is compared, the matching one included, so the time does not tell which row
        // (or whether any) matched.
        let mut matched: Option<OscRights> = None;
        for row in &access.passcodes {
            if constant_time_eq(&row.passcode, guess) && matched.is_none() {
                matched = Some(row.rights);
            }
        }
        // A correct passcode from an address that connected before gets through the GLOBAL budget
        // (rotating forged sources must not keep the operator out after a dropped link), but
        // never past its own address's lockout: that would let a peer sharing the address keep
        // guessing for free. Not for a peer that is connected already: it needs no way back in.
        let own_locked = self.own_locked(peer.ip(), now);
        let known_and_right =
            current.is_none() && matched.is_some() && self.known.contains(&peer.ip());
        let locked = own_locked || (self.global_locked(now) && !known_and_right);
        let Some(rights) = matched else {
            // A wrong guess counts even during a lockout, and a connected peer's counts too (it
            // stays connected): otherwise it could search the table for more rights for free.
            if count_failure {
                self.record_failure(peer.ip(), now);
            }
            if current.is_some() {
                self.touch(peer, now);
            }
            return if locked {
                ConnectResult::LockedOut
            } else {
                ConnectResult::BadPasscode
            };
        };
        if locked {
            return ConnectResult::LockedOut;
        }
        if !self.ensure(peer, now) {
            return ConnectResult::TableFull;
        }
        self.remember(peer.ip());
        if let Some(session) = self.peers.get_mut(&peer) {
            session.rights = Some(rights);
        }
        ConnectResult::Accepted(rights)
    }

    /// Notes that `ip` completed a `/connect`; the oldest is forgotten past [`MAX_KNOWN_IPS`].
    fn remember(&mut self, ip: IpAddr) {
        self.known.retain(|k| *k != ip);
        if self.known.len() >= MAX_KNOWN_IPS {
            self.known.remove(0);
        }
        self.known.push(ip);
    }

    fn global_locked(&self, now: Duration) -> bool {
        self.global_count >= GLOBAL_MAX_FAILURES
            && now.saturating_sub(self.global_start) < GLOBAL_WINDOW
    }

    fn own_locked(&self, ip: IpAddr, now: Duration) -> bool {
        self.failures
            .get(&ip)
            .and_then(|f| f.locked_until)
            .is_some_and(|until| now < until)
    }

    fn record_failure(&mut self, ip: IpAddr, now: Duration) {
        if self.global_count == 0 || now.saturating_sub(self.global_start) >= GLOBAL_WINDOW {
            self.global_start = now;
            self.global_count = 0;
        }
        self.global_count = self.global_count.saturating_add(1);

        if !self.failures.contains_key(&ip) && self.failures.len() >= MAX_TRACKED_IPS {
            // Forget expired records first, then the oldest.
            self.failures
                .retain(|_, f| f.locked_until.is_some_and(|u| now < u));
            if self.failures.len() >= MAX_TRACKED_IPS {
                let oldest = self
                    .failures
                    .iter()
                    .min_by_key(|(_, f)| f.last)
                    .map(|(k, _)| *k);
                if let Some(oldest) = oldest {
                    self.failures.remove(&oldest);
                }
            }
        }
        let entry = self.failures.entry(ip).or_default();
        entry.last = now;
        // A finished lockout starts the count again.
        if entry.locked_until.is_some_and(|until| now >= until) {
            entry.locked_until = None;
            entry.count = 0;
        }
        entry.count = entry.count.saturating_add(1);
        if entry.count >= MAX_BAD_PASSCODES {
            entry.count = 0;
            entry.locked_until = Some(now.saturating_add(LOCKOUT));
        }
    }

    /// `/updates`. Returns false if the peer could not be given state (table full).
    pub fn set_updates(&mut self, peer: Peer, now: Duration, on: bool) -> bool {
        if !on {
            if let Some(session) = self.peers.get_mut(&peer) {
                session.updates = false;
                session.last_seen = now;
                if session.rights.is_none() && session.reply_port.is_none() {
                    self.peers.remove(&peer);
                }
            }
            return true;
        }
        if !self.ensure(peer, now) {
            return false;
        }
        if let Some(session) = self.peers.get_mut(&peer) {
            session.updates = true;
            session.last_seen = now;
        }
        true
    }

    /// `/udpReplyPort`: replies and updates to the UDP peer go to `port` from now on, until its
    /// session lapses. Returns false if the peer could not be given state (table full).
    pub fn set_reply_port(&mut self, peer: Peer, now: Duration, port: u16) -> bool {
        if !self.ensure(peer, now) {
            return false;
        }
        if let Some(session) = self.peers.get_mut(&peer) {
            session.reply_port = Some(port);
            session.last_seen = now;
        }
        true
    }

    /// The port set with `/udpReplyPort` for `peer`, if its session is still alive at `now`.
    pub fn reply_port(&self, peer: Peer, now: Duration) -> Option<u16> {
        let session = self.peers.get(&peer)?;
        if matches!(peer, Peer::Udp(_)) && now.saturating_sub(session.last_seen) >= UDP_IDLE {
            return None;
        }
        session.reply_port
    }

    /// Peers that asked for updates, in no particular order. A UDP peer silent for
    /// [`UDP_IDLE`] is not one, even before the next sweep removes it.
    pub fn update_subscribers(&self, now: Duration) -> Vec<Peer> {
        self.peers
            .iter()
            .filter(|(p, s)| {
                s.updates
                    && (matches!(p, Peer::Tcp { .. }) || now.saturating_sub(s.last_seen) < UDP_IDLE)
            })
            .map(|(p, _)| *p)
            .collect()
    }

    /// A TCP connection closed.
    pub fn remove(&mut self, peer: Peer) {
        self.peers.remove(&peer);
    }

    /// Forgets every `/connect`, subscription and known address: call when a different
    /// workspace opens. Failure records and reply budgets stay.
    pub fn clear(&mut self) {
        self.peers.clear();
        self.known.clear();
    }

    /// Ends every session but keeps the known addresses (the passcode table changed: consoles
    /// must connect again, and the global budget must not lock them out meanwhile).
    pub fn end_all(&mut self) {
        self.peers.clear();
    }

    /// How many addresses are remembered as having connected.
    pub fn known_len(&self) -> usize {
        self.known.len()
    }

    /// Drops UDP peers silent for [`UDP_IDLE`], and failure and budget records that no longer
    /// matter.
    pub fn expire(&mut self, now: Duration) {
        self.peers.retain(|peer, session| {
            matches!(peer, Peer::Tcp { .. }) || now.saturating_sub(session.last_seen) < UDP_IDLE
        });
        self.failures.retain(|_, f| {
            f.locked_until.is_some_and(|u| now < u) || now.saturating_sub(f.last) < LOCKOUT
        });
    }

    /// Makes room for one more session without rights: the cap is on those only.
    fn evict_rightless_over_cap(&mut self) {
        if self.strangers_may_view {
            return;
        }
        while self.peers.values().filter(|s| s.rights.is_none()).count() >= MAX_RIGHTLESS_SESSIONS {
            let oldest = self
                .peers
                .iter()
                .filter(|(_, s)| s.rights.is_none())
                .min_by_key(|(_, s)| s.last_seen)
                .map(|(p, _)| *p);
            match oldest {
                Some(p) => self.peers.remove(&p),
                None => break,
            };
        }
    }

    fn ensure(&mut self, peer: Peer, now: Duration) -> bool {
        if self.peers.contains_key(&peer) {
            return true;
        }
        if self.peers.len() >= MAX_PEERS {
            return false;
        }
        self.evict_rightless_over_cap();
        self.peers.insert(
            peer,
            Session {
                rights: None,
                updates: false,
                reply_port: None,
                last_seen: now,
            },
        );
        true
    }
}

/// The reply byte budget of each UDP source address (docs/osc.md, "Limits"). Per IP, whatever
/// workspace the replies are about: several open docs must not multiply what a forged source
/// can be sent.
#[derive(Debug, Default)]
pub struct Budgets {
    budgets: HashMap<(IpAddr, Budget), Bucket>,
}

impl Budgets {
    /// Drops the buckets that have refilled.
    pub fn expire(&mut self, now: Duration) {
        self.budgets.retain(|(_, kind), b| {
            let (burst, rate) = kind.limits();
            refilled(b.tokens, b.updated, now, burst, rate) < burst
        });
    }

    /// Takes `bytes` from the reply budget of the UDP source `ip`; false if it has less left
    /// (the reply must then not be sent).
    pub fn spend_reply_bytes(
        &mut self,
        ip: IpAddr,
        kind: Budget,
        now: Duration,
        bytes: u64,
    ) -> bool {
        let (burst, rate) = kind.limits();
        let key = (ip, kind);
        if !self.budgets.contains_key(&key) && self.budgets.len() >= MAX_TRACKED_IPS {
            self.budgets.retain(|(_, k), b| {
                let (burst, rate) = k.limits();
                refilled(b.tokens, b.updated, now, burst, rate) < burst
            });
            // Only a refilled bucket may go (the retain above dropped them): evicting one that
            // is still depleted would hand its IP a fresh burst, and cycling forged addresses
            // would then remove the budget. A new address finding the table full of depleted
            // buckets is sent nothing until one refills (a few seconds).
            if self.budgets.len() >= MAX_TRACKED_IPS {
                return false;
            }
        }
        let bucket = self.budgets.entry(key).or_insert(Bucket {
            tokens: burst,
            updated: now,
        });
        let available = refilled(bucket.tokens, bucket.updated, now, burst, rate);
        bucket.updated = now;
        if available >= bytes {
            bucket.tokens = available - bytes;
            true
        } else {
            bucket.tokens = available;
            false
        }
    }
}

/// Whether `given` is a passcode of `access`'s table (every row compared, as in `connect`).
pub fn passcode_matches(access: &OscAccess, given: &str) -> bool {
    let mut matched = false;
    for row in &access.passcodes {
        matched |= constant_time_eq(&row.passcode, given);
    }
    matched
}

/// Tokens after refilling for the time since `updated`, capped at the burst size.
fn refilled(tokens: u64, updated: Duration, now: Duration, burst: u64, rate: u64) -> u64 {
    let elapsed_ms = u64::try_from(now.saturating_sub(updated).as_millis()).unwrap_or(u64::MAX);
    tokens
        .saturating_add(elapsed_ms.saturating_mul(rate) / 1000)
        .min(burst)
}

/// Compares every byte of the longer string against the shorter one padded with zeros, so the
/// time depends on the longer length but not on where the first difference is. It does not hide
/// whether the lengths are equal.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = u8::from(a.len() != b.len());
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    diff == 0
}
