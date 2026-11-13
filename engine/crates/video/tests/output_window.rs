// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

//! The shipped output window as AppKit holds it (macOS): built exactly as the stage host builds
//! one for a stage, for the main screen's frame, and never ordered front, so nothing appears on
//! any display. Its own `main` (harness = false), because AppKit windows are made on the main
//! thread.

#[cfg(target_os = "macos")]
fn main() {
    let Some(r) = openlab_video::window_report() else {
        // No screen: a machine without a display (a CI runner has a virtual one). Reported,
        // never passed silently; on CI it fails.
        if std::env::var_os("CI").is_some() {
            panic!("no screen to build an output window on");
        }
        println!("output_window: SKIPPED (no screen on this machine)");
        return;
    };
    assert_eq!(
        r.level, r.screen_saver_level,
        "screen-saver level, over the menu bar and Dock"
    );
    assert!(
        r.behaviour_as_built,
        "every Space, stationary, out of cycling"
    );
    assert!(r.ignores_mouse, "ignores the mouse");
    assert!(
        !r.can_become_key,
        "never key: the operator's keys stay with the app"
    );
    assert!(!r.can_become_main);
    assert!(!r.has_shadow);
    assert!(r.borderless);
    assert!(r.frame_is_the_screens, "covers the display's whole frame");
    assert!(!r.visible, "built, never shown");
    println!("output_window: ok");
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("output_window: SKIPPED (output windows are macOS only)");
}
