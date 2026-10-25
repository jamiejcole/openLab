// SPDX-License-Identifier: GPL-3.0-or-later

//! `<app-support>/openLab/settings.json`: per-machine settings (WS-06, WS-07).

use std::fs;
use std::io;

use openlab_core::{
    LoadedSettings, MAX_SETTINGS_BYTES, MachineSettings, load_settings, save_settings,
};

use super::{AppDirs, PersistError, read_text, write_atomic};

/// Reads the settings. Never fails: a missing file gives the defaults silently; an unreadable
/// or invalid one gives the defaults (or what could be salvaged) plus warnings, and a copy of
/// the file is kept as `settings.json.corrupt` so saving does not destroy it.
pub fn load_machine_settings(dirs: &AppDirs) -> LoadedSettings {
    let path = dirs.settings_path();
    let loaded = match read_text(&path, MAX_SETTINGS_BYTES) {
        Ok(text) => load_settings(&text),
        Err(PersistError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return LoadedSettings {
                settings: MachineSettings::default(),
                warnings: Vec::new(),
            };
        }
        Err(e) => LoadedSettings {
            settings: MachineSettings::default(),
            warnings: vec![format!("{e}; using default settings")],
        },
    };
    if !loaded.warnings.is_empty() {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(".corrupt");
        let _ = fs::copy(&path, path.with_file_name(name));
    }
    loaded
}

pub fn save_machine_settings(
    dirs: &AppDirs,
    settings: &MachineSettings,
) -> Result<(), PersistError> {
    let path = dirs.settings_path();
    let text = save_settings(settings).map_err(|e| {
        PersistError::io("serialise", &path)(io::Error::new(io::ErrorKind::InvalidData, e))
    })?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(PersistError::io("create", dir))?;
    }
    write_atomic(&path, text.as_bytes())
}
