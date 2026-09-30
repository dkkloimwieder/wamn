//! Typed Receiving accessors over the frozen `wamn:postgres` capability.
//!
//! Runtime access uses content-addressed [`wamn_postgres_statements`] accessors and the generated
//! Wamn projections. The conformance verifier consumes native sibling
//! projections and the same physical SQL files; this crate authors no SQL.
//! The generated operations are generated whole, in [`generated`].

mod cursor;
mod error;
pub mod read;
pub mod record_receipt;
mod statements;

pub use error::{AccessError, AccessErrorType};

/// The generated operations, one module for each model, and their refusal.
pub mod generated {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/data/mod.rs"
    ));
}
