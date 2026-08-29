//! Integration and measurement proofs that compose real repository adapters.
//!
//! MVP outcome: proof floor.

pub use wamn_test_infrastructure::ctl_process;

pub mod catalog_live;
pub mod causation_e2e;
mod cdc_reader_process;
pub mod cdcbench;
pub mod dashproof;
mod dispatcher_process;
mod hot_route_trace;
pub mod m1;
pub mod provisionbench;
pub mod readerbench;
mod release_fixture;
mod router_tap_live;
pub mod retention;
#[cfg(test)]
mod rie2ebench;
pub mod streambench;
pub mod trusted_http_route;
pub mod walbench;
