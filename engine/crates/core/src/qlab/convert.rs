// SPDX-License-Identifier: GPL-3.0-or-later

//! QLab 5 workspace to openLab workspace. Field names are QLab's as stored; levels in the file
//! are linear gains (1.0 is 0 dB) and become dB here.

use std::collections::HashMap;

use uuid::Uuid;

use super::archive::{Archive, Fields};
use super::plist::{Budget, MAX_READS, Value};
use super::{
    MAX_QLAB_CUES, Placeholder, QLAB_MAJOR, QlabAudioPatch, QlabError, QlabFile, QlabImport,
    QlabNote, QlabNoteKind, QlabPlaceholder, QlabReport, QlabStage, QlabUnsupported,
    QlabUnsupportedReason, QlabVideoOutput,
};
use crate::cue::{
    ContinueMode, Crosspoint, Cue, CueColor, CueKind, CueLevels, CueOutput, DevampMode, DuckOthers,
    FadeAndStopOthers, FadeCrosspoint, FadeCurve, FadeLevels, FadeMode, FadeSpec, GroupMode,
    Hotkey, MAX_CHANNELS, MAX_CUE_OUTPUTS, MAX_DB, MAX_DEVICE_OUTPUTS, MAX_TIME, MediaRef,
    Modifiers, OscArg, OthersScope, PlayCount, SILENCE_DB, SecondTrigger, SliceMarker, Triggers,
    WallClockTrigger, Weekday, Weekdays,
};
use crate::model::{
    CueId, CueList, CueListId, DeviceOutputLevel, NetworkPatch, NetworkPatchId, NetworkProtocol,
    OutputPatch, PatchCrosspoint, PatchId, PatchOutput, RegionId, RouteId, StageId,
    TransportHotkeys, Workspace, WorkspaceId,
};
use crate::osc_access::OscAccess;
use crate::validate::{MAX_CUE_NAME_BYTES, MAX_CUE_NOTES_BYTES, MAX_GROUP_DEPTH, MAX_NAME_SCALARS};
use crate::video::{OutputRoute, Region, RouteScaling, Stage, next_guide_colour};
use crate::video_geometry::{Crop, FadeGeometry, FillStyle, Pair, VideoGeometry};
use crate::video_sound::VideoSound;
use crate::video_timing::VideoTiming;
use crate::{MAX_CUE_LAYER, OUTPUT_MAX_DB_RANGE, OUTPUT_MIN_DB_RANGE};

/// Rows of a level matrix that are inputs (file channels): QLab's 24, within openLab's
/// limit on file channels.
const MAX_INPUT_ROWS: u32 = 24;

pub(super) fn convert(
    bytes: &[u8],
    ids: &mut dyn FnMut() -> Uuid,
    media: &mut dyn FnMut(&QlabFile) -> Option<MediaRef>,
) -> Result<QlabImport, QlabError> {
    let budget = Budget::new(MAX_READS);
    let mut opened = 0;
    let top = Archive::open(bytes, &budget, &mut opened)?;
    let root: HashMap<String, Value<'_>> = top.dict(Some(top.root()?))?.into_iter().collect();
    check_version(&top, &root)?;
    let lists_data = top
        .data(root.get("cueLists").copied())?
        .ok_or(QlabError::NotQlab)?;
    let lists = Archive::open(lists_data, &budget, &mut opened)?;
    let name = top
        .string(root.get("workspaceName").copied())?
        .unwrap_or_default();
    let name = name.strip_suffix(".qlab5").unwrap_or(&name).to_owned();
    let mut c = Converter {
        ar: &lists,
        ids,
        media,
        report: QlabReport {
            workspace_name: name.clone(),
            qlab_version: top
                .string(root.get("QLabShortVersionString").copied())?
                .unwrap_or_default(),
            ..QlabReport::default()
        },
        placeholders: Vec::new(),
        cue_ids: HashMap::new(),
        cue_order: Vec::new(),
        numbers: HashMap::new(),
        pending: Vec::new(),
        patches: Vec::new(),
        net_patches: Vec::new(),
        stages: Vec::new(),
        simple_stage: None,
        count: 0,
    };
    let mut ws = Workspace::new_empty(
        WorkspaceId(c.id()),
        CueListId(c.id()),
        PatchId(c.id()),
        NetworkPatchId(c.id()),
    );
    ws.name = cut_name(&name);
    let seed = c.id().as_u128() as u32;
    ws.settings.osc_access = OscAccess::new_workspace(seed);
    c.note_setting(
        "OSC access passcodes are not imported; the workspace has a new passcode of its own",
    );
    let settings: HashMap<String, Value<'_>> = top
        .dict(root.get("settings").copied())?
        .into_iter()
        .collect();
    c.settings(&top, &settings, &mut ws)?;
    c.patches(&top, &settings, &mut ws)?;
    c.network_patches(&top, &settings, &mut ws)?;
    c.video(&top, &settings, &mut ws)?;
    c.cue_lists(&mut ws)?;
    c.resolve_targets(&mut ws);
    let Converter {
        report,
        placeholders,
        cue_order,
        patches,
        ..
    } = c;
    let routes = ws_routes(&report, &ws);
    Ok(QlabImport {
        workspace: ws,
        report,
        placeholders,
        patches: patches.into_iter().map(|(q, id, _)| (q, id)).collect(),
        routes,
        cues: cue_order,
    })
}

/// The new route of each QLab route, in order (routes are made in the report's order).
fn ws_routes(report: &QlabReport, ws: &Workspace) -> Vec<(String, RouteId)> {
    report
        .video_outputs
        .iter()
        .zip(&ws.routes)
        .map(|(q, r)| (q.qlab_id.clone(), r.id))
        .collect()
}

/// Refuses a file that is not a QLab 5 workspace.
fn check_version(top: &Archive<'_>, root: &HashMap<String, Value<'_>>) -> Result<(), QlabError> {
    let version = top.string(root.get("QLabShortVersionString").copied())?;
    let Some(version) = version else {
        // A file with cue lists but no version is from before QLab 5 wrote one.
        return Err(if root.contains_key("cueLists") {
            QlabError::TooOld("4 or earlier".into())
        } else {
            QlabError::NotQlab
        });
    };
    if major(&version) < QLAB_MAJOR {
        return Err(QlabError::TooOld(version));
    }
    let needs = match root.get("QLabRequireMinimumVersion") {
        Some(v) => top.fields(*v)?.map(|f| top.string(f.get("versionString"))),
        None => None,
    }
    .transpose()?
    .flatten();
    if let Some(needs) = &needs
        && major(needs) > QLAB_MAJOR
    {
        return Err(QlabError::TooNew(needs.clone()));
    }
    if major(&version) > QLAB_MAJOR && needs.is_none() {
        return Err(QlabError::TooNew(version));
    }
    Ok(())
}

fn major(version: &str) -> u32 {
    version
        .split('.')
        .next()
        .and_then(|m| m.trim().parse().ok())
        .unwrap_or(0)
}

/// A target to resolve once every cue is converted.
enum Pending {
    Target { cue: CueId, qlab: String },
    NewTarget { cue: CueId, number: String },
}

struct Converter<'a, 'b> {
    ar: &'b Archive<'a>,
    ids: &'b mut dyn FnMut() -> Uuid,
    media: &'b mut dyn FnMut(&QlabFile) -> Option<MediaRef>,
    report: QlabReport,
    placeholders: Vec<QlabPlaceholder>,
    cue_ids: HashMap<String, CueId>,
    cue_order: Vec<(String, CueId)>,
    numbers: HashMap<String, CueId>,
    pending: Vec<Pending>,
    /// QLab id, new id, cue outputs.
    patches: Vec<(String, PatchId, u32)>,
    net_patches: Vec<(String, NetworkPatchId, bool)>,
    stages: Vec<(String, StageId)>,
    /// The stage made for QLab's single built-in stage, when it saved no stages.
    simple_stage: Option<StageId>,
    count: usize,
}

/// The fields every QLab cue has.
struct Common {
    qlab_id: String,
    number: String,
    name: String,
    notes: String,
    pre_wait: f64,
    post_wait: f64,
    continue_mode: ContinueMode,
}

/// Why a cue becomes a placeholder.
enum Kind {
    Converted(Box<CueKind>),
    Unsupported(QlabUnsupportedReason),
}

impl Converter<'_, '_> {
    fn id(&mut self) -> Uuid {
        (self.ids)()
    }

    fn note(&mut self, common: Option<&Common>, kind: QlabNoteKind, detail: impl Into<String>) {
        if self.report.notes.len() >= super::MAX_QLAB_NOTES {
            self.report.notes_omitted = self.report.notes_omitted.saturating_add(1);
            return;
        }
        self.report.notes.push(QlabNote {
            qlab_id: common.map(|c| c.qlab_id.clone()),
            number: common.map(|c| c.number.clone()).unwrap_or_default(),
            name: common.map(|c| c.name.clone()).unwrap_or_default(),
            kind,
            detail: detail.into(),
        });
    }

    fn note_setting(&mut self, detail: &str) {
        self.note(None, QlabNoteKind::Setting, detail);
    }

    // ---- Workspace settings ---------------------------------------------------------------

    fn settings(
        &mut self,
        top: &Archive<'_>,
        settings: &HashMap<String, Value<'_>>,
        ws: &mut Workspace,
    ) -> Result<(), QlabError> {
        let general: HashMap<String, Value<'_>> = top
            .dict(settings.get("General").copied())?
            .into_iter()
            .collect();
        let num =
            |key: &str| -> Result<Option<f64>, QlabError> { top.number(general.get(key).copied()) };
        let flag = |key: &str| -> Result<Option<bool>, QlabError> {
            top.boolean(general.get(key).copied())
        };
        let s = &mut ws.settings;
        if let Some(v) = num("panicDuration")? {
            s.panic_duration = v.clamp(0.0, 60.0);
        }
        if let Some(v) = num("minTimeBetweenGOs")? {
            s.min_go_interval = v.clamp(0.0, 10.0);
        }
        if let Some(v) = num("autoNumberIncrement")?.filter(|v| *v > 0.0) {
            s.auto_number_increment = v.min(crate::validate::MAX_AUTO_NUMBER);
        }
        if let Some(v) = flag("autoNumber")? {
            s.auto_number_new_cues = v;
        }
        if let Some(v) = flag("goRequiresKeyUp")? {
            s.go_needs_key_up = v;
        }
        if let Some(v) = flag("playbackPositionLockedToSelection")? {
            s.lock_playhead = v;
        }
        if let Some(v) = flag("autoLoad")? {
            s.new_cues_auto_load = v;
        }
        if let Some(v) = flag("doAutoCopy")? {
            s.copy_media_on_add = v;
        }
        let audio: HashMap<String, Value<'_>> = top
            .dict(settings.get("Audio").copied())?
            .into_iter()
            .collect();
        if let Some(v) = top.number(audio.get("maxVolume").copied())? {
            s.output_max_db = v.clamp(OUTPUT_MAX_DB_RANGE.0, OUTPUT_MAX_DB_RANGE.1);
        }
        if let Some(v) = top.number(audio.get("minVolume").copied())? {
            s.output_min_db = v.clamp(OUTPUT_MIN_DB_RANGE.0, OUTPUT_MIN_DB_RANGE.1);
        }
        self.key_map(top, settings.get("Key Map").copied(), ws)
    }

    /// The transport and editing keys. A key QLab's map leaves unset has none here.
    fn key_map(
        &mut self,
        top: &Archive<'_>,
        map: Option<Value<'_>>,
        ws: &mut Workspace,
    ) -> Result<(), QlabError> {
        let entries = top.dict(map)?;
        if entries.is_empty() {
            return Ok(());
        }
        let mut keys = TransportHotkeys {
            go: None,
            stop_all: None,
            pause_all: None,
            resume_all: None,
            load_selected: None,
            pause_resume_selected: None,
            start_selected: None,
            stop_selected: None,
            reset_selected: None,
            preview_selected: None,
            panic_selected: None,
            edit_number: None,
            edit_name: None,
            edit_notes: None,
            edit_target: None,
            edit_pre_wait: None,
            edit_duration: None,
            edit_post_wait: None,
            cycle_continue: None,
            toggle_flag: None,
        };
        for (name, value) in entries {
            let slot = match name.as_str() {
                "go" => &mut keys.go,
                "stopAll" => &mut keys.stop_all,
                "pauseAll" => &mut keys.pause_all,
                "resumeAll" => &mut keys.resume_all,
                "loadSelected" => &mut keys.load_selected,
                "pauseResumeSelected" => &mut keys.pause_resume_selected,
                "startSelected" => &mut keys.start_selected,
                "stopSelected" => &mut keys.stop_selected,
                "resetSelected" => &mut keys.reset_selected,
                "preview" => &mut keys.preview_selected,
                "panicSelected" => &mut keys.panic_selected,
                "editNumber" => &mut keys.edit_number,
                "editName" => &mut keys.edit_name,
                "editNotes" => &mut keys.edit_notes,
                "editTarget" => &mut keys.edit_target,
                "editPreWait" => &mut keys.edit_pre_wait,
                "editAction" => &mut keys.edit_duration,
                "editPostWait" => &mut keys.edit_post_wait,
                "cycleContinue" => &mut keys.cycle_continue,
                "flag" => &mut keys.toggle_flag,
                other => {
                    let detail = format!("key map entry \"{other}\" has no openLab action");
                    self.note_setting(&detail);
                    continue;
                }
            };
            let entry: HashMap<String, Value<'_>> = top.dict(Some(value))?.into_iter().collect();
            let get = |k: &str| entry.get(k).copied();
            let key = hotkey(
                top.number(get("keyCharacter"))?,
                [
                    top.boolean(get("shift"))?,
                    top.boolean(get("control"))?,
                    top.boolean(get("option"))?,
                    top.boolean(get("command"))?,
                ],
            );
            if key.is_none() {
                let detail = format!("key map entry \"{name}\" has a key openLab cannot name");
                self.note(None, QlabNoteKind::Hotkey, detail);
            }
            *slot = key;
        }
        ws.settings.hotkeys = keys;
        Ok(())
    }

    // ---- Patches and video outputs -----------------------------------------------------------

    fn patches(
        &mut self,
        top: &Archive<'_>,
        settings: &HashMap<String, Value<'_>>,
        ws: &mut Workspace,
    ) -> Result<(), QlabError> {
        let audio = top.entry(settings.get("Audio").copied(), "audioOutputPatches")?;
        let mut patches = Vec::new();
        for value in top.array(audio)? {
            let Some(f) = top.fields(value)? else {
                continue;
            };
            let qlab_id = top.string(f.get("uniqueID"))?.unwrap_or_default();
            let name = top.string(f.get("name"))?.unwrap_or_default();
            let outputs = top
                .number(f.get("cueOutputChannels"))?
                .map_or(2, |n| n as u32)
                .clamp(1, MAX_CUE_OUTPUTS);
            let device: HashMap<String, Value<'_>> =
                top.dict(f.get("deviceDescription"))?.into_iter().collect();
            let device_outputs = top
                .number(device.get("outputChannels").copied())?
                .map_or(0, |n| n.clamp(0.0, 4096.0) as u32);
            self.report.audio_patches.push(QlabAudioPatch {
                qlab_id: qlab_id.clone(),
                name: name.clone(),
                outputs,
                system: top.boolean(f.get("useSystemDevice"))?.unwrap_or(false),
                device_uid: top
                    .string(device.get("deviceUID").copied())?
                    .unwrap_or_default(),
                device_name: top.string(device.get("name").copied())?.unwrap_or_default(),
                device_outputs,
            });
            let id = PatchId(self.id());
            let matrix = Matrix::read(top, f.get("levels"), MAX_CUE_OUTPUTS, MAX_DEVICE_OUTPUTS)?;
            let mut patch = OutputPatch {
                id,
                name: cut_name(&name),
                master_db: matrix.db(0, 0),
                outputs: (1..=outputs)
                    .map(|i| PatchOutput {
                        name: String::new(),
                        level_db: matrix.db(i, 0),
                        mute: false,
                        polarity_inverted: false,
                    })
                    .collect(),
                crosspoints: Vec::new(),
                device_outputs: Vec::new(),
            };
            let columns = device_outputs.clamp(1, MAX_DEVICE_OUTPUTS);
            for j in 1..=columns {
                let level_db = matrix.db(0, j);
                if level_db != 0.0 {
                    patch.device_outputs.push(DeviceOutputLevel {
                        device_output: j - 1,
                        level_db,
                        mute: false,
                    });
                }
            }
            // QLab's default routing (cue output n to device output n at 0 dB) is openLab's
            // when no crosspoint is listed; anything else is listed in full, since an unlisted
            // pair is silent in QLab and mapped in openLab.
            let default = (1..=outputs).all(|i| {
                (1..=columns).all(|j| {
                    let want = if i == j { 0.0 } else { SILENCE_DB };
                    matrix.db(i, j) == want
                })
            });
            if !default {
                for i in 1..=outputs {
                    for j in 1..=columns {
                        patch.crosspoints.push(PatchCrosspoint {
                            output: i - 1,
                            device_output: j - 1,
                            db: matrix.db(i, j),
                        });
                    }
                }
            }
            let effects = top
                .array(f.get("patchOutputEffects"))?
                .into_iter()
                .chain(top.array(f.get("cueOutputEffects"))?)
                .map(|bus| top.fields(bus))
                .collect::<Result<Vec<_>, _>>()?;
            let mut has_effects = false;
            for bus in effects.into_iter().flatten() {
                has_effects |= !top.array(bus.get("effects"))?.is_empty();
            }
            if has_effects {
                let detail = format!("audio patch \"{name}\": its effects are not imported");
                self.note(None, QlabNoteKind::Effects, detail);
            }
            self.patches.push((qlab_id, id, outputs));
            patches.push(patch);
        }
        if !patches.is_empty() {
            ws.patches = patches;
        } else if let Some(p) = ws.patches.first() {
            self.patches.push((String::new(), p.id, 2));
        }
        Ok(())
    }

    fn network_patches(
        &mut self,
        top: &Archive<'_>,
        settings: &HashMap<String, Value<'_>>,
        ws: &mut Workspace,
    ) -> Result<(), QlabError> {
        let list = top.entry(settings.get("Network").copied(), "networkPatches")?;
        let mut patches = Vec::new();
        for value in top.array(list)? {
            let entry: HashMap<String, Value<'_>> = top.dict(Some(value))?.into_iter().collect();
            let data: HashMap<String, Value<'_>> =
                top.dict(entry.get("data").copied())?.into_iter().collect();
            let get = |k: &str| data.get(k).copied();
            let qlab_id = top.string(get("uniqueID"))?.unwrap_or_default();
            let name = top.string(get("name"))?.unwrap_or_default();
            let osc = top.string(get("deviceIdentifier"))?.as_deref() == Some(OSC_DEVICE);
            let states = top.array(get("clientStates"))?;
            let state: HashMap<String, Value<'_>> = match states.first() {
                Some(s) => top.dict(Some(*s))?.into_iter().collect(),
                None => HashMap::new(),
            };
            let sget = |k: &str| state.get(k).copied();
            let host = top.string(sget("host"))?.unwrap_or_default();
            let port = top
                .number(sget("port"))?
                .map_or(53000, |p| p.clamp(0.0, 65535.0) as u16);
            let interface = top.string(sget("interface"))?.filter(|i| !i.is_empty());
            if states.len() > 1 {
                let detail =
                    format!("network patch \"{name}\": only its first destination is imported");
                self.note_setting(&detail);
            }
            let id = NetworkPatchId(self.id());
            patches.push(NetworkPatch {
                id,
                name: cut_name(&name),
                host: host.chars().take(253).collect(),
                port,
                protocol: if top.boolean(sget("useTcp"))?.unwrap_or(false) {
                    NetworkProtocol::Tcp
                } else {
                    NetworkProtocol::Udp
                },
                interface,
            });
            self.net_patches.push((qlab_id, id, osc));
        }
        if !patches.is_empty() {
            ws.network_patches = patches;
        } else if let Some(p) = ws.network_patches.first() {
            self.net_patches.push((String::new(), p.id, true));
        }
        Ok(())
    }

    fn video(
        &mut self,
        top: &Archive<'_>,
        settings: &HashMap<String, Value<'_>>,
        ws: &mut Workspace,
    ) -> Result<(), QlabError> {
        let video: HashMap<String, Value<'_>> = top
            .dict(settings.get("Video").copied())?
            .into_iter()
            .collect();
        for value in top.array(video.get("routes").copied())? {
            let Some(f) = top.fields(value)? else {
                continue;
            };
            let qlab_id = top.string(f.get("uniqueID"))?.unwrap_or_default();
            let name = top.string(f.get("name"))?.unwrap_or_default();
            let (width, height) = size(&top.string(f.get("naturalSize"))?).unwrap_or((1920, 1080));
            let dest: HashMap<String, Value<'_>> =
                top.dict(f.get("destinationInfo"))?.into_iter().collect();
            self.report.video_outputs.push(QlabVideoOutput {
                qlab_id,
                name: name.clone(),
                screen: top.string(dest.get("name").copied())?.unwrap_or_default(),
                width,
                height,
            });
            let mut route = OutputRoute::new(RouteId(self.id()), cut_name(&name), width, height);
            let rotation = top.number(f.get("rotationDegrees"))?.unwrap_or(0.0);
            route.rotation = match rotation as i64 {
                r @ (0 | 90 | 180 | 270) if rotation.fract() == 0.0 => r as u16,
                _ => {
                    let detail = format!("output \"{name}\": a turn of {rotation} degrees");
                    self.note(None, QlabNoteKind::Video, detail);
                    0
                }
            };
            route.scaling = match top.number(f.get("scalingMode"))?.map(|m| m as i64) {
                Some(0) => RouteScaling::Centre,
                Some(1) => RouteScaling::Fit,
                Some(2) => RouteScaling::Stretch,
                _ => RouteScaling::Fill,
            };
            if top.boolean(f.get("rear"))?.unwrap_or(false) {
                let detail = format!("output \"{name}\": rear projection (mirroring)");
                self.note(None, QlabNoteKind::Video, detail);
            }
            route.guide_colour = Some(next_guide_colour(&ws.routes));
            ws.routes.push(route);
        }
        for value in top.array(video.get("stages").copied())? {
            let Some(f) = top.fields(value)? else {
                continue;
            };
            let qlab_id = top.string(f.get("uniqueID"))?.unwrap_or_default();
            let name = top.string(f.get("name"))?.unwrap_or_default();
            let fallback = ws
                .routes
                .first()
                .map_or((1920, 1080), |r| (r.width, r.height));
            let (width, height) = size(&top.string(f.get("size"))?)
                .or(size(&top.string(f.get("naturalSize"))?))
                .unwrap_or(fallback);
            self.report.stages.push(QlabStage {
                qlab_id: qlab_id.clone(),
                name: name.clone(),
                width,
                height,
            });
            let stage = Stage::new(StageId(self.id()), cut_name(&name), width, height);
            let detail =
                format!("stage \"{name}\": its regions are not imported; set them in openLab");
            self.note(None, QlabNoteKind::Video, detail);
            self.stages.push((qlab_id, stage.id));
            ws.stages.push(stage);
        }
        if ws.stages.is_empty() {
            // QLab's single built-in stage: one openLab stage the size of the first output,
            // shown whole on it.
            let route = ws.routes.first().map(|r| (r.id, r.width, r.height));
            let (width, height) = route.map_or((1920, 1080), |(_, w, h)| (w, h));
            let mut stage = Stage::new(StageId(self.id()), "Stage 1", width, height);
            if let Some((route, _, _)) = route {
                let (w, h) = (stage.width, stage.height);
                stage
                    .regions
                    .push(Region::whole(RegionId(self.id()), Some(route), w, h));
            }
            self.simple_stage = Some(stage.id);
            ws.stages.push(stage);
        }
        Ok(())
    }

    // ---- Cue lists and cues ----------------------------------------------------------------

    fn cue_lists(&mut self, ws: &mut Workspace) -> Result<(), QlabError> {
        let ar = self.ar;
        let root = ar.fields(ar.root()?)?.ok_or(QlabError::NotQlab)?;
        let mut lists = Vec::new();
        for value in ar.array(root.get("cues"))? {
            let Some(f) = ar.fields(value)? else {
                continue;
            };
            let qlab_id = ar.string(f.get("uniqueID"))?.unwrap_or_default();
            let name = ar.string(f.get("name"))?.unwrap_or_default();
            let id = CueListId(self.id());
            if num(ar, &f, "groupMode")? == Some(5) {
                self.report.unsupported.push(QlabUnsupported {
                    qlab_id: qlab_id.clone(),
                    number: String::new(),
                    name: name.clone(),
                    qlab_type: "GroupCue".into(),
                    reason: QlabUnsupportedReason::Cart,
                });
                self.placeholders.push(QlabPlaceholder {
                    qlab_id,
                    at: Placeholder::List(id),
                });
            }
            let cues = self.cues(ar.array(f.get("cues"))?, 1)?;
            lists.push(CueList {
                id,
                name: cut_name(&name),
                cues,
            });
        }
        if !lists.is_empty() {
            ws.cue_lists = lists;
        }
        Ok(())
    }

    fn cues(&mut self, values: Vec<Value<'_>>, depth: usize) -> Result<Vec<Cue>, QlabError> {
        let mut out = Vec::new();
        for value in values {
            if let Some(cue) = self.cue(value, depth)? {
                out.push(cue);
            }
        }
        Ok(out)
    }

    fn cue(&mut self, value: Value<'_>, depth: usize) -> Result<Option<Cue>, QlabError> {
        let ar = self.ar;
        let Some(f) = ar.fields(value)? else {
            self.note(
                None,
                QlabNoteKind::Structure,
                "an entry of a cue list is not a cue",
            );
            return Ok(None);
        };
        self.count += 1;
        if self.count > MAX_QLAB_CUES {
            return Err(QlabError::TooLarge("cues"));
        }
        self.report.cue_count = self.count as u32;
        let class = ar.class(&f)?.unwrap_or_default();
        let mut common = self.common(&f)?;
        if common.qlab_id.is_empty() {
            // Every cue has one; a damaged file may not. Named so the report and choices can
            // still name it.
            common.qlab_id = format!("no-id-{}", self.count);
        }
        if self.cue_ids.contains_key(&common.qlab_id) {
            self.note(
                Some(&common),
                QlabNoteKind::Structure,
                "the cue appears twice in the file; the second is left out",
            );
            return Ok(None);
        }
        if depth > MAX_GROUP_DEPTH {
            let detail = format!("nested deeper than {MAX_GROUP_DEPTH} groups; left out");
            self.note(Some(&common), QlabNoteKind::Structure, detail);
            return Ok(None);
        }
        let id = CueId(self.id());
        self.cue_ids.insert(common.qlab_id.clone(), id);
        self.cue_order.push((common.qlab_id.clone(), id));
        let kind = match self.kind(&class, &f, &common, id, depth) {
            Ok(kind) => kind,
            // A cue that cannot be read becomes a placeholder; limits stop the whole read.
            Err(QlabError::Damaged(_) | QlabError::NotKeyedArchive | QlabError::NotPlist) => {
                Kind::Unsupported(QlabUnsupportedReason::Damaged)
            }
            Err(e) => return Err(e),
        };
        let mut cue = Cue {
            id,
            number: (!common.number.is_empty()).then(|| common.number.clone()),
            name: common.name.clone(),
            notes: common.notes.clone(),
            color: colour(ar.string(f.get("colorName"))?.as_deref()),
            second_color: if ar.boolean(f.get("useSecondColor"))?.unwrap_or(false) {
                colour(ar.string(f.get("secondColorName"))?.as_deref())
            } else {
                CueColor::None
            },
            flagged: ar.boolean(f.get("flagged"))?.unwrap_or(false),
            armed: ar.boolean(f.get("armed"))?.unwrap_or(true),
            auto_load: ar.boolean(f.get("autoLoad"))?.unwrap_or(false),
            skip_if_disarmed: ar.boolean(f.get("skipIfDisarmed"))?.unwrap_or(false),
            pre_wait: common.pre_wait,
            post_wait: common.post_wait,
            continue_mode: common.continue_mode,
            triggers: Triggers {
                hotkey: None,
                hotkey_enabled: true,
                wall_clock: WallClockTrigger::default(),
            },
            second_trigger: SecondTrigger::Nothing,
            second_trigger_on_release: false,
            fade_and_stop_others: FadeAndStopOthers::default(),
            duck_others: DuckOthers::default(),
            kind: CueKind::Memo,
        };
        match kind {
            Kind::Converted(kind) => {
                self.triggers(&f, &common, &mut cue)?;
                cue.kind = *kind;
                if !common.number.is_empty() {
                    self.numbers.entry(common.number.clone()).or_insert(id);
                }
            }
            Kind::Unsupported(reason) => {
                self.report.unsupported.push(QlabUnsupported {
                    qlab_id: common.qlab_id.clone(),
                    number: common.number.clone(),
                    name: common.name.clone(),
                    qlab_type: class.clone(),
                    reason,
                });
                self.placeholders.push(QlabPlaceholder {
                    qlab_id: common.qlab_id.clone(),
                    at: Placeholder::Cue(id),
                });
                cue.color = CueColor::Red;
                if cue.name.is_empty() {
                    cue.name = format!("QLab {} cue (not converted)", type_words(&class));
                }
            }
        }
        Ok(Some(cue))
    }

    fn common(&mut self, f: &Fields<'_>) -> Result<Common, QlabError> {
        let ar = self.ar;
        let qlab_id = ar.string(f.get("uniqueID"))?.unwrap_or_default();
        let number = ar.string(f.get("number"))?.unwrap_or_default();
        let name = ar.string(f.get("name"))?.unwrap_or_default();
        let notes = ar.string(f.get("notes"))?.unwrap_or_default();
        let mut common = Common {
            qlab_id,
            number,
            name: String::new(),
            notes: String::new(),
            pre_wait: time(ar.number(f.get("preWait"))?),
            post_wait: time(ar.number(f.get("postWait"))?),
            continue_mode: match num(ar, f, "continueMode")? {
                Some(1) => ContinueMode::AutoContinue,
                Some(2) => ContinueMode::AutoFollow,
                _ => ContinueMode::DoNotContinue,
            },
        };
        common.name = cut_bytes(&name, MAX_CUE_NAME_BYTES);
        common.notes = cut_bytes(&notes, MAX_CUE_NOTES_BYTES);
        if common.name.len() < name.len() {
            self.note(
                Some(&common),
                QlabNoteKind::Truncated,
                "name cut to openLab's limit",
            );
        }
        if common.notes.len() < notes.len() {
            self.note(
                Some(&common),
                QlabNoteKind::Truncated,
                "notes cut to openLab's limit",
            );
        }
        Ok(common)
    }

    /// Hotkey, wall clock, second trigger, duck and fade-and-stop others.
    fn triggers(
        &mut self,
        f: &Fields<'_>,
        common: &Common,
        cue: &mut Cue,
    ) -> Result<(), QlabError> {
        let ar = self.ar;
        if let Some(hot) = ar.fields(f.get("hotKey").unwrap_or(Value::Null))? {
            let key = hotkey(
                ar.number(hot.get("QLActionKeyKeyCharacter"))?,
                [
                    ar.boolean(hot.get("QLActionKeyShift"))?,
                    ar.boolean(hot.get("QLActionKeyControl"))?,
                    ar.boolean(hot.get("QLActionKeyOption"))?,
                    ar.boolean(hot.get("QLActionKeyCommand"))?,
                ],
            );
            let used = ar.boolean(f.get("useHotKey"))?.unwrap_or(false);
            let set = ar
                .number(hot.get("QLActionKeyKeyCharacter"))?
                .unwrap_or(0.0)
                != 0.0;
            if set && key.is_none() {
                self.note(
                    Some(common),
                    QlabNoteKind::Hotkey,
                    "a hotkey openLab cannot name",
                );
            }
            cue.triggers.hotkey_enabled = key.is_none() || used;
            cue.triggers.hotkey = key;
        }
        let wall = &mut cue.triggers.wall_clock;
        wall.enabled = ar.boolean(f.get("useWallClock"))?.unwrap_or(false);
        wall.hours = num(ar, f, "wallHours")?.unwrap_or(0).clamp(0, 23) as u8;
        wall.minutes = num(ar, f, "wallMinutes")?.unwrap_or(0).clamp(0, 59) as u8;
        wall.seconds = num(ar, f, "wallSeconds")?.unwrap_or(0).clamp(0, 59) as u8;
        let mut days = Weekdays::NONE;
        for (key, day) in [
            ("wallSunday", Weekday::Sunday),
            ("wallMonday", Weekday::Monday),
            ("wallTuesday", Weekday::Tuesday),
            ("wallWednesday", Weekday::Wednesday),
            ("wallThursday", Weekday::Thursday),
            ("wallFriday", Weekday::Friday),
            ("wallSaturday", Weekday::Saturday),
        ] {
            days = days.with(day, ar.boolean(f.get(key))?.unwrap_or(true));
        }
        wall.days = days;
        wall.twelve_hour = num(ar, f, "wallClockMode")? == Some(1);
        cue.second_trigger = match num(ar, f, "secondTriggerAction")? {
            Some(1) => SecondTrigger::Panic,
            Some(2) => SecondTrigger::Stop,
            Some(3) => SecondTrigger::HardStop,
            Some(4) => SecondTrigger::HardStopAndRestart,
            Some(5) => SecondTrigger::Devamp,
            Some(6) => SecondTrigger::PlayNext,
            Some(7) => SecondTrigger::PlayPrevious,
            _ => SecondTrigger::Nothing,
        };
        cue.second_trigger_on_release = ar
            .boolean(f.get("secondTriggerOnRelease"))?
            .unwrap_or(false);
        cue.duck_others = DuckOthers {
            enabled: ar.boolean(f.get("duckOthers"))?.unwrap_or(false),
            db: gain_db(ar.number(f.get("duckLevel"))?.unwrap_or(1.0)),
            duration: time(ar.number(f.get("duckTime"))?),
        };
        // Stored as a boolean when off and a scope (1 to 3) when on.
        let fso = num(ar, f, "fadeAndStopOthers")?.unwrap_or(0);
        cue.fade_and_stop_others = FadeAndStopOthers {
            enabled: (1..=3).contains(&fso),
            scope: match fso {
                2 => OthersScope::List,
                3 => OthersScope::All,
                _ => OthersScope::Peers,
            },
            duration: time(ar.number(f.get("fadeAndStopOthersTime"))?),
        };
        if ar.boolean(f.get("useMIDITrigger"))?.unwrap_or(false) {
            self.note(
                Some(common),
                QlabNoteKind::Trigger,
                "MIDI trigger not imported",
            );
        }
        if ar.boolean(f.get("useTimecode"))?.unwrap_or(false) {
            self.note(
                Some(common),
                QlabNoteKind::Trigger,
                "timecode trigger not imported",
            );
        }
        Ok(())
    }

    /// The cue's kind, or why it is a placeholder.
    fn kind(
        &mut self,
        class: &str,
        f: &Fields<'_>,
        common: &Common,
        id: CueId,
        depth: usize,
    ) -> Result<Kind, QlabError> {
        let ar = self.ar;
        let target_mode = num(ar, f, "targetMode")?.unwrap_or(0);
        if matches!(class, "FadeCue" | "ResetCue") && target_mode != 0 {
            return Ok(Kind::Unsupported(QlabUnsupportedReason::PatchTarget));
        }
        if !matches!(
            class,
            "AudioCue" | "VideoCue" | "GroupCue" | "WaitCue" | "MemoCue" | "OSCCue" | "NetworkCue"
        ) && let Some(target) = ar.string(f.get("cueTargetUniqueID"))?
            && !target.is_empty()
        {
            self.pending.push(Pending::Target {
                cue: id,
                qlab: target,
            });
        }
        let kind = match class {
            "AudioCue" => self.audio(f, common)?,
            "VideoCue" => self.video_cue(f, common)?,
            "FadeCue" => self.fade(f, common)?,
            "GroupCue" => {
                let mode = match num(ar, f, "groupMode")? {
                    Some(1) => GroupMode::StartFirstAndEnter,
                    Some(2) => GroupMode::StartFirstAndGoToNext,
                    Some(4) => GroupMode::StartRandom,
                    Some(5) => return Ok(Kind::Unsupported(QlabUnsupportedReason::Cart)),
                    Some(6) => GroupMode::Playlist {
                        loop_: ar.boolean(f.get("playlistLoop"))?.unwrap_or(false),
                        shuffle: ar.boolean(f.get("playlistShuffle"))?.unwrap_or(false),
                    },
                    _ => GroupMode::Timeline,
                };
                if matches!(mode, GroupMode::Playlist { .. })
                    && ar.boolean(f.get("playlistCrossfade"))?.unwrap_or(false)
                {
                    self.note(
                        Some(common),
                        QlabNoteKind::Crossfade,
                        "crossfade not imported",
                    );
                }
                let children = self.cues(ar.array(f.get("cues"))?, depth + 1)?;
                CueKind::Group { mode, children }
            }
            "StartCue" => CueKind::Start { target: None },
            "StopCue" => CueKind::Stop {
                target: None,
                fade_out: no_fade(),
            },
            "PauseCue" => CueKind::Pause { target: None },
            "LoadCue" => CueKind::Load {
                target: None,
                load_to_time: ar
                    .number(f.get("loadTime"))?
                    .map_or(0.0, |t| round_time(t).clamp(-MAX_TIME, MAX_TIME)),
            },
            "ResetCue" => CueKind::Reset { target: None },
            "DevampCue" => CueKind::Devamp {
                target: None,
                start_next_cue: ar
                    .boolean(f.get("fireNextCueAtMomentLoopEnds"))?
                    .unwrap_or(false),
                stop_target_when_slice_ends: ar
                    .boolean(f.get("stopTargetWhenDone"))?
                    .unwrap_or(false),
                devamp_mode: if num(ar, f, "devampType")? == Some(2) {
                    DevampMode::LoopingCue
                } else {
                    DevampMode::CurrentSlice
                },
            },
            "GotoCue" => CueKind::GoTo {
                target: None,
                then_go: false,
            },
            "ArmCue" => CueKind::Arm { target: None },
            "DisarmCue" => CueKind::Disarm { target: None },
            "TargetCue" => {
                if let Some(number) = ar.string(f.get("targetCueNumber"))?
                    && !number.is_empty()
                {
                    self.pending.push(Pending::NewTarget { cue: id, number });
                }
                CueKind::Target {
                    target: None,
                    new_target: None,
                }
            }
            "WaitCue" => CueKind::Wait {
                duration: time(ar.number(f.get("duration"))?),
            },
            "MemoCue" => CueKind::Memo,
            "OSCCue" | "NetworkCue" => match self.network(f, common)? {
                Some(kind) => kind,
                None => return Ok(Kind::Unsupported(QlabUnsupportedReason::NetworkMessage)),
            },
            _ => return Ok(Kind::Unsupported(QlabUnsupportedReason::CueType)),
        };
        Ok(Kind::Converted(Box::new(kind)))
    }

    fn patch_for(&self, qlab: Option<String>) -> (PatchId, u32) {
        let found = qlab.and_then(|q| self.patches.iter().find(|(id, _, _)| *id == q));
        found
            .or(self.patches.first())
            .map_or((PatchId(Uuid::nil()), 2), |(_, id, n)| (*id, *n))
    }

    fn file(&mut self, f: &Fields<'_>) -> Result<Option<MediaRef>, QlabError> {
        let ar = self.ar;
        let Some(alias) = ar.fields(f.get("fileTarget").unwrap_or(Value::Null))? else {
            return Ok(None);
        };
        let file = QlabFile {
            relative: ar
                .string(alias.get("relativePath"))?
                .filter(|s| !s.is_empty()),
            last_known: ar
                .string(alias.get("lastKnownPath"))?
                .filter(|s| !s.is_empty()),
        };
        if file.relative.is_none() && file.last_known.is_none() {
            return Ok(None);
        }
        Ok((self.media)(&file))
    }

    /// Trims, slices and loops, shared by Audio and Video cues.
    fn timing(
        &mut self,
        f: &Fields<'_>,
        common: &Common,
    ) -> Result<(f64, Option<f64>, Vec<SliceMarker>, PlayCount), QlabError> {
        let ar = self.ar;
        let start = time(ar.number(f.get("startTime"))?);
        let length = ar.number(f.get("lastSeenFileDuration"))?;
        let end = ar
            .number(f.get("endTime"))?
            .map(round_time)
            .filter(|e| *e > 0.0 && length.is_none_or(|l| *e < round_time(l)))
            .map(|e| e.min(MAX_TIME));
        let mut slices = Vec::new();
        for s in ar.array(f.get("slices"))? {
            let Some(s) = ar.fields(s)? else {
                continue;
            };
            slices.push(SliceMarker {
                time: time(ar.number(s.get("time"))?),
                play_count: plays(ar, &s)?,
            });
        }
        let last = match ar.fields(f.get("lastSlice").unwrap_or(Value::Null))? {
            Some(s) => plays(ar, &s)?,
            None => PlayCount::ONCE,
        };
        let whole = plays(ar, f)?;
        let last = if slices.is_empty() {
            // Without markers the last slice is the whole cue; QLab keeps a count for each.
            match (whole, last) {
                (PlayCount::ONCE, last) => last,
                (whole, PlayCount::ONCE) => whole,
                (whole, _) => {
                    self.note(
                        Some(common),
                        QlabNoteKind::PlayCount,
                        "the whole cue's play count and its slice's were both set; the cue's is kept",
                    );
                    whole
                }
            }
        } else {
            if whole != PlayCount::ONCE {
                self.note(
                    Some(common),
                    QlabNoteKind::PlayCount,
                    "the whole cue's play count or loop is not kept on a cue with slices",
                );
            }
            last
        };
        Ok((start, end, slices, last))
    }

    fn audio(&mut self, f: &Fields<'_>, common: &Common) -> Result<CueKind, QlabError> {
        let ar = self.ar;
        let media = self.file(f)?;
        let (patch, outputs) = self.patch_for(ar.string(f.get("audioOutputPatchID"))?);
        let (start_time, end_time, slices, last) = self.timing(f, common)?;
        let levels = self.levels(f, common, outputs)?;
        self.audio_notes(f, common)?;
        Ok(CueKind::Audio {
            media,
            patch,
            start_time,
            end_time,
            fade_in: no_fade(),
            fade_out: no_fade(),
            slices,
            last_slice_play_count: last,
            levels,
            preload: false,
        })
    }

    /// What an Audio cue (or a Video cue's sound) has that openLab drops.
    fn audio_notes(&mut self, f: &Fields<'_>, common: &Common) -> Result<(), QlabError> {
        let ar = self.ar;
        if ar.number(f.get("rate"))?.is_some_and(|r| r != 1.0) {
            self.note(
                Some(common),
                QlabNoteKind::Rate,
                "playback rate not imported",
            );
        }
        if !ar.array(f.get("effects"))?.is_empty() {
            self.note(
                Some(common),
                QlabNoteKind::Effects,
                "audio effects not imported",
            );
        }
        if ar.boolean(f.get("doFade"))?.unwrap_or(false) {
            self.note(
                Some(common),
                QlabNoteKind::IntegratedFade,
                "integrated fade not imported",
            );
        }
        let map = ar.string(f.get("audioMapID"))?.unwrap_or_default();
        let objects = match ar.fields(f.get("audioObjects").unwrap_or(Value::Null))? {
            Some(o) => !ar.array(o.get("audioObjects"))?.is_empty(),
            None => false,
        };
        if !map.is_empty() || objects {
            self.note(
                Some(common),
                QlabNoteKind::AudioObjects,
                "audio objects and audio map not imported",
            );
        }
        if !ar.array(f.get("muteChannels"))?.is_empty()
            || !ar.array(f.get("soloChannels"))?.is_empty()
        {
            self.note(
                Some(common),
                QlabNoteKind::LevelDropped,
                "mutes and solos not imported",
            );
        }
        Ok(())
    }

    /// An Audio cue's level matrix: row 0 column 0 is the master, row 0 column n cue output n,
    /// row i column 0 input i's level, row i column n the crosspoint from input i to output n.
    /// A cell QLab does not store is silent. Trims and input levels fold into the levels.
    fn levels(
        &mut self,
        f: &Fields<'_>,
        common: &Common,
        outputs: u32,
    ) -> Result<CueLevels, QlabError> {
        let matrix = Matrix::read(self.ar, f.get("levels"), MAX_CHANNELS, MAX_CUE_OUTPUTS)?;
        if matrix.trimmed {
            self.note(
                Some(common),
                QlabNoteKind::LevelFolded,
                "trims folded into the levels",
            );
        }
        let outputs = outputs.min(MAX_CUE_OUTPUTS);
        let mut levels = CueLevels {
            master_db: matrix.db(0, 0),
            outputs: (1..=outputs)
                .map(|n| CueOutput {
                    db: matrix.db(0, n),
                    mute: false,
                })
                .collect(),
            crosspoints: Vec::new(),
            gangs: Vec::new(),
        };
        let mut folded = false;
        for input in 1..=MAX_INPUT_ROWS.min(MAX_CHANNELS) {
            let input_gain = matrix.gain(input, 0);
            for output in 1..=outputs {
                let gain = matrix.gain(input, output);
                if gain <= 0.0 {
                    continue;
                }
                folded |= input_gain != 1.0 && input_gain > 0.0;
                if input_gain > 0.0 {
                    levels.crosspoints.push(Crosspoint {
                        input: input - 1,
                        output: output - 1,
                        db: gain_db(gain * input_gain),
                        mute: false,
                    });
                }
            }
        }
        if folded {
            self.note(
                Some(common),
                QlabNoteKind::LevelFolded,
                "input levels folded into the crosspoints",
            );
        }
        let mut dropped_gang = false;
        for (_, cells) in matrix.gangs {
            let members: Vec<u32> = cells
                .iter()
                .filter(|(row, col)| *row == 0 && (1..=outputs).contains(col))
                .map(|(_, col)| col - 1)
                .collect();
            dropped_gang |= members.len() != cells.len();
            if members.len() >= 2 {
                levels.gangs.push(members);
            }
        }
        if dropped_gang {
            self.note(
                Some(common),
                QlabNoteKind::LevelDropped,
                "gangs of the master or crosspoints not imported (openLab gangs cue outputs)",
            );
        }
        Ok(levels)
    }

    fn fade(&mut self, f: &Fields<'_>, common: &Common) -> Result<CueKind, QlabError> {
        let ar = self.ar;
        let fade = ar.fields(f.get("fade").unwrap_or(Value::Null))?;
        let relative = num(ar, f, "mode")? == Some(1);
        let mut levels = FadeLevels {
            master_db: None,
            output_db: Vec::new(),
            crosspoints: Vec::new(),
        };
        let mut curve = FadeCurve::SCurve;
        if let Some(fade) = &fade {
            let mut input_levels = false;
            for (_, entry) in ar.dict(fade.get("entries"))? {
                let Some(e) = ar.fields(entry)? else {
                    continue;
                };
                if !ar.boolean(e.get("live"))?.unwrap_or(false) {
                    continue;
                }
                let row = cell_index(ar.number(e.get("row"))?);
                let col = cell_index(ar.number(e.get("column"))?);
                let db = gain_db(ar.number(e.get("endValue"))?.unwrap_or(0.0));
                match (row, col) {
                    (Some(0), Some(0)) => levels.master_db = Some(db),
                    (Some(0), Some(c)) if (1..=MAX_CUE_OUTPUTS).contains(&c) => {
                        let at = (c - 1) as usize;
                        if levels.output_db.len() <= at {
                            levels.output_db.resize(at + 1, None);
                        }
                        if let Some(slot) = levels.output_db.get_mut(at) {
                            *slot = Some(db);
                        }
                    }
                    (Some(r), Some(0)) if r >= 1 => input_levels = true,
                    (Some(r), Some(c))
                        if (1..=MAX_CHANNELS).contains(&r)
                            && (1..=MAX_CUE_OUTPUTS).contains(&c) =>
                    {
                        levels.crosspoints.push(FadeCrosspoint {
                            input: r - 1,
                            output: c - 1,
                            db,
                        });
                    }
                    _ => {}
                }
            }
            if input_levels {
                self.note(
                    Some(common),
                    QlabNoteKind::LevelDropped,
                    "input levels faded by the cue not imported",
                );
            }
            curve = self.curve(fade, f, common)?;
        }
        levels.crosspoints.sort_by_key(|c| (c.input, c.output));
        let geometry = FadeGeometry {
            fade_opacity: ar.boolean(f.get("doOpacity"))?.unwrap_or(false),
            opacity: ar.number(f.get("opacity"))?.unwrap_or(1.0).clamp(0.0, 1.0),
            fade_translation: ar.boolean(f.get("doTranslation"))?.unwrap_or(false),
            translation: Pair {
                x: ar.number(f.get("translationX"))?.unwrap_or(0.0),
                y: ar.number(f.get("translationY"))?.unwrap_or(0.0),
            },
            fade_scale: ar.boolean(f.get("doScale"))?.unwrap_or(false),
            scale: Pair {
                x: ar.number(f.get("scaleX"))?.unwrap_or(1.0),
                y: ar.number(f.get("scaleY"))?.unwrap_or(1.0),
            },
            scale_locked: ar.boolean(f.get("preserveAspectRatio"))?.unwrap_or(true),
            fade_rotation: ar.boolean(f.get("doRotation"))?.unwrap_or(false),
            rotation: ar.number(f.get("rotation"))?.unwrap_or(0.0),
        };
        if ar.boolean(f.get("doRate"))?.unwrap_or(false) {
            self.note(Some(common), QlabNoteKind::Rate, "rate fade not imported");
        }
        Ok(CueKind::Fade {
            target: None,
            duration: time(ar.number(f.get("duration"))?),
            curve,
            levels,
            stop_target_when_done: ar.boolean(f.get("stopTargetWhenDone"))?.unwrap_or(false),
            mode: if relative {
                FadeMode::Relative
            } else {
                FadeMode::Absolute
            },
            geometry,
        })
    }

    /// The nearest openLab curve to QLab's shape type (0 custom, 1 S-curve, 2 parametric,
    /// 3 linear, 4 path) and audio domain (0 slider, 1 decibel, 2 linear). openLab's curve model
    /// is replaced by QLab's in #756; until then shapes and domains map to the nearest curve.
    fn curve(
        &mut self,
        fade: &Fields<'_>,
        f: &Fields<'_>,
        common: &Common,
    ) -> Result<FadeCurve, QlabError> {
        let ar = self.ar;
        let shapes = ar.fields(fade.get("shapes").unwrap_or(Value::Null))?;
        let shape_type = match &shapes {
            Some(s) => match ar.fields(s.get("downShape").unwrap_or(Value::Null))? {
                Some(down) => num(ar, &down, "type")?,
                None => None,
            },
            None => None,
        };
        let curve = match shape_type {
            Some(3) => FadeCurve::Linear,
            Some(2) => FadeCurve::EqualPower,
            Some(0) => {
                self.note(
                    Some(common),
                    QlabNoteKind::FadeCurve,
                    "custom curve imported as an S-curve",
                );
                FadeCurve::SCurve
            }
            Some(4) => {
                self.note(
                    Some(common),
                    QlabNoteKind::FadeCurve,
                    "path fade imported as linear",
                );
                FadeCurve::Linear
            }
            _ => FadeCurve::SCurve,
        };
        match num(ar, f, "audioDomain")? {
            Some(1) => self.note(
                Some(common),
                QlabNoteKind::FadeCurve,
                "decibel audio domain not kept",
            ),
            Some(2) => self.note(
                Some(common),
                QlabNoteKind::FadeCurve,
                "linear audio domain not kept",
            ),
            _ => {}
        }
        Ok(curve)
    }

    fn network(&mut self, f: &Fields<'_>, common: &Common) -> Result<Option<CueKind>, QlabError> {
        let ar = self.ar;
        let qlab_patch = ar.string(f.get("networkPatchID"))?;
        let found = qlab_patch
            .as_ref()
            .and_then(|q| self.net_patches.iter().find(|(id, _, _)| id == q))
            .or(self.net_patches.first());
        let Some((_, destination, osc)) = found.cloned() else {
            return Ok(None);
        };
        let message = ar.string(f.get("oscString"))?.unwrap_or_default();
        if !osc || message.trim().is_empty() {
            return Ok(None);
        }
        let (address, args) = parse_osc(&message);
        let values = ar.dict(f.get("allParameterValues"))?;
        for (key, value) in values {
            if key == OSC_DEVICE
                && ar
                    .boolean(ar.entry(Some(value), "f53.custom.fadeEnabled")?)?
                    .unwrap_or(false)
            {
                self.note(
                    Some(common),
                    QlabNoteKind::Rate,
                    "OSC parameter fade not imported",
                );
            }
        }
        Ok(Some(CueKind::Network {
            destination,
            address,
            args,
        }))
    }

    fn video_cue(&mut self, f: &Fields<'_>, common: &Common) -> Result<CueKind, QlabError> {
        let ar = self.ar;
        let media = self.file(f)?;
        let viewport = ar.string(f.get("viewportID"))?.unwrap_or_default();
        let stage = if viewport.is_empty() {
            self.simple_stage
        } else {
            self.stages
                .iter()
                .find(|(q, _)| *q == viewport)
                .map(|(_, id)| *id)
        };
        let (start_time, end_time, slices, last) = self.timing(f, common)?;
        let number =
            |key: &str| -> Result<f64, QlabError> { Ok(ar.number(f.get(key))?.unwrap_or(0.0)) };
        let geometry = VideoGeometry {
            fill_stage: ar.boolean(f.get("fullScreen"))?.unwrap_or(true),
            fill_style: match num(ar, f, "fillStyle")? {
                Some(1) => FillStyle::Fill,
                Some(2) => FillStyle::Stretch,
                _ => FillStyle::Fit,
            },
            translation: Pair {
                x: number("translationX")?,
                y: number("translationY")?,
            },
            scale: Pair {
                x: ar.number(f.get("scaleX"))?.unwrap_or(1.0),
                y: ar.number(f.get("scaleY"))?.unwrap_or(1.0),
            },
            scale_locked: ar.boolean(f.get("preserveAspectRatio"))?.unwrap_or(true),
            rotation: 0.0,
            crop: Crop {
                top: number("cropTop")?,
                bottom: number("cropBottom")?,
                left: number("cropLeft")?,
                right: number("cropRight")?,
            },
            anchor: Pair {
                x: number("originX")?,
                y: number("originY")?,
            },
            opacity: ar.number(f.get("opacity"))?.unwrap_or(1.0),
            layer: ar.number(f.get("layer"))?.map_or(MAX_CUE_LAYER, |l| {
                l.clamp(0.0, f64::from(MAX_CUE_LAYER)) as u16
            }),
            smooth: ar.boolean(f.get("antialias"))?.unwrap_or(true),
        };
        if !matches!(
            ar.resolve(f.get("quaternion").unwrap_or(Value::Null))?,
            Value::Null
        ) {
            self.note(Some(common), QlabNoteKind::Video, "rotation not imported");
        }
        if let Some(blend) = ar.fields(f.get("blendMode").unwrap_or(Value::Null))?
            && num(ar, &blend, "index")?.unwrap_or(0) != 0
        {
            self.note(Some(common), QlabNoteKind::Video, "blend mode not imported");
        }
        if let Some(effects) = ar.fields(f.get("videoEffects").unwrap_or(Value::Null))?
            && !ar.array(effects.get("effectEntries"))?.is_empty()
        {
            self.note(
                Some(common),
                QlabNoteKind::Effects,
                "video effects not imported",
            );
        }
        if ar.number(f.get("rate"))?.is_some_and(|r| r != 1.0) {
            self.note(
                Some(common),
                QlabNoteKind::Rate,
                "playback rate not imported",
            );
        }
        let mut sound = VideoSound::default();
        if let Some(audio) = ar.fields(f.get("audioCue").unwrap_or(Value::Null))? {
            let (patch, outputs) = self.patch_for(ar.string(audio.get("audioOutputPatchID"))?);
            sound.patch = Some(patch);
            sound.levels = self.levels(&audio, common, outputs)?;
            self.audio_notes(&audio, common)?;
        }
        let duration = ar
            .number(f.get("duration"))?
            .filter(|d| *d > 0.0)
            .map(round_time);
        Ok(CueKind::Video {
            media,
            stage,
            duration: duration.map(|d| d.min(MAX_TIME)),
            geometry,
            timing: Box::new(VideoTiming {
                start_time,
                end_time,
                fade_in: no_fade(),
                fade_out: no_fade(),
                slices,
                last_slice_play_count: last,
                hold_at_end: ar.boolean(f.get("holdLastFrame"))?.unwrap_or(false),
            }),
            sound: Box::new(sound),
        })
    }

    /// Points every target at its converted cue; a target that did not convert becomes none.
    fn resolve_targets(&mut self, ws: &mut Workspace) {
        for pending in std::mem::take(&mut self.pending) {
            let (cue, found) = match &pending {
                Pending::Target { cue, qlab } => (*cue, self.cue_ids.get(qlab).copied()),
                Pending::NewTarget { cue, number } => (*cue, self.numbers.get(number).copied()),
            };
            let Some(target) = ws.find_cue_mut(cue) else {
                continue;
            };
            match (&pending, found) {
                (Pending::Target { .. }, Some(t)) => {
                    target.kind.set_target(Some(t));
                }
                (Pending::NewTarget { .. }, Some(t)) => {
                    if let CueKind::Target { new_target, .. } = &mut target.kind {
                        *new_target = Some(t);
                    }
                }
                (_, None) => {
                    let detail = "its target is not in the file";
                    let note = QlabNote {
                        qlab_id: self
                            .cue_order
                            .iter()
                            .find(|(_, id)| *id == cue)
                            .map(|(q, _)| q.clone()),
                        number: target.number.clone().unwrap_or_default(),
                        name: target.name.clone(),
                        kind: QlabNoteKind::Target,
                        detail: detail.to_owned(),
                    };
                    if self.report.notes.len() < super::MAX_QLAB_NOTES {
                        self.report.notes.push(note);
                    } else {
                        self.report.notes_omitted = self.report.notes_omitted.saturating_add(1);
                    }
                }
            }
        }
    }
}

/// The QLab network patch type that sends OSC messages.
const OSC_DEVICE: &str = "com.figure53.oscmessage";

/// A QLab level matrix (`AudioLevelMatrix`), as linear gains times trims.
struct Matrix {
    cells: HashMap<(u32, u32), f64>,
    gangs: HashMap<String, Vec<(u32, u32)>>,
    trimmed: bool,
}

impl Matrix {
    /// Reads the cells up to row `rows` and column `columns`; the rest are left out.
    fn read(
        ar: &Archive<'_>,
        value: Option<Value<'_>>,
        rows: u32,
        columns: u32,
    ) -> Result<Self, QlabError> {
        let mut matrix = Self {
            cells: HashMap::new(),
            gangs: HashMap::new(),
            trimmed: false,
        };
        let Some(fields) = ar.fields(value.unwrap_or(Value::Null))? else {
            return Ok(matrix);
        };
        for (_, entry) in ar.dict(fields.get("entries"))? {
            let Some(e) = ar.fields(entry)? else {
                continue;
            };
            let (Some(row), Some(col)) = (
                cell_index(ar.number(e.get("row"))?),
                cell_index(ar.number(e.get("column"))?),
            ) else {
                continue;
            };
            if row > rows || col > columns {
                continue;
            }
            let level = ar.number(e.get("initialLevel"))?.unwrap_or(0.0).max(0.0);
            let trim = ar.number(e.get("trimLevel"))?.unwrap_or(1.0).max(0.0);
            matrix.trimmed |= trim != 1.0 && level > 0.0;
            matrix.cells.insert((row, col), level * trim);
            if let Some(gang) = ar.string(e.get("gang"))?.filter(|g| !g.is_empty()) {
                matrix.gangs.entry(gang).or_default().push((row, col));
            }
        }
        for cells in matrix.gangs.values_mut() {
            cells.sort_unstable();
        }
        Ok(matrix)
    }

    /// The linear gain at a cell; 0 (silent) where QLab stores none.
    fn gain(&self, row: u32, col: u32) -> f64 {
        self.cells.get(&(row, col)).copied().unwrap_or(0.0)
    }

    fn db(&self, row: u32, col: u32) -> f64 {
        gain_db(self.gain(row, col))
    }
}

fn cell_index(n: Option<f64>) -> Option<u32> {
    n.filter(|n| (0.0..=1_000_000.0).contains(n) && n.fract() == 0.0)
        .map(|n| n as u32)
}

/// A linear gain in dB, to a thousandth (QLab stores 32-bit values); silence at or below
/// [`SILENCE_DB`].
fn gain_db(gain: f64) -> f64 {
    if !gain.is_finite() || gain <= 0.0 {
        return SILENCE_DB;
    }
    let db = (20.0 * gain.log10() * 1000.0).round() / 1000.0;
    db.clamp(SILENCE_DB, MAX_DB)
}

/// A time to the microsecond (QLab stores 32-bit values), 0 to [`MAX_TIME`].
fn time(t: Option<f64>) -> f64 {
    t.map_or(0.0, round_time).clamp(0.0, MAX_TIME)
}

fn round_time(t: f64) -> f64 {
    if t.is_finite() {
        (t * 1_000_000.0).round() / 1_000_000.0
    } else {
        0.0
    }
}

fn no_fade() -> FadeSpec {
    FadeSpec {
        duration: 0.0,
        curve: FadeCurve::Linear,
    }
}

/// An integer field.
fn num(ar: &Archive<'_>, f: &Fields<'_>, key: &str) -> Result<Option<i64>, QlabError> {
    Ok(ar
        .number(f.get(key))?
        .filter(|n| n.fract() == 0.0 && n.abs() < 1e15)
        .map(|n| n as i64))
}

/// A slice's plays: `infiniteLoop` loops until devamped, else `playCount` (0 skips it).
fn plays(ar: &Archive<'_>, f: &Fields<'_>) -> Result<PlayCount, QlabError> {
    if ar.boolean(f.get("infiniteLoop"))?.unwrap_or(false) {
        return Ok(PlayCount::Endless);
    }
    Ok(match num(ar, f, "playCount")? {
        Some(0) => PlayCount::Skip,
        Some(n) if n > 0 => PlayCount::from_stored(u32::try_from(n).unwrap_or(u32::MAX)),
        _ => PlayCount::ONCE,
    })
}

/// QLab's colour names to openLab's (the twenty of QLab 5.2, in menu order).
fn colour(name: Option<&str>) -> CueColor {
    match name.unwrap_or("none") {
        "red" => CueColor::Red,
        "orange" => CueColor::Orange,
        "yellow" => CueColor::Yellow,
        "green" => CueColor::Green,
        "cyan" => CueColor::Cyan,
        "blue" => CueColor::Blue,
        "purple" => CueColor::Purple,
        "magenta" => CueColor::Magenta,
        "crimson" => CueColor::DarkRed,
        "peach" => CueColor::Apricot,
        "olive" => CueColor::Olive,
        "forest" => CueColor::DarkGreen,
        "sky blue" => CueColor::LightBlue,
        "midnight" => CueColor::Navy,
        "indigo" => CueColor::Indigo,
        "lavender" => CueColor::Lilac,
        "plum" => CueColor::Plum,
        "berry" => CueColor::Raspberry,
        "hot pink" => CueColor::Pink,
        "gray" | "grey" => CueColor::Grey,
        _ => CueColor::None,
    }
}

/// The openLab hotkey for QLab's key character and modifiers (shift, control, option,
/// command); `None` for no key or one openLab cannot name.
fn hotkey(character: Option<f64>, mods: [Option<bool>; 4]) -> Option<Hotkey> {
    let c = character.filter(|c| *c > 0.0 && c.fract() == 0.0)? as u32;
    let key = match c {
        32 => "space".to_owned(),
        13 | 3 => "return".to_owned(),
        9 => "tab".to_owned(),
        27 => "escape".to_owned(),
        0xF700 => "up".to_owned(),
        0xF701 => "down".to_owned(),
        0xF702 => "left".to_owned(),
        0xF703 => "right".to_owned(),
        0xF704..=0xF70F => format!("f{}", c - 0xF704 + 1),
        _ => {
            let ch = char::from_u32(c)
                .filter(|ch| !ch.is_control() && !('\u{F700}'..='\u{F8FF}').contains(ch))?;
            ch.to_lowercase().collect()
        }
    };
    let [shift, control, alt, command] = mods.map(|m| m.unwrap_or(false));
    Some(Hotkey {
        key,
        modifiers: Modifiers {
            shift,
            control,
            alt,
            command,
        },
    })
}

/// "{w, h}" as whole pixels, each at least 1.
fn size(text: &Option<String>) -> Option<(u32, u32)> {
    let inner = text
        .as_deref()?
        .trim()
        .strip_prefix('{')?
        .strip_suffix('}')?;
    let (w, h) = inner.split_once(',')?;
    let side = |s: &str| -> Option<u32> {
        let v: f64 = s.trim().parse().ok()?;
        (v.is_finite() && v >= 1.0).then(|| v.min(1_000_000.0) as u32)
    };
    Some((side(w)?, side(h)?))
}

/// An OSC message as QLab's text field holds it: the address, then arguments separated by
/// spaces, a quoted string kept whole; a whole number is an int, a number with a point a float,
/// anything else a string.
fn parse_osc(text: &str) -> (String, Vec<OscArg>) {
    let mut tokens = Vec::new();
    let mut chars = text.trim().chars().peekable();
    while chars.peek().is_some() {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let Some(&first) = chars.peek() else {
            break;
        };
        let mut token = String::new();
        if first == '"' {
            chars.next();
            while let Some(c) = chars.next() {
                match c {
                    '"' => break,
                    '\\' => {
                        if let Some(n) = chars.next() {
                            token.push(n);
                        }
                    }
                    c => token.push(c),
                }
            }
            tokens.push((token, true));
        } else {
            while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
                token.push(c);
            }
            tokens.push((token, false));
        }
    }
    let mut tokens = tokens.into_iter();
    let address = tokens.next().map(|(t, _)| t).unwrap_or_default();
    let args = tokens
        .map(|(t, quoted)| {
            if !quoted {
                if let Ok(i) = t.parse::<i32>() {
                    return OscArg::Int { value: i };
                }
                if let Ok(f) = t.parse::<f32>()
                    && f.is_finite()
                    && t.contains('.')
                {
                    return OscArg::Float { value: f };
                }
            }
            OscArg::String { value: t }
        })
        .collect();
    (address, args)
}

/// QLab's class, in our words, for a placeholder's name.
fn type_words(class: &str) -> &str {
    match class {
        "MIDIFileCue" => "MIDI File",
        "TitlesCue" | "TextCue" => "Text",
        "OSCCue" => "Network",
        "GroupCue" => "Cart",
        other => other
            .strip_suffix("Cue")
            .filter(|s| !s.is_empty())
            .unwrap_or("unknown"),
    }
}

/// A name within openLab's name limit (characters).
fn cut_name(name: &str) -> String {
    name.chars().take(MAX_NAME_SCALARS).collect()
}

/// `text` cut to at most `max` bytes on a character boundary.
fn cut_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc_text_splits_into_typed_arguments() {
        let (address, args) = parse_osc("/cue/1/level 0 -6.5 \"a b\" word");
        assert_eq!(address, "/cue/1/level");
        assert_eq!(
            args,
            vec![
                OscArg::Int { value: 0 },
                OscArg::Float { value: -6.5 },
                OscArg::String {
                    value: "a b".into()
                },
                OscArg::String {
                    value: "word".into()
                },
            ]
        );
    }

    #[test]
    fn qlab_keys_name_as_the_app_does() {
        let none = [None; 4];
        assert_eq!(hotkey(Some(97.0), none).map(|k| k.key), Some("a".into()));
        assert_eq!(
            hotkey(Some(32.0), none).map(|k| k.key),
            Some("space".into())
        );
        assert_eq!(
            hotkey(Some(63240.0), none).map(|k| k.key),
            Some("f5".into())
        );
        assert_eq!(hotkey(Some(0.0), none), None);
    }
}
