// SPDX-License-Identifier: GPL-3.0-or-later

//! The stage host's state, the same on every platform: which stages the engine wants, which
//! displays exist, and where each stage is. A [`Platform`] does the drawing and the windows.

use std::collections::HashMap;
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

/// The windows and the drawing of one platform.
pub trait Platform {
    /// Whether this build shows video at all.
    fn video(&self) -> bool;
    /// The displays connected now.
    fn displays(&mut self) -> Vec<Display>;
    /// Shows `stage` on `display` drawing `picture`, opening or moving its window as needed.
    fn show(
        &mut self,
        stage: &StageOutput,
        display: &Display,
        picture: Picture,
    ) -> Result<(), String>;
    /// Takes `stage`'s window down, if it has one.
    fn hide(&mut self, stage: StageId);
    /// The colour (RGBA) at each point (fractions of the stage, from the top left) of `stage`
    /// drawing `picture`, read back from the GPU.
    #[cfg(feature = "test-support")]
    fn probe(
        &mut self,
        stage: &StageOutput,
        picture: Picture,
        points: &[[f32; 2]],
    ) -> Result<Vec<[u8; 4]>, String>;
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
}

impl<P: Platform> Host<P> {
    pub fn new(platform: P) -> Self {
        Self {
            platform,
            stages: Vec::new(),
            displays: Vec::new(),
            states: HashMap::new(),
            pictures: HashMap::new(),
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
                for old in &self.stages {
                    if !stages.iter().any(|s| s.id == old.id) {
                        self.platform.hide(old.id);
                        self.states.remove(&old.id);
                        self.pictures.remove(&old.id);
                    }
                }
                self.stages = stages;
                self.apply(out);
            }
            ToStage::Ping { seq } => out.push(FromStage::Pong { seq }),
            ToStage::Quit => {
                for stage in &self.stages {
                    self.platform.hide(stage.id);
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
            ToStage::Probe { stage, points } => out.push(self.probe(stage, &points)),
            #[cfg(feature = "test-support")]
            ToStage::Hang => return Flow::Hang,
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

    /// Shows, hides or redraws each stage and reports each state that changed.
    fn apply(&mut self, out: &mut Vec<FromStage>) {
        for stage in &self.stages {
            let picture = self
                .pictures
                .get(&stage.id)
                .copied()
                .unwrap_or(Picture::Black);
            let state = match &stage.display {
                _ if !self.platform.video() => StageState::Failed {
                    reason: "this computer's openLab has no video output".into(),
                },
                None => StageState::NoDisplay,
                Some(id) => match self.displays.iter().find(|d| &d.id == id) {
                    None => StageState::DisplayMissing {
                        display: id.clone(),
                    },
                    Some(_) if !stage.keep_black && picture == Picture::Black => StageState::Hidden,
                    Some(display) => match self.platform.show(stage, display, picture) {
                        Ok(()) => StageState::Shown {
                            display: id.clone(),
                        },
                        Err(reason) => StageState::Failed { reason },
                    },
                },
            };
            if !matches!(state, StageState::Shown { .. }) {
                self.platform.hide(stage.id);
            }
            if self.states.get(&stage.id) != Some(&state) {
                self.states.insert(stage.id, state.clone());
                out.push(FromStage::StageStatus {
                    stage: stage.id,
                    state,
                });
            }
        }
    }

    #[cfg(feature = "test-support")]
    fn probe(&mut self, id: StageId, points: &[[f32; 2]]) -> FromStage {
        let Some(stage) = self.stages.iter().find(|s| s.id == id) else {
            return FromStage::Probed {
                stage: id,
                frame: None,
                colours: Vec::new(),
            };
        };
        let picture = self.pictures.get(&id).copied().unwrap_or(Picture::Black);
        let mut all: Vec<[f32; 2]> = crate::card::barcode_points().to_vec();
        all.extend_from_slice(points);
        match self.platform.probe(stage, picture, &all) {
            Ok(colours) => {
                let (code, asked) = colours.split_at(colours.len().min(crate::card::CELLS));
                FromStage::Probed {
                    stage: id,
                    frame: crate::card::read_frame(code),
                    colours: asked.to_vec(),
                }
            }
            Err(reason) => FromStage::Log {
                level: LinkLevel::Error,
                message: format!("probe failed: {reason}"),
            },
        }
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
        shown: HashMap<StageId, String>,
        fail: bool,
    }

    impl Platform for Fake {
        fn video(&self) -> bool {
            true
        }
        fn displays(&mut self) -> Vec<Display> {
            self.displays.clone()
        }
        fn show(&mut self, s: &StageOutput, d: &Display, _: Picture) -> Result<(), String> {
            if self.fail {
                return Err("no GPU".into());
            }
            self.shown.insert(s.id, d.id.clone());
            Ok(())
        }
        fn hide(&mut self, stage: StageId) {
            self.shown.remove(&stage);
        }
        #[cfg(feature = "test-support")]
        fn probe(
            &mut self,
            _: &StageOutput,
            _: Picture,
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
        assert_eq!(h.platform.shown.len(), 1);
        // The same set again changes nothing and reports nothing.
        out.clear();
        h.handle(ToStage::Stages { stages }, &mut out);
        assert!(out.is_empty());
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
