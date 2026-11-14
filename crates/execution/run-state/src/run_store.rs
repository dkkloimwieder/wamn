//! Completion and error types for the queued-run lifecycle.

use std::fmt::{Display, Formatter};

use crate::{FailType, RunStatus};

/// Stable category for a production-lease failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductionLeaseErrorType {
    /// Required host identity or database role authority was absent.
    Identity,
    /// The admitted run has no complete, valid frozen wiring identity.
    WiringIdentity,
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

    /// Preserve the database message and constraint, without row-bearing DETAIL or HINT.
    /// Run rows contain application inputs, results, and invocation context.
    pub fn postgres(operation: &'static str, error: &sqlx::Error) -> Self {
        let detail = match error.as_database_error() {
            Some(database) => match database.constraint() {
                Some(constraint) => format!("{} ({constraint})", database.message()),
                None => database.message().to_owned(),
            },
            None => error.to_string(),
        };
        ProductionLeaseError::new(ProductionLeaseErrorType::Storage, operation, detail)
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
