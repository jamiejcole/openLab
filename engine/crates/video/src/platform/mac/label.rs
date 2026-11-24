// SPDX-License-Identifier: GPL-3.0-or-later

//! A route's name as its alignment guides write it: the text drawn by AppKit into a bitmap of
//! our own, any script the system can draw, and handed back as coverage the GPU colours.

use std::ffi::c_void;

use objc2_app_kit::{NSGraphicsContext, NSStringDrawing};
use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapInfo, CGColorSpace, CGContext, CGImageAlphaInfo,
    CGImageByteOrderInfo,
};
use objc2_foundation::NSString;

/// The widest a name is drawn, in pixels: a longer one is drawn smaller.
pub const MAX_LABEL_WIDTH: u32 = 4096;

/// `text` drawn `height` pixels tall (the line's height, at least 1, at most 1024), at most
/// [`MAX_LABEL_WIDTH`] wide (a longer text is drawn smaller), as premultiplied BGRA pixels of
/// `colour`: its size and its pixels in rows of `width * 4` bytes.
pub fn label(text: &str, height: u32, colour: [u8; 3]) -> Result<((u32, u32), Vec<u8>), String> {
    let string = NSString::from_str(text);
    // SAFETY: no attributes: the system's default font, black. The size is checked below.
    let line = unsafe { string.sizeWithAttributes(None) };
    if !(line.width.is_finite() && line.height.is_finite() && line.width > 0.0 && line.height > 0.0)
    {
        return Err("the name has nothing to draw".into());
    }
    let height = f64::from(height.clamp(1, 1024));
    let scale = (height / line.height).min(f64::from(MAX_LABEL_WIDTH) / line.width);
    let w = ((line.width * scale).ceil() as usize).clamp(1, MAX_LABEL_WIDTH as usize);
    let h = ((line.height * scale).ceil() as usize).clamp(1, 1024);
    let mut px = vec![0u8; w * h * 4];
    let space = CGColorSpace::new_device_rgb().ok_or("no colour space")?;
    let info = CGBitmapInfo(
        CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0,
    );
    // SAFETY: a bitmap context over `px`, exactly w x h BGRA pixels in rows of w*4 bytes, alive
    // while the context draws into it; the graphics state saved here is restored before the
    // context goes, so no other drawing on this thread sees it.
    unsafe {
        let ctx: CFRetained<CGContext> = CGBitmapContextCreate(
            px.as_mut_ptr().cast::<c_void>(),
            w,
            h,
            8,
            w * 4,
            Some(&space),
            info.0,
        )
        .ok_or("no bitmap context")?;
        CGContext::scale_ctm(Some(&ctx), scale, scale);
        let drawing = NSGraphicsContext::graphicsContextWithCGContext_flipped(&ctx, false);
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&drawing));
        string.drawAtPoint_withAttributes(CGPoint::new(0.0, 0.0), None);
        NSGraphicsContext::restoreGraphicsState_class();
    }
    // The text's coverage is its alpha; each pixel becomes the colour times it (BGRA,
    // premultiplied).
    let [r, g, b] = colour.map(u32::from);
    for p in px.as_chunks_mut::<4>().0 {
        let [_, _, _, alpha] = *p;
        let a = u32::from(alpha);
        let times = |c: u32| u8::try_from(c * a / 255).unwrap_or(u8::MAX);
        *p = [times(b), times(g), times(r), alpha];
    }
    let size = (
        u32::try_from(w).unwrap_or(MAX_LABEL_WIDTH),
        u32::try_from(h).unwrap_or(1024),
    );
    Ok((size, px))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A name is drawn at the height asked, in the colour asked where it covers, clear
    /// elsewhere; a long one is held to the widest; an empty one has nothing to draw.
    #[test]
    fn a_name_is_drawn_in_its_colour_at_its_height() {
        let ((w, h), px) = label("Output 1", 64, [0, 200, 255]).unwrap();
        assert!((62..=66).contains(&h), "{h}");
        assert!(w > h * 2, "{w} x {h}");
        let covered: Vec<&[u8; 4]> = px
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] == 255)
            .collect();
        assert!(covered.len() > 100, "{} solid pixels", covered.len());
        assert!(
            covered.iter().all(|p| **p == [255, 200, 0, 255]),
            "BGRA of the colour"
        );
        assert!(
            px.as_chunks::<4>().0.iter().any(|p| p[3] == 0),
            "clear around the text"
        );
        let ((w, _), _) = label(&"W".repeat(512), 256, [255, 255, 255]).unwrap();
        assert!(w <= MAX_LABEL_WIDTH);
        assert!(label("", 64, [255, 255, 255]).is_err());
        // Another script draws too.
        let ((w, _), px) = label("\u{6295}\u{5f71}\u{6a5f}", 40, [255, 255, 255]).unwrap();
        assert!(w > 0 && px.as_chunks::<4>().0.iter().any(|p| p[3] > 0));
    }
}
