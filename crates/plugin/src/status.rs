//! The persisted status/type vocabularies — the storage-literal side of the
//! engine's execution taxonomy. Each maps 1:1 to a `text` column value via
//! `as_sql`/`from_sql` (the SQL literals are exactly the serde kebab-case names,
//! tied to `deploy/sql/run-state.sql` by a drift-guard test).

use serde::{Deserialize, Serialize};

/// A run's lifecycle status. `Dispatched` is the write-ahead pre-state (a run row
/// exists before the runner picks it up). `InfrastructureFailure` is a janitor
/// verdict for a run that never reported back. The storage layer owns those two.
/// The engine only produces the other three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunStatus {
    Dispatched,
    Running,
    Completed,
    Failed,
    InfrastructureFailure,
}

impl RunStatus {
    pub const ALL: [RunStatus; 5] = [
        RunStatus::Dispatched,
        RunStatus::Running,
        RunStatus::Completed,
        RunStatus::Failed,
        RunStatus::InfrastructureFailure,
    ];

    pub fn as_sql(self) -> &'static str {
        match self {
            RunStatus::Dispatched => "dispatched",
            RunStatus::Running => "running",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
            RunStatus::InfrastructureFailure => "infrastructure-failure",
        }
    }

    pub fn from_sql(s: &str) -> Option<RunStatus> {
        RunStatus::ALL.into_iter().find(|v| v.as_sql() == s)
    }
}

/// Why a run failed, including engine failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailType {
    Terminal,
    RetryExhausted,
    InvalidInput,
    /// The run spent its per-invocation node-execution budget (cjv.4) — a
    /// permitted loop that never terminated.
    RunawayBudget,
    /// A referenced flow name could not be resolved in the pinned release.
    UnresolvableName,
    /// A resolved artifact's bytes did not match its content hash.
    HashInvalidBytes,
    /// A resolved artifact belongs to a different revision.
    ForeignRevision,
    /// A resolved flow's contract is incompatible with its call site.
    IncompatibleContract,
}

impl FailType {
    pub const ALL: [FailType; 8] = [
        FailType::Terminal,
        FailType::RetryExhausted,
        FailType::InvalidInput,
        FailType::RunawayBudget,
        FailType::UnresolvableName,
        FailType::HashInvalidBytes,
        FailType::ForeignRevision,
        FailType::IncompatibleContract,
    ];

    pub fn as_sql(self) -> &'static str {
        match self {
            FailType::Terminal => "terminal",
            FailType::RetryExhausted => "retry-exhausted",
            FailType::InvalidInput => "invalid-input",
            FailType::RunawayBudget => "runaway-budget",
            FailType::UnresolvableName => "unresolvable-name",
            FailType::HashInvalidBytes => "hash-invalid-bytes",
            FailType::ForeignRevision => "foreign-revision",
            FailType::IncompatibleContract => "incompatible-contract",
        }
    }

    pub fn from_sql(s: &str) -> Option<FailType> {
        FailType::ALL.into_iter().find(|v| v.as_sql() == s)
    }
}
