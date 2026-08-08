//! Integration and measurement proofs that compose real repository adapters.

pub use wamn_test_fixtures::{apifixture, f1fixture};
pub use wamn_test_infrastructure::{ctl_process, erp_sim, node_host_support, publish_catalog_demo};

#[cfg(test)]
use wamn_run_state::schema_drift;

pub mod apibench;
pub mod bench;
pub mod callable_cron;
pub mod capturebench;
pub mod catalog_live;
pub mod causation_e2e;
mod cdc_reader_process;
pub mod cdcbench;
pub mod contextproof;
pub mod credproof;
pub mod dashproof;
pub mod dispatchbench;
mod dispatcher_process;
pub mod exposure_live;
pub mod f1bench;
pub mod f2invoke;
pub mod f3proof;
pub mod f4fixture;
pub mod f4proof;
pub mod failoverbench;
pub mod flowbench;
mod flowrunner_linker;
pub mod impactproof;
pub mod invocationproof;
pub mod logbench;
pub mod matbench;
pub mod materializer;
pub mod metricbench;
pub mod never_replay;
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
pub mod runstate_baseline;
pub mod samplebench;
pub mod streambench;
pub mod suiteproof;
pub mod testhostbench;
pub mod testkitbench;
pub mod tracebench;
pub mod wakeproof;
pub mod walbench;
