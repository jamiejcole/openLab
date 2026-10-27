// SPDX-License-Identifier: GPL-3.0-or-later

//! Rate-limited logging of refused connections, shared by the WebSocket and OSC listeners.

use std::time::Duration;

use crossbeam_channel::Sender;
use openlab_core::protocol::LogLevel;
use tokio::time::Instant;

use crate::control::Inbox;

/// Counts refused connections and logs them at most once per `refusal_log_interval`, so a UI
/// that cannot connect (every slot held by something else) can be diagnosed. Refusals inside
/// the interval are logged, with their count, once it has passed.
pub struct Refusals {
    name: &'static str,
    interval: Duration,
    count: u64,
    limit: &'static str,
    last_log: Option<Instant>,
}

impl Refusals {
    pub fn new(name: &'static str, interval: Duration) -> Self {
        Self {
            name,
            interval,
            count: 0,
            limit: "",
            last_log: None,
        }
    }

    pub fn refused(&mut self, inbox: &Sender<Inbox>, limit: &'static str) {
        self.count += 1;
        self.limit = limit;
        if self.due().is_some_and(|t| t <= Instant::now()) {
            self.flush(inbox);
        }
    }

    /// When the pending count may be logged; `None` if nothing is pending.
    pub fn due(&self) -> Option<Instant> {
        if self.count == 0 {
            return None;
        }
        Some(match self.last_log {
            Some(t) => t + self.interval,
            None => Instant::now(),
        })
    }

    pub fn flush(&mut self, inbox: &Sender<Inbox>) {
        if self.count == 0 {
            return;
        }
        let _ = inbox.send(Inbox::Log {
            level: LogLevel::Warn,
            message: format!(
                "{} refused {} connection(s): {}",
                self.name, self.count, self.limit
            ),
        });
        self.count = 0;
        self.last_log = Some(Instant::now());
    }
}
