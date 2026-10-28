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
//! Document requests run too (M4.10a), against a fresh folder per input: the application folder
//! (autosaves, recovery) is inside it, and every path `open`, `save_as` and `relink` name is
//! moved into it (its file name kept, sanitised), so a fuzzer never chooses which file is read
//! or written. Media probes are a stand-in that finds no file for the same reason (a fuzzed path
//! must not open `/dev/zero`), so audio cues end up broken; the cue engine, the edit path, undo,
//! redo, the transport and the workspaces opened and closed still run in full. Messages go to
//! the first doc, an app connection or the newest doc in turn (by request id), as a bound
//! connection, the OSC side and the app's would send them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use openlab_audio::{MediaError, MixerConfig, Probed};
use openlab_core::Workspace;
use openlab_core::protocol::{
    ClientMessage, DocId, Request, ServerMessage, decode_client, decode_server, encode_client,
    encode_server,
};
use openlab_engine::control::ControlConfig;
use openlab_engine::control::offline::OfflineShow;
use openlab_engine::persist::AppDirs;

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

/// A folder of its own for one input, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self, String> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("openlab-fuzz-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(Self(dir))
    }

    /// `path`'s file name, inside this folder: letters, digits, `.`, `-` and `_` only, at most
    /// 40 of them, never `.` or `..` (an empty one is `show.openlab`).
    fn file(&self, path: &str) -> PathBuf {
        let name: String = path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            .take(40)
            .collect();
        if name.is_empty() || name.chars().all(|c| c == '.') {
            self.0.join("show.openlab")
        } else {
            self.0.join(name)
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `message` with its file paths moved into `scratch`.
fn confine(message: ClientMessage, scratch: &Scratch) -> ClientMessage {
    let path = |p: &str| scratch.file(p).to_string_lossy().into_owned();
    match message {
        ClientMessage::Request { id, request } => {
            let request = match request {
                Request::Open { path: p, recovery } => Request::Open {
                    path: path(&p),
                    recovery,
                },
                Request::SaveAs { path: p } => Request::SaveAs { path: path(&p) },
                Request::Relink { .. } => Request::Relink {
                    folder: scratch.0.to_string_lossy().into_owned(),
                },
                other => other,
            };
            ClientMessage::Request { id, request }
        }
        other => other,
    }
}

fn new_show(scratch: &Scratch) -> Result<OfflineShow, String> {
    let ws = workspace()?;
    let mut config = ControlConfig::new(RATE, BLOCK);
    config.app_dirs = Some(AppDirs::new(scratch.0.join("support")));
    config.seed = 7;
    // No watch thread per fuzz case.
    config.watch.mode = openlab_engine::control::watch::WatchMode::Manual;
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

/// The request id of `m`, which picks where it is sent (see the module comment).
fn id_of(m: &ClientMessage) -> Option<u64> {
    match m {
        ClientMessage::Command { id, .. }
        | ClientMessage::Transport { id, .. }
        | ClientMessage::Undo { id }
        | ClientMessage::Redo { id }
        | ClientMessage::Request { id, .. }
        | ClientMessage::Close { id, .. }
        | ClientMessage::Panic { id }
        | ClientMessage::Hotkey { id, .. } => Some(id.0),
        ClientMessage::Hello { .. } | ClientMessage::Goodbye { .. } => None,
    }
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
    let mut show: Option<(OfflineShow, Scratch)> = None;
    let mut revisions: HashMap<DocId, u64> = HashMap::new();
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
        let (s, scratch) = match show.as_mut() {
            Some(s) => s,
            None => {
                let scratch = Scratch::new()?;
                show.insert((new_show(&scratch)?, scratch))
            }
        };
        let message = confine(message, scratch);
        let shutdown = matches!(message, ClientMessage::Goodbye { shutdown: true });
        let newest = s.control().doc_ids().last().copied();
        let replies = match (id_of(&message).map(|id| id % 3), newest) {
            (Some(1), _) => s.send_app(message),
            (Some(2), Some(doc)) => s.send_doc(doc, message),
            _ => s.send(message),
        };
        for reply in replies {
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
        // Each doc's revisions only rise (a doc's numbering is its own).
        for doc in s.control().doc_ids() {
            let now = s.control().doc(doc).map_or(0, |d| d.revision());
            let before = revisions.insert(doc, now).unwrap_or(0);
            if now < before {
                return Err(format!("doc {doc}'s revision went from {before} to {now}"));
            }
        }
        if s.stalls() != 0 {
            return Err("the offline driver stalled".into());
        }
    }
    Ok(())
}
