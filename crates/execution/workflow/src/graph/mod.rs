//! The private graph walk and its invocation outcomes.
mod outcome;
mod retry;
mod terminal;
mod walk;
mod wiring;

pub(crate) use outcome::{ErrorDetail, NodeError, NodeOutcome, RateLimitDetail};
pub use terminal::Verdict;
#[cfg(test)]
pub(crate) use walk::NodeCall;
pub(crate) use walk::{Delivery, Step};
pub use walk::{Failure, FailureType, WalkStatus};
pub(crate) use wiring::Wiring;

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub status: WalkStatus,
    pub result: serde_json::Value,
    pub failure: Option<Failure>,
    pub hops: u64,
    pub verdict: Option<Verdict>,
}

#[cfg(test)]
#[path = "route_tests.rs"]
mod route_tests;
#[cfg(test)]
#[path = "terminal_tests.rs"]
mod terminal_tests;
#[cfg(test)]
#[path = "walk_tests.rs"]
mod walk_tests;
