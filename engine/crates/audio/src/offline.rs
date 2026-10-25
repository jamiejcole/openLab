// SPDX-License-Identifier: GPL-3.0-or-later

//! Offline render driver (ARCHITECTURE §3.6): runs the same mixer callback in a loop into memory,
//! as fast as the CPU allows.

use crate::mixer::Mixer;

pub struct OfflineRenderer {
    mixer: Mixer,
}

impl OfflineRenderer {
    pub fn new(mixer: Mixer) -> Self {
        OfflineRenderer { mixer }
    }

    pub fn mixer_mut(&mut self) -> &mut Mixer {
        &mut self.mixer
    }

    /// Renders `frames` interleaved frames using block sizes cycled from `blocks` (empty = one
    /// block; zero sizes count as 1).
    pub fn render(&mut self, frames: usize, blocks: &[usize]) -> Vec<f32> {
        let mut out = vec![0.0; frames * self.mixer.output_channels()];
        self.render_into(&mut out, blocks);
        out
    }

    /// As [`OfflineRenderer::render`], into a caller-provided buffer. Does not allocate.
    pub fn render_into(&mut self, out: &mut [f32], blocks: &[usize]) {
        let ch = self.mixer.output_channels();
        let total = out.len() / ch;
        let mut sizes = blocks.iter().copied().cycle();
        let mut done = 0;
        while done < total {
            let n = sizes.next().unwrap_or(total).clamp(1, total - done);
            if let Some(buf) = out.get_mut(done * ch..(done + n) * ch) {
                self.mixer.process_guarded(buf, n);
            }
            done += n;
        }
    }
}
