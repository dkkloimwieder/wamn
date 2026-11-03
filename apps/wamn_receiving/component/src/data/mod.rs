//! Typed Receiving accessors over the frozen `wamn:postgres` capability.
//!
//! Runtime access uses content-addressed [`wamn_postgres_statements`] accessors and the generated
//! Wamn projections. The conformance verifier consumes native sibling
//! projections and the same physical SQL files; this crate authors no SQL.
//! The generated operations are generated whole, in [`generated`].

mod error;
pub mod read;
pub mod record_receipt;
mod statements;

pub use error::{AccessError, AccessErrorType};

/// The generated operations, one module for each model, and their refusal.
#[expect(
    unused_imports,
    reason = "generated data exposes row types for every declared operation"
)]
#[expect(
    clippy::enum_variant_names,
    reason = "generated refusal names match the closed wire vocabulary"
)]
#[expect(
    clippy::option_option,
    reason = "generated updates distinguish an omitted field from an explicit null"
)]
#[expect(
    clippy::struct_field_names,
    reason = "generated row fields retain their authored projection names"
)]
pub mod generated {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/wamn_receiving/data/mod.rs"
    ));
}
