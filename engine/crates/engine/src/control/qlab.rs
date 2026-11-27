// SPDX-License-Identifier: GPL-3.0-or-later

//! A new workspace from a QLab 5 workspace file (`inspect_qlab`, `import_qlab`,
//! docs/protocol.md). The file is read and converted on the document worker (core's
//! `read_qlab`), never written; its media are looked for there too. The operator's device
//! choices are stored in the machine settings when the new doc is installed, as
//! `set_patch_mapping` and `set_route_display` store them.

use std::path::{Path, PathBuf};

use openlab_core::protocol::{DeviceEntry, ErrorBody, ErrorCode, LogLevel, ResultData};
use openlab_core::{
    CueListId, LoadedShow, MAX_QLAB_FILE_BYTES, MediaRef, PatchId, PatchMapping, PathStyle,
    QlabCueAction, QlabCueChoice, QlabError, QlabFile, QlabOutputChoice, QlabPatchChoice, RouteId,
    portable_ref, read_qlab,
};

use super::docs::{DocAnswer, Loaded, failed, load_notices, random_id};
use super::{Control, DocId};
use crate::persist::{PersistError, read_bytes, regular_file_or_absent};

/// What `import_qlab` asks for besides the file.
#[derive(Debug, Default)]
pub struct QlabChoices {
    pub cues: Vec<QlabCueChoice>,
    pub other_cues: QlabCueAction,
    pub audio_patches: Vec<QlabPatchChoice>,
    pub video_outputs: Vec<QlabOutputChoice>,
}

/// The device choices of an imported workspace, by its new patch and route ids, stored in the
/// machine settings when the doc is installed.
#[derive(Debug, Default)]
pub struct ImportedDevices {
    /// `None`: left unmapped (stored as cleared, so the default mapping leaves it alone).
    pub patches: Vec<(PatchId, Option<PatchMapping>)>,
    pub routes: Vec<(RouteId, Option<String>)>,
}

/// Reads the file: the protocol code of a failure with its message.
fn read(path: &Path) -> Result<Vec<u8>, (ErrorCode, String)> {
    read_bytes(path, MAX_QLAB_FILE_BYTES).map_err(|e| {
        let code = match &e {
            PersistError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
                ErrorCode::NotFound
            }
            PersistError::TooLarge { .. } => ErrorCode::BadShowFile,
            _ => ErrorCode::Io,
        };
        (code, e.to_string())
    })
}

fn refusal(path: &Path, e: &QlabError) -> (ErrorCode, String) {
    let code = match e {
        QlabError::NotPlist | QlabError::NotKeyedArchive | QlabError::NotQlab => {
            ErrorCode::NotQlabWorkspace
        }
        QlabError::TooOld(_) => ErrorCode::QlabTooOld,
        QlabError::TooNew(_) => ErrorCode::QlabTooNew,
        QlabError::Damaged(_) | QlabError::TooLarge(_) | QlabError::TooComplex => {
            ErrorCode::BadShowFile
        }
    };
    (code, format!("{}: {e}", path.display()))
}

/// A media file QLab stored: next to the QLab workspace (its relative path), then where it
/// last was; neither there, the one next to the workspace, shown missing for the operator to
/// relink. Always an absolute path: the new workspace has no folder yet.
fn find_media(dir: &Path, file: &QlabFile) -> Option<MediaRef> {
    let beside = file.relative.as_ref().map(|r| dir.join(r));
    let last = file.last_known.as_ref().map(PathBuf::from);
    let found = [&beside, &last]
        .into_iter()
        .flatten()
        .find(|p| matches!(regular_file_or_absent(p), Ok(true)));
    let path = found.or(beside.as_ref()).or(last.as_ref())?;
    let text = path.to_str()?;
    Some(portable_ref(text, None, PathStyle::host()))
}

/// `inspect_qlab`, on the document worker.
pub(super) fn inspect(ticket: u64, path: &Path) -> DocAnswer {
    let bytes = match read(path) {
        Ok(b) => b,
        Err((code, message)) => return failed(ticket, code, message),
    };
    match read_qlab(&bytes, &mut random_id, &mut |_| None) {
        Ok(import) => DocAnswer::Data {
            ticket,
            data: Box::new(ResultData::QlabReport(Box::new(import.report))),
        },
        Err(e) => {
            let (code, message) = refusal(path, &e);
            failed(ticket, code, message)
        }
    }
}

/// `import_qlab`, on the document worker: the converted workspace, unsaved and untitled,
/// with the operator's choices applied.
pub(super) fn import(ticket: u64, path: &Path, choices: &QlabChoices) -> DocAnswer {
    let bytes = match read(path) {
        Ok(b) => b,
        Err((code, message)) => return failed(ticket, code, message),
    };
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let import = match read_qlab(&bytes, &mut random_id, &mut |f| find_media(&dir, f)) {
        Ok(import) => import,
        Err(e) => {
            let (code, message) = refusal(path, &e);
            return failed(ticket, code, message);
        }
    };
    let devices = ImportedDevices {
        patches: choices
            .audio_patches
            .iter()
            .filter_map(|c| {
                let (_, id) = import.patches.iter().find(|(q, _)| *q == c.qlab_id)?;
                Some((*id, c.mapping.clone()))
            })
            .collect(),
        routes: choices
            .video_outputs
            .iter()
            .filter_map(|c| {
                let (_, id) = import.routes.iter().find(|(q, _)| *q == c.qlab_id)?;
                Some((*id, c.display.clone()))
            })
            .collect(),
    };
    let (workspace, warnings) =
        import.finish(&choices.cues, choices.other_cues, CueListId(random_id()));
    let shown = LoadedShow {
        workspace,
        warnings,
    };
    let notices = load_notices(&shown);
    DocAnswer::Loaded {
        ticket,
        loaded: Box::new(Loaded::imported(shown.workspace, notices, devices)),
    }
}

/// `mapping` with the channels the device it plays on lacks left out: a QLab patch often has
/// more cue outputs than this Mac's output has channels, and the patch keeps the outputs the
/// device has (as the default mapping does) rather than the whole mapping being refused. A
/// device that is not listed leaves the mapping as it is, for the refusal to report.
fn fit_to_device(mut mapping: PatchMapping, devices: &[DeviceEntry]) -> PatchMapping {
    let device = if mapping.system {
        devices.iter().find(|d| d.is_default)
    } else {
        devices.iter().find(|d| d.name == mapping.device)
    };
    if let Some(d) = device {
        mapping.channels.retain(|&c| c < d.output_channels);
    }
    mapping
}

impl Control {
    /// Stores an imported workspace's device choices for doc `doc`, before its patches are
    /// first resolved. A mapping this machine cannot use now (its device unplugged since the
    /// choice) leaves the patch unmapped and is logged; so does a display not listed.
    pub(super) fn apply_imported_devices(&mut self, doc: DocId, devices: ImportedDevices) {
        let devices_listed = self.eng.device_list();
        let displays = self.eng.stages.displays.clone();
        let Some(d) = self.docs.iter().find(|d| d.id == doc) else {
            return;
        };
        let ws = d.model.ws();
        let workspace = ws.id;
        let mut problems = Vec::new();
        for (patch, mapping) in devices.patches {
            let outputs = ws
                .patches
                .iter()
                .find(|p| p.id == patch)
                .map_or(0, |p| p.outputs.len());
            let mapping = mapping.map(|m| fit_to_device(m, &devices_listed));
            let refusal = mapping
                .as_ref()
                .and_then(|m| super::mapping::mapping_refusal(m, outputs, &devices_listed));
            let machine = &mut self.eng.config.machine;
            match (mapping, refusal) {
                (Some(m), None) => {
                    if let Err(why) = machine.set_patch_mapping(workspace, patch, m) {
                        problems.push(why);
                        machine.clear_patch_mapping(workspace, patch);
                    }
                }
                (_, Some(ErrorBody { message, .. })) => {
                    problems.push(message);
                    machine.clear_patch_mapping(workspace, patch);
                }
                (None, None) => machine.clear_patch_mapping(workspace, patch),
            }
        }
        for (route, display) in devices.routes {
            let choice = display.and_then(|id| {
                let found = displays.iter().find(|d| d.id == id);
                if found.is_none() {
                    problems.push(format!("no display {id}"));
                }
                found.map(|d| openlab_core::DisplayChoice {
                    display: d.id.clone(),
                    name: d.name.clone(),
                    width: d.width,
                    height: d.height,
                })
            });
            if let Err(why) = self
                .eng
                .config
                .machine
                .set_route_display(workspace, route, choice)
            {
                problems.push(why.to_owned());
            }
        }
        self.eng.stages.mappings = self.eng.stages.mappings.wrapping_add(1);
        self.eng.save_machine();
        for why in problems {
            self.eng.log(
                LogLevel::Warn,
                format!("imported workspace: a device choice was not kept: {why}"),
            );
        }
    }
}
