// SPDX-License-Identifier: GPL-3.0-or-later

//! Fuzz entry for the OSC decoder and dispatcher (QA-04). Lives in the library so the fuzz crate
//! can call it; nothing else uses it.

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use openlab_core::{CueId, CueListId, WorkspaceId};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{CueField, Osc, OscShow, Outcome, Peer, SlipDecoder, decode_packet, encode_message};

struct FuzzShow {
    passcode: Option<&'static str>,
}

impl OscShow for FuzzShow {
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

/// Feeds `data` to the endpoint as datagrams or as a TCP stream (first byte picks, and whether a
/// passcode is set), and checks that every reply is a valid OSC message that survives a round
/// trip. Returns an error only for a broken invariant; any input must otherwise just not panic.
pub fn fuzz_osc(data: &[u8]) -> Result<(), String> {
    let Some((&mode, rest)) = data.split_first() else {
        return Ok(());
    };
    let show = FuzzShow {
        passcode: (mode & 2 != 0).then_some("1234"),
    };
    let mut osc = Osc::new();
    let udp = Peer::Udp(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let tcp = Peer::Tcp {
        conn: 1,
        ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
    };
    let mut now = Duration::ZERO;
    let mut outcomes: Vec<Outcome> = Vec::new();
    if mode & 1 == 0 {
        outcomes.push(osc.handle_datagram(udp, now, rest, &show));
    } else {
        let mut decoder = SlipDecoder::new(4096);
        let chunk = usize::from(mode >> 2) % 31 + 1;
        for part in rest.chunks(chunk) {
            now += Duration::from_millis(1);
            outcomes.push(osc.handle_stream(tcp, now, &mut decoder, part, &show));
        }
    }
    for reply in outcomes.iter().flat_map(|o| &o.replies) {
        let bytes = encode_message(reply).map_err(|e| e.to_string())?;
        let back = decode_packet(&bytes).map_err(|e| e.to_string())?;
        if back.as_slice() != std::slice::from_ref(reply) {
            return Err("a reply did not survive encode and decode".into());
        }
    }
    Ok(())
}
