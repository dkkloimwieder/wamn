//! Event workflow matching and input projection.
mod condition;
mod context;
mod decide;
mod input;
mod oldref;
mod workflow;
pub use workflow::{EventTrigger, TriggerMatchError, TriggerMatchErrorType};
