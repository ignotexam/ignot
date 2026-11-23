//! The random numbers behind every question: xoshiro128** (Blackman and Vigna), seeded from SHA-256.
//!
//! It is written out here instead of taken from a crate because an exam must regenerate bit for bit, on every
//! platform and for as long as the exam is worth checking.

use crate::hash::sha256;

/// A deterministic generator. Equal seeds give equal streams.
#[derive(Debug, Clone)]
pub struct Rng {
    s: [u32; 4],
}

impl Rng {
    /// Seeds from the SHA-256 of a text, the way each question of an exam is seeded.
    #[must_use]
    pub fn from_text(seed: &str) -> Self {
        let h = sha256(seed.as_bytes());
        let word = |i: usize| u32::from_le_bytes([h[i], h[i + 1], h[i + 2], h[i + 3]]);
        let mut s = [word(0), word(4), word(8), word(12)];
        if s == [0; 4] {
            s[0] = 1;
        }
        Self { s }
    }

    /// The next 32 bits.
    pub fn next_u32(&mut self) -> u32 {
        let result = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 9;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(11);
        result
    }

    /// A uniform integer in `lo..=hi`, by rejection so that no value is favoured. `lo` must not exceed `hi`.
    pub fn int(&mut self, lo: u32, hi: u32) -> u32 {
        debug_assert!(lo <= hi, "Rng::int: lo above hi");
        let range = u64::from(hi.saturating_sub(lo)) + 1;
        let limit = (1u64 << 32) / range * range;
        loop {
            let r = u64::from(self.next_u32());
            if r < limit {
                // r % range fits: range is at most 2^32 and lo + it is at most hi.
                return lo + u32::try_from(r % range).unwrap_or(0);
            }
        }
    }

    /// A uniform index into a collection of `len` items. `len` must not be zero.
    pub fn index(&mut self, len: usize) -> usize {
        debug_assert!(len > 0, "Rng::index: empty");
        let last = u32::try_from(len.saturating_sub(1)).unwrap_or(u32::MAX);
        self.int(0, last) as usize
    }

    /// One item, chosen uniformly. The slice must not be empty.
    pub fn choice<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.index(items.len())]
    }

    /// `k` distinct items in the order they are drawn. `k` must not exceed the number of items.
    pub fn sample<T: Clone>(&mut self, items: &[T], k: usize) -> Vec<T> {
        debug_assert!(k <= items.len(), "Rng::sample: k above the number of items");
        let mut pool = items.to_vec();
        let mut out = Vec::with_capacity(k);
        for _ in 0..k.min(items.len()) {
            let i = self.index(pool.len());
            out.push(pool.remove(i));
        }
        out
    }

    /// A uniform integer in `lo..=hi` for ranges wider than 32 bits. `lo` must not exceed `hi`.
    pub fn big(&mut self, lo: u128, hi: u128) -> u128 {
        debug_assert!(lo <= hi, "Rng::big: lo above hi");
        let span = hi.saturating_sub(lo);
        if span == u128::MAX {
            return self.wide(4);
        }
        let range = span + 1;
        let bits = 128 - range.leading_zeros();
        let mask = if bits == 128 { u128::MAX } else { (1u128 << bits) - 1 };
        loop {
            let x = self.wide(bits.div_ceil(32)) & mask;
            if x < range {
                return lo + x;
            }
        }
    }

    fn wide(&mut self, words: u32) -> u128 {
        let mut x = 0u128;
        for _ in 0..words {
            x = (x << 32) | u128::from(self.next_u32());
        }
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stream_is_fixed_by_the_seed() {
        let mut a = Rng::from_text("seed");
        let mut b = Rng::from_text("seed");
        let mut c = Rng::from_text("seed2");
        let first: Vec<u32> = (0..8).map(|_| a.next_u32()).collect();
        assert_eq!(first, (0..8).map(|_| b.next_u32()).collect::<Vec<_>>());
        assert_ne!(first, (0..8).map(|_| c.next_u32()).collect::<Vec<_>>());
    }

    /// The first draws from one seed, as they were when the first exams were made. Any change to this stream
    /// changes every question of every exam, so exams already published would no longer regenerate.
    #[test]
    fn the_stream_is_the_one_the_first_exams_were_drawn_from() {
        let mut rng = Rng::from_text("a");
        let words: Vec<u32> = (0..12).map(|_| rng.next_u32()).collect();
        assert_eq!(
            words,
            [
                2666612042, 2876307811, 3554085701, 3710363984, 2493630513, 2760130167, 2894130497, 888419422,
                766802966, 3896236483, 722514216, 1906060041
            ]
        );
        let ints: Vec<u32> = (0..40).map(|i| rng.int(i, i * 7 + 3)).collect();
        assert_eq!(
            ints,
            [
                3, 1, 4, 17, 18, 7, 37, 17, 51, 12, 21, 62, 44, 29, 20, 31, 50, 57, 79, 73, 68, 115, 65, 115, 136, 123,
                122, 109, 42, 60, 71, 129, 36, 74, 64, 242, 52, 190, 154, 145
            ]
        );
        let bigs: Vec<u128> = (0..10).map(|i| rng.big(1 << (i * 9), (1 << (i * 12 + 5)) + 17)).collect();
        assert_eq!(
            bigs,
            [
                3,
                31819,
                107752276,
                131334770623,
                420949556723601,
                13723843451326843210,
                5358226373603297195549,
                490363045904992130469977048,
                1669409638475629589679214570412,
                8446011463066824670173275527267827
            ]
        );
        assert_eq!(rng.sample(&(0..23).collect::<Vec<u32>>(), 9), [2, 15, 12, 19, 22, 1, 14, 6, 9]);
        assert_eq!((rng.int(0, u32::MAX), rng.int(5, 5)), (63912634, 5));
    }

    #[test]
    fn int_stays_in_range_and_reaches_both_ends() {
        let mut rng = Rng::from_text("range");
        let mut seen = [false; 7];
        for _ in 0..2000 {
            let x = rng.int(3, 9);
            assert!((3..=9).contains(&x));
            seen[(x - 3) as usize] = true;
        }
        assert!(seen.iter().all(|&s| s));
        assert_eq!(rng.int(5, 5), 5);
        // The widest range takes every draw as it comes.
        let mut same = rng.clone();
        assert_eq!(rng.int(0, u32::MAX), same.next_u32());
    }

    #[test]
    fn sample_gives_distinct_items() {
        let mut rng = Rng::from_text("sample");
        let items: Vec<u32> = (0..20).collect();
        for _ in 0..200 {
            let mut got = rng.sample(&items, 7);
            got.sort_unstable();
            got.dedup();
            assert_eq!(got.len(), 7);
        }
    }

    #[test]
    fn big_stays_in_range() {
        let mut rng = Rng::from_text("big");
        let lo = 16u128.pow(17);
        let hi = 16u128.pow(18) - 1;
        for _ in 0..500 {
            let x = rng.big(lo, hi);
            assert!((lo..=hi).contains(&x));
        }
        assert_eq!(rng.big(7, 7), 7);
    }
}
