// SPDX-License-Identifier: GPL-3.0-or-later

//! Diagnostics: the show log (DX-02), system status (DX-03), local crash reports and the
//! unclean-exit marker (DX-04).
//! The decisions are here and take their inputs as arguments; the control thread feeds them
//! (`control::observe`). Nothing in this module runs on the audio thread. See docs/logging.md.

pub mod cpu;
pub mod crash;
pub mod log;
pub mod run_marker;
pub mod status;
pub mod time;
pub mod writer;

pub use log::{ShowLog, format_entry};
pub use status::{Counters, Peek, StatusConfig, StatusReporter};
pub use writer::{LogLimits, LogSink, LogWriter};
