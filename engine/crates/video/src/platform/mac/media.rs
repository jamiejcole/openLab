// SPDX-License-Identifier: GPL-3.0-or-later

//! macOS: the pictures Video cues play. Every open and decode runs on a decode thread of the
//! picture's own (`openlab-stage-decode`), never on the main thread, which draws every stage
//! and answers the engine's pings: a file on a slow or stalled volume, or one slow to decode,
//! holds up only its own picture.
//!
//! The decode thread owns the file. A still is decoded once with ImageIO; a movie is read in
//! order with AVFoundation's asset reader (the platform's hardware decoders), one BGRA frame at
//! a time. A HAP movie's frames are read as the file holds them (no platform decoder) and
//! unwrapped here ([`crate::hap`]) into the block-compressed texture the GPU draws as it is. The main thread posts the position it wants (the newest only: an older ask is
//! dropped, never queued) and takes the newest decoded frame from a one-slot mailbox (a newer
//! frame replaces one not yet taken), uploads it to one of the picture's two textures and
//! draws. The frame for a position is the last whose time is at or before it, so the picture
//! follows the engine's clock and never runs ahead of it. A position more than a frame behind
//! the frame held, or far past it, starts reading again there (load to time, a
//! resume after a pause elsewhere); a smaller step back (the clock corrected by a few
//! milliseconds) holds the frame.
//!
//! An open or decode that fails, or does not answer within its limit, fails the picture alone
//! (the main thread finds it when it polls). A movie whose reader fails after its open (a
//! decoder fault, a media-services reset) is first opened again, once, on its decode thread at
//! the newest position asked ([`REOPEN_SETTLE`]); only if that fails too does the picture fail,
//! so a damaged file is still reported. A decode thread stuck in the platform's code
//! cannot be stopped: it is left to end by itself, and the number of decode threads alive is
//! capped, so a dead volume cannot use up the process.
//!
//! A movie that loops (a play count or an infinite loop over its trims, VID-07) jumps back at
//! each seam. The main thread passes the jump on with the position it asks for, up to a second
//! before the seam: the decode thread then opens a second reader at the loop's start (a frame
//! early, passing over without copying any frame before the one on show there) and reads that
//! frame ahead (`prime`), after answering the frame asked for, and swaps it in at the seam
//! instead of opening one then. Until the next pass's first frame is ready the frame shown
//! stays up (the main thread never waits and never draws a picture without a frame), so a seam
//! is never a black frame.
//!
//! Memory per picture: a still holds its pixels once while decoding and then one texture; a
//! movie at most six frames of its size (two textures, the mailbox, the decode thread's frame
//! in hand, the one read past it, and a looping movie's next pass's first frame), besides the
//! platform's own decoder buffers. A picture is at most `MAX_PICTURE_PIXELS` (checked before
//! anything of its size is allocated).
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
use objc2_core_media::{CMFormatDescription, CMSampleBuffer};
use objc2_core_media::{CMTime, CMTimeRange};
use objc2_core_video::{
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
    CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress, kCVImageBufferAlphaChannelMode_StraightAlpha,
    kCVImageBufferAlphaChannelModeKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_32BGRA,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};
use objc2_image_io::CGImageSource;
use objc2_metal::{
    MTLCommandBuffer, MTLCommandBufferStatus, MTLDevice, MTLPixelFormat, MTLTexture,
};
use openlab_core::stage_link::MAX_PICTURES;
use openlab_core::video_probe::{MAX_PICTURE_PIXELS, picture_size_plays};

use super::gpu::{Colour, Texture, empty_texture, hap_texture, replace};
use crate::hap::{self, Variant};

/// How far past the frame held a position may be before reading starts again there instead of
/// reading every frame in between (a jump, or a decoder that fell behind: it skips to where the
/// clock is rather than showing every late frame).
const SKIP_AHEAD: f64 = 0.5;

/// A step back of up to this many frames holds the frame shown instead of reading again from
/// the keyframe before it: the clock the picture follows is corrected backwards by a few
/// milliseconds now and then, and that must never restart the reader or show an earlier frame.
const BACK_HOLD_FRAMES: f64 = 1.0;

/// A movie whose reader failed and was opened again may be opened again at a later failure
/// only after reading this many seconds of its frames without one: a fault that comes back at
/// once (a damaged part of the file, a decoder that keeps failing) breaks the cue at its second
/// failure instead of re-opening the movie over and over.
pub const REOPEN_SETTLE: f64 = 10.0;

/// What an open is doing, named in the reason it failed when it took too long: a stall on a
/// show machine (a volume, the system's media services, the decoder) says where it was.
const READING_STILL: &str = "reading the image";
const READING_TRACKS: &str = "reading the movie's tracks";
const DECODING_FIRST: &str = "decoding its first frame";

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

/// Rounding allowed when a frame's time plus the frame period is compared with a place: far
/// below `HOLD_BEFORE_END`, so a held end, a microsecond before the next frame, never counts
/// as that frame's. Too little only costs a frame copied that is then passed over.
const FRAME_TIME_SLACK: f64 = 1e-9;

/// How far ahead of the clock a picture drawn on a real display is asked for: it is shown at
/// the display's next refresh.
pub const FRAME_AHEAD: f64 = 1.0 / 60.0;

/// A texture to draw, its picture's pixel size, and how it holds its colour.
pub type Drawable = (Texture, (u32, u32), Colour);

/// How long an offscreen probe waits between looks at a picture's decode thread.
const WAIT_SLICE: Duration = Duration::from_millis(5);
/// The most real time an offscreen probe waits for one picture, whatever the media clock does
/// (longer when the picture's open and frame limits together are longer: test support).
const PROBE_GUARD: Duration = Duration::from_secs(60);

/// The time the media limits are measured on: the system's, or (test support, with
/// `--media-clock-by-hand`) a clock a test moves.
fn now() -> Instant {
    #[cfg(feature = "test-support")]
    if let Some(t) = fault::hand_now() {
        return t;
    }
    Instant::now()
}

/// Why a picture whose frame overran `limits` failed.
fn frame_overrun(limits: Limits) -> String {
    format!(
        "a frame took longer than {} s to decode",
        limits.frame.as_secs_f64()
    )
}

/// How long it is on the media clock since `t`.
fn since(t: Instant) -> Duration {
    now().saturating_duration_since(t)
}

/// What the main thread learns of a picture when it polls.
pub enum Polled {
    Opened,
    Failed(String),
}

/// One decoded frame: its time in the movie (0 for a still), its size, and its BGRA pixels in
/// rows of `stride` bytes, or (`hap`) a HAP flavour's texture in rows of 4 x 4 blocks.
struct Frame {
    time: f64,
    size: (u32, u32),
    pixels: Vec<u8>,
    stride: usize,
    hap: Option<Variant>,
}

impl Frame {
    /// The texture format the frame is uploaded into, and how it holds its colour.
    fn texture(&self) -> (MTLPixelFormat, Colour) {
        self.hap.map_or(
            (MTLPixelFormat::BGRA8Unorm, Colour::Premultiplied),
            hap_texture,
        )
    }
}

/// What the main thread and a picture's decode thread share.
#[derive(Default)]
struct Mail {
    /// Main to decoder: the newest position wanted, numbered, and the pass of the picture's plan
    /// it is in (a new pass is a seam: the reader starts again there however near it is).
    want: Option<(u64, f64, u64)>,
    /// Main to decoder: where the picture jumps to when the pass it is in ends (a loop's start),
    /// once that is near.
    upcoming: Option<f64>,
    /// The number of the last position the decoder answered.
    answered: u64,
    /// Since when a position has been wanted and not answered.
    waiting_since: Option<Instant>,
    /// The picture's size, once its file is open.
    opened: Option<(u32, u32)>,
    /// Decoder to main: the newest frame not yet taken.
    frame: Option<Frame>,
    failed: Option<String>,
    /// Decoder to main: what the open is doing, for the reason given when it takes too long.
    opening: &'static str,
    /// Decoder to main: the movie was opened again after its reader failed, and why (for the
    /// show log), not yet taken.
    reopened: Option<String>,
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
    colour: Colour,
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
    asked: (u64, Option<(f64, u64)>),
    /// The last jump passed on.
    told_upcoming: Option<f64>,
    /// How long its open and frames may take.
    limits: Limits,
    /// Test support: its file's name asks its decoder to hang, so a probe does not wait for it.
    hangs: bool,
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
    pub fn open(file: &str, still: bool, offset: f64, limits: Limits) -> Result<Self, String> {
        if DECODERS.fetch_add(1, Ordering::AcqRel) >= MAX_DECODERS {
            DECODERS.fetch_sub(1, Ordering::AcqRel);
            return Err("too many pictures are still being read".into());
        }
        let alive = Alive;
        let shared = Arc::new(Shared {
            mail: Mutex::new(Mail {
                opening: if still { READING_STILL } else { READING_TRACKS },
                ..Mail::default()
            }),
            wake: Condvar::new(),
        });
        let theirs = Arc::clone(&shared);
        #[cfg(feature = "test-support")]
        let hangs = [
            fault::HANG_OPEN,
            fault::HANG_FIRST_FRAME,
            fault::HANG_DECODE,
        ]
        .iter()
        .any(|f| file.contains(f));
        #[cfg(not(feature = "test-support"))]
        let hangs = false;
        let file = file.to_owned();
        let started = now();
        std::thread::Builder::new()
            .name("openlab-stage-decode".into())
            .spawn(move || {
                let _alive = alive;
                decode(&theirs, &file, still, offset, limits, started);
            })
            .map_err(|e| format!("no thread to read the file ({e})"))?;
        Ok(Self {
            shared,
            still,
            started,
            told_open: false,
            slots: [None, None],
            front: None,
            asked: (0, None),
            told_upcoming: None,
            limits,
            hangs,
        })
    }

    /// Whether the file opened (said once), or the picture failed: its open or decode failed,
    /// or took longer than `limits`.
    pub fn poll(&mut self) -> Option<Polled> {
        let limits = self.limits;
        let mail = self.shared.lock();
        if let Some(why) = &mail.failed {
            return Some(Polled::Failed(why.clone()));
        }
        if mail.opened.is_none() {
            return (since(self.started) >= limits.open).then(|| {
                Polled::Failed(format!(
                    "its file took longer than {} s to open ({})",
                    limits.open.as_secs_f64(),
                    mail.opening
                ))
            });
        }
        if mail.waiting_since.is_some_and(|t| since(t) >= limits.frame) {
            return Some(Polled::Failed(frame_overrun(limits)));
        }
        if self.told_open {
            return None;
        }
        self.told_open = true;
        Some(Polled::Opened)
    }

    /// Why the movie was opened again after its reader failed, once, if it was since the last
    /// call: one line for the show log.
    pub fn take_reopened(&mut self) -> Option<String> {
        self.shared.lock().reopened.take()
    }

    /// The texture to draw at `position` seconds and its pixel size, if a frame has been
    /// decoded: the newest the decode thread has, uploaded when a texture is free. Asks the
    /// decode thread for `position` (in its plan's pass `pass`), and tells it where the picture
    /// jumps next (`upcoming`, a
    /// loop's start near the end of a pass) so it can have that frame ready. With `wait`
    /// (offscreen probes only, never on real displays), waits within `limits` for the frame at
    /// `position` first.
    pub fn frame(
        &mut self,
        device: &ProtocolObject<dyn MTLDevice>,
        position: f64,
        pass: u64,
        upcoming: Option<f64>,
        wait: bool,
    ) -> Result<Option<Drawable>, String> {
        let mut mail = self.shared.lock();
        if !self.still && upcoming != self.told_upcoming {
            self.told_upcoming = upcoming;
            mail.upcoming = upcoming;
            self.shared.wake.notify_all();
        }
        if !self.still && self.asked.1 != Some((position, pass)) {
            self.asked = (self.asked.0 + 1, Some((position, pass)));
            mail.want = Some((self.asked.0, position, pass));
            mail.waiting_since.get_or_insert_with(now);
            self.shared.wake.notify_all();
        }
        if wait && !self.hangs {
            let limits = self.limits;
            // On the media clock (moved by hand in some tests), looked at every few
            // milliseconds; `PROBE_GUARD` of real time (or the limits together, if longer)
            // bounds the wait whatever that clock does.
            let guard = Instant::now() + PROBE_GUARD.max(limits.open.saturating_add(limits.frame));
            while mail.failed.is_none() && mail.opened.is_none() {
                if since(self.started) >= limits.open || Instant::now() >= guard {
                    break;
                }
                mail = self.wait(mail, WAIT_SLICE);
            }
            if !self.still && mail.opened.is_some() {
                let asked = self.asked.0;
                // The limit runs from the decode thread's last frame: a slow one that keeps
                // decoding is waited for.
                while mail.failed.is_none() && mail.answered < asked {
                    if mail.waiting_since.is_some_and(|t| since(t) >= limits.frame)
                        || Instant::now() >= guard
                    {
                        break;
                    }
                    mail = self.wait(mail, WAIT_SLICE);
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
            .map(|(slot, size)| (slot.texture.clone(), size, slot.colour)))
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
        let (format, colour) = frame.texture();
        if place
            .as_ref()
            .is_none_or(|s| s.texture.pixelFormat() != format)
        {
            let (w, h) = frame.size;
            *place = Some(Slot {
                texture: empty_texture(device, w as usize, h as usize, format)?,
                colour,
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
        self.asked.1.map(|(p, _)| p)
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
/// position the main thread wants with its frame until the picture is let go. `started` is
/// when the load was taken (the media clock then).
fn decode(shared: &Shared, file: &str, still: bool, offset: f64, limits: Limits, started: Instant) {
    #[cfg(not(feature = "test-support"))]
    let _ = started;
    #[cfg(feature = "test-support")]
    {
        fault::hang_if(shared, file, fault::HANG_OPEN);
        fault::slow_open_if(shared, file, started);
    }
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
    // Each frame read is progress: the frame limit times a decoder that stopped, not a
    // catch-up of many frames.
    // A frame read after the limit had passed since the last one is too late: the picture
    // fails, as the main thread would have found when it polled in between.
    let progress = || {
        let mut mail = shared.lock();
        match mail.waiting_since {
            Some(t) if since(t) >= limits.frame => {
                mail.failed.get_or_insert_with(|| frame_overrun(limits));
                shared.wake.notify_all();
            }
            Some(_) => mail.waiting_since = Some(now()),
            None => {}
        }
    };
    #[cfg(feature = "test-support")]
    {
        movie.slow = file.contains(fault::SLOW_DECODE);
    }
    shared.lock().opening = DECODING_FIRST;
    #[cfg(feature = "test-support")]
    {
        shared.wake.notify_all();
        fault::hang_if(shared, file, fault::HANG_FIRST_FRAME);
    }
    // The open's own frame is under the open's limit, not the frames'.
    let first = match movie.frame_at(offset, 0, &|| {}) {
        Ok(frame) => frame,
        Err(why) => return shared.fail(why),
    };
    {
        let mut mail = shared.lock();
        mail.opened = Some(movie.size);
        mail.frame = first;
        // A position asked while the file opened is timed from now: the open had its own limit.
        if mail.waiting_since.is_some() {
            mail.waiting_since = Some(now());
        }
        shared.wake.notify_all();
    }
    loop {
        let (asked, position, pass) = {
            let mut mail = shared.lock();
            loop {
                if mail.closed {
                    return;
                }
                match mail.want {
                    Some((n, p, pass)) if n > mail.answered => break (n, p, pass),
                    _ => {}
                }
                // Nothing asked: the next pass's first frame is read ahead, if a jump is near.
                if let Some(next) = mail.upcoming
                    && !movie.primed(next)
                {
                    drop(mail);
                    movie.prime(next);
                    mail = shared.lock();
                    continue;
                }
                mail = shared
                    .wake
                    .wait(mail)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        #[cfg(feature = "test-support")]
        fault::hang_if(shared, file, fault::HANG_DECODE);
        #[cfg(feature = "test-support")]
        fault::crash_if(file, fault::CRASH_DECODE);
        let (asked, frame) = match movie.frame_at(position, pass, &progress) {
            Ok(frame) => (asked, frame),
            Err(why) => match reopen(shared, file, &mut movie, why, &progress) {
                Ok(answer) => answer,
                Err(why) => return shared.fail(why),
            },
        };
        let mut mail = shared.lock();
        if mail.failed.is_some() || mail.closed {
            return;
        }
        if frame.is_some() {
            mail.frame = frame;
        }
        mail.answered = asked;
        mail.waiting_since = match mail.want {
            Some((n, ..)) if n > asked => Some(now()),
            _ => None,
        };
        shared.wake.notify_all();
    }
}

/// After `movie`'s reader failed (`why`) mid-stream: opens its file again, at the newest
/// position asked, and reads the frame there; returns that ask's number and frame. A fresh
/// asset and reader, as a reset media service or decoder needs. AVFoundation's asset reader
/// has no supported way to ask for a software decoder (VideoToolbox takes that only when a
/// session of its own is made), so the system picks the decoder again.
///
/// Allowed once per failure ([`REOPEN_SETTLE`]); the time it takes counts against the frame
/// limit of the position waiting (the main thread fails the picture past it, as for any slow
/// frame). An error when it is not allowed or fails too: the picture then fails, and a damaged
/// file is reported as before.
fn reopen(
    shared: &Shared,
    file: &str,
    movie: &mut Movie,
    why: String,
    progress: &dyn Fn(),
) -> Result<(u64, Option<Frame>), String> {
    if !movie.may_reopen() {
        return Err(format!(
            "{why}; it failed again {:.1} s of frames after it was opened again",
            movie.good_seconds()
        ));
    }
    let (asked, position, pass) = {
        let mail = shared.lock();
        // Let go, or already failed (a frame past its limit on a stalled volume): nothing to
        // open again for, and no line saying it was.
        if mail.closed || mail.failed.is_some() {
            return Err(why);
        }
        mail.want.ok_or_else(|| why.clone())?
    };
    let failed_again = |e: String| format!("{why}; opening it again failed: {e}");
    // The failed readers and the frames read ahead go first: a picture never holds more than
    // its share of frames, opening again or not.
    movie.let_go();
    let mut fresh = Movie::open(file).map_err(failed_again)?;
    if fresh.size != movie.size {
        return Err(failed_again("the file changed".into()));
    }
    fresh.reopened = true;
    #[cfg(feature = "test-support")]
    {
        fresh.slow = movie.slow;
    }
    let frame = fresh
        .frame_at(position, pass, progress)
        .map_err(failed_again)?;
    let name = Path::new(file)
        .file_name()
        .map_or_else(|| file.into(), |n| n.to_string_lossy());
    let note = format!("reading {name} failed ({why}); opened it again at {position:.2} s");
    *movie = fresh;
    shared.lock().reopened = Some(note);
    Ok((asked, frame))
}

/// Multiplies each BGRA pixel's colour by its alpha (straight to premultiplied), rounding to
/// the nearest; an opaque pixel is left alone.
pub fn premultiply(px: &mut [u8]) {
    for p in px.as_chunks_mut::<4>().0 {
        let a = u16::from(p[3]);
        if a == 255 {
            continue;
        }
        for c in &mut p[..3] {
            *c = ((u16::from(*c) * a + 127) / 255) as u8;
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
        hap: None,
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

/// An asset reader and its output.
type Reader = (Retained<AVAssetReader>, Retained<AVAssetReaderTrackOutput>);

/// A second reader opened at a jump's place ahead of the jump, with its first frame read.
struct Spare {
    from: f64,
    reader: Reader,
    first: Option<Frame>,
}

/// A movie read in order on its decode thread.
struct Movie {
    asset: Retained<AVURLAsset>,
    track: Retained<AVAssetTrack>,
    /// A HAP movie's flavour: its frames are read as stored and unwrapped here.
    hap: Option<Variant>,
    reader: Option<Reader>,
    /// The next pass's reader, ready before a loop's seam.
    spare: Option<Spare>,
    /// A place a reader could not be primed at: not tried again until another is asked for.
    prime_failed: Option<f64>,
    /// The plan's pass of the position last asked for.
    pass: u64,
    size: (u32, u32),
    /// The time of the frame last handed out, if one was.
    shown: Option<f64>,
    /// The frame read past the position last asked for.
    next: Option<Frame>,
    /// Where reading last started again: the next read passes over the frames before the one
    /// on show there.
    skip: Option<f64>,
    /// The reader has no frame after `next`.
    ended: bool,
    /// The movie's length and one frame's time, in seconds, as its file says.
    length: f64,
    frame: f64,
    /// This movie was opened again after the reader of the one before it failed.
    reopened: bool,
    /// Frames read since it was opened.
    good: u32,
    /// Times reading started again, and of those how many took a reader primed ahead (tests).
    #[cfg(test)]
    restarts: u32,
    #[cfg(test)]
    spares_taken: u32,
    /// Frames copied out of the decoder (tests).
    #[cfg(test)]
    copied: std::cell::Cell<u32>,
    /// Test support: each frame takes `fault::SLOW_FRAME` to read.
    #[cfg(feature = "test-support")]
    slow: bool,
    /// Test support: the reader failures its file's name asks for, and the frames asked of it
    /// so far (the open's own first).
    #[cfg(feature = "test-support")]
    fault: Option<fault::Fail>,
    #[cfg(feature = "test-support")]
    asks: u32,
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
        let hap = match codec_code(&track) {
            Some(code) => hap_variant(code)?,
            None => None,
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
            hap,
            reader: None,
            spare: None,
            prime_failed: None,
            pass: 0,
            size,
            shown: None,
            next: None,
            skip: None,
            ended: false,
            length,
            frame: 1.0 / f64::from(fps),
            reopened: false,
            good: 0,
            #[cfg(test)]
            restarts: 0,
            #[cfg(test)]
            spares_taken: 0,
            #[cfg(test)]
            copied: std::cell::Cell::new(0),
            #[cfg(feature = "test-support")]
            slow: false,
            #[cfg(feature = "test-support")]
            fault: fault::fail_of(file),
            #[cfg(feature = "test-support")]
            asks: 0,
        })
    }

    /// Seconds of frames read since it was opened.
    fn good_seconds(&self) -> f64 {
        f64::from(self.good) * self.frame
    }

    /// Whether a reader failure may open the movie again: the first time, or once the movie
    /// opened again has read [`REOPEN_SETTLE`] of frames without one.
    fn may_reopen(&self) -> bool {
        !self.reopened || self.good_seconds() >= REOPEN_SETTLE
    }

    /// Starts reading at `from` seconds: with the reader primed for there when there is one.
    fn restart(&mut self, from: f64) -> Result<(), String> {
        if let Some((reader, _)) = self.reader.take() {
            // SAFETY: cancelling a reader this movie owns.
            unsafe { reader.cancelReading() };
        }
        self.next = None;
        self.skip = None;
        self.ended = false;
        #[cfg(test)]
        {
            self.restarts += 1;
        }
        // A reader primed at or a little before `from` (within one frame) reads on to it.
        if let Some(spare) = self.spare.take() {
            if from >= spare.from - self.frame && from < spare.from + SKIP_AHEAD {
                #[cfg(test)]
                {
                    self.spares_taken += 1;
                }
                self.reader = Some(spare.reader);
                self.next = spare.first;
                return Ok(());
            }
            // SAFETY: cancelling a reader this movie owns.
            unsafe { spare.reader.0.cancelReading() };
        }
        self.reader = Some(self.open_reader(from)?);
        self.skip = Some(from);
        Ok(())
    }

    /// Whether a reader is primed at `from`, or priming one there already failed (the jump then
    /// opens its own; trying again at once would only fail again, over and over).
    fn primed(&self, from: f64) -> bool {
        self.prime_failed == Some(from) || self.spare.as_ref().is_some_and(|s| s.from == from)
    }

    /// Opens a second reader at `from` and reads its first frame, for a jump there. A failure
    /// leaves none: the jump then opens one itself, and reports what fails then.
    fn prime(&mut self, from: f64) {
        if let Some(old) = self.spare.take() {
            // SAFETY: cancelling a reader this movie owns.
            unsafe { old.reader.0.cancelReading() };
        }
        self.prime_failed = None;
        let Ok(reader) = self.open_reader(from) else {
            self.prime_failed = Some(from);
            return;
        };
        let Ok(first) = self.read_from(&reader, from) else {
            // SAFETY: cancelling a reader this movie owns.
            unsafe { reader.0.cancelReading() };
            self.prime_failed = Some(from);
            return;
        };
        self.spare = Some(Spare {
            from,
            reader,
            first,
        });
    }

    /// A reader of BGRA frames from the one on show at `from` seconds on. AVFoundation starts
    /// a time range at the first frame dated at or after its start, so a range from inside a
    /// frame would skip that frame and show the next (past an end trim, when `from` is in the
    /// range's last frame): the range starts a frame earlier, and the first read passes over
    /// the frames before the one on show at `from` (`read_from`).
    fn open_reader(&self, from: f64) -> Result<Reader, String> {
        let start = (from - self.frame).max(0.0);
        // SAFETY: AVFoundation calls on the movie's own asset and track; results are checked.
        unsafe {
            let reader = AVAssetReader::assetReaderWithAsset_error(&self.asset)
                .map_err(|e| e.localizedDescription().to_string())?;
            let key: &NSString = &*std::ptr::from_ref::<CFString>(kCVPixelBufferPixelFormatTypeKey)
                .cast::<NSString>();
            let format = NSNumber::new_u32(kCVPixelFormatType_32BGRA);
            let values: [&AnyObject; 1] = [format.as_ref()];
            let settings = NSDictionary::from_slices(&[key], &values);
            // A HAP movie's samples come as the file holds them: no platform decoder reads HAP.
            let settings = if self.hap.is_some() {
                None
            } else {
                Some(&*settings)
            };
            let output = AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(
                &self.track,
                settings,
            );
            output.setAlwaysCopiesSampleData(false);
            reader.addOutput(&output);
            reader.setTimeRange(CMTimeRange {
                start: CMTime::with_seconds(start, 600),
                duration: objc2_core_media::kCMTimePositiveInfinity,
            });
            if !reader.startReading() {
                return Err(reader_error(&reader));
            }
            Ok((reader, output))
        }
    }

    /// Reads the next frame; `None` at the end of the movie.
    fn read(&mut self) -> Result<Option<Frame>, String> {
        let before = self.skip.take().unwrap_or(f64::NEG_INFINITY);
        match &self.reader {
            Some(reader) => self.read_from(reader, before),
            None => Ok(None),
        }
    }

    /// Reads `reader`'s next frame at or after the one on show at `before` seconds; `None` at
    /// its end. A reader opens a frame early (`open_reader`): a frame whose successor, a frame
    /// period on, is at or before `before` is passed over without copying its pixels, so a
    /// seam or a jump copies no more frames than a reader opened at the place itself would.
    /// The last frame there is is kept, whatever its time.
    fn read_from(&self, (reader, output): &Reader, before: f64) -> Result<Option<Frame>, String> {
        // SAFETY: reading the reader's own output; a sample buffer's image is a pixel buffer
        // of BGRA (asked for above), locked while its bytes are copied out.
        unsafe {
            let mut passed = None;
            let sample = loop {
                #[cfg(feature = "test-support")]
                if self.slow {
                    fault::slow_frame();
                }
                let Some(sample) = output.copyNextSampleBuffer() else {
                    if reader.status() == AVAssetReaderStatus::Failed {
                        return Err(reader_error(reader));
                    }
                    match passed {
                        Some(last) => break last,
                        None => return Ok(None),
                    }
                };
                // A read holding no frame (a marker the file may carry) is passed over.
                if self.hap.is_some() && sample.num_samples() == 0 {
                    continue;
                }
                let time = sample.presentation_time_stamp().seconds();
                if time + self.frame <= before + FRAME_TIME_SLACK {
                    passed = Some(sample);
                    continue;
                }
                break sample;
            };
            #[cfg(test)]
            self.copied.set(self.copied.get() + 1);
            let time = sample.presentation_time_stamp().seconds();
            if let Some(variant) = self.hap {
                if !time.is_finite() {
                    return Err("a frame has no time".into());
                }
                return self.hap_frame(&sample, variant, time).map(Some);
            }
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
            let mut pixels = bytes.ok_or("a frame could not be read")?;
            // The stage draws premultiplied colour. A decoder that says its frame's alpha is
            // straight (ProRes 4444, HEVC with alpha) has it multiplied in here, on this decode
            // thread; a frame that says nothing is opaque or premultiplied already.
            let straight = image
                .attachment(kCVImageBufferAlphaChannelModeKey, std::ptr::null_mut())
                .and_then(|v| v.downcast::<CFString>().ok())
                .is_some_and(|mode| *mode == *kCVImageBufferAlphaChannelMode_StraightAlpha);
            if straight {
                premultiply(&mut pixels);
            }
            if !time.is_finite() {
                return Err("a frame has no time".into());
            }
            Ok(Some(Frame {
                time,
                size: self.size,
                pixels,
                stride,
                hap: None,
            }))
        }
    }

    /// A HAP frame read as the file holds it (`sample`), unwrapped into its texture. Its bytes
    /// are bounded by what a frame of the movie's size may need before they are copied.
    fn hap_frame(
        &self,
        sample: &CMSampleBuffer,
        variant: Variant,
        time: f64,
    ) -> Result<Frame, String> {
        let (w, h) = self.size;
        // SAFETY: a sample buffer the reader handed out, alive for this call.
        let (samples, data) = unsafe { (sample.num_samples(), sample.data_buffer()) };
        if samples != 1 {
            return Err(format!("a read of {samples} HAP frames at once"));
        }
        let data = data.ok_or("a HAP frame has no data")?;
        // SAFETY: the length of a block buffer the sample holds.
        let len = unsafe { data.data_length() };
        if variant.max_frame_len(w, h).is_none_or(|most| len > most) {
            return Err(format!(
                "a HAP frame of {len} bytes is larger than a {w} x {h} picture allows"
            ));
        }
        let mut bytes = vec![0u8; len];
        if len > 0 {
            let to = std::ptr::NonNull::new(bytes.as_mut_ptr().cast::<c_void>())
                .ok_or("no memory for a HAP frame")?;
            // SAFETY: copies exactly `len` bytes, the block buffer's own length, into `bytes`,
            // which holds `len`.
            let status = unsafe { data.copy_data_bytes(0, len, to) };
            if status != 0 {
                return Err(format!("a HAP frame could not be read (error {status})"));
            }
        }
        let pixels = hap::unwrap(&bytes, variant, w, h)?;
        Ok(Frame {
            time,
            size: self.size,
            pixels,
            stride: variant.row_bytes(w),
            hap: Some(variant),
        })
    }

    /// The frame at `position` seconds: the last whose time is at or before it (the first
    /// frame before the first frame's time); `None` when the frame handed out last is still the
    /// one (a step back of up to [`BACK_HOLD_FRAMES`] included). A position in another `pass`
    /// of the picture's plan that is behind the frame shown is a seam (a loop's start again):
    /// reading starts there however small the step back, so a loop of a frame or two still
    /// goes round.
    fn frame_at(
        &mut self,
        position: f64,
        pass: u64,
        progress: &dyn Fn(),
    ) -> Result<Option<Frame>, String> {
        #[cfg(feature = "test-support")]
        {
            let ask = self.asks;
            self.asks += 1;
            if let Some(fault) = self.fault
                && fault.fails(ask, position, self.reopened, self.good_seconds())
            {
                return Err("the movie could not be read (test fault)".into());
            }
        }
        let seam = pass != self.pass;
        self.pass = pass;
        let back = self.shown.is_some_and(|t| {
            position < t - BACK_HOLD_FRAMES * self.frame || (seam && position < t)
        });
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
                None => {
                    let read = self.read()?;
                    progress();
                    if read.is_some() {
                        self.good = self.good.saturating_add(1);
                    }
                    read
                }
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

/// The four-character code of `track`'s first format, if it has one.
pub(super) fn codec_code(track: &AVAssetTrack) -> Option<u32> {
    // SAFETY: an asset track's format descriptions are CMFormatDescriptions.
    let descriptions = unsafe { track.formatDescriptions() };
    let first = descriptions.firstObject()?;
    let description: *const CMFormatDescription = Retained::as_ptr(&first).cast();
    // SAFETY: the object is a CMFormatDescription kept alive by `first`.
    Some(unsafe { (*description).media_sub_type() })
}

/// The HAP flavour a track's code names (`None` for any other codec), or why a HAP flavour
/// openLab does not play is refused.
pub(super) fn hap_variant(code: u32) -> Result<Option<Variant>, String> {
    if let Some(name) = Variant::unplayed(code) {
        return Err(format!(
            "{name} movies are not supported; use HAP, HAP Alpha or HAP Q"
        ));
    }
    Ok(Variant::of_code(code))
}

/// Whether a HAP movie's first frame reads and unwraps as the stage host would read it: the
/// probe's check that a HAP file plays (no platform decoder can say so).
pub fn hap_first_frame(file: &str) -> Result<(), String> {
    let mut movie = Movie::open(file)?;
    match movie.frame_at(0.0, 0, &|| {})? {
        Some(_) => Ok(()),
        None => Err("it has no frames".into()),
    }
}

fn reader_error(reader: &AVAssetReader) -> String {
    // SAFETY: reading a reader's error.
    unsafe { reader.error() }.map_or_else(
        || "the movie could not be read".into(),
        |e| format!("the movie could not be read ({})", e.localizedDescription()),
    )
}

impl Movie {
    /// Cancels its readers and drops the frames it read ahead.
    fn let_go(&mut self) {
        self.next = None;
        self.skip = None;
        let spare = self.spare.take().map(|s| s.reader);
        for (reader, _) in self.reader.take().into_iter().chain(spare) {
            // SAFETY: cancelling a reader this movie owns.
            unsafe { reader.cancelReading() };
        }
    }
}

impl Drop for Movie {
    fn drop(&mut self) {
        self.let_go();
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
    /// Hangs once the movie's tracks are read, before its first frame is decoded.
    pub const HANG_FIRST_FRAME: &str = "openlab-test-hang-first-frame";
    /// Crashes while the file opens (its first frame decoded).
    pub const CRASH_OPEN: &str = "openlab-test-crash-open";
    /// Crashes at the first frame asked for after the open.
    pub const CRASH_DECODE: &str = "openlab-test-crash-decode";
    /// Reads every frame slowly: each moves the hand-moved media clock on by `SLOW_FRAME`, a
    /// decoder slower than real time that keeps going, whatever the machine's speed.
    pub const SLOW_DECODE: &str = "openlab-test-slow-decode";
    /// Opens slowly: the open goes on only once the hand-moved media clock has moved
    /// `SLOW_OPEN` on from its start (a file on a slow disk, however fast the machine).
    pub const SLOW_OPEN_FILE: &str = "openlab-test-slow-open";
    /// The reader fails at the first frame asked at or past [`FAIL_FROM`] seconds after the
    /// open; the movie opened again reads on.
    pub const FAIL_DECODE_ONCE: &str = "openlab-test-fail-decode-once";
    /// As `FAIL_DECODE_ONCE`, and the movie opened again fails the same way at the next frame
    /// asked after the one it opened at.
    pub const FAIL_DECODE_AGAIN: &str = "openlab-test-fail-decode-again";
    /// Each movie (the one opened first, and each opened again) fails at the first frame asked
    /// once it has read a second more than `REOPEN_SETTLE` of frames.
    pub const FAIL_DECODE_OFTEN: &str = "openlab-test-fail-decode-often";
    /// Where `FAIL_DECODE_ONCE` and `FAIL_DECODE_AGAIN` fail: mid-stream, well after the open.
    pub const FAIL_FROM: f64 = 1.0;

    /// A reader failure a file's name asks for.
    #[derive(Clone, Copy, Debug)]
    pub enum Fail {
        Once,
        Again,
        Often,
    }

    pub(super) fn fail_of(file: &str) -> Option<Fail> {
        [
            (FAIL_DECODE_ONCE, Fail::Once),
            (FAIL_DECODE_AGAIN, Fail::Again),
            (FAIL_DECODE_OFTEN, Fail::Often),
        ]
        .into_iter()
        .find(|(name, _)| file.contains(name))
        .map(|(_, fail)| fail)
    }

    impl Fail {
        /// Whether the frame asked (`ask`: 0 is the one the movie opened at, never failed) at
        /// `position` fails, on a movie opened again or not, with `good` seconds of frames read.
        pub(super) fn fails(self, ask: u32, position: f64, reopened: bool, good: f64) -> bool {
            ask > 0
                && match self {
                    Self::Once => !reopened && position >= FAIL_FROM,
                    Self::Again => position >= FAIL_FROM,
                    Self::Often => good >= super::REOPEN_SETTLE + 1.0,
                }
        }
    }

    /// The hand-moved media clock: its origin, and how far it has been moved.
    static HAND: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    static HAND_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// Measures the media limits on a clock only `advance` (and slow frames) move.
    pub fn use_hand_clock() {
        let _ = HAND.set(std::time::Instant::now());
    }

    /// Moves the hand clock on by `ms`.
    pub fn advance(ms: u64) {
        HAND_MS.fetch_add(ms, std::sync::atomic::Ordering::SeqCst);
    }

    pub(super) fn hand_now() -> Option<std::time::Instant> {
        HAND.get().map(|base| {
            *base
                + std::time::Duration::from_millis(
                    HAND_MS.load(std::sync::atomic::Ordering::SeqCst),
                )
        })
    }

    /// One slow frame: `SLOW_FRAME` of the hand clock (nothing without one: the fault is for
    /// tests on a hand-moved media clock).
    pub(super) fn slow_frame() {
        advance(SLOW_FRAME.as_millis() as u64);
    }

    pub const SLOW_FRAME: std::time::Duration = std::time::Duration::from_millis(400);

    /// How long a `SLOW_OPEN_FILE` open takes on the hand-moved media clock (inside the open
    /// limit tests give fault files).
    pub const SLOW_OPEN: std::time::Duration = std::time::Duration::from_millis(300);

    /// Holds a `SLOW_OPEN_FILE` open until the hand clock has moved `SLOW_OPEN` on from
    /// `started` (when the main thread took the load, so a clock moved after the load was sent
    /// counts whenever this thread runs), or the picture is let go. Without a hand clock it
    /// does nothing: the fault is for tests that move that clock.
    pub(super) fn slow_open_if(shared: &Shared, file: &str, started: std::time::Instant) {
        if !file.contains(SLOW_OPEN_FILE) || hand_now().is_none() {
            return;
        }
        let until = started + SLOW_OPEN;
        let mut mail = shared.lock();
        while !mail.closed && hand_now().is_some_and(|t| t < until) {
            // The hand clock moves without waking this thread: look again shortly.
            mail = shared
                .wake
                .wait_timeout(mail, std::time::Duration::from_millis(5))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Whether `file`'s name asks for a fault.
    pub fn named(file: &str) -> bool {
        file.contains("openlab-test-")
    }

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

    /// Straight alpha to premultiplied: each colour times alpha / 255, rounded; opaque and
    /// fully transparent pixels by hand.
    #[test]
    fn straight_alpha_is_premultiplied() {
        let mut px = vec![
            255, 128, 0, 255, // opaque: unchanged
            255, 255, 255, 128, // half white: 128 each
            0, 0, 255, 64, // a quarter red (BGRA): 64
            200, 100, 50, 0, // transparent: nothing
        ];
        premultiply(&mut px);
        assert_eq!(
            px,
            vec![
                255, 128, 0, 255, 128, 128, 128, 128, 0, 0, 64, 64, 0, 0, 0, 0
            ]
        );
    }

    /// A picture whose file is not there fails on its own thread, reported when polled; the
    /// main thread is never held.
    #[test]
    fn an_open_that_fails_is_reported_when_polled() {
        let limits = Limits {
            open: Duration::from_secs(30),
            frame: Duration::from_secs(30),
        };
        let mut p = Picture::open("/nonexistent/openlab/clip.mov", false, 0.0, limits).unwrap();
        let device = objc2_metal::MTLCreateSystemDefaultDevice().unwrap();
        let got = p.frame(&device, 0.0, 0, None, true);
        assert_eq!(got.err().as_deref(), Some("the file is not there"));
        assert!(matches!(p.poll(), Some(Polled::Failed(_))));
        assert_eq!(p.asked(), Some(0.0));
        assert_eq!(
            p.take_reopened(),
            None,
            "an open that fails is not tried again"
        );
    }

    /// An open past its limit says what it was doing, so a stall on a show machine (the
    /// file's tracks never read, or its first frame never decoded) says where it stopped.
    #[cfg(feature = "test-support")]
    #[test]
    fn an_open_past_its_limit_says_where_it_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let none = Limits {
            open: Duration::ZERO,
            frame: Duration::from_secs(60),
        };
        let failed = |p: &mut Picture| match p.poll() {
            Some(Polled::Failed(why)) => why,
            _ => panic!("the open did not fail past its limit"),
        };
        let tracks = clip(dir.path(), "openlab-test-hang-open.mov", 5);
        let mut p = Picture::open(&tracks, false, 0.0, none).unwrap();
        assert_eq!(
            failed(&mut p),
            "its file took longer than 0 s to open (reading the movie's tracks)"
        );
        let first = clip(dir.path(), "openlab-test-hang-first-frame.mov", 5);
        let mut p = Picture::open(&first, false, 0.0, none).unwrap();
        {
            // Bounded: the decode thread reads five frames' tracks in well under this.
            let mut mail = p.shared.lock();
            let mut rounds = 600;
            while mail.opening != DECODING_FIRST && mail.failed.is_none() && rounds > 0 {
                rounds -= 1;
                mail = p
                    .shared
                    .wake
                    .wait_timeout(mail, Duration::from_millis(100))
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
            assert_eq!(mail.failed, None);
            assert_eq!(mail.opening, DECODING_FIRST, "the tracks were never read");
        }
        assert_eq!(
            failed(&mut p),
            "its file took longer than 0 s to open (decoding its first frame)"
        );
    }

    /// A movie of `frames` at 25 fps in `dir`, under `name` (a fault switch's, for some).
    fn clip(dir: &Path, name: &str, frames: u16) -> String {
        let file = dir.join(name).to_str().unwrap().to_owned();
        super::super::testmedia::make(&[
            "h264".into(),
            file.clone(),
            frames.to_string(),
            "25".into(),
        ])
        .unwrap();
        file
    }

    /// Asks `p` for the frame at each position in turn, as a probe does (waiting for it):
    /// whether each was answered with a picture, and the re-open notes said meanwhile.
    fn play_through(p: &mut Picture, positions: impl Iterator<Item = f64>) -> Vec<String> {
        let device = objc2_metal::MTLCreateSystemDefaultDevice().unwrap();
        let mut notes = Vec::new();
        for at in positions {
            let got = p.frame(&device, at, 0, None, true);
            notes.extend(p.take_reopened());
            assert!(
                matches!(got, Ok(Some(_))),
                "at {at} s: {:?} after {notes:?}",
                got.err()
            );
            assert!(
                !matches!(p.poll(), Some(Polled::Failed(_))),
                "failed at {at} s"
            );
        }
        notes
    }

    const FOREVER: Limits = Limits {
        open: Duration::from_secs(60),
        frame: Duration::from_secs(60),
    };

    /// A reader that fails mid-stream: the decode thread opens the movie again at the position
    /// asked and answers it there, says so once, and the picture plays on.
    #[test]
    fn a_reader_failure_mid_stream_opens_the_movie_again_once_and_it_plays_on() {
        let dir = tempfile::tempdir().unwrap();
        let file = clip(dir.path(), "openlab-test-fail-decode-once.mov", 75);
        let mut p = Picture::open(&file, false, 0.0, FOREVER).unwrap();
        let notes = play_through(&mut p, (0..60).map(|i| f64::from(i) * 0.04));
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("openlab-test-fail-decode-once.mov")
                && notes[0].contains("at 1.00 s")
                && notes[0].contains("opened it again"),
            "{}",
            notes[0]
        );
        assert_eq!(p.asked(), Some(59.0 * 0.04));
    }

    /// The movie opened again fails again before it has read a stretch of good frames: the
    /// picture fails, with both reasons, and the movie is not opened a third time.
    #[test]
    fn a_movie_that_fails_again_after_it_was_opened_again_fails_the_picture() {
        let dir = tempfile::tempdir().unwrap();
        let file = clip(dir.path(), "openlab-test-fail-decode-again.mov", 75);
        let mut p = Picture::open(&file, false, 0.0, FOREVER).unwrap();
        let device = objc2_metal::MTLCreateSystemDefaultDevice().unwrap();
        let mut notes = Vec::new();
        let mut failed = None;
        for i in 0..60 {
            let at = f64::from(i) * 0.04;
            match p.frame(&device, at, 0, None, true) {
                Ok(_) => notes.extend(p.take_reopened()),
                Err(why) => {
                    notes.extend(p.take_reopened());
                    failed = Some((at, why));
                    break;
                }
            }
        }
        let (at, why) = failed.expect("the picture failed");
        assert_eq!(notes.len(), 1, "opened again once: {notes:?}");
        assert!((at - 1.04).abs() < 1e-9, "at the next frame asked: {at}");
        assert!(why.contains("opened again"), "{why}");
        assert!(matches!(p.poll(), Some(Polled::Failed(_))));
    }

    /// After the movie opened again has read `REOPEN_SETTLE` of good frames, a later failure
    /// may open it again: a fault every eleven seconds never breaks a picture.
    #[test]
    fn the_movie_may_be_opened_again_after_a_stretch_of_good_frames() {
        let dir = tempfile::tempdir().unwrap();
        let file = clip(dir.path(), "openlab-test-fail-decode-often.mov", 750);
        let mut p = Picture::open(&file, false, 0.0, FOREVER).unwrap();
        // Every frame read, a frame at a time, to 29 s: failures at about 11 s and 22 s.
        let notes = play_through(&mut p, (0..725).map(|i| f64::from(i) * 0.04));
        assert_eq!(notes.len(), 2, "{notes:?}");
    }

    /// A picture that already failed (a frame past its limit) is not opened again when its
    /// reader then fails too: no note says it was, and the reason is the reader's.
    #[test]
    fn a_picture_that_already_failed_is_not_opened_again() {
        let dir = tempfile::tempdir().unwrap();
        let file = clip(dir.path(), "clip.mov", 5);
        let mut movie = Movie::open(&file).unwrap();
        let shared = Shared {
            mail: Mutex::new(Mail::default()),
            wake: Condvar::new(),
        };
        {
            let mut mail = shared.lock();
            mail.want = Some((1, 0.04, 0));
            mail.failed = Some(frame_overrun(LIMITS));
        }
        let got = reopen(&shared, &file, &mut movie, "reader failed".into(), &|| {});
        assert_eq!(got.err().as_deref(), Some("reader failed"));
        assert_eq!(shared.lock().reopened, None);
        assert!(!movie.reopened);
        // Not failed: the same call opens it again.
        shared.lock().failed = None;
        let got = reopen(&shared, &file, &mut movie, "reader failed".into(), &|| {});
        assert!(matches!(got, Ok((1, Some(_)))), "{:?}", got.err());
        assert!(movie.reopened && shared.lock().reopened.is_some());
    }

    /// A movie opened again has its allowance back only after `REOPEN_SETTLE` of frames read.
    #[test]
    fn a_movie_opened_again_may_be_opened_again_only_after_the_settle() {
        let dir = tempfile::tempdir().unwrap();
        let file = clip(dir.path(), "clip.mov", 5);
        let mut m = Movie::open(&file).unwrap();
        assert!(m.may_reopen(), "the first failure");
        m.reopened = true;
        assert!(!m.may_reopen(), "at once again");
        m.good = (REOPEN_SETTLE * 25.0) as u32 - 1;
        assert!(!m.may_reopen(), "a frame short of the settle");
        m.good += 1;
        assert!(m.may_reopen());
    }

    /// The frame number a decoded test-card frame carries (its time at 25 fps).
    fn number(frame: &Frame) -> u16 {
        (frame.time * 25.0).round() as u16
    }

    /// A clock stepped back by a few milliseconds across a frame boundary holds the frame shown
    /// and does not read again; a step back of more than a frame, or one ahead of `SKIP_AHEAD`
    /// or more, reads again from there.
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
        let f = m.frame_at(1970.0 / 48_000.0, 0, &|| {}).unwrap().unwrap();
        assert_eq!(number(&f), 1);
        let restarts = m.restarts;
        assert!(
            m.frame_at(1826.0 / 48_000.0, 0, &|| {}).unwrap().is_none(),
            "held"
        );
        assert_eq!(m.restarts, restarts, "the reader was not started again");
        // Forward again: the next frame, still without a restart.
        let f = m.frame_at(3890.0 / 48_000.0, 0, &|| {}).unwrap().unwrap();
        assert_eq!(number(&f), 2);
        assert_eq!(m.restarts, restarts);
        // Half a second or more ahead (a jump, or a decoder that fell behind) is not read frame
        // by frame: reading starts again there.
        let f = m.frame_at(0.6, 0, &|| {}).unwrap().unwrap();
        assert_eq!(number(&f), 15);
        assert_eq!(m.restarts, restarts + 1);
        let restarts = m.restarts;
        // A real jump back (load to time) reads again from there.
        let f = m.frame_at(0.0, 0, &|| {}).unwrap().unwrap();
        assert_eq!(number(&f), 0);
        assert_eq!(m.restarts, restarts + 1);
    }

    /// A reader opened inside a frame (a load to time, a pause on a new decoder, a seam at a
    /// loop start between frames) starts with the frame on show there, not the next one.
    #[test]
    fn a_reader_opened_inside_a_frame_starts_with_that_frame() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mov");
        let file = path.to_str().unwrap().to_owned();
        super::super::testmedia::make(&["h264".into(), file.clone(), "50".into(), "25".into()])
            .unwrap();
        // A held end, just before the next frame, is still the frame before it.
        let held = 1.2 - openlab_core::HOLD_BEFORE_END;
        for (at, want) in [(1.19, 29), (0.41, 10), (0.0, 0), (1.96, 49), (held, 29)] {
            let mut m = Movie::open(&file).unwrap();
            let f = m.frame_at(at, 0, &|| {}).unwrap().unwrap();
            assert_eq!(number(&f), want, "opened at {at} s");
        }
    }

    /// A loop's seam (VID-07): with the loop's start primed ahead, the jump back takes the
    /// primed reader and shows the loop's first frame, with no new reader opened at the seam; a
    /// jump somewhere else still opens one, and the primed reader is let go.
    #[test]
    fn a_loop_seam_takes_the_reader_primed_at_the_loops_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mov");
        let file = path.to_str().unwrap().to_owned();
        super::super::testmedia::make(&["h264".into(), file.clone(), "50".into(), "25".into()])
            .unwrap();
        let mut m = Movie::open(&file).unwrap();
        // The loop is 0.4 to 1.2 s: near its end the decode thread primes 0.4.
        m.frame_at(0.8, 0, &|| {}).unwrap().unwrap();
        let f = m.frame_at(1.15, 0, &|| {}).unwrap().unwrap();
        assert_eq!(number(&f), 28);
        m.prime(0.4);
        assert!(m.primed(0.4));
        assert!(!m.primed(0.8));
        let f = m.frame_at(1.19, 0, &|| {}).unwrap().unwrap();
        assert_eq!(number(&f), 29, "the pass plays on to its end");
        let f = m.frame_at(0.4, 0, &|| {}).unwrap().unwrap();
        assert_eq!(number(&f), 10, "the next pass's first frame");
        assert_eq!(m.spares_taken, 1);
        assert!(!m.primed(0.4), "taken");
        // Frames follow on from the primed reader.
        let f = m.frame_at(0.45, 0, &|| {}).unwrap().unwrap();
        assert_eq!(number(&f), 11);
        // Primed for one place, a jump elsewhere opens its own reader.
        m.prime(0.4);
        let f = m.frame_at(1.6, 0, &|| {}).unwrap().unwrap();
        assert_eq!(number(&f), 40);
        assert_eq!(m.spares_taken, 1);
        assert!(!m.primed(0.4), "let go");
    }

    /// The reader primed at a loop's start opens a frame early but copies only the frame on
    /// show there: priming and the seam together copy the loop's first frame and the one after
    /// it, as a reader opened at the loop's start would. A loop start inside a frame primes that
    /// frame. A jump that is not primed passes over the early frame too.
    #[test]
    fn a_primed_seam_copies_only_the_frames_it_shows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mov");
        let file = path.to_str().unwrap().to_owned();
        super::super::testmedia::make(&["h264".into(), file.clone(), "50".into(), "25".into()])
            .unwrap();
        for (from, first) in [(0.4, 10), (0.41, 10)] {
            let mut m = Movie::open(&file).unwrap();
            m.frame_at(1.15, 0, &|| {}).unwrap().unwrap();
            m.frame_at(1.19, 0, &|| {}).unwrap();
            let before = m.copied.get();
            m.prime(from);
            assert_eq!(
                m.copied.get() - before,
                1,
                "primed at {from}: one frame copied"
            );
            let f = m.frame_at(from, 1, &|| {}).unwrap().unwrap();
            assert_eq!(number(&f), first, "the seam at {from}");
            assert_eq!(m.spares_taken, 1);
            assert_eq!(
                m.copied.get() - before,
                2,
                "and the one after it, at the seam"
            );
        }
        let mut m = Movie::open(&file).unwrap();
        m.frame_at(0.0, 0, &|| {}).unwrap().unwrap();
        let before = m.copied.get();
        let f = m.frame_at(1.6, 0, &|| {}).unwrap().unwrap();
        assert_eq!(number(&f), 40);
        assert_eq!(
            m.copied.get() - before,
            2,
            "the frame shown and the one after it"
        );
    }

    /// A loop of two frames: going round, the position steps back by a frame or less, which
    /// alone would be held as a clock correction; in a new pass of the plan it is a seam, and
    /// the loop's first frame is shown again. In the same pass the small step back still holds.
    #[test]
    fn a_step_back_into_a_new_pass_is_a_seam_however_small() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mov");
        let file = path.to_str().unwrap().to_owned();
        super::super::testmedia::make(&["h264".into(), file.clone(), "50".into(), "25".into()])
            .unwrap();
        let mut m = Movie::open(&file).unwrap();
        // A loop 0.40 to 0.48 s: frames 10 and 11.
        assert_eq!(number(&m.frame_at(0.40, 0, &|| {}).unwrap().unwrap()), 10);
        assert_eq!(number(&m.frame_at(0.46, 0, &|| {}).unwrap().unwrap()), 11);
        let f = m.frame_at(0.40, 1, &|| {}).unwrap();
        assert_eq!(f.map(|f| number(&f)), Some(10), "the next pass");
        assert_eq!(number(&m.frame_at(0.46, 1, &|| {}).unwrap().unwrap()), 11);
        assert!(
            m.frame_at(0.43, 1, &|| {}).unwrap().is_none(),
            "a small step back in one pass holds"
        );
        // Round again with the loop's start primed, as the decode thread does near a seam.
        for pass in 2..5 {
            m.prime(0.40);
            let f = m.frame_at(0.42, pass, &|| {}).unwrap();
            assert_eq!(f.map(|f| number(&f)), Some(10), "pass {pass}");
            // Primed again for the next seam while this pass plays.
            m.prime(0.40);
            let f = m.frame_at(0.445, pass, &|| {}).unwrap();
            assert_eq!(f.map(|f| number(&f)), Some(11), "pass {pass}");
        }
    }

    /// Priming that failed is not tried again for the same place (the decode thread would
    /// otherwise retry it in a busy loop until the seam); another place is. AVFoundation gives
    /// no reader failure to provoke on a file already open (a deleted file's asset still reads),
    /// so the failure is set as `prime` records it.
    #[test]
    fn a_failed_prime_is_not_retried_for_the_same_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mov");
        let file = path.to_str().unwrap().to_owned();
        super::super::testmedia::make(&["h264".into(), file.clone(), "50".into(), "25".into()])
            .unwrap();
        let mut m = Movie::open(&file).unwrap();
        m.prime_failed = Some(0.4);
        assert!(m.primed(0.4), "the failure is remembered for that place");
        assert!(!m.primed(0.8), "another place is tried");
        m.prime(0.8);
        assert!(m.primed(0.8) && m.spare.is_some(), "primed there");
        assert_eq!(m.prime_failed, None, "a new place forgets the old failure");
    }

    /// A movie with an end trim, paused or loaded to within one refresh of the trim, asked for
    /// with the real display's look-ahead → the frame drawn is the range's last frame, never one
    /// past it. Driven as a display's window draws: the host's picture at each refresh, asked
    /// of the decoder a refresh ahead. A paused picture stands still, so the pause neither
    /// moves the frame nor starts reading again.
    #[test]
    fn a_movie_paused_or_loaded_within_a_refresh_of_its_end_trim_draws_the_ranges_last_frame() {
        use crate::host::Host;
        use crate::host::tests::{Fake, clock, drawn, host, load, send, stage};
        use openlab_core::stage_link::ToStage;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mov");
        let file = path.to_str().unwrap().to_owned();
        super::super::testmedia::make(&["h264".into(), file.clone(), "50".into(), "25".into()])
            .unwrap();
        // File 0.4 to 1.2 s twice (frames 10 to 29 each pass), on a 1 kHz clock; paused 10 ms
        // before the end trim in the last pass, on frame 29.
        let play = ToStage::Play {
            picture: 1,
            at: 0,
            offset: 0.4,
            action: 0.0,
            plan: vec![openlab_core::PlanPass {
                from: 0.4,
                length: 0.8,
                plays: 2,
            }],
            into: 0.0,
            fades: openlab_core::PictureFades::NONE,
        };
        let pause = ToStage::Pause {
            picture: 1,
            at: 1590,
            offset: 1.19,
            action: 1.59,
        };
        let shown = |m: &mut Movie, h: &mut Host<Fake>, sample: u64| {
            clock(h, sample);
            let c = *drawn(h, 1).first().expect("drawn");
            m.frame_at(
                c.position_after(FRAME_AHEAD),
                c.pass_after(FRAME_AHEAD),
                &|| {},
            )
            .unwrap()
            .map(|f| number(&f))
        };
        let refreshes =
            |from: u64, to: u64| (from * 60 / 1000..to * 60 / 1000).map(|r| r * 1000 / 60);
        let up = |h: &mut Host<Fake>| {
            send(
                h,
                ToStage::Stages {
                    stages: vec![stage(1, Some("D"), false)],
                },
            );
            load(h, 1, 1);
            send(h, play.clone());
        };

        // Paused while it plays.
        let mut h = host(&["D"]);
        up(&mut h);
        let mut m = Movie::open(&file).unwrap();
        let mut on = None;
        for sample in refreshes(0, 1590) {
            on = shown(&mut m, &mut h, sample).or(on);
        }
        assert_eq!(on, Some(29), "the range's last frame as it pauses");
        send(&mut h, pause.clone());
        let restarts = m.restarts;
        for sample in refreshes(1590, 3000) {
            on = shown(&mut m, &mut h, sample).or(on);
        }
        assert_eq!(on, Some(29), "held on the range's last frame while paused");
        assert_eq!(
            m.restarts, restarts,
            "the pause did not start reading again"
        );

        // Loaded there: a stage host started again is sent the load, the first play and the
        // pause, and a fresh decoder draws its first frame.
        let mut h = host(&["D"]);
        up(&mut h);
        send(&mut h, pause);
        let mut m = Movie::open(&file).unwrap();
        let mut seen = Vec::new();
        for sample in refreshes(1600, 3000) {
            seen.extend(shown(&mut m, &mut h, sample));
        }
        assert_eq!(seen, vec![29], "the range's last frame, and only it");

        // Paused in the loop's second pass 5 ms before a frame's start, just after a refresh
        // that asked a refresh ahead and so showed that frame: the pause holds it. In the pass
        // it paused in, the decoder sees no seam, so it neither steps back a frame nor reads
        // again.
        let mut h = host(&["D"]);
        up(&mut h);
        let mut m = Movie::open(&file).unwrap();
        let mut on = None;
        for sample in refreshes(0, 1390).chain([1390]) {
            on = shown(&mut m, &mut h, sample).or(on);
        }
        assert_eq!(on, Some(25), "a refresh ahead of 0.99 s into the file");
        send(
            &mut h,
            ToStage::Pause {
                picture: 1,
                at: 1395,
                offset: 0.995,
                action: 1.395,
            },
        );
        let restarts = m.restarts;
        for sample in refreshes(1400, 2000) {
            on = shown(&mut m, &mut h, sample).or(on);
        }
        assert_eq!(on, Some(25), "the frame shown is held");
        assert_eq!(
            m.restarts, restarts,
            "the pause did not start reading again"
        );
    }

    /// On a real display the frame asked for is the one due a refresh on. In the last pass that
    /// never reaches past the end trim: the frame after the range is not shown for the last
    /// refresh, and with hold at end the range's last frame is the one held.
    #[test]
    fn a_frame_asked_a_refresh_ahead_at_the_plans_end_is_never_past_the_end_trim() {
        use crate::host::CuePicture;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mov");
        let file = path.to_str().unwrap().to_owned();
        super::super::testmedia::make(&["h264".into(), file.clone(), "50".into(), "25".into()])
            .unwrap();
        let mut m = Movie::open(&file).unwrap();
        // File 0.4 to 1.2 s once: frames 10 to 29.
        let plan = [openlab_core::PlanPass {
            from: 0.4,
            length: 0.8,
            plays: 1,
        }];
        let at = |elapsed: f64| {
            let p = openlab_core::plan_point(&plan, elapsed).unwrap();
            CuePicture {
                picture: 1,
                position: p.position,
                left: p.left,
                next: p.next,
                held: p.ended,
                last: p.last,
                pass: p.pass,
                opacity: 1.0,
                geometry: Default::default(),
            }
        };
        // A redraw each refresh, as a display's window draws: through the plan's end (where a
        // cue without hold at end leaves) and on into the hold.
        let mut seen = Vec::new();
        for refresh in 18..90 {
            let c = at(f64::from(refresh) / 60.0);
            let frame = m
                .frame_at(
                    c.position_after(FRAME_AHEAD),
                    c.pass_after(FRAME_AHEAD),
                    &|| {},
                )
                .unwrap();
            seen.extend(frame.map(|f| number(&f)));
        }
        assert_eq!(seen.iter().max(), Some(&29), "nothing past the range");
        assert_eq!(seen.last(), Some(&29), "held on the range's last frame");
    }
}
