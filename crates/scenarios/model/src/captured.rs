//! Captured facts consumed by the pure MVP test-set evaluator.

use serde::{Deserialize, Serialize};

use crate::{FlowFailureKind, NamedNodeTerminal, RunTerminalStatus, TerminalRespond};

/// The bounded facts available to the four MVP assertion families.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Captured {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_terminal_outcome: Option<RunTerminalStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_respond: Option<TerminalRespond>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typed_flow_failure: Option<FlowFailureKind>,
    /// Multiplicity-preserving observations projected from frame-keyed node facts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub named_node_terminals: Vec<NamedNodeTerminal>,
}
