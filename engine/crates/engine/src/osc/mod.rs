// SPDX-License-Identifier: GPL-3.0-or-later

//! The socket-free OSC layer (ARCHITECTURE section 7, `docs/osc.md`): decoding, address
//! dispatch onto the inputs the UI sends, per-peer passcode and updates state, and QLab-style
//! replies. The UDP and TCP listeners (M4.5a) call [`Osc`] with what they read and send what
//! comes back; nothing in here opens a socket, reads a clock or spawns a thread.

mod codec;
mod dedupe;
mod dispatch;
#[doc(hidden)]
pub mod fuzz;
mod pattern;
mod session;

use std::time::Duration;

use rosc::OscMessage;
use serde_json::json;

pub use codec::{
    DecodeError, EncodeError, MAX_ARRAY_DEPTH, MAX_BUNDLE_DEPTH, MAX_FRAME_BYTES,
    MAX_MESSAGES_PER_PACKET, MAX_PACKET_BYTES, MAX_UNAUTH_FRAME_BYTES, SlipDecoder, SlipEvent,
    decode_packet, encode_message, slip_encode,
};
pub use dedupe::{DUPLICATE_WINDOW, Dedupe, MAX_PEERS as MAX_DEDUPE_PEERS};
pub use dispatch::{
    Address, CueField, CueRef, MAX_ADDRESS_BYTES, MAX_ADDRESS_PARTS, MAX_MATCHES,
    MAX_REPLY_JSON_BYTES, MAX_UDP_DATAGRAM_BYTES, OscInput, OscShow, Parsed, PlayheadTarget, Query,
    Reply, Resolved, STATUS_DENIED, STATUS_ERROR, STATUS_OK, Scope, ServerInfo, Target, Verb,
    parse_address, reply_message, resolve, upper,
};
pub use pattern::{
    BudgetSpent, CuePattern, MATCH_BUDGET, MAX_ALTERNATIVES, MAX_MATCH_CHARS, PatternError,
    has_pattern_chars,
};
pub use session::{
    ConnectResult, GLOBAL_MAX_FAILURES, GLOBAL_WINDOW, LOCKOUT, MAX_BAD_PASSCODES, MAX_KNOWN_IPS,
    MAX_PEERS, Peer, REPLY_BURST_BYTES, REPLY_BYTES_PER_SEC, Sessions, UDP_IDLE,
};

/// The UDP port QLab listens on, and so do we (UDP and TCP).
pub const OSC_PORT: u16 = 53000;
/// The UDP port replies and updates go to.
pub const OSC_REPLY_PORT: u16 = 53001;

/// The rights every `/connect` is granted, until passcodes carry rights of their own (PROGRESS
/// M4.5c): QLab's `ok:` + the rights joined by `|`.
pub const GRANTED_RIGHTS: &str = "view|edit|control";

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
    /// Valid addresses nothing answers to, and that are no cue's trigger (answered with an error).
    pub unknown_address: u64,
    /// `/workspace/{id}/...` for another workspace (or the right id in the wrong case); answered
    /// with an error.
    pub wrong_workspace: u64,
    /// Messages refused for want of `/connect`.
    pub denied: u64,
    /// Messages answered with an error reply (bad cue, bad argument, unsupported).
    pub rejected: u64,
    /// Replies not sent: over the per-call cap, or not encodable.
    pub replies_dropped: u64,
    /// Replies not sent to an unauthenticated UDP source that used up its byte budget.
    pub replies_throttled: u64,
    /// Update messages not encodable.
    pub updates_dropped: u64,
    /// Messages dropped for repeating the peer's previous one within [`DUPLICATE_WINDOW`].
    pub duplicates_dropped: u64,
    /// Messages whose pattern or `/cue/active` matched more cues than [`MAX_MATCHES`] or used up
    /// the matching budget: the cues found were used, the rest were not.
    pub matches_capped: u64,
    /// Inputs not produced because one call already had [`MAX_INPUTS_PER_CALL`].
    pub inputs_capped: u64,
}

/// IP and UDP headers, counted against the reply budget.
const UDP_HEADER_BYTES: u64 = 28;

/// Most replies from one datagram or one stream read. A wildcard answers once per matched cue
/// (up to [`MAX_MATCHES`]), so a show of several hundred cues must be able to answer
/// `/cue/*/name` in full: 1024 replies of at most a few hundred bytes each (a reply is about 250
/// bytes unless a note or a `/cueLists` is in it) is about the 1 MiB a TCP peer may already send
/// in one frame, and the TCP writer's own pending-byte cap bounds what reaches a slow peer.
pub const MAX_REPLIES_PER_CALL: usize = 1024;
/// Most inputs one datagram or stream read may produce, for the same reason (a wildcard
/// action `/cue/*/stop` acts on every match).
pub const MAX_INPUTS_PER_CALL: usize = 1024;
/// Most reply bytes (JSON bodies) one datagram or stream read may produce. A thousand replies
/// of a few hundred bytes are far under it; it stops a wildcard over large notes from making
/// us build gigabytes for one small message.
pub const MAX_REPLY_BYTES_PER_CALL: usize = 2 * 1024 * 1024;

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
#[derive(Debug)]
pub struct Osc {
    sessions: Sessions,
    dedupe: Dedupe,
    stats: OscStats,
    last_expire: Duration,
    info: ServerInfo,
    /// Reply bytes produced so far by the call being handled.
    call_reply_bytes: usize,
}

impl Default for Osc {
    fn default() -> Self {
        Self::with_info(ServerInfo {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            port: OSC_PORT,
            reply_port: OSC_REPLY_PORT,
        })
    }
}

/// What a message's replies need to know.
struct Asked<'a> {
    peer: Peer,
    now: Duration,
    workspace: openlab_core::WorkspaceId,
    asked: &'a str,
    path: &'a str,
    /// The peer may ask (no passcode is set, or it has connected).
    authed: bool,
}

impl Osc {
    pub fn new() -> Self {
        Self::default()
    }

    /// An endpoint that tells `/version` and `/workspaces` what `info` says.
    pub fn with_info(info: ServerInfo) -> Self {
        Self {
            sessions: Sessions::default(),
            dedupe: Dedupe::default(),
            stats: OscStats::default(),
            last_expire: Duration::ZERO,
            info,
            call_reply_bytes: 0,
        }
    }

    /// How close an identical message from the same peer must follow another to be dropped
    /// ([`DUPLICATE_WINDOW`] by default; zero drops nothing).
    pub fn set_duplicate_window(&mut self, window: Duration) {
        self.dedupe.set_window(window);
    }

    pub fn stats(&self) -> &OscStats {
        &self.stats
    }

    /// Peers that asked for updates (`/updates 1`), for the fan-out. A UDP peer silent for
    /// [`UDP_IDLE`] is not one (`now` as for the handlers).
    pub fn update_subscribers(&self, now: Duration) -> Vec<Peer> {
        self.sessions.update_subscribers(now)
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
        self.dedupe.forget(peer);
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
        self.call_reply_bytes = 0;
        if bytes.len() > MAX_PACKET_BYTES {
            self.stats.packets += 1;
            self.stats.malformed += 1;
            return out;
        }
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
        self.call_reply_bytes = 0;
        // A peer that may not ask yet gets small frames only (it can still send `/connect`).
        let may_ask = self.may_ask(peer, show);
        decoder.set_max_frame(if may_ask {
            MAX_FRAME_BYTES
        } else {
            MAX_UNAUTH_FRAME_BYTES
        });
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

    fn may_ask(&self, peer: Peer, show: &dyn OscShow) -> bool {
        show.passcode().is_none_or(str::is_empty) || self.sessions.is_connected(peer)
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
            self.dedupe.expire(now);
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
        // QLab ignores a message identical to the peer's previous one a moment before.
        if let Ok(encoded) = encode_message(message)
            && self.dedupe.is_duplicate(peer, now, &encoded)
        {
            self.stats.duplicates_dropped += 1;
            return;
        }
        let Some(parsed) = parse_address(&message.addr) else {
            self.stats.bad_address += 1;
            return;
        };
        let workspace = show.workspace_id();
        let passcode = show.passcode().filter(|p| !p.is_empty());
        let authed = passcode.is_none() || self.sessions.is_connected(peer);
        let ask = Asked {
            peer,
            now,
            workspace,
            asked: &message.addr,
            path: &parsed.path,
            authed,
        };

        // Application-level questions need no passcode.
        if let Some(reply) = dispatch::app_reply(show, &self.info, &parsed.address) {
            self.push_reply(&ask, &reply, out);
            return;
        }
        // The id is compared as QLab writes it: upper case. Another id, or the right one in
        // lower case, is an error that says nothing else.
        if let Some(id) = &parsed.workspace
            && *id != upper(workspace)
        {
            self.stats.wrong_workspace += 1;
            self.push_reply(&ask, &Reply::error().scoped(Scope::Bare), out);
            return;
        }
        if parsed.address == Address::Connect {
            let given = dispatch::passcode_arg(&message.args);
            let result = self.sessions.connect(peer, now, passcode, given.as_deref());
            let reply = reply_for_connect(result);
            self.push_reply(&ask, &reply, out);
            return;
        }
        if !authed {
            self.stats.denied += 1;
            let scope = if parsed.address == Address::Updates {
                Scope::App
            } else {
                Scope::Workspace
            };
            self.push_reply(&ask, &Reply::denied().scoped(scope), out);
            return;
        }
        if parsed.address == Address::Updates {
            self.updates(&ask, &message.args, out);
            return;
        }
        let resolved = resolve(show, &parsed, &message.args);
        if resolved.unknown {
            self.stats.unknown_address += 1;
        }
        if resolved.capped {
            self.stats.matches_capped += 1;
        }
        for input in resolved.inputs {
            if out.inputs.len() >= MAX_INPUTS_PER_CALL {
                self.stats.inputs_capped += 1;
            } else {
                out.inputs.push(input);
            }
        }
        for reply in &resolved.replies {
            if reply.status == STATUS_ERROR {
                self.stats.rejected += 1;
            }
            self.push_reply(&ask, reply, out);
        }
    }

    /// `/updates 1` and `/updates 0`: QLab sends no reply when it is allowed.
    fn updates(&mut self, ask: &Asked<'_>, args: &[rosc::OscType], out: &mut Outcome) {
        // A bad argument, or no room for another peer.
        let failed = match dispatch::updates_arg(args) {
            None => true,
            Some(on) => !self.sessions.set_updates(ask.peer, ask.now, on),
        };
        if failed {
            self.stats.rejected += 1;
            self.push_reply(ask, &Reply::error().scoped(Scope::App), out);
        }
    }

    /// Encodes an update message for the fan-out (NET-05), or `None` if it is not encodable.
    /// Updates are small and follow what the show does, so they are not held to the byte budget
    /// of unauthenticated sources: a peer only gets them after `/connect` when a passcode is set.
    pub fn encode_update(&mut self, message: &OscMessage) -> Option<Vec<u8>> {
        let bytes = encode_message(message).ok();
        if bytes.is_none() {
            self.stats.updates_dropped += 1;
        }
        bytes
    }

    /// Queues a reply unless a cap says no: at most [`MAX_REPLIES_PER_CALL`] per datagram or
    /// stream read, a body no larger than the peer may be sent (a datagram's worth to a UDP peer
    /// that may ask, 1 MiB over TCP, [`MAX_REPLY_JSON_BYTES`] to one that may not), and for a UDP
    /// peer that may not ask a per-source-IP byte budget (a forged source address must not turn
    /// us into an amplifier). Dropped replies are counted.
    fn push_reply(&mut self, ask: &Asked<'_>, reply: &Reply, out: &mut Outcome) {
        if out.replies.len() >= MAX_REPLIES_PER_CALL {
            self.stats.replies_dropped += 1;
            return;
        }
        let framing = ask.asked.len() + "/reply".len() + 16;
        let max_json = match (ask.authed, ask.peer) {
            (false, _) => MAX_REPLY_JSON_BYTES,
            (true, Peer::Udp(_)) => MAX_UDP_DATAGRAM_BYTES.saturating_sub(framing),
            (true, Peer::Tcp { .. }) => MAX_FRAME_BYTES.saturating_sub(framing),
        };
        let message = reply_message(ask.workspace, ask.asked, ask.path, reply, max_json);
        let size = message
            .args
            .iter()
            .fold(message.addr.len(), |n, arg| match arg {
                rosc::OscType::String(text) => n + text.len(),
                _ => n,
            });
        if self.call_reply_bytes.saturating_add(size) > MAX_REPLY_BYTES_PER_CALL {
            self.stats.replies_dropped += 1;
            return;
        }
        self.call_reply_bytes += size;
        if matches!(ask.peer, Peer::Udp(_)) && !ask.authed {
            let Ok(bytes) = encode_message(&message) else {
                self.stats.replies_dropped += 1;
                return;
            };
            let cost = u64::try_from(bytes.len())
                .unwrap_or(u64::MAX)
                .saturating_add(UDP_HEADER_BYTES);
            if !self
                .sessions
                .spend_reply_bytes(ask.peer.ip(), ask.now, cost)
            {
                self.stats.replies_throttled += 1;
                return;
            }
        }
        out.replies.push(message);
    }
}

/// QLab's answers to `/connect`: `ok:` and the granted rights, `badpass` as an `ok` status (not
/// an error), `denied` while locked out.
fn reply_for_connect(result: ConnectResult) -> Reply {
    match result {
        ConnectResult::Accepted => Reply::ok(json!(format!("ok:{GRANTED_RIGHTS}"))),
        ConnectResult::BadPasscode => Reply::ok(json!("badpass")),
        ConnectResult::LockedOut => Reply::denied(),
        ConnectResult::TableFull => Reply::error(),
    }
}
