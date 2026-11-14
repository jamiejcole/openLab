// SPDX-License-Identifier: GPL-3.0-or-later

//! Video stages on the control thread (VID-01, VID-12): which display each open workspace's
//! stages are shown on, handed to the stage host supervisor (`crate::stage`), and what the stage
//! host says back. The stage host starts with the control loop, so its displays are known when
//! a new workspace is made; the control thread never waits on it.

use std::collections::HashMap;

use openlab_core::StageId;
use openlab_core::protocol::{
    ErrorBody, ErrorCode, LogLevel, OkBody, Outcome, RequestId, ResultData, ServerMessage,
    StageMappingEntry,
};
use openlab_core::stage_link::{Display, LinkLevel, StageOutput, StageState};

use super::inbox::reply;
use super::{Control, DocId, Engine, Inbox, ReplyTx, failed, ok};
use crate::stage::{StageEvent, StageHost};

/// What a test probe reads: one stage's own picture, or what a display shows.
#[cfg(feature = "test-support")]
#[derive(Clone, Debug, PartialEq, Eq)]
enum ProbeTarget {
    Stage(StageId),
    Display(String),
}

/// The least time between two `video_outputs` pushes to clients, so a stage host reporting
/// in a burst cannot flood them.
const PUSH_EVERY_MS: i64 = 100;

/// Log lines from the stage host's reports put in the show log per second.
const MAX_REPORT_LINES_PER_SECOND: u32 = 20;

/// An open doc's stages, as the stage set was last made from them.
type DocStages = (DocId, Vec<openlab_core::Stage>);

/// The stage host as the control thread knows it.
#[derive(Default)]
pub(super) struct Stages {
    host: Option<StageHost>,
    /// The stage host has answered `hello` and not stopped since.
    up: bool,
    /// Its process id, from that `hello`.
    pid: Option<u32>,
    /// What its `hello` said; `None` before the first.
    video: Option<bool>,
    displays: Vec<Display>,
    states: HashMap<StageId, StageState>,
    /// The stage set last handed to the supervisor.
    sent: Option<Vec<StageOutput>>,
    /// What that set was made from: each open doc's stages, and [`Stages::mappings`] then. An
    /// idle tick finds them unchanged and does no more.
    synced: Option<(Vec<DocStages>, u64)>,
    /// Counts changes to this machine's stage display choices.
    mappings: u64,
    /// Something a `video_outputs` push carries changed since the last one went out.
    push_due: bool,
    /// When the last `video_outputs` went out (control clock, ms).
    last_push_ms: Option<i64>,
    /// The no-video warning went out.
    no_video_warned: bool,
    /// Log lines from the stage host's reports in the current second: (second, lines,
    /// dropped). At most [`MAX_REPORT_LINES_PER_SECOND`] a second reach the show log.
    report_lines: (i64, u32, u32),
    /// Test support: probes waiting for their answer, oldest first.
    #[cfg(feature = "test-support")]
    probes: Vec<(ProbeTarget, RequestId, ReplyTx)>,
}

impl Engine {
    /// Starts the stage host supervisor if the engine has one configured and it is not running.
    pub(super) fn start_stage_host(&mut self) {
        if self.stages.host.is_some() {
            return;
        }
        let Some(config) = self.config.stage_host.clone() else {
            return;
        };
        let inbox = self.inbox.clone();
        let events: crate::stage::EventSink = std::sync::Arc::new(move |e| {
            let _ = inbox.send(Inbox::Stage(e));
        });
        match StageHost::start(config, events) {
            Ok(host) => self.stages.host = Some(host),
            Err(e) => self.log(
                LogLevel::Error,
                format!("could not start the video output supervisor ({e}); stages are not shown"),
            ),
        }
    }

    /// A new workspace's stages (QLab 5 manual p. 226): one per connected display, named
    /// "Stage 1", "Stage 2"... in the stage host's display order, sized as the display and
    /// shown on it on this machine. None while the stage host has listed no display yet.
    pub(super) fn add_display_stages(&mut self, ws: &mut openlab_core::Workspace) {
        let displays = self.stages.displays.clone();
        for (n, display) in displays.iter().enumerate().take(openlab_core::MAX_STAGES) {
            let stage = openlab_core::Stage::new(
                StageId(super::docs::random_id()),
                format!("Stage {}", n + 1),
                display.width,
                display.height,
            );
            let mapping = openlab_core::StageMapping {
                display: display.id.clone(),
                name: display.name.clone(),
                width: display.width,
                height: display.height,
            };
            // A refusal cannot happen for a display the link accepted; the stage then has no
            // display, which is shown as such.
            let _ = self
                .config
                .machine
                .set_stage_mapping(ws.id, stage.id, Some(mapping));
            self.stages.mappings = self.stages.mappings.wrapping_add(1);
            ws.stages.push(stage);
        }
        if !displays.is_empty() {
            self.save_machine();
        }
    }

    /// A log line from the stage host's reports (states, displays, its own log): at most
    /// [`MAX_REPORT_LINES_PER_SECOND`] a second, the rest counted and said once the second
    /// is over, so a stage host cannot flood the log or the clients.
    fn report_line(&mut self, level: LogLevel, message: String) {
        self.flush_report_lines();
        let (_, lines, dropped) = self.stages.report_lines;
        if lines >= MAX_REPORT_LINES_PER_SECOND {
            self.stages.report_lines.2 = dropped.saturating_add(1);
            return;
        }
        self.stages.report_lines.1 += 1;
        self.log(level, message);
    }

    /// Starts a new second of report budget once the clock has left the last one, saying how
    /// many lines that second dropped. Called every tick too, so a burst followed by quiet
    /// still says what it dropped.
    pub(super) fn flush_report_lines(&mut self) {
        let second = self.config.clock.now_ms().div_euclid(1000);
        let (current, _, dropped) = self.stages.report_lines;
        if second == current {
            return;
        }
        self.stages.report_lines = (second, 0, 0);
        if dropped > 0 {
            self.log(
                LogLevel::Warn,
                format!("{dropped} more video output reports were not logged"),
            );
        }
    }

    /// The stage host supervisor, if video output runs.
    pub(super) fn stages_host(&self) -> Option<&StageHost> {
        self.stages.host.as_ref()
    }

    /// The stage host has been told about every stage and has said where each one is.
    pub(super) fn stages_settled(&self) -> bool {
        let s = &self.stages;
        #[cfg(feature = "test-support")]
        if !s.probes.is_empty() {
            return false;
        }
        match &s.sent {
            None => true,
            Some(sent) => s.up && sent.iter().all(|o| s.states.contains_key(&o.id)),
        }
    }
}

impl Control {
    /// Hands the stage host every open workspace's stages, each with the display this machine
    /// shows it on, when that set changed. Called every tick; cheap when nothing changed.
    pub(super) fn sync_stages(&mut self) {
        let unchanged = self
            .eng
            .stages
            .synced
            .as_ref()
            .is_some_and(|(docs, mappings)| {
                *mappings == self.eng.stages.mappings
                    && docs.len() == self.docs.len()
                    && docs
                        .iter()
                        .zip(&self.docs)
                        .all(|((id, stages), d)| *id == d.id && *stages == d.model.ws().stages)
            });
        if unchanged {
            return;
        }
        self.eng.stages.synced = Some((
            self.docs
                .iter()
                .map(|d| (d.id, d.model.ws().stages.clone()))
                .collect(),
            self.eng.stages.mappings,
        ));
        let mut outputs = Vec::new();
        for doc in &self.docs {
            let ws = doc.model.ws();
            for stage in &ws.stages {
                outputs.push(StageOutput {
                    id: stage.id,
                    width: stage.width,
                    height: stage.height,
                    display: self
                        .eng
                        .config
                        .machine
                        .stage_mapping(ws.id, stage.id)
                        .map(|m| m.display.clone()),
                    keep_black: stage.keep_black,
                    stack: 0,
                });
            }
        }
        // Ranked across every open workspace: two workspaces' stages may share a display.
        let rank: HashMap<StageId, u32> =
            openlab_core::stack_order(self.docs.iter().flat_map(|d| d.model.ws().stages.iter()))
                .into_iter()
                .zip(0u32..)
                .collect();
        for output in &mut outputs {
            output.stack = rank.get(&output.id).copied().unwrap_or(0);
        }
        if self.eng.stages.sent.as_ref() == Some(&outputs)
            || (self.eng.stages.sent.is_none() && outputs.is_empty())
        {
            return;
        }
        self.eng.start_stage_host();
        if let Some(host) = &self.eng.stages.host {
            host.set_stages(outputs.clone());
        }
        // A stage whose output is new or changed waits for the stage host's word on it again.
        let sent = self.eng.stages.sent.take().unwrap_or_default();
        self.eng.stages.states.retain(|id, _| {
            outputs.iter().find(|o| o.id == *id) == sent.iter().find(|o| o.id == *id)
        });
        self.eng.stages.sent = Some(outputs);
        self.eng.stages.push_due = true;
    }

    /// Sends each open workspace's connections a `video_outputs` when something it carries
    /// changed, at most once per [`PUSH_EVERY_MS`]. Called every tick.
    pub(super) fn push_video_outputs(&mut self) {
        if !self.eng.stages.push_due {
            return;
        }
        let now = self.eng.config.clock.now_ms();
        if self
            .eng
            .stages
            .last_push_ms
            .is_some_and(|last| now >= last && now - last < PUSH_EVERY_MS)
        {
            return;
        }
        let mut all_sent = true;
        for doc in &self.docs {
            let message = self.video_outputs(doc);
            all_sent &= self.eng.out.send_state(Some(doc.id), message);
        }
        if all_sent {
            self.eng.stages.push_due = false;
            self.eng.stages.last_push_ms = Some(now);
        }
    }

    /// Sends `doc`'s connections its `video_outputs` now, whatever the gap since the last push:
    /// after a snapshot of it (a connection binding), whose client must have it. The pushes'
    /// spacing and what is due for other docs are left as they are. If the outbound channel is
    /// full, the push is made at the next tick instead.
    pub(super) fn push_doc_video_outputs(&mut self, doc: DocId) {
        let Some(d) = self.docs.iter().find(|d| d.id == doc) else {
            return;
        };
        let message = self.video_outputs(d);
        if !self.eng.out.send_state(Some(doc), message) {
            self.eng.stages.push_due = true;
            self.eng.stages.last_push_ms = None;
        }
    }

    /// `doc`'s `video_outputs`: this machine's displays and where each of its stages is.
    fn video_outputs(&self, doc: &super::Doc) -> ServerMessage {
        ServerMessage::VideoOutputs {
            displays: self.eng.stages.displays.clone(),
            video: self.eng.stages.video,
            stages: self.stage_entries(doc),
        }
    }

    /// Each of `doc`'s stages: its display on this machine and where the stage host says it is.
    fn stage_entries(&self, doc: &super::Doc) -> Vec<StageMappingEntry> {
        let ws = doc.model.ws();
        ws.stages
            .iter()
            .map(|s| StageMappingEntry {
                stage: s.id,
                mapping: self.eng.config.machine.stage_mapping(ws.id, s.id).cloned(),
                state: self.eng.stages.states.get(&s.id).cloned(),
            })
            .collect()
    }

    /// What the stage host supervisor reported.
    pub(super) fn stage_event(&mut self, event: StageEvent) {
        let reported = !matches!(event, StageEvent::Log { .. });
        self.eng.stages.push_due |= reported;
        match event {
            StageEvent::Started { pid, video } => {
                let s = &mut self.eng.stages;
                s.up = true;
                s.pid = Some(pid);
                s.video = Some(video);
                s.states.clear();
                self.eng.log(
                    LogLevel::Info,
                    format!("video output started (stage host {pid})"),
                );
                if !video && !std::mem::replace(&mut self.eng.stages.no_video_warned, true) {
                    self.eng.log(
                        LogLevel::Warn,
                        "this computer's openLab has no video output: stages are not shown"
                            .to_owned(),
                    );
                }
                // A new stage host (after a crash too) shows the pictures that are up.
                self.eng.resend_pictures();
            }
            StageEvent::Displays(displays) => {
                if displays == self.eng.stages.displays {
                    return;
                }
                let names: Vec<String> = displays
                    .iter()
                    .map(|d| format!("{} ({}x{})", d.name, d.width, d.height))
                    .collect();
                let text = if names.is_empty() {
                    "none".to_owned()
                } else {
                    names.join(", ")
                };
                self.eng.stages.displays = displays;
                self.eng
                    .report_line(LogLevel::Info, format!("displays: {text}"));
            }
            StageEvent::Status { stage, state } => {
                // Only stages the stage host was given are kept, so the map stays bounded by
                // the stages that exist.
                let known = self
                    .eng
                    .stages
                    .sent
                    .as_ref()
                    .is_some_and(|s| s.iter().any(|o| o.id == stage));
                if !known {
                    return;
                }
                if let Some((level, message)) = self.stage_state_message(stage, &state) {
                    self.eng.report_line(level, message);
                }
                self.eng.stages.states.insert(stage, state);
            }
            StageEvent::Stopped { reason, restarting } => {
                let s = &mut self.eng.stages;
                s.up = false;
                s.pid = None;
                s.states.clear();
                if restarting {
                    self.eng.log(
                        LogLevel::Warn,
                        format!("video output stopped: {reason}; restarting it"),
                    );
                } else {
                    self.eng.log(
                        LogLevel::Error,
                        format!("video output stopped: {reason}; stages are not shown"),
                    );
                }
                #[cfg(feature = "test-support")]
                for (_, id, to) in std::mem::take(&mut self.eng.stages.probes) {
                    reply(
                        &to,
                        failed(
                            id,
                            ErrorBody::new(ErrorCode::Internal, "video output stopped"),
                        ),
                    );
                }
            }
            StageEvent::PictureFailed { picture, reason } => {
                self.eng.picture_failed(picture, reason);
            }
            StageEvent::Log { level, message } => {
                let level = match level {
                    LinkLevel::Info => LogLevel::Info,
                    LinkLevel::Warn => LogLevel::Warn,
                    LinkLevel::Error => LogLevel::Error,
                };
                self.eng.report_line(level, message);
            }
            #[cfg(feature = "test-support")]
            StageEvent::Probed {
                target,
                frame,
                colours,
            } => {
                let target = match target {
                    crate::stage::Probed::Stage(stage) => ProbeTarget::Stage(stage),
                    crate::stage::Probed::Display(display) => ProbeTarget::Display(display),
                };
                let probes = &mut self.eng.stages.probes;
                if let Some(i) = probes.iter().position(|(s, ..)| *s == target) {
                    let (_, id, to) = probes.remove(i);
                    reply(
                        &to,
                        ServerMessage::Result {
                            id,
                            outcome: Outcome::Ok(OkBody {
                                revision: None,
                                data: Some(ResultData::StageProbe { frame, colours }),
                            }),
                        },
                    );
                }
            }
        }
    }

    /// The log line for a stage reaching `state`: a warning when it cannot be seen.
    fn stage_state_message(
        &self,
        stage: StageId,
        state: &StageState,
    ) -> Option<(LogLevel, String)> {
        if self.eng.stages.states.get(&stage) == Some(state) {
            return None;
        }
        let name = self
            .docs
            .iter()
            .flat_map(|d| d.model.ws().stages.iter())
            .find(|s| s.id == stage)
            .map_or_else(|| stage.to_string(), |s| s.name.clone());
        let display_name = |id: &str| {
            self.eng
                .stages
                .displays
                .iter()
                .find(|d| d.id == id)
                .map_or_else(|| id.to_owned(), |d| d.name.clone())
        };
        Some(match state {
            StageState::Shown { display } => (
                LogLevel::Info,
                format!("stage '{name}' is shown on {}", display_name(display)),
            ),
            StageState::Hidden => (
                LogLevel::Info,
                format!("stage '{name}' is hidden while nothing plays on it"),
            ),
            StageState::NoDisplay => (
                LogLevel::Warn,
                format!("stage '{name}' has no display on this computer"),
            ),
            StageState::DisplayMissing { display } => {
                let named = self
                    .docs
                    .iter()
                    .filter_map(|d| {
                        self.eng
                            .config
                            .machine
                            .stage_mapping(d.model.ws().id, stage)
                    })
                    .find(|m| &m.display == display)
                    .map_or_else(|| display.clone(), |m| m.name.clone());
                (
                    LogLevel::Warn,
                    format!("stage '{name}': its display {named} is not connected"),
                )
            }
            StageState::Failed { reason } => (
                LogLevel::Warn,
                format!("stage '{name}' cannot be shown: {reason}"),
            ),
        })
    }

    /// `list_displays` (app-scope): the displays as the stage host last listed them. Starts the
    /// stage host if it is not running, so a later ask finds them.
    pub(super) fn list_displays(&mut self, id: RequestId, to: &ReplyTx) {
        self.eng.start_stage_host();
        reply(
            to,
            ServerMessage::Result {
                id,
                outcome: Outcome::Ok(OkBody {
                    revision: None,
                    data: Some(ResultData::Displays {
                        displays: self.eng.stages.displays.clone(),
                        video: self.eng.stages.video,
                    }),
                }),
            },
        );
    }

    /// `list_stage_mappings`, for doc `doc`.
    pub(super) fn list_stage_mappings(&mut self, doc: DocId, id: RequestId, to: &ReplyTx) {
        let Some(d) = self.docs.iter().find(|d| d.id == doc) else {
            return reply(
                to,
                failed(
                    id,
                    ErrorBody::new(ErrorCode::NoWorkspace, "no such workspace"),
                ),
            );
        };
        let stages = self.stage_entries(d);
        reply(
            to,
            ServerMessage::Result {
                id,
                outcome: Outcome::Ok(OkBody {
                    revision: None,
                    data: Some(ResultData::StageMappings { stages }),
                }),
            },
        );
    }

    /// `set_stage_mapping`, for doc `doc`: the display must be one the stage host lists now.
    pub(super) fn set_stage_mapping(
        &mut self,
        doc: DocId,
        id: RequestId,
        stage: StageId,
        display: Option<String>,
        to: &ReplyTx,
    ) {
        let outcome = self.apply_stage_mapping(doc, stage, display);
        reply(
            to,
            match outcome {
                Ok(()) => ok(id, None),
                Err(error) => failed(id, error),
            },
        );
    }

    fn apply_stage_mapping(
        &mut self,
        doc: DocId,
        stage: StageId,
        display: Option<String>,
    ) -> Result<(), ErrorBody> {
        let d = self
            .docs
            .iter()
            .find(|d| d.id == doc)
            .ok_or_else(|| ErrorBody::new(ErrorCode::NoWorkspace, "no such workspace"))?;
        let ws = d.model.ws();
        let workspace = ws.id;
        let name = ws
            .stages
            .iter()
            .find(|s| s.id == stage)
            .map(|s| s.name.clone())
            .ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, "no such stage"))?;
        let mapping = match display {
            None => None,
            Some(display) => {
                let found = self
                    .eng
                    .stages
                    .displays
                    .iter()
                    .find(|d| d.id == display)
                    .ok_or_else(|| {
                        ErrorBody::new(ErrorCode::NotFound, format!("no display {display}"))
                    })?;
                Some(openlab_core::StageMapping {
                    display: found.id.clone(),
                    name: found.name.clone(),
                    width: found.width,
                    height: found.height,
                })
            }
        };
        self.eng.require_saveable()?;
        let message = match &mapping {
            Some(m) => format!("stage '{name}' shown on {} on this machine", m.name),
            None => format!("stage '{name}' has no display on this machine"),
        };
        self.eng
            .config
            .machine
            .set_stage_mapping(workspace, stage, mapping)
            .map_err(|why| ErrorBody::new(ErrorCode::BadRequest, why))?;
        self.eng.stages.mappings = self.eng.stages.mappings.wrapping_add(1);
        self.eng.save_machine();
        self.eng.log(LogLevel::Info, message);
        // The stage host gets the new set at once, not at the next tick.
        self.sync_stages();
        Ok(())
    }

    /// Test support: `stage_test_card` and `stage_probe` for doc `doc`'s `stage`.
    #[cfg(feature = "test-support")]
    pub(super) fn stage_test_request(
        &mut self,
        doc: DocId,
        id: RequestId,
        stage: StageId,
        probe: Option<Vec<[f32; 2]>>,
        frame: u16,
        to: &ReplyTx,
    ) {
        use openlab_core::stage_link::ToStage;
        let known = self
            .docs
            .iter()
            .find(|d| d.id == doc)
            .is_some_and(|d| d.model.ws().stages.iter().any(|s| s.id == stage));
        let host = self.eng.stages.host.as_ref().filter(|_| self.eng.stages.up);
        match (known, host) {
            (false, _) => reply(
                to,
                failed(id, ErrorBody::new(ErrorCode::NotFound, "no such stage")),
            ),
            (true, None) => reply(
                to,
                failed(
                    id,
                    ErrorBody::new(ErrorCode::Unsupported, "video output is not running"),
                ),
            ),
            (true, Some(_)) => match probe {
                Some(points) => {
                    // Drawn at the engine's clock now.
                    self.eng.picture_clock_now();
                    if let Some(host) = self.eng.stages_host() {
                        host.send(ToStage::Probe { stage, points });
                    }
                    self.eng
                        .stages
                        .probes
                        .push((ProbeTarget::Stage(stage), id, to.clone()));
                }
                None => {
                    if let Some(host) = self.eng.stages_host() {
                        host.send(ToStage::TestCard { stage, frame });
                    }
                    reply(to, ok(id, None));
                }
            },
        }
    }

    /// Test support: `display_probe`: reads what `display` shows (every stage on it, stacked).
    #[cfg(feature = "test-support")]
    pub(super) fn display_probe(
        &mut self,
        id: RequestId,
        display: String,
        points: Vec<[f32; 2]>,
        to: &ReplyTx,
    ) {
        use openlab_core::stage_link::ToStage;
        match self.eng.stages.host.as_ref().filter(|_| self.eng.stages.up) {
            None => reply(
                to,
                failed(
                    id,
                    ErrorBody::new(ErrorCode::Unsupported, "video output is not running"),
                ),
            ),
            Some(_) => {
                self.eng.picture_clock_now();
                if let Some(host) = self.eng.stages_host() {
                    host.send(ToStage::ProbeDisplay {
                        display: display.clone(),
                        points,
                    });
                }
                self.eng
                    .stages
                    .probes
                    .push((ProbeTarget::Display(display), id, to.clone()));
            }
        }
    }

    /// Test support: makes the running stage host stop answering, as a hung one would.
    #[cfg(feature = "test-support")]
    pub fn hang_stage_host(&mut self) {
        if let Some(host) = &self.eng.stages.host {
            host.send(openlab_core::stage_link::ToStage::Hang);
        }
    }

    /// Test support: a stage host on virtual displays now has `displays`, as if displays were
    /// plugged in or unplugged.
    #[cfg(feature = "test-support")]
    pub fn set_virtual_displays(&mut self, displays: Vec<Display>) {
        if let Some(host) = &self.eng.stages.host {
            host.send(openlab_core::stage_link::ToStage::SetDisplays { displays });
        }
    }

    /// Whether the stage host has been told every stage and said where each one is, with no
    /// probe waiting (tests and the offline driver).
    pub fn stages_settled(&self) -> bool {
        self.eng.stages_settled()
    }

    /// Where the stage host last said `stage` is.
    pub fn stage_state(&self, stage: StageId) -> Option<&StageState> {
        self.eng.stages.states.get(&stage)
    }

    /// How many stages the engine holds a state for (bounded by the stages it sent).
    pub fn stage_states_known(&self) -> usize {
        self.eng.stages.states.len()
    }

    /// The displays the stage host last listed.
    pub fn displays(&self) -> &[Display] {
        &self.eng.stages.displays
    }

    /// The running stage host's process id, once it has answered `hello`.
    pub fn stage_host_pid(&self) -> Option<u32> {
        self.eng.stages.pid.filter(|_| self.eng.stages.up)
    }
}
