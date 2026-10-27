// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The OSC session fuzz target's body, run on stable in the normal suite (QA-04): the seed
//! scripts in `fuzz/osc-session-seeds`, mutations of them, and noise. See `fuzz_protocol.rs`.

#[path = "../../../fuzz/src/osc_session_input.rs"]
mod osc_session_input;

use std::fs;
use std::path::Path;

use proptest::prelude::*;

fn seeds() -> Vec<Vec<u8>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/osc-session-seeds");
    let mut paths: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    paths.sort();
    assert!(paths.len() >= 4, "seeds missing in {}", dir.display());
    paths.iter().map(|p| fs::read(p).unwrap()).collect()
}

#[test]
fn every_seed_passes() {
    for seed in seeds() {
        osc_session_input::run(&seed).unwrap();
    }
}

#[test]
fn degenerate_inputs_pass() {
    for input in [
        &b""[..],
        &[1],
        &[1, 0],
        &[1, 0, 0, 0],
        &[0x1e; 300],
        &[1, 1, 1, 255, 0x1e, 0, 0, 0, 0xc0, 0xdb],
    ] {
        osc_session_input::run(input).unwrap();
    }
}

#[test]
fn a_passcode_in_the_input_does_not_trip_the_check() {
    // The seeds with the passcode connect and act: the "nothing gets through without it" check
    // only applies to inputs that never contain it, and must not fire on the ones that do.
    let seed = seeds()
        .into_iter()
        .find(|s| s.windows(4).any(|w| w == b"1234"))
        .unwrap();
    osc_session_input::run(&seed).unwrap();
}

fn mutated() -> impl Strategy<Value = Vec<u8>> {
    let pool = seeds();
    let n = pool.len();
    (
        0..n,
        proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 0..12),
        proptest::option::of(any::<prop::sample::Index>()),
    )
        .prop_map(move |(pick, edits, cut)| {
            let mut data = pool.get(pick).cloned().unwrap_or_default();
            for (at, byte) in edits {
                if !data.is_empty() {
                    let i = at.index(data.len());
                    if let Some(slot) = data.get_mut(i) {
                        *slot = byte;
                    }
                }
            }
            if let Some(cut) = cut
                && !data.is_empty()
            {
                data.truncate(cut.index(data.len()));
            }
            data
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn mutated_seeds_pass(data in mutated()) {
        if let Err(e) = osc_session_input::run(&data) {
            return Err(TestCaseError::fail(e));
        }
    }

    #[test]
    fn noise_passes(data in proptest::collection::vec(any::<u8>(), 0..1024)) {
        if let Err(e) = osc_session_input::run(&data) {
            return Err(TestCaseError::fail(e));
        }
    }
}
