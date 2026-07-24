//! The shared rate-limit throttle + per-flow concurrency accounting.
//!
//! These are the pure, testable data structures behind two 5.2 clauses that are
//! *cross-run* (they coordinate multiple executions), so they live beside the
//! per-run reducer rather than inside it. The component's async driver consults
//! them; they never touch the clock themselves (time is a `now_ms` argument).
//!
//! - [`ThrottleKey`] + [`ThrottleTable`]: when a node returns `rate-limited`, all
//!   parallel executions against the *same* limited system — keyed by (node type,
//!   credential, target host) — back off **together**, while unrelated flows
//!   proceed. Deliberately **not** global queue backpressure (that is 5.14): one
//!   throttled upstream must not stall the platform.
//! - [`Scheduler`]: the per-flow in-flight cap + claim-side backpressure — the
//!   runner stops admitting a flow's runs past its concurrency limit.

use std::collections::HashMap;

/// The identity a shared throttle gate is keyed on: `(node type, credential
/// handle, target host)`. Two executions that would hammer the same upstream
/// share a gate; everything else is independent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ThrottleKey {
    pub node_type: String,
    pub credential: Option<String>,
    pub host: Option<String>,
}

impl ThrottleKey {
    pub fn new(
        node_type: impl Into<String>,
        credential: Option<String>,
        host: Option<String>,
    ) -> ThrottleKey {
        ThrottleKey {
            node_type: node_type.into(),
            credential,
            host,
        }
    }
}

/// A table of active throttle gates: each key is closed until a monotonic
/// millisecond deadline. Closing is idempotent-max (a later deadline wins, an
/// earlier one never shortens an active gate).
#[derive(Debug, Clone, Default)]
pub struct ThrottleTable {
    gated_until: HashMap<ThrottleKey, u64>,
}

impl ThrottleTable {
    pub fn new() -> ThrottleTable {
        ThrottleTable::default()
    }

    /// Close the gate for `key` until at least `until_ms` (never shortens an
    /// existing, later gate).
    pub fn gate(&mut self, key: ThrottleKey, until_ms: u64) {
        let e = self.gated_until.entry(key).or_insert(0);
        *e = (*e).max(until_ms);
    }

    /// Whether `key` is open (no gate, or its gate has elapsed) at `now_ms`.
    pub fn ready(&self, key: &ThrottleKey, now_ms: u64) -> bool {
        self.gated_until
            .get(key)
            .is_none_or(|&until| now_ms >= until)
    }

    /// The deadline `key` is gated until, if it is currently closed at `now_ms`.
    pub fn gated_until(&self, key: &ThrottleKey, now_ms: u64) -> Option<u64> {
        self.gated_until
            .get(key)
            .copied()
            .filter(|&until| now_ms < until)
    }

    /// Drop gates that have elapsed at `now_ms` (housekeeping; not required for
    /// correctness since [`ready`](Self::ready) already treats elapsed as open).
    pub fn sweep(&mut self, now_ms: u64) {
        self.gated_until.retain(|_, &mut until| now_ms < until);
    }
}

/// Per-flow in-flight accounting + the shared throttle: the runner's claim-side
/// backpressure. `try_admit` refuses a flow's run once it is at its concurrency
/// limit (the driver then leaves it on the queue — the queue itself is 5.14).
#[derive(Debug, Clone)]
pub struct Scheduler {
    per_flow_limit: usize,
    in_flight: HashMap<String, usize>,
    pub throttle: ThrottleTable,
}

impl Scheduler {
    /// A scheduler with a per-flow concurrency cap (`0` = unlimited).
    pub fn new(per_flow_limit: usize) -> Scheduler {
        Scheduler {
            per_flow_limit,
            in_flight: HashMap::new(),
            throttle: ThrottleTable::new(),
        }
    }

    /// Try to admit one run of `flow_id`. Returns `true` and records it in-flight
    /// if under the cap; `false` (backpressure) if the flow is at its limit.
    pub fn try_admit(&mut self, flow_id: &str) -> bool {
        let n = self.in_flight.get(flow_id).copied().unwrap_or(0);
        if self.per_flow_limit != 0 && n >= self.per_flow_limit {
            return false;
        }
        *self.in_flight.entry(flow_id.to_string()).or_insert(0) += 1;
        true
    }

    /// Mark one run of `flow_id` finished, freeing a slot.
    pub fn finish(&mut self, flow_id: &str) {
        if let Some(n) = self.in_flight.get_mut(flow_id) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.in_flight.remove(flow_id);
            }
        }
    }

    /// Current in-flight count for a flow.
    pub fn in_flight(&self, flow_id: &str) -> usize {
        self.in_flight.get(flow_id).copied().unwrap_or(0)
    }
}
