//! Typed Receiving accessors over the frozen `wamn:postgres` capability.
//!
//! Runtime access uses content-addressed [`wamn_postgres_statements`] accessors and the generated
//! Wamn projections. The conformance verifier consumes native sibling
//! projections and the same physical SQL files; this crate authors no SQL.

mod cursor;
mod error;
mod generated;
pub mod purchase_order;
pub mod read;
pub mod receipt;
pub mod record_receipt;
pub mod supplier;

pub use error::{AccessError, AccessErrorKind};
