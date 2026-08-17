//! Pure contracts and evaluation for the MVP publish test gate.
//!
//! MVP outcome: publish gate.
//!
//! A test case is one golden input and one flat expected observable. Effectful
//! callers collect the bounded [`Captured`] facts; this crate only evaluates
//! them.

mod captured;
mod evaluate;
mod expect;
mod status;
mod test_set;

pub use captured::{Captured, CapturedResponse};
pub use evaluate::{Outcome, evaluate};
pub use expect::{Expect, ExpectedOutcome};
pub use status::FlowFailureKind;
pub use test_set::{
    MAX_TEST_SET_CASES, TestSetCase, TestSetCasesError, TestSetCasesErrorKind, validate_cases,
};
