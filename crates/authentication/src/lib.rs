//! Route authentication and issuer verification.

#[cfg(not(test))]
pub mod authorization;
// The Postgres adapter uses the library's trait, including in unit tests.
#[cfg(test)]
pub use wamn_authentication::authorization;
pub mod route_authentication;
pub mod session_keys;
