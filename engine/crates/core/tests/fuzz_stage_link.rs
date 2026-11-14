// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The stage link fuzz target's body, run on stable in the normal suite: the seed sessions in
//! `fuzz/stage-link-seeds`, mutations of them, noise, and lines past the limit must pass. The
//! libFuzzer target (`make fuzz`) goes much further.

#[path = "../../../fuzz/src/stage_link_input.rs"]
mod stage_link_input;

use std::fs;
use std::path::Path;

use openlab_core::stage_link::MAX_LINE_BYTES;
use proptest::prelude::*;

fn seeds() -> Vec<Vec<u8>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/stage-link-seeds");
    let mut paths: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    paths.sort();
    assert!(paths.len() >= 3, "seeds missing in {}", dir.display());
    paths.iter().map(|p| fs::read(p).unwrap()).collect()
}

#[test]
fn every_seed_passes() {
    for seed in seeds() {
        stage_link_input::run(&seed).unwrap();
    }
}

#[test]
fn degenerate_inputs_pass() {
    let long = vec![b'{'; MAX_LINE_BYTES + 1];
    let mut long_then_good = long.clone();
    long_then_good.extend_from_slice(b"\n{\"type\":\"pong\",\"seq\":1}\n");
    for input in [
        &b""[..],
        b"\n\n\n",
        &[0xff, 0xfe, 0x00],
        &[b'['; 60_000],
        &long,
        &long_then_good,
    ] {
        stage_link_input::run(input).unwrap();
    }
}

fn mutated() -> impl Strategy<Value = Vec<u8>> {
    let pool = seeds();
    let n = pool.len();
    (
        0..n,
        proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 0..8),
    )
        .prop_map(move |(pick, edits)| {
            let mut data = pool.get(pick).cloned().unwrap_or_default();
            for (at, byte) in edits {
                if !data.is_empty() {
                    let i = at.index(data.len());
                    if let Some(slot) = data.get_mut(i) {
                        *slot = byte;
                    }
                }
            }
            data
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn mutated_seeds_pass(data in mutated()) {
        if let Err(e) = stage_link_input::run(&data) {
            return Err(TestCaseError::fail(e));
        }
    }

    #[test]
    fn noise_passes(data in proptest::collection::vec(any::<u8>(), 0..512)) {
        if let Err(e) = stage_link_input::run(&data) {
            return Err(TestCaseError::fail(e));
        }
    }
}
