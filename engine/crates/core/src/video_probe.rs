// SPDX-License-Identifier: GPL-3.0-or-later

//! What a first look at a video file or still image found (CT-03, VID-03). The engine never
//! opens such a file itself: a throwaway stage host process (`openlab-stage --probe <file>`)
//! reads it and prints one line, [`ProbeReport`], then exits. A damaged file that crashes or
//! hangs the platform's decoder takes only that process with it. Contract: docs/protocol.md,
//! "Stage host link".
//!
//! The line is validated like every link line: anything too long, not JSON, of an unknown kind
//! or outside the limits below is refused, never a panic.

use serde::{Deserialize, Serialize};

use crate::cue::MAX_TIME;
use crate::stage_link::{LinkError, MAX_TEXT_BYTES};

/// Widest and tallest picture a Video cue plays, in pixels (the stage limit).
pub const MAX_VIDEO_PIXELS: u32 = crate::video::MAX_STAGE_PIXELS;
/// Fastest frame rate a video may have, frames per second.
pub const MAX_FRAME_RATE: f64 = 1000.0;
/// Longest probe line, newline excluded.
pub const MAX_PROBE_LINE_BYTES: usize = 4096;

/// The moving-picture codecs a Video cue plays (VID-03; the 2014 Mac mini target is H.264,
/// ProRes Proxy and HAP).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    H264,
    Hevc,
    /// Every ProRes 422 and 4444 flavour.
    #[serde(rename = "prores")]
    ProRes,
    /// HAP, HAP Alpha and HAP Q; decoded by openLab itself, not the platform.
    Hap,
}

/// The still image formats a Video cue shows (VID-03).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StillFormat {
    Png,
    Jpeg,
    Tiff,
}

/// What the probe found.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProbeReport {
    /// A video file it can play: its codec, pixel size, frame rate and length in seconds.
    Video {
        codec: VideoCodec,
        width: u32,
        height: u32,
        fps: f64,
        duration: f64,
    },
    /// A still image it can show.
    Still {
        format: StillFormat,
        width: u32,
        height: u32,
    },
    /// No file there.
    Missing,
    /// A file it cannot play; `reason` is English, for the cue's problem and the log.
    Unsupported { reason: String },
}

impl ProbeReport {
    /// The report as one line (no newline).
    pub fn to_line(&self) -> Result<String, LinkError> {
        serde_json::to_string(self).map_err(|e| LinkError::Unreadable(e.to_string()))
    }
}

/// Reads and checks a probe line. Every number is in range and every text bounded, so what
/// the engine keeps of a file is as safe as what it keeps of a show file.
pub fn decode_probe_report(line: &str) -> Result<ProbeReport, LinkError> {
    if line.len() > MAX_PROBE_LINE_BYTES {
        return Err(LinkError::TooLong { bytes: line.len() });
    }
    let report: ProbeReport =
        serde_json::from_str(line).map_err(|e| LinkError::Unreadable(e.to_string()))?;
    let side = |n: u32| (1..=MAX_VIDEO_PIXELS).contains(&n);
    let ok = match &report {
        ProbeReport::Video {
            width,
            height,
            fps,
            duration,
            ..
        } => {
            side(*width)
                && side(*height)
                && fps.is_finite()
                && *fps > 0.0
                && *fps <= MAX_FRAME_RATE
                && duration.is_finite()
                && *duration > 0.0
                && *duration <= MAX_TIME
        }
        ProbeReport::Still { width, height, .. } => side(*width) && side(*height),
        ProbeReport::Missing => true,
        ProbeReport::Unsupported { reason } => {
            !reason.is_empty()
                && reason.len() <= MAX_TEXT_BYTES
                && !reason.chars().any(char::is_control)
        }
    };
    if ok {
        Ok(report)
    } else {
        Err(LinkError::Invalid("a probe report value is out of range"))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn a_report_goes_round_the_line_and_values_out_of_range_are_refused() {
        let video = ProbeReport::Video {
            codec: VideoCodec::ProRes,
            width: 1920,
            height: 1080,
            fps: 25.0,
            duration: 12.5,
        };
        assert_eq!(
            decode_probe_report(&video.to_line().unwrap()).unwrap(),
            video
        );
        for bad in [
            r#"{"type":"video","codec":"h264","width":0,"height":1080,"fps":25.0,"duration":1.0}"#,
            r#"{"type":"video","codec":"h264","width":16385,"height":1080,"fps":25.0,"duration":1.0}"#,
            r#"{"type":"video","codec":"h264","width":8,"height":8,"fps":0.0,"duration":1.0}"#,
            r#"{"type":"video","codec":"h264","width":8,"height":8,"fps":1001.0,"duration":1.0}"#,
            r#"{"type":"video","codec":"h264","width":8,"height":8,"fps":25.0,"duration":0.0}"#,
            r#"{"type":"video","codec":"h264","width":8,"height":8,"fps":25.0,"duration":90000.0}"#,
            r#"{"type":"video","codec":"vp9","width":8,"height":8,"fps":25.0,"duration":1.0}"#,
            r#"{"type":"still","format":"gif","width":8,"height":8}"#,
            r#"{"type":"still","format":"png","width":8,"height":-1}"#,
            r#"{"type":"unsupported","reason":""}"#,
            r#"{"type":"unsupported","reason":"a\nb"}"#,
            r#"{"type":"nothing"}"#,
            "not json",
        ] {
            assert!(decode_probe_report(bad).is_err(), "{bad}");
        }
        let long = format!(
            r#"{{"type":"unsupported","reason":"{}"}}"#,
            "x".repeat(MAX_TEXT_BYTES + 1)
        );
        assert!(decode_probe_report(&long).is_err());
        assert!(decode_probe_report(&" ".repeat(MAX_PROBE_LINE_BYTES + 1)).is_err());
    }
}
