//! Typed WMS accessors over the frozen `wamn:postgres` capability.
//!
//! Runtime access uses content-addressed [`wamn_postgres_statements`]
//! accessors and the generated Wamn projections; this crate authors no SQL.
//!
//! Twenty operations: the four commands (`inventory.move`, the contended one,
//! then `adjust`, `merge` and `split`), the
//! `inventory.aggregate` projection, and the generated model operations in
//! [`generated`], which are generated whole. Every
//! command runs in the transaction its generated operation holds for the write
//! log, takes each new id from its insert's `RETURNING`, and locks more than
//! one row of a table in the order the database shares.

mod error;
pub mod inventory_adjust;
pub mod inventory_aggregate;
pub mod inventory_merge;
pub mod inventory_move;
pub mod inventory_split;
mod scalar;
mod statements;

pub use error::AccessError;

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
        "/../../target/wamn/wamn_wms/data/mod.rs"
    ));
}
