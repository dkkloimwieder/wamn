//! Plain application contracts for route admission, delivery and operation sessions.

use std::fmt;
use std::time::{Duration, Instant};

pub mod application_config;
pub mod database;
pub mod ingress;
pub mod invocation_authority;
pub mod native_routes;
pub mod result_signing;
pub mod session_identity;
pub mod workload;

/// Hard upper bound for one guest attempt and any host call it starts.
pub const MAX_HOST_CALL_DURATION: Duration = Duration::from_secs(30);

/// Invocation lifetime exposed to host work that can outlive a guest call.
#[async_trait::async_trait]
pub trait InvocationLifetime: fmt::Debug + Send + Sync {
    fn deadline(&self) -> Instant;
    /// Resolves when the invocation is cancelled or reaches its deadline.
    async fn cancelled(&self);
}
