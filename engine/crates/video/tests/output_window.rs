// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

//! The shipped output window as AppKit holds it (macOS): built exactly as the stage host builds
//! one for a stage, for the main screen's frame, then fitted to the display with the menu bar
//! and to another; and one presented through the stage host's own present on the menu bar's
//! display. None is ordered front, so nothing appears on any display. Its own `main`
//! (harness = false), because AppKit windows are made on the main thread.

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
        "built at screen-saver level, over everything, until fitted to its display"
    );
    // One display, a stage on it, Black ticked: the app's windows, sheets, alerts, menus and
    // pop-ups (all at normal level or above), and the menu bar and Dock, stay over the stage;
    // the desktop and its icons (levels near the minimum) stay under it.
    assert_eq!(
        r.level_on_main,
        r.normal_level - 1,
        "on the menu bar's display: just under every app's windows"
    );
    assert!(r.level_on_main < r.normal_level && r.level_on_main < r.menu_bar_level);
    assert!(
        r.level_on_main > -1000,
        "still over the desktop and its icons"
    );
    // Two displays, the stage on the one without the menu bar: unchanged, over everything.
    assert_eq!(
        r.level_on_other, r.screen_saver_level,
        "another display: screen-saver level"
    );
    assert_eq!(
        r.level_back_on_main, r.level_on_main,
        "the menu bar moved back: the level follows"
    );
    assert!(
        r.behaviour_as_built,
        "every Space, stationary, out of cycling"
    );
    // The shipped present on the menu bar's display: the window is already under the app's
    // windows when it is ordered front, never over them for a moment.
    let (at_order_front, after) = r
        .presented_on_main
        .clone()
        .expect("a stage presented on the display with the menu bar");
    assert_eq!(
        at_order_front,
        [r.normal_level - 1],
        "present orders the window front once, already under the app's windows"
    );
    assert_eq!(after, r.normal_level - 1, "and leaves it there");
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
