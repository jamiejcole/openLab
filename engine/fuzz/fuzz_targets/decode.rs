// SPDX-License-Identifier: GPL-3.0-or-later

//! Media decoding (AUD-04, QA-04): any bytes must probe, preload, stream and seek or fail with a
//! typed error, and whatever probes must play exactly as probed.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = openlab_audio::fuzz_decode(data) {
        panic!("{e}");
    }
});
