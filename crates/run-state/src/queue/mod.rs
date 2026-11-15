//! SQL for the durable run queue.

mod sql;
/// The workflow contract's statements: admit, park, release, and list.
mod workflow;

/// Maximum delay after a recoverable lease failure, in milliseconds.
pub const MAX_RETRY_BACKOFF_MS: i64 = 300_000;
pub use sql::{
    advance_lease_attempts_sql, clear_pre_effect_state_sql, grant_production_lease_sql,
    release_production_lease_sql, renew_production_lease_sql, select_exhausted_production_sql,
    select_production_lease_sql, terminalize_exhausted_production_sql,
    terminalize_released_production_sql,
};
pub use workflow::{
    insert_run_queue_sql, insert_run_sql, list_workflow_runs_sql, park_queued_run_sql,
    release_parked_run_sql, select_run_queue_state_sql, select_run_sql,
};
