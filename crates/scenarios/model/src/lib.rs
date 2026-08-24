//! Pure evaluation of the MVP publish test gate.
//!
//! MVP outcome: publish gate.
//!
//! A test case is one golden input and one flat expected observable, both
//! carried by the flow document ([`wamn_execution_contract::TestSetCase`]). Effectful callers
//! collect the bounded [`Captured`] facts; this crate only evaluates them.
//!
//! No such caller exists yet. The producer that lands is effectful in both
//! directions — it reads the terminal `wamn_run.runs` columns of a case's run
//! and finalizes the [`Outcome`] against that case's durable mapping — so it
//! belongs to the service that owns the whole report lifecycle,
//! `wamn-scenario-worker`. [`Captured::responded`], [`Captured::failed`], and
//! [`Captured::validate`] are the whole of the contract it owes this crate
//! (wamn-0h0g.15.77).

mod captured;
mod evaluate;

pub use captured::{Captured, CapturedError, CapturedResponse};
pub use evaluate::{Outcome, evaluate};
