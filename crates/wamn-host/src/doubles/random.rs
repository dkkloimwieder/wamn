//! The `wasi:random` seed double (production delta 5, design-note 9) and the
//! test-host `WasiCtx` builder.
//!
//! No shipped guest imports `wasi:random` today (only a server-side
//! `gen_random_uuid` comment reference exists), so [`SeededRng`] is a FORWARD
//! HOOK: it makes the test host's randomness reproducible the moment a guest
//! does read it, with zero cost until then.
//!
//! The honest seam: the pinned `wasmtime_wasi::WasiCtxBuilder` exposes
//! `secure_random`, `insecure_random`, and `insecure_random_seed` overrides —
//! all three of which [`build_virtual_wasi`] seeds — so NO fork change is
//! required (the design-note-9 deferral does not apply to this wasmtime rev).
//! `secure_random`/`insecure_random` want an `impl rand::Rng`; `rand_core`
//! blanket-implements `Rng` for any infallible [`TryRng`], so implementing the
//! fallible trait (its documented path) is all [`SeededRng`] needs.

use std::convert::Infallible;

use rand_core::TryRng;
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder};

use super::clock::{VirtualClock, VirtualWallClock};

/// A deterministic splitmix64 generator seeded from a `u64`. The same seed
/// always yields the same stream — the property the test host relies on for a
/// reproducible `wasi:random`.
#[derive(Clone, Debug)]
pub struct SeededRng {
    state: u64,
}

impl SeededRng {
    /// A generator seeded with `seed`. Two `SeededRng`s built from the same seed
    /// produce byte-identical streams.
    pub fn from_seed(seed: u64) -> Self {
        Self { state: seed }
    }

    /// splitmix64 (Vigna): a tiny, well-known deterministic step. Not
    /// cryptographic — determinism, not unpredictability, is the point here.
    fn step(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

impl TryRng for SeededRng {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        // The high bits of splitmix64 are the best-mixed; take them for u32.
        Ok((self.step() >> 32) as u32)
    }

    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        Ok(self.step())
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Infallible> {
        let mut chunks = dst.chunks_exact_mut(8);
        for chunk in &mut chunks {
            chunk.copy_from_slice(&self.step().to_le_bytes());
        }
        let tail = chunks.into_remainder();
        if !tail.is_empty() {
            let bytes = self.step().to_le_bytes();
            tail.copy_from_slice(&bytes[..tail.len()]);
        }
        Ok(())
    }
}

/// The seed folded into the `insecure_random` generator so it does not alias the
/// `secure_random` stream while staying deterministic.
const INSECURE_SEED_FOLD: u64 = 0x5DEE_CE66_D9E3_779B;

/// Build the test host's `WasiCtx`: a virtual wall clock a scheduler drives
/// ([`VirtualClock`]) plus a deterministic RNG on every `wasi:random` surface
/// (secure, insecure, and the insecure seed). `epoch_secs` bases the clock;
/// `seed` seeds the randomness. This is the exact `WasiCtx` the run-worker's
/// `--test-doubles` selector injects.
pub fn build_virtual_wasi(clock: &VirtualClock, seed: u64) -> WasiCtx {
    WasiCtxBuilder::new()
        .args(&["main.wasm"])
        .inherit_stderr()
        .wall_clock(VirtualWallClock(clock.clone()))
        .secure_random(SeededRng::from_seed(seed))
        .insecure_random(SeededRng::from_seed(seed ^ INSECURE_SEED_FOLD))
        .insecure_random_seed(seed as u128)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::Rng as _;

    #[test]
    fn same_seed_is_deterministic_and_reproducible() {
        let mut a = SeededRng::from_seed(0xC0FF_EE);
        let mut b = SeededRng::from_seed(0xC0FF_EE);
        let sa: Vec<u64> = (0..16).map(|_| a.next_u64()).collect();
        let sb: Vec<u64> = (0..16).map(|_| b.next_u64()).collect();
        assert_eq!(sa, sb, "the same seed must reproduce the same stream");
        // Not the degenerate all-zero/constant stream.
        assert!(sa.windows(2).any(|w| w[0] != w[1]));
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = SeededRng::from_seed(1);
        let mut b = SeededRng::from_seed(2);
        let sa: Vec<u64> = (0..8).map(|_| a.next_u64()).collect();
        let sb: Vec<u64> = (0..8).map(|_| b.next_u64()).collect();
        assert_ne!(sa, sb, "distinct seeds must produce distinct streams");
    }

    #[test]
    fn fill_bytes_is_deterministic_across_widths() {
        // A non-multiple-of-8 length exercises the tail path.
        let mut a = SeededRng::from_seed(7);
        let mut b = SeededRng::from_seed(7);
        let mut ba = [0u8; 13];
        let mut bb = [0u8; 13];
        a.fill_bytes(&mut ba);
        b.fill_bytes(&mut bb);
        assert_eq!(ba, bb);
    }
}
