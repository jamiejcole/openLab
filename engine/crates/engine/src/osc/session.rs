// SPDX-License-Identifier: GPL-3.0-or-later

//! Per-peer OSC state: whether the peer has given the passcode (`/connect`) and whether it wants
//! updates (`/updates 1`). Pure logic: the caller passes the time (a monotonic `Duration`).
//!
//! A peer only gets an entry by doing something stateful (a `/connect` attempt, `/updates`), so
//! stray traffic from spoofed UDP sources costs nothing.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;

/// Who sent a message. A UDP peer is its IP address (the source port changes per sender
/// socket, and replies go to the fixed port 53001); a TCP peer is the connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Peer {
    Udp(IpAddr),
    /// Caller-chosen connection number, unique while the connection lives.
    Tcp(u64),
}

/// Most peers with state. A full table drops the longest-silent UDP peer; if there is none the
/// new peer is refused.
pub const MAX_PEERS: usize = 1024;
/// A UDP peer silent this long is forgotten (there is no disconnect on UDP).
pub const UDP_IDLE: Duration = Duration::from_secs(10 * 60);
/// Wrong passcodes in a row before the peer is locked out.
pub const MAX_BAD_PASSCODES: u32 = 5;
/// How long a lockout lasts; attempts during it are refused without being checked.
pub const LOCKOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectResult {
    Accepted,
    BadPasscode,
    LockedOut,
    /// The peer table is full of peers that cannot be dropped.
    TableFull,
}

#[derive(Debug)]
struct Session {
    connected: bool,
    updates: bool,
    last_seen: Duration,
    bad_attempts: u32,
    locked_until: Option<Duration>,
}

#[derive(Debug, Default)]
pub struct Sessions {
    peers: HashMap<Peer, Session>,
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
    pub fn connect(
        &mut self,
        peer: Peer,
        now: Duration,
        expected: Option<&str>,
        given: Option<&str>,
    ) -> ConnectResult {
        if !self.ensure(peer, now) {
            return ConnectResult::TableFull;
        }
        let Some(session) = self.peers.get_mut(&peer) else {
            return ConnectResult::TableFull;
        };
        session.last_seen = now;
        if session.locked_until.is_some_and(|until| now < until) {
            return ConnectResult::LockedOut;
        }
        session.locked_until = None;
        let ok = match expected.filter(|p| !p.is_empty()) {
            None => true,
            Some(expected) => given.is_some_and(|g| constant_time_eq(expected, g)),
        };
        if ok {
            session.connected = true;
            session.bad_attempts = 0;
            ConnectResult::Accepted
        } else {
            session.connected = false;
            session.updates = false;
            session.bad_attempts = session.bad_attempts.saturating_add(1);
            if session.bad_attempts >= MAX_BAD_PASSCODES {
                session.bad_attempts = 0;
                session.locked_until = Some(now.saturating_add(LOCKOUT));
            }
            ConnectResult::BadPasscode
        }
    }

    /// `/updates`. Returns false if the peer could not be given state (table full).
    pub fn set_updates(&mut self, peer: Peer, now: Duration, on: bool) -> bool {
        if !on && !self.peers.contains_key(&peer) {
            return true;
        }
        if !self.ensure(peer, now) {
            return false;
        }
        if let Some(session) = self.peers.get_mut(&peer) {
            session.updates = on;
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
    /// a different workspace opens.
    pub fn clear(&mut self) {
        self.peers.clear();
    }

    /// Drops UDP peers silent for [`UDP_IDLE`].
    pub fn expire(&mut self, now: Duration) {
        self.peers.retain(|peer, session| {
            matches!(peer, Peer::Tcp(_)) || now.saturating_sub(session.last_seen) < UDP_IDLE
        });
    }

    fn ensure(&mut self, peer: Peer, now: Duration) -> bool {
        if self.peers.contains_key(&peer) {
            return true;
        }
        if self.peers.len() >= MAX_PEERS {
            let oldest_udp = self
                .peers
                .iter()
                .filter(|(p, _)| matches!(p, Peer::Udp(_)))
                // Peers that never got in go first, then the longest silent.
                .min_by_key(|(_, s)| (s.connected, s.last_seen))
                .map(|(p, _)| *p);
            match oldest_udp {
                Some(victim) => {
                    self.peers.remove(&victim);
                }
                None => return false,
            }
        }
        self.peers.insert(
            peer,
            Session {
                connected: false,
                updates: false,
                last_seen: now,
                bad_attempts: 0,
                locked_until: None,
            },
        );
        true
    }
}

/// Compares without stopping at the first difference, so timing does not tell a peer how much
/// of a guess was right.
fn constant_time_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}
