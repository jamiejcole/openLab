// SPDX-License-Identifier: GPL-3.0-or-later

//! macOS: AppKit output windows drawn with Metal.
//!
//! The process is an accessory app (no Dock icon, no menu bar) that never activates: its
//! windows come up with `orderFrontRegardless`, are borderless, ignore the mouse and cannot
//! become key, so the operator's keys stay with the openLab app. A stage's window covers its
//! display's whole frame at screen-saver level (over that display's menu bar and Dock), joins
//! every Space and stays out of Mission Control and window cycling. System full-screen mode is
//! never used: it would need activation and a Space of its own.
//!
//! AppKit and the drawing live on the main thread, with the [`Host`]. A reader thread decodes
//! the engine's lines and hands each to the main queue. When the engine's pipe closes the
//! process exits, so it never outlives the engine.

mod displays;
mod gpu;

use std::cell::RefCell;
use std::collections::HashMap;
use std::process::ExitCode;

use dispatch2::DispatchQueue;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSScreen,
    NSScreenSaverWindowLevel, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::CGSize;
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayChangeSummaryFlags, CGDisplayRegisterReconfigurationCallback,
};
use objc2_foundation::{NSNumber, NSRect, NSString};
use objc2_metal::{MTLCommandBuffer, MTLDrawable, MTLPixelFormat};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};
use openlab_core::StageId;
use openlab_core::stage_link::{Display, FromStage, LinkLevel, StageOutput, ToStage};

use self::displays::Found;
use self::gpu::{Gpu, fit};
use crate::host::{Flow, Host, Picture, Platform, write_all};
use openlab_core::stage_link::{Line, read_line};

pub fn run(args: Vec<String>) -> ExitCode {
    match args.as_slice() {
        [] => windowed(),
        #[cfg(feature = "test-support")]
        [flag, list] if flag == "--virtual-displays" => match virtual_displays(list) {
            Some(displays) => offscreen(displays),
            None => ExitCode::from(super::USAGE_EXIT),
        },
        _ => ExitCode::from(super::USAGE_EXIT),
    }
}

/// The Mac platform: the GPU, and the output windows (none when offscreen).
pub struct Mac {
    gpu: Result<Gpu, String>,
    windows: Option<Windows>,
    /// The displays last read, with their CoreGraphics ids.
    found: Vec<Found>,
    /// Test support: displays that stand in for real ones, drawn offscreen only.
    #[cfg(feature = "test-support")]
    virtual_displays: Option<Vec<Display>>,
}

struct Windows {
    mtm: MainThreadMarker,
    open: HashMap<StageId, StageWindow>,
}

struct StageWindow {
    window: Retained<NSWindow>,
    layer: Retained<CAMetalLayer>,
    display: String,
}

impl Platform for Mac {
    fn video(&self) -> bool {
        true
    }

    fn displays(&mut self) -> Vec<Display> {
        #[cfg(feature = "test-support")]
        if let Some(v) = &self.virtual_displays {
            return v.clone();
        }
        let mtm = self.windows.as_ref().map(|w| w.mtm);
        self.found = displays::active(|cg| {
            mtm.and_then(|mtm| screen_of(mtm, cg))
                .map(|s| s.localizedName().to_string())
        });
        self.found.iter().map(|f| f.display.clone()).collect()
    }

    fn show(
        &mut self,
        stage: &StageOutput,
        display: &Display,
        picture: Picture,
    ) -> Result<(), String> {
        let gpu = self.gpu.as_mut().map_err(|e| e.clone())?;
        let Some(windows) = self.windows.as_mut() else {
            // Offscreen: the stage is drawn only when a probe asks.
            return Ok(());
        };
        let cg = self
            .found
            .iter()
            .find(|f| f.display.id == display.id)
            .map(|f| f.cg)
            .ok_or("the display is not connected")?;
        let screen = screen_of(windows.mtm, cg).ok_or("the display has no screen")?;
        let frame = screen.frame();
        let w = match windows.open.get(&stage.id) {
            Some(w) if w.display == display.id => w,
            _ => {
                let new = open_window(windows.mtm, frame, gpu)?;
                if let Some(old) = windows.open.insert(
                    stage.id,
                    StageWindow {
                        display: display.id.clone(),
                        ..new
                    },
                ) {
                    old.window.close();
                }
                windows.open.get(&stage.id).ok_or("window lost")?
            }
        };
        w.window.setFrame_display(frame, false);
        w.layer.setDrawableSize(CGSize::new(
            f64::from(display.width),
            f64::from(display.height),
        ));
        w.window.orderFrontRegardless();
        let drawable = w
            .layer
            .nextDrawable()
            .ok_or("no drawable for the stage's window")?;
        let rect = fit((stage.width, stage.height), (display.width, display.height));
        let cb = gpu.draw(&drawable.texture(), rect, picture)?;
        let d: &ProtocolObject<dyn MTLDrawable> = ProtocolObject::from_ref(&*drawable);
        cb.presentDrawable(d);
        cb.commit();
        Ok(())
    }

    fn hide(&mut self, stage: StageId) {
        if let Some(w) = self.windows.as_mut().and_then(|w| w.open.remove(&stage)) {
            w.window.close();
        }
    }

    #[cfg(feature = "test-support")]
    fn set_virtual_displays(&mut self, displays: Vec<Display>) -> bool {
        match &mut self.virtual_displays {
            Some(v) => {
                *v = displays;
                true
            }
            None => false,
        }
    }

    #[cfg(feature = "test-support")]
    fn probe(
        &mut self,
        stage: &StageOutput,
        picture: Picture,
        points: &[[f32; 2]],
    ) -> Result<Vec<[u8; 4]>, String> {
        let gpu = self.gpu.as_mut().map_err(|e| e.clone())?;
        gpu.probe((stage.width, stage.height), picture, points)
    }
}

/// A borderless, never-key window covering `frame`, with a Metal layer.
fn open_window(mtm: MainThreadMarker, frame: NSRect, gpu: &Gpu) -> Result<StageWindow, String> {
    let window = output_window(mtm, frame);
    let layer = CAMetalLayer::new();
    layer.setDevice(Some(gpu.device()));
    layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
    layer.setFramebufferOnly(true);
    let view = window
        .contentView()
        .ok_or("the window has no content view")?;
    view.setLayer(Some(&layer));
    view.setWantsLayer(true);
    Ok(StageWindow {
        window,
        layer,
        display: String::new(),
    })
}

/// The collection behaviour of an output window: on every Space, staying put in Mission
/// Control, out of window cycling, beside full-screen apps.
fn output_behaviour() -> NSWindowCollectionBehavior {
    NSWindowCollectionBehavior::CanJoinAllSpaces
        | NSWindowCollectionBehavior::Stationary
        | NSWindowCollectionBehavior::IgnoresCycle
        | NSWindowCollectionBehavior::FullScreenAuxiliary
}

/// An output window covering `frame`, not yet ordered front: borderless (so it can never
/// become key or main), at screen-saver level, ignoring the mouse, without a shadow, black.
fn output_window(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSWindow> {
    // SAFETY: AppKit on the main thread (the marker proves it); a borderless buffered window.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc::<NSWindow>(),
            frame,
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: the window is released by our `Retained`, not by AppKit on close.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setLevel(NSScreenSaverWindowLevel);
    window.setCollectionBehavior(output_behaviour());
    window.setIgnoresMouseEvents(true);
    window.setHasShadow(false);
    window.setBackgroundColor(Some(&NSColor::blackColor()));
    window
}

/// Test support: what an output window built for the main screen's frame is, read back from
/// AppKit. The window is never ordered front, so nothing appears on any display.
#[cfg(feature = "test-support")]
pub struct WindowReport {
    pub level: isize,
    pub screen_saver_level: isize,
    pub behaviour_as_built: bool,
    pub ignores_mouse: bool,
    pub can_become_key: bool,
    pub can_become_main: bool,
    pub has_shadow: bool,
    pub borderless: bool,
    pub frame_is_the_screens: bool,
    pub visible: bool,
}

/// Test support: builds an output window on the main thread for the main screen's frame and
/// reads its properties back; `None` off the main thread or without a screen.
#[cfg(feature = "test-support")]
pub fn window_report() -> Option<WindowReport> {
    let mtm = MainThreadMarker::new()?;
    let screen = NSScreen::mainScreen(mtm)?;
    let frame = screen.frame();
    let window = output_window(mtm, frame);
    let got = window.frame();
    let report = WindowReport {
        level: window.level(),
        screen_saver_level: NSScreenSaverWindowLevel,
        behaviour_as_built: window.collectionBehavior() == output_behaviour(),
        ignores_mouse: window.ignoresMouseEvents(),
        can_become_key: window.canBecomeKeyWindow(),
        can_become_main: window.canBecomeMainWindow(),
        has_shadow: window.hasShadow(),
        borderless: window.styleMask() == NSWindowStyleMask::Borderless,
        frame_is_the_screens: got.origin.x == frame.origin.x
            && got.origin.y == frame.origin.y
            && got.size.width == frame.size.width
            && got.size.height == frame.size.height,
        visible: window.isVisible(),
    };
    window.close();
    Some(report)
}

/// Ends the process at once, from any thread: no exit handlers and no stdio flush, so a hung
/// main thread holding a lock cannot keep the stage host alive after the engine is gone.
fn end_now() -> ! {
    unsafe extern "C" {
        fn _exit(status: std::ffi::c_int) -> !;
    }
    // SAFETY: `_exit` is the C library's, declared with its C signature; it takes a status and
    // never returns, and nothing is left to clean up that the kernel does not (windows go with
    // the process). Covered by the test of the stage host ending when its input closes.
    unsafe { _exit(0) }
}

/// The AppKit screen of CoreGraphics display `cg`.
fn screen_of(mtm: MainThreadMarker, cg: CGDirectDisplayID) -> Option<Retained<NSScreen>> {
    let key = NSString::from_str("NSScreenNumber");
    NSScreen::screens(mtm).iter().find(|screen| {
        screen
            .deviceDescription()
            .objectForKey(&key)
            .and_then(|n| n.downcast::<NSNumber>().ok())
            .is_some_and(|n| n.unsignedIntValue() == cg)
    })
}

thread_local! {
    /// The windowed host, on the main thread only.
    static HOST: RefCell<Option<Host<Mac>>> = const { RefCell::new(None) };
}

fn windowed() -> ExitCode {
    let Some(mtm) = MainThreadMarker::new() else {
        return ExitCode::FAILURE;
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let platform = Mac {
        gpu: Gpu::new(),
        windows: Some(Windows {
            mtm,
            open: HashMap::new(),
        }),
        found: Vec::new(),
        #[cfg(feature = "test-support")]
        virtual_displays: None,
    };
    HOST.with(|h| *h.borrow_mut() = Some(Host::new(platform)));
    // SAFETY: a plain function with no user data; it only queues work on the main thread.
    unsafe {
        CGDisplayRegisterReconfigurationCallback(Some(reconfigured), std::ptr::null_mut());
    }
    if std::thread::Builder::new()
        .name("openlab-stage-reader".into())
        .spawn(read_engine)
        .is_err()
    {
        return ExitCode::FAILURE;
    }
    app.run();
    ExitCode::SUCCESS
}

/// Reads the engine's lines and hands each to the main thread; the end of input ends the
/// process.
fn read_engine() {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut line = Vec::new();
    loop {
        line.clear();
        let message = match read_line(&mut input, &mut line) {
            Ok(Line::End) | Err(_) => end_now(),
            Ok(Line::TooLong(bytes)) => {
                Err(format!("ignored a line of {bytes} bytes from the engine"))
            }
            Ok(Line::Read) => openlab_core::stage_link::decode_to_stage(&line)
                .map_err(|e| format!("ignored a message from the engine: {e}")),
        };
        match message {
            Ok(message) => DispatchQueue::main().exec_async(move || on_main(message)),
            Err(warning) => send(vec![FromStage::Log {
                level: LinkLevel::Warn,
                message: warning,
            }]),
        }
    }
}

fn on_main(message: ToStage) {
    let mut out = Vec::new();
    let flow = HOST.with(|h| {
        h.borrow_mut()
            .as_mut()
            .map(|host| host.handle(message, &mut out))
    });
    send(out);
    if flow == Some(Flow::Quit) {
        std::process::exit(0);
    }
}

fn send(mut answers: Vec<FromStage>) {
    if write_all(&mut std::io::stdout().lock(), &mut answers).is_err() {
        // The engine is gone.
        end_now();
    }
}

unsafe extern "C-unwind" fn reconfigured(
    _display: CGDirectDisplayID,
    flags: CGDisplayChangeSummaryFlags,
    _user: *mut std::ffi::c_void,
) {
    if flags.contains(CGDisplayChangeSummaryFlags::BeginConfigurationFlag) {
        return;
    }
    DispatchQueue::main().exec_async(read_displays_again);
    // AppKit's screen list may not have caught up when the first read runs (a display just
    // plugged back in would stay missing until the next display event), so read once more
    // a moment later; a read that finds nothing new changes and reports nothing.
    if let Ok(later) = dispatch2::DispatchTime::try_from(SECOND_DISPLAY_READ) {
        let _ = DispatchQueue::main().after(later, read_displays_again);
    }
}

/// How long after a display change the displays are read a second time.
const SECOND_DISPLAY_READ: std::time::Duration = std::time::Duration::from_millis(500);

fn read_displays_again() {
    let mut out = Vec::new();
    HOST.with(|h| {
        if let Some(host) = h.borrow_mut().as_mut() {
            host.displays_changed(&mut out);
        }
    });
    send(out);
}

/// Test support: `ID:WxH,ID:WxH...` displays drawn offscreen, no windows.
#[cfg(feature = "test-support")]
fn virtual_displays(list: &str) -> Option<Vec<Display>> {
    list.split(',')
        .map(|item| {
            let (id, size) = item.split_once(':')?;
            let (w, h) = size.split_once('x')?;
            Some(Display {
                id: id.to_owned(),
                name: format!("Virtual {id}"),
                width: w.parse().ok()?,
                height: h.parse().ok()?,
            })
        })
        .collect()
}

/// Test support: the host on the main thread with no windows, its stages drawn offscreen for
/// probes. A `hang` stops answering but still ends with the engine's pipe.
#[cfg(feature = "test-support")]
fn offscreen(displays: Vec<Display>) -> ExitCode {
    let mut host = Host::new(Mac {
        gpu: Gpu::new(),
        windows: None,
        found: Vec::new(),
        virtual_displays: Some(displays),
    });
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let flow = crate::host::serve(&mut input, &mut std::io::stdout().lock(), |m, out| {
        host.handle(m, out)
    });
    if flow == Flow::Hang {
        let _ = std::io::copy(&mut input, &mut std::io::sink());
    }
    ExitCode::SUCCESS
}

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use super::*;

    #[test]
    fn virtual_displays_are_parsed_or_refused() {
        let d = virtual_displays("A:1920x1080,B:640x480").unwrap();
        assert_eq!(d.len(), 2);
        assert_eq!((d[1].id.as_str(), d[1].width, d[1].height), ("B", 640, 480));
        for bad in ["", "A", "A:1920", "A:x1080", "A:1920x"] {
            assert!(virtual_displays(bad).is_none(), "{bad}");
        }
    }
}
