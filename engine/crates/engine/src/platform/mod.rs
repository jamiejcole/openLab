// SPDX-License-Identifier: GPL-3.0-or-later

//! What differs per operating system (ARCHITECTURE §13): shutdown signals, sleep prevention and
//! relaunching the app (M4.9). Each part has a version for macOS and a fallback that does
//! nothing (and says so) elsewhere, so the engine builds and runs on every platform.

pub mod power;
pub mod signals;
