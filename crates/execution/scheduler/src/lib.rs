//! Adaptive polling cadence shared by execution services.
//!
//! MVP outcome: wake-from-zero.

mod dispatch;

pub use dispatch::{Cadence, CadenceError, DEFAULT_MAX_INTERVAL_MS, DEFAULT_MIN_INTERVAL_MS};
