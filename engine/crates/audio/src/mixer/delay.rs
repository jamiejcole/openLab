// SPDX-License-Identifier: GPL-3.0-or-later

//! Per-output delay lines (speaker alignment), crossfading between delay times.

pub(super) struct DelayLine {
    buf: Vec<f32>,
    write: usize,
    delay: usize,
    old_delay: usize,
    xfade: u64,
    xfade_len: u64,
    /// A change requested mid-crossfade; starts when the running crossfade ends.
    next: Option<usize>,
}

impl DelayLine {
    pub(super) fn new(max_delay: usize, xfade_len: u64) -> Self {
        DelayLine {
            buf: vec![0.0; max_delay + 1],
            write: 0,
            delay: 0,
            old_delay: 0,
            xfade: xfade_len,
            xfade_len,
            next: None,
        }
    }

    /// As new: no delay, no crossfade, silence stored. Clears the whole line (at most
    /// [`MAX_DELAY_SECONDS`] of samples), which only a fault does.
    pub(super) fn reset(&mut self) {
        self.buf.fill(0.0);
        self.write = 0;
        self.delay = 0;
        self.old_delay = 0;
        self.xfade = self.xfade_len;
        self.next = None;
    }

    pub(super) fn set_delay(&mut self, frames: usize) {
        let d = frames.min(self.buf.len() - 1);
        if self.xfade < self.xfade_len {
            self.next = Some(d);
            return;
        }
        // A request queued by the crossfade that just ended is superseded by this one.
        self.next = None;
        if d != self.delay {
            self.old_delay = self.delay;
            self.delay = d;
            self.xfade = 0;
        }
    }

    fn tap(&self, d: usize) -> f32 {
        let i = if self.write >= d {
            self.write - d
        } else {
            self.write + self.buf.len() - d
        };
        self.buf.get(i).copied().unwrap_or(0.0)
    }

    /// [`Self::tick`] of every sample of `xs`, in place. Without a delay or a crossfade, each
    /// sample is stored and comes straight back out, so only the store is done (a copy).
    pub(super) fn process(&mut self, xs: &mut [f32]) {
        let idle = self.xfade >= self.xfade_len && self.next.is_none_or(|d| d == self.delay);
        if !idle || self.delay != 0 {
            xs.iter_mut().for_each(|x| *x = self.tick(*x));
            return;
        }
        self.next = None;
        let mut src: &[f32] = xs;
        while !src.is_empty() {
            let room = self.buf.len().saturating_sub(self.write);
            let k = room.min(src.len());
            let (head, rest) = src.split_at(k);
            if let Some(dst) = self.buf.get_mut(self.write..self.write + k) {
                dst.copy_from_slice(head);
            }
            self.write += k;
            if self.write >= self.buf.len() {
                self.write = 0;
            }
            src = rest;
            if k == 0 {
                break;
            }
        }
    }

    fn tick(&mut self, x: f32) -> f32 {
        if self.xfade >= self.xfade_len
            && let Some(d) = self.next.take()
            && d != self.delay
        {
            self.old_delay = self.delay;
            self.delay = d;
            self.xfade = 0;
        }
        if let Some(s) = self.buf.get_mut(self.write) {
            *s = x;
        }
        let mut y = self.tap(self.delay);
        if self.xfade < self.xfade_len {
            let a = self.xfade as f32 / self.xfade_len as f32;
            y = self.tap(self.old_delay) * (1.0 - a) + y * a;
            self.xfade += 1;
        }
        self.write += 1;
        if self.write == self.buf.len() {
            self.write = 0;
        }
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_retarget_mid_crossfade_waits_for_the_running_one() {
        let mut d = DelayLine::new(100, 10);
        d.set_delay(20);
        for _ in 0..3 {
            d.tick(1.0);
        }
        d.set_delay(40);
        assert_eq!((d.delay, d.next), (20, Some(40)));
        for _ in 0..7 {
            d.tick(1.0);
        }
        d.tick(1.0);
        assert_eq!((d.old_delay, d.delay, d.next), (20, 40, None));
    }

    /// Review 2 M2: a change on the exact sample a crossfade ends supersedes the queued one.
    #[test]
    fn delay_change_on_the_sample_a_crossfade_ends_wins() {
        let mut d = DelayLine::new(100, 10);
        d.set_delay(20);
        for _ in 0..3 {
            d.tick(1.0);
        }
        d.set_delay(40);
        for _ in 0..7 {
            d.tick(1.0);
        }
        // Crossfade done, queued 40 not yet consumed.
        assert_eq!((d.xfade, d.next), (10, Some(40)));
        d.set_delay(60);
        for _ in 0..30 {
            d.tick(1.0);
        }
        assert_eq!((d.delay, d.next), (60, None));
    }

    /// `DelayLine::process` on a block gives the bits of `tick` on each sample, through delay
    /// changes, crossfades, queued changes and blocks longer than the line.
    #[test]
    fn delay_process_matches_tick_sample_for_sample() {
        let (mut a, mut b) = (DelayLine::new(50, 10), DelayLine::new(50, 10));
        let mut x = 0.0f32;
        let steps: [(Option<usize>, usize); 17] = [
            (None, 7),
            (None, 120),
            (Some(5), 3),
            (Some(0), 4),
            (None, 30),
            (Some(0), 9),
            (Some(30), 64),
            (Some(0), 2),
            (Some(12), 5),
            (None, 200),
            (Some(0), 51),
            // A same-value change left queued when the crossfade ends: the fast path must
            // drop it like `tick` does.
            (Some(5), 30),
            (Some(0), 3),
            (Some(3), 0),
            (Some(0), 7),
            (None, 20),
            (None, 77),
        ];
        let mut buf = [0.0f32; 200];
        for (delay, n) in steps {
            if let Some(d) = delay {
                a.set_delay(d);
                b.set_delay(d);
            }
            let xs = &mut buf[..n];
            for s in xs.iter_mut() {
                x += 0.37;
                *s = x.sin();
            }
            let want: Vec<f32> = xs.iter().map(|s| b.tick(*s)).collect();
            a.process(xs);
            assert_eq!(
                xs.iter().map(|s| s.to_bits()).collect::<Vec<_>>(),
                want.iter().map(|s| s.to_bits()).collect::<Vec<_>>()
            );
            assert_eq!(
                (a.write, a.delay, a.old_delay, a.xfade, a.next),
                (b.write, b.delay, b.old_delay, b.xfade, b.next)
            );
        }
    }
}
