//! Integration and measurement tests that compose real repository adapters.
//!
//! MVP outcome: test coverage.

pub use wamn_test_infrastructure::ctl_process;

#[cfg(test)]
mod control_client_live;
#[cfg(test)]
mod route_authentication_live;
#[cfg(test)]
mod route_interface_live;
pub mod trusted_http_route;

#[cfg(test)]
mod fixture_membership;

#[cfg(test)]
#[path = "../../../crates/platform/runtime/tests/support/session_fixture.rs"]
#[expect(
    dead_code,
    reason = "The shared fixture also serves verifier lifecycle tests."
)]
mod session_fixture;

/// Product deployment fixtures shared by application acceptance tests.
pub mod product_application;
