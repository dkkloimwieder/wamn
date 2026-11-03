//! Typed edge sample accessors over the frozen `wamn:postgres` capability.
//!
//! Runtime access uses the generated statement accessors of the package, and
//! this crate authors no SQL. The generated operation `sample.get` is
//! generated whole, in [`generated`]. The command `sample.record`, which an
//! edge box calls once for each sample it forwards, is authored here.

mod error;
pub mod sample;
mod scalar;
mod statements;

pub use error::AccessError;

/// The generated operations, one module for each model, and their refusal.
#[expect(
    clippy::enum_variant_names,
    reason = "generated refusal names match the closed wire vocabulary"
)]
pub mod generated {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/edge_samples/data/mod.rs"
    ));
}
