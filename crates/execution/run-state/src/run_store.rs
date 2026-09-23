//! The queued-run lifecycle as a storage trait, and its decision types.
//!
//! The host-owned Postgres adapter (`WamnPostgres` in `wamn-runtime`) is the
//! first implementation. The claim result stays with the adapter as
//! [`RunStore::ClaimResult`], because a candidate claim carries the adapter's
//! frozen connection-binding world.

use std::fmt::{Display, Formatter};

use async_trait::async_trait;

use crate::{FailKind, RunStatus};

/// The queued-run lifecycle: claim, lease renewal, completion with caller
/// release, the exhausted-run reap, and deadline reports.
#[async_trait]
pub trait RunStore: Send + Sync {
    /// The result of one claim turn.
    type ClaimResult: Send;

    /// Take the next claimable run: grant a lease and serialize its effect intent.
    async fn claim_next(
        &self,
        component_id: &str,
        package_ids: &[String],
        environment: &str,
        lease_ttl_ms: i64,
    ) -> Result<Self::ClaimResult, ProductionClaimError>;

    /// Extend a held lease, fenced by lease generation.
    async fn renew(
        &self,
        component_id: &str,
        run_id: &str,
        lease_generation: i64,
        lease_ttl_ms: i64,
    ) -> Result<ProductionLeaseRenewal, ProductionClaimError>;

    /// Record the outcome and release a waiting caller.
    async fn complete(
        &self,
        component_id: &str,
        run_id: &str,
        lease_generation: i64,
        completion: &ProductionCompletion,
    ) -> Result<ProductionCompletionResult, ProductionClaimError>;

    /// Reap at most one crash-budget-exhausted run. A run with effect evidence
    /// is left for the claimant to end as effect-uncertain. It is never replayed.
    async fn reap_exhausted(
        &self,
        component_id: &str,
        package_ids: &[String],
        environment: &str,
        grace_ms: i64,
    ) -> Result<ProductionReapResult, ProductionClaimError>;

    /// Record host deadline adjustments for a held run. Returns false when the
    /// run no longer belongs to this lease.
    async fn record_deadline_adjustments(
        &self,
        component_id: &str,
        run_id: &str,
        lease_generation: i64,
        adjustments: &serde_json::Value,
    ) -> Result<bool, ProductionClaimError>;
}

/// Stable category for a production-claim failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductionClaimErrorKind {
    /// Required host identity or database role authority was absent.
    Identity,
    /// The admitted run has no complete, valid frozen wiring identity.
    WiringIdentity,
    /// PostgreSQL checkout, transaction, query, or commit failed.
    Storage,
    /// Stored data or a typed database result violated the claim contract.
    Contract,
}

/// Contextual failure from the host-only production claim boundary.
#[derive(Debug)]
pub struct ProductionClaimError {
    kind: ProductionClaimErrorKind,
    operation: &'static str,
    detail: String,
}

impl ProductionClaimError {
    /// Build a failure of `kind` for the named `operation`.
    pub fn new(
        kind: ProductionClaimErrorKind,
        operation: &'static str,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            operation,
            detail: detail.into(),
        }
    }

    /// Return the stable failure category.
    pub fn kind(&self) -> ProductionClaimErrorKind {
        self.kind
    }

    /// Return the operation that failed.
    pub fn operation(&self) -> &'static str {
        self.operation
    }
}

impl Display for ProductionClaimError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "production claim {} failed: {}",
            self.operation, self.detail
        )
    }
}

impl std::error::Error for ProductionClaimError {}

/// Caller result stored before a queue run becomes terminal.
#[derive(Debug, Clone, PartialEq)]
pub struct ProductionCallerOutcome {
    kind: &'static str,
    body: serde_json::Value,
    http_status: u16,
    release_node_id: Option<String>,
}

impl ProductionCallerOutcome {
    /// A router `respond` verdict and its exact wiring node coordinate.
    pub fn responded(
        body: serde_json::Value,
        http_status: u16,
        release_node_id: impl Into<String>,
    ) -> Self {
        Self {
            kind: "responded",
            body,
            http_status,
            release_node_id: Some(release_node_id.into()),
        }
    }

    /// A router failure returned to an attached caller.
    pub fn failed(
        body: serde_json::Value,
        http_status: u16,
        release_node_id: Option<String>,
    ) -> Self {
        Self {
            kind: "failed",
            body,
            http_status,
            release_node_id,
        }
    }

    /// The persisted outcome kind: `responded` or `failed`.
    pub fn kind(&self) -> &'static str {
        self.kind
    }

    /// The body returned to the caller.
    pub fn body(&self) -> &serde_json::Value {
        &self.body
    }

    /// The HTTP status returned to the caller.
    pub fn http_status(&self) -> u16 {
        self.http_status
    }

    /// The wiring node that released the caller, if known.
    pub fn release_node_id(&self) -> Option<&str> {
        self.release_node_id.as_deref()
    }
}

/// Storage-shaped terminal fact derived from one router outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct ProductionCompletion {
    status: RunStatus,
    terminal_reason: &'static str,
    result: serde_json::Value,
    fail_kind: Option<FailKind>,
    caller: Option<ProductionCallerOutcome>,
}

impl ProductionCompletion {
    /// A completed router walk, optionally carrying a caller response.
    pub fn completed(result: serde_json::Value, caller: Option<ProductionCallerOutcome>) -> Self {
        Self {
            status: RunStatus::Completed,
            terminal_reason: "router-completed",
            result,
            fail_kind: None,
            caller,
        }
    }

    /// A failed router walk and its persisted failure class.
    pub fn failed(
        result: serde_json::Value,
        fail_kind: FailKind,
        caller: Option<ProductionCallerOutcome>,
    ) -> Self {
        Self {
            status: RunStatus::Failed,
            terminal_reason: "router-failed",
            result,
            fail_kind: Some(fail_kind),
            caller,
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
    pub fn fail_kind(&self) -> Option<FailKind> {
        self.fail_kind
    }

    /// The caller outcome released with the run, if a caller is attached.
    pub fn caller(&self) -> Option<&ProductionCallerOutcome> {
        self.caller.as_ref()
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
    /// No exhausted row was visible to this tenant.
    Empty,
    /// Immutable effect evidence owns this row; the ordinary claimant must
    /// terminalize it as effect-uncertain.
    EffectAttempt { run_id: String },
    /// The selected pre-effect row was marked infrastructure-failure and
    /// dequeued with exact caller compare-and-set semantics.
    Reaped { run_id: String },
}
