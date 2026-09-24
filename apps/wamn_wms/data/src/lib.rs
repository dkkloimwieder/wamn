//! Typed WMS accessors over the frozen `wamn:postgres` capability.
//!
//! Runtime access uses content-addressed [`wamn_postgres_statements`]
//! accessors and the generated Wamn projections; this crate authors no SQL.
//!
//! Twenty operations: the four commands (`inventory.move`, the contended one,
//! then `adjust`, `merge` and `split`), the
//! `inventory.aggregate` projection, and the generated model operations. Every
//! command follows the two laws the authored SQL already obeys: identity
//! comes from the claim, never from the work, and more than one row of a
//! table is locked in the order the database shares.

mod cursor;
mod error;
mod generated;
pub mod inventory_adjust;
pub mod inventory_aggregate;
pub mod inventory_merge;
pub mod inventory_move;
pub mod inventory_movement;
pub mod inventory_split;
pub mod location;
mod page;
pub mod pallet;
pub mod pallet_quantity;
pub mod product;
mod scalar;

pub use error::{AccessError, AccessErrorKind};
pub use page::Page;
