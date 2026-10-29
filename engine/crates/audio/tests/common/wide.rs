// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    dead_code
)]

//! Many-channel PCM writers: WAV (plain and WAVE_FORMAT_EXTENSIBLE) and AIFF/AIFF-C in
//! every sample format the reader supports. Samples are generated as exact `f64` values (whole
//! multiples of the format's step), so a decode must reproduce them bit for bit.

/// Sample formats.
#[derive(Clone, Copy, Debug)]
pub enum Fmt {
    I8,
    I16,
    I24,
    I32,
    F32,
    F64,
}

impl Fmt {
    pub const ALL: [Fmt; 6] = [Fmt::I8, Fmt::I16, Fmt::I24, Fmt::I32, Fmt::F32, Fmt::F64];

    pub fn bytes(self) -> usize {
        match self {
            Fmt::I8 => 1,
            Fmt::I16 => 2,
            Fmt::I24 => 3,
            Fmt::I32 | Fmt::F32 => 4,
            Fmt::F64 => 8,
        }
    }

    pub fn float(self) -> bool {
        matches!(self, Fmt::F32 | Fmt::F64)
    }

    /// Full scale of the integer formats: a sample is `k / full` for an integer `k`.
    fn full(self) -> f64 {
        (1u64 << (8 * self.bytes() - 1)) as f64
    }

    /// The value of sample (`frame`, `ch`): exact in this format, within ±1.
    pub fn value(self, frame: u64, ch: usize) -> f64 {
        let mut h = frame
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add((ch as u64 + 1).wrapping_mul(0xBF58_476D_1CE4_E5B9));
        h ^= h >> 29;
        h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
        h ^= h >> 32;
        let k = (h as u32) as i32;
        match self {
            // Floats get 24 significant bits: exact in f32 and f64.
            Fmt::F32 | Fmt::F64 => f64::from(k >> 8) / 8_388_608.0,
            _ => f64::from(k >> (32 - 8 * self.bytes())) / self.full(),
        }
    }

    /// `frames` frames of samples, interleaved.
    pub fn samples(self, frames: u64, channels: usize) -> Vec<f64> {
        (0..frames)
            .flat_map(|f| (0..channels).map(move |c| self.value(f, c)))
            .collect()
    }

    fn put(self, out: &mut Vec<u8>, x: f64, big_endian: bool) {
        let mut b: Vec<u8> = match self {
            Fmt::F32 => (x as f32).to_le_bytes().to_vec(),
            Fmt::F64 => x.to_le_bytes().to_vec(),
            _ => ((x * self.full()).round() as i64).to_le_bytes()[..self.bytes()].to_vec(),
        };
        if big_endian {
            b.reverse();
        }
        out.extend_from_slice(&b);
    }
}

/// How a WAV's `fmt ` chunk is written.
#[derive(Clone, Copy, Debug)]
pub enum Header {
    Plain,
    /// WAVE_FORMAT_EXTENSIBLE with this channel mask.
    Extensible(u32),
}

fn chunk(v: &mut Vec<u8>, id: &[u8; 4], body: &[u8], big_endian: bool) {
    v.extend_from_slice(id);
    let n = body.len() as u32;
    v.extend_from_slice(&if big_endian {
        n.to_be_bytes()
    } else {
        n.to_le_bytes()
    });
    v.extend_from_slice(body);
    if body.len() % 2 == 1 {
        v.push(0);
    }
}

/// A RIFF/WAVE file of interleaved `samples`.
pub fn wav(samples: &[f64], channels: u16, rate: u32, fmt: Fmt, header: Header) -> Vec<u8> {
    let mut data = Vec::new();
    for &x in samples {
        if matches!(fmt, Fmt::I8) {
            // 8-bit WAV is unsigned.
            data.push(((x * 128.0).round() as i32 + 128) as u8);
        } else {
            fmt.put(&mut data, x, false);
        }
    }
    let bits = (fmt.bytes() * 8) as u16;
    let tag: u16 = if fmt.float() { 3 } else { 1 };
    let mut f = Vec::new();
    let tag_field = if matches!(header, Header::Plain) {
        tag
    } else {
        0xFFFE
    };
    f.extend_from_slice(&tag_field.to_le_bytes());
    f.extend_from_slice(&channels.to_le_bytes());
    f.extend_from_slice(&rate.to_le_bytes());
    let align = channels * (bits / 8);
    f.extend_from_slice(&(rate * u32::from(align)).to_le_bytes());
    f.extend_from_slice(&align.to_le_bytes());
    f.extend_from_slice(&bits.to_le_bytes());
    if let Header::Extensible(mask) = header {
        f.extend_from_slice(&22u16.to_le_bytes());
        f.extend_from_slice(&bits.to_le_bytes());
        f.extend_from_slice(&mask.to_le_bytes());
        f.extend_from_slice(&tag.to_le_bytes());
        f.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
        ]);
    }
    let mut v = Vec::new();
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&0u32.to_le_bytes());
    v.extend_from_slice(b"WAVE");
    chunk(&mut v, b"fmt ", &f, false);
    // A chunk the reader must skip, of odd length (so it is padded).
    chunk(&mut v, b"JUNK", b"abc", false);
    chunk(&mut v, b"data", &data, false);
    let riff = (v.len() - 8) as u32;
    v[4..8].copy_from_slice(&riff.to_le_bytes());
    v
}

/// An AIFF (`comp` = None) or AIFF-C file of interleaved `samples`.
pub fn aiff(
    samples: &[f64],
    channels: u16,
    rate: u32,
    fmt: Fmt,
    comp: Option<&[u8; 4]>,
) -> Vec<u8> {
    // `sowt` is the little-endian AIFF-C integer format.
    let be = comp != Some(b"sowt");
    let mut data = vec![0u8; 8];
    for &x in samples {
        if matches!(fmt, Fmt::I8) {
            data.push((x * 128.0).round() as i8 as u8);
        } else {
            fmt.put(&mut data, x, be);
        }
    }
    let frames = samples.len() / usize::from(channels);
    let mut c = Vec::new();
    c.extend_from_slice(&channels.to_be_bytes());
    c.extend_from_slice(&(frames as u32).to_be_bytes());
    c.extend_from_slice(&((fmt.bytes() * 8) as u16).to_be_bytes());
    let exp = f64::from(rate).log2().floor() as i32;
    let mant = (f64::from(rate) / 2f64.powi(exp) * (1u64 << 63) as f64) as u64;
    c.extend_from_slice(&((exp + 16383) as u16).to_be_bytes());
    c.extend_from_slice(&mant.to_be_bytes());
    if let Some(comp) = comp {
        c.extend_from_slice(comp);
        c.extend_from_slice(&[0, 0]); // empty pstring, padded
    }
    let mut v = Vec::new();
    v.extend_from_slice(b"FORM");
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(if comp.is_some() { b"AIFC" } else { b"AIFF" });
    chunk(&mut v, b"COMM", &c, true);
    chunk(&mut v, b"SSND", &data, true);
    let form = (v.len() - 8) as u32;
    v[4..8].copy_from_slice(&form.to_be_bytes());
    v
}
