//! Contract proofs for component policy, WIT behavior, schemas, and artifacts.

pub mod buildproof;
pub mod catalog;
pub mod credprobe;
pub mod egressbench;
pub mod flow;
pub mod invocation;
pub mod raw_sql;
pub mod socketguard;
pub mod testgate;

#[cfg(test)]
mod docker_component_provenance;

#[cfg(test)]
mod manifest_dependencies;

#[cfg(test)]
mod runtime_inventory;

#[cfg(test)]
mod schema_drift;
