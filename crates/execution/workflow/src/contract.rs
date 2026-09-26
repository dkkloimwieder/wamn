//! The workflow contract: start, park, release, and list workflow runs.
//!
//! [`Workflows`] is the one interface a client uses, for example
//! `wamn-ctl-ops`, the event trigger, or a test. [`PostgresWorkflows`]
//! implements it over the run plane of one tenant and environment.
//!
//! A start admits a run: one `runs` row and one `run_queue` row, and the queue
//! then runs the wiring. A park holds a queued run that no replica holds, and a
//! release returns it to the queue. Park acts on the queue row only, so a
//! parked run keeps the status `dispatched`. A park inside a walk belongs to
//! approvals, which this contract does not cover (docs/plan/workflow-feature.md).

mod postgres;

use std::fmt;

use serde::Serialize;
use wamn_run_state::RunStatus;

pub use postgres::PostgresWorkflows;

/// Start, park, release, and list the workflow runs of one environment.
#[async_trait::async_trait]
pub trait Workflows: Send + Sync {
    /// Admit one run of a released wiring and return its run id. A repeated
    /// start with the same idempotency key and the same request returns the
    /// first run.
    async fn start(&self, request: &StartRequest) -> Result<String, WorkflowError>;

    /// Hold a queued run, so no claim takes it. Parking a parked run changes
    /// nothing.
    async fn park(&self, run_id: &str) -> Result<(), WorkflowError>;

    /// Return a parked run to the queue.
    async fn release(&self, run_id: &str) -> Result<(), WorkflowError>;

    /// The newest runs of the environment, at most `limit`.
    async fn list(&self, limit: u32) -> Result<Vec<WorkflowRun>, WorkflowError>;
}

/// What starts a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// An operator or a test acting as a service principal. The run executes
    /// with that principal's permissions at the time it runs.
    Automation { service_principal_id: String },
}

/// One run to admit.
#[derive(Debug, Clone, PartialEq)]
pub struct StartRequest {
    /// The minted release whose wiring the run executes.
    pub effective_release_id: u32,
    pub package_id: String,
    pub wiring_id: String,
    pub wiring_version: u32,
    pub idempotency_key: String,
    pub input: serde_json::Value,
    pub trigger: Trigger,
}

/// One run as [`Workflows::list`] reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowRun {
    pub run_id: String,
    pub package_id: String,
    pub wiring_id: String,
    pub wiring_version: u32,
    pub trigger_source: Option<String>,
    pub status: RunStatus,
    /// A queue row exists: the run waits for a claim or is running.
    pub queued: bool,
    pub parked: bool,
    /// RFC 3339, UTC.
    pub created_at: String,
}

/// Why a contract call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowErrorKind {
    /// No run with this id exists in the environment.
    NotFound,
    /// The release, the environment policy, or the service principal does
    /// not admit the start.
    Refused,
    /// The idempotency key belongs to a different request.
    Conflict,
    /// The run is running or finished, so it cannot be parked.
    NotParkable,
    /// The run is not parked, so it cannot be released.
    NotParked,
    /// The database failed.
    Storage,
}

/// A contract call failure, with its kind and context.
#[derive(Debug)]
pub struct WorkflowError {
    kind: WorkflowErrorKind,
    message: String,
    source: Option<anyhow::Error>,
}

impl WorkflowError {
    fn new(kind: WorkflowErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
        }
    }

    fn with_source(
        kind: WorkflowErrorKind,
        context: &str,
        source: impl Into<anyhow::Error>,
    ) -> Self {
        Self {
            kind,
            message: context.to_owned(),
            source: Some(source.into()),
        }
    }

    fn storage(context: &str, source: impl Into<anyhow::Error>) -> Self {
        Self::with_source(WorkflowErrorKind::Storage, context, source)
    }

    pub fn kind(&self) -> WorkflowErrorKind {
        self.kind
    }
}

impl fmt::Display for WorkflowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for WorkflowError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(AsRef::as_ref)
    }
}
