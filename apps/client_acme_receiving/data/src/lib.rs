//! Typed Acme Receiving overlay operations over the frozen PostgreSQL capability.

mod error;
pub mod operation;
mod statements;

pub use error::{AccessError, AccessErrorType};

/// The generated operations, one module for each model, and their refusal.
pub mod generated {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/data/mod.rs"
    ));
}
