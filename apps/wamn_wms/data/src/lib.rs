//! Typed WMS accessors over the frozen `wamn:postgres` capability.
//!
//! Runtime access uses content-addressed [`wamn_postgres_statements`]
//! accessors and the generated Wamn projections; this crate authors no SQL.
//!
//! Seven operations: the four commands (`inventory.move`, the contended one
//! the composed-wiring gate runs; `adjust`, `merge` and `split`), the
//! `inventory.aggregate` projection, and the `pallet` model reads. Every
//! command follows the two laws the authored SQL already obeys: identity
//! comes from the claim, never from the work, and more than one row of a
//! table is locked in the order the database shares.

mod cursor;
mod error;
mod generated;
mod inventory_adjust;
mod inventory_aggregate;
mod inventory_merge;
pub mod inventory_move;
mod inventory_split;
pub mod operation;
mod pallet;
mod scalar;

pub use error::{AccessError, AccessErrorKind};
