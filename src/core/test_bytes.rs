//! Deterministic pseudo-random bytes for untrusted-input robustness tests.
//!
//! A fixed-seed generator keeps failures reproducible without pulling in an
//! external fuzzing dependency.

/// Minimal xorshift64* generator.
pub(crate) struct ByteFuzzer {
    state: u64,
}

impl ByteFuzzer {
    /// Creates a generator that always yields the same sequence for `seed`.
    pub(crate) fn new(seed: u64) -> Self {
        // xorshift is degenerate at zero, so force a non-zero state.
        Self { state: seed | 1 }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Returns a fresh buffer of `len` pseudo-random bytes.
    pub(crate) fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next_u64() as u8).collect()
    }

    /// Returns a pseudo-random length in `0..=max`.
    pub(crate) fn len(&mut self, max: usize) -> usize {
        if max == 0 {
            return 0;
        }
        (self.next_u64() % (max as u64 + 1)) as usize
    }
}
