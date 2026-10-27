// SPDX-License-Identifier: GPL-3.0-or-later

//! Runs a show on the default output device: the mixer on the device's audio thread, the
//! control loop on its own thread (ARCHITECTURE §3.1), until stdin closes.
//!
//! The stream is opened, closed and reopened on the device thread (`control::devices`), which
//! keeps it (a cpal stream is not `Send` on every platform); the control thread gets the mixer
//! controller and decides when (docs/control.md "Devices").

use std::path::Path;
use std::thread;

use openlab_audio::{
    DeviceError, MAX_PATCHES, Mixer, MixerConfig, OpenRequest, default_output_device_id,
    output_devices,
};
use openlab_core::{MAX_SHOW_FILE_BYTES, load_show};

use crate::control::devices::{self, CpalDevices, StartError};
use crate::control::{
    Backend, Control, ControlConfig, ControlError, Inbox, OUTBOUND_CAPACITY, outbound_channel,
    runner,
};
use crate::net::{NetError, OscConfig, ServerConfig, WsServer};
use crate::persist::{AppDirs, PersistError, load_machine_settings, read_text};

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
}

/// Loads the show at `path` and plays it until stdin is closed.
pub fn run_show(path: &Path) -> Result<(), HostError> {
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
    if let Some(dirs) = AppDirs::system() {
        config.machine = load_machine_settings(&dirs).settings;
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
    let _ = worker.join();
    server.stop();
    Ok(())
}
