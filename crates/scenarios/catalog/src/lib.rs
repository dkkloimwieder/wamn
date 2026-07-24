//! Persistence and pin-from-run transforms for product scenarios.
//!
//! Scenario shapes and evaluation live in `wamn-scenario-model`. This crate
//! owns the `test_suites` / `test_cases` SQL contract and the application
//! transform from durable run records into a replayable scenario case.

pub mod compat;
pub mod pin;
pub mod sql;

pub use pin::{PinError, PinOptions, pin_run};
