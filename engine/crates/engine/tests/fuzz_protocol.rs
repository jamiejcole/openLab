// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The protocol fuzz target's body, run on stable in the normal suite (QA-04): the seed corpus
//! (every protocol fixture, and the scripts in `fuzz/protocol-seeds`) must pass, and so must
//! mutations of it and plain noise. The libFuzzer target (`make fuzz`) goes much further; this
//! keeps a regression it found, and the body itself, from rotting between fuzz runs.

#[path = "../../../fuzz/src/protocol_input.rs"]
mod protocol_input;

use std::fs;
use std::path::{Path, PathBuf};

use proptest::prelude::*;

fn files(dir: &str) -> Vec<(PathBuf, Vec<u8>)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
    let mut out: Vec<_> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file())
        .map(|p| {
            let bytes = fs::read(&p).unwrap();
            (p, bytes)
        })
        .collect();
    out.sort();
    out
}

fn seeds() -> Vec<Vec<u8>> {
    [
        "../../fixtures/protocol/client",
        "../../fixtures/protocol/server",
        "../../fuzz/protocol-seeds",
    ]
    .into_iter()
    .flat_map(files)
    .map(|(_, bytes)| bytes)
    .collect()
}

#[test]
fn every_seed_passes() {
    let mut count = 0;
    for dir in [
        "../../fixtures/protocol/client",
        "../../fixtures/protocol/server",
        "../../fuzz/protocol-seeds",
    ] {
        let found = files(dir);
        assert!(!found.is_empty(), "{dir} has no seeds");
        for (path, bytes) in found {
            if let Err(e) = protocol_input::run(&bytes) {
                panic!("{}: {e}", path.display());
            }
            count += 1;
        }
    }
    assert!(count > 100, "only {count} seeds");
}

#[test]
fn all_client_fixtures_as_one_script_pass() {
    // Every client message in one session, in file order: the control loop gets all of them
    // (documents excepted) one after another, with its state carried along.
    let script = files("../../fixtures/protocol/client")
        .into_iter()
        .map(|(_, b)| b)
        .collect::<Vec<_>>()
        .join(&protocol_input::SEPARATOR);
    // More records than the body runs: it takes the first sixteen. Rotate to cover them all.
    let records: Vec<&[u8]> = script.split(|&b| b == protocol_input::SEPARATOR).collect();
    for start in 0..records.len() {
        let rotated = records
            .iter()
            .cycle()
            .skip(start)
            .take(16)
            .copied()
            .collect::<Vec<_>>()
            .join(&protocol_input::SEPARATOR);
        protocol_input::run(&rotated).unwrap();
    }
}

#[test]
fn degenerate_inputs_pass() {
    for input in [
        &b""[..],
        &[protocol_input::SEPARATOR; 40],
        b"{}",
        b"null",
        b"[",
        &[0xff, 0xfe, 0x00],
        b"{\"type\":\"transport\",\"id\":1,\"action\":{\"type\":\"go\",\"list\":\"nope\"}}",
        b"{\"type\":\"goodbye\",\"shutdown\":true}",
        // Deep nesting must be refused, not overflow the stack.
        &[b'['; 100_000],
    ] {
        protocol_input::run(input).unwrap();
    }
}

fn mutated() -> impl Strategy<Value = Vec<u8>> {
    let pool = seeds();
    let n = pool.len();
    (
        proptest::collection::vec(0..n, 1..4),
        proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 0..8),
        proptest::option::of(any::<prop::sample::Index>()),
    )
        .prop_map(move |(picks, edits, cut)| {
            let mut data: Vec<u8> = picks
                .iter()
                .filter_map(|&i| pool.get(i))
                .cloned()
                .collect::<Vec<_>>()
                .join(&protocol_input::SEPARATOR);
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
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn mutated_seeds_pass(data in mutated()) {
        if let Err(e) = protocol_input::run(&data) {
            return Err(TestCaseError::fail(e));
        }
    }

    #[test]
    fn noise_passes(data in proptest::collection::vec(any::<u8>(), 0..512)) {
        if let Err(e) = protocol_input::run(&data) {
            return Err(TestCaseError::fail(e));
        }
    }
}
