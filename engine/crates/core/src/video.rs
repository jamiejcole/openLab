// SPDX-License-Identifier: GPL-3.0-or-later

//! Video stages and output routes (VID-01, VID-02, VID-12). QLab 5's chain (manual pp. 207,
//! 225-238): a stage is the named raster Video cues are drawn on; its regions are rectangles of
//! it, each sent to one output route (or none); a route is drawn on one display, turned and
//! scaled to it. Which display a route uses is per machine (`settings.rs`), as audio patch
//! mappings are, so a show file names no display.

use serde::{Deserialize, Serialize};

use crate::model::{RegionId, RouteId, StageId};

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
    /// Black stays up on the stage's display while no cue plays on it, once a picture has
    /// played there, until a panic (QLab's "keep rendering between cues", p. 227). On by default
    /// and for a file that does not say: what QLab shows after a cue's media ends. Off: the
    /// stage's window is hidden while nothing plays.
    #[serde(default = "default_keep_black")]
    pub keep_black: bool,
    /// Stacking order among stages on the same display (p. 227): a higher layer is drawn on
    /// top; on the same layer, the stage whose name sorts first is on top. 0 to
    /// [`MAX_STAGE_LAYER`].
    #[serde(default = "default_layer")]
    pub layer: u16,
    /// Its regions (p. 228), at most [`MAX_REGIONS`]: each a rectangle inside the stage sent to
    /// one route. Absent from a file: none (the stage is drawn on no display). Each region's
    /// letter and colour come from its place in this list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<Region>,
}

/// Largest output route side in pixels (QLab 5 manual p. 233). No buffer of a route's size is
/// ever made: a route only says how its regions are placed on its display.
pub const MAX_ROUTE_PIXELS: u32 = 30_000;
/// Most output routes one workspace holds. A file with more keeps the first ones.
pub const MAX_ROUTES: usize = 64;
/// Most regions one stage holds. A file with more keeps the first ones.
pub const MAX_REGIONS: usize = 16;

/// How a route's picture is put on its display (p. 233).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteScaling {
    /// One route pixel per display pixel, centred; what does not fit is cut off.
    Centre,
    /// As large as fits whole, shape kept, centred; black elsewhere.
    Fit,
    /// Squeezed or stretched to the display's shape.
    Stretch,
    /// Covers the display, shape kept, centred; what overhangs is cut off.
    #[default]
    Fill,
}

/// One output route of a workspace (QLab 5 manual pp. 232-234): a raster of its own size that
/// its regions fill, turned by `rotation` and put on its display by `scaling`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputRoute {
    pub id: RouteId,
    pub name: String,
    /// Pixel size, 1 to [`MAX_ROUTE_PIXELS`] each.
    pub width: u32,
    pub height: u32,
    /// Clockwise quarter turns in degrees: 0, 90, 180 or 270.
    #[serde(default)]
    pub rotation: u16,
    #[serde(default)]
    pub scaling: RouteScaling,
    /// The colour its alignment guides are drawn in (p. 233), as the operator picked it on the
    /// Output Routing page. A route is given one when it is added to a workspace or loaded
    /// without one ([`next_guide_colour`]) and keeps it whatever happens to the other routes;
    /// `None` only before that. Whether the guides show is not saved: the engine keeps it while
    /// the workspace is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guide_colour: Option<GuideColour>,
}

/// An alignment guide's colour (red, green, blue); `"#rrggbb"` in show files and on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GuideColour(pub [u8; 3]);

impl GuideColour {
    /// `"#rrggbb"` (either case) as a colour; `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        let hex = text.strip_prefix('#')?;
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let part = |at: usize| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok();
        Some(Self([part(0)?, part(2)?, part(4)?]))
    }
}

impl std::fmt::Display for GuideColour {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let [r, g, b] = self.0;
        write!(f, "#{r:02x}{g:02x}{b:02x}")
    }
}

impl Serialize for GuideColour {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for GuideColour {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text)
            .ok_or_else(|| serde::de::Error::custom("a guide colour is written #rrggbb"))
    }
}

/// The guide colours a new route takes by default, in turn, so the guides of routes can be told
/// apart.
pub const GUIDE_COLOURS: [GuideColour; 8] = [
    GuideColour([0x00, 0xc8, 0xff]),
    GuideColour([0xff, 0x3c, 0xc8]),
    GuideColour([0xff, 0xd2, 0x00]),
    GuideColour([0x3c, 0xdc, 0x50]),
    GuideColour([0xff, 0x82, 0x00]),
    GuideColour([0x96, 0x6e, 0xff]),
    GuideColour([0xff, 0x46, 0x46]),
    GuideColour([0xff, 0xff, 0xff]),
];

/// The default colour for a route added to `routes`: the first of [`GUIDE_COLOURS`] no route
/// there has, or, with all in use, the one the fewest have (the earliest on a tie).
pub fn next_guide_colour(routes: &[OutputRoute]) -> GuideColour {
    next_guide_colour_except(routes, usize::MAX)
}

/// [`next_guide_colour`] for the route at `except` (its own colour does not count).
pub fn next_guide_colour_except(routes: &[OutputRoute], except: usize) -> GuideColour {
    let uses = |c: &GuideColour| {
        (routes.iter().enumerate())
            .filter(|(at, r)| *at != except && r.guide_colour == Some(*c))
            .count()
    };
    GUIDE_COLOURS
        .iter()
        .min_by_key(|c| uses(c))
        .copied()
        .unwrap_or(GUIDE_COLOURS[0])
}

/// Gives each route with no colour one, in list order (a stable repair for routes saved
/// without).
pub fn fill_guide_colours(routes: &mut [OutputRoute]) {
    for at in 0..routes.len() {
        if routes.get(at).is_some_and(|r| r.guide_colour.is_none()) {
            let colour = next_guide_colour(routes);
            if let Some(r) = routes.get_mut(at) {
                r.guide_colour = Some(colour);
            }
        }
    }
}

impl OutputRoute {
    /// A route sized as `width` x `height`, not turned, filling its display.
    pub fn new(id: RouteId, name: impl Into<String>, width: u32, height: u32) -> Self {
        Self {
            id,
            name: name.into(),
            width: width.clamp(1, MAX_ROUTE_PIXELS),
            height: height.clamp(1, MAX_ROUTE_PIXELS),
            rotation: 0,
            scaling: RouteScaling::Fill,
            guide_colour: None,
        }
    }

    /// The colour its guides are drawn in: the one it was given when it was created or loaded
    /// (never changed by its place in the list). A route not yet in a workspace has none.
    pub fn shown_guide_colour(&self) -> GuideColour {
        self.guide_colour.unwrap_or(GUIDE_COLOURS[0])
    }

    /// Puts size and rotation within their limits; `(size, rotation)` say which changed.
    pub fn repair(&mut self) -> (bool, bool) {
        let (w, h) = (
            self.width.clamp(1, MAX_ROUTE_PIXELS),
            self.height.clamp(1, MAX_ROUTE_PIXELS),
        );
        let size = (w, h) != (self.width, self.height);
        self.width = w;
        self.height = h;
        let rotation = !rotation_ok(self.rotation);
        if rotation {
            self.rotation = 0;
        }
        (size, rotation)
    }
}

/// Whether `degrees` is a route rotation: 0, 90, 180 or 270.
pub fn rotation_ok(degrees: u16) -> bool {
    matches!(degrees, 0 | 90 | 180 | 270)
}

/// Whether `side` is a route side length in pixels.
pub fn route_side_ok(side: u32) -> bool {
    (1..=MAX_ROUTE_PIXELS).contains(&side)
}

/// Why a route's size or rotation is refused, if it is: what an edit of its size, rotation or
/// scaling is checked by, so a route loaded with a blank name can still be edited.
pub(crate) fn route_shape_problem(route: &OutputRoute) -> Option<&'static str> {
    if !route_side_ok(route.width) || !route_side_ok(route.height) {
        return Some("an output route is 1 to 30000 pixels a side");
    }
    if !rotation_ok(route.rotation) {
        return Some("an output route turns by 0, 90, 180 or 270 degrees");
    }
    None
}

/// Why `route` cannot be added or become what it is.
pub(crate) fn route_problem(route: &OutputRoute) -> Option<&'static str> {
    if let Some(why) = route_shape_problem(route) {
        return Some(why);
    }
    if route.name.trim().is_empty() {
        return Some("an output route needs a name");
    }
    crate::validate::name_problem(&route.name)
}

/// The name of a new route: "Output N", N one past the highest such number in use.
pub fn next_route_name(routes: &[OutputRoute]) -> String {
    let highest = routes
        .iter()
        .filter_map(|r| r.name.strip_prefix("Output ")?.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    format!("Output {}", highest.saturating_add(1))
}

/// One region of a stage (p. 229): a rectangle of the stage, in whole stage pixels from its top
/// left, sent to one route (`None`: drawn nowhere). It fills its route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Region {
    pub id: RegionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<RouteId>,
    pub x: u32,
    pub y: u32,
    /// At least 1; the region lies wholly inside its stage.
    pub width: u32,
    pub height: u32,
}

impl Region {
    /// A region covering a stage of `width` x `height`.
    pub fn whole(id: RegionId, route: Option<RouteId>, width: u32, height: u32) -> Self {
        Self {
            id,
            route,
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    /// Whether the region lies wholly inside a stage of `width` x `height`.
    pub fn inside(&self, (width, height): (u32, u32)) -> bool {
        self.width >= 1
            && self.height >= 1
            && self.x.checked_add(self.width).is_some_and(|r| r <= width)
            && self.y.checked_add(self.height).is_some_and(|b| b <= height)
    }

    /// Moves the region inside a stage of `width` x `height`, shrinking it first if it is
    /// larger; `true` if it changed.
    pub fn move_inside(&mut self, (width, height): (u32, u32)) -> bool {
        let before = self.clone();
        self.width = self.width.clamp(1, width.max(1));
        self.height = self.height.clamp(1, height.max(1));
        self.x = self.x.min(width.saturating_sub(self.width));
        self.y = self.y.min(height.saturating_sub(self.height));
        *self != before
    }

    /// The region scaled with its stage from `from` to `to` pixels (regions follow a stage's
    /// resize), its edges rounded to whole pixels, at least one pixel, inside the stage.
    pub fn scaled(&self, from: (u32, u32), to: (u32, u32)) -> Self {
        // Nearest whole pixel of v * t / f.
        let edge = |v: u32, f: u32, t: u32| -> u32 {
            let f = u64::from(f.max(1));
            let scaled = (u64::from(v) * u64::from(t) * 2 + f) / (2 * f);
            u32::try_from(scaled).unwrap_or(t)
        };
        let (left, right) = (
            edge(self.x, from.0, to.0),
            edge(self.x.saturating_add(self.width), from.0, to.0),
        );
        let (top, bottom) = (
            edge(self.y, from.1, to.1),
            edge(self.y.saturating_add(self.height), from.1, to.1),
        );
        let mut out = Self {
            id: self.id,
            route: self.route,
            x: left,
            y: top,
            width: right.saturating_sub(left),
            height: bottom.saturating_sub(top),
        };
        out.move_inside(to);
        out
    }
}

fn default_keep_black() -> bool {
    true
}

fn default_layer() -> u16 {
    DEFAULT_STAGE_LAYER
}

impl Stage {
    /// A stage sized as `width` x `height`, keeping black between cues.
    pub fn new(id: StageId, name: impl Into<String>, width: u32, height: u32) -> Self {
        Self {
            id,
            name: name.into(),
            width: clamp_side(width),
            height: clamp_side(height),
            keep_black: true,
            layer: DEFAULT_STAGE_LAYER,
            regions: Vec::new(),
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
    /// Pixel size, 1 to [`MAX_STAGE_PIXELS`] each. Its regions scale with it, unless `regions`
    /// is given: then they become exactly those (an undo putting back regions a resize rounded).
    Size {
        width: u32,
        height: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        regions: Option<Vec<Region>>,
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
                regions: Some(stage.regions.clone()),
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
            Self::Size {
                width,
                height,
                regions,
            } => {
                let from = (stage.width, stage.height);
                stage.width = *width;
                stage.height = *height;
                match regions {
                    Some(regions) => stage.regions.clone_from(regions),
                    None => {
                        for region in &mut stage.regions {
                            *region = region.scaled(from, (*width, *height));
                        }
                    }
                }
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
    if let Some(why) = regions_problem(stage) {
        return Some(why);
    }
    crate::validate::name_problem(&stage.name)
}

/// Why `stage`'s regions cannot be what they are: too many, one outside the stage, or one id
/// twice. Which routes they name is the workspace's question ([`region_routes_problem`]).
pub(crate) fn regions_problem(stage: &Stage) -> Option<&'static str> {
    if stage.regions.len() > MAX_REGIONS {
        return Some("a stage has at most 16 regions");
    }
    if stage
        .regions
        .iter()
        .any(|r| !r.inside((stage.width, stage.height)))
    {
        return Some("a region lies wholly inside its stage");
    }
    let mut ids: Vec<RegionId> = stage.regions.iter().map(|r| r.id).collect();
    ids.sort();
    if ids.windows(2).any(|w| w.first() == w.get(1)) {
        return Some("a region id is used twice");
    }
    None
}

/// Why `regions` cannot be in a workspace whose routes are `routes`: one names a route it lacks.
pub(crate) fn region_routes_problem<'a>(
    regions: impl IntoIterator<Item = &'a Region>,
    routes: &[OutputRoute],
) -> Option<&'static str> {
    regions
        .into_iter()
        .filter_map(|r| r.route)
        .any(|id| !routes.iter().any(|r| r.id == id))
        .then_some("a region names an output route the workspace does not have")
}

/// One editable property of an output route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RouteProperty {
    Name {
        value: String,
    },
    /// Pixel size, 1 to [`MAX_ROUTE_PIXELS`] each.
    Size {
        width: u32,
        height: u32,
    },
    /// 0, 90, 180 or 270 (clockwise).
    Rotation {
        value: u16,
    },
    Scaling {
        value: RouteScaling,
    },
    /// The colour its guides are drawn in; `None`: a default no other route has
    /// ([`next_guide_colour`]). Saved with the show but no undo step (`Command::is_view_aid`).
    GuideColour {
        #[serde(default)]
        value: Option<GuideColour>,
    },
}

impl RouteProperty {
    pub(crate) fn current(&self, route: &OutputRoute) -> Self {
        match self {
            Self::Name { .. } => Self::Name {
                value: route.name.clone(),
            },
            Self::Size { .. } => Self::Size {
                width: route.width,
                height: route.height,
            },
            Self::Rotation { .. } => Self::Rotation {
                value: route.rotation,
            },
            Self::Scaling { .. } => Self::Scaling {
                value: route.scaling,
            },
            Self::GuideColour { .. } => Self::GuideColour {
                value: route.guide_colour,
            },
        }
    }

    pub(crate) fn write(&self, route: &mut OutputRoute) {
        match self {
            Self::Name { value } => route.name.clone_from(value),
            Self::Size { width, height } => {
                route.width = *width;
                route.height = *height;
            }
            Self::Rotation { value } => route.rotation = *value,
            Self::Scaling { value } => route.scaling = *value,
            Self::GuideColour { value } => route.guide_colour = *value,
        }
    }
}

/// One editable property of a region.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RegionProperty {
    /// Where it is on its stage, in stage pixels from the top left; wholly inside the stage.
    Bounds {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    /// The route it is sent to; `None`: none.
    Route { value: Option<RouteId> },
}

impl RegionProperty {
    pub(crate) fn current(&self, region: &Region) -> Self {
        match self {
            Self::Bounds { .. } => Self::Bounds {
                x: region.x,
                y: region.y,
                width: region.width,
                height: region.height,
            },
            Self::Route { .. } => Self::Route {
                value: region.route,
            },
        }
    }

    pub(crate) fn write(&self, region: &mut Region) {
        match self {
            Self::Bounds {
                x,
                y,
                width,
                height,
            } => {
                region.x = *x;
                region.y = *y;
                region.width = *width;
                region.height = *height;
            }
            Self::Route { value } => region.route = *value,
        }
    }
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
    fn a_stage_without_keep_black_in_its_file_keeps_black_between_cues() {
        let s: Stage = serde_json::from_str(
            r#"{"id":"00000000-0000-0000-0000-000000000001","name":"S","width":1920,"height":1080}"#,
        )
        .unwrap();
        assert!(s.keep_black);
        let new = Stage::new(s.id, "S", 1920, 1080);
        assert!(new.keep_black, "a new stage keeps black too");
        assert_eq!(s.layer, DEFAULT_STAGE_LAYER);
    }
}
