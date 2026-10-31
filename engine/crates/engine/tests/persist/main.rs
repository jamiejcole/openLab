// SPDX-License-Identifier: GPL-3.0-or-later

//! One test binary for the persist tests (one link step instead of 5); each file is a module.

#[path = "../common/mod.rs"]
mod common;
#[path = "../common/spin.rs"]
mod spin;

mod recovery_scan;
mod recovery_settings;
mod save;
mod save_robust;
mod writer;
