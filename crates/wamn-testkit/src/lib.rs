//! # wamn-testkit — the flow/node test-case + assertion vocabulary (11.4)
//!
//! A test case is **data, not code**: a [`TestCase`] loads from a JSON file
//! (the gate's `--cases` fixture) or a catalog jsonb column identically, and
//! [`evaluate`] is a PURE fold of a [`Captured`] fact bundle into an
//! [`Outcome`]. The effect shell (the `wamn-gates testkitbench` gate) drives the
//! node or flow and FILLS the [`Captured`] bundle; this crate only decides.
//!
//! PURE — serde only, no DB / clock / wasm / host dep — so the vocabulary is the
//! shared contract three lanes reconcile to. The status/kind taxonomy is REUSED
//! verbatim from `wamn-run-store` and the run-context mirrors
//! `wamn-node-invoke`'s [`WireRunContext`], so an assertion is stated in the same
//! enums the runner records and the node contract freezes — no parallel types.
//!
//! ## Two case shapes
//!
//! A case targets EITHER a node or a flow:
//! - `node_ref` present ⇒ a **node-level** case: the gate drives the pure
//!   `run(ctx, input)` handler in a warm `ServeNode` and captures the emission /
//!   port / error.
//! - `flow_ref` present ⇒ a **flow-level** case: the gate drives the flow under
//!   the test-double set (virtual clock + seeded random + egress recorder) and
//!   captures the run outcome, egress log, and admin-pool DB reads.
//!
//! ## The node-level case shape (consumed by the 7se and 828 lanes)
//!
//! For hand-authoring node cases, [`NodeCase`] is the compact shape the 7se lane
//! expresses and the 828 lane stores:
//!
//! ```json
//! {"name": "reject", "input": {"hold": {"moisture_pct": "12.00"}},
//!  "config": null,
//!  "expect": {"ok": {"value": {"recommended": "reject"}, "match": "subset", "port": "main"}}}
//! ```
//! or an error case:
//! ```json
//! {"name": "bad-input", "input": {"hold": {"moisture_pct": "x"}},
//!  "expect": {"error": "invalid-input"}}
//! ```
//! [`NodeCase::into_test_case`] lowers it to a [`TestCase`]: an `ok`-with-`match`
//! becomes [`Assertion::Equals`] or [`Assertion::Subset`] (+ [`Assertion::Port`]
//! when a `port` is given); an `error` becomes [`Assertion::ErrorClass`]. So the
//! sibling lanes' reconcile is a re-import, not a rewrite.

mod assertion;
mod captured;
mod evaluate;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use assertion::{Assertion, DbExpect, EgressAssertion, EgressMatcher};
pub use captured::{Captured, DbCapture, EgressRecord, RunFacts};
pub use evaluate::{AssertionResult, Outcome, evaluate, subset_match};
// The run-context is reused verbatim from the frozen wamn:node wire type.
pub use wamn_node_invoke::WireRunContext;
// The status/kind taxonomy is reused verbatim from the store — an assertion
// about a run/node uses the SAME enums the runner persists.
pub use wamn_run_store::{FailKind, NodeErrorKind, NodeRunStatus, RunStatus};

/// The case-format version this crate implements. Mirrors the
/// `wamn_catalog::SCHEMA_VERSION` precedent: `0.1.x` is additive/clarifying only;
/// a breaking wire change waits for `0.2`. Carried on every [`TestCase`] so a
/// stored case (JSON file or catalog jsonb) declares the shape it was written
/// against.
pub const SCHEMA_VERSION: &str = "0.1";

fn default_schema_version() -> String {
    SCHEMA_VERSION.to_string()
}

/// Which flow a flow-level case targets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct FlowRef {
    pub flow_id: String,
    pub version: u32,
}

/// Which node a node-level case targets. The gate serves a single node per run
/// (v0), so `node_id` is an informational label; its PRESENCE marks the case as
/// node-level.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct NodeRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
}

/// A single test case: an input (+ optional config/ctx) plus the assertions its
/// output/run must satisfy. Exactly one of `flow_ref` / `node_ref` is expected —
/// `node_ref` routes to the node path, `flow_ref` to the flow path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct TestCase {
    /// The case-format version (defaults to [`SCHEMA_VERSION`] when absent).
    #[serde(default = "default_schema_version")]
    pub schema_version: String,
    /// A human-readable case identifier (unique within a suite).
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_ref: Option<FlowRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_ref: Option<NodeRef>,
    /// The trigger/input payload (a node input, or a flow trigger body).
    pub input: Value,
    /// The node config document, if any (a node-level case).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
    /// An explicit run-context; when absent the gate builds a default one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx: Option<WireRunContext>,
    /// The assertions this case's output/run must satisfy.
    pub expect: Vec<Assertion>,
}

/// Exact vs deep-subset matching for an `ok` node-case emission.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MatchMode {
    /// Deep JSON equality.
    #[default]
    Exact,
    /// Deep subset ([`subset_match`]).
    Subset,
}

/// The success expectation of a [`NodeCase`]: the emitted `value` (matched
/// `exact` or `subset`) plus an optional `port`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct NodeOk {
    pub value: Value,
    #[serde(default, rename = "match")]
    pub match_mode: MatchMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
}

/// A node-case's expectation: a success emission or a classified error. Serde
/// shape: `{"ok": {value, match, port?}}` or `{"error": "<node-error kebab>"}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeExpect {
    Ok(NodeOk),
    Error(NodeErrorKind),
}

/// The compact node-level case shape the 7se lane expresses and the 828 lane
/// stores — `{name, input, config?, expect}`. [`into_test_case`](Self::into_test_case)
/// lowers it to the canonical [`TestCase`] vocabulary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct NodeCase {
    pub name: String,
    pub input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
    pub expect: NodeExpect,
}

impl NodeCase {
    /// Lower this compact node case to the canonical [`TestCase`]: an
    /// `ok`-with-`match` becomes [`Assertion::Equals`]/[`Assertion::Subset`]
    /// (+ [`Assertion::Port`] when a port is given); an `error` becomes
    /// [`Assertion::ErrorClass`]. The result carries `node_ref` (the node-level
    /// marker) and the current [`SCHEMA_VERSION`].
    pub fn into_test_case(self) -> TestCase {
        let expect = match self.expect {
            NodeExpect::Ok(ok) => {
                let mut assertions = vec![match ok.match_mode {
                    MatchMode::Exact => Assertion::Equals(ok.value),
                    MatchMode::Subset => Assertion::Subset(ok.value),
                }];
                if let Some(port) = ok.port {
                    assertions.push(Assertion::Port(port));
                }
                assertions
            }
            NodeExpect::Error(node_error) => vec![Assertion::ErrorClass { node_error }],
        };
        TestCase {
            schema_version: SCHEMA_VERSION.to_string(),
            name: self.name,
            flow_ref: None,
            node_ref: Some(NodeRef::default()),
            input: self.input,
            config: self.config,
            ctx: None,
            expect,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_case_round_trips_and_defaults_schema_version() {
        let case = TestCase {
            schema_version: SCHEMA_VERSION.to_string(),
            name: "reject".into(),
            flow_ref: None,
            node_ref: Some(NodeRef::default()),
            input: json!({"hold": {"moisture_pct": "12.00"}}),
            config: None,
            ctx: None,
            expect: vec![
                Assertion::Subset(json!({"recommended": "reject"})),
                Assertion::Port("main".into()),
            ],
        };
        let wire = serde_json::to_string(&case).unwrap();
        let back: TestCase = serde_json::from_str(&wire).unwrap();
        assert_eq!(case, back);

        // schema_version defaults when absent (a hand-authored case may omit it).
        let minimal: TestCase = serde_json::from_value(json!({
            "name": "n",
            "node-ref": {},
            "input": {},
            "expect": []
        }))
        .unwrap();
        assert_eq!(minimal.schema_version, SCHEMA_VERSION);
        assert!(minimal.node_ref.is_some());
    }

    /// The 7se node-case shape round-trips through JSON and lowers to the
    /// canonical vocabulary: `ok`+`subset`+`port` → Subset + Port; `error` →
    /// ErrorClass. This is the drift-guard for the sibling-lane reconcile.
    #[test]
    fn node_case_shape_and_lowering() {
        let ok_wire = json!({
            "name": "reject",
            "input": {"hold": {"moisture_pct": "12.00"}},
            "expect": {"ok": {"value": {"recommended": "reject"}, "match": "subset", "port": "main"}}
        });
        let nc: NodeCase = serde_json::from_value(ok_wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(&nc).unwrap(), ok_wire);
        let tc = nc.into_test_case();
        assert_eq!(
            tc.expect,
            vec![
                Assertion::Subset(json!({"recommended": "reject"})),
                Assertion::Port("main".into()),
            ]
        );
        assert!(tc.node_ref.is_some(), "a node case carries the node marker");

        // The error shape.
        let err_wire = json!({
            "name": "bad",
            "input": {"hold": {"moisture_pct": "x"}},
            "expect": {"error": "invalid-input"}
        });
        let nc: NodeCase = serde_json::from_value(err_wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(&nc).unwrap(), err_wire);
        assert_eq!(
            nc.into_test_case().expect,
            vec![Assertion::ErrorClass {
                node_error: NodeErrorKind::InvalidInput
            }]
        );
    }

    /// The default `match` is exact (an `ok` without `match` is an Equals).
    #[test]
    fn node_case_default_match_is_exact() {
        let nc: NodeCase = serde_json::from_value(json!({
            "name": "n",
            "input": {},
            "expect": {"ok": {"value": {"a": 1}}}
        }))
        .unwrap();
        assert_eq!(
            nc.into_test_case().expect,
            vec![Assertion::Equals(json!({"a": 1}))]
        );
    }
}
