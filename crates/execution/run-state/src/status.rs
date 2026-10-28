//! The persisted status/kind vocabularies — the storage-literal side of the
//! engine's execution taxonomy. Each maps 1:1 to a `text` column value via
//! `as_sql`/`from_sql` (the SQL literals are exactly the serde kebab-case names,
//! tied to `deploy/sql/run-state.sql` by a drift-guard test). A `From<…>` conversion
//! adapts `wamn_execution_contract::node_contract::NodeError` into its persisted
//! form.

use serde::{Deserialize, Serialize};
use wamn_execution_contract::node_contract::NodeError;

/// A run's lifecycle status. `Dispatched` is the write-ahead pre-state (a run row
/// exists before the runner picks it up); `InfrastructureFailure` is a janitor
/// verdict for a run that never reported back; `EffectUncertain` is an unresolved
/// operator state after an effectful crash. The storage layer owns those three;
/// the engine only produces the other three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunStatus {
    Dispatched,
    Running,
    Completed,
    Failed,
    InfrastructureFailure,
    /// An effectful attempt may have escaped without a durable outcome, so the
    /// platform cannot safely dispatch it again or claim whether it occurred.
    EffectUncertain,
}

impl RunStatus {
    pub const ALL: [RunStatus; 6] = [
        RunStatus::Dispatched,
        RunStatus::Running,
        RunStatus::Completed,
        RunStatus::Failed,
        RunStatus::InfrastructureFailure,
        RunStatus::EffectUncertain,
    ];

    pub fn as_sql(self) -> &'static str {
        match self {
            RunStatus::Dispatched => "dispatched",
            RunStatus::Running => "running",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
            RunStatus::InfrastructureFailure => "infrastructure-failure",
            RunStatus::EffectUncertain => "effect-uncertain",
        }
    }

    pub fn from_sql(s: &str) -> Option<RunStatus> {
        RunStatus::ALL.into_iter().find(|v| v.as_sql() == s)
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RunStatus::Completed | RunStatus::Failed | RunStatus::InfrastructureFailure
        )
    }
}

/// Why a run failed, including engine failures and unresolved effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailType {
    Terminal,
    RetryExhausted,
    InvalidInput,
    /// The run spent its per-invocation node-execution budget (cjv.4) — a
    /// permitted loop that never terminated.
    RunawayBudget,
    /// An effect may have escaped before its durable completion
    /// record was written, so repeating it would be unsafe.
    EffectUncertain,
    /// The root run exhausted its maximum call depth.
    DepthBudget,
    /// The root run exhausted its total-dispatched-node budget.
    DispatchBudget,
    /// A referenced flow name could not be resolved in the pinned release.
    UnresolvableName,
    /// A resolved artifact's bytes did not match its content hash.
    HashInvalidBytes,
    /// A resolved artifact belongs to a different revision.
    ForeignRevision,
    /// A resolved flow's contract is incompatible with its call site.
    IncompatibleContract,
    /// A resolved flow has a requirement without an environment binding.
    UnboundRequirement,
}

impl FailType {
    pub const ALL: [FailType; 12] = [
        FailType::Terminal,
        FailType::RetryExhausted,
        FailType::InvalidInput,
        FailType::RunawayBudget,
        FailType::EffectUncertain,
        FailType::DepthBudget,
        FailType::DispatchBudget,
        FailType::UnresolvableName,
        FailType::HashInvalidBytes,
        FailType::ForeignRevision,
        FailType::IncompatibleContract,
        FailType::UnboundRequirement,
    ];

    pub fn as_sql(self) -> &'static str {
        match self {
            FailType::Terminal => "terminal",
            FailType::RetryExhausted => "retry-exhausted",
            FailType::InvalidInput => "invalid-input",
            FailType::RunawayBudget => "runaway-budget",
            FailType::EffectUncertain => "effect-uncertain",
            FailType::DepthBudget => "depth-budget",
            FailType::DispatchBudget => "dispatch-budget",
            FailType::UnresolvableName => "unresolvable-name",
            FailType::HashInvalidBytes => "hash-invalid-bytes",
            FailType::ForeignRevision => "foreign-revision",
            FailType::IncompatibleContract => "incompatible-contract",
            FailType::UnboundRequirement => "unbound-requirement",
        }
    }

    pub fn from_sql(s: &str) -> Option<FailType> {
        FailType::ALL.into_iter().find(|v| v.as_sql() == s)
    }
}

/// A single node execution's status. `Started` rows are outstanding; `Success`
/// and `Error` are completed history facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeRunStatus {
    Started,
    Success,
    Error,
}

impl NodeRunStatus {
    pub const ALL: [NodeRunStatus; 3] = [
        NodeRunStatus::Started,
        NodeRunStatus::Success,
        NodeRunStatus::Error,
    ];

    pub fn as_sql(self) -> &'static str {
        match self {
            NodeRunStatus::Started => "started",
            NodeRunStatus::Success => "success",
            NodeRunStatus::Error => "error",
        }
    }

    pub fn from_sql(s: &str) -> Option<NodeRunStatus> {
        NodeRunStatus::ALL.into_iter().find(|v| v.as_sql() == s)
    }
}

/// A completed node-run's classified failure kind for run history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeErrorType {
    Retryable,
    RateLimited,
    Terminal,
    InvalidInput,
}

impl NodeErrorType {
    pub const ALL: [NodeErrorType; 4] = [
        NodeErrorType::Retryable,
        NodeErrorType::RateLimited,
        NodeErrorType::Terminal,
        NodeErrorType::InvalidInput,
    ];

    pub fn as_sql(self) -> &'static str {
        match self {
            NodeErrorType::Retryable => "retryable",
            NodeErrorType::RateLimited => "rate-limited",
            NodeErrorType::Terminal => "terminal",
            NodeErrorType::InvalidInput => "invalid-input",
        }
    }

    pub fn from_sql(s: &str) -> Option<NodeErrorType> {
        NodeErrorType::ALL.into_iter().find(|v| v.as_sql() == s)
    }
}

impl From<&NodeError> for NodeErrorType {
    fn from(e: &NodeError) -> NodeErrorType {
        match e {
            NodeError::Retryable(_) => NodeErrorType::Retryable,
            NodeError::RateLimited(_) => NodeErrorType::RateLimited,
            NodeError::Terminal(_) => NodeErrorType::Terminal,
            NodeError::InvalidInput(_) => NodeErrorType::InvalidInput,
        }
    }
}
