//! The matcher vocabulary (11.4): the `Assertion` enum and its supporting
//! shapes. Every type is `#[serde(rename_all = "kebab-case")]` so a case is
//! hand-authorable JSON and a catalog jsonb column reads identically. The exact
//! wire form is PINNED by the round-trip drift-guard tests below — the two
//! sibling lanes (7se node-cases, 828 cases-as-jsonb) reconcile TO this shape.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use wamn_run_store::{FailKind, NodeErrorKind, RunStatus};

/// One assertion against a [`Captured`](crate::Captured) fact bundle. The
/// evaluator folds each variant to an [`AssertionResult`](crate::AssertionResult).
///
/// The families:
/// - **node output** — `Equals` / `Subset` / `PathEquals` / `Port` read the
///   node's emission ([`Captured::node_output`](crate::Captured::node_output) /
///   [`node_port`](crate::Captured::node_port)).
/// - **db state** — `DbState` reads a captured query result.
/// - **egress** — `Egress` reads the recorded outbound requests for one flow.
/// - **error path** — `ErrorClass` reads the node error; `RunOutcome` the run's
///   terminal status.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Assertion {
    /// The node output equals this value EXACTLY (deep JSON equality).
    Equals(Value),
    /// The node output DEEP-SUBSET-matches this value
    /// ([`subset_match`](crate::subset_match)): every expected object key is
    /// present and recursively matches; every expected array element matches
    /// SOME actual element (order-insensitive, no length constraint).
    Subset(Value),
    /// The node output at this JSON pointer (RFC 6901, e.g. `/hold/moisture`)
    /// equals this value exactly.
    PathEquals { pointer: String, value: Value },
    /// The node emitted on this port (the harness maps the absent/default port
    /// to the literal `main`).
    Port(String),
    /// A DB-state assertion: the named query (run by the harness via the admin
    /// pool) satisfies `expect`. Correlated to its capture by `(query, params)`.
    DbState {
        query: String,
        #[serde(default)]
        params: Vec<Value>,
        expect: DbExpect,
    },
    /// An egress assertion over the outbound requests recorded for `flow` (the
    /// declaring workload id).
    Egress {
        flow: String,
        calls: EgressAssertion,
    },
    /// The node returned this error class (the frozen `wamn:node` taxonomy).
    #[serde(rename_all = "kebab-case")]
    ErrorClass { node_error: NodeErrorKind },
    /// The run reached this terminal status; `fail_kind` / `fail_node`, when
    /// present, are additional constraints (absent = don't-care).
    #[serde(rename_all = "kebab-case")]
    RunOutcome {
        status: RunStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fail_kind: Option<FailKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fail_node: Option<String>,
    },
}

/// What a [`DbState`](Assertion::DbState) query's result must be.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DbExpect {
    /// The query returned exactly this many rows.
    RowCount(u64),
    /// The first row matches `row` — exactly, or by deep subset when
    /// `subset` is true (the common "assert a few columns" DB case).
    FirstRow {
        row: Value,
        #[serde(default)]
        subset: bool,
    },
    /// The query returned no rows.
    Empty,
}

/// A partial match against one recorded outbound request: a present field must
/// equal the record's; an absent field is a wildcard.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct EgressMatcher {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// How a flow's recorded egress must relate to a set of matchers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EgressAssertion {
    /// Set-equality: the flow's recorded calls are EXACTLY these — every call is
    /// matched, every matcher is used, and the counts agree. "Nothing else": an
    /// EXTRA outbound call fails this. This is the security regression.
    ExactlyThese(Vec<EgressMatcher>),
    /// The flow made AT LEAST these calls (each matcher matches some record); an
    /// extra call is permitted.
    Includes(Vec<EgressMatcher>),
    /// No recorded call for the flow was denied (all egress was permitted).
    NoneDenied,
    /// The flow made exactly this many recorded outbound calls.
    Count(u64),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Round-trip one assertion and pin its EXACT kebab-case wire form. A drift
    /// in any tag breaks a sibling lane's reconcile, so every variant is pinned.
    fn pin(a: &Assertion, wire: Value) {
        let got = serde_json::to_value(a).expect("serialize");
        assert_eq!(got, wire, "wire form drifted for {a:?}");
        let back: Assertion = serde_json::from_value(wire).expect("deserialize");
        assert_eq!(&back, a, "round-trip changed {a:?}");
    }

    #[test]
    fn equals_wire_form() {
        pin(
            &Assertion::Equals(json!({"recommended": "reject"})),
            json!({"equals": {"recommended": "reject"}}),
        );
    }

    #[test]
    fn subset_wire_form() {
        pin(
            &Assertion::Subset(json!({"recommended": "accept"})),
            json!({"subset": {"recommended": "accept"}}),
        );
    }

    #[test]
    fn path_equals_wire_form() {
        pin(
            &Assertion::PathEquals {
                pointer: "/hold/moisture".into(),
                value: json!(4.0),
            },
            json!({"path-equals": {"pointer": "/hold/moisture", "value": 4.0}}),
        );
    }

    #[test]
    fn port_wire_form() {
        pin(&Assertion::Port("main".into()), json!({"port": "main"}));
    }

    #[test]
    fn db_state_wire_forms() {
        pin(
            &Assertion::DbState {
                query: "select to_jsonb(sink) from sink".into(),
                params: vec![],
                expect: DbExpect::RowCount(1),
            },
            json!({"db-state": {"query": "select to_jsonb(sink) from sink", "params": [], "expect": {"row-count": 1}}}),
        );
        pin(
            &Assertion::DbState {
                query: "q".into(),
                params: vec![json!("s6-tenant")],
                expect: DbExpect::FirstRow {
                    row: json!({"n": 1}),
                    subset: true,
                },
            },
            json!({"db-state": {"query": "q", "params": ["s6-tenant"], "expect": {"first-row": {"row": {"n": 1}, "subset": true}}}}),
        );
        pin(
            &Assertion::DbState {
                query: "q".into(),
                params: vec![],
                expect: DbExpect::Empty,
            },
            json!({"db-state": {"query": "q", "params": [], "expect": "empty"}}),
        );
    }

    #[test]
    fn egress_wire_forms() {
        pin(
            &Assertion::Egress {
                flow: "s6-runworker".into(),
                calls: EgressAssertion::ExactlyThese(vec![EgressMatcher {
                    method: Some("GET".into()),
                    authority: Some("echo.local:8080".into()),
                    path: None,
                }]),
            },
            json!({"egress": {"flow": "s6-runworker", "calls": {"exactly-these": [{"method": "GET", "authority": "echo.local:8080"}]}}}),
        );
        pin(
            &Assertion::Egress {
                flow: "f".into(),
                calls: EgressAssertion::NoneDenied,
            },
            json!({"egress": {"flow": "f", "calls": "none-denied"}}),
        );
        pin(
            &Assertion::Egress {
                flow: "f".into(),
                calls: EgressAssertion::Count(1),
            },
            json!({"egress": {"flow": "f", "calls": {"count": 1}}}),
        );
    }

    #[test]
    fn error_and_run_outcome_wire_forms() {
        pin(
            &Assertion::ErrorClass {
                node_error: NodeErrorKind::InvalidInput,
            },
            json!({"error-class": {"node-error": "invalid-input"}}),
        );
        pin(
            &Assertion::RunOutcome {
                status: RunStatus::Failed,
                fail_kind: Some(FailKind::Terminal),
                fail_node: Some("h".into()),
            },
            json!({"run-outcome": {"status": "failed", "fail-kind": "terminal", "fail-node": "h"}}),
        );
        // The don't-care shape: only a status.
        pin(
            &Assertion::RunOutcome {
                status: RunStatus::Completed,
                fail_kind: None,
                fail_node: None,
            },
            json!({"run-outcome": {"status": "completed"}}),
        );
    }
}
