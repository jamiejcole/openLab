// SPDX-License-Identifier: GPL-3.0-or-later

//! The OSC session fuzz body (QA-04): the dispatcher fed a script of datagrams and stream chunks
//! from several peers over virtual time, with disconnects, session resets and update fan-out in
//! between. The `osc` target feeds one datagram or one stream to a fresh endpoint; this one
//! reaches what only a sequence reaches: lockouts, the failure windows, idle expiry, the peer
//! table, the reply byte budget. Shared by the `osc_session` fuzz target and the engine's
//! `fuzz_osc_session` test; public APIs only, no `unwrap`, `expect`, `panic!` or indexing.
//!
//! Input: byte 0 picks whether the workspace has the passcode `1234`. The rest is records
//! separated by 0x1e; a record is `[op][peer][delay][payload...]`:
//!
//! - op 0, 1: the payload as a UDP datagram, or as a chunk of the peer's TCP stream
//! - op 2: the peer's TCP connection closes
//! - op 3: every session is reset (a passcode change)
//! - op 4: an update message is prepared for every subscriber
//!
//! `delay` moves the clock on by `delay` x 250 ms (255: eleven minutes, past the UDP idle time).
//!
//! Invariants: every reply and update encodes, and survives decode; the tables stay within their
//! bounds; a closed or reset peer is not connected; and with a passcode set, input that never
//! contains the passcode's bytes gets nothing through: no engine input, no connected peer, no
//! subscriber.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use openlab_core::{CueId, CueListId, WorkspaceId};
use openlab_engine::osc::{
    CueField, MAX_KNOWN_IPS, MAX_PEERS, Osc, OscShow, Outcome, Peer, SlipDecoder, decode_packet,
    encode_message,
};
use rosc::OscMessage;
use serde_json::{Value, json};
use uuid::Uuid;

pub const SEPARATOR: u8 = 0x1e;
const PASSCODE: &str = "1234";
const MAX_RECORDS: usize = 256;
const PEERS: u8 = 6;

struct Show {
    passcode: Option<&'static str>,
}

impl OscShow for Show {
    fn workspace_id(&self) -> WorkspaceId {
        WorkspaceId(Uuid::from_u128(1))
    }
    fn passcode(&self) -> Option<&str> {
        self.passcode
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

/// Runs one input. `Err` is a broken invariant.
pub fn run(data: &[u8]) -> Result<(), String> {
    let Some((&config, rest)) = data.split_first() else {
        return Ok(());
    };
    let show = Show {
        passcode: (config & 1 != 0).then_some(PASSCODE),
    };
    // Without the passcode's bytes anywhere in the input, nobody can have connected.
    let locked_out_for_good = show.passcode.is_some() && !contains(rest, PASSCODE.as_bytes());
    let mut osc = Osc::new();
    let mut decoders: HashMap<u8, SlipDecoder> = HashMap::new();
    let mut now = Duration::ZERO;
    for record in rest.split(|&b| b == SEPARATOR).take(MAX_RECORDS) {
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
        match op % 5 {
            0 => {
                let outcome = osc.handle_datagram(udp(peer), now, payload, &show);
                check_replies(&outcome)?;
                if locked_out_for_good && !outcome.inputs.is_empty() {
                    return Err("input got through without the passcode".into());
                }
            }
            1 => {
                let decoder = decoders
                    .entry(peer % PEERS)
                    .or_insert_with(|| SlipDecoder::new(4096));
                let outcome = osc.handle_stream(tcp(peer), now, decoder, payload, &show);
                check_replies(&outcome)?;
                if locked_out_for_good && !outcome.inputs.is_empty() {
                    return Err("input got through without the passcode".into());
                }
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
                if !osc.update_subscribers().is_empty() || osc.known_ips() != 0 {
                    return Err("a reset left sessions behind".into());
                }
                if (0..PEERS).any(|n| osc.is_connected(udp(n)) || osc.is_connected(tcp(n))) {
                    return Err("a reset left a peer connected".into());
                }
            }
            _ => {
                let update = OscMessage {
                    addr: "/update/workspace/x".into(),
                    args: vec![],
                };
                for subscriber in osc.update_subscribers() {
                    if let Some(bytes) = osc.prepare_update(subscriber, now, &update)
                        && decode_packet(&bytes).is_err()
                    {
                        return Err("an update did not decode".into());
                    }
                }
            }
        }
        if osc.known_ips() > MAX_KNOWN_IPS || osc.update_subscribers().len() > MAX_PEERS {
            return Err("a table grew past its bound".into());
        }
        if locked_out_for_good
            && (!osc.update_subscribers().is_empty()
                || (0..PEERS).any(|n| osc.is_connected(udp(n)) || osc.is_connected(tcp(n))))
        {
            return Err("a peer got a session without the passcode".into());
        }
    }
    Ok(())
}
