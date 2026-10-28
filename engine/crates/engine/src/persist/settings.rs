// SPDX-License-Identifier: GPL-3.0-or-later

//! `<app-support>/openLab/settings.json`: per-machine settings (WS-06, WS-07).

use std::fs;
use std::io;
use std::time::Duration;

use openlab_core::{
    LoadedSettings, MAX_SETTINGS_BYTES, MachineSettings, load_settings, save_settings,
};

use super::writer::DROP_WAIT;
use super::{AppDirs, PersistError, read_text, write_atomic};

/// Reads the settings. Never fails: a missing file gives the defaults silently; an unreadable
/// or invalid one gives the defaults (or what could be salvaged) plus warnings, and a copy of
/// the file is kept as `settings.json.corrupt` so saving does not destroy it.
pub fn load_machine_settings(dirs: &AppDirs) -> LoadedSettings {
    load_machine_settings_checked(dirs).0
}

/// [`load_machine_settings`], and whether it is safe to write the settings back: false when the
/// file could not be read (anything but "not there") or when it produced warnings and its
/// `.corrupt` copy could not be made, because the in-memory settings are then defaults standing
/// in for a file the operator still has.
pub fn load_machine_settings_checked(dirs: &AppDirs) -> (LoadedSettings, bool) {
    let path = dirs.settings_path();
    let (loaded, read_ok) = match read_text(&path, MAX_SETTINGS_BYTES) {
        Ok(text) => (load_settings(&text), true),
        Err(PersistError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return (
                LoadedSettings {
                    settings: MachineSettings::default(),
                    warnings: Vec::new(),
                },
                true,
            );
        }
        Err(e) => (
            LoadedSettings {
                settings: MachineSettings::default(),
                warnings: vec![format!("{e}; using default settings")],
            },
            false,
        ),
    };
    let mut writable = read_ok;
    if !loaded.warnings.is_empty() {
        // Copying a FIFO would block; only a regular file has anything to keep.
        let copied = super::files::regular_file_or_absent(&path).unwrap_or(false)
            && fs::copy(&path, super::unique_sibling(&path, "corrupt")).is_ok();
        writable = writable && copied;
    }
    (loaded, writable)
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

/// Writes machine settings off the control thread. Snapshots queue up and only the newest is
/// written; a failed write is reported through [`SettingsWriter::take_error`]. Dropping it
/// finishes the pending write first (the file is small and local), but waits at most
/// [`DROP_WAIT`], as the save writer does: a write stuck on a hung volume must not keep quit
/// (or a relaunch) waiting, so the thread is then left to end by itself.
pub struct SettingsWriter {
    tx: Option<crossbeam_channel::Sender<MachineSettings>>,
    errors: crossbeam_channel::Receiver<String>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Disconnected when the thread ends.
    ended: crossbeam_channel::Receiver<()>,
    drop_wait: Duration,
}

/// Called on the writer thread just before each write (tests).
type Hook = Box<dyn Fn() + Send>;

impl SettingsWriter {
    pub fn spawn(dirs: AppDirs) -> io::Result<Self> {
        Self::spawn_inner(dirs, None)
    }

    #[cfg(test)]
    fn spawn_with_hook(dirs: AppDirs, hook: impl Fn() + Send + 'static) -> io::Result<Self> {
        Self::spawn_inner(dirs, Some(Box::new(hook)))
    }

    fn spawn_inner(dirs: AppDirs, hook: Option<Hook>) -> io::Result<Self> {
        let (tx, rx) = crossbeam_channel::unbounded::<MachineSettings>();
        let (etx, errors) = crossbeam_channel::unbounded();
        let (ending, ended) = crossbeam_channel::bounded::<()>(0);
        let thread = std::thread::Builder::new()
            .name("openlab-settings".into())
            .spawn(move || {
                let _ending = ending;
                while let Ok(mut latest) = rx.recv() {
                    while let Ok(newer) = rx.try_recv() {
                        latest = newer;
                    }
                    if let Some(hook) = &hook {
                        hook();
                    }
                    if let Err(e) = save_machine_settings(&dirs, &latest) {
                        let _ = etx.send(e.to_string());
                    }
                }
            })?;
        Ok(Self {
            tx: Some(tx),
            errors,
            thread: Some(thread),
            ended,
            drop_wait: DROP_WAIT,
        })
    }

    /// Queues a write. False when the writer thread is gone (the settings were not saved).
    pub fn save(&self, settings: MachineSettings) -> bool {
        self.tx.as_ref().is_some_and(|tx| tx.send(settings).is_ok())
    }

    /// The next write failure, if any.
    pub fn take_error(&self) -> Option<String> {
        self.errors.try_recv().ok()
    }
}

impl Drop for SettingsWriter {
    fn drop(&mut self) {
        self.tx = None;
        let ended = matches!(
            self.ended.recv_timeout(self.drop_wait),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected)
        );
        if let Some(thread) = self.thread.take()
            && ended
        {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M7: a write stuck on a hung volume must not hold up the drop (quit) for ever.
    #[test]
    fn dropping_the_writer_does_not_wait_for_ever_on_a_stuck_write() {
        let root = tempfile::tempdir().unwrap();
        let (entered_tx, entered) = crossbeam_channel::bounded::<()>(1);
        let (release_tx, release) = crossbeam_channel::bounded::<()>(0);
        let mut writer = SettingsWriter::spawn_with_hook(AppDirs::new(root.path()), move || {
            let _ = entered_tx.send(());
            // Stuck until the test releases it (after the drop, or never if the drop hangs).
            let _ = release.recv();
        })
        .unwrap();
        writer.drop_wait = Duration::from_millis(50);
        assert!(writer.save(MachineSettings::default()));
        entered.recv().unwrap();
        let (dropped_tx, dropped) = crossbeam_channel::bounded::<()>(1);
        std::thread::spawn(move || {
            drop(writer);
            let _ = dropped_tx.send(());
        });
        // A hang guard, not a timing assertion: the drop waits 50 ms.
        let done = dropped.recv_timeout(Duration::from_secs(20));
        drop(release_tx);
        assert!(
            done.is_ok(),
            "dropping the settings writer hung on a stuck write"
        );
    }

    #[test]
    fn dropping_the_writer_finishes_the_pending_write() {
        let root = tempfile::tempdir().unwrap();
        let dirs = AppDirs::new(root.path());
        let writer = SettingsWriter::spawn(dirs.clone()).unwrap();
        assert!(writer.save(MachineSettings::default()));
        drop(writer);
        assert!(dirs.settings_path().exists());
    }
}
