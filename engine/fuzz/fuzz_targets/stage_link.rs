// SPDX-License-Identifier: GPL-3.0-or-later

//! The engine-stage host link (both directions): arbitrary bytes read and decoded as either end
//! reads them. Never a panic, never a line held past the limit. The body is shared with
//! openlab-core's `fuzz_stage_link` test.

#![no_main]

use libfuzzer_sys::fuzz_target;

#[path = "../src/stage_link_input.rs"]
mod stage_link_input;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = stage_link_input::run(data) {
        panic!("{e}");
    }
});
