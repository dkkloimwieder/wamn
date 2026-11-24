//! Retry policy from a call node's reserved configuration.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value;

pub(crate) struct RetryPolicy {
    pub(crate) attempts: u32,
    base_ms: u64,
    factor: f64,
    cap_ms: u64,
}

impl RetryPolicy {
    pub(crate) fn from_config(params: &BTreeMap<String, Value>) -> Self {
        const DEFAULT: RetryPolicy = RetryPolicy {
            attempts: 3,
            base_ms: 100,
            factor: 2.0,
            cap_ms: 30_000,
        };
        // Cap author-supplied backoff at one hour, as in the old router.
        const CAP_MS_CEILING: u64 = 3_600_000;

        let Some(retry) = params.get("retry").and_then(Value::as_object) else {
            return DEFAULT;
        };
        Self {
            attempts: retry
                .get("max-attempts")
                .and_then(Value::as_u64)
                .map_or(DEFAULT.attempts, |value| {
                    u32::try_from(value.max(1)).unwrap_or(u32::MAX)
                }),
            base_ms: retry
                .get("base-ms")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT.base_ms),
            factor: retry
                .get("factor")
                .and_then(Value::as_f64)
                .filter(|factor| *factor >= 1.0)
                .unwrap_or(DEFAULT.factor),
            cap_ms: retry
                .get("cap-ms")
                .and_then(Value::as_u64)
                .map_or(DEFAULT.cap_ms, |value| value.min(CAP_MS_CEILING)),
        }
    }

    pub(crate) fn backoff_ms(&self, attempt: u32) -> u64 {
        if self.base_ms == 0 {
            return 0;
        }
        let base = Duration::from_millis(self.base_ms).as_secs_f64();
        let cap = Duration::from_millis(self.cap_ms);
        let exponent = i32::try_from(attempt).unwrap_or(i32::MAX);
        let scaled = Duration::try_from_secs_f64(base * self.factor.powi(exponent)).unwrap_or(cap);
        u64::try_from(scaled.min(cap).as_millis()).unwrap_or(self.cap_ms)
    }
}
