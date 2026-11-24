// SPDX-License-Identifier: GPL-3.0-or-later

//! The stage host's state, the same on every platform: which stages and output routes the engine
//! wants, which displays exist, and where each route is. A [`Platform`] does the drawing and the
//! windows.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, Write};
use std::time::Instant;

use openlab_core::stage_link::{
    Display, FromStage, LINK_VERSION, Line, LinkLevel, RouteOutput, RouteState, StageOutput,
    ToStage, decode_to_stage, encode, read_line,
};
use openlab_core::{
    HOLD_BEFORE_END, PictureFades, PictureParam, PictureRamp, PlanPass, RouteId, RouteScaling,
    StageId, VideoGeometry, fade_factor, plan_point,
};

/// What a stage shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Picture {
    /// Nothing plays on it.
    Black,
    /// The test card for a frame number (test support).
    #[cfg(feature = "test-support")]
    TestCard(u16),
}

/// One Video cue's picture as a stage draws it: where in its file it is and how opaque.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CuePicture {
    pub picture: u64,
    /// Seconds into the file (a still ignores it).
    pub position: f64,
    /// Seconds until the pass of its plan under way ends (infinite without one).
    pub left: f64,
    /// Where it goes when that pass ends, when not straight on (a loop's start): the decoder
    /// gets that frame ready while the pass ends ([`PRIME_AHEAD`] before).
    pub next: Option<f64>,
    /// It stands still: paused, loaded, without a plan, or its plan is over (it holds its last
    /// frame). The frame at `position` is the one shown, however far ahead it is asked for.
    pub held: bool,
    /// Its plan ends with the pass under way: nothing past that pass's end is ever shown.
    pub last: bool,
    /// The number of the plan's pass it is in (`PlanPoint::pass`): a new number is a seam.
    pub pass: u64,
    /// Its integrated fades times how far its stop's fade has gone (1 until it stops, then
    /// down to 0, gone). Drawn times the geometry's opacity.
    pub opacity: f32,
    /// Where it sits on its stage, how opaque, its layer and its sampling.
    pub geometry: VideoGeometry,
}

/// How long before a jump in a picture's plan (the end of a loop's pass) its decoder is told
/// where the next pass starts, so it can have that frame ready at the seam.
pub const PRIME_AHEAD: f64 = 1.0;

impl CuePicture {
    /// Where in its file the picture is `ahead` seconds later (a frame shown at the display's
    /// next refresh): past the end of the pass under way, in the next pass; held when its plan
    /// is over, and where that plan will hold when it ends within `ahead` (the frame after its
    /// range is never asked for, so it is neither flashed nor held).
    pub fn position_after(&self, ahead: f64) -> f64 {
        if self.held || ahead <= 0.0 {
            return self.position;
        }
        match self.next {
            Some(next) if ahead >= self.left => next + (ahead - self.left),
            None if self.last => {
                let end = self.position + self.left - HOLD_BEFORE_END;
                (self.position + ahead).min(end).max(self.position)
            }
            _ => self.position + ahead,
        }
    }

    /// The plan's pass the picture is in `ahead` seconds later: the next once the pass under way
    /// ends (never past a plan that is over, or about to be).
    pub fn pass_after(&self, ahead: f64) -> u64 {
        let crosses = !self.held && !self.last && ahead > 0.0 && ahead >= self.left;
        self.pass.saturating_add(u64::from(crosses))
    }

    /// The jump the decoder may get ready for now, if one is near.
    pub fn upcoming(&self) -> Option<f64> {
        self.next.filter(|_| self.left <= PRIME_AHEAD)
    }
}

/// One stage, or one region of it, as a target draws it: the stage's pixel size, what it shows
/// behind its cues, its cues' pictures, the earliest started first (so the later-started is
/// drawn on top), and where on the target the part drawn lands.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub stage: StageId,
    pub width: u32,
    pub height: u32,
    pub picture: Picture,
    pub cues: Vec<CuePicture>,
    pub place: Placement,
}

/// Where a rectangle of a stage lands on a target (QLab 5 manual pp. 229-234): `region` (left,
/// top, width, height in stage pixels) fills a route of `route` pixels, which is turned clockwise
/// by `rotation` degrees and put on the target by `scaling`, centred. What lands outside the
/// target is cut off; the rest of the target is black.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub region: [u32; 4],
    pub route: (u32, u32),
    pub rotation: u16,
    pub scaling: RouteScaling,
    /// The pixel size of the display the route is shown on, which the scaling is worked out
    /// against whatever size the target is drawn at (a probe draws a display smaller); `None`:
    /// the target's own (a monitor window, a stage's own probe).
    pub screen: Option<(u32, u32)>,
}

impl Placement {
    /// A whole stage of `width` x `height` fitted to the target, its shape kept: a monitor
    /// window, or a probe of the stage itself.
    pub fn whole(width: u32, height: u32) -> Self {
        Self {
            region: [0, 0, width, height],
            route: (width, height),
            rotation: 0,
            scaling: RouteScaling::Fit,
            screen: None,
        }
    }

    /// The region's rectangle on a target of `size` pixels: left, top, right, bottom as
    /// fractions of the target from its top left (a scaling that overhangs goes past 0 or 1).
    /// Worked out on the display's own size when it is known, so a target of another size (of
    /// its shape) shows the same.
    pub fn dest(&self, size: (u32, u32)) -> [f64; 4] {
        let (tw, th) = self.screen.unwrap_or(size);
        let (tw, th) = (f64::from(tw.max(1)), f64::from(th.max(1)));
        let (rw, rh) = (
            f64::from(self.route.0.max(1)),
            f64::from(self.route.1.max(1)),
        );
        let (rw, rh) = if matches!(self.rotation, 90 | 270) {
            (rh, rw)
        } else {
            (rw, rh)
        };
        let (w, h) = match self.scaling {
            RouteScaling::Centre => (rw, rh),
            RouteScaling::Fit => {
                let s = (tw / rw).min(th / rh);
                (rw * s, rh * s)
            }
            RouteScaling::Fill => {
                let s = (tw / rw).max(th / rh);
                (rw * s, rh * s)
            }
            RouteScaling::Stretch => (tw, th),
        };
        let (left, top) = ((tw - w) / 2.0, (th - h) / 2.0);
        [left / tw, top / th, (left + w) / tw, (top + h) / th]
    }

    /// Where the stage point `[x, y]` (stage pixels from its top left) lands on a target of
    /// `size` pixels, as fractions of the target from its top left.
    pub fn map(&self, size: (u32, u32), [x, y]: [f64; 2]) -> [f64; 2] {
        let [rx, ry, rw, rh] = self.region.map(f64::from);
        // Where in the route, 0 to 1 from its top left.
        let (u, v) = ((x - rx) / rw.max(1.0), (y - ry) / rh.max(1.0));
        // A quarter turn clockwise takes the route's top edge down the right side.
        let (u, v) = match self.rotation {
            90 => (1.0 - v, u),
            180 => (1.0 - u, 1.0 - v),
            270 => (v, 1.0 - u),
            _ => (u, v),
        };
        let [l, t, r, b] = self.dest(size);
        [l + u * (r - l), t + v * (b - t)]
    }
}

/// A route's alignment guides as a display draws them, over everything on it (QLab 5 manual
/// p. 233): an outline of the route's whole raster, its centre lines, marks at its corners and
/// centre, and its name, in its colour, placed (turned and scaled) as the route's regions are.
#[derive(Clone, Debug, PartialEq)]
pub struct Guide {
    pub route: RouteId,
    pub name: String,
    pub colour: [u8; 3],
    /// The route's whole raster on the display: `region` covers the route.
    pub place: Placement,
}

/// A guide line's thickness, in display pixels per 360 pixels of the display's shorter side
/// (3 on a 1080-line display), at least [`GUIDE_LINE_LEAST`] and at most [`GUIDE_LINE_MOST`].
const GUIDE_LINE_PER: f64 = 360.0;
const GUIDE_LINE_LEAST: f64 = 2.0;
const GUIDE_LINE_MOST: f64 = 8.0;
/// A corner mark's centre, in from the route's corner, as a fraction of its shorter side.
const GUIDE_MARK_IN: f64 = 0.1;
/// A mark's arm, from its centre, as a fraction of the route's shorter side.
const GUIDE_MARK_ARM: f64 = 0.05;
/// The name's height as a fraction of the route's height, and where its centre is (fractions of
/// the route from its top left: in the top left quarter, clear of the centre lines and marks).
const GUIDE_LABEL_HEIGHT: f64 = 0.06;
const GUIDE_LABEL_CENTRE: (f64, f64) = (0.25, 0.25);
/// The widest the name may be, as a fraction of the route's width (within its quarter): a
/// longer one is drawn smaller.
const GUIDE_LABEL_WIDEST: f64 = 0.4;
/// The name's least and greatest height in display pixels.
pub const GUIDE_LABEL_LEAST_PX: u32 = 10;
pub const GUIDE_LABEL_MOST_PX: u32 = 256;

impl Guide {
    /// Route pixels per display pixel across the route's x and its y: the route as turned and
    /// scaled onto the display (its own size when known, else the target's), so lines and the
    /// name are as thick on a probe drawn smaller as on the display.
    fn route_per_pixel(&self, target: (u32, u32)) -> (f64, f64) {
        let (tw, th) = self.place.screen.unwrap_or(target);
        let [l, t, r, b] = self.place.dest(target);
        let (across, down) = ((r - l) * f64::from(tw), (b - t) * f64::from(th));
        // A quarter turn lays the route's x down the display.
        let (along_x, along_y) = if matches!(self.place.rotation, 90 | 270) {
            (down, across)
        } else {
            (across, down)
        };
        let (rw, rh) = (
            f64::from(self.place.route.0.max(1)),
            f64::from(self.place.route.1.max(1)),
        );
        (rw / along_x.max(1e-6), rh / along_y.max(1e-6))
    }

    /// The guide's lines in route pixels (left, top, right, bottom): the outline inside the
    /// route's edge, the two centre lines, and a mark (a cross in a square) at each corner and
    /// at the centre. Each is a line thick on the display (`GUIDE_LINE_*`).
    pub fn bars(&self, target: (u32, u32)) -> Vec<[f64; 4]> {
        let (kx, ky) = self.route_per_pixel(target);
        let (tw, th) = self.place.screen.unwrap_or(target);
        let line =
            (f64::from(tw.min(th)) / GUIDE_LINE_PER).clamp(GUIDE_LINE_LEAST, GUIDE_LINE_MOST);
        // Across x a vertical line is `vx` route pixels thick, across y a horizontal one `hy`.
        let (vx, hy) = (line * kx, line * ky);
        let (rw, rh) = (
            f64::from(self.place.route.0.max(1)),
            f64::from(self.place.route.1.max(1)),
        );
        let (cx, cy) = (rw / 2.0, rh / 2.0);
        let mut bars = vec![
            [0.0, 0.0, rw, hy],
            [0.0, rh - hy, rw, rh],
            [0.0, 0.0, vx, rh],
            [rw - vx, 0.0, rw, rh],
            [0.0, cy - hy / 2.0, rw, cy + hy / 2.0],
            [cx - vx / 2.0, 0.0, cx + vx / 2.0, rh],
        ];
        let short = rw.min(rh);
        let (inset, arm) = (short * GUIDE_MARK_IN, short * GUIDE_MARK_ARM);
        let marks = [
            (inset, inset),
            (rw - inset, inset),
            (inset, rh - inset),
            (rw - inset, rh - inset),
            (cx, cy),
        ];
        for (x, y) in marks {
            // The cross.
            bars.push([x - arm, y - hy / 2.0, x + arm, y + hy / 2.0]);
            bars.push([x - vx / 2.0, y - arm, x + vx / 2.0, y + arm]);
            // The square, half the arm out.
            let half = arm / 2.0;
            bars.push([x - half, y - half, x + half, y - half + hy]);
            bars.push([x - half, y + half - hy, x + half, y + half]);
            bars.push([x - half, y - half, x - half + vx, y + half]);
            bars.push([x + half - vx, y - half, x + half, y + half]);
        }
        bars
    }

    /// The height in display pixels the name is drawn at on a target of `target` pixels.
    pub fn label_height(&self, target: (u32, u32)) -> u32 {
        let (_, ky) = self.route_per_pixel(target);
        let rh = f64::from(self.place.route.1.max(1));
        let px = rh * GUIDE_LABEL_HEIGHT / ky;
        (px.round() as u32).clamp(GUIDE_LABEL_LEAST_PX, GUIDE_LABEL_MOST_PX)
    }

    /// Where the name drawn `size` display pixels (width, height) lands, in route pixels (left,
    /// top, right, bottom): centred on `GUIDE_LABEL_CENTRE`, made smaller if it would be wider
    /// than `GUIDE_LABEL_WIDEST` of the route.
    pub fn label_box(&self, target: (u32, u32), (w, h): (u32, u32)) -> [f64; 4] {
        let (kx, ky) = self.route_per_pixel(target);
        let (rw, rh) = (
            f64::from(self.place.route.0.max(1)),
            f64::from(self.place.route.1.max(1)),
        );
        let (mut lw, mut lh) = (f64::from(w) * kx, f64::from(h) * ky);
        let widest = rw * GUIDE_LABEL_WIDEST;
        if lw > widest {
            lh *= widest / lw;
            lw = widest;
        }
        let (cx, cy) = (rw * GUIDE_LABEL_CENTRE.0, rh * GUIDE_LABEL_CENTRE.1);
        [cx - lw / 2.0, cy - lh / 2.0, cx + lw / 2.0, cy + lh / 2.0]
    }
}

/// Where a picture is in its run.
#[derive(Clone, Debug, PartialEq)]
enum Run {
    /// Loaded at `offset`, not playing yet.
    Held { offset: f64 },
    /// Playing from sample `at`, `offset` seconds into the file, `action` seconds into its
    /// cue's action and `into` seconds into `plan` then; held at `offset` without a plan.
    Playing {
        at: u64,
        offset: f64,
        action: f64,
        plan: Vec<PlanPass>,
        into: f64,
    },
    /// Holding the frame at `offset` from sample `at`, `action` seconds into its cue's action,
    /// in pass `pass` of the plan it was playing (so its decoder sees no seam at the pause).
    Paused {
        at: u64,
        offset: f64,
        action: f64,
        pass: u64,
    },
}

/// Where a run is at a sample: its file position, the pass's time left and next jump, whether
/// its plan is over, and how far into its cue's action it is.
struct Place {
    position: f64,
    left: f64,
    next: Option<f64>,
    held: bool,
    last: bool,
    action: f64,
    pass: u64,
}

/// One Video cue run's picture on the stage host.
#[derive(Clone, Debug)]
struct Pic {
    stage: StageId,
    run: Run,
    /// Where it was before its last change, for the samples before that change is due (a
    /// pause or resume dated a little ahead of the clock).
    before: Option<Run>,
    /// The sample it first played on: drawn from then on.
    since: Option<u64>,
    /// Fading out from sample `.0` over `.1` samples; gone on sample `.2` (the fade's end, or
    /// the stop's cut if that comes first).
    stop: Option<(u64, u64, u64)>,
    /// When it first played, for the stacking (later on top): (sample, arrival order).
    order: (u64, u64),
    geometry: VideoGeometry,
    /// Its integrated fades, as its last play gave them.
    fades: PictureFades,
    /// The geometry values Fade cues move (VID-05), drawn over `geometry`'s: per value, its
    /// ramps by start sample, each in force from its sample until the next one's.
    ramps: BTreeMap<PictureParam, Vec<PictureRamp>>,
}

/// Most ramps of one value kept at once: the one in force and those dated ahead of the clock
/// (a lookahead holds one or two; a hostile engine cannot grow the list past this).
const MAX_RAMPS: usize = 8;

impl Pic {
    /// Takes `ramp` for `param` from its sample on: a ramp dated at or after it is replaced,
    /// and ramps the clock (`clock`) has left behind for a later one are dropped.
    fn ramp(&mut self, param: PictureParam, ramp: PictureRamp, clock: u64) {
        let list = self.ramps.entry(param).or_default();
        list.retain(|r| r.at < ramp.at);
        list.push(ramp);
        // The last one started by the clock is in force; those before it are over.
        let current = list.iter().rposition(|r| r.at <= clock).unwrap_or(0);
        list.drain(..current);
        if list.len() > MAX_RAMPS {
            let over = list.len() - MAX_RAMPS;
            list.drain(..over);
        }
    }

    /// Its geometry on sample `clock`: the values Fade cues move as their ramps have them, the
    /// rest its own.
    fn geometry_at(&self, clock: u64) -> VideoGeometry {
        let mut g = self.geometry;
        for (param, list) in &self.ramps {
            let value = match list.iter().rev().find(|r| r.at <= clock) {
                Some(r) => r.value(clock),
                None => match list.first() {
                    Some(r) => r.from,
                    None => continue,
                },
            };
            param.set(&mut g, value);
        }
        g
    }

    fn place(run: &Run, clock: u64, rate: u32) -> Place {
        // Standing still, it is asked for its own frame whatever the look-ahead: a frame due a
        // refresh later is the same one, and never one past the range it was held in.
        let still = |offset, action, pass| Place {
            position: offset,
            left: f64::INFINITY,
            next: None,
            held: true,
            last: false,
            action,
            pass,
        };
        match run {
            Run::Held { offset } => still(*offset, 0.0, 0),
            Run::Paused {
                offset,
                action,
                pass,
                ..
            } => still(*offset, *action, *pass),
            Run::Playing {
                at,
                offset,
                action,
                plan,
                into,
            } => {
                let elapsed = clock.saturating_sub(*at) as f64 / f64::from(rate.max(1));
                let action = action + elapsed;
                match plan_point(plan, into + elapsed) {
                    Some(p) => Place {
                        position: p.position,
                        left: p.left,
                        next: p.next,
                        held: p.ended,
                        last: p.last,
                        action,
                        pass: p.pass,
                    },
                    // No plan: held where it is (a still has no position; a movie never runs
                    // on past the range it was given).
                    None => still(*offset, action, 0),
                }
            }
        }
    }

    /// What is drawn at sample `clock`, if anything.
    fn at(&self, picture: u64, clock: u64, rate: u32) -> Option<CuePicture> {
        let since = self.since?;
        if clock < since {
            return None;
        }
        let due = match self.run {
            Run::Held { .. } => true,
            Run::Playing { at, .. } | Run::Paused { at, .. } => clock >= at,
        };
        let run = if due {
            &self.run
        } else {
            self.before.as_ref().unwrap_or(&self.run)
        };
        let stopping = match self.stop {
            Some((_, _, end)) if clock >= end => return None,
            Some((at, _, _)) if clock < at => 1.0,
            Some((at, fade, _)) => {
                let gone = clock - at;
                if gone >= fade {
                    return None;
                }
                1.0 - gone as f32 / fade as f32
            }
            None => 1.0,
        };
        let place = Self::place(run, clock, rate);
        let faded = fade_factor(&self.fades, place.action) as f32;
        Some(CuePicture {
            picture,
            position: place.position,
            left: place.left,
            next: place.next,
            held: place.held,
            last: place.last,
            pass: place.pass,
            opacity: stopping * faded,
            geometry: self.geometry_at(clock),
        })
    }
}

/// What became of one display's frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presented {
    Drawn,
    /// The window is up but could not take this frame (the system gave no drawable for it in
    /// time, as under heavy GPU load): it keeps showing what it showed.
    Skipped,
}

/// The windows and the drawing of one platform.
pub trait Platform {
    /// Whether this build shows video at all.
    fn video(&self) -> bool;
    /// The displays connected now.
    fn displays(&mut self) -> Vec<Display>;
    /// Draws `layers` (bottom first, each placed by its route) and over them `guides` on
    /// `display`, opening or moving its window as needed. An error: the display's routes cannot
    /// be drawn.
    fn present(
        &mut self,
        display: &Display,
        layers: &[Layer],
        guides: &[Guide],
    ) -> Result<Presented, String>;
    /// Takes the window of display `display` down, if it has one.
    fn close(&mut self, display: &str);
    /// Draws `layers` (bottom first) and over them `guides` on a target of `size` pixels and
    /// reads the colour (RGBA) at each point (fractions of the target, from the top left) back
    /// from the GPU.
    #[cfg(feature = "test-support")]
    fn probe(
        &mut self,
        size: (u32, u32),
        layers: &[Layer],
        guides: &[Guide],
        points: &[[f32; 2]],
    ) -> Result<Vec<[u8; 4]>, String>;
    /// Test support: moves the media clock on (`--media-clock-by-hand`).
    #[cfg(feature = "test-support")]
    fn advance_media_clock(&mut self, _ms: u64) {}
    /// Test support: replaces virtual displays; `false` where the displays are real.
    #[cfg(feature = "test-support")]
    fn set_virtual_displays(&mut self, _displays: Vec<Display>) -> bool {
        false
    }
    /// Starts opening a Video cue's file for `picture` (a video, or a still with `still`), to
    /// be shown from `offset` seconds; the open never blocks the caller: its end is a
    /// [`PictureEvent`]. An error here (nothing was started) fails the picture at once.
    fn open_picture(
        &mut self,
        _picture: u64,
        _file: &str,
        _still: bool,
        _offset: f64,
    ) -> Result<(), String> {
        Err("this computer's openLab has no video output".into())
    }
    /// Lets the picture's file go.
    fn close_picture(&mut self, _picture: u64) {}
    /// What happened to pictures since the last call: opened, opened again after its reader
    /// failed, or failed (could not be opened, decoded or drawn, or took too long) with why.
    fn take_picture_events(&mut self) -> Vec<PictureEvent> {
        Vec::new()
    }
    /// Whether the engine's clock runs on between its messages (windows on real displays):
    /// offscreen, a picture is drawn exactly at the last clock the engine sent.
    fn live(&self) -> bool {
        false
    }
    /// Draws `layer` (one stage) fitted to its monitor window titled `title`, black bars
    /// where the shapes differ, opening the window (at the stage's shape) if it is not open.
    /// Called after every display has been drawn; a monitor never holds the displays up, so
    /// one that cannot take a frame now skips it.
    fn present_monitor(&mut self, _title: &str, _layer: &Layer) -> Result<(), String> {
        Ok(())
    }
    /// Brings the stage's monitor window to the front, if it is open.
    fn raise_monitor(&mut self, _stage: StageId) {}
    /// Takes the stage's monitor window down, if it has one.
    fn close_monitor(&mut self, _stage: StageId) {}
    /// The stages whose monitor window the operator closed since the last call.
    fn take_closed_monitors(&mut self) -> Vec<StageId> {
        Vec::new()
    }
    /// The display with the menu bar (the operator's; a laptop's only one), if it is one of
    /// the displays listed.
    fn menu_bar_display(&self) -> Option<String> {
        None
    }
    /// The window on `display` (none: on no display) stands back under every app's windows,
    /// over the desktop, from its next present; every other window covers its display.
    fn stand_back(&mut self, _display: Option<&str>) {}
    /// The operator's app, by process id (none known: `None`): a click on a stage brings it
    /// forward with the keys.
    fn set_operator(&mut self, _pid: Option<u32>) {}
    /// Called from every poll: the stage host never keeps the keys. If something made it the
    /// active app (a click on a stage, the system at its start), the operator's app gets them back.
    fn keep_keys_with_operator(&mut self) {}
}

/// How long the stage host may hear nothing at all from the engine (it pings every second while
/// its control thread runs, and sends the clock while pictures play) before the window on the
/// display with the menu bar stands back under the app's windows: four missed pings, so one
/// late or slow tick never does, and the operator of a hung engine sees the app again within
/// about five seconds.
pub const ENGINE_SILENCE: std::time::Duration = std::time::Duration::from_secs(4);
/// How long a display may give no drawable, frame after frame, before its stages fail: three of
/// macOS's one-second drawable timeouts, past a burst of GPU load, while a frozen picture on the
/// audience's display is still told to the engine and the operator within seconds.
pub const SKIP_LIMIT: std::time::Duration = std::time::Duration::from_secs(3);
/// How long after its stages failed for lack of drawables a display is tried again (and again
/// after each try that finds none): a try with no drawable can hold the stage host up to
/// macOS's one-second drawable timeout, stalling every other display, so it is not made every
/// frame; every 3 s holds the others at most a second in four while the display is dead, and
/// brings a keep-black stage back within a few seconds of drawables returning with nothing
/// else to draw it.
pub const SKIP_RETRY: std::time::Duration = std::time::Duration::from_secs(3);

/// One stage's monitor window as the engine asked for it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Monitor {
    title: String,
    /// The operator closed it: it stays closed until the engine raises it again or drops it.
    closed: bool,
}

/// How far past the last clock message the stage host moves the clock on by itself: three of
/// the engine's clock periods (it tells the clock at least every 10 ms), so one late tick does
/// not stop a picture.
const CLOCK_HOLD: std::time::Duration = std::time::Duration::from_millis(30);

/// What became of a picture's file on the platform.
#[derive(Clone, Debug, PartialEq)]
pub enum PictureEvent {
    Opened(u64),
    /// Its movie's reader failed and the movie was opened again, and why: it plays on.
    Reopened(u64, String),
    Failed(u64, String),
}

/// The engine's sample clock as last told.
#[derive(Clone, Copy, Debug)]
struct Clock {
    sample: u64,
    rate: u32,
    told: Instant,
}

/// What the host loop does after a message.
#[derive(Debug, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
    /// Test support: stop answering, as a hung stage host would.
    #[cfg(feature = "test-support")]
    Hang,
}

pub struct Host<P: Platform> {
    platform: P,
    stages: Vec<StageOutput>,
    routes: Vec<RouteOutput>,
    displays: Vec<Display>,
    states: HashMap<RouteId, RouteState>,
    pictures: HashMap<StageId, Picture>,
    /// Video cues' pictures, by the engine's id.
    pics: BTreeMap<u64, Pic>,
    /// Plays seen, for the stacking order of pictures started on one sample.
    plays: u64,
    clock: Option<Clock>,
    /// Displays with a window up.
    presented: BTreeSet<String>,
    /// Displays whose last frame was skipped, with when the stretch of skips began (warned once
    /// for the stretch).
    skipping: BTreeMap<String, Instant>,
    /// Displays whose stages failed for lack of drawables ([`SKIP_LIMIT`]), with when they were
    /// last tried: not presented again until [`SKIP_RETRY`] after that, when the next poll tries
    /// once (a display change tries at once).
    starved: BTreeMap<String, Instant>,
    /// When the engine last sent anything.
    heard: Instant,
    /// The engine has been silent past `silence_limit`: the window on the display with the menu
    /// bar stands back, so a hung engine never locks the operator out of their own display.
    silent: bool,
    /// The app asked the window on the display with the menu bar to stand back (a question of
    /// its own is up).
    stand_back: bool,
    silence_limit: std::time::Duration,
    /// An app is connected to the engine (`ToStage::App`; until the engine says otherwise, one
    /// is taken to be). While none is, every output window is down: no stage outlives the app.
    app_connected: bool,
    /// A failed picture was dropped since the stages were last drawn: the next poll draws them
    /// again, so its last frame never stays in a window.
    redraw_due: bool,
    /// Monitor windows the engine asked for, by stage.
    monitors: BTreeMap<StageId, Monitor>,
}

impl<P: Platform> Host<P> {
    pub fn new(platform: P) -> Self {
        Self {
            platform,
            stages: Vec::new(),
            routes: Vec::new(),
            displays: Vec::new(),
            states: HashMap::new(),
            pictures: HashMap::new(),
            pics: BTreeMap::new(),
            plays: 0,
            clock: None,
            presented: BTreeSet::new(),
            skipping: BTreeMap::new(),
            starved: BTreeMap::new(),
            heard: Instant::now(),
            silent: false,
            stand_back: false,
            silence_limit: ENGINE_SILENCE,
            app_connected: true,
            redraw_due: false,
            monitors: BTreeMap::new(),
        }
    }

    /// The engine's sample now, and its rate: the last clock told, moved on by this process's
    /// own clock since when the platform is live, by at most `CLOCK_HOLD`. The engine tells the
    /// clock at least every 10 ms while a picture is up; when it stops (no output device
    /// running, so the show's clock stands still, or a stalled engine) the picture stops with
    /// it instead of playing on and jumping back when the clock moves again.
    fn now(&self) -> (u64, u32) {
        self.now_after(self.clock.map(|c| c.told.elapsed()))
    }

    /// [`Self::now`] when `since` has passed since the last clock message.
    fn now_after(&self, since: Option<std::time::Duration>) -> (u64, u32) {
        match self.clock {
            None => (0, 1),
            Some(c) if self.platform.live() => {
                let since = since.unwrap_or_default().min(CLOCK_HOLD);
                let ran = since.as_secs_f64() * f64::from(c.rate);
                (c.sample.saturating_add(ran as u64), c.rate)
            }
            Some(c) => (c.sample, c.rate),
        }
    }

    /// Whether any picture is up or due: the windows are drawn again as the clock moves.
    pub fn playing(&self) -> bool {
        !self.pics.is_empty()
    }

    /// Draws again (the clock moved) and drops pictures whose fade is over; reports any that
    /// failed.
    pub fn redraw(&mut self, out: &mut Vec<FromStage>) {
        if self.pics.is_empty() && !self.redraw_due {
            return;
        }
        self.apply(out);
    }

    fn picture_message(&mut self, message: ToStage, out: &mut Vec<FromStage>) {
        let (sample, rate) = self.now();
        match message {
            ToStage::Clock { sample, rate } => {
                // Drawn by the platform's own frame loop (or a probe), not on every clock.
                self.clock = Some(Clock {
                    sample,
                    rate,
                    told: Instant::now(),
                });
                return;
            }
            ToStage::Load {
                picture,
                stage,
                file,
                still,
                offset,
                geometry,
            } => {
                self.drop_picture(picture);
                if !self.stages.iter().any(|s| s.id == stage) {
                    out.push(FromStage::PictureFailed {
                        picture,
                        reason: "its stage is not there".into(),
                    });
                } else if self.pics.len() >= openlab_core::stage_link::MAX_PICTURES {
                    out.push(FromStage::PictureFailed {
                        picture,
                        reason: "too many pictures at once".into(),
                    });
                } else {
                    match self.platform.open_picture(picture, &file, still, offset) {
                        Ok(()) => {
                            self.pics.insert(
                                picture,
                                Pic {
                                    stage,
                                    run: Run::Held { offset },
                                    before: None,
                                    since: None,
                                    stop: None,
                                    order: (u64::MAX, 0),
                                    geometry,
                                    fades: PictureFades::NONE,
                                    ramps: BTreeMap::new(),
                                },
                            );
                        }
                        Err(reason) => out.push(FromStage::PictureFailed {
                            picture,
                            reason: short(reason),
                        }),
                    }
                }
            }
            ToStage::Play {
                picture,
                at,
                offset,
                action,
                plan,
                into,
                fades,
            } => {
                self.plays += 1;
                let plays = self.plays;
                if let Some(p) = self.pics.get_mut(&picture) {
                    let run = Run::Playing {
                        at,
                        offset,
                        action,
                        plan,
                        into,
                    };
                    let was = std::mem::replace(&mut p.run, run);
                    p.before = p.since.is_some().then_some(was);
                    // Taken at once: a play dated ahead (a devamp's continuation) changes the
                    // fades only where the run before it ends anyway.
                    p.fades = fades;
                    if p.since.is_none() {
                        p.since = Some(at);
                        p.order = (at, plays);
                    }
                }
            }
            ToStage::Pause {
                picture,
                at,
                offset,
                action,
            } => {
                if let Some(p) = self.pics.get_mut(&picture) {
                    // The pass it pauses in, so its decoder holds the frame shown rather than
                    // reading again as at a loop's seam.
                    let pass = Pic::place(&p.run, at, rate).pass;
                    let paused = Run::Paused {
                        at,
                        offset,
                        action,
                        pass,
                    };
                    p.before = Some(std::mem::replace(&mut p.run, paused));
                }
            }
            ToStage::Stop {
                picture,
                at,
                fade,
                cut,
            } => {
                let end = at.saturating_add(fade).min(cut.unwrap_or(u64::MAX));
                let gone = match self.pics.get_mut(&picture) {
                    // Never shown, or a stop that ends by now: gone now.
                    Some(p) if p.since.is_none() || end <= sample => true,
                    Some(p) => {
                        p.stop = Some((at, fade, end));
                        false
                    }
                    None => false,
                };
                if gone {
                    self.drop_picture(picture);
                }
            }
            ToStage::Unload { picture } => self.drop_picture(picture),
            // Drawn so from the next frame; its run (and the decoder) are untouched.
            ToStage::Geometry { picture, geometry } => match self.pics.get_mut(&picture) {
                Some(p) => p.geometry = geometry,
                None => return,
            },
            ToStage::Ramp {
                picture,
                param,
                ramp,
            } => match self.pics.get_mut(&picture) {
                Some(p) => p.ramp(param, ramp, sample),
                None => return,
            },
            _ => return,
        }
        self.apply(out);
    }

    /// Reports the pictures the platform opened, or could not open, decode or draw, since it
    /// was last asked, and drops the failed ones.
    fn report_failures(&mut self, out: &mut Vec<FromStage>) {
        for event in self.platform.take_picture_events() {
            match event {
                PictureEvent::Opened(picture) if self.pics.contains_key(&picture) => {
                    out.push(FromStage::PictureOpened { picture });
                }
                PictureEvent::Opened(_) => {}
                // One line in the show log; the cue is not told, as it plays on.
                PictureEvent::Reopened(_, why) => out.push(FromStage::Log {
                    level: LinkLevel::Warn,
                    message: short(why),
                }),
                PictureEvent::Failed(picture, reason) => {
                    self.redraw_due |= self.pics.contains_key(&picture);
                    self.drop_picture(picture);
                    out.push(FromStage::PictureFailed {
                        picture,
                        reason: short(reason),
                    });
                }
            }
        }
    }

    /// Between messages: reports what became of pictures' files (an open that ended, a decode
    /// that failed or fell behind) and the monitor windows the operator closed.
    pub fn poll(&mut self, out: &mut Vec<FromStage>) {
        let now = Instant::now();
        self.platform.keep_keys_with_operator();
        self.check_silence(now, out);
        self.report_failures(out);
        // A picture that failed leaves its display now, whether or not others still play:
        // nothing else would draw a stage left with none. A display starved of drawables is
        // tried again when due, likewise.
        let retry =
            (self.starved.values()).any(|&t| now.saturating_duration_since(t) >= SKIP_RETRY);
        if self.redraw_due || retry {
            self.apply(out);
        }
        for stage in self.platform.take_closed_monitors() {
            self.monitor_closed(stage, out);
        }
    }

    /// The stage's monitor window is gone (the operator closed it, or it could not be drawn):
    /// the engine is told, and it stays closed until the engine raises it again.
    fn monitor_closed(&mut self, stage: StageId, out: &mut Vec<FromStage>) {
        if let Some(m) = self.monitors.get_mut(&stage)
            && !m.closed
        {
            m.closed = true;
            self.platform.close_monitor(stage);
            out.push(FromStage::MonitorClosed { stage });
        }
    }

    /// Follows a new stage set's monitors: one no longer asked for goes, a new one opens, a
    /// title follows its stage's name. One the operator closed stays closed.
    fn follow_monitors(&mut self, stages: &[StageOutput]) {
        let wanted: BTreeMap<StageId, &str> = stages
            .iter()
            .filter_map(|s| Some((s.id, s.monitor.as_deref()?)))
            .collect();
        let gone: Vec<StageId> = self
            .monitors
            .keys()
            .filter(|id| !wanted.contains_key(id))
            .copied()
            .collect();
        for stage in gone {
            self.monitors.remove(&stage);
            self.platform.close_monitor(stage);
        }
        for (stage, title) in wanted {
            let m = self.monitors.entry(stage).or_insert_with(|| Monitor {
                title: String::new(),
                closed: false,
            });
            title.clone_into(&mut m.title);
        }
    }

    /// One message from [`serve`], or (`None`) a poll between messages, which also draws
    /// playing pictures again as a display's frame loop does: a picture whose stop is over
    /// goes, and a stage left with nothing to show is hidden or kept black.
    pub fn step(&mut self, message: Option<ToStage>, out: &mut Vec<FromStage>) -> Flow {
        match message {
            Some(message) => self.handle(message, out),
            None => {
                self.poll(out);
                self.redraw(out);
                Flow::Continue
            }
        }
    }

    fn drop_picture(&mut self, picture: u64) {
        if self.pics.remove(&picture).is_some() {
            self.platform.close_picture(picture);
        }
    }

    /// The pictures drawn on `stage` at `clock`, bottom first: the lowest layer first, and on
    /// one layer the earliest started first (QLab 5 manual p. 213).
    fn cues_on(&self, stage: StageId, (clock, rate): (u64, u32)) -> Vec<CuePicture> {
        let mut on: Vec<((u16, u64, u64), CuePicture)> = self
            .pics
            .iter()
            .filter(|(_, p)| p.stage == stage)
            .filter_map(|(id, p)| {
                let order = (p.geometry.layer, p.order.0, p.order.1);
                Some((order, p.at(*id, clock, rate)?))
            })
            .collect();
        on.sort_by_key(|(order, _)| *order);
        on.into_iter().map(|(_, c)| c).collect()
    }

    /// Drops the pictures whose fade-out is over at `clock`.
    fn prune(&mut self, (clock, _): (u64, u32)) {
        let done: Vec<u64> = self
            .pics
            .iter()
            .filter(|(_, p)| p.stop.is_some_and(|(_, _, end)| clock >= end))
            .map(|(id, _)| *id)
            .collect();
        for id in done {
            self.drop_picture(id);
        }
    }

    pub fn platform_mut(&mut self) -> &mut P {
        &mut self.platform
    }

    /// Test support: how long the engine may be silent (`--engine-silence-ms`).
    #[cfg(feature = "test-support")]
    pub fn set_engine_silence(&mut self, limit: std::time::Duration) {
        self.silence_limit = limit;
    }

    /// Stands the window on the display with the menu bar back once the engine has said nothing
    /// for `silence_limit` at `now` (a hung engine: its pings stop when its control thread
    /// does). What every display shows is unchanged.
    pub fn check_silence(&mut self, now: Instant, out: &mut Vec<FromStage>) {
        if self.silent || now.saturating_duration_since(self.heard) < self.silence_limit {
            return;
        }
        self.silent = true;
        if let Some(display) = self.platform.menu_bar_display() {
            out.push(FromStage::Log {
                level: LinkLevel::Warn,
                message: format!(
                    "the engine has not answered for {} s: the stage window on the display with the menu bar ({display}) stands back under the app's windows until it does",
                    self.silence_limit.as_secs_f64()
                ),
            });
        }
        self.apply(out);
    }

    /// Handles one message from the engine; the answers go to `out`. The engine is heard: a
    /// window standing back for its silence covers its display again.
    pub fn handle(&mut self, message: ToStage, out: &mut Vec<FromStage>) -> Flow {
        self.heard = Instant::now();
        let back = std::mem::take(&mut self.silent);
        let flow = self.handle_message(message, out);
        if back && flow == Flow::Continue {
            self.apply(out);
        }
        flow
    }

    fn handle_message(&mut self, message: ToStage, out: &mut Vec<FromStage>) -> Flow {
        match message {
            ToStage::Hello { version } => {
                out.push(FromStage::Hello {
                    version: LINK_VERSION,
                    pid: std::process::id(),
                    video: self.platform.video(),
                });
                if version != LINK_VERSION {
                    return Flow::Quit;
                }
                self.displays_changed(out);
            }
            ToStage::Stages { stages, routes } => {
                self.pictures
                    .retain(|id, _| stages.iter().any(|s| s.id == *id));
                let gone: Vec<u64> = self
                    .pics
                    .iter()
                    .filter(|(_, p)| !stages.iter().any(|s| s.id == p.stage))
                    .map(|(id, _)| *id)
                    .collect();
                // Its cue cannot be seen any more: it breaks, as a decode failure does.
                for picture in gone {
                    self.drop_picture(picture);
                    out.push(FromStage::PictureFailed {
                        picture,
                        reason: "its stage was removed".into(),
                    });
                }
                self.follow_monitors(&stages);
                self.stages = stages;
                self.routes = routes;
                // Every route's state is reported after a new set, changed or not, so the
                // engine knows each route it sent has been dealt with.
                self.states.clear();
                self.apply(out);
            }
            ToStage::Ping { seq } => out.push(FromStage::Pong { seq }),
            ToStage::Quit => {
                for display in std::mem::take(&mut self.presented) {
                    self.platform.close(&display);
                }
                for stage in std::mem::take(&mut self.monitors).into_keys() {
                    self.platform.close_monitor(stage);
                }
                return Flow::Quit;
            }
            ToStage::StandBack { on } => {
                if std::mem::replace(&mut self.stand_back, on) != on
                    && let Some(display) = self.platform.menu_bar_display()
                {
                    out.push(FromStage::Log {
                        level: LinkLevel::Info,
                        message: if on {
                            format!(
                                "the stage window on {display} stands back for the app's question"
                            )
                        } else {
                            format!("the stage window on {display} covers its display again")
                        },
                    });
                }
                self.apply(out);
            }
            ToStage::App { connected, pid } => {
                self.platform.set_operator(pid);
                if std::mem::replace(&mut self.app_connected, connected) != connected {
                    out.push(FromStage::Log {
                        level: LinkLevel::Info,
                        message: if connected {
                            "the app is back: the stage windows are up again".into()
                        } else {
                            "the app is gone: every stage window is down until it is back".into()
                        },
                    });
                    self.apply(out);
                }
            }
            ToStage::RaiseMonitor { stage } => {
                if let Some(m) = self.monitors.get_mut(&stage) {
                    // Closed by the operator: drawn (so opened) again by the next apply.
                    m.closed = false;
                    self.platform.raise_monitor(stage);
                    self.apply(out);
                }
            }
            #[cfg(feature = "test-support")]
            ToStage::CloseMonitorWindow { stage } => self.monitor_closed(stage, out),
            #[cfg(feature = "test-support")]
            ToStage::ProbeMonitor {
                stage,
                width,
                height,
                points,
            } => {
                self.redraw(out);
                let probed = self.probe_monitor(stage, (width, height), &points, out);
                self.report_failures(out);
                out.push(probed);
            }
            #[cfg(feature = "test-support")]
            ToStage::TestCard { stage, frame } => {
                if self.stages.iter().any(|s| s.id == stage) {
                    self.pictures.insert(stage, Picture::TestCard(frame));
                    self.apply(out);
                }
            }
            #[cfg(feature = "test-support")]
            ToStage::Probe { stage, points } => {
                // As a frame drawn now would: pictures whose stop is over go and the stages'
                // states follow, reported ahead of the answer.
                self.redraw(out);
                let probed = self.probe_stage(stage, &points, out);
                // A picture that failed as the probe drew it is reported first, so the engine
                // has it (and a test that finds nothing on a stage can say why) once it has the
                // probe's answer.
                self.report_failures(out);
                out.push(probed);
            }
            #[cfg(feature = "test-support")]
            ToStage::ProbeDisplay { display, points } => {
                // As a frame drawn now would: pictures whose stop is over go and the stages'
                // states follow, reported ahead of the answer.
                self.redraw(out);
                let probed = self.probe_display(display, &points, out);
                // A picture that failed as the probe drew it is reported first, so the engine
                // has it (and a test that finds nothing on a stage can say why) once it has the
                // probe's answer.
                self.report_failures(out);
                out.push(probed);
            }
            #[cfg(feature = "test-support")]
            ToStage::Hang => return Flow::Hang,
            #[cfg(feature = "test-support")]
            ToStage::AdvanceMediaClock { ms } => {
                self.platform.advance_media_clock(ms);
                self.report_failures(out);
            }
            #[cfg(feature = "test-support")]
            ToStage::SetDisplays { displays } => {
                if self.platform.set_virtual_displays(displays) {
                    self.displays_changed(out);
                }
            }
            other => self.picture_message(other, out),
        }
        Flow::Continue
    }

    /// Reads the displays again and puts every stage where it now belongs (a display came or
    /// went, or its size changed).
    pub fn displays_changed(&mut self, out: &mut Vec<FromStage>) {
        let mut displays = self.platform.displays();
        displays.truncate(openlab_core::stage_link::MAX_DISPLAYS);
        // The main display is the one with the menu bar, else the first listed.
        let main = self.platform.menu_bar_display();
        let main = main
            .filter(|m| displays.iter().any(|d| &d.id == m))
            .or_else(|| displays.first().map(|d| d.id.clone()));
        for d in &mut displays {
            d.main = main.as_deref() == Some(d.id.as_str());
        }
        // The system's displays changed: one starved of drawables is tried again at once.
        self.starved.clear();
        if displays != self.displays {
            self.displays = displays.clone();
            out.push(FromStage::Displays { displays });
        }
        self.apply(out);
    }

    /// `display` could not take this frame. Its window stays up with its last frame and the next
    /// poll draws again (nothing else might: no picture may be playing), warning once per
    /// stretch so a GPU under load does not flood the show log. Past [`SKIP_LIMIT`] of skips
    /// the display counts as failed.
    fn skipped(&mut self, display: &str, out: &mut Vec<FromStage>) -> Result<(), String> {
        let now = Instant::now();
        let began = *self.skipping.entry(display.to_owned()).or_insert_with(|| {
            out.push(FromStage::Log {
                level: LinkLevel::Warn,
                message: short(format!(
                    "display {display} skipped frames: the system gave no picture to draw into"
                )),
            });
            now
        });
        if now.saturating_duration_since(began) >= SKIP_LIMIT {
            self.starved.insert(display.to_owned(), now);
            return Err(starved_reason());
        }
        self.redraw_due = true;
        Ok(())
    }

    /// What every output would show at `now`: each route's state, the layers each display draws
    /// (bottom first: by stage stack, then region order) with the routes drawn there, and each
    /// open monitor's title and layer.
    fn plan(&self, now: (u64, u32)) -> Plan {
        let mut plan = Plan::default();
        // Each route's regions with something to draw, found in one pass over the stages.
        let mut parts: HashMap<RouteId, Vec<StagePart<'_>>> = HashMap::new();
        for stage in &self.stages {
            let on = self.cues_on(stage.id, now);
            let picture = self.picture(stage.id);
            if let Some(m) = self.monitors.get(&stage.id).filter(|m| !m.closed)
                && self.platform.video()
                && self.app_connected
            {
                let layer = layer_of(stage, picture, on.clone(), None);
                plan.watched.push((m.title.clone(), layer));
            }
            // Off, a stage with nothing to show draws nothing: the desktop shows there.
            if !stage.keep_black && picture == Picture::Black && on.is_empty() {
                continue;
            }
            for (index, region) in stage.regions.iter().enumerate() {
                parts
                    .entry(region.route)
                    .or_default()
                    .push((index, stage, picture, on.clone()));
            }
        }
        for route in &self.routes {
            let screen = (self.displays.iter())
                .find(|d| Some(&d.id) == route.display.as_ref())
                .map(|d| (d.width, d.height));
            let mut layers: Vec<Drawn> = Vec::new();
            for (index, stage, picture, on) in parts.remove(&route.id).unwrap_or_default() {
                if let Some(region) = stage.regions.get(index) {
                    let layer = layer_of(stage, picture, on, Some((region, route, screen)));
                    layers.push(((stage.stack, index), route.id, layer));
                }
            }
            let guide = route.guide.as_ref().map(|g| Guide {
                route: route.id,
                name: g.name.clone(),
                colour: g.colour.0,
                place: Placement {
                    region: [0, 0, route.width, route.height],
                    route: (route.width, route.height),
                    rotation: route.rotation,
                    scaling: route.scaling,
                    screen,
                },
            });
            let state = match &route.display {
                _ if !self.platform.video() => RouteState::Failed {
                    reason: "this computer's openLab has no video output".into(),
                },
                None => RouteState::NoDisplay,
                Some(id) if !self.displays.iter().any(|d| &d.id == id) => {
                    RouteState::DisplayMissing {
                        display: id.clone(),
                    }
                }
                Some(_) if layers.is_empty() && guide.is_none() => RouteState::Hidden,
                // No app: nothing is shown, whatever plays (the cues play on unseen).
                Some(_) if !self.app_connected => RouteState::Hidden,
                Some(id) => {
                    // A route showing only its guides still has its display's window up.
                    plan.on.entry(id.clone()).or_default().extend(layers);
                    if let Some(guide) = guide {
                        plan.guides.entry(id.clone()).or_default().push(guide);
                    }
                    RouteState::Shown {
                        display: id.clone(),
                    }
                }
            };
            plan.states.push((route.id, state));
        }
        for on in plan.on.values_mut() {
            on.sort_by_key(|(order, ..)| *order);
        }
        plan
    }

    /// Shows, hides or redraws each route and reports each state that changed. Every region
    /// shown on one display is drawn in one window, bottom first by its stage's `stack`, then
    /// by region order.
    fn apply(&mut self, out: &mut Vec<FromStage>) {
        self.redraw_due = false;
        let now = self.now();
        self.prune(now);
        let Plan {
            mut states,
            mut on,
            mut guides,
            watched,
        } = self.plan(now);
        // A question of the app's, or a hung engine: the operator's display's window stands back.
        let back = (self.stand_back || self.silent)
            .then(|| self.platform.menu_bar_display())
            .flatten();
        self.platform.stand_back(back.as_deref());
        // A window closed since starts a new stretch.
        let presented = &self.presented;
        self.skipping.retain(|d, _| presented.contains(d));
        let instant = Instant::now();
        for display in self.displays.clone() {
            let Some(drawn) = on.remove(&display.id) else {
                self.starved.remove(&display.id);
                if self.presented.remove(&display.id) {
                    self.platform.close(&display.id);
                }
                continue;
            };
            let guides = guides.remove(&display.id).unwrap_or_default();
            let routes: BTreeSet<RouteId> = (drawn.iter().map(|(_, r, _)| *r))
                .chain(guides.iter().map(|g| g.route))
                .collect();
            let layers: Vec<Layer> = drawn.into_iter().map(|(.., l)| l).collect();
            let tried = self.starved.get(&display.id).copied();
            let presented = match tried {
                // Starved of drawables, not due a try: still failed, nothing presented.
                Some(t) if instant.saturating_duration_since(t) < SKIP_RETRY => {
                    Err(starved_reason())
                }
                _ => match self.platform.present(&display, &layers, &guides) {
                    Ok(Presented::Drawn) => {
                        self.skipping.remove(&display.id);
                        self.starved.remove(&display.id);
                        Ok(())
                    }
                    // A try that finds none fails it again at once (already said), until the
                    // next try; the window the try opened closes again.
                    Ok(Presented::Skipped) if tried.is_some() => {
                        self.starved.insert(display.id.clone(), instant);
                        self.platform.close(&display.id);
                        Err(starved_reason())
                    }
                    Ok(Presented::Skipped) => self.skipped(&display.id, out),
                    Err(reason) => {
                        self.starved.remove(&display.id);
                        Err(reason)
                    }
                },
            };
            match presented {
                Ok(()) => {
                    self.presented.insert(display.id.clone());
                }
                Err(reason) => {
                    self.skipping.remove(&display.id);
                    // Only the routes on this display fail.
                    for (id, state) in &mut states {
                        if routes.contains(id) {
                            *state = RouteState::Failed {
                                reason: reason.clone(),
                            };
                        }
                    }
                    if self.presented.remove(&display.id) {
                        self.platform.close(&display.id);
                    }
                }
            }
        }
        // Displays that went away take their windows with them.
        let gone: Vec<String> = self
            .presented
            .iter()
            .filter(|id| !self.displays.iter().any(|d| &&d.id == id))
            .cloned()
            .collect();
        for id in gone {
            self.presented.remove(&id);
            self.platform.close(&id);
        }
        // After every display: a monitor never holds an output up.
        for (title, layer) in watched {
            if let Err(why) = self.platform.present_monitor(&title, &layer) {
                out.push(FromStage::Log {
                    level: LinkLevel::Warn,
                    message: short(format!("the monitor of a stage could not be drawn: {why}")),
                });
                self.monitor_closed(layer.stage, out);
            }
        }
        self.report_failures(out);
        self.states
            .retain(|id, _| states.iter().any(|(s, _)| s == id));
        for (route, state) in states {
            if self.states.get(&route) != Some(&state) {
                self.states.insert(route, state.clone());
                out.push(FromStage::RouteStatus { route, state });
            }
        }
    }

    fn picture(&self, stage: StageId) -> Picture {
        self.pictures.get(&stage).copied().unwrap_or(Picture::Black)
    }

    /// The probe points with the test card's barcode cells first, and the answer split back.
    #[cfg(feature = "test-support")]
    fn read(
        &mut self,
        size: (u32, u32),
        layers: &[Layer],
        guides: &[Guide],
        points: &[[f32; 2]],
    ) -> Result<Read, String> {
        self.read_in(size, layers, guides, points, [0.0, 0.0, 1.0, 1.0])
    }

    /// [`Self::read`] with the test card's barcode read inside `card` (left, top, right,
    /// bottom: fractions of the target), where a stage fitted to a target of another shape is.
    #[cfg(feature = "test-support")]
    fn read_in(
        &mut self,
        size: (u32, u32),
        layers: &[Layer],
        guides: &[Guide],
        points: &[[f32; 2]],
        [left, top, right, bottom]: [f32; 4],
    ) -> Result<Read, String> {
        let mut all: Vec<[f32; 2]> = crate::card::barcode_points()
            .iter()
            .map(|[x, y]| [left + x * (right - left), top + y * (bottom - top)])
            .collect();
        all.extend_from_slice(points);
        let colours = self.platform.probe(size, layers, guides, &all)?;
        let (code, asked) = colours.split_at(colours.len().min(crate::card::CELLS));
        Ok(Read {
            frame: crate::card::read_frame(code),
            colours: asked.to_vec(),
            code: code.to_vec(),
        })
    }

    /// One stage's own picture, at its own size.
    #[cfg(feature = "test-support")]
    fn probe_stage(
        &mut self,
        id: StageId,
        points: &[[f32; 2]],
        out: &mut Vec<FromStage>,
    ) -> FromStage {
        let now = self.now();
        let layer = self
            .stages
            .iter()
            .find(|s| s.id == id)
            .map(|s| layer_of(s, self.picture(id), self.cues_on(id, now), None));
        let cues = (
            layer.as_ref().map_or(0, |l| l.cues.len()),
            self.pics.values().filter(|p| p.stage == id).count(),
        );
        let read = match layer {
            Some(l) => self.read((l.width, l.height), std::slice::from_ref(&l), &[], points),
            None => Err("no such stage".into()),
        };
        let (frame, colours) = probe_answer(read, cues, now, out);
        FromStage::Probed {
            stage: id,
            frame,
            colours,
        }
    }

    /// What the stage's monitor window shows at a content size of `size`: the stage fitted,
    /// black bars elsewhere. No colours when it has no monitor window open.
    #[cfg(feature = "test-support")]
    fn probe_monitor(
        &mut self,
        id: StageId,
        size: (u32, u32),
        points: &[[f32; 2]],
        out: &mut Vec<FromStage>,
    ) -> FromStage {
        let open = self.platform.video() && self.monitors.get(&id).is_some_and(|m| !m.closed);
        let now = self.now();
        let layer = self
            .stages
            .iter()
            .find(|s| s.id == id)
            .filter(|_| open)
            .map(|s| layer_of(s, self.picture(id), self.cues_on(id, now), None));
        let Some(layer) = layer else {
            return FromStage::MonitorProbed {
                stage: id,
                frame: None,
                colours: Vec::new(),
            };
        };
        let cues = (
            layer.cues.len(),
            self.pics.values().filter(|p| p.stage == id).count(),
        );
        let card = layer.place.dest(size).map(|v| v as f32);
        let read = self.read_in(size, std::slice::from_ref(&layer), &[], points, card);
        let (frame, colours) = probe_answer(read, cues, now, out);
        FromStage::MonitorProbed {
            stage: id,
            frame,
            colours,
        }
    }

    /// What display `display` shows: every region shown on it, stacked, at its pixel size.
    #[cfg(feature = "test-support")]
    fn probe_display(
        &mut self,
        display: String,
        points: &[[f32; 2]],
        out: &mut Vec<FromStage>,
    ) -> FromStage {
        let now = self.now();
        let mut plan = self.plan(now);
        let drawn = plan.on.remove(&display).unwrap_or_default();
        // Only what is up there: a failed (or starved) route shows nothing.
        let up = |route: &RouteId| matches!(self.states.get(route), Some(RouteState::Shown { .. }));
        let layers: Vec<Layer> = drawn
            .into_iter()
            .filter(|(_, route, _)| up(route))
            .map(|(.., l)| l)
            .collect();
        let guides: Vec<Guide> = (plan.guides.remove(&display).unwrap_or_default())
            .into_iter()
            .filter(|g| up(&g.route))
            .collect();
        let due: usize = layers.iter().map(|l| l.cues.len()).sum();
        let cues = (due, due);
        let size = self
            .displays
            .iter()
            .find(|d| d.id == display)
            .map(|d| (d.width, d.height));
        let read = match size {
            Some(size) => self.read(size, &layers, &guides, points),
            None => Err("no such display".into()),
        };
        let (frame, colours) = probe_answer(read, cues, now, out);
        FromStage::DisplayProbed {
            display,
            frame,
            colours,
        }
    }
}

/// A region of a stage with something to draw: its index, its stage, the stage's picture and
/// its cues' pictures.
type StagePart<'a> = (usize, &'a StageOutput, Picture, Vec<CuePicture>);

/// One layer a display draws: its order (stage stack, region index) and its route.
type Drawn = ((u32, usize), RouteId, Layer);

/// One region of a stage on its route, on a display of the size given (if known).
type Part<'a> = (
    &'a openlab_core::stage_link::RegionOutput,
    &'a RouteOutput,
    Option<(u32, u32)>,
);

/// What every output would show (`Host::plan`).
#[derive(Default)]
struct Plan {
    states: Vec<(RouteId, RouteState)>,
    /// By display: its layers with their order (stage stack, region index), and its routes.
    on: BTreeMap<String, Vec<Drawn>>,
    /// By display: the guides drawn over its layers, in route order.
    guides: BTreeMap<String, Vec<Guide>>,
    /// Each open monitor's title and its stage's whole layer.
    watched: Vec<(String, Layer)>,
}

/// Test support: what a probe drew: the test card's frame read from its barcode cells, if
/// they make one, the colours at the points asked, and the cells' own colours.
#[cfg(feature = "test-support")]
struct Read {
    frame: Option<u16>,
    colours: Vec<[u8; 4]>,
    code: Vec<[u8; 4]>,
}

/// Test support: a probe's frame and colours. A probe that could not be drawn, or that read no
/// frame with cue pictures loaded (`loaded`, of which `due` were drawn at the clock), is said
/// in the show log with the barcode cells' colours, so a test that finds nothing on a stage
/// shows why (a GPU failure, no frame decoded at that clock: black cells, or a frame drawn
/// that is not a whole test card: other colours).
#[cfg(feature = "test-support")]
fn probe_answer(
    read: Result<Read, String>,
    (due, loaded): (usize, usize),
    (clock, rate): (u64, u32),
    out: &mut Vec<FromStage>,
) -> (Option<u16>, Vec<[u8; 4]>) {
    match read {
        Ok(Read {
            frame: None,
            colours,
            code,
        }) if loaded > 0 => {
            let cells: Vec<String> = code
                .iter()
                .map(|[r, g, b, _]| format!("{r:02x}{g:02x}{b:02x}"))
                .collect();
            out.push(FromStage::Log {
                level: LinkLevel::Info,
                message: short(format!(
                    "probe at sample {clock} ({rate} Hz): {loaded} picture(s) loaded, {due} due, \
                     no frame read; cells {}",
                    cells.join(" ")
                )),
            });
            (None, colours)
        }
        Ok(Read { frame, colours, .. }) => (frame, colours),
        Err(why) => {
            out.push(FromStage::Log {
                level: LinkLevel::Warn,
                message: short(format!("probe not drawn: {why}")),
            });
            (None, Vec::new())
        }
    }
}

/// `stage`'s layer: one of its regions on its route, or (`None`) the whole stage fitted.
fn layer_of(
    stage: &StageOutput,
    picture: Picture,
    cues: Vec<CuePicture>,
    part: Option<Part<'_>>,
) -> Layer {
    let place = match part {
        Some((region, route, screen)) => Placement {
            region: [region.x, region.y, region.width, region.height],
            route: (route.width, route.height),
            rotation: route.rotation,
            scaling: route.scaling,
            screen,
        },
        None => Placement::whole(stage.width, stage.height),
    };
    Layer {
        stage: stage.id,
        width: stage.width,
        height: stage.height,
        picture,
        cues,
        place,
    }
}

/// A reason as the link takes it: one line, at most its text limit.
fn short(reason: String) -> String {
    let one: String = reason
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut cut = one.trim().to_owned();
    while cut.len() > openlab_core::stage_link::MAX_TEXT_BYTES {
        cut.pop();
    }
    if cut.is_empty() {
        "the picture failed".into()
    } else {
        cut
    }
}

/// How often the loop looks at pictures between messages.
pub const POLL_EVERY: std::time::Duration = std::time::Duration::from_millis(16);

/// Why a display's stages failed when it gave no drawable for [`SKIP_LIMIT`].
fn starved_reason() -> String {
    format!(
        "the system gave no picture to draw into for {} s",
        SKIP_LIMIT.as_secs()
    )
}

/// What the reader thread hands the loop: a message, a warning about a line it skipped, or the
/// end of input.
enum Incoming {
    Message(ToStage),
    Skipped(String),
    End,
}

/// Reads lines from `input` on a thread of its own and handles each with `handle` until the
/// input ends, a `quit`, or (test support) a `hang`, writing what it returns to `output`.
/// Between messages (every [`POLL_EVERY`]) `handle` is called with no message, so a picture's open or decode that ends
/// or fails is reported without a message from the engine. A line that is too long or invalid
/// is answered with a `log` warning and skipped. Returns when the engine is gone (end of input
/// or a failed write), so the stage host never outlives its engine; after a `hang` nothing more
/// is answered until then.
pub fn serve(
    input: impl BufRead + Send + 'static,
    output: &mut impl Write,
    mut handle: impl FnMut(Option<ToStage>, &mut Vec<FromStage>) -> Flow,
) -> Flow {
    // Bounded: a loop that falls behind stops the reader, and the engine sees its pipe fill.
    let (tx, rx) = std::sync::mpsc::sync_channel::<Incoming>(1024);
    let reader = std::thread::Builder::new()
        .name("openlab-stage-reader".into())
        .spawn(move || read_into(input, &tx));
    if reader.is_err() {
        return Flow::Quit;
    }
    let mut answers = Vec::new();
    loop {
        let flow = match rx.recv_timeout(POLL_EVERY) {
            Ok(Incoming::Message(message)) => handle(Some(message), &mut answers),
            Ok(Incoming::Skipped(warning)) => {
                answers.push(FromStage::Log {
                    level: LinkLevel::Warn,
                    message: warning,
                });
                Flow::Continue
            }
            Ok(Incoming::End) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Flow::Quit;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => handle(None, &mut answers),
        };
        if write_all(output, &mut answers).is_err() {
            return Flow::Quit;
        }
        match flow {
            Flow::Continue => {}
            Flow::Quit => return flow,
            #[cfg(feature = "test-support")]
            Flow::Hang => {
                while !matches!(rx.recv(), Ok(Incoming::End) | Err(_)) {}
                return flow;
            }
        }
    }
}

/// The reader thread: each line of `input` as a message, until it ends.
fn read_into(mut input: impl BufRead, tx: &std::sync::mpsc::SyncSender<Incoming>) {
    let mut line = Vec::new();
    loop {
        line.clear();
        let incoming = match read_line(&mut input, &mut line) {
            Ok(Line::End) | Err(_) => {
                let _ = tx.send(Incoming::End);
                return;
            }
            Ok(Line::TooLong(bytes)) => {
                Incoming::Skipped(format!("ignored a line of {bytes} bytes from the engine"))
            }
            Ok(Line::Read) => match decode_to_stage(&line) {
                Ok(message) => Incoming::Message(message),
                Err(e) => Incoming::Skipped(format!("ignored a message from the engine: {e}")),
            },
        };
        if tx.send(incoming).is_err() {
            return;
        }
    }
}

/// Writes and empties `answers`.
pub fn write_all(output: &mut impl Write, answers: &mut Vec<FromStage>) -> std::io::Result<()> {
    for answer in answers.drain(..) {
        if let Ok(text) = encode(&answer) {
            output.write_all(text.as_bytes())?;
        }
    }
    output.flush()
}

/// Also used by the platforms' tests, to drive a host down to their decoders.
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use openlab_core::stage_link::MAX_LINE_BYTES;

    /// A platform with displays given by the test and no drawing.
    pub(crate) struct Fake {
        displays: Vec<Display>,
        /// Each display's layers as last presented, bottom first.
        shown: HashMap<String, Vec<StageId>>,
        /// Where each of those layers was placed.
        places: HashMap<String, Vec<Placement>>,
        /// Each display's guides as last presented.
        guides: HashMap<String, Vec<Guide>>,
        /// Each display's pictures as last presented, over all its layers.
        pictures_drawn: HashMap<String, Vec<u64>>,
        /// The display with the menu bar.
        menu_bar: Option<String>,
        /// The display whose window stands back, as last told.
        stood_back: Option<String>,
        fail: bool,
        /// Each frame is skipped (no drawable), the window staying up.
        skip: bool,
        /// Presents asked for, drawn or not.
        presents: u32,
        /// Windows taken down.
        closes: u32,
        live: bool,
        events: Vec<PictureEvent>,
        /// Each monitor window up: its title and the stage drawn in it last, with its cues.
        monitors: HashMap<StageId, (String, Layer)>,
        /// Monitor windows the operator closed, not yet taken.
        user_closed: Vec<StageId>,
        /// Monitor windows brought to the front.
        raised: Vec<StageId>,
        fail_monitors: bool,
        /// The operator's app as last told.
        operator: Option<u32>,
        /// Polls that asked the platform to keep the keys with the operator's app.
        keeps: u32,
    }

    impl Platform for Fake {
        fn video(&self) -> bool {
            true
        }
        fn displays(&mut self) -> Vec<Display> {
            self.displays.clone()
        }
        fn present(
            &mut self,
            d: &Display,
            layers: &[Layer],
            guides: &[Guide],
        ) -> Result<Presented, String> {
            self.presents += 1;
            if self.fail {
                return Err("no GPU".into());
            }
            if self.skip {
                // The window is up (opened if it was not), its last frame kept.
                self.shown.entry(d.id.clone()).or_default();
                return Ok(Presented::Skipped);
            }
            self.shown
                .insert(d.id.clone(), layers.iter().map(|l| l.stage).collect());
            self.places
                .insert(d.id.clone(), layers.iter().map(|l| l.place).collect());
            self.guides.insert(d.id.clone(), guides.to_vec());
            let pictures = layers.iter().flat_map(|l| l.cues.iter().map(|c| c.picture));
            self.pictures_drawn.insert(d.id.clone(), pictures.collect());
            Ok(Presented::Drawn)
        }
        fn close(&mut self, display: &str) {
            self.closes += 1;
            self.shown.remove(display);
            self.guides.remove(display);
            self.pictures_drawn.remove(display);
        }
        #[cfg(feature = "test-support")]
        fn probe(
            &mut self,
            _: (u32, u32),
            _: &[Layer],
            _: &[Guide],
            p: &[[f32; 2]],
        ) -> Result<Vec<[u8; 4]>, String> {
            if self.fail {
                return Err("no GPU".into());
            }
            Ok(vec![[0, 0, 0, 255]; p.len()])
        }
        fn live(&self) -> bool {
            self.live
        }
        fn open_picture(
            &mut self,
            picture: u64,
            file: &str,
            _: bool,
            _: f64,
        ) -> Result<(), String> {
            if file == "bad" {
                Err("cannot open\nthis".into())
            } else {
                self.events.push(PictureEvent::Opened(picture));
                Ok(())
            }
        }
        fn take_picture_events(&mut self) -> Vec<PictureEvent> {
            std::mem::take(&mut self.events)
        }
        fn present_monitor(&mut self, title: &str, layer: &Layer) -> Result<(), String> {
            if self.fail_monitors {
                return Err("no drawable".into());
            }
            self.monitors
                .insert(layer.stage, (title.to_owned(), layer.clone()));
            Ok(())
        }
        fn raise_monitor(&mut self, stage: StageId) {
            self.raised.push(stage);
        }
        fn close_monitor(&mut self, stage: StageId) {
            self.monitors.remove(&stage);
        }
        fn menu_bar_display(&self) -> Option<String> {
            self.menu_bar.clone()
        }
        fn stand_back(&mut self, display: Option<&str>) {
            self.stood_back = display.map(str::to_owned);
        }
        fn set_operator(&mut self, pid: Option<u32>) {
            self.operator = pid;
        }
        fn keep_keys_with_operator(&mut self) {
            self.keeps += 1;
        }
        fn take_closed_monitors(&mut self) -> Vec<StageId> {
            let closed = std::mem::take(&mut self.user_closed);
            for stage in &closed {
                self.monitors.remove(stage);
            }
            closed
        }
    }

    /// What stage `n` draws for its cues, by picture: position and opacity.
    fn cues(host: &mut Host<Fake>, n: u8) -> Vec<(u64, f64, f32)> {
        drawn(host, n)
            .into_iter()
            .map(|c| (c.picture, c.position, c.opacity))
            .collect()
    }

    pub(crate) fn drawn(host: &mut Host<Fake>, n: u8) -> Vec<CuePicture> {
        let now = host.now();
        host.cues_on(id(n), now)
    }

    fn plain_play(picture: u64, at: u64, offset: f64) -> ToStage {
        ToStage::Play {
            picture,
            at,
            offset,
            action: offset,
            // One long pass from the offset: it runs on as the clock moves.
            plan: vec![PlanPass {
                from: offset,
                length: 3600.0,
                plays: 1,
            }],
            into: 0.0,
            fades: PictureFades::NONE,
        }
    }

    pub(crate) fn send(host: &mut Host<Fake>, m: ToStage) -> Vec<FromStage> {
        let mut out = Vec::new();
        host.handle(m, &mut out);
        out
    }

    pub(crate) fn clock(host: &mut Host<Fake>, sample: u64) {
        send(host, ToStage::Clock { sample, rate: 1000 });
    }

    pub(crate) fn load(host: &mut Host<Fake>, picture: u64, n: u8) -> Vec<FromStage> {
        send(
            host,
            ToStage::Load {
                picture,
                stage: id(n),
                file: "clip.mov".into(),
                still: false,
                offset: 0.0,
                geometry: VideoGeometry::default(),
            },
        )
    }

    fn on_layer(layer: u16) -> VideoGeometry {
        VideoGeometry {
            layer,
            ..VideoGeometry::default()
        }
    }

    fn play(host: &mut Host<Fake>, picture: u64, at: u64) {
        send(host, plain_play(picture, at, 0.0));
    }

    fn logs(out: &[FromStage]) -> Vec<(LinkLevel, String)> {
        out.iter()
            .filter_map(|m| match m {
                FromStage::Log { level, message } => Some((*level, message.clone())),
                _ => None,
            })
            .collect()
    }

    /// A probe that reads no frame with a picture loaded, or that cannot be drawn, says so in
    /// the log, so a test that finds nothing on its stage shows why.
    #[cfg(feature = "test-support")]
    #[test]
    fn a_probe_that_reads_nothing_says_why() {
        let mut h = host(&["D"]);
        send(&mut h, stages_msg(vec![stage(1, Some("D"), false)]));
        let probe = ToStage::Probe {
            stage: id(1),
            points: vec![[0.5, 0.5]],
        };
        assert_eq!(logs(&send(&mut h, probe.clone())), vec![], "nothing loaded");
        clock(&mut h, 1000);
        load(&mut h, 7, 1);
        let said = logs(&send(&mut h, probe.clone()));
        // With the barcode cells' colours: here all black, nothing drawn.
        let black = ["000000"; crate::card::CELLS].join(" ");
        assert!(
            matches!(said.as_slice(), [(LinkLevel::Info, m)]
                if m.contains("1 picture(s) loaded, 0 due") && m.ends_with(&format!("cells {black}"))),
            "{said:?}"
        );
        h.platform_mut().fail = true;
        let out = send(&mut h, probe);
        assert!(
            matches!(logs(&out).as_slice(), [(LinkLevel::Warn, m)] if m == "probe not drawn: no GPU"),
            "{out:?}"
        );
        assert!(
            out.iter()
                .any(|m| matches!(m, FromStage::Probed { frame: None, .. })),
            "still answered: {out:?}"
        );
    }

    /// A picture that fails as a probe draws it (its decoder failed while the probe waited for
    /// its frame) is reported before the probe's answer, so the engine knows why the probe found
    /// nothing once it has that answer.
    #[cfg(feature = "test-support")]
    #[test]
    fn a_picture_that_fails_as_a_probe_draws_it_is_reported_before_the_answer() {
        let mut h = host(&["D"]);
        send(&mut h, stages_msg(vec![stage(1, Some("D"), false)]));
        clock(&mut h, 1000);
        load(&mut h, 7, 1);
        send(&mut h, plain_play(7, 1000, 0.0));
        h.platform_mut().events.push(PictureEvent::Failed(
            7,
            "the movie could not be read".into(),
        ));
        let out = send(
            &mut h,
            ToStage::Probe {
                stage: id(1),
                points: vec![[0.5, 0.5]],
            },
        );
        let failed = out
            .iter()
            .position(|m| matches!(m, FromStage::PictureFailed { picture: 7, .. }));
        let probed = out
            .iter()
            .position(|m| matches!(m, FromStage::Probed { .. }));
        assert!(
            matches!((failed, probed), (Some(f), Some(p)) if f < p),
            "{out:?}"
        );
    }

    /// A movie opened again after its reader failed is one warning line for the show log and
    /// plays on: no failure is reported and it is still drawn.
    #[test]
    fn a_movie_opened_again_is_one_log_line_and_stays_drawn() {
        let mut h = host(&["D"]);
        send(&mut h, stages_msg(vec![stage(1, Some("D"), false)]));
        clock(&mut h, 1000);
        load(&mut h, 7, 1);
        send(&mut h, plain_play(7, 1000, 0.0));
        h.platform_mut().events.push(PictureEvent::Reopened(
            7,
            "reading clip.mov failed (Cannot Decode); opened it again at 1.20 s".into(),
        ));
        let mut out = Vec::new();
        h.poll(&mut out);
        assert_eq!(
            logs(&out),
            vec![(
                LinkLevel::Warn,
                "reading clip.mov failed (Cannot Decode); opened it again at 1.20 s".into()
            )]
        );
        assert!(
            !out.iter()
                .any(|m| matches!(m, FromStage::PictureFailed { .. })),
            "{out:?}"
        );
        clock(&mut h, 2000);
        assert_eq!(cues(&mut h, 1).len(), 1, "still drawn");
    }

    /// A picture is drawn from its play's sample, its position following the clock; a pause
    /// holds it (after its own sample); a stop fades it out and it goes.
    #[test]
    fn a_picture_follows_the_clock_holds_when_paused_and_fades_out_when_stopped() {
        let mut h = host(&["D"]);
        send(&mut h, stages_msg(vec![stage(1, Some("D"), false)]));
        clock(&mut h, 1000);
        load(&mut h, 7, 1);
        assert_eq!(cues(&mut h, 1), vec![], "loaded is not drawn");
        send(&mut h, plain_play(7, 1500, 2.0));
        assert_eq!(cues(&mut h, 1), vec![], "not before its sample");
        clock(&mut h, 2000);
        assert_eq!(cues(&mut h, 1), vec![(7, 2.5, 1.0)]);
        send(
            &mut h,
            ToStage::Pause {
                picture: 7,
                at: 2500,
                offset: 3.0,
                action: 3.0,
            },
        );
        clock(&mut h, 2400);
        assert_eq!(
            cues(&mut h, 1),
            vec![(7, 2.9, 1.0)],
            "plays on until the pause's sample"
        );
        clock(&mut h, 4000);
        assert_eq!(cues(&mut h, 1), vec![(7, 3.0, 1.0)]);
        send(&mut h, plain_play(7, 4000, 3.0));
        send(
            &mut h,
            ToStage::Stop {
                picture: 7,
                at: 4000,
                fade: 1000,
                cut: None,
            },
        );
        clock(&mut h, 4500);
        assert_eq!(cues(&mut h, 1), vec![(7, 3.5, 0.5)]);
        clock(&mut h, 5000);
        send(
            &mut h,
            ToStage::Clock {
                sample: 5000,
                rate: 1000,
            },
        );
        h.redraw(&mut Vec::new());
        assert!(!h.playing(), "gone once its fade is over");

        // A stop cut short (its cue's action ends mid-fade): it fades as the stop says until
        // the cut, then it is gone.
        load(&mut h, 8, 1);
        send(&mut h, plain_play(8, 5000, 0.0));
        send(
            &mut h,
            ToStage::Stop {
                picture: 8,
                at: 5000,
                fade: 1000,
                cut: Some(5600),
            },
        );
        clock(&mut h, 5500);
        assert_eq!(cues(&mut h, 1), vec![(8, 0.5, 0.5)], "fading, not cut yet");
        clock(&mut h, 5600);
        assert_eq!(cues(&mut h, 1), vec![], "gone on the cut");
        h.redraw(&mut Vec::new());
        assert!(!h.playing(), "dropped on the cut");
        // A cut already passed when the stop comes: gone at once.
        load(&mut h, 9, 1);
        send(&mut h, plain_play(9, 5600, 0.0));
        let stop = ToStage::Stop {
            picture: 9,
            at: 5600,
            fade: 1000,
            cut: Some(5600),
        };
        send(&mut h, stop);
        assert!(!h.playing(), "gone at once");
    }

    /// A movie's plan (VID-07): the picture follows its passes (a trimmed range played twice),
    /// says where the next pass starts as a seam nears, and holds just before the plan's end
    /// once it is over; its integrated fades scale its opacity by its action time.
    #[test]
    fn a_picture_follows_its_plan_through_a_loop_fades_and_holds_at_its_end() {
        let mut h = host(&["D"]);
        send(&mut h, stages_msg(vec![stage(1, Some("D"), false)]));
        clock(&mut h, 0);
        load(&mut h, 7, 1);
        let linear = |duration| openlab_core::FadeSpec {
            duration,
            curve: openlab_core::FadeCurve::Linear,
        };
        send(
            &mut h,
            ToStage::Play {
                picture: 7,
                at: 0,
                offset: 1.0,
                action: 0.0,
                plan: vec![PlanPass {
                    from: 1.0,
                    length: 2.0,
                    plays: 2,
                }],
                into: 0.0,
                fades: PictureFades {
                    fade_in: linear(1.0),
                    fade_out: linear(1.0),
                    end: Some(4.0),
                },
            },
        );
        let at = |h: &mut Host<Fake>, sample| {
            clock(h, sample);
            drawn(h, 1).first().copied().unwrap()
        };
        let p = at(&mut h, 500);
        assert_eq!((p.position, p.opacity, p.held), (1.5, 0.5, false));
        assert_eq!(p.upcoming(), None, "the seam is far");
        let p = at(&mut h, 1990);
        assert!((p.position - 2.99).abs() < 1e-9);
        assert_eq!(p.upcoming(), Some(1.0), "the loop's start, primed ahead");
        // A frame shown a refresh later is in the next pass.
        assert!((p.position_after(0.02) - 1.01).abs() < 1e-9);
        assert_eq!(p.pass_after(0.02), p.pass + 1);
        let p = at(&mut h, 2500);
        assert_eq!((p.position, p.opacity), (1.5, 1.0));
        assert_eq!(p.upcoming(), None, "the last pass jumps nowhere");
        let p = at(&mut h, 3500);
        assert_eq!(p.opacity, 0.5, "fading out to the end");
        // A frame shown a refresh later than the plan's end is the one it will hold there,
        // in the same pass: nothing past the range is asked for.
        let p = at(&mut h, 3990);
        let ahead = p.position_after(0.02);
        assert!(ahead > p.position && ahead < 3.0, "{ahead}");
        assert_eq!(p.pass_after(0.02), p.pass);
        let p = at(&mut h, 9000);
        assert!(p.held && p.position < 3.0 && p.position > 2.999);
        assert_eq!(p.position_after(0.5), p.position, "held");
        assert_eq!(p.opacity, 0.0, "held where its fade ended");
        // A pause holds its action time's fade.
        send(
            &mut h,
            ToStage::Pause {
                picture: 7,
                at: 9000,
                offset: 1.5,
                action: 0.25,
            },
        );
        let p = at(&mut h, 9500);
        assert_eq!((p.position, p.opacity), (1.5, 0.25));
    }

    /// A play with no plan holds at its offset: whatever sends one, a movie never runs on into
    /// its file past the range it was given; one `into` its plan follows the plan from there.
    #[test]
    fn a_play_without_a_plan_holds_and_one_into_its_plan_starts_there() {
        let mut h = host(&["D"]);
        send(&mut h, stages_msg(vec![stage(1, Some("D"), false)]));
        clock(&mut h, 0);
        load(&mut h, 7, 1);
        let mut empty = plain_play(7, 0, 1.16);
        if let ToStage::Play { plan, .. } = &mut empty {
            plan.clear();
        }
        send(&mut h, empty);
        clock(&mut h, 3000);
        assert_eq!(cues(&mut h, 1), vec![(7, 1.16, 1.0)], "held, not run on");
        send(
            &mut h,
            ToStage::Play {
                picture: 7,
                at: 3000,
                offset: 1.5,
                action: 2.5,
                plan: vec![PlanPass {
                    from: 1.0,
                    length: 1.0,
                    plays: 3,
                }],
                into: 2.5,
                fades: PictureFades::NONE,
            },
        );
        clock(&mut h, 3200);
        let p = drawn(&mut h, 1)[0];
        assert!((p.position - 1.7).abs() < 1e-9, "{}", p.position);
        assert_eq!(p.pass, 2, "its third pass");
        clock(&mut h, 9000);
        let p = drawn(&mut h, 1)[0];
        assert!(p.held && p.position < 2.0 && p.position > 1.999);
    }

    /// On real displays the clock runs on between the engine's messages, but no further than
    /// `CLOCK_HOLD` past the last: an engine clock that stops stops the picture.
    #[test]
    fn the_clock_runs_on_between_messages_only_a_little() {
        let mut h = host(&["D"]);
        h.platform.live = true;
        send(
            &mut h,
            ToStage::Clock {
                sample: 1000,
                rate: 48_000,
            },
        );
        let after = |ms| Some(std::time::Duration::from_millis(ms));
        assert_eq!(h.now_after(after(10)).0, 1000 + 480);
        assert_eq!(
            h.now_after(after(60_000)).0,
            1000 + 48 * 30,
            "a minute later: held at the cap"
        );
    }

    /// Two pictures on one stage: the later-started is drawn last (on top), whatever their ids.
    #[test]
    fn the_later_started_picture_is_drawn_on_top() {
        let mut h = host(&["D"]);
        send(&mut h, stages_msg(vec![stage(1, Some("D"), false)]));
        clock(&mut h, 0);
        load(&mut h, 9, 1);
        load(&mut h, 3, 1);
        send(&mut h, plain_play(9, 0, 0.0));
        send(&mut h, plain_play(3, 100, 0.0));
        clock(&mut h, 200);
        let order: Vec<u64> = cues(&mut h, 1).iter().map(|c| c.0).collect();
        assert_eq!(order, vec![9, 3]);
    }

    /// A higher layer is drawn on top whatever the start order; on one layer the later-started
    /// is on top (QLab 5 manual p. 213). A layer changed while both play restacks them at once.
    #[test]
    fn a_higher_layer_is_on_top_whatever_the_start_order() {
        let mut h = host(&["D"]);
        send(&mut h, stages_msg(vec![stage(1, Some("D"), false)]));
        clock(&mut h, 0);
        load(&mut h, 1, 1);
        load(&mut h, 2, 1);
        load(&mut h, 3, 1);
        let geometry = |h: &mut Host<Fake>, picture, layer| {
            send(
                h,
                ToStage::Geometry {
                    picture,
                    geometry: on_layer(layer),
                },
            );
        };
        geometry(&mut h, 1, 10);
        geometry(&mut h, 2, 5);
        geometry(&mut h, 3, 5);
        play(&mut h, 1, 0);
        play(&mut h, 2, 10);
        play(&mut h, 3, 20);
        clock(&mut h, 100);
        let order = |h: &mut Host<Fake>| -> Vec<u64> { cues(h, 1).iter().map(|c| c.0).collect() };
        assert_eq!(order(&mut h), vec![2, 3, 1]);
        geometry(&mut h, 2, 11);
        assert_eq!(order(&mut h), vec![3, 1, 2]);
        // An unknown picture's geometry is ignored.
        geometry(&mut h, 99, 0);
        assert_eq!(order(&mut h), vec![3, 1, 2]);
    }

    /// A geometry change reaches the picture as it plays: its position in the file runs on.
    #[test]
    fn a_geometry_change_keeps_the_picture_running() {
        let mut h = host(&["D"]);
        send(&mut h, stages_msg(vec![stage(1, Some("D"), false)]));
        clock(&mut h, 0);
        load(&mut h, 1, 1);
        play(&mut h, 1, 0);
        clock(&mut h, 500);
        let g = VideoGeometry {
            opacity: 0.5,
            fill_stage: false,
            ..VideoGeometry::default()
        };
        send(
            &mut h,
            ToStage::Geometry {
                picture: 1,
                geometry: g,
            },
        );
        clock(&mut h, 1000);
        let now = h.now();
        let drawn = h.cues_on(id(1), now);
        assert_eq!(drawn.len(), 1);
        assert_eq!(drawn[0].geometry, g);
        assert_eq!(drawn[0].position, 1.0);
    }

    fn ramp_msg(
        picture: u64,
        param: PictureParam,
        from: f64,
        to: f64,
        at: u64,
        frames: u64,
    ) -> ToStage {
        ToStage::Ramp {
            picture,
            param,
            ramp: PictureRamp {
                from,
                to,
                at,
                frames,
                curve: openlab_core::FadeCurve::Linear,
            },
        }
    }

    /// A Fade cue's ramp (VID-05) is drawn over the picture's own geometry along its curve from
    /// its sample; the other values stay the picture's own, an edit of its geometry included.
    #[test]
    fn a_ramp_moves_one_value_of_a_playing_picture_along_its_curve() {
        let mut h = host(&["D"]);
        send(&mut h, stages_msg(vec![stage(1, Some("D"), false)]));
        clock(&mut h, 0);
        load(&mut h, 1, 1);
        play(&mut h, 1, 0);
        send(
            &mut h,
            ramp_msg(1, PictureParam::Opacity, 1.0, 0.0, 1000, 2000),
        );
        let drawn = |h: &mut Host<Fake>| drawn(h, 1)[0];
        clock(&mut h, 500);
        assert_eq!(drawn(&mut h).geometry.opacity, 1.0, "not before its sample");
        clock(&mut h, 2000);
        let d = drawn(&mut h);
        assert_eq!(d.geometry.opacity, 0.5);
        assert_eq!(d.position, 2.0, "the movie runs on");
        // An edit of another value is drawn; the faded one keeps its ramp.
        send(
            &mut h,
            ToStage::Geometry {
                picture: 1,
                geometry: VideoGeometry {
                    opacity: 0.9,
                    rotation: 30.0,
                    fill_stage: false,
                    ..VideoGeometry::default()
                },
            },
        );
        clock(&mut h, 2500);
        let d = drawn(&mut h);
        assert_eq!((d.geometry.opacity, d.geometry.rotation), (0.25, 30.0));
        clock(&mut h, 9000);
        assert_eq!(drawn(&mut h).geometry.opacity, 0.0, "held at its end");
        // An unknown picture's ramp is ignored.
        send(&mut h, ramp_msg(99, PictureParam::Opacity, 1.0, 0.0, 0, 0));
    }

    /// A later ramp dated ahead of the clock (a takeover inside the lookahead) leaves the one in
    /// force drawn until its own sample, so the picture never jumps ahead.
    #[test]
    fn a_ramp_dated_ahead_takes_over_only_from_its_sample() {
        let mut h = host(&["D"]);
        send(&mut h, stages_msg(vec![stage(1, Some("D"), false)]));
        clock(&mut h, 0);
        load(&mut h, 1, 1);
        play(&mut h, 1, 0);
        send(
            &mut h,
            ramp_msg(1, PictureParam::TranslationX, 0.0, 400.0, 0, 4000),
        );
        clock(&mut h, 1000);
        send(
            &mut h,
            ramp_msg(1, PictureParam::TranslationX, 150.0, -100.0, 1500, 1000),
        );
        clock(&mut h, 1200);
        assert_eq!(drawn(&mut h, 1)[0].geometry.translation.x, 120.0);
        clock(&mut h, 2000);
        assert_eq!(drawn(&mut h, 1)[0].geometry.translation.x, 25.0);
        // A burst of ramps all dated ahead keeps a bounded list.
        for n in 0..100 {
            send(
                &mut h,
                ramp_msg(1, PictureParam::ScaleX, 1.0, 2.0, 10_000 + n, 10),
            );
        }
        let kept = h.pics.get(&1).map(|p| p.ramps[&PictureParam::ScaleX].len());
        assert_eq!(kept, Some(MAX_RAMPS));
    }

    /// A picture whose file cannot be opened, whose stage is not there, or whose stage is
    /// removed while it plays is reported failed, with a one-line reason.
    #[test]
    fn a_picture_that_cannot_be_shown_is_reported_failed() {
        let mut h = host(&["D"]);
        send(&mut h, stages_msg(vec![stage(1, Some("D"), false)]));
        let failed = |out: &[FromStage]| -> Vec<(u64, String)> {
            out.iter()
                .filter_map(|m| match m {
                    FromStage::PictureFailed { picture, reason } => {
                        Some((*picture, reason.clone()))
                    }
                    _ => None,
                })
                .collect()
        };
        let out = send(
            &mut h,
            ToStage::Load {
                picture: 1,
                stage: id(1),
                file: "bad".into(),
                still: true,
                offset: 0.0,
                geometry: VideoGeometry::default(),
            },
        );
        assert_eq!(failed(&out), vec![(1, "cannot open this".to_owned())]);
        assert_eq!(failed(&load(&mut h, 2, 9)).len(), 1, "no such stage");
        load(&mut h, 3, 1);
        send(&mut h, plain_play(3, 0, 0.0));
        let out = send(&mut h, stages_msg(vec![]));
        assert_eq!(failed(&out), vec![(3, "its stage was removed".to_owned())]);
        assert!(!h.playing());
    }

    /// #770: a picture whose decoder fails while it plays leaves its display at once, with
    /// nothing else to tell the stage host: the next frame-loop tick draws the display without
    /// it (black on a stage keeping black), or takes the stage down (hidden) when it does not.
    /// Never its last frame left in the window.
    #[test]
    fn a_picture_that_fails_while_playing_leaves_the_display_at_once() {
        for keep_black in [true, false] {
            let mut h = host(&["D"]);
            let version = LINK_VERSION;
            send(&mut h, ToStage::Hello { version });
            send(&mut h, stages_msg(vec![stage(1, Some("D"), keep_black)]));
            load(&mut h, 3, 1);
            play(&mut h, 3, 0);
            clock(&mut h, 100);
            h.step(None, &mut Vec::new());
            assert_eq!(h.platform.pictures_drawn.get("D"), Some(&vec![3]));
            h.platform.events.push(PictureEvent::Failed(
                3,
                "the movie could not be read".into(),
            ));
            // The frame loop's tick: nothing playing is left.
            let mut out = Vec::new();
            h.step(None, &mut out);
            assert!(
                out.iter()
                    .any(|m| matches!(m, FromStage::PictureFailed { picture: 3, .. })),
                "{out:?}"
            );
            if keep_black {
                assert_eq!(
                    h.platform.pictures_drawn.get("D"),
                    Some(&vec![]),
                    "drawn black"
                );
            } else {
                assert_eq!(h.platform.shown.get("D"), None, "taken down");
                assert!(
                    states(&out).contains(&(id(1), RouteState::Hidden)),
                    "{out:?}"
                );
            }
        }
    }

    /// #789: a display that gives no drawable for `SKIP_LIMIT` fails its stages and its window
    /// closes, so the engine and operator are told; short of the limit it is still a skip. The
    /// stretch's start is moved back by hand, as the silence test moves `heard`.
    #[test]
    fn a_display_skipping_frames_past_the_limit_fails_its_stages() {
        let mut h = host(&["D1"]);
        let mut out = Vec::new();
        let version = LINK_VERSION;
        send(&mut h, ToStage::Hello { version });
        send(&mut h, stages_msg(vec![stage(1, Some("D1"), true)]));
        h.platform.skip = true;
        h.displays_changed(&mut out);
        let move_back = |h: &mut Host<Fake>, by: std::time::Duration| {
            let began = h.skipping.get_mut("D1").expect("a stretch of skips");
            *began = began.checked_sub(by).expect("an earlier instant");
        };
        // Short of the limit (a second to spare): still skipped, the stage still shown.
        move_back(&mut h, SKIP_LIMIT - std::time::Duration::from_secs(1));
        out.clear();
        h.displays_changed(&mut out);
        assert_eq!(states(&out), vec![]);
        assert!(h.platform.shown.contains_key("D1"));
        // Past it: failed with the reason, the window closed.
        move_back(&mut h, std::time::Duration::from_secs(1));
        h.displays_changed(&mut out);
        assert_eq!(
            states(&out),
            vec![(
                id(1),
                RouteState::Failed {
                    reason: "the system gave no picture to draw into for 3 s".into()
                }
            )]
        );
        assert!(!h.platform.shown.contains_key("D1"), "the window stayed up");
        // Drawables again: the next frame opens the window and shows the stage.
        h.platform.skip = false;
        out.clear();
        h.displays_changed(&mut out);
        assert_eq!(
            states(&out),
            vec![(
                id(1),
                RouteState::Shown {
                    display: "D1".into()
                }
            )]
        );
        assert!(h.platform.shown.contains_key("D1"));
    }

    /// A keep-black stage on display D1, which gives no drawable from now on, failed past
    /// `SKIP_LIMIT` by the next step (the stretch's start moved back by hand), with what it
    /// said.
    fn starved_host() -> (Host<Fake>, Vec<FromStage>) {
        let mut h = host(&["D1"]);
        let mut out = Vec::new();
        let version = LINK_VERSION;
        send(&mut h, ToStage::Hello { version });
        send(&mut h, stages_msg(vec![stage(1, Some("D1"), true)]));
        h.platform.skip = true;
        h.displays_changed(&mut out);
        let began = h.skipping.get_mut("D1").expect("a stretch of skips");
        *began = began.checked_sub(SKIP_LIMIT).expect("an earlier instant");
        h.step(None, &mut out);
        assert_eq!(
            states(&out),
            vec![(
                id(1),
                RouteState::Failed {
                    reason: "the system gave no picture to draw into for 3 s".into()
                }
            )]
        );
        assert!(!h.platform.shown.contains_key("D1"), "the window stayed up");
        (h, out)
    }

    /// Display D1's last try after starving is moved back by `SKIP_RETRY`: the next poll tries.
    fn retry_due(h: &mut Host<Fake>) {
        if let Some(tried) = h.starved.get_mut("D1") {
            *tried = tried.checked_sub(SKIP_RETRY).expect("an earlier instant");
        }
    }

    fn warnings(out: &[FromStage]) -> usize {
        out.iter()
            .filter(|m| {
                matches!(
                    m,
                    FromStage::Log {
                        level: LinkLevel::Warn,
                        ..
                    }
                )
            })
            .count()
    }

    /// #790 flow: a keep-black stage with nothing playing whose display stopped giving
    /// drawables past the limit, then gives them again: with no message from the engine, the
    /// stage shows black again by itself at the next try, `SKIP_RETRY` after it failed.
    #[test]
    fn a_keep_black_stage_failed_for_lack_of_drawables_shows_black_again_by_itself() {
        let (mut h, _) = starved_host();
        h.platform.skip = false;
        let mut out = Vec::new();
        h.step(None, &mut out);
        assert_eq!(states(&out), vec![], "tried again before its time");
        assert!(!h.platform.shown.contains_key("D1"));
        retry_due(&mut h);
        h.step(None, &mut out);
        assert_eq!(
            states(&out),
            vec![(
                id(1),
                RouteState::Shown {
                    display: "D1".into()
                }
            )]
        );
        assert_eq!(h.platform.pictures_drawn.get("D1"), Some(&vec![]), "black");
        assert!(h.starved.is_empty());
    }

    /// #790 flow: while the display still gives no drawable, it is tried again only every
    /// `SKIP_RETRY`, not every frame, though a picture plays on its stage and the engine keeps
    /// talking; a try that finds none leaves the stage failed without a word: the failure
    /// logged one line.
    #[test]
    fn a_retry_after_lack_of_drawables_is_paced_and_logs_one_line_per_failure() {
        let (mut h, mut out) = starved_host();
        load(&mut h, 3, 1);
        play(&mut h, 3, 0);
        let presents = h.platform.presents;
        for k in 1..=50 {
            clock(&mut h, 100 * k);
            h.step(None, &mut out);
        }
        assert_eq!(h.platform.presents, presents, "presented within the pace");
        let said = out.len();
        retry_due(&mut h);
        h.step(None, &mut out);
        assert_eq!(h.platform.presents, presents + 1, "one try when due");
        for k in 51..=100 {
            clock(&mut h, 100 * k);
            h.step(None, &mut out);
        }
        assert_eq!(
            h.platform.presents,
            presents + 1,
            "one try, then paced again"
        );
        assert_eq!(
            states(&out[said..]),
            vec![],
            "the failed try said something"
        );
        assert!(!h.platform.shown.contains_key("D1"), "the window reopened");
        assert_eq!(warnings(&out), 1, "{out:?}");
    }

    /// #789: the frame that should take a failed picture down is skipped (no drawable): the
    /// next frame-loop tick draws again, though nothing plays, so its last frame never stays up.
    #[test]
    fn a_skipped_frame_after_a_picture_fails_is_drawn_again_at_the_next_tick() {
        let mut h = host(&["D"]);
        let version = LINK_VERSION;
        send(&mut h, ToStage::Hello { version });
        send(&mut h, stages_msg(vec![stage(1, Some("D"), true)]));
        load(&mut h, 3, 1);
        play(&mut h, 3, 0);
        clock(&mut h, 100);
        h.step(None, &mut Vec::new());
        assert_eq!(h.platform.pictures_drawn.get("D"), Some(&vec![3]));
        h.platform.skip = true;
        h.platform.events.push(PictureEvent::Failed(
            3,
            "the movie could not be read".into(),
        ));
        h.step(None, &mut Vec::new());
        assert_eq!(
            h.platform.pictures_drawn.get("D"),
            Some(&vec![3]),
            "skipped"
        );
        h.platform.skip = false;
        h.step(None, &mut Vec::new());
        assert_eq!(
            h.platform.pictures_drawn.get("D"),
            Some(&vec![]),
            "the failed picture's last frame stayed up"
        );
    }

    /// A host with displays M (the menu bar's) and D, a stage keeping black on each.
    fn menu_bar_host() -> Host<Fake> {
        let mut h = host(&["M", "D"]);
        h.platform.menu_bar = Some("M".into());
        let version = LINK_VERSION;
        send(&mut h, ToStage::Hello { version });
        send(
            &mut h,
            stages_msg(vec![stage(1, Some("M"), true), stage(2, Some("D"), true)]),
        );
        h
    }

    /// #708: the app's question up (`stand_back`) → the window on the display with the menu bar
    /// stands back, drawing what it drew, its stage still shown; the question answered → it
    /// covers again. Other displays never stand back.
    #[test]
    fn the_menu_bars_window_stands_back_while_the_app_asks_and_covers_again_after() {
        let mut h = menu_bar_host();
        assert_eq!(h.platform.stood_back, None);
        let out = send(&mut h, ToStage::StandBack { on: true });
        assert_eq!(h.platform.stood_back.as_deref(), Some("M"));
        assert_eq!(h.platform.shown.get("M"), Some(&vec![id(1)]), "still drawn");
        assert_eq!(h.platform.shown.get("D"), Some(&vec![id(2)]));
        assert!(states(&out).is_empty(), "no stage changed: {out:?}");
        send(&mut h, ToStage::StandBack { on: false });
        assert_eq!(h.platform.stood_back, None);
    }

    /// #795: the app gone (force-quit) → every stage window comes down, its stage hidden, said
    /// once in the log, whatever plays (the picture runs on unseen, its monitor not drawn); the app
    /// back → every window up again with what it shows. The app's process is the one a click on a
    /// stage brings forward, and every poll keeps the keys with it.
    #[test]
    fn the_app_gone_takes_every_stage_window_down_until_it_is_back() {
        let mut h = menu_bar_host();
        send(
            &mut h,
            ToStage::App {
                connected: true,
                pid: Some(77),
            },
        );
        assert_eq!(h.platform.operator, Some(77));
        load(&mut h, 3, 2);
        play(&mut h, 3, 0);
        clock(&mut h, 10);
        h.step(None, &mut Vec::new());
        assert_eq!(h.platform.pictures_drawn.get("D"), Some(&vec![3]));
        let polls = h.platform.keeps;
        h.step(None, &mut Vec::new());
        assert!(h.platform.keeps > polls, "every poll keeps the keys away");
        let out = send(
            &mut h,
            ToStage::App {
                connected: false,
                pid: None,
            },
        );
        assert!(
            h.platform.shown.is_empty(),
            "no window up: {:?}",
            h.platform.shown
        );
        assert_eq!(h.platform.operator, None);
        assert_eq!(
            states(&out),
            vec![(id(1), RouteState::Hidden), (id(2), RouteState::Hidden)]
        );
        assert!(matches!(&logs(&out)[..], [(LinkLevel::Info, m)] if m.contains("app is gone")));
        // Frames go on (the picture plays on, unseen): still nothing up.
        clock(&mut h, 20);
        h.step(None, &mut Vec::new());
        assert!(h.platform.shown.is_empty());
        assert_eq!(drawn(&mut h, 2).len(), 1, "its picture still runs");
        // The same word again changes nothing and says nothing.
        let again = send(
            &mut h,
            ToStage::App {
                connected: false,
                pid: None,
            },
        );
        assert!(again.is_empty(), "{again:?}");
        let out = send(
            &mut h,
            ToStage::App {
                connected: true,
                pid: Some(78),
            },
        );
        assert_eq!(h.platform.shown.get("M"), Some(&vec![id(1)]));
        assert_eq!(h.platform.pictures_drawn.get("D"), Some(&vec![3]));
        assert_eq!(h.platform.operator, Some(78));
        assert_eq!(states(&out).len(), 2, "both shown again: {out:?}");
    }

    /// #708: a hung engine (nothing heard for `ENGINE_SILENCE`) stands the window on the display
    /// with the menu bar (the operator's) back, and nothing else: what it draws and every stage's
    /// state are unchanged, why goes to the log once. Any word from the engine covers again.
    #[test]
    fn a_silent_engine_stands_only_the_menu_bars_window_back_until_it_speaks() {
        let mut h = menu_bar_host();
        // Times from when the engine was last heard.
        let heard = h.heard;
        let mut out = Vec::new();
        h.check_silence(heard + ENGINE_SILENCE / 2, &mut out);
        assert!(out.is_empty() && h.platform.stood_back.is_none(), "not yet");
        h.check_silence(heard + ENGINE_SILENCE, &mut out);
        assert_eq!(
            h.platform.stood_back.as_deref(),
            Some("M"),
            "the operator's display"
        );
        assert_eq!(h.platform.shown.get("M"), Some(&vec![id(1)]), "still drawn");
        assert_eq!(
            h.platform.shown.get("D"),
            Some(&vec![id(2)]),
            "the audience's kept"
        );
        assert!(states(&out).is_empty(), "{out:?}");
        assert!(matches!(&logs(&out)[..], [(LinkLevel::Warn, m)] if m.contains("menu bar")));
        // Said once; still back at the next frame-loop tick.
        out.clear();
        h.check_silence(heard + ENGINE_SILENCE * 2, &mut out);
        h.step(None, &mut out);
        assert!(logs(&out).is_empty());
        assert_eq!(h.platform.stood_back.as_deref(), Some("M"));
        // The engine speaks (a ping): it covers again.
        send(&mut h, ToStage::Ping { seq: 1 });
        assert_eq!(h.platform.stood_back, None);
    }

    pub(crate) fn id(n: u8) -> StageId {
        StageId(uuid::Uuid::from_u128(n.into()))
    }

    fn display(id: &str) -> Display {
        Display {
            id: id.into(),
            name: "Projector".into(),
            width: 1920,
            height: 1080,
            fps: 60,
            main: false,
        }
    }

    pub(crate) fn route_id(n: u8) -> RouteId {
        RouteId(uuid::Uuid::from_u128(n.into()))
    }

    /// A stage of the tests: stage `n` with one region covering it on route `n`, which is on
    /// `display`.
    #[derive(Clone)]
    pub(crate) struct TestStage {
        pub(crate) out: StageOutput,
        pub(crate) display: Option<String>,
    }

    pub(crate) fn stage(n: u8, display: Option<&str>, keep_black: bool) -> TestStage {
        TestStage {
            out: StageOutput {
                id: id(n),
                width: 1920,
                height: 1080,
                regions: vec![openlab_core::stage_link::RegionOutput {
                    route: route_id(n),
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                }],
                keep_black,
                stack: u32::from(n),
                monitor: None,
            },
            display: display.map(Into::into),
        }
    }

    /// The stage set of `stages`, each with its own route (same number) on its display.
    pub(crate) fn stages_msg(stages: Vec<TestStage>) -> ToStage {
        let routes = stages
            .iter()
            .flat_map(|s| {
                s.out
                    .regions
                    .iter()
                    .map(move |r| (r.route, s.display.clone()))
            })
            .map(|(id, display)| RouteOutput {
                id,
                width: 1920,
                height: 1080,
                rotation: 0,
                scaling: RouteScaling::Fill,
                display,
                guide: None,
            })
            .collect();
        ToStage::Stages {
            stages: stages.into_iter().map(|s| s.out).collect(),
            routes,
        }
    }

    pub(crate) fn host(displays: &[&str]) -> Host<Fake> {
        Host::new(Fake {
            displays: displays.iter().map(|d| display(d)).collect(),
            shown: HashMap::new(),
            places: HashMap::new(),
            guides: HashMap::new(),
            pictures_drawn: HashMap::new(),
            menu_bar: None,
            stood_back: None,
            fail: false,
            skip: false,
            presents: 0,
            closes: 0,
            live: false,
            events: Vec::new(),
            monitors: HashMap::new(),
            user_closed: Vec::new(),
            raised: Vec::new(),
            fail_monitors: false,
            operator: None,
            keeps: 0,
        })
    }

    /// The route states reported, each named by the stage of the same number (each test stage
    /// has its own route).
    fn states(out: &[FromStage]) -> Vec<(StageId, RouteState)> {
        out.iter()
            .filter_map(|m| match m {
                FromStage::RouteStatus { route, state } => Some((StageId(route.0), state.clone())),
                _ => None,
            })
            .collect()
    }

    /// VID-01, VID-12: a stage with keep-black on and its display connected is shown; one with
    /// no display or a missing one is not shown and says why; keep-black off hides it.
    #[test]
    fn each_stage_is_shown_on_its_display_or_says_why_not() {
        let mut h = host(&["D1"]);
        let mut out = Vec::new();
        h.handle(
            ToStage::Hello {
                version: LINK_VERSION,
            },
            &mut out,
        );
        assert!(matches!(out[0], FromStage::Hello { video: true, .. }));
        // The only display is the main one.
        assert_eq!(
            out[1],
            FromStage::Displays {
                displays: vec![Display {
                    main: true,
                    ..display("D1")
                }]
            }
        );
        out.clear();
        let stages = vec![
            stage(1, Some("D1"), true),
            stage(2, None, true),
            stage(3, Some("D9"), true),
            stage(4, Some("D1"), false),
        ];
        h.handle(stages_msg(stages.clone()), &mut out);
        assert_eq!(
            states(&out),
            vec![
                (
                    id(1),
                    RouteState::Shown {
                        display: "D1".into()
                    }
                ),
                (id(2), RouteState::NoDisplay),
                (
                    id(3),
                    RouteState::DisplayMissing {
                        display: "D9".into()
                    }
                ),
                (id(4), RouteState::Hidden),
            ]
        );
        assert_eq!(h.platform.shown["D1"], [id(1)]);
        // The same set again is reported again (the engine waits for each set's states) and
        // changes no window.
        out.clear();
        h.handle(stages_msg(stages), &mut out);
        assert_eq!(states(&out).len(), 4);
        assert_eq!(h.platform.shown["D1"], [id(1)]);
        // A display change reports only what changed.
        out.clear();
        h.displays_changed(&mut out);
        assert!(out.is_empty(), "{out:?}");
    }

    /// Stages on one display are drawn in one window, bottom first by their stack rank.
    #[test]
    fn stages_sharing_a_display_are_presented_bottom_first() {
        let mut h = host(&["D1"]);
        let mut out = Vec::new();
        h.handle(
            ToStage::Hello {
                version: LINK_VERSION,
            },
            &mut out,
        );
        let mut top = stage(1, Some("D1"), true);
        top.out.stack = 9;
        let bottom = stage(2, Some("D1"), true);
        h.handle(stages_msg(vec![top, bottom]), &mut out);
        assert_eq!(h.platform.shown["D1"], [id(2), id(1)]);
        h.handle(stages_msg(vec![]), &mut out);
        assert!(
            h.platform.shown.is_empty(),
            "the window goes with its last stage"
        );
    }

    /// A display unplugged takes its stage down and reports it; plugged back, the stage
    /// returns.
    #[test]
    fn a_display_that_goes_and_comes_back_takes_its_stage_with_it() {
        let mut h = host(&["D1"]);
        let mut out = Vec::new();
        h.handle(
            ToStage::Hello {
                version: LINK_VERSION,
            },
            &mut out,
        );
        h.handle(stages_msg(vec![stage(1, Some("D1"), true)]), &mut out);
        out.clear();
        h.platform.displays.clear();
        h.displays_changed(&mut out);
        assert_eq!(out[0], FromStage::Displays { displays: vec![] });
        assert_eq!(
            states(&out),
            vec![(
                id(1),
                RouteState::DisplayMissing {
                    display: "D1".into()
                }
            )]
        );
        assert!(h.platform.shown.is_empty());
        out.clear();
        h.platform.displays.push(display("D1"));
        h.displays_changed(&mut out);
        assert_eq!(
            states(&out),
            vec![(
                id(1),
                RouteState::Shown {
                    display: "D1".into()
                }
            )]
        );
    }

    /// A display change that leaves the list as it was (the menu bar moved to another display)
    /// still presents every shown stage again, so a window standing back does so on the display
    /// that has the menu bar now, at once.
    #[test]
    fn a_display_change_with_the_same_displays_presents_the_shown_stages_again() {
        let mut h = host(&["D1", "D2"]);
        let mut out = Vec::new();
        h.handle(
            ToStage::Hello {
                version: LINK_VERSION,
            },
            &mut out,
        );
        h.handle(
            stages_msg(vec![stage(1, Some("D1"), true), stage(2, Some("D2"), true)]),
            &mut out,
        );
        h.platform.shown.clear();
        out.clear();
        h.displays_changed(&mut out);
        assert_eq!(h.platform.shown["D1"], [id(1)]);
        assert_eq!(h.platform.shown["D2"], [id(2)]);
        assert!(out.is_empty(), "nothing changed to report: {out:?}");
    }

    #[test]
    fn a_stage_that_cannot_be_drawn_fails_with_the_reason_and_a_removed_stage_goes() {
        let mut h = host(&["D1"]);
        h.platform.fail = true;
        let mut out = Vec::new();
        h.handle(
            ToStage::Hello {
                version: LINK_VERSION,
            },
            &mut out,
        );
        out.clear();
        h.handle(stages_msg(vec![stage(1, Some("D1"), true)]), &mut out);
        assert_eq!(
            states(&out),
            vec![(
                id(1),
                RouteState::Failed {
                    reason: "no GPU".into()
                }
            )]
        );
        h.platform.fail = false;
        h.handle(stages_msg(vec![stage(1, Some("D1"), true)]), &mut out);
        assert_eq!(h.platform.shown.len(), 1);
        h.handle(stages_msg(vec![]), &mut out);
        assert!(h.platform.shown.is_empty());
    }

    /// #789 flow: a frame for which the display gives no drawable is skipped: the window stays
    /// up, nothing is marked failed, one warning per stretch of skipped frames; a real failure
    /// still fails the stage.
    #[test]
    fn a_frame_with_no_drawable_is_skipped_and_the_window_stays_up() {
        let mut h = host(&["D1"]);
        let mut out = Vec::new();
        h.handle(
            ToStage::Hello {
                version: LINK_VERSION,
            },
            &mut out,
        );
        h.handle(stages_msg(vec![stage(1, Some("D1"), true)]), &mut out);
        let shown = RouteState::Shown {
            display: "D1".into(),
        };
        assert_eq!(states(&out), vec![(id(1), shown.clone())]);
        let warnings = |out: &[FromStage]| {
            logs(out)
                .into_iter()
                .filter(|(level, _)| *level == LinkLevel::Warn)
                .count()
        };
        for stretch in 0..2 {
            out.clear();
            h.platform.skip = true;
            for _ in 0..5 {
                h.displays_changed(&mut out);
            }
            assert_eq!(states(&out), vec![], "stretch {stretch}: a state changed");
            assert!(h.platform.shown.contains_key("D1"), "the window was closed");
            assert_eq!(warnings(&out), 1, "stretch {stretch}: {out:?}");
            h.platform.skip = false;
            h.displays_changed(&mut out);
            assert_eq!(states(&out), vec![]);
            assert_eq!(warnings(&out), 1);
        }
        // A window closed mid-stretch: skipped frames in the next one warn again.
        h.platform.skip = true;
        h.displays_changed(&mut out);
        h.handle(stages_msg(vec![]), &mut out);
        out.clear();
        h.handle(stages_msg(vec![stage(1, Some("D1"), true)]), &mut out);
        assert_eq!(warnings(&out), 1, "{out:?}");
        h.platform.skip = false;
        h.platform.fail = true;
        out.clear();
        h.displays_changed(&mut out);
        assert_eq!(
            states(&out),
            vec![(
                id(1),
                RouteState::Failed {
                    reason: "no GPU".into()
                }
            )]
        );
        assert!(!h.platform.shown.contains_key("D1"));
    }

    #[test]
    fn another_link_version_is_answered_then_the_host_quits() {
        let mut h = host(&[]);
        let mut out = Vec::new();
        assert_eq!(
            h.handle(ToStage::Hello { version: 99 }, &mut out),
            Flow::Quit
        );
        assert!(matches!(
            out[0],
            FromStage::Hello {
                version: LINK_VERSION,
                ..
            }
        ));
    }

    /// The loop answers pings, skips bad and over-long lines with a warning, and ends when its
    /// input ends (the engine is gone), so the stage host is never orphaned.
    #[test]
    fn the_loop_skips_bad_lines_and_ends_with_its_input() {
        let mut input = Vec::new();
        input.extend_from_slice(b"{\"type\":\"ping\",\"seq\":1}\n");
        input.extend_from_slice(b"garbage\n");
        input.extend(std::iter::repeat_n(b'x', MAX_LINE_BYTES + 10));
        input.extend_from_slice(b"\n{\"type\":\"ping\",\"seq\":2}");
        let mut output = Vec::new();
        let mut h = host(&[]);
        let flow = serve(std::io::Cursor::new(input), &mut output, |m, out| {
            h.step(m, out)
        });
        assert_eq!(flow, Flow::Quit);
        let text = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "{text}");
        assert!(lines[0].contains("pong"));
        assert!(lines[1].contains("ignored a message"));
        assert!(lines[2].contains(&format!("{} bytes", MAX_LINE_BYTES + 10)));
        assert!(lines[3].contains("\"seq\":2"));
    }

    #[test]
    fn a_quit_takes_every_window_down_and_ends_the_loop() {
        let mut h = host(&["D1"]);
        let mut out = Vec::new();
        h.handle(
            ToStage::Hello {
                version: LINK_VERSION,
            },
            &mut out,
        );
        h.handle(stages_msg(vec![stage(1, Some("D1"), true)]), &mut out);
        assert_eq!(h.handle(ToStage::Quit, &mut out), Flow::Quit);
        assert!(h.platform.shown.is_empty());
    }

    /// A host that has said hello, so it knows its displays.
    fn greeted(displays: &[&str]) -> Host<Fake> {
        let mut h = host(displays);
        send(
            &mut h,
            ToStage::Hello {
                version: LINK_VERSION,
            },
        );
        h
    }

    fn watched(n: u8, display: Option<&str>) -> TestStage {
        let mut s = stage(n, display, false);
        s.out.monitor = Some(format!("Stage {n}"));
        s
    }

    fn monitor_closed(out: &[FromStage]) -> Vec<StageId> {
        out.iter()
            .filter_map(|m| match m {
                FromStage::MonitorClosed { stage } => Some(*stage),
                _ => None,
            })
            .collect()
    }

    /// #648 flows: a monitor shows its stage whether the stage is shown on a display, hidden
    /// between cues or has no display at all, with what its cues draw; it goes when the engine
    /// stops asking for it, and with a quit.
    #[test]
    fn a_monitor_draws_its_stage_shown_or_not_and_goes_with_its_stage_set() {
        let mut h = greeted(&["D1"]);
        let mut out = Vec::new();
        send(
            &mut h,
            stages_msg(vec![
                watched(1, Some("D1")),
                watched(2, None),
                stage(3, None, true),
            ]),
        );
        assert_eq!(h.platform.monitors.len(), 2);
        assert_eq!(h.platform.monitors[&id(1)].0, "Stage 1");
        assert_eq!(h.platform.monitors[&id(2)].1.stage, id(2));
        assert_eq!(h.states.get(&route_id(2)), Some(&RouteState::NoDisplay));
        assert_eq!(h.states.get(&route_id(1)), Some(&RouteState::Hidden));
        // A cue on the stage with no display: the monitor draws it all the same.
        load(&mut h, 7, 2);
        play(&mut h, 7, 0);
        clock(&mut h, 4800);
        let drawn = &h.platform.monitors[&id(2)].1.cues;
        assert_eq!(drawn.iter().map(|c| c.picture).collect::<Vec<_>>(), [7]);
        // Renamed: the title follows; dropped from the set: the window goes.
        let mut renamed = watched(1, Some("D1"));
        renamed.out.monitor = Some("Front".into());
        h.handle(stages_msg(vec![renamed, stage(2, None, false)]), &mut out);
        assert_eq!(h.platform.monitors[&id(1)].0, "Front");
        assert!(!h.platform.monitors.contains_key(&id(2)));
        assert!(
            monitor_closed(&out).is_empty(),
            "the engine's own close is not echoed"
        );
        assert_eq!(h.handle(ToStage::Quit, &mut out), Flow::Quit);
        assert!(h.platform.monitors.is_empty());
    }

    /// The operator closes a monitor window: the engine is told once; a stage set sent before
    /// the engine heard of it does not open it again; a raise does.
    #[test]
    fn a_monitor_the_operator_closes_stays_closed_until_raised() {
        let mut h = greeted(&["D1"]);
        send(&mut h, stages_msg(vec![watched(1, Some("D1"))]));
        h.platform.user_closed.push(id(1));
        let mut out = Vec::new();
        h.step(None, &mut out);
        assert_eq!(monitor_closed(&out), [id(1)]);
        let out = send(&mut h, stages_msg(vec![watched(1, Some("D1"))]));
        assert!(monitor_closed(&out).is_empty());
        assert!(h.platform.monitors.is_empty(), "stays closed");
        let mut out = Vec::new();
        h.step(None, &mut out);
        assert!(monitor_closed(&out).is_empty(), "said once");
        send(&mut h, ToStage::RaiseMonitor { stage: id(1) });
        assert!(h.platform.monitors.contains_key(&id(1)), "opened again");
        assert_eq!(h.platform.raised, [id(1)]);
        // A raise for a stage with no monitor asked for does nothing.
        send(&mut h, ToStage::RaiseMonitor { stage: id(9) });
        assert!(!h.platform.monitors.contains_key(&id(9)));
    }

    /// A monitor that cannot be drawn closes with one warning, and the stages' output is
    /// untouched.
    #[test]
    fn a_monitor_that_cannot_be_drawn_closes_with_a_warning_and_the_stage_stays_up() {
        let mut h = greeted(&["D1"]);
        h.platform.fail_monitors = true;
        let out = send(
            &mut h,
            stages_msg(vec![{
                let mut w = watched(1, Some("D1"));
                w.out.keep_black = true;
                w
            }]),
        );
        assert_eq!(monitor_closed(&out), [id(1)]);
        assert!(
            logs(&out)
                .iter()
                .any(|(l, m)| *l == LinkLevel::Warn && m.contains("monitor"))
        );
        assert_eq!(
            states(&out),
            vec![(
                id(1),
                RouteState::Shown {
                    display: "D1".into()
                }
            )]
        );
        let out = send(&mut h, ToStage::Ping { seq: 1 });
        assert!(monitor_closed(&out).is_empty());
    }

    fn routed(
        id: u8,
        size: (u32, u32),
        rotation: u16,
        scaling: RouteScaling,
        display: Option<&str>,
    ) -> RouteOutput {
        RouteOutput {
            id: route_id(id),
            width: size.0,
            height: size.1,
            rotation,
            scaling,
            display: display.map(Into::into),
            guide: None,
        }
    }

    fn region(route: u8, x: u32, width: u32) -> openlab_core::stage_link::RegionOutput {
        openlab_core::stage_link::RegionOutput {
            route: route_id(route),
            x,
            y: 0,
            width,
            height: 1080,
        }
    }

    /// The G4 stage: 3840 x 1080, region A (left half) on route 1 on D1, region B (right half)
    /// on route 2 on D2, keeping black as `keep_black` says.
    fn split_stage(keep_black: bool) -> ToStage {
        let (stages, routes) = split_parts(keep_black);
        ToStage::Stages { stages, routes }
    }

    fn split_parts(keep_black: bool) -> (Vec<StageOutput>, Vec<RouteOutput>) {
        (
            vec![StageOutput {
                id: id(1),
                width: 3840,
                height: 1080,
                regions: vec![region(1, 0, 1920), region(2, 1920, 1920)],
                keep_black,
                stack: 0,
                monitor: None,
            }],
            vec![
                routed(1, (1920, 1080), 0, RouteScaling::Fill, Some("D1")),
                routed(2, (1920, 1080), 0, RouteScaling::Fill, Some("D2")),
            ],
        )
    }

    fn near(a: [f64; 2], b: [f64; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9
    }

    /// G8: a region fills its route whatever its size: a 960 x 1080 region's corners land on the
    /// corners of a 1920 x 1080 route filling a display of its shape.
    #[test]
    fn a_region_fills_its_route() {
        let place = Placement {
            region: [960, 0, 960, 1080],
            route: (1920, 1080),
            rotation: 0,
            scaling: RouteScaling::Fill,
            screen: None,
        };
        let size = (1920, 1080);
        assert!(near(place.map(size, [960.0, 0.0]), [0.0, 0.0]));
        assert!(near(place.map(size, [1920.0, 1080.0]), [1.0, 1.0]));
        assert!(near(place.map(size, [1440.0, 540.0]), [0.5, 0.5]));
        assert_eq!(place.dest(size), [0.0, 0.0, 1.0, 1.0]);
    }

    /// R5: a 1920 x 1080 route on a 1280 x 1024 display. Centre: one route pixel per display
    /// pixel, cut off at every edge. Fit: as wide as the display, black above and below.
    /// Stretch: the display's shape. Fill: covering, the sides cut off.
    #[test]
    fn each_scaling_puts_the_route_on_its_display_as_it_says() {
        let at = |scaling| {
            Placement {
                region: [0, 0, 1920, 1080],
                route: (1920, 1080),
                rotation: 0,
                scaling,
                screen: None,
            }
            .dest((1280, 1024))
        };
        let centre = at(RouteScaling::Centre);
        assert!(centre[0] < 0.0 && centre[2] > 1.0, "{centre:?}");
        assert!((centre[2] - centre[0] - 1920.0 / 1280.0).abs() < 1e-9);
        assert!((centre[3] - centre[1] - 1080.0 / 1024.0).abs() < 1e-9);
        let fit = at(RouteScaling::Fit);
        assert_eq!((fit[0], fit[2]), (0.0, 1.0));
        assert!(
            fit[1] > 0.0 && fit[3] < 1.0,
            "black above and below: {fit:?}"
        );
        assert!((fit[3] - fit[1] - 720.0 / 1024.0).abs() < 1e-9);
        assert_eq!(at(RouteScaling::Stretch), [0.0, 0.0, 1.0, 1.0]);
        let fill = at(RouteScaling::Fill);
        assert_eq!((fill[1], fill[3]), (0.0, 1.0));
        assert!(
            fill[0] < 0.0 && fill[2] > 1.0,
            "the sides cut off: {fill:?}"
        );
        assert!((fill[2] - fill[0] - (1024.0 * 1920.0 / 1080.0) / 1280.0).abs() < 1e-9);
        // Drawn smaller (a probe), the display's own size still decides: centre is the same.
        let on_display = Placement {
            region: [0, 0, 1920, 1080],
            route: (1920, 1080),
            rotation: 0,
            scaling: RouteScaling::Centre,
            screen: Some((1280, 1024)),
        };
        assert_eq!(on_display.dest((640, 512)), centre);
    }

    /// R6: a quarter turn is clockwise: the route's top edge runs down the display's right side;
    /// 180 turns it upside down; 270 takes the top edge down the left side. A turned 1920 x
    /// 1080 route fills a portrait 1080 x 1920 display.
    #[test]
    fn a_route_turns_clockwise_by_its_rotation() {
        let turned = |rotation| Placement {
            region: [0, 0, 1920, 1080],
            route: (1920, 1080),
            rotation,
            scaling: RouteScaling::Fill,
            screen: None,
        };
        let portrait = (1080, 1920);
        let q = turned(90);
        assert_eq!(q.dest(portrait), [0.0, 0.0, 1.0, 1.0]);
        assert!(
            near(q.map(portrait, [0.0, 0.0]), [1.0, 0.0]),
            "top left to top right"
        );
        assert!(
            near(q.map(portrait, [1920.0, 0.0]), [1.0, 1.0]),
            "top edge down the right"
        );
        assert!(near(q.map(portrait, [0.0, 1080.0]), [0.0, 0.0]));
        let h = turned(180);
        assert!(near(h.map((1920, 1080), [0.0, 0.0]), [1.0, 1.0]));
        let t = turned(270);
        assert!(near(t.map(portrait, [0.0, 0.0]), [0.0, 1.0]));
        assert!(near(t.map(portrait, [1920.0, 0.0]), [0.0, 0.0]));
        let none = turned(0);
        assert!(near(none.map((1920, 1080), [1920.0, 0.0]), [1.0, 0.0]));
    }

    /// G4, V1c: a stage with two regions on two routes on two displays is drawn on both, each
    /// display its own part, and keeping black covers both; with keep-black off and nothing
    /// playing, both show nothing. Each route reports its own state.
    #[test]
    fn a_stage_split_over_two_routes_draws_each_part_on_its_display() {
        let mut h = greeted(&["D1", "D2"]);
        let out = send(&mut h, split_stage(true));
        let shown = |d: &str| RouteState::Shown { display: d.into() };
        let got: Vec<(RouteId, RouteState)> = out
            .iter()
            .filter_map(|m| match m {
                FromStage::RouteStatus { route, state } => Some((*route, state.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            got,
            [(route_id(1), shown("D1")), (route_id(2), shown("D2"))]
        );
        assert_eq!(h.platform.shown["D1"], [id(1)]);
        assert_eq!(h.platform.shown["D2"], [id(1)]);
        assert_eq!(h.platform.places["D1"][0].region, [0, 0, 1920, 1080]);
        assert_eq!(h.platform.places["D2"][0].region, [1920, 0, 1920, 1080]);
        // Keep-black off with nothing playing: both displays go back to the desktop at once.
        let out = send(&mut h, split_stage(false));
        assert!(h.platform.shown.is_empty(), "{:?}", h.platform.shown);
        assert_eq!(
            out.iter()
                .filter(|m| matches!(
                    m,
                    FromStage::RouteStatus {
                        state: RouteState::Hidden,
                        ..
                    }
                ))
                .count(),
            2
        );
    }

    /// G5, G7, R9: a region whose route is taken away (none, removed, or the route itself
    /// removed) leaves that display: its window closes; the other display keeps its part.
    #[test]
    fn a_region_off_its_route_leaves_that_display_only() {
        let mut h = greeted(&["D1", "D2"]);
        send(&mut h, split_stage(true));
        let (mut stages, mut routes) = split_parts(true);
        stages[0].regions.pop();
        send(
            &mut h,
            ToStage::Stages {
                stages: stages.clone(),
                routes: routes.clone(),
            },
        );
        assert_eq!(h.platform.shown["D1"], [id(1)]);
        assert!(
            !h.platform.shown.contains_key("D2"),
            "D2 back to the desktop"
        );
        assert_eq!(h.states.get(&route_id(2)), Some(&RouteState::Hidden));
        routes.pop();
        send(&mut h, ToStage::Stages { stages, routes });
        assert_eq!(h.platform.shown["D1"], [id(1)]);
        assert!(
            !h.states.contains_key(&route_id(2)),
            "a removed route has no state"
        );
    }

    /// R7: a route moved to another display takes its picture with it at once; the picture's
    /// run is untouched.
    #[test]
    fn a_route_moved_to_another_display_takes_its_picture_with_it() {
        let mut h = greeted(&["D1", "D2"]);
        let one = |display| ToStage::Stages {
            stages: vec![stage(1, None, true).out],
            routes: vec![routed(
                1,
                (1920, 1080),
                0,
                RouteScaling::Fill,
                Some(display),
            )],
        };
        send(&mut h, one("D1"));
        load(&mut h, 7, 1);
        clock(&mut h, 0);
        play(&mut h, 7, 0);
        clock(&mut h, 500);
        assert_eq!(h.platform.pictures_drawn["D1"], [7]);
        send(&mut h, one("D2"));
        assert!(!h.platform.shown.contains_key("D1"));
        assert_eq!(h.platform.pictures_drawn["D2"], [7]);
        assert_eq!(cues(&mut h, 1)[0].1, 0.5, "at its current point");
    }

    /// A failed display fails only the routes on it.
    #[test]
    fn a_failed_display_fails_only_its_routes() {
        let mut h = greeted(&["D1", "D2"]);
        h.platform.fail = true;
        let out = send(&mut h, split_stage(true));
        assert!(out.iter().all(|m| !matches!(
            m,
            FromStage::RouteStatus {
                state: RouteState::Shown { .. },
                ..
            }
        )));
        h.platform.fail = false;
        let (stages, mut routes) = split_parts(true);
        routes[1].display = Some("D9".into());
        let out = send(&mut h, ToStage::Stages { stages, routes });
        let got: Vec<(RouteId, RouteState)> = out
            .iter()
            .filter_map(|m| match m {
                FromStage::RouteStatus { route, state } => Some((*route, state.clone())),
                _ => None,
            })
            .collect();
        assert!(got.contains(&(
            route_id(1),
            RouteState::Shown {
                display: "D1".into()
            }
        )));
        assert!(got.contains(&(
            route_id(2),
            RouteState::DisplayMissing {
                display: "D9".into()
            }
        )));
    }

    /// The display with the menu bar is the main one; without one known, the first listed.
    #[test]
    fn the_display_with_the_menu_bar_is_the_main_one() {
        let mut h = host(&["D1", "D2"]);
        h.platform.menu_bar = Some("D2".into());
        let out = send(
            &mut h,
            ToStage::Hello {
                version: LINK_VERSION,
            },
        );
        let main: Vec<(String, bool)> = out
            .iter()
            .find_map(|m| match m {
                FromStage::Displays { displays } => {
                    Some(displays.iter().map(|d| (d.id.clone(), d.main)).collect())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(main, [("D1".into(), false), ("D2".into(), true)]);
    }

    /// V1: keep-black unticked on a stage over two displays takes each window down once, with
    /// nothing drawn first; ticked again, each comes back once, drawn black.
    #[test]
    fn keep_black_off_and_on_changes_each_window_once() {
        let mut h = greeted(&["D1", "D2"]);
        send(&mut h, split_stage(true));
        let (presents, closes) = (h.platform.presents, h.platform.closes);
        send(&mut h, split_stage(false));
        assert_eq!(
            h.platform.presents, presents,
            "nothing drawn on the way down"
        );
        assert_eq!(h.platform.closes, closes + 2, "each window down once");
        let closes = h.platform.closes;
        send(&mut h, split_stage(true));
        assert_eq!(h.platform.presents, presents + 2, "each drawn once");
        assert_eq!(
            h.platform.closes, closes,
            "nothing taken down on the way up"
        );
        assert_eq!(h.platform.shown["D1"], [id(1)]);
        assert_eq!(h.platform.shown["D2"], [id(1)]);
    }

    /// The split stage with keep-black as given and guides on the routes named (by number),
    /// each with its name and a colour of its own.
    fn guided(keep_black: bool, on: &[u8]) -> ToStage {
        let (stages, mut routes) = split_parts(keep_black);
        for (n, r) in (1u8..).zip(&mut routes) {
            r.guide = on
                .contains(&n)
                .then(|| openlab_core::stage_link::RouteGuide {
                    name: format!("Output {n}"),
                    colour: openlab_core::GuideColour([n, 200, 255]),
                });
        }
        ToStage::Stages { stages, routes }
    }

    fn guide_names(h: &Host<Fake>, display: &str) -> Vec<String> {
        (h.platform.guides.get(display).into_iter().flatten())
            .map(|g| g.name.clone())
            .collect()
    }

    /// #821 GD1, GD2: a route with guides on is shown on its display with nothing else to draw
    /// (no black, no picture): the display draws its guides alone, named and coloured as sent,
    /// placed as the route's whole raster. Another route's display is untouched. Guides off
    /// again take the window down at once (the desktop is back); each route's guides come and
    /// go alone.
    #[test]
    fn a_route_with_guides_on_is_shown_with_them_alone_and_hidden_when_they_go() {
        let mut h = greeted(&["D1", "D2"]);
        let out = send(&mut h, guided(false, &[2]));
        assert!(states(&out).contains(&(
            id(2),
            RouteState::Shown {
                display: "D2".into()
            }
        )));
        assert!(states(&out).contains(&(id(1), RouteState::Hidden)));
        assert!(!h.platform.shown.contains_key("D1"), "D1 untouched");
        assert_eq!(
            h.platform.shown["D2"],
            Vec::<StageId>::new(),
            "no stage drawn"
        );
        assert_eq!(guide_names(&h, "D2"), ["Output 2"]);
        let g = &h.platform.guides["D2"][0];
        assert_eq!(g.colour, [2, 200, 255]);
        assert_eq!(g.place.region, [0, 0, 1920, 1080]);
        assert_eq!(g.place.route, (1920, 1080));
        // Both on: each display its own.
        send(&mut h, guided(false, &[1, 2]));
        assert_eq!(guide_names(&h, "D1"), ["Output 1"]);
        assert_eq!(guide_names(&h, "D2"), ["Output 2"]);
        // Output 2 off: D2's window goes, D1 keeps its guides.
        let closes = h.platform.closes;
        let out = send(&mut h, guided(false, &[1]));
        assert!(states(&out).contains(&(id(2), RouteState::Hidden)));
        assert_eq!(h.platform.closes, closes + 1);
        assert!(!h.platform.shown.contains_key("D2"));
        assert_eq!(guide_names(&h, "D1"), ["Output 1"]);
    }

    /// #821 GD5, GD6: guides on a route keeping black, or with a picture playing, are drawn over
    /// what the route shows; off again, the black (or picture) stays with no window taken down
    /// in between, and the picture plays on.
    #[test]
    fn guides_draw_over_black_and_pictures_and_leave_them_as_they_were() {
        let mut h = greeted(&["D1", "D2"]);
        send(&mut h, guided(true, &[]));
        load(&mut h, 7, 1);
        clock(&mut h, 0);
        play(&mut h, 7, 0);
        clock(&mut h, 500);
        let closes = h.platform.closes;
        send(&mut h, guided(true, &[1]));
        assert_eq!(h.platform.shown["D1"], [id(1)], "the stage under them");
        assert_eq!(h.platform.pictures_drawn["D1"], [7]);
        assert_eq!(guide_names(&h, "D1"), ["Output 1"]);
        clock(&mut h, 1000);
        send(&mut h, guided(true, &[]));
        assert_eq!(h.platform.closes, closes, "no window down on the way");
        assert_eq!(h.platform.shown["D1"], [id(1)]);
        assert!(guide_names(&h, "D1").is_empty());
        assert_eq!(cues(&mut h, 1)[0].1, 1.0, "the picture ran on underneath");
    }

    /// #821 GD13: guides on a route whose display is unplugged draw nothing and fail nothing
    /// (the route says its display is missing); the display back, they are drawn at once.
    /// The app gone takes them down with every window.
    #[test]
    fn guides_wait_for_their_display_and_go_with_the_app() {
        let mut h = greeted(&["D1", "D2"]);
        send(&mut h, guided(false, &[1]));
        let mut out = Vec::new();
        h.platform.displays.retain(|d| d.id != "D1");
        h.displays_changed(&mut out);
        assert!(states(&out).contains(&(
            id(1),
            RouteState::DisplayMissing {
                display: "D1".into()
            }
        )));
        assert!(h.platform.guides.get("D1").is_none());
        out.clear();
        h.platform.displays.push(display("D1"));
        h.displays_changed(&mut out);
        assert!(states(&out).contains(&(
            id(1),
            RouteState::Shown {
                display: "D1".into()
            }
        )));
        assert_eq!(guide_names(&h, "D1"), ["Output 1"]);
        send(
            &mut h,
            ToStage::App {
                connected: false,
                pid: None,
            },
        );
        assert!(h.platform.guides.get("D1").is_none());
    }

    /// #821 GD4: a guide's lines are a few display pixels thick whatever the route's size and
    /// however small a probe draws it; the outline lies on the route's edges and the centre
    /// lines cross at its middle; the name sits in the top left quarter, held to its width.
    #[test]
    fn a_guide_is_drawn_on_the_routes_edges_at_a_display_thickness() {
        let guide = |route: (u32, u32), screen| Guide {
            route: route_id(1),
            name: "Output 1".into(),
            colour: [0, 200, 255],
            place: Placement {
                region: [0, 0, route.0, route.1],
                route,
                rotation: 0,
                scaling: RouteScaling::Stretch,
                screen: Some(screen),
            },
        };
        let g = guide((1920, 1080), (1920, 1080));
        let bars = g.bars((1920, 1080));
        assert_eq!(
            bars[0],
            [0.0, 0.0, 1920.0, 3.0],
            "top outline, 3 pixels on 1080 lines"
        );
        assert_eq!(bars[3], [1917.0, 0.0, 1920.0, 1080.0], "right outline");
        assert_eq!(
            bars[4],
            [0.0, 538.5, 1920.0, 541.5],
            "horizontal centre line"
        );
        // Drawn smaller (a probe) the bars are the same in route pixels.
        assert_eq!(g.bars((960, 540)), bars);
        // A route of twice the display's pixels, stretched onto it: twice the route pixels.
        let big = guide((3840, 2160), (1920, 1080));
        assert_eq!(big.bars((1920, 1080))[0], [0.0, 0.0, 3840.0, 6.0]);
        assert_eq!(g.label_height((1920, 1080)), 65);
        assert_eq!(
            big.label_height((1920, 1080)),
            65,
            "the same on the display"
        );
        let [l, t, r, b] = g.label_box((1920, 1080), (260, 65));
        assert_eq!([l, t, r, b], [350.0, 237.5, 610.0, 302.5]);
        // A long name is held to its quarter's width, its shape kept.
        let [l, _, r, _] = g.label_box((1920, 1080), (4000, 65));
        assert!((r - l - 768.0).abs() < 1e-9, "{l} {r}");
        // A tiny route on a big display: the name is never under its least height.
        assert_eq!(
            guide((8, 8), (8, 8)).label_height((8, 8)),
            GUIDE_LABEL_LEAST_PX
        );
    }
}
