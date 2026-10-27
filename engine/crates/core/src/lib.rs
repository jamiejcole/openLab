// SPDX-License-Identifier: GPL-3.0-or-later

//! openLab core: workspace model, cue engine and protocol types.
//!
//! Pure logic only: no I/O, threads or clocks. Time is passed in by the caller.

#![forbid(unsafe_code)]

mod backup;
mod changes;
mod command;
mod cue;
mod cue_engine;
mod cue_index;
mod history;
mod media_path;
mod model;
pub mod protocol;
mod recovery;
mod relink;
mod settings;
mod show_file;
mod tree;
mod validate;

pub use backup::*;
pub use changes::*;
pub use command::*;
pub use cue::*;
pub use cue_engine::*;
pub use cue_index::*;
pub use history::*;
pub use media_path::*;
pub use model::*;
pub use recovery::*;
pub use relink::*;
pub use settings::*;
pub use show_file::*;
pub use validate::*;

/// Crate version, as set in the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_set() {
        assert!(!super::VERSION.is_empty());
    }
}
