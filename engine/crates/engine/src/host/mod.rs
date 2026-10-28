// SPDX-License-Identifier: GPL-3.0-or-later

//! The engine process (ARCHITECTURE §1, §3.1, §8): the mixer on the default output device's
//! audio thread, the control loop on its own thread, the UI server and OSC on the network
//! runtime. It starts with no workspace open (M4.15g): a client opens what it wants (`new`,
//! `open`, which offers recovery, `open_recovery`); `--open` sends the same `open` for
//! development and tests. `engine.json` tells clients where to connect (M4.9).
//!
//! The run ends on `goodbye { shutdown: true }`, `SIGTERM`, `SIGINT` or `SIGHUP` (macOS), or,
//! with `--watch-stdin`, when stdin closes: the control loop stops the show and writes a
//! recovery file for every workspace with unsaved changes (bounded), then the server stops,
//! `engine.json` and the run marker go and the log is flushed.
//!
//! The stream is opened, closed and reopened on the device thread (`control::devices`), which
//! keeps it (a cpal stream is not `Send` on every platform); the control thread gets the mixer
//! controller and decides when (docs/control.md "Devices").

pub mod engine_file;

use std::io::Write as _;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use openlab_audio::{
    DeviceError, MAX_PATCHES, Mixer, MixerConfig, OpenRequest, default_output_device_id,
    output_devices,
};
use openlab_core::protocol::{
    ClientMessage, LogLevel, OkBody, Outcome, PROTOCOL_VERSION, Request, RequestId, ResultData,
    ServerMessage,
};

use crate::control::devices::{self, CpalDevices, StartError};
use crate::control::{
    Backend, Control, ControlConfig, ControlError, EpochClock, Inbox, OUTBOUND_CAPACITY,
    outbound_channel, reply_channel, runner,
};
use crate::diag::crash::{self, CrashConfig};
use crate::diag::run_marker::{self, RunInfo};
use crate::diag::{LogLimits, LogWriter, ShowLog, StatusConfig};
use crate::net::{NetError, OscConfig, ServerConfig, WsServer};
use crate::persist::{AppDirs, load_machine_settings_checked};
use crate::platform::{power, signals};
use engine_file::{EngineFile, EngineInfo};

/// Frames per device callback asked for: about 5 ms at 48 kHz.
const BUFFER_FRAMES: u32 = 256;
/// Rates tried, in order, when the device supports several.
const RATES: [u32; 3] = [48_000, 44_100, 96_000];

#[derive(Debug, thiserror::Error)]
pub enum HostError {
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
    /// One engine per user (M4.9): the app attaches to the one running.
    #[error("another openLab engine is running (pid {pid}, port {port})")]
    AlreadyRunning { pid: u32, port: u16 },
    /// Without it no client can find the engine.
    #[error("could not write {path}: {error}")]
    EngineFile { path: String, error: std::io::Error },
    /// A crash, not a controlled end: the run marker stays for the next start to report.
    #[error("the control thread panicked; the show stopped")]
    ControlPanicked,
}

/// How the engine was started (`main.rs` parses the command line).
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Shows to open at start, through the same `open` request a client sends (dev and tests).
    pub open: Vec<PathBuf>,
    /// The application-support folder (`<it>/openLab/...`); `None`: the user's.
    pub app_support: Option<PathBuf>,
    /// Shut down when stdin closes (a parent that ends takes the engine with it: tests).
    pub watch_stdin: bool,
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

/// Runs the engine until it is told to shut down (module docs).
pub fn run(options: Options) -> Result<(), HostError> {
    // First of all, before any thread starts, so every thread inherits the mask.
    let signals = signals::block();
    let dirs = match &options.app_support {
        Some(dir) => Some(AppDirs::new(dir)),
        None => AppDirs::system(),
    };
    supervise(dirs.clone(), CrashHook::Install, |show_log| {
        run_with(&options, dirs.as_ref(), signals, show_log)
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

/// Refuses to start while `engine.json` names a live engine (module docs).
fn check_single_instance(dirs: &AppDirs) -> Result<(), HostError> {
    match engine_file::live_engine(&dirs.engine_file_path(), &dirs.run_markers_dir()) {
        Some(other) => Err(HostError::AlreadyRunning {
            pid: other.pid,
            port: other.port,
        }),
        None => Ok(()),
    }
}

fn run_with(
    options: &Options,
    dirs: Option<&AppDirs>,
    signals: bool,
    show_log: ShowLog,
) -> Result<(), HostError> {
    if let Some(dirs) = dirs {
        check_single_instance(dirs)?;
    }
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
    config.mixer_patches = MAX_PATCHES;
    config.show_log = show_log;
    config.status = Some(StatusConfig::default());
    config.push_log = true;
    // Not essential: without it the computer may sleep, which the log says.
    match power::spawn(inbox_tx.clone()) {
        Ok(awake) => config.awake = Some(awake),
        Err(e) => {
            let _ = inbox_tx.send(Inbox::Log {
                level: LogLevel::Warn,
                message: format!(
                    "could not start sleep prevention ({e}); the computer may sleep during the show"
                ),
            });
        }
    }
    if let Some(dirs) = dirs {
        let (loaded, writable) = load_machine_settings_checked(dirs);
        config.machine = loaded.settings;
        config.machine_writable = writable;
        config.app_dirs = Some(dirs.clone());
    }
    let (out_tx, outbound) = outbound_channel(OUTBOUND_CAPACITY);
    // OSC (UDP and TCP 53000) shares the runtime; if its port is taken it logs and stays off.
    let net_config = ServerConfig {
        osc: Some(OscConfig::default()),
        ..ServerConfig::default()
    };
    let server = WsServer::start(inbox_tx.clone(), outbound, net_config)?;
    // Before the loop starts: a failure leaves nothing running.
    let engine_file = match dirs {
        Some(dirs) => {
            let path = dirs.engine_file_path();
            let info = EngineInfo {
                pid: std::process::id(),
                port: server.port(),
                token: server.token().to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
                protocol_version: PROTOCOL_VERSION,
                started_ms: EpochClock::system().now_ms(),
            };
            Some(
                EngineFile::write(&path, info).map_err(|error| HostError::EngineFile {
                    path: path.display().to_string(),
                    error,
                })?,
            )
        }
        None => {
            say(
                "no application-support folder: engine.json is not written, no client can find this engine",
            );
            None
        }
    };
    if signals {
        let signal_inbox = inbox_tx.clone();
        signals::spawn_waiter(move |signal| {
            let _ = signal_inbox.send(Inbox::Log {
                level: LogLevel::Info,
                message: format!("{}: shutting down", signal.name()),
            });
            let _ = signal_inbox.send(Inbox::Shutdown);
        })
        .map_err(HostError::Thread)?;
    }
    if options.watch_stdin {
        let stdin_inbox = inbox_tx.clone();
        thread::Builder::new()
            .name("openlab-stdin".into())
            .spawn(move || {
                let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
                let _ = stdin_inbox.send(Inbox::Shutdown);
            })
            .map_err(HostError::Thread)?;
    }
    let control = Control::empty(
        ctl,
        Backend::Device(Box::new(device)),
        config,
        inbox_tx.clone(),
        out_tx,
    )?;
    open_at_start(&options.open, &inbox_tx)?;
    say(&format!("listening on 127.0.0.1:{}", server.port()));
    let worker = thread::Builder::new()
        .name("openlab-control".into())
        .spawn(move || runner::run(control, &inbox))
        .map_err(HostError::Thread)?;
    // The control loop stops the show and writes the recovery files; dropping it ends the
    // device thread, which drops the stream (and the mixer) there.
    let joined = join_control(worker);
    server.stop();
    // A crash leaves the file (stale: its run marker is no longer held).
    if joined.is_ok()
        && let Some(file) = engine_file
    {
        let _ = file.remove();
    }
    joined
}

/// `--open`: the same app-scope `open` a client sends (recovery offered, not chosen), queued
/// before the loop runs. A thread prints each outcome; an open that fails or is answered with
/// a recovery offer leaves the engine running with what did open, as the app's would.
fn open_at_start(
    paths: &[PathBuf],
    inbox: &crossbeam_channel::Sender<Inbox>,
) -> Result<(), HostError> {
    if paths.is_empty() {
        return Ok(());
    }
    let (reply, mut replies) = reply_channel();
    let mut asked = Vec::new();
    for (n, path) in paths.iter().enumerate() {
        let id = RequestId(n as u64);
        // The protocol takes absolute paths only.
        let path = std::path::absolute(path).unwrap_or_else(|_| path.clone());
        asked.push(path.display().to_string());
        let _ = inbox.send(Inbox::Conn {
            doc: None,
            message: ClientMessage::Request {
                id,
                request: Request::Open {
                    path: path.to_string_lossy().into_owned(),
                    recovery: None,
                },
            },
            reply: reply.clone(),
        });
    }
    drop(reply);
    thread::Builder::new()
        .name("openlab-open".into())
        .spawn(move || {
            while let Some(message) = replies.blocking_recv() {
                let ServerMessage::Result { id, outcome } = message else {
                    continue;
                };
                let path = usize::try_from(id.0)
                    .ok()
                    .and_then(|n| asked.get(n))
                    .map_or("?", String::as_str);
                say(&open_outcome(path, &outcome));
            }
        })
        .map(|_| ())
        .map_err(HostError::Thread)
}

/// What `--open` prints for one outcome.
fn open_outcome(path: &str, outcome: &Outcome) -> String {
    match outcome {
        Outcome::Ok(OkBody {
            data: Some(ResultData::Opened { doc }),
            ..
        }) => format!("opened {path} (doc {})", doc.0),
        Outcome::Ok(OkBody {
            data: Some(ResultData::RecoveryOffer { .. }),
            ..
        }) => format!(
            "not opened {path}: an autosave newer than the file exists; open it from the app to restore or ignore it"
        ),
        Outcome::Ok(_) => format!("opened {path}"),
        Outcome::Error(e) => format!("could not open {path}: {}", e.message),
    }
}

/// A line on stderr, for whoever runs the engine by hand.
fn say(text: &str) {
    let _ = writeln!(std::io::stderr().lock(), "openlab-engine: {text}");
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

    /// M4.9: one engine per user. A start while `engine.json` names a live engine fails (and
    /// is a controlled end: its own marker goes) and leaves that engine's file alone.
    #[test]
    fn a_start_while_another_engine_runs_is_refused_and_leaves_its_file() {
        let root = tempfile::tempdir().unwrap();
        let dirs = AppDirs::new(root.path());
        let other = RunInfo {
            pid: 4_000_000,
            started_ms: 0,
            version: "0.0.1".into(),
        };
        // A second open in this process stands for the other engine (the lock is per file).
        let (held, _) =
            run_marker::RunMarker::start(&dirs.run_markers_dir(), &dirs.crash_dir(), &other);
        let info = EngineInfo {
            pid: other.pid,
            port: 50_000,
            token: "t".into(),
            version: "0.0.1".into(),
            protocol_version: PROTOCOL_VERSION,
            started_ms: 0,
        };
        let _file = EngineFile::write(&dirs.engine_file_path(), info.clone()).unwrap();
        let result = supervise(Some(dirs.clone()), CrashHook::Skip, |log| {
            run_with(&Options::default(), Some(&dirs), false, log)
        });
        assert!(
            matches!(
                result,
                Err(HostError::AlreadyRunning {
                    pid: 4_000_000,
                    port: 50_000
                })
            ),
            "{result:?}"
        );
        assert_eq!(markers(&dirs), vec!["running-4000000.json".to_owned()]);
        assert_eq!(engine_file::read(&dirs.engine_file_path()), Some(info));
        held.unwrap().finish().unwrap();
    }

    #[test]
    fn open_outcomes_are_said_plainly() {
        let opened = Outcome::Ok(OkBody {
            revision: None,
            data: Some(ResultData::Opened {
                doc: crate::control::DocId(3),
            }),
        });
        assert_eq!(
            open_outcome("/s.openlab", &opened),
            "opened /s.openlab (doc 3)"
        );
        let failed = Outcome::Error(openlab_core::protocol::ErrorBody::new(
            openlab_core::protocol::ErrorCode::Io,
            "no such file",
        ));
        assert_eq!(
            open_outcome("/s.openlab", &failed),
            "could not open /s.openlab: no such file"
        );
    }

    #[test]
    fn a_panicking_control_thread_is_an_unclean_exit_that_keeps_the_marker() {
        let _serial = crate::diag::run_marker::tests::serialize_spawn_and_lock_release();
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
