//! Adaptive polling cadence for the dispatcher.
//!
//! MVP outcome: wake-from-zero.

mod dispatch;

pub use dispatch::{Cadence, CadenceError, DEFAULT_MAX_INTERVAL_MS, DEFAULT_MIN_INTERVAL_MS};
