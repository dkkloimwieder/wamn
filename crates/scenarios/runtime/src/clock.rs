//! The virtual wall clock a scenario swaps in for `wasi:clocks/wall-clock`
//! (production delta 2, design-note 9).
//!
//! A [`ScenarioClock`] is an absolute virtual Unix instant shared by every
//! scheduling decision in one scenario. It is an `Arc`-shared atomic nanosecond
//! counter a scenario scheduler drives; [`VirtualWallClock`] adapts it to the
//! fork's [`HostWallClock`] so it can be injected into a store's `WasiCtx` via
//! `WasiCtxBuilder::wall_clock`. Guest code that reads the wall clock sees the
//! deterministic instant chosen by the scenario scheduler.
//!
//! Extracted from the S6 proof before becoming a product scenario adapter.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use wasmtime_wasi::HostWallClock;

/// The absolute virtual Unix instant governing one scenario's scheduling.
///
/// Retry and deadline comparisons all use the inclusive
/// [`ScenarioClock::is_due`] rule. Cheap to [`Clone`] (an `Arc` to the shared
/// instant), so the scheduler advances the same instant a store's `WasiCtx`
/// reads.
#[derive(Clone, Debug)]
pub struct ScenarioClock {
    nanos: Arc<AtomicU64>,
}

impl ScenarioClock {
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

    /// Whether `deadline_nanos` is due at the current scenario instant.
    ///
    /// Equality is due. This is the single comparison contract used for retry
    /// and deadline decisions.
    pub fn is_due(&self, deadline_nanos: u64) -> bool {
        deadline_nanos <= self.now_nanos()
    }
}

/// The one boundary translating a scenario due decision into PostgreSQL time.
///
/// PostgreSQL keeps its production `timestamptz`/`now()` domain, but its queue
/// timestamp is only an opaque stale token in scenario execution. This boundary
/// captures one database instant and the one shared [`ScenarioClock`]. It
/// releases a row at the captured database instant only after that logical
/// clock declares the virtual schedule due.
#[derive(Clone, Debug)]
pub struct DatabaseClockBoundary {
    clock: ScenarioClock,
    database_origin_nanos: u64,
}

impl DatabaseClockBoundary {
    /// Capture `database_origin_nanos` beside `clock`'s current instant.
    pub fn capture(clock: &ScenarioClock, database_origin_nanos: u64) -> Self {
        Self {
            clock: clock.clone(),
            database_origin_nanos,
        }
    }

    /// The captured database instant used to release a logically-due queue row.
    pub fn database_origin_nanos(&self) -> u64 {
        self.database_origin_nanos
    }

    /// Convert a logically-due scenario schedule into the database marker used
    /// by the production claim path.
    ///
    /// A not-yet-due schedule has no database release instant. A due schedule
    /// maps to the captured database origin, which was read before the case was
    /// enqueued and therefore precedes the later release update and claim.
    pub fn release_nanos(&self, scenario_deadline_nanos: u64) -> Option<u64> {
        self.clock
            .is_due(scenario_deadline_nanos)
            .then_some(self.database_origin_nanos)
    }
}

/// [`HostWallClock`] backed by a shared [`ScenarioClock`]. Inject into a store's
/// `WasiCtx` via `WasiCtxBuilder::wall_clock`; the fork reads it for every
/// `wasi:clocks/wall-clock` call the guest makes.
#[derive(Debug)]
pub struct VirtualWallClock(pub ScenarioClock);

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
        let c = ScenarioClock::at_secs(1_000);
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
        let c = ScenarioClock::at_secs(100);
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
        let a = ScenarioClock::at_secs(0);
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
        let c = ScenarioClock::at_secs(42);
        let wc = VirtualWallClock(c.clone());
        assert_eq!(wc.now(), Duration::from_secs(42));
        c.advance_secs(8);
        assert_eq!(wc.now(), Duration::from_secs(50));
    }

    #[test]
    fn every_schedule_kind_uses_the_same_inclusive_due_boundary() {
        let clock = ScenarioClock::at_secs(10);
        let now = clock.now_nanos();

        for kind in ["retry", "deadline"] {
            assert!(clock.is_due(now - 1), "{kind}: just-before is due");
            assert!(clock.is_due(now), "{kind}: equality is due");
            assert!(!clock.is_due(now + 1), "{kind}: just-after is not due");
        }
    }

    #[test]
    fn database_calendar_date_does_not_change_release_classification() {
        let first = ScenarioClock::at_secs(1_700_000_000);
        let second = ScenarioClock::at_secs(1_700_000_000);
        let day = 86_400 * 1_000_000_000;
        let delay = 3_600 * 1_000_000_000;
        let july = DatabaseClockBoundary::capture(&first, 1_800_000_000_000_000_000);
        let august = DatabaseClockBoundary::capture(&second, 1_800_000_000_000_000_000 + 31 * day);
        let deadline = first.now_nanos() + delay;

        assert_eq!(july.release_nanos(deadline), None);
        assert_eq!(august.release_nanos(deadline), None);
        first.advance_to_nanos(deadline);
        second.advance_to_nanos(deadline);
        assert_eq!(
            july.release_nanos(deadline),
            Some(july.database_origin_nanos())
        );
        assert_eq!(
            august.release_nanos(deadline),
            Some(august.database_origin_nanos())
        );
    }

    #[test]
    fn only_a_logically_due_schedule_crosses_the_database_boundary() {
        let clock = ScenarioClock::at_secs(50);
        let boundary = DatabaseClockBoundary::capture(&clock, 100_000_000_000);
        let now = clock.now_nanos();

        assert_eq!(
            boundary.release_nanos(now - 1),
            Some(boundary.database_origin_nanos())
        );
        assert_eq!(
            boundary.release_nanos(now),
            Some(boundary.database_origin_nanos())
        );
        assert_eq!(boundary.release_nanos(now + 1), None);
    }
}
