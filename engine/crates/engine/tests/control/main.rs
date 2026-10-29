// SPDX-License-Identifier: GPL-3.0-or-later

//! One test binary for the control tests (one link step instead of 21); each file is a module.

#[path = "../common/mod.rs"]
mod common;
#[path = "../show/mod.rs"]
mod show;

mod default_mapping;
mod devices;
mod docs;
mod feedback;
mod live_levels;
mod media;
mod media_durations;
mod media_paths;
mod model;
mod model_edits;
mod multi_docs;
mod network;
mod patch_mapping;
mod preshow;
mod progress;
mod protocol;
mod shutdown;
mod timing;
mod undo_active;
mod watch;
mod wide_media;
