//! Integration and measurement proofs that compose real repository adapters.

pub use wamn_test_infrastructure::{ctl_process, node_host_support};

#[cfg(test)]
use wamn_run_state::schema_drift;

pub mod capturebench;
pub mod catalog_live;
mod catalog_pin;
pub mod causation_e2e;
mod cdc_reader_process;
pub mod cdcbench;
pub mod contextproof;
pub mod credproof;
pub mod dashproof;
mod dispatcher_process;
pub mod exposure_live;
pub mod failoverbench;
pub mod flowbench;
mod flowrunner_linker;
pub mod impactproof;
pub mod invocationproof;
pub mod matbench;
pub mod materializer;
pub mod metricbench;
pub mod never_replay;
pub mod nodebench;
pub mod nodeinvoke;
pub mod pinproof;
pub mod provisionbench;
pub mod queuebench;
pub mod readerbench;
pub mod rie2ebench;
pub mod runnerbench;
pub mod samplebench;
pub mod streambench;
pub mod suiteproof;
pub mod testhostbench;
pub mod testkitbench;
pub mod wakeproof;
pub mod walbench;
