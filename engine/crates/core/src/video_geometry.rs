// SPDX-License-Identifier: GPL-3.0-or-later

//! A Video cue's geometry (VID-04; QLab 5 manual pp. 212-214): how its picture sits on its
//! stage. Either it fills the stage (fit, fill or stretch), or it is drawn at its own pixel
//! size, moved, scaled, rotated about its anchor and cropped. Opacity, layer and smoothing
//! apply in both modes.
//!
//! Stored in the Video cue's kind (file format §5) and sent to the stage host with each
//! picture (docs/protocol.md, "Stage host link"); [`quad`] is where the stage host draws it.

use serde::{Deserialize, Serialize};

use crate::video::MAX_STAGE_PIXELS;

/// The highest cue layer: drawn on top of every other layer (p. 213: layers 0 to 1000).
pub const MAX_CUE_LAYER: u16 = 1000;
/// A new Video cue's layer: the top one, as QLab gives a new cue.
pub const DEFAULT_CUE_LAYER: u16 = MAX_CUE_LAYER;
/// Furthest a picture may be moved, or its anchor set, from the centre, in pixels either way.
pub const MAX_TRANSLATION: f64 = 65_536.0;
/// Largest scale either way (a negative scale mirrors the picture).
pub const MAX_SCALE: f64 = 100.0;
/// Largest rotation either way, in degrees.
pub const MAX_ROTATION: f64 = 360.0;
/// Most pixels cropped from one edge: the largest picture side.
pub const MAX_CROP: f64 = MAX_STAGE_PIXELS as f64;

/// How a picture fills its stage in fill-stage mode (p. 212).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FillStyle {
    /// As large as the stage allows with its shape kept; what is left of the stage shows what
    /// is below.
    #[default]
    Fit,
    /// Covers the stage with its shape kept; what overflows is not drawn.
    Fill,
    /// Takes the stage's shape.
    Stretch,
}

/// A pair of numbers: a position in pixels or a scale.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pair {
    pub x: f64,
    pub y: f64,
}

/// Pixels cut off each edge of the picture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Crop {
    pub top: f64,
    pub bottom: f64,
    pub left: f64,
    pub right: f64,
}

/// A Video cue's geometry. Stored as fields of the cue's kind; a field missing from a file is
/// its default.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoGeometry {
    /// Fill the stage in `fill_style` (true), or draw the picture at its own size placed by
    /// the custom fields (false).
    pub fill_stage: bool,
    pub fill_style: FillStyle,
    /// Where the anchor is moved to, in stage pixels from the stage's centre, y up
    /// (±[`MAX_TRANSLATION`]). Custom mode only.
    pub translation: Pair,
    /// Times the picture's own pixel size, ±[`MAX_SCALE`]. Custom mode only.
    pub scale: Pair,
    /// The scale's x and y are edited together (the app keeps them equal).
    pub scale_locked: bool,
    /// Degrees anticlockwise about the anchor, ±[`MAX_ROTATION`]. Custom mode only.
    pub rotation: f64,
    /// Pixels of the picture cut off each edge, 0 to [`MAX_CROP`]. Custom mode only.
    pub crop: Crop,
    /// The point the picture is scaled and rotated about and moved by, in picture pixels from
    /// its centre, y up (±[`MAX_TRANSLATION`]). Custom mode only.
    pub anchor: Pair,
    /// 0 (transparent) to 1 (opaque); the picture's own transparency is kept at 1.
    pub opacity: f64,
    /// Stacking among the cues on its stage, 0 (bottom) to [`MAX_CUE_LAYER`] (top).
    pub layer: u16,
    /// Smooth scaling (true) or nearest-neighbour, hard pixel edges (false).
    pub smooth: bool,
}

impl Default for VideoGeometry {
    fn default() -> Self {
        Self {
            fill_stage: true,
            fill_style: FillStyle::Fit,
            translation: Pair { x: 0.0, y: 0.0 },
            scale: Pair { x: 1.0, y: 1.0 },
            scale_locked: true,
            rotation: 0.0,
            crop: Crop::default(),
            anchor: Pair { x: 0.0, y: 0.0 },
            opacity: 1.0,
            layer: DEFAULT_CUE_LAYER,
            smooth: true,
        }
    }
}

/// One numeric field of the geometry: its stored name and its range.
struct Field {
    name: &'static str,
    min: f64,
    max: f64,
}

const TRANSLATION_X: Field = Field::pm("translation.x", MAX_TRANSLATION);
const TRANSLATION_Y: Field = Field::pm("translation.y", MAX_TRANSLATION);
const SCALE_X: Field = Field::pm("scale.x", MAX_SCALE);
const SCALE_Y: Field = Field::pm("scale.y", MAX_SCALE);
const ROTATION: Field = Field::pm("rotation", MAX_ROTATION);
const CROP_TOP: Field = Field::up("crop.top", MAX_CROP);
const CROP_BOTTOM: Field = Field::up("crop.bottom", MAX_CROP);
const CROP_LEFT: Field = Field::up("crop.left", MAX_CROP);
const CROP_RIGHT: Field = Field::up("crop.right", MAX_CROP);
const ANCHOR_X: Field = Field::pm("anchor.x", MAX_TRANSLATION);
const ANCHOR_Y: Field = Field::pm("anchor.y", MAX_TRANSLATION);
const OPACITY: Field = Field::up("opacity", 1.0);

impl Field {
    const fn pm(name: &'static str, max: f64) -> Self {
        Self {
            name,
            min: -max,
            max,
        }
    }

    const fn up(name: &'static str, max: f64) -> Self {
        Self {
            name,
            min: 0.0,
            max,
        }
    }

    fn ok(&self, v: f64) -> bool {
        v.is_finite() && (self.min..=self.max).contains(&v)
    }
}

impl VideoGeometry {
    /// Each number with its range, mutable.
    fn numbers(&mut self) -> [(&mut f64, Field, f64); 12] {
        let d = Self::default();
        [
            (&mut self.translation.x, TRANSLATION_X, d.translation.x),
            (&mut self.translation.y, TRANSLATION_Y, d.translation.y),
            (&mut self.scale.x, SCALE_X, d.scale.x),
            (&mut self.scale.y, SCALE_Y, d.scale.y),
            (&mut self.rotation, ROTATION, d.rotation),
            (&mut self.crop.top, CROP_TOP, 0.0),
            (&mut self.crop.bottom, CROP_BOTTOM, 0.0),
            (&mut self.crop.left, CROP_LEFT, 0.0),
            (&mut self.crop.right, CROP_RIGHT, 0.0),
            (&mut self.anchor.x, ANCHOR_X, d.anchor.x),
            (&mut self.anchor.y, ANCHOR_Y, d.anchor.y),
            (&mut self.opacity, OPACITY, d.opacity),
        ]
    }

    /// The stored names of the fields outside their ranges (a number that is not finite too).
    pub fn out_of_range(&self) -> Vec<&'static str> {
        let mut copy = *self;
        let mut bad: Vec<&'static str> = copy
            .numbers()
            .into_iter()
            .filter(|(v, f, _)| !f.ok(**v))
            .map(|(_, f, _)| f.name)
            .collect();
        if self.layer > MAX_CUE_LAYER {
            bad.push("layer");
        }
        bad
    }

    /// Whether every field is within its range.
    pub fn is_valid(&self) -> bool {
        self.out_of_range().is_empty()
    }

    /// Puts every field within its range: a number beyond it at its end, one that is not
    /// finite at its default. Returns the names of the fields changed.
    pub fn repair(&mut self) -> Vec<&'static str> {
        let mut changed = Vec::new();
        for (v, f, default) in self.numbers() {
            if f.ok(*v) {
                continue;
            }
            *v = if v.is_finite() {
                v.clamp(f.min, f.max)
            } else {
                default
            };
            changed.push(f.name);
        }
        if self.layer > MAX_CUE_LAYER {
            self.layer = MAX_CUE_LAYER;
            changed.push("layer");
        }
        changed
    }
}

/// Where a picture is drawn on its stage: the corners of the (cropped) picture in stage pixels
/// from the stage's top left, y down, in the order top left, top right, bottom left, bottom
/// right of the picture; and the part of the picture's texture they show (`[u0, v0, u1, v1]`,
/// fractions from its top left).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quad {
    pub corners: [[f64; 2]; 4],
    pub uv: [f64; 4],
}

/// Where a picture of `picture` pixels with `geometry` is drawn on a stage of `stage` pixels.
/// `None` when nothing of it is drawn: a picture or stage of no size, or a crop that leaves
/// nothing. Pure maths: the stage host draws this quad; tests check it for every mode.
pub fn quad(geometry: &VideoGeometry, picture: (u32, u32), stage: (u32, u32)) -> Option<Quad> {
    let (pw, ph) = (f64::from(picture.0), f64::from(picture.1));
    let (sw, sh) = (f64::from(stage.0), f64::from(stage.1));
    if pw <= 0.0 || ph <= 0.0 || sw <= 0.0 || sh <= 0.0 {
        return None;
    }
    if geometry.fill_stage {
        let (w, h) = match geometry.fill_style {
            FillStyle::Fit => {
                let s = (sw / pw).min(sh / ph);
                (pw * s, ph * s)
            }
            FillStyle::Fill => {
                let s = (sw / pw).max(sh / ph);
                (pw * s, ph * s)
            }
            FillStyle::Stretch => (sw, sh),
        };
        let (x0, y0) = ((sw - w) / 2.0, (sh - h) / 2.0);
        let (x1, y1) = (x0 + w, y0 + h);
        return Some(Quad {
            corners: [[x0, y0], [x1, y0], [x0, y1], [x1, y1]],
            uv: [0.0, 0.0, 1.0, 1.0],
        });
    }
    // Custom: the picture's own pixels, centre at the origin, y up.
    let c = geometry.crop;
    let (left, right) = (-pw / 2.0 + c.left.max(0.0), pw / 2.0 - c.right.max(0.0));
    let (bottom, top) = (-ph / 2.0 + c.bottom.max(0.0), ph / 2.0 - c.top.max(0.0));
    if right <= left || top <= bottom {
        return None;
    }
    let a = geometry.anchor;
    let (sin, cos) = geometry.rotation.to_radians().sin_cos();
    let place = |x: f64, y: f64| {
        // Scaled and rotated about the anchor, then the anchor moved by the translation.
        let (dx, dy) = ((x - a.x) * geometry.scale.x, (y - a.y) * geometry.scale.y);
        let (rx, ry) = (dx * cos - dy * sin, dx * sin + dy * cos);
        let (x, y) = (
            rx + a.x + geometry.translation.x,
            ry + a.y + geometry.translation.y,
        );
        // Stage pixels from the top left, y down.
        [sw / 2.0 + x, sh / 2.0 - y]
    };
    Some(Quad {
        corners: [
            place(left, top),
            place(right, top),
            place(left, bottom),
            place(right, bottom),
        ],
        uv: [
            (left + pw / 2.0) / pw,
            (ph / 2.0 - top) / ph,
            (right + pw / 2.0) / pw,
            (ph / 2.0 - bottom) / ph,
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom() -> VideoGeometry {
        VideoGeometry {
            fill_stage: false,
            ..VideoGeometry::default()
        }
    }

    fn fill(style: FillStyle) -> VideoGeometry {
        VideoGeometry {
            fill_style: style,
            ..VideoGeometry::default()
        }
    }

    fn close(a: [[f64; 2]; 4], b: [[f64; 2]; 4]) -> bool {
        a.iter()
            .flatten()
            .zip(b.iter().flatten())
            .all(|(x, y)| (x - y).abs() < 1e-9)
    }

    /// The rect a quad covers when it is not rotated: (x0, y0, x1, y1).
    fn rect(q: Quad) -> [f64; 4] {
        [
            q.corners[0][0],
            q.corners[0][1],
            q.corners[3][0],
            q.corners[3][1],
        ]
    }

    #[test]
    fn fit_shows_the_whole_picture_as_large_as_the_stage_allows_for_every_shape() {
        let stage = (1920, 1080);
        // Wider than the stage: bars above and below.
        assert_eq!(
            rect(quad(&fill(FillStyle::Fit), (3840, 1080), stage).unwrap()),
            [0.0, 270.0, 1920.0, 810.0]
        );
        // Taller: bars at the sides.
        assert_eq!(
            rect(quad(&fill(FillStyle::Fit), (1080, 1920), stage).unwrap()),
            [960.0 - 303.75, 0.0, 960.0 + 303.75, 1080.0]
        );
        // The same shape, smaller: scaled up to the whole stage.
        assert_eq!(
            rect(quad(&fill(FillStyle::Fit), (640, 360), stage).unwrap()),
            [0.0, 0.0, 1920.0, 1080.0]
        );
    }

    #[test]
    fn fill_covers_the_stage_and_lets_the_overflow_go_for_every_shape() {
        let stage = (1920, 1080);
        assert_eq!(
            rect(quad(&fill(FillStyle::Fill), (3840, 1080), stage).unwrap()),
            [-960.0, 0.0, 2880.0, 1080.0]
        );
        let tall = rect(quad(&fill(FillStyle::Fill), (1080, 1920), stage).unwrap());
        assert_eq!([tall[0], tall[2]], [0.0, 1920.0]);
        assert!(tall[1] < 0.0 && tall[3] > 1080.0);
        assert_eq!(
            rect(quad(&fill(FillStyle::Fill), (640, 360), stage).unwrap()),
            [0.0, 0.0, 1920.0, 1080.0]
        );
    }

    #[test]
    fn stretch_takes_the_stage_shape_for_every_shape() {
        for picture in [(3840, 1080), (1080, 1920), (640, 360), (1, 1)] {
            let q = quad(&fill(FillStyle::Stretch), picture, (1920, 1080)).unwrap();
            assert_eq!(rect(q), [0.0, 0.0, 1920.0, 1080.0]);
            assert_eq!(q.uv, [0.0, 0.0, 1.0, 1.0]);
        }
    }

    #[test]
    fn custom_draws_the_picture_at_its_own_size_in_the_middle() {
        let q = quad(&custom(), (640, 360), (1920, 1080)).unwrap();
        assert_eq!(rect(q), [640.0, 360.0, 1280.0, 720.0]);
    }

    #[test]
    fn translation_moves_right_and_up() {
        let g = VideoGeometry {
            translation: Pair { x: 100.0, y: 50.0 },
            ..custom()
        };
        let q = quad(&g, (640, 360), (1920, 1080)).unwrap();
        assert_eq!(rect(q), [740.0, 310.0, 1380.0, 670.0]);
    }

    #[test]
    fn scale_is_about_the_anchor_and_x_and_y_may_differ() {
        let g = VideoGeometry {
            scale: Pair { x: 2.0, y: 0.5 },
            ..custom()
        };
        let q = quad(&g, (100, 100), (1000, 1000)).unwrap();
        assert_eq!(rect(q), [400.0, 475.0, 600.0, 525.0]);
        // About the top-left corner: that corner stays put.
        let g = VideoGeometry {
            scale: Pair { x: 2.0, y: 2.0 },
            anchor: Pair { x: -50.0, y: 50.0 },
            ..custom()
        };
        let q = quad(&g, (100, 100), (1000, 1000)).unwrap();
        assert_eq!(rect(q), [450.0, 450.0, 650.0, 650.0]);
    }

    #[test]
    fn rotation_turns_about_the_anchor() {
        // A quarter turn anticlockwise about the centre: the top left corner goes to the
        // bottom left.
        let g = VideoGeometry {
            rotation: 90.0,
            ..custom()
        };
        let q = quad(&g, (200, 100), (1000, 1000)).unwrap();
        assert!(close(
            q.corners,
            [
                [450.0, 600.0],
                [450.0, 400.0],
                [550.0, 600.0],
                [550.0, 400.0]
            ]
        ));
        // About its right edge's middle: that point stays put.
        let g = VideoGeometry {
            rotation: 180.0,
            anchor: Pair { x: 100.0, y: 0.0 },
            ..custom()
        };
        let q = quad(&g, (200, 100), (1000, 1000)).unwrap();
        assert!(close(
            q.corners,
            [
                [800.0, 550.0],
                [600.0, 550.0],
                [800.0, 450.0],
                [600.0, 450.0]
            ]
        ));
    }

    #[test]
    fn crop_cuts_pixels_off_each_edge_and_shows_only_what_is_left() {
        let g = VideoGeometry {
            crop: Crop {
                top: 10.0,
                bottom: 20.0,
                left: 30.0,
                right: 40.0,
            },
            ..custom()
        };
        let q = quad(&g, (200, 100), (1000, 1000)).unwrap();
        assert_eq!(rect(q), [430.0, 460.0, 560.0, 530.0]);
        assert!(close(
            [[q.uv[0], q.uv[1]], [q.uv[2], q.uv[3]], [0.0; 2], [0.0; 2]],
            [[0.15, 0.1], [0.8, 0.8], [0.0; 2], [0.0; 2]]
        ));
        // Crop that leaves nothing draws nothing.
        let g = VideoGeometry {
            crop: Crop {
                left: 120.0,
                right: 80.0,
                ..Crop::default()
            },
            ..custom()
        };
        assert_eq!(quad(&g, (200, 100), (1000, 1000)), None);
    }

    #[test]
    fn fill_stage_ignores_the_custom_fields() {
        let g = VideoGeometry {
            translation: Pair { x: 500.0, y: 0.0 },
            rotation: 45.0,
            crop: Crop {
                top: 50.0,
                ..Crop::default()
            },
            ..VideoGeometry::default()
        };
        assert_eq!(
            quad(&g, (1920, 1080), (1920, 1080)),
            quad(&VideoGeometry::default(), (1920, 1080), (1920, 1080))
        );
    }

    #[test]
    fn nothing_of_no_size_is_drawn() {
        assert_eq!(quad(&custom(), (0, 10), (10, 10)), None);
        assert_eq!(quad(&custom(), (10, 10), (10, 0)), None);
    }

    #[test]
    fn ranges_are_checked_and_repaired_field_by_field() {
        assert!(VideoGeometry::default().is_valid());
        let mut g = VideoGeometry {
            opacity: 1.5,
            layer: 1001,
            rotation: f64::NAN,
            crop: Crop {
                left: -1.0,
                ..Crop::default()
            },
            scale: Pair {
                x: 101.0,
                y: -101.0,
            },
            translation: Pair {
                x: MAX_TRANSLATION + 1.0,
                y: 0.0,
            },
            ..VideoGeometry::default()
        };
        let bad = g.out_of_range();
        assert_eq!(
            bad,
            [
                "translation.x",
                "scale.x",
                "scale.y",
                "rotation",
                "crop.left",
                "opacity",
                "layer"
            ]
        );
        assert_eq!(g.repair(), bad);
        assert!(g.is_valid());
        assert_eq!(g.opacity, 1.0);
        assert_eq!(g.layer, MAX_CUE_LAYER);
        assert_eq!(g.rotation, 0.0);
        assert_eq!(g.crop.left, 0.0);
        assert_eq!(
            g.scale,
            Pair {
                x: 100.0,
                y: -100.0
            }
        );
        assert_eq!(g.translation.x, MAX_TRANSLATION);
        // The ends of each range are inside it.
        let edge = VideoGeometry {
            opacity: 0.0,
            layer: 0,
            rotation: -MAX_ROTATION,
            scale: Pair {
                x: -MAX_SCALE,
                y: 0.0,
            },
            crop: Crop {
                top: MAX_CROP,
                ..Crop::default()
            },
            ..VideoGeometry::default()
        };
        assert!(edge.is_valid());
    }
}
