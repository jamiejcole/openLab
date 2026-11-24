// SPDX-License-Identifier: GPL-3.0-or-later

//! Video stages and output routes on the control thread (VID-01, VID-12): every open
//! workspace's stages with their regions, and its routes with the display each is shown on here,
//! handed to the stage host supervisor (`crate::stage`), and what the stage host says back. The stage host starts with the control loop, so its displays are known when
//! a new workspace is made; the control thread never waits on it.

use std::collections::{HashMap, HashSet};

use openlab_core::protocol::{
    ErrorBody, ErrorCode, LogLevel, OkBody, Outcome, RequestId, ResultData, RouteEntry,
    ServerMessage, StageEntry,
};
use openlab_core::stage_link::{
    Display, LinkLevel, RegionOutput, RouteGuide, RouteOutput, RouteState, StageOutput,
};
use openlab_core::{Command, CommandError, CueKind, CueState, RouteId, StageId};

use super::inbox::reply;
use super::{Control, DocCx, DocId, Engine, Inbox, ReplyTx, failed, ok};
use crate::stage::{StageEvent, StageHost, StageSet};

/// What a test probe reads: one stage's own picture, or what a display shows.
#[cfg(feature = "test-support")]
#[derive(Clone, Debug, PartialEq, Eq)]
enum ProbeTarget {
    Stage(StageId),
    Display(String),
    Monitor(StageId),
}

/// The least time between two `video_outputs` pushes to clients, so a stage host reporting
/// in a burst cannot flood them.
const PUSH_EVERY_MS: i64 = 100;

/// The least time between two words to the supervisor that the control thread runs: a quarter
/// of its ping period, so a running engine never misses a ping.
const ALIVE_EVERY_MS: i64 = 250;

/// Log lines from the stage host's reports put in the show log per second.
const MAX_REPORT_LINES_PER_SECOND: u32 = 20;

/// An open doc's stages, routes and routes showing guides, as the stage set was last made from
/// them.
type DocStages = (
    DocId,
    Vec<openlab_core::Stage>,
    Vec<openlab_core::OutputRoute>,
    std::collections::BTreeSet<RouteId>,
);

/// The stage host as the control thread knows it.
#[derive(Default)]
pub(super) struct Stages {
    host: Option<StageHost>,
    /// The stage host has answered `hello` and not stopped since.
    pub(super) up: bool,
    /// Its process id, from that `hello`.
    pid: Option<u32>,
    /// What its `hello` said; `None` before the first.
    pub(super) video: Option<bool>,
    displays: Vec<Display>,
    /// Where each route is, as the stage host last said.
    states: HashMap<RouteId, RouteState>,
    /// The stage set last handed to the supervisor.
    sent: Option<StageSet>,
    /// What that set was made from: each open doc's stages and routes, [`Stages::mappings`],
    /// [`Stages::arms`] and [`Stages::monitor_changes`] then. An idle tick finds them
    /// unchanged and does no more.
    synced: Option<(Vec<DocStages>, u64, u64, u64)>,
    /// Counts changes to this machine's route display choices.
    mappings: u64,
    /// Stages a picture has played on since their workspace opened (or the stage was added)
    /// and its last panic. Only these keep black: a stage covers its display after its first
    /// picture, never before (a new or opened workspace locks no operator out), and a panic
    /// takes the black down (QLab 5 manual p. 227: kept until the workspace panics) until a
    /// picture plays there again.
    armed: HashSet<StageId>,
    /// Counts changes to [`Stages::armed`].
    arms: u64,
    /// Stages whose monitor window the operator has open (`set_stage_monitor`): not saved,
    /// closed with their stage, their workspace or the app.
    monitors: HashSet<StageId>,
    /// Counts changes to [`Stages::monitors`].
    monitor_changes: u64,
    /// When the control thread last told the supervisor it runs (control clock, ms).
    alive_ms: Option<i64>,
    /// The app asked the stage on the display with the menu bar to stand back
    /// (`set_stand_back`): an alert, sheet or panel of its own is up.
    pub(super) stand_back: bool,
    /// The operator's app as last heard (`set_app`): connected or not, and its process id.
    /// `None` until an app first connects or goes, so an engine started without one (tests, the
    /// offline driver) shows its stages as before.
    app: Option<(bool, Option<u32>)>,
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

    /// A new workspace's video outputs: one route, "Output 1", sized as the main display and
    /// shown on it on this machine, and one stage, "Stage 1", of that size with one region
    /// covering it sent to Output 1. With no display listed (none on this machine, or the stage
    /// host has not listed them yet) the route is 1920 x 1080 on no display.
    pub(super) fn add_default_outputs(&mut self, ws: &mut openlab_core::Workspace) {
        let display = (self.stages.displays.iter())
            .find(|d| d.main)
            .or_else(|| self.stages.displays.first())
            .cloned();
        let (width, height) = display
            .as_ref()
            .map_or((1920, 1080), |d| (d.width, d.height));
        let route = openlab_core::OutputRoute::new(
            RouteId(super::docs::random_id()),
            "Output 1",
            width,
            height,
        );
        let mut stage = openlab_core::Stage::new(
            StageId(super::docs::random_id()),
            "Stage 1".to_string(),
            width,
            height,
        );
        stage.regions.push(openlab_core::Region::whole(
            openlab_core::RegionId(super::docs::random_id()),
            Some(route.id),
            stage.width,
            stage.height,
        ));
        if let Some(display) = display {
            let choice = openlab_core::DisplayChoice {
                display: display.id.clone(),
                name: display.name.clone(),
                width: display.width,
                height: display.height,
            };
            // A refusal cannot happen for a display the link accepted; the route then has no
            // display, which is shown as such.
            let _ = self
                .config
                .machine
                .set_route_display(ws.id, route.id, Some(choice));
            self.stages.mappings = self.stages.mappings.wrapping_add(1);
            self.save_machine();
        }
        ws.routes.push(route);
        ws.stages.push(stage);
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

    /// A panic of the workspace these stages belong to: each one's black goes down once its
    /// pictures have faded, until a picture plays on it again.
    pub(super) fn release_stages(&mut self, stages: impl IntoIterator<Item = StageId>) {
        let mut changed = false;
        for id in stages {
            changed |= self.stages.armed.remove(&id);
        }
        if changed {
            self.stages.arms = self.stages.arms.wrapping_add(1);
        }
    }

    /// A picture plays on `stage`: its black (if it keeps black) stays up between cues from now.
    pub(super) fn arm_stage(&mut self, stage: StageId) {
        if self.stages.armed.insert(stage) {
            self.stages.arms = self.stages.arms.wrapping_add(1);
        }
    }

    /// Whether `stage` keeps black now: it does, and a picture has played on it since its
    /// workspace opened and last panicked.
    fn keeps_black(&self, stage: &openlab_core::Stage) -> bool {
        stage.keep_black && self.stages.armed.contains(&stage.id)
    }

    /// `set_stand_back`: told to the stage host now, and again to each one that starts.
    pub(super) fn set_stand_back(&mut self, on: bool) {
        self.stages.stand_back = on;
        if let Some(host) = &self.stages.host {
            host.picture(openlab_core::stage_link::ToStage::StandBack { on });
        }
    }

    /// An app connected (`connected`, with its process id if it gave one) or the last one went:
    /// told to the stage host supervisor, which tells the stage host now and each one that starts
    /// (before its stages). While no app is connected every stage's window is down (the cues play
    /// on unseen), so no stage outlives the app.
    pub(super) fn set_app(&mut self, connected: bool, pid: Option<u32>) {
        let was = self.stages.app.replace((connected, pid));
        if was.map(|(c, _)| c) != Some(connected) {
            self.log(
                LogLevel::Info,
                if connected {
                    "the app is connected: the stages are shown".to_owned()
                } else {
                    "the app is gone: the stages are taken down until it is back; cues play on"
                        .to_owned()
                },
            );
        }
        self.start_stage_host();
        if let Some(host) = &self.stages.host {
            host.set_app(connected, pid);
        }
    }

    /// Whether the stages are down for want of an app (`set_app`).
    fn app_gone(&self) -> bool {
        self.stages.app.is_some_and(|(connected, _)| !connected)
    }

    /// Each tick: tells the supervisor the control thread runs, at most every
    /// [`ALIVE_EVERY_MS`], so its pings (and so the stage host's) stop when this thread hangs.
    pub(super) fn stages_alive(&mut self) {
        let now = self.config.clock.now_ms();
        if self
            .stages
            .alive_ms
            .is_some_and(|last| now >= last && now - last < ALIVE_EVERY_MS)
        {
            return;
        }
        if let Some(host) = &self.stages.host {
            host.alive();
            self.stages.alive_ms = Some(now);
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
            Some(sent) => s.up && sent.routes.iter().all(|o| s.states.contains_key(&o.id)),
        }
    }
}

impl Control {
    /// An open ahead answered (or the stage host stopped or started): one is asked at a time
    /// across every doc, so every doc's next priming pass may open its next.
    pub(super) fn reprime(&mut self) {
        for d in &mut self.docs {
            d.primes_dirty = true;
        }
    }

    /// Hands the stage host every open workspace's stages and routes, each route with the display
    /// this machine shows it on, when that set changed. Called every tick; cheap when nothing
    /// changed.
    pub(super) fn sync_stages(&mut self) {
        let unchanged =
            self.eng
                .stages
                .synced
                .as_ref()
                .is_some_and(|(docs, mappings, arms, monitors)| {
                    *mappings == self.eng.stages.mappings
                        && *arms == self.eng.stages.arms
                        && *monitors == self.eng.stages.monitor_changes
                        && docs.len() == self.docs.len()
                        && docs
                            .iter()
                            .zip(&self.docs)
                            .all(|((id, stages, routes, guides), d)| {
                                *id == d.id
                                    && *stages == d.model.ws().stages
                                    && *routes == d.model.ws().routes
                                    && *guides == d.guides
                            })
                });
        if unchanged {
            return;
        }
        // A route's name or guide colour is in `video_outputs` too, shown or not.
        self.eng.stages.push_due = true;
        self.eng.stages.synced = Some((
            self.docs
                .iter()
                .map(|d| {
                    let ws = d.model.ws();
                    (d.id, ws.stages.clone(), ws.routes.clone(), d.guides.clone())
                })
                .collect(),
            self.eng.stages.mappings,
            self.eng.stages.arms,
            self.eng.stages.monitor_changes,
        ));
        let mut set = StageSet::default();
        for doc in &self.docs {
            let ws = doc.model.ws();
            for (index, route) in ws.routes.iter().enumerate() {
                // A show copied in Finder and opened beside its original has the same route ids:
                // the first open keeps each such route, so the set is still sent, not refused.
                if set.routes.iter().any(|r| r.id == route.id) {
                    continue;
                }
                set.routes.push(RouteOutput {
                    id: route.id,
                    width: route.width,
                    height: route.height,
                    rotation: route.rotation,
                    scaling: route.scaling,
                    display: self
                        .eng
                        .config
                        .machine
                        .route_display(ws.id, route.id)
                        .map(|m| m.display.clone()),
                    guide: doc.guides.contains(&route.id).then(|| RouteGuide {
                        name: openlab_core::stage_link::link_text(&route.name, "Output"),
                        colour: route.guide_colour_at(index),
                    }),
                });
            }
            for stage in &ws.stages {
                set.stages.push(StageOutput {
                    id: stage.id,
                    width: stage.width,
                    height: stage.height,
                    // Only regions sent to a route of this workspace are drawn.
                    regions: stage
                        .regions
                        .iter()
                        .filter_map(|r| {
                            let route =
                                r.route.filter(|id| ws.routes.iter().any(|x| x.id == *id))?;
                            Some(RegionOutput {
                                route,
                                x: r.x,
                                y: r.y,
                                width: r.width,
                                height: r.height,
                            })
                        })
                        .collect(),
                    keep_black: self.eng.keeps_black(stage),
                    stack: 0,
                    monitor: self
                        .eng
                        .stages
                        .monitors
                        .contains(&stage.id)
                        .then(|| openlab_core::stage_link::link_text(&stage.name, "Stage")),
                });
            }
        }
        // A closed workspace's or a removed stage's played picture is forgotten, so the set
        // stays within the stages open, and the workspace opened again starts with none.
        let outputs = &set.stages;
        self.eng
            .stages
            .armed
            .retain(|id| outputs.iter().any(|o| o.id == *id));
        // Likewise a monitor: it goes with its stage.
        self.eng
            .stages
            .monitors
            .retain(|id| outputs.iter().any(|o| o.id == *id));
        // Ranked across every open workspace: two workspaces' stages may share a display.
        let rank: HashMap<StageId, u32> =
            openlab_core::stack_order(self.docs.iter().flat_map(|d| d.model.ws().stages.iter()))
                .into_iter()
                .zip(0u32..)
                .collect();
        for output in &mut set.stages {
            output.stack = rank.get(&output.id).copied().unwrap_or(0);
        }
        if self.eng.stages.sent.as_ref() == Some(&set)
            || (self.eng.stages.sent.is_none() && set.stages.is_empty() && set.routes.is_empty())
        {
            return;
        }
        self.eng.start_stage_host();
        if let Some(host) = &self.eng.stages.host {
            host.set_stages(set.clone());
        }
        // A route whose output, or the regions drawn on it, are new or changed waits for the
        // stage host's word on it again (a monitor opened or closed changes no route).
        let sent = self.eng.stages.sent.take().unwrap_or_default();
        let drawn = |set: &StageSet, id: RouteId| {
            let route = set.routes.iter().find(|o| o.id == id)?.clone();
            let regions: Vec<(StageId, bool, u32, RegionOutput)> = (set.stages.iter())
                .flat_map(|s| {
                    s.regions
                        .iter()
                        .filter(|r| r.route == id)
                        .map(move |r| (s.id, s.keep_black, s.stack, r.clone()))
                })
                .collect();
            Some((route, regions))
        };
        self.eng
            .stages
            .states
            .retain(|id, _| drawn(&set, *id) == drawn(&sent, *id));
        self.eng.stages.sent = Some(set);
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

    /// `doc`'s `video_outputs`: this machine's displays, where each of its routes is, and its
    /// stages' black and monitors.
    fn video_outputs(&self, doc: &super::Doc) -> ServerMessage {
        ServerMessage::VideoOutputs {
            displays: self.eng.stages.displays.clone(),
            video: self.eng.stages.video,
            routes: self.route_entries(doc),
            stages: self.stage_entries(doc),
        }
    }

    /// Each of `doc`'s routes: its display on this machine and where the stage host says it is.
    fn route_entries(&self, doc: &super::Doc) -> Vec<RouteEntry> {
        let ws = doc.model.ws();
        ws.routes
            .iter()
            .enumerate()
            .map(|(index, r)| RouteEntry {
                route: r.id,
                mapping: self.eng.config.machine.route_display(ws.id, r.id).cloned(),
                state: self.eng.stages.states.get(&r.id).cloned(),
                guides: doc.guides.contains(&r.id),
                guide_colour: Some(r.guide_colour_at(index)),
            })
            .collect()
    }

    /// Each of `doc`'s stages: whether its black is down and its monitor open.
    fn stage_entries(&self, doc: &super::Doc) -> Vec<StageEntry> {
        doc.model
            .ws()
            .stages
            .iter()
            .map(|s| StageEntry {
                stage: s.id,
                // Whether or not it keeps black now: a stage whose keep-black is ticked again
                // after a panic took its black down keeps it down until its next picture, and the
                // app must not put its own black up meanwhile.
                panicked: !self.eng.stages.armed.contains(&s.id),
                monitor: self.eng.stages.monitors.contains(&s.id),
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
                // A new stage host (after a crash too) shows the pictures that are up, and is
                // sent the pictures opened ahead again, and stands back if the app asks.
                self.eng.resend_pictures();
                if self.eng.stages.stand_back {
                    self.eng.set_stand_back(true);
                }
                self.eng.primes_host_started();
                self.reprime();
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
            StageEvent::Status { route, state } => {
                // Only routes the stage host was given are kept, so the map stays bounded by
                // the routes that exist.
                let known = self
                    .eng
                    .stages
                    .sent
                    .as_ref()
                    .is_some_and(|s| s.routes.iter().any(|o| o.id == route));
                if !known {
                    return;
                }
                if let Some((level, message)) = self.route_state_message(route, &state) {
                    self.eng.report_line(level, message);
                }
                self.eng.stages.states.insert(route, state);
            }
            StageEvent::Stopped {
                reason,
                restarting,
                crashed,
            } => {
                let s = &mut self.eng.stages;
                s.up = false;
                s.pid = None;
                s.states.clear();
                self.eng.pictures_host_stopped(crashed);
                self.reprime();
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
            StageEvent::MonitorClosed { stage } => {
                if self.eng.stages.monitors.remove(&stage) {
                    self.eng.stages.monitor_changes =
                        self.eng.stages.monitor_changes.wrapping_add(1);
                    let name = self.stage_name(stage);
                    self.eng.log(
                        LogLevel::Info,
                        format!("the monitor of stage '{name}' was closed"),
                    );
                    self.sync_stages();
                }
            }
            StageEvent::PictureOpened { picture } => {
                if self.eng.picture_opened(picture) {
                    self.reprime();
                }
            }
            StageEvent::PictureFailed { picture, reason } => {
                if self.eng.prime_failed(picture) {
                    self.reprime();
                } else {
                    self.eng.picture_failed(picture, reason);
                }
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
                    crate::stage::Probed::Monitor(stage) => ProbeTarget::Monitor(stage),
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

    /// The log line for a route reaching `state`: a warning when it cannot be seen.
    fn route_state_message(
        &self,
        route: RouteId,
        state: &RouteState,
    ) -> Option<(LogLevel, String)> {
        if self.eng.stages.states.get(&route) == Some(state) {
            return None;
        }
        let name = self.route_name(route);
        let display_name = |id: &str| {
            self.eng
                .stages
                .displays
                .iter()
                .find(|d| d.id == id)
                .map_or_else(|| id.to_owned(), |d| d.name.clone())
        };
        Some(match state {
            RouteState::Shown { display } => (
                LogLevel::Info,
                format!(
                    "output route '{name}' is shown on {}",
                    display_name(display)
                ),
            ),
            RouteState::Hidden if self.eng.app_gone() => (
                LogLevel::Info,
                format!("output route '{name}' is down while the app is gone"),
            ),
            RouteState::Hidden => (
                LogLevel::Info,
                format!("output route '{name}' shows nothing while nothing plays on it"),
            ),
            RouteState::NoDisplay => (
                LogLevel::Warn,
                format!("output route '{name}' has no display on this computer"),
            ),
            RouteState::DisplayMissing { display } => {
                let named = self
                    .docs
                    .iter()
                    .filter_map(|d| {
                        self.eng
                            .config
                            .machine
                            .route_display(d.model.ws().id, route)
                    })
                    .find(|m| &m.display == display)
                    .map_or_else(|| display.clone(), |m| m.name.clone());
                (
                    LogLevel::Warn,
                    format!("output route '{name}': its display {named} is not connected"),
                )
            }
            RouteState::Failed { reason } => (
                LogLevel::Warn,
                format!("output route '{name}' cannot be shown: {reason}"),
            ),
        })
    }

    /// The name of `route` in whichever open workspace has it (its id if none does).
    fn route_name(&self, route: RouteId) -> String {
        self.docs
            .iter()
            .flat_map(|d| d.model.ws().routes.iter())
            .find(|r| r.id == route)
            .map_or_else(|| route.to_string(), |r| r.name.clone())
    }

    /// The name of `stage` in whichever open workspace has it (its id if none does).
    fn stage_name(&self, stage: StageId) -> String {
        self.docs
            .iter()
            .flat_map(|d| d.model.ws().stages.iter())
            .find(|s| s.id == stage)
            .map_or_else(|| stage.to_string(), |s| s.name.clone())
    }

    /// `set_stage_monitor`, for doc `doc`: opens (or brings to the front) or closes the stage's
    /// monitor window, a live copy of the stage the stage host draws in a window of its own.
    pub(super) fn set_stage_monitor(
        &mut self,
        doc: DocId,
        id: RequestId,
        stage: StageId,
        open: bool,
        to: &ReplyTx,
    ) {
        let Some(d) = self.docs.iter().find(|d| d.id == doc) else {
            return reply(
                to,
                failed(
                    id,
                    ErrorBody::new(ErrorCode::NoWorkspace, "no such workspace"),
                ),
            );
        };
        if !d.model.ws().stages.iter().any(|s| s.id == stage) {
            return reply(
                to,
                failed(id, ErrorBody::new(ErrorCode::NotFound, "no such stage")),
            );
        }
        let changed = if open {
            self.eng.stages.monitors.insert(stage)
        } else {
            self.eng.stages.monitors.remove(&stage)
        };
        if changed {
            self.eng.stages.monitor_changes = self.eng.stages.monitor_changes.wrapping_add(1);
            // The stage host gets the new set at once, not at the next tick.
            self.sync_stages();
        } else if open && let Some(host) = &self.eng.stages.host {
            host.raise_monitor(stage);
        }
        reply(to, ok(id, None));
    }

    /// `set_route_guides`, for doc `doc`: shows or hides the guides of `routes`, all or none (a
    /// route the doc lacks refuses the whole ask). Not saved and no undo step; Show Mode allows
    /// it.
    pub(super) fn set_route_guides(
        &mut self,
        doc: DocId,
        id: RequestId,
        routes: &[RouteId],
        show: bool,
        to: &ReplyTx,
    ) {
        let Some(d) = self.docs.iter_mut().find(|d| d.id == doc) else {
            return reply(
                to,
                failed(
                    id,
                    ErrorBody::new(ErrorCode::NoWorkspace, "no such workspace"),
                ),
            );
        };
        if routes.len() > openlab_core::MAX_ROUTES {
            return reply(
                to,
                failed(
                    id,
                    ErrorBody::new(
                        ErrorCode::BadRequest,
                        "more output routes than a workspace has",
                    ),
                ),
            );
        }
        let ws = d.model.ws();
        if !routes.iter().all(|r| ws.routes.iter().any(|x| x.id == *r)) {
            return reply(
                to,
                failed(
                    id,
                    ErrorBody::new(ErrorCode::NotFound, "no such output route"),
                ),
            );
        }
        let before = d.guides.len();
        let mut changed = false;
        for route in routes {
            changed |= if show {
                d.guides.insert(*route)
            } else {
                d.guides.remove(route)
            };
        }
        if changed {
            let (now, word) = (d.guides.len(), if show { "shown" } else { "hidden" });
            self.eng.log(
                LogLevel::Info,
                format!(
                    "alignment guides {word} on {} output route(s)",
                    now.abs_diff(before)
                ),
            );
            // The stage host gets the new set at once, not at the next tick.
            self.sync_stages();
        }
        reply(to, ok(id, None));
    }

    /// Every monitor window closes (the app is gone); the stages' output is untouched.
    pub(super) fn close_monitors(&mut self) {
        if self.eng.stages.monitors.is_empty() {
            return;
        }
        self.eng.stages.monitors.clear();
        self.eng.stages.monitor_changes = self.eng.stages.monitor_changes.wrapping_add(1);
        self.eng.log(
            LogLevel::Info,
            "the app is gone: its monitor windows are closed; the stages play on".to_owned(),
        );
        self.sync_stages();
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

    /// `list_route_displays`, for doc `doc`.
    pub(super) fn list_route_displays(&mut self, doc: DocId, id: RequestId, to: &ReplyTx) {
        let Some(d) = self.docs.iter().find(|d| d.id == doc) else {
            return reply(
                to,
                failed(
                    id,
                    ErrorBody::new(ErrorCode::NoWorkspace, "no such workspace"),
                ),
            );
        };
        let (routes, stages) = (self.route_entries(d), self.stage_entries(d));
        reply(
            to,
            ServerMessage::Result {
                id,
                outcome: Outcome::Ok(OkBody {
                    revision: None,
                    data: Some(ResultData::RouteDisplays { routes, stages }),
                }),
            },
        );
    }

    /// `set_route_display`, for doc `doc`: the display must be one the stage host lists now.
    /// Not an undo step: the choice belongs to this machine.
    pub(super) fn set_route_display(
        &mut self,
        doc: DocId,
        id: RequestId,
        route: RouteId,
        display: Option<String>,
        to: &ReplyTx,
    ) {
        let outcome = self.apply_route_display(doc, route, display);
        reply(
            to,
            match outcome {
                Ok(()) => ok(id, None),
                Err(error) => failed(id, error),
            },
        );
    }

    fn apply_route_display(
        &mut self,
        doc: DocId,
        route: RouteId,
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
            .routes
            .iter()
            .find(|r| r.id == route)
            .map(|r| r.name.clone())
            .ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, "no such output route"))?;
        let choice = match display {
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
                Some(openlab_core::DisplayChoice {
                    display: found.id.clone(),
                    name: found.name.clone(),
                    width: found.width,
                    height: found.height,
                })
            }
        };
        self.eng.require_saveable()?;
        let message = match &choice {
            Some(m) => format!("output route '{name}' shown on {} on this machine", m.name),
            None => format!("output route '{name}' has no display on this machine"),
        };
        self.eng
            .config
            .machine
            .set_route_display(workspace, route, choice)
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
                    // A picture shown on the stage, as a cue's is: its black stays up after.
                    self.eng.arm_stage(stage);
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

    /// Test support: `monitor_probe`: reads what `stage`'s monitor window shows at a content size
    /// of `size`.
    #[cfg(feature = "test-support")]
    pub(super) fn monitor_probe(
        &mut self,
        doc: DocId,
        id: RequestId,
        stage: StageId,
        (width, height): (u32, u32),
        points: Vec<[f32; 2]>,
        to: &ReplyTx,
    ) {
        use openlab_core::stage_link::ToStage;
        let known = self
            .docs
            .iter()
            .find(|d| d.id == doc)
            .is_some_and(|d| d.model.ws().stages.iter().any(|s| s.id == stage));
        if !known {
            return reply(
                to,
                failed(id, ErrorBody::new(ErrorCode::NotFound, "no such stage")),
            );
        }
        if self.eng.stages.host.is_none() || !self.eng.stages.up {
            return reply(
                to,
                failed(
                    id,
                    ErrorBody::new(ErrorCode::Unsupported, "video output is not running"),
                ),
            );
        }
        self.eng.picture_clock_now();
        if let Some(host) = self.eng.stages_host() {
            host.send(ToStage::ProbeMonitor {
                stage,
                width,
                height,
                points,
            });
        }
        self.eng
            .stages
            .probes
            .push((ProbeTarget::Monitor(stage), id, to.clone()));
    }

    /// Test support: the operator closes `stage`'s monitor window on the stage host (its close
    /// button).
    #[cfg(feature = "test-support")]
    pub fn close_monitor_window(&self, stage: StageId) {
        if let Some(host) = &self.eng.stages.host {
            host.send(openlab_core::stage_link::ToStage::CloseMonitorWindow { stage });
        }
    }

    /// Test support: makes the running stage host stop answering, as a hung one would. The flag
    /// is set once the hang is queued for it: a ping the supervisor sends after that goes
    /// unanswered, one it sent before may still be answered.
    #[cfg(feature = "test-support")]
    pub fn hang_stage_host(&mut self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        match &self.eng.stages.host {
            Some(host) => host.send_queued(openlab_core::stage_link::ToStage::Hang),
            None => std::sync::Arc::default(),
        }
    }

    /// Test support: moves the stage host's media clock on by `ms` (one started with
    /// `--media-clock-by-hand`).
    #[cfg(feature = "test-support")]
    pub fn advance_stage_media_clock(&self, ms: u64) {
        if let Some(host) = &self.eng.stages.host {
            host.send(openlab_core::stage_link::ToStage::AdvanceMediaClock { ms });
        }
    }

    /// Test support: the stage host supervisor looks at its clock again (a test moved it by
    /// hand).
    #[cfg(feature = "test-support")]
    pub fn wake_stage_supervisor(&self) {
        if let Some(host) = &self.eng.stages.host {
            host.wake();
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

    /// Where the stage host last said `route` is.
    pub fn route_state(&self, route: RouteId) -> Option<&RouteState> {
        self.eng.stages.states.get(&route)
    }

    /// Whether `stage`'s monitor window is open, as the engine keeps it.
    pub fn monitor_open(&self, stage: StageId) -> bool {
        self.eng.stages.monitors.contains(&stage)
    }

    /// How many routes the engine holds a state for (bounded by the routes it sent).
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

impl DocCx<'_> {
    /// Why `command` (a step of it, for a transaction, an undo or a redo) may not run now: it
    /// removes a stage that a cue is using (loaded, waiting, playing, paused or stopping on
    /// it). Its picture would otherwise vanish from the audience mid-cue. `None`: it may run.
    pub(super) fn stage_removal_refused(&self, command: &Command) -> Option<CommandError> {
        match command {
            Command::RemoveStage { id } => self.stage_user(*id),
            Command::Transaction { commands } => {
                commands.iter().find_map(|c| self.stage_removal_refused(c))
            }
            _ => None,
        }
    }

    /// The refusal naming the first cue using `stage`, if one does: an active Video cue aimed
    /// at it, or a cue whose picture is up on it (a cue aimed elsewhere since its load keeps its
    /// picture on the stage it loaded on).
    fn stage_user(&self, stage: StageId) -> Option<CommandError> {
        let ws = self.doc.model.ws();
        let engine = self.doc.model.engine();
        let drawn = self.eng.cue_with_picture_on(self.doc.id, stage);
        let cue = ws.cues().find(|cue| {
            Some(cue.id) == drawn
                || matches!(&cue.kind, CueKind::Video { stage: Some(s), .. } if *s == stage)
                    && matches!(
                        engine.state(cue.id),
                        CueState::Loaded
                            | CueState::PreWaiting
                            | CueState::Running
                            | CueState::Paused
                            | CueState::Fading
                            | CueState::Stopping
                    )
        })?;
        let name = ws
            .stages
            .iter()
            .find(|s| s.id == stage)
            .map_or_else(String::new, |s| s.name.clone());
        let label = match &cue.number {
            Some(n) if !n.is_empty() => n.clone(),
            _ => openlab_core::display_name(cue).into_owned(),
        };
        Some(CommandError::StageInUse {
            stage: name,
            cue: label,
        })
    }
}
