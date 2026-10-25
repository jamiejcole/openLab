// SPDX-License-Identifier: GPL-3.0-or-later

//! A tiny seeded pseudo-random generator (SplitMix64) for random groups and shuffled playlists.
//! Core stays deterministic: the seed comes from `EngineConfig`, so the same seed and inputs
//! always make the same choices. Not for anything security-related.

#[derive(Clone, Debug)]
pub(super) struct SplitMix64(u64);

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..n` (0 when `n` is 0). Multiply-shift keeps the bias below 2^-64 per value.
    pub fn below(&mut self, n: usize) -> usize {
        let wide = u128::from(self.next_u64()) * n as u128;
        usize::try_from(wide >> 64).unwrap_or(0)
    }

    /// Fisher–Yates shuffle.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i + 1);
            items.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let mut a = SplitMix64::new(42);
        let mut b = SplitMix64::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        assert_ne!(SplitMix64::new(1).next_u64(), SplitMix64::new(2).next_u64());
    }

    #[test]
    fn known_first_value() {
        // Reference value of SplitMix64 for seed 0.
        assert_eq!(SplitMix64::new(0).next_u64(), 0xE220_A839_7B1D_CDAF);
    }

    #[test]
    fn below_stays_in_range_and_hits_every_value() {
        let mut rng = SplitMix64::new(7);
        let mut seen = [false; 5];
        for _ in 0..1000 {
            let v = rng.below(5);
            assert!(v < 5);
            seen[v] = true;
        }
        assert!(seen.iter().all(|s| *s));
        assert_eq!(rng.below(0), 0);
        assert_eq!(rng.below(1), 0);
    }

    #[test]
    fn shuffle_is_a_permutation() {
        let mut rng = SplitMix64::new(3);
        let mut items: Vec<u32> = (0..20).collect();
        rng.shuffle(&mut items);
        let mut sorted = items.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..20).collect::<Vec<_>>());
        assert_ne!(items, sorted);
    }
}
