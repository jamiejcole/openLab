// SPDX-License-Identifier: GPL-3.0-or-later

//! QLab workspace reader: any input is converted or refused with a typed error, never a panic
//! or a hang, and whatever converts saves and loads back as an openLab show.

#![no_main]

use libfuzzer_sys::fuzz_target;
use openlab_core::{CueListId, MediaRef, QlabFile, load_show, read_qlab, save_show};

fuzz_target!(|data: &[u8]| {
    let mut n = 0u128;
    let mut ids = || {
        n += 1;
        uuid::Uuid::from_u128(n)
    };
    let mut media = |f: &QlabFile| f.relative.clone().map(MediaRef::new);
    if let Ok(import) = read_qlab(data, &mut ids, &mut media) {
        let list = CueListId(uuid::Uuid::from_u128(u128::MAX));
        let (workspace, _) = import.finish(&[], openlab_core::QlabCueAction::Placeholder, list);
        let saved = save_show(&workspace).expect("a converted workspace saves");
        load_show(&saved).expect("a converted workspace loads");
    }
});
