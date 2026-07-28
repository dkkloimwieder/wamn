//! Pure trigger scheduling decisions over an injected clock.
//!
//! This crate decides when a trigger is due and constructs its deterministic
//! firing envelope. It owns no clock, queue, lease, database, or durable anchor;
//! adapters persist firings through `wamn-run-state`.

mod cron;
mod dispatch;
mod reconcile;

/// Unix epoch milliseconds supplied by an adapter.
pub type Millis = i64;

pub use cron::{
    CronError, canonical_tick, cron_firing, cron_tick_of, due_tick, mint_cron_run_id,
    mint_cron_run_id_for_generation, next_fire,
};
pub use dispatch::{
    Cadence, CadenceError, DEFAULT_MAX_INTERVAL_MS, DEFAULT_MIN_INTERVAL_MS, Firing,
};
pub use reconcile::{next_reconcile, reconcile_due};
