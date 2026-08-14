//! Pure contracts and evaluation for the MVP publish test gate.
//!
//! Test-set parsing is closed to four assertion families. Effectful callers
//! collect the bounded [`Captured`] facts; this crate only evaluates them.

mod assertion;
mod captured;
mod evaluate;
mod status;
mod test_set;

pub use assertion::{
    Assertion, NamedNodeTerminal, RunTerminalOutcome, TerminalRespond, TypedFlowFailure,
};
pub use captured::Captured;
pub use evaluate::{AssertionResult, Outcome, evaluate};
pub use status::{FlowFailureKind, NodeFailureKind, NodeTerminalStatus, RunTerminalStatus};
pub use test_set::{
    MAX_TEST_SET_BYTES, MAX_TEST_SET_CASES, MAX_TEST_SET_EXPECTATIONS, TEST_SET_SCHEMA_VERSION,
    TestSetCase, TestSetDocument, TestSetDocumentError, TestSetDocumentErrorKind,
};
