// SPDX-License-Identifier: GPL-3.0-or-later

//! The protocol fuzz body (QA-04): bytes in, as client and server messages, and as a script for
//! the control loop on the offline driver. Shared by the `protocol` fuzz target and by the
//! engine's `fuzz_protocol` test (which replays the seed corpus and mutations of it on stable),
//! so it uses public APIs only and no `unwrap`, `expect`, `panic!` or indexing.
//!
//! The input is one or more records separated by 0x1e. Each record is decoded as a client
//! message and as a server message; neither may panic, and what decodes must encode and decode
//! back to itself (client messages) or encode (server messages). Client messages that decode
//! are then run through the control loop on a small workspace, with a few blocks of virtual time
//! between them. Every reply and broadcast must survive the wire format, revisions never go
//! backwards, and nothing may hang.
//!
//! Documents (`new`, `open`, `save`, ...) are not sent: once the engine persists them they act on
//! files, and a fuzzer must not choose which. Media probes are a stand-in that finds no file for
//! the same reason (a fuzzed path must not open `/dev/zero`), so audio cues end up broken; the
//! cue engine, the edit path, undo, redo and the transport still run in full.

use std::path::Path;

use openlab_audio::{MediaError, MixerConfig, Probed};
use openlab_core::Workspace;
use openlab_core::protocol::{
    ClientMessage, Request, ServerMessage, decode_client, decode_server, encode_client,
    encode_server,
};
use openlab_engine::control::ControlConfig;
use openlab_engine::control::offline::OfflineShow;

/// The record separator.
pub const SEPARATOR: u8 = 0x1e;
/// Records run per input: a sequence long enough to undo what it did, short enough to be fast.
const MAX_RECORDS: usize = 16;
const RATE: u32 = 8000;
const BLOCK: u64 = 64;
/// The workspace the protocol fixtures talk about (cue list 1, cues 0a and 0b).
const SNAPSHOT: &str = include_str!("../../fixtures/protocol/server/snapshot.json");

fn no_media(path: &Path) -> Result<Probed, MediaError> {
    Err(MediaError::NotFound(path.to_path_buf()))
}

fn workspace() -> Result<Workspace, String> {
    match decode_server(SNAPSHOT).map_err(|e| e.to_string())? {
        ServerMessage::Snapshot { snapshot } => Ok(snapshot.workspace),
        other => Err(format!("the snapshot fixture decodes as {other:?}")),
    }
}

fn new_show() -> Result<OfflineShow, String> {
    let ws = workspace()?;
    let mut config = ControlConfig::new(RATE, BLOCK);
    config.seed = 7;
    config.probe = no_media;
    config.mixer_patches = ws.patches.len().max(1);
    let mixer = MixerConfig {
        sample_rate: RATE,
        output_channels: 2,
        max_block_frames: BLOCK as usize,
        max_voices: 16,
        max_voice_channels: 8,
        patches: config.mixer_patches,
        lookahead_ms: 500,
    };
    OfflineShow::new(ws, mixer, config).map_err(|e| e.to_string())
}

/// A message a fuzzer must not send to the control loop (see the module comment).
fn touches_files(m: &ClientMessage) -> bool {
    matches!(
        m,
        ClientMessage::Request {
            request: Request::New
                | Request::Open { .. }
                | Request::Save
                | Request::SaveAs { .. }
                | Request::Revert
                | Request::Relink { .. },
            ..
        }
    )
}

fn check_server(m: &ServerMessage) -> Result<(), String> {
    let text = encode_server(m).map_err(|e| format!("a server message does not encode: {e}"))?;
    match decode_server(&text) {
        Ok(back) if &back == m => Ok(()),
        other => Err(format!("a server message came back as {other:?}: {text}")),
    }
}

/// Runs one input. `Err` is a broken invariant; a panic or a hang is a bug the fuzzer finds by
/// itself.
pub fn run(data: &[u8]) -> Result<(), String> {
    let mut show: Option<OfflineShow> = None;
    let mut revision = 0u64;
    for record in data.split(|&b| b == SEPARATOR).take(MAX_RECORDS) {
        let text = String::from_utf8_lossy(record);
        // Servers' messages are decoded by clients (and by tests): never a panic.
        if let Ok(m) = decode_server(&text) {
            let _ = encode_server(&m);
        }
        let Ok(message) = decode_client(&text) else {
            continue;
        };
        let encoded = encode_client(&message).map_err(|e| format!("does not encode: {e}"))?;
        match decode_client(&encoded) {
            Ok(back) if back == message => {}
            other => return Err(format!("{message:?} came back as {other:?} via {encoded}")),
        }
        if touches_files(&message) {
            continue;
        }
        let s = match show.as_mut() {
            Some(s) => s,
            None => show.insert(new_show()?),
        };
        let before = s.control().revision();
        let shutdown = matches!(message, ClientMessage::Goodbye { shutdown: true });
        for reply in s.send(message) {
            check_server(&reply)?;
        }
        if shutdown {
            break;
        }
        s.settle_media();
        s.run(BLOCK * 4);
        for broadcast in s.take_outbound() {
            check_server(&broadcast)?;
        }
        let now = s.control().revision();
        if now < before || now < revision {
            return Err(format!("the revision went from {before} to {now}"));
        }
        revision = now;
        if s.stalls() != 0 {
            return Err("the offline driver stalled".into());
        }
    }
    Ok(())
}
