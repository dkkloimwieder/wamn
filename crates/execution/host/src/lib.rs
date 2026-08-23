//! Shared production router driver and digest-keyed component instance pool.

mod pool;
mod router_driver;

pub use pool::{
    ExecutionInstancePool, ExecutionLease, ExecutionPoolKey, ExecutionPoolLimits,
    ExecutionPoolSnapshot, INVOCATIONS_PER_INSTANCE, IdentityBindFailed,
    InvalidExecutionPoolLimits, InvocationDisposition, PoolCapacityError, PoolCleanupError,
    RetirementReason, ReusableExecutionInstance,
};
pub use router_driver::{
    DEFAULT_WIRING_CACHE_CAPACITY, RouterDelivery, RouterDriver, RouterDriverConfig,
    RouterDriverRequest, RouterDriverSnapshot, WIRING_CACHE_CAPACITY_ENV, WiringCacheCapacity,
    WiringResolution,
};
