// SPDX-License-Identifier: GPL-3.0-or-later

//! macOS: the pictures Video cues play, as textures the stage drawing samples. A still is
//! decoded once with ImageIO. A movie is read in order with AVFoundation's asset reader (the
//! platform's hardware decoders), one BGRA frame at a time: the frame shown at a position is the
//! last whose time is at or before it, so the picture follows the engine's clock and never runs
//! ahead of it. A position behind the frame held, or far past it, starts reading again there
//! (load to time, a resume after a pause elsewhere).
//!
//! The file was probed in a process of its own before it gets here (`--probe`); a read that
//! still fails here is reported, and the picture goes.

use std::ffi::c_void;
use std::path::Path;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_av_foundation::{
    AVAssetReader, AVAssetReaderStatus, AVAssetReaderTrackOutput, AVAssetTrack, AVMediaTypeVideo,
    AVURLAsset,
};
use objc2_core_foundation::{CFRetained, CFString, CFURL, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapInfo, CGColorSpace, CGContext, CGImageAlphaInfo,
    CGImageByteOrderInfo,
};
use objc2_core_media::{CMTime, CMTimeRange};
use objc2_core_video::{
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
    CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress, kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_32BGRA,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};
use objc2_image_io::CGImageSource;
use objc2_metal::{MTLDevice, MTLTexture};
use openlab_core::video_probe::MAX_VIDEO_PIXELS;

use super::gpu::{Texture, new_texture, replace};

/// How far past the frame held a position may be before reading starts again there instead of
/// reading every frame in between (a jump, not playback).
const SKIP_AHEAD: f64 = 2.0;

/// One picture's file, ready to be drawn.
pub enum Source {
    Still(Texture),
    Movie(Box<Movie>),
}

impl Source {
    pub fn open(
        device: &ProtocolObject<dyn MTLDevice>,
        file: &str,
        still: bool,
    ) -> Result<Self, String> {
        if still {
            Ok(Self::Still(still_texture(device, file)?))
        } else {
            Ok(Self::Movie(Box::new(Movie::open(device, file)?)))
        }
    }

    /// The texture to draw at `position` seconds, and its pixel size.
    pub fn frame(&mut self, position: f64) -> Result<(&Texture, (u32, u32)), String> {
        match self {
            Self::Still(t) => {
                let size = (side(t.width())?, side(t.height())?);
                Ok((t, size))
            }
            Self::Movie(m) => m.frame_at(position),
        }
    }
}

fn side(n: usize) -> Result<u32, String> {
    u32::try_from(n)
        .ok()
        .filter(|n| (1..=MAX_VIDEO_PIXELS).contains(n))
        .ok_or_else(|| format!("a picture side of {n} pixels"))
}

/// Decodes a still (PNG, JPEG, TIFF) into a BGRA texture.
fn still_texture(device: &ProtocolObject<dyn MTLDevice>, file: &str) -> Result<Texture, String> {
    let url = CFURL::from_file_path(file).ok_or("the path cannot be read")?;
    // SAFETY: ImageIO reads the file; every result is checked before use.
    let image = unsafe {
        CGImageSource::with_url(&url, None)
            .and_then(|s| s.image_at_index(0, None))
            .ok_or("the image cannot be decoded")?
    };
    let (w, h) = (
        objc2_core_graphics::CGImage::width(Some(&image)),
        objc2_core_graphics::CGImage::height(Some(&image)),
    );
    side(w)?;
    side(h)?;
    let mut px = vec![0u8; w * h * 4];
    let space = CGColorSpace::new_device_rgb().ok_or("no colour space")?;
    let info = CGBitmapInfo(
        CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0,
    );
    // SAFETY: a bitmap context over `px`, exactly w x h BGRA pixels in rows of w*4 bytes,
    // alive while the context draws into it.
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
        CGContext::draw_image(
            Some(&ctx),
            CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w as f64, h as f64)),
            Some(&image),
        );
    }
    new_texture(device, w, h, &mut px)
}

/// A movie read in order, with its last frame decoded.
pub struct Movie {
    asset: Retained<AVURLAsset>,
    track: Retained<AVAssetTrack>,
    reader: Option<(Retained<AVAssetReader>, Retained<AVAssetReaderTrackOutput>)>,
    texture: Texture,
    size: (u32, u32),
    /// The time of the frame in `texture`, if one is.
    shown: Option<f64>,
    /// The frame read past the position last asked for: its time, and its BGRA pixels.
    next: Option<(f64, Vec<u8>, usize)>,
    /// The reader has no frame after `next`.
    ended: bool,
    /// The movie's length and one frame's time, in seconds, as its file says.
    length: f64,
    frame: f64,
}

impl Movie {
    fn open(device: &ProtocolObject<dyn MTLDevice>, file: &str) -> Result<Self, String> {
        if !Path::new(file).is_file() {
            return Err("the file is not there".into());
        }
        let url = NSURL::fileURLWithPath(&NSString::from_str(file));
        // SAFETY: a file URL and no options; the tracks are read synchronously (the probe has
        // read this file before, so this is quick).
        let (asset, track) = unsafe {
            let asset = AVURLAsset::URLAssetWithURL_options(&url, None);
            let video = AVMediaTypeVideo.ok_or("the video framework is not available")?;
            #[allow(deprecated)]
            let tracks = asset.tracksWithMediaType(video);
            let track = tracks.firstObject().ok_or("the file has no picture")?;
            (asset, track)
        };
        // SAFETY: a plain property of a loaded track.
        let natural = unsafe { track.naturalSize() };
        let (w, h) = (natural.width.round(), natural.height.round());
        if !(w >= 1.0
            && h >= 1.0
            && w <= f64::from(MAX_VIDEO_PIXELS)
            && h <= f64::from(MAX_VIDEO_PIXELS))
        {
            return Err("the picture size cannot be read".into());
        }
        let size = (w as u32, h as u32);
        // SAFETY: plain properties of a loaded asset and track.
        let (length, fps) = unsafe { (asset.duration().seconds(), track.nominalFrameRate()) };
        if !(length.is_finite() && length > 0.0 && fps.is_finite() && fps > 0.0) {
            return Err("the movie's length or frame rate cannot be read".into());
        }
        let mut black = vec![0u8; size.0 as usize * size.1 as usize * 4];
        let texture = new_texture(device, size.0 as usize, size.1 as usize, &mut black)?;
        Ok(Self {
            asset,
            track,
            reader: None,
            texture,
            size,
            shown: None,
            next: None,
            ended: false,
            length,
            frame: 1.0 / f64::from(fps),
        })
    }

    /// Starts reading at `from` seconds.
    fn restart(&mut self, from: f64) -> Result<(), String> {
        if let Some((reader, _)) = self.reader.take() {
            // SAFETY: cancelling a reader this movie owns.
            unsafe { reader.cancelReading() };
        }
        self.next = None;
        self.ended = false;
        // SAFETY: AVFoundation calls on the movie's own asset and track; results are checked.
        unsafe {
            let reader = AVAssetReader::assetReaderWithAsset_error(&self.asset)
                .map_err(|e| e.localizedDescription().to_string())?;
            let key: &NSString = &*std::ptr::from_ref::<CFString>(kCVPixelBufferPixelFormatTypeKey)
                .cast::<NSString>();
            let format = NSNumber::new_u32(kCVPixelFormatType_32BGRA);
            let values: [&AnyObject; 1] = [format.as_ref()];
            let settings = NSDictionary::from_slices(&[key], &values);
            let output = AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(
                &self.track,
                Some(&settings),
            );
            output.setAlwaysCopiesSampleData(false);
            reader.addOutput(&output);
            reader.setTimeRange(CMTimeRange {
                start: CMTime::with_seconds(from.max(0.0), 600),
                duration: objc2_core_media::kCMTimePositiveInfinity,
            });
            if !reader.startReading() {
                return Err(reader_error(&reader));
            }
            self.reader = Some((reader, output));
        }
        Ok(())
    }

    /// Reads the next frame: its time and BGRA pixels (rows of the returned stride); `None` at
    /// the end of the movie.
    fn read(&mut self) -> Result<Option<(f64, Vec<u8>, usize)>, String> {
        let Some((reader, output)) = &self.reader else {
            return Ok(None);
        };
        // SAFETY: reading the reader's own output; a sample buffer's image is a pixel buffer
        // of BGRA (asked for above), locked while its bytes are copied out.
        unsafe {
            let Some(sample) = output.copyNextSampleBuffer() else {
                return match reader.status() {
                    AVAssetReaderStatus::Failed => Err(reader_error(reader)),
                    _ => Ok(None),
                };
            };
            let time = sample.presentation_time_stamp().seconds();
            let Some(image) = sample.image_buffer() else {
                return Err("a frame had no picture".into());
            };
            let (w, h) = (
                CVPixelBufferGetWidth(&image),
                CVPixelBufferGetHeight(&image),
            );
            if (w, h) != (self.size.0 as usize, self.size.1 as usize) {
                return Err(format!(
                    "a frame of {w} x {h} in a {} x {} movie",
                    self.size.0, self.size.1
                ));
            }
            CVPixelBufferLockBaseAddress(&image, CVPixelBufferLockFlags::ReadOnly);
            let base = CVPixelBufferGetBaseAddress(&image).cast::<u8>();
            let stride = CVPixelBufferGetBytesPerRow(&image);
            let bytes = if base.is_null() || stride < w * 4 {
                None
            } else {
                Some(std::slice::from_raw_parts(base, stride * h).to_vec())
            };
            CVPixelBufferUnlockBaseAddress(&image, CVPixelBufferLockFlags::ReadOnly);
            let bytes = bytes.ok_or("a frame could not be read")?;
            if !time.is_finite() {
                return Err("a frame has no time".into());
            }
            Ok(Some((time, bytes, stride)))
        }
    }

    /// The frame at `position` seconds: the last whose time is at or before it (the first frame
    /// before the first frame's time).
    fn frame_at(&mut self, position: f64) -> Result<(&Texture, (u32, u32)), String> {
        let behind = self.shown.is_some_and(|t| position < t);
        let far = self.shown.is_some_and(|t| position > t + SKIP_AHEAD);
        if self.reader.is_none() || behind || far {
            self.restart(position)?;
            self.shown = None;
        }
        let mut best: Option<(f64, Vec<u8>, usize)> = None;
        loop {
            let frame = match self.next.take() {
                Some(f) => Some(f),
                None if self.ended => None,
                None => self.read()?,
            };
            let Some(frame) = frame else {
                self.ended = true;
                break;
            };
            // The first frame read always counts: a position before it shows it.
            if frame.0 <= position || (self.shown.is_none() && best.is_none()) {
                best = Some(frame);
            } else {
                self.next = Some(frame);
                break;
            }
        }
        if let Some((time, mut bytes, stride)) = best {
            replace(&self.texture, self.size, &mut bytes, stride)?;
            self.shown = Some(time);
        }
        // The frames ran out well before the movie's end: its file is damaged or was cut short
        // under it. Holding the last frame would freeze the picture; the cue breaks instead.
        let short = self.ended
            && position < self.length - 2.0 * self.frame
            && self.shown.is_none_or(|t| position > t + 2.0 * self.frame);
        if short {
            return Err("the movie stops before its end (the file may be damaged)".into());
        }
        Ok((&self.texture, self.size))
    }
}

fn reader_error(reader: &AVAssetReader) -> String {
    // SAFETY: reading a reader's error.
    unsafe { reader.error() }.map_or_else(
        || "the movie could not be read".into(),
        |e| format!("the movie could not be read ({})", e.localizedDescription()),
    )
}

impl Drop for Movie {
    fn drop(&mut self) {
        if let Some((reader, _)) = self.reader.take() {
            // SAFETY: cancelling a reader this movie owns.
            unsafe { reader.cancelReading() };
        }
    }
}
