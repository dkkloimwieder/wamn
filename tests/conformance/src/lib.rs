//! Contract tests for component policy, WIT behavior, schemas, and artifacts.
//!
//! MVP outcome: test coverage.

pub mod catalog;
pub mod invocation;
pub mod kubernetes_gate_verdict;
pub mod repo_policy;
pub mod socket_test;

#[cfg(test)]
mod ip_name_lookup;

#[cfg(test)]
mod manifest_dependencies;

#[cfg(test)]
mod runtime_policy;

#[cfg(test)]
mod schema_drift;
