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

/// Who sent a message. A UDP peer is its IP address (the source port changes per sender
/// socket, and replies go to the fixed port 53001); a TCP peer is the connection, with the
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
/// A UDP peer silent this long is forgotten (there is no disconnect on UDP).
pub const UDP_IDLE: Duration = Duration::from_secs(10 * 60);
/// Wrong passcodes from one IP, while not connected, before that IP is locked out.
pub const MAX_BAD_PASSCODES: u32 = 5;
/// How long a lockout lasts; attempts during it are refused without being checked.
pub const LOCKOUT: Duration = Duration::from_secs(30);
/// Wrong passcodes from all IPs together allowed per [`GLOBAL_WINDOW`]; past it every new
/// `/connect` is refused until the window ends (so rotating source addresses does not give
/// unlimited guesses). Peers that are already connected are not affected.
pub const GLOBAL_MAX_FAILURES: u32 = 50;
pub const GLOBAL_WINDOW: Duration = Duration::from_secs(60);
/// Addresses remembered as having connected. A correct passcode from one of them passes the
/// lockouts above.
pub const MAX_KNOWN_IPS: usize = 64;
/// IPs tracked for failures / reply budgets.
pub const MAX_TRACKED_IPS: usize = 1024;
/// Reply bytes one UDP source may be sent at once, and per second after that. Bounds how much
/// traffic a forged source address can make us send to a victim.
pub const REPLY_BURST_BYTES: u64 = 16 * 1024;
pub const REPLY_BYTES_PER_SEC: u64 = 8 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectResult {
    Accepted,
    /// Wrong passcode. A peer that was already connected stays connected.
    BadPasscode,
    /// This IP, or everyone, has failed too often lately.
    LockedOut,
    /// The peer table is full.
    TableFull,
}

#[derive(Debug)]
struct Session {
    connected: bool,
    updates: bool,
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
    budgets: HashMap<IpAddr, Bucket>,
    /// Addresses that completed a `/connect`, oldest first, until [`Sessions::clear`].
    known: Vec<IpAddr>,
}

impl Sessions {
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Notes that `peer` sent something (keeps a UDP peer from idling out).
    pub fn touch(&mut self, peer: Peer, now: Duration) {
        if let Some(session) = self.peers.get_mut(&peer) {
            session.last_seen = now;
        }
    }

    pub fn is_connected(&self, peer: Peer) -> bool {
        self.peers.get(&peer).is_some_and(|s| s.connected)
    }

    /// `/connect`. `expected` is the workspace passcode (`None` or empty: none set, anyone is
    /// accepted); `given` is what the peer sent.
    ///
    /// A peer that is already connected is never disconnected or locked out by a later
    /// `/connect`, right or wrong: UDP peers are only an IP address, so anyone sharing it (or
    /// forging it) could otherwise cut a console off mid-show.
    pub fn connect(
        &mut self,
        peer: Peer,
        now: Duration,
        expected: Option<&str>,
        given: Option<&str>,
    ) -> ConnectResult {
        let ok = match expected.filter(|p| !p.is_empty()) {
            None => true,
            Some(expected) => given.is_some_and(|g| constant_time_eq(expected, g)),
        };
        if self.is_connected(peer) {
            self.touch(peer, now);
            return if ok {
                ConnectResult::Accepted
            } else {
                ConnectResult::BadPasscode
            };
        }
        // A correct passcode from an address that connected before gets through the lockouts,
        // so an attacker's wrong guesses cannot keep the operator out after a dropped link.
        let known_and_right = ok && self.known.contains(&peer.ip());
        if !known_and_right && self.locked_out(peer.ip(), now) {
            return ConnectResult::LockedOut;
        }
        if !ok {
            self.record_failure(peer.ip(), now);
            return ConnectResult::BadPasscode;
        }
        if !self.ensure(peer, now) {
            return ConnectResult::TableFull;
        }
        self.failures.remove(&peer.ip());
        self.remember(peer.ip());
        if let Some(session) = self.peers.get_mut(&peer) {
            session.connected = true;
        }
        ConnectResult::Accepted
    }

    /// Notes that `ip` completed a `/connect`; the oldest is forgotten past [`MAX_KNOWN_IPS`].
    fn remember(&mut self, ip: IpAddr) {
        self.known.retain(|k| *k != ip);
        if self.known.len() >= MAX_KNOWN_IPS {
            self.known.remove(0);
        }
        self.known.push(ip);
    }

    fn locked_out(&self, ip: IpAddr, now: Duration) -> bool {
        let global = self.global_count >= GLOBAL_MAX_FAILURES
            && now.saturating_sub(self.global_start) < GLOBAL_WINDOW;
        let own = self
            .failures
            .get(&ip)
            .and_then(|f| f.locked_until)
            .is_some_and(|until| now < until);
        global || own
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
                if !session.connected {
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

    /// Peers that asked for updates, in no particular order.
    pub fn update_subscribers(&self) -> Vec<Peer> {
        self.peers
            .iter()
            .filter(|(_, s)| s.updates)
            .map(|(p, _)| *p)
            .collect()
    }

    /// A TCP connection closed.
    pub fn remove(&mut self, peer: Peer) {
        self.peers.remove(&peer);
    }

    /// Forgets every `/connect` and subscription: call when the workspace passcode changes or
    /// a different workspace opens. Failure records and reply budgets stay.
    pub fn clear(&mut self) {
        self.peers.clear();
        self.known.clear();
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
        self.budgets
            .retain(|_, b| refilled(b.tokens, b.updated, now) < REPLY_BURST_BYTES);
    }

    /// Takes `bytes` from the reply budget of the UDP source `ip`; false if it has less left
    /// (the reply must then not be sent).
    pub fn spend_reply_bytes(&mut self, ip: IpAddr, now: Duration, bytes: u64) -> bool {
        if !self.budgets.contains_key(&ip) && self.budgets.len() >= MAX_TRACKED_IPS {
            self.budgets
                .retain(|_, b| refilled(b.tokens, b.updated, now) < REPLY_BURST_BYTES);
            if self.budgets.len() >= MAX_TRACKED_IPS {
                let oldest = self
                    .budgets
                    .iter()
                    .min_by_key(|(_, b)| b.updated)
                    .map(|(k, _)| *k);
                if let Some(oldest) = oldest {
                    self.budgets.remove(&oldest);
                }
            }
        }
        let bucket = self.budgets.entry(ip).or_insert(Bucket {
            tokens: REPLY_BURST_BYTES,
            updated: now,
        });
        let available = refilled(bucket.tokens, bucket.updated, now);
        bucket.updated = now;
        if available >= bytes {
            bucket.tokens = available - bytes;
            true
        } else {
            bucket.tokens = available;
            false
        }
    }

    fn ensure(&mut self, peer: Peer, now: Duration) -> bool {
        if self.peers.contains_key(&peer) {
            return true;
        }
        if self.peers.len() >= MAX_PEERS {
            return false;
        }
        self.peers.insert(
            peer,
            Session {
                connected: false,
                updates: false,
                last_seen: now,
            },
        );
        true
    }
}

/// Tokens after refilling for the time since `updated`, capped at the burst size.
fn refilled(tokens: u64, updated: Duration, now: Duration) -> u64 {
    let elapsed_ms = u64::try_from(now.saturating_sub(updated).as_millis()).unwrap_or(u64::MAX);
    tokens
        .saturating_add(elapsed_ms.saturating_mul(REPLY_BYTES_PER_SEC) / 1000)
        .min(REPLY_BURST_BYTES)
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
