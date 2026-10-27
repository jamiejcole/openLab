// SPDX-License-Identifier: GPL-3.0-or-later

//! Drops a message identical to the previous one from the same peer received a moment before
//! (QLab 5 does: measured, same address and arguments within about 100 ms, `/go` included, no
//! action and no reply). Bounded: one fingerprint per peer, and no more peers than
//! [`MAX_PEERS`] are tracked at once.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Duration;

use super::Peer;

/// A repeat closer than this to the message it repeats is dropped.
pub const DUPLICATE_WINDOW: Duration = Duration::from_millis(100);
/// Peers whose last message is remembered. A peer past it is not deduplicated until an entry
/// leaves the window (that is at most 100 ms away), so the table never grows and never evicts
/// one that is working.
pub const MAX_PEERS: usize = 1024;

#[derive(Debug)]
pub struct Dedupe {
    /// The last message handled for each peer: its fingerprint and when it arrived.
    last: HashMap<Peer, (u64, Duration)>,
    window: Duration,
}

impl Default for Dedupe {
    fn default() -> Self {
        Self {
            last: HashMap::new(),
            window: DUPLICATE_WINDOW,
        }
    }
}

impl Dedupe {
    /// Changes the window; zero turns the dropping off.
    pub fn set_window(&mut self, window: Duration) {
        self.window = window;
    }

    /// Whether the message with `bytes` (its encoded form) repeats the one handled for `peer`
    /// within [`DUPLICATE_WINDOW`]. A message that is not a repeat becomes the one remembered; a
    /// dropped repeat does not refresh the time, so a message sent every 50 ms is handled every
    /// other time, as QLab does.
    pub fn is_duplicate(&mut self, peer: Peer, now: Duration, bytes: &[u8]) -> bool {
        let mut hasher = DefaultHasher::new();
        bytes.hash(&mut hasher);
        let fingerprint = hasher.finish();
        if let Some((last, at)) = self.last.get(&peer)
            && *last == fingerprint
            && now.saturating_sub(*at) < self.window
        {
            return true;
        }
        if !self.last.contains_key(&peer) && self.last.len() >= MAX_PEERS {
            self.expire(now);
            if self.last.len() >= MAX_PEERS {
                return false;
            }
        }
        self.last.insert(peer, (fingerprint, now));
        false
    }

    /// Forgets a peer (a TCP connection closed).
    pub fn forget(&mut self, peer: Peer) {
        self.last.remove(&peer);
    }

    /// Forgets entries that are out of the window.
    pub fn expire(&mut self, now: Duration) {
        let window = self.window;
        self.last
            .retain(|_, (_, at)| now.saturating_sub(*at) < window);
    }

    pub fn len(&self) -> usize {
        self.last.len()
    }

    pub fn is_empty(&self) -> bool {
        self.last.is_empty()
    }
}
