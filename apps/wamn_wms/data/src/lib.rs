//! Typed WMS accessors over the frozen `wamn:postgres` capability.
//!
//! Runtime access uses content-addressed [`wamn_postgres_statements`]
//! accessors and the generated Wamn projections; this crate authors no SQL.
//!
//! Twenty operations: the four commands (`inventory.move`, the contended one,
//! then `adjust`, `merge` and `split`), the
//! `inventory.aggregate` projection, and the generated model operations in
//! [`generated`], which are generated whole. Every
//! command runs in the transaction its generated codec holds for the write
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

pub use error::{AccessError, AccessErrorKind};

/// The generated operations, one module for each model, and their refusal.
pub mod generated {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/data/mod.rs"
    ));
}
