// SPDX-License-Identifier: GPL-3.0-or-later

//! The content of an output window: it takes every click over its stage, so none reaches an
//! app behind it, and the pointer's moves (an app behind no longer sets its shape there). A
//! click brings the operator's app forward with the keys, so Esc then panics, as a click on
//! QLab's own output window does. The stage host never keeps the keys: whenever it is the
//! active app it hands them to the operator's app (`keep_keys_with_operator`).
//!
//! The pointer is hidden over a stage by the operator's app, not here: macOS lets only the
//! active app hide the pointer or set its shape (a background app's `hide` and `set` are
//! ignored, measured on the machine), and the stage host is never the active app for long.

use std::cell::Cell;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSEvent, NSRunningApplication, NSView,
};
use objc2_foundation::NSRect;

thread_local! {
    /// The operator's app (the engine's app connection's process), on the main thread.
    static OPERATOR: Cell<Option<u32>> = const { Cell::new(None) };
    /// When the keys were last handed on, so a handover the system refuses is not asked again
    /// every frame.
    static HANDED: Cell<Option<Instant>> = const { Cell::new(None) };
    /// Test support: handovers asked for (a click, or the stage host found active).
    #[cfg(feature = "test-support")]
    static HANDOVERS: Cell<u32> = const { Cell::new(0) };
    /// Test support: set, a handover is counted but no app is activated or deactivated.
    #[cfg(feature = "test-support")]
    static NEVER_HAND_OVER: Cell<bool> = const { Cell::new(false) };
}

/// The least time between two handovers the stage host makes by itself.
const HANDOVER_EVERY: Duration = Duration::from_millis(250);

define_class!(
    // SAFETY: NSView has no subclassing requirements beyond its designated initialiser, which
    // `StageView::new` calls; the class has no ivars and no `Drop`.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "OpenLabStageView"]
    pub struct StageView;

    impl StageView {
        /// A click on a stage of a stage host that is not active is still delivered.
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            hand_keys_to_operator(self.mtm());
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, _event: &NSEvent) {
            hand_keys_to_operator(self.mtm());
        }

        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, _event: &NSEvent) {
            hand_keys_to_operator(self.mtm());
        }

        /// Scrolling over a stage goes nowhere (no app behind scrolls).
        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, _event: &NSEvent) {}
    }
);

impl StageView {
    /// A stage view over `frame`.
    pub fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSView's designated initialiser on a freshly allocated view.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

/// The operator's app is now the process `pid` (`None`: none known).
pub fn set_operator(pid: Option<u32>) {
    OPERATOR.with(|o| o.set(pid));
}

/// A click on a stage: the operator's app comes forward and its key window takes the keys again.
/// Nothing is done when it is the active app already (the stage's window does not activate the
/// stage host, so a click then changes nothing on screen); only its key and main windows are
/// brought forward, never every window, so no window of it is ordered or redrawn without need.
/// With no operator's app known (or one that cannot be found), the stage host only lets go of
/// the keys (the app active before it gets them back).
pub fn hand_keys_to_operator(mtm: MainThreadMarker) {
    HANDED.with(|h| h.set(Some(Instant::now())));
    #[cfg(feature = "test-support")]
    {
        HANDOVERS.with(|n| n.set(n.get() + 1));
        if NEVER_HAND_OVER.with(Cell::get) {
            return;
        }
    }
    let app = NSApplication::sharedApplication(mtm);
    let target = OPERATOR
        .with(Cell::get)
        .and_then(|pid| i32::try_from(pid).ok().filter(|p| *p > 0))
        .and_then(NSRunningApplication::runningApplicationWithProcessIdentifier);
    let Some(target) = target else {
        app.deactivate();
        return;
    };
    if target.isActive() {
        return;
    }
    let options = NSApplicationActivationOptions::empty();
    let handed = if objc2::available!(macos = 14.0) {
        // Cooperative activation: the stage host, active from the click, hands it on.
        app.yieldActivationToApplication(&target);
        target.activateFromApplication_options(&NSRunningApplication::currentApplication(), options)
    } else {
        #[allow(deprecated)]
        let options = options | NSApplicationActivationOptions::ActivateIgnoringOtherApps;
        target.activateWithOptions(options)
    };
    if !handed {
        app.deactivate();
    }
}

/// From every poll: the stage host is never the active app for longer than a frame or two. A
/// click on a stage, or the system at the stage host's start, may have made it so; the
/// operator's app (or the app before) gets the keys back, at most every [`HANDOVER_EVERY`].
pub fn keep_keys_with_operator(mtm: MainThreadMarker) {
    if !NSApplication::sharedApplication(mtm).isActive() {
        return;
    }
    if HANDED.with(|h| h.get().is_none_or(|t| t.elapsed() >= HANDOVER_EVERY)) {
        hand_keys_to_operator(mtm);
    }
}

/// Test support: handovers asked for since the last call; while `quiet`, none really
/// activates or deactivates an app on the machine.
#[cfg(feature = "test-support")]
pub fn take_handovers(quiet: bool) -> u32 {
    NEVER_HAND_OVER.with(|n| n.set(quiet));
    HANDOVERS.with(|n| n.replace(0))
}
