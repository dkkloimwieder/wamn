//! Typed platform fixture accessors over the frozen `wamn:postgres` capability.
//!
//! Runtime access uses the generated statement accessors of the fixture
//! package, and this crate authors no SQL. The generated operations are
//! generated whole, in [`generated`]. The custom operations are authored here.

mod error;
mod scalar;
mod statements;
pub mod widget;
pub mod widget_maker;

pub use error::{AccessError, AccessErrorKind};

/// The generated operations, one module for each model, and their refusal.
pub mod generated {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/data/mod.rs"
    ));
}
