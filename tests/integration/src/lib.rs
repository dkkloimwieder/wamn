//! Integration and measurement proofs that compose real repository adapters.

pub use wamn_test_fixtures::{apifixture, f1fixture};
pub use wamn_test_infrastructure::{ctl_process, erp_sim, node_host_support, publish_catalog_demo};

#[cfg(test)]
use wamn_run_state::schema_drift;

pub mod apibench;
pub mod bench;
pub mod capturebench;
mod cdc_reader_process;
pub mod cdcbench;
pub mod dashproof;
pub mod dispatchbench;
mod dispatcher_process;
pub mod f1bench;
pub mod f2invoke;
pub mod f3proof;
pub mod f4proof;
pub mod failoverbench;
pub mod flowbench;
pub mod impactproof;
pub mod logbench;
pub mod matbench;
pub mod metricbench;
pub mod nodebench;
pub mod nodeinvoke;
pub mod pgbench;
pub mod pinproof;
pub mod pocsuiteproof;
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
pub mod tracebench;
pub mod wakeproof;
pub mod walbench;
