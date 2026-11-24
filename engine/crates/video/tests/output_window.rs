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
    // #708 flow (one display with the menu bar, a stage on it, GO a Video cue → the picture covers
    // the whole display, over the app's windows, the menu bar and the Dock): the shipped present
    // on the menu bar's display orders the window front at screen-saver level over the screen's
    // whole frame (the menu bar's and the Dock's places included), and leaves it there.
    let p = r
        .presented_on_main
        .as_ref()
        .expect("a stage presented on the display with the menu bar");
    assert_eq!(
        p.ordered_front_at.first(),
        Some(&r.screen_saver_level),
        "present orders the window front over everything"
    );
    let [covering, standing_back, covering_again] = p.levels;
    assert_eq!(covering, r.screen_saver_level, "and leaves it there");
    assert!(
        p.frame_is_the_screens,
        "the window's frame is the screen's whole frame"
    );
    assert!(
        p.holds_the_visible_frame,
        "the frame holds the screen's visible frame and the menu bar's and Dock's places"
    );
    // #708 flow (a hung engine while a stage covers the only display → the cover comes down by
    // itself): standing back (a silent engine, or the app's question) puts that window just under
    // every app's windows, still over the desktop; covering again puts it back over everything.
    assert_eq!(
        standing_back,
        r.normal_level - 1,
        "standing back: under every app's windows"
    );
    assert_eq!(covering_again, r.screen_saver_level, "covering again");
    assert_eq!(
        p.ordered_front_at,
        [
            r.screen_saver_level,
            r.normal_level - 1,
            r.screen_saver_level
        ],
        "each level set before the window is ordered front"
    );
    if !p.visible_is_smaller {
        println!("output_window: the main screen's menu bar and Dock take no room here");
    }
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
    assert!(
        r.non_activating,
        "a click on it never makes the stage host active, so the operator's app is not drawn inactive"
    );
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
