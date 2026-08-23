//! The terminal verdict: what one delivery's walk ultimately did.
//!
//! Three verdicts, exhaustively (`docs/exe-model.md` rev 4, "terminal
//! `respond`/`emit`/discard"). [`Verdict::Respond`] releases the synchronous
//! caller attached by ingress paths 1 and 3; [`Verdict::Emit`] hands a derived
//! event and its author-supplied dedup id to the boundary; [`Verdict::Discard`]
//! is the frontier emptying with neither — a stream delivery that produced no
//! published event and had nobody waiting.
//!
//! The verdict is **wiring data, not a node-type string**. The retired engine
//! decided `respond` by comparing a node's reserved type name
//! (`node_type == "respond"`); the node language retires with that engine, so a
//! wiring node here declares its [`Terminal`] as a variant and the walk reads
//! it. Which nodes carry one is the wiring compiler's business, upstream of the
//! router.

use serde_json::Value;

/// The reserved field of an [`Terminal::Emit`] node's emitted JSON object
/// carrying the dedup id, e.g. `{"dedup-id": "...", ...}`.
///
/// Author-supplied by construction: delivery is at-least-once, so the component
/// derives the key itself from the identities in its `node-context`
/// (`(wiring-id, wiring-version, node-id, delivery-id)` — see
/// `wit/package.wit`) and returns it in the event. The router only carries it;
/// what dedups on it is the boundary (wamn-0h0g.19.7).
pub const DEDUP_ID_FIELD: &str = "dedup-id";

/// The terminal a wiring node declares, if it is one.
///
/// Absent on ordinary nodes: most of a graph is intermediate work, and only a
/// node that ends the delivery carries this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminal {
    /// Releases the synchronous caller waiting on this delivery, with the
    /// node's emitted payload as the answer.
    Respond,
    /// Publishes the node's emitted payload as a derived event, keyed by the
    /// [`DEDUP_ID_FIELD`] the author put in it.
    Emit,
}

/// How one delivery ended — the router's verdict, decided by the walk and
/// carried out by the host that drove it.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Answer the attached caller with `payload`. `node_id` is the exact wiring
    /// node that declared [`Terminal::Respond`], copied from the host-owned walk
    /// coordinate by `terminal_verdict`; it is never read from component output
    /// or the delivery payload. The verdict may be recorded well before the
    /// frontier empties: responding early and continuing to work is a wiring's
    /// prerogative, and the caller's answer is this payload, not whatever the
    /// last node happened to produce.
    Respond { payload: Value, node_id: String },
    /// Publish `event` into the boundary under `dedup_id`.
    Emit { event: Value, dedup_id: String },
    /// Nothing to answer and nothing to publish: the frontier emptied with no
    /// caller attached and no terminal node reached.
    Discard,
}
