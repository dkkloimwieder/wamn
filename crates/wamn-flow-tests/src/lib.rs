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
//! **Purity + validate-on-write.** This crate is pure (serde only): it validates
//! the ENVELOPE (ids, ordinals, the schema-version discriminator) AND, since the
//! 828 reconcile, each case BODY against the canonical case/assertion vocabulary
//! ([`wamn_testkit::TestCase`]). [`TestSuite::validate`] runs the envelope checks
//! first, then a validate-on-write pass that parses every `case` as a `TestCase`
//! — a malformed body is rejected, naming the offending `case-id`. The body is
//! still STORED as an opaque [`serde_json::Value`] (round-trips verbatim);
//! validation only gates writes.

pub mod sql;

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
    /// The opaque case body. STORED verbatim (round-trips as-is), but parsed
    /// against the [`wamn_testkit::TestCase`] vocabulary by [`TestSuite::validate`]
    /// (validate-on-write) — a malformed body is rejected on write.
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
    /// A case body did not parse against the canonical case/assertion vocabulary
    /// ([`wamn_testkit::TestCase`]) — the validate-on-write pass. Names the
    /// offending `case-id` and the serde detail.
    CaseBody { case_id: String, error: String },
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
            TestSuiteError::CaseBody { case_id, error } => {
                write!(f, "case {case_id:?} body is not a valid test case: {error}")
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
    /// unique case ids, and coherent (unique) ordinals. THEN a validate-on-write
    /// pass (828 reconcile) parses each opaque case BODY against the canonical
    /// [`wamn_testkit::TestCase`] vocabulary. Envelope checks run FIRST, so a
    /// structural defect (empty/duplicate id, duplicate ordinal) is reported
    /// before any body defect.
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
        // Validate-on-write: each opaque body must additionally parse against the
        // canonical case/assertion vocabulary. A SEPARATE pass so an envelope
        // defect above is reported before a body defect here.
        for case in &self.cases {
            if let Err(e) = serde_json::from_value::<wamn_testkit::TestCase>(case.case.clone()) {
                return Err(TestSuiteError::CaseBody {
                    case_id: case.case_id.clone(),
                    error: e.to_string(),
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
            { "case-id": "c1", "ordinal": 0,
              "case": { "name": "c1", "node-ref": {}, "input": { "x": 1 },
                        "expect": [ { "subset": { "recommended": "reject" } } ] } },
            { "case-id": "c2", "ordinal": 1,
              "case": { "name": "c2", "flow-ref": { "flow-id": "escalate-holds", "version": 1 },
                        "input": [1, 2, 3], "expect": [ { "run-outcome": { "status": "completed" } } ] } },
        ]));
        let suite = TestSuite::from_json(&src).expect("valid suite parses");
        assert_eq!(suite.flow_id, "escalate-holds");
        assert_eq!(suite.flow_version, 1);
        assert_eq!(suite.cases.len(), 2);
        // The opaque body survives verbatim (stored as-is, not normalized).
        assert_eq!(suite.cases[0].case["input"], json!({ "x": 1 }));
        // from_json(to_json(x)) == x.
        let back = TestSuite::from_json(&suite.to_json()).expect("re-parse");
        assert_eq!(back, suite);
    }

    /// Validate-on-write (828 reconcile): a case body that is not a valid
    /// `wamn-testkit` TestCase is rejected, naming the offending case-id.
    #[test]
    fn rejects_an_invalid_case_body() {
        // A body missing the required `input`/`expect` is not a TestCase.
        let src = suite_json(json!([
            { "case-id": "bad", "ordinal": 0, "case": { "nope": 1 } },
        ]));
        assert!(matches!(
            TestSuite::from_json(&src),
            Err(TestSuiteError::CaseBody { case_id, .. }) if case_id == "bad"
        ));
    }

    /// A valid `wamn-testkit` TestCase body passes validate-on-write.
    #[test]
    fn accepts_a_valid_testkit_case_body() {
        let src = suite_json(json!([
            { "case-id": "ok", "ordinal": 0,
              "case": { "name": "ok", "node-ref": {}, "input": {},
                        "expect": [ { "error-class": { "node-error": "invalid-input" } } ] } },
        ]));
        assert!(TestSuite::from_json(&src).is_ok());
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
