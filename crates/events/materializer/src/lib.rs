//! Pure registration filtering and event-input projection for stream ingress.

mod condition;
mod context;
mod decide;
mod input;
pub mod sql;

pub use condition::{CompiledCondition, ConditionOutcome, compile_condition};
pub use context::{event_context, tenant_of};
pub use decide::{
    DecideError, RefuseReason, SkipReason, Verdict, VerifiedSourceEventId, decide, serviceable,
    verified_source_event_id,
};
pub use input::event_input;
pub use wamn_event_reg::{EventRegistration, condition_references_old, references_old};
pub use wamn_event_wire::{Causation, Envelope, Op};

/// Maximum admitted event-causation depth.
pub const MAX_CAUSATION_DEPTH: u32 = 16;
