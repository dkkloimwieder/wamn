//! Captured facts consumed by the pure MVP test-case evaluator.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::FlowFailureKind;

/// The bounded facts one run makes available to the flat expectation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Captured {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<CapturedResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<FlowFailureKind>,
}

/// The terminal response a run produced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CapturedResponse {
    pub status: u16,
    pub body: Value,
}
