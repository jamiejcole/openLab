// SPDX-License-Identifier: GPL-3.0-or-later

//! macOS: the first look at a video file or still image (`openlab-stage --probe <file>`).
//!
//! Stills go through ImageIO, which decodes the first image (a damaged file fails here, not on
//! stage). Movies go through AVFoundation: the first video track's codec (its four-character
//! code), pixel size and nominal frame rate, and the asset's length. A movie in a codec the
//! platform decodes has its first frame decoded too, for the same reason. HAP is not played
//! yet (the platform has no decoder for it and openLab's own is not built): it is reported
//! unsupported with that reason.
//!
//! The process does nothing else and exits after its one line: a decoder that crashes or hangs
//! on a hostile file takes only this process with it, and the engine kills one that does not
//! answer in time.

use std::path::Path;

use objc2::rc::Retained;
use objc2_av_foundation::{AVAssetImageGenerator, AVMediaTypeVideo, AVURLAsset};
use objc2_core_foundation::{CFRetained, CFURL};
use objc2_core_graphics::CGImage;
use objc2_core_media::{CMFormatDescription, CMTime};
use objc2_foundation::{NSString, NSURL};
use objc2_image_io::CGImageSource;
use openlab_core::video_probe::{MAX_VIDEO_PIXELS, ProbeReport, StillFormat, VideoCodec};

/// What the file at `path` is.
pub fn probe(path: &Path) -> ProbeReport {
    match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ProbeReport::Missing,
        Err(e) => return unsupported(format!("the file cannot be read ({e})")),
        Ok(m) if !m.is_file() => return unsupported("it is not a file".into()),
        Ok(_) => {}
    }
    let Some(text) = path.to_str() else {
        return unsupported("its path is not valid Unicode".into());
    };
    if let Some(report) = still(text) {
        return report;
    }
    movie(text)
}

fn unsupported(reason: String) -> ProbeReport {
    ProbeReport::Unsupported { reason }
}

fn side(n: f64) -> Option<u32> {
    let n = n.round();
    (n >= 1.0 && n <= f64::from(MAX_VIDEO_PIXELS)).then_some(n as u32)
}

/// A still image, or `None` when ImageIO does not take the file for an image at all.
fn still(path: &str) -> Option<ProbeReport> {
    let url = CFURL::from_file_path(path)?;
    // SAFETY: a file URL and no options; ImageIO reads the file lazily.
    let source: CFRetained<CGImageSource> = unsafe { CGImageSource::with_url(&url, None) }?;
    // SAFETY: `source` is a valid image source.
    let kind = unsafe { source.r#type() }?.to_string();
    let format = match kind.as_str() {
        "public.png" => StillFormat::Png,
        "public.jpeg" => StillFormat::Jpeg,
        "public.tiff" => StillFormat::Tiff,
        // A movie container ImageIO also claims is left to AVFoundation.
        "com.apple.quicktime-movie" | "public.mpeg-4" => return None,
        other => {
            return Some(unsupported(format!(
                "the image format ({other}) is not supported; use PNG, JPEG or TIFF"
            )));
        }
    };
    // SAFETY: `source` is a valid image source; index 0 is checked by ImageIO.
    let image: Option<CFRetained<CGImage>> = unsafe { source.image_at_index(0, None) };
    let Some(image) = image else {
        return Some(unsupported("the image cannot be decoded".into()));
    };
    let (w, h) = (CGImage::width(Some(&image)), CGImage::height(Some(&image)));
    Some(match (side(w as f64), side(h as f64)) {
        (Some(width), Some(height)) => ProbeReport::Still {
            format,
            width,
            height,
        },
        _ => unsupported(format!(
            "the image is {w} x {h} pixels; at most {MAX_VIDEO_PIXELS} a side plays"
        )),
    })
}

/// The codec a movie track's four-character code names, if openLab plays it.
fn codec_of(code: u32) -> Option<VideoCodec> {
    match &code.to_be_bytes() {
        b"avc1" | b"avc3" => Some(VideoCodec::H264),
        b"hvc1" | b"hev1" => Some(VideoCodec::Hevc),
        b"apco" | b"apcs" | b"apcn" | b"apch" | b"ap4h" | b"ap4x" => Some(VideoCodec::ProRes),
        _ => None,
    }
}

/// Whether the four-character code is one of HAP's.
fn is_hap(code: u32) -> bool {
    matches!(
        &code.to_be_bytes(),
        b"Hap1" | b"Hap5" | b"HapY" | b"HapM" | b"HapA" | b"Hap7" | b"HapH"
    )
}

fn fourcc_text(code: u32) -> String {
    code.to_be_bytes()
        .iter()
        .map(|&b| if b.is_ascii_graphic() { b as char } else { '?' })
        .collect()
}

fn movie(path: &str) -> ProbeReport {
    let url = NSURL::fileURLWithPath(&NSString::from_str(path));
    // SAFETY: a file URL and no options.
    let asset: Retained<AVURLAsset> = unsafe { AVURLAsset::URLAssetWithURL_options(&url, None) };
    // SAFETY: the media type constant is a framework static that is always set.
    let Some(video) = (unsafe { AVMediaTypeVideo }) else {
        return unsupported("the video framework is not available".into());
    };
    #[allow(deprecated)]
    // SAFETY: loads the tracks synchronously; this process does nothing else meanwhile, and the
    // engine kills it if the read does not end.
    let tracks = unsafe { asset.tracksWithMediaType(video) };
    let Some(track) = tracks.firstObject() else {
        return unsupported("it is not a video file openLab can read (no picture found)".into());
    };
    // SAFETY: an asset track's format descriptions are CMFormatDescriptions.
    let descriptions = unsafe { track.formatDescriptions() };
    let Some(first) = descriptions.firstObject() else {
        return unsupported("its video track has no format".into());
    };
    let description: *const CMFormatDescription = Retained::as_ptr(&first).cast();
    // SAFETY: the object is a CMFormatDescription kept alive by `first`.
    let code = unsafe { (*description).media_sub_type() };
    if is_hap(code) {
        return unsupported("HAP movies are not played yet; use H.264, HEVC or ProRes".into());
    }
    let Some(codec) = codec_of(code) else {
        return unsupported(format!(
            "its video codec ({}) is not supported; use H.264, HEVC or ProRes",
            fourcc_text(code)
        ));
    };
    // SAFETY: plain properties of a loaded track and asset.
    let (size, fps, length): (_, f32, CMTime) = unsafe {
        (
            track.naturalSize(),
            track.nominalFrameRate(),
            asset.duration(),
        )
    };
    // SAFETY: CMTimeGetSeconds takes any CMTime (an invalid one gives NaN).
    let seconds = unsafe { length.seconds() };
    let (Some(width), Some(height)) = (side(size.width), side(size.height)) else {
        return unsupported(format!(
            "the picture is {} x {} pixels; at least 1 and at most {MAX_VIDEO_PIXELS} a side plays",
            size.width, size.height
        ));
    };
    let fps = f64::from(fps);
    if !(fps.is_finite() && fps > 0.0 && fps <= openlab_core::video_probe::MAX_FRAME_RATE) {
        return unsupported("its frame rate cannot be read".into());
    }
    if !(seconds.is_finite() && seconds > 0.0 && seconds <= openlab_core::MAX_TIME) {
        return unsupported("its length cannot be read".into());
    }
    if !first_frame_decodes(&asset) {
        return unsupported("its first frame cannot be decoded".into());
    }
    ProbeReport::Video {
        codec,
        width,
        height,
        fps,
        duration: seconds,
    }
}

/// Whether the platform decodes the movie's first frame.
fn first_frame_decodes(asset: &AVURLAsset) -> bool {
    // SAFETY: a generator for a valid asset; the call blocks until the frame is decoded or
    // fails, and the engine kills this process if it never returns.
    unsafe {
        let generator = AVAssetImageGenerator::assetImageGeneratorWithAsset(asset);
        let zero = CMTime::new(0, 1);
        // The asynchronous form needs a run loop this one-shot process does not have; the
        // synchronous one is still there on every macOS openLab runs on.
        #[allow(deprecated)]
        let frame = generator.copyCGImageAtTime_actualTime_error(zero, std::ptr::null_mut());
        frame.is_ok()
    }
}
