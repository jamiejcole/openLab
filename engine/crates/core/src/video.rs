// SPDX-License-Identifier: GPL-3.0-or-later

//! Video stages (VID-01, VID-02, VID-12): the named rasters Video cues are drawn on, each shown
//! on one display. QLab 5's simplest stage (manual pp. 207, 225-227): one region covering the
//! stage, one display. Which display a stage uses is per machine (`settings.rs`), as audio patch
//! mappings are, so a show file names no display.

use serde::{Deserialize, Serialize};

use crate::model::StageId;

/// Largest stage side in pixels (QLab 5 manual p. 208).
pub const MAX_STAGE_PIXELS: u32 = 16_384;
/// Most stages one workspace holds. A file with more keeps the first ones.
pub const MAX_STAGES: usize = 64;

/// One stage of a workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stage {
    pub id: StageId,
    pub name: String,
    /// Pixel size, 1 to [`MAX_STAGE_PIXELS`] each.
    pub width: u32,
    pub height: u32,
    /// Black stays up on the stage's display while no cue plays on it (QLab's "keep rendering
    /// between cues", p. 227). Off: the stage's window is hidden while nothing plays.
    #[serde(default = "keep_black_default")]
    pub keep_black: bool,
}

fn keep_black_default() -> bool {
    true
}

impl Stage {
    /// A stage sized as `width` x `height`, black between cues.
    pub fn new(id: StageId, name: impl Into<String>, width: u32, height: u32) -> Self {
        Self {
            id,
            name: name.into(),
            width: clamp_side(width),
            height: clamp_side(height),
            keep_black: true,
        }
    }

    /// Whether the stage's size is one a stage may have.
    pub fn size_ok(&self) -> bool {
        side_ok(self.width) && side_ok(self.height)
    }

    /// Puts the size within 1 to [`MAX_STAGE_PIXELS`]; `true` if it changed.
    pub fn clamp_size(&mut self) -> bool {
        let (w, h) = (clamp_side(self.width), clamp_side(self.height));
        let changed = (w, h) != (self.width, self.height);
        self.width = w;
        self.height = h;
        changed
    }
}

/// Whether `side` is a stage side length in pixels.
pub fn side_ok(side: u32) -> bool {
    (1..=MAX_STAGE_PIXELS).contains(&side)
}

fn clamp_side(side: u32) -> u32 {
    side.clamp(1, MAX_STAGE_PIXELS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn sizes_are_kept_within_one_to_the_largest_stage() {
        let mut s = Stage::new(StageId(Uuid::nil()), "S", 0, 99_999);
        assert_eq!((s.width, s.height), (1, MAX_STAGE_PIXELS));
        assert!(s.size_ok());
        s.width = 0;
        assert!(!s.size_ok());
        assert!(s.clamp_size());
        assert!(!s.clamp_size());
        assert_eq!(s.width, 1);
    }

    #[test]
    fn a_stage_without_keep_black_in_its_file_keeps_black() {
        let s: Stage = serde_json::from_str(
            r#"{"id":"00000000-0000-0000-0000-000000000001","name":"S","width":1920,"height":1080}"#,
        )
        .unwrap();
        assert!(s.keep_black);
    }
}
