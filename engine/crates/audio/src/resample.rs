// SPDX-License-Identifier: GPL-3.0-or-later

//! Device-rate view of a [`FrameSource`] (ARCHITECTURE §3.5): rubato's synchronous FFT resampler
//! when the rates differ, a pass-through otherwise. Runs on disk threads.
//!
//! Positions are device frames on one fixed grid: frame `p` is the signal at time `p / device
//! rate` (to within half a frame: the resampler's delay is fractional and we discard a whole
//! number of frames), whether reached by reading from the start or by seeking. A seek restarts
//! the resampler one FFT chunk early, on a chunk boundary (where input and output frames coincide
//! exactly), and discards the delay plus the pre-roll, so no position is rounded. The length is
//! exactly [`MediaInfo::frames_at`](crate::MediaInfo::frames_at); past the end of the file the
//! resampler is fed silence to flush its tail. A source that ends before its probed length (the
//! file changed since the probe) is an error, never padded.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};

use crate::decode::{FrameSource, MediaError, scale_round};

/// Target FFT input chunk; the resampler rounds it to a whole number of rate-ratio periods.
const FFT_CHUNK: usize = 1024;

struct Rs {
    fft: Fft<f32>,
    chunk_in: usize,
    chunk_out: usize,
    /// Input chunk, interleaved.
    inbuf: Vec<f32>,
    /// Output chunk, interleaved; frames `out_pos..out_len` not yet returned.
    outbuf: Vec<f32>,
    out_pos: usize,
    out_len: usize,
    /// The resampler's delay in output frames, rounded.
    delay: u64,
    /// Output frames still to throw away (delay + pre-roll) after a seek.
    discard: u64,
}

pub(crate) struct Pipeline {
    src: Box<dyn FrameSource>,
    ch: usize,
    rs: Option<Rs>,
    /// Next device frame to be returned.
    pos: u64,
    /// Exact length in device frames.
    total: u64,
    /// Source frames (at the file's rate): its probed length and the next one to be read.
    src_frames: u64,
    src_pos: u64,
    src_eof: bool,
}

impl Pipeline {
    /// `frames` is the source's exact length at its own rate (from probe).
    pub(crate) fn new(
        src: Box<dyn FrameSource>,
        frames: u64,
        device_rate: u32,
    ) -> Result<Self, MediaError> {
        let ch = src.channels();
        let rate = src.sample_rate();
        let rs = if rate == device_rate {
            None
        } else {
            let fft = Fft::<f32>::new(
                rate as usize,
                device_rate as usize,
                FFT_CHUNK,
                ch,
                FixedSync::Both,
            )
            .map_err(|e| MediaError::Unsupported(format!("resampler: {e}")))?;
            let (chunk_in, chunk_out) = (fft.input_frames_next(), fft.output_frames_next());
            // The anti-aliasing filter is centred on input frame fft_in / 2 (rounded down), so
            // the true delay is that many input frames in output frames. rubato reports
            // fft_out / 2, which is off by up to a frame when fft_in is odd.
            let (fin, fout) = (fft.fft_size_in() as u64, fft.fft_size_out() as u64);
            let delay = ((fin / 2) * fout * 2 + fin) / (2 * fin).max(1);
            Some(Rs {
                inbuf: vec![0.0; chunk_in * ch],
                outbuf: vec![0.0; chunk_out * ch],
                out_pos: 0,
                out_len: 0,
                delay,
                discard: delay,
                fft,
                chunk_in,
                chunk_out,
            })
        };
        Ok(Pipeline {
            src,
            ch,
            rs,
            pos: 0,
            total: scale_round(frames, device_rate, rate),
            src_frames: frames,
            src_pos: 0,
            src_eof: false,
        })
    }

    pub(crate) fn channels(&self) -> usize {
        self.ch
    }

    pub(crate) fn position(&self) -> u64 {
        self.pos
    }

    /// Positions the pipeline so the next frame read is device frame `p`.
    pub(crate) fn seek(&mut self, p: u64) -> Result<(), MediaError> {
        let p = p.min(self.total);
        self.src_eof = false;
        match &mut self.rs {
            None => {
                self.src.seek(p)?;
                self.src_pos = p;
            }
            Some(rs) => {
                let j = (p / rs.chunk_out as u64).saturating_sub(1);
                self.src_pos = j * rs.chunk_in as u64;
                self.src.seek(self.src_pos)?;
                rs.fft.reset();
                rs.out_pos = 0;
                rs.out_len = 0;
                rs.discard = rs.delay + (p - j * rs.chunk_out as u64);
            }
        }
        self.pos = p;
        Ok(())
    }

    /// Fills `out` (interleaved, a whole number of frames) up to the end of the stream. Returns
    /// the frames written; fewer than requested only at the end.
    pub(crate) fn read(&mut self, out: &mut [f32]) -> Result<usize, MediaError> {
        let ch = self.ch;
        let left = usize::try_from(self.total - self.pos).unwrap_or(usize::MAX);
        let want = (out.len() / ch).min(left);
        let mut got = 0;
        while got < want {
            let Some(dst) = out.get_mut(got * ch..want * ch) else {
                break;
            };
            let n = match &mut self.rs {
                None => {
                    let n = self.src.read(dst)?;
                    self.src_pos += n as u64;
                    if n == 0 {
                        // `want` stops at the probed length, so the file has shrunk.
                        return Err(self.truncated());
                    }
                    n
                }
                Some(rs) => {
                    if rs.out_pos == rs.out_len {
                        let read = produce(rs, &mut *self.src, &mut self.src_eof, ch)?;
                        self.src_pos += read;
                        if self.src_eof && self.src_pos < self.src_frames {
                            return Err(self.truncated());
                        }
                        continue;
                    }
                    let n = (rs.out_len - rs.out_pos).min(dst.len() / ch);
                    let src = rs.outbuf.get(rs.out_pos * ch..(rs.out_pos + n) * ch);
                    if let (Some(src), Some(d)) = (src, dst.get_mut(..n * ch)) {
                        d.copy_from_slice(src);
                    }
                    rs.out_pos += n;
                    n
                }
            };
            got += n;
        }
        self.pos += got as u64;
        Ok(got)
    }

    fn truncated(&self) -> MediaError {
        MediaError::Truncated {
            expected: self.src_frames,
            actual: self.src_pos,
        }
    }
}

/// Resamples one input chunk into `rs.outbuf`, dropping frames still owed to `discard`. Returns
/// the source frames read.
fn produce(
    rs: &mut Rs,
    src: &mut dyn FrameSource,
    src_eof: &mut bool,
    ch: usize,
) -> Result<u64, MediaError> {
    let mut filled = 0;
    while filled < rs.chunk_in && !*src_eof {
        let Some(dst) = rs.inbuf.get_mut(filled * ch..) else {
            break;
        };
        let n = src.read(dst)?;
        if n == 0 {
            *src_eof = true;
        }
        filled += n;
    }
    if let Some(rest) = rs.inbuf.get_mut(filled * ch..) {
        rest.fill(0.0);
    }
    let resample_err =
        |e: &dyn std::fmt::Display| MediaError::Unsupported(format!("resampler: {e}"));
    let input =
        InterleavedSlice::new(&rs.inbuf[..], ch, rs.chunk_in).map_err(|e| resample_err(&e))?;
    let mut output = InterleavedSlice::new_mut(&mut rs.outbuf[..], ch, rs.chunk_out)
        .map_err(|e| resample_err(&e))?;
    let (_, produced) = rs
        .fft
        .process_into_buffer(&input, &mut output, None)
        .map_err(|e| resample_err(&e))?;
    let skip = usize::try_from(rs.discard)
        .unwrap_or(usize::MAX)
        .min(produced);
    rs.discard -= skip as u64;
    rs.out_pos = skip;
    rs.out_len = produced;
    Ok(filled as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::{Media, Preloaded, open_source};
    use std::f64::consts::TAU;

    fn sine(frames: usize, rate: u32, freq: f64) -> Vec<f32> {
        (0..frames)
            .map(|i| (0.5 * (TAU * freq * i as f64 / f64::from(rate)).sin()) as f32)
            .collect()
    }

    fn pipeline(samples: Vec<f32>, rate: u32, device_rate: u32) -> Pipeline {
        let p = Preloaded::from_samples(1, rate, samples);
        let frames = p.info().frames;
        Pipeline::new(
            open_source(Media::Preloaded(p)).unwrap(),
            frames,
            device_rate,
        )
        .unwrap()
    }

    fn read_all(p: &mut Pipeline) -> Vec<f32> {
        let mut out = Vec::new();
        let ch = p.channels();
        let mut buf = vec![0.0; 777 * ch];
        loop {
            let n = p.read(&mut buf).unwrap();
            if n == 0 {
                return out;
            }
            out.extend_from_slice(&buf[..n * ch]);
        }
    }

    /// The resampled signal sits on the device-rate grid: frame `p` is the source at `p / rate`.
    #[test]
    fn resampled_length_frequency_and_alignment_are_exact() {
        for (from, to) in [
            (44_100, 48_000),
            (48_000, 44_100),
            (22_050, 48_000),
            (96_000, 48_000),
            (8_000, 48_000),
        ] {
            let mut p = pipeline(sine(from as usize, from, 1000.0), from, to);
            let out = read_all(&mut p);
            assert_eq!(out.len(), to as usize, "{from}->{to}");
            // Away from the edges (where the source starts/stops abruptly) it matches the ideal.
            let edge = to as usize / 100;
            // Best-fit offset from the ideal grid, in hundredths of a frame.
            let fit = |d: f64| {
                (edge..out.len() - edge)
                    .map(|i| {
                        let t = (i as f64 - d) / f64::from(to);
                        (f64::from(out[i]) - 0.5 * (TAU * 1000.0 * t).sin()).abs()
                    })
                    .fold(0.0, f64::max)
            };
            let (best, err) = (-100i32..=100)
                .map(|d| (d, fit(f64::from(d) / 100.0)))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap();
            assert!(best.abs() <= 50, "{from}->{to}: offset {best}/100 frame");
            assert!(
                err < 1e-3,
                "{from}->{to}: max error {err} at offset {best}/100"
            );
        }
    }

    /// A seek restarts the resampler, yet lands on the same grid as reading through.
    #[test]
    fn seeking_matches_reading_through() {
        let src = sine(30_000, 44_100, 997.0);
        let through = read_all(&mut pipeline(src.clone(), 44_100, 48_000));
        let mut p = pipeline(src, 44_100, 48_000);
        for at in [
            0usize,
            1,
            159,
            160,
            1119,
            1120,
            1121,
            5000,
            20_000,
            through.len() - 10,
        ] {
            p.seek(at as u64).unwrap();
            assert_eq!(p.position(), at as u64);
            let mut buf = vec![0.0; 300];
            let n = p.read(&mut buf).unwrap();
            let want = &through[at..(at + 300).min(through.len())];
            assert_eq!(n, want.len());
            let err = buf[..n]
                .iter()
                .zip(want)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f32::max);
            assert!(err < 1e-5, "seek to {at}: max error {err}");
        }
        p.seek(through.len() as u64 + 5).unwrap();
        assert_eq!(p.read(&mut [0.0; 8]).unwrap(), 0);
    }

    /// Seeking lands on the reading-through grid when downsampling too, and in stereo.
    #[test]
    fn seeking_matches_reading_through_downsampling_stereo() {
        let src: Vec<f32> = sine(30_000, 48_000, 997.0)
            .into_iter()
            .zip(sine(30_000, 48_000, 313.0))
            .flat_map(|(l, r)| [l, r])
            .collect();
        let open = || {
            let p = Preloaded::from_samples(2, 48_000, src.clone());
            Pipeline::new(open_source(Media::Preloaded(p)).unwrap(), 30_000, 44_100).unwrap()
        };
        let through = read_all(&mut open());
        assert_eq!(through.len(), 2 * 27_563);
        let mut p = open();
        for at in [0usize, 1, 146, 147, 1029, 9_999, 27_000] {
            p.seek(at as u64).unwrap();
            let mut buf = vec![0.0; 2 * 200];
            let n = p.read(&mut buf).unwrap();
            let want = &through[2 * at..(2 * (at + 200)).min(through.len())];
            assert_eq!(2 * n, want.len());
            let err = buf[..2 * n]
                .iter()
                .zip(want)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f32::max);
            assert!(err < 1e-5, "seek to {at}: max error {err}");
        }
    }

    /// Lengths stay exact for sources shorter than one resampler chunk and at extreme ratios.
    #[test]
    fn short_sources_and_extreme_ratios_have_exact_lengths() {
        for (frames, from, to) in [
            (1usize, 44_100, 48_000),
            (7, 48_000, 44_100),
            (100, 22_050, 48_000),
            (999, 192_000, 44_100),
            (5_000, 8_000, 192_000),
        ] {
            let mut p = pipeline(sine(frames, from, 100.0), from, to);
            let want = scale_round(frames as u64, to, from) as usize;
            assert_eq!(read_all(&mut p).len(), want, "{frames} at {from}->{to}");
        }
    }

    /// A source that ends before its probed length (the file shrank) is an error, not padding.
    #[test]
    fn a_source_shorter_than_probed_is_truncated() {
        for (rate, device) in [(48_000, 48_000), (44_100, 48_000)] {
            let p = Preloaded::from_samples(1, rate, sine(10_000, rate, 440.0));
            let mut pipe =
                Pipeline::new(open_source(Media::Preloaded(p)).unwrap(), 20_000, device).unwrap();
            let mut buf = vec![0.0; 777];
            let r = loop {
                match pipe.read(&mut buf) {
                    Ok(0) => break Ok(0),
                    Ok(_) => continue,
                    Err(e) => break Err(e),
                }
            };
            assert!(
                matches!(
                    r,
                    Err(MediaError::Truncated {
                        expected: 20_000,
                        actual: 10_000
                    })
                ),
                "{rate}->{device}: {r:?}"
            );
        }
    }

    #[test]
    fn equal_rates_pass_through_exactly() {
        let src = sine(5000, 48_000, 440.0);
        let mut p = pipeline(src.clone(), 48_000, 48_000);
        assert_eq!(read_all(&mut p), src);
        p.seek(1234).unwrap();
        let mut buf = vec![0.0; 10];
        p.read(&mut buf).unwrap();
        assert_eq!(buf, src[1234..1244]);
    }

    /// CPU cost of high-quality 48k↔44.1k stereo resampling. Run with
    /// `cargo test --release -p openlab-audio resample_cpu_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "benchmark; prints timing"]
    fn resample_cpu_cost() {
        for (from, to) in [(44_100u32, 48_000u32), (48_000, 44_100)] {
            let secs = 60;
            let src: Vec<f32> = sine(from as usize * secs, from, 1000.0)
                .into_iter()
                .flat_map(|s| [s, -s])
                .collect();
            let p = Preloaded::from_samples(2, from, src);
            let frames = p.info().frames;
            let mut pipe =
                Pipeline::new(open_source(Media::Preloaded(p)).unwrap(), frames, to).unwrap();
            let mut buf = vec![0.0; 4096 * 2];
            let t = std::time::Instant::now();
            while pipe.read(&mut buf).unwrap() > 0 {}
            let el = t.elapsed().as_secs_f64();
            println!(
                "{from}->{to} stereo: {secs} s of audio in {el:.3} s = {:.0}x real time",
                secs as f64 / el
            );
        }
    }
}
