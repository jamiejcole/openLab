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
pub use dispatch::{
    Address, CueField, CueRef, MAX_ADDRESS_BYTES, MAX_ADDRESS_PARTS, OscInput, OscShow, Parsed,
    PlayheadTarget, Reply, Resolved, STATUS_DENIED, STATUS_ERROR, STATUS_OK, Verb, parse_address,
    reply_message, resolve,
};
pub use session::{ConnectResult, LOCKOUT, MAX_BAD_PASSCODES, MAX_PEERS, Peer, Sessions, UDP_IDLE};

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
}

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
        let reply = |out: &mut Outcome, reply: Reply| {
            out.replies
                .push(reply_message(workspace, &message.addr, &reply));
        };

        if parsed.address == Address::Connect {
            let given = dispatch::passcode_arg(&message.args);
            let result = reply_for_connect(self.sessions.connect(peer, now, passcode, given));
            reply(out, result);
            return;
        }
        if passcode.is_some() && !self.sessions.is_connected(peer) {
            self.stats.denied += 1;
            // No reply to an unauthenticated UDP source: its address may be forged, and the
            // reply would go to someone else. (`/connect` above does answer.)
            if matches!(peer, Peer::Tcp(_)) {
                reply(out, Reply::denied());
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
                reply(out, result);
            }
            address => match resolve(show, &address, &message.args) {
                Resolved::Input(input) => out.inputs.push(input),
                Resolved::Reply(r) => {
                    if r.status == STATUS_ERROR {
                        self.stats.rejected += 1;
                    }
                    reply(out, r);
                }
                Resolved::Unknown => self.stats.unknown_address += 1,
            },
        }
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
