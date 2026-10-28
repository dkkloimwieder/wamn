//! The workflow queue and its graph executor.
//!
//! A wiring is a graph of operation nodes with edges. This crate holds:
//!
//! - [`RouterDriver`]: it walks a wiring and runs each node through
//!   `invoke_operation` on the process's one operation host,
//! - [`QueueService`]: the queued run executor,
//! - [`Workflows`]: the workflow contract (park, release, list), and
//!   [`PostgresWorkflows`], its implementation over the run plane,
//! - [`lower_resolved_wiring`]: it lowers a resolved catalog wiring into the
//!   graph that the router walks.
//!
//! Manual starts use the host route. Every run pins its release and resolves
//! its graph from an environment workflow row.
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
//! `tools/repo-lint-dependencies` makes sure that `wamn-engine`, `wamn-runtime`,
//! and `wamn-execution-host` link neither this crate nor `wamn-router`.

mod contract;
mod queue;
mod router_action;
mod router_driver;
pub mod schedule;
mod wiring_lowering;

/// [`RouterDelivery::outcome`] is a [`wamn_router::Outcome`], so this crate
/// already hands its callers a verdict; re-exporting the type lets them read one
/// without taking a direct router dependency for a field they were already
/// given.
pub use wamn_router::Verdict;

pub use contract::{PostgresWorkflows, WorkflowError, WorkflowErrorType, WorkflowRun, Workflows};
pub use queue::{DEFAULT_QUEUE_LEASE_TTL_MS, QueueService, QueueServiceConfig};
pub use router_driver::{
    DeadlineAdjustments, RouterDelivery, RouterDriver, RouterDriverRequest, WorkflowIdentity,
    WorkflowRunRequest,
};
pub use wiring_lowering::{
    GatedActiveWiring, ScopedWiringOperationFacts, WiringLoweringError, WiringLoweringErrorType,
    WiringOperationFact, WiringParameterFact, WiringScope, lower_active_wiring,
    lower_resolved_wiring, project_component_operations,
};
