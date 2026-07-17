//! The persisted status/kind vocabularies — the storage-literal side of the
//! engine's execution taxonomy. Each maps 1:1 to a `text` column value via
//! `as_sql`/`from_sql` (the SQL literals are exactly the serde kebab-case names,
//! tied to `deploy/run-state.sql` by a drift-guard test). `From<…>` conversions
//! adapt the pure-engine enums (`wamn_runner::{RunStatus, FailKind, NodeError}`)
//! into their persisted form, the way `wamn_api::SqlValue` mirrors the WIT.

use serde::{Deserialize, Serialize};

/// A run's lifecycle status. `Dispatched` is the write-ahead pre-state (a run row
/// exists before the runner picks it up); `InfrastructureFailure` is a janitor
/// verdict for a run that never reported back (both set by the trigger/queue
/// layer, 5.14 — the engine only produces the middle four).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunStatus {
    Dispatched,
    Running,
    Completed,
    Failed,
    Cancelled,
    InfrastructureFailure,
}

impl RunStatus {
    pub const ALL: [RunStatus; 6] = [
        RunStatus::Dispatched,
        RunStatus::Running,
        RunStatus::Completed,
        RunStatus::Failed,
        RunStatus::Cancelled,
        RunStatus::InfrastructureFailure,
    ];

    pub fn as_sql(self) -> &'static str {
        match self {
            RunStatus::Dispatched => "dispatched",
            RunStatus::Running => "running",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
            RunStatus::Cancelled => "cancelled",
            RunStatus::InfrastructureFailure => "infrastructure-failure",
        }
    }

    pub fn from_sql(s: &str) -> Option<RunStatus> {
        RunStatus::ALL.into_iter().find(|v| v.as_sql() == s)
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RunStatus::Completed
                | RunStatus::Failed
                | RunStatus::Cancelled
                | RunStatus::InfrastructureFailure
        )
    }
}

impl From<wamn_runner::RunStatus> for RunStatus {
    fn from(s: wamn_runner::RunStatus) -> RunStatus {
        match s {
            wamn_runner::RunStatus::Running => RunStatus::Running,
            wamn_runner::RunStatus::Completed => RunStatus::Completed,
            wamn_runner::RunStatus::Failed => RunStatus::Failed,
            wamn_runner::RunStatus::Cancelled => RunStatus::Cancelled,
        }
    }
}

/// Why a run failed — the persisted form of `wamn_runner::FailKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailKind {
    Terminal,
    RetryExhausted,
    InvalidInput,
    /// The run spent its per-invocation node-execution budget (cjv.4) — a
    /// permitted loop that never terminated.
    RunawayBudget,
}

impl FailKind {
    pub const ALL: [FailKind; 4] = [
        FailKind::Terminal,
        FailKind::RetryExhausted,
        FailKind::InvalidInput,
        FailKind::RunawayBudget,
    ];

    pub fn as_sql(self) -> &'static str {
        match self {
            FailKind::Terminal => "terminal",
            FailKind::RetryExhausted => "retry-exhausted",
            FailKind::InvalidInput => "invalid-input",
            FailKind::RunawayBudget => "runaway-budget",
        }
    }

    pub fn from_sql(s: &str) -> Option<FailKind> {
        FailKind::ALL.into_iter().find(|v| v.as_sql() == s)
    }
}

impl From<wamn_runner::FailKind> for FailKind {
    fn from(k: wamn_runner::FailKind) -> FailKind {
        match k {
            wamn_runner::FailKind::Terminal => FailKind::Terminal,
            wamn_runner::FailKind::RetryExhausted => FailKind::RetryExhausted,
            wamn_runner::FailKind::InvalidInput => FailKind::InvalidInput,
            wamn_runner::FailKind::RunawayBudget => FailKind::RunawayBudget,
        }
    }
}

/// A single node execution's status. `Running`/`Parked` rows are outstanding
/// (the driver re-dispatches them on resume); `Success`/`Error` are the completed
/// rows reconstruction replays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeRunStatus {
    Running,
    Parked,
    Success,
    Error,
}

impl NodeRunStatus {
    pub const ALL: [NodeRunStatus; 4] = [
        NodeRunStatus::Running,
        NodeRunStatus::Parked,
        NodeRunStatus::Success,
        NodeRunStatus::Error,
    ];

    pub fn as_sql(self) -> &'static str {
        match self {
            NodeRunStatus::Running => "running",
            NodeRunStatus::Parked => "parked",
            NodeRunStatus::Success => "success",
            NodeRunStatus::Error => "error",
        }
    }

    pub fn from_sql(s: &str) -> Option<NodeRunStatus> {
        NodeRunStatus::ALL.into_iter().find(|v| v.as_sql() == s)
    }

    /// Whether this node-run is a completed step reconstruction replays.
    pub fn is_completed(self) -> bool {
        matches!(self, NodeRunStatus::Success | NodeRunStatus::Error)
    }
}

/// A completed node-run's classified failure kind — the persisted form of the
/// `wamn:node` error taxonomy, for run history (reconstruction itself keys off
/// the recorded emission port, not this).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeErrorKind {
    Retryable,
    RateLimited,
    Terminal,
    InvalidInput,
    Cancelled,
}

impl NodeErrorKind {
    pub const ALL: [NodeErrorKind; 5] = [
        NodeErrorKind::Retryable,
        NodeErrorKind::RateLimited,
        NodeErrorKind::Terminal,
        NodeErrorKind::InvalidInput,
        NodeErrorKind::Cancelled,
    ];

    pub fn as_sql(self) -> &'static str {
        match self {
            NodeErrorKind::Retryable => "retryable",
            NodeErrorKind::RateLimited => "rate-limited",
            NodeErrorKind::Terminal => "terminal",
            NodeErrorKind::InvalidInput => "invalid-input",
            NodeErrorKind::Cancelled => "cancelled",
        }
    }

    pub fn from_sql(s: &str) -> Option<NodeErrorKind> {
        NodeErrorKind::ALL.into_iter().find(|v| v.as_sql() == s)
    }
}

impl From<&wamn_runner::NodeError> for NodeErrorKind {
    fn from(e: &wamn_runner::NodeError) -> NodeErrorKind {
        match e {
            wamn_runner::NodeError::Retryable(_) => NodeErrorKind::Retryable,
            wamn_runner::NodeError::RateLimited(_) => NodeErrorKind::RateLimited,
            wamn_runner::NodeError::Terminal(_) => NodeErrorKind::Terminal,
            wamn_runner::NodeError::InvalidInput(_) => NodeErrorKind::InvalidInput,
            wamn_runner::NodeError::Cancelled => NodeErrorKind::Cancelled,
        }
    }
}
