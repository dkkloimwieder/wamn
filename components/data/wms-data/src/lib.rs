//! Typed WMS accessors over the frozen `wamn:postgres` capability.
//!
//! Runtime access uses content-addressed [`wamn_postgres_statements`]
//! accessors and the generated Wamn projections; this crate authors no SQL.
//!
//! It implements ONE operation — `inventory.move`, the contended command the
//! composed-wiring gate runs. The remaining six WMS operations are follow-on
//! work: the gate asks for one wiring and one contention proof, and a guest
//! implementing everything would be six operations of scope spent before the
//! thesis it exists to prove is demonstrated once.

mod error;
mod generated;
pub mod inventory_move;
pub mod operation;

pub use error::{AccessError, AccessErrorKind};
