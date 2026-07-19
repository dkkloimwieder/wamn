//! Dispatcher decisions — the adaptive poll cadence ("adaptive intervals … no
//! polling herd", platform-plan 5.14) and the firing record the cron/outbox
//! halves both produce. Pure: the driver owns the clock and the sleep; these are
//! the decisions it folds.
//!
//! The cadence: each project's sweep interval TIGHTENS to `min` the moment a
//! sweep finds work and DECAYS exponentially toward `max` while idle, so a busy
//! project is served at doorbell-class latency while an idle one costs a single
//! cheap scan per `max` — the 30 s–5 min reconciliation band with zero continuous
//! polling. Intervals are per-project state in the driver (no cross-project
//! herd: projects tighten and decay independently).

use crate::model::Millis;

/// Default tightest per-project sweep interval (a busy project's poll cadence).
pub const DEFAULT_MIN_INTERVAL_MS: Millis = 250;
/// Default widest per-project sweep interval (an idle project's reconciliation
/// cadence — the 30 s–5 min band's floor).
pub const DEFAULT_MAX_INTERVAL_MS: Millis = 30_000;

/// The next sweep interval for one project: work tightens to `min`, idleness
/// doubles toward `max`. `min <= max` is the caller's contract (the `clamp`
/// below panics on an inverted range); [`Cadence`] is how a production caller
/// upholds it once, at the config boundary.
pub fn next_interval(current: Millis, found_work: bool, min: Millis, max: Millis) -> Millis {
    if found_work {
        min
    } else {
        current.saturating_mul(2).clamp(min, max)
    }
}

/// The floor both cadence bounds are raised to: a sub-10 ms sweep interval is a
/// busy-loop, not a cadence.
const MIN_INTERVAL_FLOOR_MS: Millis = 10;

/// A validated adaptive-cadence band: the tightest (`min`) and widest (`max`)
/// per-project sweep intervals, with `min <= max` guaranteed and both floored at
/// [`MIN_INTERVAL_FLOOR_MS`]. Built once, at the config boundary, from
/// unvalidated CLI/env millis — so [`next_interval`]'s `clamp`, which panics on
/// an inverted range, can never see `min > max` (M-STRONG-TYPES-GUARD; and
/// M-PANIC-ON-BUG: bad user input is rejected at the boundary, not panicked on
/// downstream during an idle sweep).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cadence {
    min: Millis,
    max: Millis,
}

impl Cadence {
    /// Validate a cadence band from raw (CLI/env) millis: reject an inverted
    /// range, then floor both bounds at [`MIN_INTERVAL_FLOOR_MS`].
    pub fn new(min: Millis, max: Millis) -> Result<Cadence, CadenceError> {
        if min > max {
            return Err(CadenceError::MinExceedsMax { min, max });
        }
        Ok(Cadence {
            min: min.max(MIN_INTERVAL_FLOOR_MS),
            max: max.max(MIN_INTERVAL_FLOOR_MS),
        })
    }

    /// The tightest sweep interval (a busy project's cadence).
    pub fn min(&self) -> Millis {
        self.min
    }

    /// The widest sweep interval (an idle project's reconciliation cadence).
    pub fn max(&self) -> Millis {
        self.max
    }
}

/// A cadence band was rejected at construction. A structured enum (house rule 2,
/// as [`crate::cron::CronError`]) — one variant per failure mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CadenceError {
    /// The tightest interval exceeds the widest — an inverted band. Names both
    /// values: the fix is to correct or swap one of the two flags.
    MinExceedsMax { min: Millis, max: Millis },
}

impl std::fmt::Display for CadenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CadenceError::MinExceedsMax { min, max } => write!(
                f,
                "cadence min-interval-ms ({min}) exceeds max-interval-ms ({max})"
            ),
        }
    }
}

impl std::error::Error for CadenceError {}

/// One trigger firing the dispatcher dispatches: the deterministic run id (the
/// exactly-once handle — a redelivered/re-fired/replica-raced firing collides on
/// it and the write-ahead `ON CONFLICT` absorbs the duplicate), the flow to run,
/// the trigger input the run is replayed from (5.7), and the audit
/// `trigger_source`. Fired via [`crate::write_ahead_triggered_run_sql`] +
/// [`crate::enqueue_sql`] in one transaction, then a doorbell hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Firing {
    pub run_id: String,
    pub flow_id: String,
    pub flow_version: i32,
    pub input_json: String,
    pub trigger_source: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    // R13: an inverted band is user error caught at the boundary, not a
    // downstream `clamp` panic on the first idle sweep. The message must name
    // both flags so the operator knows which one to fix.
    #[test]
    fn cadence_rejects_inverted_band_naming_both_bounds() {
        let err = Cadence::new(5000, 1000).expect_err("min > max must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("5000"), "error names min: {msg}");
        assert!(msg.contains("1000"), "error names max: {msg}");
    }

    #[test]
    fn cadence_accepts_equal_bounds() {
        let c = Cadence::new(250, 250).expect("min == max is a valid (degenerate) band");
        assert_eq!((c.min(), c.max()), (250, 250));
    }

    #[test]
    fn cadence_accepts_normal_band() {
        let c = Cadence::new(DEFAULT_MIN_INTERVAL_MS, DEFAULT_MAX_INTERVAL_MS)
            .expect("min < max is the normal case");
        assert_eq!(
            (c.min(), c.max()),
            (DEFAULT_MIN_INTERVAL_MS, DEFAULT_MAX_INTERVAL_MS)
        );
    }
}
