// SPDX-License-Identifier: GPL-3.0-or-later

//! Monitor windows: a live copy of one stage in an ordinary titled window the operator can
//! move, resize and close, for building video without a second display (QLab 5 manual
//! p. 157). The stage is fitted to the window, black bars where the shapes differ.
//!
//! The window is a non-activating panel that only becomes key for a view that needs typing
//! (it has none), so clicking, moving or resizing it never activates the stage host or takes
//! the operator's keys from the app. It stays up while the app is active (a panel would
//! otherwise hide when its own app is not).
//!
//! A monitor never holds an output up: it is drawn after every display, and skips a frame
//! when the window cannot be seen (minimized, covered, on another Space) or the GPU has not
//! finished its last one, so asking its layer for a drawable never waits on the window
//! server. Its layer presents without waiting for the display's refresh, so its drawables
//! come back at once.

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSPanel, NSWindowOcclusionState, NSWindowStyleMask,
};
use objc2_core_foundation::CGSize;
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_metal::{MTLCommandBuffer, MTLCommandBufferStatus, MTLPixelFormat};
use objc2_quartz_core::CAMetalLayer;

use super::gpu::Gpu;

/// The longer side of a new monitor window's content, in points: the stage's shape at this
/// size, so it opens with no black bars.
const OPENING_SIDE: f64 = 640.0;
/// The least a monitor window's content may be made, in points, and the least side a new one
/// opens with (a very wide stage still opens tall enough to see).
const LEAST_SIDE: f64 = 90.0;
/// The most pixels a side of a monitor's drawing has: a window stretched over a large display
/// is drawn at this and scaled up, so a monitor never costs the GPU more than a display does.
const MOST_PIXELS: f64 = 4096.0;
/// How far each further monitor window opens from the last, so they do not hide each other.
const CASCADE: f64 = 24.0;

/// The style of a monitor window: titled, closable, resizable and minimizable, and never
/// activating the stage host.
pub fn style() -> NSWindowStyleMask {
    NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Resizable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::NonactivatingPanel
}

/// A new monitor window's content size, in points: the stage's shape with its longer side
/// [`OPENING_SIDE`], neither side under [`LEAST_SIDE`].
pub fn opening_size((w, h): (u32, u32)) -> (f64, f64) {
    let (w, h) = (f64::from(w.max(1)), f64::from(h.max(1)));
    let scale = OPENING_SIDE / w.max(h);
    ((w * scale).max(LEAST_SIDE), (h * scale).max(LEAST_SIDE))
}

/// A drawing size in pixels for content of `points` at `scale` pixels a point: at least 1,
/// at most [`MOST_PIXELS`] a side.
pub fn pixels((w, h): (f64, f64), scale: f64) -> (f64, f64) {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let side = |v: f64| {
        let p = (v * scale).round();
        if p.is_finite() {
            p.clamp(1.0, MOST_PIXELS)
        } else {
            1.0
        }
    };
    (side(w), side(h))
}

/// One stage's monitor window.
pub struct Monitor {
    pub window: Retained<NSPanel>,
    layer: Retained<CAMetalLayer>,
    /// The command buffer that drew its last frame: no frame is drawn until it is done.
    last: Option<Retained<ProtocolObject<dyn MTLCommandBuffer>>>,
}

/// A monitor window for a stage of `stage` pixels titled `title`, built but not yet ordered
/// front: `opened` monitors are open already, and it opens that many steps down from the
/// centre of the main screen.
pub fn build(
    mtm: MainThreadMarker,
    title: &str,
    stage: (u32, u32),
    opened: usize,
    gpu: &Gpu,
) -> Result<Monitor, String> {
    let window = panel(mtm, title, stage);
    window.center();
    if opened > 0 {
        let step = CASCADE * opened as f64;
        let frame = window.frame();
        window.setFrameOrigin(NSPoint::new(frame.origin.x + step, frame.origin.y - step));
    }
    let layer = CAMetalLayer::new();
    layer.setDevice(Some(gpu.device()));
    layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
    layer.setFramebufferOnly(true);
    layer.setDisplaySyncEnabled(false);
    let view = window
        .contentView()
        .ok_or("the monitor window has no content view")?;
    view.setLayer(Some(&layer));
    view.setWantsLayer(true);
    Ok(Monitor {
        window,
        layer,
        last: None,
    })
}

/// The panel of a monitor window, not ordered front.
pub fn panel(mtm: MainThreadMarker, title: &str, stage: (u32, u32)) -> Retained<NSPanel> {
    let (w, h) = opening_size(stage);
    let content = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h));
    let window = NSPanel::initWithContentRect_styleMask_backing_defer(
        mtm.alloc::<NSPanel>(),
        content,
        style(),
        NSBackingStoreType::Buffered,
        false,
    );
    // SAFETY: the window is released by our `Retained`, not by AppKit on close.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str(title));
    window.setBecomesKeyOnlyIfNeeded(true);
    window.setHidesOnDeactivate(false);
    window.setFloatingPanel(false);
    window.setContentMinSize(NSSize::new(LEAST_SIDE, LEAST_SIDE));
    window.setBackgroundColor(Some(&NSColor::blackColor()));
    window
}

impl Monitor {
    /// Whether a frame drawn now would be seen and can be drawn without waiting: the window is
    /// on screen and not covered, and the GPU is done with the last frame.
    pub fn ready(&self) -> bool {
        let seen = self
            .window
            .occlusionState()
            .contains(NSWindowOcclusionState::Visible);
        let done = self.last.as_ref().is_none_or(|cb| {
            matches!(
                cb.status(),
                MTLCommandBufferStatus::Completed | MTLCommandBufferStatus::Error
            )
        });
        seen && done
    }

    /// Fits the layer's drawing to the window's content now (the operator may have resized
    /// it, or moved it to a display of another scale).
    pub fn fit(&self) {
        let Some(view) = self.window.contentView() else {
            return;
        };
        let size = view.frame().size;
        let (w, h) = pixels((size.width, size.height), self.window.backingScaleFactor());
        let now = self.layer.drawableSize();
        if now.width != w || now.height != h {
            self.layer.setDrawableSize(CGSize::new(w, h));
        }
    }

    pub fn layer(&self) -> &CAMetalLayer {
        &self.layer
    }

    /// The frame drawn last was drawn by `cb`.
    pub fn drawn_with(&mut self, cb: &Retained<ProtocolObject<dyn MTLCommandBuffer>>) {
        self.last = Some(cb.clone());
    }

    /// The operator closed it (its close button): it is no longer on screen and not
    /// minimized.
    pub fn closed(&self) -> bool {
        !self.window.isVisible() && !self.window.isMiniaturized()
    }

    pub fn set_title(&self, title: &str) {
        let title = NSString::from_str(title);
        if self.window.title() != title {
            self.window.setTitle(&title);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_monitor_opens_at_its_stages_shape() {
        assert_eq!(opening_size((1920, 1080)), (640.0, 360.0));
        assert_eq!(opening_size((1080, 1920)), (360.0, 640.0));
        assert_eq!(opening_size((1000, 1000)), (640.0, 640.0));
        // A panorama still opens tall enough to see.
        assert_eq!(opening_size((16384, 16)), (640.0, LEAST_SIDE));
        assert_eq!(opening_size((0, 0)), (640.0, 640.0));
    }

    #[test]
    fn a_monitor_draws_at_most_its_pixel_limit() {
        assert_eq!(pixels((640.0, 360.0), 2.0), (1280.0, 720.0));
        assert_eq!(pixels((5000.0, 100.0), 2.0), (MOST_PIXELS, 200.0));
        assert_eq!(pixels((0.0, 0.0), 1.0), (1.0, 1.0));
        assert_eq!(pixels((f64::NAN, 10.0), f64::NAN), (1.0, 10.0));
    }
}
