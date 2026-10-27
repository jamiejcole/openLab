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
use crate::diag::{Counters, Peek, StatusReporter};

/// Entries `log_tail` returns when the request names no limit: the whole ring.
const TAIL_ALL: usize = usize::MAX;

/// The "fell behind the disk" notice is sent at most this often (wall clock): while the writer
/// is stuck every record is refused, the notice included, so it must not answer itself.
const DROP_NOTE_GAP_MS: i64 = 10_000;
/// Trigger records per source per window; the rest are counted and summarised (a chatty
/// controller must not flush the GO history out of the 1000-entry ring).
const TRIGGER_WINDOW_MS: i64 = 10_000;
const TRIGGERS_LOGGED_PER_WINDOW: u32 = 5;
/// Live `log_entry` pushes per window (M4.7c), on the same 10 s wall-clock window as the
/// trigger records: a burst (a GO that starts a hundred cues) goes out whole, a flood is cut
/// to 20 a second on average. A record over the budget is not pushed (the client sees the gap
/// in `seq` and reads `log_tail`); the newest one held back is pushed when the window ends.
const LOG_PUSH_WINDOW_MS: i64 = TRIGGER_WINDOW_MS;
const LOG_PUSHES_PER_WINDOW: u32 = 200;
/// Sources tracked at once; more share one entry.
const MAX_TRIGGER_SOURCES: usize = 256;
const OTHER_SOURCES: &str = "other sources";

/// One source's trigger records in the current window.
struct TriggerWindow {
    start_ms: i64,
    logged: u32,
    suppressed: u64,
}

/// State of the observers, kept in [`Control`].
pub(super) struct Observe {
    /// The last state logged per cue (idle cues are forgotten), to tell a start from a resume.
    states: HashMap<CueId, WireCueState>,
    status: Option<StatusReporter>,
    /// `log_dropped` already reported to the log.
    seen_log_dropped: u64,
    /// When the "fell behind" notice was last logged.
    last_drop_note_ms: Option<i64>,
    triggers: HashMap<String, TriggerWindow>,
    /// The workspace path last told to the crash hook.
    path: Option<PathBuf>,
    /// The `log_entry` budget: (window start, pushes in it).
    push_window: Option<(i64, u32)>,
    /// The newest `log_entry` not delivered (over the budget, or the channel was too full);
    /// offered again each tick until a newer one goes out.
    unsent: Option<ServerMessage>,
}

impl Observe {
    pub(super) fn new(config: &ControlConfig) -> Self {
        Self {
            states: HashMap::new(),
            status: config.status.map(StatusReporter::new),
            seen_log_dropped: 0,
            last_drop_note_ms: None,
            triggers: HashMap::new(),
            path: None,
            push_window: None,
            unsent: None,
        }
    }

    /// A new workspace: no cue is running in it.
    pub(super) fn reset(&mut self) {
        self.states.clear();
    }
}

/// True when `then` is at least `gap` ms before `now`; a clock that went back counts as long
/// enough (it must not silence a notice).
fn elapsed(now: i64, then: i64, gap: i64) -> bool {
    let d = now.saturating_sub(then);
    d < 0 || d >= gap
}

impl Control {
    /// Seconds of audio rendered: the show's own time.
    fn show_time(&self) -> f64 {
        self.mixer.samples_rendered() as f64 / f64::from(self.rate.get())
    }

    /// Adds a record to the show log (ring and disk) and, when the host asked for it, offers it
    /// to the clients as a `log_entry` (lossy, rate-limited).
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
        if !self.config.push_log {
            self.config.show_log.record(entry);
            return;
        }
        let (seq, entry) = self.config.show_log.record_copy(entry);
        let now = self.config.clock.now_ms();
        self.push(now, ServerMessage::LogEntry { seq, entry });
    }

    /// Offers a `log_entry` within the window's budget. Only a delivered push spends from it:
    /// a client that is not keeping up must not use up the budget of the records after it.
    /// What is not delivered is kept as the newest unsent (an older one is superseded).
    fn push(&mut self, now: i64, message: ServerMessage) {
        let (start, pushed) = match self.observe.push_window {
            Some((start, n)) if !elapsed(now, start, LOG_PUSH_WINDOW_MS) => (start, n),
            _ => (now, 0),
        };
        self.observe.push_window = Some((start, pushed));
        if pushed >= LOG_PUSHES_PER_WINDOW {
            self.observe.unsent = Some(message);
            return;
        }
        match self.out.try_lossy(message) {
            Ok(()) => {
                self.observe.push_window = Some((start, pushed + 1));
                self.observe.unsent = None;
            }
            Err(message) => self.observe.unsent = Some(message),
        }
    }

    /// Once a tick: the newest record not yet delivered, once the budget and the channel allow,
    /// so a client learns of a gap without waiting for the next record.
    fn flush_log_push(&mut self, now: i64) {
        if let Some(message) = self.observe.unsent.take() {
            self.push(now, message);
        }
    }

    /// `cue 5 "Storm"`, for log lines.
    pub(super) fn cue_label(&self, id: CueId) -> String {
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
        let now = self.config.clock.now_ms();
        let key = if self.observe.triggers.contains_key(&source)
            || self.observe.triggers.len() < MAX_TRIGGER_SOURCES
        {
            source
        } else {
            OTHER_SOURCES.to_owned()
        };
        let window = self
            .observe
            .triggers
            .entry(key.clone())
            .or_insert(TriggerWindow {
                start_ms: now,
                logged: 0,
                suppressed: 0,
            });
        let mut summary = 0;
        if elapsed(now, window.start_ms, TRIGGER_WINDOW_MS) {
            summary = std::mem::take(&mut window.suppressed);
            window.start_ms = now;
            window.logged = 0;
        }
        let log_it = window.logged < TRIGGERS_LOGGED_PER_WINDOW;
        if log_it {
            window.logged += 1;
        } else {
            window.suppressed += 1;
        }
        self.note_suppressed(&key, summary);
        if log_it {
            self.record(LogKind::Trigger, LogLevel::Info, detail, None, Some(key));
        }
    }

    fn note_suppressed(&mut self, source: &str, suppressed: u64) {
        if suppressed > 0 {
            self.record(
                LogKind::Trigger,
                LogLevel::Info,
                format!("{suppressed} more input(s) not logged (too many too fast)"),
                None,
                Some(source.to_owned()),
            );
        }
    }

    /// Summarises and forgets the sources whose window is over.
    fn flush_trigger_windows(&mut self, now: i64) {
        let over: Vec<String> = self
            .observe
            .triggers
            .iter()
            .filter(|(_, w)| elapsed(now, w.start_ms, TRIGGER_WINDOW_MS))
            .map(|(k, _)| k.clone())
            .collect();
        for key in over {
            if let Some(w) = self.observe.triggers.remove(&key) {
                self.note_suppressed(&key, w.suppressed);
            }
        }
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

    /// The entries `log_tail` answers with, the records the disk writer lost, and the `seq`
    /// of the newest record.
    pub(super) fn log_tail(&self, max: Option<u32>) -> (Vec<LogEntry>, u64, u64) {
        let max = max.map_or(TAIL_ALL, |m| usize::try_from(m).unwrap_or(TAIL_ALL));
        let (entries, seq) = self.config.show_log.tail_seq(max);
        (entries, self.config.show_log.dropped(), seq)
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
        let now = self.config.clock.now_ms();
        self.flush_trigger_windows(now);
        self.flush_log_push(now);
        let lost = self.config.show_log.dropped();
        if lost > self.observe.seen_log_dropped
            && self
                .observe
                .last_drop_note_ms
                .is_none_or(|at| elapsed(now, at, DROP_NOTE_GAP_MS))
        {
            let more = lost - self.observe.seen_log_dropped;
            self.observe.last_drop_note_ms = Some(now);
            self.log(
                LogLevel::Warn,
                format!(
                    "the show log fell behind the disk: {more} more record(s) not written \
                     ({lost} so far; still available to log_tail)"
                ),
            );
            // The notice is a record too, and a full queue refuses it: count it as seen, or
            // it would be reported again on the next tick, and so on.
            self.observe.seen_log_dropped = self.config.show_log.dropped();
        }
        // Taken out so the counters can borrow `self` while the reporter decides.
        if let Some(mut reporter) = self.observe.status.take() {
            // Counters are gathered only when a status is due: the CPU probe is a syscall or a
            // file read, and the device name a copy.
            let status = reporter.poll(now, &self.status_peek(), || self.status_counters());
            self.observe.status = Some(reporter);
            if let Some(status) = status {
                let mut message = ServerMessage::Status {
                    status: Box::new(status),
                };
                message.sanitise();
                self.out.send_lossy(message);
            }
        }
    }

    /// What decides whether a status is due: atomics and borrowed fields, no copies.
    fn status_peek(&self) -> Peek<'_> {
        let (device_name, device_state) = match self.backend.device() {
            Some(d) => (Some(d.info().name.as_str()), wire_device_state(d)),
            None => (None, DeviceState::Absent),
        };
        Peek {
            xruns: self.mixer.xruns(),
            underrun_frames: self.mixer.underrun_frames(),
            device_name,
            device_state,
            network_output_disabled: self.net.disabled(),
        }
    }

    fn status_counters(&self) -> Counters {
        let device = match self.backend.device() {
            Some(d) => {
                let info = d.info();
                DeviceStatus {
                    name: Some(info.name.clone()),
                    state: wire_device_state(d),
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
            log_seq: self.config.show_log.seq(),
            network_output_disabled: self.net.disabled(),
            device,
            cpu_seconds: (self.config.cpu_probe)(),
            // Worst callback since the previous status (read-and-reset; this is its only
            // reader). Offline there is no audio callback to time.
            engine_load: self
                .backend
                .device()
                .and_then(|d| d.info().monitor.take_engine_load())
                .filter(|l| l.is_finite()),
        }
    }

    /// The show log, for tests and the host.
    pub fn show_log(&self) -> &crate::diag::ShowLog {
        &self.config.show_log
    }
}

fn wire_device_state(d: &super::devices::Device) -> DeviceState {
    if d.unrecoverable().is_some() {
        DeviceState::Failed
    } else if d.is_down() {
        DeviceState::Lost
    } else {
        DeviceState::Ok
    }
}
