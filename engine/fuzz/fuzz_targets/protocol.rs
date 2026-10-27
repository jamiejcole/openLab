// SPDX-License-Identifier: GPL-3.0-or-later

//! UI protocol input (QA-04): arbitrary bytes as client and server messages, and decoded client
//! messages run through the control loop on the offline driver. Never a panic or a hang; what
//! decodes encodes back to itself. The body is shared with the engine's `fuzz_protocol` test.

#![no_main]

use libfuzzer_sys::fuzz_target;

#[path = "../src/protocol_input.rs"]
mod protocol_input;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = protocol_input::run(data) {
        panic!("{e}");
    }
});
