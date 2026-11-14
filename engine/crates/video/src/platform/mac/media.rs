// SPDX-License-Identifier: GPL-3.0-or-later

//! macOS: the pictures Video cues play. Every open and decode runs on a decode thread of the
//! picture's own (`openlab-stage-decode`), never on the main thread, which draws every stage
//! and answers the engine's pings: a file on a slow or stalled volume, or one slow to decode,
//! holds up only its own picture.
//!
//! The decode thread owns the file. A still is decoded once with ImageIO; a movie is read in
//! order with AVFoundation's asset reader (the platform's hardware decoders), one BGRA frame at
//! a time. The main thread posts the position it wants (the newest only: an older ask is
//! dropped, never queued) and takes the newest decoded frame from a one-slot mailbox (a newer
//! frame replaces one not yet taken), uploads it to one of the picture's two textures and
//! draws. The frame for a position is the last whose time is at or before it, so the picture
//! follows the engine's clock and never runs ahead of it. A position more than a frame behind
//! the frame held, or far past it, starts reading again there (load to time, a
//! resume after a pause elsewhere); a smaller step back (the clock corrected by a few
//! milliseconds) holds the frame.
//!
//! An open or decode that fails, or does not answer within its limit, fails the picture alone
//! (the main thread finds it when it polls). A decode thread stuck in the platform's code
//! cannot be stopped: it is left to end by itself, and the number of decode threads alive is
//! capped, so a dead volume cannot use up the process.
//!
//! Memory per picture: a still holds its pixels once while decoding and then one texture; a
//! movie at most five frames of its size (two textures, the mailbox, and the decode thread's
//! frame in hand and the one read past it), besides the platform's own decoder buffers. A
//! picture is at most `MAX_PICTURE_PIXELS` (checked before anything of its size is allocated).
//!
//! The file was probed in a process of its own before it gets here (`--probe`); a read that
//! still fails here is reported, and the picture goes.

use std::ffi::c_void;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_av_foundation::{
    AVAssetReader, AVAssetReaderStatus, AVAssetReaderTrackOutput, AVAssetTrack, AVMediaTypeVideo,
    AVURLAsset,
};
use objc2_core_foundation::{CFRetained, CFString, CFURL, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapInfo, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
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
use objc2_metal::{MTLCommandBuffer, MTLCommandBufferStatus, MTLDevice};
use openlab_core::stage_link::MAX_PICTURES;
use openlab_core::video_probe::{MAX_PICTURE_PIXELS, picture_size_plays};

use super::gpu::{Texture, empty_texture, replace};

/// How far past the frame held a position may be before reading starts again there instead of
/// reading every frame in between (a jump, not playback).
const SKIP_AHEAD: f64 = 2.0;

/// A step back of up to this many frames holds the frame shown instead of reading again from
/// the keyframe before it: the clock the picture follows is corrected backwards by a few
/// milliseconds now and then, and that must never restart the reader or show an earlier frame.
const BACK_HOLD_FRAMES: f64 = 1.0;

/// Decode threads alive at once, stuck ones included: past this a new picture is refused
/// instead of starting another thread.
const MAX_DECODERS: usize = 2 * MAX_PICTURES;

/// Decode threads alive now.
static DECODERS: AtomicUsize = AtomicUsize::new(0);

/// How long an open, and a frame asked for, may take before the picture fails.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub open: Duration,
    pub frame: Duration,
}

/// The limits a show runs with: an open on a slow disk (a 2014 Mac with a hard disk reading a
/// movie's index while other cues stream) is well under 10 s, and a frame, even read from the
/// keyframe before a jump, well under 5 s. A picture past them is on a stalled volume or a
/// decoder that hung.
pub const LIMITS: Limits = Limits {
    open: Duration::from_secs(10),
    frame: Duration::from_secs(5),
};

/// How far ahead of the clock a picture drawn on a real display is asked for: it is shown at
/// the display's next refresh.
pub const FRAME_AHEAD: f64 = 1.0 / 60.0;

/// A texture to draw, and its picture's pixel size.
pub type Drawable = (Texture, (u32, u32));

/// What the main thread learns of a picture when it polls.
pub enum Polled {
    Opened,
    Failed(String),
}

/// One decoded frame: its time in the movie (0 for a still), its size, and its BGRA pixels in
/// rows of `stride` bytes.
struct Frame {
    time: f64,
    size: (u32, u32),
    pixels: Vec<u8>,
    stride: usize,
}

/// What the main thread and a picture's decode thread share.
#[derive(Default)]
struct Mail {
    /// Main to decoder: the newest position wanted, numbered.
    want: Option<(u64, f64)>,
    /// The number of the last position the decoder answered.
    answered: u64,
    /// Since when a position has been wanted and not answered.
    waiting_since: Option<Instant>,
    /// The picture's size, once its file is open.
    opened: Option<(u32, u32)>,
    /// Decoder to main: the newest frame not yet taken.
    frame: Option<Frame>,
    failed: Option<String>,
    /// The main thread let the picture go: the decode thread ends.
    closed: bool,
}

struct Shared {
    mail: Mutex<Mail>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Mail> {
        // A decode thread that panicked leaves the mail as it was; it is still read.
        self.mail.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn fail(&self, why: String) {
        let mut mail = self.lock();
        mail.failed.get_or_insert(why);
        self.wake.notify_all();
    }
}

/// One of a picture's two textures, and the command buffer that last drew from it: a texture
/// is written only when that buffer is done with it, so a frame never changes under the GPU.
struct Slot {
    texture: Texture,
    used_by: Option<Retained<ProtocolObject<dyn MTLCommandBuffer>>>,
}

impl Slot {
    fn free(&self) -> bool {
        self.used_by.as_ref().is_none_or(|cb| {
            matches!(
                cb.status(),
                MTLCommandBufferStatus::Completed | MTLCommandBufferStatus::Error
            )
        })
    }
}

/// A picture as the main thread holds it.
pub struct Picture {
    shared: Arc<Shared>,
    still: bool,
    started: Instant,
    told_open: bool,
    slots: [Option<Slot>; 2],
    /// The slot drawn from, once one holds a frame.
    front: Option<usize>,
    /// The number of the last position posted, and the position.
    asked: (u64, Option<f64>),
}

/// Ends the count of a decode thread when it ends, however it ends.
struct Alive;

impl Drop for Alive {
    fn drop(&mut self) {
        DECODERS.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Picture {
    /// Starts opening `file` on a decode thread of its own; returns at once. A movie's open
    /// includes decoding its frame at `offset` seconds, so a file that cannot be decoded (or
    /// crashes the decoder) does so before the picture is said to be open.
    pub fn open(file: &str, still: bool, offset: f64) -> Result<Self, String> {
        if DECODERS.fetch_add(1, Ordering::AcqRel) >= MAX_DECODERS {
            DECODERS.fetch_sub(1, Ordering::AcqRel);
            return Err("too many pictures are still being read".into());
        }
        let alive = Alive;
        let shared = Arc::new(Shared {
            mail: Mutex::new(Mail::default()),
            wake: Condvar::new(),
        });
        let theirs = Arc::clone(&shared);
        let file = file.to_owned();
        std::thread::Builder::new()
            .name("openlab-stage-decode".into())
            .spawn(move || {
                let _alive = alive;
                decode(&theirs, &file, still, offset);
            })
            .map_err(|e| format!("no thread to read the file ({e})"))?;
        Ok(Self {
            shared,
            still,
            started: Instant::now(),
            told_open: false,
            slots: [None, None],
            front: None,
            asked: (0, None),
        })
    }

    /// Whether the file opened (said once), or the picture failed: its open or decode failed,
    /// or took longer than `limits`.
    pub fn poll(&mut self, limits: Limits) -> Option<Polled> {
        let mail = self.shared.lock();
        if let Some(why) = &mail.failed {
            return Some(Polled::Failed(why.clone()));
        }
        if mail.opened.is_none() {
            return (self.started.elapsed() >= limits.open).then(|| {
                Polled::Failed(format!(
                    "its file took longer than {} s to open",
                    limits.open.as_secs_f64()
                ))
            });
        }
        if mail
            .waiting_since
            .is_some_and(|t| t.elapsed() >= limits.frame)
        {
            return Some(Polled::Failed(format!(
                "a frame took longer than {} s to decode",
                limits.frame.as_secs_f64()
            )));
        }
        if self.told_open {
            return None;
        }
        self.told_open = true;
        Some(Polled::Opened)
    }

    /// The texture to draw at `position` seconds and its pixel size, if a frame has been
    /// decoded: the newest the decode thread has, uploaded when a texture is free. Asks the
    /// decode thread for `position`. With `wait` (offscreen probes only, never on real
    /// displays), waits within `limits` for the frame at `position` first.
    pub fn frame(
        &mut self,
        device: &ProtocolObject<dyn MTLDevice>,
        position: f64,
        wait: Option<Limits>,
    ) -> Result<Option<Drawable>, String> {
        let mut mail = self.shared.lock();
        if !self.still && self.asked.1 != Some(position) {
            self.asked = (self.asked.0 + 1, Some(position));
            mail.want = Some((self.asked.0, position));
            mail.waiting_since.get_or_insert_with(Instant::now);
            self.shared.wake.notify_all();
        }
        if let Some(limits) = wait {
            let deadline = self.started + limits.open;
            while mail.failed.is_none() && mail.opened.is_none() {
                let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                    break;
                };
                mail = self.wait(mail, left);
            }
            if !self.still && mail.opened.is_some() {
                let asked = self.asked.0;
                let deadline = mail.waiting_since.unwrap_or_else(Instant::now) + limits.frame;
                while mail.failed.is_none() && mail.answered < asked {
                    let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                        break;
                    };
                    mail = self.wait(mail, left);
                }
            }
        }
        if let Some(why) = &mail.failed {
            return Err(why.clone());
        }
        let back = match self.front {
            None => 0,
            Some(front) => 1 - front.min(1),
        };
        let free = self
            .slots
            .get(back)
            .and_then(Option::as_ref)
            .is_none_or(Slot::free);
        let frame = if free { mail.frame.take() } else { None };
        drop(mail);
        if let Some(mut frame) = frame {
            self.upload(device, back, &mut frame)?;
            self.front = Some(back);
        }
        Ok(self
            .front
            .and_then(|f| self.slots.get(f))
            .and_then(Option::as_ref)
            .zip(self.size())
            .map(|(slot, size)| (slot.texture.clone(), size)))
    }

    fn wait<'a>(&self, mail: MutexGuard<'a, Mail>, left: Duration) -> MutexGuard<'a, Mail> {
        self.shared
            .wake
            .wait_timeout(mail, left)
            .map_or_else(|e| e.into_inner().0, |(m, _)| m)
    }

    fn size(&self) -> Option<(u32, u32)> {
        self.shared.lock().opened
    }

    /// Writes `frame` into slot `slot`, making its texture first if it has none.
    fn upload(
        &mut self,
        device: &ProtocolObject<dyn MTLDevice>,
        slot: usize,
        frame: &mut Frame,
    ) -> Result<(), String> {
        let Some(place) = self.slots.get_mut(slot) else {
            return Err("no texture slot".into());
        };
        if place.is_none() {
            let (w, h) = frame.size;
            *place = Some(Slot {
                texture: empty_texture(device, w as usize, h as usize)?,
                used_by: None,
            });
        }
        let Some(place) = place.as_mut() else {
            return Err("no texture slot".into());
        };
        replace(&place.texture, frame.size, &mut frame.pixels, frame.stride)?;
        place.used_by = None;
        Ok(())
    }

    /// The texture drawn from was drawn by `cb`: it is not written again until `cb` is done.
    pub fn drawn_with(&mut self, cb: &Retained<ProtocolObject<dyn MTLCommandBuffer>>) {
        if let Some(slot) = self
            .front
            .and_then(|f| self.slots.get_mut(f))
            .and_then(Option::as_mut)
        {
            slot.used_by = Some(cb.clone());
        }
    }

    /// The position last asked, for a test of what the decode thread was asked.
    #[cfg(test)]
    fn asked(&self) -> Option<f64> {
        self.asked.1
    }
}

impl Drop for Picture {
    fn drop(&mut self) {
        let mut mail = self.shared.lock();
        mail.closed = true;
        mail.frame = None;
        self.shared.wake.notify_all();
    }
}

/// The decode thread: opens the file (a movie's frame at `offset` decoded), then answers each
/// position the main thread wants with its frame until the picture is let go.
fn decode(shared: &Shared, file: &str, still: bool, offset: f64) {
    #[cfg(feature = "test-support")]
    fault::hang_if(shared, file, fault::HANG_OPEN);
    if still {
        match still_frame(file) {
            Ok(frame) => {
                let mut mail = shared.lock();
                mail.opened = Some(frame.size);
                mail.frame = Some(frame);
                shared.wake.notify_all();
            }
            Err(why) => shared.fail(why),
        }
        return;
    }
    #[cfg(feature = "test-support")]
    fault::crash_if(file, fault::CRASH_OPEN);
    let mut movie = match Movie::open(file) {
        Ok(m) => m,
        Err(why) => return shared.fail(why),
    };
    let first = match movie.frame_at(offset) {
        Ok(frame) => frame,
        Err(why) => return shared.fail(why),
    };
    {
        let mut mail = shared.lock();
        mail.opened = Some(movie.size);
        mail.frame = first;
        shared.wake.notify_all();
    }
    loop {
        let (asked, position) = {
            let mut mail = shared.lock();
            loop {
                if mail.closed {
                    return;
                }
                match mail.want {
                    Some((n, p)) if n > mail.answered => break (n, p),
                    _ => {
                        mail = shared
                            .wake
                            .wait(mail)
                            .unwrap_or_else(PoisonError::into_inner);
                    }
                }
            }
        };
        #[cfg(feature = "test-support")]
        fault::hang_if(shared, file, fault::HANG_DECODE);
        #[cfg(feature = "test-support")]
        fault::crash_if(file, fault::CRASH_DECODE);
        match movie.frame_at(position) {
            Ok(frame) => {
                let mut mail = shared.lock();
                if frame.is_some() {
                    mail.frame = frame;
                }
                mail.answered = asked;
                mail.waiting_since = match mail.want {
                    Some((n, _)) if n > asked => Some(Instant::now()),
                    _ => None,
                };
                shared.wake.notify_all();
            }
            Err(why) => return shared.fail(why),
        }
    }
}

/// Decodes a still (PNG, JPEG, TIFF) into BGRA pixels.
fn still_frame(file: &str) -> Result<Frame, String> {
    let url = CFURL::from_file_path(file).ok_or("the path cannot be read")?;
    // SAFETY: ImageIO reads the file; every result is checked before use. The image is
    // decoded lazily: nothing of its size is allocated until it is drawn below.
    let image = unsafe {
        CGImageSource::with_url(&url, None)
            .and_then(|s| s.image_at_index(0, None))
            .ok_or("the image cannot be decoded")?
    };
    let (w, h) = (CGImage::width(Some(&image)), CGImage::height(Some(&image)));
    let size = picture_size(w, h)?;
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
    Ok(Frame {
        time: 0.0,
        size,
        pixels: px,
        stride: w * 4,
    })
}

/// A picture's size if it is within the limits, else why not.
fn picture_size(w: usize, h: usize) -> Result<(u32, u32), String> {
    match (u32::try_from(w), u32::try_from(h)) {
        (Ok(width), Ok(height)) if picture_size_plays(width, height) => Ok((width, height)),
        _ => Err(format!(
            "the picture is {w} x {h} pixels; at most {MAX_PICTURE_PIXELS} pixels in all play"
        )),
    }
}

/// A movie read in order on its decode thread.
struct Movie {
    asset: Retained<AVURLAsset>,
    track: Retained<AVAssetTrack>,
    reader: Option<(Retained<AVAssetReader>, Retained<AVAssetReaderTrackOutput>)>,
    size: (u32, u32),
    /// The time of the frame last handed out, if one was.
    shown: Option<f64>,
    /// The frame read past the position last asked for.
    next: Option<Frame>,
    /// The reader has no frame after `next`.
    ended: bool,
    /// The movie's length and one frame's time, in seconds, as its file says.
    length: f64,
    frame: f64,
    /// Times reading started again (tests).
    #[cfg(test)]
    restarts: u32,
}

impl Movie {
    fn open(file: &str) -> Result<Self, String> {
        if !Path::new(file).is_file() {
            return Err("the file is not there".into());
        }
        let url = NSURL::fileURLWithPath(&NSString::from_str(file));
        // SAFETY: a file URL and no options; the tracks are read synchronously, on this
        // picture's own decode thread.
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
        if !(w >= 1.0 && h >= 1.0 && w <= f64::from(u32::MAX) && h <= f64::from(u32::MAX)) {
            return Err("the picture size cannot be read".into());
        }
        let size = picture_size(w as usize, h as usize)?;
        // SAFETY: plain properties of a loaded asset and track.
        let (length, fps) = unsafe { (asset.duration().seconds(), track.nominalFrameRate()) };
        if !(length.is_finite() && length > 0.0 && fps.is_finite() && fps > 0.0) {
            return Err("the movie's length or frame rate cannot be read".into());
        }
        Ok(Self {
            asset,
            track,
            reader: None,
            size,
            shown: None,
            next: None,
            ended: false,
            length,
            frame: 1.0 / f64::from(fps),
            #[cfg(test)]
            restarts: 0,
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
        #[cfg(test)]
        {
            self.restarts += 1;
        }
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

    /// Reads the next frame; `None` at the end of the movie.
    fn read(&mut self) -> Result<Option<Frame>, String> {
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
            let bytes = if base.is_null() || stride < w * 4 || stride > w * 4 + 4096 {
                None
            } else {
                Some(std::slice::from_raw_parts(base, stride * h).to_vec())
            };
            CVPixelBufferUnlockBaseAddress(&image, CVPixelBufferLockFlags::ReadOnly);
            let pixels = bytes.ok_or("a frame could not be read")?;
            if !time.is_finite() {
                return Err("a frame has no time".into());
            }
            Ok(Some(Frame {
                time,
                size: self.size,
                pixels,
                stride,
            }))
        }
    }

    /// The frame at `position` seconds: the last whose time is at or before it (the first
    /// frame before the first frame's time); `None` when the frame handed out last is still the
    /// one (a step back of up to [`BACK_HOLD_FRAMES`] included).
    fn frame_at(&mut self, position: f64) -> Result<Option<Frame>, String> {
        let back = self
            .shown
            .is_some_and(|t| position < t - BACK_HOLD_FRAMES * self.frame);
        let far = self.shown.is_some_and(|t| position > t + SKIP_AHEAD);
        if self.reader.is_none() || back || far {
            self.restart(position)?;
            self.shown = None;
        }
        let mut best: Option<Frame> = None;
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
            if frame.time <= position || (self.shown.is_none() && best.is_none()) {
                best = Some(frame);
            } else {
                self.next = Some(frame);
                break;
            }
        }
        if let Some(f) = &best {
            self.shown = Some(f.time);
        }
        // The frames ran out well before the movie's end: its file is damaged or was cut short
        // under it. Holding the last frame would freeze the picture; the cue breaks instead.
        let short = self.ended
            && position < self.length - 2.0 * self.frame
            && self.shown.is_none_or(|t| position > t + 2.0 * self.frame);
        if short {
            return Err("the movie stops before its end (the file may be damaged)".into());
        }
        Ok(best)
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

/// Test support: faults a decode thread runs into when its file's name asks for one, so tests
/// can make an open or a frame decode hang (until the picture is let go) or crash the stage
/// host. Never in a shipped stage host.
#[cfg(feature = "test-support")]
pub mod fault {
    use super::Shared;
    use std::sync::PoisonError;

    pub const HANG_OPEN: &str = "openlab-test-hang-open";
    pub const HANG_DECODE: &str = "openlab-test-hang-decode";
    /// Crashes while the file opens (its first frame decoded).
    pub const CRASH_OPEN: &str = "openlab-test-crash-open";
    /// Crashes at the first frame asked for after the open.
    pub const CRASH_DECODE: &str = "openlab-test-crash-decode";

    pub(super) fn hang_if(shared: &Shared, file: &str, fault: &str) {
        if !file.contains(fault) {
            return;
        }
        let mut mail = shared.lock();
        while !mail.closed {
            mail = shared
                .wake
                .wait(mail)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    pub(super) fn crash_if(file: &str, fault: &str) {
        if file.contains(fault) {
            std::process::abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture whose file is not there fails on its own thread, reported when polled; the
    /// main thread is never held.
    #[test]
    fn an_open_that_fails_is_reported_when_polled() {
        let mut p = Picture::open("/nonexistent/openlab/clip.mov", false, 0.0).unwrap();
        let limits = Limits {
            open: Duration::from_secs(30),
            frame: Duration::from_secs(30),
        };
        let device = objc2_metal::MTLCreateSystemDefaultDevice().unwrap();
        let got = p.frame(&device, 0.0, Some(limits));
        assert_eq!(got.err().as_deref(), Some("the file is not there"));
        assert!(matches!(p.poll(limits), Some(Polled::Failed(_))));
        assert_eq!(p.asked(), Some(0.0));
    }

    /// The frame number a decoded test-card frame carries (its time at 25 fps).
    fn number(frame: &Frame) -> u16 {
        (frame.time * 25.0).round() as u16
    }

    /// A clock stepped back by a few milliseconds across a frame boundary holds the frame shown
    /// and does not read again; a step back of more than a frame does read again, from there.
    #[test]
    fn a_small_step_back_holds_the_frame_and_a_jump_back_reads_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mov");
        let file = path.to_str().unwrap().to_owned();
        super::super::testmedia::make(&["h264".into(), file.clone(), "50".into(), "25".into()])
            .unwrap();
        let mut m = Movie::open(&file).unwrap();
        // The reviewer's sequence at 48 kHz: sample 1970 (41.0 ms, frame 1), then 1826 (38.0
        // ms, 3 ms back across the boundary).
        let f = m.frame_at(1970.0 / 48_000.0).unwrap().unwrap();
        assert_eq!(number(&f), 1);
        let restarts = m.restarts;
        assert!(m.frame_at(1826.0 / 48_000.0).unwrap().is_none(), "held");
        assert_eq!(m.restarts, restarts, "the reader was not started again");
        // Forward again: the next frame, still without a restart.
        let f = m.frame_at(3890.0 / 48_000.0).unwrap().unwrap();
        assert_eq!(number(&f), 2);
        assert_eq!(m.restarts, restarts);
        // A real jump back (load to time) reads again from there.
        let f = m.frame_at(0.0).unwrap().unwrap();
        assert_eq!(number(&f), 0);
        assert_eq!(m.restarts, restarts + 1);
    }
}
