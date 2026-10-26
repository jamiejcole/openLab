// SPDX-License-Identifier: GPL-3.0-or-later

//! Runs a show on the default output device: the mixer on the device's audio thread
//! (`open_output`), the control loop on its own thread (ARCHITECTURE §3.1), until stdin closes.
//!
//! The device is fixed for the run: device lists, loss and reopen are M4.2b. The cpal stream
//! stays on the thread that opened it (it is not `Send` on every platform); the control thread
//! gets the mixer controller and the device monitor.

use std::path::Path;
use std::thread;

use openlab_audio::{
    DeviceError, MAX_PATCHES, Mixer, MixerConfig, OpenError, OpenRequest, default_output_device_id,
    open_output, output_devices,
};
use openlab_core::{DeviceInfo, MAX_SHOW_FILE_BYTES, load_show};

use crate::control::{Backend, Control, ControlConfig, ControlError, Inbox, runner};
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
    #[error("audio device: {0}")]
    Open(String),
    #[error("mixer: {0}")]
    Mixer(openlab_audio::ConfigError),
    #[error("{0}")]
    Control(#[from] ControlError),
    #[error("could not start the control thread: {0}")]
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
    let stream = open_output(mixer, &request)
        .map_err(|e: OpenError| HostError::Open(e.error.to_string()))?;
    let block = stream
        .latency()
        .buffer_frames
        .map_or(mixer_config.max_block_frames as u64, u64::from);
    let mut config = ControlConfig::new(rate, block);
    config.show_path = Some(path.to_path_buf());
    config.mixer_patches = MAX_PATCHES;
    config.devices = vec![DeviceInfo {
        name: device.name.clone(),
        output_channels: u32::try_from(outputs).unwrap_or(u32::MAX),
    }];
    if let Some(dirs) = AppDirs::system() {
        config.machine = load_machine_settings(&dirs).settings;
    }
    let (inbox_tx, inbox) = crossbeam_channel::unbounded();
    // No network layer yet (M4.3): nobody reads the broadcasts, so they are dropped.
    let (out_tx, _) = crossbeam_channel::bounded(1);
    let control = Control::new(
        ws,
        ctl,
        Backend::Device {
            monitor: stream.monitor(),
        },
        config,
        inbox_tx.clone(),
        out_tx,
    )?;
    let worker = thread::Builder::new()
        .name("openlab-control".into())
        .spawn(move || runner::run(control, &inbox))
        .map_err(HostError::Thread)?;
    // Runs until the parent closes stdin (or it is not connected).
    let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
    let _ = inbox_tx.send(Inbox::Shutdown);
    let _ = worker.join();
    let _ = stream.close();
    Ok(())
}
