//! Black-box proofs that drive deployed or public repository surfaces.

#[cfg(test)]
use wamn_run_state::schema_drift;
pub use wamn_test_fixtures::{apifixture, f1fixture};

pub mod apiproof;
pub mod callable_f0;
pub mod callable_f1;
pub mod callable_f2;
pub mod callable_f3;
pub mod callable_wave1;
pub mod childproof;
pub mod credproof;
pub mod deadlineproof;
pub mod f1proof;
pub mod invocationproof;
pub mod ladderproof;
pub mod pocsuiteproof;
pub mod traceproof;
