//! Integration and measurement tests that compose real repository adapters.
//!
//! MVP outcome: test coverage.

pub use wamn_test_infrastructure::ctl_process;

#[cfg(test)]
mod control_client_live;
mod hot_route_trace;
pub mod local_application;
#[cfg(test)]
mod route_authentication_live;
#[cfg(test)]
mod route_interface_live;
pub mod trusted_http_route;
#[cfg(test)]
mod virtualized_std_guest;
#[cfg(test)]
mod write_log_live;

#[cfg(test)]
mod fixture_membership;
