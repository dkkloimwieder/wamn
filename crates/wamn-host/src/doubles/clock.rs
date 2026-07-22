//! The virtual wall clock the test host swaps in for `wasi:clocks/wall-clock`
//! (production delta 2, design-note 9).
//!
//! A [`VirtualClock`] is an `Arc`-shared atomic nanosecond counter a test
//! scheduler drives; [`VirtualWallClock`] adapts it to the fork's
//! [`HostWallClock`] so it can be injected into a store's `WasiCtx` via
//! `WasiCtxBuilder::wall_clock`. Guest code that reads the wall clock (a `delay`
//! node computing its wake deadline, say) then sees the time the scheduler
//! chooses — so a 24h delay collapses to milliseconds of real wall time once the
//! scheduler advances the clock past the deadline.
//!
//! Extracted verbatim from the S6 `testhostbench` (the bench kept a private copy
//! before this crate owned the doubles); the bench now drives THIS type, so the
//! extraction is regression-proved by the unchanged bench modes.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use wasmtime_wasi::HostWallClock;

/// A wall clock a test scheduler drives. Cheap to [`Clone`] (an `Arc` to the
/// shared instant), so the scheduler can advance the very instant a store's
/// `WasiCtx` reads.
#[derive(Clone, Debug)]
pub struct VirtualClock {
    nanos: Arc<AtomicU64>,
}

impl VirtualClock {
    /// A clock reading `secs` seconds since the unix epoch. Tests pick an
    /// arbitrary but fixed base so the guest's `now()` is deterministic.
    pub fn at_secs(secs: u64) -> Self {
        Self {
            nanos: Arc::new(AtomicU64::new(secs.saturating_mul(1_000_000_000))),
        }
    }

    /// Advance the clock by `secs` seconds. Monotonic (time only moves forward).
    pub fn advance_secs(&self, secs: u64) {
        self.nanos
            .fetch_add(secs.saturating_mul(1_000_000_000), Ordering::SeqCst);
    }

    /// Advance the clock TO `target` nanoseconds-since-epoch, if that is in the
    /// future of the current reading. Never moves time backward — a `target`
    /// at or before now is a no-op — so the clock a scheduler drives stays
    /// monotonic even when a stale/earlier deadline is replayed. Returns whether
    /// the clock moved.
    pub fn advance_to_nanos(&self, target: u64) -> bool {
        // A single CAS loop keeps the max monotonic under concurrent readers.
        let mut cur = self.nanos.load(Ordering::SeqCst);
        loop {
            if target <= cur {
                return false;
            }
            match self
                .nanos
                .compare_exchange_weak(cur, target, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return true,
                Err(actual) => cur = actual,
            }
        }
    }

    /// The current reading, nanoseconds since the unix epoch.
    pub fn now_nanos(&self) -> u64 {
        self.nanos.load(Ordering::SeqCst)
    }
}

/// [`HostWallClock`] backed by a shared [`VirtualClock`]. Inject into a store's
/// `WasiCtx` via `WasiCtxBuilder::wall_clock`; the fork reads it for every
/// `wasi:clocks/wall-clock` call the guest makes.
#[derive(Debug)]
pub struct VirtualWallClock(pub VirtualClock);

impl HostWallClock for VirtualWallClock {
    fn resolution(&self) -> Duration {
        Duration::from_nanos(1)
    }
    fn now(&self) -> Duration {
        Duration::from_nanos(self.0.now_nanos())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_secs_is_monotonic_and_additive() {
        let c = VirtualClock::at_secs(1_000);
        assert_eq!(c.now_nanos(), 1_000_000_000_000);
        c.advance_secs(5);
        assert_eq!(c.now_nanos(), 1_005_000_000_000);
        c.advance_secs(0);
        assert_eq!(
            c.now_nanos(),
            1_005_000_000_000,
            "advancing by 0 is a no-op"
        );
    }

    // Load-bearing (delta 2): the scheduler advances the clock TO a deadline, and
    // that jump must be monotonic — a stale/earlier deadline must NOT rewind time,
    // or a re-driven run would read a `now()` before a deadline it already passed
    // and spin. A mutant that lets `advance_to_nanos` move backward fails here.
    #[test]
    fn advance_to_nanos_moves_forward_only() {
        let c = VirtualClock::at_secs(100);
        let base = c.now_nanos();

        // Forward: moves and reports it moved.
        assert!(c.advance_to_nanos(base + 500));
        assert_eq!(c.now_nanos(), base + 500);

        // Equal: no move.
        assert!(!c.advance_to_nanos(base + 500));
        assert_eq!(c.now_nanos(), base + 500);

        // Backward: no move, clock unchanged (monotonic).
        assert!(!c.advance_to_nanos(base));
        assert_eq!(c.now_nanos(), base + 500, "time must never rewind");
    }

    #[test]
    fn cloned_handles_share_one_instant() {
        let a = VirtualClock::at_secs(0);
        let b = a.clone();
        a.advance_secs(7);
        assert_eq!(
            b.now_nanos(),
            7_000_000_000,
            "clones observe the same clock"
        );
    }

    #[test]
    fn wall_clock_reads_the_shared_instant() {
        let c = VirtualClock::at_secs(42);
        let wc = VirtualWallClock(c.clone());
        assert_eq!(wc.now(), Duration::from_secs(42));
        c.advance_secs(8);
        assert_eq!(wc.now(), Duration::from_secs(50));
    }
}
