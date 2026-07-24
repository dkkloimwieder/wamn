//! Cron next-fire evaluation — the trigger dispatcher's cron half (5.14). Pure
//! over an injected `now` like everything else in this crate: the schedule math
//! reads no clock, so the 11.1 "fast-forwardable cron" discipline holds — a
//! virtual-time driver (the dispatchbench gate, the test host) passes any
//! `now: Millis` it likes and gets the same answers production gets. Schedules
//! are classic 5-field cron (croner; seconds optional), evaluated in **UTC** —
//! per-project timezones are a later refinement.
//!
//! The exactly-once story: a fire's identity is its **scheduled tick instant**,
//! not the moment a dispatcher observed it. [`due_tick`] canonicalizes the tick
//! (truncated to the second, so replicas observing the same tick at different
//! sub-second offsets agree) and [`mint_cron_run_id`] derives the run id from it
//! (zero-padded, so lexical order == chronological order within one flow's cron
//! ids); the write-ahead `ON CONFLICT` then absorbs a re-fired tick from a
//! restarted or concurrently racing dispatcher. The `runs` table itself is the
//! dispatcher's cron state — [`crate::cron_last_run_sql`] recovers the last
//! fired tick from the flow's OWN cron runs (a flow-exclusive predicate, never
//! a lexical id range — flow ids are unconstrained user text and text ordering
//! is collation-dependent), so there is no dispatcher-local storage to desync.

use std::str::FromStr as _;

use chrono::{DateTime, Utc};
use croner::Cron;

use crate::dispatch::Firing;
use crate::model::Millis;

/// A cron schedule failed to parse or evaluate. A structured enum (SR5, house
/// rule 2) — never a stringly-typed error: one variant per failure mode. All
/// three are pure evaluation faults; there is no I/O variant because `cron.rs`
/// reads no clock and no connection (house rule 1) — the dispatcher's anchor
/// read is the driver's own `tokio_postgres` error, not this type's concern.
/// `Display` preserves the original `cron: …` log strings the dispatcher
/// quarantine (dispatch.rs) records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CronError {
    /// The schedule string did not parse (croner rejected it).
    InvalidExpression { schedule: String, detail: String },
    /// An epoch-millis instant fell outside the representable `DateTime<Utc>`
    /// range (an `i64` far past the calendar horizon).
    OutOfRangeInstant { ms: Millis },
    /// The schedule parsed but has no occurrence croner could find — an
    /// unsatisfiable calendar (a Feb 30) or one that matches nowhere in the
    /// search horizon. Silently returning "nothing due" would make such a flow
    /// never fire with zero diagnostics, so this is an error, not `None`.
    NoOccurrence { schedule: String, detail: String },
}

impl std::fmt::Display for CronError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CronError::InvalidExpression { schedule, detail } => {
                write!(f, "cron: bad schedule {schedule:?}: {detail}")
            }
            CronError::OutOfRangeInstant { ms } => write!(f, "cron: out-of-range instant {ms}"),
            CronError::NoOccurrence { schedule, detail } => {
                write!(f, "cron: no occurrence for {schedule:?}: {detail}")
            }
        }
    }
}

impl std::error::Error for CronError {}

fn parse(schedule: &str) -> Result<Cron, CronError> {
    Cron::from_str(schedule).map_err(|e| CronError::InvalidExpression {
        schedule: schedule.to_string(),
        detail: e.to_string(),
    })
}

fn to_dt(ms: Millis) -> Result<DateTime<Utc>, CronError> {
    DateTime::<Utc>::from_timestamp_millis(ms).ok_or(CronError::OutOfRangeInstant { ms })
}

/// The next scheduled fire **strictly after** `after` (epoch ms, UTC) — the
/// driver's sleep-until hint for an idle cron flow.
pub fn next_fire(schedule: &str, after: Millis) -> Result<Millis, CronError> {
    let cron = parse(schedule)?;
    let next = cron
        .find_next_occurrence(&to_dt(after)?, false)
        .map_err(|e| CronError::NoOccurrence {
            schedule: schedule.to_string(),
            detail: e.to_string(),
        })?;
    Ok(next.timestamp_millis())
}

/// The dispatcher's due decision: the most recent scheduled tick in
/// `(anchor, now]`, canonicalized to the second, or `None` if no tick has passed
/// since `anchor` (the last fired tick, or the flow's first-sight instant).
///
/// This is the **misfire collapse** policy: if several ticks passed since
/// `anchor` (dispatcher down over a nightly cron, say), only the LATEST fires —
/// one run per tick instant, older missed ticks are skipped, never replayed in a
/// burst. The truncation makes the tick replica-independent: any dispatcher
/// observing within the tick's second computes the same instant, so all of them
/// mint the same run id.
pub fn due_tick(schedule: &str, anchor: Millis, now: Millis) -> Result<Option<Millis>, CronError> {
    if now <= anchor {
        return Ok(None);
    }
    let cron = parse(schedule)?;
    // Inclusive: `now` landing exactly on (or within the second of) a tick is due.
    // A search failure is an ERROR, not "nothing due": it means the schedule is
    // parseable but unsatisfiable (a Feb 30) or matches nowhere in croner's
    // horizon — silently returning None would make such a flow never fire with
    // zero diagnostics (and re-walk the full search horizon every sweep; the
    // driver quarantines an erroring schedule instead).
    let prev = cron
        .find_previous_occurrence(&to_dt(now)?, true)
        .map_err(|e| CronError::NoOccurrence {
            schedule: schedule.to_string(),
            detail: e.to_string(),
        })?;
    // Canonicalize: a self-match returns the observed instant sub-second included;
    // the tick's identity is the scheduled second.
    let tick = prev.timestamp_millis().div_euclid(1_000) * 1_000;
    Ok((tick > anchor).then_some(tick))
}

/// Deterministic run id for a cron firing: one run per (flow, tick instant),
/// `{flow_id}:cron:{tick:013}`. Zero-padding makes lexical order chronological
/// WITHIN one flow's cron ids (equal-length digit suffixes order the same under
/// any collation), which is what lets [`crate::cron_last_run_sql`] recover the
/// last fired tick as `max(run_id)` over the flow's own cron runs — and two
/// replicas racing the same tick collide on the same id (the write-ahead
/// `ON CONFLICT` absorbs the loser).
pub fn mint_cron_run_id(flow_id: &str, tick: Millis) -> String {
    format!("{flow_id}:cron:{tick:013}")
}

/// Recover the tick instant from one of `flow_id`'s own [`mint_cron_run_id`]
/// ids (the last-fired anchor read back from `runs`). EXACT-prefix parse:
/// `None` for anything that is not precisely `{flow_id}:cron:` + 13 digits —
/// flow ids are unconstrained user text (one may literally contain `:cron:`),
/// so a suffix-based parse could read a FOREIGN flow's tick as this flow's
/// anchor and silently skip its due tick.
pub fn cron_tick_of(flow_id: &str, run_id: &str) -> Option<Millis> {
    let tick = run_id.strip_prefix(flow_id)?.strip_prefix(":cron:")?;
    if tick.len() != 13 || !tick.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    tick.parse().ok()
}

/// Assemble a cron firing: the deterministic run id, the trigger input the run
/// is replayed from (5.7: `input_json` is what a replay re-runs), and the audit
/// `trigger_source`.
pub fn cron_firing(flow_id: &str, flow_version: i32, schedule: &str, tick: Millis) -> Firing {
    let input = serde_json::json!({
        "trigger": "cron",
        "schedule": schedule,
        "fire-at-ms": tick,
    });
    Firing {
        run_id: mint_cron_run_id(flow_id, tick),
        flow_id: flow_id.to_string(),
        flow_version,
        input_json: input.to_string(),
        trigger_source: "cron".to_string(),
    }
}
