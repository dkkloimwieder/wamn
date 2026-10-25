//! Event trigger predicates shared by authoring and the materializer.
//!
//! The materializer adapts environment workflow rows into [`EventRegistration`]
//! for condition evaluation. This crate owns the condition grammar and op set.
//! It performs no database access and stores no package registrations.

mod model;
mod oldref;
mod validate;

pub use model::{EventRegistration, RegistrationInput, SCHEMA_VERSION};
pub use oldref::{condition_references_old, references_old};
pub use validate::{RegistrationIssue, validate};

// Re-exported so a consumer names the op set through this one crate; it is the
// same [`Op`] the CDC envelope carries (`wamn_event_wire`).
pub use wamn_event_wire::Op;
