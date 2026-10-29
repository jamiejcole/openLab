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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use openlab_core::protocol::DocId;
use openlab_core::{OscAccess, OscRight, OscRights};
use rosc::OscMessage;
use serde_json::json;

pub use codec::{
    DecodeError, EncodeError, MAX_ARRAY_DEPTH, MAX_BUNDLE_DEPTH, MAX_FRAME_BYTES,
    MAX_MESSAGES_PER_PACKET, MAX_PACKET_BYTES, MAX_UNAUTH_FRAME_BYTES, SlipDecoder, SlipEvent,
    decode_packet, encode_message, slip_encode,
};
pub use dedupe::{DUPLICATE_WINDOW, Dedupe, MAX_PEERS as MAX_DEDUPE_PEERS};
pub use dispatch::{
    Address, CueField, CueRef, DocShow, MAX_ADDRESS_BYTES, MAX_ADDRESS_PARTS, MAX_MATCHES,
    MAX_REPLY_JSON_BYTES, MAX_UDP_DATAGRAM_BYTES, Named, OscInput, OscShow, Parsed, PlayheadTarget,
    Query, Reply, Resolved, STATUS_DENIED, STATUS_ERROR, STATUS_OK, Scope, ServerInfo, Target,
    Verb, find_doc, parse_address, reply_message, required_right, resolve, upper,
};
pub use pattern::{
    BudgetSpent, CuePattern, MATCH_BUDGET, MAX_ALTERNATIVES, MAX_MATCH_CHARS, PatternError,
    has_pattern_chars,
};
pub use session::{
    Budget, Budgets, ConnectResult, GLOBAL_MAX_FAILURES, GLOBAL_WINDOW, LOCKOUT, Lockouts,
    MAX_BAD_PASSCODES, MAX_KNOWN_IPS, MAX_PEERS, OPEN_REPLY_BURST_BYTES, OPEN_REPLY_BYTES_PER_SEC,
    Peer, REPLY_BURST_BYTES, REPLY_BYTES_PER_SEC, Sessions, UDP_IDLE,
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
    /// Valid addresses nothing answers to, and that are no cue's trigger (answered with an error).
    pub unknown_address: u64,
    /// `/workspace/{id}/...` for another workspace (or the right id in the wrong case); answered
    /// with an error.
    pub wrong_workspace: u64,
    /// Messages refused for want of a right: before `/connect`, or the passcode's rights do not
    /// include what the message needs.
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
    /// The doc each of `inputs` is for (the same length): an un-prefixed `/go` is one input per
    /// open doc.
    pub input_docs: Vec<DocId>,
    /// Warnings for the show log (a workspace name that several docs share).
    pub warnings: Vec<String>,
    /// To send back to the peer that sent the bytes: UDP to its address on [`OSC_REPLY_PORT`]
    /// (or the port it set with `/udpReplyPort`, see [`Osc::reply_port`]),
    /// TCP SLIP-framed on the same connection.
    pub replies: Vec<OscMessage>,
}

impl Outcome {
    /// Each input with the doc it is for.
    pub fn routed(self) -> impl Iterator<Item = (DocId, OscInput)> {
        self.input_docs.into_iter().zip(self.inputs)
    }
}

/// The workspaces the messages of one packet (a datagram or a SLIP frame) name with a
/// `/workspace/{id}` prefix, in order, as written; `None` when one of them can reach every open
/// doc: it has no prefix, or it is `/version` or `/workspaces`, which are answered from every
/// doc whatever the prefix, or the packet does not decode (fail safe: what it reaches is not
/// known). A message with an unusable address reaches no doc. Lets the runtime tell which docs a
/// packet needs before it handles it. No size cut of its own: a TCP frame may be larger than a
/// datagram, and the frame limit already bounds it.
pub fn named_workspaces(packet: &[u8]) -> Option<Vec<String>> {
    let messages = decode_packet(packet).ok()?;
    let mut named = Vec::new();
    for message in &messages {
        let Some(parsed) = parse_address(&message.addr) else {
            continue;
        };
        if matches!(parsed.address, Address::Version | Address::Workspaces) {
            return None;
        }
        named.push(parsed.workspace?);
    }
    Some(named)
}

/// Most (peer, name) pairs remembered as warned about an ambiguous name.
const MAX_AMBIGUOUS_WARNED: usize = 64;

/// The doc the single-show calls ([`Osc::handle_datagram`] and friends) stand for.
pub const SINGLE_DOC: DocId = DocId(0);

/// The OSC endpoint's state: per-(peer, doc) sessions, per-IP reply budgets and counters.
#[derive(Debug)]
pub struct Osc {
    /// One table per open doc (ARCHITECTURE section 7: `/connect`, its passcode, sessions and
    /// lockouts are per peer and doc). Doc ids rise in open order, so this is open order too.
    docs: BTreeMap<DocId, Sessions>,
    /// Wrong-passcode failures per IP and in all, for every doc together: a guesser gets no
    /// fresh budget per doc.
    lockouts: Lockouts,
    /// What each peer asked of the application with `/updates` and `/udpReplyPort`. These are
    /// not workspace settings (QLab: a client asks for updates, and for its own reply port), so
    /// a doc opened later gets them for the peers that asked, and a closed doc does not take
    /// them along.
    wishes: HashMap<Peer, Wish>,
    /// Per source IP, whatever doc a reply is about.
    budgets: Budgets,
    /// (peer, name) pairs already warned about an ambiguous workspace name.
    ambiguous_warned: Vec<(Peer, String)>,
    dedupe: Dedupe,
    stats: OscStats,
    last_expire: Duration,
    info: ServerInfo,
    /// Reply bytes produced so far by the call being handled.
    call_reply_bytes: usize,
    /// Pattern-matching steps left for the call being handled (all its messages, for every doc,
    /// share them: fanning a message out to several docs does not multiply the work).
    call_match_budget: u64,
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

/// A peer's application-level requests, kept until it goes silent (UDP) or disconnects (TCP).
#[derive(Debug, Default)]
struct Wish {
    updates: bool,
    reply_port: Option<u16>,
    last_seen: Duration,
}

/// What a message's replies need to know.
struct Asked<'a> {
    peer: Peer,
    now: Duration,
    /// The doc's workspace (unused by application and bare replies).
    workspace: openlab_core::WorkspaceId,
    asked: &'a str,
    path: &'a str,
    /// The peer may ask: its rights include View.
    authed: bool,
    /// The UDP byte budget the reply is held to; `None`: none (the peer connected with a real
    /// passcode, which a forged source cannot).
    budget: Option<Budget>,
}

impl Osc {
    pub fn new() -> Self {
        Self::default()
    }

    /// An endpoint that tells `/version` and `/workspaces` what `info` says.
    pub fn with_info(info: ServerInfo) -> Self {
        Self {
            docs: BTreeMap::new(),
            lockouts: Lockouts::default(),
            wishes: HashMap::new(),
            budgets: Budgets::default(),
            ambiguous_warned: Vec::new(),
            dedupe: Dedupe::default(),
            stats: OscStats::default(),
            last_expire: Duration::ZERO,
            info,
            call_reply_bytes: 0,
            call_match_budget: MATCH_BUDGET,
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

    fn sessions(&mut self, doc: DocId) -> &mut Sessions {
        self.docs.entry(doc).or_default()
    }

    /// Gives a doc this endpoint has not seen yet the `/updates` and `/udpReplyPort` of the peers
    /// that asked before it opened (`access`: its passcode table). A peer that may not view the
    /// doc keeps no reply port in it, as when it sends one there.
    pub fn open_doc(&mut self, doc: DocId, now: Duration, access: &OscAccess) {
        if self.docs.contains_key(&doc) {
            return;
        }
        let may_view = access.no_passcode.allows(OscRight::View);
        let sessions = self.docs.entry(doc).or_default();
        sessions.set_strangers_may_view(may_view);
        for (peer, wish) in &self.wishes {
            let alive =
                matches!(peer, Peer::Tcp { .. }) || now.saturating_sub(wish.last_seen) < UDP_IDLE;
            if !alive {
                continue;
            }
            if wish.updates {
                sessions.set_updates(*peer, wish.last_seen, true);
            }
            if let Some(port) = wish.reply_port.filter(|_| may_view) {
                sessions.set_reply_port(*peer, wish.last_seen, port);
            }
        }
    }

    /// Records (or, with nothing left to remember, forgets) what `peer` asked for.
    fn note_wish(&mut self, peer: Peer, now: Duration, set: impl FnOnce(&mut Wish)) {
        if !self.wishes.contains_key(&peer) && self.wishes.len() >= MAX_PEERS {
            return;
        }
        let wish = self.wishes.entry(peer).or_default();
        set(wish);
        wish.last_seen = now;
        if !wish.updates && wish.reply_port.is_none() {
            self.wishes.remove(&peer);
        }
    }

    /// Peers that asked any doc for updates (`/updates 1`). A UDP peer silent for [`UDP_IDLE`]
    /// is not one (`now` as for the handlers).
    pub fn update_subscribers(&self, now: Duration) -> Vec<Peer> {
        // A set, not `Vec::contains`: up to 8 docs of 1024 subscribers each, and this runs on
        // every eviction a connect flood asks for.
        let mut seen = HashSet::new();
        self.docs
            .values()
            .flat_map(|sessions| sessions.update_subscribers(now))
            .filter(|peer| seen.insert(*peer))
            .collect()
    }

    /// Peers subscribed to `doc`'s updates: the fan-out of that doc's changes.
    pub fn update_subscribers_in(&self, doc: DocId, now: Duration) -> Vec<Peer> {
        self.docs
            .get(&doc)
            .map(|s| s.update_subscribers(now))
            .unwrap_or_default()
    }

    /// How many addresses are remembered as having connected to `doc` (at most
    /// [`MAX_KNOWN_IPS`]).
    pub fn known_ips_in(&self, doc: DocId) -> usize {
        self.docs.get(&doc).map_or(0, Sessions::known_len)
    }

    /// The same, for the single-show calls.
    pub fn known_ips(&self) -> usize {
        self.known_ips_in(SINGLE_DOC)
    }

    /// Whether `peer` completed `/connect` with a passcode of some doc's table.
    pub fn is_connected(&self, peer: Peer) -> bool {
        self.docs.values().any(|s| s.is_connected(peer))
    }

    /// Whether `peer` completed `/connect` with a passcode of `doc`'s table.
    pub fn is_connected_in(&self, doc: DocId, peer: Peer) -> bool {
        self.docs.get(&doc).is_some_and(|s| s.is_connected(peer))
    }

    /// A TCP connection closed: its sessions with every doc end.
    pub fn disconnect(&mut self, peer: Peer) {
        for sessions in self.docs.values_mut() {
            sessions.remove(peer);
        }
        self.wishes.remove(&peer);
        self.dedupe.forget(peer);
    }

    /// Drops every `/connect`, subscription and known address of every doc.
    pub fn reset_sessions(&mut self) {
        self.wishes.clear();
        for sessions in self.docs.values_mut() {
            sessions.clear();
        }
    }

    /// Drops `doc`'s `/connect`s, subscriptions and known addresses (its copy now holds another
    /// workspace).
    pub fn reset_doc(&mut self, doc: DocId) {
        if let Some(sessions) = self.docs.get_mut(&doc) {
            sessions.clear();
        }
    }

    /// Drops every doc's `/connect`s and subscriptions but keeps the known addresses.
    pub fn end_sessions(&mut self) {
        for sessions in self.docs.values_mut() {
            sessions.end_all();
        }
    }

    /// Drops `doc`'s `/connect`s and subscriptions but keeps its known addresses (its passcode
    /// table changed).
    pub fn end_doc_sessions(&mut self, doc: DocId) {
        if let Some(sessions) = self.docs.get_mut(&doc) {
            sessions.end_all();
        }
    }

    /// `doc` closed: everything about it is forgotten, its sessions, lockouts and known
    /// addresses (a doc that opens later starts afresh, as QLab's do).
    pub fn close_doc(&mut self, doc: DocId) {
        self.docs.remove(&doc);
    }

    /// Handles one UDP datagram from `peer` (`now` is monotonic time since the engine started),
    /// for one open show.
    pub fn handle_datagram(
        &mut self,
        peer: Peer,
        now: Duration,
        bytes: &[u8],
        show: &dyn OscShow,
    ) -> Outcome {
        self.handle_datagram_arrived(peer, now, now, bytes, show)
    }

    /// As [`Osc::handle_datagram`], for a datagram that arrived at `arrived` (on the same clock
    /// as `now`) and was handled later: repeats are judged by arrival, so two GOs 250 ms apart
    /// that the task gets to back to back are both GOs.
    pub fn handle_datagram_arrived(
        &mut self,
        peer: Peer,
        now: Duration,
        arrived: Duration,
        bytes: &[u8],
        show: &dyn OscShow,
    ) -> Outcome {
        let docs = [DocShow {
            doc: SINGLE_DOC,
            show,
        }];
        self.handle_datagram_docs(peer, now, arrived, bytes, &docs)
    }

    /// One UDP datagram, for the docs open now (every one, in open order; none is fine). Sessions
    /// of docs not among them are forgotten (they closed).
    pub fn handle_datagram_docs(
        &mut self,
        peer: Peer,
        now: Duration,
        arrived: Duration,
        bytes: &[u8],
        docs: &[DocShow<'_>],
    ) -> Outcome {
        let mut out = Outcome::default();
        self.start_call(docs);
        if bytes.len() > MAX_PACKET_BYTES {
            self.stats.packets += 1;
            self.stats.malformed += 1;
            return out;
        }
        self.handle_packet(peer, now, arrived, bytes, docs, &mut out);
        out
    }

    /// Handles bytes read from a TCP connection, for one open show. `decoder` is that
    /// connection's own decoder.
    pub fn handle_stream(
        &mut self,
        peer: Peer,
        now: Duration,
        decoder: &mut SlipDecoder,
        bytes: &[u8],
        show: &dyn OscShow,
    ) -> Outcome {
        self.handle_stream_arrived(peer, now, now, decoder, bytes, show)
    }

    /// As [`Osc::handle_stream`], for bytes that were read at `arrived`.
    pub fn handle_stream_arrived(
        &mut self,
        peer: Peer,
        now: Duration,
        arrived: Duration,
        decoder: &mut SlipDecoder,
        bytes: &[u8],
        show: &dyn OscShow,
    ) -> Outcome {
        let docs = [DocShow {
            doc: SINGLE_DOC,
            show,
        }];
        self.handle_stream_docs(peer, now, arrived, decoder, bytes, &docs)
    }

    /// Bytes read from a TCP connection, for the docs open now (as
    /// [`Osc::handle_datagram_docs`]).
    pub fn handle_stream_docs(
        &mut self,
        peer: Peer,
        now: Duration,
        arrived: Duration,
        decoder: &mut SlipDecoder,
        bytes: &[u8],
        docs: &[DocShow<'_>],
    ) -> Outcome {
        let mut out = Outcome::default();
        self.start_call(docs);
        let may_ask = self.may_ask_any(peer, docs);
        for frame in self.read_frames(decoder, bytes, may_ask) {
            self.handle_packet(peer, now, arrived, &frame, docs, &mut out);
        }
        out
    }

    /// Whether `peer` may ask anything (has View) in one of `docs`: its TCP frames may then be
    /// full size.
    pub fn may_ask_any(&self, peer: Peer, docs: &[DocShow<'_>]) -> bool {
        docs.iter().any(|d| self.may_ask(d.doc, peer, d.show))
    }

    /// The complete frames in bytes read from a TCP connection (`decoder` is its own), without
    /// handling them; oversize and badly escaped frames are counted and dropped. A peer that may
    /// not ask yet (`may_ask`, see [`Osc::may_ask_any`]) gets small frames only: it can still
    /// send `/connect`.
    pub fn read_frames(
        &mut self,
        decoder: &mut SlipDecoder,
        bytes: &[u8],
        may_ask: bool,
    ) -> Vec<Vec<u8>> {
        decoder.set_max_frame(if may_ask {
            MAX_FRAME_BYTES
        } else {
            MAX_UNAUTH_FRAME_BYTES
        });
        let mut events = Vec::new();
        decoder.feed(bytes, &mut events);
        let mut frames = Vec::new();
        for event in events {
            match event {
                SlipEvent::Frame(frame) => frames.push(frame),
                SlipEvent::Oversize => self.stats.oversize_frames += 1,
                SlipEvent::BadEscape => self.stats.malformed += 1,
            }
        }
        frames
    }

    /// Frames [`Osc::read_frames`] took from a TCP connection, handled as one read, for the docs
    /// open now (as [`Osc::handle_datagram_docs`]).
    pub fn handle_frames_docs(
        &mut self,
        peer: Peer,
        now: Duration,
        arrived: Duration,
        frames: &[Vec<u8>],
        docs: &[DocShow<'_>],
    ) -> Outcome {
        let mut out = Outcome::default();
        self.start_call(docs);
        for frame in frames {
            self.handle_packet(peer, now, arrived, frame, docs, &mut out);
        }
        out
    }

    /// The limits that are per datagram or per read start afresh; a doc no longer open is
    /// forgotten.
    fn start_call(&mut self, docs: &[DocShow<'_>]) {
        self.call_reply_bytes = 0;
        self.call_match_budget = MATCH_BUDGET;
        self.docs
            .retain(|doc, _| docs.iter().any(|d| d.doc == *doc));
    }

    /// What `peer` may do in `doc`: the rights `/connect` gave it there, else the No Passcode
    /// rights.
    fn rights_of(&self, doc: DocId, peer: Peer, show: &dyn OscShow) -> OscRights {
        self.docs
            .get(&doc)
            .and_then(|s| s.rights(peer))
            .unwrap_or(show.access().no_passcode)
    }

    /// Whether `doc`'s updates (which carry cue data) may be sent to `peer` now: it needs View
    /// there, judged when each update goes out, so a table edit takes effect on subscribers at
    /// once.
    pub fn may_receive_updates(&self, doc: DocId, peer: Peer, access: &OscAccess) -> bool {
        self.docs
            .get(&doc)
            .and_then(|s| s.rights(peer))
            .unwrap_or(access.no_passcode)
            .allows(OscRight::View)
    }

    fn may_ask(&self, doc: DocId, peer: Peer, show: &dyn OscShow) -> bool {
        self.rights_of(doc, peer, show).allows(OscRight::View)
    }

    fn handle_packet(
        &mut self,
        peer: Peer,
        now: Duration,
        arrived: Duration,
        bytes: &[u8],
        docs: &[DocShow<'_>],
        out: &mut Outcome,
    ) {
        self.stats.packets += 1;
        if now.saturating_sub(self.last_expire) >= EXPIRE_EVERY {
            for sessions in self.docs.values_mut() {
                sessions.expire(now);
            }
            self.lockouts.expire(now);
            self.wishes.retain(|p, w| {
                matches!(p, Peer::Tcp { .. }) || now.saturating_sub(w.last_seen) < UDP_IDLE
            });
            self.budgets.expire(now);
            self.dedupe.expire(now);
            self.last_expire = now;
        }
        for d in docs {
            self.open_doc(d.doc, now, d.show.access());
        }
        // A UDP session lives while the peer sends anything, to whichever doc.
        for sessions in self.docs.values_mut() {
            sessions.touch(peer, now);
        }
        if let Some(wish) = self.wishes.get_mut(&peer) {
            if matches!(peer, Peer::Udp(_)) && now.saturating_sub(wish.last_seen) >= UDP_IDLE {
                self.wishes.remove(&peer);
            } else {
                wish.last_seen = now;
            }
        }
        match decode_packet(bytes) {
            Ok(messages) => {
                for message in &messages {
                    self.handle_message(peer, now, arrived, message, docs, out);
                }
            }
            Err(_) => self.stats.malformed += 1,
        }
    }

    /// What a reply about no doc in particular (an application message, or one for a workspace
    /// that is not open) needs: View in any doc lets it be large, and it is held to a byte
    /// budget unless the peer connected to a doc with a real passcode; the strict one when no
    /// open doc lets a stranger ask (or none is open).
    fn app_asked<'a>(
        &self,
        peer: Peer,
        now: Duration,
        asked: &'a str,
        path: &'a str,
        docs: &[DocShow<'_>],
    ) -> Asked<'a> {
        let proved = docs
            .iter()
            .any(|d| d.show.access().requires_passcode() && self.is_connected_in(d.doc, peer));
        let open = docs.iter().any(|d| !d.show.access().requires_passcode());
        Asked {
            peer,
            now,
            workspace: openlab_core::WorkspaceId(uuid::Uuid::nil()),
            asked,
            path,
            authed: docs.iter().any(|d| self.may_ask(d.doc, peer, d.show)),
            budget: if proved {
                None
            } else if open {
                Some(Budget::Open)
            } else {
                Some(Budget::Strict)
            },
        }
    }

    fn handle_message(
        &mut self,
        peer: Peer,
        now: Duration,
        arrived: Duration,
        message: &OscMessage,
        docs: &[DocShow<'_>],
        out: &mut Outcome,
    ) {
        self.stats.messages += 1;
        // QLab ignores a message identical to the peer's previous one a moment before.
        if let Ok(encoded) = encode_message(message)
            && self.dedupe.is_duplicate(peer, arrived, &encoded)
        {
            self.stats.duplicates_dropped += 1;
            return;
        }
        let Some(parsed) = parse_address(&message.addr) else {
            self.stats.bad_address += 1;
            return;
        };
        // Application-level questions need no passcode, and are answered once.
        if let Some(reply) = dispatch::app_reply(docs, &self.info, &parsed.address) {
            let ask = self.app_asked(peer, now, &message.addr, &parsed.path, docs);
            self.push_reply(&ask, &reply, out);
            return;
        }
        // `/workspace/{id}` picks one doc: the id as QLab writes it (upper case), or the name.
        // Another id, the right one in lower case, or any workspace message while no doc is open,
        // is an error that says nothing else.
        let target = match &parsed.workspace {
            Some(id) => match dispatch::find_doc(docs, id) {
                Named::Doc(doc) => Some(doc),
                Named::None => {
                    self.wrong_workspace(peer, now, &message.addr, &parsed.path, docs, out);
                    return;
                }
                Named::Ambiguous => {
                    self.warn_ambiguous(peer, id, out);
                    self.wrong_workspace(peer, now, &message.addr, &parsed.path, docs, out);
                    return;
                }
            },
            None => None,
        };
        if target.is_none() {
            match parsed.address {
                Address::Updates => return self.updates(peer, now, message, &parsed, docs, out),
                Address::UdpReplyPort => {
                    return self.udp_reply_port(peer, now, message, &parsed, docs, out);
                }
                _ if docs.is_empty() => {
                    self.wrong_workspace(peer, now, &message.addr, &parsed.path, docs, out);
                    return;
                }
                _ => {}
            }
        }
        match target {
            Some(doc) => {
                self.doc_message(peer, now, (message, &parsed), doc, true, out);
            }
            // QLab 5: a message without the prefix goes to every open workspace on the port,
            // and each answers for itself.
            None => {
                // A passcode that some doc's table accepts is a console connecting to that doc,
                // not a guess at the others: it must not lock itself out of them (two docs with
                // two passcodes, each sent un-prefixed).
                let accepted_somewhere = parsed.address == Address::Connect
                    && dispatch::passcode_arg(&message.args).is_some_and(|given| {
                        docs.iter()
                            .any(|d| session::passcode_matches(d.show.access(), &given))
                    });
                // The lockouts are shared by the docs, so one wrong guess is one failure, however
                // many docs answer it.
                let mut count = !accepted_somewhere;
                for doc in docs {
                    let counted = self.doc_message(peer, now, (message, &parsed), *doc, count, out);
                    count &= !counted;
                }
            }
        }
    }

    /// Says once per peer and name that a name matched several docs (the operator must rename
    /// one or address it by id). Remembers at most [`MAX_AMBIGUOUS_WARNED`] pairs; past that it
    /// starts afresh, so a flood of names costs a bounded table and an occasional repeat.
    fn warn_ambiguous(&mut self, peer: Peer, name: &str, out: &mut Outcome) {
        if self
            .ambiguous_warned
            .iter()
            .any(|(p, n)| *p == peer && n == name)
        {
            return;
        }
        if self.ambiguous_warned.len() >= MAX_AMBIGUOUS_WARNED {
            self.ambiguous_warned.clear();
        }
        self.ambiguous_warned.push((peer, name.to_owned()));
        out.warnings.push(format!(
            "OSC from {} named workspace \"{name}\", which several open workspaces are called: \
             refused; rename one or address it by its id",
            peer.ip()
        ));
    }

    fn wrong_workspace(
        &mut self,
        peer: Peer,
        now: Duration,
        asked: &str,
        path: &str,
        docs: &[DocShow<'_>],
        out: &mut Outcome,
    ) {
        self.stats.wrong_workspace += 1;
        let ask = self.app_asked(peer, now, asked, path, docs);
        self.push_reply(&ask, &Reply::error().scoped(Scope::Bare), out);
    }

    /// A workspace message for one doc: its rights, its passcode, its cues; its replies carry
    /// its `workspace_id`.
    fn doc_message(
        &mut self,
        peer: Peer,
        now: Duration,
        (message, parsed): (&OscMessage, &Parsed),
        target: DocShow<'_>,
        count_failure: bool,
        out: &mut Outcome,
    ) -> bool {
        let DocShow { doc, show } = target;
        let access = show.access();
        self.sessions(doc)
            .set_strangers_may_view(access.no_passcode.allows(OscRight::View));
        let rights = self.rights_of(doc, peer, show);
        let connected = self.is_connected_in(doc, peer);
        let ask = Asked {
            peer,
            now,
            workspace: show.workspace_id(),
            asked: &message.addr,
            path: &parsed.path,
            authed: rights.allows(OscRight::View),
            budget: match (connected, access.requires_passcode()) {
                (true, true) => None,
                (false, true) => Some(Budget::Strict),
                (_, false) => Some(Budget::Open),
            },
        };
        if parsed.address == Address::Connect {
            let given = dispatch::passcode_arg(&message.args);
            let (result, counted) = self.docs.entry(doc).or_default().connect_counting(
                &mut self.lockouts,
                peer,
                now,
                access,
                given.as_deref(),
                count_failure,
            );
            let reply = reply_for_connect(result);
            self.push_reply(&ask, &reply, out);
            return counted;
        }
        if required_right(&parsed.address, &message.args).is_some_and(|r| !rights.allows(r)) {
            self.stats.denied += 1;
            self.push_reply(&ask, &Reply::denied(), out);
            return false;
        }
        let resolved = resolve(show, parsed, &message.args, &mut self.call_match_budget);
        if resolved.unknown {
            self.stats.unknown_address += 1;
        }
        if resolved.capped {
            self.stats.matches_capped += 1;
        }
        if resolved.trigger && !rights.allows(OscRight::Control) {
            // The same `error` as an address nothing answers to, so a peer without Control
            // cannot tell a trigger from a missing cue.
            self.stats.rejected += 1;
            self.push_reply(&ask, &Reply::error(), out);
            return false;
        }
        for input in resolved.inputs {
            if out.inputs.len() >= MAX_INPUTS_PER_CALL {
                self.stats.inputs_capped += 1;
            } else {
                out.inputs.push(input);
                out.input_docs.push(doc);
            }
        }
        for reply in &resolved.replies {
            if reply.status == STATUS_ERROR {
                self.stats.rejected += 1;
            }
            self.push_reply(&ask, reply, out);
        }
        false
    }

    /// `/updates 1` and `/updates 0`, for every open doc and any opened later (an application
    /// message: it cannot be sent to one; it is the peer's, not the workspace's). QLab sends no reply when it is allowed; a bad argument, no doc open, or a
    /// doc with no room for another peer is one `error`.
    fn updates(
        &mut self,
        peer: Peer,
        now: Duration,
        message: &OscMessage,
        parsed: &Parsed,
        docs: &[DocShow<'_>],
        out: &mut Outcome,
    ) {
        let failed = match dispatch::updates_arg(&message.args) {
            None => true,
            Some(_) if docs.is_empty() => true,
            Some(on) => {
                let mut refused = false;
                for d in docs {
                    let sessions = self.sessions(d.doc);
                    sessions
                        .set_strangers_may_view(d.show.access().no_passcode.allows(OscRight::View));
                    refused |= !sessions.set_updates(peer, now, on);
                }
                if !refused {
                    self.note_wish(peer, now, |w| w.updates = on);
                }
                refused
            }
        };
        if failed {
            self.stats.rejected += 1;
            let ask = self.app_asked(peer, now, &message.addr, &parsed.path, docs);
            self.push_reply(&ask, &Reply::error().scoped(Scope::App), out);
        }
    }

    /// `/udpReplyPort {port}`: QLab sends no reply when it is allowed, like `/updates`. It needs
    /// View in some doc, and is kept in the session of each doc where the peer has it and for docs
    /// opened later (where it again needs View). Ignored
    /// over TCP (replies go back on the connection).
    fn udp_reply_port(
        &mut self,
        peer: Peer,
        now: Duration,
        message: &OscMessage,
        parsed: &Parsed,
        docs: &[DocShow<'_>],
        out: &mut Outcome,
    ) {
        if matches!(peer, Peer::Tcp { .. }) {
            return;
        }
        let ask = self.app_asked(peer, now, &message.addr, &parsed.path, docs);
        let viewing: Vec<DocShow<'_>> = docs
            .iter()
            .copied()
            .filter(|d| self.may_ask(d.doc, peer, d.show))
            .collect();
        if viewing.is_empty() {
            self.stats.denied += 1;
            self.push_reply(&ask, &Reply::denied().scoped(Scope::App), out);
            return;
        }
        let failed = match dispatch::reply_port_arg(&message.args) {
            None => true,
            Some(port) => {
                let mut refused = false;
                for d in &viewing {
                    let sessions = self.sessions(d.doc);
                    sessions
                        .set_strangers_may_view(d.show.access().no_passcode.allows(OscRight::View));
                    refused |= !sessions.set_reply_port(peer, now, port);
                }
                if !refused {
                    self.note_wish(peer, now, |w| w.reply_port = Some(port));
                }
                refused
            }
        };
        if failed {
            self.stats.rejected += 1;
            self.push_reply(&ask, &Reply::error().scoped(Scope::App), out);
        }
    }

    /// The port to send `peer` (a UDP peer) its replies and updates on: the one it set with
    /// `/udpReplyPort` while a session of it lives (the first doc's, in open order; it is set in
    /// all of them at once), else `None` (use the server default).
    pub fn reply_port(&self, peer: Peer, now: Duration) -> Option<u16> {
        self.docs.values().find_map(|s| s.reply_port(peer, now))
    }

    /// Encodes an update message for the fan-out (NET-05), or `None` if it is not encodable.
    pub fn encode_update(&mut self, message: &OscMessage) -> Option<Vec<u8>> {
        let bytes = encode_message(message).ok();
        if bytes.is_none() {
            self.stats.updates_dropped += 1;
        }
        bytes
    }

    /// Whether an encoded update of `doc`'s, `len` bytes, may be sent to `peer` now. A TCP peer
    /// is paced by its connection, and a UDP peer that completed `/connect` to that doc with a
    /// passcode is not held to a budget; any other UDP peer shares the byte budget of its
    /// replies (per IP, whatever the doc), so a forged `/updates 1` cannot aim more than that at
    /// a victim.
    pub fn update_allowed(
        &mut self,
        doc: DocId,
        peer: Peer,
        now: Duration,
        len: usize,
        passcode_set: bool,
    ) -> bool {
        if matches!(peer, Peer::Tcp { .. }) || (passcode_set && self.is_connected_in(doc, peer)) {
            return true;
        }
        let cost = u64::try_from(len)
            .unwrap_or(u64::MAX)
            .saturating_add(UDP_HEADER_BYTES);
        let allowed = self
            .budgets
            .spend_reply_bytes(peer.ip(), Budget::Open, now, cost);
        if !allowed {
            self.stats.updates_dropped += 1;
        }
        allowed
    }

    /// Queues a reply unless a cap says no: at most [`MAX_REPLIES_PER_CALL`] per datagram or
    /// stream read (for every doc together), a body no larger than the peer may be sent (a
    /// datagram's worth to a UDP peer that may ask, 1 MiB over TCP, [`MAX_REPLY_JSON_BYTES`] to
    /// one that may not), and for a UDP peer that has not connected with a passcode a
    /// per-source-IP byte budget (a forged source address must not turn us into an amplifier).
    /// Dropped replies are counted.
    fn push_reply(&mut self, ask: &Asked<'_>, reply: &Reply, out: &mut Outcome) {
        // Checked before the reply is built: once a call is at a cap the rest of a flood costs
        // nothing.
        if out.replies.len() >= MAX_REPLIES_PER_CALL
            || self.call_reply_bytes >= MAX_REPLY_BYTES_PER_CALL
        {
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
            // The call is full from here on: the early check above must stop later replies
            // (even small ones) before they are built.
            self.call_reply_bytes = MAX_REPLY_BYTES_PER_CALL;
            self.stats.replies_dropped += 1;
            return;
        }
        self.call_reply_bytes += size;
        if let (Peer::Udp(ip), Some(kind)) = (ask.peer, ask.budget) {
            let Ok(bytes) = encode_message(&message) else {
                self.stats.replies_dropped += 1;
                return;
            };
            let cost = u64::try_from(bytes.len())
                .unwrap_or(u64::MAX)
                .saturating_add(UDP_HEADER_BYTES);
            if !self.budgets.spend_reply_bytes(ip, kind, ask.now, cost) {
                self.stats.replies_throttled += 1;
                return;
            }
        }
        out.replies.push(message);
    }
}

/// QLab's answers to `/connect`: `ok:` and the granted rights (`ok:view|edit`, `ok:` for none),
/// `badpass` as an `ok` status (not an error), `denied` while locked out.
fn reply_for_connect(result: ConnectResult) -> Reply {
    match result {
        ConnectResult::Accepted(rights) => Reply::ok(json!(format!("ok:{}", rights.text()))),
        ConnectResult::BadPasscode => Reply::ok(json!("badpass")),
        ConnectResult::LockedOut => Reply::denied(),
        ConnectResult::TableFull => Reply::error(),
    }
}
