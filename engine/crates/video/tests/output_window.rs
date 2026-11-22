// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

//! The shipped output window as AppKit holds it (macOS): built exactly as the stage host builds
//! one for a stage, for the main screen's frame; one presented through the stage host's own
//! present on the menu bar's display; and a stage's monitor window. None is ordered front, so
//! nothing appears on any display. Its own `main` (harness = false), because AppKit windows are
//! made on the main thread.

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
    // #708: a stage covers its whole display over everything (every app's windows, the menu
    // bar and the Dock), the display with the menu bar and a laptop's only one included.
    assert_eq!(
        r.level, r.screen_saver_level,
        "built at screen-saver level, over everything"
    );
    assert!(r.level > r.menu_bar_level && r.level > r.normal_level);
    assert!(
        r.behaviour_as_built,
        "every Space, stationary, out of cycling"
    );
    // The shipped present on the menu bar's display: ordered front once, at screen-saver level,
    // and left there.
    let (at_order_front, after) = r
        .presented_on_main
        .clone()
        .expect("a stage presented on the display with the menu bar");
    assert_eq!(
        at_order_front,
        [r.screen_saver_level],
        "present orders the window front once, over everything"
    );
    assert_eq!(after, r.screen_saver_level, "and leaves it there");
    // #795: a click on a stage never reaches an app behind it, and brings the operator's app
    // forward with the keys (the stage host hands them over), the first click too.
    assert!(!r.ignores_mouse, "takes every click over the stage");
    assert!(
        r.accepts_first_mouse,
        "the first click too, the stage host not active"
    );
    assert!(
        r.click_lands_on_stage,
        "a click lands on the stage, not behind it"
    );
    assert_eq!(
        r.clicks_handed_over, 3,
        "a left, right or other click hands the keys to the operator's app"
    );
    assert!(
        !r.can_become_key,
        "never key: the operator's keys stay with the app"
    );
    assert!(!r.can_become_main);
    assert!(!r.has_shadow);
    assert!(r.borderless);
    assert!(r.frame_is_the_screens, "covers the display's whole frame");
    assert!(!r.visible, "built, never shown");
    // #648: a stage's monitor window, as its monitor button opens it: titled with the stage's
    // name, closable and resizable, at the stage's shape, at normal level among the other
    // windows, never activating the stage host or taking the operator's keys, and not hidden
    // while the app (not the stage host) is active.
    let m = &r.monitor;
    assert_eq!(m.title, "Stage 1");
    assert!(
        m.titled && m.closable && m.resizable,
        "a window the operator moves, sizes and closes"
    );
    assert!(
        m.non_activating,
        "clicking it never activates the stage host"
    );
    assert!(
        m.key_only_if_needed,
        "the operator's keys stay with the app"
    );
    assert!(!m.hides_on_deactivate, "stays up while the app is active");
    assert!(!m.floating);
    assert_eq!(m.level, r.normal_level);
    assert_eq!(m.content, (640.0, 360.0), "opens at the stage's shape");
    assert!(!m.visible, "built, never shown");
    println!("output_window: ok");
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("output_window: SKIPPED (output windows are macOS only)");
}
