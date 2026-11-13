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

/// The link version both ends speak; a stage host answering another version is not used.
pub const LINK_VERSION: u32 = 1;
/// Longest line either end accepts, newline excluded.
pub const MAX_LINE_BYTES: usize = 64 * 1024;
/// Most displays one `displays` message lists.
pub const MAX_DISPLAYS: usize = 32;
/// Longest display id, display name or log message, in bytes.
pub const MAX_TEXT_BYTES: usize = 512;
/// Most stages one `stages` message carries: every stage of every open workspace (at most 8).
pub const MAX_LINK_STAGES: usize = MAX_STAGES * 8;
/// Most points one probe reads.
pub const MAX_PROBE_POINTS: usize = 64;

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
    /// Test support: stop reading and answering, as a hung stage host would.
    #[cfg(feature = "test-support")]
    Hang,
    /// Test support: a stage host on virtual displays now has these, as if displays were
    /// plugged in or unplugged.
    #[cfg(feature = "test-support")]
    SetDisplays { displays: Vec<Display> },
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
    /// Test support: the answer to `probe`: the frame number read from the barcode, if the
    /// stage shows one, and the colour (RGBA, 0-255) at each point asked for.
    #[cfg(feature = "test-support")]
    Probed {
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
    }
    Ok(())
}

fn check_to_stage(message: &ToStage) -> Result<(), LinkError> {
    match message {
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
            }
            Ok(())
        }
        #[cfg(feature = "test-support")]
        ToStage::SetDisplays { displays } => check_displays(displays),
        #[cfg(feature = "test-support")]
        ToStage::Probe { points, .. } => {
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
        #[cfg(feature = "test-support")]
        FromStage::Probed { colours, .. } if colours.len() > MAX_PROBE_POINTS => {
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
        let chunk = input.fill_buf()?;
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
                }],
            },
            ToStage::Ping { seq: 7 },
            ToStage::Quit,
        ];
        for m in to {
            let line = encode(&m).unwrap();
            assert_eq!(line.matches('\n').count(), 1);
            assert!(line.ends_with('\n'));
            assert_eq!(decode_to_stage(line.as_bytes()).unwrap(), m);
        }
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
                }],
            },
            FromStage::StageStatus {
                stage: stage(1),
                state: StageState::DisplayMissing {
                    display: "D1".into(),
                },
            },
            FromStage::Pong { seq: 7 },
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
            }],
        };
        let line = encode(&big).unwrap();
        assert_eq!(
            decode_to_stage(line.as_bytes()),
            Err(LinkError::Invalid("stage size out of range"))
        );
        let log = r#"{"type":"log","level":"warn","message":"a\u0000b"}"#;
        assert!(decode_from_stage(log.as_bytes()).is_err());
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
            colours,
        })
        .unwrap();
        assert!(decode_from_stage(line.as_bytes()).is_err());
    }
}
