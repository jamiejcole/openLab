// SPDX-License-Identifier: GPL-3.0-or-later

//! The socket-free OSC layer (ARCHITECTURE section 7, `docs/osc.md`): decoding, address
//! dispatch onto the inputs the UI sends, per-peer passcode and updates state, and QLab-style
//! replies. The UDP and TCP listeners (M4.5a) call [`Osc`] with what they read and send what
//! comes back; nothing in here opens a socket, reads a clock or spawns a thread.

mod codec;
mod dispatch;
#[doc(hidden)]
pub mod fuzz;
mod session;

use std::time::Duration;

use rosc::OscMessage;

pub use codec::{
    DecodeError, EncodeError, MAX_ARRAY_DEPTH, MAX_BUNDLE_DEPTH, MAX_MESSAGES_PER_PACKET,
    MAX_PACKET_BYTES, SlipDecoder, SlipEvent, decode_packet, encode_message, slip_encode,
};
pub use dispatch::MAX_REPLY_JSON_BYTES;
pub use dispatch::{
    Address, CueField, CueRef, MAX_ADDRESS_BYTES, MAX_ADDRESS_PARTS, OscInput, OscShow, Parsed,
    PlayheadTarget, Reply, Resolved, STATUS_DENIED, STATUS_ERROR, STATUS_OK, Verb, parse_address,
    reply_message, resolve,
};
pub use session::{
    ConnectResult, GLOBAL_MAX_FAILURES, GLOBAL_WINDOW, LOCKOUT, MAX_BAD_PASSCODES, MAX_KNOWN_IPS,
    MAX_PEERS, Peer, REPLY_BURST_BYTES, REPLY_BYTES_PER_SEC, Sessions, UDP_IDLE,
};

/// The UDP port QLab listens on, and so do we (UDP and TCP).
pub const OSC_PORT: u16 = 53000;
/// The UDP port replies and updates go to.
pub const OSC_REPLY_PORT: u16 = 53001;

/// How often idle UDP peers are looked for.
const EXPIRE_EVERY: Duration = Duration::from_secs(10);

/// Counters for what was dropped or refused. The runtime publishes them (status, logs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OscStats {
    /// Datagrams and SLIP frames handed to the decoder.
    pub packets: u64,
    /// Messages dispatched (a bundle counts each message).
    pub messages: u64,
    /// Packets that did not decode, and SLIP frames with a bad escape.
    pub malformed: u64,
    /// SLIP frames over the size limit.
    pub oversize_frames: u64,
    /// Addresses that were not usable (too long, empty parts, not starting with `/`).
    pub bad_address: u64,
    /// Valid addresses nothing answers to, and that are no cue's trigger.
    pub unknown_address: u64,
    /// `/workspace/{id}/...` for another workspace; ignored.
    pub wrong_workspace: u64,
    /// Messages refused for want of `/connect`.
    pub denied: u64,
    /// Messages answered with an error reply (bad cue, bad argument, unsupported).
    pub rejected: u64,
    /// Replies not sent: over the per-call cap, or not encodable.
    pub replies_dropped: u64,
    /// Replies not sent to a UDP source that used up its byte budget.
    pub replies_throttled: u64,
    /// Update messages not sent to a UDP subscriber that used up its byte budget, or not
    /// encodable.
    pub updates_throttled: u64,
}

/// IP and UDP headers, counted against the reply budget.
const UDP_HEADER_BYTES: u64 = 28;

/// Most replies from one datagram or one stream read.
pub const MAX_REPLIES_PER_CALL: usize = 16;

/// What handling some bytes produced.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    /// To carry out, in order.
    pub inputs: Vec<OscInput>,
    /// To send back to the peer that sent the bytes: UDP to its address on [`OSC_REPLY_PORT`],
    /// TCP SLIP-framed on the same connection.
    pub replies: Vec<OscMessage>,
}

/// The OSC endpoint's state: per-peer sessions and counters.
#[derive(Debug, Default)]
pub struct Osc {
    sessions: Sessions,
    stats: OscStats,
    last_expire: Duration,
}

impl Osc {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stats(&self) -> &OscStats {
        &self.stats
    }

    /// Peers that asked for updates (`/updates 1`), for the fan-out.
    pub fn update_subscribers(&self) -> Vec<Peer> {
        self.sessions.update_subscribers()
    }

    /// How many addresses are remembered as having connected (at most [`MAX_KNOWN_IPS`]).
    pub fn known_ips(&self) -> usize {
        self.sessions.known_len()
    }

    /// Whether `peer` has completed `/connect`.
    pub fn is_connected(&self, peer: Peer) -> bool {
        self.sessions.is_connected(peer)
    }

    /// A TCP connection closed.
    pub fn disconnect(&mut self, peer: Peer) {
        self.sessions.remove(peer);
    }

    /// Drops every `/connect` and subscription (the passcode changed, or another show opened).
    pub fn reset_sessions(&mut self) {
        self.sessions.clear();
    }

    /// Handles one UDP datagram from `peer` (`now` is monotonic time since the engine started).
    pub fn handle_datagram(
        &mut self,
        peer: Peer,
        now: Duration,
        bytes: &[u8],
        show: &dyn OscShow,
    ) -> Outcome {
        let mut out = Outcome::default();
        self.handle_packet(peer, now, bytes, show, &mut out);
        out
    }

    /// Handles bytes read from a TCP connection. `decoder` is that connection's own decoder.
    pub fn handle_stream(
        &mut self,
        peer: Peer,
        now: Duration,
        decoder: &mut SlipDecoder,
        bytes: &[u8],
        show: &dyn OscShow,
    ) -> Outcome {
        let mut out = Outcome::default();
        let mut events = Vec::new();
        decoder.feed(bytes, &mut events);
        for event in events {
            match event {
                SlipEvent::Frame(frame) => self.handle_packet(peer, now, &frame, show, &mut out),
                SlipEvent::Oversize => self.stats.oversize_frames += 1,
                SlipEvent::BadEscape => self.stats.malformed += 1,
            }
        }
        out
    }

    fn handle_packet(
        &mut self,
        peer: Peer,
        now: Duration,
        bytes: &[u8],
        show: &dyn OscShow,
        out: &mut Outcome,
    ) {
        self.stats.packets += 1;
        if now.saturating_sub(self.last_expire) >= EXPIRE_EVERY {
            self.sessions.expire(now);
            self.last_expire = now;
        }
        self.sessions.touch(peer, now);
        match decode_packet(bytes) {
            Ok(messages) => {
                for message in &messages {
                    self.handle_message(peer, now, message, show, out);
                }
            }
            Err(_) => self.stats.malformed += 1,
        }
    }

    fn handle_message(
        &mut self,
        peer: Peer,
        now: Duration,
        message: &OscMessage,
        show: &dyn OscShow,
        out: &mut Outcome,
    ) {
        self.stats.messages += 1;
        let Some(parsed) = parse_address(&message.addr) else {
            self.stats.bad_address += 1;
            return;
        };
        let workspace = show.workspace_id();
        if let Some(id) = &parsed.workspace
            && !id.eq_ignore_ascii_case(&workspace.to_string())
        {
            self.stats.wrong_workspace += 1;
            return;
        }
        let passcode = show.passcode().filter(|p| !p.is_empty());
        let reply = |this: &mut Self, out: &mut Outcome, reply: Reply| {
            let message = reply_message(workspace, &message.addr, &reply);
            this.push_reply(peer, now, message, out);
        };

        if parsed.address == Address::Connect {
            let given = dispatch::passcode_arg(&message.args);
            let result = reply_for_connect(self.sessions.connect(peer, now, passcode, given));
            reply(self, out, result);
            return;
        }
        if passcode.is_some() && !self.sessions.is_connected(peer) {
            self.stats.denied += 1;
            // No reply to an unauthenticated UDP source: its address may be forged, and the
            // reply would go to someone else. (`/connect` above does answer.)
            if matches!(peer, Peer::Tcp { .. }) {
                reply(self, out, Reply::denied());
            }
            return;
        }
        match parsed.address {
            Address::Updates => {
                let result = match dispatch::updates_arg(&message.args) {
                    None => {
                        self.stats.rejected += 1;
                        Reply::error("bad argument")
                    }
                    Some(on) if self.sessions.set_updates(peer, now, on) => Reply::ok("ok".into()),
                    Some(_) => {
                        self.stats.rejected += 1;
                        Reply::error("too many peers")
                    }
                };
                reply(self, out, result);
            }
            address => match resolve(show, &address, &message.args) {
                Resolved::Input(input) => out.inputs.push(input),
                Resolved::Reply(r) => {
                    if r.status == STATUS_ERROR {
                        self.stats.rejected += 1;
                    }
                    reply(self, out, r);
                }
                Resolved::Unknown => self.stats.unknown_address += 1,
            },
        }
    }

    /// Encodes an update message for `peer` (NET-05), or `None` if it must not be sent: not
    /// encodable, or `peer` is a UDP source that used up its byte budget (shared with its
    /// replies, so subscribing from a forged address cannot aim more than the budget at a
    /// victim). TCP peers are paced by their connection instead. `now` as for the handlers.
    pub fn prepare_update(
        &mut self,
        peer: Peer,
        now: Duration,
        message: &OscMessage,
    ) -> Option<Vec<u8>> {
        let Ok(bytes) = encode_message(message) else {
            self.stats.updates_throttled += 1;
            return None;
        };
        if matches!(peer, Peer::Udp(_)) {
            let cost = u64::try_from(bytes.len())
                .unwrap_or(u64::MAX)
                .saturating_add(UDP_HEADER_BYTES);
            if !self.sessions.spend_reply_bytes(peer.ip(), now, cost) {
                self.stats.updates_throttled += 1;
                return None;
            }
        }
        Some(bytes)
    }

    /// Queues a reply unless a cap says no: at most [`MAX_REPLIES_PER_CALL`] per datagram or
    /// stream read, and for UDP peers a per-source-IP byte budget (a forged source address must
    /// not turn us into an amplifier). Dropped replies are counted.
    fn push_reply(&mut self, peer: Peer, now: Duration, message: OscMessage, out: &mut Outcome) {
        if out.replies.len() >= MAX_REPLIES_PER_CALL {
            self.stats.replies_dropped += 1;
            return;
        }
        if matches!(peer, Peer::Udp(_)) {
            let Ok(bytes) = encode_message(&message) else {
                self.stats.replies_dropped += 1;
                return;
            };
            let cost = u64::try_from(bytes.len())
                .unwrap_or(u64::MAX)
                .saturating_add(UDP_HEADER_BYTES);
            if !self.sessions.spend_reply_bytes(peer.ip(), now, cost) {
                self.stats.replies_throttled += 1;
                return;
            }
        }
        out.replies.push(message);
    }
}

fn reply_for_connect(result: ConnectResult) -> Reply {
    match result {
        ConnectResult::Accepted => Reply::ok("ok".into()),
        ConnectResult::BadPasscode => Reply::error("badpass"),
        ConnectResult::LockedOut => Reply::denied(),
        ConnectResult::TableFull => Reply::error("too many peers"),
    }
}
