// SPDX-License-Identifier: GPL-3.0-or-later

//! The test card (test support only): a picture a test can read back from a stage without
//! comparing images. The top tenth is a barcode of [`CELLS`] cells: a white guard cell, the
//! frame number's 16 bits (most significant first, white = 1), a white guard cell. Below it
//! four quadrants: red top left, green top right, blue bottom left, yellow bottom right.

/// Cells across the barcode band.
pub const CELLS: usize = 18;
/// The card's pixel size: 16 pixels a cell, the band 16 pixels high.
pub const WIDTH: usize = CELLS * 16;
pub const HEIGHT: usize = 160;
const BAND: usize = HEIGHT / 10;

pub const RED: [u8; 4] = [255, 0, 0, 255];
pub const GREEN: [u8; 4] = [0, 255, 0, 255];
pub const BLUE: [u8; 4] = [0, 0, 255, 255];
pub const YELLOW: [u8; 4] = [255, 255, 0, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];

/// The card for `frame` as RGBA rows, top row first.
pub fn pixels(frame: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(WIDTH * HEIGHT * 4);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let colour = if y < BAND {
                let cell = x / (WIDTH / CELLS);
                match cell {
                    0 => WHITE,
                    c if c == CELLS - 1 => WHITE,
                    c if (frame >> (16 - c)) & 1 == 1 => WHITE,
                    _ => BLACK,
                }
            } else {
                let right = x >= WIDTH / 2;
                let bottom = y >= BAND + (HEIGHT - BAND) / 2;
                match (right, bottom) {
                    (false, false) => RED,
                    (true, false) => GREEN,
                    (false, true) => BLUE,
                    (true, true) => YELLOW,
                }
            };
            out.extend_from_slice(&colour);
        }
    }
    out
}

/// Where each barcode cell's centre is, as a fraction of the stage.
pub fn barcode_points() -> [[f32; 2]; CELLS] {
    let mut points = [[0.0; 2]; CELLS];
    for (i, p) in points.iter_mut().enumerate() {
        *p = [(i as f32 + 0.5) / CELLS as f32, 0.05];
    }
    points
}

/// Where the centre of each quadrant is: red, green, blue, yellow.
pub fn quadrant_points() -> [[f32; 2]; 4] {
    [[0.25, 0.3], [0.75, 0.3], [0.25, 0.8], [0.75, 0.8]]
}

/// The frame number the barcode cells' colours show, or `None` when they are not a barcode
/// (both guards white, every cell clearly white or black).
pub fn read_frame(cells: &[[u8; 4]]) -> Option<u16> {
    let bits: Option<Vec<bool>> = cells.iter().map(|c| level(*c)).collect();
    let bits = bits?;
    if bits.len() != CELLS || bits.first() != Some(&true) || bits.last() != Some(&true) {
        return None;
    }
    Some(
        bits.get(1..CELLS - 1)?
            .iter()
            .fold(0u16, |n, &b| (n << 1) | u16::from(b)),
    )
}

/// White is `true`, black `false`, anything else is not a barcode cell.
fn level(c: [u8; 4]) -> Option<bool> {
    let [r, g, b, _] = c;
    if r > 200 && g > 200 && b > 200 {
        Some(true)
    } else if r < 55 && g < 55 && b < 55 {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(px: &[u8], p: [f32; 2]) -> [u8; 4] {
        let x = (p[0] * WIDTH as f32) as usize;
        let y = (p[1] * HEIGHT as f32) as usize;
        let i = (y * WIDTH + x) * 4;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    }

    #[test]
    fn the_card_reads_back_as_its_frame_and_quadrants() {
        for frame in [0, 1, 0x8001, 12345, u16::MAX] {
            let px = pixels(frame);
            assert_eq!(px.len(), WIDTH * HEIGHT * 4);
            let cells: Vec<_> = barcode_points().iter().map(|&p| at(&px, p)).collect();
            assert_eq!(read_frame(&cells), Some(frame));
            let quads: Vec<_> = quadrant_points().iter().map(|&p| at(&px, p)).collect();
            assert_eq!(quads, vec![RED, GREEN, BLUE, YELLOW]);
        }
    }

    #[test]
    fn black_or_grey_is_no_barcode() {
        assert_eq!(read_frame(&[BLACK; CELLS]), None);
        let mut grey = [WHITE; CELLS];
        grey[5] = [128, 128, 128, 255];
        assert_eq!(read_frame(&grey), None);
        assert_eq!(read_frame(&[WHITE; 3]), None);
    }
}
