// SPDX-License-Identifier: GPL-3.0-or-later

//! `openlab-stage`: the stage host. Started by the engine, never by hand.

use std::process::ExitCode;

fn main() -> ExitCode {
    openlab_video::run(std::env::args().skip(1).collect())
}
