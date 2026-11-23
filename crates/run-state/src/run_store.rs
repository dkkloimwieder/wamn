//! Storage contract and results for the queued-run lifecycle.

use crate::admission::{RunAdmission, RunAdmitted};
use crate::persistence::LeaseTurn;
use crate::{FailType, RunStatus};
use std::fmt::{Display, Formatter};

/// Stable category for a production-lease failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductionLeaseErrorType {
    /// Required host identity or database role authority was absent.
    Identity,
    /// The admitted run has no complete, valid frozen graph identity.
    GraphIdentity,
    /// PostgreSQL checkout, transaction, query, or commit failed.
    Storage,
    /// Stored data or a typed database result violated the lease contract.
    Contract,
}

/// Contextual failure from the host-only production lease boundary.
#[derive(Debug)]
pub struct ProductionLeaseError {
    type_: ProductionLeaseErrorType,
    operation: &'static str,
    detail: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl ProductionLeaseError {
    /// Build a failure of `type` for the named `operation`.
    pub fn new(
        type_: ProductionLeaseErrorType,
        operation: &'static str,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            type_,
            operation,
            detail: detail.into(),
            source: None,
        }
    }

    /// Retain the underlying failure at a host admission boundary.
    pub fn with_source(
        type_: ProductionLeaseErrorType,
        operation: &'static str,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        let source = source.into();
        Self {
            type_,
            operation,
            detail: source.to_string(),
            source: Some(source),
        }
    }

    /// A storage failure with its native source.
    pub fn storage(
        operation: &'static str,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self::with_source(ProductionLeaseErrorType::Storage, operation, source)
    }

    /// A refusal of the run persistence contract.
    pub fn contract(operation: &'static str, detail: impl Into<String>) -> Self {
        Self::new(ProductionLeaseErrorType::Contract, operation, detail)
    }

    /// Invalid stored or supplied data, retaining its parser error.
    pub fn invalid(
        operation: &'static str,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self::with_source(ProductionLeaseErrorType::Contract, operation, source)
    }

    /// Return the stable failure category.
    pub fn error_type(&self) -> ProductionLeaseErrorType {
        self.type_
    }

    /// Return the operation that failed.
    pub fn operation(&self) -> &'static str {
        self.operation
    }
}

impl Display for ProductionLeaseError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "production lease {} failed: {}",
            self.operation, self.detail
        )
    }
}

impl std::error::Error for ProductionLeaseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

/// Storage-shaped terminal fact derived from one router outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct ProductionCompletion {
    status: RunStatus,
    terminal_reason: &'static str,
    result: serde_json::Value,
    fail_type: Option<FailType>,
}

impl ProductionCompletion {
    /// A completed router walk, with its result.
    pub fn completed(result: serde_json::Value) -> Self {
        Self {
            status: RunStatus::Completed,
            terminal_reason: "router-completed",
            result,
            fail_type: None,
        }
    }

    /// A failed router walk and its persisted failure class.
    pub fn failed(result: serde_json::Value, fail_type: FailType) -> Self {
        Self {
            status: RunStatus::Failed,
            terminal_reason: "router-failed",
            result,
            fail_type: Some(fail_type),
        }
    }

    /// The terminal run status.
    pub fn status(&self) -> RunStatus {
        self.status
    }

    /// The persisted terminal reason.
    pub fn terminal_reason(&self) -> &'static str {
        self.terminal_reason
    }

    /// The run result.
    pub fn result(&self) -> &serde_json::Value {
        &self.result
    }

    /// The persisted failure class of a failed walk.
    pub fn fail_type(&self) -> Option<FailType> {
        self.fail_type
    }
}

/// Result of committing a router outcome under the exact queue fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductionCompletionResult {
    Terminalized,
    AlreadyTerminal(RunStatus),
    FenceLost,
    NotFound,
}

/// Result of one generation-fenced lease heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductionLeaseRenewal {
    Renewed,
    FenceLost,
}

/// Result of one host-owned crash-budget janitor turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProductionReapResult {
    /// No exhausted row was visible to this project.
    Empty,
    /// The selected row was marked infrastructure-failure and
    /// dequeued under the queue locks.
    Reaped { run_id: String },
}

/// Run storage operations on the caller’s authorized transaction.
pub trait RunStore {
    /// Insert and lease the run atomically on an authorized transaction.
    fn admit_run(
        &mut self,
        run: &RunAdmission<'_>,
        runner: &str,
        lease_ttl_ms: i64,
    ) -> impl Future<Output = Result<RunAdmitted, ProductionLeaseError>> + Send;

    /// Find the history actor for the CDC row when the source has a history table.
    fn history_actor(
        &mut self,
        envelope: &wamn_event::Envelope,
    ) -> impl Future<
        Output = Result<Option<wamn_identity_protocol::PrincipalId>, ProductionLeaseError>,
    > + Send;

    /// Renew a live lease under its owner and generation fence.
    fn renew(
        &mut self,
        run_id: &str,
        runner: &str,
        lease_generation: i64,
        lease_ttl_ms: i64,
    ) -> impl Future<Output = Result<ProductionLeaseRenewal, ProductionLeaseError>> + Send;

    /// Store the terminal outcome and remove the queue entry under the lease fence.
    fn complete(
        &mut self,
        run_id: &str,
        runner: &str,
        lease_generation: i64,
        completion: &ProductionCompletion,
    ) -> impl Future<Output = Result<ProductionCompletionResult, ProductionLeaseError>> + Send;

    /// Lease one run. The caller commits both `Leased` and `GrantRefused` turns.
    fn lease_next(
        &mut self,
        runner: &str,
        application: &wamn_registry::ApplicationName,
        environment: &wamn_registry::EnvName,
        lease_ttl_ms: i64,
    ) -> impl Future<Output = Result<LeaseTurn, ProductionLeaseError>> + Send;

    /// Fail and dequeue one expired run whose retry budget is spent.
    fn reap_exhausted(
        &mut self,
        application: &wamn_registry::ApplicationName,
        environment: &wamn_registry::EnvName,
        grace_ms: i64,
    ) -> impl Future<Output = Result<ProductionReapResult, ProductionLeaseError>> + Send;

    /// Record the Response checkpoint under the live lease fence.
    fn checkpoint(
        &mut self,
        run_id: &str,
        runner: &str,
        lease_generation: i64,
        state: &serde_json::Value,
    ) -> impl Future<Output = Result<bool, ProductionLeaseError>> + Send;

    /// Check that the host still owns the unexpired lease before dispatching a node.
    fn lease_remaining_ms(
        &mut self,
        run: &str,
        runner: &str,
        generation: i64,
    ) -> impl Future<Output = Result<Option<i64>, ProductionLeaseError>> + Send;

    /// Delete expired terminal history on an authorized retention connection.
    fn prune_terminal_runs(
        &mut self,
        retention_days: u32,
    ) -> impl Future<Output = Result<u64, ProductionLeaseError>> + Send;
}
