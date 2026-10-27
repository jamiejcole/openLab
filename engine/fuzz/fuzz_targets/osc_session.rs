// SPDX-License-Identifier: GPL-3.0-or-later

//! OSC sessions (NET-02, NET-05, QA-04): a script of datagrams and stream chunks from several
//! peers over virtual time, with closes, resets and update fan-out between them. Tables stay
//! bounded, replies stay valid, and nothing gets past a passcode that was never sent. The body
//! is shared with the engine's `fuzz_osc_session` test.

#![no_main]

use libfuzzer_sys::fuzz_target;

#[path = "../src/osc_session_input.rs"]
mod osc_session_input;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = osc_session_input::run(data) {
        panic!("{e}");
    }
});
