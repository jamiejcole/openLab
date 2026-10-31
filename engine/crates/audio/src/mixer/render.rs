// SPDX-License-Identifier: GPL-3.0-or-later

//! Rendering a segment: voices into patch buses, patch matrices into device outputs, then the
//! master.

use std::sync::atomic::Ordering;

use crate::meter::track_all;

use super::voice::{Move, ReadOutcome, Remaining, Target, Voice};
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
            // A paused voice past its declick tail contributes nothing and is not visited; a
            // move it was making is over, there is nothing left to crossfade.
            if v.paused.is_some_and(|p| p.done >= p.len) {
                if v.moving.is_some() {
                    v.advance_move(usize::MAX);
                    let (voice, moves) = (v.id, v.moves);
                    outbox.emit(Event::MoveEnded {
                        voice,
                        at: t0,
                        moves,
                    });
                }
                continue;
            }
            if let Some(m) = v.moving {
                render_moving(v, m, len, t0, block, patches, move_bus, scratch, outbox);
                continue;
            }
            let Some(p) = patches.get_mut(v.patch) else {
                continue;
            };
            if !p.live {
                p.live = true;
                clear(&mut p.bus, block, len);
            }
            if v.paused.is_some() {
                render_tail(v, len, block, &mut p.bus, scratch);
            } else {
                render_voice(v, len, t0, block, &mut p.bus, scratch, outbox);
            }
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
            if !p.live {
                if p.dirty {
                    p.dirty = false;
                    for g in p.gains.iter_mut().filter(|g| g.is_ramping()) {
                        g.advance(len);
                        p.dirty |= g.is_ramping();
                    }
                }
                continue;
            }
            p.dirty = false;
            for (d, dv) in dev.chunks_exact_mut(block).enumerate() {
                for (o, bo) in p.bus.chunks_exact(block).enumerate() {
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
                    g.fill(tmp);
                    p.dirty |= g.is_ramping();
                    for ((y, x), g) in dv.iter_mut().zip(bo).zip(tmp.iter()) {
                        *y += x * g;
                    }
                }
            }
        }

        let Some(mb) = scratch.master.get_mut(..len) else {
            return;
        };
        if master_health.disabled {
            mb.fill(1.0);
        } else {
            stage.enter(Stage::Master);
            master.fill(mb);
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
    clear(move_bus, block, len);
    if v.paused.is_some() {
        render_tail(v, len, block, move_bus, s);
    } else {
        render_voice(v, len, t0, block, move_bus, s, outbox);
    }
    for (k, leaving) in [(m.from, true), (v.patch, false)] {
        let Some(p) = patches.get_mut(k) else {
            continue;
        };
        if !p.live {
            p.live = true;
            clear(&mut p.bus, block, len);
        }
        for (bo, vo) in p
            .bus
            .chunks_exact_mut(block)
            .zip(move_bus.chunks_exact(block))
        {
            for (j, (y, x)) in bo.iter_mut().zip(vo).take(len).enumerate() {
                let g = m.gain(j);
                *y += x * if leaving { 1.0 - g } else { g };
            }
        }
    }
    v.advance_move(len);
    // A voice that ended meanwhile was reported finished instead.
    if v.moving.is_none() && v.is_active() {
        let at = t0.saturating_add(len as u64);
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
    // The crossfaded tail is peeked before the read moves the voice on.
    let xfade = v.xfade.map(|x| {
        let n = usize::try_from(x.tail.len.saturating_sub(x.tail.done))
            .unwrap_or(usize::MAX)
            .min(len);
        let from = x.tail.done.saturating_sub(v.pos.saturating_sub(x.pos_at));
        (x, n, v.peek(from, n, Target::Xfade))
    });
    let (got, outcome, rem) = v.read(live, len, block, t0, outbox);
    let (Some(vg), Some(og), Some(tmp), Some(acc)) = (
        s.vgain.get_mut(..len),
        s.og.get_mut(..len),
        s.tmp.get_mut(..len),
        s.acc.get_mut(..len),
    ) else {
        return;
    };

    // Voice gain: master × stop fade × integrated fade envelope. Multiplying by an exact 1.0 is
    // an identity, so skipping it keeps output independent of block segmentation.
    v.master.fill(vg);
    if v.stop.is_ramping() || v.stop.current() != 1.0 {
        v.stop.fill(tmp);
        vg.iter_mut().zip(tmp.iter()).for_each(|(g, t)| *g *= t);
    }
    // The declick acts on the input, so the tail it crossfades with shares the voice's gains.
    if v.declick.is_ramping() || v.declick.current() != 1.0 {
        v.declick.fill(tmp);
        let Voice {
            input,
            xbuf,
            xstride,
            channels,
            ..
        } = v;
        for c in 0..*channels {
            let Some(inp) = input.get_mut(c * block..c * block + len) else {
                continue;
            };
            inp.iter_mut().zip(tmp.iter()).for_each(|(x, d)| *x *= d);
            if let Some((x, _, xgot)) = xfade
                && let Some(tail) = xbuf.get(c * *xstride..c * *xstride + xgot)
            {
                for (j, (y, t)) in inp.iter_mut().zip(tail).enumerate() {
                    *y += t * x.scale * x.tail.gain(x.tail.done + j as u64);
                }
            }
        }
    }
    if let Some((mut x, n, _)) = xfade {
        x.tail.done += n as u64;
        v.xfade = (x.tail.done < x.tail.len).then_some(x);
    }
    v.apply_envelope(vg, pos0, got, &rem);

    let Voice {
        cue_out,
        xp,
        input,
        channels,
        meter,
        ..
    } = v;
    let (outs, ch) = (cue_out.len(), *channels);
    for (o, co) in cue_out.iter_mut().enumerate() {
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
                co.fill(og);
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
                    p.fill(tmp);
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
/// falling declick ramp and the integrated fade envelope those frames would have had.
fn render_tail(v: &mut Voice, len: usize, block: usize, bus: &mut [f32], s: &mut Scratch) {
    let Some(p) = v.paused else { return };
    let n = usize::try_from(p.len.saturating_sub(p.done))
        .unwrap_or(usize::MAX)
        .min(len);
    let got = v.peek(p.done, n, Target::Input(block));
    let (Some(vg), Some(acc)) = (s.vgain.get_mut(..n), s.acc.get_mut(..n)) else {
        return;
    };
    let g = v.master.current() * v.stop.current() * v.declick.current();
    for (j, x) in vg.iter_mut().enumerate() {
        *x = g * p.gain(p.done + j as u64);
    }
    let rem = Remaining {
        first: v.remaining().map(|r| r.saturating_sub(p.done)),
        split: usize::MAX,
        after: None,
        refade_first: v.refade,
        refade_after: None,
    };
    v.apply_envelope(vg, v.pos.saturating_add(p.done), got, &rem);

    let Voice {
        cue_out,
        xp,
        input,
        channels,
        meter,
        ..
    } = v;
    let (outs, ch) = (cue_out.len(), *channels);
    for (o, co) in cue_out.iter().enumerate() {
        let co = co.current();
        // As in `render_voice`: an output nothing reaches costs nothing (Pause All on many
        // voices × outputs renders every tail in one block).
        if co == 0.0 || (0..ch).all(|i| xp.get(i * outs + o).is_none_or(|x| x.current() == 0.0)) {
            continue;
        }
        let Some(bo) = bus.get_mut(o * block..o * block + got) else {
            continue;
        };
        let inputs = input.chunks(block).take(ch);
        mix_output(bo, acc, &mut [], inputs, vg, meter.get_mut(o), |i, _| {
            let g = xp.get(i * outs + o).map_or(0.0, |x| x.current()) * co;
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
    if let Some(p) = v.paused.as_mut() {
        p.done += n as u64;
    }
}

/// Zeroes the first `len` frames of each `block`-frame channel of `bus`.
fn clear(bus: &mut [f32], block: usize, len: usize) {
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
