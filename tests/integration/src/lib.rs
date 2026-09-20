//! Integration and measurement tests that compose real repository adapters.
//!
//! MVP outcome: test coverage.

pub use wamn_test_infrastructure::ctl_process;

pub mod agent_pilot;
mod cdc_reader_process;
pub mod cdcbench;
#[cfg(test)]
mod claim_law_live;
pub mod dashboard_test;
pub mod host_session_test;
mod hot_route_trace;
pub mod identity_keys_test;
pub mod identity_session_test;
pub mod local_application;
pub mod measure;
mod measurement_schema;
pub mod membership_test;
pub mod operator_recovery;
pub mod p3_shell;
pub mod rc;
pub mod readerbench;
#[cfg(test)]
mod reconcile_live;
#[cfg(test)]
mod route_authentication_live;
mod router_tap_live;
pub mod startup_burst;
pub mod streambench;
pub mod throughput_bench;
pub mod trusted_http_route;
#[cfg(test)]
mod virtualized_std_guest;
pub mod walbench;
