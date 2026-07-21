//! Per-node retry policy + backoff. The engine owns the backoff *curve*; the
//! `wamn:node` contract owns only the *classification* (retryable / rate-limited /
//! terminal / …) and the optional source-authoritative `retry-after` delay.
//!
//! The policy is read from a reserved `"retry"` object inside the node's opaque
//! `config` (config is typed per node-type by the node library 5.3, but `retry`
//! is a runner-reserved key). Absent → [`RetryPolicy::DEFAULT`].

use serde_json::Value;

/// A node's retry policy. Backoff is **deterministic** exponential — no jitter —
/// so the engine stays a pure function (a driver may add jitter around the
/// returned delay if it wants to de-correlate a thundering herd).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetryPolicy {
    /// Total attempts including the first (so `1` = no retry). `0` is treated as
    /// `1`.
    pub max_attempts: u32,
    /// Base delay before the first retry, in milliseconds.
    pub base_ms: u64,
    /// Multiplier applied per subsequent retry.
    pub factor: f64,
    /// Upper bound on any single backoff delay, in milliseconds.
    pub cap_ms: u64,
}

impl RetryPolicy {
    /// The default policy applied to a node with no `retry` config: 3 attempts,
    /// 100 ms base, doubling, capped at 30 s.
    pub const DEFAULT: RetryPolicy = RetryPolicy {
        max_attempts: 3,
        base_ms: 100,
        factor: 2.0,
        cap_ms: 30_000,
    };

    /// Platform ceiling on the *author-supplied* backoff cap (`cap-ms`),
    /// 3_600_000 ms (1 h). A single retry backoff must not out-park the janitor's
    /// reap grace — the longest timescale the run lifecycle acknowledges before a
    /// stuck non-terminal run is reaped as `infrastructure-failure` (the 1 h
    /// grace the janitor sweep is invoked with, run-queue `janitor_sweep_sql`).
    /// A retry is error *recovery*, not an intentional `delay`-node wait (which
    /// parks unboundedly by design); an author cap past this horizon turns the
    /// exponential backoff into an effectively-infinite park, indistinguishable
    /// from a wedged run, so it is clamped here at parse time. The bound applies
    /// only to author input — the default (30 s) is well under it and unchanged.
    const CAP_MS_CEILING: u64 = 3_600_000;

    /// Read the policy from a node's opaque `config`, honoring a reserved
    /// `"retry"` object (`max-attempts` / `base-ms` / `factor` / `cap-ms`, all
    /// optional). A missing object, a non-object config, or a `Null` config all
    /// yield [`RetryPolicy::DEFAULT`]; individual missing keys fall back per field.
    pub fn from_config(config: &Value) -> RetryPolicy {
        let Some(retry) = config.get("retry").filter(|v| v.is_object()) else {
            return RetryPolicy::DEFAULT;
        };
        let d = RetryPolicy::DEFAULT;
        RetryPolicy {
            max_attempts: retry
                .get("max-attempts")
                .and_then(Value::as_u64)
                .map(|n| n.max(1) as u32)
                .unwrap_or(d.max_attempts),
            base_ms: retry
                .get("base-ms")
                .and_then(Value::as_u64)
                .unwrap_or(d.base_ms),
            factor: retry
                .get("factor")
                .and_then(Value::as_f64)
                .filter(|f| *f >= 1.0)
                .unwrap_or(d.factor),
            cap_ms: retry
                .get("cap-ms")
                .and_then(Value::as_u64)
                .map(|n| n.min(Self::CAP_MS_CEILING))
                .unwrap_or(d.cap_ms),
        }
    }

    /// Whether an `attempt`-th execution (0-based) may be retried — i.e. a further
    /// attempt is within budget.
    pub fn may_retry(&self, attempt: u32) -> bool {
        attempt + 1 < self.max_attempts.max(1)
    }

    /// Backoff delay (ms) to wait *before* the retry that follows a failed
    /// `attempt` (0-based): `min(cap, base * factor^attempt)`.
    pub fn backoff_ms(&self, attempt: u32) -> u64 {
        if self.base_ms == 0 {
            return 0;
        }
        let scaled = (self.base_ms as f64) * self.factor.powi(attempt as i32);
        // Saturate into u64 before the cap; a huge factor^attempt must not wrap.
        let scaled = if scaled >= self.cap_ms as f64 {
            self.cap_ms
        } else {
            scaled as u64
        };
        scaled.min(self.cap_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // R20: an author cap-ms past the platform ceiling (the janitor reap grace) is
    // clamped at parse time — a retry must not out-park the run lifecycle's own
    // horizon. A day-long cap lands on the 1 h ceiling.
    #[test]
    fn author_cap_ms_over_ceiling_is_clamped() {
        let cfg = json!({ "retry": { "cap-ms": 86_400_000u64 } });
        assert_eq!(
            RetryPolicy::from_config(&cfg).cap_ms,
            RetryPolicy::CAP_MS_CEILING
        );
    }

    // An in-range author cap-ms passes through untouched.
    #[test]
    fn author_cap_ms_in_range_passes_through() {
        let cfg = json!({ "retry": { "cap-ms": 60_000u64 } });
        assert_eq!(RetryPolicy::from_config(&cfg).cap_ms, 60_000);
    }

    // The default policy (no `retry` object) is unchanged: cap stays at 30 s,
    // well under the ceiling.
    #[test]
    fn default_cap_ms_unchanged() {
        assert_eq!(RetryPolicy::from_config(&json!({})), RetryPolicy::DEFAULT);
        assert_eq!(RetryPolicy::DEFAULT.cap_ms, 30_000);
    }
}
