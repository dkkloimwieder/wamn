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
