// SPDX-License-Identifier: GPL-3.0-or-later

//! One test binary for the net tests (one link step instead of 4); each file is a module.

#[path = "../common/mod.rs"]
mod common;
#[path = "../show/mod.rs"]
mod show;

mod docs;
mod osc;
mod session;
mod ws;
