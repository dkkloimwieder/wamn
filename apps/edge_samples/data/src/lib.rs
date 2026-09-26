//! Typed edge sample accessors over the frozen `wamn:postgres` capability.
//!
//! Runtime access uses the generated statement accessors of the package, and
//! this crate authors no SQL. It covers the two operations that
//! `apps/edge_samples/wamn.json` declares: `sample.get` and the command
//! `sample.record`, which an edge box calls once for each sample it forwards.

mod error;
mod generated;
pub mod sample;
mod scalar;

pub use error::{AccessError, AccessErrorKind};
