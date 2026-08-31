//! Integration and measurement proofs that compose real repository adapters.
//!
//! MVP outcome: proof floor.

pub use wamn_test_infrastructure::ctl_process;

pub mod causation_e2e;
mod cdc_reader_process;
pub mod cdcbench;
pub mod dashproof;
mod dispatcher_process;
mod hot_route_trace;
pub mod m1;
mod measurement_schema;
pub mod provisionbench;
pub mod readerbench;
#[cfg(test)]
mod receiving_data_access;
mod release_fixture;
pub mod retention;
#[cfg(test)]
mod route_authentication_live;
mod router_tap_live;
pub mod streambench;
pub mod trusted_http_route;
#[cfg(test)]
mod virtualized_std_guest;
pub mod walbench;
