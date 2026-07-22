//! The flow test-suite envelope (11.2): a flow's test cases as **catalog data**,
//! versioned WITH the flow they test.
//!
//! A [`TestSuite`] pins a concrete `(flow_id, flow_version)` and carries an
//! ordered list of [`CaseEntry`]s. It is the import/export shape over the
//! `deploy/sql/flow-tests.sql` rows (`wamn_run.test_suites` +
//! `wamn_run.test_cases`): a suite and its flow version promote together through
//! the copy-project-env definition path, and the FK to `wamn_run.flows` ON
//! DELETE CASCADE makes that binding structural.
//!
//! **Purity + the v0 seam.** This crate is pure (serde + serde_json only): it
//! validates the ENVELOPE (ids, ordinals, the schema-version discriminator), not
//! the case body. The case body is an opaque [`serde_json::Value`] in v0 — the
//! canonical case/assertion vocabulary is a sibling crate (`wamn-testkit`); at
//! integration [`TestSuite`] gains a validate-on-write pass that parses each
//! `case` against those serde types. Until then a body is any well-formed JSON.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The suite-envelope **format** version this crate implements (mirrors
/// `wamn_catalog::SCHEMA_VERSION` / the flow-schema freeze): `0.1.x` is
/// additive/clarifying only; a breaking change waits for `0.2`.
pub const SCHEMA_VERSION: &str = "0.1";

/// A versioned suite of test cases for one flow version. Round-trips to/from the
/// `wamn_run.test_suites` + `wamn_run.test_cases` rows; `cases` maps one-to-one
/// onto `test_cases`, in `ordinal` order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestSuite {
    /// The envelope format version — must equal [`SCHEMA_VERSION`].
    #[serde(rename = "schema-version")]
    pub schema_version: String,
    /// The flow this suite tests (the `flows.flow_id` it pins).
    #[serde(rename = "flow-id")]
    pub flow_id: String,
    /// The flow VERSION this suite is bound to (the `flows.version` it pins). A
    /// suite tests one concrete version — there is no "active suite" pointer.
    #[serde(rename = "flow-version")]
    pub flow_version: u32,
    /// The suite id, unique within `(tenant, flow_id, flow_version)`.
    #[serde(rename = "suite-id")]
    pub suite_id: String,
    /// A human label for the suite.
    pub name: String,
    /// The cases, one per `test_cases` row.
    pub cases: Vec<CaseEntry>,
}

/// One test case: its id, its position in the suite, and the opaque case body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseEntry {
    /// The case id, unique within the suite.
    #[serde(rename = "case-id")]
    pub case_id: String,
    /// The case's position in the suite. Ordinals are unique across the suite
    /// (two cases cannot claim the same slot).
    pub ordinal: u32,
    /// The opaque case body (v0). At integration this is parsed against the
    /// `wamn-testkit` case/assertion vocabulary; here it is any valid JSON.
    pub case: Value,
}

/// Why a suite envelope is malformed. Enum (the repo's WIT-mirroring house
/// style) so the driver can name the exact defect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestSuiteError {
    /// The JSON did not parse into the envelope shape.
    Parse(String),
    /// `schema-version` was not [`SCHEMA_VERSION`].
    SchemaVersion { found: String },
    /// A required id (`flow-id`, `suite-id`, or a `case-id`) was empty.
    EmptyId { field: &'static str },
    /// Two cases share a `case-id`.
    DuplicateCaseId { case_id: String },
    /// Two cases share an `ordinal`.
    DuplicateOrdinal { ordinal: u32 },
}

impl std::fmt::Display for TestSuiteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TestSuiteError::Parse(e) => write!(f, "test suite parse: {e}"),
            TestSuiteError::SchemaVersion { found } => write!(
                f,
                "unsupported test-suite schema-version {found:?} (this build implements {SCHEMA_VERSION:?})"
            ),
            TestSuiteError::EmptyId { field } => write!(f, "empty {field}"),
            TestSuiteError::DuplicateCaseId { case_id } => {
                write!(f, "duplicate case-id {case_id:?}")
            }
            TestSuiteError::DuplicateOrdinal { ordinal } => {
                write!(f, "duplicate ordinal {ordinal}")
            }
        }
    }
}

impl std::error::Error for TestSuiteError {}

impl TestSuite {
    /// Parse + validate a suite envelope from JSON.
    pub fn from_json(json: &str) -> Result<TestSuite, TestSuiteError> {
        let suite: TestSuite =
            serde_json::from_str(json).map_err(|e| TestSuiteError::Parse(e.to_string()))?;
        suite.validate()?;
        Ok(suite)
    }

    /// The canonical JSON of this suite (round-trips through [`TestSuite::from_json`]).
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("TestSuite serializes")
    }

    /// Validate the envelope: the schema-version discriminator, non-empty ids,
    /// unique case ids, and coherent (unique) ordinals. The case BODY is NOT
    /// validated here (v0 opaque seam).
    pub fn validate(&self) -> Result<(), TestSuiteError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(TestSuiteError::SchemaVersion {
                found: self.schema_version.clone(),
            });
        }
        if self.flow_id.is_empty() {
            return Err(TestSuiteError::EmptyId { field: "flow-id" });
        }
        if self.suite_id.is_empty() {
            return Err(TestSuiteError::EmptyId { field: "suite-id" });
        }
        let mut seen_ids: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        let mut seen_ordinals: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
        for case in &self.cases {
            if case.case_id.is_empty() {
                return Err(TestSuiteError::EmptyId { field: "case-id" });
            }
            if !seen_ids.insert(case.case_id.as_str()) {
                return Err(TestSuiteError::DuplicateCaseId {
                    case_id: case.case_id.clone(),
                });
            }
            if !seen_ordinals.insert(case.ordinal) {
                return Err(TestSuiteError::DuplicateOrdinal {
                    ordinal: case.ordinal,
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn suite_json(cases: Value) -> String {
        json!({
            "schema-version": "0.1",
            "flow-id": "escalate-holds",
            "flow-version": 1,
            "suite-id": "smoke",
            "name": "smoke suite",
            "cases": cases,
        })
        .to_string()
    }

    #[test]
    fn round_trips_through_json() {
        let src = suite_json(json!([
            { "case-id": "c1", "ordinal": 0, "case": { "input": { "x": 1 }, "expect": "ok" } },
            { "case-id": "c2", "ordinal": 1, "case": [1, 2, 3] },
        ]));
        let suite = TestSuite::from_json(&src).expect("valid suite parses");
        assert_eq!(suite.flow_id, "escalate-holds");
        assert_eq!(suite.flow_version, 1);
        assert_eq!(suite.cases.len(), 2);
        // The opaque body survives verbatim.
        assert_eq!(suite.cases[0].case["expect"], json!("ok"));
        // from_json(to_json(x)) == x.
        let back = TestSuite::from_json(&suite.to_json()).expect("re-parse");
        assert_eq!(back, suite);
    }

    #[test]
    fn rejects_a_foreign_schema_version() {
        let src = json!({
            "schema-version": "0.2",
            "flow-id": "f", "flow-version": 1, "suite-id": "s", "name": "n", "cases": [],
        })
        .to_string();
        assert!(matches!(
            TestSuite::from_json(&src),
            Err(TestSuiteError::SchemaVersion { .. })
        ));
    }

    #[test]
    fn rejects_empty_ids() {
        let empty_flow = json!({
            "schema-version": "0.1",
            "flow-id": "", "flow-version": 1, "suite-id": "s", "name": "n", "cases": [],
        })
        .to_string();
        assert!(matches!(
            TestSuite::from_json(&empty_flow),
            Err(TestSuiteError::EmptyId { field: "flow-id" })
        ));
        let empty_case = suite_json(json!([{ "case-id": "", "ordinal": 0, "case": {} }]));
        assert!(matches!(
            TestSuite::from_json(&empty_case),
            Err(TestSuiteError::EmptyId { field: "case-id" })
        ));
    }

    /// Mutation guard: a validate that skipped the case-id uniqueness check
    /// would accept two cases claiming the same id. It MUST reject.
    #[test]
    fn rejects_duplicate_case_ids() {
        let src = suite_json(json!([
            { "case-id": "dup", "ordinal": 0, "case": {} },
            { "case-id": "dup", "ordinal": 1, "case": {} },
        ]));
        assert!(matches!(
            TestSuite::from_json(&src),
            Err(TestSuiteError::DuplicateCaseId { .. })
        ));
    }

    /// Mutation guard: two cases at the same ordinal are incoherent (they claim
    /// one slot); validate MUST reject.
    #[test]
    fn rejects_duplicate_ordinals() {
        let src = suite_json(json!([
            { "case-id": "c1", "ordinal": 3, "case": {} },
            { "case-id": "c2", "ordinal": 3, "case": {} },
        ]));
        assert!(matches!(
            TestSuite::from_json(&src),
            Err(TestSuiteError::DuplicateOrdinal { ordinal: 3 })
        ));
    }

    #[test]
    fn empty_suite_is_valid() {
        let suite = TestSuite::from_json(&suite_json(json!([]))).expect("empty suite is valid");
        assert!(suite.cases.is_empty());
    }
}
