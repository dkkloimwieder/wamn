//! Terminal verdicts carried to the queue owner.
use serde_json::Value;
use wamn_event_wire::Op;

/// The reserved field of an [`wamn_catalog::WiringTerminal::Emit`] node's emitted JSON object
/// carrying the dedup id, e.g. `{"dedup-id": "...", ...}`.
///
/// Author-supplied by construction: delivery is at-least-once, so the component
/// derives the key itself from the identities in its `node-context`
/// (`(wiring-id, wiring-version, node-id, delivery-id)` — see
/// `wit/package.wit`) and returns it in the event. The router only carries it;
/// what dedups on it is the boundary (wamn-0h0g.19.7).
pub const DEDUP_ID_FIELD: &str = "dedup-id";

/// How one delivery ended — the router's verdict, decided by the walk and
/// carried out by the host that drove it.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Answer the attached caller with `payload`. `node_id` is the exact wiring
    /// node that declared [`wamn_catalog::WiringTerminal::Respond`], copied from the host-owned walk
    /// coordinate by `terminal_verdict`; it is never read from component output
    /// or the delivery payload. The verdict may be recorded well before the
    /// frontier empties: responding early and continuing to work is a wiring's
    /// prerogative, and the caller's answer is this payload, not whatever the
    /// last node happened to produce.
    Respond { payload: Value, node_id: String },
    /// Publish `event` into the boundary under `dedup_id`, using only the
    /// selector copied from the admitted terminal occurrence. `node_id` is the
    /// exact wiring node that declared [`wamn_catalog::WiringTerminal::Emit`], copied from the
    /// host-owned walk coordinate by `terminal_verdict` exactly as
    /// [`Verdict::Respond`] copies it; it is never read from component output or
    /// the delivery payload. The publishing host carries it onto the effect
    /// span, so a derived event names the node that produced it.
    Emit {
        event: Value,
        dedup_id: String,
        entity: String,
        operation: Op,
        node_id: String,
    },
    /// Nothing to answer and nothing to publish: the frontier emptied with no
    /// caller attached and no terminal node reached.
    Discard,
}
