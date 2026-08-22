//! Integration and measurement proofs that compose real repository adapters.
//!
//! MVP outcome: proof floor.

pub use wamn_test_infrastructure::ctl_process;

#[cfg(test)]
use wamn_run_state::schema_drift;

pub mod capturebench;
pub mod catalog_live;
mod catalog_pin;
pub mod causation_e2e;
mod cdc_reader_process;
pub mod cdcbench;
pub mod dashproof;
mod dispatcher_process;
pub mod exposure_live;
pub mod m1;
pub mod matbench;
pub mod materializer;
pub mod provisionbench;
pub mod readerbench;
mod release_fixture;
#[cfg(test)]
mod rie2ebench;
pub mod streambench;
pub mod walbench;
