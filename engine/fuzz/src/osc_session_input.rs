// SPDX-License-Identifier: GPL-3.0-or-later

//! The OSC session fuzz body (QA-04): the dispatcher fed a script of datagrams and stream chunks
//! from several peers over virtual time, with disconnects, session resets and update fan-out in
//! between. The `osc` target feeds one datagram or one stream to a fresh endpoint; this one
//! reaches what only a sequence reaches: lockouts, the failure windows, idle expiry, the peer
//! table, the reply byte budget. Shared by the `osc_session` fuzz target and the engine's
//! `fuzz_osc_session` test; public APIs only, no `unwrap`, `expect`, `panic!` or indexing.
//!
//! Two docs are open (M4.15h: an un-prefixed message goes to each). Input: byte 0 picks whether
//! the first has the passcode `1234` (bit 0) and whether the second has its own, `5678` (bit 1;
//! else it is open). The passcodes differ so that a peer with rights in one doc and a leak into
//! the other cannot hide. The rest is records separated by 0x1e; a record is `[op][peer][delay][payload...]`:
//!
//! - op 0, 1: the payload as a UDP datagram, or as a chunk of the peer's TCP stream
//! - op 2: the peer's TCP connection closes
//! - op 3: every session is reset (a passcode change)
//! - op 4: an update message is prepared for every subscriber of each doc
//! - op 5: the second doc closes, or (when closed) opens again as a new doc
//!
//! `delay` moves the clock on by `delay` x 250 ms (255: eleven minutes, past the UDP idle time).
//!
//! Invariants: every reply and update encodes, and survives decode; the tables stay within their
//! bounds; a closed or reset peer is not connected; a closed doc has no session (checked on the
//! calls after it closed, when a later message could have brought one back); every input is for
//! a doc that is open; and, doc by doc, input that never contains that doc's passcode gets
//! nothing through to it: no engine input, no `ok` reply naming it, no connected peer, no
//! subscriber that may view. The passcode of the other doc, or a leak from one doc's session to
//! the other, is therefore caught.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use openlab_core::protocol::DocId;
use openlab_core::{CueId, CueListId, OscAccess, OscPasscode, OscRights, WorkspaceId};
use openlab_engine::osc::{
    CueField, DocShow, MAX_KNOWN_IPS, MAX_PEERS, Osc, OscShow, Outcome, Peer, SlipDecoder,
    decode_packet, encode_message, upper,
};
use rosc::OscMessage;
use serde_json::{Value, json};
use uuid::Uuid;

pub const SEPARATOR: u8 = 0x1e;
const FIRST_PASSCODE: &str = "1234";
const SECOND_PASSCODE: &str = "5678";
const MAX_RECORDS: usize = 256;
const PEERS: u8 = 6;

struct Show {
    id: u128,
    access: OscAccess,
}

impl OscShow for Show {
    fn workspace_id(&self) -> WorkspaceId {
        WorkspaceId(Uuid::from_u128(self.id))
    }
    fn access(&self) -> &OscAccess {
        &self.access
    }
    fn active_list(&self) -> Option<CueListId> {
        Some(CueListId(Uuid::from_u128(2)))
    }
    fn cue_by_number(&self, number: &str) -> Option<CueId> {
        matches!(number, "1" | "2.5").then_some(CueId(Uuid::from_u128(3)))
    }
    fn cue_exists(&self, id: CueId) -> bool {
        id == CueId(Uuid::from_u128(3))
    }
    fn cue_property(&self, _id: CueId, _field: CueField) -> Option<Value> {
        Some(json!("x"))
    }
    fn cue_by_osc_trigger(&self, address: &str) -> Option<CueId> {
        (address == "/trigger").then_some(CueId(Uuid::from_u128(3)))
    }
}

fn udp(n: u8) -> Peer {
    Peer::Udp(IpAddr::V4(Ipv4Addr::new(10, 0, 0, n % PEERS)))
}

fn tcp(n: u8) -> Peer {
    let n = n % PEERS;
    Peer::Tcp {
        conn: u64::from(n) + 1,
        ip: IpAddr::V4(Ipv4Addr::new(10, 0, 1, n)),
    }
}

fn check_replies(outcome: &Outcome) -> Result<(), String> {
    for reply in &outcome.replies {
        let bytes = encode_message(reply).map_err(|e| e.to_string())?;
        let back = decode_packet(&bytes).map_err(|e| e.to_string())?;
        if back.as_slice() != std::slice::from_ref(reply) {
            return Err("a reply did not survive encode and decode".into());
        }
    }
    Ok(())
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn access(locked: bool, passcode: &str) -> OscAccess {
    if locked {
        OscAccess {
            no_passcode: OscRights::NONE,
            passcodes: vec![OscPasscode {
                passcode: passcode.to_owned(),
                rights: OscRights::ALL,
            }],
        }
    } else {
        OscAccess::default()
    }
}

/// Whether `passcode`'s bytes could reach the dispatcher: inside one datagram, or inside what a
/// peer's TCP chunks add up to (a chunk may end mid-string, and the record separator is not part
/// of the stream). An over-approximation: it ignores disconnects, which only lose bytes.
fn passcode_sent(records: &[&[u8]], passcode: &str) -> bool {
    let mut streams: HashMap<u8, Vec<u8>> = HashMap::new();
    for record in records {
        let [op, peer, _delay, payload @ ..] = record else {
            continue;
        };
        match op % 6 {
            0 if contains(payload, passcode.as_bytes()) => return true,
            1 => streams
                .entry(peer % PEERS)
                .or_default()
                .extend_from_slice(payload),
            _ => {}
        }
    }
    streams
        .values()
        .any(|bytes| contains(bytes, passcode.as_bytes()))
}

/// Runs one input. `Err` is a broken invariant.
pub fn run(data: &[u8]) -> Result<(), String> {
    let Some((&config, rest)) = data.split_first() else {
        return Ok(());
    };
    let first = Show {
        id: 1,
        access: access(config & 1 != 0, FIRST_PASSCODE),
    };
    let second = Show {
        id: 2,
        access: access(config & 2 != 0, SECOND_PASSCODE),
    };
    let records: Vec<&[u8]> = rest.split(|&b| b == SEPARATOR).take(MAX_RECORDS).collect();
    // Without a doc's passcode bytes anywhere in the input, nobody can have rights in it. The
    // first doc is doc 0; the second is whichever id it has when open.
    let first_never = !passcode_sent(&records, FIRST_PASSCODE);
    let second_never = !passcode_sent(&records, SECOND_PASSCODE);
    // Every id the second doc has closed under: nothing may be left of it, checked after every
    // later record.
    let mut closed_docs: Vec<DocId> = Vec::new();
    let mut osc = Osc::new();
    let mut decoders: HashMap<u8, SlipDecoder> = HashMap::new();
    let mut now = Duration::ZERO;
    // The second doc's id while it is open; it opens again under a new one.
    let mut second_doc: Option<DocId> = Some(DocId(1));
    let mut next_doc = 2u64;
    for record in records {
        let (&op, tail) = match record.split_first() {
            Some(x) => x,
            None => continue,
        };
        let (&peer, tail) = match tail.split_first() {
            Some(x) => x,
            None => continue,
        };
        let (&delay, payload) = match tail.split_first() {
            Some(x) => x,
            None => continue,
        };
        now += if delay == u8::MAX {
            Duration::from_secs(11 * 60)
        } else {
            Duration::from_millis(u64::from(delay) * 250)
        };
        let mut docs = vec![DocShow {
            doc: DocId(0),
            show: &first,
        }];
        if let Some(doc) = second_doc {
            docs.push(DocShow { doc, show: &second });
        }
        let open = |doc: DocId| docs.iter().any(|d| d.doc == doc);
        // A locked doc nobody sent the passcode of: no peer may have any right in it.
        let sealed = |d: &DocShow<'_>| {
            d.show.access().requires_passcode()
                && if d.show.workspace_id() == first.workspace_id() {
                    first_never
                } else {
                    second_never
                }
        };
        let sealed_doc = |doc: DocId| docs.iter().any(|d| d.doc == doc && sealed(d));
        let check_inputs = |outcome: &Outcome| -> Result<(), String> {
            if outcome.input_docs.len() != outcome.inputs.len() {
                return Err("an input without its doc".into());
            }
            if outcome.input_docs.iter().any(|d| !open(*d)) {
                return Err("an input for a doc that is not open".into());
            }
            if outcome.input_docs.iter().any(|d| sealed_doc(*d)) {
                return Err("input got through to a doc without its passcode".into());
            }
            // An `ok` reply that names a doc is a right used in it.
            for reply in &outcome.replies {
                let [rosc::OscType::String(text)] = reply.args.as_slice() else {
                    continue;
                };
                let Ok(body) = serde_json::from_str::<Value>(text) else {
                    continue;
                };
                // A wrong passcode is an `ok` reply that says `badpass` (QLab's), and a `/connect` with none
                // on a locked doc one that grants no rights (`ok:`): neither uses a right.
                let field = |name: &str| body.get(name).and_then(Value::as_str);
                if field("status") == Some("ok")
                    && !matches!(field("data"), Some("badpass" | "ok:"))
                    && docs.iter().any(|d| {
                        sealed(d)
                            && field("workspace_id") == Some(upper(d.show.workspace_id()).as_str())
                    })
                {
                    return Err(format!(
                        "an ok reply for a doc without its passcode: {text}"
                    ));
                }
            }
            Ok(())
        };
        match op % 6 {
            0 => {
                let outcome = osc.handle_datagram_docs(udp(peer), now, now, payload, &docs);
                check_replies(&outcome)?;
                check_inputs(&outcome)?;
            }
            1 => {
                let decoder = decoders
                    .entry(peer % PEERS)
                    .or_insert_with(|| SlipDecoder::new(4096));
                let outcome = osc.handle_stream_docs(tcp(peer), now, now, decoder, payload, &docs);
                check_replies(&outcome)?;
                check_inputs(&outcome)?;
            }
            2 => {
                osc.disconnect(tcp(peer));
                decoders.remove(&(peer % PEERS));
                if osc.is_connected(tcp(peer)) {
                    return Err("a closed connection is still connected".into());
                }
            }
            3 => {
                osc.reset_sessions();
                if !osc.update_subscribers(now).is_empty()
                    || docs.iter().any(|d| osc.known_ips_in(d.doc) != 0)
                {
                    return Err("a reset left sessions behind".into());
                }
                if (0..PEERS).any(|n| osc.is_connected(udp(n)) || osc.is_connected(tcp(n))) {
                    return Err("a reset left a peer connected".into());
                }
            }
            4 => {
                let update = OscMessage {
                    addr: "/update/workspace/x".into(),
                    args: vec![],
                };
                for d in &docs {
                    for subscriber in osc.update_subscribers_in(d.doc, now) {
                        if let Some(bytes) = osc.encode_update(&update)
                            && osc.update_allowed(
                                d.doc,
                                subscriber,
                                now,
                                bytes.len(),
                                d.show.access().requires_passcode(),
                            )
                            && decode_packet(&bytes).is_err()
                        {
                            return Err("an update did not decode".into());
                        }
                    }
                }
            }
            _ => match second_doc.take() {
                Some(doc) => {
                    osc.close_doc(doc);
                    closed_docs.push(doc);
                }
                None => {
                    second_doc = Some(DocId(next_doc));
                    next_doc += 1;
                }
            },
        }
        // Checked here, after the records that followed the close, not right after
        // `close_doc` (which has just removed the entries): a later message must not bring a
        // closed doc's sessions back.
        for &doc in &closed_docs {
            if !osc.update_subscribers_in(doc, now).is_empty()
                || osc.known_ips_in(doc) != 0
                || (0..PEERS)
                    .any(|n| osc.is_connected_in(doc, udp(n)) || osc.is_connected_in(doc, tcp(n)))
            {
                return Err("a closed doc kept a session".into());
            }
        }
        for d in &docs {
            if osc.known_ips_in(d.doc) > MAX_KNOWN_IPS
                || osc.update_subscribers_in(d.doc, now).len() > MAX_PEERS
            {
                return Err("a table grew past its bound".into());
            }
            // `/updates` works at any access level, but a subscriber that may not view is sent
            // nothing.
            if sealed(d)
                && (osc
                    .update_subscribers_in(d.doc, now)
                    .into_iter()
                    .any(|p| osc.may_receive_updates(d.doc, p, d.show.access()))
                    || (0..PEERS).any(|n| {
                        osc.is_connected_in(d.doc, udp(n)) || osc.is_connected_in(d.doc, tcp(n))
                    }))
            {
                return Err("a peer got a session without the passcode".into());
            }
        }
    }
    Ok(())
}
