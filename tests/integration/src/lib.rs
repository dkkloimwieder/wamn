//! Integration and measurement proofs that compose real repository adapters.

#[path = "../../../test-support/fixtures/apifixture.rs"]
mod apifixture;
#[path = "../../../test-support/infrastructure/erp_sim.rs"]
mod erp_sim;
#[path = "../../../test-support/fixtures/f1fixture.rs"]
mod f1fixture;
#[path = "../../../test-support/infrastructure/node_host_support.rs"]
mod node_host_support;
#[path = "../../../test-support/infrastructure/publish_catalog_demo.rs"]
mod publish_catalog_demo;

#[path = "../../system/src/ladderproof.rs"]
mod ladderproof;
#[cfg(test)]
#[path = "../../conformance/src/schema_drift.rs"]
mod schema_drift;
#[path = "../../system/src/traceproof.rs"]
mod traceproof;

mod apibench;
mod bench;
mod capturebench;
mod cdcbench;
mod dashproof;
mod dispatchbench;
mod f1bench;
mod f2invoke;
mod f3proof;
mod f4proof;
mod failoverbench;
mod flowbench;
mod impactproof;
mod logbench;
mod matbench;
mod metricbench;
mod nodebench;
mod nodeinvoke;
mod pgbench;
mod pinproof;
mod pocsuiteproof;
mod provisionbench;
mod queuebench;
mod readerbench;
mod rie2ebench;
mod runnerbench;
mod samplebench;
mod streambench;
mod suiteproof;
mod testhostbench;
mod testkitbench;
mod tracebench;
mod wakeproof;
mod walbench;
