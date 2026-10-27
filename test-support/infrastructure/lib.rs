//! Repository-only temporary services and test adapters.
//!
//! MVP outcome: test coverage.

pub mod ctl_process;
pub mod declarations;
pub mod delivery_cluster;
pub mod event_broker;
pub mod fixture_digest;
pub mod locked_database;
pub mod operations;
pub mod platform;
pub use wamn_test_postgres as postgres;

pub mod record_history;
pub mod release_chart;
pub mod rendering;
pub mod scratch;
pub mod secrets;
pub mod telemetry;

pub mod workload;
