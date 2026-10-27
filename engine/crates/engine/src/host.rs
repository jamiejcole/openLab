// SPDX-License-Identifier: GPL-3.0-or-later

//! Runs a show on the default output device: the mixer on the device's audio thread, the
//! control loop on its own thread (ARCHITECTURE §3.1), until stdin closes.
//!
//! The stream is opened, closed and reopened on the device thread (`control::devices`), which
//! keeps it (a cpal stream is not `Send` on every platform); the control thread gets the mixer
//! controller and decides when (docs/control.md "Devices").

use std::io::Write as _;
use std::path::Path;
use std::thread;
use std::time::Duration;

use openlab_audio::{
    DeviceError, MAX_PATCHES, Mixer, MixerConfig, OpenRequest, default_output_device_id,
    output_devices,
};
use openlab_core::{MAX_SHOW_FILE_BYTES, load_show};

use crate::control::devices::{self, CpalDevices, StartError};
use crate::control::{
    Backend, Control, ControlConfig, ControlError, EpochClock, Inbox, OUTBOUND_CAPACITY,
    outbound_channel, runner,
};
use crate::diag::crash::{self, CrashConfig};
use crate::diag::run_marker::{self, RunInfo};
use crate::diag::{LogLimits, LogWriter, ShowLog, StatusConfig};
use crate::net::{NetError, OscConfig, ServerConfig, WsServer};
use crate::persist::{AppDirs, PersistError, load_machine_settings_checked, read_text};

/// Frames per device callback asked for: about 5 ms at 48 kHz.
const BUFFER_FRAMES: u32 = 256;
/// Rates tried, in order, when the device supports several.
const RATES: [u32; 3] = [48_000, 44_100, 96_000];

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("{0}")]
    Read(#[from] PersistError),
    #[error("{0}")]
    Load(#[from] openlab_core::LoadError),
    #[error("audio device: {0}")]
    Device(#[from] DeviceError),
    #[error("no audio output device")]
    NoDevice,
    #[error("{0}")]
    Open(#[from] StartError),
    #[error("mixer: {0}")]
    Mixer(openlab_audio::ConfigError),
    #[error("{0}")]
    Control(#[from] ControlError),
    #[error("network: {0}")]
    Net(#[from] NetError),
    #[error("could not start a thread: {0}")]
    Thread(std::io::Error),
    /// A crash, not a controlled end: the run marker stays for the next start to report.
    #[error("the control thread panicked; the show stopped")]
    ControlPanicked,
}

/// How long shutdown waits for the log writer to write what is queued.
const LOG_FLUSH_WAIT: Duration = Duration::from_secs(3);

/// Whether [`supervise`] installs the process-wide panic hook (tests do not).
#[derive(Clone, Copy, PartialEq, Eq)]
enum CrashHook {
    Install,
    #[cfg(test)]
    Skip,
}

/// Loads the show at `path` and plays it until stdin is closed.
pub fn run_show(path: &Path) -> Result<(), HostError> {
    supervise(AppDirs::system(), CrashHook::Install, |show_log| {
        run_show_with(path, show_log)
    })
}

/// The diagnostics around a run (`run`): the show log and its writer, the panic hook and the
/// run marker. A controlled end, with or without an error, removes the marker; a panic on the
/// control thread ([`HostError::ControlPanicked`]) is a crash and leaves it.
fn supervise(
    dirs: Option<AppDirs>,
    hook: CrashHook,
    run: impl FnOnce(ShowLog) -> Result<(), HostError>,
) -> Result<(), HostError> {
    // First, so a crash while starting is captured too. Without an app-support folder the log
    // keeps its in-memory ring only and there are no crash files (docs/logging.md).
    let mut show_log = ShowLog::default();
    let mut log_writer = None;
    let mut run_marker = None;
    if let Some(dirs) = dirs {
        let clock = EpochClock::system();
        // An unwritable log folder must not stop the show: the ring still works.
        if let Ok((sink, writer)) =
            LogWriter::start(dirs.logs_dir(), LogLimits::default(), clock.clone())
        {
            show_log = ShowLog::new(Some(sink));
            log_writer = Some(writer);
        }
        let now = clock.now_ms();
        if hook == CrashHook::Install {
            crash::install(CrashConfig {
                dir: dirs.crash_dir(),
                version: env!("CARGO_PKG_VERSION"),
                shared: show_log.shared(),
                clock,
            });
        }
        // After the log is up, so a leftover from an unclean exit reaches the file too.
        let me = RunInfo {
            pid: std::process::id(),
            started_ms: now,
            version: env!("CARGO_PKG_VERSION").to_owned(),
        };
        let (marker, previous) = run_marker::begin(
            &dirs.run_markers_dir(),
            &dirs.crash_dir(),
            &me,
            &show_log,
            now,
        );
        for run in &previous {
            // Also on the console, for whoever runs the engine by hand.
            let _ = writeln!(
                std::io::stderr().lock(),
                "openlab-engine: {}",
                run.message()
            );
        }
        run_marker = marker;
    }
    let result = run(show_log);
    // The control loop is gone, so its sink is: the writer writes what is queued and ends. A
    // disk that hangs must not hold the process up, so the wait is bounded.
    if let Some(writer) = log_writer {
        writer.finish(LOG_FLUSH_WAIT);
    }
    // A controlled end, with or without an error, is not an unclean exit. A marker that cannot
    // be removed is reported (wrongly) at the next start; nothing better to do with it here.
    // After a control-thread panic the marker is dropped instead: closed and unlocked, but
    // left in place, so the next start reports the crash (docs/handoff/decisions/m4-7b.md).
    if let Some(marker) = run_marker
        && !matches!(result, Err(HostError::ControlPanicked))
    {
        let _ = marker.finish();
    }
    result
}

/// Waits for the control thread. A panic there stopped the show: it is reported as
/// [`HostError::ControlPanicked`] (non-zero exit, marker kept), not as a clean end.
fn join_control(worker: thread::JoinHandle<()>) -> Result<(), HostError> {
    worker.join().map_err(|_| HostError::ControlPanicked)
}

fn run_show_with(path: &Path, show_log: ShowLog) -> Result<(), HostError> {
    let text = read_text(path, MAX_SHOW_FILE_BYTES)?;
    let ws = load_show(&text)?.workspace;
    let devices = output_devices()?;
    let default = default_output_device_id();
    let device = devices
        .iter()
        .find(|d| Some(&d.id) == default.as_ref())
        .or_else(|| devices.first())
        .ok_or(HostError::NoDevice)?;
    let rate = RATES
        .into_iter()
        .find(|&r| {
            device
                .configs
                .iter()
                .any(|c| (c.min_rate..=c.max_rate).contains(&r))
        })
        .or_else(|| device.configs.first().map(|c| c.max_rate))
        .ok_or(HostError::NoDevice)?;
    let outputs = usize::from(device.max_channels().clamp(1, 64));
    let mixer_config = MixerConfig {
        sample_rate: rate,
        output_channels: outputs,
        patches: MAX_PATCHES,
        ..MixerConfig::default()
    };
    let (ctl, mixer) = Mixer::new(mixer_config).map_err(HostError::Mixer)?;
    let request = OpenRequest {
        device_id: Some(device.id.clone()),
        buffer_frames: Some(BUFFER_FRAMES),
    };
    let (inbox_tx, inbox) = crossbeam_channel::unbounded();
    let device = devices::start(
        Box::new(CpalDevices),
        mixer,
        request,
        devices,
        inbox_tx.clone(),
        devices::system_clock(),
    )?;
    let block = device
        .info()
        .block_frames
        .map_or(mixer_config.max_block_frames as u64, u64::from);
    let mut config = ControlConfig::new(rate, block);
    config.show_path = Some(path.to_path_buf());
    config.mixer_patches = MAX_PATCHES;
    config.show_log = show_log;
    config.status = Some(StatusConfig::default());
    config.push_log = true;
    if let Some(dirs) = AppDirs::system() {
        let (loaded, writable) = load_machine_settings_checked(&dirs);
        config.machine = loaded.settings;
        config.machine_writable = writable;
        config.app_dirs = Some(dirs);
    }
    let (out_tx, outbound) = outbound_channel(OUTBOUND_CAPACITY);
    // Port and token are what M4.9 writes to `engine.json`.
    // OSC (UDP and TCP 53000) shares the runtime; if its port is taken it logs and stays off.
    let net_config = ServerConfig {
        osc: Some(OscConfig::default()),
        ..ServerConfig::default()
    };
    let server = WsServer::start(inbox_tx.clone(), outbound, net_config)?;
    // Runs until the parent closes stdin (or it is not connected) or a client says
    // `goodbye { shutdown }`, which ends the control loop. Process exit policy is M4.9's.
    // Started before the control loop so that a failure leaves nothing running: without it
    // the engine would outlive its parent.
    let stdin_inbox = inbox_tx.clone();
    thread::Builder::new()
        .name("openlab-stdin".into())
        .spawn(move || {
            let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
            let _ = stdin_inbox.send(Inbox::Shutdown);
        })
        .map_err(HostError::Thread)?;
    let control = Control::new(
        ws,
        ctl,
        Backend::Device(Box::new(device)),
        config,
        inbox_tx.clone(),
        out_tx,
    )?;
    let worker = thread::Builder::new()
        .name("openlab-control".into())
        .spawn(move || runner::run(control, &inbox))
        .map_err(HostError::Thread)?;
    // The control loop stops the show; dropping it ends the device thread, which drops the
    // stream (and the mixer) there.
    let joined = join_control(worker);
    server.stop();
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    fn markers(dirs: &AppDirs) -> Vec<String> {
        fs::read_dir(dirs.run_markers_dir())
            .map(|r| {
                r.filter_map(Result::ok)
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn mine() -> Vec<String> {
        vec![format!("running-{}.json", std::process::id())]
    }

    #[test]
    fn a_graceful_end_removes_the_run_marker() {
        let root = tempfile::tempdir().unwrap();
        let dirs = AppDirs::new(root.path());
        let result = supervise(Some(dirs.clone()), CrashHook::Skip, |_| {
            assert_eq!(markers(&dirs), mine(), "held while the show runs");
            Ok(())
        });
        assert!(result.is_ok());
        assert!(markers(&dirs).is_empty());
    }

    #[test]
    fn a_start_that_fails_with_an_error_removes_the_run_marker() {
        let root = tempfile::tempdir().unwrap();
        let dirs = AppDirs::new(root.path());
        let missing = root.path().join("no such show.openlab");
        let result = supervise(Some(dirs.clone()), CrashHook::Skip, |log| {
            run_show_with(&missing, log)
        });
        assert!(matches!(result, Err(HostError::Read(_))), "{result:?}");
        assert!(markers(&dirs).is_empty());
    }

    #[test]
    fn a_panicking_control_thread_is_an_unclean_exit_that_keeps_the_marker() {
        let root = tempfile::tempdir().unwrap();
        let dirs = AppDirs::new(root.path());
        let result = supervise(Some(dirs.clone()), CrashHook::Skip, |_| {
            let worker = thread::spawn(|| panic!("injected control-thread panic"));
            join_control(worker)
        });
        assert!(
            matches!(result, Err(HostError::ControlPanicked)),
            "{result:?}"
        );
        assert_eq!(markers(&dirs), mine());
        // The next start reports it (a crash, not a clean shutdown) and removes it.
        let me = RunInfo {
            pid: std::process::id(),
            started_ms: 0,
            version: "test".into(),
        };
        let (_next, previous) =
            run_marker::RunMarker::start(&dirs.run_markers_dir(), &dirs.crash_dir(), &me);
        let [run] = previous.as_slice() else {
            panic!("{previous:?}")
        };
        assert_eq!(run.info.as_ref().map(|i| i.pid), Some(std::process::id()));
    }

    #[test]
    fn a_clean_control_thread_joins_ok() {
        assert!(join_control(thread::spawn(|| {})).is_ok());
    }
}
