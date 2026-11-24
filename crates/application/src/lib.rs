//! Plain application contracts for route admission, delivery and operation sessions.

use std::time::Duration;

pub mod application_config;
pub mod database;
pub mod ingress;
pub mod invocation_authority;
pub mod native_routes;
pub mod result_signing;
pub mod session_identity;

/// Hard upper bound for one guest attempt and any host call it starts.
pub const MAX_HOST_CALL_DURATION: Duration = Duration::from_secs(30);
