//! Shared production router driver.

mod readiness;
mod router_delivery;
mod router_driver;

pub use readiness::{
    RELEASE_READINESS_CHECK_FAILED, RELEASE_READINESS_INVALIDATED, RouterReadinessProbe,
    RouterReadinessSnapshot, RouterReadinessStatus,
};
pub use router_delivery::{ROUTER_DELIVERY_ID, RouterDeliveryBridge};
pub use router_driver::{
    CandidateCaseRequest, CandidateExecutionRefusal, CandidateExecutionRefusalKind,
    CandidateWiringTarget, DEFAULT_WIRING_CACHE_CAPACITY, PreloadedWiringMissing, RouterDelivery,
    RouterDriver, RouterDriverConfig, RouterDriverRequest, RouterDriverSnapshot,
    WIRING_CACHE_CAPACITY_ENV, WiringCacheCapacity, WiringResolution,
};

/// Exercise the production attachment resolver and registered-operation guard
/// from an integration proof.
#[cfg(feature = "test-util")]
pub fn authorize_attachment_for_test(
    release: &wamn_runtime::release_manifest::ReleaseManifestWeld,
    attachment_id: &str,
    caller: Option<&wamn_runtime::plugins::flow_http_routing::AuthenticatedCaller>,
) -> Result<(), Box<str>> {
    router_delivery::authorize_attachment_for_test(release, attachment_id, caller)
}
