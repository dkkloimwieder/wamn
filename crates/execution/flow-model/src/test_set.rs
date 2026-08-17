//! Bounded validation for the `cases` array carried by a flow document.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::expect::Expect;

/// Maximum number of cases one flow document may carry.
pub const MAX_TEST_SET_CASES: usize = 256;

/// One bounded test case: a golden input and the observable it requires.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct TestSetCase {
    pub case_id: String,
    pub input: Value,
    pub expect: Expect,
}

/// Stable classification for a refused `cases` array.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestSetCasesErrorKind {
    EmptyCases,
    CaseCountOverflow,
    EmptyCaseId,
    DuplicateCaseId,
    InvalidExpect,
}

/// Why a `cases` array is refused.
#[derive(Debug)]
pub struct TestSetCasesError {
    kind: TestSetCasesErrorKind,
    detail: Box<str>,
}

impl TestSetCasesError {
    fn new(kind: TestSetCasesErrorKind, detail: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    /// Stable failure classification.
    pub const fn kind(&self) -> TestSetCasesErrorKind {
        self.kind
    }
}

impl fmt::Display for TestSetCasesError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for TestSetCasesError {}

/// Validate every non-vacuous cardinality and expectation bound.
///
/// A caller that requires a test set calls this directly and gets
/// [`TestSetCasesErrorKind::EmptyCases`] for an empty slice. [`crate::Flow`]
/// does not: `cases` is an optional document array whose absent and empty
/// forms are the same bytes, so flow validation applies these bounds only to a
/// flow that carries at least one case.
pub fn validate_cases(cases: &[TestSetCase]) -> Result<(), TestSetCasesError> {
    if cases.is_empty() {
        return Err(TestSetCasesError::new(
            TestSetCasesErrorKind::EmptyCases,
            "flow document cases must not be empty",
        ));
    }
    if cases.len() > MAX_TEST_SET_CASES {
        return Err(TestSetCasesError::new(
            TestSetCasesErrorKind::CaseCountOverflow,
            format!(
                "flow document has {} cases; maximum is {MAX_TEST_SET_CASES}",
                cases.len()
            ),
        ));
    }

    let mut case_ids = BTreeSet::new();
    for case in cases {
        if case.case_id.is_empty() {
            return Err(TestSetCasesError::new(
                TestSetCasesErrorKind::EmptyCaseId,
                "case-id must not be empty",
            ));
        }
        if !case_ids.insert(case.case_id.as_str()) {
            return Err(TestSetCasesError::new(
                TestSetCasesErrorKind::DuplicateCaseId,
                format!("duplicate case-id {:?}", case.case_id),
            ));
        }
        if let Err(error) = case.expect.validate() {
            return Err(TestSetCasesError::new(
                TestSetCasesErrorKind::InvalidExpect,
                format!("case {:?} expect is invalid: {error}", case.case_id),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{MAX_TEST_SET_CASES, TestSetCase, TestSetCasesErrorKind, validate_cases};
    use crate::expect::{Expect, ExpectedOutcome};
    use crate::status::FlowFailureKind;

    fn case(case_id: &str) -> TestSetCase {
        TestSetCase {
            case_id: case_id.to_owned(),
            input: json!({"value": 1}),
            expect: Expect {
                outcome: ExpectedOutcome::Responded,
                status: Some(200),
                body_subset: None,
                failure_code: None,
            },
        }
    }

    fn refusal(cases: &[TestSetCase], expected_kind: TestSetCasesErrorKind) -> String {
        let error = validate_cases(cases).expect_err("cases must be refused");
        assert_eq!(error.kind(), expected_kind);
        error.to_string()
    }

    #[test]
    fn accepts_the_exact_count_boundary() {
        let cases: Vec<TestSetCase> = (0..MAX_TEST_SET_CASES)
            .map(|ordinal| case(&format!("case-{ordinal}")))
            .collect();
        validate_cases(&cases).expect("maximum case count is accepted");
    }

    #[test]
    fn rejects_empty_overflowing_and_duplicate_cardinalities() {
        refusal(&[], TestSetCasesErrorKind::EmptyCases);

        let too_many: Vec<TestSetCase> = (0..=MAX_TEST_SET_CASES)
            .map(|ordinal| case(&format!("case-{ordinal}")))
            .collect();
        let detail = refusal(&too_many, TestSetCasesErrorKind::CaseCountOverflow);
        assert!(detail.contains("257 cases"));
        assert!(detail.contains("maximum is 256"));

        refusal(&[case("")], TestSetCasesErrorKind::EmptyCaseId);
        let duplicate = refusal(
            &[case("same"), case("same")],
            TestSetCasesErrorKind::DuplicateCaseId,
        );
        assert!(duplicate.contains("same"));
    }

    #[test]
    fn rejects_a_semantically_invalid_expect() {
        let mut invalid = case("bad");
        invalid.expect.failure_code = Some(FlowFailureKind::Terminal);
        let detail = refusal(
            std::slice::from_ref(&invalid),
            TestSetCasesErrorKind::InvalidExpect,
        );
        assert!(detail.contains("bad"));
        assert!(detail.contains("forbids failure-code"));
    }

    #[test]
    fn the_case_document_shape_is_closed() {
        for wire in [
            json!({"case-id": "one", "input": {}, "expect": {"outcome": "responded"},
                   "normalize": {}}),
            json!({"case-id": "one", "input": {},
                   "expect": [{"run-terminal-outcome": {"status": "completed"}}]}),
            json!({"case-id": "one", "expect": {"outcome": "responded"}}),
        ] {
            assert!(serde_json::from_value::<TestSetCase>(wire).is_err());
        }
        serde_json::from_value::<TestSetCase>(json!({
            "case-id": "one", "input": {}, "expect": {"outcome": "responded"}
        }))
        .expect("the exact case shape parses");
    }
}
