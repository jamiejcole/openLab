// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

pub mod media;

use assert_no_alloc::{AllocDisabler, assert_no_alloc, violation_count};
use openlab_audio::{
    CommandKind, Mixer, MixerConfig, MixerController, OfflineRenderer, VoiceId, VoiceStart,
};
use rtrb::{Consumer, Producer, RingBuffer};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

/// Runs `f` with allocation forbidden on this thread; any (de)allocation fails the test.
pub fn no_alloc<T>(f: impl FnOnce() -> T) -> T {
    let before = violation_count();
    let r = assert_no_alloc(f);
    assert_eq!(
        violation_count(),
        before,
        "allocation on the real-time path"
    );
    r
}

pub fn config(outputs: usize) -> MixerConfig {
    MixerConfig {
        output_channels: outputs,
        max_block_frames: 256,
        max_voices: 16,
        ..MixerConfig::default()
    }
}

pub fn setup(config: MixerConfig) -> (MixerController, OfflineRenderer) {
    let (ctl, mixer) = Mixer::new(config).unwrap();
    (ctl, OfflineRenderer::new(mixer))
}

/// A finished source: all of `data` is queued and the producer dropped (end of stream).
pub fn source(data: &[f32]) -> Consumer<f32> {
    let (mut tx, rx) = RingBuffer::new(data.len().max(1));
    tx.push_entire_slice(data).unwrap();
    rx
}

/// A source whose producer is kept open (more data may follow; empty = underrun).
pub fn open_source(capacity: usize, data: &[f32]) -> (Producer<f32>, Consumer<f32>) {
    let (mut tx, rx) = RingBuffer::new(capacity);
    tx.push_entire_slice(data).unwrap();
    (tx, rx)
}

/// A voice start with identity routing (input n → cue output n, unity).
pub fn voice(ctl: &MixerController, id: u64, rx: Consumer<f32>, channels: u16) -> VoiceStart {
    let mut v = VoiceStart::new(VoiceId(id), rx, channels, ctl.cue_outputs());
    for i in 0..usize::from(channels) {
        v.set_crosspoint(i, i, 1.0);
    }
    v
}

pub fn send_start(ctl: &mut MixerController, at: u64, v: VoiceStart) {
    ctl.send(at, CommandKind::StartVoice(Box::new(v)));
}

/// Starts a voice with identity routing.
pub fn start(ctl: &mut MixerController, at: u64, id: u64, rx: Consumer<f32>, channels: u16) {
    let v = voice(ctl, id, rx, channels);
    send_start(ctl, at, v);
}

/// Channel `ch` of interleaved `buf`.
pub fn channel(buf: &[f32], channels: usize, ch: usize) -> Vec<f32> {
    buf.iter().skip(ch).step_by(channels).copied().collect()
}

pub fn first_nonzero(samples: &[f32]) -> Option<usize> {
    samples.iter().position(|s| *s != 0.0)
}

pub fn events(ctl: &mut MixerController) -> Vec<openlab_audio::Event> {
    std::iter::from_fn(|| ctl.poll_event()).collect()
}
