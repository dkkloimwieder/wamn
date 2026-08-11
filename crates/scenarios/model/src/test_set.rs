//! Inline, bounded test-set document parsing.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Assertion;

/// Test-set document schema version shipped for MVP.
pub const TEST_SET_SCHEMA_VERSION: &str = "0.1";

/// Maximum exact UTF-8 definition size accepted by `test-set-run` (1 MiB).
pub const MAX_TEST_SET_BYTES: usize = 1024 * 1024;

/// Maximum number of cases in one test set.
pub const MAX_TEST_SET_CASES: usize = 256;

/// Maximum number of expectations in one test case.
pub const MAX_TEST_SET_EXPECTATIONS: usize = 64;

/// A self-describing inline test-set document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct TestSetDocument {
    pub schema_version: String,
    pub cases: Vec<TestSetCase>,
}

/// One bounded test-set case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct TestSetCase {
    pub case_id: String,
    pub input: Value,
    pub expect: Vec<Assertion>,
}

/// Stable classification for a refused inline test-set definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestSetDocumentErrorKind {
    SizeOverflow,
    Parse,
    SchemaVersion,
    EmptyCases,
    CaseCountOverflow,
    EmptyCaseId,
    DuplicateCaseId,
    EmptyExpect,
    ExpectationCountOverflow,
    InvalidAssertion,
}

/// Why an inline test-set definition is refused.
#[derive(Debug)]
pub struct TestSetDocumentError {
    kind: TestSetDocumentErrorKind,
    source: Option<serde_json::Error>,
    detail: Box<str>,
}

impl TestSetDocumentError {
    fn local(kind: TestSetDocumentErrorKind, detail: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            source: None,
            detail: detail.into(),
        }
    }

    fn parse(source: serde_json::Error) -> Self {
        Self {
            kind: TestSetDocumentErrorKind::Parse,
            source: Some(source),
            detail: "test-set document parse failed".into(),
        }
    }

    /// Stable failure classification.
    pub const fn kind(&self) -> TestSetDocumentErrorKind {
        self.kind
    }
}

impl fmt::Display for TestSetDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)?;
        if let Some(source) = &self.source {
            write!(formatter, ": {source}")?;
        }
        Ok(())
    }
}

impl std::error::Error for TestSetDocumentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

impl TestSetDocument {
    /// Parse and validate exact UTF-8 definition text without normalizing it.
    pub fn from_definition(definition: &str) -> Result<Self, TestSetDocumentError> {
        let byte_length = definition.len();
        if byte_length > MAX_TEST_SET_BYTES {
            return Err(TestSetDocumentError::local(
                TestSetDocumentErrorKind::SizeOverflow,
                format!(
                    "test-set definition is {byte_length} bytes; maximum is {MAX_TEST_SET_BYTES} bytes"
                ),
            ));
        }
        let document: Self =
            serde_json::from_str(definition).map_err(TestSetDocumentError::parse)?;
        document.validate()?;
        Ok(document)
    }

    /// Validate schema version and all non-vacuous cardinality bounds.
    pub fn validate(&self) -> Result<(), TestSetDocumentError> {
        if self.schema_version != TEST_SET_SCHEMA_VERSION {
            return Err(TestSetDocumentError::local(
                TestSetDocumentErrorKind::SchemaVersion,
                format!(
                    "unsupported test-set schema-version {:?}; supported version is {TEST_SET_SCHEMA_VERSION:?}",
                    self.schema_version
                ),
            ));
        }
        if self.cases.is_empty() {
            return Err(TestSetDocumentError::local(
                TestSetDocumentErrorKind::EmptyCases,
                "test-set cases must not be empty",
            ));
        }
        if self.cases.len() > MAX_TEST_SET_CASES {
            return Err(TestSetDocumentError::local(
                TestSetDocumentErrorKind::CaseCountOverflow,
                format!(
                    "test-set has {} cases; maximum is {MAX_TEST_SET_CASES}",
                    self.cases.len()
                ),
            ));
        }

        let mut case_ids = BTreeSet::new();
        for case in &self.cases {
            if case.case_id.is_empty() {
                return Err(TestSetDocumentError::local(
                    TestSetDocumentErrorKind::EmptyCaseId,
                    "test-set case-id must not be empty",
                ));
            }
            if !case_ids.insert(case.case_id.as_str()) {
                return Err(TestSetDocumentError::local(
                    TestSetDocumentErrorKind::DuplicateCaseId,
                    format!("duplicate test-set case-id {:?}", case.case_id),
                ));
            }
            if case.expect.is_empty() {
                return Err(TestSetDocumentError::local(
                    TestSetDocumentErrorKind::EmptyExpect,
                    format!("test-set case {:?} expect must not be empty", case.case_id),
                ));
            }
            if case.expect.len() > MAX_TEST_SET_EXPECTATIONS {
                return Err(TestSetDocumentError::local(
                    TestSetDocumentErrorKind::ExpectationCountOverflow,
                    format!(
                        "test-set case {:?} has {} expectations; maximum is {MAX_TEST_SET_EXPECTATIONS}",
                        case.case_id,
                        case.expect.len()
                    ),
                ));
            }
            for (index, assertion) in case.expect.iter().enumerate() {
                if let Err(error) = assertion.validate() {
                    return Err(TestSetDocumentError::local(
                        TestSetDocumentErrorKind::InvalidAssertion,
                        format!(
                            "test-set case {:?} assertion {index} is invalid: {error}",
                            case.case_id
                        ),
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        MAX_TEST_SET_BYTES, MAX_TEST_SET_CASES, MAX_TEST_SET_EXPECTATIONS, TestSetDocument,
        TestSetDocumentError, TestSetDocumentErrorKind,
    };

    fn case(case_id: impl Into<String>, expectation_count: usize) -> Value {
        json!({
            "case-id": case_id.into(),
            "input": {"value": 1},
            "expect": vec![json!({"run-terminal-outcome": {"status": "completed"}}); expectation_count],
        })
    }

    fn definition(cases: Vec<Value>) -> String {
        json!({"schema-version": "0.1", "cases": cases}).to_string()
    }

    fn refusal(definition: &str, expected_kind: TestSetDocumentErrorKind) -> TestSetDocumentError {
        let error = TestSetDocument::from_definition(definition)
            .expect_err("test-set definition must be refused");
        assert_eq!(error.kind(), expected_kind);
        error
    }

    #[test]
    fn accepts_exact_count_and_size_boundaries() {
        let cases = (0..MAX_TEST_SET_CASES)
            .map(|ordinal| case(format!("case-{ordinal}"), 1))
            .collect();
        TestSetDocument::from_definition(&definition(cases))
            .expect("maximum case count is accepted");

        TestSetDocument::from_definition(&definition(vec![case(
            "max-expect",
            MAX_TEST_SET_EXPECTATIONS,
        )]))
        .expect("maximum expectation count is accepted");

        let mut exact_size = definition(vec![case("max-bytes", 1)]);
        exact_size.extend(std::iter::repeat_n(
            ' ',
            MAX_TEST_SET_BYTES - exact_size.len(),
        ));
        assert_eq!(exact_size.len(), MAX_TEST_SET_BYTES);
        TestSetDocument::from_definition(&exact_size).expect("maximum byte size is accepted");
    }

    #[test]
    fn rejects_empty_and_overflowing_cardinalities() {
        refusal(
            &definition(Vec::new()),
            TestSetDocumentErrorKind::EmptyCases,
        );
        let empty_expect = refusal(
            &definition(vec![case("empty-expect", 0)]),
            TestSetDocumentErrorKind::EmptyExpect,
        );
        assert!(empty_expect.to_string().contains("empty-expect"));

        let too_many_cases = (0..=MAX_TEST_SET_CASES)
            .map(|ordinal| case(format!("case-{ordinal}"), 1))
            .collect();
        let case_overflow = refusal(
            &definition(too_many_cases),
            TestSetDocumentErrorKind::CaseCountOverflow,
        );
        let detail = case_overflow.to_string();
        assert!(detail.contains("257 cases"));
        assert!(detail.contains("maximum is 256"));

        let expectation_overflow = refusal(
            &definition(vec![case(
                "too-many-expectations",
                MAX_TEST_SET_EXPECTATIONS + 1,
            )]),
            TestSetDocumentErrorKind::ExpectationCountOverflow,
        );
        let detail = expectation_overflow.to_string();
        assert!(detail.contains("too-many-expectations"));
        assert!(detail.contains("65 expectations"));
        assert!(detail.contains("maximum is 64"));

        let exact_size = {
            let mut value = definition(vec![case("max-bytes", 1)]);
            value.extend(std::iter::repeat_n(' ', MAX_TEST_SET_BYTES - value.len()));
            value
        };
        let size_overflow = refusal(
            &format!("{exact_size} "),
            TestSetDocumentErrorKind::SizeOverflow,
        );
        let detail = size_overflow.to_string();
        assert!(detail.contains("1048577 bytes"));
        assert!(detail.contains("maximum is 1048576 bytes"));
    }

    #[test]
    fn rejects_malformed_versions_and_document_draft_selectors() {
        let wrong_version = json!({"schema-version": "0.2", "cases": [case("one", 1)]}).to_string();
        let version_error = refusal(&wrong_version, TestSetDocumentErrorKind::SchemaVersion);
        assert!(version_error.to_string().contains("schema-version \"0.2\""));

        let parse_error = refusal(
            &json!({"cases": [case("one", 1)]}).to_string(),
            TestSetDocumentErrorKind::Parse,
        );
        assert!(std::error::Error::source(&parse_error).is_some());

        for selector in ["draft", "draft-id", "draft-revision", "validated-draft"] {
            let mut document = json!({"schema-version": "0.1", "cases": [case("one", 1)]});
            document
                .as_object_mut()
                .expect("test document is an object")
                .insert(selector.into(), json!("forbidden"));
            let error = refusal(&document.to_string(), TestSetDocumentErrorKind::Parse);
            assert!(
                std::error::Error::source(&error).is_some(),
                "selector {selector:?} parse source must be retained"
            );
        }
    }

    #[test]
    fn refuses_unknown_removed_and_semantically_invalid_assertions() {
        let refused = [
            json!({"equals": {"value": 1}}),
            json!({"subset": {"value": 1}}),
            json!({"path-equals": {"pointer": "/x", "value": 1}}),
            json!({"port": "main"}),
            json!({"db-state": {"query": "select 1", "expect": "empty"}}),
            json!({"egress": {"flow": "flow", "calls": "none-denied"}}),
            json!({"error-class": {"node-error": "terminal"}}),
            json!({"run-outcome": {"status": "completed"}}),
            json!({"unknown": {}}),
            json!({"typed-flow-failure": {"kind": "cancelled"}}),
            json!({"named-node-terminal": {
                "flow-id": "orders",
                "node-id": "write",
                "status": "error",
                "failure-kind": "cancelled"
            }}),
            json!({"named-node-terminal": {
                "path": ["write"],
                "status": "success"
            }}),
        ];
        for assertion in refused {
            let error = refusal(
                &definition(vec![json!({
                    "case-id": "refused",
                    "input": {},
                    "expect": [assertion]
                })]),
                TestSetDocumentErrorKind::Parse,
            );
            assert!(std::error::Error::source(&error).is_some());
        }

        for assertion in [
            json!({"terminal-respond": {"status": 99, "body": null}}),
            json!({"terminal-respond": {"status": 600, "body": null}}),
            json!({"named-node-terminal": {
                "flow-id": "orders",
                "node-id": "write",
                "status": "success",
                "failure-kind": "terminal"
            }}),
            json!({"named-node-terminal": {
                "flow-id": "orders", "node-id": "write", "status": "error"
            }}),
        ] {
            refusal(
                &definition(vec![json!({
                    "case-id": "invalid",
                    "input": {},
                    "expect": [assertion]
                })]),
                TestSetDocumentErrorKind::InvalidAssertion,
            );
        }
    }

    #[test]
    fn named_node_success_refuses_present_null_failure_kind() {
        let omitted = definition(vec![json!({
            "case-id": "success",
            "input": {},
            "expect": [{"named-node-terminal": {
                "flow-id": "orders",
                "node-id": "write",
                "status": "success"
            }}]
        })]);
        TestSetDocument::from_definition(&omitted)
            .expect("success accepts an omitted failure-kind field");

        let present_null = definition(vec![json!({
            "case-id": "success",
            "input": {},
            "expect": [{"named-node-terminal": {
                "flow-id": "orders",
                "node-id": "write",
                "status": "success",
                "failure-kind": null
            }}]
        })]);
        let error = refusal(&present_null, TestSetDocumentErrorKind::Parse);
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn assertion_payloads_and_cases_are_closed() {
        for value in [
            json!({
                "schema-version": "0.1",
                "cases": [{
                    "case-id": "case",
                    "input": {},
                    "expect": [{"terminal-respond": {
                        "status": 200,
                        "body": {},
                        "headers": {}
                    }}]
                }]
            }),
            json!({
                "schema-version": "0.1",
                "cases": [{
                    "case-id": "case",
                    "input": {},
                    "expect": [{"run-terminal-outcome": {"status": "completed"}}],
                    "normalize": {}
                }]
            }),
        ] {
            refusal(&value.to_string(), TestSetDocumentErrorKind::Parse);
        }
    }
}
