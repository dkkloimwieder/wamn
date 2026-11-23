//! Run storage contracts for admission, leases, settlement and pruning.

pub mod admission;
pub mod persistence;
pub mod run_store;
mod status;
pub mod transitions;
pub use status::{FailType, RunStatus};
