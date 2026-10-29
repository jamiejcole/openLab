// SPDX-License-Identifier: GPL-3.0-or-later

//! One test binary for the diag tests (one link step instead of 2); each file is a module.

#[path = "../common/mod.rs"]
mod common;
#[path = "../show/mod.rs"]
mod show;

mod log;
mod status;
