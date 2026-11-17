// SPDX-License-Identifier: GPL-3.0-or-later

//! HAP movie frames (VID-03): HAP, HAP Alpha and HAP Q. A HAP frame is a block-compressed
//! texture the GPU draws as it is (DXT1, DXT5, or DXT5 holding scaled YCoCg for HAP Q), wrapped
//! in a small section header and usually packed again with Snappy, whole or in chunks. This
//! module takes a frame's bytes as a movie holds them and gives back the texture, or why it
//! cannot: every length is checked against the frame's bytes and against the texture a frame of
//! that size must be before anything of its size is allocated, so a damaged or hostile file is
//! a reason, never a panic, a hang or an allocation past its picture's.
//!
//! Layout, as the format's public description has it: a section header is 4 bytes, a 24-bit
//! little-endian length then a type byte, or (when those 24 bits are 0) 8 bytes, the type byte
//! then a 32-bit length. A frame is one section whose type's low nibble is the texture format
//! and high nibble how it is packed: as it is, with Snappy, or in chunks. Chunks are described
//! by a "decode instructions" section (a packing byte and a length per chunk, offsets
//! optional) followed by the chunks' bytes; unpacked one after another they are the texture.
//!
//! Platform-free: the stage host's macOS reader hands it each frame's sample data.

/// The HAP flavours a Video cue plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// HAP: opaque colour in DXT1 (BC1).
    Hap,
    /// HAP Alpha: colour and straight alpha in DXT5 (BC3).
    HapAlpha,
    /// HAP Q: scaled YCoCg in DXT5 (BC3): Co, Cg and the block's scale in the colour channels,
    /// luma in alpha.
    HapQ,
}

/// The other HAP flavours' four-character codes, with their names: recognised so the reason a
/// file is refused names what it is.
const OTHER_CODES: [(&[u8; 4], &str); 4] = [
    (b"HapM", "HAP Q Alpha"),
    (b"HapA", "HAP Alpha-Only"),
    (b"Hap7", "HAP R"),
    (b"HapH", "HAP HDR"),
];

/// Texture format nibbles of a section's type.
const FORMAT_DXT1: u8 = 0x0B;
const FORMAT_DXT5: u8 = 0x0E;
const FORMAT_YCOCG_DXT5: u8 = 0x0F;

/// Packing nibbles (high nibble of a frame's type; also each chunk's byte in the table).
const PACK_NONE: u8 = 0x0A;
const PACK_SNAPPY: u8 = 0x0B;
const PACK_CHUNKS: u8 = 0x0C;

/// Section types inside a chunked frame.
const DECODE_INSTRUCTIONS: u8 = 0x01;
const CHUNK_PACKING: u8 = 0x02;
const CHUNK_SIZES: u8 = 0x03;
const CHUNK_OFFSETS: u8 = 0x04;

/// How much bigger than its texture a frame's bytes may be: Snappy can grow incompressible
/// data by a sixth plus a few bytes per chunk, and the headers and chunk tables add a little.
/// A sample larger than this is refused before it is copied.
const FRAME_SLACK: usize = 1 << 20;

impl Variant {
    /// The flavour a movie track's four-character code names, if openLab plays it.
    pub fn of_code(code: u32) -> Option<Self> {
        match &code.to_be_bytes() {
            b"Hap1" => Some(Self::Hap),
            b"Hap5" => Some(Self::HapAlpha),
            b"HapY" => Some(Self::HapQ),
            _ => None,
        }
    }

    /// The name of a HAP flavour openLab does not play, if `code` is one.
    pub fn unplayed(code: u32) -> Option<&'static str> {
        let bytes = code.to_be_bytes();
        OTHER_CODES
            .iter()
            .find(|(c, _)| **c == bytes)
            .map(|(_, name)| *name)
    }

    /// The flavour's four-character code.
    pub fn code(self) -> u32 {
        u32::from_be_bytes(*match self {
            Self::Hap => b"Hap1",
            Self::HapAlpha => b"Hap5",
            Self::HapQ => b"HapY",
        })
    }

    fn format(self) -> u8 {
        match self {
            Self::Hap => FORMAT_DXT1,
            Self::HapAlpha => FORMAT_DXT5,
            Self::HapQ => FORMAT_YCOCG_DXT5,
        }
    }

    /// Bytes per 4 x 4 block: 8 for DXT1, 16 for DXT5.
    pub fn block_bytes(self) -> usize {
        match self {
            Self::Hap => 8,
            Self::HapAlpha | Self::HapQ => 16,
        }
    }

    /// Bytes from one row of blocks to the next in a texture `width` pixels wide.
    pub fn row_bytes(self, width: u32) -> usize {
        (width as usize).div_ceil(4) * self.block_bytes()
    }

    /// The texture's length for a picture of `width` x `height`: every 4 x 4 block, the last
    /// row and column of blocks padded. `None` past what the platform can address.
    pub fn texture_len(self, width: u32, height: u32) -> Option<usize> {
        self.row_bytes(width)
            .checked_mul((height as usize).div_ceil(4))
    }

    /// The most bytes a frame of a picture of `width` x `height` may hold.
    pub fn max_frame_len(self, width: u32, height: u32) -> Option<usize> {
        let texture = self.texture_len(width, height)?;
        texture.checked_mul(2)?.checked_add(FRAME_SLACK)
    }
}

/// A section: its type and its contents, and what follows it.
struct Section<'a> {
    kind: u8,
    body: &'a [u8],
    rest: &'a [u8],
}

fn damaged(why: &str) -> String {
    format!("a HAP frame is damaged ({why})")
}

/// The section at the start of `bytes`.
fn section(bytes: &[u8]) -> Result<Section<'_>, String> {
    let head = bytes
        .get(..4)
        .ok_or_else(|| damaged("a section header is cut short"))?;
    let (len, kind, header) = match head {
        [0, 0, 0, kind] => {
            let long = bytes
                .get(4..8)
                .and_then(|b| <[u8; 4]>::try_from(b).ok())
                .ok_or_else(|| damaged("a section header is cut short"))?;
            (u32::from_le_bytes(long) as usize, *kind, 8usize)
        }
        [a, b, c, kind] => (
            usize::from(*a) | usize::from(*b) << 8 | usize::from(*c) << 16,
            *kind,
            4,
        ),
        _ => return Err(damaged("a section header is cut short")),
    };
    let end = header
        .checked_add(len)
        .ok_or_else(|| damaged("a section is longer than its frame"))?;
    let body = bytes
        .get(header..end)
        .ok_or_else(|| damaged("a section is longer than its frame"))?;
    let rest = bytes.get(end..).unwrap_or_default();
    Ok(Section { kind, body, rest })
}

/// The texture in `frame` (a HAP frame of `variant` for a picture of `width` x `height`), of
/// exactly [`Variant::texture_len`] bytes, rows of [`Variant::row_bytes`].
pub fn unwrap(frame: &[u8], variant: Variant, width: u32, height: u32) -> Result<Vec<u8>, String> {
    let want = variant
        .texture_len(width, height)
        .filter(|&n| n > 0)
        .ok_or_else(|| format!("a {width} x {height} HAP picture cannot be held"))?;
    if variant
        .max_frame_len(width, height)
        .is_none_or(|most| frame.len() > most)
    {
        return Err(damaged("it is larger than its picture allows"));
    }
    let top = section(frame)?;
    if top.kind & 0x0F != variant.format() {
        return Err(damaged("its texture format is not the movie's"));
    }
    match top.kind >> 4 {
        PACK_NONE => {
            if top.body.len() != want {
                return Err(damaged("its texture is the wrong size for its picture"));
            }
            Ok(top.body.to_vec())
        }
        PACK_SNAPPY => {
            let mut out = Vec::new();
            unsnap_into(top.body, want, &mut out)?;
            Ok(out)
        }
        PACK_CHUNKS => chunks(top.body, want),
        _ => Err(damaged("it is packed in a way openLab does not read")),
    }
}

/// Unpacks Snappy `input`, which must unpack to exactly `want` bytes, onto the end of `out`.
fn unsnap_into(input: &[u8], want: usize, out: &mut Vec<u8>) -> Result<(), String> {
    let len = snap::raw::decompress_len(input).map_err(|_| damaged("its Snappy data is bad"))?;
    if len != want {
        return Err(damaged("its texture is the wrong size for its picture"));
    }
    let from = out.len();
    out.resize(from + len, 0);
    let place = out
        .get_mut(from..)
        .ok_or_else(|| damaged("its texture is the wrong size for its picture"))?;
    let n = snap::raw::Decoder::new()
        .decompress(input, place)
        .map_err(|_| damaged("its Snappy data is bad"))?;
    if n != len {
        return Err(damaged("its Snappy data is bad"));
    }
    Ok(())
}

/// A chunked frame's texture: its decode instructions, then each chunk unpacked in turn.
fn chunks(body: &[u8], want: usize) -> Result<Vec<u8>, String> {
    let instructions = section(body)?;
    if instructions.kind != DECODE_INSTRUCTIONS {
        return Err(damaged("its chunks are not described"));
    }
    let data = instructions.rest;
    let (mut packing, mut sizes, mut offsets) = (None, None, None);
    let mut inner = instructions.body;
    while !inner.is_empty() {
        let s = section(inner)?;
        match s.kind {
            CHUNK_PACKING => packing = Some(s.body),
            CHUNK_SIZES => sizes = Some(s.body),
            CHUNK_OFFSETS => offsets = Some(s.body),
            // Anything else in the instructions is for another decoder.
            _ => {}
        }
        inner = s.rest;
    }
    let (Some(packing), Some(sizes)) = (packing, sizes) else {
        return Err(damaged("its chunk tables are missing"));
    };
    let count = packing.len();
    let words = |table: &[u8]| -> Option<Vec<usize>> {
        (table.len() == count.checked_mul(4)?).then(|| {
            table
                .as_chunks::<4>()
                .0
                .iter()
                .map(|w| u32::from_le_bytes(*w) as usize)
                .collect()
        })
    };
    let sizes = words(sizes).ok_or_else(|| damaged("its chunk tables disagree"))?;
    let offsets = match offsets {
        Some(table) => Some(words(table).ok_or_else(|| damaged("its chunk tables disagree"))?),
        None => None,
    };
    if count == 0 {
        return Err(damaged("it has no chunks"));
    }
    // What each chunk unpacks to, all known before the texture is allocated.
    let mut places = Vec::with_capacity(count);
    let mut next = 0usize;
    let mut total = 0usize;
    for (i, (&pack, &size)) in packing.iter().zip(&sizes).enumerate() {
        let start = match &offsets {
            Some(o) => o.get(i).copied().unwrap_or(usize::MAX),
            None => next,
        };
        let end = start
            .checked_add(size)
            .ok_or_else(|| damaged("a chunk lies outside its frame"))?;
        let bytes = data
            .get(start..end)
            .ok_or_else(|| damaged("a chunk lies outside its frame"))?;
        next = end;
        let len = match pack {
            PACK_NONE => bytes.len(),
            PACK_SNAPPY => {
                snap::raw::decompress_len(bytes).map_err(|_| damaged("its Snappy data is bad"))?
            }
            _ => return Err(damaged("a chunk is packed in a way openLab does not read")),
        };
        total = total
            .checked_add(len)
            .filter(|&t| t <= want)
            .ok_or_else(|| damaged("its texture is the wrong size for its picture"))?;
        places.push((pack, bytes, len));
    }
    if total != want {
        return Err(damaged("its texture is the wrong size for its picture"));
    }
    let mut out = Vec::with_capacity(want);
    for (pack, bytes, len) in places {
        if pack == PACK_NONE {
            out.extend_from_slice(bytes);
        } else {
            unsnap_into(bytes, len, &mut out)?;
        }
    }
    Ok(out)
}

/// Test support: an encoder of HAP frames of our own, for test movies (`--make-test-media`)
/// and the unit tests. Simple and exact for pictures of flat colours: each 4 x 4 block takes
/// its two most different colours as its endpoints. Never in a shipped stage host.
#[cfg(any(test, feature = "test-support"))]
#[allow(clippy::indexing_slicing)]
pub mod write {
    use super::{
        CHUNK_OFFSETS, CHUNK_PACKING, CHUNK_SIZES, DECODE_INSTRUCTIONS, PACK_CHUNKS, PACK_NONE,
        PACK_SNAPPY, Variant,
    };

    /// How a frame's texture is packed.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Pack {
        /// As it is.
        None,
        /// With Snappy, whole.
        Snappy,
        /// In this many Snappy chunks (the last one stored as it is), with an offset table.
        Chunks(usize),
    }

    /// A section of `kind` holding `body`: the short header when the length fits 24 bits and
    /// is not 0, else the long one.
    pub fn section(kind: u8, body: &[u8]) -> Vec<u8> {
        let len = body.len();
        let mut out = if len > 0 && len < 1 << 24 {
            vec![len as u8, (len >> 8) as u8, (len >> 16) as u8, kind]
        } else {
            let mut h = vec![0, 0, 0, kind];
            h.extend_from_slice(&(len as u32).to_le_bytes());
            h
        };
        out.extend_from_slice(body);
        out
    }

    /// The HAP frame of `variant` for `rgba` (straight alpha, rows of `width` pixels).
    pub fn frame(
        variant: Variant,
        width: u32,
        height: u32,
        rgba: &[u8],
        pack: Pack,
    ) -> Result<Vec<u8>, String> {
        let texture = texture(variant, width, height, rgba);
        let snappy = |b: &[u8]| {
            snap::raw::Encoder::new()
                .compress_vec(b)
                .map_err(|e| e.to_string())
        };
        Ok(match pack {
            Pack::None => section(PACK_NONE << 4 | variant.format(), &texture),
            Pack::Snappy => section(PACK_SNAPPY << 4 | variant.format(), &snappy(&texture)?),
            Pack::Chunks(n) => {
                let n = n.max(1);
                let size = texture.len().div_ceil(n).max(1);
                let parts: Vec<&[u8]> = texture.chunks(size).collect();
                let mut packing = Vec::new();
                let mut data = Vec::new();
                let (mut sizes, mut offsets) = (Vec::new(), Vec::new());
                for (i, part) in parts.iter().enumerate() {
                    let last = i + 1 == parts.len();
                    let bytes = if last { part.to_vec() } else { snappy(part)? };
                    packing.push(if last { PACK_NONE } else { PACK_SNAPPY });
                    offsets.extend_from_slice(&(data.len() as u32).to_le_bytes());
                    sizes.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                    data.extend_from_slice(&bytes);
                }
                let mut tables = section(CHUNK_PACKING, &packing);
                tables.extend(section(CHUNK_SIZES, &sizes));
                tables.extend(section(CHUNK_OFFSETS, &offsets));
                let mut body = section(DECODE_INSTRUCTIONS, &tables);
                body.extend(data);
                section(PACK_CHUNKS << 4 | variant.format(), &body)
            }
        })
    }

    /// The block-compressed texture of `rgba` for `variant`, edge pixels repeated into the
    /// padding of the last row and column of blocks.
    pub fn texture(variant: Variant, width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        let (w, h) = (width as usize, height as usize);
        let mut out = Vec::new();
        for by in 0..h.div_ceil(4) {
            for bx in 0..w.div_ceil(4) {
                let mut block = [[0u8; 4]; 16];
                for (i, px) in block.iter_mut().enumerate() {
                    let x = (bx * 4 + i % 4).min(w - 1);
                    let y = (by * 4 + i / 4).min(h - 1);
                    let at = (y * w + x) * 4;
                    px.copy_from_slice(&rgba[at..at + 4]);
                    if variant == Variant::HapQ {
                        *px = ycocg(*px);
                    }
                }
                match variant {
                    Variant::Hap => out.extend(colour_block(&block, true)),
                    Variant::HapAlpha | Variant::HapQ => {
                        out.extend(alpha_block(&block));
                        out.extend(colour_block(&block, false));
                    }
                }
            }
        }
        out
    }

    /// HAP Q's scaled YCoCg of an RGB colour at scale 1: Co and Cg offset by half, the scale
    /// byte 0 (scale 1), luma in alpha.
    fn ycocg([r, g, b, _]: [u8; 4]) -> [u8; 4] {
        let (r, g, b) = (f32::from(r), f32::from(g), f32::from(b));
        let byte = |v: f32| v.round().clamp(0.0, 255.0) as u8;
        [
            byte((r - b) / 2.0 + 128.0),
            byte((2.0 * g - r - b) / 4.0 + 128.0),
            0,
            byte((r + 2.0 * g + b) / 4.0),
        ]
    }

    fn to565([r, g, b, _]: [u8; 4]) -> u16 {
        let q = |v: u8, max: u32| (u32::from(v) * max + 127) / 255;
        (q(r, 31) << 11 | q(g, 63) << 5 | q(b, 31)) as u16
    }

    fn from565(c: u16) -> [i32; 3] {
        let (r, g, b) = (
            i32::from(c >> 11),
            i32::from(c >> 5 & 63),
            i32::from(c & 31),
        );
        [r << 3 | r >> 2, g << 2 | g >> 4, b << 3 | b >> 2]
    }

    fn distance(a: [i32; 3], b: [i32; 3]) -> i32 {
        a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum()
    }

    /// A DXT colour block: the two most different colours as endpoints, four-colour mode
    /// (endpoint 0 the larger, as DXT1 needs it).
    fn colour_block(block: &[[u8; 4]; 16], dxt1: bool) -> [u8; 8] {
        let colours: Vec<u16> = block.iter().map(|&p| to565(p)).collect();
        let (mut c0, mut c1) = (colours[0], colours[0]);
        let mut far = -1;
        for &a in &colours {
            for &b in &colours {
                let d = distance(from565(a), from565(b));
                if d > far {
                    (far, c0, c1) = (d, a, b);
                }
            }
        }
        if c0 < c1 || (!dxt1 && c0 == c1) {
            std::mem::swap(&mut c0, &mut c1);
        }
        let (e0, e1) = (from565(c0), from565(c1));
        let mix = |a: i32, b: i32, wa: i32, wb: i32| (a * wa + b * wb) / (wa + wb);
        let palette = [
            e0,
            e1,
            [0, 1, 2].map(|i| mix(e0[i], e1[i], 2, 1)),
            [0, 1, 2].map(|i| mix(e0[i], e1[i], 1, 2)),
        ];
        let mut indices = 0u32;
        if c0 != c1 {
            for (i, &p) in block.iter().enumerate() {
                let rgb = [i32::from(p[0]), i32::from(p[1]), i32::from(p[2])];
                let best = (0..4)
                    .min_by_key(|&k| distance(palette[k], rgb))
                    .unwrap_or(0);
                indices |= (best as u32) << (2 * i);
            }
        }
        let mut out = [0u8; 8];
        out[..2].copy_from_slice(&c0.to_le_bytes());
        out[2..4].copy_from_slice(&c1.to_le_bytes());
        out[4..].copy_from_slice(&indices.to_le_bytes());
        out
    }

    /// A DXT5 alpha block: the largest and smallest alpha as endpoints, eight-value mode.
    fn alpha_block(block: &[[u8; 4]; 16]) -> [u8; 8] {
        let a0 = block.iter().map(|p| p[3]).max().unwrap_or(255);
        let a1 = block.iter().map(|p| p[3]).min().unwrap_or(255);
        let (e0, e1) = (i32::from(a0), i32::from(a1));
        let value = |k: usize| match k {
            0 => e0,
            1 => e1,
            k => ((8 - k as i32) * e0 + (k as i32 - 1) * e1) / 7,
        };
        let mut bits = 0u64;
        if a0 != a1 {
            for (i, p) in block.iter().enumerate() {
                let a = i32::from(p[3]);
                let best = (0..8).min_by_key(|&k| (value(k) - a).abs()).unwrap_or(0);
                bits |= (best as u64) << (3 * i);
            }
        }
        let mut out = [0u8; 8];
        out[0] = a0;
        out[1] = a1;
        out[2..].copy_from_slice(&bits.to_le_bytes()[..6]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::write::{Pack, frame, section};
    use super::*;

    /// A picture of four flat colours (red, green, blue, yellow quadrants), half alpha at the
    /// bottom right.
    fn picture(w: u32, h: u32) -> Vec<u8> {
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                px.extend(match (x >= w / 2, y >= h / 2) {
                    (false, false) => [255, 0, 0, 255],
                    (true, false) => [0, 255, 0, 255],
                    (false, true) => [0, 0, 255, 255],
                    (true, true) => [255, 255, 0, 128],
                });
            }
        }
        px
    }

    const VARIANTS: [Variant; 3] = [Variant::Hap, Variant::HapAlpha, Variant::HapQ];
    const PACKS: [Pack; 4] = [Pack::None, Pack::Snappy, Pack::Chunks(1), Pack::Chunks(5)];

    #[test]
    fn every_variant_and_packing_unwraps_to_its_texture() {
        for (w, h) in [(16, 8), (6, 5), (1, 1), (288, 160)] {
            let px = picture(w, h);
            for variant in VARIANTS {
                let texture = write::texture(variant, w, h, &px);
                assert_eq!(Some(texture.len()), variant.texture_len(w, h));
                for pack in PACKS {
                    let f = frame(variant, w, h, &px, pack).unwrap();
                    assert_eq!(
                        unwrap(&f, variant, w, h).as_ref(),
                        Ok(&texture),
                        "{variant:?} {pack:?} {w}x{h}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_four_character_codes_name_the_variants() {
        for variant in VARIANTS {
            assert_eq!(Variant::of_code(variant.code()), Some(variant));
            assert_eq!(Variant::unplayed(variant.code()), None);
        }
        let code = |c: &[u8; 4]| u32::from_be_bytes(*c);
        assert_eq!(Variant::unplayed(code(b"HapM")), Some("HAP Q Alpha"));
        assert_eq!(Variant::of_code(code(b"HapM")), None);
        assert_eq!(Variant::of_code(code(b"avc1")), None);
        assert_eq!(Variant::unplayed(code(b"avc1")), None);
    }

    /// A long section header (24-bit length 0, then 32 bits) is read like a short one.
    #[test]
    fn a_long_section_header_is_read() {
        let px = picture(8, 8);
        let texture = write::texture(Variant::Hap, 8, 8, &px);
        let mut long = vec![0, 0, 0, 0xAB];
        long.extend_from_slice(&(texture.len() as u32).to_le_bytes());
        long.extend_from_slice(&texture);
        assert_eq!(unwrap(&long, Variant::Hap, 8, 8), Ok(texture));
    }

    fn refused(bytes: &[u8], variant: Variant, w: u32, h: u32) -> String {
        match unwrap(bytes, variant, w, h) {
            Err(why) => why,
            Ok(t) => panic!("unwrapped {} bytes", t.len()),
        }
    }

    #[test]
    fn a_damaged_frame_is_refused_with_a_reason() {
        let px = picture(16, 16);
        let good = frame(Variant::HapAlpha, 16, 16, &px, Pack::Snappy).unwrap();
        // Cut short, in the header and in the body.
        assert!(refused(&good[..2], Variant::HapAlpha, 16, 16).contains("cut short"));
        assert!(refused(&good[..good.len() - 1], Variant::HapAlpha, 16, 16).contains("longer"));
        // Another variant's texture, or a picture of another size.
        assert!(refused(&good, Variant::Hap, 16, 16).contains("format"));
        assert!(refused(&good, Variant::HapAlpha, 16, 20).contains("wrong size"));
        // Snappy data that is not Snappy.
        let mut bad = good.clone();
        for b in bad.iter_mut().skip(4) {
            *b = 0xFF;
        }
        assert!(refused(&bad, Variant::HapAlpha, 16, 16).contains("Snappy"));
        // An unknown packing.
        let mut odd = good.clone();
        odd[3] = 0xDE;
        assert!(refused(&odd, Variant::HapAlpha, 16, 16).contains("packed"));
        // Nothing at all, and a picture of no size.
        assert!(refused(&[], Variant::Hap, 16, 16).contains("cut short"));
        assert!(refused(&good, Variant::HapAlpha, 0, 16).contains("cannot be held"));
        // Larger than its picture could ever need.
        let huge = vec![0u8; Variant::Hap.max_frame_len(4, 4).unwrap() + 1];
        assert!(refused(&huge, Variant::Hap, 4, 4).contains("larger"));
    }

    #[test]
    fn a_chunked_frame_whose_tables_lie_is_refused() {
        let w = 16;
        let texture = write::texture(Variant::Hap, w, w, &picture(w, w));
        let build = |packing: &[u8], sizes: &[u32], offsets: Option<&[u32]>, data: &[u8]| {
            let words = |v: &[u32]| v.iter().flat_map(|n| n.to_le_bytes()).collect::<Vec<_>>();
            let mut tables = section(0x02, packing);
            tables.extend(section(0x03, &words(sizes)));
            if let Some(o) = offsets {
                tables.extend(section(0x04, &words(o)));
            }
            let mut body = section(0x01, &tables);
            body.extend_from_slice(data);
            section(0xCB, &body)
        };
        let n = texture.len() as u32;
        let half = n / 2;
        // Good: two raw chunks, with and without offsets.
        let ok = build(&[0x0A, 0x0A], &[half, n - half], None, &texture);
        assert_eq!(unwrap(&ok, Variant::Hap, w, w).as_ref(), Ok(&texture));
        let ok = build(&[0x0A, 0x0A], &[half, n - half], Some(&[0, half]), &texture);
        assert_eq!(unwrap(&ok, Variant::Hap, w, w).as_ref(), Ok(&texture));
        let cases: [(&str, Vec<u8>); 8] = [
            (
                "tables disagree",
                build(&[0x0A], &[half, n - half], None, &texture),
            ),
            ("outside", build(&[0x0A, 0x0A], &[half, n], None, &texture)),
            (
                "outside",
                build(
                    &[0x0A, 0x0A],
                    &[half, n - half],
                    Some(&[0, u32::MAX]),
                    &texture,
                ),
            ),
            ("wrong size", build(&[0x0A], &[half], None, &texture)),
            (
                "packed",
                build(&[0x0A, 0x07], &[half, n - half], None, &texture),
            ),
            ("no chunks", build(&[], &[], None, &texture)),
            ("not described", section(0xCB, &section(0x09, &texture))),
            (
                "missing",
                section(0xCB, &section(0x01, &section(0x02, &[0x0A]))),
            ),
        ];
        for (want, bytes) in cases {
            let why = refused(&bytes, Variant::Hap, w, w);
            assert!(why.contains(want), "{want}: {why}");
        }
    }

    /// Random bytes, every frame cut at every length, and frames with bytes changed at random
    /// (a seeded walk): each unwraps or gives a reason, never a panic, and what unwraps is
    /// always the texture's exact length.
    #[test]
    fn random_cut_and_mutated_frames_never_panic() {
        let mut seed: u64 = 0x4a50;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let (w, h) = (24, 12);
        let px = picture(w, h);
        let mut checked = 0;
        let mut check = |bytes: &[u8], variant: Variant| {
            if let Ok(t) = unwrap(bytes, variant, w, h) {
                assert_eq!(Some(t.len()), variant.texture_len(w, h));
            }
            checked += 1;
        };
        for variant in VARIANTS {
            for pack in PACKS {
                let good = frame(variant, w, h, &px, pack).unwrap();
                for cut in 0..good.len() {
                    check(&good[..cut], variant);
                }
                for round in 0..400 {
                    let mut bytes = good.clone();
                    for _ in 0..(1 + round % 6) {
                        let at = (next() % bytes.len() as u64) as usize;
                        bytes[at] = next() as u8;
                    }
                    check(&bytes, variant);
                }
            }
            for len in [0, 1, 3, 4, 7, 8, 9, 64, 4096] {
                for _ in 0..50 {
                    let bytes: Vec<u8> = (0..len).map(|_| next() as u8).collect();
                    check(&bytes, variant);
                }
            }
        }
        assert!(checked > 5_000, "{checked}");
    }
}
