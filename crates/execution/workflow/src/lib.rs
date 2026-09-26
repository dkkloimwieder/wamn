//! The wiring layer: everything that walks or delivers a wiring.
//!
//! A wiring is a graph of operation nodes with edges. This crate holds:
//!
//! - [`RouterDriver`]: it walks a wiring and runs each node through
//!   `invoke_operation` on the process's one operation host,
//! - the wiring arm of the delivery bridge, as the driver's
//!   [`WiringDelivery`](wamn_execution_host::WiringDelivery) implementation,
//! - [`QueueService`]: the queued run executor,
//! - [`Workflows`]: the workflow contract (start, park, release, list), and
//!   [`PostgresWorkflows`], its implementation over the run plane,
//! - [`lower_resolved_wiring`]: it lowers a resolved catalog wiring into the
//!   graph that the router walks.
//!
//! A client reads the release snapshot that publish writes, through
//! `wamn-control`, and binds [`PostgresWorkflows`] to it.
//!
//! `docs/architecture/overview.md` places this crate among the runtime owners.
//! The walk is the crate `wamn-router`, at
//! `crates/execution/workflow/router`. This crate is its only consumer.
//!
//! Every node calls `invoke_operation`. This crate owns no invocation path of
//! its own. A route never enters this crate.
//!
//! This crate sits above `wamn-execution-host` and below `services/host`. It
//! depends on the host crate, and the host crate never depends on it.
//! `tests/dependency_boundary.rs` checks that `wamn-engine`, `wamn-runtime`,
//! and `wamn-execution-host` link neither this crate nor `wamn-router`.

mod contract;
mod queue;
mod router_action;
mod router_driver;
mod router_response;
mod wiring_delivery;
mod wiring_lowering;

/// [`RouterDelivery::outcome`] is a [`wamn_router::Outcome`], so this crate
/// already hands its callers a verdict; re-exporting the type lets them read one
/// without taking a direct router dependency for a field they were already
/// given.
pub use wamn_router::Verdict;

pub use contract::{
    PostgresWorkflows, StartRequest, Trigger, WorkflowError, WorkflowErrorKind, WorkflowRun,
    Workflows,
};
pub use queue::{DEFAULT_QUEUE_LEASE_TTL_MS, QueueService, QueueServiceConfig};
pub use router_driver::{
    CandidateCaseRequest, CandidateExecutionRefusal, CandidateExecutionRefusalKind,
    CandidateWiringTarget, DEFAULT_WIRING_CACHE_CAPACITY, DeadlineAdjustments, RouterDelivery,
    RouterDriver, RouterDriverConfig, RouterDriverRequest, RouterDriverSnapshot,
    WIRING_CACHE_CAPACITY_ENV, WiringCacheCapacity,
};
pub use wiring_lowering::{
    GatedActiveWiring, ScopedWiringOperationFacts, WiringLoweringError, WiringLoweringErrorKind,
    WiringOperationFact, WiringParameterFact, WiringScope, lower_active_wiring,
    lower_resolved_wiring, project_component_operations,
};
