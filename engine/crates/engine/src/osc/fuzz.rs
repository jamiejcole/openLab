// SPDX-License-Identifier: GPL-3.0-or-later

//! Fuzz entry for the OSC decoder and dispatcher (QA-04), and the fake show both OSC fuzz
//! targets dispatch to. Lives in the library so the fuzz crate can call it; nothing else uses it.

use std::net::{IpAddr, Ipv4Addr};
use std::ops::ControlFlow;
use std::time::Duration;

use openlab_core::{CueId, CueListId, OscAccess, OscPasscode, OscRights, WorkspaceId};
use serde_json::{Value, json};
use uuid::Uuid;

use openlab_core::protocol::DocId;

use super::{
    CueField, DocShow, Osc, OscShow, Outcome, Peer, SlipDecoder, Target, decode_packet,
    encode_message,
};

/// A one-list, one-cue show for the OSC fuzz targets: list 2 holds cue 3, numbered `1` and `2.5`
/// and triggered by `/trigger`. Workspace `1` is named "one", any other "two".
pub struct FuzzShow {
    pub id: u128,
    pub access: OscAccess,
}

impl OscShow for FuzzShow {
    fn workspace_id(&self) -> WorkspaceId {
        WorkspaceId(Uuid::from_u128(self.id))
    }
    fn workspace_name(&self) -> &str {
        if self.id == 1 { "one" } else { "two" }
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
    fn top_level_list(&self, cue: CueId) -> Option<CueListId> {
        self.cue_exists(cue)
            .then_some(CueListId(Uuid::from_u128(2)))
    }
    fn playhead(&self, _list: CueListId) -> Option<CueId> {
        Some(CueId(Uuid::from_u128(3)))
    }
    fn list_name(&self, _list: CueListId) -> Option<String> {
        Some("list".into())
    }
    fn running_cues(&self, _include_paused: bool) -> Vec<CueId> {
        vec![CueId(Uuid::from_u128(3))]
    }
    fn visit_targets(&self, visit: &mut dyn FnMut(Target, &str) -> ControlFlow<()>) {
        let cue = Target::Cue(CueId(Uuid::from_u128(3)));
        for (target, number) in [
            (Target::List(CueListId(Uuid::from_u128(2))), ""),
            (cue, "1"),
            (cue, "2.5"),
            (cue, ""),
            (cue, "a1b"),
        ] {
            if visit(target, number).is_break() {
                return;
            }
        }
    }
    fn cue_object(&self, id: CueId) -> Option<Value> {
        Some(json!({"uniqueID": super::upper(id)}))
    }
    fn cue_lists_object(&self) -> Value {
        json!([{"type": "Cue List"}])
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
        id: 1,
        // Bit 1: a table with passcodes (all rights; view only), No Passcode with no rights,
        // or view only when bit 2 is also set; else open.
        access: if mode & 2 != 0 {
            OscAccess {
                no_passcode: if mode & 4 != 0 {
                    OscRights {
                        view: true,
                        edit: false,
                        control: false,
                    }
                } else {
                    OscRights::NONE
                },
                passcodes: vec![
                    OscPasscode {
                        passcode: "1234".to_owned(),
                        rights: OscRights::ALL,
                    },
                    OscPasscode {
                        passcode: "5678".to_owned(),
                        rights: OscRights {
                            view: true,
                            edit: true,
                            control: false,
                        },
                    },
                ],
            }
        } else {
            OscAccess::default()
        },
    };
    // A second doc, open to everyone: every un-prefixed message goes to both (M4.15h).
    let other = FuzzShow {
        id: 9,
        access: OscAccess::default(),
    };
    let docs = [
        DocShow {
            doc: DocId(0),
            show: &show,
        },
        DocShow {
            doc: DocId(1),
            show: &other,
        },
    ];
    let mut osc = Osc::new();
    let udp = Peer::Udp(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let tcp = Peer::Tcp {
        conn: 1,
        ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
    };
    let mut now = Duration::ZERO;
    let mut outcomes: Vec<Outcome> = Vec::new();
    if mode & 1 == 0 {
        outcomes.push(osc.handle_datagram_docs(udp, now, now, rest, &docs));
    } else {
        let mut decoder = SlipDecoder::new(4096);
        let chunk = usize::from(mode >> 2) % 31 + 1;
        for part in rest.chunks(chunk) {
            now += Duration::from_millis(1);
            outcomes.push(osc.handle_stream_docs(tcp, now, now, &mut decoder, part, &docs));
        }
    }
    for outcome in &outcomes {
        if outcome.input_docs.len() != outcome.inputs.len()
            || outcome.replies.len() > super::MAX_REPLIES_PER_CALL
        {
            return Err("inputs without their doc, or too many replies".into());
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
