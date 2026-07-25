//! Black-box proofs that drive deployed or public repository surfaces.

#[cfg(test)]
use wamn_run_state::schema_drift;
pub use wamn_test_fixtures::{apifixture, f1fixture};

pub mod apiproof;
pub mod credproof;
pub mod f1proof;
pub mod ladderproof;
pub mod traceproof;
