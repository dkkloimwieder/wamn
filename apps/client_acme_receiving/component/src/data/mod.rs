//! Typed Acme Receiving overlay operations over the frozen PostgreSQL capability.

mod error;
pub mod operation;
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
pub mod generated {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/client_acme_receiving/data/mod.rs"
    ));
}
