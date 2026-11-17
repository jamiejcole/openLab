// SPDX-License-Identifier: GPL-3.0-or-later

//! Test support: media files made at test time (`openlab-stage --make-test-media ...`), so no
//! test needs a checked-in movie. Every frame is the test card ([`crate::card`]) for its own
//! frame number: a test that reads a stage back knows which frame of the file is shown.
//!
//! Movies are written with AVFoundation's own software encoders (H.264, HEVC, ProRes 422 Proxy:
//! the hardware encoder is turned off, so making a test file never depends on a machine's
//! video hardware, which a virtual machine shares and can fail under load); stills
//! with ImageIO (PNG, JPEG, TIFF). A codec this Mac cannot encode fails the command, and the
//! test that wanted it fails: it never passes without the file. HAP movies have no platform
//! encoder: their frames are made by openLab's own test writer (`hap::write`) and written to
//! the movie as they are (AVFoundation's writer passes them through).

use std::path::Path;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_av_foundation::{
    AVAssetWriter, AVAssetWriterInput, AVAssetWriterInputPixelBufferAdaptor, AVAssetWriterStatus,
    AVFileTypeQuickTimeMovie, AVMediaTypeAudio, AVMediaTypeVideo, AVVideoCodecKey,
    AVVideoCodecTypeAppleProRes422Proxy, AVVideoCodecTypeAppleProRes4444, AVVideoCodecTypeH264,
    AVVideoCodecTypeHEVC, AVVideoEncoderSpecificationKey, AVVideoHeightKey, AVVideoWidthKey,
};
use objc2_core_foundation::{CFData, CFRetained, CFString, CFURL};
use objc2_core_graphics::{
    CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage, CGImageAlphaInfo,
};
use objc2_core_media::{
    CMFormatDescription, CMSampleBuffer, CMSampleTimingInfo, CMTime, kCMTimeInvalid,
};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    kCVPixelFormatType_32BGRA, kCVReturnSuccess,
};
use objc2_foundation::{NSData, NSDictionary, NSNumber, NSString, NSURL};
use objc2_image_io::CGImageDestination;

use crate::card;
use crate::hap::write::Pack;
use crate::hap::{Variant, write};

/// `--make-test-media <kind> <path> [frames fps]`: `kind` is `h264`, `hevc`, `prores`, or
/// `prores4444` (the card's right half below the barcode at half alpha), or `hap` (Snappy),
/// `hap-alpha` (Snappy; alpha as `prores4444`'s) or `hap-q` (in Snappy chunks), any HAP kind
/// with `@<n>` after it to have every frame from `n` on damaged ([`damage`]) (with a frame count
/// and rate), each with `+<format>:<channels>:<rate>` after it for every sound track it is to have
/// (`h264+pcm:2:48000`, `h264+aac:2:44100+pcm:6:48000`; see [`sound_sample`] for what a track
/// carries), or `png`, `jpeg`, `tiff` (frame 0's card), `huge-png` (a black
/// PNG of 16384 x 8192 pixels, one bit each: small on disk, too many pixels to play), or the
/// geometry tests' PNGs: `wide-png` (400 x 100, four bands left to right: red, green, blue,
/// yellow), `tall-png` (100 x 400, the bands top to bottom), `alpha-png` (200 x 100, left half
/// red, right half transparent), `tiny-png` (2 x 1: red, blue). The file must not exist.
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

/// One sound track of a test movie.
struct Sound {
    aac: bool,
    channels: u32,
    rate: u32,
}

fn sounds(specs: &[&str]) -> Result<Vec<Sound>, String> {
    specs
        .iter()
        .map(|spec| {
            let parts: Vec<&str> = spec.split(':').collect();
            let [format, channels, rate] = parts.as_slice() else {
                return Err(format!("bad sound track {spec}"));
            };
            let aac = match *format {
                "aac" => true,
                "pcm" => false,
                _ => return Err(format!("unknown sound format {format}")),
            };
            let channels: u32 = channels.parse().map_err(|_| "bad channel count")?;
            let rate: u32 = rate.parse().map_err(|_| "bad sample rate")?;
            if !(1..=64).contains(&channels) || !(8000..=192_000).contains(&rate) {
                return Err(format!("bad sound track {spec}"));
            }
            Ok(Sound {
                aac,
                channels,
                rate,
            })
        })
        .collect()
}

/// What sound track `track` (from 0) of a test movie holds at `frame` of its `rate` in
/// `channel`, so a test that reads the rendered output knows where in the file the sound is:
/// channel 0 rises from 0 to 0.5 over each second, channel 1 is the second's number plus one
/// over 2048 (negative in every track after the first), every further channel a constant
/// `channel / 64`.
pub fn sound_sample(track: usize, channel: u32, frame: u64, rate: u32) -> f32 {
    let rate = u64::from(rate.max(1));
    match channel {
        0 => (frame % rate) as f32 / rate as f32 * 0.5,
        1 => {
            let second = ((frame / rate + 1) as f32 / 2048.0).min(1.0);
            if track == 0 { second } else { -second }
        }
        c => c as f32 / 64.0,
    }
}

/// CoreAudio's stream description of linear PCM.
#[repr(C)]
struct StreamDescription {
    rate: f64,
    format: u32,
    flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels: u32,
    bits: u32,
    reserved: u32,
}

/// CoreAudio's channel layout without channel descriptions (a layout tag says it all).
#[repr(C)]
struct ChannelLayout {
    tag: u32,
    bitmap: u32,
    descriptions: u32,
}

const FORMAT_LPCM: u32 = u32::from_be_bytes(*b"lpcm");
const FORMAT_AAC: u32 = u32::from_be_bytes(*b"aac ");
/// Signed integer, packed.
const LPCM_FLAGS: u32 = 0x4 | 0x8;

/// The layout tag for `channels`: mono, stereo, 5.1, else that many discrete channels.
fn layout_tag(channels: u32) -> u32 {
    match channels {
        1 => (100 << 16) | 1,
        2 => (101 << 16) | 2,
        6 => (121 << 16) | 6,
        n => (147 << 16) | n,
    }
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMAudioFormatDescriptionCreate(
        allocator: *const std::ffi::c_void,
        description: *const StreamDescription,
        layout_size: usize,
        layout: *const ChannelLayout,
        cookie_size: usize,
        cookie: *const std::ffi::c_void,
        extensions: *const std::ffi::c_void,
        out: *mut *const CMFormatDescription,
    ) -> i32;
    fn CMBlockBufferCreateWithMemoryBlock(
        structure_allocator: *const std::ffi::c_void,
        memory: *mut std::ffi::c_void,
        length: usize,
        block_allocator: *const std::ffi::c_void,
        source: *const std::ffi::c_void,
        offset: usize,
        data_length: usize,
        flags: u32,
        out: *mut *mut std::ffi::c_void,
    ) -> i32;
    fn CMBlockBufferReplaceDataBytes(
        source: *const std::ffi::c_void,
        destination: *mut std::ffi::c_void,
        offset: usize,
        length: usize,
    ) -> i32;
    fn CMVideoFormatDescriptionCreate(
        allocator: *const std::ffi::c_void,
        codec: u32,
        width: i32,
        height: i32,
        extensions: *const std::ffi::c_void,
        out: *mut *const CMFormatDescription,
    ) -> i32;
    fn CMSampleBufferCreateReady(
        allocator: *const std::ffi::c_void,
        data: *mut std::ffi::c_void,
        format: *const CMFormatDescription,
        samples: isize,
        timings: isize,
        timing: *const CMSampleTimingInfo,
        sizes: isize,
        size: *const usize,
        out: *mut *mut CMSampleBuffer,
    ) -> i32;
    fn CMAudioSampleBufferCreateReadyWithPacketDescriptions(
        allocator: *const std::ffi::c_void,
        data: *mut std::ffi::c_void,
        format: *const CMFormatDescription,
        samples: isize,
        time: CMTime,
        packets: *const std::ffi::c_void,
        out: *mut *mut CMSampleBuffer,
    ) -> i32;
}

/// A writer input for sound track `s`, encoding 16-bit PCM it is given.
///
/// # Safety
/// AVFoundation calls with valid arguments, on the writer's thread.
unsafe fn sound_input(s: &Sound) -> Result<Retained<AVAssetWriterInput>, String> {
    let layout = ChannelLayout {
        tag: layout_tag(s.channels),
        bitmap: 0,
        descriptions: 0,
    };
    // SAFETY: the layout's twelve bytes, a plain struct of three u32.
    let layout_bytes = unsafe {
        std::slice::from_raw_parts(
            (&raw const layout).cast::<u8>(),
            std::mem::size_of::<ChannelLayout>(),
        )
    };
    let layout_data = NSData::with_bytes(layout_bytes);
    let format = NSNumber::new_u32(if s.aac { FORMAT_AAC } else { FORMAT_LPCM });
    let rate = NSNumber::new_f64(f64::from(s.rate));
    let channels = NSNumber::new_u32(s.channels);
    let (bits, no) = (NSNumber::new_u32(16), NSNumber::new_bool(false));
    let mut keys = vec![
        NSString::from_str("AVFormatIDKey"),
        NSString::from_str("AVSampleRateKey"),
        NSString::from_str("AVNumberOfChannelsKey"),
        NSString::from_str("AVChannelLayoutKey"),
    ];
    let mut values: Vec<&AnyObject> = vec![
        format.as_ref(),
        rate.as_ref(),
        channels.as_ref(),
        layout_data.as_ref(),
    ];
    if !s.aac {
        for key in [
            "AVLinearPCMIsFloatKey",
            "AVLinearPCMIsBigEndianKey",
            "AVLinearPCMIsNonInterleaved",
        ] {
            keys.push(NSString::from_str(key));
            values.push(no.as_ref());
        }
        keys.push(NSString::from_str("AVLinearPCMBitDepthKey"));
        values.push(bits.as_ref());
    }
    let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let settings = NSDictionary::from_slices(&key_refs, &values);
    // SAFETY: a framework constant, always set on a Mac with AVFoundation.
    let media = unsafe { AVMediaTypeAudio }.ok_or("media type missing")?;
    // SAFETY: valid audio output settings.
    Ok(unsafe {
        AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(media, Some(&settings))
    })
}

/// Frames `from..to` of sound track `track` as a sample buffer of 16-bit PCM.
///
/// # Safety
/// CoreMedia calls with valid arguments; every result is checked.
unsafe fn sound_buffer(
    s: &Sound,
    track: usize,
    from: u64,
    to: u64,
) -> Result<CFRetained<CMSampleBuffer>, String> {
    let mut pcm = Vec::with_capacity((to - from) as usize * s.channels as usize * 2);
    for frame in from..to {
        for channel in 0..s.channels {
            let v = (sound_sample(track, channel, frame, s.rate) * 32767.0).round() as i16;
            pcm.extend_from_slice(&v.to_le_bytes());
        }
    }
    let description = StreamDescription {
        rate: f64::from(s.rate),
        format: FORMAT_LPCM,
        flags: LPCM_FLAGS,
        bytes_per_packet: s.channels * 2,
        frames_per_packet: 1,
        bytes_per_frame: s.channels * 2,
        channels: s.channels,
        bits: 16,
        reserved: 0,
    };
    let layout = ChannelLayout {
        tag: layout_tag(s.channels),
        bitmap: 0,
        descriptions: 0,
    };
    // SAFETY: plain structs passed by pointer with their sizes; the block buffer allocates its
    // own memory (a null block with the default allocator and the assure-memory flag) and the
    // PCM is copied into it; each object made is released when its owner here is dropped.
    unsafe {
        let mut format: *const CMFormatDescription = std::ptr::null();
        let status = CMAudioFormatDescriptionCreate(
            std::ptr::null(),
            &raw const description,
            std::mem::size_of::<ChannelLayout>(),
            &raw const layout,
            0,
            std::ptr::null(),
            std::ptr::null(),
            &raw mut format,
        );
        let format = NonNull::new(format.cast_mut()).ok_or("no sound format")?;
        let format = CFRetained::from_raw(format);
        if status != 0 {
            return Err(format!("sound format error {status}"));
        }
        let mut block: *mut std::ffi::c_void = std::ptr::null_mut();
        let status = CMBlockBufferCreateWithMemoryBlock(
            std::ptr::null(),
            std::ptr::null_mut(),
            pcm.len(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            pcm.len(),
            1,
            &raw mut block,
        );
        if status != 0 || block.is_null() {
            return Err(format!("sound block error {status}"));
        }
        let copied = CMBlockBufferReplaceDataBytes(pcm.as_ptr().cast(), block, 0, pcm.len());
        let mut out: *mut CMSampleBuffer = std::ptr::null_mut();
        let status = if copied == 0 {
            CMAudioSampleBufferCreateReadyWithPacketDescriptions(
                std::ptr::null(),
                block,
                &raw const *format,
                (to - from) as isize,
                CMTime::new(from as i64, s.rate as i32),
                std::ptr::null(),
                &raw mut out,
            )
        } else {
            copied
        };
        CFRelease(block.cast_const());
        let out = NonNull::new(out).ok_or(format!("sound buffer error {status}"))?;
        Ok(CFRetained::from_raw(out))
    }
}

/// A HAP test movie's frames: the flavour, how they are packed, and the first damaged frame.
struct Hap {
    variant: Variant,
    pack: Pack,
    damaged_from: Option<u16>,
}

/// The HAP kind `kind` names (`hap`, `hap-alpha`, `hap-q`, each optionally `@<n>`), if it is one.
fn hap_kind(kind: &str) -> Result<Option<Hap>, String> {
    let (name, damaged_from) = match kind.split_once('@') {
        Some((name, from)) => (
            name,
            Some(from.parse::<u16>().map_err(|_| "bad damaged frame")?),
        ),
        None => (kind, None),
    };
    let (variant, pack) = match name {
        "hap" => (Variant::Hap, Pack::Snappy),
        "hap-alpha" => (Variant::HapAlpha, Pack::Snappy),
        "hap-q" => (Variant::HapQ, Pack::Chunks(4)),
        _ if damaged_from.is_some() => return Err(format!("unknown HAP kind {name}")),
        _ => return Ok(None),
    };
    Ok(Some(Hap {
        variant,
        pack,
        damaged_from,
    }))
}

/// Damages a HAP frame, in one of four ways by frame number: cut to half, a section length
/// longer than the frame, its Snappy data overwritten, or another texture format.
fn damage(bytes: &mut Vec<u8>, frame: u16) {
    match frame % 4 {
        0 => bytes.truncate(bytes.len() / 2),
        1 => {
            if let Some(b) = bytes.get_mut(2) {
                *b = b.wrapping_add(1);
            }
        }
        2 => {
            for b in bytes.iter_mut().skip(8) {
                *b = 0xFF;
            }
        }
        _ => {
            if let Some(kind) = bytes.get_mut(3) {
                *kind ^= 0x01;
            }
        }
    }
}

/// The card for `frame` as straight RGBA, with the right half below the barcode at half alpha
/// when `alpha` (as a straight-alpha file holds it).
fn card_rgba(frame: u16, alpha: bool) -> Vec<u8> {
    let mut px = card::pixels(frame);
    if alpha {
        for (i, p) in px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = (i % card::WIDTH, i / card::WIDTH);
            if x >= card::WIDTH / 2 && y >= card::HEIGHT / 10 {
                p[3] = 128;
            }
        }
    }
    px
}

/// A sample buffer holding HAP frame `frame` of `h`, timed at `fps`.
///
/// # Safety
/// CoreMedia calls with valid arguments; every result is checked.
unsafe fn hap_sample(
    h: &Hap,
    format: &CMFormatDescription,
    frame: u16,
    fps: i32,
) -> Result<CFRetained<CMSampleBuffer>, String> {
    let rgba = card_rgba(frame, h.variant == Variant::HapAlpha);
    let (w, ht) = (card::WIDTH as u32, card::HEIGHT as u32);
    let mut bytes = write::frame(h.variant, w, ht, &rgba, h.pack)?;
    if h.damaged_from.is_some_and(|from| frame >= from) {
        damage(&mut bytes, frame);
    }
    // SAFETY: plain times, and a framework constant.
    let timing = unsafe {
        CMSampleTimingInfo {
            duration: CMTime::new(1, fps),
            presentationTimeStamp: CMTime::new(i64::from(frame), fps),
            decodeTimeStamp: kCMTimeInvalid,
        }
    };
    let size = bytes.len();
    // SAFETY: as `sound_buffer`: a block buffer of its own memory, the frame copied into it,
    // and a sample buffer of one sample of that size and timing; released when dropped here.
    unsafe {
        let mut block: *mut std::ffi::c_void = std::ptr::null_mut();
        let status = CMBlockBufferCreateWithMemoryBlock(
            std::ptr::null(),
            std::ptr::null_mut(),
            size,
            std::ptr::null(),
            std::ptr::null(),
            0,
            size,
            1,
            &raw mut block,
        );
        if status != 0 || block.is_null() {
            return Err(format!("frame block error {status}"));
        }
        let copied = CMBlockBufferReplaceDataBytes(bytes.as_ptr().cast(), block, 0, size);
        let mut out: *mut CMSampleBuffer = std::ptr::null_mut();
        let status = if copied == 0 {
            CMSampleBufferCreateReady(
                std::ptr::null(),
                block,
                format,
                1,
                1,
                &raw const timing,
                1,
                &raw const size,
                &raw mut out,
            )
        } else {
            copied
        };
        CFRelease(block.cast_const());
        let out = NonNull::new(out).ok_or(format!("frame buffer error {status}"))?;
        Ok(CFRetained::from_raw(out))
    }
}

/// A writer input that passes HAP frames of `variant` through as they are, and their format.
///
/// # Safety
/// AVFoundation and CoreMedia calls with valid arguments; every result is checked.
unsafe fn hap_input(
    variant: Variant,
) -> Result<
    (
        Retained<AVAssetWriterInput>,
        CFRetained<CMFormatDescription>,
    ),
    String,
> {
    // SAFETY: a video format description of the card's size for the flavour's code, no
    // extensions; owned from here.
    unsafe {
        let mut format: *const CMFormatDescription = std::ptr::null();
        let status = CMVideoFormatDescriptionCreate(
            std::ptr::null(),
            variant.code(),
            card::WIDTH as i32,
            card::HEIGHT as i32,
            std::ptr::null(),
            &raw mut format,
        );
        let format = NonNull::new(format.cast_mut()).ok_or("no HAP format")?;
        let format = CFRetained::from_raw(format);
        if status != 0 {
            return Err(format!("HAP format error {status}"));
        }
        let media = AVMediaTypeVideo.ok_or("media type missing")?;
        let input =
            AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings_sourceFormatHint(
                media,
                None,
                Some(&format),
            );
        Ok((input, format))
    }
}

fn movie(kind: &str, path: &Path, frames: u16, fps: i32) -> Result<(), String> {
    let mut parts = kind.split('+');
    let kind = parts.next().unwrap_or_default();
    let sounds = sounds(&parts.collect::<Vec<_>>())?;
    let hap = hap_kind(kind)?;
    // SAFETY: framework constants, always set on a Mac with AVFoundation.
    let codec = unsafe {
        match kind {
            "h264" => AVVideoCodecTypeH264,
            "hevc" => AVVideoCodecTypeHEVC,
            "prores" => AVVideoCodecTypeAppleProRes422Proxy,
            "prores4444" => AVVideoCodecTypeAppleProRes4444,
            // HAP passes its frames through; the codec named is not used.
            _ if hap.is_some() => AVVideoCodecTypeH264,
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
        let (input, hap_format) = match &hap {
            Some(h) => {
                let (input, format) = hap_input(h.variant)?;
                (input, Some(format))
            }
            None => (
                AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
                    media,
                    Some(&settings),
                ),
                None,
            ),
        };
        let adaptor = AVAssetWriterInputPixelBufferAdaptor::assetWriterInputPixelBufferAdaptorWithAssetWriterInput_sourcePixelBufferAttributes(&input, None);
        // The container's index first, as a file made for streaming has it: a test can then
        // damage the frames that follow and leave the file readable.
        writer.setShouldOptimizeForNetworkUse(true);
        writer.addInput(&input);
        let mut sound_inputs = Vec::new();
        for s in &sounds {
            let input = sound_input(s)?;
            writer.addInput(&input);
            sound_inputs.push((input, 0u64));
        }
        if !writer.startWriting() {
            return Err(writer_error(&writer));
        }
        writer.startSessionAtSourceTime(CMTime::new(0, fps));
        // Whichever input takes more data is given its next piece (a frame, or a tenth of a
        // second of a sound track): the writer interleaves its tracks and decides itself which
        // may run ahead.
        let total = |s: &Sound| u64::from(frames) * u64::from(s.rate) / fps as u64;
        let mut frame = 0u16;
        let mut idle = 0u32;
        while frame < frames
            || sounds
                .iter()
                .zip(&sound_inputs)
                .any(|(s, (_, d))| *d < total(s))
        {
            let mut fed = false;
            if frame < frames && input.isReadyForMoreMediaData() {
                let appended = match (&hap, &hap_format) {
                    (Some(h), Some(format)) => {
                        let sample = hap_sample(h, format, frame, fps)?;
                        input.appendSampleBuffer(&sample)
                    }
                    _ => {
                        // ProRes 4444 carries alpha: the card's right half below the barcode
                        // at half alpha (straight: the colour itself, as a straight-alpha file
                        // holds it).
                        let mut px = card_rgba(frame, kind == "prores4444");
                        for p in px.as_chunks_mut::<4>().0 {
                            p.swap(0, 2);
                        }
                        let buffer = pixel_buffer(&px)?;
                        adaptor.appendPixelBuffer_withPresentationTime(
                            &buffer,
                            CMTime::new(i64::from(frame), fps),
                        )
                    }
                };
                if !appended {
                    return Err(writer_error(&writer));
                }
                frame += 1;
                if frame == frames {
                    input.markAsFinished();
                }
                fed = true;
            }
            for (track, (s, (sound, done))) in sounds.iter().zip(&mut sound_inputs).enumerate() {
                let end = total(s);
                if *done >= end || !sound.isReadyForMoreMediaData() {
                    continue;
                }
                let to = (*done + u64::from(s.rate) / 10).min(end);
                let buffer = sound_buffer(s, track, *done, to)?;
                if !sound.appendSampleBuffer(&buffer) {
                    return Err(writer_error(&writer));
                }
                *done = to;
                if to == end {
                    sound.markAsFinished();
                }
                fed = true;
            }
            if fed {
                idle = 0;
                continue;
            }
            idle += 1;
            if idle > 10_000 {
                return Err(format!(
                    "the encoder never became ready: {}",
                    writer_error(&writer)
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
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
        "png" | "wide-png" | "tall-png" | "alpha-png" | "tiny-png" => "public.png",
        "jpeg" => "public.jpeg",
        "tiff" => "public.tiff",
        _ => return Err(format!("unknown still format {kind}")),
    };
    let (width, height, rgba) = match kind {
        "wide-png" => bands(400, 100, false),
        "tall-png" => bands(100, 400, true),
        // Left half opaque red, right half transparent.
        "alpha-png" => (
            200,
            100,
            (0..100)
                .flat_map(|_| (0..200).map(|x| if x < 100 { card::RED } else { [0; 4] }))
                .flatten()
                .collect(),
        ),
        // Two pixels: red, blue.
        "tiny-png" => (2, 1, [card::RED, card::BLUE].concat()),
        _ => (card::WIDTH, card::HEIGHT, card::pixels(0)),
    };
    let text = path.to_str().ok_or("path is not Unicode")?;
    let url = CFURL::from_file_path(text).ok_or("bad path")?;
    let data = CFData::from_bytes(&rgba);
    let provider = CGDataProvider::with_cf_data(Some(&data)).ok_or("no data provider")?;
    let space = CGColorSpace::new_device_rgb().ok_or("no colour space")?;
    // SAFETY: an RGBA image over `rgba`'s bytes, written to a new file and finalised.
    unsafe {
        let image = CGImage::new(
            width,
            height,
            8,
            32,
            width * 4,
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

/// A picture of `width` x `height` RGBA pixels in four bands, red, green, blue and yellow:
/// left to right, or top to bottom when `down`.
fn bands(width: usize, height: usize, down: bool) -> (usize, usize, Vec<u8>) {
    let colours = [card::RED, card::GREEN, card::BLUE, card::YELLOW];
    let px = (0..height)
        .flat_map(|y| {
            (0..width).map(move |x| {
                let band = if down { y * 4 / height } else { x * 4 / width };
                colours.get(band).copied().unwrap_or(card::YELLOW)
            })
        })
        .flatten()
        .collect();
    (width, height, px)
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
