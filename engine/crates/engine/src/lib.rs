// SPDX-License-Identifier: GPL-3.0-or-later

//! openlab-engine library: the engine's modules, so integration tests can reach them. The
//! binary in `main.rs` wires them together.

pub mod control;
pub mod diag;
pub mod host;
pub mod net;
pub mod osc;
pub mod persist;
pub mod platform;

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks `m`, taking the data even if a thread panicked while holding it: a panic on one thread
/// (already logged by the panic hook) must not stop saving, logging or the network for the rest
/// of the show. Every lock in the engine goes through here.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The hang guard for tests that wait on a thread of their own, shared with the integration tests.
#[cfg(test)]
#[path = "../../audio/tests/common/spin.rs"]
pub(crate) mod spin;

#[cfg(test)]
mod tests {
    #[test]
    fn a_lock_poisoned_by_a_panic_still_hands_out_its_data() {
        let m = std::sync::Arc::new(std::sync::Mutex::new(1));
        let held = std::sync::Arc::clone(&m);
        let _ = std::thread::spawn(move || {
            let _guard = held.lock();
            panic!("poison it");
        })
        .join();
        assert!(m.is_poisoned());
        *super::lock(&m) += 1;
        assert_eq!(*super::lock(&m), 2);
    }
}
