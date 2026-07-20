//! # wamn-materializer (EVT-MAT, D19 v3 §5 / l5i9.17) — CDC event → run decisions
//!
//! The **pure per-event pipeline** the Service-first materializer guest
//! (`components/materializer`) drives: given a subscribing flow's declaration
//! (an [`EventRegistration`] + the flow's [`Ordering`]/policy) and one
//! delivered CDC envelope with its JetStream `stream_seq`, decide — fire a
//! run, skip, or refuse — and, for a fire, mint everything the enqueue needs:
//! the deterministic zero-padded run id ([`wamn_run_queue::mint_evt_run_id`]),
//! the run-input envelope (causation embedded, the event-chain thread), the
//! partition key + policy (kq0z-coherent), and the numeric stream position.
//!
//! Like `wamn-run-queue` / `wamn-runner`, this crate is **pure**: no DB, no
//! NATS, no clock. The guest supplies the `wamn:postgres` transaction
//! (write-ahead + enqueue, `ON CONFLICT DO NOTHING` — the exactly-once
//! guarantee), the `wamn:jetstream` fetch/ack, and the post-commit doorbell.
//!
//! ## The decision contract (the .17 design fields, load-bearing)
//!
//! - **Tenant guard**: an event fires only for the tenant the workload is
//!   bound to (`new.tenant_id` vs the injected tenant). Another tenant's event
//!   is a normal [`Skip`](Verdict::Skip) (that tenant's own materializer owns
//!   it); an event that CANNOT be tenant-scoped — a DELETE under REPLICA
//!   IDENTITY DEFAULT (old image = `id` only), or a table with no `tenant_id`
//!   column — is an alertable [`Refuse`](Verdict::Refuse), never a cross-tenant
//!   enqueue.
//! - **REPLICA IDENTITY DEFAULT contract** (Q2/l5i9.56): old-absent =
//!   cannot-evaluate, never condition-false. v1 enforces it structurally: a
//!   condition that references the ROOT `old` image is refused —
//!   [`serviceable`] HOLDS the registration (delayed, never lost) until the
//!   per-entity FULL knob (l5i9.31) ships and applies at/before registration.
//! - **Causation budget** (depth 16, l5i9.1): a child run's depth is
//!   `parent.depth + 1` (0 for an organic write); over-budget is a distinct,
//!   alertable refusal. The child causation rides the run input so the
//!   flow-runner declares it (`wamn:runner/causation`) and the NEXT hop's
//!   envelopes carry the incremented depth — the event-chain thread.
//! - **Ordering** (fqg.20/D20/kq0z): the FLOW's `ordering` declaration is
//!   authoritative. `partitioned` keys come from the registration's
//!   `partition-key` extractor over the EVENT context when declared, else the
//!   flow's own expression over the run input — both folded by
//!   [`Ordering::partition_key_for`]'s rules (a null/missing/non-scalar key
//!   degrades to the flow-wide stream, never NULL). Policy stamps only on
//!   keyed rows.

mod condition;
mod context;
mod decide;
mod input;
pub mod sql;

pub use condition::{CompiledCondition, ConditionOutcome, compile_condition, references_old};
pub use context::{event_context, tenant_of};
pub use decide::{
    DecideError, FirePlan, FlowDeclaration, RefuseReason, SkipReason, Verdict, child_causation,
    decide, rq_policy, serviceable,
};
pub use input::evt_input_json;

pub use wamn_event_reg::EventRegistration;
pub use wamn_event_wire::{Causation, Envelope, Op};
pub use wamn_flow::Ordering;

/// The causation depth ceiling (l5i9.1 sign-off: owner set 16, overriding the
/// doc's proposed ~8). A child at depth > this is refused, alertably.
pub const MAX_CAUSATION_DEPTH: u32 = 16;
