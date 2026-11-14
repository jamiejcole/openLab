// SPDX-License-Identifier: GPL-3.0-or-later

//! The stage link fuzz body: arbitrary bytes as what either end of the engine-stage host pipe
//! may read, and what a probe process may print. Every line is read with the bounded reader and
//! decoded every way; never a panic, never a line held past the limit, and what decodes encodes
//! back to itself. Shared with openlab-core's `fuzz_stage_link` test.

use openlab_core::stage_link::{
    Line, MAX_LINE_BYTES, decode_from_stage, decode_to_stage, encode, read_line,
};
use openlab_core::video_probe::decode_probe_report;

pub fn run(data: &[u8]) -> Result<(), String> {
    let mut input = data;
    let mut line = Vec::new();
    // Each round consumes at least one byte or ends, so this is bounded by the input's length.
    for _ in 0..=data.len() {
        line.clear();
        match read_line(&mut input, &mut line).map_err(|e| e.to_string())? {
            Line::End => return Ok(()),
            Line::TooLong(bytes) => {
                if bytes <= MAX_LINE_BYTES {
                    return Err(format!("{bytes} bytes reported too long"));
                }
            }
            Line::Read => {
                if line.len() > MAX_LINE_BYTES {
                    return Err(format!("held a line of {} bytes", line.len()));
                }
                if let Ok(message) = decode_from_stage(&line) {
                    let again = encode(&message).map_err(|e| e.to_string())?;
                    if decode_from_stage(again.as_bytes()).as_ref() != Ok(&message) {
                        return Err(format!("{message:?} did not survive a round trip"));
                    }
                }
                if let Ok(Ok(report)) = std::str::from_utf8(&line).map(decode_probe_report) {
                    let again = report.to_line().map_err(|e| e.to_string())?;
                    if decode_probe_report(&again).as_ref() != Ok(&report) {
                        return Err(format!("{report:?} did not survive a round trip"));
                    }
                }
                if let Ok(message) = decode_to_stage(&line) {
                    let again = encode(&message).map_err(|e| e.to_string())?;
                    if decode_to_stage(again.as_bytes()).as_ref() != Ok(&message) {
                        return Err(format!("{message:?} did not survive a round trip"));
                    }
                }
            }
        }
    }
    Err("the reader did not reach the end".into())
}
