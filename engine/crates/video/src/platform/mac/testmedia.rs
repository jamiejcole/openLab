// SPDX-License-Identifier: GPL-3.0-or-later

//! Test support: media files made at test time (`openlab-stage --make-test-media ...`), so no
//! test needs a checked-in movie. Every frame is the test card ([`crate::card`]) for its own
//! frame number: a test that reads a stage back knows which frame of the file is shown.
//!
//! Movies are written with AVFoundation's own software encoders (H.264, HEVC, ProRes 422 Proxy:
//! the hardware encoder is turned off, so making a test file never depends on a machine's
//! video hardware, which a virtual machine shares and can fail under load); stills
//! with ImageIO (PNG, JPEG, TIFF). A codec this Mac cannot encode fails the command, and the
//! test that wanted it fails: it never passes without the file.

use std::path::Path;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_av_foundation::{
    AVAssetWriter, AVAssetWriterInput, AVAssetWriterInputPixelBufferAdaptor, AVAssetWriterStatus,
    AVFileTypeQuickTimeMovie, AVMediaTypeVideo, AVVideoCodecKey,
    AVVideoCodecTypeAppleProRes422Proxy, AVVideoCodecTypeH264, AVVideoCodecTypeHEVC,
    AVVideoEncoderSpecificationKey, AVVideoHeightKey, AVVideoWidthKey,
};
use objc2_core_foundation::{CFData, CFRetained, CFString, CFURL};
use objc2_core_graphics::{
    CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage, CGImageAlphaInfo,
};
use objc2_core_media::{CMFormatDescription, CMTime};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    kCVPixelFormatType_32BGRA, kCVReturnSuccess,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};
use objc2_image_io::CGImageDestination;

use crate::card;

/// `--make-test-media <kind> <path> [frames fps]`: `kind` is `h264`, `hevc`, `prores` (with a
/// frame count and rate) or `png`, `jpeg`, `tiff` (frame 0's card), or `huge-png` (a black
/// PNG of 16384 x 8192 pixels, one bit each: small on disk, too many pixels to play). The file
/// must not exist.
pub fn make(args: &[String]) -> Result<(), String> {
    match args {
        [kind, path, frames, fps] => {
            let frames: u16 = frames.parse().map_err(|_| "bad frame count")?;
            let fps: i32 = fps.parse().map_err(|_| "bad frame rate")?;
            if frames == 0 || !(1..=240).contains(&fps) {
                return Err("frames and fps must be positive".into());
            }
            // AVFoundation wants its writer finished off the main thread.
            let (kind, path) = (kind.clone(), path.clone());
            std::thread::spawn(move || movie(&kind, Path::new(&path), frames, fps))
                .join()
                .map_err(|_| "the writer thread failed".to_owned())?
        }
        [kind, path] => still(kind, Path::new(path)),
        _ => Err("usage: --make-test-media <kind> <path> [frames fps]".into()),
    }
}

fn file_url(path: &Path) -> Result<Retained<NSURL>, String> {
    let text = path.to_str().ok_or("path is not Unicode")?;
    Ok(NSURL::fileURLWithPath(&NSString::from_str(text)))
}

/// The card for `frame`, as BGRA rows.
fn bgra(frame: u16) -> Vec<u8> {
    let mut px = card::pixels(frame);
    for p in px.as_chunks_mut::<4>().0 {
        p.swap(0, 2);
    }
    px
}

fn movie(kind: &str, path: &Path, frames: u16, fps: i32) -> Result<(), String> {
    // SAFETY: framework constants, always set on a Mac with AVFoundation.
    let codec = unsafe {
        match kind {
            "h264" => AVVideoCodecTypeH264,
            "hevc" => AVVideoCodecTypeHEVC,
            "prores" => AVVideoCodecTypeAppleProRes422Proxy,
            _ => return Err(format!("unknown codec {kind}")),
        }
    }
    .ok_or("codec constant missing")?;
    let url = file_url(path)?;
    // SAFETY: AVFoundation calls with valid arguments; every result is checked. The process
    // does nothing else, so waiting for the input to be ready is a plain loop.
    unsafe {
        let file_type = AVFileTypeQuickTimeMovie.ok_or("file type missing")?;
        let writer = AVAssetWriter::assetWriterWithURL_fileType_error(&url, file_type)
            .map_err(|e| e.localizedDescription().to_string())?;
        // VideoToolbox's encoder specification key (its string value): false means hardware
        // encode is never used.
        let no_hardware = NSDictionary::from_slices(
            &[&*NSString::from_str(
                "EnableHardwareAcceleratedVideoEncoder",
            )],
            &[&*NSNumber::new_bool(false)],
        );
        let keys: [&NSString; 4] = [
            AVVideoCodecKey.ok_or("key missing")?,
            AVVideoWidthKey.ok_or("key missing")?,
            AVVideoHeightKey.ok_or("key missing")?,
            AVVideoEncoderSpecificationKey.ok_or("key missing")?,
        ];
        let width = NSNumber::new_usize(card::WIDTH);
        let height = NSNumber::new_usize(card::HEIGHT);
        let values: [&AnyObject; 4] = [
            codec.as_ref(),
            width.as_ref(),
            height.as_ref(),
            no_hardware.as_ref(),
        ];
        let settings = NSDictionary::from_slices(&keys, &values);
        let media = AVMediaTypeVideo.ok_or("media type missing")?;
        let input = AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
            media,
            Some(&settings),
        );
        let adaptor = AVAssetWriterInputPixelBufferAdaptor::assetWriterInputPixelBufferAdaptorWithAssetWriterInput_sourcePixelBufferAttributes(&input, None);
        // The container's index first, as a file made for streaming has it: a test can then
        // damage the frames that follow and leave the file readable.
        writer.setShouldOptimizeForNetworkUse(true);
        writer.addInput(&input);
        if !writer.startWriting() {
            return Err(writer_error(&writer));
        }
        writer.startSessionAtSourceTime(CMTime::new(0, fps));
        for frame in 0..frames {
            let buffer = pixel_buffer(&bgra(frame))?;
            let mut spins = 0u32;
            while !input.isReadyForMoreMediaData() {
                spins += 1;
                if spins > 10_000 {
                    return Err("the encoder never became ready".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            if !adaptor
                .appendPixelBuffer_withPresentationTime(&buffer, CMTime::new(i64::from(frame), fps))
            {
                return Err(writer_error(&writer));
            }
        }
        input.markAsFinished();
        writer.endSessionAtSourceTime(CMTime::new(i64::from(frames), fps));
        #[allow(deprecated)]
        let done = writer.finishWriting();
        if !done || writer.status() != AVAssetWriterStatus::Completed {
            return Err(writer_error(&writer));
        }
    }
    Ok(())
}

fn writer_error(writer: &AVAssetWriter) -> String {
    // SAFETY: reading a writer's error.
    unsafe { writer.error() }.map_or_else(
        || "the writer failed".into(),
        |e| e.localizedDescription().to_string(),
    )
}

fn pixel_buffer(bgra: &[u8]) -> Result<CFRetained<CVPixelBuffer>, String> {
    let mut out: *mut CVPixelBuffer = std::ptr::null_mut();
    // SAFETY: a fresh buffer of the card's size; filled row by row within its bytes per row,
    // between a lock and an unlock.
    unsafe {
        let status = CVPixelBufferCreate(
            None,
            card::WIDTH,
            card::HEIGHT,
            kCVPixelFormatType_32BGRA,
            None,
            NonNull::from(&mut out),
        );
        let out = NonNull::new(out).ok_or("no pixel buffer")?;
        if status != kCVReturnSuccess {
            return Err(format!("pixel buffer error {status}"));
        }
        let buffer = CFRetained::from_raw(out);
        CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags(0));
        let base = CVPixelBufferGetBaseAddress(&buffer).cast::<u8>();
        let stride = CVPixelBufferGetBytesPerRow(&buffer);
        let row = card::WIDTH * 4;
        for y in 0..card::HEIGHT {
            std::ptr::copy_nonoverlapping(bgra.as_ptr().add(y * row), base.add(y * stride), row);
        }
        CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags(0));
        Ok(buffer)
    }
}

fn still(kind: &str, path: &Path) -> Result<(), String> {
    if kind == "huge-png" {
        return huge_png(path);
    }
    let uti = match kind {
        "png" => "public.png",
        "jpeg" => "public.jpeg",
        "tiff" => "public.tiff",
        _ => return Err(format!("unknown still format {kind}")),
    };
    let text = path.to_str().ok_or("path is not Unicode")?;
    let url = CFURL::from_file_path(text).ok_or("bad path")?;
    let rgba = card::pixels(0);
    let data = CFData::from_bytes(&rgba);
    let provider = CGDataProvider::with_cf_data(Some(&data)).ok_or("no data provider")?;
    let space = CGColorSpace::new_device_rgb().ok_or("no colour space")?;
    // SAFETY: an RGBA image over `rgba`'s bytes, written to a new file and finalised.
    unsafe {
        let image = CGImage::new(
            card::WIDTH,
            card::HEIGHT,
            8,
            32,
            card::WIDTH * 4,
            Some(&space),
            CGBitmapInfo(CGImageAlphaInfo::PremultipliedLast.0),
            Some(&provider),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
        .ok_or("no image")?;
        let destination = CGImageDestination::with_url(&url, &CFString::from_str(uti), 1, None)
            .ok_or("no image destination")?;
        destination.add_image(&image, None);
        if !destination.finalize() {
            return Err("the image was not written".into());
        }
    }
    Ok(())
}

/// A black one-bit grey PNG of 16384 x 8192 pixels.
fn huge_png(path: &Path) -> Result<(), String> {
    const W: usize = 16384;
    const H: usize = 8192;
    let text = path.to_str().ok_or("path is not Unicode")?;
    let url = CFURL::from_file_path(text).ok_or("bad path")?;
    let bits = vec![0u8; W / 8 * H];
    let data = CFData::from_bytes(&bits);
    let provider = CGDataProvider::with_cf_data(Some(&data)).ok_or("no data provider")?;
    let space = CGColorSpace::new_device_gray().ok_or("no colour space")?;
    // SAFETY: a one-bit grey image over `bits` (rows of W / 8 bytes), written to a new file.
    unsafe {
        let image = CGImage::new(
            W,
            H,
            1,
            1,
            W / 8,
            Some(&space),
            CGBitmapInfo(CGImageAlphaInfo::None.0),
            Some(&provider),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
        .ok_or("no image")?;
        let destination =
            CGImageDestination::with_url(&url, &CFString::from_str("public.png"), 1, None)
                .ok_or("no image destination")?;
        destination.add_image(&image, None);
        if !destination.finalize() {
            return Err("the image was not written".into());
        }
    }
    Ok(())
}

#[link(name = "VideoToolbox", kind = "framework")]
unsafe extern "C" {
    fn VTDecompressionSessionCreate(
        allocator: *const std::ffi::c_void,
        format: *const CMFormatDescription,
        decoder_specification: *const std::ffi::c_void,
        image_buffer_attributes: *const std::ffi::c_void,
        output_callback: *const std::ffi::c_void,
        session_out: *mut *mut std::ffi::c_void,
    ) -> i32;
    fn VTSessionCopyProperty(
        session: *mut std::ffi::c_void,
        key: *const std::ffi::c_void,
        allocator: *const std::ffi::c_void,
        value_out: *mut *const std::ffi::c_void,
    ) -> i32;
    fn VTDecompressionSessionInvalidate(session: *mut std::ffi::c_void);
    static kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder:
        *const std::ffi::c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFBooleanGetValue(boolean: *const std::ffi::c_void) -> u8;
    fn CFRelease(object: *const std::ffi::c_void);
}

/// `--test-decoder <movie>`: whether VideoToolbox, asked with its default decoder
/// specification, picks a hardware decoder for the movie's first video track: `Ok(true)`
/// hardware, `Ok(false)` software. The stage host's reader takes no decoder specification, so
/// this is taken as what it gets. Tests use it to tell whether a movie's frames depend on a
/// machine's video hardware.
pub fn uses_hardware_decoder(path: &str) -> Result<bool, String> {
    let url = NSURL::fileURLWithPath(&NSString::from_str(path));
    // SAFETY: a file URL and no options.
    let asset = unsafe { objc2_av_foundation::AVURLAsset::URLAssetWithURL_options(&url, None) };
    // SAFETY: the media type constant is a framework static that is always set.
    let video = unsafe { AVMediaTypeVideo }.ok_or("media type missing")?;
    #[allow(deprecated)]
    // SAFETY: loads the tracks synchronously; this process does nothing else meanwhile.
    let tracks = unsafe { asset.tracksWithMediaType(video) };
    let track = tracks.firstObject().ok_or("no video track")?;
    // SAFETY: an asset track's format descriptions are CMFormatDescriptions.
    let descriptions = unsafe { track.formatDescriptions() };
    let first = descriptions.firstObject().ok_or("no format")?;
    let format: *const CMFormatDescription = Retained::as_ptr(&first).cast();
    let mut session: *mut std::ffi::c_void = std::ptr::null_mut();
    // SAFETY: a format description kept alive by `first`, default allocator, specification
    // and attributes, no callback (nothing is decoded); the session is checked before use.
    let status = unsafe {
        VTDecompressionSessionCreate(
            std::ptr::null(),
            format,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            &mut session,
        )
    };
    if status != 0 || session.is_null() {
        return Err(format!("no decoder for it (status {status})"));
    }
    let mut value: *const std::ffi::c_void = std::ptr::null();
    // SAFETY: a live session we own, a framework key constant, and an out pointer; the value
    // (a CFBoolean, or none when the decoder does not say) is released after reading.
    let hardware = unsafe {
        let read = VTSessionCopyProperty(
            session,
            kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder,
            std::ptr::null(),
            &mut value,
        );
        let hardware = read == 0 && !value.is_null() && CFBooleanGetValue(value) != 0;
        if !value.is_null() {
            CFRelease(value);
        }
        VTDecompressionSessionInvalidate(session);
        CFRelease(session);
        hardware
    };
    Ok(hardware)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The decoder query answers for a movie (hardware or software, whichever this Mac picks)
    /// and refuses a file with no video track.
    #[test]
    fn the_decoder_query_answers_for_a_movie_and_refuses_a_still() {
        let dir = tempfile::tempdir().unwrap();
        let movie = dir.path().join("clip.mov");
        let still = dir.path().join("card.png");
        let arg = |p: &Path| p.to_str().unwrap().to_owned();
        make(&["h264".into(), arg(&movie), "5".into(), "25".into()]).unwrap();
        make(&["png".into(), arg(&still)]).unwrap();
        assert!(uses_hardware_decoder(&arg(&movie)).is_ok());
        assert!(uses_hardware_decoder(&arg(&still)).is_err());
        assert!(uses_hardware_decoder(&arg(&dir.path().join("gone.mov"))).is_err());
    }
}
