// SPDX-License-Identifier: GPL-3.0-or-later

//! When a connected peer counts as dead.
//!
//! Silence is counted in pings the peer left unanswered, not in time since it was last heard. A
//! clock cannot tell a silent peer from a server that was itself late: an engine starved of CPU,
//! suspended, or on a machine that slept through the interval finds the time long past though it
//! sent the peer nothing to answer. A ping that was really sent, and a further interval that
//! really passed on this side, is the only evidence that the peer had its chance.

use std::time::Duration;

/// The pings one connection has sent since its peer was last heard.
#[derive(Debug)]
pub(crate) struct Liveness {
    unanswered: u32,
    limit: u32,
}

impl Liveness {
    /// A peer that leaves the pings of `dead_after` unanswered (one per `ping_interval`, at
    /// least one) is dead.
    pub(crate) fn new(ping_interval: Duration, dead_after: Duration) -> Self {
        let pings = dead_after
            .as_nanos()
            .div_ceil(ping_interval.as_nanos().max(1));
        Self {
            unanswered: 0,
            limit: u32::try_from(pings).unwrap_or(u32::MAX).max(1),
        }
    }

    /// Anything arrived from the peer.
    pub(crate) fn heard(&mut self) {
        self.unanswered = 0;
    }

    /// A ping is due. `false`: the peer answered none of the pings it was allowed, and is dead;
    /// otherwise the ping about to be sent is counted.
    pub(crate) fn ping_due(&mut self) -> bool {
        if self.unanswered >= self.limit {
            return false;
        }
        self.unanswered += 1;
        true
    }

    /// How many pings in a row the peer has left unanswered.
    pub(crate) fn unanswered(&self) -> u32 {
        self.unanswered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PING: Duration = Duration::from_secs(10);
    const DEAD: Duration = Duration::from_secs(30);

    #[test]
    fn a_peer_that_leaves_every_ping_of_the_limit_unanswered_is_dead() {
        let mut l = Liveness::new(PING, DEAD);
        assert!(l.ping_due() && l.ping_due() && l.ping_due(), "three pings");
        assert!(!l.ping_due(), "none answered by the fourth");
        assert_eq!(l.unanswered(), 3);
    }

    #[test]
    fn anything_heard_starts_the_count_again() {
        let mut l = Liveness::new(PING, DEAD);
        for _ in 0..100 {
            assert!(l.ping_due());
            assert!(l.ping_due());
            l.heard();
        }
        assert_eq!(l.unanswered(), 0);
    }

    /// The engine's own ping timer came late by more than the whole limit (the process was
    /// starved or suspended): the peer was sent nothing meanwhile, so it is still alive, and
    /// still has the full count of pings to answer.
    #[test]
    fn a_ping_timer_that_comes_late_does_not_count_against_the_peer() {
        let mut l = Liveness::new(PING, DEAD);
        assert!(l.ping_due());
        l.heard();
        // 45 s pass on the clock before the next tick is delivered: one tick, one ping.
        assert!(l.ping_due(), "the late tick only sends a ping");
        assert!(l.ping_due() && l.ping_due());
        assert!(
            !l.ping_due(),
            "dead only after three real pings went unanswered"
        );
    }

    #[test]
    fn the_limit_is_at_least_one_ping_and_never_overflows() {
        let mut l = Liveness::new(PING, Duration::ZERO);
        assert!(l.ping_due(), "one ping is always sent");
        assert!(!l.ping_due());
        let mut l = Liveness::new(Duration::ZERO, Duration::MAX);
        assert!(l.ping_due());
        // Part of an interval is a whole ping more.
        let mut l = Liveness::new(PING, Duration::from_secs(11));
        assert!(l.ping_due() && l.ping_due());
        assert!(!l.ping_due());
    }
}
