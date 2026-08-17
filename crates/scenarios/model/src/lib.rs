//! Pure evaluation of the MVP publish test gate.
//!
//! MVP outcome: publish gate.
//!
//! A test case is one golden input and one flat expected observable, both
//! carried by the flow document ([`wamn_flow::TestSetCase`]). Effectful callers
//! collect the bounded [`Captured`] facts; this crate only evaluates them.

mod captured;
mod evaluate;

pub use captured::{Captured, CapturedResponse};
pub use evaluate::{Outcome, evaluate};
