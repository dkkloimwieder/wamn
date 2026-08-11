//! Pure contracts and evaluation for the MVP publish test gate.
//!
//! Test-set parsing is closed to four assertion families. Effectful callers
//! collect the bounded [`Captured`] facts; this crate only evaluates them.

mod assertion;
mod authoring_report;
mod captured;
mod evaluate;
mod report;
mod status;
mod test_set;

pub use assertion::{
    Assertion, NamedNodeTerminal, RunTerminalOutcome, TerminalRespond, TypedFlowFailure,
};
pub use authoring_report::{
    AuthoringCaseReport, AuthoringExecutionResult, AuthoringReport, AuthoringReportState,
    ExecutionLineage, PendingAuthoringReport, PendingAuthoringReportReason,
};
pub use captured::Captured;
pub use evaluate::{AssertionResult, Outcome, evaluate};
pub use report::{CaseReport, ScenarioRefusal, ScenarioReport};
pub use status::{FlowFailureKind, NodeFailureKind, NodeTerminalStatus, RunTerminalStatus};
pub use test_set::{
    MAX_TEST_SET_BYTES, MAX_TEST_SET_CASES, MAX_TEST_SET_EXPECTATIONS, TEST_SET_SCHEMA_VERSION,
    TestSetCase, TestSetDocument, TestSetDocumentError, TestSetDocumentErrorKind,
};
