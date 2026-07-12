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
/// doubles toward `max`.
pub fn next_interval(current: Millis, found_work: bool, min: Millis, max: Millis) -> Millis {
    if found_work {
        min
    } else {
        current.saturating_mul(2).clamp(min, max)
    }
}

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
