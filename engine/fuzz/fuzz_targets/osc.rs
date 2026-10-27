// SPDX-License-Identifier: GPL-3.0-or-later

//! OSC input (NET-01, QA-04): any bytes, as datagrams or as a SLIP stream, must decode and
//! dispatch or be dropped and counted, never panic, and every reply must be valid OSC.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = openlab_engine::osc::fuzz::fuzz_osc(data) {
        panic!("{e}");
    }
});
