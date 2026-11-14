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
/// The highest stage layer (QLab's layers run 0 to 1000).
pub const MAX_STAGE_LAYER: u16 = 1000;
/// A new stage's layer.
pub const DEFAULT_STAGE_LAYER: u16 = 1;

/// One stage of a workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stage {
    pub id: StageId,
    pub name: String,
    /// Pixel size, 1 to [`MAX_STAGE_PIXELS`] each.
    pub width: u32,
    pub height: u32,
    /// Black stays up on the stage's display while no cue plays on it (QLab's "keep rendering
    /// between cues", p. 227). Off (the default, as QLab draws a stage only while a cue runs on
    /// it): the stage's window is hidden while nothing plays, so a stage on the operator's own
    /// display never covers it between cues.
    #[serde(default)]
    pub keep_black: bool,
    /// Stacking order among stages on the same display (p. 227): a higher layer is drawn on
    /// top; on the same layer, the stage whose name sorts first is on top. 0 to
    /// [`MAX_STAGE_LAYER`].
    #[serde(default = "default_layer")]
    pub layer: u16,
}

fn default_layer() -> u16 {
    DEFAULT_STAGE_LAYER
}

impl Stage {
    /// A stage sized as `width` x `height`, hidden between cues.
    pub fn new(id: StageId, name: impl Into<String>, width: u32, height: u32) -> Self {
        Self {
            id,
            name: name.into(),
            width: clamp_side(width),
            height: clamp_side(height),
            keep_black: false,
            layer: DEFAULT_STAGE_LAYER,
        }
    }

    /// Whether the stage's size is one a stage may have.
    pub fn size_ok(&self) -> bool {
        side_ok(self.width) && side_ok(self.height)
    }

    /// Puts the layer within 0 to [`MAX_STAGE_LAYER`]; `true` if it changed.
    pub fn clamp_layer(&mut self) -> bool {
        let layer = self.layer.min(MAX_STAGE_LAYER);
        std::mem::replace(&mut self.layer, layer) != layer
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

/// One editable property of a stage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StageProperty {
    Name {
        value: String,
    },
    /// Pixel size, 1 to [`MAX_STAGE_PIXELS`] each.
    Size {
        width: u32,
        height: u32,
    },
    KeepBlack {
        value: bool,
    },
    /// 0 to [`MAX_STAGE_LAYER`].
    Layer {
        value: u16,
    },
}

impl StageProperty {
    pub(crate) fn current(&self, stage: &Stage) -> Self {
        match self {
            Self::Name { .. } => Self::Name {
                value: stage.name.clone(),
            },
            Self::Size { .. } => Self::Size {
                width: stage.width,
                height: stage.height,
            },
            Self::KeepBlack { .. } => Self::KeepBlack {
                value: stage.keep_black,
            },
            Self::Layer { .. } => Self::Layer { value: stage.layer },
        }
    }

    pub(crate) fn write(&self, stage: &mut Stage) {
        match self {
            Self::Name { value } => stage.name.clone_from(value),
            Self::Size { width, height } => {
                stage.width = *width;
                stage.height = *height;
            }
            Self::KeepBlack { value } => stage.keep_black = *value,
            Self::Layer { value } => stage.layer = *value,
        }
    }
}

/// Why `stage` cannot be added or become what it is (size, layer or name out of their limits).
pub(crate) fn stage_problem(stage: &Stage) -> Option<&'static str> {
    if !stage.size_ok() {
        return Some("a stage is 1 to 16384 pixels a side");
    }
    if stage.layer > MAX_STAGE_LAYER {
        return Some("a stage layer is 0 to 1000");
    }
    crate::validate::name_problem(&stage.name)
}

/// The stacking order of stages that share a display, bottom first. A higher layer is on top;
/// on the same layer the stage whose name sorts first is on top (QLab 5 manual p. 227); the id
/// breaks a tie of both.
pub fn stack_order<'a>(stages: impl IntoIterator<Item = &'a Stage>) -> Vec<StageId> {
    let mut all: Vec<&Stage> = stages.into_iter().collect();
    all.sort_by(|a, b| {
        a.layer
            .cmp(&b.layer)
            .then_with(|| b.name.cmp(&a.name))
            .then_with(|| a.id.cmp(&b.id))
    });
    all.into_iter().map(|s| s.id).collect()
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

    /// p. 227: a higher layer is on top; on one layer the name sorting first is on top.
    #[test]
    fn stages_stack_by_layer_then_name() {
        let st = |n: u128, name: &str, layer: u16| {
            let mut s = Stage::new(StageId(Uuid::from_u128(n)), name, 8, 8);
            s.layer = layer;
            s
        };
        let stages = [
            st(1, "Angelica", 100),
            st(2, "Mary", 99),
            st(3, "Eliza", 50),
            st(4, "Abraham", 99),
        ];
        let id = |n: u128| StageId(Uuid::from_u128(n));
        // Bottom to top.
        assert_eq!(stack_order(&stages), [id(3), id(2), id(4), id(1)]);
    }

    #[test]
    fn a_stage_without_keep_black_in_its_file_is_hidden_between_cues() {
        let s: Stage = serde_json::from_str(
            r#"{"id":"00000000-0000-0000-0000-000000000001","name":"S","width":1920,"height":1080}"#,
        )
        .unwrap();
        assert!(!s.keep_black);
        assert_eq!(s.layer, DEFAULT_STAGE_LAYER);
    }
}
