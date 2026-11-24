// SPDX-License-Identifier: GPL-3.0-or-later

//! macOS: AppKit output windows drawn with Metal.
//!
//! The process is an accessory app (no Dock icon, no menu bar) that never keeps the keys: its
//! windows come up with `orderFrontRegardless`, are borderless and cannot become key, and if
//! anything makes it the active app (a click on a stage) it hands the keys to the openLab app at
//! once (`stage_view`). A stage's window takes every click and the pointer over it, so nothing
//! behind it sees either: the pointer is blank there, and a click brings the openLab app forward.
//! A stage's window covers its display's whole frame, joins every Space and stays out of
//! Mission Control and window cycling. It is at screen-saver level, over everything, on every display; on the one with the
//! menu bar (the operator's, a laptop's only one) it stands back just under every app's windows
//! while the app has a question up or the engine has gone silent, so the operator can see the
//! app. System full-screen mode is never used: it would need activation and a Space of its own.
//!
//! AppKit and the drawing live on the main thread, with the [`Host`]. A reader thread decodes
//! the engine's lines and hands each to the main queue. Every picture's file is opened and
//! decoded on a thread of its own (`media`): the main thread only uploads finished frames and
//! draws, so it always answers the engine. When the engine's pipe closes the process exits, so
//! it never outlives the engine.

mod displays;
mod gpu;
mod media;
mod monitor;
mod probe;
mod stage_view;
#[cfg(feature = "test-support")]
mod testmedia;

use std::cell::RefCell;
use std::collections::HashMap;
use std::process::ExitCode;

use dispatch2::DispatchQueue;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSNormalWindowLevel,
    NSPanel, NSScreen, NSScreenSaverWindowLevel, NSWindow, NSWindowCollectionBehavior,
    NSWindowLevel, NSWindowStyleMask,
};
use objc2_core_foundation::CGSize;
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayChangeSummaryFlags, CGDisplayRegisterReconfigurationCallback,
    CGMainDisplayID,
};
use objc2_foundation::{NSNumber, NSPoint, NSRect, NSString};
use objc2_metal::{MTLCommandBuffer, MTLDevice, MTLDrawable, MTLPixelFormat};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};
use openlab_core::StageId;
use openlab_core::stage_link::{Display, FromStage, LinkLevel, ToStage};

use self::displays::Found;
use self::gpu::Gpu;
use crate::host::{Flow, Host, Layer, PictureEvent, Platform, Presented, write_all};
use openlab_core::stage_link::{Line, read_line};

pub fn run(args: Vec<String>) -> ExitCode {
    match args.as_slice() {
        [] => windowed(),
        [flag, path] if flag == "--probe" => {
            super::print_probe(&probe::probe(std::path::Path::new(path)))
        }
        #[cfg(feature = "test-support")]
        [flag, rest @ ..] if flag == "--make-test-media" => match testmedia::make(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(why) => {
                // Test support: the test that ran it shows this when the file is not made.
                use std::io::Write;
                let _ = writeln!(std::io::stderr().lock(), "{why}");
                ExitCode::FAILURE
            }
        },
        #[cfg(feature = "test-support")]
        [flag, path] if flag == "--test-decoder" => {
            use std::io::Write;
            match testmedia::uses_hardware_decoder(path) {
                Ok(hardware) => {
                    let said = if hardware { "hardware" } else { "software" };
                    let _ = writeln!(std::io::stdout().lock(), "{said}");
                    ExitCode::SUCCESS
                }
                Err(why) => {
                    let _ = writeln!(std::io::stderr().lock(), "{why}");
                    ExitCode::FAILURE
                }
            }
        }
        #[cfg(feature = "test-support")]
        [flag, list, options @ ..] if flag == "--virtual-displays" => {
            let (list, starred) = menu_bar_marked(list);
            match (virtual_displays(&list), offscreen_options(options)) {
                (Some(displays), Some(mut options)) => {
                    options.menu_bar = options.menu_bar.or(starred);
                    if options.hand_clock {
                        media::fault::use_hand_clock();
                    }
                    offscreen(displays, options)
                }
                _ => ExitCode::from(super::USAGE_EXIT),
            }
        }
        _ => ExitCode::from(super::USAGE_EXIT),
    }
}

/// The Mac platform: the GPU, and the output windows (none when offscreen).
pub struct Mac {
    gpu: Result<Gpu, String>,
    windows: Option<Windows>,
    /// Video cues' pictures, by the engine's id.
    media: HashMap<u64, media::Picture>,
    /// What became of pictures that failed while drawn, not yet reported.
    failed: Vec<PictureEvent>,
    /// Test support: the limits on the open and frames of a file a fault switch names
    /// (`limits_of`).
    #[cfg(feature = "test-support")]
    fault_limits: media::Limits,
    /// Test support: the limits on every other file's open and frames (`limits_of`).
    #[cfg(feature = "test-support")]
    good_limits: media::Limits,
    /// The displays last read, with their CoreGraphics ids.
    found: Vec<Found>,
    /// The display whose window stands back under the app's windows (`Platform::stand_back`).
    standing_back: Option<String>,
    /// Test support: displays that stand in for real ones, drawn offscreen only.
    #[cfg(feature = "test-support")]
    virtual_displays: Option<Vec<Display>>,
    /// Test support: the virtual display that stands for the one with the menu bar
    /// (`--menu-bar-display`).
    #[cfg(feature = "test-support")]
    virtual_menu_bar: Option<String>,
}

struct Windows {
    mtm: MainThreadMarker,
    /// One window per display with stages shown on it, by display id.
    open: HashMap<String, DisplayWindow>,
    /// The monitor windows open, by stage.
    monitors: HashMap<StageId, monitor::Monitor>,
}

struct DisplayWindow {
    window: Retained<NSWindow>,
    layer: Retained<CAMetalLayer>,
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
            mtm.and_then(|mtm| screen_of(mtm, cg)).map(|s| {
                (
                    s.localizedName().to_string(),
                    u32::try_from(s.maximumFramesPerSecond()).unwrap_or(0),
                )
            })
        });
        self.found.iter().map(|f| f.display.clone()).collect()
    }

    fn present(&mut self, display: &Display, layers: &[Layer]) -> Result<Presented, String> {
        let gpu = self.gpu.as_mut().map_err(|e| e.clone())?;
        let Some(windows) = self.windows.as_mut() else {
            // Offscreen: the stages are drawn only when a probe asks.
            return Ok(Presented::Drawn);
        };
        let cg = self
            .found
            .iter()
            .find(|f| f.display.id == display.id)
            .map(|f| f.cg)
            .ok_or("the display is not connected")?;
        let screen = screen_of(windows.mtm, cg).ok_or("the display has no screen")?;
        let frame = screen.frame();
        if !windows.open.contains_key(&display.id) {
            let window = open_window(windows.mtm, frame, gpu)?;
            windows.open.insert(display.id.clone(), window);
        }
        let w = windows.open.get(&display.id).ok_or("window lost")?;
        w.window.setFrame_display(frame, false);
        let level = level_for(self.standing_back.as_deref() == Some(display.id.as_str()));
        if w.window.level() != level {
            w.window.setLevel(level);
        }
        w.layer.setDrawableSize(CGSize::new(
            f64::from(display.width),
            f64::from(display.height),
        ));
        order_front(&w.window);
        // No drawable in time (the GPU under load): this frame is skipped, the window kept.
        let Some(drawable) = w.layer.nextDrawable() else {
            return Ok(Presented::Skipped);
        };
        let device = gpu.device_owned();
        let (media, failed) = (&mut self.media, &mut self.failed);
        let mut drawn = Vec::new();
        let cb = gpu.draw(&drawable.texture(), layers, &mut |c| {
            drawn.push(c.picture);
            // Shown at the next refresh: the frame due then.
            let ahead = media::FRAME_AHEAD;
            frame_of(&device, media, failed, c, ahead, false)
        })?;
        for picture in drawn {
            if let Some(p) = self.media.get_mut(&picture) {
                p.drawn_with(&cb);
            }
        }
        let d: &ProtocolObject<dyn MTLDrawable> = ProtocolObject::from_ref(&*drawable);
        cb.presentDrawable(d);
        cb.commit();
        Ok(Presented::Drawn)
    }

    fn close(&mut self, display: &str) {
        if let Some(w) = self.windows.as_mut().and_then(|w| w.open.remove(display)) {
            w.window.close();
        }
    }

    #[cfg(feature = "test-support")]
    fn advance_media_clock(&mut self, ms: u64) {
        media::fault::advance(ms);
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
        size: (u32, u32),
        layers: &[Layer],
        points: &[[f32; 2]],
    ) -> Result<Vec<[u8; 4]>, String> {
        let gpu = self.gpu.as_mut().map_err(|e| e.clone())?;
        let device = gpu.device_owned();
        let (media, failed) = (&mut self.media, &mut self.failed);
        // Offscreen, a probe shows the frame at the clock exactly: it waits (within the
        // limits) for each picture's decode thread. Never on real displays.
        gpu.probe(size, layers, points, &mut |c| {
            frame_of(&device, media, failed, c, 0.0, true)
        })
    }

    fn open_picture(
        &mut self,
        picture: u64,
        file: &str,
        still: bool,
        offset: f64,
    ) -> Result<(), String> {
        self.gpu.as_ref().map_err(|e| e.clone())?;
        let source = media::Picture::open(file, still, offset, self.limits_of(file))?;
        self.media.insert(picture, source);
        Ok(())
    }

    fn close_picture(&mut self, picture: u64) {
        self.media.remove(&picture);
    }

    fn take_picture_events(&mut self) -> Vec<PictureEvent> {
        let mut events = std::mem::take(&mut self.failed);
        let mut gone = Vec::new();
        for (id, picture) in &mut self.media {
            // Before a failure it may have led to: said even when the picture then goes.
            if let Some(why) = picture.take_reopened() {
                events.push(PictureEvent::Reopened(*id, why));
            }
            match picture.poll() {
                Some(media::Polled::Opened) => events.push(PictureEvent::Opened(*id)),
                Some(media::Polled::Failed(why)) => {
                    gone.push(*id);
                    events.push(PictureEvent::Failed(*id, why));
                }
                None => {}
            }
        }
        // Let go: its decode thread ends (or, stuck, is left to end by itself).
        for id in gone {
            self.media.remove(&id);
        }
        events
    }

    fn live(&self) -> bool {
        self.windows.is_some()
    }

    fn present_monitor(&mut self, title: &str, layer: &Layer) -> Result<(), String> {
        let gpu = self.gpu.as_mut().map_err(|e| e.clone())?;
        let Some(windows) = self.windows.as_mut() else {
            // Offscreen: drawn only when a probe asks.
            return Ok(());
        };
        if !windows.monitors.contains_key(&layer.stage) {
            let opened = windows.monitors.len();
            let m = monitor::build(windows.mtm, title, (layer.width, layer.height), opened, gpu)?;
            order_front(&m.window);
            windows.monitors.insert(layer.stage, m);
        }
        let m = windows
            .monitors
            .get_mut(&layer.stage)
            .ok_or("monitor lost")?;
        m.set_title(title);
        if !m.ready() {
            return Ok(());
        }
        m.fit();
        // A drawable not there now is a frame skipped, never a monitor closed.
        let Some(drawable) = m.layer().nextDrawable() else {
            return Ok(());
        };
        let device = gpu.device_owned();
        let (media, failed) = (&mut self.media, &mut self.failed);
        let mut drawn = Vec::new();
        let cb = gpu.draw(&drawable.texture(), std::slice::from_ref(layer), &mut |c| {
            drawn.push(c.picture);
            // The frame the displays show at the next refresh.
            frame_of(&device, media, failed, c, media::FRAME_AHEAD, false)
        })?;
        for picture in drawn {
            if let Some(p) = self.media.get_mut(&picture) {
                p.drawn_with(&cb);
            }
        }
        m.drawn_with(&cb);
        let d: &ProtocolObject<dyn MTLDrawable> = ProtocolObject::from_ref(&*drawable);
        cb.presentDrawable(d);
        cb.commit();
        Ok(())
    }

    fn raise_monitor(&mut self, stage: StageId) {
        if let Some(m) = self.windows.as_ref().and_then(|w| w.monitors.get(&stage)) {
            if m.window.isMiniaturized() {
                m.window.deminiaturize(None);
            }
            order_front(&m.window);
        }
    }

    fn close_monitor(&mut self, stage: StageId) {
        if let Some(m) = self
            .windows
            .as_mut()
            .and_then(|w| w.monitors.remove(&stage))
        {
            m.window.close();
        }
    }

    fn stand_back(&mut self, display: Option<&str>) {
        self.standing_back = display.map(str::to_owned);
    }

    fn set_operator(&mut self, pid: Option<u32>) {
        stage_view::set_operator(pid);
    }

    fn keep_keys_with_operator(&mut self) {
        if let Some(windows) = &self.windows {
            stage_view::keep_keys_with_operator(windows.mtm);
        }
    }

    fn menu_bar_display(&self) -> Option<String> {
        #[cfg(feature = "test-support")]
        if self.virtual_displays.is_some() {
            return self.virtual_menu_bar.clone();
        }
        let main = CGMainDisplayID();
        (self.found.iter())
            .find(|f| f.cg == main)
            .map(|f| f.display.id.clone())
    }

    fn take_closed_monitors(&mut self) -> Vec<StageId> {
        let Some(windows) = self.windows.as_mut() else {
            return Vec::new();
        };
        let closed: Vec<StageId> = windows
            .monitors
            .iter()
            .filter(|(_, m)| m.closed())
            .map(|(id, _)| *id)
            .collect();
        for id in &closed {
            windows.monitors.remove(id);
        }
        closed
    }
}

impl Mac {
    /// Test support: the limits on `file`'s open and frames the test gave (each the shipped one
    /// unless given): shorter for the files whose names ask for a fault, so a hung test file
    /// fails quickly, and longer for every other file, so a good file is never failed by a slow
    /// machine (a CI virtual machine whose media framework stalls for seconds).
    #[cfg(feature = "test-support")]
    fn limits_of(&self, file: &str) -> media::Limits {
        if media::fault::named(file) {
            self.fault_limits
        } else {
            self.good_limits
        }
    }

    /// The limits on a file's open and frames: the shipped ones.
    #[cfg(not(feature = "test-support"))]
    fn limits_of(&self, _file: &str) -> media::Limits {
        media::LIMITS
    }
}

/// A cue picture's texture at its position (plus `ahead` seconds), from `media`, if a frame is
/// ready; a failure is noted in `failed` (the host reports it and drops the picture) and
/// nothing is drawn for it. With `wait`, waits for the frame (offscreen probes).
fn frame_of(
    device: &ProtocolObject<dyn MTLDevice>,
    media: &mut HashMap<u64, media::Picture>,
    failed: &mut Vec<PictureEvent>,
    cue: &crate::host::CuePicture,
    ahead: f64,
    wait: bool,
) -> Option<(gpu::Texture, (u32, u32), gpu::Colour)> {
    let source = media.get_mut(&cue.picture)?;
    match source.frame(
        device,
        cue.position_after(ahead),
        cue.pass_after(ahead),
        cue.upcoming(),
        wait,
    ) {
        Ok(frame) => frame,
        Err(why) => {
            if let Some(note) = source.take_reopened() {
                failed.push(PictureEvent::Reopened(cue.picture, note));
            }
            media.remove(&cue.picture);
            failed.push(PictureEvent::Failed(cue.picture, why));
            None
        }
    }
}

/// A borderless, never-key window covering `frame`, with a Metal layer.
fn open_window(mtm: MainThreadMarker, frame: NSRect, gpu: &Gpu) -> Result<DisplayWindow, String> {
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
    Ok(DisplayWindow { window, layer })
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
/// become key or main), at screen-saver level, without a shadow, black. It is a non-activating
/// panel: a click on it is delivered without making the stage host the active app, so the
/// operator's app (active or not) is not deactivated and drawn inactive for a moment, which
/// flashed its windows. Its content is a [`stage_view::StageView`]: every click and the pointer
/// over it stay with it.
fn output_window(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSWindow> {
    // The non-activating style is given at creation: AppKit ignores it when set afterwards.
    let window = NSPanel::initWithContentRect_styleMask_backing_defer(
        mtm.alloc::<NSPanel>(),
        frame,
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        false,
    )
    .into_super();
    // SAFETY: the window is released by our `Retained`, not by AppKit on close.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setLevel(NSScreenSaverWindowLevel);
    window.setCollectionBehavior(output_behaviour());
    // Clicks on a stage never reach an app behind it.
    window.setIgnoresMouseEvents(false);
    window.setHasShadow(false);
    window.setBackgroundColor(Some(&NSColor::blackColor()));
    let view = stage_view::StageView::new(mtm, NSRect::new(NSPoint::new(0.0, 0.0), frame.size));
    window.setContentView(Some(&view));
    window
}

/// An output window's level: screen-saver, over every app's windows, the menu bar and the Dock;
/// or, standing back (a question of the app's is up, or the engine has gone silent), just under
/// every app's normal windows, still over the desktop and its icons, so the operator sees the app
/// while what the stage draws is unchanged.
fn level_for(standing_back: bool) -> NSWindowLevel {
    if standing_back {
        NSNormalWindowLevel - 1
    } else {
        NSScreenSaverWindowLevel
    }
}

/// Orders an output window front, without making it key or activating the stage host.
fn order_front(window: &NSWindow) {
    #[cfg(feature = "test-support")]
    {
        ORDERED_FRONT_AT.with(|l| l.borrow_mut().push(window.level()));
        if NEVER_ORDER_FRONT.with(std::cell::Cell::get) {
            return;
        }
    }
    window.orderFrontRegardless();
}

#[cfg(feature = "test-support")]
thread_local! {
    /// Test support: the level of each output window as it was ordered front.
    static ORDERED_FRONT_AT: RefCell<Vec<isize>> = const { RefCell::new(Vec::new()) };
    /// Test support: set, output windows are presented but never really ordered front, so a
    /// test shows nothing on the machine's displays even when it fails.
    static NEVER_ORDER_FRONT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test support: a stage presented through the shipped `present` on the display with the menu
/// bar (the operator's, a laptop's only one), never ordered front: the levels its window had as
/// it was ordered front, and its level after. `None` without a GPU or a display with the menu
/// bar.
#[cfg(feature = "test-support")]
fn presented_on_main(mtm: MainThreadMarker) -> Option<(Vec<isize>, isize)> {
    let mut mac = Mac {
        gpu: Gpu::new(),
        windows: Some(Windows {
            mtm,
            open: HashMap::new(),
            monitors: HashMap::new(),
        }),
        media: HashMap::new(),
        failed: Vec::new(),
        fault_limits: media::LIMITS,
        good_limits: media::LIMITS,
        found: Vec::new(),
        standing_back: None,
        virtual_displays: None,
        virtual_menu_bar: None,
    };
    mac.displays();
    let main = CGMainDisplayID();
    let display = mac.found.iter().find(|f| f.cg == main)?.display.clone();
    NEVER_ORDER_FRONT.with(|n| n.set(true));
    ORDERED_FRONT_AT.with(|l| l.borrow_mut().clear());
    // A drawable may not come for a window never on screen; the order front comes before it.
    let _ = mac.present(&display, &[]);
    let after = mac
        .windows
        .as_ref()
        .and_then(|w| w.open.get(&display.id))
        .map(|w| w.window.level());
    mac.close(&display.id);
    NEVER_ORDER_FRONT.with(|n| n.set(false));
    let at = ORDERED_FRONT_AT.with(|l| std::mem::take(&mut *l.borrow_mut()));
    Some((at, after?))
}

/// Test support: what an output window built for the main screen's frame is, read back from
/// AppKit. The window is never ordered front, so nothing appears on any display.
#[cfg(feature = "test-support")]
pub struct WindowReport {
    pub level: isize,
    pub screen_saver_level: isize,
    pub normal_level: isize,
    /// A stage presented on the menu bar's display through the shipped path (never really
    /// ordered front): its window's level each time it was ordered front, and after.
    pub presented_on_main: Option<(Vec<isize>, isize)>,
    pub menu_bar_level: isize,
    pub behaviour_as_built: bool,
    pub ignores_mouse: bool,
    /// Its content takes a click even while the stage host is not active.
    pub accepts_first_mouse: bool,
    /// Clicks (left, right, other button) sent to it through AppKit, each landing on the stage
    /// and handing the keys to the operator's app (counted; no app is really activated).
    pub clicks_handed_over: u32,
    /// The point clicked is the stage's own view, not something behind.
    pub click_lands_on_stage: bool,
    pub can_become_key: bool,
    pub can_become_main: bool,
    pub has_shadow: bool,
    pub borderless: bool,
    /// A click on it never makes the stage host the active app.
    pub non_activating: bool,
    pub frame_is_the_screens: bool,
    pub visible: bool,
    /// A monitor window built for a 1920 x 1080 stage titled "Stage 1", never ordered front.
    pub monitor: MonitorReport,
}

/// Test support: a monitor window as AppKit holds it.
#[cfg(feature = "test-support")]
pub struct MonitorReport {
    pub title: String,
    pub titled: bool,
    pub closable: bool,
    pub resizable: bool,
    /// Clicking it never activates the stage host.
    pub non_activating: bool,
    /// It becomes key only for a view that takes typing (it has none).
    pub key_only_if_needed: bool,
    pub hides_on_deactivate: bool,
    pub floating: bool,
    pub level: isize,
    pub content: (f64, f64),
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
    let level = window.level();
    let report = WindowReport {
        level,
        screen_saver_level: NSScreenSaverWindowLevel,
        normal_level: NSNormalWindowLevel,
        presented_on_main: presented_on_main(mtm),
        menu_bar_level: objc2_app_kit::NSMainMenuWindowLevel,
        behaviour_as_built: window.collectionBehavior() == output_behaviour(),
        ignores_mouse: window.ignoresMouseEvents(),
        accepts_first_mouse: window
            .contentView()
            .is_some_and(|v| v.acceptsFirstMouse(None)),
        click_lands_on_stage: window.contentView().is_some_and(|v| {
            let hit = v.hitTest(NSPoint::new(
                frame.size.width / 2.0,
                frame.size.height / 2.0,
            ));
            hit.is_some_and(|h| std::ptr::eq(&*h, &*v))
        }),
        clicks_handed_over: clicks_handed_over(&window),
        can_become_key: window.canBecomeKeyWindow(),
        can_become_main: window.canBecomeMainWindow(),
        has_shadow: window.hasShadow(),
        borderless: window.styleMask() & !NSWindowStyleMask::NonactivatingPanel
            == NSWindowStyleMask::Borderless,
        non_activating: window
            .styleMask()
            .contains(NSWindowStyleMask::NonactivatingPanel),
        frame_is_the_screens: got.origin.x == frame.origin.x
            && got.origin.y == frame.origin.y
            && got.size.width == frame.size.width
            && got.size.height == frame.size.height,
        visible: window.isVisible(),
        monitor: monitor_report(mtm),
    };
    window.close();
    Some(report)
}

/// Test support: a left, a right and an other-button click at the middle of `window`, each given
/// to the view AppKit's hit test finds there (a window never ordered front takes no events from
/// `sendEvent`): how many handed the keys to the operator's app (counted only: nothing is
/// activated).
#[cfg(feature = "test-support")]
fn clicks_handed_over(window: &NSWindow) -> u32 {
    use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
    stage_view::take_handovers(true);
    let size = window.frame().size;
    let at = NSPoint::new(size.width / 2.0, size.height / 2.0);
    let Some(hit) = window.contentView().and_then(|v| v.hitTest(at)) else {
        return stage_view::take_handovers(false);
    };
    for kind in [
        NSEventType::LeftMouseDown,
        NSEventType::RightMouseDown,
        NSEventType::OtherMouseDown,
    ] {
        let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind,
            at,
            NSEventModifierFlags::empty(),
            0.0,
            window.windowNumber(),
            None,
            0,
            1,
            1.0,
        );
        match (kind, event) {
            (NSEventType::LeftMouseDown, Some(e)) => hit.mouseDown(&e),
            (NSEventType::RightMouseDown, Some(e)) => hit.rightMouseDown(&e),
            (_, Some(e)) => hit.otherMouseDown(&e),
            (_, None) => {}
        }
    }
    stage_view::take_handovers(false)
}

/// Test support: a monitor window built as the stage host builds one, read back, never
/// ordered front.
#[cfg(feature = "test-support")]
fn monitor_report(mtm: MainThreadMarker) -> MonitorReport {
    let panel = monitor::panel(mtm, "Stage 1", (1920, 1080));
    let style = panel.styleMask();
    let content = panel.contentRectForFrameRect(panel.frame()).size;
    let report = MonitorReport {
        title: panel.title().to_string(),
        titled: style.contains(NSWindowStyleMask::Titled),
        closable: style.contains(NSWindowStyleMask::Closable),
        resizable: style.contains(NSWindowStyleMask::Resizable),
        non_activating: style.contains(NSWindowStyleMask::NonactivatingPanel),
        key_only_if_needed: panel.becomesKeyOnlyIfNeeded(),
        hides_on_deactivate: panel.hidesOnDeactivate(),
        floating: panel.isFloatingPanel(),
        level: panel.level(),
        content: (content.width, content.height),
        visible: panel.isVisible(),
    };
    panel.close();
    report
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
            monitors: HashMap::new(),
        }),
        media: HashMap::new(),
        failed: Vec::new(),
        #[cfg(feature = "test-support")]
        fault_limits: media::LIMITS,
        #[cfg(feature = "test-support")]
        good_limits: media::LIMITS,
        found: Vec::new(),
        standing_back: None,
        #[cfg(feature = "test-support")]
        virtual_displays: None,
        #[cfg(feature = "test-support")]
        virtual_menu_bar: None,
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
    if std::thread::Builder::new()
        .name("openlab-stage-frames".into())
        .spawn(frames)
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

/// How often playing pictures are drawn again.
const FRAME_EVERY: std::time::Duration = std::time::Duration::from_millis(16);

/// One redraw queued on the main thread at a time.
static REDRAW_QUEUED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The frame loop: asks the main thread to report what became of pictures' files and to draw
/// playing pictures again, about 60 times a second, never queueing a draw behind one not yet
/// done (a slow draw skips frames).
fn frames() {
    use std::sync::atomic::Ordering;
    loop {
        std::thread::sleep(FRAME_EVERY);
        if !REDRAW_QUEUED.swap(true, Ordering::AcqRel) {
            DispatchQueue::main().exec_async(|| {
                REDRAW_QUEUED.store(false, Ordering::Release);
                let mut out = Vec::new();
                HOST.with(|h| {
                    if let Some(host) = h.borrow_mut().as_mut() {
                        host.poll(&mut out);
                        if host.playing() {
                            host.redraw(&mut out);
                        }
                    }
                });
                send(out);
            });
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
                fps: 60,
            })
        })
        .collect()
}

/// Test support: a `--virtual-displays` list with a `*` after one display's size (`V2:1280x720*`)
/// names that display as the one with the menu bar (as `--menu-bar-display` does): the list
/// without the mark, and the display marked.
#[cfg(feature = "test-support")]
fn menu_bar_marked(list: &str) -> (String, Option<String>) {
    let mut marked = None;
    let items: Vec<&str> = list
        .split(',')
        .map(|item| match item.strip_suffix('*') {
            Some(plain) => {
                marked = plain.split_once(':').map(|(id, _)| id.to_owned());
                plain
            }
            None => item,
        })
        .collect();
    (items.join(","), marked)
}

/// Test support: `OPEN_MS,FRAME_MS`, the limits on a picture's open and frames, each at most
/// an hour (so a deadline counted from now never overflows).
#[cfg(feature = "test-support")]
fn media_limits(text: &str) -> Option<media::Limits> {
    const MOST_MS: u64 = 3_600_000;
    let ms = |t: &str| t.parse::<u64>().ok().filter(|ms| *ms <= MOST_MS);
    let (open, frame) = text.split_once(',')?;
    Some(media::Limits {
        open: std::time::Duration::from_millis(ms(open)?),
        frame: std::time::Duration::from_millis(ms(frame)?),
    })
}

/// Test support: what follows `--virtual-displays LIST`, in any order, each at most once.
#[cfg(feature = "test-support")]
#[derive(Debug)]
struct Offscreen {
    /// `--media-limits OPEN_MS,FRAME_MS`: the limits on a file a fault switch names.
    fault_limits: media::Limits,
    /// `--good-media-limits OPEN_MS,FRAME_MS`: the limits on every other file.
    good_limits: media::Limits,
    /// `--media-clock-by-hand`: the limits run on a media clock only the test moves.
    hand_clock: bool,
    /// `--menu-bar-display ID`: the virtual display that stands for the one with the menu bar.
    menu_bar: Option<String>,
    /// `--engine-silence-ms MS`: how long the engine may be silent (shipped `ENGINE_SILENCE`).
    engine_silence: Option<std::time::Duration>,
}

/// Test support: the offscreen options, or `None` for anything unknown, malformed or repeated.
/// Without an option its shipped value stays.
#[cfg(feature = "test-support")]
fn offscreen_options(mut args: &[String]) -> Option<Offscreen> {
    let (mut fault, mut good, mut hand_clock) = (None, None, false);
    let (mut menu_bar, mut engine_silence) = (None, None);
    loop {
        args = match args {
            [] => {
                return Some(Offscreen {
                    fault_limits: fault.unwrap_or(media::LIMITS),
                    good_limits: good.unwrap_or(media::LIMITS),
                    hand_clock,
                    menu_bar,
                    engine_silence,
                });
            }
            [flag, value, rest @ ..] if flag == "--menu-bar-display" && menu_bar.is_none() => {
                menu_bar = Some(value.clone());
                rest
            }
            [flag, value, rest @ ..]
                if flag == "--engine-silence-ms" && engine_silence.is_none() =>
            {
                let ms: u64 = value
                    .parse()
                    .ok()
                    .filter(|ms| (1..=3_600_000).contains(ms))?;
                engine_silence = Some(std::time::Duration::from_millis(ms));
                rest
            }
            [flag, value, rest @ ..] if flag == "--media-limits" && fault.is_none() => {
                fault = Some(media_limits(value)?);
                rest
            }
            [flag, value, rest @ ..] if flag == "--good-media-limits" && good.is_none() => {
                good = Some(media_limits(value)?);
                rest
            }
            [flag, rest @ ..] if flag == "--media-clock-by-hand" && !hand_clock => {
                hand_clock = true;
                rest
            }
            _ => return None,
        };
    }
}

/// Test support: the host on the main thread with no windows, its stages drawn offscreen for
/// probes. A `hang` stops answering but still ends with the engine's pipe.
#[cfg(feature = "test-support")]
fn offscreen(displays: Vec<Display>, options: Offscreen) -> ExitCode {
    let mut host = Host::new(Mac {
        gpu: Gpu::new(),
        windows: None,
        media: HashMap::new(),
        failed: Vec::new(),
        fault_limits: options.fault_limits,
        good_limits: options.good_limits,
        found: Vec::new(),
        standing_back: None,
        virtual_displays: Some(displays),
        virtual_menu_bar: options.menu_bar,
    });
    if let Some(limit) = options.engine_silence {
        host.set_engine_silence(limit);
    }
    crate::host::serve(
        std::io::BufReader::new(std::io::stdin()),
        &mut std::io::stdout().lock(),
        |m, out| host.step(m, out),
    );
    ExitCode::SUCCESS
}

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use super::*;

    #[test]
    fn a_virtual_display_marked_with_a_star_has_the_menu_bar() {
        assert_eq!(
            menu_bar_marked("V1:1920x1080,V2:1280x720*"),
            ("V1:1920x1080,V2:1280x720".to_owned(), Some("V2".to_owned()))
        );
        assert_eq!(menu_bar_marked("V1:64x36"), ("V1:64x36".to_owned(), None));
    }

    #[test]
    fn virtual_displays_are_parsed_or_refused() {
        let d = virtual_displays("A:1920x1080,B:640x480").unwrap();
        assert_eq!(d.len(), 2);
        assert_eq!((d[1].id.as_str(), d[1].width, d[1].height), ("B", 640, 480));
        for bad in ["", "A", "A:1920", "A:x1080", "A:1920x"] {
            assert!(virtual_displays(bad).is_none(), "{bad}");
        }
    }

    /// A file a fault switch names gets the fault limits, every other file the good ones.
    #[test]
    fn fault_files_and_good_files_get_their_own_limits() {
        let ms = |ms| std::time::Duration::from_millis(ms);
        let mac = Mac {
            gpu: Err("no GPU needed".into()),
            windows: None,
            media: HashMap::new(),
            failed: Vec::new(),
            fault_limits: media::Limits {
                open: ms(1),
                frame: ms(2),
            },
            good_limits: media::Limits {
                open: ms(3),
                frame: ms(4),
            },
            found: Vec::new(),
            standing_back: None,
            virtual_displays: None,
            virtual_menu_bar: None,
        };
        let fault = mac.limits_of("/shows/openlab-test-hang-open.mov");
        assert_eq!((fault.open, fault.frame), (ms(1), ms(2)));
        let good = mac.limits_of("/shows/clip.mov");
        assert_eq!((good.open, good.frame), (ms(3), ms(4)));
    }

    fn options(args: &[&str]) -> Option<Offscreen> {
        offscreen_options(&args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn offscreen_options_are_parsed_in_any_order_or_refused() {
        let none = options(&[]).unwrap();
        assert_eq!(none.fault_limits.open, media::LIMITS.open);
        assert_eq!(none.good_limits.frame, media::LIMITS.frame);
        assert!(!none.hand_clock);
        let all = options(&[
            "--good-media-limits",
            "60000,30000",
            "--media-clock-by-hand",
            "--media-limits",
            "400,500",
        ])
        .unwrap();
        assert_eq!(all.good_limits.open.as_millis(), 60_000);
        assert_eq!(all.good_limits.frame.as_millis(), 30_000);
        assert_eq!(all.fault_limits.open.as_millis(), 400);
        assert_eq!(all.fault_limits.frame.as_millis(), 500);
        assert!(all.hand_clock);
        for bad in [
            &["--media-limits"][..],
            &["--media-limits", "400"],
            &["--good-media-limits", "x,1"],
            &["--good-media-limits", "3600001,1"],
            &["--media-limits", "1,1", "--media-limits", "2,2"],
            &["--media-clock-by-hand", "--media-clock-by-hand"],
            &["--other"],
        ] {
            assert!(options(bad).is_none(), "{bad:?}");
        }
    }
}
