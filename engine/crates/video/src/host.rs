// SPDX-License-Identifier: GPL-3.0-or-later

//! The stage host's state, the same on every platform: which stages the engine wants, which
//! displays exist, and where each stage is. A [`Platform`] does the drawing and the windows.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, Write};
use std::time::Instant;

use openlab_core::StageId;
use openlab_core::stage_link::{
    Display, FromStage, LINK_VERSION, Line, LinkLevel, StageOutput, StageState, ToStage,
    decode_to_stage, encode, read_line,
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
    /// 0 (gone) to 1.
    pub opacity: f32,
}

/// One stage as a display draws it: its pixel size, what it shows behind its cues, and its
/// cues' pictures, the earliest started first (so the later-started is drawn on top).
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub stage: StageId,
    pub width: u32,
    pub height: u32,
    pub picture: Picture,
    pub cues: Vec<CuePicture>,
}

/// Where a picture is in its run.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Run {
    /// Loaded at `offset`, not playing yet.
    Held { offset: f64 },
    /// Playing from sample `at`, `offset` seconds into the file then.
    Playing { at: u64, offset: f64 },
    /// Holding the frame at `offset` from sample `at`.
    Paused { at: u64, offset: f64 },
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
    /// Fading out from sample `.0` over `.1` samples, then gone.
    stop: Option<(u64, u64)>,
    /// When it first played, for the stacking (later on top): (sample, arrival order).
    order: (u64, u64),
}

impl Pic {
    fn position(run: Run, clock: u64, rate: u32) -> f64 {
        match run {
            Run::Held { offset } | Run::Paused { offset, .. } => offset,
            Run::Playing { at, offset } => {
                offset + clock.saturating_sub(at) as f64 / f64::from(rate.max(1))
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
            self.run
        } else {
            self.before.unwrap_or(self.run)
        };
        let opacity = match self.stop {
            Some((at, _)) if clock < at => 1.0,
            Some((at, fade)) => {
                let gone = clock - at;
                if gone >= fade {
                    return None;
                }
                1.0 - gone as f32 / fade as f32
            }
            None => 1.0,
        };
        Some(CuePicture {
            picture,
            position: Self::position(run, clock, rate),
            opacity,
        })
    }
}

/// The windows and the drawing of one platform.
pub trait Platform {
    /// Whether this build shows video at all.
    fn video(&self) -> bool;
    /// The displays connected now.
    fn displays(&mut self) -> Vec<Display>;
    /// Draws `layers` (bottom first, each stage fitted to the display) on `display`, opening
    /// or moving its window as needed.
    fn present(&mut self, display: &Display, layers: &[Layer]) -> Result<(), String>;
    /// Takes the window of display `display` down, if it has one.
    fn close(&mut self, display: &str);
    /// Draws `layers` (bottom first, each fitted) on a target of `size` pixels and reads the
    /// colour (RGBA) at each point (fractions of the target, from the top left) back from the
    /// GPU.
    #[cfg(feature = "test-support")]
    fn probe(
        &mut self,
        size: (u32, u32),
        layers: &[Layer],
        points: &[[f32; 2]],
    ) -> Result<Vec<[u8; 4]>, String>;
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
    /// What happened to pictures since the last call: opened, or failed (could not be opened,
    /// decoded or drawn, or took too long) with why.
    fn take_picture_events(&mut self) -> Vec<PictureEvent> {
        Vec::new()
    }
    /// Whether the engine's clock runs on between its messages (windows on real displays):
    /// offscreen, a picture is drawn exactly at the last clock the engine sent.
    fn live(&self) -> bool {
        false
    }
}

/// How far past the last clock message the stage host moves the clock on by itself: three of
/// the engine's clock periods (it tells the clock at least every 10 ms), so one late tick does
/// not stop a picture.
const CLOCK_HOLD: std::time::Duration = std::time::Duration::from_millis(30);

/// What became of a picture's file on the platform.
#[derive(Clone, Debug, PartialEq)]
pub enum PictureEvent {
    Opened(u64),
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
    displays: Vec<Display>,
    states: HashMap<StageId, StageState>,
    pictures: HashMap<StageId, Picture>,
    /// Video cues' pictures, by the engine's id.
    pics: BTreeMap<u64, Pic>,
    /// Plays seen, for the stacking order of pictures started on one sample.
    plays: u64,
    clock: Option<Clock>,
    /// Displays with a window up.
    presented: BTreeSet<String>,
}

impl<P: Platform> Host<P> {
    pub fn new(platform: P) -> Self {
        Self {
            platform,
            stages: Vec::new(),
            displays: Vec::new(),
            states: HashMap::new(),
            pictures: HashMap::new(),
            pics: BTreeMap::new(),
            plays: 0,
            clock: None,
            presented: BTreeSet::new(),
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
        if self.pics.is_empty() {
            return;
        }
        self.apply(out);
    }

    fn picture_message(&mut self, message: ToStage, out: &mut Vec<FromStage>) {
        let (sample, _) = self.now();
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
            } => {
                self.plays += 1;
                let plays = self.plays;
                if let Some(p) = self.pics.get_mut(&picture) {
                    p.before = p.since.is_some().then_some(p.run);
                    p.run = Run::Playing { at, offset };
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
            } => {
                if let Some(p) = self.pics.get_mut(&picture) {
                    p.before = Some(p.run);
                    p.run = Run::Paused { at, offset };
                }
            }
            ToStage::Stop { picture, at, fade } => {
                let gone = match self.pics.get_mut(&picture) {
                    // Never shown, or a stop with no fade that is due: gone now.
                    Some(p) if p.since.is_none() || (fade == 0 && at <= sample) => true,
                    Some(p) => {
                        p.stop = Some((at, fade));
                        false
                    }
                    None => false,
                };
                if gone {
                    self.drop_picture(picture);
                }
            }
            ToStage::Unload { picture } => self.drop_picture(picture),
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
                PictureEvent::Failed(picture, reason) => {
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
    /// that failed or fell behind).
    pub fn poll(&mut self, out: &mut Vec<FromStage>) {
        self.report_failures(out);
    }

    /// One message from [`serve`], or (`None`) a poll between messages.
    pub fn step(&mut self, message: Option<ToStage>, out: &mut Vec<FromStage>) -> Flow {
        match message {
            Some(message) => self.handle(message, out),
            None => {
                self.poll(out);
                Flow::Continue
            }
        }
    }

    fn drop_picture(&mut self, picture: u64) {
        if self.pics.remove(&picture).is_some() {
            self.platform.close_picture(picture);
        }
    }

    /// The pictures drawn on `stage` at `clock`, earliest started first.
    fn cues_on(&self, stage: StageId, (clock, rate): (u64, u32)) -> Vec<CuePicture> {
        let mut on: Vec<((u64, u64), CuePicture)> = self
            .pics
            .iter()
            .filter(|(_, p)| p.stage == stage)
            .filter_map(|(id, p)| Some((p.order, p.at(*id, clock, rate)?)))
            .collect();
        on.sort_by_key(|(order, _)| *order);
        on.into_iter().map(|(_, c)| c).collect()
    }

    /// Drops the pictures whose fade-out is over at `clock`.
    fn prune(&mut self, (clock, _): (u64, u32)) {
        let done: Vec<u64> = self
            .pics
            .iter()
            .filter(|(_, p)| {
                p.stop
                    .is_some_and(|(at, fade)| clock >= at.saturating_add(fade))
            })
            .map(|(id, _)| *id)
            .collect();
        for id in done {
            self.drop_picture(id);
        }
    }

    pub fn platform_mut(&mut self) -> &mut P {
        &mut self.platform
    }

    /// Handles one message from the engine; the answers go to `out`.
    pub fn handle(&mut self, message: ToStage, out: &mut Vec<FromStage>) -> Flow {
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
            ToStage::Stages { stages } => {
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
                self.stages = stages;
                // Every stage's state is reported after a new set, changed or not, so the
                // engine knows each stage it sent has been dealt with.
                self.states.clear();
                self.apply(out);
            }
            ToStage::Ping { seq } => out.push(FromStage::Pong { seq }),
            ToStage::Quit => {
                for display in std::mem::take(&mut self.presented) {
                    self.platform.close(&display);
                }
                return Flow::Quit;
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
                out.push(self.probe_stage(stage, &points));
                self.report_failures(out);
            }
            #[cfg(feature = "test-support")]
            ToStage::ProbeDisplay { display, points } => {
                out.push(self.probe_display(display, &points));
                self.report_failures(out);
            }
            #[cfg(feature = "test-support")]
            ToStage::Hang => return Flow::Hang,
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
        if displays != self.displays {
            self.displays = displays.clone();
            out.push(FromStage::Displays { displays });
        }
        self.apply(out);
    }

    /// Shows, hides or redraws each stage and reports each state that changed. The stages
    /// shown on one display are drawn in one window, bottom first by their `stack`.
    fn apply(&mut self, out: &mut Vec<FromStage>) {
        let now = self.now();
        self.prune(now);
        let mut states: Vec<(StageId, StageState)> = Vec::new();
        let mut on: BTreeMap<String, Vec<(u32, Layer)>> = BTreeMap::new();
        for stage in &self.stages {
            let picture = self.picture(stage.id);
            let cues = self.cues_on(stage.id, now);
            let state = match &stage.display {
                _ if !self.platform.video() => StageState::Failed {
                    reason: "this computer's openLab has no video output".into(),
                },
                None => StageState::NoDisplay,
                Some(id) if !self.displays.iter().any(|d| &d.id == id) => {
                    StageState::DisplayMissing {
                        display: id.clone(),
                    }
                }
                Some(_) if !stage.keep_black && picture == Picture::Black && cues.is_empty() => {
                    StageState::Hidden
                }
                Some(id) => {
                    on.entry(id.clone())
                        .or_default()
                        .push((stage.stack, layer_of(stage, picture, cues)));
                    StageState::Shown {
                        display: id.clone(),
                    }
                }
            };
            states.push((stage.id, state));
        }
        for display in self.displays.clone() {
            let Some(mut layers) = on.remove(&display.id) else {
                if self.presented.remove(&display.id) {
                    self.platform.close(&display.id);
                }
                continue;
            };
            layers.sort_by_key(|(stack, _)| *stack);
            let layers: Vec<Layer> = layers.into_iter().map(|(_, l)| l).collect();
            match self.platform.present(&display, &layers) {
                Ok(()) => {
                    self.presented.insert(display.id.clone());
                }
                Err(reason) => {
                    for (id, state) in &mut states {
                        if layers.iter().any(|l| l.stage == *id) {
                            *state = StageState::Failed {
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
        self.report_failures(out);
        self.states
            .retain(|id, _| states.iter().any(|(s, _)| s == id));
        for (stage, state) in states {
            if self.states.get(&stage) != Some(&state) {
                self.states.insert(stage, state.clone());
                out.push(FromStage::StageStatus { stage, state });
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
        points: &[[f32; 2]],
    ) -> Result<(Option<u16>, Vec<[u8; 4]>), String> {
        let mut all: Vec<[f32; 2]> = crate::card::barcode_points().to_vec();
        all.extend_from_slice(points);
        let colours = self.platform.probe(size, layers, &all)?;
        let (code, asked) = colours.split_at(colours.len().min(crate::card::CELLS));
        Ok((crate::card::read_frame(code), asked.to_vec()))
    }

    /// One stage's own picture, at its own size.
    #[cfg(feature = "test-support")]
    fn probe_stage(&mut self, id: StageId, points: &[[f32; 2]]) -> FromStage {
        let now = self.now();
        let layer = self
            .stages
            .iter()
            .find(|s| s.id == id)
            .map(|s| layer_of(s, self.picture(id), self.cues_on(id, now)));
        let read = match layer {
            Some(l) => self.read((l.width, l.height), std::slice::from_ref(&l), points),
            None => Err("no such stage".into()),
        };
        let (frame, colours) = read.unwrap_or_default();
        FromStage::Probed {
            stage: id,
            frame,
            colours,
        }
    }

    /// What display `display` shows: every stage shown on it, stacked, at its pixel size.
    #[cfg(feature = "test-support")]
    fn probe_display(&mut self, display: String, points: &[[f32; 2]]) -> FromStage {
        let now = self.now();
        let mut layers: Vec<(u32, Layer)> = self
            .stages
            .iter()
            .filter(|s| {
                s.display.as_deref() == Some(display.as_str())
                    && matches!(self.states.get(&s.id), Some(StageState::Shown { .. }))
            })
            .map(|s| {
                (
                    s.stack,
                    layer_of(s, self.picture(s.id), self.cues_on(s.id, now)),
                )
            })
            .collect();
        layers.sort_by_key(|(stack, _)| *stack);
        let layers: Vec<Layer> = layers.into_iter().map(|(_, l)| l).collect();
        let size = self
            .displays
            .iter()
            .find(|d| d.id == display)
            .map(|d| (d.width, d.height));
        let read = match size {
            Some(size) => self.read(size, &layers, points),
            None => Err("no such display".into()),
        };
        let (frame, colours) = read.unwrap_or_default();
        FromStage::DisplayProbed {
            display,
            frame,
            colours,
        }
    }
}

fn layer_of(stage: &StageOutput, picture: Picture, cues: Vec<CuePicture>) -> Layer {
    Layer {
        stage: stage.id,
        width: stage.width,
        height: stage.height,
        picture,
        cues,
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

#[cfg(test)]
mod tests {
    use super::*;
    use openlab_core::stage_link::MAX_LINE_BYTES;

    /// A platform with displays given by the test and no drawing.
    struct Fake {
        displays: Vec<Display>,
        /// Each display's layers as last presented, bottom first.
        shown: HashMap<String, Vec<StageId>>,
        fail: bool,
        live: bool,
        events: Vec<PictureEvent>,
    }

    impl Platform for Fake {
        fn video(&self) -> bool {
            true
        }
        fn displays(&mut self) -> Vec<Display> {
            self.displays.clone()
        }
        fn present(&mut self, d: &Display, layers: &[Layer]) -> Result<(), String> {
            if self.fail {
                return Err("no GPU".into());
            }
            self.shown
                .insert(d.id.clone(), layers.iter().map(|l| l.stage).collect());
            Ok(())
        }
        fn close(&mut self, display: &str) {
            self.shown.remove(display);
        }
        #[cfg(feature = "test-support")]
        fn probe(
            &mut self,
            _: (u32, u32),
            _: &[Layer],
            p: &[[f32; 2]],
        ) -> Result<Vec<[u8; 4]>, String> {
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
    }

    /// What stage `n` draws for its cues, by picture: position and opacity.
    fn cues(host: &mut Host<Fake>, n: u8) -> Vec<(u64, f64, f32)> {
        let now = host.now();
        host.cues_on(id(n), now)
            .into_iter()
            .map(|c| (c.picture, c.position, c.opacity))
            .collect()
    }

    fn send(host: &mut Host<Fake>, m: ToStage) -> Vec<FromStage> {
        let mut out = Vec::new();
        host.handle(m, &mut out);
        out
    }

    fn clock(host: &mut Host<Fake>, sample: u64) {
        send(host, ToStage::Clock { sample, rate: 1000 });
    }

    fn load(host: &mut Host<Fake>, picture: u64, n: u8) -> Vec<FromStage> {
        send(
            host,
            ToStage::Load {
                picture,
                stage: id(n),
                file: "clip.mov".into(),
                still: false,
                offset: 0.0,
            },
        )
    }

    /// A picture is drawn from its play's sample, its position following the clock; a pause
    /// holds it (after its own sample); a stop fades it out and it goes.
    #[test]
    fn a_picture_follows_the_clock_holds_when_paused_and_fades_out_when_stopped() {
        let mut h = host(&["D"]);
        send(
            &mut h,
            ToStage::Stages {
                stages: vec![stage(1, Some("D"), false)],
            },
        );
        clock(&mut h, 1000);
        load(&mut h, 7, 1);
        assert_eq!(cues(&mut h, 1), vec![], "loaded is not drawn");
        send(
            &mut h,
            ToStage::Play {
                picture: 7,
                at: 1500,
                offset: 2.0,
            },
        );
        assert_eq!(cues(&mut h, 1), vec![], "not before its sample");
        clock(&mut h, 2000);
        assert_eq!(cues(&mut h, 1), vec![(7, 2.5, 1.0)]);
        send(
            &mut h,
            ToStage::Pause {
                picture: 7,
                at: 2500,
                offset: 3.0,
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
        send(
            &mut h,
            ToStage::Play {
                picture: 7,
                at: 4000,
                offset: 3.0,
            },
        );
        send(
            &mut h,
            ToStage::Stop {
                picture: 7,
                at: 4000,
                fade: 1000,
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
        send(
            &mut h,
            ToStage::Stages {
                stages: vec![stage(1, Some("D"), false)],
            },
        );
        clock(&mut h, 0);
        load(&mut h, 9, 1);
        load(&mut h, 3, 1);
        send(
            &mut h,
            ToStage::Play {
                picture: 9,
                at: 0,
                offset: 0.0,
            },
        );
        send(
            &mut h,
            ToStage::Play {
                picture: 3,
                at: 100,
                offset: 0.0,
            },
        );
        clock(&mut h, 200);
        let order: Vec<u64> = cues(&mut h, 1).iter().map(|c| c.0).collect();
        assert_eq!(order, vec![9, 3]);
    }

    /// A picture whose file cannot be opened, whose stage is not there, or whose stage is
    /// removed while it plays is reported failed, with a one-line reason.
    #[test]
    fn a_picture_that_cannot_be_shown_is_reported_failed() {
        let mut h = host(&["D"]);
        send(
            &mut h,
            ToStage::Stages {
                stages: vec![stage(1, Some("D"), false)],
            },
        );
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
            },
        );
        assert_eq!(failed(&out), vec![(1, "cannot open this".to_owned())]);
        assert_eq!(failed(&load(&mut h, 2, 9)).len(), 1, "no such stage");
        load(&mut h, 3, 1);
        send(
            &mut h,
            ToStage::Play {
                picture: 3,
                at: 0,
                offset: 0.0,
            },
        );
        let out = send(&mut h, ToStage::Stages { stages: vec![] });
        assert_eq!(failed(&out), vec![(3, "its stage was removed".to_owned())]);
        assert!(!h.playing());
    }

    fn id(n: u8) -> StageId {
        StageId(uuid::Uuid::from_u128(n.into()))
    }

    fn display(id: &str) -> Display {
        Display {
            id: id.into(),
            name: "Projector".into(),
            width: 1920,
            height: 1080,
        }
    }

    fn stage(n: u8, display: Option<&str>, keep_black: bool) -> StageOutput {
        StageOutput {
            id: id(n),
            width: 1920,
            height: 1080,
            display: display.map(Into::into),
            keep_black,
            stack: u32::from(n),
        }
    }

    fn host(displays: &[&str]) -> Host<Fake> {
        Host::new(Fake {
            displays: displays.iter().map(|d| display(d)).collect(),
            shown: HashMap::new(),
            fail: false,
            live: false,
            events: Vec::new(),
        })
    }

    fn states(out: &[FromStage]) -> Vec<(StageId, StageState)> {
        out.iter()
            .filter_map(|m| match m {
                FromStage::StageStatus { stage, state } => Some((*stage, state.clone())),
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
        assert_eq!(
            out[1],
            FromStage::Displays {
                displays: vec![display("D1")]
            }
        );
        out.clear();
        let stages = vec![
            stage(1, Some("D1"), true),
            stage(2, None, true),
            stage(3, Some("D9"), true),
            stage(4, Some("D1"), false),
        ];
        h.handle(
            ToStage::Stages {
                stages: stages.clone(),
            },
            &mut out,
        );
        assert_eq!(
            states(&out),
            vec![
                (
                    id(1),
                    StageState::Shown {
                        display: "D1".into()
                    }
                ),
                (id(2), StageState::NoDisplay),
                (
                    id(3),
                    StageState::DisplayMissing {
                        display: "D9".into()
                    }
                ),
                (id(4), StageState::Hidden),
            ]
        );
        assert_eq!(h.platform.shown["D1"], [id(1)]);
        // The same set again is reported again (the engine waits for each set's states) and
        // changes no window.
        out.clear();
        h.handle(ToStage::Stages { stages }, &mut out);
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
        top.stack = 9;
        let bottom = stage(2, Some("D1"), true);
        h.handle(
            ToStage::Stages {
                stages: vec![top, bottom],
            },
            &mut out,
        );
        assert_eq!(h.platform.shown["D1"], [id(2), id(1)]);
        h.handle(ToStage::Stages { stages: vec![] }, &mut out);
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
        h.handle(
            ToStage::Stages {
                stages: vec![stage(1, Some("D1"), true)],
            },
            &mut out,
        );
        out.clear();
        h.platform.displays.clear();
        h.displays_changed(&mut out);
        assert_eq!(out[0], FromStage::Displays { displays: vec![] });
        assert_eq!(
            states(&out),
            vec![(
                id(1),
                StageState::DisplayMissing {
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
                StageState::Shown {
                    display: "D1".into()
                }
            )]
        );
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
        h.handle(
            ToStage::Stages {
                stages: vec![stage(1, Some("D1"), true)],
            },
            &mut out,
        );
        assert_eq!(
            states(&out),
            vec![(
                id(1),
                StageState::Failed {
                    reason: "no GPU".into()
                }
            )]
        );
        h.platform.fail = false;
        h.handle(
            ToStage::Stages {
                stages: vec![stage(1, Some("D1"), true)],
            },
            &mut out,
        );
        assert_eq!(h.platform.shown.len(), 1);
        h.handle(ToStage::Stages { stages: vec![] }, &mut out);
        assert!(h.platform.shown.is_empty());
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
        h.handle(
            ToStage::Stages {
                stages: vec![stage(1, Some("D1"), true)],
            },
            &mut out,
        );
        assert_eq!(h.handle(ToStage::Quit, &mut out), Flow::Quit);
        assert!(h.platform.shown.is_empty());
    }
}
