//! The per-call intent record that `invoke_operation` takes, as a storage trait.
//!
//! The route path needs no lease and no queue. No adapter implements this trait
//! yet: a Postgres implementation needs its own table, which is a later epic.

use std::fmt::{Display, Formatter};

use async_trait::async_trait;

use crate::operator_action::OperatorActionBasis;

/// Write-ahead intent records for single operation calls. The host never
/// replays a call whose intent began and never finished.
#[async_trait]
pub trait IntentStore: Send + Sync {
    /// Write the intent before the export runs. A repeated key returns the
    /// stored record, and a repeated key with a different input conflicts.
    async fn begin(&self, intent: &Intent<'_>) -> Result<Begun, StoreError>;
    /// Record the outcome of a begun intent.
    async fn finish(&self, id: &IntentId, outcome: &StoredOutcome) -> Result<(), StoreError>;
    /// List intents that began and never finished. The host never replays them.
    async fn uncertain(&self, limit: u32) -> Result<Vec<UncertainIntent>, StoreError>;
    /// Close an uncertain intent by an operator decision.
    async fn resolve(&self, id: &IntentId, basis: OperatorActionBasis) -> Result<(), StoreError>;
}

/// One operation call, recorded before its export runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Intent<'a> {
    pub tenant: &'a str,
    pub release: &'a str,
    pub package: &'a str,
    pub operation: &'a str,
    /// The caller's key. A repeated key within the tenant is the same intent.
    pub idempotency_key: &'a str,
    /// The canonical JSON SHA-256 of the call input.
    pub input_hash: &'a str,
    /// The call's bounded deadline in milliseconds.
    pub deadline_ms: u64,
}

/// The store-assigned identity of one intent record.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IntentId(pub String);

/// The result of [`IntentStore::begin`].
#[derive(Debug, Clone, PartialEq)]
pub enum Begun {
    /// No record had the key. The export may run.
    New(IntentId),
    /// The key already finished. The stored outcome is the answer.
    Finished(StoredOutcome),
    /// The key began and never finished. The export must not run again.
    Uncertain(IntentId),
    /// An operator resolved the uncertain key. It is never uncertain again, and
    /// the export never runs for it. The caller sends a new key.
    Resolved {
        id: IntentId,
        basis: OperatorActionBasis,
    },
    /// The key repeats with a different input. The stored intent is unchanged.
    Conflict(IntentId),
}

/// The recorded outcome of a finished intent.
#[derive(Debug, Clone, PartialEq)]
pub enum StoredOutcome {
    /// The export returned this value.
    Completed(serde_json::Value),
    /// The export failed with this error body.
    Failed(serde_json::Value),
}

/// An intent that began and never finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UncertainIntent {
    pub id: IntentId,
    pub tenant: String,
    pub release: String,
    pub package: String,
    pub operation: String,
    pub idempotency_key: String,
}

/// Stable category for an intent-store failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreErrorKind {
    /// The backing store failed.
    Storage,
    /// Stored data or a store result violated the intent contract.
    Contract,
}

/// Contextual failure from an intent store.
#[derive(Debug)]
pub struct StoreError {
    kind: StoreErrorKind,
    operation: &'static str,
    detail: String,
}

impl StoreError {
    /// Build a failure of `kind` for the named `operation`.
    pub fn new(kind: StoreErrorKind, operation: &'static str, detail: impl Into<String>) -> Self {
        Self {
            kind,
            operation,
            detail: detail.into(),
        }
    }

    /// Return the stable failure category.
    pub fn kind(&self) -> StoreErrorKind {
        self.kind
    }

    /// Return the operation that failed.
    pub fn operation(&self) -> &'static str {
        self.operation
    }
}

impl Display for StoreError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "intent store {} failed: {}",
            self.operation, self.detail
        )
    }
}

impl std::error::Error for StoreError {}
