//! Adaptive poll cadence shared by the dispatcher and executor services. Pure:
//! each driver owns its clock and sleep and folds these interval decisions.
//!
//! The cadence: each project's sweep interval TIGHTENS to `min` the moment a
//! sweep finds work and DECAYS exponentially toward `max` while idle, so a busy
//! project is served at doorbell-class latency while an idle one costs a single
//! cheap scan per `max` — the 30 s–5 min reconciliation band with zero continuous
//! polling. Intervals are per-project state in the driver (no cross-project
//! herd: projects tighten and decay independently).

/// Default tightest per-project sweep interval (a busy project's poll cadence).
pub const DEFAULT_MIN_INTERVAL_MS: i64 = 250;
/// Default widest per-project sweep interval (an idle project's reconciliation
/// cadence — the 30 s–5 min band's floor).
pub const DEFAULT_MAX_INTERVAL_MS: i64 = 30_000;

/// The floor both cadence bounds are raised to: a sub-10 ms sweep interval is a
/// busy-loop, not a cadence.
const MIN_INTERVAL_FLOOR_MS: i64 = 10;

/// A validated adaptive-cadence band: the tightest (`min`) and widest (`max`)
/// per-project sweep intervals, with `min <= max` guaranteed and both floored at
/// `MIN_INTERVAL_FLOOR_MS`. Built once, at the config boundary, from
/// unvalidated CLI/env millis — so [`Cadence::next_interval`]'s `clamp` (which
/// would panic on an inverted range) can never see `min > max`: the band is the
/// method's own receiver, so an inverted range is unrepresentable
/// (M-STRONG-TYPES-GUARD; and M-PANIC-ON-BUG: bad user input is rejected at the
/// boundary, not panicked on downstream during an idle sweep).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cadence {
    min: i64,
    max: i64,
}

impl Cadence {
    /// Validate a cadence band from raw (CLI/env) millis: reject an inverted
    /// range, then floor both bounds at `MIN_INTERVAL_FLOOR_MS`.
    pub fn new(min: i64, max: i64) -> Result<Cadence, CadenceError> {
        if min > max {
            return Err(CadenceError::MinExceedsMax { min, max });
        }
        Ok(Cadence {
            min: min.max(MIN_INTERVAL_FLOOR_MS),
            max: max.max(MIN_INTERVAL_FLOOR_MS),
        })
    }

    /// The tightest sweep interval (a busy project's cadence).
    pub fn min(&self) -> i64 {
        self.min
    }

    /// The widest sweep interval (an idle project's reconciliation cadence).
    pub fn max(&self) -> i64 {
        self.max
    }

    /// The next sweep interval for one project: work tightens to `min`, idleness
    /// doubles `current` toward `max`. `min <= max` holds by construction (it is
    /// this band's own invariant), so the `clamp` can never see an inverted range.
    pub fn next_interval(&self, current: i64, found_work: bool) -> i64 {
        if found_work {
            self.min
        } else {
            current.saturating_mul(2).clamp(self.min, self.max)
        }
    }
}

/// A cadence band was rejected at construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CadenceError {
    /// The tightest interval exceeds the widest — an inverted band. Names both
    /// values: the fix is to correct or swap one of the two flags.
    MinExceedsMax { min: i64, max: i64 },
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

    // R13-hardening: `next_interval` as an inherent method preserves the exact
    // progression the free fn had — work snaps to `min` from anywhere, idleness
    // doubles and caps at `max`, a degenerate `current` clamps up into the band.
    #[test]
    fn next_interval_progression_via_inherent_method() {
        let c = Cadence::new(DEFAULT_MIN_INTERVAL_MS, DEFAULT_MAX_INTERVAL_MS).unwrap();
        let (min, max) = (c.min(), c.max());
        // Work snaps the cadence to the tight bound, from anywhere.
        assert_eq!(c.next_interval(max, true), min);
        assert_eq!(c.next_interval(min, true), min);
        // Idleness decays exponentially and caps at max (the reconciliation band).
        assert_eq!(c.next_interval(min, false), 2 * min);
        assert_eq!(c.next_interval(2 * min, false), 4 * min);
        assert_eq!(c.next_interval(20_000, false), max); // 40k clamps to 30k
        assert_eq!(c.next_interval(max, false), max);
        // A degenerate current clamps up into the band.
        assert_eq!(c.next_interval(0, false), min);
    }
}
