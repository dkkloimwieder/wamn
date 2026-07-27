//! The pure Rust mirror of `wamn:flow-invocation@0.1.0`.
//!
//! This crate mirrors [`wit/package.wit`](../wit/package.wit), the versioned
//! caller-facing boundary between ingress and flow execution from FLOW-SPEC
//! rev18 §8, §§9.1–9.7, and §11. The two-stage handshake returns a durable run
//! identity from [`FlowInvocation::begin`] before a caller performs bounded
//! [`FlowInvocation::wait`] calls or requests [`FlowInvocation::cancel`].
//!
//! This is not the runner-to-custom-node protocol. It deliberately shares no
//! types or dependencies with `wamn-node-invoke`.

/// W3C trace context propagated into an admitted run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceContext {
    pub traceparent: String,
    pub tracestate: Option<String>,
}

/// Stage-B output presented to final admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvokeRequest {
    pub attachment_id: String,
    pub expected_catalog_version: u64,
    pub expected_definition_hash: String,
    pub client_request_fingerprint: String,
    /// A mapped, validated JSON document encoded as UTF-8 text.
    pub payload: String,
    pub idempotency_key: Option<String>,
    /// Opaque authenticated identity verified by the ingress adapter.
    pub principal: String,
    /// Optional caller-selected deadline override, in milliseconds.
    pub deadline_override: Option<u64>,
    pub trace: Option<TraceContext>,
}

/// A durable run handle returned immediately after admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admitted {
    pub run_id: String,
}

/// A pre-run refusal. No run exists, so this deliberately has no run ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    pub status: u16,
    pub code: String,
}

/// The first stage of the invocation handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeginResult {
    Admitted(Admitted),
    Rejected(Rejection),
}

/// A stored successful caller outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub run_id: String,
    /// The authored response JSON encoded as UTF-8 text.
    pub body: String,
    pub status_hint: Option<u16>,
}

/// A stored failed or cancelled caller envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowError {
    pub code: String,
    pub message: Option<String>,
    pub run_id: String,
    pub flow_id: String,
    pub flow_version: u32,
}

/// A stored failure and its durable `caller_http_status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub status: u16,
    pub error: FlowError,
}

/// Exactly one stored caller outcome; there is no accepted or pending arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvokeResult {
    Responded(Response),
    Failed(Failure),
    Cancelled(Failure),
}

/// Idempotent acknowledgement of the requested run cancellation.
///
/// This does not claim cancellation won its race with release or terminal
/// transitions. The run identity makes the acknowledgement unambiguous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelAck {
    pub run_id: String,
}

/// The application-level mirror of the WIT `invocation` interface.
pub trait FlowInvocation {
    /// Admit a new run or return a typed pre-run rejection.
    fn begin(&mut self, request: InvokeRequest) -> BeginResult;

    /// Wait at most `timeout_ms`; `None` means the run is still unreleased.
    fn wait(&mut self, run_id: String, timeout_ms: u32) -> Option<InvokeResult>;

    /// Request observed-disconnect cancellation for an admitted run.
    fn cancel(&mut self, run_id: String) -> CancelAck;
}
