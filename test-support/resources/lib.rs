//! Repository-only temporary services and test adapters.
//!
//! MVP outcome: test coverage.

pub mod collector;
pub mod event_broker;
pub mod fixture_digest;
pub mod nats_tls;
pub mod operations;
pub mod process_input;
pub use wamn_test_postgres as postgres;

pub mod record_history;
pub mod scratch;

pub mod credentials;
