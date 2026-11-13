// SPDX-License-Identifier: GPL-3.0-or-later

//! The stage host's state, the same on every platform: which stages the engine wants, which
//! displays exist, and where each stage is. A [`Platform`] does the drawing and the windows.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, Write};

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

/// One stage as a display draws it: its pixel size and what it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layer {
    pub stage: StageId,
    pub width: u32,
    pub height: u32,
    pub picture: Picture,
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
            presented: BTreeSet::new(),
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
            ToStage::Probe { stage, points } => out.push(self.probe_stage(stage, &points)),
            #[cfg(feature = "test-support")]
            ToStage::ProbeDisplay { display, points } => {
                out.push(self.probe_display(display, &points));
            }
            #[cfg(feature = "test-support")]
            ToStage::Hang => return Flow::Hang,
            #[cfg(feature = "test-support")]
            ToStage::SetDisplays { displays } => {
                if self.platform.set_virtual_displays(displays) {
                    self.displays_changed(out);
                }
            }
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
        let mut states: Vec<(StageId, StageState)> = Vec::new();
        let mut on: BTreeMap<String, Vec<(u32, Layer)>> = BTreeMap::new();
        for stage in &self.stages {
            let picture = self.picture(stage.id);
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
                Some(_) if !stage.keep_black && picture == Picture::Black => StageState::Hidden,
                Some(id) => {
                    on.entry(id.clone())
                        .or_default()
                        .push((stage.stack, layer_of(stage, picture)));
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
        let layer = self
            .stages
            .iter()
            .find(|s| s.id == id)
            .map(|s| layer_of(s, self.picture(id)));
        let read = match layer {
            Some(l) => self.read((l.width, l.height), &[l], points),
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
        let mut layers: Vec<(u32, Layer)> = self
            .stages
            .iter()
            .filter(|s| {
                s.display.as_deref() == Some(display.as_str())
                    && matches!(self.states.get(&s.id), Some(StageState::Shown { .. }))
            })
            .map(|s| (s.stack, layer_of(s, self.picture(s.id))))
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

fn layer_of(stage: &StageOutput, picture: Picture) -> Layer {
    Layer {
        stage: stage.id,
        width: stage.width,
        height: stage.height,
        picture,
    }
}

/// Reads lines from `input` until it ends, a `quit`, or (test support) a `hang`, handling each
/// with `handle` and writing what it returns to `output`. A line that is too long or invalid is
/// answered with a `log` warning and skipped. Returns when the engine is gone (end of input or
/// a failed write), so the stage host never outlives its engine.
pub fn serve(
    input: &mut impl BufRead,
    output: &mut impl Write,
    mut handle: impl FnMut(ToStage, &mut Vec<FromStage>) -> Flow,
) -> Flow {
    let mut line = Vec::new();
    let mut answers = Vec::new();
    loop {
        line.clear();
        let flow = match read_line(input, &mut line) {
            Ok(Line::End) | Err(_) => return Flow::Quit,
            Ok(Line::TooLong(bytes)) => {
                answers.push(FromStage::Log {
                    level: LinkLevel::Warn,
                    message: format!("ignored a line of {bytes} bytes from the engine"),
                });
                Flow::Continue
            }
            Ok(Line::Read) => match decode_to_stage(&line) {
                Ok(message) => handle(message, &mut answers),
                Err(e) => {
                    answers.push(FromStage::Log {
                        level: LinkLevel::Warn,
                        message: format!("ignored a message from the engine: {e}"),
                    });
                    Flow::Continue
                }
            },
        };
        if write_all(output, &mut answers).is_err() {
            return Flow::Quit;
        }
        if flow != Flow::Continue {
            return flow;
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
        let flow = serve(&mut input.as_slice(), &mut output, |m, out| {
            h.handle(m, out)
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
