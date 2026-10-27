// SPDX-License-Identifier: GPL-3.0-or-later

//! openlab-engine library: the engine's modules, so integration tests can reach them. The
//! binary in `main.rs` wires them together.

pub mod control;
pub mod host;
pub mod osc;
pub mod persist;
