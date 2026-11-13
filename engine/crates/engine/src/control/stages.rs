// SPDX-License-Identifier: GPL-3.0-or-later

//! Video stages on the control thread (VID-01, VID-12): which display each open workspace's
//! stages are shown on, handed to the stage host supervisor (`crate::stage`), and what the stage
//! host says back. The stage host starts when a workspace with stages is open (or the displays
//! are asked for); the control thread never waits on it.

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
    /// The no-video warning went out.
    no_video_warned: bool,
    /// Test support: probes waiting for their answer, oldest first.
    #[cfg(feature = "test-support")]
    probes: Vec<(StageId, RequestId, ReplyTx)>,
}

impl Engine {
    /// Starts the stage host supervisor if the engine has one configured and it is not running.
    fn start_stage_host(&mut self) {
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
                });
            }
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
    }

    /// What the stage host supervisor reported.
    pub(super) fn stage_event(&mut self, event: StageEvent) {
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
            }
            StageEvent::Displays(displays) => {
                let names: Vec<String> = displays
                    .iter()
                    .map(|d| format!("{} ({}x{})", d.name, d.width, d.height))
                    .collect();
                self.eng.log(
                    LogLevel::Info,
                    format!(
                        "displays: {}",
                        if names.is_empty() {
                            "none".into()
                        } else {
                            names.join(", ")
                        }
                    ),
                );
                self.eng.stages.displays = displays;
            }
            StageEvent::Status { stage, state } => {
                if let Some(message) = self.stage_state_message(stage, &state) {
                    self.eng.log(message.0, message.1);
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
            StageEvent::Log { level, message } => {
                let level = match level {
                    LinkLevel::Info => LogLevel::Info,
                    LinkLevel::Warn => LogLevel::Warn,
                    LinkLevel::Error => LogLevel::Error,
                };
                self.eng.log(level, message);
            }
            #[cfg(feature = "test-support")]
            StageEvent::Probed {
                stage,
                frame,
                colours,
            } => {
                let probes = &mut self.eng.stages.probes;
                if let Some(i) = probes.iter().position(|(s, ..)| *s == stage) {
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
        let ws = d.model.ws();
        let stages = ws
            .stages
            .iter()
            .map(|s| StageMappingEntry {
                stage: s.id,
                mapping: self.eng.config.machine.stage_mapping(ws.id, s.id).cloned(),
                state: self.eng.stages.states.get(&s.id).cloned(),
            })
            .collect();
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
            (true, Some(host)) => match probe {
                Some(points) => {
                    host.send(ToStage::Probe { stage, points });
                    self.eng.stages.probes.push((stage, id, to.clone()));
                }
                None => {
                    host.send(ToStage::TestCard { stage, frame });
                    reply(to, ok(id, None));
                }
            },
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

    /// The displays the stage host last listed.
    pub fn displays(&self) -> &[Display] {
        &self.eng.stages.displays
    }

    /// The running stage host's process id, once it has answered `hello`.
    pub fn stage_host_pid(&self) -> Option<u32> {
        self.eng.stages.pid.filter(|_| self.eng.stages.up)
    }
}
