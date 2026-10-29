// SPDX-License-Identifier: GPL-3.0-or-later
// Helpers outside #[test] fns are test code too; clippy's in-test allowances miss them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! A golden checksum of the mixer's output over a varied workload: the speed-ups of
//! the mix loop are meant to be bit-identical, and this pins that down across platforms. Only
//! Linear curves and plain linear gains are used: curved knots and `db_to_linear` go through the
//! platform's libm, which differs between operating systems in the last bit.

mod common;

use common::*;
use openlab_audio::{
    CommandKind, Curve, Fade, GainTarget, MixerController, OutputSettings, VoiceId,
};

/// FNV-1a (64 bit) over the bit patterns of every output sample and meter peak of the workload.
/// To regenerate after an intended change to the mixer's arithmetic: run
/// `cargo test -p openlab-audio --test golden`, which prints the new value on a mismatch, and
/// paste it here (say why in the commit message).
const GOLDEN: u64 = 0xd0b5_9ed1_43e7_e2be;

struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    fn f32s(&mut self, xs: &[f32]) {
        for x in xs {
            for b in x.to_bits().to_le_bytes() {
                self.0 ^= u64::from(b);
                self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }
}

/// Deterministic pseudo-random samples in ±1 (an LCG): no libm in the data either.
fn noise(n: usize, seed: u32) -> Vec<f32> {
    let mut s = seed;
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (s >> 8) as f32 / (1u32 << 23) as f32 - 1.0
        })
        .collect()
}

fn lin(frames: u64) -> Fade {
    Fade::new(frames, Curve::Linear)
}

fn output(delay_frames: u32, level: f32, invert: bool) -> OutputSettings {
    OutputSettings {
        level,
        invert,
        delay_frames,
        ..OutputSettings::default()
    }
}

fn ramp(ctl: &mut MixerController, at: u64, voice: u64, target: GainTarget, gain: f32, fade: Fade) {
    ctl.send(
        at,
        CommandKind::Ramp {
            voice: VoiceId(voice),
            target,
            gain,
            fade,
        },
    );
}

fn workload() -> u64 {
    const CH: usize = 3;
    const FRAMES: usize = 1500;
    let (mut ctl, mut r) = setup(config(CH));
    let mut hash = Fnv::new();

    // Voice 1: three channels through a ring whose capacity is not a whole number of frames, so
    // reads wrap mid-frame. It is stopped with a fade before its data runs out.
    let data = noise(FRAMES * CH, 1);
    let (mut tx, rx) = rtrb::RingBuffer::new(10);
    start(&mut ctl, 0, 1, rx, CH as u16);
    // Voice 2: mono, mixed into all three outputs from a later start.
    let mut v2 = voice(&ctl, 2, source(&noise(1200, 2)), 1);
    for out in 0..CH {
        v2.set_crosspoint(0, out, 0.5);
    }
    send_start(&mut ctl, 200, v2);

    let m = |input, output| GainTarget::Crosspoint { input, output };
    ramp(&mut ctl, 300, 1, m(0, 1), 0.5, lin(200));
    ramp(&mut ctl, 350, 1, GainTarget::CueOutput(2), 0.25, lin(150));
    ramp(&mut ctl, 500, 2, GainTarget::CueOutput(0), 0.0, lin(100));
    ramp(&mut ctl, 520, 1, GainTarget::Master, 0.75, lin(90));
    ramp(&mut ctl, 600, 1, m(2, 2), 0.5, Fade::NONE);
    ctl.send(
        400,
        CommandKind::SetOutput {
            output: 0,
            settings: output(40, 1.0, false),
        },
    );
    // A retarget while the first delay change is still crossfading.
    ctl.send(
        430,
        CommandKind::SetOutput {
            output: 0,
            settings: output(90, 0.5, false),
        },
    );
    ctl.send(
        700,
        CommandKind::SetOutput {
            output: 0,
            settings: output(0, 1.0, false),
        },
    );
    ctl.send(
        800,
        CommandKind::SetOutput {
            output: 2,
            settings: output(7, 0.5, true),
        },
    );
    ctl.send(
        900,
        CommandKind::StopVoice {
            voice: VoiceId(1),
            fade: lin(120),
        },
    );

    let (mut fed, mut rendered) = (0, 0);
    while rendered < FRAMES + 200 {
        let n = tx.slots().min(data.len() - fed);
        tx.push_entire_slice(&data[fed..fed + n]).unwrap();
        fed += n;
        // Fewer frames than the ring holds, so reads start at every offset in it.
        let frames = 1 + rendered % 3;
        hash.f32s(&r.render(frames, &[frames]));
        hash.f32s(&ctl.output_meters());
        for (_, peaks) in ctl.voice_meters() {
            hash.f32s(&peaks);
        }
        rendered += frames;
    }
    assert_eq!(ctl.underrun_frames(), 0);
    hash.0
}

#[test]
fn golden_checksum_of_a_varied_linear_workload() {
    let got = workload();
    assert_eq!(
        got, GOLDEN,
        "checksum is {got:#018x}; see GOLDEN's comment before changing it"
    );
}
