//! wamn event registration model (EVT-REG, D19 v3 §5).
//!
//! The **declaration surface** for the event plane's materializer (l5i9.17): an
//! [`EventRegistration`] is a subscribing flow's "a registration, not code" —
//! an entity id, an op set, an optional condition, and an optional partition-key
//! expression. This crate models and validates that declaration; it is pure Rust
//! (no DB, no clock, no wasm) and does not decode WAL, evaluate conditions, or
//! enqueue runs — the materializer consumes what this crate stores.
//!
//! Registrations are stored as jsonb in `catalog.event_registrations`
//! (deploy/sql/catalog-schema.sql), managed through the minimal CRUD surface in
//! [`wamn_api::registration`]. Rename-proof by entity-id keying (EVT-OIDMAP,
//! wamn-l5i9.11); 11.8 impact analysis (wamn-wvb) covers registrations via the
//! `entity_id` storage column.

mod model;
mod oldref;
mod validate;

pub use model::{EventRegistration, RegistrationState, SCHEMA_VERSION};
pub use oldref::{condition_references_old, references_old};
pub use validate::validate;

// Re-exported so a consumer names the op set through this one crate; it is the
// same [`Op`] the CDC envelope carries (`wamn_event_wire`).
pub use wamn_event_wire::Op;
