// SPDX-License-Identifier: GPL-3.0-or-later

//! The control thread's side of the show log and the status message (DX-02, DX-03;
//! docs/logging.md): turns what the loop does into log records, and gathers the counters for
//! `status`. The decisions about files, rotation, cadence and crash reports are in `diag`.

use std::collections::HashMap;
use std::path::PathBuf;

use openlab_core::protocol::{
    DeviceState, DeviceStatus, LogEntry, LogKind, LogLevel, RuntimeChange, ServerMessage,
    TransportAction, WireCueState,
};
use openlab_core::{CueId, CueListId};

use super::{Control, ControlConfig};
use crate::diag::{Counters, StatusReporter};

/// Entries `log_tail` returns when the request names no limit: the whole ring.
const TAIL_ALL: usize = usize::MAX;

/// State of the observers, kept in [`Control`].
pub(super) struct Observe {
    /// The last state logged per cue (idle cues are forgotten), to tell a start from a resume.
    states: HashMap<CueId, WireCueState>,
    status: Option<StatusReporter>,
    /// `log_dropped` already reported to the log.
    seen_log_dropped: u64,
    /// The workspace path last told to the crash hook.
    path: Option<PathBuf>,
}

impl Observe {
    pub(super) fn new(config: &ControlConfig) -> Self {
        Self {
            states: HashMap::new(),
            status: config.status.map(StatusReporter::new),
            seen_log_dropped: 0,
            path: None,
        }
    }

    /// A new workspace: no cue is running in it.
    pub(super) fn reset(&mut self) {
        self.states.clear();
    }
}

impl Control {
    /// Seconds of audio rendered: the show's own time.
    fn show_time(&self) -> f64 {
        self.mixer.samples_rendered() as f64 / f64::from(self.rate.get())
    }

    /// Adds a record to the show log (ring and disk) without sending it to the clients.
    pub(super) fn record(
        &mut self,
        kind: LogKind,
        level: LogLevel,
        message: String,
        cue: Option<CueId>,
        source: Option<String>,
    ) {
        let entry = LogEntry {
            time_ms: self.config.clock.now_ms(),
            show_time: Some(self.show_time()),
            level,
            kind,
            message,
            cue,
            source,
        };
        self.config.show_log.record(entry);
    }

    /// `cue 5 "Storm"`, for log lines.
    fn cue_label(&self, id: CueId) -> String {
        match self.ws.find_cue(id) {
            Some(cue) => match &cue.number {
                Some(n) if !n.is_empty() => format!("cue {n} \"{}\"", cue.name),
                _ => format!("cue \"{}\"", cue.name),
            },
            None => format!("cue {id}"),
        }
    }

    /// A transport action is about to be carried out: GO with the cue it starts, anything else
    /// with the cue it names. Described from the action's wire form, so a new action is logged
    /// without a change here.
    pub(super) fn observe_transport(&mut self, action: &TransportAction) {
        let value = serde_json::to_value(action).unwrap_or_default();
        let name = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("transport")
            .to_owned();
        let named: Option<CueId> = value
            .get("cue")
            .and_then(|c| serde_json::from_value(c.clone()).ok());
        if name == "go" {
            let at = value
                .get("list")
                .and_then(|l| serde_json::from_value::<CueListId>(l.clone()).ok())
                .and_then(|list| self.engine.playhead(list));
            let message = match at {
                Some(cue) => format!("GO: {}", self.cue_label(cue)),
                None => "GO: nothing at the playhead".to_owned(),
            };
            self.record(LogKind::Go, LogLevel::Info, message, at, None);
        } else {
            let message = match named {
                Some(cue) => format!("{name}: {}", self.cue_label(cue)),
                None => name,
            };
            self.record(LogKind::Transport, LogLevel::Info, message, named, None);
        }
    }

    /// An input reached the control thread from a client or OSC peer (sent by the network
    /// layer just before the input itself, on the same channel, so the order holds).
    pub(super) fn observe_trigger(&mut self, source: String, detail: String) {
        self.record(LogKind::Trigger, LogLevel::Info, detail, None, Some(source));
    }

    /// Cue state changes and warnings the engine produced: starts, stops and failures.
    pub(super) fn observe_changes(&mut self, changes: &[RuntimeChange]) {
        use WireCueState::{Broken, Fading, Idle, Loaded, Paused, PostWaiting};
        use WireCueState::{PreWaiting, Running, Stopping};
        for change in changes {
            match change {
                RuntimeChange::CueState { cue, state, reason } => {
                    let before = self.observe.states.get(cue).copied();
                    if *state == Idle {
                        self.observe.states.remove(cue);
                    } else {
                        self.observe.states.insert(*cue, *state);
                    }
                    let label = self.cue_label(*cue);
                    match (*state, before) {
                        // Static problems break a cue without a reason; those are not run-time
                        // failures and arrive as `problems`.
                        (Broken, prior) if prior != Some(Broken) => {
                            if let Some(reason) = reason {
                                let why = serde_json::to_value(reason)
                                    .ok()
                                    .and_then(|v| v.as_str().map(str::to_owned))
                                    .unwrap_or_default();
                                self.record(
                                    LogKind::CueFail,
                                    LogLevel::Error,
                                    format!("{label} failed: {why}"),
                                    Some(*cue),
                                    None,
                                );
                            }
                        }
                        (PreWaiting | Running, None | Some(Idle | Loaded | Broken)) => self.record(
                            LogKind::CueStart,
                            LogLevel::Info,
                            format!("{label} started"),
                            Some(*cue),
                            None,
                        ),
                        (
                            Idle,
                            Some(PreWaiting | Running | Paused | PostWaiting | Fading | Stopping),
                        ) => self.record(
                            LogKind::CueStop,
                            LogLevel::Info,
                            format!("{label} stopped"),
                            Some(*cue),
                            None,
                        ),
                        _ => {}
                    }
                }
                RuntimeChange::Warning { warning } => {
                    let value = serde_json::to_value(warning).unwrap_or_default();
                    let cue: Option<CueId> = value
                        .get("cue")
                        .and_then(|c| serde_json::from_value(c.clone()).ok());
                    let text = value.to_string();
                    self.record(
                        LogKind::Engine,
                        LogLevel::Warn,
                        format!("warning {text}"),
                        cue,
                        None,
                    );
                }
                _ => {}
            }
        }
    }

    /// Records and broadcasts a message of `kind`: [`Control::log`] for events with their own
    /// kind (device, document).
    pub(super) fn log_kind(&mut self, kind: LogKind, level: LogLevel, message: String) {
        self.record(kind, level, message.clone(), None, None);
        self.out.send(ServerMessage::Log { level, message });
    }

    /// The entries `log_tail` answers with, and the records the disk writer lost.
    pub(super) fn log_tail(&self, max: Option<u32>) -> (Vec<LogEntry>, u64) {
        let max = max.map_or(TAIL_ALL, |m| usize::try_from(m).unwrap_or(TAIL_ALL));
        (
            self.config.show_log.tail(max),
            self.config.show_log.dropped(),
        )
    }

    /// Once per tick: the crash hook's workspace path, a note when log records were lost, and
    /// the `status` message when one is due.
    pub(super) fn observe_tick(&mut self) {
        if self.observe.path.as_deref() != self.config.show_path.as_deref() {
            self.observe.path = self.config.show_path.clone();
            self.config.show_log.set_workspace(
                self.config
                    .show_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
            );
        }
        let lost = self.config.show_log.dropped();
        if lost > self.observe.seen_log_dropped {
            self.observe.seen_log_dropped = lost;
            self.log(
                LogLevel::Warn,
                format!(
                    "the show log fell behind the disk: {lost} record(s) not written so far \
                     (still available to log_tail)"
                ),
            );
        }
        if self.observe.status.is_some() {
            let counters = self.status_counters();
            let now = self.config.clock.now_ms();
            if let Some(status) = self
                .observe
                .status
                .as_mut()
                .and_then(|s| s.poll(now, &counters))
            {
                let mut message = ServerMessage::Status {
                    status: Box::new(status),
                };
                message.sanitise();
                self.out.send_lossy(message);
            }
        }
    }

    fn status_counters(&self) -> Counters {
        let device = match self.backend.device() {
            Some(d) => {
                let info = d.info();
                DeviceStatus {
                    name: Some(info.name.clone()),
                    state: if d.unrecoverable().is_some() {
                        DeviceState::Failed
                    } else if d.is_down() {
                        DeviceState::Lost
                    } else {
                        DeviceState::Ok
                    },
                    sample_rate: self.rate.get(),
                    block_frames: info.block_frames,
                    channels: u32::from(info.channels),
                    latency_ms: info.monitor.device_latency_seconds().map(|s| s * 1000.0),
                }
            }
            None => DeviceStatus {
                name: None,
                state: DeviceState::Absent,
                sample_rate: self.rate.get(),
                block_frames: u32::try_from(self.block).ok(),
                channels: u32::try_from(self.cue_outputs).unwrap_or(u32::MAX),
                latency_ms: None,
            },
        };
        Counters {
            xruns: self.mixer.xruns(),
            underrun_frames: self.mixer.underrun_frames(),
            streams: u32::try_from(self.streamer.open_streams()).unwrap_or(u32::MAX),
            log_dropped: self.config.show_log.dropped(),
            device,
            cpu_seconds: (self.config.cpu_probe)(),
            // Needs a render-time measurement on the audio thread (PROGRESS M4.7a).
            engine_load: None,
        }
    }

    /// The show log, for tests and the host.
    pub fn show_log(&self) -> &crate::diag::ShowLog {
        &self.config.show_log
    }
}
