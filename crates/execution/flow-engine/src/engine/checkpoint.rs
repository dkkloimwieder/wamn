//! Versioned serialization for the active recovery checkpoint.
//!
//! A checkpoint is a direct snapshot of the reducer's resume point. It carries
//! no capture fields, node history, storage location, or other environment
//! configuration. The durable driver persists the returned JSON as one value
//! and restores it against the run's already-pinned [`Plan`].

use std::collections::{BTreeMap, HashMap, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Active, CallerState, ExecutionState, ExecutionStatus, Token};
use crate::{Plan, ThrottleKey};

const RECOVERY_CHECKPOINT_VERSION: u32 = 1;

/// A recovery checkpoint could not be encoded or restored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointError {
    /// The checkpoint is not valid JSON or does not have the V1 shape.
    InvalidEncoding { message: String },
    /// The serialized shape belongs to a checkpoint version this binary cannot read.
    UnsupportedVersion { version: u32 },
    /// Only a nonterminal resume point may be checkpointed.
    TerminalState { status: ExecutionStatus },
    /// A persisted token or visit counter names no node in the pinned plan.
    UnknownNode { node: String },
    /// Durable context must remain a JSON object.
    InvalidContext,
}

impl std::fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidEncoding { message } => {
                write!(f, "recovery checkpoint is invalid: {message}")
            }
            Self::UnsupportedVersion { version } => {
                write!(f, "unsupported recovery checkpoint version {version}")
            }
            Self::TerminalState { status } => {
                write!(f, "cannot snapshot terminal execution state {status:?}")
            }
            Self::UnknownNode { node } => {
                write!(f, "recovery checkpoint names unknown node {node:?}")
            }
            Self::InvalidContext => write!(f, "recovery checkpoint context must be an object"),
        }
    }
}

impl std::error::Error for CheckpointError {}

/// The first active recovery-checkpoint encoding.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RecoveryCheckpointV1 {
    version: u32,
    frontier: Vec<CheckpointToken>,
    current: Option<CheckpointCurrent>,
    visits: BTreeMap<String, u32>,
    step_seq: u64,
    context: Value,
    result: Value,
    caller: CheckpointCaller,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct CheckpointToken {
    node: String,
    payload: CheckpointPayload,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct CheckpointCurrent {
    node: String,
    payload: CheckpointPayload,
    attempt: u32,
    retry_until_ms: u64,
    throttle: Option<CheckpointThrottle>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct CheckpointThrottle {
    node_type: String,
    credential: Option<String>,
    host: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "value")]
enum CheckpointPayload {
    Inline(Value),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum CheckpointCaller {
    None,
    Attached,
    Released,
}

/// Serialize the complete nonterminal resume point as RecoveryCheckpointV1 JSON.
///
/// The per-invocation dispatch counter is deliberately absent: a restored
/// invocation receives a fresh dispatch budget. Occurrences remain derived from
/// `visits` when an ordered frontier token is promoted, matching the live
/// reducer rather than persisting a second counter on every token.
pub fn snapshot(state: &ExecutionState) -> Result<String, CheckpointError> {
    if state.status.is_terminal() {
        return Err(CheckpointError::TerminalState {
            status: state.status,
        });
    }

    let checkpoint = RecoveryCheckpointV1 {
        version: RECOVERY_CHECKPOINT_VERSION,
        frontier: state
            .frontier
            .iter()
            .map(|token| CheckpointToken {
                node: token.node.clone(),
                payload: CheckpointPayload::Inline(token.payload.clone()),
            })
            .collect(),
        current: state.current.as_ref().map(|current| CheckpointCurrent {
            node: current.node.clone(),
            payload: CheckpointPayload::Inline(current.payload.clone()),
            attempt: current.attempt,
            retry_until_ms: current.retry_until_ms,
            throttle: current
                .throttle
                .as_ref()
                .map(|throttle| CheckpointThrottle {
                    node_type: throttle.node_type.clone(),
                    credential: throttle.credential.clone(),
                    host: throttle.host.clone(),
                }),
        }),
        visits: state
            .visits
            .iter()
            .map(|(node, count)| (node.clone(), *count))
            .collect(),
        step_seq: state.step_seq,
        context: state.context.clone(),
        result: state.result.clone(),
        caller: match state.caller {
            CallerState::None => CheckpointCaller::None,
            CallerState::Attached => CheckpointCaller::Attached,
            CallerState::Released => CheckpointCaller::Released,
        },
    };

    serde_json::to_string(&checkpoint).map_err(|error| CheckpointError::InvalidEncoding {
        message: error.to_string(),
    })
}

/// Restore a reducer state directly from RecoveryCheckpointV1 JSON.
///
/// Restore validates every node against the pinned plan and rejects unknown
/// versions. It never accepts node history or capture data as a fallback.
pub fn restore(
    plan: &Plan<'_>,
    run_id: impl Into<String>,
    encoded: &str,
) -> Result<ExecutionState, CheckpointError> {
    let checkpoint: RecoveryCheckpointV1 =
        serde_json::from_str(encoded).map_err(|error| CheckpointError::InvalidEncoding {
            message: error.to_string(),
        })?;

    if checkpoint.version != RECOVERY_CHECKPOINT_VERSION {
        return Err(CheckpointError::UnsupportedVersion {
            version: checkpoint.version,
        });
    }
    if !checkpoint.context.is_object() {
        return Err(CheckpointError::InvalidContext);
    }

    for node in checkpoint
        .frontier
        .iter()
        .map(|token| token.node.as_str())
        .chain(
            checkpoint
                .current
                .iter()
                .map(|current| current.node.as_str()),
        )
        .chain(checkpoint.visits.keys().map(String::as_str))
    {
        if plan.node(node).is_none() {
            return Err(CheckpointError::UnknownNode {
                node: node.to_string(),
            });
        }
    }

    let frontier = checkpoint
        .frontier
        .into_iter()
        .map(|token| Token {
            node: token.node,
            payload: inline_payload(token.payload),
        })
        .collect::<VecDeque<_>>();
    let current = checkpoint.current.map(|current| Active {
        node: current.node,
        payload: inline_payload(current.payload),
        attempt: current.attempt,
        retry_until_ms: current.retry_until_ms,
        throttle: current.throttle.map(|throttle| ThrottleKey {
            node_type: throttle.node_type,
            credential: throttle.credential,
            host: throttle.host,
        }),
    });
    let visits = checkpoint.visits.into_iter().collect::<HashMap<_, _>>();

    Ok(ExecutionState {
        run_id: run_id.into(),
        status: ExecutionStatus::Running,
        frontier,
        current,
        step_seq: checkpoint.step_seq,
        dispatched: 0,
        visits,
        context: checkpoint.context,
        result: checkpoint.result,
        failure: None,
        caller: match checkpoint.caller {
            CheckpointCaller::None => CallerState::None,
            CheckpointCaller::Attached => CallerState::Attached,
            CheckpointCaller::Released => CallerState::Released,
        },
    })
}

fn inline_payload(payload: CheckpointPayload) -> Value {
    match payload {
        CheckpointPayload::Inline(value) => value,
    }
}
