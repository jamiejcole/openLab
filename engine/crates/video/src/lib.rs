// SPDX-License-Identifier: GPL-3.0-or-later

//! The stage host (`openlab-stage`): the process the engine spawns and supervises that owns the
//! video output windows, so the engine links no GUI framework and picture survives an app
//! crash. It speaks the stage link (`openlab_core::stage_link`) on stdin and stdout and exits
//! when its input ends, so it never outlives the engine.

#[cfg(feature = "test-support")]
pub mod card;
pub mod host;
mod platform;

pub use platform::run;
