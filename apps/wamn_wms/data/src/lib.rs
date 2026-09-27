//! Typed WMS accessors over the frozen `wamn:postgres` capability.
//!
//! Runtime access uses content-addressed [`wamn_postgres_statements`]
//! accessors and the generated Wamn projections; this crate authors no SQL.
//!
//! Twenty operations: the four commands (`inventory.move`, the contended one,
//! then `adjust`, `merge` and `split`), the
//! `inventory.aggregate` projection, and the generated model operations. Every
//! command runs in the transaction its generated codec holds for the write
//! log, takes each new id from its insert's `RETURNING`, and locks more than
//! one row of a table in the order the database shares.

mod cursor;
mod error;
mod generated;
pub mod inventory_adjust;
pub mod inventory_aggregate;
pub mod inventory_merge;
pub mod inventory_move;
pub mod inventory_split;
pub mod inventory_transaction;
pub mod location;
pub mod packaging;
pub mod packaging_quantity;
mod page;
pub mod product;
mod scalar;

pub use error::{AccessError, AccessErrorKind};
pub use page::Page;
