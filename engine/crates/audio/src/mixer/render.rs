// SPDX-License-Identifier: GPL-3.0-or-later

//! Rendering a segment: voices into patch buses, patch matrices into device outputs, then the
//! master.

use std::sync::atomic::Ordering;

use crate::meter::track_all;

use super::voice::{Move, ReadOutcome, Target, Voice};
use super::{Event, FinishReason, Mixer, Outbox, Patch, SampleTime, Scratch, Stage};

impl Mixer {
    /// Renders frames `a..a + len` of the chunk: voices → patch buses → patch matrices →
    /// master.
    pub(super) fn render(&mut self, out: &mut [f32], a: usize, len: usize) {
        let t0 = self.now + a as u64;
        let (block, outs) = (self.block, self.outputs);
        let Mixer {
            voices,
            outbox,
            patches,
            out_health,
            master,
            master_health,
            dev,
            move_bus,
            out_peak,
            scratch,
            stage,
            #[cfg(test)]
            panic_voice,
            ..
        } = self;

        patches.iter_mut().for_each(|p| p.live = false);
        for (i, v) in voices.iter_mut().enumerate().filter(|(_, v)| v.is_active()) {
            stage.enter(Stage::Voice(i));
            #[cfg(test)]
            if *panic_voice == Some(v.id) {
                panic!("injected voice fault");
            }
            render_one(v, len, t0, block, patches, move_bus, scratch, outbox);
        }
        stage.enter(Stage::Idle);

        let Some(tmp) = scratch.tmp.get_mut(..len) else {
            return;
        };
        clear(dev, block, len);
        // A disabled patch is not mixed: its voices play on into a bus nobody hears.
        for (k, p) in patches
            .iter_mut()
            .enumerate()
            .filter(|(_, p)| !p.health.disabled)
        {
            stage.enter(Stage::Patch(k));
            // Rows no voice wrote to in this segment are not mixed (their bus is stale), but a
            // ramp in them still moves on.
            let mixed = if p.live { p.rows } else { 0 };
            let was_dirty = std::mem::take(&mut p.dirty);
            if was_dirty && p.dirty_rows > mixed {
                let rest = p
                    .gains
                    .get_mut(mixed * outs..p.dirty_rows * outs)
                    .unwrap_or_default();
                for g in rest.iter_mut().filter(|g| g.is_ramping()) {
                    g.advance(len);
                    p.dirty |= g.is_ramping();
                }
            }
            if !p.live {
                if !p.dirty {
                    p.dirty_rows = 0;
                }
                continue;
            }
            for (d, dv) in dev.chunks_exact_mut(block).enumerate() {
                for (o, bo) in p.bus.chunks_exact(block).enumerate().take(mixed) {
                    let Some(g) = p.gains.get_mut(o * outs + d) else {
                        continue;
                    };
                    if g.is_silent() {
                        continue;
                    }
                    if let Some(g) = g.steady() {
                        dv.iter_mut()
                            .zip(bo)
                            .take(len)
                            .for_each(|(y, x)| *y += x * g);
                        continue;
                    }
                    g.fill(tmp, &mut scratch.shapes);
                    p.dirty |= g.is_ramping();
                    for ((y, x), g) in dv.iter_mut().zip(bo).zip(tmp.iter()) {
                        *y += x * g;
                    }
                }
            }
            if !p.dirty {
                p.dirty_rows = 0;
            }
        }

        let Some(mb) = scratch.master.get_mut(..len) else {
            return;
        };
        if master_health.disabled {
            mb.fill(1.0);
        } else {
            stage.enter(Stage::Master);
            master.fill(mb, &mut scratch.shapes);
        }
        for (o, ((dv, pk), h)) in dev
            .chunks_exact_mut(block)
            .zip(out_peak.iter_mut())
            .zip(out_health.iter())
            .enumerate()
        {
            let Some(dv) = dv.get_mut(..len) else {
                continue;
            };
            if h.disabled {
                dv.fill(0.0);
                continue;
            }
            stage.enter(Stage::Output(o));
            dv.iter_mut().zip(mb.iter()).for_each(|(y, m)| *y *= m);
            track_all(pk, dv);
        }
        stage.enter(Stage::Idle);

        for (f, frame) in out.chunks_exact_mut(outs).skip(a).take(len).enumerate() {
            for (d, s) in frame.iter_mut().enumerate() {
                *s = dev.get(d * block + f).copied().unwrap_or(0.0);
            }
        }
    }
}

/// Renders `len` frames of one active voice from sample `t0` into its patch's bus (both patches
/// of a move), and notes that it has played them.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_one(
    v: &mut Voice,
    len: usize,
    t0: SampleTime,
    block: usize,
    patches: &mut [Patch],
    move_bus: &mut [f32],
    s: &mut Scratch,
    outbox: &mut Outbox,
) {
    if v.tail_over() {
        // A paused voice past its declick tails contributes nothing; a move it was making is
        // over, there is nothing left to crossfade.
        if v.moving.is_some() {
            v.advance_move(usize::MAX);
            let (voice, moves) = (v.id, v.moves);
            outbox.emit(Event::MoveEnded {
                voice,
                at: t0,
                moves,
            });
        }
    } else if let Some(m) = v.moving {
        render_moving(v, m, len, t0, block, patches, move_bus, s, outbox);
    } else if let Some(p) = patches.get_mut(v.patch) {
        p.reach(v.width, block, len);
        if v.paused.is_some() {
            render_tail(v, len, t0, block, &mut p.bus, s, outbox);
        } else {
            render_voice(v, len, t0, block, &mut p.bus, s, outbox);
        }
    }
    // Last: a voice whose render faulted has not played these frames.
    v.upto = t0.saturating_add(len as u64);
}

/// Renders a moving voice ([`Move`]) into `move_bus`, then shares it out: `1 - g` of it to the
/// patch it leaves, `g` to the one it moves to.
#[allow(clippy::too_many_arguments)]
fn render_moving(
    v: &mut Voice,
    m: Move,
    len: usize,
    t0: SampleTime,
    block: usize,
    patches: &mut [Patch],
    move_bus: &mut [f32],
    s: &mut Scratch,
    outbox: &mut Outbox,
) {
    let width = v.width;
    if let Some(b) = move_bus.get_mut(..width * block) {
        clear(b, block, len);
    }
    let tail = if v.paused.is_some() {
        Some(render_tail(v, len, t0, block, move_bus, s, outbox))
    } else {
        render_voice(v, len, t0, block, move_bus, s, outbox);
        None
    };
    for (k, leaving) in [(m.from, true), (v.patch, false)] {
        let Some(p) = patches.get_mut(k) else {
            continue;
        };
        p.reach(width, block, len);
        for (bo, vo) in p
            .bus
            .chunks_exact_mut(block)
            .zip(move_bus.chunks_exact(block))
            .take(width)
        {
            for (j, (y, x)) in bo.iter_mut().zip(vo).take(len).enumerate() {
                let g = m.gain(j);
                *y += x * if leaving { 1.0 - g } else { g };
            }
        }
    }
    // A paused voice whose last tail ended in this segment has nothing left to crossfade: the
    // move is over on that sample, wherever the segment ends.
    let played = match tail {
        Some(sounded) if v.tail_over() => {
            v.advance_move(usize::MAX);
            sounded
        }
        _ => {
            v.advance_move(len);
            len
        }
    };
    // A voice that ended meanwhile was reported finished instead.
    if v.moving.is_none() && v.is_active() {
        let at = t0.saturating_add(played as u64);
        let (voice, moves) = (v.id, v.moves);
        outbox.emit(Event::MoveEnded { voice, at, moves });
    }
}

fn render_voice(
    v: &mut Voice,
    len: usize,
    t0: SampleTime,
    block: usize,
    bus: &mut [f32],
    s: &mut Scratch,
    outbox: &mut Outbox,
) {
    // Nothing after a stop fade ends is read, played or counted as underrun.
    let live = v.stop_end.map_or(len, |e| {
        usize::try_from(e.saturating_sub(t0))
            .unwrap_or(len)
            .min(len)
    });
    let pos0 = v.pos;
    let (got, outcome, rem) = v.read(live, len, block, t0, outbox);
    // The voice has moved on: a fault from here on must not make it play these frames again.
    v.upto = t0.saturating_add(len as u64);
    let (Some(vg), Some(og), Some(tmp), Some(acc)) = (
        s.vgain.get_mut(..len),
        s.og.get_mut(..len),
        s.tmp.get_mut(..len),
        s.acc.get_mut(..len),
    ) else {
        return;
    };

    // Voice gain: master × duck × stop fade × integrated fade envelope. Multiplying by an exact
    // 1.0 is an identity, so skipping it keeps output independent of block segmentation, and a
    // voice nothing ducks pays nothing for the duck stage.
    let shapes = &mut s.shapes;
    v.master.fill(vg, shapes);
    if v.duck.is_ramping() || v.duck.current() != 1.0 {
        v.duck.fill(tmp);
        // A boost stops at the cap (but never takes the voice below its own master), so a
        // master raised while a boost comes down cannot overshoot the workspace maximum. The
        // cap moves over the de-zipper like any gain, so a lowered maximum is no step; `og` is
        // free until the outputs are mixed below.
        if v.duck_cap.is_ramping() {
            v.duck_cap.fill(og, shapes);
        } else {
            og.fill(v.duck_cap.current());
        }
        for ((g, d), cap) in vg.iter_mut().zip(tmp.iter()).zip(og.iter()) {
            let m = *g;
            *g = m * d;
            if *d > 1.0 {
                *g = g.min(cap.max(m));
            }
        }
    } else if v.duck_cap.is_ramping() {
        v.duck_cap.advance(len);
    }
    if v.stop.is_ramping() || v.stop.current() != 1.0 {
        v.stop.fill(tmp, shapes);
        vg.iter_mut().zip(tmp.iter()).for_each(|(g, t)| *g *= t);
    }
    // The declick acts on the input, so the tails it crossfades with share the voice's gains.
    if v.declick.is_ramping() || v.declick.current() != 1.0 {
        v.declick.fill(tmp, shapes);
        for inp in v.input.chunks_mut(block).take(v.channels) {
            inp.iter_mut().zip(tmp.iter()).for_each(|(x, d)| *x *= d);
        }
    }
    // On the samples a kept tail still sounds on, the voice's envelope goes on its input and not
    // on the gain the tail shares: the tail has its own, of the frames it plays.
    let xn = v.xlen.saturating_sub(v.xdone).min(len);
    let kept = match (og.get_mut(..xn), tmp.get_mut(..xn), vg.get(..xn)) {
        (Some(env), Some(plain), Some(g)) if xn > 0 => {
            env.fill(1.0);
            v.apply_envelope(env, pos0, got, &rem);
            plain.copy_from_slice(g);
            Some((env, plain))
        }
        _ => None,
    };
    v.apply_envelope(vg, pos0, got, &rem);
    if let Some((env, plain)) = kept {
        vg.iter_mut().zip(plain.iter()).for_each(|(g, p)| *g = *p);
        let Voice {
            input,
            xbuf,
            xstride,
            xdone,
            channels,
            ..
        } = v;
        let tails = xbuf
            .chunks(*xstride)
            .map(|b| b.get(*xdone..).unwrap_or_default());
        for (inp, tail) in input.chunks_mut(block).zip(tails).take(*channels) {
            for ((x, e), t) in inp.iter_mut().zip(env.iter()).zip(tail) {
                *x = *x * e + t;
            }
        }
        *xdone += xn;
    }

    let Voice {
        cue_out,
        xp,
        input,
        channels,
        meter,
        width,
        ..
    } = v;
    let (outs, ch) = (cue_out.len(), *channels);
    for (o, co) in cue_out.iter_mut().enumerate().take(*width) {
        let audible = (0..ch).any(|i| xp.get(i * outs + o).is_some_and(|p| !p.is_silent()));
        if !audible || co.is_silent() {
            co.advance(len);
            xp.iter_mut()
                .skip(o)
                .step_by(outs)
                .take(ch)
                .for_each(|p| p.advance(len));
            continue;
        }
        // A steady gain is the value `fill` would write to every frame: the same products.
        match co.steady() {
            Some(c) => og.iter_mut().zip(vg.iter()).for_each(|(g, v)| *g = c * v),
            None => {
                co.fill(og, shapes);
                og.iter_mut().zip(vg.iter()).for_each(|(g, v)| *g *= v);
            }
        }
        let Some(bo) = bus.get_mut(o * block..o * block + len) else {
            continue;
        };
        let inputs = input.chunks(block).take(ch);
        mix_output(bo, acc, tmp, inputs, og, meter.get_mut(o), |i, tmp| {
            let Some(p) = xp.get_mut(i * outs + o).filter(|p| !p.is_silent()) else {
                return XGain::Skip;
            };
            match p.steady() {
                Some(g) => XGain::Steady(g),
                None => {
                    p.fill(tmp, shapes);
                    XGain::Frames
                }
            }
        });
        #[cfg(test)]
        {
            s.metered += acc.len() as u64;
        }
    }
    v.fresh = false;

    let mut end = match outcome {
        ReadOutcome::Ended(r) => Some((t0 + got as u64, r)),
        ReadOutcome::Underrun => {
            outbox
                .shared
                .underrun_frames
                .fetch_add((live - got) as u64, Ordering::Relaxed);
            v.missed = v.missed.saturating_add((live - got) as u64);
            if !v.underrunning {
                v.underrunning = true;
                outbox.emit(Event::Underrun {
                    voice: v.id,
                    at: t0 + got as u64,
                });
            }
            None
        }
        ReadOutcome::Full => {
            // What the voice missed is told until the queue takes it, with what it missed
            // since: a picture that follows this voice is put back by this count, and one
            // lost with a full queue would leave it ahead of the sound for the rest of the run.
            if v.missed > 0 {
                let end = Event::UnderrunEnded {
                    voice: v.id,
                    at: t0,
                    frames: v.missed,
                };
                if outbox.try_emit(end, v.underrunning) {
                    v.missed = 0;
                }
            }
            v.underrunning = false;
            None
        }
    };
    if let Some(e) = v.stop_end
        && e < t0 + len as u64
        && end.is_none_or(|(at, _)| e < at)
    {
        end = Some((e, FinishReason::Stopped));
    }
    if let Some((at, reason)) = end {
        v.finish(at, reason, outbox);
    }
}

/// Renders the declick fade-out of a paused voice: the frames after its pause point, peeked
/// without consuming them, at the gains it had when it paused (none of them advance), times the
/// falling declick ramp and the integrated fade envelope those frames would have had; and what
/// is left of the tails earlier resumes cut short. A voice stopped meanwhile ends with the last
/// of them. Returns the frames of this segment a tail sounded on.
fn render_tail(
    v: &mut Voice,
    len: usize,
    t0: SampleTime,
    block: usize,
    bus: &mut [f32],
    s: &mut Scratch,
    outbox: &mut Outbox,
) -> usize {
    let Some(p) = v.paused else { return 0 };
    let n = usize::try_from(p.len.saturating_sub(p.done))
        .unwrap_or(usize::MAX)
        .min(len);
    let got = v.peek(p.done, n, Target::Input(block));
    let (m, d) = (v.master.current(), v.duck.current());
    let ducked = if d > 1.0 {
        (m * d).min(v.duck_cap.current().max(m))
    } else {
        m * d
    };
    let g = ducked * v.stop.current();
    if let Some(vg) = s.vgain.get_mut(..n) {
        v.tail_gain(p, p.done, got, g * v.declick.current(), vg);
        mix_frozen(v, false, got, n, block, bus, s);
    }
    // The kept tails carry their own declick and envelope.
    let xn = v.xlen.saturating_sub(v.xdone).min(len);
    if xn > 0
        && let Some(vg) = s.vgain.get_mut(..xn)
    {
        vg.fill(g);
        mix_frozen(v, true, xn, xn, block, bus, s);
    }
    v.xdone += xn;
    if let Some(p) = v.paused.as_mut() {
        p.done += n as u64;
    }
    let sounded = n.max(xn);
    if p.stopped && v.tail_over() {
        let at = t0.saturating_add(sounded as u64);
        v.finish(at, FinishReason::Stopped, outbox);
    }
    sounded
}

/// Mixes `got` frames of a paused voice into its outputs at the gains it paused with (none of
/// them advance), times the first `n` per-frame gains in `s.vgain`: frames of `xbuf` from
/// `xdone` (`kept`), or of `input`.
fn mix_frozen(
    v: &mut Voice,
    kept: bool,
    got: usize,
    n: usize,
    block: usize,
    bus: &mut [f32],
    s: &mut Scratch,
) {
    let (Some(vg), Some(acc)) = (s.vgain.get(..n), s.acc.get_mut(..n)) else {
        return;
    };
    let shapes = &mut s.shapes;
    let Voice {
        cue_out,
        xp,
        input,
        xbuf,
        xstride,
        xdone,
        channels,
        meter,
        width,
        ..
    } = v;
    let (src, stride, from) = if kept {
        (&*xbuf, *xstride, *xdone)
    } else {
        (&*input, block, 0)
    };
    let (outs, ch) = (cue_out.len(), *channels);
    for (o, co) in cue_out.iter().enumerate().take(*width) {
        let co = co.current_in(shapes);
        // As in `render_voice`: an output nothing reaches costs nothing (Pause All on many
        // voices × outputs renders every tail in one block).
        if co == 0.0
            || (0..ch).all(|i| {
                xp.get(i * outs + o)
                    .is_none_or(|x| x.current_in(shapes) == 0.0)
            })
        {
            continue;
        }
        let Some(bo) = bus.get_mut(o * block..o * block + got) else {
            continue;
        };
        let inputs = src
            .chunks(stride)
            .take(ch)
            .map(|c| c.get(from..).unwrap_or_default());
        mix_output(bo, acc, &mut [], inputs, vg, meter.get_mut(o), |i, _| {
            let g = xp.get(i * outs + o).map_or(0.0, |x| x.current_in(shapes)) * co;
            if g == 0.0 {
                XGain::Skip
            } else {
                XGain::Steady(g)
            }
        });
        #[cfg(test)]
        {
            s.metered += acc.len() as u64;
        }
    }
}

/// Zeroes the first `len` frames of each `block`-frame channel of `bus`.
pub(super) fn clear(bus: &mut [f32], block: usize, len: usize) {
    for b in bus.chunks_exact_mut(block) {
        b.iter_mut().take(len).for_each(|x| *x = 0.0);
    }
}

/// Copies interleaved `ch`-channel frames, `a` then `b` (one run, split anywhere: an `rtrb`
/// chunk wraps mid-frame when the ring's size is not a whole number of frames), into planar
/// `out`: channel `c` of frame `f` goes to `out[c * stride + at + f]`.
#[inline]
pub(super) fn deinterleave(
    a: &[f32],
    b: &[f32],
    ch: usize,
    out: &mut [f32],
    stride: usize,
    at: usize,
) {
    let Some(frames) = (a.len() + b.len()).checked_div(ch) else {
        return;
    };
    // Frame `fa` may be split by the wrap: the last `cut` samples of `a`, then `head`.
    let (fa, cut) = (a.len() / ch, a.len() % ch);
    let (head, rest) = b.split_at(if cut == 0 { 0 } else { (ch - cut).min(b.len()) });
    let split = a.get(fa * ch..).unwrap_or_default().iter().chain(head);
    for (c, x) in split.enumerate() {
        if let Some(d) = out.get_mut(c * stride + at + fa) {
            *d = *x;
        }
    }
    for c in 0..ch {
        let Some(dst) = out.get_mut(c * stride + at..c * stride + at + frames) else {
            continue;
        };
        let (da, db) = dst.split_at_mut(fa.min(dst.len()));
        let db = db.get_mut(usize::from(cut > 0)..).unwrap_or_default();
        let src = a
            .chunks_exact(ch)
            .zip(da)
            .chain(rest.chunks_exact(ch).zip(db));
        for (fr, d) in src {
            if let Some(x) = fr.get(c) {
                *d = *x;
            }
        }
    }
}

/// A crosspoint's gain over a segment: none (the input is not mixed), one value, or one per
/// frame, written to `tmp`.
enum XGain {
    Skip,
    Steady(f32),
    Frames,
}

/// Mixes a voice's input channels into one output's bus segment `bo`: input `i` times
/// `gain(i, tmp)` times the per-frame gain `h`. What it adds is summed in `acc` for the meter.
#[inline]
fn mix_output<'a>(
    bo: &mut [f32],
    acc: &mut [f32],
    tmp: &mut [f32],
    inputs: impl Iterator<Item = &'a [f32]>,
    h: &[f32],
    meter: Option<&mut f32>,
    mut gain: impl FnMut(usize, &mut [f32]) -> XGain,
) {
    acc.fill(0.0);
    for (i, inp) in inputs.enumerate() {
        let g = gain(i, tmp);
        let Some(inp) = inp.get(..bo.len()) else {
            continue;
        };
        match g {
            XGain::Skip => {}
            XGain::Steady(g) => {
                for (((b, a), x), h) in bo.iter_mut().zip(acc.iter_mut()).zip(inp).zip(h) {
                    let y = x * g * h;
                    *b += y;
                    *a += y;
                }
            }
            XGain::Frames => {
                let gh = tmp.iter().zip(h);
                for (((b, a), x), (g, h)) in bo.iter_mut().zip(acc.iter_mut()).zip(inp).zip(gh) {
                    let y = x * g * h;
                    *b += y;
                    *a += y;
                }
            }
        }
    }
    if let Some(m) = meter {
        track_all(m, acc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OfflineRenderer;
    use crate::mixer::{CommandKind, MixerConfig, VoiceId, VoiceStart};
    use rtrb::RingBuffer;

    /// A stereo voice in a 128-cue-output mixer renders its 2 cue outputs only, and its patch
    /// clears and mixes 2 bus rows: the wide matrix costs a 2-output show nothing.
    #[test]
    fn a_two_output_voice_costs_two_cue_outputs_in_a_128_row_mixer() {
        let cfg = MixerConfig {
            cue_outputs: 128,
            ..MixerConfig::default()
        };
        let (mut ctl, mixer) = Mixer::new(cfg).unwrap();
        let (mut tx, rx) = RingBuffer::new(8192);
        tx.push_entire_slice(&[0.5; 8192]).unwrap();
        let mut v = VoiceStart::new(VoiceId(1), rx, 2, 2);
        v.set_crosspoint(0, 0, 1.0);
        v.set_crosspoint(1, 1, 1.0);
        ctl.send(0, CommandKind::StartVoice(Box::new(v)));
        let mut r = OfflineRenderer::new(mixer);
        r.render(1000, &[]);
        let m = r.mixer_mut();
        assert_eq!(m.scratch.metered, 2 * 1000, "two outputs metered per frame");
        assert_eq!(m.voices[0].width, 2);
        assert_eq!(m.patches[0].rows, 2);
        assert!(m.patches[0].live);
    }

    #[test]
    fn a_pause_tail_meters_only_the_outputs_it_reaches() {
        let cfg = MixerConfig {
            output_channels: 8,
            ..MixerConfig::default()
        };
        let (mut ctl, mixer) = Mixer::new(cfg).unwrap();
        let (mut tx, rx) = RingBuffer::new(8192);
        tx.push_entire_slice(&[0.5; 8192]).unwrap();
        let mut v = VoiceStart::new(VoiceId(1), rx, 1, 8);
        v.set_crosspoint(0, 2, 1.0);
        ctl.send(0, CommandKind::StartVoice(Box::new(v)));
        ctl.send(1000, CommandKind::PauseVoice { voice: VoiceId(1) });
        let mut r = OfflineRenderer::new(mixer);
        r.render(1000, &[]);
        ctl.voice_meters();
        let before = r.mixer_mut().scratch.metered;
        let out = r.render(1000, &[]);
        // The 240-frame declick tail on the one routed output, nothing for the other seven.
        assert_eq!(r.mixer_mut().scratch.metered - before, 240);
        let peaks = ctl.voice_meters();
        assert_eq!(peaks.len(), 1);
        assert!(
            peaks[0]
                .1
                .iter()
                .enumerate()
                .all(|(o, p)| (o == 2) == (*p > 0.0))
        );
        assert!(
            out.chunks(8)
                .all(|f| f.iter().enumerate().all(|(o, x)| o == 2 || *x == 0.0))
        );
    }
}
