//! # wamn-router — the host-side graph walk
//!
//! Per delivery: walk a resolved [`Wiring`] in frontier order, invoke each
//! node's component operation through the caller's [`NodeInvoker`], route the
//! emitted payload by **port**, and stop when the frontier empties or a failure
//! goes uncaught. What the walk decides is exactly what the retired flow engine
//! decided — frontier ordering, port routing, error-edge semantics — with the
//! node *language* (flow documents, expression configs, reserved node types,
//! frames) removed rather than relocated. Budgets become a per-delivery **hop
//! limit**: there are no frames to bound.
//!
//! The walk itself holds no clock, no pool, no cache, no runtime and no DB.
//! Node invocation is a plain Rust trait so the typed WIT seam binds outside
//! this crate, and instance pooling belongs to the host that drives it; the
//! store read that resolves a wiring is the host's too, but the memory it is
//! read into ONCE is [`WiringCache`], here, because the graph it holds is this
//! crate's type. The walk does DECIDE the terminal [`Verdict`] — `respond` / `emit` /
//! discard — because only the walk knows which node ended the delivery; acting
//! on that verdict (answering the caller, publishing to the boundary) is the
//! host's.
//!
//! ## Driving the walk
//! [`route`] is the synchronous entry point and drives the loop for you. A host
//! that must await its invocations drives the same two functions directly:
//!
//! ```ignore
//! let mut walk = wiring.start(delivery);
//! loop {
//!     match wiring.next(&mut walk, now_ms()) {
//!         Step::Invoke(call) => {
//!             let o = invoke(&call).await;
//!             // `?` here propagates a component's own bad data as a host fault.
//!             // Drivers that cannot re-ask their component fold it instead:
//!             // `wiring.fail_on_node_data(&mut walk, &call.node, refused)?`.
//!             wiring.apply(&mut walk, &call, o, now_ms())?;
//!         }
//!         Step::Wait { until_ms, throttle, .. } => { /* gate `throttle`, wait to until_ms */ }
//!         Step::Done(status) => break status,
//!     }
//! }
//! ```

mod outcome;
mod resolution;
mod retry;
mod terminal;
mod walk;
mod wiring;

use serde_json::Value;

#[doc(inline)]
pub use outcome::{ERROR_PORT, ErrorDetail, MAIN_PORT, NodeError, NodeOutcome, RateLimitDetail};
#[doc(inline)]
pub use resolution::{ActiveWiring, WiringCache};
#[doc(inline)]
pub use retry::{RetryPolicy, ThrottleKey};
#[doc(inline)]
pub use terminal::{DEDUP_ID_FIELD, Terminal, Verdict};
#[doc(inline)]
pub use walk::{
    ApplyError, ApplyErrorKind, Delivery, Failure, FailureKind, NodeCall, Step, Walk, WalkStatus,
};
#[doc(inline)]
pub use wiring::{DEFAULT_HOP_LIMIT, Wiring, WiringEdge, WiringError, WiringErrorKind, WiringNode};

/// The seam a node invocation crosses. `invoke` is where the typed
/// node-operation WIT binding lands; the clock methods are host services the
/// walk needs to honor a retry backoff.
pub trait NodeInvoker {
    /// Acquire an instance of `call.component` and invoke `call.operation`.
    fn invoke(&mut self, call: &NodeCall) -> NodeOutcome;

    /// The current monotonic millisecond reading.
    fn now_ms(&mut self) -> u64;

    /// Wait until `until_ms` before the next invocation, coordinating on
    /// `throttle` if the host runs a shared rate-limit gate.
    fn wait_until(&mut self, until_ms: u64, throttle: Option<&ThrottleKey>);
}

/// What one delivery's walk produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub status: WalkStatus,
    /// The payload of the last node that completed successfully.
    pub result: Value,
    /// The recorded failure, if the walk failed.
    pub failure: Option<Failure>,
    /// Invocations handed out, against the wiring's hop limit.
    pub hops: u64,
    /// What the delivery ended up doing, for the host to carry out. `None` when
    /// the walk failed before reaching a terminal — a failure is not a verdict,
    /// and turning one into a caller's answer is ingress's decision, not the
    /// walk's.
    pub verdict: Option<Verdict>,
}

/// Walk `delivery` through `wiring` to a terminal status.
pub fn route(delivery: Delivery, wiring: &Wiring, invoker: &mut impl NodeInvoker) -> Outcome {
    let mut walk = wiring.start(delivery);
    loop {
        let now = invoker.now_ms();
        match wiring.next(&mut walk, now) {
            Step::Done(status) => {
                return Outcome {
                    status,
                    result: walk.result().clone(),
                    failure: walk.failure().cloned(),
                    hops: walk.hops(),
                    verdict: walk.verdict().cloned(),
                };
            }
            Step::Wait {
                until_ms, throttle, ..
            } => invoker.wait_until(until_ms, throttle.as_ref()),
            Step::Invoke(call) => {
                let outcome = invoker.invoke(&call);
                let now = invoker.now_ms();
                if let Err(refused) = wiring.apply(&mut walk, &call, outcome, now) {
                    // A refusal DATA caused — a component's context replacement,
                    // or a wiring's terminal meeting a delivery it does not fit —
                    // ends this delivery down the node's error edge. The panic is
                    // kept for the refusals that describe a driver feeding back an
                    // outcome the walk never handed out, which this loop, handing
                    // out one call and feeding back its answer, cannot do.
                    wiring
                        .fail_on_node_data(&mut walk, &call.node, refused)
                        .expect("the walk accepts the outcome of the call it handed out");
                }
            }
        }
    }
}
