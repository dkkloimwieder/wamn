//! The persisted status/kind vocabularies — the storage-literal side of the
//! engine's execution taxonomy. Each maps 1:1 to a `text` column value via
//! `as_sql`/`from_sql` (the SQL literals are exactly the serde kebab-case names,
//! tied to `deploy/sql/run-state.sql` by a drift-guard test). `From<…>` conversions
//! adapt the pure-engine enums
//! (`wamn_runner::{ExecutionStatus, ExecutionFailureKind}` and
//! `wamn_flow::node_contract::NodeError`)
//! into their persisted form, the way `wamn_pg_core::SqlValue` mirrors the WIT.

use serde::{Deserialize, Serialize};
use wamn_flow::node_contract::NodeError;

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

/// The single caller-facing failure body for a run whose external effect is
/// uncertain. This envelope deliberately reports no external-effect verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectUncertainFailure {
    code: EffectUncertainCode,
    #[serde(deserialize_with = "deserialize_effect_uncertain_run_id")]
    run_id: String,
}

impl EffectUncertainFailure {
    /// Build the non-committal failure body for `run_id`.
    pub fn new(run_id: impl Into<String>) -> Result<Self, InvalidEffectUncertainRunId> {
        let run_id = validate_effect_uncertain_run_id(run_id.into())?;
        Ok(Self {
            code: EffectUncertainCode::EffectUncertain,
            run_id,
        })
    }

    /// The frozen failure-code spelling carried by this envelope.
    pub fn code(&self) -> &'static str {
        "effect-uncertain"
    }

    /// The run whose external effect has an unknown outcome.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// RFC 8785 bytes persisted and hashed for caller-outcome replay.
    pub fn canonical_json_bytes(&self) -> Vec<u8> {
        wamn_flow::canonical_json_bytes(&self.as_json())
    }

    /// Stable identity of the exact persisted caller-outcome body.
    pub fn canonical_json_hash(&self) -> String {
        wamn_flow::canonical_json_sha256(&self.as_json())
    }

    /// The JSON value stored in `runs.caller_outcome_json`.
    pub fn as_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("effect-uncertain failure always serializes")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum EffectUncertainCode {
    #[serde(rename = "effect-uncertain")]
    EffectUncertain,
}

/// An effect-uncertain failure cannot identify an empty run id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidEffectUncertainRunId {
    run_id: String,
}

impl InvalidEffectUncertainRunId {
    /// The rejected run id.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }
}

impl std::fmt::Display for InvalidEffectUncertainRunId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("effect-uncertain failure run_id must not be empty")
    }
}

impl std::error::Error for InvalidEffectUncertainRunId {}

fn validate_effect_uncertain_run_id(run_id: String) -> Result<String, InvalidEffectUncertainRunId> {
    if run_id.is_empty() {
        Err(InvalidEffectUncertainRunId { run_id })
    } else {
        Ok(run_id)
    }
}

fn deserialize_effect_uncertain_run_id<'de, Deserializer>(
    deserializer: Deserializer,
) -> Result<String, Deserializer::Error>
where
    Deserializer: serde::Deserializer<'de>,
{
    let run_id = String::deserialize(deserializer)?;
    validate_effect_uncertain_run_id(run_id).map_err(serde::de::Error::custom)
}

impl From<wamn_runner::ExecutionStatus> for RunStatus {
    fn from(s: wamn_runner::ExecutionStatus) -> RunStatus {
        match s {
            wamn_runner::ExecutionStatus::Running => RunStatus::Running,
            wamn_runner::ExecutionStatus::Completed => RunStatus::Completed,
            wamn_runner::ExecutionStatus::Failed => RunStatus::Failed,
        }
    }
}

/// Why a run failed — the persisted vocabulary, including engine failures and
/// failures owned by claim-time resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailKind {
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

impl FailKind {
    pub const ALL: [FailKind; 12] = [
        FailKind::Terminal,
        FailKind::RetryExhausted,
        FailKind::InvalidInput,
        FailKind::RunawayBudget,
        FailKind::EffectUncertain,
        FailKind::DepthBudget,
        FailKind::DispatchBudget,
        FailKind::UnresolvableName,
        FailKind::HashInvalidBytes,
        FailKind::ForeignRevision,
        FailKind::IncompatibleContract,
        FailKind::UnboundRequirement,
    ];

    pub fn as_sql(self) -> &'static str {
        match self {
            FailKind::Terminal => "terminal",
            FailKind::RetryExhausted => "retry-exhausted",
            FailKind::InvalidInput => "invalid-input",
            FailKind::RunawayBudget => "runaway-budget",
            FailKind::EffectUncertain => "effect-uncertain",
            FailKind::DepthBudget => "depth-budget",
            FailKind::DispatchBudget => "dispatch-budget",
            FailKind::UnresolvableName => "unresolvable-name",
            FailKind::HashInvalidBytes => "hash-invalid-bytes",
            FailKind::ForeignRevision => "foreign-revision",
            FailKind::IncompatibleContract => "incompatible-contract",
            FailKind::UnboundRequirement => "unbound-requirement",
        }
    }

    pub fn from_sql(s: &str) -> Option<FailKind> {
        FailKind::ALL.into_iter().find(|v| v.as_sql() == s)
    }
}

impl From<wamn_runner::ExecutionFailureKind> for FailKind {
    fn from(k: wamn_runner::ExecutionFailureKind) -> FailKind {
        match k {
            wamn_runner::ExecutionFailureKind::Terminal => FailKind::Terminal,
            wamn_runner::ExecutionFailureKind::RetryExhausted => FailKind::RetryExhausted,
            wamn_runner::ExecutionFailureKind::InvalidInput => FailKind::InvalidInput,
            wamn_runner::ExecutionFailureKind::RunawayBudget => FailKind::RunawayBudget,
        }
    }
}

/// A single node execution's status. `Started` rows are outstanding; `Success`
/// and `Error` are the completed rows reconstruction replays.
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

    /// Whether this node-run is a completed step reconstruction replays.
    pub fn is_completed(self) -> bool {
        matches!(self, NodeRunStatus::Success | NodeRunStatus::Error)
    }
}

/// A completed node-run's classified failure kind, for run history
/// (reconstruction itself keys off the recorded emission port, not this).
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

impl From<&NodeError> for NodeErrorKind {
    fn from(e: &NodeError) -> NodeErrorKind {
        match e {
            NodeError::Retryable(_) => NodeErrorKind::Retryable,
            NodeError::RateLimited(_) => NodeErrorKind::RateLimited,
            NodeError::Terminal(_) => NodeErrorKind::Terminal,
            NodeError::InvalidInput(_) => NodeErrorKind::InvalidInput,
        }
    }
}
