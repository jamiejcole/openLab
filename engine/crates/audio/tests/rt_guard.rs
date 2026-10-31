// SPDX-License-Identifier: GPL-3.0-or-later

//! The allocation guard itself (NFR-02): it must catch an allocation. This test allocates on purpose,
//! which raises `assert_no_alloc`'s process-wide violation counter; every test that asserts "no
//! violations" compares that counter before and after, so it would fail if it ran in the same process.
//! Keep this binary to deliberate violations only: no other test here, and no deliberate violation in
//! a binary whose tests use `common::no_alloc`.

use assert_no_alloc::{AllocDisabler, assert_no_alloc, violation_count};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

#[test]
fn harness_detects_allocation() {
    let before = violation_count();
    let v = assert_no_alloc(|| std::hint::black_box(vec![1u8; 16]));
    assert!(violation_count() > before);
    drop(v);
}
