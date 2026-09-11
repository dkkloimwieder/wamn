//! Integration and measurement proofs that compose real repository adapters.
//!
//! MVP outcome: proof floor.

pub use wamn_test_infrastructure::ctl_process;

#[cfg(test)]
#[path = "../../../apps/client_acme_receiving/tests/acme_overlay_publication.rs"]
mod acme_overlay_publication;
mod cdc_reader_process;
pub mod cdcbench;
#[cfg(test)]
mod claim_law_live;
pub mod dashproof;
pub mod host_session_proof;
mod hot_route_trace;
pub mod identity_keys_proof;
pub mod identity_session_proof;
mod measurement_schema;
pub mod membershipproof;
pub mod provisionbench;
pub mod readerbench;
pub mod rc;
pub mod retention;
#[cfg(test)]
mod route_authentication_live;
mod router_tap_live;
pub mod streambench;
pub mod throughput_bench;
mod throughput_bench_live;
pub mod trusted_http_route;
#[cfg(test)]
mod virtualized_std_guest;
pub mod walbench;
