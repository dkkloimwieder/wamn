//! Shared production router driver.

mod queue;
mod readiness;
mod router_delivery;
mod router_driver;
mod router_response;

/// [`RouterDelivery::outcome`] is a [`wamn_router::Outcome`], so this crate
/// already hands its callers a verdict; re-exporting the type lets them read one
/// without taking a direct router dependency for a field they were already
/// given.
pub use wamn_router::Verdict;

pub use queue::{DEFAULT_QUEUE_LEASE_TTL_MS, QueueService, QueueServiceConfig};
pub use readiness::{
    RELEASE_READINESS_CHECK_FAILED, RELEASE_READINESS_INVALIDATED, RouterReadinessProbe,
    RouterReadinessSnapshot, RouterReadinessStatus,
};
pub use router_delivery::{ROUTER_DELIVERY_ID, RouterDeliveryBridge};
pub use router_driver::{
    CandidateCaseRequest, CandidateExecutionRefusal, CandidateExecutionRefusalKind,
    CandidateWiringTarget, DEFAULT_WIRING_CACHE_CAPACITY, DeadlineAdjustment, DeadlineAdjustments,
    RouterDelivery, RouterDriver, RouterDriverConfig, RouterDriverRequest, RouterDriverSnapshot,
    WIRING_CACHE_CAPACITY_ENV, WiringCacheCapacity,
};

/// Exercise the production attachment resolver and registered-operation guard
/// from an integration test.
#[cfg(feature = "test-util")]
pub fn authorize_attachment_for_test(
    release: &wamn_runtime::release_manifest::LoadedRelease,
    attachment_id: &str,
    caller: Option<&wamn_runtime::plugins::flow_http_routing::AuthenticatedCaller>,
) -> Result<(), Box<str>> {
    router_delivery::authorize_attachment_for_test(release, attachment_id, caller)
}

pub mod warm_reuse;
