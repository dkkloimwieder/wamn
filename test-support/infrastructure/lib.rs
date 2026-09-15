//! Repository-only temporary services and test adapters.
//!
//! MVP outcome: test coverage.

pub mod ctl_process;
pub mod declarations;
pub mod event_broker;
pub mod executor;
pub mod platform;
pub use wamn_test_postgres as postgres;

pub mod rendering;
pub mod scratch;
pub mod secrets;
pub mod traces;

pub mod workload;
