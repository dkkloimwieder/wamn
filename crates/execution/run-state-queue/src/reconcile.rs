//! Reconciliation cadence — the slow safety-net sweep that backstops lost NATS
//! doorbell hints. Doorbells are fire-and-forget over NATS-core (the least
//! durable link by design, D3/D15): if a hint is dropped, a run would sit unclaimed.
//! A periodic reconciliation claim (every 30 s–5 min) guarantees eventual pickup
//! with zero continuous polling. These are the pure timing decisions.

use crate::model::Millis;

/// Whether a reconciliation sweep is due at `now` (at least `interval` has elapsed
/// since `last_sweep`).
pub fn reconcile_due(now: Millis, last_sweep: Millis, interval: Millis) -> bool {
    now - last_sweep >= interval
}

/// When the next reconciliation sweep should run.
pub fn next_reconcile(last_sweep: Millis, interval: Millis) -> Millis {
    last_sweep + interval
}
