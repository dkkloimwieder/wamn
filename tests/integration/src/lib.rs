//! Integration and measurement proofs that compose real repository adapters.

pub use wamn_test_infrastructure::ctl_process;

#[cfg(test)]
use wamn_run_state::schema_drift;

pub mod capturebench;
pub mod catalog_live;
mod catalog_pin;
pub mod causation_e2e;
mod cdc_reader_process;
pub mod cdcbench;
pub mod contextproof;
pub mod dashproof;
mod dispatcher_process;
pub mod exposure_live;
pub mod flowbench;
mod flowrunner_linker;
pub mod impactproof;
pub mod matbench;
pub mod materializer;
pub mod metricbench;
pub mod provisionbench;
pub mod readerbench;
pub mod rie2ebench;
pub mod runnerbench;
pub mod streambench;
pub mod walbench;
