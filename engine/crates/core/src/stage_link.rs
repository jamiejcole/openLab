// SPDX-License-Identifier: GPL-3.0-or-later

//! The link between the engine and the stage host (`openlab-stage`, the process that owns the
//! video output windows): one JSON object per line over the stage host's stdin (engine to stage
//! host) and stdout (stage host to engine). Contract: docs/protocol.md, "Stage host link".
//!
//! Both ends validate everything they read: a line that is too long, not JSON, of an unknown
//! type or outside the limits below is a [`LinkError`], never a panic. The engine treats a bad
//! line from the stage host as a warning and goes on reading.

use std::fmt;
use std::io::BufRead;

use serde::{Deserialize, Serialize};

use crate::model::StageId;
use crate::video::{MAX_STAGES, side_ok};
use crate::video_geometry::{PictureParam, PictureRamp, VideoGeometry};
use crate::video_timing::{PictureFades, PlanPass, picture_run_ok};

/// The link version both ends speak; a stage host answering another version is not used.
pub const LINK_VERSION: u32 = 6;
/// Longest line either end accepts, newline excluded: enough for a `stages` set of
/// [`MAX_LINK_STAGES`] stages whose display ids and monitor titles are all [`MAX_TEXT_BYTES`]
/// long.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;
/// Most displays one `displays` message lists.
pub const MAX_DISPLAYS: usize = 32;
/// The most frames a second a display may report.
pub const MAX_DISPLAY_FPS: u32 = 1000;
/// Longest display id, display name or log message, in bytes.
pub const MAX_TEXT_BYTES: usize = 512;
/// Most stages one `stages` message carries: every stage of every open workspace (at most 8).
pub const MAX_LINK_STAGES: usize = MAX_STAGES * 8;
/// Most points one probe reads.
pub const MAX_PROBE_POINTS: usize = 64;
/// Longest media file path a `load` carries, in bytes.
pub const MAX_PATH_BYTES: usize = 4096;
/// Most pictures (Video cues' runs) the stage host holds at once: far more than any show plays.
pub const MAX_PICTURES: usize = 256;
/// Highest sample rate a `clock` may name (the engine's own limit).
pub const MAX_CLOCK_RATE: u32 = 384_000;

/// A message from the engine to the stage host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToStage {
    /// The first message: the link version the engine speaks.
    Hello { version: u32 },
    /// Every stage the stage host should have, replacing the previous set.
    Stages { stages: Vec<StageOutput> },
    /// Health check, answered by `pong` with the same `seq`.
    Ping { seq: u64 },
    /// Close every window and exit.
    Quit,
    /// The stage's monitor window (it has one in the `stages` set) comes to the front, opened
    /// again if the operator closed it since.
    RaiseMonitor { stage: StageId },
    /// While `on`, the window on the display with the menu bar stands back under every app's
    /// windows (over the desktop), so the operator can see the app's question; what it draws
    /// is unchanged.
    StandBack { on: bool },
    /// The operator's app: `connected` while one is connected to the engine, with its process
    /// id when it gave one. While none is (the app quit or was force-quit), every output window
    /// is down, whatever the stages show, until one is again: no stage outlives the app. A click
    /// on a stage brings the app with `pid` forward with the keys.
    App { connected: bool, pid: Option<u32> },
    /// The engine's sample clock: `sample` is now, at `rate` samples a second. Every picture's
    /// position follows it; the stage host moves it on by its own clock between messages.
    Clock { sample: u64, rate: u32 },
    /// Opens `file` (a video, or a still with `still`) for `picture` (one Video cue run's
    /// picture, an id of the engine's) on `stage`, held at `offset` seconds and not drawn
    /// until it plays. Replaces a picture of the same id.
    Load {
        picture: u64,
        stage: StageId,
        file: String,
        still: bool,
        offset: f64,
        /// How it sits on its stage, how opaque and on which layer (VID-04).
        geometry: VideoGeometry,
    },
    /// The picture's geometry changed (an edit while it is up): it is drawn so from the next
    /// frame, its run untouched.
    Geometry {
        picture: u64,
        geometry: VideoGeometry,
    },
    /// One value of the picture's geometry moves (a Fade cue, VID-05): drawn as `ramp` says
    /// from its sample on, over the geometry's own value, until the picture goes or the next
    /// `ramp` of that value (from its own sample; before it, this one still applies).
    Ramp {
        picture: u64,
        param: PictureParam,
        ramp: PictureRamp,
    },
    /// The picture plays from sample `at` (engine clock), `offset` seconds into the file then;
    /// a picture not loaded is ignored. A movie follows its `plan` (its trims, slices and loops
    /// from the top of its action), `into` seconds into the plan on `at`, held just before the
    /// end of the plan's last pass once it is over; a movie with no plan holds at `offset` (it
    /// never runs on past its range). A still ignores both. `action` is how far into its cue's
    /// action it is on `at`, which the integrated `fades` are counted in.
    Play {
        picture: u64,
        at: u64,
        offset: f64,
        action: f64,
        plan: Vec<PlanPass>,
        into: f64,
        fades: PictureFades,
    },
    /// The picture holds the frame at `offset` seconds from sample `at`, `action` seconds into
    /// its cue's action (its fades held there).
    Pause {
        picture: u64,
        at: u64,
        offset: f64,
        action: f64,
    },
    /// The picture fades out over `fade` samples from sample `at` (0: gone on `at`), then goes;
    /// with `cut`, it goes on that sample if the fade is not over by then (its cue's action
    /// ends there, mid-fade).
    Stop {
        picture: u64,
        at: u64,
        fade: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cut: Option<u64>,
    },
    /// The picture goes now.
    Unload { picture: u64 },
    /// Test support: draw the test card for `frame` on `stage` (a 16-bit frame-number barcode
    /// across the top tenth, then four coloured quadrants), until the stages change.
    #[cfg(feature = "test-support")]
    TestCard { stage: StageId, frame: u16 },
    /// Test support: read the stage's picture at `points` (fractions of its width and height,
    /// from the top left), answered by `probed`.
    #[cfg(feature = "test-support")]
    Probe {
        stage: StageId,
        points: Vec<[f32; 2]>,
    },
    /// Test support: read what `display` shows (every stage on it, stacked) at `points`,
    /// answered by `display_probed`.
    #[cfg(feature = "test-support")]
    ProbeDisplay {
        display: String,
        points: Vec<[f32; 2]>,
    },
    /// Test support: read what the stage's monitor window would show at a content size of
    /// `width` x `height` pixels (the stage fitted, black bars elsewhere) at `points` (fractions
    /// of the window from the top left), answered by `monitor_probed`.
    #[cfg(feature = "test-support")]
    ProbeMonitor {
        stage: StageId,
        width: u32,
        height: u32,
        points: Vec<[f32; 2]>,
    },
    /// Test support: the operator closes the stage's monitor window (its close button).
    #[cfg(feature = "test-support")]
    CloseMonitorWindow { stage: StageId },
    /// Test support: stop reading and answering, as a hung stage host would.
    #[cfg(feature = "test-support")]
    Hang,
    /// Test support: a stage host on virtual displays now has these, as if displays were
    /// plugged in or unplugged.
    #[cfg(feature = "test-support")]
    SetDisplays { displays: Vec<Display> },
    /// Test support: moves the stage host's media clock on by `ms` (one started with
    /// `--media-clock-by-hand`; at most a day at a time), for the limits on pictures' opens
    /// and frames.
    #[cfg(feature = "test-support")]
    AdvanceMediaClock { ms: u64 },
}

/// One stage as the stage host draws it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageOutput {
    pub id: StageId,
    pub width: u32,
    pub height: u32,
    /// The display the stage is shown on (an id from `displays`); `None`: drawn but shown nowhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    /// Black stays up while nothing plays; off, the window is hidden while nothing plays.
    pub keep_black: bool,
    /// Stacking rank among all stages, 0 at the bottom: of the stages on one display, a higher
    /// rank is drawn on top (the engine ranks them by layer, then name).
    #[serde(default)]
    pub stack: u32,
    /// The title of the stage's monitor window (a live copy of the stage, drawn whether or not
    /// the stage is shown on a display) while the operator has it open; `None`: no monitor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<String>,
}

/// A message from the stage host to the engine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromStage {
    /// The answer to `hello`. `video` is false where this build has no video output (Windows
    /// for now): the stage host then only answers pings.
    Hello {
        version: u32,
        pid: u32,
        video: bool,
    },
    /// The connected displays, sent after `hello` and again on every change.
    Displays {
        displays: Vec<Display>,
    },
    /// Where a stage is now, sent whenever that changes.
    StageStatus {
        stage: StageId,
        state: StageState,
    },
    Pong {
        seq: u64,
    },
    /// Something the stage host wants in the show log.
    Log {
        level: LinkLevel,
        message: String,
    },
    /// A picture's file is open and it can be drawn (its `load` is done). The engine does not
    /// send a load again to a stage host that started after one died with it still opening.
    PictureOpened {
        picture: u64,
    },
    /// A picture could not be opened, decoded or drawn: it is gone from its stage.
    PictureFailed {
        picture: u64,
        reason: String,
    },
    /// The operator closed the stage's monitor window: it stays closed until the engine asks
    /// for it again.
    MonitorClosed {
        stage: StageId,
    },
    /// Test support: the answer to `probe`: the frame number read from the barcode, if the
    /// stage shows one, and the colour (RGBA, 0-255) at each point asked for.
    #[cfg(feature = "test-support")]
    Probed {
        stage: StageId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frame: Option<u16>,
        colours: Vec<[u8; 4]>,
    },
    /// Test support: the answer to `probe_display`, as `probed` is to `probe`.
    #[cfg(feature = "test-support")]
    DisplayProbed {
        display: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frame: Option<u16>,
        colours: Vec<[u8; 4]>,
    },
    /// Test support: the answer to `probe_monitor`, as `probed` is to `probe`; no colours when
    /// the stage has no monitor window open.
    #[cfg(feature = "test-support")]
    MonitorProbed {
        stage: StageId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frame: Option<u16>,
        colours: Vec<[u8; 4]>,
    },
}

/// A display the stage host can show a stage on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Display {
    /// Stable across restarts and reconnection (on macOS the display's UUID).
    pub id: String,
    pub name: String,
    /// Pixel size (not points: QLab shows pixels, p. 212).
    pub width: u32,
    pub height: u32,
    /// The display's refresh rate in frames a second (its fastest mode, as the system names it),
    /// 0 when the stage host does not know it.
    #[serde(default)]
    pub fps: u32,
}

/// Where a stage is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StageState {
    /// Up on `display`.
    Shown { display: String },
    /// Its window is hidden: keep-black is off and nothing plays on it.
    Hidden,
    /// It has no display on this machine.
    NoDisplay,
    /// Its display is not connected.
    DisplayMissing { display: String },
    /// It cannot be shown (the reason is for the log and the operator).
    Failed { reason: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkLevel {
    Info,
    Warn,
    Error,
}

/// Why a line was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkError {
    TooLong {
        bytes: usize,
    },
    /// Not valid JSON, or not a message this end knows.
    Unreadable(String),
    /// A message of a known type whose values are outside the link's limits.
    Invalid(&'static str),
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinkError::TooLong { bytes } => {
                write!(f, "line of {bytes} bytes; the limit is {MAX_LINE_BYTES}")
            }
            LinkError::Unreadable(e) => write!(f, "unreadable message ({e})"),
            LinkError::Invalid(why) => write!(f, "invalid message: {why}"),
        }
    }
}

impl std::error::Error for LinkError {}

/// One message as one line, newline included.
pub fn encode<T: Serialize>(message: &T) -> Result<String, LinkError> {
    let mut line =
        serde_json::to_string(message).map_err(|e| LinkError::Unreadable(e.to_string()))?;
    if line.len() > MAX_LINE_BYTES {
        return Err(LinkError::TooLong { bytes: line.len() });
    }
    line.push('\n');
    Ok(line)
}

fn parse<'a, T: Deserialize<'a>>(line: &'a [u8]) -> Result<T, LinkError> {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    if line.len() > MAX_LINE_BYTES {
        return Err(LinkError::TooLong { bytes: line.len() });
    }
    serde_json::from_slice(line).map_err(|e| LinkError::Unreadable(e.to_string()))
}

/// Reads a line the engine sent (the stage host's side).
pub fn decode_to_stage(line: &[u8]) -> Result<ToStage, LinkError> {
    let message: ToStage = parse(line)?;
    check_to_stage(&message)?;
    Ok(message)
}

/// Reads a line the stage host sent (the engine's side).
pub fn decode_from_stage(line: &[u8]) -> Result<FromStage, LinkError> {
    let message: FromStage = parse(line)?;
    check_from_stage(&message)?;
    Ok(message)
}

fn text_ok(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_TEXT_BYTES && !text.chars().any(char::is_control)
}

/// `text` (a stage's name, say) as the link takes a title: control characters as spaces, cut
/// to [`MAX_TEXT_BYTES`] on a character boundary, `fallback` when nothing is left. A title the
/// link refuses would cost the whole stage set its line.
pub fn link_text(text: &str, fallback: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_TEXT_BYTES));
    for c in text.chars().map(|c| if c.is_control() { ' ' } else { c }) {
        if out.len() + c.len_utf8() > MAX_TEXT_BYTES {
            break;
        }
        out.push(c);
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        fallback.chars().take(MAX_TEXT_BYTES / 4).collect()
    } else {
        trimmed.to_owned()
    }
}

/// Log text may be empty; it is otherwise held to the same limits as names.
fn message_ok(text: &str) -> bool {
    text.len() <= MAX_TEXT_BYTES && !text.chars().any(|c| c.is_control() && c != '\n')
}

fn check_displays(displays: &[Display]) -> Result<(), LinkError> {
    if displays.len() > MAX_DISPLAYS {
        return Err(LinkError::Invalid("too many displays"));
    }
    for d in displays {
        if !text_ok(&d.id) || !text_ok(&d.name) {
            return Err(LinkError::Invalid("bad display id or name"));
        }
        if !side_ok(d.width) || !side_ok(d.height) {
            return Err(LinkError::Invalid("display size out of range"));
        }
        if d.fps > MAX_DISPLAY_FPS {
            return Err(LinkError::Invalid("display refresh rate out of range"));
        }
    }
    Ok(())
}

#[cfg(feature = "test-support")]
fn check_points(points: &[[f32; 2]]) -> Result<(), LinkError> {
    if points.is_empty() || points.len() > MAX_PROBE_POINTS {
        return Err(LinkError::Invalid("wrong number of probe points"));
    }
    if points
        .iter()
        .flatten()
        .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
    {
        return Err(LinkError::Invalid("probe point outside the stage"));
    }
    Ok(())
}

/// A position in a file, in seconds: finite, 0 to 24 h.
fn offset_ok(offset: f64) -> bool {
    offset.is_finite() && (0.0..=crate::MAX_TIME).contains(&offset)
}

fn check_to_stage(message: &ToStage) -> Result<(), LinkError> {
    match message {
        ToStage::Clock { rate, .. } if *rate == 0 || *rate > MAX_CLOCK_RATE => {
            Err(LinkError::Invalid("clock rate out of range"))
        }
        ToStage::Load { file, offset, .. }
            if file.is_empty()
                || file.len() > MAX_PATH_BYTES
                || file.chars().any(char::is_control)
                || !offset_ok(*offset) =>
        {
            Err(LinkError::Invalid("bad picture file or offset"))
        }
        ToStage::Load { geometry, .. } | ToStage::Geometry { geometry, .. }
            if !geometry.is_valid() =>
        {
            Err(LinkError::Invalid("picture geometry out of range"))
        }
        ToStage::Ramp { param, ramp, .. } if !ramp.ok(*param) => {
            Err(LinkError::Invalid("picture value out of range"))
        }
        ToStage::Play { offset, .. } | ToStage::Pause { offset, .. } if !offset_ok(*offset) => {
            Err(LinkError::Invalid("picture offset out of range"))
        }
        ToStage::Play {
            action,
            plan,
            fades,
            into,
            ..
        } if !picture_run_ok(plan, *action, fades) || !offset_ok(*into) => {
            Err(LinkError::Invalid("picture plan or fades out of range"))
        }
        ToStage::Pause { action, .. } if !offset_ok(*action) => {
            Err(LinkError::Invalid("picture action time out of range"))
        }
        ToStage::Stages { stages } => {
            if stages.len() > MAX_LINK_STAGES {
                return Err(LinkError::Invalid("too many stages"));
            }
            for stage in stages {
                if !side_ok(stage.width) || !side_ok(stage.height) {
                    return Err(LinkError::Invalid("stage size out of range"));
                }
                if stage.display.as_deref().is_some_and(|d| !text_ok(d)) {
                    return Err(LinkError::Invalid("bad display id"));
                }
                if stage.monitor.as_deref().is_some_and(|t| !text_ok(t)) {
                    return Err(LinkError::Invalid("bad monitor title"));
                }
            }
            Ok(())
        }
        #[cfg(feature = "test-support")]
        ToStage::ProbeMonitor {
            width,
            height,
            points,
            ..
        } => {
            if !side_ok(*width) || !side_ok(*height) {
                return Err(LinkError::Invalid("monitor size out of range"));
            }
            check_points(points)
        }
        #[cfg(feature = "test-support")]
        ToStage::SetDisplays { displays } => check_displays(displays),
        #[cfg(feature = "test-support")]
        ToStage::AdvanceMediaClock { ms } if *ms > 86_400_000 => {
            Err(LinkError::Invalid("media clock step out of range"))
        }
        #[cfg(feature = "test-support")]
        ToStage::ProbeDisplay { display, points } => {
            if !text_ok(display) {
                return Err(LinkError::Invalid("bad display id"));
            }
            check_points(points)
        }
        #[cfg(feature = "test-support")]
        ToStage::Probe { points, .. } => check_points(points),
        _ => Ok(()),
    }
}

fn check_from_stage(message: &FromStage) -> Result<(), LinkError> {
    match message {
        FromStage::Displays { displays } => check_displays(displays),
        FromStage::StageStatus { state, .. } => match state {
            StageState::Shown { display } | StageState::DisplayMissing { display }
                if !text_ok(display) =>
            {
                Err(LinkError::Invalid("bad display id"))
            }
            StageState::Failed { reason } if !message_ok(reason) => {
                Err(LinkError::Invalid("bad reason"))
            }
            _ => Ok(()),
        },
        FromStage::Log { message, .. } if !message_ok(message) => {
            Err(LinkError::Invalid("bad log message"))
        }
        FromStage::PictureFailed { reason, .. } if !text_ok(reason) => {
            Err(LinkError::Invalid("bad picture failure reason"))
        }
        #[cfg(feature = "test-support")]
        FromStage::Probed { colours, .. } if colours.len() > MAX_PROBE_POINTS => {
            Err(LinkError::Invalid("too many probe colours"))
        }
        #[cfg(feature = "test-support")]
        FromStage::DisplayProbed {
            display, colours, ..
        } if colours.len() > MAX_PROBE_POINTS || !text_ok(display) => {
            Err(LinkError::Invalid("bad display probe"))
        }
        #[cfg(feature = "test-support")]
        FromStage::MonitorProbed { colours, .. } if colours.len() > MAX_PROBE_POINTS => {
            Err(LinkError::Invalid("too many probe colours"))
        }
        _ => Ok(()),
    }
}

/// What [`read_line`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    /// A line, in the buffer.
    Read,
    /// The end of input, nothing read.
    End,
    /// A line longer than [`MAX_LINE_BYTES`], skipped to its end.
    TooLong(usize),
}

/// Reads one line (without its newline) into `buf` from either end's pipe, never holding more
/// than [`MAX_LINE_BYTES`] of it: a longer line is skipped to its newline and reported. A last
/// line without a newline counts.
pub fn read_line(input: &mut impl BufRead, buf: &mut Vec<u8>) -> std::io::Result<Line> {
    let mut total = 0usize;
    let mut saw_any = false;
    loop {
        // A read a signal interrupted has read nothing: read again (the link is not over).
        let chunk = match input.fill_buf() {
            Ok(chunk) => chunk,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if chunk.is_empty() {
            return Ok(match (saw_any, total > MAX_LINE_BYTES) {
                (false, _) => Line::End,
                (true, true) => Line::TooLong(total),
                (true, false) => Line::Read,
            });
        }
        saw_any = true;
        let (part, done) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (chunk.get(..i).unwrap_or_default(), Some(i + 1)),
            None => (chunk, None),
        };
        total = total.saturating_add(part.len());
        if total <= MAX_LINE_BYTES {
            buf.extend_from_slice(part);
        } else {
            buf.clear();
        }
        let used = done.unwrap_or(chunk.len());
        input.consume(used);
        if done.is_some() {
            return Ok(if total > MAX_LINE_BYTES {
                Line::TooLong(total)
            } else {
                Line::Read
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// A pipe read that a signal interrupts is read again: the line comes through whole and
    /// the link is not taken to be over.
    #[test]
    fn a_read_a_signal_interrupts_is_read_again() {
        struct Interrupted<'a> {
            left: u32,
            data: &'a [u8],
        }
        impl std::io::Read for Interrupted<'_> {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                if self.left > 0 {
                    self.left -= 1;
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                let n = out.len().min(self.data.len()).min(3);
                out[..n].copy_from_slice(&self.data[..n]);
                self.data = &self.data[n..];
                Ok(n)
            }
        }
        let mut input = std::io::BufReader::new(Interrupted {
            left: 2,
            data: b"{\"a\":1}\n",
        });
        let mut line = Vec::new();
        assert!(matches!(read_line(&mut input, &mut line), Ok(Line::Read)));
        assert_eq!(line, b"{\"a\":1}");
        line.clear();
        assert!(matches!(read_line(&mut input, &mut line), Ok(Line::End)));
    }

    fn stage(n: u128) -> StageId {
        StageId(Uuid::from_u128(n))
    }

    #[test]
    fn messages_survive_a_round_trip_as_one_line() {
        let to = [
            ToStage::Hello {
                version: LINK_VERSION,
            },
            ToStage::Stages {
                stages: vec![StageOutput {
                    id: stage(1),
                    width: 1920,
                    height: 1080,
                    display: Some("D1".into()),
                    keep_black: true,
                    stack: 3,
                    monitor: Some("Stage 1".into()),
                }],
            },
            ToStage::Ping { seq: 7 },
            ToStage::Quit,
            ToStage::RaiseMonitor { stage: stage(1) },
            ToStage::StandBack { on: true },
            ToStage::App {
                connected: true,
                pid: Some(501),
            },
            ToStage::App {
                connected: false,
                pid: None,
            },
            ToStage::Load {
                picture: 4,
                stage: stage(1),
                file: "/show/a.mov".into(),
                still: false,
                offset: 1.5,
                geometry: VideoGeometry::default(),
            },
            ToStage::Geometry {
                picture: 4,
                geometry: VideoGeometry {
                    fill_stage: false,
                    rotation: -30.0,
                    opacity: 0.25,
                    layer: 0,
                    smooth: false,
                    ..VideoGeometry::default()
                },
            },
            ToStage::Stop {
                picture: 4,
                at: 100,
                fade: 50,
                cut: None,
            },
            ToStage::Stop {
                picture: 4,
                at: 100,
                fade: 50,
                cut: Some(120),
            },
        ];
        for m in to {
            let line = encode(&m).unwrap();
            assert_eq!(line.matches('\n').count(), 1);
            assert!(line.ends_with('\n'));
            assert_eq!(decode_to_stage(line.as_bytes()).unwrap(), m);
        }
        // A stop with no cut is written without one, as before the cut existed.
        let plain = br#"{"type":"stop","picture":4,"at":100,"fade":50}"#;
        assert!(matches!(
            decode_to_stage(plain),
            Ok(ToStage::Stop { cut: None, .. })
        ));
        let from = [
            FromStage::Hello {
                version: 1,
                pid: 42,
                video: true,
            },
            FromStage::Displays {
                displays: vec![Display {
                    id: "D1".into(),
                    name: "Projector".into(),
                    width: 3840,
                    height: 2160,
                    fps: 60,
                }],
            },
            FromStage::StageStatus {
                stage: stage(1),
                state: StageState::DisplayMissing {
                    display: "D1".into(),
                },
            },
            FromStage::Pong { seq: 7 },
            FromStage::PictureOpened { picture: 3 },
            FromStage::MonitorClosed { stage: stage(1) },
            FromStage::PictureFailed {
                picture: 3,
                reason: "the file is damaged".into(),
            },
            FromStage::Log {
                level: LinkLevel::Warn,
                message: "display changed".into(),
            },
        ];
        for m in from {
            let line = encode(&m).unwrap();
            assert_eq!(decode_from_stage(line.as_bytes()).unwrap(), m);
        }
    }

    /// The largest stage set the link allows (every stage with the longest display id) fits in
    /// one line and reads back.
    #[test]
    fn the_largest_stage_set_fits_one_line() {
        let stages: Vec<StageOutput> = (0..MAX_LINK_STAGES as u128)
            .map(|n| StageOutput {
                id: stage(n),
                width: crate::MAX_STAGE_PIXELS,
                height: crate::MAX_STAGE_PIXELS,
                display: Some("d".repeat(MAX_TEXT_BYTES)),
                keep_black: true,
                stack: u32::MAX,
                monitor: Some("m".repeat(MAX_TEXT_BYTES)),
            })
            .collect();
        let message = ToStage::Stages { stages };
        let line = encode(&message).unwrap();
        assert!(line.len() <= MAX_LINE_BYTES + 1, "{} bytes", line.len());
        assert_eq!(decode_to_stage(line.as_bytes()).unwrap(), message);
    }

    /// A picture value's ramp (VID-05) reads back; one with an end outside the value's range,
    /// or not a number, is refused.
    #[test]
    fn a_picture_ramp_reads_back_and_one_out_of_range_is_refused() {
        let ramp = |param, from, to| ToStage::Ramp {
            picture: 3,
            param,
            ramp: PictureRamp {
                from,
                to,
                at: 48_000,
                frames: 144_000,
                curve: crate::FadeCurve::SCurve,
            },
        };
        let good = ramp(PictureParam::TranslationX, -300.5, 1200.0);
        let line = encode(&good).unwrap();
        assert_eq!(decode_to_stage(line.as_bytes()).unwrap(), good);
        for bad in [
            ramp(PictureParam::Opacity, 1.0, 1.01),
            ramp(PictureParam::Rotation, 361.0, 0.0),
            ramp(PictureParam::ScaleY, 1.0, -101.0),
        ] {
            let line = serde_json::to_string(&bad).unwrap();
            assert!(matches!(
                decode_to_stage(line.as_bytes()),
                Err(LinkError::Invalid(_))
            ));
        }
        for line in [
            r#"{"type":"ramp","picture":1,"param":"opacity","ramp":{"from":1,"to":null,"at":0,"frames":0,"curve":"linear"}}"#,
            r#"{"type":"ramp","picture":1,"param":"crop","ramp":{"from":1,"to":1,"at":0,"frames":0,"curve":"linear"}}"#,
            r#"{"type":"ramp","picture":1,"param":"opacity","ramp":{"from":1,"to":1,"at":-1,"frames":0,"curve":"linear"}}"#,
            r#"{"type":"ramp","picture":1,"param":"scale_x","ramp":{"from":1e999,"to":1,"at":0,"frames":0,"curve":"linear"}}"#,
        ] {
            assert!(decode_to_stage(line.as_bytes()).is_err(), "{line}");
        }
    }

    /// Geometry out of range is refused on the link as everywhere else (VID-04).
    #[test]
    fn picture_geometry_out_of_range_is_refused() {
        let bad = |geometry| ToStage::Geometry {
            picture: 1,
            geometry,
        };
        for geometry in [
            VideoGeometry {
                opacity: 1.5,
                ..VideoGeometry::default()
            },
            VideoGeometry {
                layer: 1001,
                ..VideoGeometry::default()
            },
            VideoGeometry {
                scale: crate::Pair { x: 1e6, y: 1.0 },
                ..VideoGeometry::default()
            },
        ] {
            let line = serde_json::to_string(&bad(geometry)).unwrap();
            assert!(matches!(
                decode_to_stage(line.as_bytes()),
                Err(LinkError::Invalid(_))
            ));
        }
        let load = concat!(
            r#"{"type":"load","picture":1,"stage":"00000000-0000-0000-0000-000000000001","#,
            r#""file":"/a.mov","still":false,"offset":0,"geometry":{"opacity":-1}}"#
        );
        assert!(matches!(
            decode_to_stage(load.as_bytes()),
            Err(LinkError::Invalid(_))
        ));
    }

    /// A play with a plan and fades reads back; one out of the link's limits is refused (VID-07),
    /// and the largest plan fits one line.
    #[test]
    fn a_picture_plan_reads_back_and_one_out_of_range_is_refused() {
        let fade = crate::FadeSpec {
            duration: 0.5,
            curve: crate::FadeCurve::SCurve,
        };
        let play = |plan: Vec<PlanPass>, action: f64, end: Option<f64>| ToStage::Play {
            picture: 2,
            at: 480,
            offset: 1.0,
            action,
            plan,
            into: action,
            fades: PictureFades {
                fade_in: fade,
                fade_out: fade,
                end,
            },
        };
        let pass = |from, length, plays| PlanPass {
            from,
            length,
            plays,
        };
        let good = play(vec![pass(1.0, 2.0, 0), pass(3.0, 1.0, 1)], 0.25, None);
        let line = encode(&good).unwrap();
        assert_eq!(decode_to_stage(line.as_bytes()).unwrap(), good);
        let pause = ToStage::Pause {
            picture: 2,
            at: 9,
            offset: 1.5,
            action: 3.0,
        };
        assert_eq!(
            decode_to_stage(encode(&pause).unwrap().as_bytes()).unwrap(),
            pause
        );
        let largest = play(
            vec![pass(crate::MAX_TIME / 3.0, 1.0 / 3.0, u32::MAX); crate::MAX_PLAN_PASSES],
            crate::MAX_TIME / 7.0,
            Some(crate::MAX_TIME / 9.0),
        );
        let line = encode(&largest).unwrap();
        assert_eq!(decode_to_stage(line.as_bytes()).unwrap(), largest);
        for bad in [
            play(vec![pass(-1.0, 2.0, 1)], 0.0, None),
            play(vec![pass(0.0, 0.0, 1)], 0.0, None),
            play(vec![pass(0.0, 1.0, 1)], -0.5, None),
            play(vec![pass(0.0, 1.0, 1)], 0.0, Some(1e12)),
            play(
                vec![pass(0.0, 1.0, 1); crate::MAX_PLAN_PASSES + 1],
                0.0,
                None,
            ),
            ToStage::Pause {
                picture: 2,
                at: 9,
                offset: 1.5,
                action: f64::MAX,
            },
            ToStage::Play {
                picture: 2,
                at: 9,
                offset: 1.5,
                action: 0.0,
                plan: vec![pass(0.0, 1.0, 1)],
                into: -1.0,
                fades: PictureFades::NONE,
            },
        ] {
            let line = encode(&bad).unwrap();
            assert!(
                matches!(decode_to_stage(line.as_bytes()), Err(LinkError::Invalid(_))),
                "{bad:?}"
            );
        }
        // NaN is not JSON: a line carrying one in a plan is unreadable, never accepted.
        let nan = r#"{"type":"play","picture":1,"at":0,"offset":0,"action":0,"into":0,"plan":[{"from":NaN,"length":1,"plays":1}],"fades":{"fade_in":{"duration":0,"curve":"linear"},"fade_out":{"duration":0,"curve":"linear"},"end":null}}"#;
        assert!(decode_to_stage(nan.as_bytes()).is_err());
    }

    #[test]
    fn bad_lines_are_refused_never_panicked_on() {
        let long = vec![b' '; MAX_LINE_BYTES + 1];
        assert!(matches!(
            decode_from_stage(&long),
            Err(LinkError::TooLong { .. })
        ));
        for bad in [
            &b""[..],
            b"not json",
            b"{\"type\":\"nope\"}",
            b"{\"type\":\"pong\"}",
            b"{\"type\":\"pong\",\"seq\":-1}",
            b"[1,2,3]",
            b"\xff\xfe",
        ] {
            assert!(
                matches!(decode_from_stage(bad), Err(LinkError::Unreadable(_))),
                "{bad:?}"
            );
            assert!(decode_to_stage(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn values_outside_the_limits_are_refused() {
        let display = |id: &str, name: &str, w: u32| {
            format!(
                r#"{{"type":"displays","displays":[{{"id":"{id}","name":"{name}","width":{w},"height":1080}}]}}"#
            )
        };
        assert!(decode_from_stage(display("D", "N", 1920).as_bytes()).is_ok());
        // A refresh rate is optional (0: unknown) and at most MAX_DISPLAY_FPS.
        let with_fps = |fps: u32| {
            format!(
                r#"{{"type":"displays","displays":[{{"id":"D","name":"N","width":1,"height":1,"fps":{fps}}}]}}"#
            )
        };
        assert!(decode_from_stage(with_fps(MAX_DISPLAY_FPS).as_bytes()).is_ok());
        assert_eq!(
            decode_from_stage(with_fps(MAX_DISPLAY_FPS + 1).as_bytes()),
            Err(LinkError::Invalid("display refresh rate out of range"))
        );
        for bad in [
            display("", "N", 1920),
            display("D", "", 1920),
            display("D", "N", 0),
            display("D", "N", 16_385),
            display("D\\u0007", "N", 1920),
            display(&"x".repeat(MAX_TEXT_BYTES + 1), "N", 1920),
        ] {
            assert!(
                matches!(
                    decode_from_stage(bad.as_bytes()),
                    Err(LinkError::Invalid(_))
                ),
                "{bad}"
            );
        }
        let many: Vec<Display> = (0..=MAX_DISPLAYS)
            .map(|i| Display {
                id: format!("D{i}"),
                name: "N".into(),
                width: 1,
                height: 1,
                fps: 0,
            })
            .collect();
        let line = encode(&FromStage::Displays { displays: many }).unwrap();
        assert_eq!(
            decode_from_stage(line.as_bytes()),
            Err(LinkError::Invalid("too many displays"))
        );
        let big = ToStage::Stages {
            stages: vec![StageOutput {
                id: stage(1),
                width: 0,
                height: 1080,
                display: None,
                keep_black: true,
                stack: 0,
                monitor: None,
            }],
        };
        let line = encode(&big).unwrap();
        assert_eq!(
            decode_to_stage(line.as_bytes()),
            Err(LinkError::Invalid("stage size out of range"))
        );
        for title in ["", "a\u{7}b", &"t".repeat(MAX_TEXT_BYTES + 1)] {
            let titled = ToStage::Stages {
                stages: vec![StageOutput {
                    id: stage(1),
                    width: 1920,
                    height: 1080,
                    display: None,
                    keep_black: false,
                    stack: 0,
                    monitor: Some(title.to_owned()),
                }],
            };
            let line = encode(&titled).unwrap();
            assert_eq!(
                decode_to_stage(line.as_bytes()),
                Err(LinkError::Invalid("bad monitor title")),
                "{title:?}"
            );
        }
        let log = r#"{"type":"log","level":"warn","message":"a\u0000b"}"#;
        assert!(decode_from_stage(log.as_bytes()).is_err());
    }

    /// Any stage name becomes a title the link takes: control characters go, a long name is cut
    /// on a character boundary, an empty one takes the fallback.
    #[test]
    fn any_name_becomes_a_title_the_link_takes() {
        let ok = |t: &str| text_ok(t);
        assert_eq!(link_text("Stage 1", "Stage"), "Stage 1");
        assert_eq!(link_text("a\tb\nc", "Stage"), "a b c");
        assert_eq!(link_text("  \u{7} ", "Stage"), "Stage");
        let long = "é".repeat(MAX_TEXT_BYTES);
        let cut = link_text(&long, "Stage");
        assert!(ok(&cut) && cut.len() == MAX_TEXT_BYTES, "{}", cut.len());
        assert!(ok(&link_text(
            &format!("{}x", "y".repeat(MAX_TEXT_BYTES)),
            "S"
        )));
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn probes_are_checked_both_ways() {
        let probe = |points: &str| {
            format!(
                r#"{{"type":"probe","stage":"{}","points":{points}}}"#,
                stage(1)
            )
        };
        assert!(decode_to_stage(probe("[[0.5,0.5]]").as_bytes()).is_ok());
        for bad in ["[]", "[[1.5,0.5]]", "[[-0.1,0.5]]"] {
            assert!(decode_to_stage(probe(bad).as_bytes()).is_err(), "{bad}");
        }
        let colours = vec![[0u8; 4]; MAX_PROBE_POINTS + 1];
        let line = encode(&FromStage::Probed {
            stage: stage(1),
            frame: None,
            colours: colours.clone(),
        })
        .unwrap();
        assert!(decode_from_stage(line.as_bytes()).is_err());
        let line = encode(&FromStage::MonitorProbed {
            stage: stage(1),
            frame: None,
            colours,
        })
        .unwrap();
        assert!(decode_from_stage(line.as_bytes()).is_err());
        let monitor = |w: u32, points: Vec<[f32; 2]>| ToStage::ProbeMonitor {
            stage: stage(1),
            width: w,
            height: 300,
            points,
        };
        let good = monitor(400, vec![[0.5, 0.5]]);
        assert_eq!(
            decode_to_stage(encode(&good).unwrap().as_bytes()).unwrap(),
            good
        );
        for bad in [monitor(0, vec![[0.5, 0.5]]), monitor(400, vec![[2.0, 0.5]])] {
            assert!(decode_to_stage(encode(&bad).unwrap().as_bytes()).is_err());
        }
    }

    /// Test support: a media clock step reads back; one over a day is refused.
    #[cfg(feature = "test-support")]
    #[test]
    fn a_media_clock_step_is_bounded() {
        let step = ToStage::AdvanceMediaClock { ms: 500 };
        assert_eq!(
            decode_to_stage(encode(&step).unwrap().as_bytes()).unwrap(),
            step
        );
        let far = ToStage::AdvanceMediaClock { ms: 86_400_001 };
        assert!(decode_to_stage(encode(&far).unwrap().as_bytes()).is_err());
    }
}
