// SPDX-License-Identifier: GPL-3.0-or-later

//! QLab 5 workspace to openLab workspace. Field names are QLab's as stored; levels in the file
//! are linear gains (1.0 is 0 dB) and become dB here.

use std::collections::HashMap;

use uuid::Uuid;

use super::archive::{Archive, Fields};
use super::plist::{Budget, MAX_KEPT, MAX_WORK, Value};
use super::{
    MAX_QLAB_CUES, MAX_QLAB_LISTS, MAX_QLAB_PATCHES, Placeholder, QLAB_MAJOR, QlabAudioPatch,
    QlabError, QlabFile, QlabImport, QlabNote, QlabNoteKind, QlabNoteSeverity, QlabPlaceholder,
    QlabReport, QlabStage, QlabUnsupported, QlabUnsupportedReason, QlabVideoOutput,
};
use crate::cue::{
    ContinueMode, Crosspoint, Cue, CueColor, CueKind, CueLevels, CueOutput, DevampMode, DuckOthers,
    FadeAndStopOthers, FadeCrosspoint, FadeCurve, FadeLevels, FadeMode, FadeSpec, GroupMode,
    Hotkey, MAX_CHANNELS, MAX_CUE_OUTPUTS, MAX_DB, MAX_DEVICE_OUTPUTS, MAX_SLICE_MARKERS, MAX_TIME,
    MediaRef, Modifiers, OscArg, OthersScope, PlayCount, SILENCE_DB, SecondTrigger, SliceMarker,
    Triggers, WallClockTrigger, Weekday, Weekdays,
};
use crate::fade_curve::{
    AudioDomain, CurvePoint, CurveShape, CurveType, DEFAULT_CURVE_PARAMETER, FadeCueCurve,
    MAX_CURVE_POINTS,
};
use crate::model::{
    CueId, CueList, CueListId, DeviceOutputLevel, NetworkPatch, NetworkPatchId, NetworkProtocol,
    OutputPatch, PatchCrosspoint, PatchId, PatchOutput, RegionId, RouteId, StageId,
    TransportHotkeys, Workspace, WorkspaceId,
};
use crate::osc_access::OscAccess;
use crate::stage_link::MAX_PATH_BYTES;
use crate::validate::{
    MAX_CUE_NAME_BYTES, MAX_CUE_NOTES_BYTES, MAX_GROUP_DEPTH, MAX_NAME_SCALARS, MAX_NETWORK_ARGS,
    MAX_NETWORK_STRING_BYTES,
};
use crate::video::{
    MAX_ROUTES, MAX_STAGES, OutputRoute, Region, RouteScaling, Stage, fill_guide_colours,
};
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
    let budget = Budget::new(MAX_WORK, MAX_KEPT);
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
    let name = cut_name(name.strip_suffix(".qlab5").unwrap_or(&name));
    let mut c = Converter {
        ar: &lists,
        ids,
        media,
        report: QlabReport {
            workspace_name: name.clone(),
            qlab_version: cut_name(
                &top.string(root.get("QLabShortVersionString").copied())?
                    .unwrap_or_default(),
            ),
            ..QlabReport::default()
        },
        placeholders: Vec::new(),
        cue_ids: HashMap::new(),
        cue_order: Vec::new(),
        numbers: HashMap::new(),
        numbers_added: Vec::new(),
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
    ws.name = name;
    let seed = c.id().as_u128() as u32;
    ws.settings.osc_access = OscAccess::new_workspace(seed);
    c.note_setting(
        "OSC access passcodes were not kept; the new workspace has a passcode of its own.",
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
    /// The keys of `numbers` in the order they were added, so a damaged cue's can be removed.
    numbers_added: Vec<String>,
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

/// What was read of a cue before its data turned out damaged.
#[derive(Default)]
struct Head {
    counted: bool,
    common: Option<Common>,
    id: Option<CueId>,
    class: String,
}

/// The lengths of the per-cue records at the start of a cue ([`Converter::roll_back`]).
struct Mark {
    order: usize,
    numbers: usize,
    pending: usize,
    placeholders: usize,
    unsupported: usize,
    notes: usize,
    omitted: u32,
}

/// Longest QLab unique id or class name kept, in bytes (QLab's ids are 36-character UUIDs).
const MAX_ID_BYTES: usize = 256;

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
        self.note_as(common, kind, kind.severity(), detail);
    }

    fn note_as(
        &mut self,
        common: Option<&Common>,
        kind: QlabNoteKind,
        severity: QlabNoteSeverity,
        detail: impl Into<String>,
    ) {
        if self.report.notes.len() >= super::MAX_QLAB_NOTES {
            self.report.notes_omitted = self.report.notes_omitted.saturating_add(1);
            return;
        }
        self.report.notes.push(QlabNote {
            qlab_id: common.map(|c| c.qlab_id.clone()),
            number: common.map(|c| c.number.clone()).unwrap_or_default(),
            name: common.map(|c| c.name.clone()).unwrap_or_default(),
            kind,
            severity,
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
                // QLab actions openLab lacks (audition, cycling the target type): nothing to
                // keep and nothing the operator needs told (owner, 2026-11-26).
                _ => continue,
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
                let detail = format!(
                    "The shortcut for {} uses a key openLab cannot name, so it has none.",
                    key_action(&name)
                );
                self.note_as(
                    None,
                    QlabNoteKind::Hotkey,
                    QlabNoteSeverity::Warning,
                    detail,
                );
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
        let values = top.array(audio)?;
        if values.len() > MAX_QLAB_PATCHES {
            return Err(QlabError::TooLarge("audio patches"));
        }
        let mut patches = Vec::new();
        for value in values {
            let Some(f) = top.fields(value)? else {
                continue;
            };
            let qlab_id = cut_id(top.string(f.get("uniqueID"))?);
            let name = cut_name(&top.string(f.get("name"))?.unwrap_or_default());
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
                device_uid: cut_name(
                    &top.string(device.get("deviceUID").copied())?
                        .unwrap_or_default(),
                ),
                device_name: cut_name(
                    &top.string(device.get("name").copied())?.unwrap_or_default(),
                ),
                device_outputs,
            });
            let id = PatchId(self.id());
            let matrix = Matrix::read(top, f.get("levels"), MAX_CUE_OUTPUTS, MAX_DEVICE_OUTPUTS)?;
            let mut patch = OutputPatch {
                id,
                name: name.clone(),
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
                let cells = (outputs * columns) as usize;
                top.keep(cells * std::mem::size_of::<PatchCrosspoint>())?;
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
                let detail =
                    format!("The effects of audio patch \u{201c}{name}\u{201d} were not kept.");
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
        let values = top.array(list)?;
        if values.len() > MAX_QLAB_PATCHES {
            return Err(QlabError::TooLarge("network patches"));
        }
        let mut patches = Vec::new();
        for value in values {
            let entry: HashMap<String, Value<'_>> = top.dict(Some(value))?.into_iter().collect();
            let data: HashMap<String, Value<'_>> =
                top.dict(entry.get("data").copied())?.into_iter().collect();
            let get = |k: &str| data.get(k).copied();
            let qlab_id = cut_id(top.string(get("uniqueID"))?);
            let name = cut_name(&top.string(get("name"))?.unwrap_or_default());
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
            let interface = top
                .string(sget("interface"))?
                .filter(|i| !i.is_empty())
                .map(|i| cut_name(&i));
            if states.len() > 1 {
                let detail = format!(
                    "Network patch \u{201c}{name}\u{201d} has more than one destination; only the first was kept."
                );
                self.note_setting(&detail);
            }
            let id = NetworkPatchId(self.id());
            patches.push(NetworkPatch {
                id,
                name,
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
        let routes = top.array(video.get("routes").copied())?;
        if routes.len() > MAX_ROUTES {
            return Err(QlabError::TooLarge("video outputs"));
        }
        for value in routes {
            let Some(f) = top.fields(value)? else {
                continue;
            };
            let qlab_id = cut_id(top.string(f.get("uniqueID"))?);
            let name = cut_name(&top.string(f.get("name"))?.unwrap_or_default());
            let (width, height) = size(&top.string(f.get("naturalSize"))?).unwrap_or((1920, 1080));
            let dest: HashMap<String, Value<'_>> =
                top.dict(f.get("destinationInfo"))?.into_iter().collect();
            self.report.video_outputs.push(QlabVideoOutput {
                qlab_id,
                name: name.clone(),
                screen: cut_name(&top.string(dest.get("name").copied())?.unwrap_or_default()),
                width,
                height,
            });
            let mut route = OutputRoute::new(RouteId(self.id()), name.clone(), width, height);
            let rotation = top.number(f.get("rotationDegrees"))?.unwrap_or(0.0);
            route.rotation = match rotation as i64 {
                r @ (0 | 90 | 180 | 270) if rotation.fract() == 0.0 => r as u16,
                _ => {
                    let detail = format!(
                        "Video output \u{201c}{name}\u{201d} is turned {rotation}°, which was not kept."
                    );
                    self.note(None, QlabNoteKind::Video, detail);
                    0
                }
            };
            route.scaling = match top.number(f.get("scalingMode"))?.map(|m| m as i64) {
                Some(0) => RouteScaling::Centre,
                Some(1) => RouteScaling::Stretch,
                Some(3) => RouteScaling::Fit,
                _ => RouteScaling::Fill,
            };
            if top.boolean(f.get("rear"))?.unwrap_or(false) {
                let detail = format!(
                    "Video output \u{201c}{name}\u{201d} is set for rear projection, which was not kept."
                );
                self.note(None, QlabNoteKind::Video, detail);
            }
            ws.routes.push(route);
        }
        fill_guide_colours(&mut ws.routes);
        let stages = top.array(video.get("stages").copied())?;
        if stages.len() > MAX_STAGES {
            return Err(QlabError::TooLarge("stages"));
        }
        for value in stages {
            let Some(f) = top.fields(value)? else {
                continue;
            };
            let qlab_id = cut_id(top.string(f.get("uniqueID"))?);
            let name = cut_name(&top.string(f.get("name"))?.unwrap_or_default());
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
            let stage = Stage::new(StageId(self.id()), name.clone(), width, height);
            let detail = format!(
                "The regions of stage \u{201c}{name}\u{201d} were not kept; set them up in openLab."
            );
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
        let values = ar.array(root.get("cues"))?;
        if values.len() > MAX_QLAB_LISTS {
            return Err(QlabError::TooLarge("cue lists"));
        }
        let mut lists = Vec::new();
        for value in values {
            let Some(f) = ar.fields(value)? else {
                continue;
            };
            let qlab_id = cut_id(ar.string(f.get("uniqueID"))?);
            let name = cut_name(&ar.string(f.get("name"))?.unwrap_or_default());
            let id = CueListId(self.id());
            if num(ar, &f, "groupMode")? == Some(5) {
                self.report.unsupported.push(QlabUnsupported {
                    qlab_id: qlab_id.clone(),
                    number: String::new(),
                    name: name.clone(),
                    qlab_type: type_words("GroupCue", QlabUnsupportedReason::Cart),
                    reason: QlabUnsupportedReason::Cart,
                });
                self.placeholders.push(QlabPlaceholder {
                    qlab_id,
                    at: Placeholder::List(id),
                });
            }
            let cues = self.cues(ar.array(f.get("cues"))?, 1)?;
            lists.push(CueList { id, name, cues });
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

    /// One cue, read whole or not at all. A cue whose data cannot be read (its own fields or
    /// anything below them) becomes a placeholder, reason damaged, and leaves nothing of what
    /// was read of it behind; its siblings and its group are unaffected. Limits stop the whole
    /// read.
    fn cue(&mut self, value: Value<'_>, depth: usize) -> Result<Option<Cue>, QlabError> {
        let mark = self.mark();
        let mut head = Head::default();
        match self.read_cue(value, depth, &mut head) {
            Err(QlabError::Damaged(_) | QlabError::NotKeyedArchive | QlabError::NotPlist) => {
                self.roll_back(mark);
                self.damaged(head).map(Some)
            }
            other => other,
        }
    }

    fn count_cue(&mut self) -> Result<(), QlabError> {
        self.count += 1;
        if self.count > MAX_QLAB_CUES {
            return Err(QlabError::TooLarge("cues"));
        }
        self.report.cue_count = self.count as u32;
        Ok(())
    }

    fn read_cue(
        &mut self,
        value: Value<'_>,
        depth: usize,
        head: &mut Head,
    ) -> Result<Option<Cue>, QlabError> {
        let ar = self.ar;
        let Some(f) = ar.fields(value)? else {
            self.note(
                None,
                QlabNoteKind::Structure,
                "An entry in a cue list is not a cue and was left out.",
            );
            return Ok(None);
        };
        self.count_cue()?;
        head.counted = true;
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
                "It appears twice in the file; the second copy was left out.",
            );
            return Ok(None);
        }
        if depth > MAX_GROUP_DEPTH {
            let detail =
                format!("It is nested deeper than {MAX_GROUP_DEPTH} groups and was left out.");
            self.note(Some(&common), QlabNoteKind::Structure, detail);
            return Ok(None);
        }
        let id = CueId(self.id());
        self.cue_ids.insert(common.qlab_id.clone(), id);
        self.cue_order.push((common.qlab_id.clone(), id));
        head.id = Some(id);
        let common = &*head.common.insert(common);
        let class = cut_bytes(&ar.class(&f)?.unwrap_or_default(), MAX_ID_BYTES);
        head.class.clone_from(&class);
        let mut cue = base_cue(id, common);
        cue.color = colour(ar.string(f.get("colorName"))?.as_deref());
        if ar.boolean(f.get("useSecondColor"))?.unwrap_or(false) {
            cue.second_color = colour(ar.string(f.get("secondColorName"))?.as_deref());
        }
        cue.flagged = ar.boolean(f.get("flagged"))?.unwrap_or(false);
        cue.armed = ar.boolean(f.get("armed"))?.unwrap_or(true);
        cue.auto_load = ar.boolean(f.get("autoLoad"))?.unwrap_or(false);
        cue.skip_if_disarmed = ar.boolean(f.get("skipIfDisarmed"))?.unwrap_or(false);
        match self.kind(&class, &f, common, id, depth)? {
            Kind::Converted(kind) => {
                self.triggers(&f, common, &mut cue)?;
                cue.kind = *kind;
                if !common.number.is_empty() && !self.numbers.contains_key(&common.number) {
                    self.numbers.insert(common.number.clone(), id);
                    self.numbers_added.push(common.number.clone());
                }
            }
            Kind::Unsupported(reason) => self.placeholder(&mut cue, common, &class, reason),
        }
        Ok(Some(cue))
    }

    /// Where the per-cue records stand, to roll a damaged cue's back to.
    fn mark(&self) -> Mark {
        Mark {
            order: self.cue_order.len(),
            numbers: self.numbers_added.len(),
            pending: self.pending.len(),
            placeholders: self.placeholders.len(),
            unsupported: self.report.unsupported.len(),
            notes: self.report.notes.len(),
            omitted: self.report.notes_omitted,
        }
    }

    /// Forgets every cue registered since `mark` (a damaged cue and what was read inside it),
    /// with their targets, placeholders and notes.
    fn roll_back(&mut self, mark: Mark) {
        for (qlab, _) in self.cue_order.drain(mark.order..) {
            self.cue_ids.remove(&qlab);
        }
        for number in self.numbers_added.drain(mark.numbers..) {
            self.numbers.remove(&number);
        }
        self.pending.truncate(mark.pending);
        self.placeholders.truncate(mark.placeholders);
        self.report.unsupported.truncate(mark.unsupported);
        self.report.notes.truncate(mark.notes);
        self.report.notes_omitted = mark.omitted;
    }

    /// The placeholder of a cue that could not be read: what was read of it, red, reason
    /// damaged.
    fn damaged(&mut self, head: Head) -> Result<Cue, QlabError> {
        if !head.counted {
            self.count_cue()?;
        }
        let common = head.common.unwrap_or_else(|| Common {
            qlab_id: format!("no-id-{}", self.count),
            number: String::new(),
            name: String::new(),
            notes: String::new(),
            pre_wait: 0.0,
            post_wait: 0.0,
            continue_mode: ContinueMode::DoNotContinue,
        });
        let id = head.id.unwrap_or_else(|| CueId(self.id()));
        self.cue_ids.insert(common.qlab_id.clone(), id);
        self.cue_order.push((common.qlab_id.clone(), id));
        let mut cue = base_cue(id, &common);
        self.placeholder(
            &mut cue,
            &common,
            &head.class,
            QlabUnsupportedReason::Damaged,
        );
        Ok(cue)
    }

    /// Makes `cue` the placeholder of an unsupported cue and lists it in the report.
    fn placeholder(
        &mut self,
        cue: &mut Cue,
        common: &Common,
        class: &str,
        reason: QlabUnsupportedReason,
    ) {
        self.report.unsupported.push(QlabUnsupported {
            qlab_id: common.qlab_id.clone(),
            number: common.number.clone(),
            name: common.name.clone(),
            qlab_type: type_words(class, reason),
            reason,
        });
        self.placeholders.push(QlabPlaceholder {
            qlab_id: common.qlab_id.clone(),
            at: Placeholder::Cue(cue.id),
        });
        cue.color = CueColor::Red;
        if cue.name.is_empty() {
            cue.name = format!("QLab {} cue (not converted)", type_words(class, reason));
        }
    }

    fn common(&mut self, f: &Fields<'_>) -> Result<Common, QlabError> {
        let ar = self.ar;
        let qlab_id = cut_id(ar.string(f.get("uniqueID"))?);
        let number = ar.string(f.get("number"))?.unwrap_or_default();
        let name = ar.string(f.get("name"))?.unwrap_or_default();
        let notes = ar.string(f.get("notes"))?.unwrap_or_default();
        let common = Common {
            qlab_id,
            number: cut_bytes(&number, MAX_CUE_NAME_BYTES),
            name: cut_bytes(&name, MAX_CUE_NAME_BYTES),
            notes: cut_bytes(&notes, MAX_CUE_NOTES_BYTES),
            pre_wait: time(ar.number(f.get("preWait"))?),
            post_wait: time(ar.number(f.get("postWait"))?),
            continue_mode: match num(ar, f, "continueMode")? {
                Some(1) => ContinueMode::AutoContinue,
                Some(2) => ContinueMode::AutoFollow,
                _ => ContinueMode::DoNotContinue,
            },
        };
        ar.keep(
            common.qlab_id.len() + common.number.len() + common.name.len() + common.notes.len(),
        )?;
        for (kept, read, what) in [
            (
                common.number.len(),
                number.len(),
                "Its number was shortened to openLab's limit.",
            ),
            (
                common.name.len(),
                name.len(),
                "Its name was shortened to openLab's limit.",
            ),
            (
                common.notes.len(),
                notes.len(),
                "Its notes were shortened to openLab's limit.",
            ),
        ] {
            if kept < read {
                self.note(Some(&common), QlabNoteKind::Truncated, what);
            }
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
        // Ticked only for a cue that has a key and uses it.
        cue.triggers.hotkey_enabled = false;
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
                    "Its hotkey uses a key openLab cannot name, so it has none.",
                );
            }
            cue.triggers.hotkey_enabled = key.is_some() && used;
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
                "Its MIDI trigger was not kept.",
            );
        }
        if ar.boolean(f.get("useTimecode"))?.unwrap_or(false) {
            self.note(
                Some(common),
                QlabNoteKind::Trigger,
                "Its timecode trigger was not kept.",
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
                        "Its playlist crossfade was not kept.",
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
        // A path longer than any system opens is no path.
        let path = |p: Option<String>| p.filter(|s| !s.is_empty() && s.len() <= MAX_PATH_BYTES);
        let file = QlabFile {
            relative: path(ar.string(alias.get("relativePath"))?),
            last_known: path(ar.string(alias.get("lastKnownPath"))?),
        };
        if file.relative.is_none() && file.last_known.is_none() {
            return Ok(None);
        }
        ar.keep(file.relative.as_ref().map_or(0, String::len))?;
        ar.keep(file.last_known.as_ref().map_or(0, String::len))?;
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
        let markers = ar.array(f.get("slices"))?;
        if markers.len() > MAX_SLICE_MARKERS {
            let detail = format!("Slice markers past openLab's {MAX_SLICE_MARKERS} were left out.");
            self.note(Some(common), QlabNoteKind::Structure, detail);
        }
        ar.keep(markers.len().min(MAX_SLICE_MARKERS) * std::mem::size_of::<SliceMarker>())?;
        for s in markers.into_iter().take(MAX_SLICE_MARKERS) {
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
                        "Both the whole cue and its last slice have a play count; the whole cue's was kept.",
                    );
                    whole
                }
            }
        } else {
            if whole != PlayCount::ONCE {
                self.note(
                    Some(common),
                    QlabNoteKind::PlayCount,
                    "Its whole-cue play count or loop was not kept, because it has slices.",
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
                "Its playback rate was not kept.",
            );
        }
        if !ar.array(f.get("effects"))?.is_empty() {
            self.note(
                Some(common),
                QlabNoteKind::Effects,
                "Its audio effects were not kept.",
            );
        }
        if ar.boolean(f.get("doFade"))?.unwrap_or(false) {
            self.note(
                Some(common),
                QlabNoteKind::IntegratedFade,
                "Its built-in fade was not kept.",
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
                "Its audio objects and audio map were not kept.",
            );
        }
        if !ar.array(f.get("muteChannels"))?.is_empty()
            || !ar.array(f.get("soloChannels"))?.is_empty()
        {
            self.note(
                Some(common),
                QlabNoteKind::LevelDropped,
                "Its mutes and solos were not kept.",
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
                "Its trims were folded into its levels.",
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
                "Its input levels were folded into its crosspoints.",
            );
        }
        self.ar.keep(
            levels.outputs.len() * std::mem::size_of::<CueOutput>()
                + levels.crosspoints.len() * std::mem::size_of::<Crosspoint>(),
        )?;
        let mut dropped_gang = false;
        for (_, cells) in matrix.gangs {
            let members: Vec<u32> = cells
                .iter()
                .filter(|(row, col)| *row == 0 && (1..=outputs).contains(col))
                .map(|(_, col)| col - 1)
                .collect();
            dropped_gang |= members.len() != cells.len();
            if members.len() >= 2 {
                self.ar
                    .keep(members.len() * 4 + std::mem::size_of::<Vec<u32>>())?;
                levels.gangs.push(members);
            }
        }
        if dropped_gang {
            self.note(
                Some(common),
                QlabNoteKind::LevelDropped,
                "Its gangs of the master or crosspoints were not kept; openLab gangs only cue outputs.",
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
        let mut curve = FadeCueCurve::default();
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
                    "The input levels it fades were not kept.",
                );
            }
            curve = self.curve(fade, f, common)?;
        }
        // One value a cell: a file listing a cell twice keeps the last.
        levels.crosspoints.reverse();
        levels.crosspoints.sort_by_key(|c| (c.input, c.output));
        levels.crosspoints.dedup_by_key(|c| (c.input, c.output));
        ar.keep(
            levels.output_db.len() * std::mem::size_of::<Option<f64>>()
                + levels.crosspoints.len() * std::mem::size_of::<FadeCrosspoint>(),
        )?;
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
            self.note(
                Some(common),
                QlabNoteKind::Rate,
                "Its rate fade was not kept.",
            );
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

    /// The Fade's curve. QLab stores a shape for rising levels (`upShape`) and one for falling
    /// levels (`downShape`), each with its type, parameter and points, and the audio domain
    /// (0 slider, 1 decibel, 2 linear). A value openLab cannot hold is noted and the default
    /// takes its place.
    fn curve(
        &mut self,
        fade: &Fields<'_>,
        f: &Fields<'_>,
        common: &Common,
    ) -> Result<FadeCueCurve, QlabError> {
        let ar = self.ar;
        let audio_domain = match num(ar, f, "audioDomain")? {
            None | Some(0) => AudioDomain::Slider,
            Some(1) => AudioDomain::Decibel,
            Some(2) => AudioDomain::Linear,
            Some(_) => {
                self.note(
                    Some(common),
                    QlabNoteKind::FadeCurve,
                    "Its audio domain is one openLab does not have, so it uses the slider domain.",
                );
                AudioDomain::Slider
            }
        };
        let shapes = ar.fields(fade.get("shapes").unwrap_or(Value::Null))?;
        let (rising, falling) = match &shapes {
            Some(s) => (shape(ar, s, "upShape")?, shape(ar, s, "downShape")?),
            None => (Some(CurveShape::default()), Some(CurveShape::default())),
        };
        let (Some(rising), Some(falling)) = (rising, falling) else {
            self.note(
                Some(common),
                QlabNoteKind::FadeCurve,
                "Its curve shape is one openLab cannot hold, so it uses the S-curve.",
            );
            return Ok(FadeCueCurve {
                audio_domain,
                ..Default::default()
            });
        };
        ar.keep((rising.points.len() + falling.points.len()) * std::mem::size_of::<CurvePoint>())?;
        Ok(FadeCueCurve {
            audio_domain,
            rising,
            falling,
        })
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
        let read = ar.string(f.get("oscString"))?.unwrap_or_default();
        if !osc || read.trim().is_empty() {
            return Ok(None);
        }
        let message = cut_bytes(&read, MAX_NETWORK_STRING_BYTES);
        let (address, mut args) = parse_osc(&message);
        if message.len() < read.len() || args.len() > MAX_NETWORK_ARGS {
            args.truncate(MAX_NETWORK_ARGS);
            self.note_as(
                Some(common),
                QlabNoteKind::Truncated,
                QlabNoteSeverity::Error,
                "Its OSC message was shortened to openLab's limit.",
            );
        }
        ar.keep(message.len() + args.len() * std::mem::size_of::<OscArg>())?;
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
                    "Its OSC parameter fade was not kept.",
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
            self.note(
                Some(common),
                QlabNoteKind::Video,
                "Its rotation was not kept.",
            );
        }
        if let Some(blend) = ar.fields(f.get("blendMode").unwrap_or(Value::Null))?
            && num(ar, &blend, "index")?.unwrap_or(0) != 0
        {
            self.note(
                Some(common),
                QlabNoteKind::Video,
                "Its blend mode was not kept.",
            );
        }
        if let Some(effects) = ar.fields(f.get("videoEffects").unwrap_or(Value::Null))?
            && !ar.array(effects.get("effectEntries"))?.is_empty()
        {
            self.note(
                Some(common),
                QlabNoteKind::Effects,
                "Its video effects were not kept.",
            );
        }
        if ar.number(f.get("rate"))?.is_some_and(|r| r != 1.0) {
            self.note(
                Some(common),
                QlabNoteKind::Rate,
                "Its playback rate was not kept.",
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
                    let detail = "Its target is not in the file, so it has none.";
                    let note = QlabNote {
                        qlab_id: self
                            .cue_order
                            .iter()
                            .find(|(_, id)| *id == cue)
                            .map(|(q, _)| q.clone()),
                        number: target.number.clone().unwrap_or_default(),
                        name: target.name.clone(),
                        kind: QlabNoteKind::Target,
                        severity: QlabNoteKind::Target.severity(),
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

/// The action of a QLab key map entry, in words: "The shortcut for {} ...". A name not listed
/// is split into lower-case words.
fn key_action(name: &str) -> String {
    let words = match name {
        "go" => "GO",
        "stopAll" => "stopping all cues",
        "pauseAll" => "pausing all cues",
        "resumeAll" => "resuming all cues",
        "loadSelected" => "loading the selected cue",
        "pauseResumeSelected" => "pausing or resuming the selected cue",
        "startSelected" => "starting the selected cue",
        "stopSelected" => "stopping the selected cue",
        "resetSelected" => "resetting the selected cue",
        "preview" => "previewing the selected cue",
        "panicSelected" => "panicking the selected cue",
        "editNumber" => "editing the cue number",
        "editName" => "editing the cue name",
        "editNotes" => "editing the cue notes",
        "editTarget" => "editing the cue target",
        "editPreWait" => "editing the pre-wait",
        "editAction" => "editing the duration",
        "editPostWait" => "editing the post-wait",
        "cycleContinue" => "cycling the continue mode",
        "flag" => "flagging the cue",
        other => {
            let mut out = String::with_capacity(other.len() + 8);
            for c in other.chars() {
                if c.is_uppercase() && !out.is_empty() {
                    out.push(' ');
                }
                out.extend(c.to_lowercase());
            }
            return out;
        }
    };
    words.to_owned()
}

/// One of a Fade's shapes (`upShape` or `downShape` in `shapes`): type 0 custom, 1 S-curve,
/// 2 parametric, 3 linear; `curveParameter`; and `shapeEntries`, points `t` (the time) and `v`
/// (the height in the graph), both 0 to 1, whose first and last are the fixed ends openLab does
/// not store. `None` for a shape openLab cannot hold: another type, a parameter or point out of
/// range, points out of order or too many.
fn shape(
    ar: &Archive<'_>,
    shapes: &Fields<'_>,
    key: &str,
) -> Result<Option<CurveShape>, QlabError> {
    let Some(s) = ar.fields(shapes.get(key).unwrap_or(Value::Null))? else {
        return Ok(Some(CurveShape::default()));
    };
    let kind = match num(ar, &s, "type")? {
        None | Some(1) => CurveType::SCurve,
        Some(0) => CurveType::Custom,
        Some(2) => CurveType::Parametric,
        Some(3) => CurveType::Linear,
        Some(_) => return Ok(None),
    };
    let parameter = ar
        .number(s.get("curveParameter"))?
        .unwrap_or(DEFAULT_CURVE_PARAMETER);
    let entries = ar.array(s.get("shapeEntries"))?;
    if entries.len() > MAX_CURVE_POINTS + 2 {
        return Ok(None);
    }
    let mut points = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(e) = ar.fields(entry)? else {
            return Ok(None);
        };
        let (Some(t), Some(v)) = (ar.number(e.get("t"))?, ar.number(e.get("v"))?) else {
            return Ok(None);
        };
        if t == 0.0 || t == 1.0 {
            continue;
        }
        points.push(CurvePoint { t, v });
    }
    let shape = CurveShape {
        kind,
        parameter,
        points,
    };
    Ok(shape.is_valid().then_some(shape))
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

/// QLab's class in our words, for the report and a placeholder's name (a group is a cart only
/// when that is why it is one). A class not listed loses its `Cue` suffix and is split into
/// words.
fn type_words(class: &str, reason: QlabUnsupportedReason) -> String {
    let words = match class {
        "MIDIFileCue" => "MIDI File",
        "MIDICue" => "MIDI",
        "MicCue" => "Microphone",
        "TitlesCue" | "TextCue" => "Text",
        "OSCCue" | "NetworkCue" => "Network",
        "GroupCue" if reason == QlabUnsupportedReason::Cart => "Cart",
        "GroupCue" => "Group",
        other => {
            let stem = other.strip_suffix("Cue").unwrap_or(other);
            if stem.is_empty() {
                return "unknown".into();
            }
            let mut out = String::with_capacity(stem.len() + 4);
            let mut prev_lower = false;
            for c in stem.chars() {
                if c.is_uppercase() && prev_lower {
                    out.push(' ');
                }
                prev_lower = c.is_lowercase();
                out.push(c);
            }
            return out;
        }
    };
    words.to_owned()
}

/// A unique id as read, within [`MAX_ID_BYTES`].
fn cut_id(id: Option<String>) -> String {
    cut_bytes(&id.unwrap_or_default(), MAX_ID_BYTES)
}

/// A new cue with a QLab cue's common fields and openLab's defaults for the rest (a Memo).
fn base_cue(id: CueId, common: &Common) -> Cue {
    Cue {
        id,
        number: (!common.number.is_empty()).then(|| common.number.clone()),
        name: common.name.clone(),
        notes: common.notes.clone(),
        color: CueColor::None,
        second_color: CueColor::None,
        flagged: false,
        armed: true,
        auto_load: false,
        skip_if_disarmed: false,
        pre_wait: common.pre_wait,
        post_wait: common.post_wait,
        continue_mode: common.continue_mode,
        triggers: Triggers {
            hotkey: None,
            hotkey_enabled: false,
            wall_clock: WallClockTrigger::default(),
        },
        second_trigger: SecondTrigger::Nothing,
        second_trigger_on_release: false,
        fade_and_stop_others: FadeAndStopOthers::default(),
        duck_others: DuckOthers::default(),
        kind: CueKind::Memo,
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
