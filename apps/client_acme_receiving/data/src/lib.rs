//! Typed Acme Receiving overlay operations over the frozen PostgreSQL capability.

mod error;
mod generated;
pub mod operation;

pub use error::{AccessError, AccessErrorKind};
