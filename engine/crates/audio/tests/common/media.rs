// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    dead_code
)]

//! Test media: tiny WAV/AIFF writers, test signals and a throttled reader.

use std::f64::consts::TAU;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

/// The reference stereo tone the compressed fixtures were made from (see fixtures/generate.sh):
/// left 440 Hz at 0.5, right 1000 Hz at 0.25.
pub fn tone(frames: usize, rate: u32) -> Vec<f32> {
    (0..frames)
        .flat_map(|i| {
            let t = i as f64 / f64::from(rate);
            [
                (0.5 * (TAU * 440.0 * t).sin()) as f32,
                (0.25 * (TAU * 1000.0 * t).sin()) as f32,
            ]
        })
        .collect()
}

pub fn sine(frames: usize, rate: u32, freq: f64, amp: f64) -> Vec<f32> {
    (0..frames)
        .map(|i| (amp * (TAU * freq * i as f64 / f64::from(rate)).sin()) as f32)
        .collect()
}

/// Distinct, exactly representable value for every frame index: identifies positions in output.
pub fn marker(i: u64) -> f32 {
    ((i % 60_000) + 1) as f32 / 65_536.0
}

pub fn markers(frames: u64) -> Vec<f32> {
    (0..frames).map(marker).collect()
}

#[derive(Clone, Copy, Debug)]
pub enum Pcm {
    I16,
    I24,
    I32,
    F32,
}

impl Pcm {
    pub fn bytes(self) -> usize {
        match self {
            Pcm::I16 => 2,
            Pcm::I24 => 3,
            Pcm::I32 | Pcm::F32 => 4,
        }
    }

    /// Largest decode error expected for this format.
    pub fn lsb(self) -> f32 {
        match self {
            Pcm::I16 => 1.0 / 32768.0,
            Pcm::I24 => 1.0 / 8_388_608.0,
            Pcm::I32 => 1.0 / 2_147_483_648.0 * 256.0,
            Pcm::F32 => 0.0,
        }
    }

    fn int(self, x: f32) -> i64 {
        let full = match self {
            Pcm::I16 => 32767.0,
            Pcm::I24 => 8_388_607.0,
            _ => 2_147_483_647.0,
        };
        (f64::from(x.clamp(-1.0, 1.0)) * full).round() as i64
    }

    fn put(self, out: &mut Vec<u8>, x: f32, big_endian: bool) {
        let mut b: Vec<u8> = match self {
            Pcm::F32 => x.to_le_bytes().to_vec(),
            _ => self.int(x).to_le_bytes()[..self.bytes()].to_vec(),
        };
        if big_endian {
            b.reverse();
        }
        out.extend_from_slice(&b);
    }
}

/// A canonical RIFF/WAVE file.
pub fn wav(samples: &[f32], channels: u16, rate: u32, fmt: Pcm) -> Vec<u8> {
    let bps = fmt.bytes();
    let data_len = samples.len() * bps;
    let mut v = Vec::with_capacity(44 + data_len);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    let tag: u16 = if matches!(fmt, Pcm::F32) { 3 } else { 1 };
    v.extend_from_slice(&tag.to_le_bytes());
    v.extend_from_slice(&channels.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * u32::from(channels) * bps as u32).to_le_bytes());
    v.extend_from_slice(&((usize::from(channels) * bps) as u16).to_le_bytes());
    v.extend_from_slice(&((bps * 8) as u16).to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&(data_len as u32).to_le_bytes());
    for &x in samples {
        fmt.put(&mut v, x, false);
    }
    v
}

/// An AIFF file (16- or 24-bit big-endian PCM).
pub fn aiff(samples: &[f32], channels: u16, rate: u32, fmt: Pcm) -> Vec<u8> {
    let bps = fmt.bytes();
    let frames = samples.len() / usize::from(channels);
    let data_len = samples.len() * bps;
    let mut v = Vec::new();
    v.extend_from_slice(b"FORM");
    v.extend_from_slice(&((4 + 26 + 16 + data_len) as u32).to_be_bytes());
    v.extend_from_slice(b"AIFFCOMM");
    v.extend_from_slice(&18u32.to_be_bytes());
    v.extend_from_slice(&channels.to_be_bytes());
    v.extend_from_slice(&(frames as u32).to_be_bytes());
    v.extend_from_slice(&((bps * 8) as u16).to_be_bytes());
    v.extend_from_slice(&extended(f64::from(rate)));
    v.extend_from_slice(b"SSND");
    v.extend_from_slice(&((8 + data_len) as u32).to_be_bytes());
    v.extend_from_slice(&[0; 8]);
    for &x in samples {
        fmt.put(&mut v, x, true);
    }
    v
}

/// 80-bit IEEE extended, for the AIFF sample rate.
fn extended(x: f64) -> [u8; 10] {
    let exp = x.log2().floor() as i32;
    let mant = (x / 2f64.powi(exp) * (1u64 << 63) as f64) as u64;
    let mut b = [0; 10];
    b[..2].copy_from_slice(&((exp + 16383) as u16).to_be_bytes());
    b[2..].copy_from_slice(&mant.to_be_bytes());
    b
}

/// Writes `bytes` to a fresh file under the test temp dir. Every call gets its own file, so tests
/// running in parallel never rewrite a file another test is streaming.
pub fn temp_file(name: &str, bytes: &[u8]) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("media-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("{n}-{name}"));
    std::fs::write(&p, bytes).unwrap();
    p
}

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Estimated frequency of a mono signal from its zero crossings (interpolated).
pub fn frequency(x: &[f32], rate: u32) -> f64 {
    let mut crossings = Vec::new();
    for (i, w) in x.windows(2).enumerate() {
        let (a, b) = (f64::from(w[0]), f64::from(w[1]));
        if a < 0.0 && b >= 0.0 {
            crossings.push(i as f64 + a / (a - b));
        }
    }
    let (first, last) = (crossings[0], *crossings.last().unwrap());
    (crossings.len() - 1) as f64 / (last - first) * f64::from(rate)
}

pub fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

/// Bytes `[from, to)` are held back until [`Gate::open`]: reads that reach them block. Simulates
/// a disk that stalls mid-file.
#[derive(Clone, Default)]
pub struct Gate(Arc<(Mutex<bool>, Condvar)>);

impl Gate {
    pub fn open(&self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }

    fn wait(&self) {
        let mut open = self.0.0.lock().unwrap();
        while !*open {
            open = self.0.1.wait(open).unwrap();
        }
    }
}

pub struct GatedReader {
    inner: Cursor<Vec<u8>>,
    from: u64,
    to: u64,
    gate: Gate,
}

impl GatedReader {
    pub fn new(bytes: Vec<u8>, from: u64, to: u64, gate: Gate) -> Self {
        GatedReader {
            inner: Cursor::new(bytes),
            from,
            to,
            gate,
        }
    }
}

impl Read for GatedReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let pos = self.inner.position();
        if (self.from..self.to).contains(&pos) {
            self.gate.wait();
        }
        // Never read across the gate while it is shut.
        let limit = if pos < self.from {
            ((self.from - pos) as usize).min(buf.len())
        } else {
            buf.len()
        };
        self.inner.read(&mut buf[..limit])
    }
}

impl Seek for GatedReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// How a [`FailingReader`] fails once a read reaches its offset.
#[derive(Clone, Copy, Debug)]
pub enum Failure {
    /// An I/O error, as from a disk that went away.
    Error,
    /// A panic, standing in for a decoder bug triggered by the data.
    Panic,
}

/// Reads normally up to byte `at`, then fails every read that reaches it. Simulates a read
/// error part way through a stream.
pub struct FailingReader {
    inner: Cursor<Vec<u8>>,
    at: u64,
    failure: Failure,
}

impl FailingReader {
    pub fn new(bytes: Vec<u8>, at: u64, failure: Failure) -> Self {
        FailingReader {
            inner: Cursor::new(bytes),
            at,
            failure,
        }
    }
}

impl Read for FailingReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let pos = self.inner.position();
        if pos >= self.at {
            match self.failure {
                Failure::Error => return Err(std::io::Error::other("injected read error")),
                Failure::Panic => panic!("injected panic"),
            }
        }
        let limit = ((self.at - pos) as usize).min(buf.len());
        self.inner.read(&mut buf[..limit])
    }
}

impl Seek for FailingReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}
