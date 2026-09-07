//! Contract proofs for component policy, WIT behavior, schemas, and artifacts.
//!
//! MVP outcome: proof floor.

pub mod catalog;
pub mod invocation;
pub mod kubernetes_gate_verdict;
pub mod package_inventory;
pub mod socketguard;

#[cfg(test)]
mod docker_component_provenance;

#[cfg(test)]
mod ip_name_lookup;

#[cfg(test)]
mod manifest_dependencies;

#[cfg(test)]
mod runtime_inventory;

#[cfg(test)]
mod schema_drift;

#[cfg(test)]
mod version_identity;
