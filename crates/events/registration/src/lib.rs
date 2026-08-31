//! wamn event registration model (EVT-REG, D19 v3 §5).
//!
//! MVP outcome: event spine (causation depth = loop guard).
//!
//! The **declaration surface** for the event plane's materializer (l5i9.17): an
//! [`EventRegistration`] is a subscribing flow's "a registration, not code" —
//! an entity id, an op set, event-or-batch delivery grain, and an optional
//! condition. This crate models and validates that declaration; it is pure Rust
//! (no DB, no clock, no wasm) and does not decode WAL or evaluate conditions —
//! the materializer consumes what this crate stores.
//!
//! Registrations are stored as jsonb in `catalog.event_registrations`
//! (deploy/sql/catalog-schema.sql), managed through the minimal CRUD surface in
//! [`wamn_api::registration`]. The entity is the package-local model key;
//! `wamn.json` is the only model-key to schema/table mapping.

mod model;
mod oldref;
mod validate;

pub use model::{EventRegistration, RegistrationInput, SCHEMA_VERSION};
pub use oldref::{condition_references_old, references_old};
pub use validate::{RegistrationIssue, validate};

// Re-exported so a consumer names the op set through this one crate; it is the
// same [`Op`] the CDC envelope carries (`wamn_event_wire`).
pub use wamn_event_wire::Op;
