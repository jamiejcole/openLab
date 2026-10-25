// SPDX-License-Identifier: GPL-3.0-or-later

//! Show-file loader (QA-04): any input must load or fail cleanly, and whatever loads must save
//! and load back to the same workspace.

#![no_main]

use libfuzzer_sys::fuzz_target;
use openlab_core::{load_show, save_show};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(loaded) = load_show(text) {
        let saved = save_show(&loaded.workspace).expect("a loaded workspace saves");
        let reloaded = load_show(&saved).expect("a saved workspace loads");
        assert_eq!(reloaded.workspace, loaded.workspace);
    }
});
