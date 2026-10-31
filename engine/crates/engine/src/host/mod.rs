// SPDX-License-Identifier: GPL-3.0-or-later

//! The engine process (ARCHITECTURE §1, §3.1, §8): the mixer on the default output device's
//! audio thread, the control loop on its own thread, the UI server and OSC on the network
//! runtime. It starts with no workspace open (M4.15g): a client opens what it wants (`new`,
//! `open`, which offers recovery, `open_recovery`); `--open` sends the same `open` for
//! development and tests. `engine.json` tells clients where to connect (M4.9).
//!
//! The run ends on `goodbye { shutdown: true }`, `SIGTERM`, `SIGINT` or `SIGHUP` (macOS), or,
//! with `--watch-stdin`, when stdin closes: the control loop stops the show and writes a
//! recovery file for every workspace with unsaved changes (bounded), then the server stops, the
//! log is flushed, the run marker goes and `engine.json` goes last, so a start meanwhile is
//! refused naming this engine. As soon as the shutdown begins (whichever of those started it)
//! the engine is "stopping" ([`Stopping`]): an app connection that ends is not relaunched, and
//! `engine.json` says `stopping: true`, which tells a client that finds it to wait for this
//! engine to exit instead of attaching to it. A signal while starting, a second one while stopping, or an exit
//! that overruns its bound ends the process at once ([`stop`]).
//!
//! The stream is opened, closed and reopened on the device thread (`control::devices`), which
//! keeps it (a cpal stream is not `Send` on every platform); the control thread gets the mixer
//! controller and decides when (docs/control.md "Devices").

pub mod engine_file;
mod instance_lock;
mod stop;

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use openlab_audio::{
    DeviceError, MAX_OUTPUT_CHANNELS, MAX_PATCHES, Mixer, MixerConfig, OpenRequest,
    default_output_device_id, output_devices,
};
use openlab_core::protocol::{
    ClientMessage, LogLevel, OkBody, Outcome, PROTOCOL_VERSION, Request, RequestId, ResultData,
    ServerMessage,
};

use crate::control::devices::{self, CpalDevices, DeviceLayer, SleepClock, StartError, WallClock};
#[cfg(feature = "test-support")]
use crate::control::virtual_device::{VIRTUAL_DEVICE_ID, VIRTUAL_SEED, VirtualDevice, VirtualMode};
use crate::control::{
    Backend, Control, ControlConfig, ControlError, EpochClock, Inbox, OUTBOUND_CAPACITY,
    outbound_channel, reply_channel, runner,
};
use crate::diag::crash::{self, CrashConfig};
use crate::diag::run_marker::{self, RunInfo};
use crate::diag::{LogLimits, LogWriter, ShowLog, StatusConfig};
use crate::net::{NetError, OscConfig, Relaunch, RelaunchFn, ServerConfig, Stopping, WsServer};
use crate::persist::{AppDirs, load_machine_settings_checked};
use crate::platform::{exit, power, relaunch, signals};
use engine_file::{EngineFile, EngineInfo};
use stop::Stop;

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
    /// One engine per user (M4.9): the app attaches to the one running. Its pid and port when
    /// known (one still starting has not written `engine.json` yet).
    #[error("another openLab engine is running ({})", other_engine(*.pid, *.port))]
    AlreadyRunning { pid: Option<u32>, port: Option<u16> },
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
    /// The app to relaunch (its bundle identifier) when its connection ends without `goodbye`
    /// (M4.9; the app passes its own). `None`: only log it (dev and tests).
    pub relaunch_app: Option<String>,
    /// Play on the virtual device instead of the host's (`--virtual-device`; test-support
    /// builds only, docs/control.md "Virtual device").
    #[cfg(feature = "test-support")]
    pub virtual_device: Option<VirtualMode>,
}

/// This run's `engine.json` once written: shared by the run, the thread that marks it stopping
/// ([`mark_on_stop`]) and its removal at the very end, which take it in turn.
type EngineSlot = Arc<Mutex<Option<EngineFile>>>;

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
    // Held until the process ends (a second engine is refused before it touches anything).
    let _instance = match &dirs {
        Some(dirs) => lock_instance(dirs)?,
        None => None,
    };
    // Next, so a start stuck in the audio system still answers a signal.
    let stop = Stop::new(dirs.as_ref().map(AppDirs::engine_file_path), |code| {
        exit::exit_now(code)
    });
    if signals {
        let on_signal = Arc::clone(&stop);
        signals::spawn_waiter(move |signal| on_signal.signal(signal)).map_err(HostError::Thread)?;
    }
    supervised(dirs.clone(), CrashHook::Install, |show_log, engine_file| {
        run_with(&options, dirs.as_ref(), &stop, show_log, engine_file)
    })
}

/// [`supervise`], then this run's `engine.json` (which `run` leaves in the slot) is removed,
/// last of all: after the log flush and the run marker, so a start until then is refused
/// naming this engine, not "starting". A crash leaves the file (stale: its run marker is no
/// longer held).
fn supervised(
    dirs: Option<AppDirs>,
    hook: CrashHook,
    run: impl FnOnce(ShowLog, &EngineSlot) -> Result<(), HostError>,
) -> Result<(), HostError> {
    let engine_file = EngineSlot::default();
    let result = supervise(dirs, hook, |show_log| run(show_log, &engine_file));
    // Taken out of the slot, so a mark that comes later finds nothing to write.
    let file = crate::lock(&engine_file).take();
    if !matches!(result, Err(HostError::ControlPanicked))
        && let Some(file) = file
    {
        let _ = file.remove();
    }
    result
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

/// The single-instance lock (`instance_lock`). Refused while another engine holds it, naming
/// that engine from `engine.json` when the file is its (or the lock file names no pid). A
/// folder or filesystem where the lock cannot be taken does not stop the start: the
/// `engine.json` check ([`check_single_instance`]) still runs, and a stderr line says so.
fn lock_instance(dirs: &AppDirs) -> Result<Option<instance_lock::InstanceLock>, HostError> {
    match instance_lock::acquire(&dirs.engine_lock_path()) {
        Ok(lock) => Ok(Some(lock)),
        Err(instance_lock::Refused::Held { pid }) => {
            let file = engine_file::read(&dirs.engine_file_path())
                .filter(|info| pid.is_none_or(|pid| pid == info.pid));
            Err(HostError::AlreadyRunning {
                pid: file.as_ref().map(|info| info.pid).or(pid),
                port: file.map(|info| info.port),
            })
        }
        Err(instance_lock::Refused::Unavailable(e)) => {
            say(&format!(
                "could not take the single-instance lock ({e}); relying on engine.json"
            ));
            Ok(None)
        }
    }
}

/// What [`HostError::AlreadyRunning`] says of the other engine.
fn other_engine(pid: Option<u32>, port: Option<u16>) -> String {
    match (pid, port) {
        (Some(pid), Some(port)) => format!("pid {pid}, port {port}"),
        (Some(pid), None) => format!("pid {pid}, starting or stopping"),
        (None, _) => "starting or stopping".to_owned(),
    }
}

/// Refuses to start while `engine.json` names a live engine (module docs).
fn check_single_instance(dirs: &AppDirs) -> Result<(), HostError> {
    match engine_file::live_engine(&dirs.engine_file_path(), &dirs.run_markers_dir()) {
        Some(other) => Err(HostError::AlreadyRunning {
            pid: Some(other.pid),
            port: Some(other.port),
        }),
        None => Ok(()),
    }
}

/// Puts what start-up found (an unavailable chosen device, settings the file had wrong) in the
/// show log as warnings, once the loop runs.
fn log_start_notes(inbox: &crossbeam_channel::Sender<Inbox>, notes: impl Iterator<Item = String>) {
    for message in notes {
        let _ = inbox.send(Inbox::Log {
            level: LogLevel::Warn,
            message,
        });
    }
}

/// The device the engine opens: the one the operator chose by name (`set_output_device`), else
/// the system default, else the first listed. A chosen device that is not listed (unplugged)
/// falls back with a note for the log, so the show still starts and the operator is told.
fn choose_device<'a>(
    devices: &'a [openlab_audio::DeviceInfo],
    default: Option<&str>,
    chosen: Option<&str>,
) -> Option<(&'a openlab_audio::DeviceInfo, Option<String>)> {
    let mut note = None;
    if let Some(name) = chosen {
        if let Some(d) = devices.iter().find(|d| d.name == name) {
            return Some((d, None));
        }
        note = Some(format!(
            "the chosen output device \"{name}\" is not available; using the default device"
        ));
    }
    devices
        .iter()
        .find(|d| Some(d.id.as_str()) == default)
        .or_else(|| devices.first())
        .map(|d| (d, note))
}

/// Where the engine's sound goes: the host's devices through cpal or, in a test-support build
/// started with `--virtual-device`, the virtual device, with the clocks its policy runs on.
struct Audio {
    layer: Box<dyn DeviceLayer>,
    devices: Vec<openlab_audio::DeviceInfo>,
    default: Option<String>,
    wall: WallClock,
    sleep: SleepClock,
    #[cfg(feature = "test-support")]
    virtual_device: Option<VirtualDevice>,
}

#[cfg_attr(not(feature = "test-support"), allow(unused_variables))]
fn audio(options: &Options) -> Result<Audio, HostError> {
    #[cfg(feature = "test-support")]
    if let Some(mode) = options.virtual_device {
        let vd = VirtualDevice::new(mode);
        return Ok(Audio {
            layer: vd.layer(),
            devices: vd.listed(),
            default: Some(VIRTUAL_DEVICE_ID.to_owned()),
            wall: vd.wall_clock(),
            sleep: vd.sleep_clock(),
            virtual_device: Some(vd),
        });
    }
    Ok(Audio {
        layer: Box::new(CpalDevices),
        devices: output_devices()?,
        default: default_output_device_id(),
        wall: devices::system_clock(),
        sleep: devices::system_sleep_clock(),
        #[cfg(feature = "test-support")]
        virtual_device: None,
    })
}

/// No CPU time in `status` (a stepped engine's messages must not depend on the machine).
#[cfg(feature = "test-support")]
fn no_cpu() -> Option<f64> {
    None
}

/// The run itself. This run's `engine.json`, once written, is left in `engine_file` for the
/// caller to remove last.
fn run_with(
    options: &Options,
    dirs: Option<&AppDirs>,
    stop: &Arc<Stop>,
    show_log: ShowLog,
    engine_file: &EngineSlot,
) -> Result<(), HostError> {
    if let Some(dirs) = dirs {
        check_single_instance(dirs)?;
    }
    let Audio {
        layer,
        devices,
        default,
        wall,
        sleep,
        #[cfg(feature = "test-support")]
        virtual_device,
    } = audio(options)?;
    let machine = dirs.map(load_machine_settings_checked);
    let (device, device_note) = choose_device(
        &devices,
        default.as_deref(),
        machine
            .as_ref()
            .and_then(|(loaded, _)| loaded.settings.output_device.as_deref()),
    )
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
    let outputs = usize::from(device.max_channels()).clamp(1, MAX_OUTPUT_CHANNELS);
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
    let device = devices::start(layer, mixer, request, devices, inbox_tx.clone(), wall)?
        .with_sleep_clock(sleep);
    let block = device
        .info()
        .block_frames
        .map_or(mixer_config.max_block_frames as u64, u64::from);
    let mut config = ControlConfig::new(rate, block);
    config.mixer_patches = MAX_PATCHES;
    config.show_log = show_log;
    config.status = Some(StatusConfig::default());
    config.push_log = true;
    #[cfg(feature = "test-support")]
    if let Some(vd) = virtual_device {
        say(&format!("audio: the virtual device ({:?})", vd.mode()));
        config.clock = vd.epoch_clock();
        config.seed = VIRTUAL_SEED;
        if vd.mode() == VirtualMode::Stepped {
            config.cpu_probe = no_cpu;
        }
        config.virtual_device = Some(vd.with_inbox(inbox.clone()));
    }
    // Not essential: without it the computer may sleep, which the log says. A test engine on
    // the virtual device never keeps the test machine awake.
    #[cfg(feature = "test-support")]
    let keep_awake = config.virtual_device.is_none();
    #[cfg(not(feature = "test-support"))]
    let keep_awake = true;
    if keep_awake {
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
    }
    log_start_notes(&inbox_tx, device_note.into_iter());
    if let (Some(dirs), Some((loaded, writable))) = (dirs, machine) {
        log_start_notes(&inbox_tx, loaded.warnings.iter().cloned());
        config.machine = loaded.settings;
        config.machine_writable = writable;
        config.app_dirs = Some(dirs.clone());
    }
    let (out_tx, outbound) = outbound_channel(OUTBOUND_CAPACITY);
    // OSC (UDP and TCP 53000) shares the runtime; if its port is taken it logs and stays off.
    let relaunch = options.relaunch_app.clone().map(|id| {
        let action: RelaunchFn = std::sync::Arc::new(move || relaunch::launch(&id));
        Relaunch::new(action)
    });
    let (stopping, stopping_rx) = Stopping::new();
    // On the virtual device the OSC listeners stay off: test engines run side by side, and
    // whichever got port 53000 first would log differently from the others.
    #[cfg(feature = "test-support")]
    let osc = config.virtual_device.is_none().then(OscConfig::default);
    #[cfg(not(feature = "test-support"))]
    let osc = Some(OscConfig::default());
    let net_config = ServerConfig {
        osc,
        relaunch,
        stopping: stopping.clone(),
        ..ServerConfig::default()
    };
    let server = WsServer::start(inbox_tx.clone(), outbound, net_config)?;
    let control = Control::empty(
        ctl,
        Backend::Device(Box::new(device)),
        config,
        inbox_tx.clone(),
        out_tx,
    )?;
    open_at_start(&options.open, &inbox_tx)?;
    // From here a signal is a graceful shutdown through the loop.
    stop.running(inbox_tx.clone(), stopping.clone());
    if options.watch_stdin {
        let (stdin_inbox, stdin_stopping) = (inbox_tx.clone(), stopping.clone());
        thread::Builder::new()
            .name("openlab-stdin".into())
            .spawn(move || {
                shutdown_at_eof(std::io::stdin().lock(), &stdin_inbox, &stdin_stopping);
            })
            .map_err(HostError::Thread)?;
    }
    // Last before the loop: a client that finds the file finds an engine about to serve it.
    let written = match dirs {
        Some(dirs) => {
            let path = dirs.engine_file_path();
            let info = EngineInfo {
                pid: std::process::id(),
                port: server.port(),
                token: server.token().to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
                protocol_version: PROTOCOL_VERSION,
                started_ms: EpochClock::system().now_ms(),
                // A signal may have come already; `mark_on_stop` covers one from now on.
                stopping: stopping.is_set(),
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
    *crate::lock(engine_file) = written;
    // Not essential: without it a client finds the file as it was, not marked stopping.
    let (marked_rx, marked_slot) = (stopping_rx, Arc::clone(engine_file));
    if let Err(e) = thread::Builder::new()
        .name("openlab-stopping".into())
        .spawn(move || mark_on_stop(&marked_rx, &marked_slot))
    {
        say(&format!(
            "could not start a thread ({e}); engine.json is not marked stopping at shutdown"
        ));
    }
    say(&format!("listening on 127.0.0.1:{}", server.port()));
    let worker = thread::Builder::new()
        .name("openlab-control".into())
        .spawn(move || runner::run(control, &inbox))
        .map_err(HostError::Thread)?;
    // The control loop stops the show and writes the recovery files; dropping it ends the
    // device thread, which drops the stream (and the mixer) there.
    let joined = join_control(worker);
    // However the loop ended (a panic included), the engine is stopping now.
    stopping.begin();
    stop.loop_ended();
    server.stop();
    joined
}

/// `--watch-stdin`: reads `input` to its end (the parent closed it, or ended), then shuts the
/// engine down, saying first that the shutdown has begun.
fn shutdown_at_eof(
    mut input: impl std::io::Read,
    inbox: &crossbeam_channel::Sender<Inbox>,
    stopping: &Stopping,
) {
    let _ = std::io::copy(&mut input, &mut std::io::sink());
    stopping.begin();
    let _ = inbox.send(Inbox::Shutdown);
}

/// Waits for the shutdown to begin, then marks this run's `engine.json` stopping, unless the
/// run has already taken it out of `slot` (removed, or about to be) or the flag went without
/// being set. On a thread of its own, so the control loop and its final recovery writes never
/// wait for it (the exit deadline only starts once the loop has ended). It holds `slot` while
/// it writes: on a hung disk the final removal waits behind it until the forced exit at the
/// deadline, and a file left then names a dead pid, which the next start treats as stale.
fn mark_on_stop(stopping: &crossbeam_channel::Receiver<()>, slot: &EngineSlot) {
    if stopping.recv().is_err() {
        return;
    }
    if let Some(file) = crate::lock(slot).as_mut()
        && let Err(e) = file.mark_stopping()
    {
        say(&format!("could not mark engine.json as stopping: {e}"));
    }
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

    fn info(id: &str, name: &str) -> openlab_audio::DeviceInfo {
        openlab_audio::DeviceInfo {
            id: id.into(),
            name: name.into(),
            is_default: false,
            configs: Vec::new(),
        }
    }

    #[test]
    fn start_notes_reach_the_inbox_as_warnings() {
        let (tx, rx) = crossbeam_channel::unbounded();
        log_start_notes(&tx, ["a".to_owned(), "b".to_owned()].into_iter());
        let got: Vec<_> = rx.try_iter().collect();
        assert_eq!(got.len(), 2);
        assert!(matches!(
            &got[0],
            Inbox::Log { level: LogLevel::Warn, message } if message == "a"
        ));
    }

    #[test]
    fn the_chosen_device_opens_and_a_missing_one_falls_back_with_a_note() {
        let devices = [info("a", "Built-in"), info("b", "Scarlett 2i2")];
        let (chosen, note) = choose_device(&devices, Some("a"), Some("Scarlett 2i2")).unwrap();
        assert_eq!((chosen.id.as_str(), note), ("b", None));
        let (fallback, note) = choose_device(&devices, Some("a"), Some("Gone")).unwrap();
        assert_eq!(fallback.id, "a");
        assert!(note.unwrap().contains("Gone"));
        let (default, note) = choose_device(&devices, Some("b"), None).unwrap();
        assert_eq!((default.id.as_str(), note), ("b", None));
        assert!(choose_device(&[], None, Some("x")).is_none());
    }

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
            stopping: false,
        };
        let _file = EngineFile::write(&dirs.engine_file_path(), info.clone()).unwrap();
        let stop = Stop::new(None, |_| {});
        let result = supervised(Some(dirs.clone()), CrashHook::Skip, |log, file| {
            run_with(&Options::default(), Some(&dirs), &stop, log, file)
        });
        assert!(
            matches!(
                result,
                Err(HostError::AlreadyRunning {
                    pid: Some(4_000_000),
                    port: Some(50_000)
                })
            ),
            "{result:?}"
        );
        assert_eq!(markers(&dirs), vec!["running-4000000.json".to_owned()]);
        assert_eq!(engine_file::read(&dirs.engine_file_path()), Some(info));
        held.unwrap().finish().unwrap();
    }

    /// M8: the lock is taken before anything else, so a second start is refused even while
    /// the first is still starting (no `engine.json` yet), and named once the file is there.
    #[test]
    fn a_second_start_is_refused_by_the_instance_lock() {
        let _serial = crate::diag::run_marker::tests::serialize_spawn_and_lock_release();
        let root = tempfile::tempdir().unwrap();
        let dirs = AppDirs::new(root.path());
        let first = lock_instance(&dirs).unwrap();
        assert!(first.is_some());
        let refused = lock_instance(&dirs).unwrap_err();
        let me = std::process::id();
        if cfg!(unix) {
            assert!(
                matches!(refused, HostError::AlreadyRunning { pid: Some(p), port: None } if p == me),
                "{refused:?}"
            );
        }
        assert!(
            refused
                .to_string()
                .starts_with("another openLab engine is running ("),
            "{refused}"
        );
        let info = EngineInfo {
            pid: me,
            port: 50_001,
            token: "t".into(),
            version: "0.0.1".into(),
            protocol_version: PROTOCOL_VERSION,
            started_ms: 0,
            stopping: false,
        };
        let _file = EngineFile::write(&dirs.engine_file_path(), info).unwrap();
        let refused = lock_instance(&dirs).unwrap_err();
        assert_eq!(
            refused.to_string(),
            format!("another openLab engine is running (pid {me}, port 50001)")
        );
        // Nothing of the running engine's was touched.
        assert!(markers(&dirs).is_empty());
        drop(first);
        assert!(lock_instance(&dirs).unwrap().is_some());
    }

    /// `engine.json` outlives the run marker and the log flush: a start while this engine is
    /// still stopping is refused naming it (not "starting"), and the app's supervisor sees an
    /// engine to wait for instead of spawning into the lock. A crash leaves it.
    #[test]
    fn engine_json_is_removed_last_and_left_by_a_crash() {
        let root = tempfile::tempdir().unwrap();
        let dirs = AppDirs::new(root.path());
        let info = EngineInfo {
            pid: std::process::id(),
            port: 50_002,
            token: "t".into(),
            version: "0.0.1".into(),
            protocol_version: PROTOCOL_VERSION,
            started_ms: 0,
            stopping: false,
        };
        let path = dirs.engine_file_path();
        let write = |slot: &EngineSlot| {
            *crate::lock(slot) = Some(EngineFile::write(&path, info.clone()).unwrap());
        };
        let result = supervised(Some(dirs.clone()), CrashHook::Skip, |_, slot| {
            write(slot);
            Ok(())
        });
        assert!(result.is_ok());
        assert!(!path.exists());
        assert!(markers(&dirs).is_empty());

        let result = supervised(Some(dirs.clone()), CrashHook::Skip, |_, slot| {
            write(slot);
            Err(HostError::NoDevice)
        });
        assert!(result.is_err());
        assert!(!path.exists(), "a failed start removes it too");

        let result = supervised(Some(dirs.clone()), CrashHook::Skip, |_, slot| {
            write(slot);
            Err(HostError::ControlPanicked)
        });
        assert!(matches!(result, Err(HostError::ControlPanicked)));
        assert!(path.exists(), "a crash leaves engine.json");
    }

    /// Stdin closing with `--watch-stdin` begins the shutdown: the engine is stopping (no app
    /// relaunch, `engine.json` marked) before the loop is asked to end.
    #[test]
    fn stdin_closing_says_stopping_and_asks_the_loop_to_end() {
        let (stopping, told) = Stopping::new();
        let (inbox_tx, inbox) = crossbeam_channel::unbounded();
        shutdown_at_eof(std::io::empty(), &inbox_tx, &stopping);
        assert!(stopping.is_set());
        assert!(told.try_recv().is_ok());
        assert!(matches!(inbox.try_recv(), Ok(Inbox::Shutdown)));
    }

    /// As soon as the shutdown begins `engine.json` says `stopping: true` (the app then waits for
    /// this engine to exit instead of attaching to it); once the run has taken the file out to
    /// remove it, a late mark writes nothing back; a flag dropped unset marks nothing.
    #[test]
    fn engine_json_is_marked_stopping_when_the_shutdown_begins_and_never_brought_back() {
        let root = tempfile::tempdir().unwrap();
        let dirs = AppDirs::new(root.path());
        let path = dirs.engine_file_path();
        let info = EngineInfo {
            pid: std::process::id(),
            port: 50_003,
            token: "t".into(),
            version: "0.0.1".into(),
            protocol_version: PROTOCOL_VERSION,
            started_ms: 0,
            stopping: false,
        };
        let slot = EngineSlot::default();
        *crate::lock(&slot) = Some(EngineFile::write(&path, info.clone()).unwrap());
        let (stopping, told) = Stopping::new();
        stopping.begin();
        mark_on_stop(&told, &slot);
        assert_eq!(
            engine_file::read(&path).map(|i| i.stopping),
            Some(true),
            "marked"
        );
        // Still this run's to remove at the end.
        if let Some(file) = crate::lock(&slot).take() {
            file.remove().unwrap();
        }
        assert!(!path.exists());

        // A mark that comes after the removal.
        let (stopping, told) = Stopping::new();
        stopping.begin();
        mark_on_stop(&told, &slot);
        assert!(!path.exists(), "brought back");

        // A run that never began to stop (its flag gone unset) leaves the file as written.
        *crate::lock(&slot) = Some(EngineFile::write(&path, info.clone()).unwrap());
        let (stopping, told) = Stopping::new();
        drop(stopping);
        mark_on_stop(&told, &slot);
        assert_eq!(engine_file::read(&path), Some(info));
    }

    #[test]
    fn a_refusal_without_engine_json_says_starting_or_stopping() {
        assert_eq!(
            HostError::AlreadyRunning {
                pid: Some(7),
                port: None
            }
            .to_string(),
            "another openLab engine is running (pid 7, starting or stopping)"
        );
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
        // An autosave newer than the file: nothing is opened, and the line says why.
        let offer = Outcome::Ok(OkBody {
            revision: None,
            data: Some(ResultData::RecoveryOffer {
                path: "/s.openlab".into(),
                workspace_name: "Show".into(),
                modified_ms: 5,
            }),
        });
        assert_eq!(
            open_outcome("/s.openlab", &offer),
            "not opened /s.openlab: an autosave newer than the file exists; open it from the app to restore or ignore it"
        );
        // Any other success body is still reported as opened.
        let bare = Outcome::Ok(OkBody {
            revision: None,
            data: None,
        });
        assert_eq!(open_outcome("/s.openlab", &bare), "opened /s.openlab");
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
